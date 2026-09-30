//! The `mpmsg` custom URI scheme: `mpmsg://localhost/<account>/<row_id>`
//! answers the `message.html` rendition as its own document.
//!
//! The plan's reader rule (docs/plans/native-gui.md, "Reading HTML bodies"):
//! the document carries [`MESSAGE_CSP`] as its `Content-Security-Policy`
//! response header, and the app CSP stays strict. The header is this
//! constant and never a value read out of the message: a sender can hide a
//! meta-looking string inside the doctype, ahead of the daemon's own tag, and
//! a header (unlike a meta) may carry `report-uri`, which would turn every
//! blocked remote image into a violation report to the sender. The daemon's
//! meta tag still sits in the document, and a browser enforces both policies,
//! so a sender's leftover meta can only tighten. A rendition over 8 MiB is refused inline with
//! `-32004`, and the handler falls back to `message.materialise_html`, reads
//! the file and releases the handle at once. A message without markup is
//! served as a plain-text document under the same policy (header
//! `X-Mp-Rendition: text`), so the reader has one path; a row that does not
//! exist is 404 and a malformed URL 400.

use std::time::Duration;

use serde_json::{json, Value};
use tauri::http::{header, Response, StatusCode};

use crate::error::{rpc_code, Addressing, GuiError};
use crate::session::Door;

/// The policy `message.html` prepends as a meta tag, and the one every reader
/// response carries as its header, whatever the rendition says.
pub const MESSAGE_CSP: &str = "default-src 'none'; img-src data:; style-src 'unsafe-inline'; font-src data:; form-action 'none'; base-uri 'none'";

const HTML_BUDGET: Duration = Duration::from_secs(15);
const TEXT_BUDGET: Duration = Duration::from_secs(10);

/// Build the reader URL of one message.
pub fn message_url(account: &str, row_id: i64) -> String {
    format!("mpmsg://localhost/{}/{row_id}", percent_encode(account))
}

/// A reader request that could not be parsed.
#[derive(Debug, PartialEq, Eq)]
pub struct Malformed(pub String);

/// `/<account>/<row_id>` from a scheme request path.
pub fn parse_path(path: &str) -> Result<(String, i64), Malformed> {
    let trimmed = path.strip_prefix('/').unwrap_or(path);
    let trimmed = trimmed.strip_suffix('/').unwrap_or(trimmed);
    let mut parts = trimmed.split('/');
    let (Some(account), Some(row), None) = (parts.next(), parts.next(), parts.next()) else {
        return Err(Malformed(format!(
            "expected /<account>/<row_id>, got {path}"
        )));
    };
    let account =
        percent_decode(account).ok_or_else(|| Malformed("bad percent-encoding".into()))?;
    if account.is_empty() {
        return Err(Malformed("empty account".into()));
    }
    let row_id: i64 = row
        .parse()
        .map_err(|_| Malformed(format!("row id `{row}` is not an integer")))?;
    if row_id <= 0 {
        return Err(Malformed(format!("row id {row_id} is not positive")));
    }
    Ok((account, row_id))
}

fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = s.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

// ---------------------------------------------------------------------------
// Serving
// ---------------------------------------------------------------------------

/// What one reader URL resolves to.
#[derive(Debug, PartialEq, Eq)]
pub enum Rendered {
    Html(String),
    /// No markup: the stored plain text.
    Text(Option<String>),
}

/// Fetch the rendition, with the two fallbacks.
pub fn render(door: &Door, account: &str, row_id: i64) -> Result<Rendered, GuiError> {
    let params = json!({"account": account, "row_id": row_id});
    match door.call_within("message.html", params.clone(), HTML_BUDGET) {
        Ok(answer) => Ok(Rendered::Html(
            answer["html"].as_str().unwrap_or_default().to_string(),
        )),
        Err(e) => match rpc_code(&format!("{e:#}")) {
            Some(-32004) => materialised(door, &params).map(Rendered::Html),
            Some(-32602) => {
                let mut get = params;
                get["body"] = json!(true);
                let record = door
                    .call_within("message.get", get, TEXT_BUDGET)
                    .map_err(|e| GuiError::from_call(&e, Addressing::Resource))?;
                Ok(Rendered::Text(record["body"].as_str().map(str::to_string)))
            }
            _ => Err(GuiError::from_call(&e, Addressing::Resource)),
        },
    }
}

