use std::collections::HashMap;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use log::{info, warn};
use serde::{Deserialize, Serialize};

use super::fetch::{fetch_new_emails_on_session, fetch_server_message_ids_on_session};
use super::open_imap_session;
use super::ops::update_read_status_locally;
use crate::config::ImapConfig;
use crate::parse::{
    deduplicate_mailbox, save_fetched_emails_with_known_ids, scan_existing_message_ids,
    scan_mailbox_message_ids,
};
use crate::sync::reconcile_local_files;
use crate::timing::TimingSpan;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// In-memory index mapping message_id -> file_path, keyed by local directory.
/// Built once at startup, maintained incrementally during quick sync.
pub type MessageIdIndex = HashMap<PathBuf, HashMap<String, PathBuf>>;

/// Cached IMAP mailbox state from the SELECT response.
/// Used to detect whether reconciliation is needed on the next quick sync.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MailboxState {
    pub uid_validity: Option<u32>,
    pub uid_next: Option<u32>,
    pub exists: u32,
}

impl MailboxState {
    /// Determine whether reconciliation is needed by comparing previous
    /// (self) and current (new) mailbox state.
    ///
    /// Returns `true` when evidence of server-side moves/deletes is detected.
    pub fn needs_reconciliation(&self, new: &MailboxState) -> bool {
        // uid_validity changed -> full reconciliation needed
        if self.uid_validity != new.uid_validity {
            return true;
        }
        // If we don't have uid_next from either side, be conservative
        let (Some(old_next), Some(new_next)) = (self.uid_next, new.uid_next) else {
            return true;
        };
        if new_next > old_next {
            // New messages arrived. Check if `exists` grew by the same delta
            // (pure additions) or less (additions + deletions/moves).
            let uid_delta = new_next - old_next;
            let exists_delta = new.exists as i64 - self.exists as i64;
            if exists_delta < 0 || (exists_delta as u32) < uid_delta {
                return true;
            }
            // Pure additions -- no reconciliation needed
            return false;
        }
        if new_next == old_next && new.exists < self.exists {
            // No new UIDs but fewer messages -> deletions/moves
            return true;
        }
        // uid_next same, exists same or greater -> no change
        false
    }
}

// ---------------------------------------------------------------------------
// Mailbox-state persistence cache
// ---------------------------------------------------------------------------
//
// `MailboxState` carries the last-seen `uid_validity` / `uid_next` / `exists`
// triple per mailbox role. Persisting it across TUI sessions lets the first
// quick sync after launch use state-based reconciliation instead of falling
// through to a full Message-ID scan (~14 s on a busy mailbox -> <2 s).
//
// File lives at `<account_dir>/mailbox-states.json`. Schema is open: extra
// keys are ignored on load, missing fields fall back to `Default` (which
// triggers a full reconcile via `needs_reconciliation`, so we degrade safely
// when the file is corrupt or the schema evolves).

/// Filename of the persisted mailbox-state cache inside an account directory.
pub const MAILBOX_STATES_CACHE_FILENAME: &str = "mailbox-states.json";

/// Resolve the on-disk cache path for the given account directory.
pub fn mailbox_states_cache_path(account_root: &Path) -> PathBuf {
    account_root.join(MAILBOX_STATES_CACHE_FILENAME)
}

/// Load the persisted per-role mailbox states for an account.
///
/// Returns `Ok(HashMap::new())` when the file does not exist (fresh install
/// or never-synced account). Returns the parsed map when the file is
/// readable and valid JSON. Logs and returns an empty map on any I/O or
/// parse error -- a corrupt cache must never block startup, and the worst
/// case is one extra full reconcile.
pub fn load_mailbox_states_cache(
    account_root: &Path,
) -> HashMap<String, MailboxState> {
    let path = mailbox_states_cache_path(account_root);
    if !path.exists() {
        return HashMap::new();
    }
    match std::fs::read_to_string(&path) {
        Ok(data) => match serde_json::from_str::<HashMap<String, MailboxState>>(&data) {
            Ok(map) => {
                info!(
                    "Loaded mailbox states cache from {} ({} entries)",
                    path.display(),
                    map.len(),
                );
                map
            }
            Err(e) => {
                warn!(
                    "Failed to parse mailbox states cache at {}: {}. Ignoring.",
                    path.display(),
                    e,
                );
                HashMap::new()
            }
        },
        Err(e) => {
            warn!(
                "Failed to read mailbox states cache at {}: {}. Ignoring.",
                path.display(),
                e,
            );
            HashMap::new()
        }
    }
}

