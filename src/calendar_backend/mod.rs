//! The seam between the calendar engine and a server calendar (PERSO-200,
//! `docs/plans/calendar.md`, D7).
//!
//! Each backend (Graph in phase 1, EWS in phase 3, CalDAV in phase 4)
//! implements [`CalendarBackend`] over the neutral model of
//! [`crate::calendar_model`], and the engine merges the accounts, dedups and
//! serves the clients without knowing which backend an event came from.
//! [`fake::FakeCalendarBackend`] stands in for a server in tests.
//!
//! [`choose_backend`] and [`backend_for`] decide which backend serves an
//! account: the fake when [`fake::FAKE_CALENDAR_ENV`] arms it, Graph for
//! `calendar_backend = "graph"`, nothing for `none`, and a refusal for the
//! backends a later phase builds. [`AccountBackend`] is the closed set they
//! return, dispatched statically.

pub mod fake;
pub mod graph;

use std::fmt;
use std::future::Future;

use anyhow::anyhow;
use serde::{Deserialize, Serialize};

use crate::calendar_model::{Calendar, CalendarWindow, Event, EventId};
use crate::config::{AccountConfig, AuthMethod, CalendarBackendKind};

use fake::FakeCalendarBackend;
use graph::GraphCalendarBackend;

/// An answer to an invitation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Response {
    Accept,
    Tentative,
    Decline,
}

/// What a backend learned about a recurring series while syncing its
/// occurrences, cached in the store so the next sync need not ask again.
///
/// Graph gives every occurrence an `iCalUId` of its own, so the `UID` the
/// series shares (and the invitation mails about it carry) is only on the
/// series master, which one extra request per series reads.
///
/// Graph's delta also trims an occurrence to its ids and times, so the master
/// itself is kept too: the details such an occurrence takes from it
/// ([`Event::inherit_from`]), and the zone it places the occurrence's times
/// in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SeriesInfo {
    /// The server id of the series master.
    pub master_id: EventId,
    /// The iCalendar `UID` of the series.
    pub uid: String,
    /// The series rule as `RRULE` text, when the backend could express it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recurrence: Option<String>,
    /// The series master as an event, when the backend read one: its
    /// summary, all-day flag, people and free/busy are what a trimmed
    /// occurrence shows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub master: Option<Box<Event>>,
    /// The IANA zone the series is held in (Graph's `originalStartTimeZone`),
    /// when known; an all-day master's [`EventTime`] carries none.
    ///
    /// [`EventTime`]: crate::calendar_model::EventTime
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zone: Option<String>,
}

/// What one sync of a calendar's window brought.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SyncResult {
    /// Events new or changed since the token, or the whole window when
    /// `full_resync` is set.
    pub upserts: Vec<Event>,
    /// Server ids of events gone from the window since the token.
    pub removed: Vec<EventId>,
    /// The token the next sync of the same window passes; `None` when the
    /// backend has no incremental sync and every call is a full one.
    pub token: Option<String>,
    /// The result is a fresh baseline of the whole window: no token was given,
    /// or the server refused the one given (Graph's HTTP 410). The engine then
    /// replaces the calendar's cached window instead of merging into it.
    pub full_resync: bool,
    /// How the backend reached the window, recorded beside the token so the
    /// path a calendar syncs through is observable (Graph: `calendar-delta`,
    /// `default-delta` or `calendar-view`).
    pub path: Option<String>,
    /// Series the backend resolved during this sync, for the store's series
    /// cache ([`CalendarBackend::remember_series`] hands them back).
    pub series: Vec<SeriesInfo>,
    /// The upserts that carry their series master's details rather than
    /// their own (a Graph delta occurrence, trimmed to its ids and times):
    /// the store re-derives them from each master a later sync brings, since
    /// the server need not send the occurrences again when only the master
    /// changed.
    pub inheriting: Vec<EventId>,
}

/// Why a backend call failed.
#[derive(Debug)]
pub enum CalendarBackendError {
    /// The stored credential lacks a scope the call needs (a Graph token
    /// issued before `Calendars.ReadWrite` joined `GRAPH_SCOPES`); the clients
    /// prompt for a new sign-in instead of showing a failing call.
    MissingScope { scope: String },
    /// Anything else: transport, server refusal, unparseable answer.
    Other(anyhow::Error),
}