/// The over-8-MiB route: write the file, read it, release the handle.
fn materialised(door: &Door, params: &Value) -> Result<String, GuiError> {
    let m = door
        .call_within("message.materialise_html", params.clone(), HTML_BUDGET)
        .map_err(|e| GuiError::from_call(&e, Addressing::Resource))?;
    let path = m["path"].as_str().unwrap_or_default().to_string();
    let read = std::fs::read_to_string(&path);
    if let Err(e) = door.call_within(
        "message.release_handle",
        json!({"handle": m["handle"]}),
        TEXT_BUDGET,
    ) {
        tracing::warn!("[reader] releasing the rendition handle failed: {e:#}");
    }
    read.map_err(|e| GuiError::internal(format!("reading the materialised rendition {path}: {e}")))
}

fn text_document(body: Option<&str>) -> String {
    let text = body.map_or_else(
        || "(This message has no stored body.)".to_string(),
        escape_html,
    );
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\">\
         <meta http-equiv=\"Content-Security-Policy\" content=\"{MESSAGE_CSP}\">\
         <style>body{{margin:16px;font:14px/1.5 -apple-system,system-ui,sans-serif}}\
         pre{{white-space:pre-wrap;word-wrap:break-word;font:inherit;margin:0}}</style>\
         </head><body><pre>{text}</pre></body></html>"
    )
}

fn document(html: String, rendition: &str) -> Response<Vec<u8>> {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(header::CONTENT_SECURITY_POLICY, MESSAGE_CSP)
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(header::REFERRER_POLICY, "no-referrer")
        .header(header::CACHE_CONTROL, "no-store")
        .header("X-Mp-Rendition", rendition)
        .body(html.into_bytes())
        .unwrap_or_else(|_| Response::new(Vec::new()))
}

fn failure(status: StatusCode, message: &str) -> Response<Vec<u8>> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .header(header::CONTENT_SECURITY_POLICY, MESSAGE_CSP)
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(header::CACHE_CONTROL, "no-store")
        .body(message.as_bytes().to_vec())
        .unwrap_or_else(|_| {
            let mut r = Response::new(Vec::new());
            *r.status_mut() = status;
            r
        })
}

/// Answer one scheme request. `door` is the session's, or the error the
/// session is in.
pub fn respond(method: &str, path: &str, door: Result<Door, GuiError>) -> Response<Vec<u8>> {
    if method != "GET" {
        return failure(StatusCode::METHOD_NOT_ALLOWED, "only GET is served");
    }
    let (account, row_id) = match parse_path(path) {
        Ok(parsed) => parsed,
        Err(Malformed(why)) => return failure(StatusCode::BAD_REQUEST, &why),
    };
    let rendered = door.and_then(|door| render(&door, &account, row_id));
    answer(path, rendered)
}

