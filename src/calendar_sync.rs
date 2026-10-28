//! The calendar sync of one account (PERSO-200, `docs/plans/calendar.md`,
//! "Sync"): the backend's calendars and the events of the rolling window into
//! the store's calendar cache ([`crate::store::calendar`]).
//!
//! A sync lists the calendars, stores them, and syncs each calendar's window
//! from its stored token. The token is passed only while it is still good for
//! the window: a token is bound to the window it was minted for (Graph's
//! delta), so a moved window, or a baseline older than
//! [`REBASELINE_AFTER`], starts a fresh baseline instead.
//!
//! Not on the daemon's tick yet: the next unit of PERSO-200 wires it in.

use chrono::{DateTime, Duration, Utc};
use log::{info, warn};

use crate::calendar_backend::{BackendResult, CalendarBackend, CalendarBackendError};
use crate::calendar_model::window_for;
use crate::store::calendar as cache;
use crate::store::Store;

/// How old a calendar's last baseline may grow before the next sync takes a
/// fresh one over the current window (the daily re-baseline of the plan's
/// "Sync"). The window is day-aligned ([`window_for`]), so it moves at the
/// first sync of each UTC day anyway; the age bound also covers a clock that
/// went backwards.
pub const REBASELINE_AFTER: Duration = Duration::days(1);

/// What one account sync did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SyncReport {
    /// The calendars the backend listed.
    pub calendars: usize,
    /// Events written, across calendars.
    pub upserts: usize,
    /// Events removed by an incremental sync, across calendars.
    pub removed: usize,
    /// Calendars whose window was replaced by a baseline.
    pub baselines: usize,
    /// Calendars that failed, with why; the others synced regardless.
    pub failed: Vec<(String, String)>,
}

/// Sync `account`'s calendars from `backend` into `store` as of `now`.
///
/// A missing scope fails the whole account, so the clients can prompt for a
/// new sign-in; any other failure of one calendar is reported in
/// [`SyncReport::failed`] and leaves that calendar's cache and token as they
/// were.
pub async fn sync_account<B: CalendarBackend>(
    store: &Store,
    backend: &B,
    account: &str,
    now: DateTime<Utc>,
) -> BackendResult<SyncReport> {
    let calendars = backend.list_calendars().await?;
    cache::upsert_calendars(store, account, &calendars)?;
    backend.remember_series(cache::load_series(store, account)?);
    let window = window_for(now);
    let mut report = SyncReport {
        calendars: calendars.len(),
        ..Default::default()
    };
    for calendar in &calendars {
        let state = cache::load_sync_state(store, account, &calendar.id)?;
        let token = state
            .filter(|state| state.window == window && now - state.baselined_at < REBASELINE_AFTER)
            .and_then(|state| state.token);
        let baseline = token.is_none();
        match backend.sync_window(calendar, window, token).await {
            Ok(mut result) => {
                // A sync asked for without a token is a baseline whatever the
                // backend says: the rows of a former window must go.
                result.full_resync |= baseline;
                cache::apply_sync(store, account, &calendar.id, &result, window, now)?;
                report.upserts += result.upserts.len();
                report.removed += result.removed.len();
                report.baselines += usize::from(result.full_resync);
            }
            Err(err @ CalendarBackendError::MissingScope { .. }) => return Err(err),
            Err(err) => {
                warn!(
                    "[calendar] {account}: syncing {} failed: {err}",
                    calendar.name
                );
                report.failed.push((calendar.id.clone(), err.to_string()));
            }
        }
    }
    info!(
        "[calendar] {account}: {} calendar(s), {} event(s) written, {} removed, {} baseline(s), \
         {} failed",
        report.calendars,
        report.upserts,
        report.removed,
        report.baselines,
        report.failed.len()
    );
    Ok(report)
}

