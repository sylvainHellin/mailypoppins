//! The calendar cache of an account (PERSO-200, `docs/plans/calendar.md`,
//! "Event store").
//!
//! A disposable cache of the rolling window (D6): the server is the source of
//! truth, so [`drop_calendar_cache`] and a resync lose nothing. Six tables,
//! all created by [`super::schema::ensure_additive_tables`]:
//!
//! - [`CALENDARS_TABLE`]: the account's calendars as the server last listed
//!   them, in the server's order.
//! - [`PREFS_TABLE`]: the local visibility of each calendar. Its own table
//!   rather than a column of the calendar list, so dropping the cache is a
//!   plain `DELETE` of every cache table and the preference needs no carrying
//!   across it; a calendar with no row is visible. A schema-version rebuild of
//!   the whole store does carry it (`super::rebuild`).
//! - [`EVENTS_TABLE`]: the expanded occurrences of the window, one row per
//!   server id, with the serialised [`Event`] in `event_json` and the columns
//!   a range query filters on beside it.
//! - [`SYNC_TABLE`]: each calendar's sync token, with the window it was minted
//!   for (a Graph delta token is bound to it), when the last baseline ran,
//!   and the path the backend synced through.
//! - [`SERIES_TABLE`]: what a backend resolved about a recurring series
//!   ([`SeriesInfo`]), so Graph asks for a series master once rather than at
//!   every poll.
//! - [`INVITES_TABLE`]: the server event an invitation mail stands for, by
//!   its `messages.id` and the server id it was looked up under, so
//!   `message.invite` asks the server once per invitation mail (a mail that
//!   stands for no event is remembered too, with a `NULL` event).

use std::collections::HashSet;

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, NaiveDate, NaiveTime, Utc};
use chrono_tz::Tz;
use log::warn;
use rusqlite::{params, OptionalExtension};

use super::Store;
use crate::calendar_backend::{SeriesInfo, SyncResult};
use crate::calendar_model::{Calendar, CalendarWindow, Event, EventKey, EventTime};

pub const CALENDARS_TABLE: &str = "calendars";
pub const PREFS_TABLE: &str = "calendar_prefs";
pub const EVENTS_TABLE: &str = "calendar_events";
pub const SYNC_TABLE: &str = "calendar_sync";
pub const SERIES_TABLE: &str = "calendar_series";
pub const INVITES_TABLE: &str = "calendar_invite_events";

/// The `CREATE` statements of the six tables and their indexes, as
/// `(table, statements)` for [`super::schema::ensure_additive_tables`].
pub fn create_sql() -> [(&'static str, String); 6] {
    [
        (
            CALENDARS_TABLE,
            format!(
                "CREATE TABLE IF NOT EXISTS {CALENDARS_TABLE} (
                    account    TEXT NOT NULL,
                    id         TEXT NOT NULL,
                    name       TEXT NOT NULL,
                    color      TEXT,
                    is_default INTEGER NOT NULL DEFAULT 0,
                    can_edit   INTEGER NOT NULL DEFAULT 0,
                    position   INTEGER NOT NULL DEFAULT 0,
                    PRIMARY KEY (account, id)
                )"
            ),
        ),
        (
            PREFS_TABLE,
            format!(
                "CREATE TABLE IF NOT EXISTS {PREFS_TABLE} (
                    account     TEXT NOT NULL,
                    calendar_id TEXT NOT NULL,
                    visible     INTEGER NOT NULL DEFAULT 1,
                    PRIMARY KEY (account, calendar_id)
                )"
            ),
        ),
        (
            EVENTS_TABLE,
            // `start_utc` and `end_utc` are unix seconds. An all-day row has
            // no instants of its own, so it carries the widest pair any viewer
            // can see it at (see `all_day_bounds`) and a range query refines
            // the match with the viewer's zone.
            format!(
                "CREATE TABLE IF NOT EXISTS {EVENTS_TABLE} (
                    account       TEXT NOT NULL,
                    calendar_id   TEXT NOT NULL,
                    event_id      TEXT NOT NULL,
                    uid           TEXT NOT NULL,
                    recurrence_id TEXT,
                    start_utc     INTEGER NOT NULL,
                    end_utc       INTEGER NOT NULL,
                    all_day       INTEGER NOT NULL DEFAULT 0,
                    start_date    TEXT,
                    end_date      TEXT,
                    zone          TEXT,
                    last_modified INTEGER,
                    event_json    TEXT NOT NULL,
                    PRIMARY KEY (account, calendar_id, event_id)
                );
                CREATE INDEX IF NOT EXISTS calendar_events_event
                    ON {EVENTS_TABLE} (account, event_id);
                CREATE INDEX IF NOT EXISTS calendar_events_window
                    ON {EVENTS_TABLE} (account, start_utc, end_utc);
                CREATE INDEX IF NOT EXISTS calendar_events_key
                    ON {EVENTS_TABLE} (uid, recurrence_id)"
            ),
        ),
        (
            SYNC_TABLE,
            format!(
                "CREATE TABLE IF NOT EXISTS {SYNC_TABLE} (
                    account      TEXT NOT NULL,
                    calendar_id  TEXT NOT NULL,
                    token        TEXT,
                    window_start INTEGER NOT NULL,
                    window_end   INTEGER NOT NULL,
                    baselined_at INTEGER NOT NULL,
                    synced_at    INTEGER NOT NULL,
                    path         TEXT,
                    PRIMARY KEY (account, calendar_id)
                )"
            ),
        ),
        (
            SERIES_TABLE,
            format!(
                "CREATE TABLE IF NOT EXISTS {SERIES_TABLE} (
                    account    TEXT NOT NULL,
                    master_id  TEXT NOT NULL,
                    uid        TEXT NOT NULL,
                    recurrence TEXT,
                    PRIMARY KEY (account, master_id)
                )"
            ),
        ),
        (
            INVITES_TABLE,
            format!(
                "CREATE TABLE IF NOT EXISTS {INVITES_TABLE} (
                    account     TEXT NOT NULL,
                    message_row INTEGER NOT NULL,
                    server_id   TEXT NOT NULL,
                    event_id    TEXT,
                    PRIMARY KEY (account, message_row)
                )"
            ),
        ),
    ]
}

