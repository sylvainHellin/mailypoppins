//! The seam between the calendar engine and a server calendar (PERSO-200,
//! `docs/plans/calendar.md`, D7).
//!
//! Each backend (Graph in phase 1, EWS in phase 3, CalDAV in phase 4)
//! implements [`CalendarBackend`] over the neutral model of
//! [`crate::calendar_model`], and the engine merges the accounts, dedups and
//! serves the clients without knowing which backend an event came from.
//! [`fake::FakeCalendarBackend`] stands in for a server in tests.

pub mod fake;

use std::fmt;
use std::future::Future;

use serde::{Deserialize, Serialize};

use crate::calendar_model::{Calendar, CalendarWindow, Event, EventId};
use crate::config::{AccountConfig, AuthMethod, CalendarBackendKind};

/// An answer to an invitation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Response {
    Accept,
    Tentative,
    Decline,
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
}
