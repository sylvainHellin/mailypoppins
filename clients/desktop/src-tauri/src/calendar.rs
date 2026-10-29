//! The calendar (CAL-02, CAL-03, #0131, PERSO-200).
//!
//! `calendar.view` answers the events of a range, every account merged and
//! the hidden calendars left out, and `calendar.list` the calendars with
//! their visibility, which `calendar.set_visibility` changes; this layer
//! passes them through, and the layouts (agenda, day, week) are the
//! frontend's, as they are the TUI's. `event.get` answers one event. An
//! invitation mail's event (`imip:<row_id>`) has the mail's `invite.ics` as
//! its source, which `message.ics` answers as bytes: the TUI writes it to a
//! temp file and hands that to `$EDITOR` (`Action::OpenEventSource`), and
//! this writes it under the app's cache directory, readable by its owner
//! only, before the external editor opens it. Edits to that copy reach
//! nothing: the file is an artifact to read, not a draft.
//!
//! The answers (CAL-01, CAL-05, #0131, PERSO-200): `message.invite` answers
//! the event a message carries, which the reader's invitation card shows,
//! and on an account with a calendar backend the server event it stands for
//! (`event_id`); `event.respond` answers a server event through the backend
//! or an `imip:` event with the iMIP reply, an operation the GUI awaits as
//! `rsvp`. Which answers an account takes is the daemon's call: a refusal
//! comes back as its sentence, and nothing is probed ahead of it.
//!
//! A new invitation (SND-05) is `send.invite`, awaited as `send_invite`: the
//! daemon builds the `VEVENT` and the iMIP message and submits it through
//! its outbox. Its refusals are the CLI's (`--invite requires --subject`),
//! so the three a form can check (a subject, a start, one recipient) are
//! checked here before the call, in the form's words; any other refusal is
//! passed on as the daemon's sentence. The daemon refuses `send.invite` from
//! a Graph account before it reads anything else, so [`invite_refusal_on`]
//! asks `calendar.rsvp` with `{account}` alone, which is refused in the same
//! breath, and the form shows that sentence without a message in hand; an
//! IMAP account is never asked.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::{AppHandle, State};

use mp_client::queries;
use mp_protocol::calendar::{
    CalendarEvent, CalendarInfo, CalendarViewParams, CalendarViewResult, EventFrontmatter,
};

use crate::attachments::{cache_dir, write_private, RENDITIONS};
use crate::commands::{list_accounts_on, with_door, OperationStarted};
use crate::editor::{self, EditorLaunch, Lookup};
use crate::error::{refusal_sentence, Addressing, GuiError};
use crate::session::{Budgeted, Door, PendingKind, SessionHandle};

/// One `calendar.view` or `calendar.list` read: the server cache and the
/// invitation fold, daemon-side, which never reach a server.
const VIEW_BUDGET: Duration = Duration::from_secs(15);

/// One `event.get` or `calendar.set_visibility`.
const EVENT_BUDGET: Duration = Duration::from_secs(10);

/// One `invite.ics` blob read.
const ICS_BUDGET: Duration = Duration::from_secs(10);

/// What the TUI says when a row has no `invite.ics` (`Action::OpenEventSource`).
pub const NO_ICS: &str = "That event has no ics source in the store";

/// One invitation card read: a blob read and an ics parse.
const INVITE_BUDGET: Duration = Duration::from_secs(10);

/// Starting `event.respond`: its plan reads the event, and on an `imip:`
/// event the row and its `invite.ics`, first.
const RESPOND_START_BUDGET: Duration = Duration::from_secs(10);

/// The Graph probe: a refusal before any read.
const PROBE_BUDGET: Duration = Duration::from_secs(5);

/// Starting `send.invite`: the daemon plans the invitation first.
const INVITE_START_BUDGET: Duration = Duration::from_secs(10);

/// What a new invitation lacks, checked before `send.invite` is asked.
pub const NO_SUBJECT: &str = "An invitation needs a subject";
pub const NO_START: &str = "An invitation needs a start";
pub const NO_RECIPIENT: &str = "An invitation needs at least one recipient in To or Cc";

/// The three answers `event.respond` takes.
pub const RESPONSES: [&str; 3] = ["accept", "tentative", "decline"];