/// Persist per-role mailbox states for an account.
///
/// Best-effort: callers should log but not fail on error. Creates the
/// parent directory if needed.
pub fn save_mailbox_states_cache(
    account_root: &Path,
    states: &HashMap<String, MailboxState>,
) -> Result<()> {
    let path = mailbox_states_cache_path(account_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating cache directory at {}", parent.display()))?;
    }
    let data = serde_json::to_string_pretty(states)
        .context("serializing mailbox states cache")?;
    std::fs::write(&path, data)
        .with_context(|| format!("writing mailbox states cache at {}", path.display()))?;
    Ok(())
}

/// A mailbox target for the unified sync operation.
pub struct SyncTarget {
    pub role: String,
    pub server_name: String,
    pub local_dir: PathBuf,
    pub status: String,
}

/// One fresh address observation captured from a newly-downloaded email.
/// Consumed by the contacts-index hook after a successful sync.
#[derive(Debug, Clone)]
pub struct FreshObservation {
    /// Mailbox role this email was saved under: "inbox", "archive", "sent", or "extra".
    pub role: String,
    pub from: String,
    pub to: String,
    pub cc: Option<String>,
    /// RFC-2822 date header from the email, or empty if unavailable.
    pub date: String,
}

/// Results from a `sync_mailboxes` operation.
pub struct SyncResult {
    pub saved: usize,
    pub skipped: usize,
    pub moved: usize,
    pub removed: usize,
    pub read_updated: usize,
    pub deduped: usize,
    /// Address observations from newly-saved emails, ready to be merged
    /// into the contacts index by the caller. Empty on `dry_run`.
    pub fresh_observations: Vec<FreshObservation>,
    /// Mailbox states observed during this sync (from SELECT responses).
    /// Keyed by role (e.g. "inbox", "archive").
    pub mailbox_states: HashMap<String, MailboxState>,
    /// Local mailbox directories that were actually modified by this sync
    /// (new emails saved, read flags updated, duplicates removed, or files
    /// moved/removed by reconciliation). Empty when nothing changed on disk,
    /// letting the TUI skip cache invalidation entirely. Always empty on
    /// `dry_run`.
    pub touched_dirs: Vec<PathBuf>,
    /// Sender + subject of every genuinely new email saved to the *inbox*
    /// by this sync (#0009). Drives the TUI's desktop notification; other
    /// mailboxes, read-flag updates, dedup, and reconciliation moves are
    /// deliberately excluded. Always empty on `dry_run`.
    pub new_inbox_mail: Vec<crate::notify::NewMailMeta>,
}

// ---------------------------------------------------------------------------
// sync_mailboxes
// ---------------------------------------------------------------------------

