//! Mail hooks: run a command when a matching message arrives (#0135).
//!
//! An account's `[[accounts.hooks]]` entries each watch one mailbox. After
//! every tick that ran a sync body, the account's hook runner
//! ([`crate::daemon::runtime::hook_runner`]) calls [`scan`], which walks the
//! rows that arrived above each hook's cursor, checks them against the hook's
//! `match` table ([`auth::evaluate`]), claims the ones that pass, and saves the
//! cursor before anything runs. Each claimed message is then materialised into
//! a private directory ([`prepare`]) and handed to the hook's command
//! ([`exec::run`]) as JSON on stdin.
//!
//! # Once per message, and never for old mail
//!
//! - The cursor counts in server UIDs under one UIDVALIDITY, which survive a
//!   store rebuild; row ids do not.
//! - A hook seen for the first time is *armed*: its cursor starts at the
//!   mailbox's current top, so nothing that was already there fires. So is a
//!   hook whose mailbox changed, and a mailbox whose UIDVALIDITY changed, in
//!   which case what arrived during the renumbering is skipped and the log
//!   says so.
//! - A hook removed from the configuration loses its cursor, so adding it back
//!   later arms it afresh rather than firing for everything since.
//! - The cursor never moves past the store's arrival mark, so a message that
//!   arrives out of UID order is still considered.
//! - The Message-IDs a hook fired for are remembered too, so a message moved
//!   out and back in (a new UID) does not fire twice.
//! - The claim is saved before the command starts: a crash in between loses a
//!   run rather than repeating it. `mp hooks replay` is how to re-run one.

pub mod auth;
pub mod exec;
pub mod state;

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{anyhow, Context, Result};
use log::{info, warn};
use rusqlite::OptionalExtension;
use serde_json::{json, Value};

use crate::config::{AccountConfig, HookConfig};
use crate::store::blobs::BlobStore;
use crate::store::read;
use crate::store::Store;

use self::state::{HookCursor, HooksState, LastRun};

/// Serialises every read-modify-write of a hooks state file: a reload can
/// leave the retiring runtime's runner finishing a scan while its
/// replacement's starts one.
static STATE_LOCK: Mutex<()> = Mutex::new(());

/// One message a hook claimed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Claimed {
    pub hook: String,
    pub row_id: i64,
    pub uid: i64,
    pub message_id: String,
}

/// The store key of the mailbox `hook` watches, or `None` when the account
/// does not configure it.
pub fn mailbox_key(account: &AccountConfig, hook: &HookConfig) -> Option<String> {
    crate::config::find_sync_target(account, &hook.mailbox)
        .map(|(role, _)| role.as_str().to_string())
}