/// The reader's invitation card: the event a message carries, and on an
/// account with a calendar backend the server event it stands for, which
/// `event.respond` answers in its place (`imip:<row_id>` otherwise).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
pub struct InviteCard {
    pub event: EventFrontmatter,
    pub event_id: Option<String>,
}

/// Why `account` cannot send an invitation at all, as the daemon says it:
/// the Graph sentence, or null for an account that can.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
pub struct InviteRefusal {
    pub account: String,
    pub refusal: Option<String>,
}

/// The calendars of every account, or of `account`, with their visibility.
pub fn calendar_list_on(door: &Door, account: Option<&str>) -> Result<Vec<CalendarInfo>, GuiError> {
    let q = Budgeted {
        door,
        budget: VIEW_BUDGET,
    };
    queries::calendar_list(&q, account).map_err(|e| GuiError::from_call(&e, Addressing::Resource))
}

/// Show or hide one calendar; the daemon publishes `calendar.changed`, which
/// is what reads the view again.
pub fn calendar_set_visibility_on(
    door: &Door,
    account: &str,
    calendar: &str,
    visible: bool,
) -> Result<(), GuiError> {
    let q = Budgeted {
        door,
        budget: EVENT_BUDGET,
    };
    queries::calendar_set_visibility(&q, account, calendar, visible)
        .map_err(|e| GuiError::from_call(&e, Addressing::Resource))
        .map_err(daemon_sentence)
}

/// The events from `start` to `end` (RFC 3339 with an offset, `end`
/// exclusive) of every account's visible calendars, in start order;
/// `zone` places the all-day events' midnights.
pub fn calendar_view_on(
    door: &Door,
    start: &str,
    end: &str,
    zone: Option<&str>,
) -> Result<CalendarViewResult, GuiError> {
    let q = Budgeted {
        door,
        budget: VIEW_BUDGET,
    };
    let params = CalendarViewParams {
        start: start.to_string(),
        end: end.to_string(),
        zone: zone.map(str::to_string),
        ..CalendarViewParams::default()
    };
    queries::calendar_view(&q, &params).map_err(|e| GuiError::from_call(&e, Addressing::Params))
}

/// One event by its id, a server id or `imip:<row_id>`.
pub fn event_get_on(door: &Door, account: &str, event: &str) -> Result<CalendarEvent, GuiError> {
    let q = Budgeted {
        door,
        budget: EVENT_BUDGET,
    };
    queries::event_get(&q, account, event)
        .map_err(|e| GuiError::from_call(&e, Addressing::Resource))
}

/// Answer `event` with `response`, the TUI's respond overlay: an operation
/// awaited as `rsvp`, whose `result` is an `EventRespondResult`. A blank
/// comment is no comment, and `send_response` is the daemon's default (tell
/// the organizer) when absent. A response word the daemon does not take is
/// refused here, before any call; a daemon refusal (`send_response: false`
/// on an invitation mail, a Graph account on the iMIP path) comes back as
/// its own sentence.
pub fn event_respond_on(
    session: &SessionHandle,
    door: &Door,
    account: &str,
    event: &str,
    response: &str,
    comment: Option<&str>,
    send_response: Option<bool>,
) -> Result<OperationStarted, GuiError> {
    if !RESPONSES.contains(&response) {
        return Err(GuiError::protocol(format!(
            "An answer is accept, tentative or decline, not {response:?}"
        )));
    }
    let comment = comment.map(str::trim).filter(|c| !c.is_empty());
    let params = queries::event_respond_params(
        account,
        event,
        response,
        comment,
        send_response.unwrap_or(true),
    );
    let operation_id = session
        .start_operation(
            door,
            "event.respond",
            params,
            PendingKind::Rsvp,
            RESPOND_START_BUDGET,
        )
        .map_err(daemon_sentence)?;
    Ok(OperationStarted { operation_id })
}

/// Where row `row_id`'s `invite.ics` is written: one file per row, so the
/// same entry opened twice rewrites one file.
pub fn invite_source_path(cache_dir: &Path, account: &str, row_id: i64) -> PathBuf {
    let account: String = account
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    cache_dir
        .join(RENDITIONS)
        .join(format!("invite-{account}-{row_id}.ics"))
}

