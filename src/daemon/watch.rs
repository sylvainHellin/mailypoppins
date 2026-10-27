//! The draft and signature watcher: the TUI's one-second fingerprint poll,
//! moved into the daemon and debounced (P3b-U10).
//!
//! One [`DraftWatcher`] stats every `<account_dir>/drafts/*.md` and every
//! `config_dir()/signatures/*.md` once per [`WatchConfig::poll_interval`],
//! reads no contents while doing so, and turns what moved into
//! [`WatchEvent`]s. No `notify` dependency, which is the same deliberate
//! deferral `src/tui/mod.rs` makes, and no engine lock: the watcher opens no
//! store, so it runs whether or not this daemon starts account runtimes.
//!
//! # The debounce, which is the whole of the algorithm
//!
//! One observation is `(modified, len)` at full precision, deliberately finer
//! than [`crate::store::drafts::fingerprint`], which folds mtime to whole
//! seconds: a watcher that reparsed on the second would miss the second save of
//! a burst.
//!
//! A file whose observation differs from its settled one becomes *pending* and
//! records the moment; a pending file that moves again re-records it; a pending
//! file that has held still for [`WatchConfig::debounce`] **settles** and
//! yields exactly one event carrying the state it is in at that moment. So an
//! editor's save dance - write a temporary, rename over the target, or rename
//! the original away and create a new file at the same path - is one event
//! rather than a storm or a flicker, and a burst of saves is one reparse of the
//! final state rather than four of the first.
//!
//! Absence is debounced the same way, and a settled absent file yields
//! [`WatchEvent::DraftRemoved`] only if it had settled as present before: a
//! client that was never told about the row must not be told to drop it.
//!
//! # What it never does
//!
//! It never opens a draft for writing. Not to mint an `id:`, not to normalise,
//! not to restore something it saw disappear: minting inside a watcher would
//! write to a file an editor is holding open, which is the exact burst the
//! debounce exists to survive. That is why the id falls back to the file stem,
//! and why id minting stays on the explicit index refresh.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use log::debug;
use mp_protocol::events::{Diagnostic, KIND_SIGNATURE_CHANGED, KIND_SIGNATURE_REMOVED};
use mp_protocol::state::{DraftServer, DraftSyncState};
use serde_json::json;

use super::state::events::Event;
use super::state::{CanonicalState, Change};

/// The poll the TUI already runs, moved: one second.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(1000);

/// Longer than an editor's save dance, shorter than a person notices.
pub const DEFAULT_DEBOUNCE: Duration = Duration::from_millis(300);

/// Test hook: the poll interval in milliseconds.
pub const WATCH_POLL_ENV: &str = "MAILYPOPPINS_DAEMON_WATCH_POLL_MS";

/// Test hook: the debounce in milliseconds.
pub const WATCH_DEBOUNCE_ENV: &str = "MAILYPOPPINS_DAEMON_WATCH_DEBOUNCE_MS";

/// How often the watcher looks, and how long a file must hold still.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WatchConfig {
    /// Between two polls.
    pub poll_interval: Duration,
    /// How long an observation must hold still before it settles.
    pub debounce: Duration,
}

impl Default for WatchConfig {
    fn default() -> Self {
        WatchConfig {
            poll_interval: DEFAULT_POLL_INTERVAL,
            debounce: DEFAULT_DEBOUNCE,
        }
    }
}

impl WatchConfig {
    /// The configuration the two hooks ask for, defaulting on anything that is
    /// unset, unparseable or zero, as
    /// [`FAKE_READY_ENV`](super::state::FAKE_READY_ENV) does.
    pub fn from_env() -> WatchConfig {
        let default = WatchConfig::default();
        WatchConfig {
            poll_interval: millis(WATCH_POLL_ENV).unwrap_or(default.poll_interval),
            debounce: millis(WATCH_DEBOUNCE_ENV).unwrap_or(default.debounce),
        }
    }
}

