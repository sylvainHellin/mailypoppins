//! The app's own updates from GitHub Releases (#0139, stages 1 and 2).
//!
//! `tauri-plugin-updater` reads `latest.json` from the endpoint in
//! `tauri.conf.json` (`plugins.updater`), checks the archive's minisign
//! signature against the `pubkey` there, and swaps the running `.app` in
//! place. Everything runs in Rust, so the webview holds no `updater:*`
//! permission and the CSP stays as it is.
//!
//! - A silent check runs [`STARTUP_DELAY`] after setup, at most once per
//!   [`COOLDOWN`], and emits [`UPDATE_AVAILABLE_EVENT`] when it finds a
//!   version the user did not skip; a failure is only logged.
//! - `update_check`, `update_skip`, `update_install` and `update_restart` are
//!   the frontend's commands; the found [`Update`] is held in [`Updates`]
//!   between a check and an install, so the install does not check again.
//! - `update_status` reads what this run knows (the held and the installed
//!   version, the state file) without reaching the endpoint, for Settings
//!   and for a webview that reloaded.
//! - `update-state.json` in the app data directory keeps the last successful
//!   check, the skipped version and the `{from, to}` of an installed update
//!   ([`UpdateRecord`]); a missing or broken file reads as empty. The
//!   connector reads the `{from, to}` to restart the daemon an update left
//!   behind without the blocking screen ([`take_quiet_restart`]).
//! - Once this run installed an update, a check answers `restart_pending`
//!   without reaching the endpoint ([`answer_before_fetch`]).
//! - Nothing checks in a debug build, in fixture mode, or outside a `.app`
//!   bundle ([`gate`]): a dev build is `0.1.0` and would always see an update.
//!
//! See `clients/desktop/docs/rust-layer.md`, "Updates".

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::ipc::Channel;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_updater::{Update, UpdaterExt};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::session::SessionHandle;

/// Emitted to the window when the silent startup check finds an update the
/// user did not skip; the payload is an [`UpdateAvailable`].
pub const UPDATE_AVAILABLE_EVENT: &str = "update:available";

/// Emitted by the App menu's "Check for Updates…"; no payload. The frontend
/// runs `update_check` with `manual: true`, so its notice line shows the
/// answer.
pub const UPDATE_CHECK_REQUESTED_EVENT: &str = "update:check_requested";

/// The state file, in the app data directory.
pub const STATE_FILE: &str = "update-state.json";

/// How long after setup the silent check runs.
pub const STARTUP_DELAY: Duration = Duration::from_secs(10);

/// The silent check runs at most once in this long after a successful one.
pub const COOLDOWN: time::Duration = time::Duration::hours(24);

/// How long `update_restart` waits for `daemon.stop` to be answered.
const STOP_BUDGET: Duration = Duration::from_secs(5);

/// How long `update_restart` waits for the stopped daemon's socket to go:
/// the daemon's default grace (10 s) and a margin.
const SOCKET_GONE_WAIT: Duration = Duration::from_secs(15);

/// The least time between two `Progress` messages on the channel.
const PROGRESS_EVERY: Duration = Duration::from_millis(100);

// ---------------------------------------------------------------------------
// What the frontend is told
// ---------------------------------------------------------------------------

/// How a check went.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
#[serde(rename_all = "snake_case")]
pub enum UpdateCheckState {
    /// No newer version, or (without `manual`) only one the user skipped.
    UpToDate,
    /// A newer version is published; `version`, `notes` and `date` say which.
    Available,
    /// This build never checks; `reason` says why.
    Disabled,
    /// The check did not complete; `reason` says why.
    Failed,
    /// This run installed `version`, and a restart runs it; nothing is
    /// fetched until then.
    RestartPending,
}

/// What `update_check` answers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
pub struct UpdateCheck {
    pub state: UpdateCheckState,
    /// The running app's version, the one the updater compares against.
    pub current: String,
    /// The newer version, when `available`; the installed one, when
    /// `restart_pending`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub version: Option<String>,
    /// Its release notes, when the manifest carries them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub notes: Option<String>,
    /// Its publication date, RFC 3339.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub date: Option<String>,
    /// Why the check is `disabled` or `failed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub reason: Option<String>,
    /// The last successful check, RFC 3339; absent before the first one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub last_check: Option<String>,
}

/// The payload of [`UPDATE_AVAILABLE_EVENT`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
pub struct UpdateAvailable {
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub notes: Option<String>,
    /// RFC 3339.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub date: Option<String>,
}

/// What `update_status` answers: what this run knows, from managed state
/// and `update-state.json`, without reaching the endpoint.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
pub struct UpdateStatus {
    /// The running app's version, the one the updater compares against.
    pub current: String,
    /// Whether this build checks for updates at all.
    pub enabled: bool,
    /// Why it does not, when `enabled` is false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub reason: Option<String>,
    /// The last successful check, RFC 3339; absent before the first one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub last_check: Option<String>,
    /// The version a check of this run found and holds for `update_install`,
    /// unless it is the skipped one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub available: Option<String>,
    /// The version this run installed, waiting for `update_restart`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub installed: Option<String>,
}

