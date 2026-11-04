//! Free/busy through Graph (PERSO-255, `docs/plans/calendar.md`, phase 2,
//! slice 7): `POST /me/calendar/getSchedule`, which `Calendars.ReadWrite`
//! covers (the call needs `Calendars.ReadBasic` or more).
//!
//! The request names the addresses, the range as UTC wall times and the slot
//! length (`availabilityViewInterval`). Graph answers one
//! `scheduleInformation` per address in the order asked: the
//! `availabilityView`, one digit per slot, and the `scheduleItems` that are
//! not free, their times in UTC whatever zone the request named. An address
//! Graph cannot read (outside the tenant, or a mailbox it does not know)
//! answers an `error` instead (`responseCode` 5016, "Proxy web request
//! failed."), which becomes that schedule's error while the others stand.
//! A 403 is the token's scope.

use anyhow::{anyhow, Context};
use serde::Deserialize;
use serde_json::{json, Value};

use super::{
    error_text, instant_of, show_as, wire, GraphCalendarBackend, GraphFetch, CALENDAR_SCOPE_NAME,
};
use crate::calendar_backend::{BackendResult, CalendarBackendError};
use crate::calendar_freebusy::{FreeBusyQuery, Schedule, ScheduleItem};
use crate::graph::GRAPH_BASE;

/// The `dateTime` form of the request: UTC wall time, whole seconds.
const WALL_TIME: &str = "%Y-%m-%dT%H:%M:%S";

/// `getSchedule`'s answer, reduced to what is read.
#[derive(Debug, Deserialize)]
struct Answer {
    #[serde(default)]
    value: Vec<Information>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Information {
    schedule_id: Option<String>,
    #[serde(default)]
    availability_view: String,
    #[serde(default)]
    schedule_items: Vec<Item>,
    error: Option<FreeBusyError>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Item {
    start: wire::DateTimeZone,
    end: wire::DateTimeZone,
    status: Option<String>,
    subject: Option<String>,
    location: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FreeBusyError {
    message: Option<String>,
    response_code: Option<String>,
}

/// The body of `POST /me/calendar/getSchedule` for `query`.
pub(super) fn schedule_body(query: &FreeBusyQuery) -> Value {
    let at = |instant: chrono::DateTime<chrono::Utc>| {
        json!({
            "dateTime": instant.naive_utc().format(WALL_TIME).to_string(),
            "timeZone": "UTC",
        })
    };
    json!({
        "schedules": query.addresses,
        "startTime": at(query.window.start),
        "endTime": at(query.window.end),
        "availabilityViewInterval": query.interval,
    })
}

/// Text Graph sends empty when it has none.
fn filled(value: Option<String>) -> Option<String> {
    value
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// One `scheduleInformation`, converted; an item whose times do not read is
/// dropped, the view still showing its slots.
fn schedule_of(address: &str, info: Information) -> Schedule {
    if let Some(error) = info.error {
        let message = filled(error.message).unwrap_or_else(|| "no free/busy".to_string());
        let message = message.trim_end_matches('.').to_string();
        let why = match filled(error.response_code) {
            Some(code) => format!("{message} ({code})"),
            None => message,
        };
        return Schedule::unavailable(address, why);
    }
    let mut items: Vec<ScheduleItem> = info
        .schedule_items
        .into_iter()
        .filter_map(|item| {
            Some(ScheduleItem {
                start: instant_of(&item.start).ok()?,
                end: instant_of(&item.end).ok()?,
                status: show_as(item.status.as_deref()),
                subject: filled(item.subject),
                location: filled(item.location),
            })
        })
        .collect();
    items.sort_by_key(|item| item.start);
    Schedule {
        address: address.to_string(),
        view: info.availability_view,
        items,
        error: None,
    }
}

/// The schedules of `answer` in the order `query` asked, each matched to its
/// address by `scheduleId`; an address the answer leaves out has none.
pub(super) fn parse_schedules(query: &FreeBusyQuery, body: &str) -> anyhow::Result<Vec<Schedule>> {
    let answer: Answer =
        serde_json::from_str(body).context("unreadable Graph getSchedule answer")?;
    let mut infos: Vec<Option<Information>> = answer.value.into_iter().map(Some).collect();
    Ok(query
        .addresses
        .iter()
        .map(|address| {
            let found = infos.iter_mut().find(|info| {
                info.as_ref()
                    .and_then(|i| i.schedule_id.as_deref())
                    .is_some_and(|id| id.eq_ignore_ascii_case(address))
            });
            match found.and_then(Option::take) {
                Some(info) => schedule_of(address, info),
                None => Schedule::unavailable(address, "the server answered nothing for it"),
            }
        })
        .collect())
}

impl<F: GraphFetch> GraphCalendarBackend<F> {
    pub(super) async fn post_schedule(
        &self,
        query: &FreeBusyQuery,
    ) -> BackendResult<Vec<Schedule>> {
        let url = format!("{GRAPH_BASE}/me/calendar/getSchedule");
        let answer = self.fetch.post(&url, schedule_body(query)).await?;
        if answer.status == 403 {
            return Err(CalendarBackendError::MissingScope {
                scope: CALENDAR_SCOPE_NAME.into(),
            });
        }
        if !(200..300).contains(&answer.status) {
            return Err(
                anyhow!("Graph refused the free/busy query: {}", error_text(&answer)).into(),
            );
        }
        Ok(parse_schedules(query, &answer.body)?)
    }
}
