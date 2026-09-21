use std::io::{self, stdout};
use std::panic;
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;

use anyhow::{Context, Result};
use crossterm::{
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};

use super::app::{App, EmailEntry, MessageRef};

use crate::config::AccountConfig;
use crate::draft::parse_email_draft;
use crate::parse::FetchedEmail;

// ---------------------------------------------------------------------------
// Terminal helpers
// ---------------------------------------------------------------------------

pub(super) fn suspend_terminal(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
) -> Result<()> {
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        crossterm::cursor::Show,
    )?;
    disable_raw_mode()?;
    Ok(())
}

pub(super) fn resume_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    enable_raw_mode()?;
    execute!(terminal.backend_mut(), EnterAlternateScreen)?;
    terminal.clear()?;
    Ok(())
}

pub(super) fn init_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout());
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

pub(super) fn restore_terminal() -> Result<()> {
    execute!(stdout(), LeaveAlternateScreen)?;
    disable_raw_mode()?;
    Ok(())
}

pub(super) fn install_panic_hook() {
    let original_hook = panic::take_hook();
    panic::set_hook(Box::new(move |panic_info| {
        let _ = execute!(stdout(), LeaveAlternateScreen);
        let _ = disable_raw_mode();
        original_hook(panic_info);
    }));
}

// ---------------------------------------------------------------------------
// Editor / clipboard
// ---------------------------------------------------------------------------

fn editor() -> String {
    std::env::var("EDITOR").unwrap_or_else(|_| "hx".to_string())
}

pub(super) fn edit_file(path: &Path) -> Result<()> {
    let editor = editor();
    let status = std::process::Command::new(&editor)
        .arg(path)
        .status()
        .with_context(|| format!("Failed to launch editor: {}", editor))?;
    if !status.success() {
        anyhow::bail!("Editor exited with status: {}", status);
    }
    Ok(())
}