/// Unified sync orchestrator: fetches all mailboxes using a single IMAP connection,
/// with optional reconciliation pass.
///
/// When `known_index` is `Some`, the in-memory message-ID index is used instead
/// of scanning local directories from disk. The index is updated in-place as
/// new emails are saved. Pass `None` for CLI / full-sync (scans from disk).
///
/// When `prev_states` is `Some`, mailbox states from the previous sync are used
/// to decide whether reconciliation is needed. Pass `None` to always reconcile.
///
/// When `dry_run` is true, connects to the server and checks what *would* change
/// but does not save any files or perform any local mutations.
pub async fn sync_mailboxes(
    imap_config: &ImapConfig,
    targets: &[SyncTarget],
    limit: usize,
    reconcile: bool,
    dry_run: bool,
    mut known_index: Option<&mut MessageIdIndex>,
    prev_states: Option<&HashMap<String, MailboxState>>,
) -> Result<SyncResult> {
    info!(
        "sync_mailboxes: {} targets, limit={}, reconcile={}, dry_run={}, has_index={}, has_prev_states={}",
        targets.len(),
        limit,
        reconcile,
        dry_run,
        known_index.is_some(),
        prev_states.is_some(),
    );

    let span_label = if limit < usize::MAX { "sync_mailboxes:quick" } else { "sync_mailboxes:full" };
    let mut span = TimingSpan::with_context(span_label, format!("{} targets", targets.len()));

    let mut session = open_imap_session(imap_config).await?;
    span.mark("session_open");
    let mut total_saved = 0usize;
    let mut total_skipped = 0usize;
    let mut total_read_updated = 0usize;
    let mut fresh_observations: Vec<FreshObservation> = Vec::new();
    let mut new_mailbox_states: HashMap<String, MailboxState> = HashMap::new();
    // Local dirs actually modified on disk by this sync (saves, read-flag
    // updates, dedup, reconciliation moves/removals). Lets the TUI
    // invalidate only the affected mailbox caches.
    let mut touched_dirs: HashSet<PathBuf> = HashSet::new();
    // Sender/subject of new inbox saves, for desktop notifications (#0009).
    let mut new_inbox_mail: Vec<crate::notify::NewMailMeta> = Vec::new();

    // Build the global known_ids set.
    // If we have an in-memory index, derive it from there (zero disk I/O).
    // Otherwise, scan from disk (CLI / full-sync path).
    let mut global_known_ids = HashSet::new();
    if let Some(ref index) = known_index {
        for dir_map in index.values() {
            for mid in dir_map.keys() {
                global_known_ids.insert(mid.clone());
            }
        }
    } else {
        for target in targets {
            if let Ok(ids) = scan_existing_message_ids(&target.local_dir) {
                global_known_ids.extend(ids);
            }
        }
    }
    span.mark("build_known_ids");

    // Phase 1: Additive sync with two-pass fetch
    for target in targets {
        if dry_run {
            match count_new_emails_on_session(
                &mut session,
                &target.server_name,
                Some(limit),
                &global_known_ids,
            )
            .await
            {
                Ok((new_count, skipped)) => {
                    total_skipped += skipped;
                    total_saved += new_count;
                }
                Err(e) => {
                    warn!(
                        "Failed to check mailbox '{}': {}. Continuing with next.",
                        target.server_name, e
                    );
                }
            }
        } else {
            // Snapshot-staleness cutoff for read-flag sync: any local file
            // modified at or after this instant may carry a user change made
            // *while* this sync was reading the server, so its \Seen state
            // must not be overwritten with the (older) server snapshot.
            // Captured before the server fetch so the guard is conservative.
            let flags_cutoff = std::time::SystemTime::now();
            match fetch_new_emails_on_session(
                &mut session,
                &target.server_name,
                Some(limit),
                &global_known_ids,
            )
            .await
            {
                Ok((new_emails, skipped, known_read_flags, mb_state)) => {
                    // Capture mailbox state from SELECT for reconciliation decisions
                    if let Some(state) = mb_state {
                        new_mailbox_states.insert(target.role.clone(), state);
                    }

                    total_skipped += skipped;
                    if !new_emails.is_empty() {
                        let (saved, _dup, saved_paths) = save_fetched_emails_with_known_ids(
                            &new_emails,
                            &target.local_dir,
                            &target.status,
                            &mut global_known_ids,
                        )?;
                        total_saved += saved;
                        if saved > 0 {
                            touched_dirs.insert(target.local_dir.clone());
                        }

                        // Record sender/subject of new *inbox* saves for the
                        // desktop notification (#0009). Matched against the
                        // actually-written message IDs so duplicates the save
                        // skipped don't produce phantom notifications.
                        if saved > 0 && target.role.eq_ignore_ascii_case("inbox") {
                            new_inbox_mail.extend(crate::notify::collect_new_mail_meta(
                                &new_emails,
                                &saved_paths,
                            ));
                        }

                        // Update in-memory index with newly saved emails using the
                        // paths returned by the save (no directory re-scan needed).
                        if let Some(ref mut index) = known_index {
                            let dir_map = index.entry(target.local_dir.clone()).or_default();
                            for (mid, path) in saved_paths {
                                if let Some(mid) = mid {
                                    dir_map.insert(mid, path);
                                }
                            }
                        }

                        for e in &new_emails {
                            fresh_observations.push(FreshObservation {
                                role: target.role.clone(),
                                from: e.from.clone(),
                                to: e.to.clone(),
                                cc: e.cc.clone(),
                                date: e.date.clone(),
                            });
                        }
                    }

                    // Sync read status from server for existing local emails.
                    // Use the in-memory index if available to avoid re-scanning.
                    if !known_read_flags.is_empty() {
                        let updated = if let Some(ref index) = known_index {
                            index
                                .get(&target.local_dir)
                                .map(|dir_map| {
                                    sync_local_read_flags_with_index(
                                        dir_map,
                                        &known_read_flags,
                                        Some(flags_cutoff),
                                    )
                                })
                                .unwrap_or(0)
                        } else {
                            sync_local_read_flags(
                                &target.local_dir,
                                &known_read_flags,
                                Some(flags_cutoff),
                            )
                        };
                        total_read_updated += updated;
                        if updated > 0 {
                            touched_dirs.insert(target.local_dir.clone());
                        }
                    }
                }
                Err(e) => {
                    warn!(
                        "Failed to sync mailbox '{}': {}. Continuing with next.",
                        target.server_name, e
                    );
                }
            }
        }
    }

    span.mark("additive_fetch");

    // Dedup pass: skip when using in-memory index (index prevents duplicates).
    // Only run on full-sync / CLI path.
    let mut total_deduped = 0usize;
    if !dry_run && known_index.is_none() {
        let mut deduped_dirs = HashSet::new();
        for target in targets {
            if deduped_dirs.insert(target.local_dir.clone()) {
                match deduplicate_mailbox(&target.local_dir) {
                    Ok(n) if n > 0 => {
                        info!("Deduplicated {} file(s) in {}", n, target.local_dir.display());
                        total_deduped += n;
                        touched_dirs.insert(target.local_dir.clone());
                    }
                    Err(e) => warn!("Dedup failed for {}: {}", target.local_dir.display(), e),
                    _ => {}
                }
            }
        }
    }

    if total_deduped > 0 {
        span.mark("dedup");
    }

    let mut total_moved = 0usize;
    let mut total_removed = 0usize;

    // Phase 2: Reconciliation (only INBOX and Archive)
    if reconcile && !dry_run {
        let mut reconcile_pairs: Vec<(String, String)> = Vec::new();
        let mut local_dirs: HashMap<String, PathBuf> = HashMap::new();

        for target in targets {
            let role = target.role.to_ascii_lowercase();
            if role == "inbox" || role == "archive" {
                let key = if role == "inbox" {
                    "INBOX".to_string()
                } else {
                    "Archive".to_string()
                };

                // Check whether reconciliation is actually needed using
                // IMAP mailbox state comparison (avoids fetching all
                // server Message-IDs when nothing was moved/deleted).
                let should_reconcile = match prev_states {
                    Some(prev) => {
                        if let (Some(prev_state), Some(new_state)) =
                            (prev.get(&role), new_mailbox_states.get(&role))
                        {
                            let needed = prev_state.needs_reconciliation(new_state);
                            if !needed {
                                info!(
                                    "Reconciliation skipped for '{}': state unchanged (exists={}, uid_next={:?})",
                                    role, new_state.exists, new_state.uid_next
                                );
                            }
                            needed
                        } else {
                            true // No previous state or missing new state -- be conservative
                        }
                    }
                    None => true, // No previous states provided -- always reconcile
                };

                if should_reconcile {
                    reconcile_pairs.push((key.clone(), target.server_name.clone()));
                    local_dirs.insert(key, target.local_dir.clone());
                }
            }
        }

        if !reconcile_pairs.is_empty() {
            let mut server_ids: HashMap<String, HashSet<String>> = HashMap::new();
            for (key, imap_mailbox) in &reconcile_pairs {
                match fetch_server_message_ids_on_session(&mut session, imap_mailbox).await {
                    Ok(ids) => {
                        server_ids.insert(key.clone(), ids);
                    }
                    Err(e) => {
                        warn!(
                            "Failed to fetch Message-IDs for reconciliation from '{}': {}",
                            imap_mailbox, e
                        );
                    }
                }
            }

            if !server_ids.is_empty() {
                let (moved, removed) = reconcile_local_files(&server_ids, &local_dirs)?;
                total_moved = moved;
                total_removed = removed;

                if moved > 0 || removed > 0 {
                    // Reconciliation only ever mutates the dirs in `local_dirs`
                    // (at most INBOX + Archive). Moves touch both source and
                    // destination, so mark them all.
                    for dir in local_dirs.values() {
                        touched_dirs.insert(dir.clone());
                    }
                }

                // Update in-memory index to reflect reconciliation changes
                if let Some(ref mut index) = known_index {
                    for (_key, dir) in &local_dirs {
                        // Re-scan only the reconciled directories (at most 2)
                        if let Ok(ids) = scan_mailbox_message_ids(dir) {
                            index.insert(dir.clone(), ids);
                        }
                    }
                }
            }
        }
    } else if reconcile && dry_run {
        // Dry-run reconciliation: always run (no state-based skip for dry run)
        let mut reconcile_pairs: Vec<(String, String)> = Vec::new();
        let mut local_dirs: HashMap<String, PathBuf> = HashMap::new();

        for target in targets {
            let role = target.role.to_ascii_lowercase();
            if role == "inbox" || role == "archive" {
                let key = if role == "inbox" {
                    "INBOX".to_string()
                } else {
                    "Archive".to_string()
                };
                reconcile_pairs.push((key.clone(), target.server_name.clone()));
                local_dirs.insert(key, target.local_dir.clone());
            }
        }

        if !reconcile_pairs.is_empty() {
            let mut server_ids: HashMap<String, HashSet<String>> = HashMap::new();
            for (key, imap_mailbox) in &reconcile_pairs {
                match fetch_server_message_ids_on_session(&mut session, imap_mailbox).await {
                    Ok(ids) => {
                        server_ids.insert(key.clone(), ids);
                    }
                    Err(e) => {
                        warn!(
                            "Failed to fetch Message-IDs for reconciliation from '{}': {}",
                            imap_mailbox, e
                        );
                    }
                }
            }
            if !server_ids.is_empty() {
                let (moved, removed) = crate::sync::dry_run_reconcile(&server_ids, &local_dirs)?;
                total_moved = moved;
                total_removed = removed;
            }
        }
    }

    span.mark("reconcile");

    session.logout().await.ok();
    span.mark("logout");

    Ok(SyncResult {
        saved: total_saved,
        skipped: total_skipped,
        moved: total_moved,
        removed: total_removed,
        read_updated: total_read_updated,
        deduped: total_deduped,
        fresh_observations,
        mailbox_states: new_mailbox_states,
        touched_dirs: touched_dirs.into_iter().collect(),
        new_inbox_mail,
    })
}

