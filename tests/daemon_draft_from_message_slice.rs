//! `DFT-08` and `DFT-09` for a message the store does not hold: the reply and
//! the forward built from a server-search hit (#0126, plan unit P5-U10d).
//!
//! This is a **contract test**, written before `draft.create_from_message`
//! exists and against the shape fixed here, in `docs/daemon-protocol.md` and in
//! `crates/mp-protocol/fixtures/draft.create_from_message.{request,response}.json`.
//! It compiles at HEAD and fails at HEAD with `-32601`, which is the proof
//! there is no stub behind it. The implementer does not edit this file.
//!
//! # The surface under test
//!
//! ```text
//! draft.create_from_message {account, kind, message, no_signature?, signature?}
//!                           -> DraftCreated
//! ```
//!
//! # Why `draft.reply` cannot do this
//!
//! `src/tui/actions.rs`'s `write_fetched_draft_and_edit` is the one draft the
//! daemon cannot build today: the overlay's `r`, `R` and `w` over a hit that
//! resolved to no local row. P5-U10c-I2 was briefed to compose it out of
//! `message.fetch` plus `draft.reply` and stopped, because the composition is
//! a different behaviour on four counts, and the first is the one that matters:
//!
//! - **it would ingest the message.** A row would appear in the mailbox, the
//!   unread count would move and the hit would turn into a resolved one. The
//!   overlay has a separate key for that, `f`, and this would make the reply
//!   key do it silently.
//! - **it needs a `Message-ID` and a sidebar mailbox.** `message.fetch` refuses
//!   a hit with neither; both quote fine today.
//! - **it needs the server.** The draft is built from memory and cannot fail
//!   for a network reason; through a fetch, an unreachable account refuses the
//!   reply.
//! - **it is a round trip per reply**, for a payload already in the client's
//!   hand.
//!
//! So the message travels instead of an address. That is also why this is a
//! method rather than a fourth form of `draft.reply`'s `source`: a `source` is
//! an address into the store and every form of it takes the family's `-32006`,
//! while this one reads no message store at all.
//!
//! # What the rows below pin
//!
//! The four behaviours above, and the draft file itself: the builder is
//! `mp_core::draft`'s, so the file is `create_reply_draft_from` /
//! `create_forward_draft_from` over a `SourceMessage` filled from the payload,
//! with the id minted and the index refreshed before the selector is handed
//! out (#0050's post-write discipline, which is what makes the selector
//! resolvable the moment it is printed).
//!
//! # What this file does not assert
//!
//! The *quote layout*. The `>` prefixing, the attribution line and the
//! signature splice are `mp_core::draft`'s and its 45 tests own them; what is
//! asserted here is that the daemon ran that builder over this payload, by
//! reading the frontmatter and the quoted text back out of the file it wrote.

mod support;

use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};
use tempfile::TempDir;

use mp_client::{ClientError, ClientInfo, ClientKind, Connection, Identity};
use mp_protocol::draft::{DraftCreated, DraftListing};
use mp_protocol::{ErrorCode, RpcError};

use mailypoppins::draft::parse_email_draft;
use mailypoppins::store::read;

use support::draft_fixture as fixture;
use support::parity::{socket_path, DaemonFixture};

/// Upper bound on any single wait: a connection, a handshake, one call.
const DEADLINE: Duration = Duration::from_secs(20);

/// The method under test, written once so a rename is one diff.
const METHOD: &str = "draft.create_from_message";

/// The `Message-ID` of the hit the rows below quote, which no fixture store
/// holds: that is the whole point of it.
const HIT: &str = "<nur-auf-dem-server@example.com>";

/// The subject the hit carries, which a reply prefixes and a forward prefixes
/// differently.
const SUBJECT: &str = "Angebot vom Server";

// ---------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------

/// A seeded root and a daemon serving it. The field order is the drop order.
struct Slice {
    /// Held for its `Drop`, which stops the daemon before the directory it was
    /// reading goes away.
    #[allow(dead_code)]
    daemon: DaemonFixture,
    tmp: TempDir,
}

