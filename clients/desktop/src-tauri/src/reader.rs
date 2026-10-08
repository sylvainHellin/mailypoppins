//! The `mpmsg` custom URI scheme: `mpmsg://localhost/<account>/<row_id>`
//! answers the `message.html` rendition as its own document.
//!
//! The plan's reader rule (docs/plans/native-gui.md, "Reading HTML bodies"):
//! the document carries the reader policy as its `Content-Security-Policy`
//! response header, and the app CSP stays strict. The header is built from
//! constants and never from a value read out of the message: a sender can hide a
//! meta-looking string inside the doctype, ahead of the daemon's own tag, and
//! a header (unlike a meta) may carry `report-uri`, which would turn every
//! blocked remote image into a violation report to the sender. A browser
//! enforces the header and every meta policy, so a sender's leftover meta can
//! only tighten.
//!
//! An HTML rendition is served under the daemon's policy ([`DAEMON_CSP`])
//! plus one `script-src 'nonce-…'` source, a nonce drawn afresh for every
//! response ([`fresh_nonce`]), which admits [`BRIDGE`], the app's own script
//! (PERSO-81): the frame runs `allow-scripts` without `allow-same-origin`, and
//! the bridge scrolls the body on the parent's messages and forwards the
//! frame's keys to the keymap. No sender script, no `on*` handler and no
//! `javascript:` URL carries a nonce the sender could not know when writing
//! the message, so the sender's markup still runs nothing. A nonce rather than
//! the bridge's hash: under CSP Level 3 a hash source also admits
//! `<script src=… integrity="sha256-<the same hash>">`, and the bridge's hash
//! is public, so a sender could have made the frame fetch one URL. The
//! daemon's meta says `default-src 'none'` and would block the bridge, so
//! [`with_bridge`] replaces it with a meta carrying the response's policy and
//! puts the bridge right after it; the browser rendition (`html_open`) is the
//! daemon's and keeps its script-free meta. A message without markup is
//! served as a plain-text document the handler builds itself (header
//! `X-Mp-Rendition: text`), with the bridge under its own fresh nonce too, so
//! the reader has one path and the scroll keys work on it as on HTML. Every
//! other response (an HTML rendition without the daemon's meta, any response
//! for which no nonce could be drawn, a failure) carries [`DAEMON_CSP`] alone
//! and no script. A rendition over 8 MiB is refused inline with `-32004`, and
//! the handler falls back to `message.materialise_html`, reads the file and
//! releases the handle at once; a row that does not exist is 404 and a
//! malformed URL 400.

use std::sync::OnceLock;
use std::time::Duration;

use base64::Engine as _;
use serde_json::{json, Value};
use tauri::http::{header, Response, StatusCode};

use crate::error::{rpc_code, Addressing, GuiError};
use crate::session::Door;

/// The policy `message.html` prepends as a meta tag (`CSP_META` in
/// `mp-core`'s `inject_csp_meta`); a test pins the two to the same text.
pub const DAEMON_CSP: &str = "default-src 'none'; img-src data:; style-src 'unsafe-inline'; font-src data:; form-action 'none'; base-uri 'none'";

/// The reader frame's only script, minified by hand and admitted by the
/// response's nonce ([`message_csp`]).
///
/// - It runs only inside a frame (`parent !== window`). It needs no guard
///   against a second copy: a copy the sender pastes carries no valid nonce
///   and never runs, and the handler injects the bridge once.
/// - A `message` is obeyed only from `window.parent` and only as
///   `{type:"scroll", dy:<finite number>}` or
///   `{type:"scrollTo", y:"top"|"bottom"}`, on `document.scrollingElement`.
/// - Every `keydown` (capture phase, outside an IME composition) is posted to
///   the parent as `{type:"key", key, code, ctrlKey, metaKey, altKey,
///   shiftKey}` with target `"*"`, the frame's parent being the only window
///   it can reach. Its default is prevented unless Ctrl or Cmd is held, so
///   Space, the arrows and Page keys do not also scroll the frame natively:
///   the app's keymap decides what each key does, inside the frame as
///   outside it. Ctrl and Cmd combinations keep their default too, so copy
///   and select all still work on the message text.
pub const BRIDGE: &str = r#"(function(){var w=window,p=w.parent,d=document;if(p===w)return;w.addEventListener("message",function(e){var m=e.data,s=d.scrollingElement;if(e.source!==p||!m||typeof m!=="object"||!s)return;if(m.type==="scroll"&&typeof m.dy==="number"&&isFinite(m.dy))s.scrollTop+=m.dy;else if(m.type==="scrollTo"&&(m.y==="top"||m.y==="bottom"))s.scrollTop=m.y==="top"?0:s.scrollHeight});d.addEventListener("keydown",function(e){if(e.isComposing)return;if(!e.ctrlKey&&!e.metaKey)e.preventDefault();p.postMessage({type:"key",key:e.key,code:e.code,ctrlKey:e.ctrlKey,metaKey:e.metaKey,altKey:e.altKey,shiftKey:e.shiftKey},"*")},true)})();"#;

