//! The Graph calendar writes (PERSO-255, `docs/plans/calendar.md`, phase 2).
//!
//! - Create: `POST /me/calendars/{id}/events` with the event and its
//!   `transactionId`; the server sends the invitations.
//! - Update: `PATCH /me/events/{id}` with the fields the patch names, on an
//!   occurrence or a series master alike.
//! - Cancel: `POST /me/events/{id}/cancel`, with the comment for the
//!   attendees.
//! - Delete: `DELETE /me/events/{id}`.
//! - Reminders: `POST /me/events/{id}/dismissReminder` and
//!   `snoozeReminder` with the new reminder time.
//!
//! Each time goes out as the event's wall time plus the Windows name of its
//! zone, the form Graph takes, through CLDR's IANA-to-Windows table (every
//! territory's zones and their aliases); a zone the table lacks goes out as
//! UTC, with the instants exact. An all-day event
//! goes out as its midnights in UTC, which Graph keeps as the dates.
//!
//! A cancel, a delete, a dismiss or a snooze of an event Graph no longer has
//! (404) counts as done, the rule mp applies to deleting a mail already gone
//! from its folder; an update of one fails, since the change did not land.

use chrono::{DateTime, NaiveDateTime, Utc};
use log::{debug, warn};
use serde_json::{json, Map, Value};

use super::{
    error_text, from_graph, is_instance, series_of, wire, GraphCalendarBackend, GraphFetch,
    HttpAnswer,
};
use crate::calendar_backend::{
    BackendResult, CalendarBackend, CalendarBackendError, SeriesInfo, Written,
};
use crate::calendar_model::{AttendeeRole, Calendar, EventTime, Sensitivity, ShowAs, Tz};
use crate::calendar_write::{AttendeeInput, EventInput, EventPatch};
use crate::graph::{encode_id, GRAPH_BASE};

use anyhow::{anyhow, Context};
use std::str::FromStr;

/// The `dateTime` form of a write: wall time, whole seconds, no offset.
const WALL_TIME: &str = "%Y-%m-%dT%H:%M:%S";

/// A Graph `{dateTime, timeZone}` of `local` wall time in `zone`.
fn date_time_zone(local: NaiveDateTime, zone: &str) -> Value {
    json!({ "dateTime": local.format(WALL_TIME).to_string(), "timeZone": zone })
}

/// An instant as Graph's `{dateTime, timeZone}` in `zone`, an IANA name: its
/// wall time there and the zone's Windows name, or the UTC instant when the
/// table has no Windows name for the zone.
fn instant_in(at: DateTime<Utc>, zone: &str) -> Value {
    let windows = crate::windows_zones::iana_to_windows_zone(zone)
        .and_then(|windows| Some((windows, Tz::from_str(zone.trim()).ok()?)));
    match windows {
        Some((windows, tz)) => date_time_zone(at.with_timezone(&tz).naive_local(), windows),
        None => date_time_zone(at.naive_utc(), "UTC"),
    }
}

/// The `start`, `end` and `isAllDay` of `time`.
fn put_time(body: &mut Map<String, Value>, time: &EventTime) {
    let (start, end, all_day) = match time {
        EventTime::Timed { start, end, zone } => {
            (instant_in(*start, zone), instant_in(*end, zone), false)
        }
        EventTime::AllDay { .. } => {
            let (start, end) = time
                .all_day_midnights()
                .expect("an all-day time has its midnights");
            (
                date_time_zone(start, "UTC"),
                date_time_zone(end, "UTC"),
                true,
            )
        }
    };
    body.insert("start".into(), start);
    body.insert("end".into(), end);
    body.insert("isAllDay".into(), all_day.into());
}

fn attendee_json(attendee: &AttendeeInput) -> Value {
    let mut address = json!({ "address": attendee.email.trim() });
    if let Some(name) = attendee
        .name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
    {
        address["name"] = name.into();
    }
    let role = match attendee.role {
        AttendeeRole::Required => "required",
        AttendeeRole::Optional => "optional",
        AttendeeRole::Resource => "resource",
    };
    json!({ "emailAddress": address, "type": role })
}

fn show_as_name(show_as: ShowAs) -> &'static str {
    match show_as {
        ShowAs::Free => "free",
        ShowAs::Tentative => "tentative",
        ShowAs::Busy => "busy",
        ShowAs::Oof => "oof",
        ShowAs::WorkingElsewhere => "workingElsewhere",
        ShowAs::Unknown => "unknown",
    }
}