/// Consider every row that arrived above each hook's cursor, claim the ones
/// that match, and save the cursors. Returns the claims in UID order.
pub fn scan(
    store: &Store,
    blobs: &BlobStore,
    account: &AccountConfig,
    state_path: &Path,
) -> Result<Vec<Claimed>> {
    let _turn = STATE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut state = state::load(state_path)?;
    let before = state.clone();
    let name = account.name.as_str();

    // A hook that left the configuration forgets its cursor (module docs).
    state.hooks.retain(|hook, _| {
        account
            .hooks
            .iter()
            .any(|configured| &configured.name == hook)
    });

    let mut claimed = Vec::new();
    for hook in &account.hooks {
        let Some(key) = mailbox_key(account, hook) else {
            warn!(
                "[hooks] {name}/{}: mailbox {:?} is not configured for {name}; the hook is idle",
                hook.name, hook.mailbox
            );
            continue;
        };
        let Some(cursor) = crate::ingest::load_mailbox_cursor(store, name, &key)? else {
            // Never synced: there is no top to arm at yet.
            continue;
        };
        let top = cursor
            .last_uid
            .unwrap_or(0)
            .max(max_uid(store, name, &key)?);
        let entry = state.hooks.get_mut(&hook.name);
        let Some(entry) = entry.filter(|entry| entry.mailbox == key) else {
            info!(
                "[hooks] {name}/{}: armed on {key} above uid {top}; earlier mail never fires",
                hook.name
            );
            state.hooks.insert(
                hook.name.clone(),
                armed(&key, cursor.uidvalidity, top, None),
            );
            continue;
        };
        if entry.uidvalidity != cursor.uidvalidity {
            warn!(
                "[hooks] {name}/{}: {key} was renumbered (UIDVALIDITY {:?} to {:?}); re-armed above uid {top}, and mail that arrived during the renumbering does not fire",
                hook.name, entry.uidvalidity, cursor.uidvalidity
            );
            let previous = std::mem::take(entry);
            *entry = armed(&key, cursor.uidvalidity, top, Some(previous));
            continue;
        }
        for (row_id, uid, message_id) in rows_above(store, name, &key, entry.last_uid)? {
            if entry.considered.contains(&uid) {
                continue;
            }
            entry.considered.push(uid);
            let verdict = match load_raw_message(store, blobs, row_id) {
                Ok(raw) => match mailparse::parse_mail(&raw) {
                    Ok(parsed) => auth::evaluate(&hook.criteria, &parsed),
                    Err(e) => {
                        info!(
                            "[hooks] {name}/{}: uid {uid} does not parse ({e}); skipped",
                            hook.name
                        );
                        continue;
                    }
                },
                Err(e) => {
                    info!("[hooks] {name}/{}: uid {uid}: {e:#}; skipped", hook.name);
                    continue;
                }
            };
            if !verdict.matched() {
                let why = verdict
                    .first_failure()
                    .map(|check| format!("{}: {}", check.criterion, check.detail))
                    .unwrap_or_default();
                info!(
                    "[hooks] {name}/{}: {message_id} (uid {uid}) does not match ({why})",
                    hook.name
                );
                continue;
            }
            if entry.has_fired(&message_id) {
                info!(
                    "[hooks] {name}/{}: {message_id} (uid {uid}) matched but already fired; skipped",
                    hook.name
                );
                continue;
            }
            entry.remember_fired(&message_id);
            claimed.push(Claimed {
                hook: hook.name.clone(),
                row_id,
                uid,
                message_id,
            });
        }
        entry.advance(cursor.arrival_mark);
    }

    if state != before {
        state::save(state_path, &state)?;
    }
    Ok(claimed)
}

/// A fresh cursor at `top`, keeping what a previous one remembered.
fn armed(
    key: &str,
    uidvalidity: Option<i64>,
    top: i64,
    previous: Option<HookCursor>,
) -> HookCursor {
    let previous = previous.unwrap_or_default();
    HookCursor {
        mailbox: key.to_string(),
        uidvalidity,
        last_uid: top,
        considered: Vec::new(),
        fired: previous.fired,
        armed_at: chrono::Local::now().to_rfc3339(),
        last_run: previous.last_run,
    }
}

/// The highest UID the store holds in `mailbox`, `0` for none.
fn max_uid(store: &Store, account: &str, mailbox: &str) -> Result<i64> {
    let top: Option<i64> = store
        .conn()
        .query_row(
            "SELECT MAX(uid) FROM messages WHERE account = ?1 AND mailbox = ?2",
            [account, mailbox],
            |row| row.get(0),
        )
        .optional()
        .context("reading the top uid")?
        .flatten();
    Ok(top.unwrap_or(0))
}