/// A nonce for one response: 16 bytes from the operating system's generator
/// in standard base64 (24 characters, the last two `=`), a value the sender
/// cannot know when writing the message and which no other response reuses.
/// None if the generator fails, and the response then goes out without the
/// bridge.
fn fresh_nonce() -> Option<String> {
    let mut bytes = [0u8; 16];
    match getrandom::fill(&mut bytes) {
        Ok(()) => Some(base64::engine::general_purpose::STANDARD.encode(bytes)),
        Err(e) => {
            tracing::warn!("[reader] no nonce for the bridge ({e}); served without it");
            None
        }
    }
}

/// The policy of a response that carries the bridge, as its header and as its
/// meta: [`DAEMON_CSP`] with `nonce` as the only script source. Built from
/// constants and the nonce, never from a value read out of the message.
pub fn message_csp(nonce: &str) -> String {
    format!("{DAEMON_CSP}; script-src 'nonce-{nonce}'")
}

/// A CSP meta tag for `policy`, spelled as the daemon spells its own.
fn csp_meta(policy: &str) -> String {
    format!("<meta http-equiv=\"Content-Security-Policy\" content=\"{policy}\">")
}

/// The daemon's meta tag, byte for byte: the marker [`with_bridge`] looks for.
fn daemon_meta() -> &'static str {
    static META: OnceLock<String> = OnceLock::new();
    META.get_or_init(|| csp_meta(DAEMON_CSP))
}

/// The HTML rendition as the frame gets it under `nonce`: every copy of the
/// daemon's meta replaced by one carrying [`message_csp`]`(nonce)`, and
/// `<script nonce="…">`[`BRIDGE`]`</script>` once, right after the first.
/// None when the rendition has no daemon meta.
///
/// The bridge goes after the meta rather than at the end of `<head>`: the
/// daemon puts its meta at the very start of the document (after a leading
/// doctype) precisely because a searched-for `<head>` is spoofable, and the
/// bridge rides on the same anchor, ahead of any sender markup. Every copy is
/// replaced, not only the first, since a sender can make the daemon's own
/// stripping reassemble the exact tag further on; a leftover daemon meta
/// would block the bridge, and the served meta is no weaker than the header.
/// Without the daemon's meta the caller serves the document as it came, with
/// no bridge, under [`DAEMON_CSP`]: a rendition that does not look like the
/// daemon's gets no script.
fn with_bridge(html: &str, nonce: &str) -> Option<String> {
    let marker = daemon_meta();
    let at = html.find(marker)?;
    let meta = csp_meta(&message_csp(nonce));
    let rest = &html[at + marker.len()..];
    let mut out = String::with_capacity(html.len() + BRIDGE.len() + 256);
    out.push_str(&html[..at]);
    out.push_str(&meta);
    out.push_str(&bridge_script(nonce));
    out.push_str(&rest.replace(marker, &meta));
    Some(out)
}

/// `<script nonce="…">`[`BRIDGE`]`</script>` under `nonce`.
fn bridge_script(nonce: &str) -> String {
    format!("<script nonce=\"{nonce}\">{BRIDGE}</script>")
}