/// One message on `update_install`'s channel, in order: `started`, any
/// number of `progress`, then `finished` once the new bundle is in place.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UpdateProgress {
    /// The download began; `content_length` is the archive's size in bytes
    /// when the server sent one.
    Started {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(test, ts(optional))]
        content_length: Option<u64>,
    },
    /// `downloaded` bytes so far, at most every 100 ms and once at the end.
    Progress {
        downloaded: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(test, ts(optional))]
        content_length: Option<u64>,
    },
    /// The signature checked out and the new bundle replaced the old one;
    /// `update_restart` runs it.
    Finished,
}

impl UpdateCheck {
    fn bare(state: UpdateCheckState, current: String) -> UpdateCheck {
        UpdateCheck {
            state,
            current,
            version: None,
            notes: None,
            date: None,
            reason: None,
            last_check: None,
        }
    }

    fn disabled(current: String, reason: &str) -> UpdateCheck {
        UpdateCheck {
            reason: Some(reason.to_string()),
            ..UpdateCheck::bare(UpdateCheckState::Disabled, current)
        }
    }

    fn up_to_date(current: String, last_check: Option<String>) -> UpdateCheck {
        UpdateCheck {
            last_check,
            ..UpdateCheck::bare(UpdateCheckState::UpToDate, current)
        }
    }

    fn failed(current: String, reason: String, last_check: Option<String>) -> UpdateCheck {
        UpdateCheck {
            reason: Some(reason),
            last_check,
            ..UpdateCheck::bare(UpdateCheckState::Failed, current)
        }
    }

    fn restart_pending(
        current: String,
        installed: String,
        last_check: Option<String>,
    ) -> UpdateCheck {
        UpdateCheck {
            version: Some(installed),
            last_check,
            ..UpdateCheck::bare(UpdateCheckState::RestartPending, current)
        }
    }

    fn available(current: String, found: &Update, last_check: Option<String>) -> UpdateCheck {
        UpdateCheck {
            version: Some(found.version.clone()),
            notes: found.body.clone(),
            date: found.date.and_then(|d| d.format(&Rfc3339).ok()),
            last_check,
            ..UpdateCheck::bare(UpdateCheckState::Available, current)
        }
    }
}

// ---------------------------------------------------------------------------
// The decisions, pure
// ---------------------------------------------------------------------------

/// Whether a check runs now: always on `manual`, else when there was no
/// successful check yet, the last one is [`COOLDOWN`] old, or the clock went
/// back past it.
pub fn should_check(now: OffsetDateTime, last_check: Option<OffsetDateTime>, manual: bool) -> bool {
    if manual {
        return true;
    }
    match last_check {
        None => true,
        Some(last) => {
            let elapsed = now - last;
            elapsed.is_negative() || elapsed >= COOLDOWN
        }
    }
}

/// What a check answers without reaching the endpoint once this run has
/// installed an update: the restart is what is left to do, since the bundle
/// on disk is already the new one (a manual check after Later).
pub fn answer_before_fetch(
    current: &str,
    installed: Option<String>,
    last_check: Option<String>,
) -> Option<UpdateCheck> {
    installed.map(|v| UpdateCheck::restart_pending(current.to_string(), v, last_check))
}

/// Whether a found version is shown: always on `manual`, else unless it is
/// the version the user skipped (a leading `v` on either side ignored).
pub fn should_offer(found_version: &str, skipped_version: Option<&str>, manual: bool) -> bool {
    fn bare(v: &str) -> &str {
        v.trim().trim_start_matches('v')
    }
    manual || skipped_version.is_none_or(|skipped| bare(skipped) != bare(found_version))
}

/// What `update_status` answers, from the gate's `reason`, the state file's
/// record, the version held in managed state and the one this run installed.
pub fn status(
    current: String,
    reason: Option<&str>,
    record: &UpdateRecord,
    held: Option<&str>,
    installed: Option<String>,
) -> UpdateStatus {
    let skipped = record.skipped_version.as_deref();
    UpdateStatus {
        current,
        enabled: reason.is_none(),
        reason: reason.map(str::to_string),
        last_check: record.last_check.clone(),
        available: held
            .filter(|v| should_offer(v, skipped, false))
            .map(str::to_string),
        installed,
    }
}

/// Whether an executable runs from inside a macOS app bundle, the only
/// layout the updater can replace.
pub fn in_app_bundle(exe: &Path) -> bool {
    exe.to_string_lossy().contains(".app/Contents/MacOS/")
}

/// Why this build never checks, or `None` when it does: a debug build, the
/// fixture, or an executable outside a `.app` bundle.
pub fn gate(debug_build: bool, fixture: bool, exe: Option<&Path>) -> Option<&'static str> {
    if debug_build {
        return Some("Updates are off in a development build.");
    }
    if fixture {
        return Some("Updates are off in fixture mode.");
    }
    match exe {
        Some(exe) if in_app_bundle(exe) => None,
        _ => Some("Updates are off: the app is not running from mailypoppins.app."),
    }
}

/// [`gate`] for this process.
pub fn disabled_reason() -> Option<&'static str> {
    gate(
        cfg!(debug_assertions),
        crate::fixture_requested(),
        std::env::current_exe().ok().as_deref(),
    )
}

/// Whether this process checks for updates at all.
pub fn checks_enabled() -> bool {
    disabled_reason().is_none()
}

/// RFC 3339, as `update-state.json` keeps a time.
pub fn format_time(at: OffsetDateTime) -> String {
    at.format(&Rfc3339)
        .unwrap_or_else(|_| at.unix_timestamp().to_string())
}

/// An RFC 3339 time, or `None` for anything else.
pub fn parse_time(text: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(text.trim(), &Rfc3339).ok()
}

