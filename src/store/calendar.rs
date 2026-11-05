//! The calendar cache of an account (PERSO-200, `.agents/plans/calendar.md`,
//! "Event store").
//!
//! A disposable cache of the rolling window (D6): the server is the source of
//! truth, so [`drop_calendar_cache`] and a resync lose nothing. Seven tables,
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
//!   a range query filters on beside it. `inherits` marks an occurrence whose
//!   details are its series master's ([`SyncResult::inheriting`]), which
//!   [`apply_sync`] re-derives from each master a sync brings.
//! - [`SYNC_TABLE`]: each calendar's sync token, with the window it was minted
//!   for (a Graph delta token is bound to it), when the last baseline ran,
//!   and the path the backend synced through.
//! - [`SERIES_TABLE`]: what a backend resolved about a recurring series
//!   ([`SeriesInfo`]), so Graph asks for a series master once rather than at
//!   every poll: its `UID`, its rule (structured, in `recurrence_json`) and
//!   zone, and
//!   the master itself as an event in `master_json`, whose details a trimmed
//!   occurrence shows.
//! - [`INVITES_TABLE`]: the server event an invitation mail stands for, by
//!   its `messages.id` and the server id it was looked up under, so
//!   `message.invite` asks the server once per invitation mail (a mail that
//!   stands for no event is remembered too, with a `NULL` event).
//! - [`REMOVED_TABLE`]: each event an `event.cancel` or `event.delete` of mp
//!   took off the server and out of the cache in the last
//!   [`REMOVAL_KEPT_DAYS`] days, with who the cancel told and the scope it
//!   was made in (`occurrence`, `series`, or none for a single event), so a
//!   retry of the same call counts as done rather than as an event the cache never held
//!   ([`removal`]). A record of mp's own writes rather than of the server, so
//!   [`drop_calendar_cache`] keeps it; a schema-version rebuild of the store
//!   does not carry it, and a retry after one is refused as an unknown event.

use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, NaiveDate, NaiveTime, Utc};
use chrono_tz::Tz;
use log::warn;
use rusqlite::{params, OptionalExtension};

use super::Store;
use crate::calendar_backend::{SeriesInfo, SyncResult};
use crate::calendar_model::{
    Calendar, CalendarWindow, Event, EventKey, EventTime, OnlineMeetingProvider,
};

pub const CALENDARS_TABLE: &str = "calendars";
pub const PREFS_TABLE: &str = "calendar_prefs";
pub const EVENTS_TABLE: &str = "calendar_events";
pub const SYNC_TABLE: &str = "calendar_sync";
pub const SERIES_TABLE: &str = "calendar_series";
pub const INVITES_TABLE: &str = "calendar_invite_events";
pub const REMOVED_TABLE: &str = "calendar_removed_events";

/// How long [`REMOVED_TABLE`] remembers a cancel or a delete: longer than any
/// client waits to retry an operation that vanished.
pub const REMOVAL_KEPT_DAYS: i64 = 7;

/// The `CREATE` statements of the seven tables and their indexes, as
/// `(table, statements)` for [`super::schema::ensure_additive_tables`].
pub fn create_sql() -> [(&'static str, String); 7] {
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
                    online_meeting_providers TEXT,
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
                    inherits      INTEGER NOT NULL DEFAULT 0,
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
                    master_id   TEXT NOT NULL,
                    uid         TEXT NOT NULL,
                    recurrence_json TEXT,
                    master_json TEXT,
                    zone        TEXT,
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
        (
            REMOVED_TABLE,
            // `method` is `event.cancel` or `event.delete`, `notified` the
            // JSON list of the addresses a cancel told, `removed_at` unix
            // seconds, `scope` `occurrence` or `series` (none on a single
            // event).
            format!(
                "CREATE TABLE IF NOT EXISTS {REMOVED_TABLE} (
                    account    TEXT NOT NULL,
                    event_id   TEXT NOT NULL,
                    method     TEXT NOT NULL,
                    notified   TEXT NOT NULL,
                    removed_at INTEGER NOT NULL,
                    scope      TEXT,
                    PRIMARY KEY (account, event_id)
                )"
            ),
        ),
    ]
}

/// The columns added to a calendar table after a build created it, as
/// `(table, column, type)`: the series master's details and the mark of an
/// occurrence that inherits them (PERSO-200), the structured series rule and
/// a calendar's online-meeting services, and the scope of a removal mp made
/// (PERSO-255). A table an older build created gains them here; its event
/// rows fill at the next baseline, its calendar rows at the next sync, which
/// lists the calendars in full, and its removals read as single events'.
const ADDED_COLUMNS: [(&str, &str, &str); 6] = [
    (EVENTS_TABLE, "inherits", "INTEGER NOT NULL DEFAULT 0"),
    (SERIES_TABLE, "master_json", "TEXT"),
    (SERIES_TABLE, "zone", "TEXT"),
    (SERIES_TABLE, "recurrence_json", "TEXT"),
    (CALENDARS_TABLE, "online_meeting_providers", "TEXT"),
    (REMOVED_TABLE, "scope", "TEXT"),
];

/// Add [`ADDED_COLUMNS`] to the calendar tables that lack them, after
/// [`super::schema::ensure_additive_tables`] created the tables
/// ([`super::schema::add_columns`]).
pub fn ensure_columns(conn: &rusqlite::Connection) -> Result<()> {
    super::schema::add_columns(conn, &ADDED_COLUMNS)
}

