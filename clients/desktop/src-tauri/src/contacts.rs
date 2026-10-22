//! The Contacts view (CON-01, CON-03, CON-05, CON-06, CON-07, CON-08, #0131).
//!
//! `contact.search` answers an account's ranked contacts, a query's fuzzy
//! matches or, for an empty query, the whole index in rank order; each row
//! gains here the recipient the compose wizard writes into To, formatted by
//! `mp_core::addresses::format_recipient`, so the webview never re-implements
//! the quoting of a display name with a comma in it. `contact.rebuild` is an
//! operation the GUI awaits as `contact_rebuild`, settling as a
//! [`ContactRebuilt`] whose `saved` says whether the cache guard kept the
//! index it had; a search whose own build the guard refused says so in
//! [`ContactSearch::rebuild`]. The daemon announces every write of an index as
//! `contact.changed`, which is when the webview reads a list again.
//!
//! [`contact_vcard_draft_on`] is the TUI's `v`, one `contact.vcard_draft`: the
//! daemon writes a new draft addressed to the contact with the subject
//! `Contact: <name>`, the contact's `.vcf` under `_vcards/` beside it, and the
//! attachment.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::State;

use mp_protocol::contact::RefusedRebuild;
use mp_protocol::draft::DraftCreated;

use crate::commands::{call, decode, with_door, OperationStarted};
use crate::error::{Addressing, GuiError};
use crate::session::{Door, PendingKind, SessionHandle};

/// One search: on an account with no cache yet the daemon builds the index
/// inside the query, which walks the whole store.
const SEARCH_BUDGET: Duration = Duration::from_secs(30);

/// Starting a rebuild: the daemon answers with an id before it walks anything.
const REBUILD_START_BUDGET: Duration = Duration::from_secs(10);

/// One vCard draft: a draft and a small file written.
const VCARD_BUDGET: Duration = Duration::from_secs(10);

/// A contact as `contact.search` carries it.
#[derive(Deserialize)]
struct WireContact {
    address: String,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    sent_to: u32,
    #[serde(default)]
    sent_cc: u32,
    #[serde(default)]
    received: u32,
    #[serde(default)]
    score: u32,
    #[serde(default)]
    first_seen: String,
    #[serde(default)]
    last_seen: String,
}

#[derive(Deserialize)]
struct WireSearch {
    account: String,
    #[serde(default)]
    query: String,
    contacts: Vec<WireContact>,
    #[serde(default)]
    rebuild: Option<RefusedRebuild>,
}

/// One ranked contact: the daemon's row, whose `score` is the match score
/// (`u32::MAX` for every row of an empty query, which ranks by tier and
/// recency instead), plus `recipient`, the address as To takes it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
pub struct ContactRow {
    pub address: String,
    /// Empty when no message ever named one.
    pub display_name: String,
    pub sent_to: u32,
    pub sent_cc: u32,
    pub received: u32,
    pub score: u32,
    /// When a message first named the contact, RFC 3339; empty when the
    /// index does not know.
    pub first_seen: String,
    /// When a message last named the contact, RFC 3339; empty when the index
    /// does not know.
    pub last_seen: String,
    /// `Display Name <address>`, the name quoted when it needs it, or the
    /// bare address.
    pub recipient: String,
}

/// `contact.search`'s answer: the query as the daemon read it and its rows,
/// best first.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
pub struct ContactSearch {
    pub account: String,
    pub query: String,
    pub contacts: Vec<ContactRow>,
    /// The build this search ran because the account had no index, when the
    /// cache guard refused it: the rows are the kept index's.
    pub rebuild: Option<RefusedRebuild>,
}

/// What `contact.rebuild` settles with, the daemon's inline `json!`
/// (`src/daemon/methods/contact.rs`): `contacts` the rebuild found; `saved`
/// is `written`, or `refused_empty` / `refused_shrunk` when the cache guard
/// kept the `kept` contacts it had instead.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
pub struct ContactRebuilt {
    pub account: String,
    pub contacts: u64,
    pub kept: u64,
    pub saved: String,
    pub cache_path: String,
}

/// A contact sent as a vCard: the new draft, and the `.vcf` it attaches; the
/// shape of `mp_protocol::contact::VcardDraft`, kept under this name for the
/// frontend's generated types.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
pub struct VcardDraft {
    pub draft: DraftCreated,
    pub vcf: String,
}

