//! The `calendar.*` and `event.*` families: the iMIP agenda's rebuild, the
//! attendee's reply (P4-U14), and the server calendar (PERSO-200).
//!
//! ```text
//! calendar.list           {account?}
//!                             -> {calendars:[CalendarInfo]}
//! calendar.view           {start, end, accounts?, calendars?, include_hidden?, zone?}
//!                             -> {range, zone, window, events:[CalendarEvent]}
//! calendar.set_visibility {account, calendar, visible}
//!                             -> {account, calendar, visible}
//! calendar.rebuild        {account}
//!                             -> {operation_id}
//!                             settles {account, resolved, invites_seen,
//!                                      replies_seen, cancelled,
//!                                      server?: {calendars, upserts, removed,
//!                                                baselines, failed} | {error}}
//! calendar.rsvp           {account, selector|row_id, mailbox?, response}
//!                             -> {operation_id}
//!                             settles {account, selector, response, subject,
//!                                      organizer, message_id, delivered}
//! event.create            {account, calendar?, transaction_id?, event: EventInputParams}
//!                             -> {operation_id, transaction_id}
//!                             settles EventWriteResult
//! event.get               {account, event}
//!                             -> {account, event: CalendarEvent, series?}
//! event.respond           {account, event, response, comment?, send_response?}
//!                             -> {operation_id}
//!                             settles EventRespondResult
//! event.update            {account, event, fields: EventPatchParams}
//!                             -> {operation_id}
//!                             settles EventUpdateResult {account, event}
//! event.cancel            {account, event, comment?}
//!                             -> {operation_id}
//!                             settles EventCancelResult {account, event, notified}
//! event.delete            {account, event}
//!                             -> {operation_id}
//!                             settles EventDeleteResult {account, event}
//! ```
//!
//! `calendar.rebuild`, `calendar.rsvp` and every `event.*` but `event.get`
//! are operations. A
//! rebuild folds every stored `METHOD:REPLY` onto every stored invitation, and
//! drops and resyncs the server cache of an account with a backend; an answer
//! *sends*, over SMTP through the durable outbox or to the server's calendar
//! API, which is an unbounded network round trip. All three are
//! [`super::super::dispatch::CancelScope::Durable`]: an RSVP abandoned half
//! way is exactly the ambiguous submission
//! [`crate::outbox::sweep_pending_sends`] exists to park for a human.
//!
//! An answer observes its cancel token at its commit point, after the reply
//! is built and immediately before the submission, through [`commit_point`]: a
//! reply cancelled before then is dropped unsent. A cancel that lands once the
//! submission has started cannot recall it; the operation still reads
//! `cancelled`, which is why a client words that end as having stopped waiting.
//!
//! `event.respond` answers through the account's calendar backend when the
//! event is a server event, and through the iMIP reply of `calendar.rsvp` when
//! it is an invitation mail's (`imip:<row_id>`); `calendar.rsvp` stays as the
//! iMIP path's own address, though every client answers through `event.respond`.
//! An answer through the backend is followed by an immediate calendar sync,
//! and every answer that went through announces `calendar.changed`.
//!
//! `event.create` (PERSO-255) resolves the event a client typed and its
//! calendar before an operation exists, and answers the operation id with
//! the transaction id the create carries (the client's, or one it mints),
//! so a client whose operation vanished retries with the same id and the
//! server makes one event. The operation settles at once with an event the
//! cache already holds under that transaction id when the event matches the
//! input, and patches that event with the input's fields when the retry
//! changed them; otherwise it creates the event. A written event goes into
//! the cache under the account's calendar lock before the account syncs and
//! `calendar.changed` is announced. The cache is written only once the server
//! answered.
//!
//! Its refusals, all before an operation starts and `-32602` unless named:
//! an account without a calendar backend, the sign-in without the calendar
//! scope (the re-login prompt, `-32603`), an event the client typed wrong
//! (the sentence names the field), a calendar the account does not list, or
//! one the user may not write to.
//!
//! `event.update`, `event.cancel` and `event.delete` (PERSO-255) address a
//! server event the cache holds, read before an operation exists. An update
//! is a patch of the fields the client names, read against the cached event
//! ([`crate::calendar_write::resolve_patch`]); a cancel tells every attendee,
//! with an optional comment; a delete tells nobody. The server's answer goes
//! into the cache under the account's calendar lock, the updated event
//! upserted and the cancelled or deleted one removed, before the account
//! syncs and `calendar.changed` is announced. A cancel or a delete of an
//! event the server no longer holds counts as done, and so does a retry of
//! the same cancel or delete once the cache dropped the event: the removal
//! is remembered with it ([`cache::remove_event`]), so the retry settles as
//! the first call did without reaching the server.
//!
//! Their refusals, all `-32602` before an operation starts but the re-login
//! prompt: an invitation mail's event (`imip:`), an account without a
//! backend, an event the cache does not hold (and mp did not just cancel or
//! delete through the same call), an occurrence or a series (the
//! scopes are a later slice), a read-only calendar; for an update, an empty
//! change, a change by an attendee to anything but the reminder and the
//! show-as ([`ORGANIZER_ONLY_REFUSAL`]), turning a Teams meeting's link off,
//! a Teams meeting on a calendar that offers none, and a new description on a
//! Teams meeting, whose join details the server would drop; for a cancel, an
//! event the user does not organize or one without attendees; for a delete,
//! a live meeting, which the organizer cancels and an attendee declines.
//!
//! `calendar.set_visibility` is a command: a local preference, but one
//! `calendar.list` and `calendar.view` observe, so it answers with the
//! revision its `calendar.changed` moved the daemon to, as `signature.set_default`
//! does for its own preference.
//!
//! ## The refusals, and their order
//!
//! `ANO-4` first on the iMIP path: a Graph account is refused in the sentence
//! `mp invite` has always printed, **before** anything about the selector is
//! examined, so a GUI can show the RSVP buttons disabled with their reason
//! without naming a resolvable message. Then the response word, then the
//! selector's account, then the row, then the invitation on it. Every one of
//! them is `-32602`: the caller's parameters were wrong, not the store. On the
//! server path, a sign-in without the calendar scope is refused before an
//! operation starts, with the re-login prompt and `-32603`.

use std::sync::Arc;

use chrono::{DateTime, SecondsFormat, Utc};
use futures::future::BoxFuture;
use log::warn;
use serde_json::{json, Value};

use mp_protocol::calendar::{
    CalendarChanged, CalendarEvent, CalendarInfo, CalendarListResult, CalendarRange,
    CalendarViewParams, CalendarViewResult, EventCancelParams, EventCancelResult,
    EventCreateParams, EventDeleteParams, EventDeleteResult, EventGetResult, EventRespondParams,
    EventRespondResult, EventUpdateParams, EventUpdateResult, EventWriteResult, IMIP_CALENDAR_ID,
};
use mp_protocol::events::KIND_CALENDAR_CHANGED;
use mp_protocol::RpcError;

use crate::calendar_backend::{
    backend_for, calendar_features, choose_backend, has_backend, preflight_scope, BackendChoice,
    CalendarBackend, CalendarBackendError, Response, Written,
};
use crate::calendar_model::{
    window_for, Calendar, CalendarFeature, Event as CachedEvent, EventKind,
};
use crate::calendar_view::{
    imip_row_id, local_zone, parse_zone, Sourced, ViewFilter, IMIP_CALENDAR_NAME,
};
use crate::calendar_write::{resolve_input, resolve_patch, EventInput, EventPatch};
use crate::config::{AccountConfig, AuthMethod};
use crate::invite::Rsvp;
use crate::reconcile::reconcile_account;
use crate::store::calendar::Removal;
use crate::store::{calendar as cache, BlobStore, Store};

use super::super::config::ConfigStore;
use super::super::dispatch::{
    CancelToken, ClientCtx, Dispatcher, DomainError, Method, MethodKind, MethodSpec, Outcome,
    ResourceId,
};
use super::super::operations::{OperationHandle, OperationRegistry};
use super::super::runtime::account::off_thread;
use super::super::state::events::Event;
use super::super::state::{commit_point, CanonicalState, ConnectionId};
use super::{internal, invalid_params, only_params, server_error, string_param, INTERNAL_ERROR};