impl Slice {
    fn start() -> Slice {
        let tmp = TempDir::new().expect("a temporary draft-from-message root");
        fixture::seed(tmp.path());
        let daemon = DaemonFixture::start(tmp.path());
        Slice { daemon, tmp }
    }

    fn root(&self) -> &Path {
        self.tmp.path()
    }

    async fn connect(&self) -> Connection {
        let mut conn = within(
            "Connection::connect",
            Connection::connect(&socket_path(self.root())),
        )
        .await
        .expect("connecting to a live daemon socket succeeds");
        within(
            "Connection::initialize",
            conn.initialize(
                ClientInfo {
                    kind: ClientKind::Cli,
                    app_version: env!("CARGO_PKG_VERSION").to_string(),
                },
                Identity {
                    data_dir: self.root().to_path_buf(),
                    config_dir: self.root().to_path_buf(),
                },
                &[],
                &[],
            ),
        )
        .await
        .expect("a compatible handshake succeeds");
        conn
    }

    /// How many rows one account's store holds, across every mailbox the
    /// fixture seeds: the number a draft built from a hit may not move.
    fn stored_rows(&self, account: &str) -> usize {
        let store = support::read_fixture::store(self.root(), account);
        support::read_fixture::MAILBOXES
            .iter()
            .map(|mailbox| {
                read::list_mailbox(&store, account, mailbox)
                    .map(|rows| rows.len())
                    .unwrap_or(0)
            })
            .sum()
    }
}

/// The hit's payload, as the overlay holds it: an envelope and the two body
/// renditions, and no attachments, which is everything
/// `message.search_server` streams.
fn message() -> Value {
    json!({
        "from": "Ivana Hečimović <ivana@example.com>",
        "to": "alpha@example.com",
        "cc": "\"Prof. Petzold\" <petzold@example.com>",
        "subject": SUBJECT,
        "message_id": HIT,
        "date_display": "Mon, 29 Jun 2026 08:15:00 +0200",
        "body_text": "der ledger liegt bei\n",
        "html_body": "<html><body><p>der ledger liegt bei</p></body></html>",
    })
}

async fn within<F, T>(what: &str, future: F) -> T
where
    F: std::future::Future<Output = T>,
{
    match tokio::time::timeout(DEADLINE, future).await {
        Ok(value) => value,
        Err(_) => panic!("{what} did not answer within {DEADLINE:?}"),
    }
}

async fn call(conn: &mut Connection, method: &str, params: Value) -> Value {
    within(method, conn.call(method, params))
        .await
        .unwrap_or_else(|e| panic!("{method} failed: {e:?}"))
}

async fn call_err(conn: &mut Connection, method: &str, params: Value) -> RpcError {
    let error = within(method, conn.call(method, params))
        .await
        .expect_err("this call must be refused");
    match error {
        ClientError::Rpc(error) => error,
        other => panic!("{method}: expected a typed RPC error, got {other:?}"),
    }
}

/// The created draft, decoded, which is the same `DraftCreated` every other
/// writer of the family answers with.
async fn create(conn: &mut Connection, account: &str, kind: &str, message: Value) -> DraftCreated {
    let result = call(
        conn,
        METHOD,
        json!({"account": account, "kind": kind, "message": message}),
    )
    .await;
    serde_json::from_value(result).expect("the answer is a DraftCreated")
}

// ---------------------------------------------------------------------------
// 1. The method is registered
// ---------------------------------------------------------------------------

