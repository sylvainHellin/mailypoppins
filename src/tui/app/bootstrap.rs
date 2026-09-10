//! Building the TUI's model from a `state.bootstrap` snapshot (P5-U2, #0124).
//!
//! Two entry points, one of them the other's caller:
//!
//! - [`App::from_bootstrap`] builds a whole `App` out of a snapshot, which is
//!   what the daemon-backed golden frames (`ui::golden_frames_daemon`) render.
//! - [`App::apply_bootstrap`] lands a snapshot on an `App` that is already on
//!   screen, which is what the running TUI does: the shell paints from
//!   [`App::new`] first (#0003), and the bootstrap arrives a beat later.
//!
//! # What a bootstrap owns
//!
//! Four things a frame can show: which accounts there are, how each one is
//! doing, which mailboxes they have, and what the counts are. It carries no
//! message row by design - the whole-list transfer
//! (`docs/baselines/decisions/list-transfer.md`) is a query per mailbox open,
//! which is P5-U3's - so the list, the cursor, the overlays and the preview
//! stay whatever they were. Nothing here opens a store, takes an engine lock or
//! walks a mail directory.
//!
//! # The rule that keeps today's frames byte-identical
//!
//! [`App::apply_bootstrap`] touches an account **only while it is still
//! `opening`**: the background open of #0003 clears that marker when it lands
//! with the counts the account really has, and a bootstrap that arrives a beat
//! after it at startup may not put a snapshot's numbers back over them.
//!
//! # The rule that keeps a recovered client honest
//!
//! [`App::apply_resync_bootstrap`] is the other entry, and it skips nothing.
//! A `state.resync_required` or a reconnect to a new instance means the events
//! between the last watermark and this snapshot are gone, so an account that
//! opened long ago holds mailboxes, counts and cached listings that no event
//! will ever correct: the watermark moves past them. That entry therefore
//! drops every per-account listing cache, re-applies the snapshot's mailboxes
//! and counts over every account it names, and reloads the open mailbox
//! through [`App::reload_current_mailbox`], the same off-thread daemon-backed
//! path a mailbox switch takes.
//!
//! Both entries are the one place the event watermark is set (P5-U8), which is
//! why a resync and a reconnect are spelled "bootstrap again" rather than
//! "clear a flag".

use mp_protocol::state::{Bootstrap, MailboxRow};

use super::{AccountState, App, MailboxInfo, MailboxKind, StatusLevel};

impl App {
    /// Build an `App` from a daemon snapshot and the configuration the same
    /// daemon was seeded from.
    ///
    /// The accounts are the *snapshot's*, in the snapshot's order (which is
    /// `config.toml`'s), with `active_account` at 0: the daemon is the
    /// authority on which accounts exist, and a client that iterated its own
    /// config instead would show an account the daemon has never heard of.
    /// `global_config` supplies each account's [`crate::config::AccountConfig`]
    /// (paths, mailbox mapping, credentials), because none of that is on the
    /// wire; an account the snapshot names and the config does not gets a
    /// default config carrying its name, so it renders as a row rather than
    /// vanishing.
    pub fn from_bootstrap(
        global_config: crate::config::GlobalConfig,
        bootstrap: &Bootstrap,
    ) -> App {
        // Before the first frame, as `App::new` does it: `theme::init` is a
        // `OnceLock`, so whichever constructor runs first decides the theme for
        // the process.
        let theme_warning = crate::tui::theme::init(&global_config.theme);

        let accounts: Vec<AccountState> = bootstrap
            .snapshot
            .accounts
            .iter()
            .map(|account| {
                let config = global_config
                    .accounts
                    .iter()
                    .find(|configured| configured.name == account.name)
                    .cloned()
                    .unwrap_or_else(|| crate::config::AccountConfig {
                        name: account.name.clone(),
                        ..Default::default()
                    });
                AccountState::new(config, &global_config.email)
            })
            .collect();

        let mut app = App::shell(global_config, accounts);
        app.apply_bootstrap(bootstrap);
        // The active account's fields become the live ones. Safe here and only
        // here: this `App` has never been on screen, so there is no cursor,
        // no selection and no scroll position to overwrite.
        app.load_from_account(0);

        if let Some(warning) = theme_warning {
            app.push_status(warning, StatusLevel::Warning);
        }

        app
    }

