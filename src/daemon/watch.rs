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

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use log::debug;
use mp_protocol::events::{Diagnostic, KIND_SIGNATURE_CHANGED};
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
            WatchEvent::SignatureChanged { .. } => None,
        }
    }

    /// The lifecycle event this publishes, which only a signature has.
    pub fn as_lifecycle(&self) -> Option<Event> {
        match self {
            WatchEvent::SignatureChanged { name, path } => Some(Event::Lifecycle {
                kind: KIND_SIGNATURE_CHANGED,
                payload: json!({"name": name, "path": path.display().to_string()}),
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
                        match current {
                            Some(_) => {
                                let event = describe(root, &path);
                                state.announced = Some(announced_name(&event));
                                events.push(event);
                            }
                            None => {
                                if let (Some(account), Some(id)) =
                                    (root.account.clone(), state.announced.take())
                                {
                                    events.push(WatchEvent::DraftRemoved { account, id });
                                }
                            }
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
        events
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

/// Read one settled file and say what it is.
fn describe(root: &Root, path: &Path) -> WatchEvent {
    let Some(account) = root.account.clone() else {
        return WatchEvent::SignatureChanged {
            name: stem(path),
            path: path.to_path_buf(),
        };
    };
    match crate::draft::parse_email_draft(path) {
        Ok(draft) => WatchEvent::DraftChanged(DraftSummary {
            account,
            id: field(draft.frontmatter.id.as_deref()).unwrap_or_else(|| stem(path)),
            to: field(draft.frontmatter.to.as_deref()),
            subject: draft.frontmatter.subject.clone(),
            status: draft.frontmatter.status.to_string(),
            ready: crate::draft::validate_draft(&draft).is_ok(),
            path: path.to_path_buf(),
        }),
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

/// Poll for the life of the daemon, committing what settles.
///
/// Unconditional: watching drafts takes no engine lock and opens no store, so
/// gating it behind `MAILYPOPPINS_DAEMON_ACCOUNT_RUNTIMES` would make Phase
/// 3b's drafts untestable without also acquiring locks the tests do not want
/// held. The `stat` walk and the reparse run on `spawn_blocking`, off the
/// reactor, because both touch a filesystem that may be slow.
pub fn spawn(watch: Arc<DraftWatch>, canonical: Arc<CanonicalState>) {
    let interval = watch.config().poll_interval;
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            let polled = Arc::clone(&watch);
            let events = match tokio::task::spawn_blocking(move || polled.poll()).await {
                Ok(events) => events,
                Err(e) => {
                    debug!("[daemon] the draft poll {e}");
                    continue;
                }
            };
            for event in events {
                if let Some(change) = event.as_change() {
                    canonical.apply(change);
                } else if let Some(lifecycle) = event.as_lifecycle() {
                    canonical.publish(lifecycle);
                }
            }
        }
    });
}