// ---------------------------------------------------------------------------
// update-state.json
// ---------------------------------------------------------------------------

/// `update-state.json`: what the checks remember across launches.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdateRecord {
    /// The last successful check, RFC 3339.
    pub last_check: Option<String>,
    /// The version `update_skip` recorded; the silent check does not offer it.
    pub skipped_version: Option<String>,
    /// Written by a successful install, for the next launch: its first
    /// connect restarts a daemon still at `from` without the blocking screen
    /// ([`take_quiet_restart`]), and a daemon at `to` settles it
    /// ([`settle_update`]).
    pub pending_restart: Option<PendingRestart>,
}

/// The versions an installed update goes between.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingRestart {
    pub from: String,
    pub to: String,
}

/// `update-state.json` in `data_dir`.
pub fn state_path(data_dir: &Path) -> PathBuf {
    data_dir.join(STATE_FILE)
}

/// The file's record; a missing, unreadable or broken file is an empty one.
pub fn read_record(file: &Path) -> UpdateRecord {
    match std::fs::read_to_string(file) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
            tracing::warn!(
                "[update] {} does not parse ({e}); read as empty",
                file.display()
            );
            UpdateRecord::default()
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => UpdateRecord::default(),
        Err(e) => {
            tracing::warn!(
                "[update] could not read {} ({e}); read as empty",
                file.display()
            );
            UpdateRecord::default()
        }
    }
}

/// Serialises the read-modify-write of `update-state.json` across the
/// commands, the startup check and the connector.
static FILE_LOCK: Mutex<()> = Mutex::new(());

/// Change the state file under [`FILE_LOCK`], writing it only when the
/// change changed something; a file that cannot be written is logged, since
/// nothing the user asked for depends on it.
pub fn change_record(file: &Path, change: impl FnOnce(&mut UpdateRecord)) {
    let _guard = lock(&FILE_LOCK);
    let before = read_record(file);
    let mut record = before.clone();
    change(&mut record);
    if record == before {
        return;
    }
    if let Err(e) = write_record(file, &record) {
        tracing::warn!("[update] could not write {}: {e}", file.display());
    }
}

// ---------------------------------------------------------------------------
// After an update: the old daemon
// ---------------------------------------------------------------------------

/// Whether a version mismatch is the one an update leaves behind, which the
/// connect fixes by restarting the daemon without the blocking screen: the
/// daemon still runs the update's `from`, and this app and the `mp` it starts
/// a daemon with are its `to`. That is the new app's first connect after
/// Later and a quit; the old app in the Later window (its version is `from`)
/// and every other mismatch still get the screen.
pub fn restarts_quietly(
    pending: Option<&PendingRestart>,
    daemon_version: &str,
    app_version: Option<&str>,
    expected_version: &str,
) -> bool {
    pending.is_some_and(|p| {
        p.from == daemon_version && app_version == Some(p.to.as_str()) && expected_version == p.to
    })
}

/// Drop a recorded update that is not to the running version `current`: a
/// later launch of another version (a reinstall from a DMG, a downgrade) is
/// no longer that update's new app. Answers what it dropped.
pub fn drop_stale_update(record: &mut UpdateRecord, current: &str) -> Option<PendingRestart> {
    record.pending_restart.take_if(|p| p.to != current)
}

/// [`restarts_quietly`] against the record in `file`; when it holds, the
/// record's `pending_restart` is cleared before the caller restarts, so the
/// quiet restart runs once and a daemon that comes back at the wrong version
/// gets the screen. Answers the update it applies to.
pub fn take_quiet_restart(
    file: &Path,
    daemon_version: &str,
    app_version: Option<&str>,
    expected_version: &str,
) -> Option<PendingRestart> {
    let mut taken = None;
    change_record(file, |r| {
        taken = r
            .pending_restart
            .take_if(|p| restarts_quietly(Some(p), daemon_version, app_version, expected_version));
    });
    taken
}

/// A handshake with a daemon at `daemon_version` finished the update the
/// record in `file` holds when that is its `to` and this app's version: the
/// record is cleared, so a later mismatch is never taken for this update.
pub fn settle_update(file: &Path, daemon_version: &str, app_version: Option<&str>) {
    change_record(file, |r| {
        let finished = r
            .pending_restart
            .take_if(|p| p.to == daemon_version && app_version == Some(p.to.as_str()));
        if let Some(done) = finished {
            tracing::info!(
                "[update] the update from {} to {} is finished: the daemon runs {}",
                done.from,
                done.to,
                done.to
            );
        }
    });
}