/// A positive millisecond count from `name`, or `None`.
fn millis(name: &str) -> Option<Duration> {
    let value = std::env::var(name).ok()?.trim().parse::<u64>().ok()?;
    (value > 0).then(|| Duration::from_millis(value))
}

/// Which directories one watcher looks at.
///
/// A root need not exist: an account that has never had a draft is not an
/// error, and the directory is picked up on the poll after it appears.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WatchRoots {
    /// One `(account, <account_dir>/drafts)` per configured account, in
    /// `config.toml`'s order.
    pub drafts: Vec<(String, PathBuf)>,
    /// `<config_dir>/signatures`, or `None` for a watcher that never looks.
    pub signatures: Option<PathBuf>,
}

impl WatchRoots {
    /// A watcher over nothing.
    pub fn new() -> WatchRoots {
        WatchRoots::default()
    }

    /// The same roots plus one account's drafts directory.
    pub fn with_account(mut self, account: impl Into<String>, dir: impl Into<PathBuf>) -> Self {
        self.drafts.push((account.into(), dir.into()));
        self
    }

    /// The same roots plus the signatures directory.
    pub fn with_signatures(mut self, dir: impl Into<PathBuf>) -> Self {
        self.signatures = Some(dir.into());
        self
    }

    /// The roots as the watcher walks them: drafts in declaration order, the
    /// signatures last.
    fn walk(&self) -> Vec<Root> {
        let mut roots: Vec<Root> = self
            .drafts
            .iter()
            .map(|(account, dir)| Root {
                account: Some(account.clone()),
                dir: dir.clone(),
            })
            .collect();
        if let Some(dir) = &self.signatures {
            roots.push(Root {
                account: None,
                dir: dir.clone(),
            });
        }
        roots
    }
}

/// One watched directory: an account's drafts, or the global signatures.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Root {
    /// The account whose drafts these are, `None` for the signatures.
    account: Option<String>,
    /// The directory itself.
    dir: PathBuf,
}

/// One draft as a settled poll read it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftSummary {
    /// The account whose drafts directory holds it.
    pub account: String,
    /// The `id:` field, or the file stem when there is none.
    pub id: String,
    /// The file itself.
    pub path: PathBuf,
    /// The `to:` field, `None` when there is no recipient yet.
    pub to: Option<String>,
    /// The `subject:` field, empty when there is none.
    pub subject: String,
    /// The word the file spells: `draft`, `approved` or `sent`.
    pub status: String,
    /// Whether [`crate::draft::validate_draft`] would let it send.
    pub ready: bool,
    /// Where it stands against its server copy (#0143), `None` until the
    /// account's runtime has handed the watcher its mirror state.
    pub server: Option<DraftServer>,
    /// Whether the mirror owes it a push: the account mirrors, the draft is
    /// pushable, and its bytes are not the ones its copy was written from.
    /// What makes the daemon ask the account's runtime for a drain.
    pub owes_push: bool,
}

/// One account's drafts mirror as its runtime last read it from the store
/// (#0143): what the watcher derives each draft's `server` member from
/// without opening a store itself.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MirrorState {
    /// `[accounts.drafts] mirror`, resolved ([`crate::draft_mirror::mirror_enabled`]).
    pub enabled: bool,
    /// Each draft's `draft_sync` row, as `(synced_hash, synced_at)`.
    pub rows: BTreeMap<String, (String, i64)>,
    /// The drafts whose push was parked `failed`.
    pub failed: BTreeSet<String>,
}