/// A method may not be served without being advertised: the handshake derives
/// the capability list from the dispatcher, so the name appearing there is what
/// says it is registered rather than merely documented.
///
/// **Fails at HEAD**: nothing registers the method, so it is in no capability
/// list.
#[tokio::test]
async fn the_daemon_advertises_the_draft_built_from_a_message() {
    let slice = Slice::start();
    let mut conn = within(
        "Connection::connect",
        Connection::connect(&socket_path(slice.root())),
    )
    .await
    .expect("connect");
    let result = within(
        "Connection::initialize",
        conn.initialize(
            ClientInfo {
                kind: ClientKind::Cli,
                app_version: env!("CARGO_PKG_VERSION").to_string(),
            },
            Identity {
                data_dir: slice.root().to_path_buf(),
                config_dir: slice.root().to_path_buf(),
            },
            &[],
            &[],
        ),
    )
    .await
    .expect("handshake");

    let capabilities: Vec<&str> = result.capabilities.iter().map(String::as_str).collect();
    assert!(
        capabilities.contains(&METHOD),
        "{METHOD} must be advertised beside the writers it joins, got {capabilities:?}"
    );
    for sibling in ["draft.create", "draft.reply", "draft.forward"] {
        assert!(
            capabilities.contains(&sibling),
            "{sibling} is the family {METHOD} joins and must still be there"
        );
    }
}

// ---------------------------------------------------------------------------
// 2. The draft the builder writes
// ---------------------------------------------------------------------------

/// A reply quotes the message the client holds: the sender becomes the
/// recipient, the subject is prefixed, the body carries the quoted text and the
/// frontmatter records what it answers.
///
/// **Fails at HEAD**: `-32601`, the method is not registered.
#[tokio::test]
async fn a_reply_quotes_the_message_the_client_holds() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let created = create(&mut conn, fixture::ACCOUNT, "reply", message()).await;
    assert_eq!(created.account, fixture::ACCOUNT);
    assert_eq!(
        created.source, None,
        "there is no stored message to name as the source"
    );
    assert_eq!(
        created.selector,
        format!("mp://{}/drafts/{}", fixture::ACCOUNT, created.id),
        "the selector names the id that was minted into the file"
    );

    let path = Path::new(&created.path);
    assert!(
        path.starts_with(fixture::drafts_dir(slice.root(), fixture::ACCOUNT)),
        "the draft lands in the account's own drafts directory, got {}",
        path.display()
    );
    let draft = parse_email_draft(path).expect("the written draft parses");
    assert_eq!(
        draft.frontmatter.id.as_deref(),
        Some(created.id.as_str()),
        "the id is in the file before the selector is handed out"
    );
    assert_eq!(
        draft.frontmatter.to.as_deref(),
        Some("ivana@example.com"),
        "a reply goes back to the sender the payload named, as the builder spells an address"
    );
    assert_eq!(
        draft.frontmatter.subject,
        format!("Re: {SUBJECT}"),
        "the builder's own prefixing, not a second one"
    );
    assert_eq!(
        draft.frontmatter.in_reply_to.as_deref(),
        Some(HIT),
        "the draft records the message it answers, so a send can be threaded"
    );
    assert!(
        draft.body_markdown.contains("der ledger liegt bei"),
        "the quoted body is the payload's own text, got {:?}",
        draft.body_markdown
    );
}

/// A reply-all keeps every other recipient, which is the difference the `kind`
/// carries.
///
/// **Fails at HEAD**: `-32601`, the method is not registered.
#[tokio::test]
async fn a_reply_all_keeps_the_other_recipients() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let created = create(&mut conn, fixture::ACCOUNT, "reply_all", message()).await;
    let draft = parse_email_draft(Path::new(&created.path)).expect("the written draft parses");
    let recipients = format!(
        "{} {}",
        draft.frontmatter.to.clone().unwrap_or_default(),
        draft.frontmatter.cc.clone().unwrap_or_default()
    );
    assert!(
        recipients.contains("petzold@example.com"),
        "a reply-all answers the Cc the payload carried, got {recipients:?}"
    );
}

