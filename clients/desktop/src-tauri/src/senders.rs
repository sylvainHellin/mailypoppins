//! The blocked senders (PERSO-261): the TUI's `B` chooser and its `sb` list.
//!
//! Every command is one `sender.*` call on the daemon: `sender.block`, which
//! adds an entry and moves the sender's mail in each account's Inbox to its
//! Junk folder, `sender.unblock`, which only removes it, and `sender.list`.
//! An entry the daemon refuses (neither a full address nor `@domain`) is its
//! `-32602` carrying the sentence, as a `protocol` error with no code. Both
//! writers publish `sender.blocklist_changed` with the whole list, which the
//! event channel hands the webview as every other event. The fixture
//! answers the same methods with the same sentences.

use std::time::Duration;

use mp_protocol::sender::{
    BlockedSenders, SenderBlocked, SenderUnblocked, METHOD_SENDER_BLOCK, METHOD_SENDER_LIST,
    METHOD_SENDER_UNBLOCK,
};
use serde_json::{json, Value};
use tauri::State;

use crate::commands::{decode, with_door};
use crate::error::{refusal_sentence, rpc_code, Addressing, GuiError};
use crate::session::{Door, SessionHandle};

/// One sender call: a block moves rows in every account's store and queues
/// their server moves, which the daemon answers before it drains them.
const SENDER_BUDGET: Duration = Duration::from_secs(30);

/// One `sender.*` call: a `-32602` refusal carries the daemon's sentence as
/// a `protocol` error; anything else as every call's.
fn sender_call<T: serde::de::DeserializeOwned>(
    door: &Door,
    method: &str,
    params: Value,
) -> Result<T, GuiError> {
    let answer = door
        .call_within(method, params, SENDER_BUDGET)
        .map_err(|e| {
            let text = format!("{e:#}");
            match (rpc_code(&text), refusal_sentence(&text)) {
                (Some(-32602), Some(sentence)) => GuiError::protocol(sentence),
                _ => GuiError::from_call_text(&text, Addressing::Params),
            }
        })?;
    decode(method, answer)
}

/// Block `address`, a full address or `@domain`, moving the mail already in
/// each Inbox.
pub fn sender_block_on(door: &Door, address: &str) -> Result<SenderBlocked, GuiError> {
    sender_call(door, METHOD_SENDER_BLOCK, json!({"address": address}))
}

/// Remove `address` from the list; nothing moves back.
pub fn sender_unblock_on(door: &Door, address: &str) -> Result<SenderUnblocked, GuiError> {
    sender_call(door, METHOD_SENDER_UNBLOCK, json!({"address": address}))
}

/// The list, lowercase and sorted.
pub fn sender_list_on(door: &Door) -> Result<BlockedSenders, GuiError> {
    sender_call(door, METHOD_SENDER_LIST, json!({}))
}

// ---------------------------------------------------------------------------
// Tauri commands
// ---------------------------------------------------------------------------