impl MirrorState {
    /// The `server` member of a draft whose file hashes to `hash`.
    ///
    /// `local` on an account that does not mirror and for a file written
    /// outside mp that was never pushed, which has no `server_message_id` yet;
    /// `conflict` for a conflict copy and for a draft one names; `failed` for a
    /// draft whose push was parked; `synced` when the bytes are the ones the
    /// copy was written from, `pending` otherwise, a draft the daemon created
    /// and has not pushed yet included (PERSO-231).
    pub fn server_of(
        &self,
        id: &str,
        hash: &str,
        frontmatter: &crate::types::EmailFrontmatter,
        has_conflict_copy: bool,
    ) -> DraftServer {
        let row = self.rows.get(id);
        let synced_at = row
            .and_then(|(_, at)| chrono::DateTime::from_timestamp(*at, 0).map(|at| at.to_rfc3339()));
        let state = if !self.enabled {
            DraftSyncState::Local
        } else if frontmatter.conflict_of.is_some() || has_conflict_copy {
            DraftSyncState::Conflict
        } else if self.failed.contains(id) {
            DraftSyncState::Failed
        } else {
            match row {
                Some((synced, _)) if synced == hash => DraftSyncState::Synced,
                Some(_) => DraftSyncState::Pending,
                None if frontmatter.server_message_id.is_some() => DraftSyncState::Pending,
                None => DraftSyncState::Local,
            }
        };
        DraftServer {
            state,
            synced_at: if self.enabled { synced_at } else { None },
        }
    }

    /// Whether a draft whose file hashes to `hash` owes a push.
    fn owes_push(&self, id: Option<&str>, hash: &str, draft: &crate::types::EmailDraft) -> bool {
        let Some(id) = id else { return false };
        self.enabled
            && draft.frontmatter.conflict_of.is_none()
            && draft.frontmatter.status != crate::types::EmailStatus::Sent
            && self.rows.get(id).is_none_or(|(synced, _)| synced != hash)
    }
}

/// Whether a conflict copy the reconcile wrote stands beside the draft at
/// `path` (#0143): `<stem>.conflict.md`, or `<stem>.conflict-<n>.md` for a
/// later one, which is what marks the draft as in conflict.
pub fn has_conflict_copy(path: &Path) -> bool {
    let (Some(stem), Some(dir)) = (path.file_stem(), path.parent()) else {
        return false;
    };
    let stem = stem.to_string_lossy();
    if stem.contains(".conflict") {
        return false;
    }
    let prefix = format!("{stem}.conflict");
    std::fs::read_dir(dir).is_ok_and(|entries| {
        entries.flatten().any(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            name.ends_with(".md")
                && name
                    .strip_prefix(&prefix)
                    .is_some_and(|rest| rest == ".md" || rest.starts_with('-'))
        })
    })
}

/// The draft a conflict copy at `path` stands beside, `None` for a file that
/// is not one.
fn original_of_conflict(path: &Path) -> Option<PathBuf> {
    let stem = path.file_stem()?.to_string_lossy();
    let (base, rest) = stem.split_once(".conflict")?;
    (rest.is_empty() || rest.starts_with('-')).then(|| path.with_file_name(format!("{base}.md")))
}

/// One thing a poll settled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WatchEvent {
    /// A draft appeared or changed and it parses.
    DraftChanged(DraftSummary),
    /// A draft appeared or changed and it does not parse.
    DraftInvalid {
        account: String,
        id: String,
        path: PathBuf,
        diagnostics: Vec<Diagnostic>,
    },
    /// A draft that had been announced is gone.
    DraftRemoved { account: String, id: String },
    /// A signature file appeared or changed.
    SignatureChanged { name: String, path: PathBuf },
    /// A signature file that had been announced is gone: deleted, or renamed
    /// away, in which case the new name arrives as a [`Self::SignatureChanged`].
    SignatureRemoved { name: String },
}

