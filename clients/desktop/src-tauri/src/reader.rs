//! The `mpmsg` custom URI scheme: `mpmsg://localhost/<account>/<row_id>`
//! answers the `message.html` rendition as its own document.
//!
//! The plan's reader rule (docs/plans/native-gui.md, "Reading HTML bodies"):
//! the document carries a `Content-Security-Policy` response header copied
//! from the rendition's own meta tag, so the message's policy governs it and
//! the app CSP stays strict. A rendition over 8 MiB is refused inline with
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

/// The policy `message.html` prepends as a meta tag, and what the handler
/// falls back to when the rendition carries none it can trust.
pub const MESSAGE_CSP: &str = "default-src 'none'; img-src data:; style-src 'unsafe-inline'; font-src data:; form-action 'none'; base-uri 'none'";

/// How far into a rendition the CSP meta tag is looked for: the daemon
/// prepends it, so it is at the very start.
const CSP_SCAN_BYTES: usize = 16 * 1024;

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

// ---------------------------------------------------------------------------
// CSP extraction
// ---------------------------------------------------------------------------

/// The `content` of the first `<meta http-equiv="Content-Security-Policy">`
/// in the head of `html`.
///
/// Only the first counts: the daemon prepends its own, and a later one the
/// sender wrote cannot loosen it (a document enforces every policy it is
/// given, so the browser still applies the sender's too, which only
/// tightens).
pub fn extract_csp(html: &str) -> Option<String> {
    let mut end = html.len().min(CSP_SCAN_BYTES);
    while !html.is_char_boundary(end) {
        end -= 1;
    }
    let head = &html[..end];
    let lower = head.to_ascii_lowercase();
    let mut from = 0;
    while let Some(at) = lower[from..].find("<meta") {
        let start = from + at;
        let close = lower[start..].find('>').map_or(lower.len(), |c| start + c);
        let attrs = parse_attributes(&head[start + 5..close]);
        let is_csp = attrs.iter().any(|(k, v)| {
            k.eq_ignore_ascii_case("http-equiv")
                && v.trim().eq_ignore_ascii_case("content-security-policy")
        });
        if is_csp {
            return attrs
                .into_iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("content"))
                .map(|(_, v)| unescape(&v));
        }
        from = close;
    }
    None
}

/// The policy the response carries: the rendition's own when it is at least
/// as strict as `default-src 'none'`, else [`MESSAGE_CSP`].
pub fn effective_csp(html: &str) -> String {
    match extract_csp(html) {
        Some(csp) if csp.contains("default-src 'none'") => csp,
        Some(csp) => {
            tracing::warn!(
                "[reader] the rendition's CSP is not default-src 'none', using ours: {csp}"
            );
            MESSAGE_CSP.to_string()
        }
        None => MESSAGE_CSP.to_string(),
    }
}

/// `name=value` pairs of one tag's attribute text, values unquoted.
fn parse_attributes(text: &str) -> Vec<(String, String)> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        while i < chars.len() && (chars[i].is_whitespace() || chars[i] == '/') {
            i += 1;
        }
        let start = i;
        while i < chars.len() && !chars[i].is_whitespace() && chars[i] != '=' && chars[i] != '/' {
            i += 1;
        }
        let name: String = chars[start..i].iter().collect();
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        let mut value = String::new();
        if i < chars.len() && chars[i] == '=' {
            i += 1;
            while i < chars.len() && chars[i].is_whitespace() {
                i += 1;
            }
            if i < chars.len() && (chars[i] == '"' || chars[i] == '\'') {
                let quote = chars[i];
                i += 1;
                let vstart = i;
                while i < chars.len() && chars[i] != quote {
                    i += 1;
                }
                value = chars[vstart..i].iter().collect();
                i += 1;
            } else {
                let vstart = i;
                while i < chars.len() && !chars[i].is_whitespace() {
                    i += 1;
                }
                value = chars[vstart..i].iter().collect();
            }
        }
        if !name.is_empty() {
            out.push((name, value));
        } else if i == start {
            i += 1;
        }
    }
    out
}

fn unescape(s: &str) -> String {
    s.replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
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

fn document(html: String, csp: &str, rendition: &str) -> Response<Vec<u8>> {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(header::CONTENT_SECURITY_POLICY, csp)
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
    match rendered {
        Ok(Rendered::Html(html)) => {
            let csp = effective_csp(&html);
            document(html, &csp, "html")
        }
        Ok(Rendered::Text(body)) => document(text_document(body.as_deref()), MESSAGE_CSP, "text"),
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

    #[test]
    fn the_first_csp_meta_is_extracted() {
        let html = format!(
            "<meta http-equiv=\"Content-Security-Policy\" content=\"{MESSAGE_CSP}\">\n\
             <html><head><meta http-equiv='content-security-policy' content='default-src *'></head></html>"
        );
        assert_eq!(extract_csp(&html).as_deref(), Some(MESSAGE_CSP));
    }

    #[test]
    fn attribute_order_case_and_quotes_do_not_matter() {
        let html = "<html><HEAD><META charset=utf-8><Meta CONTENT='default-src &#39;none&#39;; img-src data:' HTTP-EQUIV=Content-Security-Policy></HEAD>";
        assert_eq!(
            extract_csp(html).as_deref(),
            Some("default-src 'none'; img-src data:")
        );
        assert_eq!(
            extract_csp("<html><meta charset=utf-8><p>no policy</p>"),
            None
        );
    }

    #[test]
    fn a_lax_or_missing_policy_is_replaced_by_ours() {
        assert_eq!(effective_csp("<p>none</p>"), MESSAGE_CSP);
        assert_eq!(
            effective_csp(
                "<meta http-equiv=\"Content-Security-Policy\" content=\"default-src *\">"
            ),
            MESSAGE_CSP
        );
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