impl fmt::Display for CalendarBackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CalendarBackendError::MissingScope { scope } => {
                write!(f, "the account's sign-in lacks the {scope} permission")
            }
            CalendarBackendError::Other(err) => write!(f, "{err:#}"),
        }
    }
}

impl std::error::Error for CalendarBackendError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CalendarBackendError::MissingScope { .. } => None,
            CalendarBackendError::Other(err) => Some(err.as_ref()),
        }
    }
}

impl From<anyhow::Error> for CalendarBackendError {
    fn from(err: anyhow::Error) -> Self {
        CalendarBackendError::Other(err)
    }
}

pub type BackendResult<T> = Result<T, CalendarBackendError>;

/// Whether `account`'s calendar is the Graph one, which signs in through the
/// account's OAuth2 token: `auth_method = "graph"` and a `calendar_backend`
/// that is (or defaults to) `graph`.
pub fn uses_graph_calendar(account: &AccountConfig) -> bool {
    account.auth_method == AuthMethod::Graph
        && account.calendar_backend() == CalendarBackendKind::Graph
}

/// The scopes `account`'s calendar needs and its stored sign-in was not
/// granted, by their short name (`Calendars.ReadWrite`). Empty for an account
/// whose calendar is not Graph's, and for one with no readable token cache,
/// which needs a sign-in for its mail first and says so through its token
/// state.
pub fn missing_scopes(account: &AccountConfig) -> Vec<String> {
    if !uses_graph_calendar(account) {
        return Vec::new();
    }
    missing_scopes_of(crate::oauth2::load_token_cache(&account.name).as_ref())
}

/// [`missing_scopes`] of a Graph calendar account over its token cache.
fn missing_scopes_of(cache: Option<&crate::oauth2::TokenCache>) -> Vec<String> {
    match cache {
        Some(cache) if !cache.has_scope(crate::oauth2::CALENDAR_SCOPE) => {
            vec![crate::oauth2::scope_name(crate::oauth2::CALENDAR_SCOPE).to_string()]
        }
        _ => Vec::new(),
    }
}

/// The check a Graph calendar call runs before reaching the server: a token
/// without the calendar scope is [`CalendarBackendError::MissingScope`], which
/// the clients turn into a re-login prompt rather than a failing call.
pub fn calendar_scope_check(account: &AccountConfig) -> BackendResult<()> {
    match missing_scopes(account).into_iter().next() {
        Some(scope) => Err(CalendarBackendError::MissingScope { scope }),
        None => Ok(()),
    }
}

/// One account's server calendar.
///
/// Async in the shape of the Graph send seam (`GraphSendApi`): each
/// method returns a `Send` future, so the engine can run it on the daemon's
/// runtime, and a test drives a fake instead of a tenant.
pub trait CalendarBackend {
    /// Every calendar of the account, each with `visible` set to `true`: the
    /// visibility is a local preference the engine overlays.
    fn list_calendars(&self) -> impl Future<Output = BackendResult<Vec<Calendar>>> + Send;

    /// The events of `calendar` in `window`, expanded into occurrences.
    ///
    /// `token` is the one the previous sync of the same window returned, or
    /// `None` for a baseline. A token is bound to the window it was minted for,
    /// so a moved window starts with `None`.
    fn sync_window(
        &self,
        calendar: &Calendar,
        window: CalendarWindow,
        token: Option<String>,
    ) -> impl Future<Output = BackendResult<SyncResult>> + Send;

    /// Answer the invitation `event` is, with an optional comment to the
    /// organizer; `send_response = false` records the answer without telling
    /// the organizer.
    fn respond(
        &self,
        event: &str,
        response: Response,
        comment: Option<String>,
        send_response: bool,
    ) -> impl Future<Output = BackendResult<()>> + Send;

    /// The series the store already knows, handed over before a sync so a
    /// backend that resolves series (Graph) does not ask for them again. A
    /// backend whose occurrences carry the series `UID` ignores it.
    fn remember_series(&self, _known: Vec<SeriesInfo>) {}

    /// The server event an invitation mail stands for, by the mail's server
    /// id (Graph: the `event` of an `eventMessage`); `None` when the mail is
    /// no invitation or the backend cannot tell.
    fn event_for_message(
        &self,
        _message_id: &str,
    ) -> impl Future<Output = BackendResult<Option<EventId>>> + Send {
        async { Ok(None) }
    }
}