/// Version of what a server sync reads into the cache: the fields of
/// [`Event`] and [`SeriesInfo`] a backend fills from the server.
/// A field of [`Calendar`] needs no bump: every sync lists the calendars in
/// full and rewrites their rows ([`upsert_calendars`]).
///
/// Bump it whenever a backend starts reading a field it did not read before.
/// The cache is delta-synced, so a row an older build wrote keeps the old
/// default of every new field until its event changes on the server; a store
/// stamped with another version drops its sync tokens on open
/// ([`rebaseline_on_model_change`]), and the next sync of each calendar is a
/// baseline that reads every event and every series master again.
///
/// 1 is the read side of PERSO-255: the occurrence kind, the online meeting
/// and its provider, the proposal and answer flags, the transaction id, each
/// attendee's proposed time and the structured series rule. A store with no
/// stamp predates it.
pub const MODEL_VERSION: i64 = 1;

/// `meta` key holding the [`MODEL_VERSION`] the cache was last baselined
/// under.
pub const META_MODEL_VERSION: &str = "calendar_model_version";

/// Stamp [`MODEL_VERSION`], on a store whose cache holds nothing an older
/// model wrote (a fresh file).
pub fn stamp_model_version(conn: &rusqlite::Connection) -> Result<()> {
    super::schema::set_meta(conn, META_MODEL_VERSION, &MODEL_VERSION.to_string())
}

/// Drop every calendar's sync token when the store was stamped under another
/// [`MODEL_VERSION`] (or none), and stamp the current one; `true` when it did.
///
/// Only [`SYNC_TABLE`] is cleared: a sync with no stored state is a baseline
/// ([`crate::calendar_sync::sync_account`]), which replaces every cached event
/// of the calendar and reads each series master again, so the cached events
/// stay readable until it lands. The clear and its stamp commit together, so
/// a failure leaves the file to try again at the next open; a store whose
/// calendar tables could not be created fails here and is retried the same
/// way.
pub fn rebaseline_on_model_change(conn: &rusqlite::Connection) -> Result<bool> {
    let stamped = super::schema::get_meta(conn, META_MODEL_VERSION)?;
    if stamped.as_deref() == Some(MODEL_VERSION.to_string().as_str()) {
        return Ok(false);
    }
    let tx = conn
        .unchecked_transaction()
        .context("beginning the calendar re-baseline")?;
    tx.execute(&format!("DELETE FROM {SYNC_TABLE}"), [])
        .context("dropping the calendar sync tokens")?;
    stamp_model_version(&tx)?;
    tx.commit().context("committing the calendar re-baseline")?;
    Ok(true)
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
                    (account, id, name, color, is_default, can_edit, position,
                     online_meeting_providers)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT (account, id) DO UPDATE SET
                    name = excluded.name, color = excluded.color,
                    is_default = excluded.is_default, can_edit = excluded.can_edit,
                    position = excluded.position,
                    online_meeting_providers = excluded.online_meeting_providers"
            ),
            params![
                account,
                calendar.id,
                calendar.name,
                calendar.color,
                calendar.is_default,
                calendar.can_edit,
                position as i64,
                serde_json::to_string(&calendar.online_meeting_providers)?,
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
                COALESCE(p.visible, 1), c.online_meeting_providers
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
                online_meeting_providers: meeting_providers(row.get(7)?),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("reading the calendar list")?;
    Ok(calendars)
}

/// A calendar row's online-meeting services: none for a row an older build
/// wrote (`NULL`) until the next sync rewrites it, and none for one this
/// build cannot read, so no client offers a meeting the server may drop.
fn meeting_providers(raw: Option<String>) -> Vec<OnlineMeetingProvider> {
    raw.and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default()
}