/// A calendar's sync state: the token of its last sync and what it is bound
/// to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncState {
    /// `None` when the backend has no incremental sync for the calendar.
    pub token: Option<String>,
    /// The window the token was minted for.
    pub window: CalendarWindow,
    /// When the last full baseline of the window ran.
    pub baselined_at: DateTime<Utc>,
    /// When the last sync of any kind ran.
    pub synced_at: DateTime<Utc>,
    /// How the backend reached the window ([`SyncResult::path`]).
    pub path: Option<String>,
}

/// Store the calendar list of `account` as the server reports it, in its
/// order. A calendar gone from the list takes its events and its sync state
/// with it; its visibility stays, so a calendar that comes back keeps it.
pub fn upsert_calendars(store: &Store, account: &str, calendars: &[Calendar]) -> Result<()> {
    let tx = store.immediate_transaction()?;
    let known: Vec<String> = {
        let mut stmt = tx.prepare(&format!(
            "SELECT id FROM {CALENDARS_TABLE} WHERE account = ?1"
        ))?;
        let ids = stmt
            .query_map([account], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        ids
    };
    for gone in known
        .iter()
        .filter(|id| !calendars.iter().any(|cal| &cal.id == *id))
    {
        tx.execute(
            &format!("DELETE FROM {CALENDARS_TABLE} WHERE account = ?1 AND id = ?2"),
            [account, gone],
        )?;
        for table in [EVENTS_TABLE, SYNC_TABLE] {
            tx.execute(
                &format!("DELETE FROM {table} WHERE account = ?1 AND calendar_id = ?2"),
                [account, gone],
            )?;
        }
    }
    for (position, calendar) in calendars.iter().enumerate() {
        tx.execute(
            &format!(
                "INSERT INTO {CALENDARS_TABLE}
                    (account, id, name, color, is_default, can_edit, position)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT (account, id) DO UPDATE SET
                    name = excluded.name, color = excluded.color,
                    is_default = excluded.is_default, can_edit = excluded.can_edit,
                    position = excluded.position"
            ),
            params![
                account,
                calendar.id,
                calendar.name,
                calendar.color,
                calendar.is_default,
                calendar.can_edit,
                position as i64,
            ],
        )?;
    }
    tx.commit().context("writing the calendar list")
}

/// Show or hide one calendar of `account` in the views.
pub fn set_calendar_visible(
    store: &Store,
    account: &str,
    calendar_id: &str,
    visible: bool,
) -> Result<()> {
    store
        .conn()
        .execute(
            &format!(
                "INSERT INTO {PREFS_TABLE} (account, calendar_id, visible) VALUES (?1, ?2, ?3)
                 ON CONFLICT (account, calendar_id) DO UPDATE SET visible = excluded.visible"
            ),
            params![account, calendar_id, visible],
        )
        .context("writing a calendar's visibility")?;
    Ok(())
}

/// Whether one calendar of `account` is shown: its preference, else visible.
/// The one read for a calendar the server list does not carry, the virtual
/// invitation calendar.
pub fn calendar_visible(store: &Store, account: &str, calendar_id: &str) -> Result<bool> {
    let visible: Option<bool> = store
        .conn()
        .query_row(
            &format!("SELECT visible FROM {PREFS_TABLE} WHERE account = ?1 AND calendar_id = ?2"),
            [account, calendar_id],
            |row| row.get(0),
        )
        .optional()
        .context("reading a calendar's visibility")?;
    Ok(visible.unwrap_or(true))
}

/// The server event remembered for the invitation mail `message_row`, when
/// it was looked up under `server_id`: `Some(None)` for a mail remembered as
/// standing for no event, `None` when it was never looked up (or under
/// another server id, which a move gives it).
pub fn cached_invite_event(
    store: &Store,
    account: &str,
    message_row: i64,
    server_id: &str,
) -> Result<Option<Option<String>>> {
    store
        .conn()
        .query_row(
            &format!(
                "SELECT event_id FROM {INVITES_TABLE}
                 WHERE account = ?1 AND message_row = ?2 AND server_id = ?3"
            ),
            params![account, message_row, server_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()
        .context("reading an invitation's server event")
}

/// Remember the server event of the invitation mail `message_row`, looked up
/// under `server_id`.
pub fn remember_invite_event(
    store: &Store,
    account: &str,
    message_row: i64,
    server_id: &str,
    event_id: Option<&str>,
) -> Result<()> {
    store
        .conn()
        .execute(
            &format!(
                "INSERT INTO {INVITES_TABLE} (account, message_row, server_id, event_id)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (account, message_row) DO UPDATE SET
                    server_id = excluded.server_id, event_id = excluded.event_id"
            ),
            params![account, message_row, server_id, event_id],
        )
        .context("writing an invitation's server event")?;
    Ok(())
}

/// The calendars of `account` in the server's order, each with its local
/// visibility.
pub fn list_calendars(store: &Store, account: &str) -> Result<Vec<Calendar>> {
    let mut stmt = store.conn().prepare(&format!(
        "SELECT c.account, c.id, c.name, c.color, c.is_default, c.can_edit,
                COALESCE(p.visible, 1)
         FROM {CALENDARS_TABLE} c
         LEFT JOIN {PREFS_TABLE} p ON p.account = c.account AND p.calendar_id = c.id
         WHERE c.account = ?1
         ORDER BY c.position, c.id"
    ))?;
    let calendars = stmt
        .query_map([account], |row| {
            Ok(Calendar {
                account: row.get(0)?,
                id: row.get(1)?,
                name: row.get(2)?,
                color: row.get(3)?,
                is_default: row.get(4)?,
                can_edit: row.get(5)?,
                visible: row.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("reading the calendar list")?;
    Ok(calendars)
}

/// Fold one sync of `calendar_id` into the cache, with its token, in one
/// transaction: a `full_resync` replaces every cached event of the calendar,
/// anything else merges its upserts and removals. A baseline stamps
/// `baselined_at` with `now`; an incremental sync keeps the stamp it had.
pub fn apply_sync(
    store: &Store,
    account: &str,
    calendar_id: &str,
    result: &SyncResult,
    window: CalendarWindow,
    now: DateTime<Utc>,
) -> Result<()> {
    let tx = store.immediate_transaction()?;
    if result.full_resync {
        tx.execute(
            &format!("DELETE FROM {EVENTS_TABLE} WHERE account = ?1 AND calendar_id = ?2"),
            [account, calendar_id],
        )?;
    }
    for id in &result.removed {
        tx.execute(
            &format!(
                "DELETE FROM {EVENTS_TABLE}
                 WHERE account = ?1 AND calendar_id = ?2 AND event_id = ?3"
            ),
            [account, calendar_id, id],
        )?;
    }
    for event in &result.upserts {
        write_event(&tx, account, calendar_id, event)?;
    }
    for series in &result.series {
        write_series(&tx, account, series)?;
    }
    let baselined_at = if result.full_resync {
        Some(now)
    } else {
        load_sync_state_in(&tx, account, calendar_id)?.map(|state| state.baselined_at)
    };
    save_sync_state_in(
        &tx,
        account,
        calendar_id,
        &SyncState {
            token: result.token.clone(),
            window,
            // A merge with no baseline behind it (a state lost under a token
            // a backend still honours) counts from now.
            baselined_at: baselined_at.unwrap_or(now),
            synced_at: now,
            path: result.path.clone(),
        },
    )?;
    tx.commit().context("applying a calendar sync")
}

fn write_event(
    conn: &rusqlite::Connection,
    account: &str,
    calendar_id: &str,
    event: &Event,
) -> Result<()> {
    let (start_utc, end_utc, start_date, end_date, zone) = match &event.time {
        EventTime::Timed { start, end, zone } => (*start, *end, None, None, Some(zone.as_str())),
        EventTime::AllDay { start, end } => {
            let (wide_start, wide_end) = all_day_bounds(*start, *end);
            (
                wide_start,
                wide_end,
                Some(start.to_string()),
                Some(end.to_string()),
                None,
            )
        }
    };
    let json = serde_json::to_string(event).context("serialising a calendar event")?;
    conn.execute(
        &format!(
            "INSERT INTO {EVENTS_TABLE}
                (account, calendar_id, event_id, uid, recurrence_id, start_utc, end_utc,
                 all_day, start_date, end_date, zone, last_modified, event_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
             ON CONFLICT (account, calendar_id, event_id) DO UPDATE SET
                uid = excluded.uid, recurrence_id = excluded.recurrence_id,
                start_utc = excluded.start_utc, end_utc = excluded.end_utc,
                all_day = excluded.all_day, start_date = excluded.start_date,
                end_date = excluded.end_date, zone = excluded.zone,
                last_modified = excluded.last_modified, event_json = excluded.event_json"
        ),
        params![
            account,
            calendar_id,
            event.id,
            event.uid,
            event.recurrence_id,
            start_utc.timestamp(),
            end_utc.timestamp(),
            event.is_all_day(),
            start_date,
            end_date,
            zone,
            event.last_modified.map(|at| at.timestamp()),
            json,
        ],
    )
    .with_context(|| format!("writing calendar event {}", event.id))?;
    Ok(())
}

/// The widest instants an all-day event on `start..end` covers for any
/// viewer: local midnight comes first at UTC+14 and last at UTC-12.
fn all_day_bounds(start: NaiveDate, end: NaiveDate) -> (DateTime<Utc>, DateTime<Utc>) {
    let midnight = |date: NaiveDate| date.and_time(NaiveTime::MIN).and_utc();
    (
        midnight(start) - Duration::hours(14),
        midnight(end) + Duration::hours(12),
    )
}

/// The cached events in `window`, seen from `viewer` (which places an all-day
/// event's midnights), ordered by start. `accounts` empty means every account
/// of the store; `visible_only` leaves out the hidden calendars.
pub fn events_in_range(
    store: &Store,
    accounts: &[&str],
    window: CalendarWindow,
    viewer: Tz,
    visible_only: bool,
) -> Result<Vec<Event>> {
    let mut sql = format!(
        "SELECT e.event_json FROM {EVENTS_TABLE} e
         LEFT JOIN {PREFS_TABLE} p
            ON p.account = e.account AND p.calendar_id = e.calendar_id
         WHERE ((e.start_utc < ?2 AND e.end_utc > ?1)
                OR (e.start_utc = e.end_utc AND e.start_utc >= ?1 AND e.start_utc < ?2))"
    );
    if visible_only {
        sql.push_str(" AND COALESCE(p.visible, 1) = 1");
    }
    let mut values: Vec<rusqlite::types::Value> = vec![
        window.start.timestamp().into(),
        window.end.timestamp().into(),
    ];
    if !accounts.is_empty() {
        let marks: Vec<String> = (0..accounts.len())
            .map(|index| format!("?{}", index + 3))
            .collect();
        sql.push_str(&format!(" AND e.account IN ({})", marks.join(", ")));
        values.extend(accounts.iter().map(|a| a.to_string().into()));
    }
    let mut stmt = store.conn().prepare(&sql)?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(values), |row| {
            row.get::<_, String>(0)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("reading cached calendar events")?;
    let mut events: Vec<Event> = rows
        .iter()
        .filter_map(|json| parse_event(json))
        .filter(|event| event.overlaps(&window, viewer))
        .collect();
    events.sort_by(|a, b| {
        let key = |e: &Event| (e.utc_interval(viewer).0, !e.is_all_day());
        key(a)
            .cmp(&key(b))
            .then_with(|| a.summary.cmp(&b.summary))
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok(events)
}

/// The dedup keys of every cached event of `account`, whatever its window
/// or calendar: what an invitation mail's copy of an event is matched
/// against.
pub fn event_keys(store: &Store, account: &str) -> Result<HashSet<EventKey>> {
    let mut stmt = store.conn().prepare(&format!(
        "SELECT uid, recurrence_id FROM {EVENTS_TABLE} WHERE account = ?1"
    ))?;
    let keys = stmt
        .query_map([account], |row| {
            Ok(EventKey::new(
                &row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?.as_deref(),
            ))
        })?
        .collect::<rusqlite::Result<HashSet<_>>>()
        .context("reading the cached event keys")?;
    Ok(keys)
}

/// One cached event of `account` by its server id.
pub fn get_event(store: &Store, account: &str, id: &str) -> Result<Option<Event>> {
    let json: Option<String> = store
        .conn()
        .query_row(
            &format!(
                "SELECT event_json FROM {EVENTS_TABLE} WHERE account = ?1 AND event_id = ?2
                 LIMIT 1"
            ),
            [account, id],
            |row| row.get(0),
        )
        .optional()
        .context("reading a cached calendar event")?;
    Ok(json.as_deref().and_then(parse_event))
}

/// A row this build cannot read back (written by another build's model) is
/// skipped with a log line: the cache is disposable, and the next baseline
/// rewrites it.
fn parse_event(json: &str) -> Option<Event> {
    match serde_json::from_str(json) {
        Ok(event) => Some(event),
        Err(err) => {
            warn!("[calendar] skipping an unreadable cached event: {err}");
            None
        }
    }
}

/// The sync state of one calendar, if it has synced.
pub fn load_sync_state(
    store: &Store,
    account: &str,
    calendar_id: &str,
) -> Result<Option<SyncState>> {
    load_sync_state_in(store.conn(), account, calendar_id)
}

fn load_sync_state_in(
    conn: &rusqlite::Connection,
    account: &str,
    calendar_id: &str,
) -> Result<Option<SyncState>> {
    conn.query_row(
        &format!(
            "SELECT token, window_start, window_end, baselined_at, synced_at, path
             FROM {SYNC_TABLE} WHERE account = ?1 AND calendar_id = ?2"
        ),
        [account, calendar_id],
        |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        },
    )
    .optional()
    .context("reading a calendar sync state")
    .map(|row| {
        row.and_then(|(token, start, end, baselined, synced, path)| {
            Some(SyncState {
                token,
                window: CalendarWindow {
                    start: DateTime::from_timestamp(start, 0)?,
                    end: DateTime::from_timestamp(end, 0)?,
                },
                baselined_at: DateTime::from_timestamp(baselined, 0)?,
                synced_at: DateTime::from_timestamp(synced, 0)?,
                path,
            })
        })
    })
}

/// Write one calendar's sync state, replacing the one it had.
pub fn save_sync_state(
    store: &Store,
    account: &str,
    calendar_id: &str,
    state: &SyncState,
) -> Result<()> {
    save_sync_state_in(store.conn(), account, calendar_id, state)
}

fn save_sync_state_in(
    conn: &rusqlite::Connection,
    account: &str,
    calendar_id: &str,
    state: &SyncState,
) -> Result<()> {
    conn.execute(
        &format!(
            "INSERT INTO {SYNC_TABLE}
                (account, calendar_id, token, window_start, window_end, baselined_at,
                 synced_at, path)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT (account, calendar_id) DO UPDATE SET
                token = excluded.token, window_start = excluded.window_start,
                window_end = excluded.window_end, baselined_at = excluded.baselined_at,
                synced_at = excluded.synced_at, path = excluded.path"
        ),
        params![
            account,
            calendar_id,
            state.token,
            state.window.start.timestamp(),
            state.window.end.timestamp(),
            state.baselined_at.timestamp(),
            state.synced_at.timestamp(),
            state.path,
        ],
    )
    .context("writing a calendar sync state")?;
    Ok(())
}

/// The series of `account` the store has resolved.
pub fn load_series(store: &Store, account: &str) -> Result<Vec<SeriesInfo>> {
    let mut stmt = store.conn().prepare(&format!(
        "SELECT master_id, uid, recurrence FROM {SERIES_TABLE} WHERE account = ?1
         ORDER BY master_id"
    ))?;
    let series = stmt
        .query_map([account], |row| {
            Ok(SeriesInfo {
                master_id: row.get(0)?,
                uid: row.get(1)?,
                recurrence: row.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("reading the calendar series")?;
    Ok(series)
}

fn write_series(conn: &rusqlite::Connection, account: &str, series: &SeriesInfo) -> Result<()> {
    conn.execute(
        &format!(
            "INSERT INTO {SERIES_TABLE} (account, master_id, uid, recurrence)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (account, master_id) DO UPDATE SET
                uid = excluded.uid, recurrence = excluded.recurrence"
        ),
        params![account, series.master_id, series.uid, series.recurrence],
    )
    .context("writing a calendar series")?;
    Ok(())
}

/// Drop everything the calendar cache of `account` holds (calendars, events,
/// sync tokens, series, invitation lookups), keeping the visibility
/// preferences: the next sync rebuilds the rest from the server
/// (`calendar.rebuild`).
pub fn drop_calendar_cache(store: &Store, account: &str) -> Result<()> {
    let tx = store.immediate_transaction()?;
    for table in [
        CALENDARS_TABLE,
        EVENTS_TABLE,
        SYNC_TABLE,
        SERIES_TABLE,
        INVITES_TABLE,
    ] {
        tx.execute(
            &format!("DELETE FROM {table} WHERE account = ?1"),
            [account],
        )
        .with_context(|| format!("clearing {table}"))?;
    }
    tx.commit().context("dropping the calendar cache")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calendar_model::{OwnResponse, Sensitivity, ShowAs};
    use chrono::TimeZone;

    fn open() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("store.sqlite3")).unwrap();
        (dir, store)
    }

    fn utc(d: u32, h: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, d, h, 0, 0).unwrap()
    }

    fn day(d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 11, d).unwrap()
    }

    fn calendar(id: &str, is_default: bool) -> Calendar {
        Calendar {
            account: "evoqs".into(),
            id: id.into(),
            name: format!("Calendar {id}"),
            color: Some("#3b82f6".into()),
            is_default,
            can_edit: true,
            visible: true,
        }
    }

    fn event(id: &str, calendar: &str, time: EventTime) -> Event {
        Event {
            account: "evoqs".into(),
            calendar: calendar.into(),
            id: id.into(),
            uid: format!("{id}@example.com"),
            recurrence_id: None,
            series_master_id: None,
            summary: id.into(),
            time,
            location: None,
            body: None,
            organizer: None,
            attendees: Vec::new(),
            own_response: OwnResponse::None,
            recurrence: None,
            reminder: None,
            online_meeting_url: None,
            sensitivity: Sensitivity::Normal,
            show_as: ShowAs::Busy,
            cancelled: false,
            is_organizer: false,
            last_modified: Some(utc(1, 8)),
            web_link: None,
        }
    }

    fn timed(id: &str, calendar: &str, d: u32, h: u32) -> Event {
        event(
            id,
            calendar,
            EventTime::timed(utc(d, h), utc(d, h + 1), "Europe/Berlin").unwrap(),
        )
    }

    fn window(start: DateTime<Utc>, end: DateTime<Utc>) -> CalendarWindow {
        CalendarWindow { start, end }
    }

    fn november() -> CalendarWindow {
        window(
            utc(1, 0),
            Utc.with_ymd_and_hms(2026, 12, 1, 0, 0, 0).unwrap(),
        )
    }

    fn baseline(upserts: Vec<Event>) -> SyncResult {
        SyncResult {
            upserts,
            token: Some("t1".into()),
            full_resync: true,
            path: Some("calendar-delta".into()),
            ..Default::default()
        }
    }

    fn ids(events: &[Event]) -> Vec<&str> {
        events.iter().map(|e| e.id.as_str()).collect()
    }

    #[test]
    fn the_calendar_tables_exist_on_a_fresh_store() {
        let (_dir, store) = open();
        for (table, _) in create_sql() {
            let present: bool = store
                .conn()
                .query_row(
                    "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE name = ?1)",
                    [table],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(present, "{table}");
        }
    }

    /// An invitation's lookup is remembered under the server id it was made
    /// with, a miss included, and a rebuild forgets it; a preference outlives
    /// both.
    #[test]
    fn an_invitation_lookup_is_remembered_per_server_id() {
        let (_dir, store) = open();
        assert_eq!(cached_invite_event(&store, "evoqs", 7, "m1").unwrap(), None);
        remember_invite_event(&store, "evoqs", 7, "m1", Some("ev1")).unwrap();
        remember_invite_event(&store, "evoqs", 8, "m2", None).unwrap();
        assert_eq!(
            cached_invite_event(&store, "evoqs", 7, "m1").unwrap(),
            Some(Some("ev1".into()))
        );
        assert_eq!(
            cached_invite_event(&store, "evoqs", 8, "m2").unwrap(),
            Some(None)
        );
        // A move gave the row another server id: looked up again.
        assert_eq!(cached_invite_event(&store, "evoqs", 7, "m9").unwrap(), None);

        assert!(calendar_visible(&store, "evoqs", "imip").unwrap());
        set_calendar_visible(&store, "evoqs", "imip", false).unwrap();
        drop_calendar_cache(&store, "evoqs").unwrap();
        assert_eq!(cached_invite_event(&store, "evoqs", 7, "m1").unwrap(), None);
        assert!(!calendar_visible(&store, "evoqs", "imip").unwrap());
    }

    #[test]
    fn calendars_keep_the_server_order_and_their_visibility() {
        let (_dir, store) = open();
        upsert_calendars(
            &store,
            "evoqs",
            &[calendar("b", true), calendar("a", false)],
        )
        .unwrap();
        set_calendar_visible(&store, "evoqs", "a", false).unwrap();
        let listed = list_calendars(&store, "evoqs").unwrap();
        assert_eq!(
            listed.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            ["b", "a"]
        );
        assert!(listed[0].visible && !listed[1].visible);
        assert!(listed[0].is_default);
        assert!(list_calendars(&store, "other").unwrap().is_empty());
    }

    #[test]
    fn a_calendar_gone_from_the_server_takes_its_events_and_token_along() {
        let (_dir, store) = open();
        upsert_calendars(
            &store,
            "evoqs",
            &[calendar("a", true), calendar("b", false)],
        )
        .unwrap();
        let result = baseline(vec![timed("e1", "b", 3, 9)]);
        apply_sync(&store, "evoqs", "b", &result, november(), utc(1, 0)).unwrap();
        upsert_calendars(&store, "evoqs", &[calendar("a", true)]).unwrap();
        assert!(get_event(&store, "evoqs", "e1").unwrap().is_none());
        assert!(load_sync_state(&store, "evoqs", "b").unwrap().is_none());
    }

    #[test]
    fn a_baseline_replaces_and_a_delta_merges() {
        let (_dir, store) = open();
        let first = baseline(vec![timed("e1", "a", 3, 9), timed("e2", "a", 4, 9)]);
        apply_sync(&store, "evoqs", "a", &first, november(), utc(1, 0)).unwrap();

        let mut changed = timed("e1", "a", 5, 9);
        changed.summary = "moved".into();
        let delta = SyncResult {
            upserts: vec![changed.clone(), timed("e3", "a", 6, 9)],
            removed: vec!["e2".into(), "not-cached".into()],
            token: Some("t2".into()),
            path: Some("calendar-delta".into()),
            ..Default::default()
        };
        apply_sync(&store, "evoqs", "a", &delta, november(), utc(1, 1)).unwrap();
        let events = events_in_range(&store, &[], november(), chrono_tz::UTC, false).unwrap();
        assert_eq!(ids(&events), ["e1", "e3"]);
        assert_eq!(get_event(&store, "evoqs", "e1").unwrap(), Some(changed));
        let state = load_sync_state(&store, "evoqs", "a").unwrap().unwrap();
        assert_eq!(state.token.as_deref(), Some("t2"));
        assert_eq!(
            state.baselined_at,
            utc(1, 0),
            "a delta keeps the baseline stamp"
        );
        assert_eq!(state.synced_at, utc(1, 1));
        assert_eq!(state.window, november());
        assert_eq!(state.path.as_deref(), Some("calendar-delta"));

        let again = baseline(vec![timed("e4", "a", 7, 9)]);
        apply_sync(&store, "evoqs", "a", &again, november(), utc(2, 0)).unwrap();
        let events = events_in_range(&store, &[], november(), chrono_tz::UTC, false).unwrap();
        assert_eq!(ids(&events), ["e4"]);
        let state = load_sync_state(&store, "evoqs", "a").unwrap().unwrap();
        assert_eq!(state.baselined_at, utc(2, 0));
    }

    #[test]
    fn a_range_query_is_half_open_and_honours_visibility_and_accounts() {
        let (_dir, store) = open();
        apply_sync(
            &store,
            "evoqs",
            "a",
            &baseline(vec![timed("e1", "a", 3, 9), timed("e2", "a", 3, 11)]),
            november(),
            utc(1, 0),
        )
        .unwrap();
        apply_sync(
            &store,
            "evoqs",
            "b",
            &baseline(vec![timed("e3", "b", 3, 10)]),
            november(),
            utc(1, 0),
        )
        .unwrap();
        let morning = window(utc(3, 10), utc(3, 11));
        assert_eq!(
            ids(&events_in_range(&store, &[], morning, chrono_tz::UTC, false).unwrap()),
            ["e3"]
        );
        let day3 = window(utc(3, 0), utc(4, 0));
        assert_eq!(
            ids(&events_in_range(&store, &["evoqs"], day3, chrono_tz::UTC, false).unwrap()),
            ["e1", "e3", "e2"]
        );
        set_calendar_visible(&store, "evoqs", "b", false).unwrap();
        assert_eq!(
            ids(&events_in_range(&store, &[], day3, chrono_tz::UTC, true).unwrap()),
            ["e1", "e2"]
        );
        assert!(
            events_in_range(&store, &["tum"], day3, chrono_tz::UTC, false)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn an_all_day_event_matches_the_days_it_covers_in_the_viewers_zone() {
        let (_dir, store) = open();
        let holiday = event("h", "a", EventTime::all_day(day(5), day(7)).unwrap());
        apply_sync(
            &store,
            "evoqs",
            "a",
            &baseline(vec![holiday]),
            november(),
            utc(1, 0),
        )
        .unwrap();
        let berlin = chrono_tz::Europe::Berlin;
        // 4 November 23:30 UTC is already 5 November in Berlin.
        let late = window(
            Utc.with_ymd_and_hms(2026, 11, 4, 23, 30, 0).unwrap(),
            utc(5, 0),
        );
        assert_eq!(
            ids(&events_in_range(&store, &[], late, berlin, false).unwrap()),
            ["h"]
        );
        assert!(events_in_range(&store, &[], late, chrono_tz::UTC, false)
            .unwrap()
            .is_empty());
        let after = window(utc(7, 0), utc(8, 0));
        assert!(events_in_range(&store, &[], after, chrono_tz::UTC, false)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn dropping_the_cache_keeps_the_visibility() {
        let (_dir, store) = open();
        upsert_calendars(&store, "evoqs", &[calendar("a", true)]).unwrap();
        set_calendar_visible(&store, "evoqs", "a", false).unwrap();
        let mut result = baseline(vec![timed("e1", "a", 3, 9)]);
        result.series = vec![SeriesInfo {
            master_id: "m".into(),
            uid: "series@example.com".into(),
            recurrence: Some("FREQ=WEEKLY".into()),
        }];
        apply_sync(&store, "evoqs", "a", &result, november(), utc(1, 0)).unwrap();
        assert_eq!(load_series(&store, "evoqs").unwrap(), result.series);

        drop_calendar_cache(&store, "evoqs").unwrap();
        assert!(list_calendars(&store, "evoqs").unwrap().is_empty());
        assert!(get_event(&store, "evoqs", "e1").unwrap().is_none());
        assert!(load_sync_state(&store, "evoqs", "a").unwrap().is_none());
        assert!(load_series(&store, "evoqs").unwrap().is_empty());

        upsert_calendars(&store, "evoqs", &[calendar("a", true)]).unwrap();
        assert!(!list_calendars(&store, "evoqs").unwrap()[0].visible);
    }
}