// ---------------------------------------------------------------------------
// Read-flag sync helpers
// ---------------------------------------------------------------------------

/// Slack subtracted from the snapshot cutoff to tolerate coarse filesystem
/// mtime granularity (some filesystems store whole seconds). Erring toward
/// skipping is safe: a skipped file just converges on the next sync.
const READ_FLAG_CUTOFF_SLACK: std::time::Duration = std::time::Duration::from_secs(1);

/// Whether a local file was modified at-or-after the sync's server-snapshot
/// cutoff, meaning the server flags in hand may be older than the local
/// state and must not overwrite it (ticket #0004). Unreadable mtimes count
/// as "recently modified" (skip -- conservative).
fn modified_since_cutoff(path: &std::path::Path, cutoff: std::time::SystemTime) -> bool {
    let effective = cutoff
        .checked_sub(READ_FLAG_CUTOFF_SLACK)
        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
    match std::fs::metadata(path).and_then(|m| m.modified()) {
        Ok(mtime) => mtime >= effective,
        Err(_) => true,
    }
}

/// Update local emails' `read:` frontmatter to match server \Seen flags.
/// Scans the directory from disk to find files. Returns the number of files updated.
///
/// `snapshot_cutoff` is the instant the server flags were captured (i.e. just
/// before the fetch). Files modified at-or-after it are skipped: their local
/// state is newer than the server snapshot (e.g. the user marked an email
/// read while the sync was in flight), and blindly applying the snapshot
/// would revert that change. This is a staleness guard, not a 3-way merge.
pub fn sync_local_read_flags(
    local_dir: &std::path::Path,
    server_flags: &HashMap<String, bool>,
    snapshot_cutoff: Option<std::time::SystemTime>,
) -> usize {
    let local_ids = match scan_mailbox_message_ids(local_dir) {
        Ok(ids) => ids,
        Err(_) => return 0,
    };
    sync_local_read_flags_with_index(&local_ids, server_flags, snapshot_cutoff)
}