fn sensitivity_name(sensitivity: Sensitivity) -> &'static str {
    match sensitivity {
        Sensitivity::Normal => "normal",
        Sensitivity::Personal => "personal",
        Sensitivity::Private => "private",
        Sensitivity::Confidential => "confidential",
    }
}

/// `location`: an empty display name clears it.
fn location_json(location: Option<&str>) -> Value {
    json!({ "displayName": location.unwrap_or_default() })
}

/// `body` as plain text: an empty content clears it.
fn body_json(body: Option<&str>) -> Value {
    json!({ "contentType": "text", "content": body.unwrap_or_default() })
}

/// `isReminderOn` and `reminderMinutesBeforeStart` of a reminder: `None` is
/// off.
fn put_reminder(body: &mut Map<String, Value>, reminder: Option<u32>) {
    match reminder {
        Some(minutes) => {
            body.insert("isReminderOn".into(), true.into());
            body.insert("reminderMinutesBeforeStart".into(), minutes.into());
        }
        None => {
            body.insert("isReminderOn".into(), false.into());
        }
    }
}

/// `isOnlineMeeting` with Teams as the provider. Only turning it on is ever
/// sent: Graph ignores `isOnlineMeeting: false` on an online meeting, whose
/// Teams link stays, and the daemon refuses that change before it gets here.
fn put_online_meeting(body: &mut Map<String, Value>) {
    body.insert("isOnlineMeeting".into(), true.into());
    body.insert("onlineMeetingProvider".into(), "teamsForBusiness".into());
}

/// The body of `POST /me/calendars/{id}/events`: what `input` sets, the
/// server's defaults left to the server.
pub(super) fn create_body(input: &EventInput, transaction_id: &str) -> Value {
    let mut body = Map::new();
    body.insert("subject".into(), input.summary.clone().into());
    put_time(&mut body, &input.time);
    body.insert("transactionId".into(), transaction_id.into());
    if let Some(location) = input.location.as_deref() {
        body.insert("location".into(), location_json(Some(location)));
    }
    if let Some(text) = input.body.as_deref() {
        body.insert("body".into(), body_json(Some(text)));
    }
    if !input.attendees.is_empty() {
        let attendees = input.attendees.iter().map(attendee_json).collect();
        body.insert("attendees".into(), Value::Array(attendees));
    }
    if input.online_meeting {
        put_online_meeting(&mut body);
    }
    if let Some(reminder) = input.reminder {
        put_reminder(&mut body, reminder);
    }
    if let Some(show_as) = input.show_as {
        body.insert("showAs".into(), show_as_name(show_as).into());
    }
    if let Some(sensitivity) = input.sensitivity {
        body.insert("sensitivity".into(), sensitivity_name(sensitivity).into());
    }
    if let Some(allow) = input.allow_new_time_proposals {
        body.insert("allowNewTimeProposals".into(), allow.into());
    }
    Value::Object(body)
}

/// The body of `PATCH /me/events/{id}`: only the fields `patch` names, so a
/// change made elsewhere to any other field survives.
pub(super) fn patch_body(patch: &EventPatch) -> Value {
    let mut body = Map::new();
    if let Some(summary) = &patch.summary {
        body.insert("subject".into(), summary.clone().into());
    }
    if let Some(time) = &patch.time {
        put_time(&mut body, time);
    }
    if let Some(location) = &patch.location {
        body.insert("location".into(), location_json(location.as_deref()));
    }
    if let Some(text) = &patch.body {
        body.insert("body".into(), body_json(text.as_deref()));
    }
    if let Some(attendees) = &patch.attendees {
        let attendees = attendees.iter().map(attendee_json).collect();
        body.insert("attendees".into(), Value::Array(attendees));
    }
    if patch.online_meeting == Some(true) {
        put_online_meeting(&mut body);
    }
    if let Some(reminder) = patch.reminder {
        put_reminder(&mut body, reminder);
    }
    if let Some(show_as) = patch.show_as {
        body.insert("showAs".into(), show_as_name(show_as).into());
    }
    if let Some(sensitivity) = patch.sensitivity {
        body.insert("sensitivity".into(), sensitivity_name(sensitivity).into());
    }
    if let Some(allow) = patch.allow_new_time_proposals {
        body.insert("allowNewTimeProposals".into(), allow.into());
    }
    Value::Object(body)
}

/// `/me/events/{id}` plus `action`, when there is one.
fn event_url(event: &str, action: Option<&str>) -> String {
    let base = format!("{GRAPH_BASE}/me/events/{}", encode_id(event));
    match action {
        Some(action) => format!("{base}/{action}"),
        None => base,
    }
}

fn success(answer: &HttpAnswer) -> bool {
    (200..300).contains(&answer.status)
}