#[tauri::command(rename_all = "snake_case")]
pub async fn sender_block(
    session: State<'_, SessionHandle>,
    address: String,
) -> Result<SenderBlocked, GuiError> {
    with_door(&session, move |_, door| sender_block_on(door, &address)).await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn sender_unblock(
    session: State<'_, SessionHandle>,
    address: String,
) -> Result<SenderUnblocked, GuiError> {
    with_door(&session, move |_, door| sender_unblock_on(door, &address)).await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn sender_list(session: State<'_, SessionHandle>) -> Result<BlockedSenders, GuiError> {
    with_door(&session, move |_, door| sender_list_on(door)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::Fixture;
    use mp_client::events::Incoming;
    use std::sync::mpsc::Receiver;
    use std::sync::Arc;

    fn fixture_door() -> (Door, Receiver<Incoming>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let fixture = Arc::new(Fixture::load(tx).expect("fixture"));
        (Door::Fixture(fixture), rx)
    }

    /// The `(kind, payload)` of every event posted so far.
    fn drained(rx: &Receiver<Incoming>) -> Vec<(String, Value)> {
        std::iter::from_fn(|| rx.try_recv().ok())
            .filter_map(|i| match i {
                Incoming::Event(e) => Some((e.kind, e.payload)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_block_adds_the_entry_lowercase_and_names_each_account_with_no_junk_folder() {
        let (d, rx) = fixture_door();
        assert!(sender_list_on(&d).expect("list").entries.is_empty());
        let blocked = sender_block_on(&d, " Offers@Shop.Example ").expect("blocked");
        assert_eq!(blocked.entry, "offers@shop.example");
        assert!(blocked.added);
        assert_eq!(blocked.entries, ["offers@shop.example"]);
        assert!(blocked.moved.is_empty());
        let skipped: Vec<(&str, &str)> = blocked
            .skipped
            .iter()
            .map(|s| (s.account.as_str(), s.reason.as_str()))
            .collect();
        assert_eq!(
            skipped,
            [
                (
                    "work",
                    "work has no Junk folder; its Inbox keeps the sender's mail"
                ),
                (
                    "home",
                    "home has no Junk folder; its Inbox keeps the sender's mail"
                ),
            ]
        );
        assert_eq!(
            drained(&rx),
            [(
                "sender.blocklist_changed".to_string(),
                json!({"entries": ["offers@shop.example"]})
            )]
        );
        let again = sender_block_on(&d, "@shop.example").expect("a domain");
        assert_eq!(again.entries, ["@shop.example", "offers@shop.example"]);
        let repeat = sender_block_on(&d, "OFFERS@shop.example").expect("a repeat");
        assert!(!repeat.added);
        assert_eq!(drained(&rx).len(), 1, "only the domain changed the list");
        assert_eq!(
            sender_list_on(&d).expect("list").entries,
            ["@shop.example", "offers@shop.example"]
        );
    }

    #[test]
    fn an_unblock_removes_the_entry_and_one_not_listed_changes_nothing() {
        let (d, rx) = fixture_door();
        sender_block_on(&d, "@spam.example").expect("blocked");
        drained(&rx);
        let unblocked = sender_unblock_on(&d, "@SPAM.example").expect("unblocked");
        assert_eq!(unblocked.entry, "@spam.example");
        assert!(unblocked.removed);
        assert!(unblocked.entries.is_empty());
        assert_eq!(
            drained(&rx),
            [(
                "sender.blocklist_changed".to_string(),
                json!({"entries": []})
            )]
        );
        let none = sender_unblock_on(&d, "@spam.example").expect("not listed");
        assert!(!none.removed);
        assert!(drained(&rx).is_empty());
    }

    #[test]
    fn an_entry_that_is_no_sender_is_refused_with_the_daemons_sentence() {
        let (d, rx) = fixture_door();
        for (raw, why) in [
            ("", "it is empty"),
            (
                "no at sign",
                "it holds a space or a character no address has",
            ),
            ("nobody", "it has no @"),
            ("a@b@c.example", "it has more than one @"),
            ("a@localhost", "its domain is not one"),
        ] {
            match sender_block_on(&d, raw) {
                Err(GuiError::Protocol { message, code }) => {
                    assert_eq!(
                        message,
                        format!(
                            "'{raw}' is not a sender to block: {why}; give a full address (spam@example.com) or a domain (@example.com)"
                        ),
                        "{raw:?}"
                    );
                    assert_eq!(code, None);
                }
                other => panic!("{raw:?}: {other:?}"),
            }
        }
        assert!(drained(&rx).is_empty());
    }

    /// The fixture accounts' own address, `me@example.com`, and its domain are
    /// refused as the daemon refuses them; a colleague at that domain is not.
    #[test]
    fn an_own_address_or_its_domain_is_refused_with_the_daemons_sentence() {
        let (d, rx) = fixture_door();
        for (raw, sentence) in [
            (
                "Me@Example.com",
                "'me@example.com' is one of your own addresses; blocking it would move your own mail to Junk",
            ),
            (
                "@example.com",
                "'@example.com' is the domain of your own address me@example.com; blocking it would move your own mail to Junk",
            ),
        ] {
            match sender_block_on(&d, raw) {
                Err(GuiError::Protocol { message, .. }) => assert_eq!(message, sentence),
                other => panic!("{raw:?}: {other:?}"),
            }
        }
        assert!(drained(&rx).is_empty());
        let colleague = sender_block_on(&d, "ivana@example.com").expect("a colleague");
        assert_eq!(colleague.entries, ["ivana@example.com"]);
    }
}