/// `account`'s contacts matching `query`, at most `limit`, best first.
pub fn contact_search_on(
    door: &Door,
    account: &str,
    query: &str,
    limit: u32,
) -> Result<ContactSearch, GuiError> {
    let answer = call(
        door,
        "contact.search",
        json!({"account": account, "query": query, "limit": limit}),
        SEARCH_BUDGET,
        Addressing::Resource,
    )?;
    let wire: WireSearch = decode("contact.search", answer)?;
    Ok(ContactSearch {
        account: wire.account,
        query: wire.query,
        contacts: wire
            .contacts
            .into_iter()
            .map(|c| ContactRow {
                recipient: mp_core::addresses::format_recipient(&c.display_name, &c.address),
                address: c.address,
                display_name: c.display_name,
                sent_to: c.sent_to,
                sent_cc: c.sent_cc,
                received: c.received,
                score: c.score,
                first_seen: c.first_seen,
                last_seen: c.last_seen,
            })
            .collect(),
        rebuild: wire.rebuild,
    })
}

/// Rebuild `account`'s contact index, awaited as `contact_rebuild`, whose
/// `result` is a [`ContactRebuilt`].
pub fn contact_rebuild_on(
    session: &SessionHandle,
    door: &Door,
    account: &str,
) -> Result<OperationStarted, GuiError> {
    let operation_id = session.start_operation(
        door,
        "contact.rebuild",
        json!({"account": account}),
        PendingKind::ContactRebuild,
        REBUILD_START_BUDGET,
    )?;
    Ok(OperationStarted { operation_id })
}

/// Send a contact as a vCard, the TUI's `v`: one `contact.vcard_draft`, which
/// writes a new draft named `name` addressed to the contact, subject
/// `Contact: <name>`, with no signature, and the contact's `.vcf` under
/// `_vcards/` beside it, attached. The frontend opens the draft in the editor.
pub fn contact_vcard_draft_on(
    door: &Door,
    account: &str,
    name: &str,
    address: &str,
    display_name: &str,
) -> Result<VcardDraft, GuiError> {
    if address.trim().is_empty() {
        return Err(GuiError::protocol("A vCard needs the contact's address"));
    }
    let answer = call(
        door,
        "contact.vcard_draft",
        json!({
            "account": account,
            "name": name,
            "address": address,
            "display_name": display_name,
        }),
        VCARD_BUDGET,
        Addressing::Params,
    )?;
    decode("contact.vcard_draft", answer)
}

// ---------------------------------------------------------------------------
// The commands
// ---------------------------------------------------------------------------

#[tauri::command(rename_all = "snake_case")]
pub async fn contact_search(
    session: State<'_, SessionHandle>,
    account: String,
    query: String,
    limit: u32,
) -> Result<ContactSearch, GuiError> {
    with_door(&session, move |_, door| {
        contact_search_on(door, &account, &query, limit)
    })
    .await
}

/// Start a rebuild, awaited as `contact_rebuild`.
#[tauri::command(rename_all = "snake_case")]
pub async fn contact_rebuild(
    session: State<'_, SessionHandle>,
    account: String,
) -> Result<OperationStarted, GuiError> {
    with_door(&session, move |session, door| {
        contact_rebuild_on(session, door, &account)
    })
    .await
}