/// Fold one sync of `calendar_id` into the cache, with its token, in one
/// transaction: a `full_resync` replaces every cached event of the calendar,
/// anything else merges its upserts and removals, a removed series master
/// taking its occurrences and its series row along and a series re-sent
/// whole ([`SyncResult::resent_series`]) keeping only the occurrences that
/// came. A baseline stamps
/// `baselined_at` with `now`; an incremental sync keeps the stamp it had.
///
/// Each series master the sync brings is handed on to the cached occurrences
/// that inherit from it ([`Event::inherit_from`]), so a renamed series shows
/// its new name on every occurrence the server did not send again.
///
/// `true` when the calendar's rows moved: an event added, removed, or written
/// with a stored form that differs from the cached one. A baseline that reads
/// back what the cache holds moves nothing, which keeps a calendar read in
/// full on every sync (Graph's `calendarView` fallback) from announcing a
/// change on every tick.
pub fn apply_sync(
    store: &Store,
    account: &str,
    calendar_id: &str,
    result: &SyncResult,
    window: CalendarWindow,
    now: DateTime<Utc>,
) -> Result<bool> {
    let tx = store.immediate_transaction()?;
    let mut cached = cached_rows(&tx, account, calendar_id)?;
    let mut changed = false;
    let delete = |id: &str| {
        tx.execute(
            &format!(
                "DELETE FROM {EVENTS_TABLE}
                 WHERE account = ?1 AND calendar_id = ?2 AND event_id = ?3"
            ),
            [account, calendar_id, id],
        )
    };
    if result.full_resync {
        let incoming: HashSet<&str> = result.upserts.iter().map(|e| e.id.as_str()).collect();
        let stale: Vec<String> = cached
            .keys()
            .filter(|id| !incoming.contains(id.as_str()))
            .cloned()
            .collect();
        for id in stale {
            delete(&id)?;
            cached.remove(&id);
            changed = true;
        }
    }
    // The cached occurrences of the series `master` in this calendar.
    let occurrences_of = |master: &str| -> rusqlite::Result<Vec<String>> {
        let mut stmt = tx.prepare(&format!(
            "SELECT event_id FROM {EVENTS_TABLE}
             WHERE account = ?1 AND calendar_id = ?2
               AND json_extract(event_json, '$.series_master_id') = ?3"
        ))?;
        let ids = stmt
            .query_map([account, calendar_id, master], |row| row.get(0))?
            .collect();
        ids
    };
    for id in &result.removed {
        if delete(id)? > 0 {
            changed = true;
        }
        cached.remove(id);
        // A removed series master takes its occurrences along, which the
        // delta never names, and its series row.
        for occurrence in occurrences_of(id)? {
            delete(&occurrence)?;
            cached.remove(&occurrence);
            changed = true;
        }
        tx.execute(
            &format!("DELETE FROM {SERIES_TABLE} WHERE account = ?1 AND master_id = ?2"),
            [account, id.as_str()],
        )
        .with_context(|| format!("removing series {id}"))?;
    }
    let inheriting: HashSet<&str> = result.inheriting.iter().map(String::as_str).collect();
    for event in &result.upserts {
        let json = serde_json::to_string(event).context("serialising a calendar event")?;
        let row = (json, inheriting.contains(event.id.as_str()));
        if cached.get(&event.id) == Some(&row) {
            continue;
        }
        write_event(&tx, account, calendar_id, event, &row.0, row.1)?;
        cached.insert(event.id.clone(), row);
        changed = true;
    }
    // A series re-sent whole keeps the occurrences that came and loses the
    // ones that did not, deleted or ended by a new rule on the server.
    if !result.full_resync {
        let brought: HashSet<&str> = result.upserts.iter().map(|e| e.id.as_str()).collect();
        for master in &result.resent_series {
            for occurrence in occurrences_of(master)? {
                if !brought.contains(occurrence.as_str()) {
                    delete(&occurrence)?;
                    cached.remove(&occurrence);
                    changed = true;
                }
            }
        }
    }
    for series in &result.series {
        write_series(&tx, account, series)?;
    }
    let masters: HashMap<&str, &Event> = result
        .series
        .iter()
        .filter_map(|series| Some((series.master_id.as_str(), series.master.as_deref()?)))
        .collect();
    if !masters.is_empty() {
        for (json, _) in cached.values().filter(|(_, inherits)| *inherits) {
            let Some(mut event) = parse_event(json) else {
                continue;
            };
            let master = event
                .series_master_id
                .as_deref()
                .and_then(|id| masters.get(id));
            let Some(master) = master else {
                continue;
            };
            event.inherit_from(master);
            let fresh = serde_json::to_string(&event).context("serialising a calendar event")?;
            if &fresh != json {
                write_event(&tx, account, calendar_id, &event, &fresh, true)?;
                changed = true;
            }
        }
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
    tx.commit().context("applying a calendar sync")?;
    Ok(changed)
}

/// The cached events of one calendar, as their stored JSON and whether they
/// inherit from their series master, by event id.
fn cached_rows(
    conn: &rusqlite::Connection,
    account: &str,
    calendar_id: &str,
) -> Result<HashMap<String, (String, bool)>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT event_id, event_json, inherits FROM {EVENTS_TABLE}
         WHERE account = ?1 AND calendar_id = ?2"
    ))?;
    let rows = stmt
        .query_map([account, calendar_id], |row| {
            Ok((row.get(0)?, (row.get(1)?, row.get(2)?)))
        })?
        .collect::<rusqlite::Result<HashMap<_, _>>>()
        .context("reading a calendar's cached events")?;
    Ok(rows)
}

