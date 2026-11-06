//! The `sender.*` family: the blocked senders (PERSO-261).
//!
//! One list, kept by the daemon and shared by every account. An entry is a
//! full address (`spam@example.com`) or a domain (`@example.com`), stored
//! lowercase and matched case-insensitively against a message's `From`
//! address. Blocking a sender moves that sender's mail in each account's
//! Inbox to that account's Junk folder, and the daemon moves what arrives from
//! it later after each sync tick; unblocking only removes the entry.
//!
//! `sender.block` and `sender.unblock` are commands that publish
//! `sender.blocklist_changed` ([`BlockedSenders`]), so every client showing
//! the list reads it again; `sender.list` is the query.

use serde::{Deserialize, Serialize};

/// `sender.block`: add an entry and, unless told otherwise, move the matching
/// Inbox mail of every account to its Junk folder.
pub const METHOD_SENDER_BLOCK: &str = "sender.block";

/// `sender.unblock`: remove an entry; nothing moves back.
pub const METHOD_SENDER_UNBLOCK: &str = "sender.unblock";

/// `sender.list`: the entries, sorted.
pub const METHOD_SENDER_LIST: &str = "sender.list";

/// The `params` of `sender.block`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct SenderBlockParams {
    /// A full address or `@domain`, any case.
    pub address: String,
    /// Whether the mail of the sender already in each Inbox moves to Junk now;
    /// `true` when absent. Mail that arrives later moves either way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub move_existing: Option<bool>,
}

/// The `params` of `sender.unblock`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct SenderUnblockParams {
    /// The entry to remove, spelled as it was blocked or in any case.
    pub address: String,
}

/// The `result` of `sender.list`, and the payload of `sender.blocklist_changed`:
/// every entry, lowercase and sorted.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct BlockedSenders {
    pub entries: Vec<String>,
}

/// The messages one account moved to its Junk folder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct BlockMoved {
    pub account: String,
    /// The store key of the Junk folder they moved to.
    pub mailbox: String,
    pub count: u64,
}

/// An account whose Inbox was left alone, and why: it has no Junk folder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct BlockSkipped {
    pub account: String,
    /// One sentence a client shows as it is.
    pub reason: String,
}

/// The `result` of `sender.block`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct SenderBlocked {
    /// The entry as the list holds it, lowercase.
    pub entry: String,
    /// `false` when the entry was already on the list.
    pub added: bool,
    /// The whole list after the call.
    pub entries: Vec<String>,
    /// One row per account that moved mail, in configuration order; empty
    /// when nothing matched or `move_existing` was `false`.
    pub moved: Vec<BlockMoved>,
    /// One row per account with a server and no Junk folder, whose Inbox
    /// keeps the sender's mail, in configuration order.
    pub skipped: Vec<BlockSkipped>,
}

/// The `result` of `sender.unblock`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct SenderUnblocked {
    /// The entry as the list held it, lowercase.
    pub entry: String,
    /// `false` when the entry was not on the list.
    pub removed: bool,
    /// The whole list after the call.
    pub entries: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(raw: &str) -> serde_json::Value {
        serde_json::from_str(raw).expect("JSON")
    }

    /// The committed `sender.*` fixtures decode into their wire types whole.
    #[test]
    fn the_sender_fixtures_decode() {
        let params: SenderBlockParams = serde_json::from_value(
            frame(include_str!("../fixtures/sender.block.request.json"))["params"].clone(),
        )
        .expect("sender.block params");
        assert_eq!(params.move_existing, Some(true));
        let blocked: SenderBlocked = serde_json::from_value(
            frame(include_str!("../fixtures/sender.block.response.json"))["result"].clone(),
        )
        .expect("sender.block");
        assert_eq!(blocked.entry, params.address);
        assert_eq!(blocked.moved[0].mailbox, "Junk Email");
        let _: SenderUnblockParams = serde_json::from_value(
            frame(include_str!("../fixtures/sender.unblock.request.json"))["params"].clone(),
        )
        .expect("sender.unblock params");
        let unblocked: SenderUnblocked = serde_json::from_value(
            frame(include_str!("../fixtures/sender.unblock.response.json"))["result"].clone(),
        )
        .expect("sender.unblock");
        assert!(unblocked.removed);
        let listed: BlockedSenders = serde_json::from_value(
            frame(include_str!("../fixtures/sender.list.response.json"))["result"].clone(),
        )
        .expect("sender.list");
        let changed: BlockedSenders = serde_json::from_value(
            frame(include_str!(
                "../fixtures/notification.sender_blocklist_changed.json"
            ))["params"]["payload"]
                .clone(),
        )
        .expect("sender.blocklist_changed");
        assert_eq!(listed, changed);
    }
}
