//! Tests of the EWS calendar backend over the sanitised answers of the spike
//! (`tests/fixtures/ews/*.xml`, `.agents/research/2026-11-13-ews-spike.md`,
//! "Fixtures"): names, addresses, subjects, ids and sync states are
//! placeholders, element names, attributes and value formats are the
//! server's.

use std::collections::VecDeque;
use std::sync::Mutex;

use super::fetch::{envelope, EwsFetch, EwsFetchError, EwsSession, REQUEST_SERVER_VERSION};

/// The answer readers on the fixtures.
mod parse;

/// The read path through the scripted seam.
mod sync;

const CALENDAR_VIEW: &str = include_str!("../../../tests/fixtures/ews/finditem_calendarview.xml");
const GET_FOLDER: &str = include_str!("../../../tests/fixtures/ews/getfolder_calendar.xml");

/// An Exchange server that answers from a script, in order: each entry names
/// the operation it answers (`GetItem`), so a request the test did not
/// expect, or one past the end of the script, panics with what was sent.
/// Every envelope is recorded.
#[derive(Default)]
pub(super) struct ScriptedFetch {
    script: Mutex<VecDeque<(String, Result<String, EwsFetchError>)>>,
    sent: Mutex<Vec<String>>,
}

impl ScriptedFetch {
    /// Answer the next `operation` with `body`.
    pub(super) fn answer(self, operation: &str, body: &str) -> Self {
        self.then(operation, Ok(body.to_string()))
    }

    /// Answer the next `operation` with `error`.
    pub(super) fn fail(self, operation: &str, error: EwsFetchError) -> Self {
        self.then(operation, Err(error))
    }

    fn then(self, operation: &str, answer: Result<String, EwsFetchError>) -> Self {
        self.script
            .lock()
            .unwrap()
            .push_back((operation.to_string(), answer));
        self
    }

    /// The envelopes sent so far, in order.
    pub(super) fn sent(&self) -> Vec<String> {
        self.sent.lock().unwrap().clone()
    }

    /// Whether every scripted answer was asked for.
    pub(super) fn exhausted(&self) -> bool {
        self.script.lock().unwrap().is_empty()
    }
}

impl EwsFetch for ScriptedFetch {
    async fn post(&self, envelope: &str) -> Result<String, EwsFetchError> {
        self.sent.lock().unwrap().push(envelope.to_string());
        let (operation, answer) = self
            .script
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| panic!("no scripted answer for {envelope}"));
        let opens = |next: char| envelope.contains(&format!("<m:{operation}{next}"));
        assert!(
            opens('>') || opens(' ') || opens('/'),
            "the script expected {operation}, the backend sent {envelope}"
        );
        answer
    }
}

/// A call sends its operation inside the envelope that names the schema
/// version the server accepts, and answers the body as the server sent it.
#[tokio::test]
async fn a_call_posts_its_operation_in_the_versioned_envelope() {
    let session = EwsSession::new(ScriptedFetch::default().answer("GetFolder", GET_FOLDER));
    let body = "<m:GetFolder><m:FolderShape><t:BaseShape>Default</t:BaseShape></m:FolderShape>\
                <m:FolderIds><t:DistinguishedFolderId Id=\"calendar\"/></m:FolderIds></m:GetFolder>";
    assert_eq!(session.call(body).await.unwrap(), GET_FOLDER);
    let sent = session.fetch().sent();
    assert_eq!(sent, [envelope(body)]);
    assert!(sent[0].starts_with("<?xml version=\"1.0\" encoding=\"utf-8\"?><s:Envelope "));
    assert!(sent[0].contains(&format!(
        "<s:Header><t:RequestServerVersion Version=\"{REQUEST_SERVER_VERSION}\"/></s:Header>"
    )));
    assert_eq!(REQUEST_SERVER_VERSION, "Exchange2013_SP1");
    assert!(sent[0].contains(&format!("<s:Body>{body}</s:Body>")));
}

/// A refused login is held for the rest of the session: the next call
/// answers the same refusal without reaching the server, while any other
/// failure lets the next call through.
#[tokio::test]
async fn a_refused_login_is_never_asked_again_in_the_session() {
    let session = EwsSession::new(
        ScriptedFetch::default()
            .fail(
                "GetFolder",
                EwsFetchError::Status {
                    status: 503,
                    body: String::new(),
                },
            )
            .fail(
                "GetFolder",
                EwsFetchError::LoginRefused("HTTP 401".to_string()),
            ),
    );
    let body = "<m:GetFolder/>";
    let first = session.call(body).await.unwrap_err();
    assert_eq!(first.to_string(), "the Exchange server answered HTTP 503");
    let refused = session.call(body).await.unwrap_err();
    assert_eq!(
        refused.to_string(),
        "the Exchange server refused the EWS login (HTTP 401)"
    );
    let again = session.call(body).await.unwrap_err();
    assert!(matches!(again, EwsFetchError::LoginRefused(ref why) if why == "HTTP 401"));
    assert_eq!(
        session.fetch().sent().len(),
        2,
        "the third call asked nobody"
    );
    assert!(session.fetch().exhausted());
}

/// Every `Id` attribute of a `StartTimeZone` or `EndTimeZone` element in `xml`.
fn zone_ids(xml: &str) -> Vec<&str> {
    let mut ids = Vec::new();
    for tag in ["<t:StartTimeZone ", "<t:EndTimeZone "] {
        for (at, _) in xml.match_indices(tag) {
            let element = &xml[at..at + xml[at..].find('>').expect("the tag closes")];
            let id = element
                .split(" Id=\"")
                .nth(1)
                .and_then(|rest| rest.split('"').next())
                .expect("the zone carries an Id");
            ids.push(id);
        }
    }
    ids
}

/// The CalendarView fixture names its zones by the Windows ids the server
/// sent (`W. Europe Standard Time`), which the Windows-to-IANA table maps,
/// and its periods by the server's registry rules; the sanitiser once
/// scrambled both.
#[test]
fn the_calendar_view_fixture_names_windows_zones() {
    let ids = zone_ids(CALENDAR_VIEW);
    assert_eq!(
        ids.len(),
        6,
        "a start and an end zone on each of three items"
    );
    for id in ids {
        assert_eq!(
            crate::windows_zones::windows_zone_to_iana(id),
            Some("Europe/Berlin"),
            "{id:?}"
        );
    }
    for rule in ["1-Standard", "1-Daylight"] {
        let id = format!("Id=\"trule:Microsoft/Registry/W. Europe Standard Time/{rule}\"");
        assert_eq!(CALENDAR_VIEW.matches(&id).count(), 6, "{rule}");
    }
}