/// `ANO-4`, in the sentence `src/main.rs` has printed for an RSVP since #0036.
pub const GRAPH_RSVP_REFUSAL: &str =
    "RSVP is not supported for Graph accounts yet (#0036, blocked on #0035)";

/// The refusal of `send_response: false` on an invitation mail's event: the
/// iMIP path keeps no answer of its own, it reads the user's answer back from
/// the reply mail it sent, so an answer that sends nothing would record
/// nothing either.
pub const IMIP_SILENT_REFUSAL: &str =
    "send_response: false needs a calendar backend: an invitation mail is answered by the reply \
     mail itself, and without one nothing would record the answer";

/// The two *operations* of the iMIP family, in method-name order.
pub const CALENDAR_METHOD_SPECS: [MethodSpec; 2] = [
    MethodSpec::new("calendar.rebuild", MethodKind::Operation, 1),
    MethodSpec::new("calendar.rsvp", MethodKind::Operation, 1),
];

/// The server calendar's reads and its one preference (PERSO-200), in
/// method-name order, an array of their own for the reason
/// [`super::message::MESSAGE_QUEUE_METHOD_SPECS`] is one (P5-U6):
/// `tests/daemon_admin_slice.rs` holds
/// `const _: () = assert!(CALENDAR_METHOD_SPECS.len() == CALENDAR_METHODS.len());`
/// over a two-name list, and growing that array would edit a pinned test to say
/// something it was not written to say. The split is a Rust-side fact: the
/// wire, the capability list and the dispatcher see one family.
pub const CALENDAR_VIEW_METHOD_SPECS: [MethodSpec; 3] = [
    MethodSpec::new("calendar.list", MethodKind::Query, 1),
    MethodSpec::new("calendar.set_visibility", MethodKind::Command, 1),
    MethodSpec::new("calendar.view", MethodKind::Query, 1),
];

/// The `event.*` family (PERSO-200, PERSO-255), in method-name order.
pub const EVENT_METHOD_SPECS: [MethodSpec; 6] = [
    MethodSpec::new("event.cancel", MethodKind::Operation, 1),
    MethodSpec::new("event.create", MethodKind::Operation, 1),
    MethodSpec::new("event.delete", MethodKind::Operation, 1),
    MethodSpec::new("event.get", MethodKind::Query, 1),
    MethodSpec::new("event.respond", MethodKind::Operation, 1),
    MethodSpec::new("event.update", MethodKind::Operation, 1),
];

/// Register both families on `dispatcher`.
pub fn register(
    dispatcher: &mut Dispatcher,
    config: Arc<ConfigStore>,
    operations: Arc<OperationRegistry>,
    canonical: Arc<CanonicalState>,
) {
    let operation_specs = CALENDAR_METHOD_SPECS
        .into_iter()
        .chain(EVENT_METHOD_SPECS)
        .filter(|spec| spec.kind == MethodKind::Operation);
    for spec in operation_specs {
        dispatcher.register(Arc::new(CalendarMethod {
            spec,
            config: Arc::clone(&config),
            operations: Arc::clone(&operations),
        }));
    }
    let read_specs = CALENDAR_VIEW_METHOD_SPECS
        .into_iter()
        .chain(EVENT_METHOD_SPECS)
        .filter(|spec| spec.kind != MethodKind::Operation);
    for spec in read_specs {
        dispatcher.register(Arc::new(CalendarQueryMethod {
            spec,
            config: Arc::clone(&config),
            canonical: Arc::clone(&canonical),
        }));
    }
}

/// The `calendar.changed` an account's calendar change publishes.
pub fn calendar_changed(account: &str) -> Event {
    Event::Lifecycle {
        kind: KIND_CALENDAR_CHANGED,
        payload: serde_json::to_value(CalendarChanged {
            account: account.to_string(),
        })
        .unwrap_or_else(|_| json!({"account": account})),
    }
}

/// The reads (`calendar.list`, `calendar.view`, `event.get`) and the
/// visibility command, selected by their own [`MethodSpec`].
///
/// The reads are one indexed query per account plus one blob read per
/// invitation row, paid on the connection's task. What they are *not* is a rebuild: none writes, because
/// attendee statuses are derived where they are displayed and the server
/// cache is written by the syncs alone.
pub struct CalendarQueryMethod {
    spec: MethodSpec,
    config: Arc<ConfigStore>,
    canonical: Arc<CanonicalState>,
}

impl Method for CalendarQueryMethod {
    fn spec(&self) -> MethodSpec {
        self.spec
    }

    fn call<'a>(
        &'a self,
        _ctx: &'a ClientCtx,
        params: Value,
        _cancel: CancelToken,
    ) -> BoxFuture<'a, Result<Outcome, DomainError>> {
        Box::pin(async move {
            let accounts = self.config.accounts();
            let answer = match self.spec.name {
                "calendar.list" => list(&params, &accounts),
                "calendar.view" => view(&params, &accounts),
                "calendar.set_visibility" => {
                    return set_visibility(&params, &accounts, &self.canonical)
                        .map_err(DomainError::from)
                }
                _ => event_get(&params, &accounts),
            };
            answer.map(Outcome::query).map_err(DomainError::from)
        })
    }
}

/// The `result` of `calendar.list`: the server calendars of every account
/// with a backend, in the server's order, and the virtual invitation calendar
/// of every account holding an invitation mail, accounts in configuration
/// order. Without `account`, the accounts with no store yet are left out.
pub fn list(params: &Value, accounts: &[AccountConfig]) -> Result<Value, RpcError> {
    only_params("calendar.list", params, &["account"])?;
    let selected: Vec<&AccountConfig> = match params.get("account") {
        None | Some(Value::Null) => ready_accounts(accounts),
        Some(_) => vec![super::account::ready_account(
            accounts,
            &string_param(params, "account")?,
        )?],
    };
    let mut calendars = Vec::new();
    for account in selected {
        let name = account.name.as_str();
        let store = open_store(name)?;
        if has_backend(account) {
            let backend = account.calendar_backend().as_str();
            let listed = cache::list_calendars(&store, name).map_err(|e| server_error(name, &e))?;
            calendars.extend(listed.into_iter().map(|calendar| {
                let features = calendar_features(account, &calendar)
                    .into_iter()
                    .map(|feature| feature.as_str().to_string())
                    .collect();
                CalendarInfo {
                    account: calendar.account,
                    id: calendar.id,
                    name: calendar.name,
                    color: calendar.color,
                    is_default: calendar.is_default,
                    can_edit: calendar.can_edit,
                    visible: calendar.visible,
                    backend: backend.to_string(),
                    features,
                }
            }));
        }
        if crate::calendar_view::has_invitations(&store, name) {
            calendars.push(CalendarInfo {
                account: name.to_string(),
                id: IMIP_CALENDAR_ID.to_string(),
                name: IMIP_CALENDAR_NAME.to_string(),
                color: None,
                is_default: false,
                can_edit: false,
                visible: cache::calendar_visible(&store, name, IMIP_CALENDAR_ID)
                    .map_err(|e| server_error(name, &e))?,
                backend: crate::config::CalendarBackendKind::None
                    .as_str()
                    .to_string(),
                features: Vec::new(),
            });
        }
    }
    to_value(&CalendarListResult { calendars })
}