/// Write the record whole: a temporary file beside it, then a rename, so a
/// crash leaves the old file or the new one and never half of either.
pub fn write_record(file: &Path, record: &UpdateRecord) -> std::io::Result<()> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_string_pretty(record).map_err(std::io::Error::other)? + "\n";
    let tmp = file.with_file_name(format!(".{STATE_FILE}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, file).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

// ---------------------------------------------------------------------------
// Managed state
// ---------------------------------------------------------------------------

/// What this run knows about updates, in Tauri managed state.
#[derive(Clone, Default)]
pub struct Updates {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    /// The update the last check found, for `update_install`.
    found: Mutex<Option<Update>>,
    /// The version this run installed, waiting for `update_restart`.
    installed: Mutex<Option<String>>,
    /// An install is running.
    installing: AtomicBool,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    match m.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Clears the install flag when the install ends, however it ends.
struct Installing(Arc<Inner>);

impl Drop for Installing {
    fn drop(&mut self) {
        self.0.installing.store(false, Ordering::SeqCst);
    }
}

impl Updates {
    fn found(&self) -> Option<Update> {
        lock(&self.inner.found).clone()
    }

    fn hold(&self, update: Option<Update>) {
        *lock(&self.inner.found) = update;
    }

    fn installed(&self) -> Option<String> {
        lock(&self.inner.installed).clone()
    }

    fn begin_install(&self) -> Result<Installing, String> {
        if self.inner.installing.swap(true, Ordering::SeqCst) {
            return Err("An update is already being installed.".into());
        }
        Ok(Installing(Arc::clone(&self.inner)))
    }

    /// [`change_record`], when there is a file.
    fn record(&self, file: Option<&Path>, change: impl FnOnce(&mut UpdateRecord)) {
        if let Some(file) = file {
            change_record(file, change);
        }
    }
}

// ---------------------------------------------------------------------------
// The check
// ---------------------------------------------------------------------------

/// The running app's version, as the updater compares it.
fn current_version(app: &AppHandle) -> String {
    app.package_info().version.to_string()
}

/// `update-state.json` of this app, `None` when there is no data directory.
pub fn record_file(app: &AppHandle) -> Option<PathBuf> {
    match app.path().app_data_dir() {
        Ok(dir) => Some(state_path(&dir)),
        Err(e) => {
            tracing::warn!("[update] no app data directory: {e}");
            None
        }
    }
}

/// One request to the endpoint.
async fn fetch(app: &AppHandle) -> Result<Option<Update>, String> {
    let updater = app.updater().map_err(|e| e.to_string())?;
    updater.check().await.map_err(|e| e.to_string())
}

/// A check, with the cooldown and the skip applied unless `manual`.
async fn run_check(app: &AppHandle, manual: bool) -> UpdateCheck {
    let current = current_version(app);
    if let Some(reason) = disabled_reason() {
        return UpdateCheck::disabled(current, reason);
    }
    let updates = app.state::<Updates>().inner().clone();
    let file = record_file(app);
    let record = file.as_deref().map(read_record).unwrap_or_default();
    if let Some(pending) =
        answer_before_fetch(&current, updates.installed(), record.last_check.clone())
    {
        return pending;
    }
    let skipped = record.skipped_version.as_deref();
    let now = OffsetDateTime::now_utc();
    let last = record.last_check.as_deref().and_then(parse_time);
    if !should_check(now, last, manual) {
        // Inside the cooldown: answer from what this run already found.
        return match updates.found() {
            Some(found) if should_offer(&found.version, skipped, false) => {
                UpdateCheck::available(current, &found, record.last_check)
            }
            _ => UpdateCheck::up_to_date(current, record.last_check),
        };
    }
    match fetch(app).await {
        Ok(found) => {
            let stamp = format_time(now);
            updates.record(file.as_deref(), |r| r.last_check = Some(stamp.clone()));
            updates.hold(found.clone());
            match found {
                Some(found) if should_offer(&found.version, skipped, manual) => {
                    tracing::info!(
                        "[update] {} is available (running {current})",
                        found.version
                    );
                    UpdateCheck::available(current, &found, Some(stamp))
                }
                Some(found) => {
                    tracing::info!("[update] {} is available but skipped", found.version);
                    UpdateCheck::up_to_date(current, Some(stamp))
                }
                None => {
                    tracing::info!("[update] {current} is up to date");
                    UpdateCheck::up_to_date(current, Some(stamp))
                }
            }
        }
        Err(why) => {
            tracing::warn!("[update] the check failed: {why}");
            UpdateCheck::failed(current, why, record.last_check)
        }
    }
}

/// The silent check, [`STARTUP_DELAY`] after setup. A recorded update that
/// is not this app's version is stale and cleared first (one that is stays
/// for the connector, which settles it once the daemon runs it), then the
/// check runs under the cooldown; a failure is only logged.
pub fn spawn_startup_check(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(STARTUP_DELAY).await;
        let current = current_version(&app);
        if let Some(reason) = disabled_reason() {
            tracing::debug!("[update] no startup check: {reason}");
            return;
        }
        if let Some(file) = record_file(&app) {
            change_record(&file, |r| {
                if let Some(stale) = drop_stale_update(r, &current) {
                    tracing::info!(
                        "[update] running {current}, so the recorded update from {} to {} is stale",
                        stale.from,
                        stale.to
                    );
                }
            });
        }
        let answer = run_check(&app, false).await;
        if answer.state == UpdateCheckState::Available {
            if let Some(version) = answer.version {
                let payload = UpdateAvailable {
                    version,
                    notes: answer.notes,
                    date: answer.date,
                };
                if let Err(e) = app.emit(UPDATE_AVAILABLE_EVENT, payload) {
                    tracing::warn!("[update] could not emit {UPDATE_AVAILABLE_EVENT}: {e}");
                }
            }
        }
    });
}

// ---------------------------------------------------------------------------
// The install
// ---------------------------------------------------------------------------

/// Turns the plugin's chunk callbacks into channel messages.
struct Reporter {
    channel: Channel<UpdateProgress>,
    started: bool,
    downloaded: u64,
    last_sent: Option<Instant>,
}

impl Reporter {
    fn new(channel: Channel<UpdateProgress>) -> Reporter {
        Reporter {
            channel,
            started: false,
            downloaded: 0,
            last_sent: None,
        }
    }

    fn send(&self, message: UpdateProgress) {
        if let Err(e) = self.channel.send(message) {
            tracing::debug!("[update] the progress channel is gone: {e}");
        }
    }

    fn start(&mut self, content_length: Option<u64>) {
        if !self.started {
            self.started = true;
            self.send(UpdateProgress::Started { content_length });
        }
    }

    fn chunk(&mut self, len: usize, content_length: Option<u64>) {
        self.start(content_length);
        self.downloaded += len as u64;
        let complete = content_length.is_some_and(|total| self.downloaded >= total);
        let due = self
            .last_sent
            .is_none_or(|at| at.elapsed() >= PROGRESS_EVERY);
        if complete || due {
            self.last_sent = Some(Instant::now());
            self.send(UpdateProgress::Progress {
                downloaded: self.downloaded,
                content_length,
            });
        }
    }

    fn finish(&mut self) {
        self.start(None);
        self.send(UpdateProgress::Finished);
    }
}

/// Check for an update, honouring the cooldown and the skip unless `manual`.
/// Never rejects: a check that cannot run says so in `state` and `reason`.
#[tauri::command(rename_all = "snake_case")]
pub async fn update_check(app: AppHandle, manual: bool) -> UpdateCheck {
    run_check(&app, manual).await
}

/// What this run knows about updates, read from managed state and the state
/// file and never from the endpoint: for Settings, and for a webview that
/// reloaded and lost an available or installed update.
#[tauri::command(rename_all = "snake_case")]
pub fn update_status(app: AppHandle, updates: tauri::State<'_, Updates>) -> UpdateStatus {
    let record = record_file(&app)
        .as_deref()
        .map(read_record)
        .unwrap_or_default();
    let held = updates.found().map(|u| u.version);
    status(
        current_version(&app),
        disabled_reason(),
        &record,
        held.as_deref(),
        updates.installed(),
    )
}

/// Record `version` as skipped: the silent check stops offering it, and a
/// manual check still does.
#[tauri::command(rename_all = "snake_case")]
pub fn update_skip(
    app: AppHandle,
    updates: tauri::State<'_, Updates>,
    version: String,
) -> Result<(), String> {
    let version = version.trim().to_string();
    if version.is_empty() {
        return Err("No version to skip.".into());
    }
    let file = record_file(&app).ok_or("The app has no data directory to record the skip in.")?;
    tracing::info!("[update] skipping {version}");
    updates.record(Some(&file), |r| r.skipped_version = Some(version));
    Ok(())
}

/// Download, verify and install the update the last check found (or a fresh
/// check's), reporting on `on_progress`. The running app and its daemon go
/// on as they are; `update_restart` switches to the new version.
#[tauri::command(rename_all = "snake_case")]
pub async fn update_install(
    app: AppHandle,
    on_progress: Channel<UpdateProgress>,
) -> Result<(), String> {
    if let Some(reason) = disabled_reason() {
        return Err(reason.to_string());
    }
    let current = current_version(&app);
    let updates = app.state::<Updates>().inner().clone();
    let _installing = updates.begin_install()?;
    let update = match updates.found() {
        Some(update) => update,
        None => {
            let found = fetch(&app).await.inspect_err(|why| {
                tracing::warn!("[update] the check before the install failed: {why}");
            })?;
            updates.hold(found.clone());
            found.ok_or_else(|| format!("mailypoppins {current} is up to date."))?
        }
    };
    let mut reporter = Reporter::new(on_progress);
    if updates.installed().as_deref() == Some(update.version.as_str()) {
        // Installed earlier in this run and not restarted yet: nothing to do.
        reporter.finish();
        return Ok(());
    }
    tracing::info!("[update] installing {} over {current}", update.version);
    let installed = update
        .download_and_install(|len, total| reporter.chunk(len, total), || {})
        .await;
    if let Err(e) = installed {
        let why = e.to_string();
        tracing::warn!("[update] installing {} failed: {why}", update.version);
        return Err(why);
    }
    tracing::info!("[update] {} is in place; a restart runs it", update.version);
    let to = update.version.clone();
    updates.record(record_file(&app).as_deref(), |r| {
        r.pending_restart = Some(PendingRestart {
            from: current.clone(),
            to: to.clone(),
        });
    });
    *lock(&updates.inner.installed) = Some(to);
    updates.hold(None);
    reporter.finish();
    Ok(())
}

/// Stop the daemon over the app's session with `daemon.stop`, wait for its
/// socket to go, then relaunch into the installed version, whose first
/// connect starts a daemon of its own version. The frontend asks about open
/// drafts first, as a window close does.
#[tauri::command(rename_all = "snake_case")]
pub async fn update_restart(app: AppHandle) -> Result<(), String> {
    let updates = app.state::<Updates>().inner().clone();
    let Some(version) = updates.installed() else {
        return Err("No update is installed yet, so there is nothing to restart into.".into());
    };
    let session = app.state::<SessionHandle>().inner().clone();
    tauri::async_runtime::spawn_blocking(move || stop_daemon(&session))
        .await
        .map_err(|e| format!("stopping the daemon failed: {e}"))?;
    tracing::info!("[update] restarting into {version}");
    app.request_restart();
    Ok(())
}

/// `daemon.stop` through the session, then wait for the socket to go. No
/// daemon is fine; a daemon that outlives the wait is logged and left to the
/// new app, whose first connect restarts it quietly (`pending_restart`).
fn stop_daemon(session: &SessionHandle) {
    if session.is_fixture() {
        return;
    }
    // The session reconnects on its own once the daemon goes; it must not
    // start one in the seconds before the relaunch.
    crate::connector::hold_autostart();
    match session.door(Duration::from_secs(2)) {
        Ok(door) => match door.call_within("daemon.stop", json!({}), STOP_BUDGET) {
            Ok(_) => tracing::info!("[update] daemon.stop answered"),
            Err(e) => tracing::warn!("[update] daemon.stop failed: {e:#}"),
        },
        Err(e) => tracing::info!("[update] no daemon session to stop: {e}"),
    }
    let socket = crate::paths::Paths::resolve().socket;
    let deadline = Instant::now() + SOCKET_GONE_WAIT;
    while socket.exists() {
        if Instant::now() >= deadline {
            tracing::warn!(
                "[update] {} is still there after {} s; relaunching anyway",
                socket.display(),
                SOCKET_GONE_WAIT.as_secs()
            );
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::scratch_dir;
    use serde_json::{json, Value};

    fn at(text: &str) -> OffsetDateTime {
        parse_time(text).expect("an RFC 3339 time")
    }

    #[test]
    fn a_silent_check_waits_out_the_cooldown_and_a_manual_one_never_does() {
        let now = at("2026-10-05T12:00:00Z");
        assert!(should_check(now, None, false));
        assert!(!should_check(now, Some(at("2026-10-05T11:00:00Z")), false));
        assert!(!should_check(now, Some(at("2026-10-04T12:00:01Z")), false));
        assert!(should_check(now, Some(at("2026-10-04T12:00:00Z")), false));
        assert!(should_check(now, Some(at("2026-09-01T00:00:00Z")), false));
        // A clock that went back does not hold checks off until it catches up.
        assert!(should_check(now, Some(at("2026-10-06T12:00:00Z")), false));
        assert!(should_check(now, Some(at("2026-10-05T11:00:00Z")), true));
    }

    #[test]
    fn a_skipped_version_is_offered_only_on_a_manual_check() {
        assert!(should_offer("0.12.0", None, false));
        assert!(!should_offer("0.12.0", Some("0.12.0"), false));
        assert!(!should_offer("0.12.0", Some("v0.12.0"), false));
        assert!(should_offer("0.12.0", Some("0.12.0"), true));
        assert!(should_offer("0.13.0", Some("0.12.0"), false));
    }

    fn update(from: &str, to: &str) -> PendingRestart {
        PendingRestart {
            from: from.into(),
            to: to.into(),
        }
    }

    /// The stage 2 rule: only the daemon of the update's `from`, met by the
    /// new app whose `mp` is the update's `to`, restarts without the screen.
    #[test]
    fn only_the_daemon_an_update_left_behind_restarts_quietly() {
        let done = update("0.11.0", "0.12.0");
        let p = Some(&done);
        assert!(restarts_quietly(p, "0.11.0", Some("0.12.0"), "0.12.0"));
        // No update recorded: the screen.
        assert!(!restarts_quietly(None, "0.11.0", Some("0.12.0"), "0.12.0"));
        // A daemon of another version than `from` (a cargo install, another app).
        assert!(!restarts_quietly(p, "0.10.0", Some("0.12.0"), "0.12.0"));
        // The old app in the Later window: it is `from`, and its reconnect
        // probes the new `mp`.
        assert!(!restarts_quietly(p, "0.11.0", Some("0.11.0"), "0.12.0"));
        // An app whose version is unknown (no Tauri app, a test).
        assert!(!restarts_quietly(p, "0.11.0", None, "0.12.0"));
        // The `mp` it would start is not the update's (MP_DESKTOP_MP_BIN).
        assert!(!restarts_quietly(p, "0.11.0", Some("0.12.0"), "0.13.0"));
    }

    #[test]
    fn the_quiet_restart_is_taken_once_and_leaves_the_rest_of_the_record() {
        let dir = scratch_dir("update-quiet");
        let file = state_path(&dir);
        let record = UpdateRecord {
            last_check: Some("2026-10-05T12:00:00Z".into()),
            skipped_version: Some("0.13.0".into()),
            pending_restart: Some(update("0.11.0", "0.12.0")),
        };
        write_record(&file, &record).expect("write");
        // Another mismatch takes nothing and keeps the record.
        assert_eq!(
            take_quiet_restart(&file, "0.10.0", Some("0.12.0"), "0.12.0"),
            None
        );
        assert_eq!(read_record(&file), record);
        assert_eq!(
            take_quiet_restart(&file, "0.11.0", Some("0.12.0"), "0.12.0"),
            Some(update("0.11.0", "0.12.0"))
        );
        assert_eq!(
            read_record(&file),
            UpdateRecord {
                pending_restart: None,
                ..record
            }
        );
        // A daemon that comes back at `from` again gets the screen.
        assert_eq!(
            take_quiet_restart(&file, "0.11.0", Some("0.12.0"), "0.12.0"),
            None
        );
    }

    #[test]
    fn a_daemon_at_the_updates_version_settles_it() {
        let dir = scratch_dir("update-settle");
        let file = state_path(&dir);
        let record = UpdateRecord {
            pending_restart: Some(update("0.11.0", "0.12.0")),
            ..UpdateRecord::default()
        };
        write_record(&file, &record).expect("write");
        // A dev build (0.1.0) sharing the data directory, or a daemon still
        // at `from`, leaves it for the new app.
        settle_update(&file, "0.1.0", Some("0.1.0"));
        settle_update(&file, "0.11.0", Some("0.12.0"));
        settle_update(&file, "0.12.0", None);
        assert_eq!(read_record(&file), record);
        settle_update(&file, "0.12.0", Some("0.12.0"));
        assert_eq!(read_record(&file), UpdateRecord::default());
    }

    #[test]
    fn the_startup_drops_an_update_to_another_version_only() {
        let mut record = UpdateRecord {
            pending_restart: Some(update("0.11.0", "0.12.0")),
            ..UpdateRecord::default()
        };
        assert_eq!(drop_stale_update(&mut record, "0.12.0"), None);
        assert!(record.pending_restart.is_some());
        assert_eq!(
            drop_stale_update(&mut record, "0.11.0"),
            Some(update("0.11.0", "0.12.0"))
        );
        assert_eq!(record.pending_restart, None);
    }

    #[test]
    fn a_change_that_changes_nothing_writes_nothing() {
        let dir = scratch_dir("update-unchanged");
        let file = state_path(&dir);
        change_record(&file, |r| r.pending_restart = None);
        assert!(!file.exists());
        change_record(&file, |r| r.skipped_version = Some("0.12.0".into()));
        assert_eq!(
            read_record(&file).skipped_version.as_deref(),
            Some("0.12.0")
        );
    }

    /// After an install and Later, a manual check says the restart is
    /// pending instead of offering the installed version again.
    #[test]
    fn a_check_after_an_install_answers_restart_pending_without_fetching() {
        let last = Some("2026-10-05T12:00:00Z".to_string());
        assert_eq!(answer_before_fetch("0.11.0", None, last.clone()), None);
        let pending = answer_before_fetch("0.11.0", Some("0.12.0".into()), last)
            .expect("a restart is pending");
        assert_eq!(
            serde_json::to_value(&pending).unwrap(),
            json!({
                "state": "restart_pending", "current": "0.11.0", "version": "0.12.0",
                "last_check": "2026-10-05T12:00:00Z",
            })
        );
    }

    #[test]
    fn only_a_release_build_in_a_bundle_outside_the_fixture_checks() {
        let bundled = Path::new("/Applications/mailypoppins.app/Contents/MacOS/mailypoppins");
        let loose = Path::new("/Users/me/code/mailypoppins/target/release/mp-desktop");
        assert_eq!(gate(false, false, Some(bundled)), None);
        assert!(gate(true, false, Some(bundled)).is_some());
        assert!(gate(false, true, Some(bundled)).is_some());
        assert!(gate(false, false, Some(loose)).is_some());
        assert!(gate(false, false, None).is_some());
        assert!(in_app_bundle(Path::new(
            "/Users/me/Applications/mailypoppins.app/Contents/MacOS/mailypoppins"
        )));
        assert!(!in_app_bundle(Path::new("/opt/mailypoppins.app")));
    }

    /// `cargo test` builds with debug assertions, so this process never checks.
    #[test]
    fn a_test_build_never_checks() {
        if cfg!(debug_assertions) {
            assert!(!checks_enabled());
            assert_eq!(
                disabled_reason(),
                Some("Updates are off in a development build.")
            );
        }
    }

    #[test]
    fn the_state_file_round_trips_and_is_replaced_whole() {
        let dir = scratch_dir("update-state");
        let file = state_path(&dir.join("nested"));
        assert_eq!(read_record(&file), UpdateRecord::default());
        let record = UpdateRecord {
            last_check: Some("2026-10-05T12:00:00Z".into()),
            skipped_version: Some("0.12.0".into()),
            pending_restart: Some(PendingRestart {
                from: "0.11.0".into(),
                to: "0.12.0".into(),
            }),
        };
        write_record(&file, &record).expect("write");
        assert_eq!(read_record(&file), record);
        let on_disk: Value =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(
            on_disk,
            json!({
                "last_check": "2026-10-05T12:00:00Z",
                "skipped_version": "0.12.0",
                "pending_restart": {"from": "0.11.0", "to": "0.12.0"},
            })
        );
        let left: Vec<_> = std::fs::read_dir(file.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left, vec![STATE_FILE.to_string()]);
        write_record(&file, &UpdateRecord::default()).expect("write");
        let on_disk: Value =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(
            on_disk,
            json!({"last_check": null, "skipped_version": null, "pending_restart": null})
        );
    }

    #[test]
    fn a_broken_or_partial_state_file_reads_as_empty_or_as_far_as_it_goes() {
        let dir = scratch_dir("update-state-broken");
        let file = state_path(&dir);
        for broken in ["", "not json", "[1, 2]", "{\"last_check\": 5}"] {
            std::fs::write(&file, broken).unwrap();
            assert_eq!(read_record(&file), UpdateRecord::default(), "{broken:?}");
        }
        std::fs::write(&file, "{\"skipped_version\": \"0.12.0\", \"later\": true}").unwrap();
        assert_eq!(
            read_record(&file),
            UpdateRecord {
                skipped_version: Some("0.12.0".into()),
                ..UpdateRecord::default()
            }
        );
    }

    #[test]
    fn a_time_round_trips_as_rfc3339() {
        let t = at("2026-10-05T12:34:56Z");
        assert_eq!(format_time(t), "2026-10-05T12:34:56Z");
        assert_eq!(parse_time(&format_time(t)), Some(t));
        assert_eq!(parse_time("yesterday"), None);
    }

    #[test]
    fn the_answers_and_the_progress_have_the_documented_shapes() {
        let check = UpdateCheck {
            version: Some("0.12.0".into()),
            last_check: Some("2026-10-05T12:00:00Z".into()),
            ..UpdateCheck::bare(UpdateCheckState::Available, "0.11.0".into())
        };
        assert_eq!(
            serde_json::to_value(&check).unwrap(),
            json!({
                "state": "available", "current": "0.11.0", "version": "0.12.0",
                "last_check": "2026-10-05T12:00:00Z",
            })
        );
        assert_eq!(
            serde_json::to_value(UpdateCheck::disabled("0.1.0".into(), "why")).unwrap(),
            json!({"state": "disabled", "current": "0.1.0", "reason": "why"})
        );
        assert_eq!(
            serde_json::to_value(UpdateProgress::Started {
                content_length: Some(10)
            })
            .unwrap(),
            json!({"type": "started", "content_length": 10})
        );
        assert_eq!(
            serde_json::to_value(UpdateProgress::Started {
                content_length: None
            })
            .unwrap(),
            json!({"type": "started"})
        );
        assert_eq!(
            serde_json::to_value(UpdateProgress::Progress {
                downloaded: 4,
                content_length: Some(10)
            })
            .unwrap(),
            json!({"type": "progress", "downloaded": 4, "content_length": 10})
        );
        assert_eq!(
            serde_json::to_value(UpdateProgress::Finished).unwrap(),
            json!({"type": "finished"})
        );
        assert_eq!(
            serde_json::to_value(UpdateAvailable {
                version: "0.12.0".into(),
                notes: None,
                date: None
            })
            .unwrap(),
            json!({"version": "0.12.0"})
        );
    }

    #[test]
    fn the_status_reads_the_record_and_the_held_versions_and_hides_a_skipped_one() {
        let record = UpdateRecord {
            last_check: Some("2026-10-05T12:00:00Z".into()),
            skipped_version: Some("0.12.0".into()),
            pending_restart: None,
        };
        assert_eq!(
            serde_json::to_value(status("0.11.0".into(), None, &record, Some("0.13.0"), None))
                .unwrap(),
            json!({
                "current": "0.11.0", "enabled": true,
                "last_check": "2026-10-05T12:00:00Z", "available": "0.13.0",
            })
        );
        let skipped = status("0.11.0".into(), None, &record, Some("v0.12.0"), None);
        assert_eq!(skipped.available, None);
        let installed = status(
            "0.11.0".into(),
            None,
            &UpdateRecord::default(),
            None,
            Some("0.12.0".into()),
        );
        assert_eq!(
            serde_json::to_value(installed).unwrap(),
            json!({"current": "0.11.0", "enabled": true, "installed": "0.12.0"})
        );
        assert_eq!(
            serde_json::to_value(status(
                "0.1.0".into(),
                Some("why"),
                &UpdateRecord::default(),
                None,
                None
            ))
            .unwrap(),
            json!({"current": "0.1.0", "enabled": false, "reason": "why"})
        );
    }

    /// A `.sig` file as `tauri build` writes it: the base64 of a minisign
    /// signature whose trusted comment carries the version.
    const SIG: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IHNpZ25hdHVyZSBmcm9tIHRhdXJpIHNlY3JldCBrZXkKUlVUSGJKQzQ5YWwxTGdBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBPQp0cnVzdGVkIGNvbW1lbnQ6IHRpbWVzdGFtcDoxNzkxMTkzNjAwCWZpbGU6bWFpbHlwb3BwaW5zLmFwcC50YXIuZ3oJdmVyc2lvbjowLjEyLjAKQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUE9PQo=";

    /// What the release workflow's `desktop-manifest` job writes with
    /// `jq -n --rawfile` (the `.sig` with its trailing newline trimmed).
    const LATEST_JSON: &str = r#"{
  "version": "0.12.0",
  "notes": "Added\n\n- The desktop app updates itself.\n",
  "pub_date": "2026-10-05T12:00:00Z",
  "platforms": {
    "darwin-aarch64": {
      "signature": "SIGNATURE",
      "url": "https://github.com/sylvainHellin/mailypoppins/releases/download/v0.12.0/mailypoppins-desktop-aarch64-apple-darwin.app.tar.gz"
    }
  }
}"#;

    #[test]
    fn the_workflow_manifest_parses_with_the_plugins_own_type() {
        let text = LATEST_JSON.replace("SIGNATURE", SIG);
        let release: tauri_plugin_updater::RemoteRelease =
            serde_json::from_str(&text).expect("latest.json parses");
        assert_eq!(release.version.to_string(), "0.12.0");
        assert_eq!(
            release.notes.as_deref(),
            Some("Added\n\n- The desktop app updates itself.\n")
        );
        assert_eq!(
            release.pub_date.map(format_time).as_deref(),
            Some("2026-10-05T12:00:00Z")
        );
        assert_eq!(
            release.download_url("darwin-aarch64").expect("url").as_str(),
            "https://github.com/sylvainHellin/mailypoppins/releases/download/v0.12.0/mailypoppins-desktop-aarch64-apple-darwin.app.tar.gz"
        );
        assert_eq!(release.signature("darwin-aarch64").expect("sig"), SIG);
        assert!(release.download_url("darwin-x86_64").is_err());
    }
}