    /// Land a snapshot on an `App` that already exists.
    ///
    /// Per account, matched by name: the mailbox rows, the counts, and whether
    /// it is still opening. Accounts the snapshot does not name are left alone
    /// rather than removed, because a configuration reload is
    /// `config.changed`'s business (P5-U8) and dropping an `AccountState` here
    /// would invalidate `active_account` mid-frame.
    ///
    /// An account whose store has already been opened in the background is
    /// skipped whole (see the module header): at startup that open is the
    /// fresher answer, and the events that follow keep it fresh.
    /// [`App::apply_resync_bootstrap`] is the entry for the case where they
    /// did not.
    pub fn apply_bootstrap(&mut self, bootstrap: &Bootstrap) {
        self.land_bootstrap(bootstrap, false);
    }

    /// Land a snapshot after a resync or a reconnect, over every account.
    ///
    /// The recovery entry, and the difference from [`App::apply_bootstrap`] is
    /// that nothing is skipped: the client has just been told that the events
    /// between its watermark and this snapshot are unavailable, so an account
    /// that opened before the gap keeps mailboxes, counts and cached listings
    /// that nothing will refresh once the watermark moves past them. Every
    /// account the snapshot names takes the snapshot's mailboxes and counts,
    /// every per-account listing cache is dropped, and the open mailbox is
    /// reloaded through the ordinary off-thread path.
    pub fn apply_resync_bootstrap(&mut self, bootstrap: &Bootstrap) {
        self.land_bootstrap(bootstrap, true);
        // Off the UI thread, through `Action::LoadMailbox`, which is where the
        // daemon-backed listing comes from since P5-U4: the stale list stays
        // visible until the fresh one lands, as it does on a same-mailbox
        // reload.
        self.reload_current_mailbox();
    }

    /// The body both entries share; `resync` is what the module header calls
    /// the recovery rule.
    fn land_bootstrap(&mut self, bootstrap: &Bootstrap, resync: bool) {
        // The only place a watermark is set (P5-U8). The instance and the
        // revision are the daemon's word about the state this snapshot
        // describes, so every event above it is comparable and everything at or
        // below it is already here.
        let orphaned = self
            .events
            .watermark(&bootstrap.instance_id, bootstrap.revision);
        if orphaned > 0 {
            // The daemon that was running them is gone, so the spinner they
            // are holding up would never come down.
            log::warn!("[events] {orphaned} operation(s) died with the previous daemon");
            self.bg_count = self.bg_count.saturating_sub(orphaned);
        }
        for account in &bootstrap.snapshot.accounts {
            let Some(index) = self
                .accounts
                .iter()
                .position(|state| state.account_config.name == account.name)
            else {
                continue;
            };
            if !resync && !self.accounts[index].opening {
                continue;
            }

            let rows = bootstrap.snapshot.mailboxes_of(&account.name);
            let state = &mut self.accounts[index];
            if resync {
                // Every cached listing predates the gap, whatever the rows
                // below do with the sidebar. Dropped rather than kept: a row
                // the client never heard about being removed is worse than a
                // reload.
                for slot in &mut state.email_cache {
                    *slot = None;
                }
            }
            if !rows.is_empty() {
                let template = super::build_mailboxes(&state.account_config);
                state.mailboxes = rows
                    .iter()
                    .map(|row| mailbox_info(&template, row))
                    .collect();
                state.mailbox_counts = rows.iter().map(|row| row.total as usize).collect();
                // The per-mailbox caches are indexed by the same position, so
                // they follow the row count or the next lookup is out of
                // bounds. Nothing is lost: on the startup entry this account
                // has not been opened, and on the resync entry the caches were
                // just dropped anyway.
                state.email_cache = vec![None; rows.len()];
            }
            // Ready clears the #0003 marker; `opening` and `blocked` both
            // leave it, because neither is an account whose counts can be
            // trusted and the TUI has one loading state, not three.
            state.opening = !account.state.is_ready();

            if index == self.active_account {
                // Mirror what a bootstrap owns into the live view, the way
                // `BgResult::AccountOpened` does, rather than reloading the
                // whole account: the cursor, the selection and the scroll
                // positions belong to the frame that is already on screen.
                self.mailboxes = self.accounts[index].mailboxes.clone();
                self.mailbox_counts = self.accounts[index].mailbox_counts.clone();
                self.email_cache = self.accounts[index].email_cache.clone();
                if self.active_mailbox >= self.mailboxes.len() {
                    self.active_mailbox = 0;
                    self.sidebar_index = 0;
                }
            }
        }
    }
}