/// Write row `row_id`'s `invite.ics` under `cache_dir` (0700 directory, 0600
/// file) and answer its path; a row with no ics is [`NO_ICS`].
pub fn invite_source_write_on(
    door: &Door,
    cache_dir: &Path,
    account: &str,
    row_id: i64,
) -> Result<PathBuf, GuiError> {
    let q = Budgeted {
        door,
        budget: ICS_BUDGET,
    };
    let bytes = queries::message_ics(&q, account, row_id)
        .map_err(|e| GuiError::from_call(&e, Addressing::Resource))?
        .ok_or_else(|| GuiError::not_found(NO_ICS))?;
    let path = invite_source_path(cache_dir, account, row_id);
    write_private(cache_dir, &path, &bytes)?;
    Ok(path)
}

/// Open row `row_id`'s `invite.ics` in the external editor (the TUI's agenda
/// Enter and `e`); with a fixture door the editor is journaled, not run.
pub fn invite_source_open_on(
    door: &Door,
    cache_dir: &Path,
    lookup: &Lookup,
    account: &str,
    row_id: i64,
    window: Duration,
) -> Result<EditorLaunch, GuiError> {
    let path = invite_source_write_on(door, cache_dir, account, row_id)?;
    let fixture = match door {
        Door::Fixture(fixture) => Some(fixture.as_ref()),
        Door::Daemon(_) => None,
    };
    editor::open_on(fixture, lookup, &path.to_string_lossy(), window)
}

/// The invitation card of row `row_id` of `account`: its event and the
/// server event it stands for; `None` for a row with no iMIP payload or one
/// that does not parse.
pub fn invite_get_on(
    door: &Door,
    account: &str,
    row_id: i64,
) -> Result<Option<InviteCard>, GuiError> {
    let q = Budgeted {
        door,
        budget: INVITE_BUDGET,
    };
    let card = queries::message_invite_card(&q, account, row_id)
        .map_err(|e| GuiError::from_call(&e, Addressing::Resource))?;
    Ok(card.map(|card| InviteCard {
        event: card.event,
        event_id: card.event_id,
    }))
}

/// Whether `account` can send invitations: a Graph account
/// (`account.list`'s `backend`) is asked `calendar.rsvp {account}` alone,
/// which the daemon refuses with its Graph sentence before it looks at
/// anything else, so no operation starts; an IMAP account answers null
/// without a call.
pub fn invite_refusal_on(door: &Door, account: &str) -> Result<InviteRefusal, GuiError> {
    let accounts = list_accounts_on(door, None)?;
    let info = accounts
        .iter()
        .find(|a| a.name == account)
        .ok_or_else(|| GuiError::not_found(format!("account_unknown: {account}")))?;
    if info.backend != "graph" {
        return Ok(InviteRefusal {
            account: account.to_string(),
            refusal: None,
        });
    }
    match door.call_within("calendar.rsvp", json!({"account": account}), PROBE_BUDGET) {
        Err(e) => {
            let text = format!("{e:#}");
            match refusal_sentence(&text) {
                Some(sentence) => Ok(InviteRefusal {
                    account: account.to_string(),
                    refusal: Some(sentence.to_string()),
                }),
                None => Err(GuiError::from_call_text(&text, Addressing::Params)),
            }
        }
        Ok(_) => Err(GuiError::protocol(
            "calendar.rsvp started with no response to send; the Graph probe expected a refusal",
        )),
    }
}

/// A new invitation, the form's fields: every one but `account`, `subject`
/// and `start` may be empty, and an empty one is not sent. `end` and
/// `duration` are exclusive, which the daemon checks.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct InviteFields {
    pub subject: String,
    pub start: String,
    pub to: Option<String>,
    pub cc: Option<String>,
    pub end: Option<String>,
    pub duration: Option<String>,
    pub location: Option<String>,
    pub description: Option<String>,
}