/// A forward prefixes its subject the forward way, addresses nobody, and
/// carries no attachments: the client has none of the original parts, because
/// `message.search_server` streams an envelope and two body renditions.
///
/// **Fails at HEAD**: `-32601`, the method is not registered.
#[tokio::test]
async fn a_forward_names_no_recipient_and_carries_no_attachment() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let created = create(&mut conn, fixture::ACCOUNT, "forward", message()).await;
    let draft = parse_email_draft(Path::new(&created.path)).expect("the written draft parses");
    assert_eq!(
        draft.frontmatter.subject,
        format!("Fwd: {SUBJECT}"),
        "a forward is prefixed the forward way"
    );
    assert_eq!(
        draft.frontmatter.to.as_deref().unwrap_or_default(),
        "",
        "a forward is addressed by the user, not by the message it forwards"
    );
    assert_eq!(
        draft.frontmatter.attachments, None,
        "a server-only hit has no parts on the client's side to forward"
    );
    assert!(
        draft.body_markdown.contains("der ledger liegt bei"),
        "and the message itself is quoted, got {:?}",
        draft.body_markdown
    );
}

/// The draft is addressable the moment the selector is handed out: the index
/// refresh is the write's own business, not the next poll's (#0050).
///
/// **Fails at HEAD**: `-32601`, the method is not registered.
#[tokio::test]
async fn the_new_draft_resolves_through_the_family_s_resolver() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let created = create(&mut conn, fixture::ACCOUNT, "reply", message()).await;

    let located = call(
        &mut conn,
        "draft.path",
        json!({"account": fixture::ACCOUNT, "id": created.id}),
    )
    .await;
    assert_eq!(
        located["path"].as_str(),
        Some(created.path.as_str()),
        "the selector the answer printed resolves to the file it wrote"
    );
    assert_eq!(located["status"].as_str(), Some("draft"));

    let listing: DraftListing = serde_json::from_value(
        call(
            &mut conn,
            "draft.list",
            json!({"account": fixture::ACCOUNT, "status": null}),
        )
        .await,
    )
    .expect("the listing decodes");
    assert!(
        listing.drafts.iter().any(|row| row.id == created.id),
        "and the new draft is in the account's list"
    );
}

// ---------------------------------------------------------------------------
// 3. The four behaviours a composition through message.fetch would have lost
// ---------------------------------------------------------------------------

/// Nothing is ingested: no row appears, no count moves, and the hit stays
/// unknown to the store.
///
/// This is the behaviour that ruled out composing the draft out of
/// `message.fetch` and `draft.reply`, and it is the one an implementation is
/// most likely to lose.
///
/// **Fails at HEAD**: `-32601`, the method is not registered.
#[tokio::test]
async fn quoting_a_message_ingests_nothing() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;
    let before = slice.stored_rows(fixture::ACCOUNT);
    let counts_before = call(
        &mut conn,
        "mailbox.list",
        json!({"account": fixture::ACCOUNT}),
    )
    .await;

    create(&mut conn, fixture::ACCOUNT, "reply", message()).await;

    assert_eq!(
        slice.stored_rows(fixture::ACCOUNT),
        before,
        "quoting a message does not put it in the store"
    );
    let store = support::read_fixture::store(slice.root(), fixture::ACCOUNT);
    let rows = read::find_by_message_id(&store, fixture::ACCOUNT, HIT)
        .expect("looking the hit up in the store");
    assert!(
        rows.is_empty(),
        "the quoted message is still unknown to the store, got {} row(s)",
        rows.len()
    );
    drop(store);

    let counts_after = call(
        &mut conn,
        "mailbox.list",
        json!({"account": fixture::ACCOUNT}),
    )
    .await;
    let drafts_row = |answer: &Value| -> Value {
        answer["mailboxes"]
            .as_array()
            .expect("mailbox rows")
            .iter()
            .find(|row| row["role"] == json!("drafts"))
            .cloned()
            .expect("the account lists a Drafts mailbox")
    };
    for (before, after) in counts_before["mailboxes"]
        .as_array()
        .expect("mailbox rows")
        .iter()
        .zip(counts_after["mailboxes"].as_array().expect("mailbox rows"))
    {
        if before["role"] == json!("drafts") {
            continue;
        }
        assert_eq!(
            before, after,
            "no message mailbox moved: quoting is not downloading"
        );
    }
    assert_eq!(
        drafts_row(&counts_after)["total"].as_u64().unwrap_or(0),
        drafts_row(&counts_before)["total"].as_u64().unwrap_or(0) + 1,
        "the one count that moves is the Drafts one, by the draft that was written"
    );
}