/// One wire mailbox row as the sidebar's [`MailboxInfo`].
///
/// The icon and the [`MailboxKind`] a renderer branches on come from the role,
/// via the same [`build_mailboxes`](super::build_mailboxes) the daemon derived
/// its seeds from, so the two cannot disagree about what an Inbox looks like.
/// The label and the slug come from the wire, because those are the two things
/// a daemon can know that a client's own config cannot: the label an account
/// gave its Inbox, and the store key its messages are filed under.
///
/// The template is matched by slug first and by kind second: an account with
/// two extra mailboxes has two `Extra` entries that only their slug tells
/// apart, and a role the client does not know yet falls through to a plain
/// extra rather than to a panic.
fn mailbox_info(template: &[MailboxInfo], row: &MailboxRow) -> MailboxInfo {
    let kind = match row.role.as_str() {
        "inbox" => MailboxKind::Inbox,
        "drafts" => MailboxKind::Drafts,
        "sent" => MailboxKind::Sent,
        "archive" => MailboxKind::Archive,
        _ => MailboxKind::Extra,
    };
    let base = template
        .iter()
        .find(|info| info.id == row.slug)
        .or_else(|| template.iter().find(|info| info.kind == kind));
    match base {
        Some(info) => MailboxInfo {
            label: row.label.clone(),
            id: row.slug.clone(),
            icon: info.icon,
            kind,
            server_name: info.server_name.clone(),
        },
        // A mailbox the local config does not describe: the server name is the
        // label, which is what `build_mailboxes` does for a configured extra.
        None => MailboxInfo {
            label: row.label.clone(),
            icon: "\u{f0247}",
            id: row.slug.clone(),
            kind,
            server_name: Some(row.label.clone()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mp_protocol::state::{AccountSnapshot, AccountState as WireAccountState, Snapshot};
    use std::collections::BTreeMap;

    fn row(role: &str, slug: &str, label: &str, total: u64) -> MailboxRow {
        MailboxRow {
            role: role.to_string(),
            slug: slug.to_string(),
            label: label.to_string(),
            total,
            unread: 0,
            badge: 0,
        }
    }

    /// The four rows a configured account always has, with the hand-built
    /// fixture's counts.
    fn four_rows() -> Vec<MailboxRow> {
        vec![
            row("inbox", "inbox", "Inbox", 5),
            row("drafts", "drafts", "Drafts", 2),
            row("sent", "sent", "Sent", 41),
            row("archive", "archive", "Archive", 128),
        ]
    }

    fn bootstrap(name: &str, state: WireAccountState, rows: Vec<MailboxRow>) -> Bootstrap {
        Bootstrap {
            instance_id: "test".to_string(),
            revision: 1,
            capabilities: vec!["state.bootstrap".to_string()],
            snapshot: Snapshot {
                accounts: vec![AccountSnapshot {
                    name: name.to_string(),
                    state,
                    ..Default::default()
                }],
                mailboxes: BTreeMap::from([(name.to_string(), rows)]),
                ..Default::default()
            },
        }
    }

    fn config(name: &str) -> crate::config::GlobalConfig {
        crate::config::GlobalConfig {
            accounts: vec![crate::config::AccountConfig {
                name: name.to_string(),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    /// A ready snapshot gives one account, its rows in the snapshot's order,
    /// its counts, and no loading marker.
    #[test]
    fn a_ready_snapshot_becomes_the_sidebar() {
        let _data = crate::config::test_env::TestDataDir::new();
        let app = App::from_bootstrap(
            config("fixture"),
            &bootstrap("fixture", WireAccountState::Ready, four_rows()),
        );

        assert_eq!(app.accounts.len(), 1);
        assert_eq!(app.active_account, 0);
        assert!(!app.accounts[0].opening, "a ready account is not opening");
        assert_eq!(
            app.mailboxes
                .iter()
                .map(|m| m.label.as_str())
                .collect::<Vec<_>>(),
            vec!["Inbox", "Drafts", "Sent", "Archive"]
        );
        assert_eq!(app.mailbox_counts, vec![5, 2, 41, 128]);
        assert_eq!(app.mailboxes[0].kind, MailboxKind::Inbox);
        assert_eq!(app.mailboxes[3].kind, MailboxKind::Archive);
        assert!(app.emails.is_empty(), "a bootstrap carries no message row");
    }

    /// An `opening` snapshot keeps the #0003 loading state, whatever counts it
    /// happens to carry.
    #[test]
    fn an_opening_snapshot_keeps_the_loading_state() {
        let _data = crate::config::test_env::TestDataDir::new();
        let app = App::from_bootstrap(
            config("fixture"),
            &bootstrap("fixture", WireAccountState::Opening, four_rows()),
        );
        assert!(app.accounts[0].opening);
    }

    /// The label and the slug are the wire's; the icon and the kind are the
    /// role's. An extra mailbox the config does not name still renders.
    #[test]
    fn a_row_takes_its_label_from_the_wire_and_its_icon_from_the_role() {
        let _data = crate::config::test_env::TestDataDir::new();
        let mut rows = four_rows();
        rows[0].label = "Posteingang".to_string();
        rows.push(row("other", "Projekte", "Projekte", 7));
        let app = App::from_bootstrap(
            config("fixture"),
            &bootstrap("fixture", WireAccountState::Ready, rows),
        );

        assert_eq!(app.mailboxes[0].label, "Posteingang");
        assert_eq!(app.mailboxes[0].kind, MailboxKind::Inbox);
        assert_eq!(app.mailboxes[0].icon, "\u{f0172}", "the icon is the role's");
        assert_eq!(app.mailboxes.len(), 5);
        assert_eq!(app.mailboxes[4].kind, MailboxKind::Extra);
        assert_eq!(app.mailboxes[4].id, "Projekte");
        assert_eq!(app.mailbox_counts, vec![5, 2, 41, 128, 7]);
    }

    /// An account the snapshot names and the configuration does not still gets
    /// a row, built on a default config carrying its name.
    #[test]
    fn an_unconfigured_account_still_gets_a_row() {
        let _data = crate::config::test_env::TestDataDir::new();
        let app = App::from_bootstrap(
            config("fixture"),
            &bootstrap("stranger", WireAccountState::Ready, four_rows()),
        );
        assert_eq!(app.accounts.len(), 1);
        assert_eq!(app.accounts[0].account_config.name, "stranger");
        assert_eq!(app.mailbox_counts, vec![5, 2, 41, 128]);
    }

    /// The rule that keeps the running TUI's frames identical: a bootstrap
    /// that lands after the store-backed open of #0003 leaves that account
    /// alone, counts included.
    #[test]
    fn a_bootstrap_does_not_overwrite_an_account_that_already_opened() {
        let _data = crate::config::test_env::TestDataDir::new();
        let mut app = App::from_bootstrap(
            config("fixture"),
            &bootstrap("fixture", WireAccountState::Opening, four_rows()),
        );
        // What `BgResult::AccountOpened` does when the store answers.
        app.accounts[0].opening = false;
        app.accounts[0].mailbox_counts = vec![9, 9, 9, 9];
        app.mailbox_counts = vec![9, 9, 9, 9];

        app.apply_bootstrap(&bootstrap(
            "fixture",
            WireAccountState::Opening,
            vec![row("inbox", "inbox", "Inbox", 0)],
        ));

        assert!(!app.accounts[0].opening, "the marker does not come back");
        assert_eq!(app.mailbox_counts, vec![9, 9, 9, 9]);
        assert_eq!(app.mailboxes.len(), 4, "and the sidebar does not shrink");
    }

    /// A snapshot with no mailbox row for an account leaves that account's
    /// sidebar alone: "I have nothing to say about your mailboxes" is not
    /// "you have none".
    #[test]
    fn an_empty_mailbox_section_leaves_the_sidebar_alone() {
        let _data = crate::config::test_env::TestDataDir::new();
        let mut app = App::from_bootstrap(
            config("fixture"),
            &bootstrap("fixture", WireAccountState::Opening, four_rows()),
        );
        app.apply_bootstrap(&bootstrap("fixture", WireAccountState::Ready, Vec::new()));
        assert_eq!(app.mailboxes.len(), 4);
        assert_eq!(app.mailbox_counts, vec![5, 2, 41, 128]);
        assert!(!app.accounts[0].opening, "readiness still lands");
    }
}