impl WatchEvent {
    /// The change this reduces into the snapshot, `None` for a signature:
    /// signatures are global, so no [`Change`] can answer `Change::account()`
    /// for one.
    pub fn as_change(&self) -> Option<Change> {
        match self {
            WatchEvent::DraftChanged(summary) => Some(Change::DraftUpsert {
                account: summary.account.clone(),
                id: summary.id.clone(),
                path: summary.path.display().to_string(),
                to: summary.to.clone(),
                subject: summary.subject.clone(),
                status: summary.status.clone(),
                valid: true,
                ready: summary.ready,
                server: summary.server.clone(),
            }),
            WatchEvent::DraftInvalid {
                account,
                id,
                path,
                diagnostics,
            } => Some(Change::DraftInvalid {
                account: account.clone(),
                id: id.clone(),
                path: path.display().to_string(),
                diagnostics: diagnostics.clone(),
            }),
            WatchEvent::DraftRemoved { account, id } => Some(Change::DraftRemoved {
                account: account.clone(),
                id: id.clone(),
            }),
            WatchEvent::SignatureChanged { .. } | WatchEvent::SignatureRemoved { .. } => None,
        }
    }

    /// The lifecycle event this publishes, which only a signature has.
    pub fn as_lifecycle(&self) -> Option<Event> {
        match self {
            WatchEvent::SignatureChanged { name, path } => Some(Event::Lifecycle {
                kind: KIND_SIGNATURE_CHANGED,
                payload: json!({"name": name, "path": path.display().to_string()}),
            }),
            WatchEvent::SignatureRemoved { name } => Some(Event::Lifecycle {
                kind: KIND_SIGNATURE_REMOVED,
                payload: json!({"name": name}),
            }),
            _ => None,
        }
    }
}

/// What one `stat` says about a file: deliberately no contents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Observation {
    modified: SystemTime,
    len: u64,
}

/// One watched file, between polls.
#[derive(Debug, Default)]
struct FileState {
    /// The observation that last settled; `None` means settled absent.
    settled: Option<Observation>,
    /// The id or name it was announced under, `None` until it settles present.
    announced: Option<String>,
    /// The observation it is moving towards, and when it was last seen moving.
    pending: Option<(Option<Observation>, Instant)>,
}

/// The poll, its debounce, and the inventory of what it has announced.
#[derive(Debug)]
pub struct DraftWatcher {
    roots: Vec<Root>,
    config: WatchConfig,
    /// One map per root, in root order, each keyed by path: iterating them
    /// yields a poll's events ordered by root and then by file name.
    files: Vec<BTreeMap<PathBuf, FileState>>,
    /// Each account's drafts mirror, as its runtime last handed it over
    /// (#0143); an account absent here has not been read yet, and its drafts
    /// carry no `server` member.
    mirror: BTreeMap<String, MirrorState>,
}

impl DraftWatcher {
    /// A watcher that has announced nothing yet.
    pub fn new(roots: WatchRoots, config: WatchConfig) -> DraftWatcher {
        let roots = roots.walk();
        let files = roots.iter().map(|_| BTreeMap::new()).collect();
        DraftWatcher {
            roots,
            config,
            files,
            mirror: BTreeMap::new(),
        }
    }

    /// The timings this watcher runs at.
    pub fn config(&self) -> WatchConfig {
        self.config
    }

    /// Point the watcher at a new set of roots, keeping what it has announced
    /// about the ones that stay (a `config.reload` adds and removes accounts).
    pub fn set_roots(&mut self, roots: WatchRoots) {
        let roots = roots.walk();
        let mut files: Vec<BTreeMap<PathBuf, FileState>> = Vec::with_capacity(roots.len());
        for root in &roots {
            let previous = self
                .roots
                .iter()
                .position(|old| old == root)
                .map(|at| std::mem::take(&mut self.files[at]));
            files.push(previous.unwrap_or_default());
        }
        self.roots = roots;
        self.files = files;
    }