/// The `result` of `calendar.view`, merged by [`crate::calendar_view::view`].
pub fn view(params: &Value, accounts: &[AccountConfig]) -> Result<Value, RpcError> {
    only_params(
        "calendar.view",
        params,
        &[
            "accounts",
            "calendars",
            "end",
            "include_hidden",
            "start",
            "zone",
        ],
    )?;
    let request: CalendarViewParams = serde_json::from_value(params.clone())
        .map_err(|e| invalid_params(format!("calendar.view: {e}")))?;
    let start = instant_param("start", &request.start)?;
    let end = instant_param("end", &request.end)?;
    let range = crate::calendar_view::range(start, end).map_err(invalid_params)?;
    let zone = match request.zone.as_deref() {
        Some(name) => parse_zone(name)
            .ok_or_else(|| invalid_params(format!("zone {name:?} is not an IANA time zone")))?,
        None => local_zone(),
    };
    let selected: Vec<&AccountConfig> = match &request.accounts {
        Some(names) => names
            .iter()
            .map(|name| super::account::ready_account(accounts, name))
            .collect::<Result<_, _>>()?,
        None => ready_accounts(accounts),
    };
    let filter = ViewFilter {
        calendars: request.calendars.clone(),
        include_hidden: request.include_hidden.unwrap_or(false),
    };
    let events = crate::calendar_view::view(&selected, range, zone, &filter)
        .map_err(|e| internal(format!("{e:#}")))?;
    let window = window_for(Utc::now());
    to_value(&CalendarViewResult {
        range: CalendarRange {
            start: rfc3339(range.start),
            end: rfc3339(range.end),
        },
        zone: zone.name().to_string(),
        window: CalendarRange {
            start: rfc3339(window.start),
            end: rfc3339(window.end),
        },
        events: wire(&events)?,
    })
}

/// The `result` of `event.get`: a cached server event, or an invitation
/// mail's (`imip:<row_id>`), whose floating times are read in the daemon's
/// zone, with the series of an occurrence.
pub fn event_get(params: &Value, accounts: &[AccountConfig]) -> Result<Value, RpcError> {
    only_params("event.get", params, &["account", "event"])?;
    let name = string_param(params, "account")?;
    let id = string_param(params, "event")?;
    let account = super::account::ready_account(accounts, &name)?;
    let found = crate::calendar_view::get(account, &id, local_zone())
        .map_err(|e| server_error(&name, &e))?
        .ok_or_else(|| invalid_params(format!("{name} has no event {id}")))?;
    let series =
        crate::calendar_view::series(account, &found.event).map_err(|e| server_error(&name, &e))?;
    let event = found.to_wire().map_err(|e| internal(format!("{e:#}")))?;
    to_value(&EventGetResult {
        account: name,
        event,
        series,
    })
}

/// `calendar.set_visibility`: show or hide one calendar of an account, a
/// server calendar it lists or its invitation calendar, and announce it.
fn set_visibility(
    params: &Value,
    accounts: &[AccountConfig],
    canonical: &CanonicalState,
) -> Result<Outcome, RpcError> {
    only_params(
        "calendar.set_visibility",
        params,
        &["account", "calendar", "visible"],
    )?;
    let name = string_param(params, "account")?;
    let calendar = string_param(params, "calendar")?;
    let visible = params
        .get("visible")
        .and_then(Value::as_bool)
        .ok_or_else(|| invalid_params("visible is a required boolean parameter"))?;
    super::account::ready_account(accounts, &name)?;
    let store = open_store(&name)?;
    let known = calendar == IMIP_CALENDAR_ID
        || cache::list_calendars(&store, &name)
            .map_err(|e| server_error(&name, &e))?
            .iter()
            .any(|listed| listed.id == calendar);
    if !known {
        return Err(invalid_params(format!(
            "{name} has no calendar {calendar}; calendar.list names them"
        )));
    }
    cache::set_calendar_visible(&store, &name, &calendar, visible)
        .map_err(|e| server_error(&name, &e))?;
    let revision = canonical.publish(calendar_changed(&name));
    Ok(Outcome::command(
        json!({"account": name, "calendar": calendar, "visible": visible}),
        revision.get(),
        vec![ResourceId::new(format!("calendar:{name}/{calendar}"))],
    ))
}

/// One of the operations, selected by its own [`MethodSpec`].
pub struct CalendarMethod {
    spec: MethodSpec,
    config: Arc<ConfigStore>,
    operations: Arc<OperationRegistry>,
}

/// The work one operation of this family performs, decided before an operation
/// id exists so a refusal never arrives after one.
enum Work {
    /// The fold of the account's invitation mails, and the resync of its
    /// server cache when it has a backend. Boxed, as the two below, for the
    /// account configuration it carries.
    Rebuild {
        account: String,
        server: Option<Box<AccountConfig>>,
    },
    /// An iMIP reply.
    Rsvp(Box<RsvpWork>),
    /// An answer through the calendar backend.
    Respond(Box<RespondWork>),
    /// A new event through the calendar backend.
    Create(Box<CreateWork>),
    /// A change to, a cancellation or a deletion of a cached server event.
    Change(Box<ChangeWork>),
    /// A retried cancel or delete of an event the same call already took
    /// off the server and out of the cache, which settles as that call did.
    Removed {
        account: String,
        event: String,
        removal: Removal,
    },
}

/// What one change does to its event.
enum Change {
    Update(EventPatch),
    Cancel { comment: Option<String> },
    Delete,
}

/// Everything one update, cancel or delete needs, resolved before the
/// operation id exists: the event as the cache holds it.
struct ChangeWork {
    account: AccountConfig,
    event: CachedEvent,
    change: Change,
}

/// Everything one create needs, resolved before the operation id exists.
struct CreateWork {
    account: AccountConfig,
    calendar: Calendar,
    input: EventInput,
    transaction_id: String,
}

/// What a settled iMIP reply reports: `calendar.rsvp`'s own shape, or
/// `event.respond`'s.
enum ReplyShape {
    Rsvp,
    Event { event: String },
}

/// Everything one iMIP reply needs, read before the operation id exists.
struct RsvpWork {
    account: AccountConfig,
    selector: String,
    rsvp: Rsvp,
    response: String,
    comment: Option<String>,
    ics: Vec<u8>,
    /// The invitation mail's `Message-ID`, which the reply threads under.
    request_message_id: String,
    secrets: crate::secrets::SecretsBackendKind,
    shape: ReplyShape,
}

/// Everything one answer through the backend needs.
struct RespondWork {
    account: AccountConfig,
    event: String,
    response: Response,
    word: String,
    comment: Option<String>,
    send_response: bool,
}

impl Method for CalendarMethod {
    fn spec(&self) -> MethodSpec {
        self.spec
    }

    fn call<'a>(
        &'a self,
        ctx: &'a ClientCtx,
        params: Value,
        _cancel: CancelToken,
    ) -> BoxFuture<'a, Result<Outcome, DomainError>> {
        Box::pin(async move {
            let snapshot = self.config.snapshot();
            let work = plan(self.spec.name, &params, &snapshot)?;
            let transaction_id = match &work {
                Work::Create(create) => Some(create.transaction_id.clone()),
                _ => None,
            };
            let (id, handle) = self.operations.start(
                ConnectionId(ctx.connection_id),
                self.spec.cancel_scope,
                self.spec.name,
            );
            tokio::spawn(run(work, handle));
            let mut started = json!({"operation_id": id.as_str()});
            if let Some(transaction_id) = transaction_id {
                started["transaction_id"] = transaction_id.into();
            }
            Ok(Outcome::query(started))
        })
    }
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Validate one call and read everything the operation needs off disk.
fn plan(
    method: &str,
    params: &Value,
    snapshot: &super::super::config::Snapshot,
) -> Result<Work, RpcError> {
    match method {
        "calendar.rebuild" => {
            only_params(method, params, &["account"])?;
            let name = string_param(params, "account")?;
            let account = super::account::ready_account(&snapshot.accounts, &name)?;
            let server = has_backend(account).then(|| Box::new(account.clone()));
            Ok(Work::Rebuild {
                account: name,
                server,
            })
        }
        "event.respond" => plan_respond(params, snapshot),
        "event.create" => plan_create(params, snapshot),
        "event.update" => plan_update(params, snapshot),
        "event.cancel" => plan_cancel(params, snapshot),
        "event.delete" => plan_delete(params, snapshot),
        _ => plan_rsvp(method, params, snapshot),
    }
}