/// Which backend serves an account's calendar, decided without a network
/// call.
#[derive(Debug, Clone)]
pub enum BackendChoice {
    /// `calendar_backend = "none"`: the iMIP path of the invitation mails.
    None,
    /// The fake [`fake::FAKE_CALENDAR_ENV`] armed, already serving this
    /// account's name.
    Fake(FakeCalendarBackend),
    /// Microsoft Graph, signed in through the account's token.
    Graph,
}

/// The backend `account`'s calendar uses.
///
/// The fake, when the daemon runs under [`fake::FAKE_CALENDAR_ENV`], stands
/// in for every account whose `calendar_backend` is not `none`, whatever that
/// backend is. A backend a later phase builds (`ews`, `caldav`) is refused
/// with a sentence naming it.
pub fn choose_backend(account: &AccountConfig) -> BackendResult<BackendChoice> {
    let kind = account.calendar_backend();
    if kind == CalendarBackendKind::None {
        return Ok(BackendChoice::None);
    }
    if let Some(fake) = FakeCalendarBackend::from_env() {
        return Ok(BackendChoice::Fake(fake.serving(&account.name)));
    }
    match kind {
        CalendarBackendKind::Graph => Ok(BackendChoice::Graph),
        CalendarBackendKind::None => Ok(BackendChoice::None),
        other => Err(unsupported(other)),
    }
}

/// The refusal of a backend this build does not implement yet.
pub fn unsupported(kind: CalendarBackendKind) -> CalendarBackendError {
    CalendarBackendError::Other(anyhow!(
        "calendar backend {} is not supported yet",
        kind.as_str()
    ))
}

/// Whether `account`'s calendar is served by a backend this build runs: the
/// views read its events from the store cache rather than only from its
/// invitation mails.
pub fn has_backend(account: &AccountConfig) -> bool {
    matches!(
        choose_backend(account),
        Ok(BackendChoice::Fake(_) | BackendChoice::Graph)
    )
}

/// The scope check a write runs before an operation starts: the fake's
/// `missing_scope`, or the Graph token's scopes ([`calendar_scope_check`]).
/// No network call either way.
pub fn preflight_scope(account: &AccountConfig) -> BackendResult<()> {
    match choose_backend(account)? {
        BackendChoice::Fake(fake) => fake.check_scope(),
        BackendChoice::Graph => calendar_scope_check(account),
        BackendChoice::None => Ok(()),
    }
}

/// The backend serving `account`, connected: `None` for an account without
/// one. A Graph token without the calendar scope is
/// [`CalendarBackendError::MissingScope`] before any network call.
pub async fn backend_for(account: &AccountConfig) -> BackendResult<Option<AccountBackend>> {
    Ok(match choose_backend(account)? {
        BackendChoice::None => None,
        BackendChoice::Fake(fake) => Some(AccountBackend::Fake(fake)),
        BackendChoice::Graph => Some(AccountBackend::Graph(Box::new(
            GraphCalendarBackend::connect(account).await?,
        ))),
    })
}

/// One connected backend of the closed set this build implements.
pub enum AccountBackend {
    Fake(FakeCalendarBackend),
    /// Boxed: the Graph client is a few hundred bytes the fake does not pay.
    Graph(Box<GraphCalendarBackend>),
}

impl CalendarBackend for AccountBackend {
    async fn list_calendars(&self) -> BackendResult<Vec<Calendar>> {
        match self {
            AccountBackend::Fake(b) => b.list_calendars().await,
            AccountBackend::Graph(b) => b.list_calendars().await,
        }
    }

    async fn sync_window(
        &self,
        calendar: &Calendar,
        window: CalendarWindow,
        token: Option<String>,
    ) -> BackendResult<SyncResult> {
        match self {
            AccountBackend::Fake(b) => b.sync_window(calendar, window, token).await,
            AccountBackend::Graph(b) => b.sync_window(calendar, window, token).await,
        }
    }

    async fn respond(
        &self,
        event: &str,
        response: Response,
        comment: Option<String>,
        send_response: bool,
    ) -> BackendResult<()> {
        match self {
            AccountBackend::Fake(b) => b.respond(event, response, comment, send_response).await,
            AccountBackend::Graph(b) => b.respond(event, response, comment, send_response).await,
        }
    }