    /// Look once, at the time the caller says it is.
    ///
    /// Everything that settled on this poll, ordered by root and then by file
    /// name, so a poll's output is a value a test can compare rather than a set
    /// it has to sort first.
    pub fn poll_once(&mut self, now: Instant) -> Vec<WatchEvent> {
        let mut events = Vec::new();
        // Conflict copies that appeared or went this poll: the draft beside
        // each is described again, since its `server` state moved with it.
        let mut conflicts_moved: Vec<(usize, PathBuf)> = Vec::new();
        for (index, root) in self.roots.iter().enumerate() {
            let seen = scan(&root.dir);
            let states = &mut self.files[index];
            // The union of what is on disk and what the watcher is tracking:
            // a file that vanished has a state and no observation.
            let paths: Vec<PathBuf> = seen
                .keys()
                .chain(states.keys())
                .cloned()
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect();
            for path in paths {
                let current = seen.get(&path).copied();
                let state = states.entry(path.clone()).or_default();
                if state.settled == current {
                    // Back where it settled: whatever it was doing is undone,
                    // which is what makes a rename-away-and-recreate one event.
                    state.pending = None;
                    continue;
                }
                match state.pending {
                    // Still moving, or moving for the first time: the window
                    // opens at this poll and every earlier one is forgotten.
                    Some((obs, _)) if obs != current => {
                        state.pending = Some((current, now));
                    }
                    None => state.pending = Some((current, now)),
                    // Held still since `since`, and long enough: settle. A
                    // poll inside the window leaves `since` alone, so the
                    // window runs from the last observed movement rather than
                    // from the last poll.
                    Some((_, since)) => {
                        if now.saturating_duration_since(since) < self.config.debounce {
                            continue;
                        }
                        state.settled = current;
                        state.pending = None;
                        if original_of_conflict(&path).is_some() {
                            conflicts_moved.push((index, path.clone()));
                        }
                        match current {
                            Some(_) => {
                                let mirror = root.account.as_ref().and_then(|a| self.mirror.get(a));
                                let event = describe(root, &path, mirror);
                                let name = announced_name(&event);
                                // The file kept its path and changed its name:
                                // a draft that gained an `id:` was announced
                                // under its stem and is announced under the id
                                // from now on. Without the removal the stem's
                                // row stays in every client's snapshot for
                                // ever, pointing at a draft that is also there
                                // under its new name.
                                if let (Some(account), Some(old)) =
                                    (root.account.clone(), state.announced.clone())
                                {
                                    if old != name {
                                        events.push(WatchEvent::DraftRemoved { account, id: old });
                                    }
                                }
                                state.announced = Some(name);
                                events.push(event);
                            }
                            None => match (root.account.clone(), state.announced.take()) {
                                (Some(account), Some(id)) => {
                                    events.push(WatchEvent::DraftRemoved { account, id });
                                }
                                (None, Some(name)) => {
                                    events.push(WatchEvent::SignatureRemoved { name });
                                }
                                _ => {}
                            },
                        }
                    }
                }
            }
            // A file that is absent, settled absent and was never announced is
            // nothing to remember: the map holds what is watched, not every
            // name that ever appeared in the directory.
            states.retain(|_, state| {
                state.settled.is_some() || state.pending.is_some() || state.announced.is_some()
            });
        }
        for (index, copy) in conflicts_moved {
            if let Some(original) = original_of_conflict(&copy) {
                self.recommit_path(index, &original);
            }
        }
        events
    }