/// The account and store of the server event an update, a cancel or a
/// delete addresses, each refused before an operation starts: an invitation
/// mail's event, an account without a backend, and a sign-in without the
/// calendar scope.
fn change_store(
    method: &str,
    name: &str,
    id: &str,
    snapshot: &super::super::config::Snapshot,
) -> Result<(AccountConfig, Store), RpcError> {
    let account = super::account::configured_account(&snapshot.accounts, name)?.clone();
    if imip_row_id(id).is_some() {
        return Err(invalid_params(format!(
            "{id} is an invitation mail's event, which can only be answered (event.respond); \
             {method} changes a server event"
        )));
    }
    match choose_backend(&account) {
        Ok(BackendChoice::None) => {
            return Err(invalid_params(format!(
                "{name} has no calendar backend, so {id} names no server event to change"
            )))
        }
        Ok(_) => {}
        Err(CalendarBackendError::Other(e)) => return Err(invalid_params(format!("{e:#}"))),
        Err(e) => return Err(backend_error(name, e)),
    }
    super::account::ready_account(&snapshot.accounts, name)?;
    preflight_scope(&account).map_err(|e| backend_error(name, e))?;
    let store = open_store(name)?;
    Ok((account, store))
}

/// The cached server event `id` of `name` a change addresses, with its
/// calendar, each refused before an operation starts: an event the cache
/// does not hold, one of a recurring series, and one on a read-only
/// calendar.
fn changed_event(store: &Store, name: &str, id: &str) -> Result<(CachedEvent, Calendar), RpcError> {
    let event = cache::get_event(store, name, id)
        .map_err(|e| server_error(name, &e))?
        .ok_or_else(|| {
            invalid_params(format!(
                "{name} has no event {id} (cancelled or deleted already, or not synced yet)"
            ))
        })?;
    if event.kind != EventKind::Single || event.series_master_id.is_some() {
        return Err(invalid_params(format!(
            "{} is part of a recurring series, which mp cannot change yet; change it in Outlook",
            event.summary
        )));
    }
    let calendar = cache::list_calendars(store, name)
        .map_err(|e| server_error(name, &e))?
        .into_iter()
        .find(|calendar| calendar.id == event.calendar)
        .ok_or_else(|| {
            invalid_params(format!(
                "{name} has no calendar {} for {id}; calendar.list names them",
                event.calendar
            ))
        })?;
    if !calendar.can_edit {
        return Err(invalid_params(format!(
            "The calendar {} of {name} is read-only",
            calendar.name
        )));
    }
    Ok((event, calendar))
}

/// A cancel or a delete of an event the cache no longer holds because the
/// same call of mp already took it off the server: a retry of an operation
/// that vanished, which counts as done ([`Work::Removed`]). `None` when the
/// cache holds the event or mp never made that call, which
/// [`changed_event`] then reads or refuses, so an id the cache never held is
/// still refused rather than reported done.
fn retried_removal(
    method: &str,
    store: &Store,
    name: &str,
    id: &str,
) -> Result<Option<Work>, RpcError> {
    if cache::get_event(store, name, id)
        .map_err(|e| server_error(name, &e))?
        .is_some()
    {
        return Ok(None);
    }
    let removal =
        cache::removal(store, name, id, Utc::now()).map_err(|e| server_error(name, &e))?;
    Ok(removal
        .filter(|removal| removal.method == method)
        .map(|removal| Work::Removed {
            account: name.to_string(),
            event: id.to_string(),
            removal,
        }))
}

/// `event.update`: the change the client typed, read against the cached
/// event, and refused when the user may not make it.
fn plan_update(
    params: &Value,
    snapshot: &super::super::config::Snapshot,
) -> Result<Work, RpcError> {
    only_params("event.update", params, &["account", "event", "fields"])?;
    let request: EventUpdateParams = serde_json::from_value(params.clone())
        .map_err(|e| invalid_params(format!("event.update: {e}")))?;
    let name = request.account.as_str();
    let (account, store) = change_store("event.update", name, &request.event, snapshot)?;
    let (event, calendar) = changed_event(&store, name, &request.event)?;
    let mut patch = resolve_patch(&request.fields, &event, local_zone()).map_err(invalid_params)?;
    // Turning off a meeting that is not online changes nothing.
    if patch.online_meeting == Some(false) && !event.is_online_meeting {
        patch.online_meeting = None;
    }
    if patch.is_empty() {
        return Err(invalid_params(
            "Nothing to change: name at least one field of the event",
        ));
    }
    if !event.is_organizer && !attendee_may_change(&patch) {
        return Err(invalid_params(ORGANIZER_ONLY_REFUSAL));
    }
    if patch.online_meeting == Some(false) {
        return Err(invalid_params(format!(
            "{} stays a Teams meeting: the server keeps its link once it exists",
            event.summary
        )));
    }
    if patch.online_meeting == Some(true)
        && !event.is_online_meeting
        && !calendar_features(&account, &calendar).contains(&CalendarFeature::OnlineMeeting)
    {
        return Err(no_teams_refusal(&account, &calendar));
    }
    if patch.body.is_some() && event.is_online_meeting {
        return Err(invalid_params(format!(
            "The description of {} cannot be changed from mp yet: the server would drop its \
             Teams join details; change it in Outlook",
            event.summary
        )));
    }
    Ok(Work::Change(Box::new(ChangeWork {
        account,
        event,
        change: Change::Update(patch),
    })))
}

/// The refusal of an attendee's change to anything but their own copy's
/// reminder and show-as.
pub const ORGANIZER_ONLY_REFUSAL: &str = "Only the organizer can change this event";

/// Whether an attendee may make `patch` on their copy of a meeting: the
/// reminder and the show-as are the attendee's own, the rest the
/// organizer's.
fn attendee_may_change(patch: &EventPatch) -> bool {
    let own = EventPatch {
        reminder: patch.reminder,
        show_as: patch.show_as,
        ..Default::default()
    };
    &own == patch
}

/// The refusal of a Teams meeting on `calendar`, which offers none.
fn no_teams_refusal(account: &AccountConfig, calendar: &Calendar) -> RpcError {
    let name = &account.name;
    invalid_params(if calendar.offers_teams() {
        format!("The calendar backend of {name} offers no Teams meetings")
    } else {
        format!(
            "The calendar {} of {name} offers no Teams meetings (the mailbox has no Teams \
             licence)",
            calendar.name
        )
    })
}

/// `event.cancel`: a meeting the user organizes, which tells its attendees.
fn plan_cancel(
    params: &Value,
    snapshot: &super::super::config::Snapshot,
) -> Result<Work, RpcError> {
    only_params("event.cancel", params, &["account", "comment", "event"])?;
    let request: EventCancelParams = serde_json::from_value(params.clone())
        .map_err(|e| invalid_params(format!("event.cancel: {e}")))?;
    let (account, store) =
        change_store("event.cancel", &request.account, &request.event, snapshot)?;
    if let Some(retry) = retried_removal("event.cancel", &store, &account.name, &request.event)? {
        return Ok(retry);
    }
    let (event, _) = changed_event(&store, &account.name, &request.event)?;
    if !event.is_organizer {
        return Err(invalid_params(format!(
            "Only the organizer can cancel {}; decline it instead",
            event.summary
        )));
    }
    if event.attendees.is_empty() {
        return Err(invalid_params(format!(
            "{} invites nobody, so there is nobody to tell; delete it instead",
            event.summary
        )));
    }
    let comment = request.comment.filter(|comment| !comment.trim().is_empty());
    Ok(Work::Change(Box::new(ChangeWork {
        account,
        event,
        change: Change::Cancel { comment },
    })))
}

