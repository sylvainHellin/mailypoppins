//! `message.html`: the browser rendition of one message, inline in the answer.
//!
//! `message.materialise_html` writes the rendition into a handle directory and
//! answers a path, which is what a client handing a `file://` URL to a browser
//! needs. A webview wants the same string without a file and without a lease
//! to release, so this method answers the string itself.
//!
//! The string is the file's bytes: the daemon builds both from one function,
//! so the charset, the `Content-Security-Policy` meta tag and the `cid:` images
//! inlined as `data:` URIs are the same in both, byte for byte.
//!
//! A rendition over [`MAX_INLINE_HTML_BYTES`] is refused with
//! `frame_too_large` (`-32004`) and an [`InlineHtmlRefusal`] as its `data`,
//! whose `fallback` names [`HTML_FALLBACK_METHOD`]: the answer would not fit
//! the frame a client should be made to hold, and the file path serves it.

use serde::{Deserialize, Serialize};

/// The method name.
pub const METHOD_MESSAGE_HTML: &str = "message.html";

/// The method a refused inline rendition falls back to, which writes the same
/// bytes into a file.
pub const HTML_FALLBACK_METHOD: &str = "message.materialise_html";

/// The largest rendition `message.html` answers inline: 8 MiB of HTML.
///
/// Half of [`crate::MAX_RESPONSE_BYTES`], so the JSON escaping of a rendition
/// that is mostly quotes still fits one frame in the ordinary case, and a
/// webview is never asked to hold a 16 MiB string for one message. Above it the
/// client asks [`HTML_FALLBACK_METHOD`] for a file instead.
pub const MAX_INLINE_HTML_BYTES: usize = 8 << 20;

/// The params of `message.html`, which address a message exactly as
/// `message.get` does: exactly one of `row_id`, `id` and `selector`, with
/// `mailbox` narrowing a selector.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageHtmlParams {
    /// The account the message is in.
    pub account: String,
    /// `messages.id`, the `id` a `message.list` row carries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub row_id: Option<i64>,
    /// `"<mailbox>/<uid>"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The selector grammar a user types.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector: Option<String>,
    /// Narrows a `selector`, as `mp show --mailbox` does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mailbox: Option<String>,
}

/// The `result` of `message.html`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageHtml {
    /// The account, echoed.
    pub account: String,
    /// The row the address resolved to, so a caller that sent a selector
    /// learns which row it rendered.
    pub row_id: i64,
    /// The rendition, byte-identical to the file `message.materialise_html`
    /// writes for the same row.
    pub html: String,
    /// The UTF-8 length of `html`, which is the `bytes` the file path reports
    /// for the same rendition.
    pub bytes: u64,
}

/// The `data` of the `frame_too_large` refusal a rendition over
/// [`MAX_INLINE_HTML_BYTES`] draws.
///
/// `limit` and `seen` are the pair every `frame_too_large` carries; `fallback`
/// is what distinguishes this one from a frame the transport refused, and names
/// the method that serves the same rendition as a file.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InlineHtmlRefusal {
    /// The inline limit, [`MAX_INLINE_HTML_BYTES`].
    pub limit: u64,
    /// The rendition's length in bytes.
    pub seen: u64,
    /// [`HTML_FALLBACK_METHOD`].
    pub fallback: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The committed request fixture decodes into the typed params, and the
    /// params serialise without the addresses they did not use.
    #[test]
    fn the_committed_request_fixture_decodes_whole() {
        let raw = include_str!("../fixtures/message.html.request.json");
        let request: serde_json::Value = serde_json::from_str(raw).expect("the fixture is JSON");
        assert_eq!(request["method"], json!(METHOD_MESSAGE_HTML));
        let params: MessageHtmlParams =
            serde_json::from_value(request["params"].clone()).expect("the params decode");
        assert_eq!(params.account, "work");
        assert_eq!(params.row_id, Some(3141));
        assert_eq!(params.id, None);
        assert_eq!(
            serde_json::to_value(&params).expect("serialises"),
            request["params"],
            "an unused address is absent rather than null"
        );
    }

    /// The committed response fixture decodes, and `bytes` is the length of
    /// the string it travels beside.
    #[test]
    fn the_committed_response_fixture_decodes_whole() {
        let raw = include_str!("../fixtures/message.html.response.json");
        let response: serde_json::Value = serde_json::from_str(raw).expect("the fixture is JSON");
        let html: MessageHtml =
            serde_json::from_value(response["result"].clone()).expect("the result decodes");
        assert_eq!(html.row_id, 3141);
        assert_eq!(html.bytes, html.html.len() as u64);
        assert!(html
            .html
            .starts_with("<meta http-equiv=\"Content-Security-Policy\""));
    }

    /// The committed refusal fixture is `frame_too_large` with the fallback
    /// named in its `data`.
    #[test]
    fn the_committed_refusal_fixture_names_the_file_path() {
        let raw = include_str!("../fixtures/error.message_html_too_large.json");
        let response: serde_json::Value = serde_json::from_str(raw).expect("the fixture is JSON");
        assert_eq!(
            response["error"]["code"],
            json!(crate::ErrorCode::FrameTooLarge.code())
        );
        let refusal: InlineHtmlRefusal =
            serde_json::from_value(response["error"]["data"].clone()).expect("the data decodes");
        assert_eq!(refusal.limit, MAX_INLINE_HTML_BYTES as u64);
        assert!(refusal.seen > refusal.limit);
        assert_eq!(refusal.fallback, HTML_FALLBACK_METHOD);
    }

    /// The inline limit leaves the frame room for the escaping.
    #[test]
    fn the_inline_limit_is_half_the_response_cap() {
        assert_eq!(MAX_INLINE_HTML_BYTES * 2, crate::MAX_RESPONSE_BYTES);
    }
}
