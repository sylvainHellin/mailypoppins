//! What became of each calendar reminder (PERSO-255, calendar phase 2).
//!
//! The daemon computes a reminder from the calendar cache ([`super::calendar`])
//! and records here, per account and event, the one it last fired, dismissed
//! or snoozed: `fire_at` is the due minute it was recorded for, so an event
//! moved or given another reminder setting reminds again at its new minute.
//!
//! Like `calendar_prefs` the table sits beside the calendar cache rather than
//! in it: `calendar.rebuild` drops the cache and leaves this table alone, so a
//! rebuild does not fire every reminder again. A row lives until its event
//! has ended (`end_at`), and [`prune`] removes it then.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension};

use super::Store;

pub const TABLE: &str = "calendar_reminders";

/// The `CREATE` statement of [`TABLE`], for
/// [`super::schema::ensure_additive_tables`]. Times are unix seconds.
pub fn create_sql() -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS {TABLE} (
            account       TEXT NOT NULL,
            event_id      TEXT NOT NULL,
            fire_at       INTEGER NOT NULL,
            end_at        INTEGER NOT NULL,
            state         TEXT NOT NULL,
            snoozed_until INTEGER,
            PRIMARY KEY (account, event_id)
        )"
    )
}

/// What became of a reminder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReminderState {
    /// Fired and waiting for the user: the clients show it.
    Fired,
    /// Snoozed until a later minute, when it fires again.
    Snoozed(DateTime<Utc>),
    /// Dismissed: it does not fire again for this due minute.
    Dismissed,
}

/// One row of [`TABLE`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReminderRow {
    pub account: String,
    /// The server id of the event.
    pub event: String,
    /// The due minute the row was recorded for: the event's start minus its
    /// reminder setting.
    pub fire_at: DateTime<Utc>,
    /// When the event ends, after which the row is pruned.
    pub end_at: DateTime<Utc>,
    pub state: ReminderState,
}

fn instant(seconds: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(seconds, 0).unwrap_or_default()
}

fn row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ReminderRow> {
    let state: String = row.get(4)?;
    let snoozed: Option<i64> = row.get(5)?;
    let state = match (state.as_str(), snoozed) {
        ("snoozed", Some(until)) => ReminderState::Snoozed(instant(until)),
        ("dismissed", _) => ReminderState::Dismissed,
        _ => ReminderState::Fired,
    };
    Ok(ReminderRow {
        account: row.get(0)?,
        event: row.get(1)?,
        fire_at: instant(row.get(2)?),
        end_at: instant(row.get(3)?),
        state,
    })
}

const COLUMNS: &str = "account, event_id, fire_at, end_at, state, snoozed_until";

/// Every row of `account`, by event id.
pub fn load(store: &Store, account: &str) -> Result<Vec<ReminderRow>> {
    let mut stmt = store.conn().prepare(&format!(
        "SELECT {COLUMNS} FROM {TABLE} WHERE account = ?1 ORDER BY event_id"
    ))?;
    let rows = stmt
        .query_map([account], row)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("reading the calendar reminders")?;
    Ok(rows)
}

/// The row of one event, if it has one.
pub fn get(store: &Store, account: &str, event: &str) -> Result<Option<ReminderRow>> {
    store
        .conn()
        .query_row(
            &format!("SELECT {COLUMNS} FROM {TABLE} WHERE account = ?1 AND event_id = ?2"),
            [account, event],
            row,
        )
        .optional()
        .context("reading a calendar reminder")
}

/// Write `row`, replacing the event's previous one.
pub fn put(store: &Store, row: &ReminderRow) -> Result<()> {
    let (state, snoozed) = match row.state {
        ReminderState::Fired => ("fired", None),
        ReminderState::Snoozed(until) => ("snoozed", Some(until.timestamp())),
        ReminderState::Dismissed => ("dismissed", None),
    };
    store
        .conn()
        .execute(
            &format!(
                "INSERT INTO {TABLE} ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT (account, event_id) DO UPDATE SET
                    fire_at = excluded.fire_at, end_at = excluded.end_at,
                    state = excluded.state, snoozed_until = excluded.snoozed_until"
            ),
            params![
                row.account,
                row.event,
                row.fire_at.timestamp(),
                row.end_at.timestamp(),
                state,
                snoozed,
            ],
        )
        .context("writing a calendar reminder")?;
    Ok(())
}

/// Remove the rows of `account` whose event ended by `now`; answers how
/// many went.
pub fn prune(store: &Store, account: &str, now: DateTime<Utc>) -> Result<usize> {
    store
        .conn()
        .execute(
            &format!("DELETE FROM {TABLE} WHERE account = ?1 AND end_at <= ?2"),
            params![account, now.timestamp()],
        )
        .context("pruning the calendar reminders")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn utc(h: u32, m: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 4, h, m, 0).unwrap()
    }

    fn reminder(event: &str, state: ReminderState) -> ReminderRow {
        ReminderRow {
            account: "work".into(),
            event: event.into(),
            fire_at: utc(8, 45),
            end_at: utc(10, 0),
            state,
        }
    }

    /// Each state reads back as written, a write replaces the event's row,
    /// and a calendar cache drop leaves the table alone.
    #[test]
    fn rows_round_trip_and_survive_a_cache_drop() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("store.sqlite3")).unwrap();
        let fired = reminder("ev-1", ReminderState::Fired);
        let snoozed = reminder("ev-2", ReminderState::Snoozed(utc(8, 50)));
        put(&store, &fired).unwrap();
        put(&store, &snoozed).unwrap();
        assert_eq!(load(&store, "work").unwrap(), vec![fired, snoozed.clone()]);

        let dismissed = reminder("ev-1", ReminderState::Dismissed);
        put(&store, &dismissed).unwrap();
        assert_eq!(get(&store, "work", "ev-1").unwrap(), Some(dismissed));
        assert_eq!(get(&store, "other", "ev-1").unwrap(), None);

        super::super::calendar::drop_calendar_cache(&store, "work").unwrap();
        assert_eq!(load(&store, "work").unwrap().len(), 2);
    }

    /// A row goes once its event has ended, and only that account's.
    #[test]
    fn prune_removes_the_rows_of_ended_events() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("store.sqlite3")).unwrap();
        put(&store, &reminder("ev-1", ReminderState::Fired)).unwrap();
        let mut later = reminder("ev-2", ReminderState::Dismissed);
        later.end_at = utc(12, 0);
        put(&store, &later).unwrap();
        assert_eq!(prune(&store, "work", utc(9, 59)).unwrap(), 0);
        assert_eq!(prune(&store, "work", utc(10, 0)).unwrap(), 1);
        assert_eq!(load(&store, "work").unwrap(), vec![later]);
    }
}