/// `event.delete`: an event that tells nobody, one without attendees or
/// the user's copy of a meeting its organizer cancelled.
fn plan_delete(
    params: &Value,
    snapshot: &super::super::config::Snapshot,
) -> Result<Work, RpcError> {
    only_params("event.delete", params, &["account", "event"])?;
    let request: EventDeleteParams = serde_json::from_value(params.clone())
        .map_err(|e| invalid_params(format!("event.delete: {e}")))?;
    let (account, store) =
        change_store("event.delete", &request.account, &request.event, snapshot)?;
    if let Some(retry) = retried_removal("event.delete", &store, &account.name, &request.event)? {
        return Ok(retry);
    }
    let (event, _) = changed_event(&store, &account.name, &request.event)?;
    if !event.cancelled {
        if event.is_organizer && !event.attendees.is_empty() {
            return Err(invalid_params(format!(
                "{} is a meeting with attendees; cancel it to tell them",
                event.summary
            )));
        }
        if !event.is_organizer && (event.organizer.is_some() || !event.attendees.is_empty()) {
            return Err(invalid_params(format!(
                "{} is a meeting you were invited to; decline it to tell the organizer",
                event.summary
            )));
        }
    }
    Ok(Work::Change(Box::new(ChangeWork {
        account,
        event,
        change: Change::Delete,
    })))
}

/// `event.create`: the account's backend and scope, the event the client
/// typed, and the calendar it goes to, each refused before an operation
/// starts.
fn plan_create(
    params: &Value,
    snapshot: &super::super::config::Snapshot,
) -> Result<Work, RpcError> {
    only_params(
        "event.create",
        params,
        &["account", "calendar", "event", "transaction_id"],
    )?;
    let request: EventCreateParams = serde_json::from_value(params.clone())
        .map_err(|e| invalid_params(format!("event.create: {e}")))?;
    let name = request.account.clone();
    let account = super::account::configured_account(&snapshot.accounts, &name)?.clone();
    match choose_backend(&account) {
        Ok(BackendChoice::None) => {
            return Err(invalid_params(format!(
                "{name} has no calendar backend, so it has no server calendar to create the \
                 event in; an invitation from it goes out by mail (mp send --invite)"
            )))
        }
        Ok(_) => {}
        Err(CalendarBackendError::Other(e)) => return Err(invalid_params(format!("{e:#}"))),
        Err(e) => return Err(backend_error(&name, e)),
    }
    super::account::ready_account(&snapshot.accounts, &name)?;
    preflight_scope(&account).map_err(|e| backend_error(&name, e))?;
    let input = resolve_input(&request.event, local_zone()).map_err(invalid_params)?;

    let store = open_store(&name)?;
    let calendars = cache::list_calendars(&store, &name).map_err(|e| server_error(&name, &e))?;
    let calendar = match request
        .calendar
        .as_deref()
        .filter(|id| !id.trim().is_empty())
    {
        Some(id) => calendars
            .into_iter()
            .find(|calendar| calendar.id == id)
            .ok_or_else(|| {
                invalid_params(format!(
                    "{name} has no calendar {id}; calendar.list names them"
                ))
            })?,
        None => calendars
            .into_iter()
            .find(|calendar| calendar.is_default)
            .ok_or_else(|| {
                invalid_params(format!(
                    "{name} has no default calendar yet: its calendars have not synced \
                     (mp calendar rebuild -A {name})"
                ))
            })?,
    };
    if !calendar.can_edit {
        return Err(invalid_params(format!(
            "The calendar {} of {name} is read-only",
            calendar.name
        )));
    }
    if input.online_meeting
        && !calendar_features(&account, &calendar).contains(&CalendarFeature::OnlineMeeting)
    {
        return Err(no_teams_refusal(&account, &calendar));
    }
    let transaction_id = match request.transaction_id.as_deref().map(str::trim) {
        Some(id) if !id.is_empty() => id.to_string(),
        _ => mint_transaction_id(),
    };
    Ok(Work::Create(Box::new(CreateWork {
        account,
        calendar,
        input,
        transaction_id,
    })))
}

/// A fresh transaction id, `mp-` and 32 hex digits of randomness.
fn mint_transaction_id() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("mp-{hex}")
}

/// `calendar.rsvp`: the iMIP reply to a message addressed by selector or row.
fn plan_rsvp(
    method: &str,
    params: &Value,
    snapshot: &super::super::config::Snapshot,
) -> Result<Work, RpcError> {
    only_params(
        method,
        params,
        &["account", "mailbox", "response", "row_id", "selector"],
    )?;
    let name = string_param(params, "account")?;
    let account = super::account::configured_account(&snapshot.accounts, &name)?.clone();

    // `ANO-4` before anything else: the refusal must not depend on a selector
    // resolving, because the surface that shows it has no message in hand.
    refuse_graph_rsvp(&account)?;

    let response = string_param(params, "response")?;
    let (rsvp, _) = parse_response(&response)?;

    let addressed = |key: &str| !matches!(params.get(key), None | Some(Value::Null));
    if addressed("row_id") && addressed("selector") {
        return Err(invalid_params(
            "row_id and selector are two addresses; send exactly one",
        ));
    }
    if !addressed("row_id") {
        // Read before the store is opened, so a selector naming another
        // account is refused in the same order it always was.
        bound_to(&string_param(params, "selector")?, &account)?;
    }
    super::account::ready_account(&snapshot.accounts, &name)?;

    let store = open_store(&name)?;
    // `row_id` is the `messages.id` a `message.list` row carries, the address
    // P5-U4 gave `message.get` and P5-U6 gives this method: the TUI's agenda
    // row holds a `MessageRef` and nothing else (#0050), and re-deriving a
    // selector for it would be a whole-message read to answer a keypress.
    let (row, canonical) = if addressed("row_id") {
        let row_id = params
            .get("row_id")
            .and_then(Value::as_i64)
            .ok_or_else(|| invalid_params("row_id is a messages.id, which is an integer"))?;
        row_by_id(&store, &name, row_id)?
    } else {
        let selector = string_param(params, "selector")?;
        let query = crate::selector::parse_in(
            &selector,
            crate::selector::Namespace::Received,
            &name,
            params.get("mailbox").and_then(Value::as_str),
        )
        .map_err(|e| invalid_params(format!("{e:#}")))?;
        crate::selector::resolve_received(&store, &query)
            .map_err(|e| invalid_params(format!("{e:#}")))?
    };
    rsvp_work(
        account,
        &store,
        &row,
        canonical,
        rsvp,
        response,
        None,
        snapshot.config.secrets_backend,
        ReplyShape::Rsvp,
    )
    .map(|work| Work::Rsvp(Box::new(work)))
}

/// `event.respond`: an answer through the backend for a server event, the
/// iMIP reply for an invitation mail's event.
fn plan_respond(
    params: &Value,
    snapshot: &super::super::config::Snapshot,
) -> Result<Work, RpcError> {
    only_params(
        "event.respond",
        params,
        &["account", "comment", "event", "response", "send_response"],
    )?;
    let request: EventRespondParams = serde_json::from_value(params.clone())
        .map_err(|e| invalid_params(format!("event.respond: {e}")))?;
    let name = request.account.clone();
    let account = super::account::configured_account(&snapshot.accounts, &name)?.clone();
    let (rsvp, response) = parse_response(&request.response)?;
    let send_response = request.send_response.unwrap_or(true);
    let comment = request
        .comment
        .clone()
        .filter(|comment| !comment.trim().is_empty());

    if let Some(row_id) = imip_row_id(&request.event) {
        refuse_graph_rsvp(&account)?;
        if !send_response {
            return Err(RpcError {
                code: super::INVALID_PARAMS,
                message: IMIP_SILENT_REFUSAL.to_string(),
                data: Some(json!({"account": name, "event": request.event})),
            });
        }
        super::account::ready_account(&snapshot.accounts, &name)?;
        let store = open_store(&name)?;
        let (row, canonical) = row_by_id(&store, &name, row_id)?;
        return rsvp_work(
            account,
            &store,
            &row,
            canonical,
            rsvp,
            request.response,
            comment,
            snapshot.config.secrets_backend,
            ReplyShape::Event {
                event: request.event,
            },
        )
        .map(|work| Work::Rsvp(Box::new(work)));
    }

    match choose_backend(&account) {
        Ok(BackendChoice::None) => {
            return Err(invalid_params(format!(
                "{name} has no calendar backend, so {} names no server event; an invitation \
                 mail's event is answered as imip:<row_id>",
                request.event
            )))
        }
        Ok(_) => {}
        Err(CalendarBackendError::Other(e)) => return Err(invalid_params(format!("{e:#}"))),
        Err(e) => return Err(backend_error(&name, e)),
    }
    super::account::ready_account(&snapshot.accounts, &name)?;
    // A sign-in without the calendar scope is refused here, before an
    // operation exists, and without a call to the server.
    preflight_scope(&account).map_err(|e| backend_error(&name, e))?;
    Ok(Work::Respond(Box::new(RespondWork {
        account,
        event: request.event,
        response,
        word: request.response,
        comment,
        send_response,
    })))
}