/// `(row id, uid, Message-ID)` of every row above `last_uid`, in UID order.
fn rows_above(
    store: &Store,
    account: &str,
    mailbox: &str,
    last_uid: i64,
) -> Result<Vec<(i64, i64, String)>> {
    let mut stmt = store.conn().prepare(
        "SELECT id, uid, message_id FROM messages
         WHERE account = ?1 AND mailbox = ?2 AND uid > ?3 ORDER BY uid",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![account, mailbox, last_uid], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("listing the rows above the hook cursor")?;
    Ok(rows)
}

/// The raw message a row was ingested from.
fn load_raw_message(store: &Store, blobs: &BlobStore, row_id: i64) -> Result<Vec<u8>> {
    read::load_raw(store, blobs, row_id)
        .ok_or_else(|| anyhow!("the raw message is not stored (evicted, or a Graph row)"))
}

/// Drop the cursors of hooks `account` no longer configures, which is what
/// [`scan`] does first; for an account left with no hooks, which gets no scan.
pub fn forget_removed(account: &AccountConfig, state_path: &Path) -> Result<()> {
    let _turn = STATE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut state = state::load(state_path)?;
    let before = state.hooks.len();
    state.hooks.retain(|hook, _| {
        account
            .hooks
            .iter()
            .any(|configured| &configured.name == hook)
    });
    if state.hooks.len() != before {
        info!(
            "[hooks] {}: forgot {} cursor(s) of removed hooks",
            account.name,
            before - state.hooks.len()
        );
        state::save(state_path, &state)?;
    }
    Ok(())
}

/// Record how a run ended, for `mp hooks list`.
pub fn record_run(state_path: &Path, hook: &str, run: LastRun) -> Result<()> {
    let _turn = STATE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut state = state::load(state_path)?;
    if let Some(entry) = state.hooks.get_mut(hook) {
        entry.last_run = Some(run);
        state::save(state_path, &state)?;
    }
    Ok(())
}

/// Read the state file for `mp hooks list`.
pub fn load_state(state_path: &Path) -> Result<HooksState> {
    let _turn = STATE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    state::load(state_path)
}

// ---------------------------------------------------------------------------
// The payload
// ---------------------------------------------------------------------------

/// A message materialised for one run: the directory and what goes on stdin.
#[derive(Debug)]
pub struct Prepared {
    pub dir: PathBuf,
    pub payload: Value,
    pub mailbox: String,
    pub message_id: String,
    pub selector: String,
    pub uid: i64,
}

/// Check `hook` against one row without running anything: the verdict and the
/// payload the command would receive, with no directory behind its paths.
pub fn describe(
    store: &Store,
    blobs: &BlobStore,
    account: &str,
    hook: &HookConfig,
    row: &read::MessageRow,
    fired_before: bool,
) -> Result<Value> {
    let raw = load_raw_message(store, blobs, row.id)?;
    let parsed = mailparse::parse_mail(&raw).context("the stored message does not parse")?;
    let verdict = auth::evaluate(&hook.criteria, &parsed);
    let payload = payload(
        store,
        blobs,
        account,
        hook,
        row,
        &parsed,
        &verdict,
        None,
        &[],
        false,
    );
    Ok(json!({
        "hook": hook.name,
        "account": account,
        "matched": verdict.matched(),
        "fired_before": fired_before,
        "checks": verdict.checks.iter().map(|check| json!({
            "criterion": check.criterion,
            "passed": check.passed,
            "detail": check.detail,
        })).collect::<Vec<_>>(),
        "payload": payload,
    }))
}

/// Materialise `row_id` for a run of `hook`: `message.eml`, `message.md`,
/// `attachments/` and `message.json` in a fresh private directory.
///
/// `require_match` re-checks the `match` table first, which is what keeps
/// `mp hooks replay` behind the same sender gate as a live run.
pub fn prepare(
    store: &Store,
    blobs: &BlobStore,
    account: &str,
    hook: &HookConfig,
    row_id: i64,
    replay: bool,
) -> Result<Prepared> {
    let row = read::find_by_id(store, row_id)?
        .ok_or_else(|| anyhow!("{account} no longer holds message row {row_id}"))?;
    let raw = load_raw_message(store, blobs, row_id)?;
    let parsed = mailparse::parse_mail(&raw).context("the stored message does not parse")?;
    let verdict = auth::evaluate(&hook.criteria, &parsed);
    if !verdict.matched() {
        let why = verdict
            .first_failure()
            .map(|check| format!("{}: {}", check.criterion, check.detail))
            .unwrap_or_default();
        return Err(anyhow!(
            "{} does not match {} ({why})",
            row.message_id,
            hook.name
        ));
    }
    let stem = format!(
        "hook-{account}-{}-{}-{:08x}",
        hook.name,
        row.uid,
        rand::random::<u32>()
    );
    let dir = crate::parse::materialisation_dir(&stem)?;
    std::fs::write(dir.join("message.eml"), &raw).context("writing message.eml")?;
    std::fs::write(
        dir.join("message.md"),
        read::render_markdown(store, blobs, &row),
    )
    .context("writing message.md")?;
    let written = read::materialise_attachments(store, blobs, row_id, &dir.join("attachments"))?;
    let payload = payload(
        store,
        blobs,
        account,
        hook,
        &row,
        &parsed,
        &verdict,
        Some(&dir),
        &written,
        replay,
    );
    std::fs::write(
        dir.join("message.json"),
        serde_json::to_vec_pretty(&payload).context("serialising the payload")?,
    )
    .context("writing message.json")?;
    let selector = payload["selector"].as_str().unwrap_or_default().to_string();
    Ok(Prepared {
        dir,
        mailbox: row.mailbox.clone(),
        message_id: row.message_id.clone(),
        selector,
        uid: row.uid,
        payload,
    })
}

/// The JSON a hook's command reads on stdin.
#[allow(clippy::too_many_arguments)]
fn payload(
    store: &Store,
    blobs: &BlobStore,
    account: &str,
    hook: &HookConfig,
    row: &read::MessageRow,
    parsed: &mailparse::ParsedMail,
    verdict: &auth::Verdict,
    dir: Option<&Path>,
    written: &[PathBuf],
    replay: bool,
) -> Value {
    let attachments = read::attachments_for(store, row.id).unwrap_or_default();
    let path = |name: &str| dir.map(|dir| dir.join(name).display().to_string());
    json!({
        "hook": hook.name,
        "account": account,
        "mailbox": row.mailbox,
        "uid": row.uid,
        "message_id": row.message_id,
        "selector": crate::selector::Selector::for_message(account, row).to_string(),
        "from": row.from,
        "authenticated_sender": verdict.authenticated_sender,
        "to": row.to,
        "cc": row.cc,
        "reply_to": row.reply_to,
        "subject": row.subject,
        "date": row.date_display,
        "headers": parsed.headers.iter().map(|header| json!({
            "name": header.get_key(),
            "value": header.get_value(),
        })).collect::<Vec<_>>(),
        "text": read::load_body(store, blobs, row.id).unwrap_or_default(),
        "dir": dir.map(|dir| dir.display().to_string()),
        "eml_path": path("message.eml"),
        "markdown_path": path("message.md"),
        "attachments": attachments.iter().enumerate().map(|(index, attachment)| json!({
            "name": attachment.name,
            "size": attachment.size,
            "path": written.get(index).map(|path| path.display().to_string()),
        })).collect::<Vec<_>>(),
        "replay": replay,
    })
}

/// Run one prepared message through `hook`'s command, remove its directory,
/// log the outcome and record it.
pub async fn fire(
    account: &str,
    hook: &HookConfig,
    prepared: Prepared,
    state_path: &Path,
    replay: bool,
) -> exec::RunOutcome {
    let stdin = serde_json::to_vec(&prepared.payload).unwrap_or_default();
    let env = exec::HookEnv {
        hook: &hook.name,
        account,
        mailbox: &prepared.mailbox,
        message_id: &prepared.message_id,
        selector: &prepared.selector,
        dir: &prepared.dir,
        replay,
    };
    let timeout = std::time::Duration::from_secs(hook.timeout_secs.max(1));
    let outcome = exec::run(&hook.exec, &stdin, &env, timeout).await;
    let what = format!(
        "[hooks] {account}/{}: {}{} (uid {}): {} in {:.1}s",
        hook.name,
        if replay { "replayed " } else { "" },
        prepared.message_id,
        prepared.uid,
        outcome.outcome,
        outcome.duration.as_secs_f64()
    );
    if outcome.ok {
        info!("{what}");
    } else if outcome.stderr.is_empty() {
        warn!("{what}");
    } else {
        warn!("{what}; stderr: {}", outcome.stderr);
    }
    if let Err(e) = std::fs::remove_dir_all(&prepared.dir) {
        warn!("[hooks] removing {}: {e}", prepared.dir.display());
    }
    let run = LastRun {
        at: chrono::Local::now().to_rfc3339(),
        message_id: prepared.message_id.clone(),
        uid: prepared.uid,
        outcome: outcome.outcome.clone(),
        ok: outcome.ok,
        duration_ms: outcome.duration.as_millis() as u64,
    };
    let path = state_path.to_path_buf();
    let name = hook.name.clone();
    match tokio::task::spawn_blocking(move || record_run(&path, &name, run)).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => warn!(
            "[hooks] recording the run of {account}/{}: {e:#}",
            hook.name
        ),
        Err(e) => warn!("[hooks] the record task of {account}/{} {e}", hook.name),
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{HookMatch, MailboxMapping};
    use crate::ingest::{self, IngestInput, MailboxCursor};

    const ACCOUNT: &str = "assistant";

    struct Fixture {
        _dir: tempfile::TempDir,
        store: Store,
        blobs: BlobStore,
        state: PathBuf,
        account: AccountConfig,
    }

    fn hook(name: &str, exec: Vec<String>) -> HookConfig {
        HookConfig {
            name: name.to_string(),
            mailbox: "INBOX".to_string(),
            criteria: HookMatch {
                authenticated_from: vec!["sylvain@hellin.me".to_string()],
                authserv_id: Some("mx.google.com".to_string()),
                ..HookMatch::default()
            },
            exec,
            timeout_secs: 10,
        }
    }

    fn fixture(hooks: Vec<HookConfig>) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("store.sqlite3")).unwrap();
        let blobs = BlobStore::new(dir.path().join("blobs"));
        let mut account = AccountConfig {
            name: ACCOUNT.to_string(),
            hooks,
            ..Default::default()
        };
        account.imap.host = "imap.gmail.com".to_string();
        account.mailboxes.inbox = Some(MailboxMapping {
            server: "INBOX".to_string(),
        });
        let state = dir.path().join("hooks-state.json");
        Fixture {
            _dir: dir,
            store,
            blobs,
            state,
            account,
        }
    }

    /// A message as Gmail stores it: its own stamp on top, `pass` for the
    /// genuine sender and `fail` for anyone else.
    fn raw(from: &str, id: &str, genuine: bool) -> Vec<u8> {
        let verdict = if genuine {
            "dkim=pass header.i=@hellin.me header.s=protonmail3; spf=pass smtp.mailfrom=sylvain@hellin.me"
        } else {
            "dkim=none; spf=fail smtp.mailfrom=x@evil.example"
        };
        format!(
            "Delivered-To: assistant@gmail.com\r\nAuthentication-Results: mx.google.com; {verdict}\r\nFrom: {from}\r\nTo: assistant@gmail.com\r\nSubject: Task {id}\r\nMessage-ID: <{id}@hellin.me>\r\nDate: Wed, 30 Sep 2026 10:00:00 +0200\r\nContent-Type: text/plain\r\n\r\nPlease reply with PONG.\r\n"
        )
        .into_bytes()
    }

    impl Fixture {
        fn arrive(&self, uid: i64, raw: &[u8]) {
            let email = crate::parse::parse_rfc822_to_fetched_email(raw).unwrap();
            ingest::ingest_message(
                &self.store,
                &self.blobs,
                &IngestInput {
                    account: ACCOUNT,
                    mailbox: "inbox",
                    uid,
                    email: &email,
                    raw: Some(raw),
                },
            )
            .unwrap();
        }

        fn cursor(&self, uidvalidity: i64, last_uid: i64, arrival_mark: Option<i64>) {
            ingest::record_mailbox_cursor(
                &self.store,
                ACCOUNT,
                "inbox",
                &MailboxCursor {
                    uidvalidity: Some(uidvalidity),
                    last_uid: Some(last_uid),
                    arrival_mark,
                    ..MailboxCursor::default()
                },
            )
            .unwrap();
        }

        fn scan(&self) -> Vec<i64> {
            scan(&self.store, &self.blobs, &self.account, &self.state)
                .unwrap()
                .into_iter()
                .map(|claim| claim.uid)
                .collect()
        }
    }

    #[test]
    fn nothing_arms_before_the_first_sync_and_nothing_old_fires() {
        let fx = fixture(vec![hook("pi", vec!["true".into()])]);
        assert!(fx.scan().is_empty());
        assert!(!fx.state.exists(), "no cursor before a sync");

        fx.arrive(1, &raw("sylvain@hellin.me", "old", true));
        fx.cursor(7, 1, None);
        assert!(fx.scan().is_empty(), "arming never fires the backlog");
        let state = state::load(&fx.state).unwrap();
        assert_eq!(state.hooks["pi"].last_uid, 1);

        fx.arrive(2, &raw("sylvain@hellin.me", "new", true));
        fx.cursor(7, 2, None);
        assert_eq!(fx.scan(), vec![2]);
        assert!(fx.scan().is_empty(), "once per message");
    }

    #[test]
    fn a_spoofed_sender_is_considered_and_never_claimed() {
        let fx = fixture(vec![hook("pi", vec!["true".into()])]);
        fx.cursor(7, 0, None);
        assert!(fx.scan().is_empty());
        fx.arrive(1, &raw("sylvain@hellin.me", "spoof", false));
        fx.arrive(2, &raw("stranger@example.com", "stranger", true));
        fx.cursor(7, 2, None);
        assert!(fx.scan().is_empty());
        assert_eq!(state::load(&fx.state).unwrap().hooks["pi"].last_uid, 2);
    }

    #[test]
    fn the_cursor_waits_at_the_arrival_mark_for_a_late_lower_uid() {
        let fx = fixture(vec![hook("pi", vec!["true".into()])]);
        fx.cursor(7, 0, None);
        fx.scan();
        fx.arrive(3, &raw("sylvain@hellin.me", "three", true));
        fx.cursor(7, 3, Some(1));
        assert_eq!(fx.scan(), vec![3]);
        fx.arrive(2, &raw("sylvain@hellin.me", "two", true));
        fx.cursor(7, 3, None);
        assert_eq!(fx.scan(), vec![2], "uid 2 arrived late and still fires");
        assert!(fx.scan().is_empty());
    }

    #[test]
    fn a_message_moved_out_and_back_does_not_fire_twice() {
        let fx = fixture(vec![hook("pi", vec!["true".into()])]);
        fx.cursor(7, 0, None);
        fx.scan();
        fx.arrive(1, &raw("sylvain@hellin.me", "same", true));
        fx.cursor(7, 1, None);
        assert_eq!(fx.scan(), vec![1]);
        fx.arrive(5, &raw("sylvain@hellin.me", "same", true));
        fx.cursor(7, 5, None);
        assert!(fx.scan().is_empty());
    }

    #[test]
    fn a_renumbered_mailbox_rearms_instead_of_firing_everything() {
        let fx = fixture(vec![hook("pi", vec!["true".into()])]);
        fx.cursor(7, 0, None);
        fx.scan();
        fx.arrive(1, &raw("sylvain@hellin.me", "a", true));
        fx.cursor(8, 1, None);
        assert!(fx.scan().is_empty());
        let state = state::load(&fx.state).unwrap();
        assert_eq!(state.hooks["pi"].uidvalidity, Some(8));
    }

    #[test]
    fn a_removed_hook_forgets_its_cursor_and_rearms_when_added_back() {
        let mut fx = fixture(vec![hook("pi", vec!["true".into()])]);
        fx.cursor(7, 0, None);
        fx.scan();
        fx.account.hooks.clear();
        forget_removed(&fx.account, &fx.state).unwrap();
        assert!(state::load(&fx.state).unwrap().hooks.is_empty());
        fx.arrive(1, &raw("sylvain@hellin.me", "while-gone", true));
        fx.cursor(7, 1, None);
        fx.account.hooks.push(hook("pi", vec!["true".into()]));
        assert!(
            fx.scan().is_empty(),
            "no backfill of what arrived while it was gone"
        );
    }

    #[tokio::test]
    async fn a_claimed_message_runs_with_its_payload_and_the_run_is_recorded() {
        let out = tempfile::tempdir().unwrap();
        let sink = out.path().join("stdin.json");
        let exec = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            format!(
                "cat > {} && test -f \"$MP_HOOK_DIR/message.eml\"",
                sink.display()
            ),
        ];
        let fx = fixture(vec![hook("pi", exec)]);
        fx.cursor(7, 0, None);
        fx.scan();
        fx.arrive(1, &raw("Sylvain <sylvain@hellin.me>", "run", true));
        fx.cursor(7, 1, None);
        let claims = scan(&fx.store, &fx.blobs, &fx.account, &fx.state).unwrap();
        assert_eq!(claims.len(), 1);
        let hook = fx.account.hooks[0].clone();
        let prepared = prepare(
            &fx.store,
            &fx.blobs,
            ACCOUNT,
            &hook,
            claims[0].row_id,
            false,
        )
        .unwrap();
        let dir = prepared.dir.clone();
        let outcome = fire(ACCOUNT, &hook, prepared, &fx.state, false).await;
        assert!(outcome.ok, "{outcome:?}");
        assert!(!dir.exists(), "the run directory is removed afterwards");

        let payload: Value = serde_json::from_slice(&std::fs::read(&sink).unwrap()).unwrap();
        assert_eq!(payload["hook"], "pi");
        assert_eq!(payload["authenticated_sender"], "sylvain@hellin.me");
        assert_eq!(payload["subject"], "Task run");
        assert_eq!(payload["message_id"], "<run@hellin.me>");
        assert!(payload["text"].as_str().unwrap().contains("PONG"));
        assert!(payload["headers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h["name"] == "Authentication-Results"));

        let run = state::load(&fx.state).unwrap().hooks["pi"]
            .last_run
            .clone()
            .unwrap();
        assert_eq!(run.outcome, "exit 0");
        assert_eq!(run.message_id, "<run@hellin.me>");
    }

    #[test]
    fn prepare_refuses_a_message_the_match_table_rejects() {
        let fx = fixture(vec![hook("pi", vec!["true".into()])]);
        fx.arrive(1, &raw("sylvain@hellin.me", "spoof", false));
        let row = read::find_row_by_uid(&fx.store, ACCOUNT, "inbox", 1)
            .unwrap()
            .unwrap();
        let err = prepare(
            &fx.store,
            &fx.blobs,
            ACCOUNT,
            &fx.account.hooks[0],
            row,
            true,
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("does not match"), "{err:#}");
    }
}