/// The `send.invite` parameters for `fields`, or the form's sentence for
/// what it lacks: a subject, a start, then one recipient in To or Cc.
pub fn invite_params(account: &str, fields: &InviteFields) -> Result<serde_json::Value, GuiError> {
    let filled = |v: &Option<String>| {
        v.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let subject = fields.subject.trim();
    if subject.is_empty() {
        return Err(GuiError::protocol(NO_SUBJECT));
    }
    let start = fields.start.trim();
    if start.is_empty() {
        return Err(GuiError::protocol(NO_START));
    }
    let to = filled(&fields.to);
    let cc = filled(&fields.cc);
    let addressed = |v: &Option<String>| {
        v.as_deref()
            .is_some_and(|s| s.split([',', ';']).any(|a| !a.trim().is_empty()))
    };
    if !addressed(&to) && !addressed(&cc) {
        return Err(GuiError::protocol(NO_RECIPIENT));
    }
    let mut params = json!({"account": account, "subject": subject, "start": start});
    for (key, value) in [
        ("to", to),
        ("cc", cc),
        ("end", filled(&fields.end)),
        ("duration", filled(&fields.duration)),
        ("location", filled(&fields.location)),
        ("description", filled(&fields.description)),
    ] {
        if let Some(value) = value {
            params[key] = json!(value);
        }
    }
    Ok(params)
}

/// Send a new invitation from `account`, awaited as `send_invite`, whose
/// `result` is a `SendOutcome`. What the form can check is refused before
/// the call ([`invite_params`]); a daemon refusal comes back as its own
/// sentence, the Graph one first of all.
pub fn send_invite_on(
    session: &SessionHandle,
    door: &Door,
    account: &str,
    fields: &InviteFields,
) -> Result<OperationStarted, GuiError> {
    let params = invite_params(account, fields)?;
    let operation_id = session
        .start_operation(
            door,
            "send.invite",
            params,
            PendingKind::SendInvite,
            INVITE_START_BUDGET,
        )
        .map_err(daemon_sentence)?;
    Ok(OperationStarted { operation_id })
}

/// A daemon refusal with its message cut to the daemon's own sentence, the
/// code kept; any other error as it was.
pub(crate) fn daemon_sentence(error: GuiError) -> GuiError {
    let cut = |message: String| refusal_sentence(&message).map_or(message.clone(), str::to_string);
    match error {
        GuiError::Protocol {
            message,
            code: Some(code),
        } => GuiError::Protocol {
            message: cut(message),
            code: Some(code),
        },
        GuiError::NotFound {
            message,
            code: Some(code),
        } => GuiError::NotFound {
            message: cut(message),
            code: Some(code),
        },
        other => other,
    }
}

// ---------------------------------------------------------------------------
// The commands
// ---------------------------------------------------------------------------

/// Send a new invitation, awaited as `send_invite`.
#[allow(clippy::too_many_arguments)]
#[tauri::command(rename_all = "snake_case")]
pub async fn send_invite(
    session: State<'_, SessionHandle>,
    account: String,
    subject: String,
    start: String,
    to: Option<String>,
    cc: Option<String>,
    end: Option<String>,
    duration: Option<String>,
    location: Option<String>,
    description: Option<String>,
) -> Result<OperationStarted, GuiError> {
    let fields = InviteFields {
        subject,
        start,
        to,
        cc,
        end,
        duration,
        location,
        description,
    };
    with_door(&session, move |session, door| {
        send_invite_on(session, door, &account, &fields)
    })
    .await
}

/// The invitation card of a row: its event and its server event, or null.
#[tauri::command(rename_all = "snake_case")]
pub async fn invite_get(
    session: State<'_, SessionHandle>,
    account: String,
    row_id: i64,
) -> Result<Option<InviteCard>, GuiError> {
    with_door(&session, move |_, door| {
        invite_get_on(door, &account, row_id)
    })
    .await
}

/// Answer an event, awaited as `rsvp`.
#[tauri::command(rename_all = "snake_case")]
pub async fn event_respond(
    session: State<'_, SessionHandle>,
    account: String,
    event: String,
    response: String,
    comment: Option<String>,
    send_response: Option<bool>,
) -> Result<OperationStarted, GuiError> {
    with_door(&session, move |session, door| {
        event_respond_on(
            session,
            door,
            &account,
            &event,
            &response,
            comment.as_deref(),
            send_response,
        )
    })
    .await
}

/// The calendars, of every account or of `account`.
#[tauri::command(rename_all = "snake_case")]
pub async fn calendar_list(
    session: State<'_, SessionHandle>,
    account: Option<String>,
) -> Result<Vec<CalendarInfo>, GuiError> {
    with_door(&session, move |_, door| {
        calendar_list_on(door, account.as_deref())
    })
    .await
}

/// Show or hide one calendar.
#[tauri::command(rename_all = "snake_case")]
pub async fn calendar_set_visibility(
    session: State<'_, SessionHandle>,
    account: String,
    calendar: String,
    visible: bool,
) -> Result<(), GuiError> {
    with_door(&session, move |_, door| {
        calendar_set_visibility_on(door, &account, &calendar, visible)
    })
    .await
}

/// The events of a range.
#[tauri::command(rename_all = "snake_case")]
pub async fn calendar_view(
    session: State<'_, SessionHandle>,
    start: String,
    end: String,
    zone: Option<String>,
) -> Result<CalendarViewResult, GuiError> {
    with_door(&session, move |_, door| {
        calendar_view_on(door, &start, &end, zone.as_deref())
    })
    .await
}

/// One event.
#[tauri::command(rename_all = "snake_case")]
pub async fn event_get(
    session: State<'_, SessionHandle>,
    account: String,
    event: String,
) -> Result<CalendarEvent, GuiError> {
    with_door(&session, move |_, door| {
        event_get_on(door, &account, &event)
    })
    .await
}

/// The Graph probe; see [`invite_refusal_on`].
#[tauri::command(rename_all = "snake_case")]
pub async fn invite_refusal(
    session: State<'_, SessionHandle>,
    account: String,
) -> Result<InviteRefusal, GuiError> {
    with_door(&session, move |_, door| invite_refusal_on(door, &account)).await
}

/// `not_found` when the row has no `invite.ics`.
#[tauri::command(rename_all = "snake_case")]
pub async fn invite_source_open(
    app: AppHandle,
    session: State<'_, SessionHandle>,
    account: String,
    row_id: i64,
) -> Result<EditorLaunch, GuiError> {
    let settings = editor::settings_file(&app)?;
    with_door(&session, move |_, door| {
        let cache = cache_dir(&app, door)?;
        invite_source_open_on(
            door,
            &cache,
            &editor::live_lookup(editor::read_setting_or_none(&settings)),
            &account,
            row_id,
            editor::EXIT_WINDOW,
        )
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::{Fixture, INVITE_ROW};
    use std::sync::Arc;

    fn fixture_door() -> (Door, Arc<Fixture>) {
        let (tx, rx) = std::sync::mpsc::channel();
        std::mem::forget(rx);
        let fixture = Arc::new(Fixture::load(tx).expect("fixture"));
        (Door::Fixture(Arc::clone(&fixture)), fixture)
    }

    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn any_file(p: &Path) -> bool {
        p.is_file()
    }

    fn lookup() -> Lookup<'static> {
        Lookup {
            env: &no_env,
            setting: Some("zed --wait".to_string()),
            is_file: &any_file,
            macos: true,
        }
    }

    #[test]
    fn calendar_view_on_decodes_the_fixture_week_and_follows_a_hidden_calendar() {
        let (door, f) = fixture_door();
        let (start, end) = ("2099-10-12T00:00:00+02:00", "2099-10-19T00:00:00+02:00");
        let week = calendar_view_on(&door, start, end, Some("Europe/Berlin")).expect("week");
        assert_eq!(week.zone, "Europe/Berlin");
        assert_eq!(week.events.len(), 5);
        let steering = week
            .events
            .iter()
            .find(|e| e.id == format!("imip:{INVITE_ROW}"))
            .expect("the invitation's event");
        assert_eq!(steering.summary, "Steering committee");
        assert_eq!(steering.source.as_ref().map(|s| s.row_id), Some(INVITE_ROW));
        let calendars = calendar_list_on(&door, None).expect("list");
        assert_eq!(calendars.len(), 3);
        calendar_set_visibility_on(&door, "home", "AAMk-cal-holidays", false).expect("hidden");
        let week = calendar_view_on(&door, start, end, None).expect("week");
        assert!(week
            .events
            .iter()
            .all(|e| e.calendar != "AAMk-cal-holidays"));
        assert_eq!(
            f.calls().last().map(|(m, _)| m.as_str()),
            Some("calendar.view")
        );
        assert!(matches!(
            calendar_set_visibility_on(&door, "home", "nope", true),
            Err(GuiError::NotFound { .. } | GuiError::Protocol { .. })
        ));
        let design = event_get_on(&door, "home", "AAMkAD-design").expect("get");
        assert!(design.online_meeting_url.is_some());
    }

    #[cfg(unix)]
    #[test]
    fn invite_source_open_writes_a_private_file_and_journals_the_editor() {
        use std::os::unix::fs::PermissionsExt;
        let (door, f) = fixture_door();
        let cache = crate::test_support::scratch_dir("invite-source");
        let launch = invite_source_open_on(
            &door,
            &cache,
            &lookup(),
            "work",
            INVITE_ROW,
            Duration::from_millis(100),
        )
        .expect("opened");
        assert_eq!(launch.pid, None, "a fixture spawns nothing");
        let path = invite_source_path(&cache, "work", INVITE_ROW);
        assert!(
            path.ends_with("renditions/invite-work-1008.ics"),
            "{path:?}"
        );
        let text = std::fs::read_to_string(&path).expect("written");
        assert!(text.contains("SUMMARY:Steering committee"), "{text}");
        let mode = |p: &Path| std::fs::metadata(p).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(path.parent().expect("dir")), 0o700);
        let opens = f.editor_opens();
        assert_eq!(opens.len(), 1);
        assert_eq!(opens[0].path, path.to_string_lossy());
        assert_eq!(
            opens[0].command.last().map(String::as_str),
            Some(opens[0].path.as_str())
        );
    }

    #[test]
    fn a_row_without_an_ics_is_not_found_and_opens_nothing() {
        let (door, f) = fixture_door();
        let cache = crate::test_support::scratch_dir("invite-none");
        let err = invite_source_open_on(
            &door,
            &cache,
            &lookup(),
            "work",
            9104,
            Duration::from_millis(100),
        )
        .expect_err("no ics");
        assert_eq!(err, GuiError::not_found(NO_ICS));
        assert!(f.editor_opens().is_empty());
        assert!(!invite_source_path(&cache, "work", 9104).exists());
        assert!(matches!(
            invite_source_open_on(
                &door,
                &cache,
                &lookup(),
                "work",
                424_242,
                Duration::from_millis(100)
            ),
            Err(GuiError::NotFound { .. })
        ));
    }

    #[test]
    fn invite_get_on_answers_the_card_of_an_invitation_and_null_for_a_plain_email() {
        let (door, _f) = fixture_door();
        let card = invite_get_on(&door, "work", INVITE_ROW)
            .expect("read")
            .expect("an event");
        assert_eq!(card.event.summary.as_deref(), Some("Steering committee"));
        assert_eq!(card.event.rsvp, "needs-action");
        assert_eq!(card.event_id, None, "work has no calendar backend");
        let server = invite_get_on(&door, "home", 9201)
            .expect("read")
            .expect("an event");
        assert_eq!(server.event_id.as_deref(), Some("AAMkAD-dentist"));
        assert_eq!(invite_get_on(&door, "work", 1009).expect("read"), None);
        assert!(matches!(
            invite_get_on(&door, "work", 424_242),
            Err(GuiError::NotFound { .. })
        ));
    }

    #[test]
    fn the_graph_probe_answers_the_daemon_sentence_and_starts_nothing() {
        let (door, f) = fixture_door();
        let probe = invite_refusal_on(&door, "home").expect("probed");
        assert_eq!(probe.account, "home");
        assert_eq!(
            probe.refusal.as_deref(),
            Some(crate::fixture::GRAPH_RSVP_REFUSAL)
        );
        assert_eq!(f.operation_count(), 0);
        let rsvps: Vec<_> = f
            .calls()
            .into_iter()
            .filter(|(m, _)| m == "calendar.rsvp")
            .collect();
        assert_eq!(
            rsvps,
            vec![("calendar.rsvp".to_string(), json!({"account": "home"}))]
        );
    }

    #[test]
    fn an_imap_account_is_not_probed() {
        let (door, f) = fixture_door();
        let probe = invite_refusal_on(&door, "work").expect("answered");
        assert_eq!(probe.refusal, None);
        assert!(f.calls().iter().all(|(m, _)| m != "calendar.rsvp"));
        assert!(matches!(
            invite_refusal_on(&door, "nobody"),
            Err(GuiError::NotFound { .. })
        ));
    }

    #[test]
    fn an_answer_word_the_daemon_does_not_take_is_refused_before_any_call() {
        let (door, f) = fixture_door();
        let session = SessionHandle::new(true);
        let imip = format!("imip:{INVITE_ROW}");
        let err = event_respond_on(&session, &door, "work", &imip, "maybe", None, None)
            .expect_err("refused");
        assert!(matches!(err, GuiError::Protocol { .. }), "{err:?}");
        assert!(f.calls().is_empty());
        let started = event_respond_on(
            &session,
            &door,
            "home",
            "AAMkAD-design",
            "decline",
            Some("  On leave "),
            Some(false),
        )
        .expect("started");
        assert_eq!(
            session.pending_kind(&started.operation_id),
            Some(PendingKind::Rsvp)
        );
        let (method, params) = f.calls().last().cloned().expect("a call");
        assert_eq!(method, "event.respond");
        assert_eq!(params["comment"], "On leave");
        assert_eq!(params["send_response"], false);
        assert_eq!(
            event_respond_on(
                &session,
                &door,
                "work",
                &imip,
                "accept",
                Some(" "),
                Some(false)
            ),
            Err(GuiError::Protocol {
                message: crate::fixture::IMIP_SILENT_REFUSAL.to_string(),
                code: Some(-32602)
            })
        );
    }

    fn fields() -> InviteFields {
        InviteFields {
            subject: "Kick-off".into(),
            start: "2099-12-01T10:00".into(),
            to: Some("robin@example.com".into()),
            duration: Some("1h".into()),
            ..InviteFields::default()
        }
    }

    #[test]
    fn a_new_invitation_is_checked_for_a_subject_a_start_and_a_recipient_in_that_order() {
        let none = InviteFields::default();
        assert_eq!(
            invite_params("work", &none),
            Err(GuiError::protocol(NO_SUBJECT))
        );
        let no_start = InviteFields {
            subject: " Kick-off ".into(),
            ..InviteFields::default()
        };
        assert_eq!(
            invite_params("work", &no_start),
            Err(GuiError::protocol(NO_START))
        );
        let nobody = InviteFields {
            to: Some(" , ".into()),
            cc: Some("".into()),
            ..fields()
        };
        assert_eq!(
            invite_params("work", &nobody),
            Err(GuiError::protocol(NO_RECIPIENT))
        );
        let cc_only = InviteFields {
            to: None,
            cc: Some("kim@example.com".into()),
            location: Some("  ".into()),
            ..fields()
        };
        assert_eq!(
            invite_params("work", &cc_only).expect("params"),
            json!({
                "account": "work", "subject": "Kick-off", "start": "2099-12-01T10:00",
                "cc": "kim@example.com", "duration": "1h"
            })
        );
    }

    #[test]
    fn send_invite_starts_send_invite_and_passes_a_daemon_refusal_on_as_its_sentence() {
        let (door, f) = fixture_door();
        let session = SessionHandle::new(true);
        let err = send_invite_on(&session, &door, "work", &InviteFields::default())
            .expect_err("no subject");
        assert_eq!(err, GuiError::protocol(NO_SUBJECT));
        assert!(f.calls().is_empty(), "checked before any call");
        let graph = send_invite_on(&session, &door, "home", &fields()).expect_err("graph");
        assert_eq!(
            graph,
            GuiError::Protocol {
                message: crate::fixture::GRAPH_INVITE_REFUSAL.to_string(),
                code: Some(-32602)
            }
        );
        let open_ended = InviteFields {
            duration: None,
            ..fields()
        };
        let err = send_invite_on(&session, &door, "work", &open_ended).expect_err("no end");
        assert_eq!(err.message(), "An invite needs --end or --duration");
        assert_eq!(f.operation_count(), 0);
        let started = send_invite_on(&session, &door, "work", &fields()).expect("started");
        assert_eq!(
            session.pending_kind(&started.operation_id),
            Some(PendingKind::SendInvite)
        );
    }

    #[test]
    fn an_account_name_cannot_leave_the_renditions_directory() {
        let path = invite_source_path(Path::new("/c"), "../x y", 7);
        assert_eq!(path, Path::new("/c/renditions/invite-___x_y-7.ics"));
    }
}