    /// Hand over `account`'s mirror state, as its runtime read it from the
    /// store (#0143), and describe again every draft whose `server` member
    /// may have moved with it: all of the account's on the first hand-over or
    /// when the switch flips, otherwise those whose row or failure changed.
    pub fn set_mirror(&mut self, account: &str, state: MirrorState) {
        let previous = self.mirror.insert(account.to_string(), state.clone());
        let moved: Option<BTreeSet<String>> = match &previous {
            Some(old) if old.enabled == state.enabled => {
                let ids: BTreeSet<&String> = old
                    .rows
                    .keys()
                    .chain(state.rows.keys())
                    .chain(old.failed.iter())
                    .chain(state.failed.iter())
                    .collect();
                Some(
                    ids.into_iter()
                        .filter(|id| {
                            old.rows.get(*id) != state.rows.get(*id)
                                || old.failed.contains(*id) != state.failed.contains(*id)
                        })
                        .cloned()
                        .collect(),
                )
            }
            _ => None,
        };
        let indices: Vec<usize> = self
            .roots
            .iter()
            .enumerate()
            .filter(|(_, root)| root.account.as_deref() == Some(account))
            .map(|(index, _)| index)
            .collect();
        for index in indices {
            let paths: Vec<PathBuf> = self.files[index]
                .iter()
                .filter(|(_, state)| {
                    state.settled.is_some()
                        && moved.as_ref().is_none_or(|ids| {
                            state.announced.as_ref().is_some_and(|id| ids.contains(id))
                        })
                })
                .map(|(path, _)| path.clone())
                .collect();
            for path in paths {
                self.recommit_path(index, &path);
            }
        }
    }

    /// The `server` member of `account`'s draft `id` at `path`, read from the
    /// file now: what `draft.list` answers, which scans fresh.
    pub fn server_of(&self, account: &str, id: &str, path: &Path) -> Option<DraftServer> {
        let mirror = self.mirror.get(account)?;
        let bytes = std::fs::read(path).ok()?;
        let draft = crate::draft::parse_email_draft(path).ok()?;
        Some(mirror.server_of(
            id,
            &crate::draft_mirror::file_hash(&bytes),
            &draft.frontmatter,
            has_conflict_copy(path),
        ))
    }

    /// Describe the file at `path` again at the next poll that finds it
    /// still, without a removal: its settled observation is forgotten, so the
    /// poll reads it as moved, and the name it was announced under stays.
    fn recommit_path(&mut self, index: usize, path: &Path) {
        if let Some(state) = self.files[index].get_mut(path) {
            if state.settled.is_some() {
                state.settled = None;
                state.pending = None;
            }
        }
    }

    /// The file the account's draft `id` was announced from, or `None`.
    ///
    /// The lookup `draft.approve` uses: the settled inventory, not the drafts
    /// index, which lives in a store behind an engine lock. A draft the watcher
    /// has only glimpsed resolves to nothing, because the inventory is what the
    /// daemon has announced.
    pub fn resolve(&self, account: &str, id: &str) -> Option<PathBuf> {
        for (index, root) in self.roots.iter().enumerate() {
            if root.account.as_deref() != Some(account) {
                continue;
            }
            for (path, state) in &self.files[index] {
                if state.announced.as_deref() == Some(id) && state.settled.is_some() {
                    return Some(path.clone());
                }
            }
        }
        None
    }
}

/// One `stat` per depth-1 `*.md` file of `dir`, reading no contents.
///
/// A missing directory is an empty one. A `.tmp`, a `.swp`, vim's `4913`
/// writability probe and a subdirectory are not drafts, which is what makes an
/// editor's save dance invisible rather than an event storm.
fn scan(dir: &Path) -> BTreeMap<PathBuf, Observation> {
    let mut seen = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return seen;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "md") {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        seen.insert(
            path,
            Observation {
                modified: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                len: meta.len(),
            },
        );
    }
    seen
}