/// `ANO-4`: no iMIP reply from a Graph account, which sends no SMTP.
fn refuse_graph_rsvp(account: &AccountConfig) -> Result<(), RpcError> {
    if account.auth_method == AuthMethod::Graph {
        return Err(RpcError {
            code: super::INVALID_PARAMS,
            message: GRAPH_RSVP_REFUSAL.to_string(),
            data: Some(json!({"account": account.name})),
        });
    }
    Ok(())
}

/// The answer a response word names, in both vocabularies.
fn parse_response(word: &str) -> Result<(Rsvp, Response), RpcError> {
    match word {
        "accept" => Ok((Rsvp::Accepted, Response::Accept)),
        "tentative" => Ok((Rsvp::Tentative, Response::Tentative)),
        "decline" => Ok((Rsvp::Declined, Response::Decline)),
        other => Err(invalid_params(format!(
            "response is one of \"accept\", \"tentative\", \"decline\", not {other:?}"
        ))),
    }
}

/// The row `row_id` of `account`'s store and its canonical selector.
fn row_by_id(
    store: &Store,
    name: &str,
    row_id: i64,
) -> Result<(crate::store::read::MessageRow, crate::selector::Selector), RpcError> {
    let row = crate::store::read::find_by_id(store, row_id)
        .map_err(|e| server_error(name, &e))?
        .ok_or_else(|| invalid_params(format!("{name} holds no message with row id {row_id}")))?;
    let canonical = crate::selector::Selector::for_message(name, &row);
    Ok((row, canonical))
}

/// One iMIP reply to the invitation on `row`, whose own payload is the source
/// of truth for the reply (#0038 item 6).
#[allow(clippy::too_many_arguments)]
fn rsvp_work(
    account: AccountConfig,
    store: &Store,
    row: &crate::store::read::MessageRow,
    canonical: crate::selector::Selector,
    rsvp: Rsvp,
    response: String,
    comment: Option<String>,
    secrets: crate::secrets::SecretsBackendKind,
    shape: ReplyShape,
) -> Result<RsvpWork, RpcError> {
    let blobs = BlobStore::for_account(&account.name);
    let ics = crate::store::read::load_invite_ics(store, &blobs, row.id)
        .ok_or_else(|| invalid_params(format!("{canonical} carries no invitation to reply to")))?;
    Ok(RsvpWork {
        account,
        selector: canonical.to_string(),
        rsvp,
        response,
        comment,
        ics,
        request_message_id: row.message_id.clone(),
        secrets,
        shape,
    })
}

/// A selector naming another account is refused rather than answered from the
/// wrong address: the reply goes out over *this* account's transport.
fn bound_to(selector: &str, account: &AccountConfig) -> Result<(), RpcError> {
    let parts = crate::selector::parse(selector).map_err(|e| invalid_params(format!("{e:#}")))?;
    match parts.account {
        Some(named) if named != account.name => Err(invalid_params(format!(
            "selector names account '{named}', but this reply is bound to '{}'",
            account.name
        ))),
        _ => Ok(()),
    }
}

/// A backend failure as the error a client shows: the re-login prompt for a
/// missing scope, the failure itself otherwise.
pub(super) fn backend_error(account: &str, error: CalendarBackendError) -> RpcError {
    match error {
        CalendarBackendError::MissingScope { scope } => RpcError {
            code: INTERNAL_ERROR,
            message: crate::oauth2::calendar_relogin_prompt(account),
            data: Some(json!({"account": account, "missing_scopes": [scope]})),
        },
        CalendarBackendError::Other(e) => server_error(account, &e),
    }
}

// ---------------------------------------------------------------------------
// The operations
// ---------------------------------------------------------------------------

/// Run one operation to its terminal state.
async fn run(work: Work, handle: OperationHandle) {
    handle.set_running();
    match work {
        Work::Rebuild { account, server } => rebuild(account, server, handle).await,
        Work::Rsvp(work) => reply(*work, handle).await,
        Work::Respond(work) => respond(*work, handle).await,
        Work::Create(work) => create(*work, handle).await,
        Work::Change(work) => change(*work, handle).await,
        Work::Removed {
            account,
            event,
            removal,
        } => {
            handle.announce(calendar_changed(&account));
            settle_removal(&handle, account, event, removal);
        }
    }
}

/// What a change left on the server, for the cache and the settle.
enum Changed {
    /// The event as the server holds it after an update.
    Updated(Box<CachedEvent>),
    /// The event is gone: cancelled or deleted.
    Removed(Removal),
}

/// Settle a cancel or a delete with its own result shape.
fn settle_removal(handle: &OperationHandle, account: String, event: String, removal: Removal) {
    if removal.method == "event.cancel" {
        settle(
            handle,
            EventCancelResult {
                account,
                event,
                notified: removal.notified,
            },
        );
    } else {
        settle(handle, EventDeleteResult { account, event });
    }
}

/// An update, a cancel or a delete: the server's answer, written into the
/// cache under the account's calendar lock (the updated event upserted, the
/// cancelled or deleted one removed), then the sync that confirms it.
async fn change(work: ChangeWork, handle: OperationHandle) {
    let ChangeWork {
        account,
        event,
        change,
    } = work;
    let name = account.name.clone();
    let token = handle.token.clone();
    let method = match &change {
        Change::Update(_) => "event.update",
        Change::Cancel { .. } => "event.cancel",
        Change::Delete => "event.delete",
    };
    let (id, calendar) = (event.id.clone(), event.calendar.clone());
    let notified: Vec<String> = event.attendees.into_iter().map(|a| a.email).collect();
    let changed = off_thread("the event change", move || async move {
        let backend = backend_for(&account)
            .await?
            .ok_or_else(|| anyhow::anyhow!("{} has no calendar backend", account.name))?;
        if !commit_point(method, &token).await {
            return Ok(None);
        }
        let changed = match change {
            Change::Update(patch) => match backend.update_event(&calendar, &id, &patch).await? {
                Written::Event(event) => Changed::Updated(event),
                Written::Series(series) => {
                    return Err(anyhow::anyhow!(
                        "the server answered the change with the series {}",
                        series.master_id
                    )
                    .into())
                }
            },
            Change::Cancel { comment } => {
                backend.cancel_event(&id, comment).await?;
                Changed::Removed(Removal {
                    method: method.to_string(),
                    notified,
                })
            }
            Change::Delete => {
                backend.delete_event(&id).await?;
                Changed::Removed(Removal {
                    method: method.to_string(),
                    notified: Vec::new(),
                })
            }
        };
        let store = Store::open(crate::config::store_path(&account.name))?;
        let lock = crate::calendar_sync::account_lock(&account.name);
        let _held = lock.lock().await;
        match &changed {
            Changed::Updated(event) => cache::upsert_event(&store, &account.name, event)?,
            Changed::Removed(removal) => {
                cache::remove_event(&store, &account.name, &id, removal, Utc::now())?;
            }
        }
        if let Err(e) =
            crate::calendar_sync::sync_account(&store, &backend, &account.name, Utc::now()).await
        {
            warn!(
                "[calendar] {}: the sync after {method} failed: {e}",
                account.name
            );
        }
        Ok::<_, CalendarBackendError>(Some(changed))
    })
    .await;
    match changed {
        // Cancelled at the commit point: nothing was sent, and the registry
        // has already settled the operation.
        Ok(Ok(None)) => {}
        Ok(Ok(Some(changed))) => {
            handle.announce(calendar_changed(&name));
            match changed {
                Changed::Updated(updated) => match (Sourced {
                    event: *updated,
                    source: None,
                })
                .to_wire()
                {
                    Ok(event) => settle(
                        &handle,
                        EventUpdateResult {
                            account: name,
                            event,
                        },
                    ),
                    Err(e) => handle.fail(DomainError::internal(format!("{e:#}"))),
                },
                Changed::Removed(removal) => settle_removal(&handle, name, event.id, removal),
            }
        }
        Ok(Err(e)) => handle.fail(DomainError::from(backend_error(&name, e))),
        Err(e) => handle.fail(DomainError::internal(format!("{e:#}"))),
    }
}

