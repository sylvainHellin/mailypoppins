//! `message.attachments`: the parts of one received message, each with the
//! `part` index `message.materialise_attachment` takes (PERSO-222).
//!
//! A client lists the parts with this method and then materialises the ones
//! the user picked, so it never derives an index from a position in another
//! answer. The list is in reading order, the order every client shows it in:
//! the real attachments first, then the images the HTML body shows by `cid:`
//! URL, each group in the message's own order. `part` is the index into the
//! message's stored order, which is why it travels on every entry rather than
//! being the entry's position.

use serde::{Deserialize, Serialize};

/// The method name.
pub const METHOD_MESSAGE_ATTACHMENTS: &str = "message.attachments";

/// One part of a received message.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct MessageAttachment {
    /// The index `message.materialise_attachment` takes for this part.
    pub part: u32,
    /// The file name the sender gave the part, as the store keeps it; the
    /// daemon sanitises it only when it becomes a path.
    pub name: String,
    /// The part's size in bytes, which is the `bytes` its handle reports.
    pub size: u64,
    /// An image the HTML body shows by `cid:` URL (PERSO-165), listed after
    /// the real attachments and still openable and saveable.
    pub inline: bool,
}

/// The `result` of `message.attachments`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct MessageAttachments {
    /// The account, echoed.
    pub account: String,
    /// The canonical selector of the message the address resolved to, the
    /// one `message.get` answers; for a server-search hit the store holds no
    /// row for, the selector a fetch would land it under.
    pub selector: String,
    /// Every part, in reading order; empty for a message with none.
    pub attachments: Vec<MessageAttachment>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The committed request fixture names the method and a row address.
    #[test]
    fn the_committed_request_fixture_addresses_one_message() {
        let raw = include_str!("../fixtures/message.attachments.request.json");
        let request: serde_json::Value = serde_json::from_str(raw).expect("the fixture is JSON");
        assert_eq!(request["method"], json!(METHOD_MESSAGE_ATTACHMENTS));
        assert_eq!(request["params"]["account"], json!("work"));
        assert_eq!(request["params"]["row_id"], json!(3141));
    }

    /// The committed response fixture decodes whole and re-serialises to
    /// itself, and its inline image follows the real attachment while keeping
    /// its own index.
    #[test]
    fn the_committed_response_fixture_decodes_whole() {
        let raw = include_str!("../fixtures/message.attachments.response.json");
        let response: serde_json::Value = serde_json::from_str(raw).expect("the fixture is JSON");
        let listing: MessageAttachments =
            serde_json::from_value(response["result"].clone()).expect("the result decodes");
        assert_eq!(
            serde_json::to_value(&listing).expect("serialises"),
            response["result"]
        );
        let order: Vec<(u32, bool)> = listing
            .attachments
            .iter()
            .map(|a| (a.part, a.inline))
            .collect();
        assert_eq!(order, vec![(1, false), (0, true)]);
    }
}