/// Read one settled file and say what it is, with its mirror state when the
/// account's runtime has handed one over.
fn describe(root: &Root, path: &Path, mirror: Option<&MirrorState>) -> WatchEvent {
    let Some(account) = root.account.clone() else {
        return WatchEvent::SignatureChanged {
            name: stem(path),
            path: path.to_path_buf(),
        };
    };
    match crate::draft::parse_email_draft(path) {
        Ok(draft) => {
            let id = field(draft.frontmatter.id.as_deref());
            let hash = std::fs::read(path)
                .map(|bytes| crate::draft_mirror::file_hash(&bytes))
                .unwrap_or_default();
            let announced = id.clone().unwrap_or_else(|| stem(path));
            WatchEvent::DraftChanged(DraftSummary {
                account,
                server: mirror.map(|mirror| {
                    mirror.server_of(
                        &announced,
                        &hash,
                        &draft.frontmatter,
                        has_conflict_copy(path),
                    )
                }),
                owes_push: mirror
                    .is_some_and(|mirror| mirror.owes_push(id.as_deref(), &hash, &draft)),
                id: announced,
                to: field(draft.frontmatter.to.as_deref()),
                subject: draft.frontmatter.subject.clone(),
                status: draft.frontmatter.status.to_string(),
                ready: crate::draft::validate_draft(&draft).is_ok(),
                path: path.to_path_buf(),
            })
        }
        Err(e) => WatchEvent::DraftInvalid {
            account,
            // The stem, because the `id:` field is inside the block that would
            // not parse.
            id: stem(path),
            path: path.to_path_buf(),
            diagnostics: vec![diagnose(path, &e)],
        },
    }
}

/// Why one file will not parse, positioned when the position exists.
///
/// Public because `draft.approve` refuses an unparseable draft with the very
/// payload the `draft.invalid` event carries, and one refusal is rendered by
/// one piece of client code. Empty for a file that parses.
pub fn diagnostics_of(path: &Path) -> Vec<Diagnostic> {
    match crate::draft::parse_email_draft(path) {
        Ok(_) => Vec::new(),
        Err(e) => vec![diagnose(path, &e)],
    }
}

/// One diagnostic: the parser's reason on one line, and the line it is about.
fn diagnose(path: &Path, error: &anyhow::Error) -> Diagnostic {
    Diagnostic {
        line: frontmatter_line(path),
        message: format!("{error:#}")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" "),
    }
}

/// The name a settled event announced its file under.
fn announced_name(event: &WatchEvent) -> String {
    match event {
        WatchEvent::DraftChanged(summary) => summary.id.clone(),
        WatchEvent::DraftInvalid { id, .. } => id.clone(),
        WatchEvent::SignatureChanged { name, .. } => name.clone(),
        WatchEvent::SignatureRemoved { name } => name.clone(),
        WatchEvent::DraftRemoved { id, .. } => id.clone(),
    }
}