pub(super) fn copy_to_clipboard(text: &str) -> Result<()> {
    let mut clipboard = arboard::Clipboard::new().context("Failed to access clipboard")?;
    clipboard
        .set_text(text)
        .context("Failed to copy to clipboard")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Library call helpers
// ---------------------------------------------------------------------------

/// The fragment every failed-drain status suffix carries, so the completion
/// handler can honestly downgrade the status level of an otherwise-successful
/// sync that also rolled mutations back (#0039 review note).
pub(crate) const FAILED_OPS_MARKER: &str = "mutation(s) failed and were rolled back";

/// The other substring a finished-sync status line must not carry on a green
/// line: a fetch that keeps downloading the same mail (#0115). Read by
/// `tui::bg::drained_sync_level` like [`FAILED_OPS_MARKER`].
pub(crate) const NON_CONVERGING_MARKER: &str = "fetch not converging";

/// The status line a tick refused the engine lock reports (#0122), and the
/// substring `tui::bg::drained_sync_level` reads to show it at
/// [`crate::tui::app::StatusLevel::Info`] rather than as a green sync that
/// happened or a red one that failed. The wording is the CLI's, so a user who
/// meets the refusal in both places reads the same sentence.
pub(crate) const SYNC_SKIPPED_MARKER: &str = "Sync skipped: another engine is syncing";

/// Turn a server-search hit into a list entry.
///
/// The hit came straight off the server and was never ingested, so it may or
/// may not correspond to a row this account already holds. The entry is
/// resolved against the store by Message-ID (`store::read::find_by_message_id`,
/// the same indexed lookup that replaced the startup Message-ID walk in
/// #0038): a hit that resolves carries `Some(MessageRef)` and behaves like any
/// other row, a hit that does not carries `None` and every row-dependent
/// operation on it declines with a status message. There is deliberately no
/// sentinel `MessageRef`: an entry that pretends to be row 0 could reach the
/// selection set and a batch action would then act on the wrong message.
///
/// This is the honest continuation of the gap #0049 recorded as 4a
/// ("server-search open hit only resolves messages already local"): before the
/// nuke the resolution was a Message-ID scan of the mailbox directory, now it
/// is the same question asked of the store. A server-only hit is still listed
/// and previewable from the fetched content and still not openable as a local
/// message; closing that needs the hit to be ingested on demand, which is not
/// this unit.
///
/// A message that lives in several mailboxes resolves to the first row in
/// `(mailbox, uid)` order, which is what the directory scan did too (it looked
/// only in the mailbox the hit came from and took the single match there).
///
/// The resolution is the daemon's since P5-U10c (`LST-08`, #0126): the hit
/// arrives with `row_id` and `selector` already filled in, which is what let
/// the store read this function used to make go away. What is left is the
/// mapping, unchanged, so the overlay's row is derived by the same rule it
/// always was.
pub(super) fn fetched_to_email_entry(
    msg: Option<MessageRef>,
    selector: Option<String>,
    fetched: &FetchedEmail,
) -> EmailEntry {
    let (date_display, date_sort) =
        if let Ok(dt) = chrono::DateTime::parse_from_rfc2822(&fetched.date) {
            (
                dt.format("%Y-%m-%d").to_string(),
                dt.format("%Y-%m-%dT%H:%M:%S").to_string(),
            )
        } else {
            (
                fetched.date.chars().take(10).collect(),
                fetched.date.clone(),
            )
        };

    EmailEntry {
        msg,
        draft_id: None,
        skip: None,
        // A server hit that resolved locally carries the selector the daemon
        // rendered for its row; one that never synced has no `messages` row
        // for a selector to point at.
        selector,
        from: fetched.from.clone(),
        to: fetched.to.clone(),
        cc: fetched.cc.clone(),
        reply_to: fetched.reply_to.clone(),
        bcc: fetched.bcc.clone(),
        subject: fetched.subject.clone(),
        status: String::new(),
        date_display,
        date_sort,
        has_attachments: fetched.has_attachments,
        read: fetched.flags.seen,
        answered: fetched.flags.answered,
        forwarded: fetched.flags.forwarded,
        flagged: fetched.flags.flagged,
        is_invite: fetched.event.is_some(),
    }
}

// ---------------------------------------------------------------------------
// Account resolution for Send
// ---------------------------------------------------------------------------

/// Which transport a send actually uses, keyed off the account's
/// `auth_method` alone.
///
/// A Graph account sends over Graph or not at all: an SMTP config that happens
/// to be loaded is not a fallback for a `GraphConfig` that is not.
/// `AccountState::new` loads the Graph config with `GraphConfig::load(..).ok()`,
/// so a Graph account whose config fails to load carries `graph_config: None`;
/// a guard that asks "is there a Graph config?" therefore sends such an account
/// over SMTP behind the user's back, under an identity Graph would have
/// stamped. Asking the account what it is instead makes that case an error the
/// user sees.
///
/// `Err` carries the status-line wording for the missing transport.
pub(super) fn resolve_send_transport(
    account_config: &AccountConfig,
    graph: Option<crate::config::GraphConfig>,
    smtp: Option<crate::config::SmtpConfig>,
) -> Result<(Option<crate::config::GraphConfig>, Option<crate::config::SmtpConfig>), &'static str> {
    if account_config.auth_method == crate::config::AuthMethod::Graph {
        match graph {
            Some(g) => Ok((Some(g), None)),
            None => Err("Graph not configured"),
        }
    } else {
        match smtp {
            Some(s) => Ok((None, Some(s))),
            None => Err("SMTP not configured"),
        }
    }
}

/// Which account sends this draft: the one whose address its `from:` names,
/// falling back to the active one.
///
/// The draft file rather than the open mailbox is the source of truth, because
/// a draft written for another configured account (a reply to mail that
/// arrived there) has to leave through that account's credentials.
pub(super) fn resolve_send_account(
    app: &App,
    draft_path: &Path,
) -> (
    usize,
    Option<crate::config::SmtpConfig>,
    Option<crate::config::ImapConfig>,
    Option<crate::config::GraphConfig>,
    AccountConfig,
    Option<String>,
) {
    if let Ok(draft) = parse_email_draft(draft_path) {
        let from = draft.frontmatter.from.unwrap_or_default().to_lowercase();
        for (i, acct) in app.accounts.iter().enumerate() {
            if from.contains(&acct.account_config.default_from.to_lowercase()) {
                return (
                    i,
                    acct.smtp_config.clone(),
                    acct.imap_config.clone(),
                    acct.graph_config.clone(),
                    acct.account_config.clone(),
                    acct.signature_content.clone(),
                );
            }
        }
    }
    let acct = &app.accounts[app.active_account];
    (
        app.active_account,
        acct.smtp_config.clone(),
        acct.imap_config.clone(),
        acct.graph_config.clone(),
        acct.account_config.clone(),
        acct.signature_content.clone(),
    )
}