/// Drop `account`'s calendar cache, keeping which calendars are hidden, and
/// sync it afresh (`calendar.rebuild` on an account with a backend).
pub async fn rebuild_account<B: CalendarBackend>(
    store: &Store,
    backend: &B,
    account: &str,
    now: DateTime<Utc>,
) -> BackendResult<SyncReport> {
    cache::drop_calendar_cache(store, account)?;
    sync_account(store, backend, account, now).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calendar_backend::fake::FakeCalendarBackend;
    use crate::calendar_backend::SyncResult;
    use crate::calendar_model::{CalendarWindow, Event, EventTime};
    use chrono::TimeZone;

    fn open() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("store.sqlite3")).unwrap();
        (dir, store)
    }

    fn at(d: u32, h: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, d, h, 0, 0).unwrap()
    }

    fn november() -> CalendarWindow {
        CalendarWindow {
            start: at(1, 0),
            end: Utc.with_ymd_and_hms(2026, 12, 1, 0, 0, 0).unwrap(),
        }
    }

    fn cached(store: &Store, visible_only: bool) -> Vec<String> {
        cache::events_in_range(store, &["evoqs"], november(), chrono_tz::UTC, visible_only)
            .unwrap()
            .into_iter()
            .map(|e| e.id)
            .collect()
    }

    fn event(fake: &FakeCalendarBackend, id: &str) -> Event {
        fake.events().into_iter().find(|e| e.id == id).unwrap()
    }

    #[tokio::test]
    async fn a_second_sync_brings_the_additions_changes_and_deletions() {
        let (_dir, store) = open();
        let fake = FakeCalendarBackend::canned();
        let first = sync_account(&store, &fake, "evoqs", at(1, 8))
            .await
            .unwrap();
        assert_eq!(first.calendars, 2);
        assert_eq!(first.baselines, 2);
        assert_eq!(
            cached(&store, false),
            ["ev-meeting", "ev-holiday", "ev-weekly-2"]
        );

        let mut meeting = event(&fake, "ev-meeting");
        meeting.summary = "Project review, moved".into();
        meeting.time = EventTime::timed(at(4, 9), at(4, 10), "Europe/Berlin").unwrap();
        fake.put_event(meeting.clone());
        let mut added = meeting.clone();
        added.id = "ev-new".into();
        added.uid = "ev-new@example.com".into();
        added.time = EventTime::timed(at(2, 9), at(2, 10), "Europe/Berlin").unwrap();
        fake.put_event(added);
        fake.remove_event("ev-weekly-2");

        let second = sync_account(&store, &fake, "evoqs", at(1, 9))
            .await
            .unwrap();
        assert_eq!(second.baselines, 0);
        assert_eq!(second.upserts, 2);
        assert_eq!(
            cached(&store, false),
            ["ev-new", "ev-meeting", "ev-holiday"]
        );
        assert_eq!(
            cache::get_event(&store, "evoqs", "ev-meeting").unwrap(),
            Some(meeting)
        );
        let state = cache::load_sync_state(&store, "evoqs", "cal-main")
            .unwrap()
            .unwrap();
        assert_eq!(state.baselined_at, at(1, 8));
        assert_eq!(state.synced_at, at(1, 9));
        assert_eq!(state.path.as_deref(), Some("fake"));
    }

    #[tokio::test]
    async fn a_refused_token_replaces_the_cached_window() {
        let (_dir, store) = open();
        let fake = FakeCalendarBackend::canned();
        sync_account(&store, &fake, "evoqs", at(1, 8))
            .await
            .unwrap();
        // A row the server never had, which only a replacement removes.
        let mut ghost = event(&fake, "ev-meeting");
        ghost.id = "ev-ghost".into();
        let stray = SyncResult {
            upserts: vec![ghost],
            token: cache::load_sync_state(&store, "evoqs", "cal-main")
                .unwrap()
                .unwrap()
                .token,
            ..Default::default()
        };
        cache::apply_sync(
            &store,
            "evoqs",
            "cal-main",
            &stray,
            window_for(at(1, 8)),
            at(1, 8),
        )
        .unwrap();
        assert!(cached(&store, false).contains(&"ev-ghost".to_string()));

        fake.expire_tokens();
        let report = sync_account(&store, &fake, "evoqs", at(1, 9))
            .await
            .unwrap();
        assert_eq!(report.baselines, 2);
        assert_eq!(
            cached(&store, false),
            ["ev-meeting", "ev-holiday", "ev-weekly-2"]
        );
        let state = cache::load_sync_state(&store, "evoqs", "cal-main")
            .unwrap()
            .unwrap();
        assert_eq!(state.baselined_at, at(1, 9));
    }

    #[tokio::test]
    async fn a_rebuild_keeps_the_hidden_calendars_hidden() {
        let (_dir, store) = open();
        let fake = FakeCalendarBackend::canned();
        sync_account(&store, &fake, "evoqs", at(1, 8))
            .await
            .unwrap();
        cache::set_calendar_visible(&store, "evoqs", "cal-second", false).unwrap();

        let report = rebuild_account(&store, &fake, "evoqs", at(1, 9))
            .await
            .unwrap();
        assert_eq!(report.baselines, 2);
        let calendars = cache::list_calendars(&store, "evoqs").unwrap();
        let visibility: Vec<_> = calendars
            .iter()
            .map(|c| (c.id.as_str(), c.visible))
            .collect();
        assert_eq!(visibility, [("cal-main", true), ("cal-second", false)]);
        assert_eq!(
            cached(&store, false),
            ["ev-meeting", "ev-holiday", "ev-weekly-2"]
        );
        assert_eq!(cached(&store, true), ["ev-meeting", "ev-weekly-2"]);
    }

    #[tokio::test]
    async fn a_moved_window_or_a_day_old_baseline_starts_afresh() {
        let (_dir, store) = open();
        let fake = FakeCalendarBackend::canned();
        sync_account(&store, &fake, "evoqs", at(1, 8))
            .await
            .unwrap();
        let same_day = sync_account(&store, &fake, "evoqs", at(1, 20))
            .await
            .unwrap();
        assert_eq!(same_day.baselines, 0);

        // The next UTC day moves the window.
        let next_day = sync_account(&store, &fake, "evoqs", at(2, 1))
            .await
            .unwrap();
        assert_eq!(next_day.baselines, 2);
        let state = cache::load_sync_state(&store, "evoqs", "cal-main")
            .unwrap()
            .unwrap();
        assert_eq!(state.window, window_for(at(2, 1)));
        assert_eq!(state.baselined_at, at(2, 1));

        // Same window, but a baseline more than a day old.
        let mut aged = state.clone();
        aged.baselined_at = at(2, 1) - REBASELINE_AFTER - Duration::seconds(1);
        cache::save_sync_state(&store, "evoqs", "cal-main", &aged).unwrap();
        let aged_sync = sync_account(&store, &fake, "evoqs", at(2, 2))
            .await
            .unwrap();
        assert_eq!(
            aged_sync.baselines, 1,
            "only the aged calendar re-baselines"
        );
        let state = cache::load_sync_state(&store, "evoqs", "cal-main")
            .unwrap()
            .unwrap();
        assert_eq!(state.baselined_at, at(2, 2));
    }

    #[tokio::test]
    async fn a_missing_scope_fails_the_account_and_writes_nothing() {
        let (_dir, store) = open();
        let fake = FakeCalendarBackend::canned().missing_scope();
        let err = sync_account(&store, &fake, "evoqs", at(1, 8))
            .await
            .unwrap_err();
        assert!(matches!(err, CalendarBackendError::MissingScope { .. }));
        assert!(cache::list_calendars(&store, "evoqs").unwrap().is_empty());
    }
}