/// A frontmatter field that is present and not blank; an empty `to:` is no
/// recipient rather than an empty one.
fn field(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// The file stem, which is a stable addressable name a file already has.
fn stem(path: &Path) -> String {
    path.file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

/// The 1-based, file-relative line a refusal points at, or `None`.
///
/// A second pass with `serde_yaml` over the frontmatter block alone, because
/// `gray_matter`'s YAML engine reports a scan failure as a null document and
/// keeps no position. A refusal by value rather than by syntax - a numeric
/// `id:` (#0083) - parses here and correctly yields no line.
fn frontmatter_line(path: &Path) -> Option<u32> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut lines = text.lines();
    if lines.next()?.trim_end() != "---" {
        return None;
    }
    let block: String = lines
        .take_while(|line| line.trim_end() != "---")
        .map(|line| format!("{line}\n"))
        .collect();
    let error = serde_yaml::from_str::<serde_yaml::Value>(&block).err()?;
    // The block starts on the file's second line, so the offset is the one
    // opening delimiter; clamped, because a scanner that stopped at the end of
    // the block may point one line past it.
    let line = error.location()?.line() as u32 + 1;
    Some(line.min(text.lines().count().max(1) as u32))
}

// ---------------------------------------------------------------------------
// The daemon's end
// ---------------------------------------------------------------------------

/// The watcher as the daemon holds it: shared, re-rooted by a `config.reload`,
/// and read by `draft.approve`.
#[derive(Debug)]
pub struct DraftWatch {
    inner: Mutex<DraftWatcher>,
    config: WatchConfig,
}

impl DraftWatch {
    /// A handle over a watcher at the configured timings.
    pub fn new(roots: WatchRoots, config: WatchConfig) -> DraftWatch {
        DraftWatch {
            inner: Mutex::new(DraftWatcher::new(roots, config)),
            config,
        }
    }

    /// The timings, which the poll loop reads without taking the lock.
    pub fn config(&self) -> WatchConfig {
        self.config
    }

    /// Re-derive the roots, which is what a configuration swap does to them.
    pub fn set_roots(&self, roots: WatchRoots) {
        self.watcher().set_roots(roots);
    }

    /// One poll, at the wall clock.
    pub fn poll(&self) -> Vec<WatchEvent> {
        self.watcher().poll_once(Instant::now())
    }

    /// The path `draft.approve` acts on.
    pub fn resolve(&self, account: &str, id: &str) -> Option<PathBuf> {
        self.watcher().resolve(account, id)
    }

    /// Hand over `account`'s mirror state ([`DraftWatcher::set_mirror`]).
    pub fn set_mirror(&self, account: &str, state: MirrorState) {
        self.watcher().set_mirror(account, state);
    }

    /// The `server` member `draft.list` answers ([`DraftWatcher::server_of`]).
    pub fn server_of(&self, account: &str, id: &str, path: &Path) -> Option<DraftServer> {
        self.watcher().server_of(account, id, path)
    }

    fn watcher(&self) -> MutexGuard<'_, DraftWatcher> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The drafts directory of every configured account, plus the signatures.
pub fn roots_from(accounts: &[crate::config::AccountConfig]) -> WatchRoots {
    let mut roots = WatchRoots::new();
    for account in accounts {
        roots = roots.with_account(&account.name, crate::config::drafts_dir(&account.name));
    }
    roots.with_signatures(crate::signatures::signatures_dir())
}

/// Poll until `stop` says the daemon is closing its watchers, committing what
/// settles.
///
/// Independent of the account runtimes: watching drafts takes no engine lock
/// and opens no store, so it runs for an account whose runtime is blocked and
/// for one that has not come up yet. The `stat` walk and the reparse run on
/// `spawn_blocking`, off the
/// reactor, because both touch a filesystem that may be slow.
///
/// `stop` is step 6 of [`shutdown`](super::shutdown): the loop ends there
/// rather than at process exit, so "closes watchers" is a step of the sequence
/// and a draft written while the daemon is leaving is not committed into a
/// state nobody will read.
///
/// A settled draft that owes the mirror a push (#0143) asks its account's
/// runtime in `runtimes` for a drain, which queues the push and runs it; the
/// watcher itself writes nothing and opens no store.
pub fn spawn(
    watch: Arc<DraftWatch>,
    canonical: Arc<CanonicalState>,
    runtimes: Arc<super::server::RuntimeTable>,
    mut stop: tokio::sync::watch::Receiver<bool>,
) {
    let interval = watch.config().poll_interval;
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = ticker.tick() => {}
                changed = stop.changed() => {
                    if changed.is_err() || *stop.borrow() {
                        debug!("[daemon] the draft watcher is stopping");
                        return;
                    }
                    continue;
                }
            }
            let polled = Arc::clone(&watch);
            let events = match tokio::task::spawn_blocking(move || polled.poll()).await {
                Ok(events) => events,
                Err(e) => {
                    debug!("[daemon] the draft poll {e}");
                    continue;
                }
            };
            for event in events {
                if let WatchEvent::DraftChanged(summary) = &event {
                    if summary.owes_push {
                        if let Some(runtime) = runtimes.get(&summary.account) {
                            runtime.request_drain();
                        }
                    }
                }
                if let Some(change) = event.as_change() {
                    canonical.apply(change);
                } else if let Some(lifecycle) = event.as_lifecycle() {
                    canonical.publish(lifecycle);
                }
            }
        }
    });
}