/// The response for an HTML rendition: with the bridge under a fresh nonce,
/// the header carrying the same policy as the meta, or, without the daemon's
/// meta or a nonce, as it came under [`DAEMON_CSP`].
fn html_document(html: &str) -> Response<Vec<u8>> {
    if !html.contains(daemon_meta()) {
        tracing::warn!("[reader] no daemon CSP meta in the rendition; served without the bridge");
        return document(html.to_string(), DAEMON_CSP, "html");
    }
    let bridged = fresh_nonce()
        .and_then(|nonce| with_bridge(html, &nonce).map(|served| (served, message_csp(&nonce))));
    match bridged {
        Some((served, policy)) => document(served, &policy, "html"),
        None => document(html.to_string(), DAEMON_CSP, "html"),
    }
}

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
            // No markup: the stored plain text, what `message_text` answers.
            Some(-32602) => crate::commands::message_text_on(door, account, row_id)
                .map(|text| Rendered::Text(text.body)),
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

/// The response for a message without markup: [`text_document`] with the
/// bridge under a fresh nonce, the header carrying the same policy as the
/// meta, or, without a nonce, with no bridge under [`DAEMON_CSP`].
fn text_response(body: Option<&str>) -> Response<Vec<u8>> {
    match fresh_nonce() {
        Some(nonce) => document(
            text_document(body, Some(nonce.as_str())),
            &message_csp(&nonce),
            "text",
        ),
        None => document(text_document(body, None), DAEMON_CSP, "text"),
    }
}

/// The plain-text document for a message without markup, with the bridge
/// under `nonce` (PERSO-81), so the scroll keys and the keys pressed inside
/// the frame work on it as on an HTML rendition.
///
/// The app builds this document itself from constants and the stored text,
/// escaped by [`escape_html`], so it holds no sender markup: the bridge is its
/// only script, and the meta carries [`message_csp`]`(nonce)` as the header
/// does. Without a nonce it carries [`DAEMON_CSP`]'s meta and no script.
fn text_document(body: Option<&str>, nonce: Option<&str>) -> String {
    let text = body.map_or_else(
        || "(This message has no stored body.)".to_string(),
        escape_html,
    );
    let head = match nonce {
        Some(nonce) => format!("{}{}", csp_meta(&message_csp(nonce)), bridge_script(nonce)),
        None => daemon_meta().to_string(),
    };
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\">{head}\
         <style>body{{margin:16px;font:14px/1.5 -apple-system,system-ui,sans-serif}}\
         pre{{white-space:pre-wrap;word-wrap:break-word;font:inherit;margin:0}}</style>\
         </head><body><pre>{text}</pre></body></html>"
    )
}