/// `calendar.rebuild`: the fold of the stored replies onto the stored
/// invitations, which every account's invitation mails feed into its view,
/// and on an account with a backend the resync of its server cache, reported
/// under `server`.
///
/// The fold settles the operation: a resync that failed is the `server`
/// member's `error`, with the re-login prompt for a sign-in without the
/// calendar scope, so `mp calendar rebuild` over every account still reports
/// each account's fold.
async fn rebuild(account: String, server: Option<Box<AccountConfig>>, handle: OperationHandle) {
    let name = account.clone();
    let folded = match tokio::task::spawn_blocking(move || fold(&account)).await {
        Ok(Ok(result)) => result,
        Ok(Err(e)) => return handle.fail(DomainError::internal(format!("{e:#}"))),
        Err(e) => {
            return handle.fail(DomainError::internal(format!(
                "the calendar fold of {name} {e}"
            )))
        }
    };
    let mut result = folded;
    if let Some(account) = server {
        let resynced = resync(*account).await;
        if resynced.get("error").is_none() {
            handle.announce(calendar_changed(&name));
        }
        result["server"] = resynced;
    }
    handle.succeed(result);
}

/// The fold: what the stored replies resolve on the stored invitations. It
/// writes nothing; attendee statuses are derived where they are displayed
/// (#0038 scope item 6).
fn fold(account: &str) -> anyhow::Result<Value> {
    let store = Store::open(crate::config::store_path(account))?;
    let blobs = BlobStore::for_account(account);
    let report = reconcile_account(&store, &blobs, account);
    Ok(json!({
        "account": account,
        "resolved": report.resolved,
        "invites_seen": report.invites_seen,
        "replies_seen": report.replies_seen,
        "cancelled": report.cancelled,
    }))
}

/// The resync: drop the account's server cache, keeping which calendars are
/// hidden, and sync it afresh. `{calendars, upserts, removed, baselines,
/// failed}`, or `{error}` with the `{code, message, data}` of the failure.
async fn resync(account: AccountConfig) -> Value {
    let name = account.name.clone();
    let rebuilt = off_thread("the calendar rebuild", move || async move {
        let backend = backend_for(&account)
            .await?
            .ok_or_else(|| anyhow::anyhow!("{} has no calendar backend", account.name))?;
        let store = Store::open(crate::config::store_path(&account.name))?;
        let lock = crate::calendar_sync::account_lock(&account.name);
        let _held = lock.lock().await;
        crate::calendar_sync::rebuild_account(&store, &backend, &account.name, Utc::now()).await
    })
    .await;
    let error = match rebuilt {
        Ok(Ok(report)) => {
            let failed: Vec<Value> = report
                .failed
                .iter()
                .map(|(calendar, error)| json!({"calendar": calendar, "error": error}))
                .collect();
            return json!({
                "calendars": report.calendars,
                "upserts": report.upserts,
                "removed": report.removed,
                "baselines": report.baselines,
                "failed": failed,
            });
        }
        Ok(Err(e)) => backend_error(&name, e),
        Err(e) => internal(format!("{e:#}")),
    };
    warn!(
        "[calendar] {name}: the calendar rebuild failed: {}",
        error.message
    );
    let mut object = json!({"code": error.code, "message": error.message});
    if let Some(data) = error.data {
        object["data"] = data;
    }
    json!({ "error": object })
}

/// An answer through the backend, then the immediate sync that brings the
/// answer back into the cache.
async fn respond(work: RespondWork, handle: OperationHandle) {
    let RespondWork {
        account,
        event,
        response,
        word,
        comment,
        send_response,
    } = work;
    let name = account.name.clone();
    let token = handle.token.clone();
    let (answer_event, answer_comment) = (event.clone(), comment.clone());
    let answered = off_thread("the event response", move || async move {
        let backend = backend_for(&account)
            .await?
            .ok_or_else(|| anyhow::anyhow!("{} has no calendar backend", account.name))?;
        if !commit_point("event.respond", &token).await {
            return Ok(false);
        }
        backend
            .respond(&answer_event, response, answer_comment, send_response)
            .await?;
        let synced = async {
            let store = Store::open(crate::config::store_path(&account.name))?;
            let lock = crate::calendar_sync::account_lock(&account.name);
            let _held = lock.lock().await;
            crate::calendar_sync::sync_account(&store, &backend, &account.name, Utc::now())
                .await
                .map(|_| ())
        };
        if let Err(e) = synced.await {
            warn!(
                "[calendar] {}: the sync after an answer failed: {e}",
                account.name
            );
        }
        Ok::<bool, CalendarBackendError>(true)
    })
    .await;
    match answered {
        // Cancelled at the commit point: nothing was sent, and the registry
        // has already settled the operation.
        Ok(Ok(false)) => {}
        Ok(Ok(true)) => {
            handle.announce(calendar_changed(&name));
            settle(
                &handle,
                EventRespondResult {
                    account: name,
                    event,
                    response: word,
                    comment,
                    send_response,
                    via: "server".into(),
                    delivered: send_response,
                },
            );
        }
        Ok(Err(e)) => handle.fail(DomainError::from(backend_error(&name, e))),
        Err(e) => handle.fail(DomainError::internal(format!("{e:#}"))),
    }
}

/// A new event: the one the cache already holds under the transaction id
/// when it matches the input, that one patched with the input's fields when
/// the retry changed them, else the server's answer to the create; a written
/// event goes into the cache before the sync that confirms it.
async fn create(work: CreateWork, handle: OperationHandle) {
    let CreateWork {
        account,
        calendar,
        input,
        transaction_id,
    } = work;
    let name = account.name.clone();
    let token = handle.token.clone();
    let tx = transaction_id.clone();
    let created = off_thread("the new event", move || async move {
        let store = Store::open(crate::config::store_path(&account.name))?;
        let made = cache::event_by_transaction(&store, &account.name, &tx)?;
        if let Some(event) = made.as_ref().filter(|event| input.matches(event)) {
            return Ok::<_, CalendarBackendError>(Some(event.clone()));
        }
        let backend = backend_for(&account)
            .await?
            .ok_or_else(|| anyhow::anyhow!("{} has no calendar backend", account.name))?;
        if !commit_point("event.create", &token).await {
            return Ok(None);
        }
        // A retry that changed the event a first try made patches it, as the
        // backend adopts it on Graph's ErrorDuplicateTransactionId.
        let written = match &made {
            Some(event) => {
                backend
                    .update_event(&event.calendar, &event.id, &input.as_patch())
                    .await?
            }
            None => backend.create_event(&calendar, &input, &tx).await?,
        };
        let event = match written {
            Written::Event(event) => *event,
            Written::Series(series) => {
                return Err(anyhow::anyhow!(
                    "the server answered the new event with the series {}",
                    series.master_id
                )
                .into())
            }
        };
        let lock = crate::calendar_sync::account_lock(&account.name);
        let _held = lock.lock().await;
        cache::upsert_event(&store, &account.name, &event)?;
        if let Err(e) =
            crate::calendar_sync::sync_account(&store, &backend, &account.name, Utc::now()).await
        {
            warn!(
                "[calendar] {}: the sync after a new event failed: {e}",
                account.name
            );
        }
        Ok(Some(event))
    })
    .await;
    match created {
        // Cancelled at the commit point: nothing was sent, and the registry
        // has already settled the operation.
        Ok(Ok(None)) => {}
        Ok(Ok(Some(event))) => {
            handle.announce(calendar_changed(&name));
            let invited = event.attendees.iter().map(|a| a.email.clone()).collect();
            let wired = Sourced {
                event,
                source: None,
            }
            .to_wire();
            match wired {
                Ok(event) => settle(
                    &handle,
                    EventWriteResult {
                        account: name,
                        event,
                        transaction_id,
                        invited,
                    },
                ),
                Err(e) => handle.fail(DomainError::internal(format!("{e:#}"))),
            }
        }
        Ok(Err(e)) => handle.fail(DomainError::from(backend_error(&name, e))),
        Err(e) => handle.fail(DomainError::internal(format!("{e:#}"))),
    }
}