/// A new draft carrying the contact's vCard; see [`contact_vcard_draft_on`].
#[tauri::command(rename_all = "snake_case")]
pub async fn contact_vcard_draft(
    session: State<'_, SessionHandle>,
    account: String,
    name: String,
    address: String,
    display_name: String,
) -> Result<VcardDraft, GuiError> {
    with_door(&session, move |_, door| {
        contact_vcard_draft_on(door, &account, &name, &address, &display_name)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::Fixture;
    use std::path::Path;
    use std::sync::Arc;

    const VCARD_DIR: &str = "_vcards";

    fn fixture_door() -> (Door, Arc<Fixture>) {
        let (tx, rx) = std::sync::mpsc::channel();
        std::mem::forget(rx);
        let fixture = Arc::new(Fixture::load(tx).expect("fixture"));
        (Door::Fixture(Arc::clone(&fixture)), fixture)
    }

    #[test]
    fn rows_decode_with_the_recipient_quoted_where_the_name_needs_it() {
        let (door, _f) = fixture_door();
        let all = contact_search_on(&door, "work", "", 1000).expect("searched");
        assert_eq!(all.account, "work");
        assert_eq!(all.contacts.len(), 25);
        assert_eq!(all.contacts[0].address, "robin@example.com");
        assert_eq!(all.contacts[0].recipient, "Robin Meyer <robin@example.com>");
        assert_eq!(all.contacts[0].first_seen, "2025-03-14T08:15:00+00:00");
        assert_eq!(all.contacts[0].last_seen, "2026-09-25T16:40:00+00:00");
        assert!(all.contacts.windows(2).all(|w| w[0].score >= w[1].score));
        let jane = all
            .contacts
            .iter()
            .find(|c| c.address == "jane.doe@example.com")
            .expect("the comma name");
        assert_eq!(jane.display_name, "Doe, Jane");
        assert_eq!(jane.recipient, "\"Doe, Jane\" <jane.doe@example.com>");
        let bare = all
            .contacts
            .iter()
            .find(|c| c.address == "ops@example.com")
            .expect("no name");
        assert_eq!(bare.recipient, "ops@example.com");

        let hits = contact_search_on(&door, "work", "DOE", 1000).expect("searched");
        assert_eq!(hits.query, "DOE");
        assert_eq!(hits.contacts.len(), 1);
        let few = contact_search_on(&door, "work", "", 5).expect("limited");
        assert_eq!(few.contacts.len(), 5);
        assert!(matches!(
            contact_search_on(&door, "nobody", "", 20),
            Err(GuiError::NotFound { .. })
        ));
    }

    #[test]
    fn a_rebuild_is_awaited_as_contact_rebuild() {
        let (door, _f) = fixture_door();
        let session = SessionHandle::new(true);
        let started = contact_rebuild_on(&session, &door, "work").expect("started");
        assert_eq!(
            session.pending_kind(&started.operation_id),
            Some(PendingKind::ContactRebuild)
        );
        assert!(contact_rebuild_on(&session, &door, "nobody").is_err());
    }

    #[test]
    fn a_vcard_draft_writes_the_vcf_beside_the_drafts_and_attaches_it_once() {
        let (door, f) = fixture_door();
        let first = contact_vcard_draft_on(
            &door,
            "work",
            "draft-vcard-one",
            "jane.doe@example.com",
            "Doe, Jane",
        )
        .expect("drafted");
        let drafts = Path::new(&first.draft.path).parent().expect("dir");
        assert_eq!(
            Path::new(&first.vcf),
            drafts.join(VCARD_DIR).join("doe-jane.vcf")
        );
        let card = std::fs::read_to_string(&first.vcf).expect("written");
        assert!(card.starts_with("BEGIN:VCARD\r\n"), "{card}");
        assert!(
            card.contains("EMAIL;TYPE=INTERNET:jane.doe@example.com"),
            "{card}"
        );
        let text = std::fs::read_to_string(&first.draft.path).expect("draft");
        let parsed = mp_core::draft::parse_email_draft(Path::new(&first.draft.path))
            .expect("the draft parses");
        assert_eq!(
            parsed.frontmatter.to.as_deref(),
            Some("\"Doe, Jane\" <jane.doe@example.com>"),
            "{text}"
        );
        assert_eq!(parsed.frontmatter.subject, "Contact: Doe, Jane");
        assert_eq!(
            parsed.frontmatter.attachments.unwrap_or_default(),
            vec![first.vcf.clone()]
        );
        assert_eq!(
            text.matches(first.vcf.as_str()).count(),
            1,
            "one attachments line: {text}"
        );
        let second = contact_vcard_draft_on(
            &door,
            "work",
            "draft-vcard-two",
            "jane.doe@example.com",
            "Doe, Jane",
        )
        .expect("drafted again");
        assert_eq!(
            Path::new(&second.vcf),
            drafts.join(VCARD_DIR).join("doe-jane-1.vcf")
        );
        // One call, the daemon's: it writes the draft, the vCard and the
        // attachment, and no editor opens; the frontend opens the draft.
        let methods: std::collections::BTreeSet<String> =
            f.calls().into_iter().map(|(m, _)| m).collect();
        assert_eq!(
            methods.into_iter().collect::<Vec<_>>(),
            ["contact.vcard_draft"]
        );
        assert!(f.editor_opens().is_empty());
    }

    #[test]
    fn a_vcard_draft_without_an_address_asks_nothing() {
        let (door, f) = fixture_door();
        let err =
            contact_vcard_draft_on(&door, "work", "draft-x", "  ", "Nobody").expect_err("refused");
        assert!(matches!(err, GuiError::Protocol { .. }), "{err:?}");
        assert!(f.calls().is_empty());
    }
}