// ---------------------------------------------------------------------------
// Tests (#0049 unit 0b)
//
// `src/tui/helpers.rs` had no tests at all. These capture the two pieces the
// audit called out as user-visible: which account a draft is sent from, and
// how a server-search hit is turned into a list row.
//
// Tagging convention, per #0049: `parity` means the new build must reproduce
// the recorded behaviour, `known-bug` means the recorded behaviour is wrong
// and the comment names the target. Nothing here is fixed in this unit.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AccountConfig;
    use crate::parse::FetchedEmail;

    // -----------------------------------------------------------------------
    // Fixtures
    // -----------------------------------------------------------------------

    /// A minimal `AccountState`. Built as a struct literal rather than through
    /// `AccountState::new`, which reads the user's config, keyring and
    /// signature files and would make these tests machine-dependent.
    fn account(name: &str, default_from: &str) -> super::super::app::AccountState {
        super::super::app::AccountState {
            account_config: AccountConfig {
                name: name.to_string(),
                default_from: default_from.to_string(),
                ..Default::default()
            },
            imap_config: None,
            smtp_config: None,
            graph_config: None,
            signature_content: Some(format!("-- \n{name}")),
            archive_server_name: "Archive".to_string(),
            drafts_dir: None,
            mailboxes: Vec::new(),
            mailbox_counts: Vec::new(),
            email_cache: Vec::new(),
            sidebar_index: 0,
            active_mailbox: 0,
            list_index: 0,
            cursor_ref: None,
            headers_scroll: 0,
            preview_scroll: 0,
            selection: std::collections::HashSet::new(),
            search_query: String::new(),
            watcher_active: false,
            opening: false,
            has_unseen: false,
            sync_health: crate::sync_health::SyncHealth::default(),
        }
    }

    fn app_with(accounts: Vec<super::super::app::AccountState>, active: usize) -> App {
        let mut app = App::default_for_tests();
        app.accounts = accounts;
        app.active_account = active;
        app
    }

    /// Write a draft whose `from:` is exactly `from`.
    fn draft(dir: &Path, name: &str, from: Option<&str>) -> PathBuf {
        let from_line = match from {
            Some(f) => format!("from: \"{f}\"\n"),
            None => String::new(),
        };
        let path = dir.join(name);
        std::fs::write(
            &path,
            format!("---\nto: someone@example.com\nsubject: hi\nstatus: draft\n{from_line}---\n\nbody\n"),
        )
        .unwrap();
        path
    }

    fn smtp() -> crate::config::SmtpConfig {
        crate::config::SmtpConfig {
            host: "smtp.example.com".to_string(),
            port: 587,
            username: "me@example.com".to_string(),
            password: "secret".to_string(),
            default_from: "me@example.com".to_string(),
            accept_invalid_certs: false,
            auth_method: crate::config::AuthMethod::Password,
        }
    }

    fn graph() -> crate::config::GraphConfig {
        crate::config::GraphConfig {
            client_id: "cid".to_string(),
            tenant_id: "tid".to_string(),
            username: "me@example.com".to_string(),
            account_name: "work".to_string(),
        }
    }

    fn account_config(auth_method: crate::config::AuthMethod) -> AccountConfig {
        AccountConfig {
            name: "work".to_string(),
            default_from: "me@example.com".to_string(),
            auth_method,
            ..Default::default()
        }
    }

    // -----------------------------------------------------------------------
    // resolve_send_transport (#0058 review)
    // -----------------------------------------------------------------------

    /// A Graph account sends over Graph, and the SMTP config it also carries
    /// is dropped rather than kept as a fallback.
    #[test]
    fn resolve_send_transport_sends_a_graph_account_over_graph_only() {
        let cfg = account_config(crate::config::AuthMethod::Graph);
        // `SmtpConfig` holds a password and deliberately has no `Debug`, so
        // these unwrap by hand rather than through `Result::unwrap`.
        let Ok((g, s)) = resolve_send_transport(&cfg, Some(graph()), Some(smtp())) else {
            panic!("a Graph account with a Graph config has a transport");
        };
        assert!(g.is_some());
        assert!(s.is_none(), "the loaded SMTP config is not a Graph fallback");
    }

    /// The regression this guard exists for: `AccountState::new` loads the
    /// Graph config with `.ok()`, so a Graph account whose config fails to
    /// load reaches the send path with `graph: None`. It must be refused, not
    /// quietly sent over the SMTP config that did load.
    #[test]
    fn resolve_send_transport_refuses_a_graph_account_with_no_graph_config() {
        let cfg = account_config(crate::config::AuthMethod::Graph);
        let Err(missing) = resolve_send_transport(&cfg, None, Some(smtp())) else {
            panic!("a Graph account with no Graph config fell back to SMTP");
        };
        assert_eq!(missing, "Graph not configured");
    }

    /// A password account sends over SMTP, and a Graph config that somehow
    /// loaded is not consulted.
    #[test]
    fn resolve_send_transport_sends_a_password_account_over_smtp_only() {
        let cfg = account_config(crate::config::AuthMethod::Password);
        let Ok((g, s)) = resolve_send_transport(&cfg, Some(graph()), Some(smtp())) else {
            panic!("a password account with an SMTP config has a transport");
        };
        assert!(g.is_none());
        assert!(s.is_some());

        let Err(missing) = resolve_send_transport(&cfg, Some(graph()), None) else {
            panic!("a password account with no SMTP config has no transport");
        };
        assert_eq!(missing, "SMTP not configured");
    }

    fn fetched(date: &str) -> FetchedEmail {
        FetchedEmail {
            from: "Jürgen Müller <juergen@example.de>".to_string(),
            to: "me@example.com".to_string(),
            cc: Some("cc@example.com".to_string()),
            reply_to: None,
            bcc: None,
            subject: "Grüße".to_string(),
            date: date.to_string(),
            body_text: "body text".to_string(),
            html_body: Some("<p>body text</p>".to_string()),
            has_attachments: true,
            message_id: Some("<m1@example.de>".to_string()),
            attachments: Vec::new(),
            flags: crate::types::MessageFlags::seen(true),
            calendar_ics: None,
            event: None,
        }
    }

    // -----------------------------------------------------------------------
    // resolve_send_account
    // -----------------------------------------------------------------------

    /// parity. The draft's `from:` selects the account, case-insensitively,
    /// and the whole per-account bundle (index, configs, signature, sent dir)
    /// comes from that account rather than from the active one.
    #[test]
    fn resolve_send_account_matches_the_draft_from_address_case_insensitively() {
        let tmp = tempfile::tempdir().unwrap();
        let app = app_with(
            vec![
                account("work", "sylvain@work.example"),
                account("perso", "sylvain@perso.example"),
            ],
            0,
        );
        let path = draft(
            tmp.path(),
            "d.md",
            Some("Sylvain Hellin <SYLVAIN@Perso.Example>"),
        );

        let (idx, _smtp, _imap, _graph, cfg, signature) =
            resolve_send_account(&app, &path);
        assert_eq!(idx, 1);
        assert_eq!(cfg.name, "perso");
        assert_eq!(signature.as_deref(), Some("-- \nperso"));
    }

    /// parity. With no `from:` in the draft, or with a `from:` that matches no
    /// account, or with an unreadable path, the active account sends. The
    /// fallback is silent: nothing tells the user the address they typed was
    /// ignored.
    #[test]
    fn resolve_send_account_falls_back_to_the_active_account() {
        let tmp = tempfile::tempdir().unwrap();
        let app = app_with(
            vec![
                account("work", "sylvain@work.example"),
                account("perso", "sylvain@perso.example"),
            ],
            1,
        );

        let no_from = draft(tmp.path(), "no-from.md", None);
        assert_eq!(resolve_send_account(&app, &no_from).0, 1);

        let foreign = draft(tmp.path(), "foreign.md", Some("someone@elsewhere.example"));
        assert_eq!(resolve_send_account(&app, &foreign).0, 1);

        let missing = tmp.path().join("does-not-exist.md");
        assert_eq!(resolve_send_account(&app, &missing).0, 1);
    }

    /// parity. When two accounts share the same address the first one in
    /// config order wins.
    #[test]
    fn resolve_send_account_takes_the_first_of_two_accounts_sharing_an_address() {
        let tmp = tempfile::tempdir().unwrap();
        let app = app_with(
            vec![
                account("first", "shared@example.com"),
                account("second", "shared@example.com"),
            ],
            1,
        );
        let path = draft(tmp.path(), "d.md", Some("shared@example.com"));
        let (idx, _, _, _, cfg, _) = resolve_send_account(&app, &path);
        assert_eq!(idx, 0);
        assert_eq!(cfg.name, "first");
    }

    /// known-bug. The match is a substring test (`from.contains(default_from)`),
    /// not an address comparison, so a draft from `not-sylvain@work.example`
    /// resolves to the `sylvain@work.example` account: the mail goes out over
    /// the wrong account's SMTP server and is filed in the wrong Sent folder.
    /// Target: compare the parsed address of the draft's `from:` with the
    /// account address for equality (case-insensitive), and fall back to the
    /// active account when nothing matches.
    #[test]
    fn resolve_send_account_substring_match_picks_a_foreign_address() {
        let tmp = tempfile::tempdir().unwrap();
        let app = app_with(
            vec![
                account("decoy", "no-such@example.org"),
                account("work", "sylvain@work.example"),
            ],
            0,
        );
        let path = draft(tmp.path(), "d.md", Some("not-sylvain@work.example"));
        let (idx, _, _, _, cfg, _) = resolve_send_account(&app, &path);
        assert_eq!(idx, 1, "a different mailbox matched by substring");
        assert_eq!(cfg.name, "work");
    }

    /// known-bug. An account with an empty `default_from` (a half-configured
    /// account, which the config wizard allows) swallows every draft, because
    /// every string contains the empty string. It wins even over the account
    /// whose address the draft actually names, since it comes first.
    /// Target: an empty account address must never match.
    #[test]
    fn resolve_send_account_empty_default_from_matches_every_draft() {
        let tmp = tempfile::tempdir().unwrap();
        let app = app_with(
            vec![account("half-configured", ""), account("work", "sylvain@work.example")],
            1,
        );
        let path = draft(tmp.path(), "d.md", Some("sylvain@work.example"));
        let (idx, _, _, _, cfg, _) = resolve_send_account(&app, &path);
        assert_eq!(idx, 0);
        assert_eq!(cfg.name, "half-configured");
    }

    // -----------------------------------------------------------------------
    // fetched_to_email_entry
    // -----------------------------------------------------------------------

    /// parity. A server-search hit becomes a list row with no store row (the
    /// account below has no store at all) and an empty status, and the
    /// remaining fields are copied through.
    #[test]
    fn fetched_to_email_entry_copies_fields_and_leaves_msg_and_status_empty() {
        let entry = fetched_to_email_entry(None, None, &fetched("Mon, 01 Jan 2024 12:00:00 +0000"));

        assert_eq!(entry.msg, None);
        assert_eq!(entry.status, "");
        assert_eq!(entry.subject, "Grüße");
        assert_eq!(entry.to, "me@example.com");
        assert_eq!(entry.cc.as_deref(), Some("cc@example.com"));
        assert!(entry.has_attachments);
        assert!(entry.read);
        assert!(!entry.is_invite);
        assert_eq!(entry.date_display, "2024-01-01");
    }

    /// known-bug. The sort key keeps the sender's local wallclock instead of
    /// being normalised to UTC, so two hits from different timezones on the
    /// same day sort by wallclock rather than by instant. `resolve_date` in
    /// `src/tui/app/types.rs` normalises (that was the fix for #0024); this
    /// path never got it, so a search result and the same email loaded from
    /// disk carry different sort keys.
    /// Target: `date_sort` in UTC, i.e. `2024-01-01T08:00:00` below.
    #[test]
    fn fetched_to_email_entry_sort_key_keeps_the_sender_local_wallclock() {
        let entry = fetched_to_email_entry(None, None, &fetched("Mon, 01 Jan 2024 10:00:00 +0200"));
        assert_eq!(entry.date_display, "2024-01-01");
        assert_eq!(entry.date_sort, "2024-01-01T10:00:00");

        // 10:00+0200 is 08:00 UTC, so this later message (09:00 UTC) must sort
        // after it. On the recorded wallclock keys it sorts before.
        let later = fetched_to_email_entry(None, None, &fetched("Mon, 01 Jan 2024 09:00:00 +0000"));
        assert_eq!(later.date_sort, "2024-01-01T09:00:00");
        assert!(
            later.date_sort < entry.date_sort,
            "the later message sorts first"
        );
    }

    /// parity. A `Date:` header that is not RFC 2822 (or the `(unknown date)`
    /// placeholder from `parse_rfc822_to_fetched_email`) degrades to its first
    /// ten characters for display, counted in characters so a multi-byte date
    /// string cannot panic, with the raw string as the sort key.
    #[test]
    fn fetched_to_email_entry_falls_back_to_the_first_ten_chars() {
        let entry = fetched_to_email_entry(None, None, &fetched("2024-01-01 12:00 (approx)"));
        assert_eq!(entry.date_display, "2024-01-01");
        assert_eq!(entry.date_sort, "2024-01-01 12:00 (approx)");

        let placeholder = fetched_to_email_entry(None, None, &fetched("(unknown date)"));
        assert_eq!(placeholder.date_display, "(unknown d");
        assert_eq!(placeholder.date_sort, "(unknown date)");

        let unicode = fetched_to_email_entry(None, None, &fetched("日本語の日付です、これは長い"));
        assert_eq!(unicode.date_display, "日本語の日付です、こ");
    }
    /// The `msg` a hit carries is the daemon's resolution, handed in: a hit
    /// that `message.search_server` resolved to a local row arrives with its
    /// `row_id`, and one that never synced arrives with `null`. There is
    /// deliberately no sentinel `MessageRef` for the second case, because a
    /// fake ref could reach the selection set and a batch action would then
    /// act on a different message.
    #[test]
    fn a_search_hit_carries_a_ref_only_when_the_daemon_resolved_one() {
        let fetched = fetched("Mon, 01 Jan 2024 12:00:00 +0000");
        let resolved = fetched_to_email_entry(
            Some(crate::tui::app::MessageRef::new(41)),
            Some("mp://alice/inbox/local@example.de".to_string()),
            &fetched,
        );
        assert_eq!(resolved.msg, Some(crate::tui::app::MessageRef::new(41)));
        assert_eq!(
            resolved.selector.as_deref(),
            Some("mp://alice/inbox/local@example.de"),
            "a resolved hit carries the name `y` copies"
        );

        let server_only = fetched_to_email_entry(None, None, &fetched);
        assert_eq!(server_only.msg, None, "a server-only hit carries no ref");
        assert_eq!(server_only.selector, None, "and no selector either");
    }

    /// known-bug. The row keeps the raw `From:` header, address included,
    /// while `parse_email` (the on-disk path) stores only the display name.
    /// The same email therefore reads "Jürgen Müller <juergen@example.de>" in
    /// the search overlay and "Jürgen Müller" in the list.
    /// Target: one projection for both paths.
    #[test]
    fn fetched_to_email_entry_keeps_the_raw_from_header() {
        let entry = fetched_to_email_entry(None, None, &fetched("Mon, 01 Jan 2024 12:00:00 +0000"));
        assert_eq!(entry.from, "Jürgen Müller <juergen@example.de>");
        assert_eq!(
            crate::tui::app::extract_display_name(&entry.from),
            "Jürgen Müller",
            "what the list would have shown for the same email"
        );
    }
}