/// The response for one fetched rendition, or for the failure to fetch it.
/// The header is always [`MESSAGE_CSP`], never a policy the message carries.
fn answer(path: &str, rendered: Result<Rendered, GuiError>) -> Response<Vec<u8>> {
    match rendered {
        Ok(Rendered::Html(html)) => document(html, "html"),
        Ok(Rendered::Text(body)) => document(text_document(body.as_deref()), "text"),
        Err(e) => {
            let status = match &e {
                GuiError::NotFound { .. } => StatusCode::NOT_FOUND,
                GuiError::DaemonUnavailable { .. } | GuiError::VersionMismatch { .. } => {
                    StatusCode::SERVICE_UNAVAILABLE
                }
                GuiError::Timeout { .. } => StatusCode::GATEWAY_TIMEOUT,
                GuiError::Protocol { .. } => StatusCode::BAD_GATEWAY,
                GuiError::Internal { .. } => StatusCode::INTERNAL_SERVER_ERROR,
            };
            tracing::warn!("[reader] {path}: {e}");
            failure(status, e.message())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::Fixture;
    use std::sync::Arc;

    fn fixture_door() -> Door {
        let (tx, _rx) = std::sync::mpsc::channel();
        Door::Fixture(Arc::new(Fixture::load(tx).expect("fixture")))
    }

    #[test]
    fn the_path_parses_into_an_account_and_a_row() {
        assert_eq!(parse_path("/work/42"), Ok(("work".to_string(), 42)));
        assert_eq!(parse_path("/work/42/"), Ok(("work".to_string(), 42)));
        assert_eq!(parse_path("/my%20mail/7"), Ok(("my mail".to_string(), 7)));
        for bad in [
            "/",
            "/work",
            "/work/abc",
            "/work/1/2",
            "//1",
            "/work/-3",
            "/w%zz/1",
            "/work/0",
        ] {
            assert!(parse_path(bad).is_err(), "{bad} is malformed");
        }
    }

    #[test]
    fn the_url_round_trips_through_the_parser() {
        let url = message_url("my mail/ü", 9);
        let parsed = tauri::Url::parse(&url).expect("a url");
        assert_eq!(parsed.scheme(), "mpmsg");
        assert_eq!(parse_path(parsed.path()), Ok(("my mail/ü".to_string(), 9)));
    }

    /// The daemon's tag, as `inject_csp_meta` writes it.
    fn daemon_meta() -> String {
        format!("<meta http-equiv=\"Content-Security-Policy\" content=\"{MESSAGE_CSP}\">")
    }

    #[test]
    fn a_policy_hidden_in_the_doctype_never_reaches_the_header() {
        // What survives the daemon: its stripping regex wants http-equiv
        // before content, and it inserts its own tag after the doctype's
        // first `>`, which here closes the sender's fake meta.
        let html = format!(
            "<!doctype html <meta content=\"default-src 'none'; report-uri https://t.example/r\" \
             http-equiv=Content-Security-Policy>{}<html><body><img src=\"https://t.example/p.gif\"></body></html>",
            daemon_meta()
        );
        let response = answer("/work/1", Ok(Rendered::Html(html)));
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[header::CONTENT_SECURITY_POLICY],
            MESSAGE_CSP
        );
    }

    #[test]
    fn a_lax_or_missing_policy_does_not_change_the_header() {
        for html in [
            "<p>none</p>".to_string(),
            "<meta http-equiv=\"Content-Security-Policy\" content=\"default-src *\">".to_string(),
            format!(
                "<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; report-uri https://t.example/r\">{}",
                daemon_meta()
            ),
        ] {
            let response = answer("/work/1", Ok(Rendered::Html(html)));
            assert_eq!(
                response.headers()[header::CONTENT_SECURITY_POLICY],
                MESSAGE_CSP
            );
        }
    }

    #[test]
    fn the_hostile_fixture_is_served_under_the_daemon_policy() {
        let response = respond("GET", "/work/1006", Ok(fixture_door()));
        assert_eq!(response.status(), StatusCode::OK);
        let h = response.headers();
        assert_eq!(h[header::CONTENT_SECURITY_POLICY], MESSAGE_CSP);
        assert_eq!(h[header::CONTENT_TYPE], "text/html; charset=utf-8");
        assert_eq!(h[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        assert_eq!(h["X-Mp-Rendition"], "html");
        let body = String::from_utf8(response.into_body()).expect("utf-8");
        assert!(
            body.starts_with("<!doctype html <meta content="),
            "the fixture carries the doctype trick"
        );
        assert!(body.contains("report-uri"));
        assert!(body.contains(&daemon_meta()), "the daemon's tag is present");
    }

    #[test]
    fn statuses_follow_the_failure() {
        assert_eq!(
            respond("GET", "/work/99999", Ok(fixture_door())).status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            respond("GET", "/nobody/1001", Ok(fixture_door())).status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            respond("GET", "/work/x", Ok(fixture_door())).status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            respond("POST", "/work/1001", Ok(fixture_door())).status(),
            StatusCode::METHOD_NOT_ALLOWED
        );
        assert_eq!(
            respond("GET", "/work/1001", Err(GuiError::unavailable("down"))).status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[test]
    fn a_message_without_markup_is_served_as_escaped_text() {
        let response = respond("GET", "/work/1002", Ok(fixture_door()));
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["X-Mp-Rendition"], "text");
        let body = String::from_utf8(response.into_body()).expect("utf-8");
        assert!(body.contains("Dachsanierung"));
        assert!(body.contains(MESSAGE_CSP));
    }
}
