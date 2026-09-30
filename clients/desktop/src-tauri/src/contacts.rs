//! The Contacts view (CON-01, CON-03, CON-05, CON-06, CON-07, CON-08, #0131).
//!
//! `contact.search` answers an account's ranked contacts, a query's fuzzy
//! matches or, for an empty query, the whole index in rank order; each row
//! gains here the recipient the compose wizard writes into To, formatted by
//! `mp_core::addresses::format_recipient`, so the webview never re-implements
//! the quoting of a display name with a comma in it. `contact.rebuild` is an
//! operation the GUI awaits as `contact_rebuild`, settling as a
//! [`ContactRebuilt`] whose `saved` says whether the cache guard kept the
//! index it had.
//!
//! No daemon method exports a vCard, so [`contact_vcard_draft_on`] does what
//! the TUI's `v` does client-side: a new draft addressed to the contact with
//! the subject `Contact: <name>`, the `.vcf` written by
//! `mp_core::contacts::contact_to_vcard` into `_vcards/` beside the draft
//! (`<stem>.vcf`, then `<stem>-1.vcf` and on while the name is taken), and
//! attached through the same `attachments:` append the Attach dialog uses.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::State;

use mp_core::contacts::{contact_to_vcard, vcard_file_stem, Contact, ContactSource};
use mp_protocol::draft::DraftCreated;

use crate::attachments::{draft_attach_on, home_dir};
use crate::commands::{call, decode, draft_create_on, with_door, DraftHeaders, OperationStarted};
use crate::error::{Addressing, GuiError};
use crate::session::{Door, PendingKind, SessionHandle};

/// One search: on an account with no cache yet the daemon builds the index
/// inside the query, which walks the whole store.
const SEARCH_BUDGET: Duration = Duration::from_secs(30);

/// Starting a rebuild: the daemon answers with an id before it walks anything.
const REBUILD_START_BUDGET: Duration = Duration::from_secs(10);

/// The directory beside the drafts the vCards live in, the TUI's.
pub const VCARD_DIR: &str = "_vcards";

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
}

#[derive(Deserialize)]
struct WireSearch {
    account: String,
    #[serde(default)]
    query: String,
    contacts: Vec<WireContact>,
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

/// A contact sent as a vCard: the new draft, and the `.vcf` it attaches.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
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
            })
            .collect(),
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

/// The name a vCard draft's subject gives the contact: the display name, else
/// the address's local part (the TUI's `vcard_display_name`).
pub fn vcard_name(display_name: &str, address: &str) -> String {
    let name = display_name.trim();
    if name.is_empty() {
        address.split('@').next().unwrap_or(address).to_string()
    } else {
        name.to_string()
    }
}

/// Where the `.vcf` of a contact whose file stem is `stem` goes in `dir`: the
/// first of `<stem>.vcf`, `<stem>-1.vcf`, `<stem>-2.vcf`, ... not taken.
pub fn vcard_path(dir: &Path, stem: &str) -> PathBuf {
    let mut path = dir.join(format!("{stem}.vcf"));
    let mut n = 1usize;
    while path.exists() {
        path = dir.join(format!("{stem}-{n}.vcf"));
        n += 1;
    }
    path
}

/// Send a contact as a vCard, the TUI's `v`: a new draft named `name`
/// addressed to the contact, subject `Contact: <name>`, with no signature
/// (the TUI's vCard draft carries none), and the contact's `.vcf` written
/// under `_vcards/` beside it and appended to its `attachments:`. The
/// frontend opens the draft in the editor.
pub fn contact_vcard_draft_on(
    door: &Door,
    account: &str,
    name: &str,
    address: &str,
    display_name: &str,
    home: Option<&Path>,
) -> Result<VcardDraft, GuiError> {
    let address = address.trim();
    if address.is_empty() {
        return Err(GuiError::protocol("A vCard needs the contact's address"));
    }
    let contact = Contact {
        address: address.to_string(),
        display_name: display_name.trim().to_string(),
        sent_to: 0,
        sent_cc: 0,
        received: 0,
        first_seen: String::new(),
        last_seen: String::new(),
        source: ContactSource::default(),
    };
    let headers = DraftHeaders {
        to: mp_core::addresses::format_recipient(&contact.display_name, &contact.address),
        cc: String::new(),
        bcc: String::new(),
        subject: format!("Contact: {}", vcard_name(&contact.display_name, address)),
    };
    let draft = draft_create_on(door, account, name, None, true, Some(&headers))?;
    let drafts = Path::new(&draft.path)
        .parent()
        .ok_or_else(|| GuiError::protocol(format!("{} has no directory", draft.path)))?;
    let dir = drafts.join(VCARD_DIR);
    std::fs::create_dir_all(&dir)
        .map_err(|e| GuiError::protocol(format!("could not create {}: {e}", dir.display())))?;
    let vcf = vcard_path(&dir, &vcard_file_stem(&contact));
    std::fs::write(&vcf, contact_to_vcard(&contact))
        .map_err(|e| GuiError::protocol(format!("could not write {}: {e}", vcf.display())))?;
    let vcf = vcf.display().to_string();
    draft_attach_on(door, account, &draft.id, &vcf, home)?;
    Ok(VcardDraft { draft, vcf })
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
        contact_vcard_draft_on(
            door,
            &account,
            &name,
            &address,
            &display_name,
            home_dir().as_deref(),
        )
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::Fixture;
    use std::sync::Arc;

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
    fn a_vcard_name_is_the_display_name_else_the_local_part() {
        assert_eq!(vcard_name(" Doe, Jane ", "jane@x.example"), "Doe, Jane");
        assert_eq!(vcard_name("", "ops@x.example"), "ops");
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
            None,
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
            None,
        )
        .expect("drafted again");
        assert_eq!(
            Path::new(&second.vcf),
            drafts.join(VCARD_DIR).join("doe-jane-1.vcf")
        );
        // A draft, its path and its attachment list: nothing else is asked,
        // and no editor opens; the frontend opens the draft.
        let methods: std::collections::BTreeSet<String> =
            f.calls().into_iter().map(|(m, _)| m).collect();
        assert_eq!(
            methods.into_iter().collect::<Vec<_>>(),
            ["draft.create", "draft.path"]
        );
        assert!(f.editor_opens().is_empty());
    }

    #[test]
    fn a_vcard_draft_without_an_address_asks_nothing() {
        let (door, f) = fixture_door();
        let err = contact_vcard_draft_on(&door, "work", "draft-x", "  ", "Nobody", None)
            .expect_err("refused");
        assert!(matches!(err, GuiError::Protocol { .. }), "{err:?}");
        assert!(f.calls().is_empty());
    }
}