fn refused(what: &str, answer: &HttpAnswer) -> CalendarBackendError {
    anyhow!("Graph refused the {what}: {}", error_text(answer)).into()
}

impl<F: GraphFetch> GraphCalendarBackend<F> {
    /// The event the server answered a create or an update with, converted:
    /// a series master as its series, an occurrence with its series' `UID`
    /// (from the series this backend knows, else read from the server).
    async fn written(
        &self,
        calendar: &str,
        what: &str,
        answer: &HttpAnswer,
    ) -> BackendResult<Written> {
        let event: wire::Event = serde_json::from_str(&answer.body)
            .with_context(|| format!("unreadable Graph answer to the {what}"))?;
        if event.kind.as_deref() == Some("seriesMaster") {
            let series = series_of(&self.account, calendar, &event).ok_or_else(|| {
                anyhow!("Graph answered the {what} with a series without iCalUId")
            })?;
            self.remember_series(vec![series.clone()]);
            return Ok(Written::Series(Box::new(series)));
        }
        let mut series: Option<SeriesInfo> = None;
        if is_instance(event.kind.as_deref()) {
            if let Some(master) = event.series_master_id.as_deref() {
                series = match self.known_series(master) {
                    Some(known) => Some(known),
                    None => match self.fetch_series(calendar, master).await {
                        Ok(read) => {
                            self.remember_series(vec![read.clone()]);
                            Some(read)
                        }
                        Err(_) => {
                            warn!(
                                "[calendar] {}: could not read series master {master} after the \
                                 {what}; the occurrence keeps its own iCalUId until the next sync",
                                self.account
                            );
                            None
                        }
                    },
                };
            }
        }
        from_graph(&self.account, calendar, &event, series.as_ref())
            .map(|converted| Written::Event(Box::new(converted)))
            .map_err(|why| {
                anyhow!("Graph answered the {what} with an unreadable event: {why}").into()
            })
    }

    pub(super) async fn post_event(
        &self,
        calendar: &Calendar,
        input: &EventInput,
        transaction_id: &str,
    ) -> BackendResult<Written> {
        let url = format!(
            "{GRAPH_BASE}/me/calendars/{}/events",
            encode_id(&calendar.id)
        );
        let answer = self
            .fetch
            .post(&url, create_body(input, transaction_id))
            .await?;
        if !success(&answer) {
            return Err(refused("new event", &answer));
        }
        self.written(&calendar.id, "new event", &answer).await
    }

    pub(super) async fn patch_event(
        &self,
        calendar: &str,
        event: &str,
        patch: &EventPatch,
    ) -> BackendResult<Written> {
        let answer = self
            .fetch
            .patch(&event_url(event, None), patch_body(patch))
            .await?;
        if !success(&answer) {
            return Err(refused("update", &answer));
        }
        self.written(calendar, "update", &answer).await
    }

    /// Settle a call whose 404 means the event is already gone, which counts
    /// as done.
    fn done_or_gone(&self, what: &str, event: &str, answer: &HttpAnswer) -> BackendResult<()> {
        if success(answer) {
            return Ok(());
        }
        if answer.status == 404 {
            debug!(
                "[calendar] {}: event {event} is gone already; the {what} counts as done",
                self.account
            );
            return Ok(());
        }
        Err(refused(what, answer))
    }

    pub(super) async fn post_cancel(
        &self,
        event: &str,
        comment: Option<String>,
    ) -> BackendResult<()> {
        let mut body = json!({});
        if let Some(comment) = comment.filter(|c| !c.trim().is_empty()) {
            body["comment"] = comment.into();
        }
        let answer = self
            .fetch
            .post(&event_url(event, Some("cancel")), body)
            .await?;
        self.done_or_gone("cancel", event, &answer)
    }

    pub(super) async fn send_delete(&self, event: &str) -> BackendResult<()> {
        let answer = self.fetch.delete(&event_url(event, None)).await?;
        self.done_or_gone("delete", event, &answer)
    }

    pub(super) async fn post_dismiss(&self, event: &str) -> BackendResult<()> {
        let answer = self
            .fetch
            .post(&event_url(event, Some("dismissReminder")), json!({}))
            .await?;
        self.done_or_gone("reminder dismissal", event, &answer)
    }

    pub(super) async fn post_snooze(&self, event: &str, until: DateTime<Utc>) -> BackendResult<()> {
        let body = json!({ "newReminderTime": date_time_zone(until.naive_utc(), "UTC") });
        let answer = self
            .fetch
            .post(&event_url(event, Some("snoozeReminder")), body)
            .await?;
        self.done_or_gone("reminder snooze", event, &answer)
    }
}