    fn remember_series(&self, known: Vec<SeriesInfo>) {
        match self {
            AccountBackend::Fake(b) => b.remember_series(known),
            AccountBackend::Graph(b) => b.remember_series(known),
        }
    }

    async fn event_for_message(&self, message_id: &str) -> BackendResult<Option<EventId>> {
        match self {
            AccountBackend::Fake(b) => b.event_for_message(message_id).await,
            AccountBackend::Graph(b) => b.event_for_message(message_id).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oauth2::TokenCache;

    fn account(auth: AuthMethod, backend: Option<CalendarBackendKind>) -> AccountConfig {
        AccountConfig {
            name: "work".to_string(),
            auth_method: auth,
            calendar_backend: backend,
            ..Default::default()
        }
    }

    fn cache(scopes: &[&str]) -> TokenCache {
        TokenCache {
            access_token: String::new(),
            refresh_token: String::new(),
            expires_at: 0,
            scopes: scopes.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// A token from before PERSO-200 records no scope and lacks the calendar;
    /// one granted it, with or without the prefix, lacks nothing; no cache is
    /// a missing sign-in rather than a missing scope.
    #[test]
    fn a_token_without_the_calendar_scope_misses_it() {
        assert_eq!(
            missing_scopes_of(Some(&cache(&[]))),
            ["Calendars.ReadWrite"]
        );
        assert_eq!(
            missing_scopes_of(Some(&cache(&["Mail.Read", "offline_access"]))),
            ["Calendars.ReadWrite"]
        );
        assert!(missing_scopes_of(Some(&cache(&["Calendars.ReadWrite"]))).is_empty());
        assert!(missing_scopes_of(Some(&cache(&[crate::oauth2::CALENDAR_SCOPE]))).is_empty());
        assert!(missing_scopes_of(None).is_empty());
    }

    /// Only a Graph account whose calendar is Graph's is asked about the
    /// scope; `calendar_backend = "none"` opts a Graph account out.
    #[test]
    fn only_a_graph_calendar_account_needs_the_scope() {
        assert!(uses_graph_calendar(&account(AuthMethod::Graph, None)));
        assert!(!uses_graph_calendar(&account(
            AuthMethod::Graph,
            Some(CalendarBackendKind::None)
        )));
        assert!(!uses_graph_calendar(&account(AuthMethod::Password, None)));
        assert!(!uses_graph_calendar(&account(
            AuthMethod::Password,
            Some(CalendarBackendKind::Graph)
        )));
        let opted_out = account(AuthMethod::Graph, Some(CalendarBackendKind::None));
        assert!(missing_scopes(&opted_out).is_empty());
        assert!(calendar_scope_check(&opted_out).is_ok());
    }

    /// `none` has no backend, `graph` is Graph's, and the backends a later
    /// phase builds are refused by name. The test process never arms the
    /// fake, so the choice is the configured one.
    #[test]
    fn the_choice_follows_the_setting_and_refuses_the_later_backends() {
        assert!(std::env::var(fake::FAKE_CALENDAR_ENV).is_err());
        let choice = |auth, kind| choose_backend(&account(auth, kind));
        assert!(matches!(
            choice(AuthMethod::Password, None),
            Ok(BackendChoice::None)
        ));
        assert!(matches!(
            choice(AuthMethod::Graph, None),
            Ok(BackendChoice::Graph)
        ));
        assert!(matches!(
            choice(AuthMethod::Graph, Some(CalendarBackendKind::None)),
            Ok(BackendChoice::None)
        ));
        for (kind, name) in [
            (CalendarBackendKind::Ews, "ews"),
            (CalendarBackendKind::Caldav, "caldav"),
        ] {
            let err = choice(AuthMethod::Password, Some(kind)).unwrap_err();
            assert_eq!(
                err.to_string(),
                format!("calendar backend {name} is not supported yet")
            );
            assert!(!has_backend(&account(AuthMethod::Password, Some(kind))));
        }
        assert!(has_backend(&account(AuthMethod::Graph, None)));
        assert!(!has_backend(&account(AuthMethod::Password, None)));
    }
}