/// Update local emails' `read:` frontmatter using a pre-built index map.
/// Avoids re-scanning the directory. Returns the number of files updated.
/// See [`sync_local_read_flags`] for the `snapshot_cutoff` semantics.
pub(crate) fn sync_local_read_flags_with_index(
    local_ids: &HashMap<String, PathBuf>,
    server_flags: &HashMap<String, bool>,
    snapshot_cutoff: Option<std::time::SystemTime>,
) -> usize {
    let mut updated = 0usize;
    for (message_id, is_seen) in server_flags {
        if let Some(file_path) = local_ids.get(message_id) {
            let local_read = read_local_read_status(file_path);
            if local_read == *is_seen {
                continue;
            }
            if let Some(cutoff) = snapshot_cutoff {
                if modified_since_cutoff(file_path, cutoff) {
                    info!(
                        "Read-flag sync: skipping {} (modified after sync started; local state wins)",
                        file_path.display()
                    );
                    continue;
                }
            }
            if update_read_status_locally(file_path, *is_seen).is_ok() {
                updated += 1;
            }
        }
    }
    updated
}

/// Read the `read:` field from a frontmatter file. Returns false if missing or unreadable.
fn read_local_read_status(path: &std::path::Path) -> bool {
    if let Ok(content) = std::fs::read_to_string(path) {
        let mut in_frontmatter = false;
        for line in content.lines() {
            if line == "---" {
                if !in_frontmatter {
                    in_frontmatter = true;
                    continue;
                } else {
                    break;
                }
            }
            if in_frontmatter && line.starts_with("read:") {
                let val = line.trim_start_matches("read:").trim();
                return val == "true";
            }
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Dry-run helper
// ---------------------------------------------------------------------------

/// Lightweight pass-1-only check: counts how many new emails exist on the server
/// without downloading full message bodies. Used for dry-run mode.
async fn count_new_emails_on_session(
    session: &mut super::ImapSession,
    mailbox: &str,
    limit: Option<usize>,
    known_ids: &HashSet<String>,
) -> Result<(usize, usize)> {
    use crate::parse::compress_uid_set;
    use anyhow::anyhow;
    use futures::TryStreamExt;

    session
        .select(mailbox)
        .await
        .map_err(|e| anyhow!("Failed to select mailbox '{}': {}", mailbox, e))?;

    let uids = session
        .uid_search("ALL")
        .await
        .map_err(|e| anyhow!("IMAP search failed: {}", e))?;

    if uids.is_empty() {
        return Ok((0, 0));
    }

    let mut uid_list: Vec<u32> = uids.into_iter().collect();
    uid_list.sort();
    let selected_uids: Vec<u32> = match limit {
        Some(n) => uid_list.into_iter().rev().take(n).collect(),
        None => uid_list,
    };

    if selected_uids.is_empty() {
        return Ok((0, 0));
    }

    let checked_count = selected_uids.len();
    let uid_set = compress_uid_set(&selected_uids);

    let header_fetched: Vec<_> = session
        .uid_fetch(&uid_set, "(UID BODY.PEEK[HEADER.FIELDS (Message-ID)])")
        .await
        .map_err(|e| anyhow!("Failed to fetch Message-ID headers: {}", e))?
        .try_collect()
        .await
        .map_err(|e| anyhow!("Failed to collect Message-ID headers: {}", e))?;

    let mut new_count = 0usize;
    for msg in header_fetched.iter() {
        let mid = msg
            .header()
            .and_then(super::fetch::parse_message_id_from_header_bytes);
        match mid {
            Some(ref id) if known_ids.contains(id) => {}
            _ => new_count += 1,
        }
    }

    let skipped = checked_count - new_count;
    Ok((new_count, skipped))
}

// ---------------------------------------------------------------------------
// list_mailboxes
// ---------------------------------------------------------------------------

pub async fn list_mailboxes(imap_config: &ImapConfig) -> Result<Vec<String>> {
    use anyhow::anyhow;
    use futures::TryStreamExt;

    let mut session = open_imap_session(imap_config).await?;

    let mailboxes: Vec<_> = session
        .list(None, Some("*"))
        .await
        .map_err(|e| anyhow!("Failed to list mailboxes: {}", e))?
        .try_collect()
        .await
        .map_err(|e| anyhow!("Failed to collect mailboxes: {}", e))?;

    let names: Vec<String> = mailboxes.iter().map(|m| m.name().to_string()).collect();

    session.logout().await.ok();
    Ok(names)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    // -----------------------------------------------------------------------
    // MailboxState::needs_reconciliation
    // -----------------------------------------------------------------------

    #[test]
    fn test_no_previous_state_needs_reconciliation() {
        let prev = MailboxState::default(); // uid_validity None
        let new = MailboxState {
            uid_validity: Some(1),
            uid_next: Some(100),
            exists: 50,
        };
        assert!(prev.needs_reconciliation(&new));
    }

    #[test]
    fn test_no_change_skips_reconciliation() {
        let prev = MailboxState {
            uid_validity: Some(1),
            uid_next: Some(100),
            exists: 50,
        };
        let new = prev.clone();
        assert!(!prev.needs_reconciliation(&new));
    }

    #[test]
    fn test_pure_addition_skips_reconciliation() {
        let prev = MailboxState {
            uid_validity: Some(1),
            uid_next: Some(100),
            exists: 50,
        };
        let new = MailboxState {
            uid_validity: Some(1),
            uid_next: Some(103),
            exists: 53, // grew by same delta (3)
        };
        assert!(!prev.needs_reconciliation(&new));
    }

    #[test]
    fn test_addition_plus_deletion_needs_reconciliation() {
        let prev = MailboxState {
            uid_validity: Some(1),
            uid_next: Some(100),
            exists: 50,
        };
        let new = MailboxState {
            uid_validity: Some(1),
            uid_next: Some(103), // 3 new UIDs
            exists: 51,          // only grew by 1 -> 2 were deleted/moved
        };
        assert!(prev.needs_reconciliation(&new));
    }

    #[test]
    fn test_exists_decreased_needs_reconciliation() {
        let prev = MailboxState {
            uid_validity: Some(1),
            uid_next: Some(100),
            exists: 50,
        };
        let new = MailboxState {
            uid_validity: Some(1),
            uid_next: Some(100),
            exists: 48,
        };
        assert!(prev.needs_reconciliation(&new));
    }

    // -----------------------------------------------------------------------
    // mailbox-states cache (load / save round-trip)
    // -----------------------------------------------------------------------

    #[test]
    fn mailbox_states_cache_path_inside_account_dir() {
        let dir = tempdir().unwrap();
        let p = mailbox_states_cache_path(dir.path());
        assert_eq!(p, dir.path().join("mailbox-states.json"));
    }

    #[test]
    fn mailbox_states_cache_load_missing_returns_empty() {
        let dir = tempdir().unwrap();
        let states = load_mailbox_states_cache(dir.path());
        assert!(states.is_empty());
    }

    #[test]
    fn mailbox_states_cache_round_trip() {
        let dir = tempdir().unwrap();
        let mut states: HashMap<String, MailboxState> = HashMap::new();
        states.insert(
            "inbox".to_string(),
            MailboxState { uid_validity: Some(42), uid_next: Some(1234), exists: 56 },
        );
        states.insert(
            "archive".to_string(),
            MailboxState { uid_validity: Some(7), uid_next: Some(99), exists: 12 },
        );

        save_mailbox_states_cache(dir.path(), &states).unwrap();
        let loaded = load_mailbox_states_cache(dir.path());

        assert_eq!(loaded.len(), 2);
        let inbox = loaded.get("inbox").unwrap();
        assert_eq!(inbox.uid_validity, Some(42));
        assert_eq!(inbox.uid_next, Some(1234));
        assert_eq!(inbox.exists, 56);
        let archive = loaded.get("archive").unwrap();
        assert_eq!(archive.uid_validity, Some(7));
        assert_eq!(archive.uid_next, Some(99));
        assert_eq!(archive.exists, 12);
    }

    #[test]
    fn mailbox_states_cache_save_creates_parent_dir() {
        let dir = tempdir().unwrap();
        let nested = dir.path().join("accounts").join("alice");
        let mut states: HashMap<String, MailboxState> = HashMap::new();
        states.insert(
            "inbox".to_string(),
            MailboxState { uid_validity: Some(1), uid_next: Some(2), exists: 3 },
        );
        save_mailbox_states_cache(&nested, &states).unwrap();
        assert!(nested.join("mailbox-states.json").exists());
    }

    #[test]
    fn mailbox_states_cache_corrupt_file_returns_empty() {
        let dir = tempdir().unwrap();
        let path = mailbox_states_cache_path(dir.path());
        fs::write(&path, "{ this is not valid JSON").unwrap();
        let states = load_mailbox_states_cache(dir.path());
        assert!(states.is_empty(), "corrupt cache must degrade to empty, not panic");
    }

    #[test]
    fn mailbox_states_cache_drives_reconcile_skip() {
        // Smoke test: a persisted state matched against an unchanged new
        // state must report "no reconciliation needed". This is the whole
        // point of persistence -- prove the round-tripped value still
        // takes the fast branch.
        let dir = tempdir().unwrap();
        let mut states: HashMap<String, MailboxState> = HashMap::new();
        states.insert(
            "inbox".to_string(),
            MailboxState { uid_validity: Some(1), uid_next: Some(100), exists: 50 },
        );
        save_mailbox_states_cache(dir.path(), &states).unwrap();

        let loaded = load_mailbox_states_cache(dir.path());
        let prev = loaded.get("inbox").unwrap();
        let new = MailboxState { uid_validity: Some(1), uid_next: Some(100), exists: 50 };
        assert!(!prev.needs_reconciliation(&new));
    }

    #[test]
    fn test_uid_validity_changed_needs_reconciliation() {
        let prev = MailboxState {
            uid_validity: Some(1),
            uid_next: Some(100),
            exists: 50,
        };
        let new = MailboxState {
            uid_validity: Some(2), // renumbered
            uid_next: Some(100),
            exists: 50,
        };
        assert!(prev.needs_reconciliation(&new));
    }

    // -----------------------------------------------------------------------
    // read_local_read_status
    // -----------------------------------------------------------------------

    #[test]
    fn test_read_local_read_status_true() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("email.md");
        fs::write(
            &path,
            "---\nfrom: a@x.com\nread: true\nstatus: inbox\n---\n\nBody",
        )
        .unwrap();
        assert!(read_local_read_status(&path));
    }

    #[test]
    fn test_read_local_read_status_false() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("email.md");
        fs::write(
            &path,
            "---\nfrom: a@x.com\nread: false\nstatus: inbox\n---\n\nBody",
        )
        .unwrap();
        assert!(!read_local_read_status(&path));
    }

    #[test]
    fn test_read_local_read_status_missing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("email.md");
        fs::write(&path, "---\nfrom: a@x.com\nstatus: inbox\n---\n\nBody").unwrap();
        assert!(!read_local_read_status(&path));
    }

    // -----------------------------------------------------------------------
    // sync_local_read_flags
    // -----------------------------------------------------------------------

    #[test]
    fn test_sync_local_read_flags_updates_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("email.md");
        fs::write(
            &path,
            "---\nfrom: a@x.com\nmessage_id: \"<msg1@x.com>\"\nread: false\nstatus: inbox\n---\n\nBody",
        ).unwrap();

        let mut flags = HashMap::new();
        flags.insert("<msg1@x.com>".to_string(), true);

        let updated = sync_local_read_flags(dir.path(), &flags, None);
        assert_eq!(updated, 1);

        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("read: true"));
    }

    #[test]
    fn test_sync_local_read_flags_no_change_needed() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("email.md");
        fs::write(
            &path,
            "---\nfrom: a@x.com\nmessage_id: \"<msg1@x.com>\"\nread: true\nstatus: inbox\n---\n\nBody",
        ).unwrap();

        let mut flags = HashMap::new();
        flags.insert("<msg1@x.com>".to_string(), true);

        let updated = sync_local_read_flags(dir.path(), &flags, None);
        assert_eq!(updated, 0);
    }

    #[test]
    fn test_sync_local_read_flags_marks_unread() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("email.md");
        fs::write(
            &path,
            "---\nfrom: a@x.com\nmessage_id: \"<msg1@x.com>\"\nread: true\nstatus: inbox\n---\n\nBody",
        ).unwrap();

        let mut flags = HashMap::new();
        flags.insert("<msg1@x.com>".to_string(), false);

        let updated = sync_local_read_flags(dir.path(), &flags, None);
        assert_eq!(updated, 1);

        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("read: false"));
    }

    // -----------------------------------------------------------------------
    // sync_local_read_flags_with_index
    // -----------------------------------------------------------------------

    #[test]
    fn test_sync_local_read_flags_with_index() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("email.md");
        fs::write(
            &path,
            "---\nfrom: a@x.com\nmessage_id: \"<msg1@x.com>\"\nread: false\nstatus: inbox\n---\n\nBody",
        ).unwrap();

        // Build an index map manually
        let mut index = HashMap::new();
        index.insert("<msg1@x.com>".to_string(), path.clone());

        let mut flags = HashMap::new();
        flags.insert("<msg1@x.com>".to_string(), true);

        let updated = sync_local_read_flags_with_index(&index, &flags, None);
        assert_eq!(updated, 1);

        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("read: true"));
    }

    // -----------------------------------------------------------------------
    // snapshot-cutoff guard (ticket #0004): a fetch must not clobber a local
    // read-status change made while the sync was in flight
    // -----------------------------------------------------------------------

    #[test]
    fn test_sync_local_read_flags_skips_file_modified_after_cutoff() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("email.md");

        // Cutoff BEFORE the local change: simulates the sync capturing the
        // server snapshot, then the user marking the email read locally
        // while the fetch was still in flight.
        let cutoff = std::time::SystemTime::now() - std::time::Duration::from_secs(30);
        fs::write(
            &path,
            "---\nfrom: a@x.com\nmessage_id: \"<msg1@x.com>\"\nread: true\nstatus: inbox\n---\n\nBody",
        ).unwrap();

        // Server snapshot still says unread -- must NOT revert the local mark.
        let mut flags = HashMap::new();
        flags.insert("<msg1@x.com>".to_string(), false);

        let updated = sync_local_read_flags(dir.path(), &flags, Some(cutoff));
        assert_eq!(updated, 0, "stale server snapshot must not clobber newer local state");
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("read: true"), "local read mark must survive the fetch");
    }

    #[test]
    fn test_sync_local_read_flags_applies_when_file_older_than_cutoff() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("email.md");
        fs::write(
            &path,
            "---\nfrom: a@x.com\nmessage_id: \"<msg1@x.com>\"\nread: false\nstatus: inbox\n---\n\nBody",
        ).unwrap();

        // Backdate the file well past the 1s mtime-granularity slack, then
        // use a cutoff of "now": the file predates the sync, so the server
        // flag must be applied.
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(old)
            .unwrap();

        let mut flags = HashMap::new();
        flags.insert("<msg1@x.com>".to_string(), true);

        let updated = sync_local_read_flags(dir.path(), &flags, Some(std::time::SystemTime::now()));
        assert_eq!(updated, 1, "server->local propagation must still work for untouched files");
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("read: true"));
    }

    #[test]
    fn test_sync_local_read_flags_with_index_respects_cutoff() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("email.md");
        let cutoff = std::time::SystemTime::now() - std::time::Duration::from_secs(30);
        fs::write(
            &path,
            "---\nfrom: a@x.com\nmessage_id: \"<msg1@x.com>\"\nread: true\nstatus: inbox\n---\n\nBody",
        ).unwrap();

        let mut index = HashMap::new();
        index.insert("<msg1@x.com>".to_string(), path.clone());
        let mut flags = HashMap::new();
        flags.insert("<msg1@x.com>".to_string(), false);

        let updated = sync_local_read_flags_with_index(&index, &flags, Some(cutoff));
        assert_eq!(updated, 0);
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("read: true"));
    }
}