fn document(html: String, policy: &str, rendition: &str) -> Response<Vec<u8>> {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(header::CONTENT_SECURITY_POLICY, policy)
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
        .header(header::CONTENT_SECURITY_POLICY, DAEMON_CSP)
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
/// The header is [`DAEMON_CSP`], or [`message_csp`] under the response's own
/// nonce when the bridge is in, never a policy the message carries.
fn answer(path: &str, rendered: Result<Rendered, GuiError>) -> Response<Vec<u8>> {
    match rendered {
        Ok(Rendered::Html(html)) => html_document(&html),
        Ok(Rendered::Text(body)) => text_response(body.as_deref()),
        Err(e) => {
            let status = match &e {
                GuiError::NotFound { .. } => StatusCode::NOT_FOUND,
                GuiError::DaemonUnavailable { .. } | GuiError::VersionMismatch { .. } => {
                    StatusCode::SERVICE_UNAVAILABLE
                }
                GuiError::Timeout { .. } => StatusCode::GATEWAY_TIMEOUT,
                GuiError::Protocol { .. } => StatusCode::BAD_GATEWAY,
                GuiError::Setup { .. } | GuiError::Internal { .. } => {
                    StatusCode::INTERNAL_SERVER_ERROR
                }
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
    use base64::Engine as _;
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

    /// The policy's `script-src` directive, or None.
    fn script_src(policy: &str) -> Option<String> {
        policy
            .split(';')
            .map(str::trim)
            .find(|d| d.starts_with("script-src"))
            .map(str::to_string)
    }

    /// The response's `Content-Security-Policy` header.
    fn header_csp(response: &Response<Vec<u8>>) -> String {
        response.headers()[header::CONTENT_SECURITY_POLICY]
            .to_str()
            .expect("ascii")
            .to_string()
    }

    /// The nonce of a policy's `script-src 'nonce-…'`, or None.
    fn policy_nonce(policy: &str) -> Option<String> {
        script_src(policy)?
            .strip_prefix("script-src 'nonce-")?
            .strip_suffix('\'')
            .map(str::to_string)
    }

    /// The meta a response under `nonce` carries.
    fn served_meta(nonce: &str) -> String {
        csp_meta(&message_csp(nonce))
    }

    /// The response, its body and the nonce its header carries.
    fn served(path: &str) -> (Response<Vec<u8>>, String, String) {
        let response = respond("GET", path, Ok(fixture_door()));
        let body = String::from_utf8(response.body().clone()).expect("utf-8");
        let nonce = policy_nonce(&header_csp(&response)).expect("a nonce in the header");
        (response, body, nonce)
    }

    #[test]
    fn the_daemon_meta_is_the_one_mp_core_writes() {
        let rendered = mp_core::parse::inject_csp_meta("<p>x</p>");
        assert!(rendered.starts_with(daemon_meta()), "{rendered}");
    }

    #[test]
    fn the_policy_admits_the_nonce_and_nothing_else() {
        let nonce = fresh_nonce().expect("a nonce");
        let policy = message_csp(&nonce);
        assert_eq!(policy, format!("{DAEMON_CSP}; script-src 'nonce-{nonce}'"));
        assert_eq!(policy_nonce(&policy), Some(nonce.clone()));
        assert_eq!(
            policy.matches("script-src").count(),
            1,
            "one script directive"
        );
        let script = script_src(&policy).expect("a script-src");
        for loosening in [
            "'unsafe-inline'",
            "'unsafe-hashes'",
            "'unsafe-eval'",
            "'strict-dynamic'",
            "'sha256-",
            "'sha384-",
            "'sha512-",
            "*",
            ":",
        ] {
            assert!(
                !script.contains(loosening),
                "script-src carries no {loosening}"
            );
        }
        assert!(!policy.contains("report-"), "no report endpoint");
        assert_eq!(
            policy.matches("'unsafe-inline'").count(),
            1,
            "only style-src is inline"
        );
    }

    #[test]
    fn a_nonce_is_sixteen_fresh_bytes_of_base64() {
        let nonces: Vec<String> = (0..64).map(|_| fresh_nonce().expect("a nonce")).collect();
        for nonce in &nonces {
            assert!(
                nonce.len() == 22 || nonce.len() == 24,
                "{nonce}: 22 or 24 characters"
            );
            assert!(
                nonce
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=')),
                "{nonce}: base64 only"
            );
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(nonce)
                .expect("decodes");
            assert_eq!(bytes.len(), 16, "{nonce}");
        }
        let distinct: std::collections::HashSet<&String> = nonces.iter().collect();
        assert_eq!(distinct.len(), nonces.len(), "no nonce repeats");
    }

    #[test]
    fn the_bridge_is_one_inert_script_body() {
        // It goes into the document between `<script>` and `</script>`, so it
        // must not close the element itself, and it is minified by hand.
        assert!(!BRIDGE.to_ascii_lowercase().contains("</script"));
        assert!(!BRIDGE.contains('\n'));
        assert!(BRIDGE.contains("e.source!==p"), "only the parent is obeyed");
        assert!(
            BRIDGE.contains("isFinite(m.dy)"),
            "only a finite distance scrolls"
        );
        assert!(!BRIDGE.contains("eval") && !BRIDGE.contains("innerHTML"));
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
        let policy = header_csp(&response);
        let nonce = policy_nonce(&policy).expect("a nonce");
        assert_eq!(policy, message_csp(&nonce));
        assert!(!policy.contains("report-"));
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
            let policy = header_csp(&response);
            match policy_nonce(&policy) {
                // The daemon's meta is there: the bridge's policy.
                Some(nonce) => assert_eq!(policy, message_csp(&nonce)),
                // It is not: the daemon's alone.
                None => assert_eq!(policy, DAEMON_CSP),
            }
        }
    }

    #[test]
    fn the_hostile_fixture_is_served_under_the_daemon_policy() {
        let (response, body, nonce) = served("/work/1006");
        assert_eq!(response.status(), StatusCode::OK);
        let h = response.headers();
        assert_eq!(
            h[header::CONTENT_SECURITY_POLICY],
            message_csp(&nonce).as_str()
        );
        assert_eq!(h[header::CONTENT_TYPE], "text/html; charset=utf-8");
        assert_eq!(h[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        assert_eq!(h["X-Mp-Rendition"], "html");
        assert!(
            body.starts_with("<!doctype html <meta content="),
            "the fixture carries the doctype trick"
        );
        assert!(body.contains("report-uri"));
        assert!(
            body.contains(&served_meta(&nonce)),
            "the served tag is present"
        );
    }

    #[test]
    fn the_hostile_fixture_gets_the_bridge_and_no_other_allowance() {
        let (response, body, nonce) = served("/work/1006");
        // The sender's script and handler are still in the markup: only the
        // policy keeps them from running, and it admits the bridge alone.
        assert!(body.contains("<script>document.title = 'script ran'"));
        assert!(body.contains("onload=\"window.open("));
        let header = header_csp(&response);
        assert_eq!(
            script_src(&header),
            Some(format!("script-src 'nonce-{nonce}'"))
        );
        assert!(!header.contains("'sha"), "no hash source");
        // The nonce sits on the bridge alone: one `nonce` attribute, ours.
        assert_eq!(body.matches("nonce=\"").count(), 1);
        assert_eq!(
            body.matches(&format!("<script nonce=\"{nonce}\">")).count(),
            1
        );
        // Every policy the document itself carries is the served one or the
        // sender's (which only tightens), never a second copy of the daemon's.
        assert_eq!(body.matches(&served_meta(&nonce)).count(), 1);
        assert_eq!(
            body.matches("'nonce-").count(),
            1,
            "in the served meta only"
        );
        assert!(
            !body.contains(daemon_meta()),
            "the daemon's tag is replaced"
        );
    }

    #[test]
    fn the_header_the_meta_and_the_script_carry_one_nonce() {
        // 1002 has no markup: the text rendition carries the bridge too.
        for path in ["/work/1006", "/work/1001", "/work/1003", "/work/1002"] {
            let (response, body, nonce) = served(path);
            assert_eq!(header_csp(&response), message_csp(&nonce), "{path}");
            assert!(body.contains(&served_meta(&nonce)), "{path}: the meta");
            assert!(
                body.contains(&format!("<script nonce=\"{nonce}\">{BRIDGE}</script>")),
                "{path}: the script"
            );
        }
    }

    #[test]
    fn two_responses_for_the_same_row_carry_different_nonces() {
        let (_, first_body, first) = served("/work/1001");
        let (_, second_body, second) = served("/work/1001");
        assert_ne!(first, second);
        assert!(!second_body.contains(&first));
        assert!(!first_body.contains(&second));
    }

    #[test]
    fn the_bridge_is_served_once_right_after_the_policy() {
        for path in ["/work/1006", "/work/1001", "/work/1003", "/work/1002"] {
            let (_, body, nonce) = served(path);
            assert_eq!(body.matches(BRIDGE).count(), 1, "{path}");
            let meta = body.find(&served_meta(&nonce)).expect("the served meta");
            let script = body.find("<script").expect("a script");
            assert_eq!(
                &body[meta..script],
                served_meta(&nonce),
                "{path}: the bridge follows the meta"
            );
            assert!(
                body[script..].starts_with(&format!("<script nonce=\"{nonce}\">{BRIDGE}</script>"))
            );
        }
        // Ahead of every sender script in the hostile message.
        let (_, body, _) = served("/work/1006");
        assert!(body.find(BRIDGE).unwrap() < body.find("script ran").unwrap());
    }

    #[test]
    fn a_reassembled_daemon_meta_is_replaced_too_and_the_bridge_stays_single() {
        let html = format!(
            "<!doctype html>{m}<html><head>{m}</head><body>{m}<p>x</p></body></html>",
            m = daemon_meta()
        );
        let nonce = "AAECAwQFBgcICQoLDA0ODw==";
        let served = with_bridge(&html, nonce).expect("the daemon's meta is there");
        assert!(!served.contains(daemon_meta()));
        assert_eq!(served.matches(&served_meta(nonce)).count(), 3);
        assert_eq!(served.matches(BRIDGE).count(), 1);
        assert!(served.starts_with(&format!(
            "<!doctype html>{}<script nonce=\"{nonce}\">{BRIDGE}</script><html>",
            served_meta(nonce)
        )));
    }

    #[test]
    fn a_rendition_without_the_daemon_meta_gets_no_bridge() {
        for html in [
            "<p>none</p>",
            "<meta http-equiv=\"Content-Security-Policy\" content=\"default-src *\"><p>x</p>",
        ] {
            assert_eq!(with_bridge(html, "AAECAwQFBgcICQoLDA0ODw=="), None);
            let response = answer("/work/1", Ok(Rendered::Html(html.to_string())));
            assert_eq!(header_csp(&response), DAEMON_CSP, "no script source");
            let body = String::from_utf8(response.into_body()).expect("utf-8");
            assert_eq!(body, html, "served as it came");
            assert!(!body.contains("<script"));
        }
    }

    #[test]
    fn every_response_is_typed_and_unsniffable() {
        // The text rendition and a failure carry the same guards as the
        // HTML one: a declared type the webview may not second-guess, and
        // the daemon policy; the text rendition adds the bridge's nonce, a
        // failure carries no script source since no bridge is in.
        let (text, _, nonce) = served("/work/1002");
        assert_eq!(
            text.headers()[header::CONTENT_TYPE],
            "text/html; charset=utf-8"
        );
        assert_eq!(text.headers()[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        assert_eq!(text.headers()[header::REFERRER_POLICY], "no-referrer");
        assert_eq!(header_csp(&text), message_csp(&nonce));
        for failed in [
            respond("GET", "/work/99999", Ok(fixture_door())),
            respond("GET", "/work/x", Ok(fixture_door())),
            respond("GET", "/work/1001", Err(GuiError::unavailable("down"))),
        ] {
            let h = failed.headers();
            assert_eq!(h[header::CONTENT_TYPE], "text/plain; charset=utf-8");
            assert_eq!(h[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
            assert_eq!(h[header::CONTENT_SECURITY_POLICY], DAEMON_CSP);
        }
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
        let (response, body, nonce) = served("/work/1002");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["X-Mp-Rendition"], "text");
        assert!(body.contains("Dachsanierung"));
        // The bridge is in, once, under the header's nonce, and the meta
        // carries the header's policy rather than the daemon's.
        assert_eq!(header_csp(&response), message_csp(&nonce));
        assert_eq!(body.matches(&served_meta(&nonce)).count(), 1);
        assert!(!body.contains(daemon_meta()));
        assert_eq!(body.matches(&bridge_script(&nonce)).count(), 1);
        assert_eq!(body.to_ascii_lowercase().matches("<script").count(), 1);
        assert_eq!(body.matches("nonce=\"").count(), 1);
    }

    #[test]
    fn the_text_document_escapes_the_body_and_carries_the_bridge_alone() {
        let nonce = "AAECAwQFBgcICQoLDA0ODw==";
        let text = "<script>alert(1)</script> <SCRIPT src=x></SCRIPT> \"q\" & <img onerror=y>";
        let doc = text_document(Some(text), Some(nonce));
        assert!(doc.contains(
            "&lt;script&gt;alert(1)&lt;/script&gt; &lt;SCRIPT src=x&gt;&lt;/SCRIPT&gt; &quot;q&quot; &amp; &lt;img onerror=y&gt;"
        ));
        assert_eq!(doc.to_ascii_lowercase().matches("<script").count(), 1);
        assert_eq!(doc.matches(&bridge_script(nonce)).count(), 1);
        assert_eq!(doc.matches(&served_meta(nonce)).count(), 1);
        assert!(!doc.contains("<img"));
        // Without a nonce: the daemon's meta and no script.
        let bare = text_document(Some(text), None);
        assert!(bare.contains(daemon_meta()));
        assert!(!bare.to_ascii_lowercase().contains("<script"));
        assert!(!bare.contains("nonce"));
    }

    #[test]
    fn a_message_without_markup_or_text_says_it_has_no_stored_body() {
        let (tx, rx) = std::sync::mpsc::channel();
        std::mem::forget(rx);
        let fixture = Arc::new(Fixture::load(tx).expect("fixture"));
        fixture.clear_body("work", 1002);
        let door = Door::Fixture(Arc::clone(&fixture));
        assert_eq!(render(&door, "work", 1002), Ok(Rendered::Text(None)));
        let response = respond("GET", "/work/1002", Ok(door));
        assert_eq!(response.headers()["X-Mp-Rendition"], "text");
        let nonce = policy_nonce(&header_csp(&response)).expect("a nonce in the header");
        let body = String::from_utf8(response.into_body()).expect("utf-8");
        assert!(body.contains("(This message has no stored body.)"));
        assert_eq!(body.matches(&bridge_script(&nonce)).count(), 1);
    }
}