/// An iMIP reply, built from the invitation's own payload and submitted
/// durably.
async fn reply(work: RsvpWork, handle: OperationHandle) {
    let RsvpWork {
        account,
        selector,
        rsvp,
        response,
        comment,
        ics,
        request_message_id,
        secrets,
        shape,
    } = work;
    if let Err(e) = super::open_secrets(&account.name, secrets) {
        return handle.fail(DomainError::from(e));
    }
    let name = account.name.clone();
    let token = handle.token.clone();
    let method = match shape {
        ReplyShape::Rsvp => "calendar.rsvp",
        ReplyShape::Event { .. } => "event.respond",
    };
    let body_comment = comment.clone();
    let sent = off_thread("the RSVP", move || async move {
        submit(
            &account,
            &ics,
            &request_message_id,
            rsvp,
            body_comment.as_deref(),
            method,
            &token,
        )
        .await
    })
    .await
    .and_then(|inner| inner);
    match sent {
        // Cancelled at the commit point: nothing went out, and the registry
        // has already settled the operation.
        Ok(None) => {}
        Ok(Some(outcome)) => match shape {
            ReplyShape::Rsvp => handle.succeed(json!({
                "account": name,
                "selector": selector,
                "response": response,
                "subject": outcome.subject,
                "organizer": outcome.organizer,
                "message_id": outcome.message_id.unwrap_or_default(),
                "delivered": outcome.send_result.any_succeeded(),
            })),
            ReplyShape::Event { event } => {
                handle.announce(calendar_changed(&name));
                settle(
                    &handle,
                    EventRespondResult {
                        account: name,
                        event,
                        response,
                        comment,
                        send_response: true,
                        via: "imip".into(),
                        delivered: outcome.send_result.any_succeeded(),
                    },
                );
            }
        },
        Err(e) => handle.fail(DomainError::internal(format!("{e:#}"))),
    }
}

/// Settle `handle` with `result`, serialised.
fn settle<T: serde::Serialize>(handle: &OperationHandle, result: T) {
    match serde_json::to_value(result) {
        Ok(value) => handle.succeed(value),
        Err(e) => handle.fail(DomainError::internal(format!(
            "serialising an operation's result: {e}"
        ))),
    }
}

/// One reply, built from the invitation's own payload and submitted durably,
/// or `None` when the token was shut before the submission.
async fn submit(
    account: &AccountConfig,
    ics: &[u8],
    request_message_id: &str,
    rsvp: Rsvp,
    comment: Option<&str>,
    method: &str,
    token: &CancelToken,
) -> anyhow::Result<Option<crate::send::RsvpOutcome>> {
    let smtp = super::send::smtp_config(account)?;
    let prepared = crate::send::prepare_rsvp(
        ics,
        &account.default_from,
        rsvp,
        comment,
        request_message_id,
    )?;
    if !commit_point(method, token).await {
        return Ok(None);
    }
    prepared.submit(account, &smtp).await.map(Some)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn open_store(name: &str) -> Result<Store, RpcError> {
    Store::open(crate::config::store_path(name)).map_err(|e| server_error(name, &e))
}

/// The configured accounts with a store to read, in configuration order.
fn ready_accounts(accounts: &[AccountConfig]) -> Vec<&AccountConfig> {
    accounts
        .iter()
        .filter(|account| super::account::ready_account(accounts, &account.name).is_ok())
        .collect()
}

fn instant_param(name: &str, raw: &str) -> Result<DateTime<Utc>, RpcError> {
    DateTime::parse_from_rfc3339(raw)
        .map(|instant| instant.with_timezone(&Utc))
        .map_err(|e| invalid_params(format!("{name} is an RFC 3339 time with an offset: {e}")))
}

fn rfc3339(instant: DateTime<Utc>) -> String {
    instant.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn wire(events: &[Sourced]) -> Result<Vec<CalendarEvent>, RpcError> {
    events
        .iter()
        .map(|sourced| sourced.to_wire().map_err(|e| internal(format!("{e:#}"))))
        .collect()
}

fn to_value<T: serde::Serialize>(value: &T) -> Result<Value, RpcError> {
    serde_json::to_value(value).map_err(|e| internal(format!("serialising an answer: {e}")))
}

#[cfg(test)]
mod tests {
    use super::super::super::dispatch::CancelScope;
    use super::*;

    /// The array is the family, in method-name order, and both of it are
    /// durable: a reply abandoned mid-submission is the state the outbox parks.
    #[test]
    fn the_family_declares_two_durable_operations_in_name_order() {
        let names: Vec<&str> = CALENDAR_METHOD_SPECS.iter().map(|spec| spec.name).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted);
        for spec in CALENDAR_METHOD_SPECS {
            assert_eq!(spec.kind, MethodKind::Operation, "{}", spec.name);
            assert_eq!(spec.cancel_scope, CancelScope::Durable, "{}", spec.name);
            assert_eq!(spec.since, 1, "{}", spec.name);
        }
    }

    /// An attendee may change their own copy's reminder and show-as, and
    /// nothing else.
    #[test]
    fn an_attendee_changes_only_the_reminder_and_show_as() {
        let own = EventPatch {
            reminder: Some(None),
            show_as: Some(crate::calendar_model::ShowAs::Free),
            ..Default::default()
        };
        assert!(attendee_may_change(&own));
        let renamed = EventPatch {
            summary: Some("Mine now".into()),
            ..own
        };
        assert!(!attendee_may_change(&renamed));
    }

    /// The server calendar's arrays are in method-name order, and its
    /// operations are durable: an answer or a write the server took cannot
    /// be recalled.
    #[test]
    fn the_server_calendar_arrays_are_in_name_order() {
        for names in [
            CALENDAR_VIEW_METHOD_SPECS
                .iter()
                .map(|spec| spec.name)
                .collect::<Vec<_>>(),
            EVENT_METHOD_SPECS.iter().map(|spec| spec.name).collect(),
        ] {
            let mut sorted = names.clone();
            sorted.sort_unstable();
            assert_eq!(names, sorted);
        }
        for spec in EVENT_METHOD_SPECS {
            if spec.name == "event.get" {
                assert_eq!(spec.kind, MethodKind::Query);
                continue;
            }
            assert_eq!(spec.kind, MethodKind::Operation, "{}", spec.name);
            assert_eq!(spec.cancel_scope, CancelScope::Durable, "{}", spec.name);
        }
    }

    #[test]
    fn a_response_word_names_one_answer() {
        assert!(matches!(
            parse_response("accept"),
            Ok((Rsvp::Accepted, Response::Accept))
        ));
        assert!(matches!(
            parse_response("decline"),
            Ok((Rsvp::Declined, Response::Decline))
        ));
        assert!(parse_response("maybe").is_err());
    }

    #[test]
    fn a_missing_scope_is_the_relogin_prompt() {
        let error = backend_error(
            "evoqs",
            CalendarBackendError::MissingScope {
                scope: "Calendars.ReadWrite".into(),
            },
        );
        assert_eq!(error.code, INTERNAL_ERROR);
        assert_eq!(
            error.message,
            "calendar needs a re-login (mp config oauth2-login -A evoqs)"
        );
        assert_eq!(
            error.data,
            Some(json!({"account": "evoqs", "missing_scopes": ["Calendars.ReadWrite"]}))
        );
    }
}