/// Write `event`, whose stored form is `json`, into the cache; `inherits`
/// when its details are its series master's.
fn write_event(
    conn: &rusqlite::Connection,
    account: &str,
    calendar_id: &str,
    event: &Event,
    json: &str,
    inherits: bool,
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
    conn.execute(
        &format!(
            "INSERT INTO {EVENTS_TABLE}
                (account, calendar_id, event_id, uid, recurrence_id, start_utc, end_utc,
                 all_day, start_date, end_date, zone, last_modified, event_json, inherits)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
             ON CONFLICT (account, calendar_id, event_id) DO UPDATE SET
                uid = excluded.uid, recurrence_id = excluded.recurrence_id,
                start_utc = excluded.start_utc, end_utc = excluded.end_utc,
                all_day = excluded.all_day, start_date = excluded.start_date,
                end_date = excluded.end_date, zone = excluded.zone,
                last_modified = excluded.last_modified, event_json = excluded.event_json,
                inherits = excluded.inherits"
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
            inherits,
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

/// Write one event the server answered a write with into the cache (the
/// write-through of `event.create`), before the delta sync brings the same
/// row, which the upsert by id then leaves alone.
pub fn upsert_event(store: &Store, account: &str, event: &Event) -> Result<()> {
    let json = serde_json::to_string(event).context("serialising a calendar event")?;
    write_event(store.conn(), account, &event.calendar, event, &json, false)
}

/// A cancel or a delete mp made, as [`REMOVED_TABLE`] remembers it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Removal {
    /// `event.cancel` or `event.delete`.
    pub method: String,
    /// The addresses a cancel told; empty for a delete.
    pub notified: Vec<String>,
    /// `occurrence` or `series` for a removal made in a scope, `None` for a
    /// single event's.
    pub scope: Option<String>,
}

/// Drop one event of `account` from the cache (the write-through of
/// `event.cancel` and `event.delete`), before the delta sync reports it
/// removed, and remember `removal` in the same transaction, forgetting the
/// removals older than [`REMOVAL_KEPT_DAYS`]. `true` when the cache held the
/// event.
pub fn remove_event(
    store: &Store,
    account: &str,
    id: &str,
    removal: &Removal,
    now: DateTime<Utc>,
) -> Result<bool> {
    let tx = store.immediate_transaction()?;
    let removed = tx
        .execute(
            &format!("DELETE FROM {EVENTS_TABLE} WHERE account = ?1 AND event_id = ?2"),
            [account, id],
        )
        .with_context(|| format!("removing calendar event {id}"))?;
    remember_removal(&tx, account, id, removal, now)?;
    tx.commit()
        .with_context(|| format!("committing the removal of calendar event {id}"))?;
    Ok(removed > 0)
}

/// Drop the series `master_id` of `account` from the cache, every
/// occurrence of it and its series row (the write-through of a cancel or a
/// delete of a whole series, whose occurrences the server's delta never
/// names), and remember `removal` under `id`, the event the call named, and
/// under the master's id, in the same transaction. `true` when the cache
/// held an occurrence of it.
pub fn remove_series(
    store: &Store,
    account: &str,
    master_id: &str,
    id: &str,
    removal: &Removal,
    now: DateTime<Utc>,
) -> Result<bool> {
    let tx = store.immediate_transaction()?;
    let removed = tx
        .execute(
            &format!(
                "DELETE FROM {EVENTS_TABLE}
                 WHERE account = ?1 AND (event_id = ?2
                       OR json_extract(event_json, '$.series_master_id') = ?2)"
            ),
            [account, master_id],
        )
        .with_context(|| format!("removing the occurrences of series {master_id}"))?;
    tx.execute(
        &format!("DELETE FROM {SERIES_TABLE} WHERE account = ?1 AND master_id = ?2"),
        [account, master_id],
    )
    .with_context(|| format!("removing series {master_id}"))?;
    remember_removal(&tx, account, id, removal, now)?;
    if id != master_id {
        remember_removal(&tx, account, master_id, removal, now)?;
    }
    tx.commit()
        .with_context(|| format!("committing the removal of series {master_id}"))?;
    Ok(removed > 0)
}

/// Remember `removal` of `id`, forgetting the removals older than
/// [`REMOVAL_KEPT_DAYS`].
fn remember_removal(
    conn: &rusqlite::Connection,
    account: &str,
    id: &str,
    removal: &Removal,
    now: DateTime<Utc>,
) -> Result<()> {
    let notified =
        serde_json::to_string(&removal.notified).context("serialising who a cancel told")?;
    conn.execute(
        &format!("DELETE FROM {REMOVED_TABLE} WHERE removed_at < ?1"),
        [(now - Duration::days(REMOVAL_KEPT_DAYS)).timestamp()],
    )
    .context("forgetting old calendar removals")?;
    conn.execute(
        &format!(
            "INSERT OR REPLACE INTO {REMOVED_TABLE}
                 (account, event_id, method, notified, removed_at, scope)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)"
        ),
        params![
            account,
            id,
            removal.method,
            notified,
            now.timestamp(),
            removal.scope
        ],
    )
    .with_context(|| format!("remembering the removal of calendar event {id}"))?;
    Ok(())
}

/// Write one series the server answered a write with into the series cache
/// (the write-through of a create or an update of a whole series), before
/// the sync brings it again.
pub fn upsert_series(store: &Store, account: &str, series: &SeriesInfo) -> Result<()> {
    write_series(store.conn(), account, series)
}

/// Drop the sync state of `account`'s calendar `calendar_id`, so its next
/// sync is a baseline of the window: after a change to a series, whose
/// occurrences the server's delta re-sends without naming the ones that
/// left it.
pub fn forget_sync_state(store: &Store, account: &str, calendar_id: &str) -> Result<()> {
    store
        .conn()
        .execute(
            &format!("DELETE FROM {SYNC_TABLE} WHERE account = ?1 AND calendar_id = ?2"),
            [account, calendar_id],
        )
        .context("dropping a calendar's sync state")?;
    Ok(())
}

/// The cancel or delete of `account`'s event `id` mp made in the last
/// [`REMOVAL_KEPT_DAYS`] days, if any: what a retried call settles with
/// instead of reaching the server.
pub fn removal(
    store: &Store,
    account: &str,
    id: &str,
    now: DateTime<Utc>,
) -> Result<Option<Removal>> {
    let row: Option<(String, String, Option<String>)> = store
        .conn()
        .query_row(
            &format!(
                "SELECT method, notified, scope FROM {REMOVED_TABLE}
                 WHERE account = ?1 AND event_id = ?2 AND removed_at >= ?3"
            ),
            params![
                account,
                id,
                (now - Duration::days(REMOVAL_KEPT_DAYS)).timestamp()
            ],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .with_context(|| format!("looking up the removal of calendar event {id}"))?;
    row.map(|(method, notified, scope)| {
        Ok(Removal {
            method,
            notified: serde_json::from_str(&notified).context("reading who a cancel told")?,
            scope,
        })
    })
    .transpose()
}

/// The cached single event of `account` a create with `transaction_id`
/// made, if the cache holds it: what a retried create settles with instead
/// of posting. An occurrence, which carries its master's transaction id,
/// is not one: a series is found by [`series_by_transaction`].
pub fn event_by_transaction(
    store: &Store,
    account: &str,
    transaction_id: &str,
) -> Result<Option<Event>> {
    let json: Option<String> = store
        .conn()
        .query_row(
            &format!(
                "SELECT event_json FROM {EVENTS_TABLE}
                 WHERE account = ?1 AND json_extract(event_json, '$.transaction_id') = ?2
                   AND json_extract(event_json, '$.series_master_id') IS NULL
                 LIMIT 1"
            ),
            [account, transaction_id],
            |row| row.get(0),
        )
        .optional()
        .context("looking up a calendar event by transaction id")?;
    Ok(json.as_deref().and_then(parse_event))
}

/// The cached series of `account` a create with `transaction_id` made, by
/// its master: what a retried create of a recurring event settles with.
pub fn series_by_transaction(
    store: &Store,
    account: &str,
    transaction_id: &str,
) -> Result<Option<SeriesInfo>> {
    store
        .conn()
        .query_row(
            &format!(
                "SELECT {SERIES_COLUMNS} FROM {SERIES_TABLE}
                 WHERE account = ?1
                   AND json_extract(master_json, '$.transaction_id') = ?2
                 LIMIT 1"
            ),
            [account, transaction_id],
            series_row,
        )
        .optional()
        .context("looking up a calendar series by transaction id")
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

/// The columns [`series_row`] reads, in its order.
const SERIES_COLUMNS: &str = "master_id, uid, recurrence_json, master_json, zone";

/// A [`SERIES_TABLE`] row selected as [`SERIES_COLUMNS`]. A rule or a master
/// this build cannot read is none, which the next baseline writes afresh.
fn series_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SeriesInfo> {
    Ok(SeriesInfo {
        master_id: row.get(0)?,
        uid: row.get(1)?,
        recurrence: row
            .get::<_, Option<String>>(2)?
            .and_then(|json| serde_json::from_str(&json).ok()),
        master: row
            .get::<_, Option<String>>(3)?
            .as_deref()
            .and_then(parse_event)
            .map(Box::new),
        zone: row.get(4)?,
    })
}

/// The series of `account` the store has resolved.
pub fn load_series(store: &Store, account: &str) -> Result<Vec<SeriesInfo>> {
    let mut stmt = store.conn().prepare(&format!(
        "SELECT {SERIES_COLUMNS} FROM {SERIES_TABLE} WHERE account = ?1 ORDER BY master_id"
    ))?;
    let series = stmt
        .query_map([account], series_row)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("reading the calendar series")?;
    Ok(series)
}

/// The series of `account` whose master is `master_id`, when the store has
/// resolved it.
pub fn get_series(store: &Store, account: &str, master_id: &str) -> Result<Option<SeriesInfo>> {
    store
        .conn()
        .query_row(
            &format!(
                "SELECT {SERIES_COLUMNS} FROM {SERIES_TABLE}
                 WHERE account = ?1 AND master_id = ?2"
            ),
            params![account, master_id],
            series_row,
        )
        .optional()
        .context("reading a calendar series")
}

fn write_series(conn: &rusqlite::Connection, account: &str, series: &SeriesInfo) -> Result<()> {
    let master = series
        .master
        .as_deref()
        .map(serde_json::to_string)
        .transpose()
        .context("serialising a series master")?;
    let rule = series
        .recurrence
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .context("serialising a series rule")?;
    conn.execute(
        &format!(
            "INSERT INTO {SERIES_TABLE}
                (account, master_id, uid, recurrence_json, master_json, zone)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (account, master_id) DO UPDATE SET
                uid = excluded.uid, recurrence_json = excluded.recurrence_json,
                master_json = excluded.master_json, zone = excluded.zone"
        ),
        params![
            account,
            series.master_id,
            series.uid,
            rule,
            master,
            series.zone
        ],
    )
    .context("writing a calendar series")?;
    Ok(())
}

/// Drop everything the calendar cache of `account` holds (calendars, events,
/// sync tokens, series, invitation lookups), keeping the visibility
/// preferences and the removals mp made ([`REMOVED_TABLE`]): the next sync
/// rebuilds the rest from the server
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
    use crate::calendar_model::{
        EventKind, Frequency, OwnResponse, Recurrence, Sensitivity, ShowAs, Weekday,
    };
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
            online_meeting_providers: vec![OnlineMeetingProvider::TeamsForBusiness],
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
            kind: EventKind::Single,
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
            is_online_meeting: false,
            online_meeting_provider: None,
            sensitivity: Sensitivity::Normal,
            show_as: ShowAs::Busy,
            cancelled: false,
            is_organizer: false,
            last_modified: Some(utc(1, 8)),
            web_link: None,
            allow_new_time_proposals: true,
            response_requested: true,
            transaction_id: None,
        }
    }

    /// Weekly on Wednesday from 4 November, ten times.
    fn weekly() -> Recurrence {
        Recurrence {
            frequency: Frequency::Weekly,
            interval: 1,
            weekdays: vec![Weekday::Wednesday],
            day_of_month: None,
            week_index: None,
            month: None,
            first_day_of_week: Some(Weekday::Monday),
            start: Some(day(4)),
            until: None,
            count: Some(10),
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

    /// A written event enters the cache before any sync, and a retried
    /// create finds it by its transaction id, on its own account only.
    #[test]
    fn a_written_event_is_found_by_its_transaction_id() {
        let (_dir, store) = open();
        upsert_calendars(&store, "evoqs", &[calendar("cal-main", true)]).unwrap();
        let mut created = timed("ev-new", "cal-main", 4, 9);
        created.transaction_id = Some("tx-1".into());
        upsert_event(&store, "evoqs", &created).unwrap();
        assert_eq!(
            get_event(&store, "evoqs", "ev-new").unwrap(),
            Some(created.clone())
        );
        assert_eq!(
            event_by_transaction(&store, "evoqs", "tx-1").unwrap(),
            Some(created)
        );
        assert_eq!(event_by_transaction(&store, "evoqs", "tx-2").unwrap(), None);
        assert_eq!(event_by_transaction(&store, "perso", "tx-1").unwrap(), None);
    }

    /// A cancelled or deleted event leaves the cache of its own account
    /// only, and a second removal finds nothing; the removal is remembered
    /// for its account and forgotten after [`REMOVAL_KEPT_DAYS`], and a
    /// calendar rebuild keeps it.
    #[test]
    fn a_removed_event_leaves_its_accounts_cache_and_is_remembered() {
        let (_dir, store) = open();
        upsert_calendars(&store, "evoqs", &[calendar("cal-main", true)]).unwrap();
        let event = timed("ev-gone", "cal-main", 4, 9);
        upsert_event(&store, "evoqs", &event).unwrap();
        let cancel = Removal {
            method: "event.cancel".into(),
            notified: vec!["ada@example.com".into()],
            scope: None,
        };
        let now = Utc::now();
        assert!(!remove_event(&store, "perso", "ev-gone", &cancel, now).unwrap());
        assert!(remove_event(&store, "evoqs", "ev-gone", &cancel, now).unwrap());
        assert_eq!(get_event(&store, "evoqs", "ev-gone").unwrap(), None);
        assert!(!remove_event(&store, "evoqs", "ev-gone", &cancel, now).unwrap());
        assert_eq!(
            removal(&store, "evoqs", "ev-gone", now).unwrap(),
            Some(cancel.clone())
        );
        assert_eq!(removal(&store, "evoqs", "ev-other", now).unwrap(), None);
        drop_calendar_cache(&store, "evoqs").unwrap();
        assert_eq!(
            removal(&store, "evoqs", "ev-gone", now).unwrap(),
            Some(cancel.clone())
        );
        let later = now + Duration::days(REMOVAL_KEPT_DAYS + 1);
        assert_eq!(removal(&store, "evoqs", "ev-gone", later).unwrap(), None);
        let delete = Removal {
            method: "event.delete".into(),
            notified: Vec::new(),
            scope: Some("occurrence".into()),
        };
        remove_event(&store, "evoqs", "ev-next", &delete, later).unwrap();
        assert_eq!(removal(&store, "evoqs", "ev-gone", now).unwrap(), None);
        assert_eq!(
            removal(&store, "evoqs", "ev-next", later).unwrap(),
            Some(delete)
        );
    }

    /// A removed series leaves the cache with every occurrence and its
    /// series row, the other series and events staying, and is remembered
    /// under the id the call named and under its master's; a series is
    /// found by its master's transaction id, an occurrence carrying it is
    /// not a single event made by it, and a dropped sync state makes the
    /// next sync a baseline.
    #[test]
    fn a_removed_series_takes_its_occurrences_and_a_series_is_found_by_transaction() {
        let (_dir, store) = open();
        upsert_calendars(&store, "evoqs", &[calendar("cal-main", true)]).unwrap();
        let mut master = timed("ev-series", "cal-main", 2, 9);
        master.kind = crate::calendar_model::EventKind::SeriesMaster;
        master.transaction_id = Some("tx-series".into());
        let series = SeriesInfo {
            master_id: "ev-series".into(),
            uid: "UID-SERIES".into(),
            recurrence: None,
            master: Some(Box::new(master.clone())),
            zone: None,
        };
        upsert_series(&store, "evoqs", &series).unwrap();
        for (id, day) in [("ev-series-1", 2), ("ev-series-2", 9)] {
            let mut occurrence = timed(id, "cal-main", day, 9);
            occurrence.kind = crate::calendar_model::EventKind::Occurrence;
            occurrence.series_master_id = Some("ev-series".into());
            occurrence.transaction_id = Some("tx-series".into());
            upsert_event(&store, "evoqs", &occurrence).unwrap();
        }
        upsert_event(&store, "evoqs", &timed("ev-other", "cal-main", 3, 9)).unwrap();
        assert_eq!(
            event_by_transaction(&store, "evoqs", "tx-series").unwrap(),
            None
        );
        assert_eq!(
            series_by_transaction(&store, "evoqs", "tx-series").unwrap(),
            Some(series)
        );

        let cancel = Removal {
            method: "event.cancel".into(),
            notified: vec!["ada@example.com".into()],
            scope: Some("series".into()),
        };
        let now = Utc::now();
        assert!(remove_series(&store, "evoqs", "ev-series", "ev-series-2", &cancel, now).unwrap());
        assert_eq!(get_event(&store, "evoqs", "ev-series-1").unwrap(), None);
        assert_eq!(get_event(&store, "evoqs", "ev-series-2").unwrap(), None);
        assert!(get_event(&store, "evoqs", "ev-other").unwrap().is_some());
        assert_eq!(get_series(&store, "evoqs", "ev-series").unwrap(), None);
        for id in ["ev-series-2", "ev-series"] {
            assert_eq!(
                removal(&store, "evoqs", id, now).unwrap(),
                Some(cancel.clone())
            );
        }
        assert_eq!(removal(&store, "evoqs", "ev-series-1", now).unwrap(), None);

        let window = crate::calendar_model::window_for(now);
        let state = SyncState {
            token: Some("t1".into()),
            window,
            baselined_at: now,
            synced_at: now,
            path: None,
        };
        save_sync_state(&store, "evoqs", "cal-main", &state).unwrap();
        forget_sync_state(&store, "evoqs", "cal-main").unwrap();
        assert_eq!(load_sync_state(&store, "evoqs", "cal-main").unwrap(), None);
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

    /// A store whose calendar tables an older build created gains the later
    /// columns, and a second pass writes nothing; the `RRULE` text column of
    /// phase 1, which nothing reads any more, stays and is left alone.
    #[test]
    fn the_later_columns_reach_tables_an_older_build_created() {
        let (_dir, store) = open();
        store
            .conn()
            .execute_batch(&format!(
                "ALTER TABLE {EVENTS_TABLE} DROP COLUMN inherits;
                 ALTER TABLE {SERIES_TABLE} DROP COLUMN master_json;
                 ALTER TABLE {SERIES_TABLE} DROP COLUMN zone;
                 ALTER TABLE {SERIES_TABLE} DROP COLUMN recurrence_json;
                 ALTER TABLE {SERIES_TABLE} ADD COLUMN recurrence TEXT;"
            ))
            .unwrap();
        ensure_columns(store.conn()).unwrap();
        ensure_columns(store.conn()).unwrap();
        let mut result = baseline(vec![timed("e1", "a", 3, 9)]);
        result.inheriting = vec!["e1".into()];
        result.series = vec![SeriesInfo {
            master_id: "m".into(),
            uid: "series@example.com".into(),
            recurrence: Some(weekly()),
            master: Some(Box::new(timed("m", "a", 2, 9))),
            zone: Some("Europe/Berlin".into()),
        }];
        apply_sync(&store, "evoqs", "a", &result, november(), utc(1, 0)).unwrap();
        assert!(get_event(&store, "evoqs", "e1").unwrap().is_some());
        assert_eq!(load_series(&store, "evoqs").unwrap(), result.series);
    }

    /// A store an older model synced (no [`META_MODEL_VERSION`] stamp, or
    /// another version) comes out of its next open with no sync token, so the
    /// next sync is a baseline that reads every event with the newer fields;
    /// its cached events stay readable meanwhile, and a fresh store and the
    /// opens after the re-baseline keep their token.
    #[test]
    fn a_store_of_an_older_model_drops_its_tokens_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.sqlite3");
        let token = |store: &Store| {
            load_sync_state(store, "evoqs", "a")
                .unwrap()
                .and_then(|state| state.token)
        };

        {
            let store = Store::open(&path).unwrap();
            assert_eq!(
                super::super::schema::get_meta(store.conn(), META_MODEL_VERSION).unwrap(),
                Some(MODEL_VERSION.to_string()),
                "a fresh store is stamped"
            );
            apply_sync(
                &store,
                "evoqs",
                "a",
                &baseline(vec![timed("e1", "a", 3, 9)]),
                november(),
                utc(1, 0),
            )
            .unwrap();
        }
        let store = Store::open(&path).unwrap();
        assert_eq!(
            token(&store).as_deref(),
            Some("t1"),
            "same model, same token"
        );
        drop(store);

        for stamp in [None, Some("0")] {
            {
                let store = Store::open(&path).unwrap();
                apply_sync(
                    &store,
                    "evoqs",
                    "a",
                    &baseline(vec![timed("e1", "a", 3, 9)]),
                    november(),
                    utc(1, 0),
                )
                .unwrap();
                match stamp {
                    None => super::super::schema::clear_meta(store.conn(), META_MODEL_VERSION),
                    Some(v) => super::super::schema::set_meta(store.conn(), META_MODEL_VERSION, v),
                }
                .unwrap();
            }
            let store = Store::open(&path).unwrap();
            assert_eq!(
                token(&store),
                None,
                "stamp {stamp:?}: the next sync is a baseline"
            );
            assert!(
                get_event(&store, "evoqs", "e1").unwrap().is_some(),
                "the cached events stay until the baseline replaces them"
            );
            assert_eq!(
                super::super::schema::get_meta(store.conn(), META_MODEL_VERSION).unwrap(),
                Some(MODEL_VERSION.to_string())
            );
            apply_sync(
                &store,
                "evoqs",
                "a",
                &baseline(vec![timed("e1", "a", 3, 9)]),
                november(),
                utc(1, 1),
            )
            .unwrap();
            drop(store);
            let store = Store::open(&path).unwrap();
            assert_eq!(
                token(&store).as_deref(),
                Some("t1"),
                "the re-baseline runs once per model change"
            );
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

    /// A calendar's online-meeting services survive the store, an empty
    /// list included, and a row an older build wrote (`NULL`) offers none.
    #[test]
    fn calendars_keep_their_online_meeting_services() {
        let (_dir, store) = open();
        let mut bare = calendar("b", false);
        bare.online_meeting_providers = Vec::new();
        upsert_calendars(&store, "evoqs", &[calendar("a", true), bare]).unwrap();
        let listed = list_calendars(&store, "evoqs").unwrap();
        assert!(listed[0].offers_teams());
        assert!(listed[1].online_meeting_providers.is_empty());

        store
            .conn()
            .execute(
                &format!("UPDATE {CALENDARS_TABLE} SET online_meeting_providers = NULL"),
                [],
            )
            .unwrap();
        let listed = list_calendars(&store, "evoqs").unwrap();
        assert!(listed.iter().all(|c| !c.offers_teams()));
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

    /// A series the delta re-sends whole keeps only the occurrences that
    /// came (one deleted in Outlook goes), a master re-sent alone prunes
    /// nothing, and a removed master takes every occurrence and its series
    /// row along, the other events staying.
    #[test]
    fn a_delta_prunes_what_left_a_series_and_a_removed_master_takes_its_occurrences() {
        let (_dir, store) = open();
        let occurrence = |id: &str, d: u32| {
            let mut event = timed(id, "a", d, 9);
            event.kind = crate::calendar_model::EventKind::Occurrence;
            event.series_master_id = Some("ev-series".into());
            event
        };
        let series = SeriesInfo {
            master_id: "ev-series".into(),
            uid: "UID-SERIES".into(),
            recurrence: None,
            master: None,
            zone: None,
        };
        let [first, second, third] =
            [("occ-1", 2), ("occ-2", 9), ("occ-3", 16)].map(|(id, d)| occurrence(id, d));
        let mut first_sync = baseline(vec![
            first.clone(),
            second,
            third.clone(),
            timed("e-other", "a", 3, 9),
        ]);
        first_sync.series = vec![series.clone()];
        apply_sync(&store, "evoqs", "a", &first_sync, november(), utc(1, 0)).unwrap();

        let delta = |upserts: Vec<Event>, resent: bool, removed: Vec<String>| SyncResult {
            upserts,
            removed,
            token: Some("t2".into()),
            series: if resent {
                vec![series.clone()]
            } else {
                Vec::new()
            },
            resent_series: if resent {
                vec!["ev-series".into()]
            } else {
                Vec::new()
            },
            ..Default::default()
        };
        let alone = delta(Vec::new(), false, Vec::new());
        assert!(!apply_sync(&store, "evoqs", "a", &alone, november(), utc(1, 1)).unwrap());
        let events = events_in_range(&store, &[], november(), chrono_tz::UTC, false).unwrap();
        assert_eq!(ids(&events), ["occ-1", "e-other", "occ-2", "occ-3"]);

        let resent = delta(vec![first, third], true, Vec::new());
        assert!(apply_sync(&store, "evoqs", "a", &resent, november(), utc(1, 2)).unwrap());
        let events = events_in_range(&store, &[], november(), chrono_tz::UTC, false).unwrap();
        assert_eq!(ids(&events), ["occ-1", "e-other", "occ-3"]);
        assert!(get_series(&store, "evoqs", "ev-series").unwrap().is_some());

        let removed = delta(Vec::new(), false, vec!["ev-series".into()]);
        assert!(apply_sync(&store, "evoqs", "a", &removed, november(), utc(1, 3)).unwrap());
        let events = events_in_range(&store, &[], november(), chrono_tz::UTC, false).unwrap();
        assert_eq!(ids(&events), ["e-other"]);
        assert_eq!(get_series(&store, "evoqs", "ev-series").unwrap(), None);
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
            recurrence: Some(weekly()),
            master: Some(Box::new(timed("m", "a", 2, 9))),
            zone: Some("Europe/Berlin".into()),
        }];
        apply_sync(&store, "evoqs", "a", &result, november(), utc(1, 0)).unwrap();
        assert_eq!(load_series(&store, "evoqs").unwrap(), result.series);
        assert_eq!(
            get_series(&store, "evoqs", "m").unwrap().as_ref(),
            result.series.first()
        );
        assert_eq!(get_series(&store, "evoqs", "other").unwrap(), None);

        drop_calendar_cache(&store, "evoqs").unwrap();
        assert!(list_calendars(&store, "evoqs").unwrap().is_empty());
        assert!(get_event(&store, "evoqs", "e1").unwrap().is_none());
        assert!(load_sync_state(&store, "evoqs", "a").unwrap().is_none());
        assert!(load_series(&store, "evoqs").unwrap().is_empty());

        upsert_calendars(&store, "evoqs", &[calendar("a", true)]).unwrap();
        assert!(!list_calendars(&store, "evoqs").unwrap()[0].visible);
    }
}