/// A hit whose server returned no `Message-ID` still gets a draft, with no
/// `in_reply_to` recorded.
///
/// `message.fetch` refuses such a hit, which is the second reason the
/// composition was refused.
///
/// **Fails at HEAD**: `-32601`, the method is not registered.
#[tokio::test]
async fn a_message_without_an_id_is_still_quotable() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let mut payload = message();
    payload["message_id"] = Value::Null;
    let created = create(&mut conn, fixture::ACCOUNT, "reply", payload).await;

    let draft = parse_email_draft(Path::new(&created.path)).expect("the written draft parses");
    assert_eq!(
        draft.frontmatter.in_reply_to, None,
        "there is no id to record, and the draft is written anyway"
    );
    assert_eq!(draft.frontmatter.subject, format!("Re: {SUBJECT}"));
}

/// An account with no store quotes too: this method reads no message store,
/// and it does not give the account one.
///
/// The second half is the rule P5-U10 fixed for the whole daemon: materialising
/// an empty database would turn the read family's `-32006` into empty answers
/// for an account the user has not set up.
///
/// **Fails at HEAD**: `-32601`, the method is not registered.
#[tokio::test]
async fn an_account_with_no_store_can_still_quote_a_message() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let created = create(&mut conn, fixture::STORELESS_ACCOUNT, "reply", message()).await;
    assert_eq!(created.account, fixture::STORELESS_ACCOUNT);
    assert!(
        Path::new(&created.path).exists(),
        "the draft file is on disk at {}",
        created.path
    );

    let accounts = call(&mut conn, "account.list", json!({})).await;
    let state = accounts["accounts"]
        .as_array()
        .expect("account rows")
        .iter()
        .find(|row| row["name"] == json!(fixture::STORELESS_ACCOUNT))
        .map(|row| row["state"].clone())
        .expect("the storeless account is configured");
    assert_eq!(
        state,
        json!("blocked"),
        "writing a draft may not hand an unsynced account a store"
    );
}

// ---------------------------------------------------------------------------
// 4. Refusals
// ---------------------------------------------------------------------------

/// The parameters are validated: a kind outside the three words, a missing
/// message, and a message that is not an object.
///
/// **Fails at HEAD**: `-32601`, where `-32602` is owed.
#[tokio::test]
async fn the_parameters_are_validated() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    for params in [
        json!({"account": fixture::ACCOUNT, "kind": "bounce", "message": message()}),
        json!({"account": fixture::ACCOUNT, "message": message()}),
        json!({"account": fixture::ACCOUNT, "kind": "reply"}),
        json!({"account": fixture::ACCOUNT, "kind": "reply", "message": "a string"}),
    ] {
        let error = call_err(&mut conn, METHOD, params.clone()).await;
        assert_eq!(
            error.code, -32602,
            "{params} is a parameter error, got {error:?}"
        );
    }
}

/// An unknown account is the family's `-32005`, and an account with no store is
/// deliberately **not** refused, which is what the row above asserts from the
/// other side.
///
/// **Fails at HEAD**: `-32601`, where `-32005` is owed.
#[tokio::test]
async fn an_unknown_account_is_refused_and_a_storeless_one_is_not() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;

    let error = call_err(
        &mut conn,
        METHOD,
        json!({"account": fixture::UNKNOWN_ACCOUNT, "kind": "reply", "message": message()}),
    )
    .await;
    assert_eq!(
        error.code,
        ErrorCode::AccountUnknown.code(),
        "an account no configuration names is -32005, got {error:?}"
    );
    assert_ne!(
        error.code,
        ErrorCode::AccountNotReady.code(),
        "and the store's absence is not what makes it a refusal"
    );
}
