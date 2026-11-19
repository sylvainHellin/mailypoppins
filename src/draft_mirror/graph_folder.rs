//! The server Drafts folder of a Graph account (#0143 slice 3, PERSO-182):
//! the [`DraftsFolder`] the push, the reconcile and the send guard run
//! against when the account uses Microsoft Graph, so they run unchanged.
//!
//! A Graph draft is an immutable id and a `changeKey`, which every edit
//! changes, Outlook's edits in place included. The folder names each
//! `(id, changeKey)` it sees by a key from the store's
//! [`graph_draft_versions`] table, minted in increasing order, so a key
//! behaves as the trait's UIDs do: a new copy or an edit anywhere is a new,
//! higher key, and a version the folder no longer holds is a key no listing
//! shows. The UIDVALIDITY is [`GRAPH_UIDVALIDITY`], since the keys never
//! restart while the table lives, and a store rebuild drops the table with
//! the `draft_sync` rows that name its keys.
//!
//! - A push creates a new copy from MIME and removes the old one for good
//!   (`permanentDelete`), as an IMAP push APPENDs and expunges: the copy
//!   carries the draft file in `X-Mailypoppins-Draft-Source` headers, which
//!   Graph sets only when it creates an item, so a `PATCH` would leave a
//!   patched copy carrying the previous file.
//! - A claim is a `permanentDelete` this session's request answered: a copy
//!   another session removed first answers 404 and is not claimed.
//! - The listing leaves out a draft a Graph staged send created, which
//!   carries an outbox row's `Message-ID`, so a message on its way out is
//!   never imported as a draft.
//!
//! The requests themselves are [`crate::graph::drafts`], behind
//! [`GraphDraftsApi`] so the mirror runs offline against [`fake::FakeGraph`].

use std::future::Future;

use anyhow::{anyhow, Context, Result};
use log::warn;

use super::folder::{bare_message_id, header_value, DraftsFolder, FolderInfo, ServerHead};
use crate::config::GraphConfig;
use crate::graph::drafts::GraphDraftHead;
use crate::graph::GraphClient;
use crate::store::{graph_draft_versions, Store};

/// The UIDVALIDITY of a Graph Drafts folder: the keys of
/// [`graph_draft_versions`] are never minted twice, so it never changes.
pub const GRAPH_UIDVALIDITY: u32 = 1;

/// The Graph requests the folder runs on: [`LiveGraphDrafts`] in the daemon,
/// [`fake::FakeGraph`] in the tests.
pub trait GraphDraftsApi {
    /// Every draft of `folder`.
    fn list(&mut self, folder: &str) -> impl Future<Output = Result<Vec<GraphDraftHead>>>;

    /// The drafts of `folder` carrying `message_id`.
    fn find(
        &mut self,
        folder: &str,
        message_id: &str,
    ) -> impl Future<Output = Result<Vec<GraphDraftHead>>>;

    /// Create a draft in Drafts from the MIME message `raw`.
    fn create(&mut self, raw: &[u8]) -> impl Future<Output = Result<GraphDraftHead>>;

    /// The MIME content of the draft `id`, `None` when it is gone.
    fn mime(&mut self, id: &str) -> impl Future<Output = Result<Option<Vec<u8>>>>;

    /// Remove the draft `id` for good: `true` when this call removed it,
    /// `false` when it was gone already.
    fn remove(&mut self, id: &str) -> impl Future<Output = Result<bool>>;
}

/// The requests over the account's Graph client, signed in on first use
/// through [`GraphClient::new_async`], which asks the daemon's list of
/// refused logins first ([`crate::login_gate`]): a refused token fails every
/// call at once, without asking Entra again.
pub struct LiveGraphDrafts {
    config: GraphConfig,
    client: Option<GraphClient>,
}

impl LiveGraphDrafts {
    pub fn new(config: GraphConfig) -> LiveGraphDrafts {
        LiveGraphDrafts {
            config,
            client: None,
        }
    }

    async fn client(&mut self) -> Result<&GraphClient> {
        if self.client.is_none() {
            self.client = Some(GraphClient::new_async(&self.config).await?);
        }
        Ok(self.client.as_ref().expect("signed in just now"))
    }
}

impl GraphDraftsApi for LiveGraphDrafts {
    async fn list(&mut self, folder: &str) -> Result<Vec<GraphDraftHead>> {
        self.client().await?.list_drafts(folder).await
    }

    async fn find(&mut self, folder: &str, message_id: &str) -> Result<Vec<GraphDraftHead>> {
        self.client().await?.find_drafts(folder, message_id).await
    }

    async fn create(&mut self, raw: &[u8]) -> Result<GraphDraftHead> {
        self.client().await?.create_draft_copy(raw).await
    }

    async fn mime(&mut self, id: &str) -> Result<Option<Vec<u8>>> {
        self.client().await?.draft_mime(id).await
    }

    async fn remove(&mut self, id: &str) -> Result<bool> {
        self.client().await?.remove_draft(id).await
    }
}

/// The Drafts folder of one Graph account, over `api`, keyed in `store`.
pub struct GraphDraftsFolder<'s, A> {
    store: &'s Store,
    account: String,
    api: A,
    folder: Option<String>,
}

impl<'s, A: GraphDraftsApi> GraphDraftsFolder<'s, A> {
    pub fn new(store: &'s Store, account: &str, api: A) -> GraphDraftsFolder<'s, A> {
        GraphDraftsFolder {
            store,
            account: account.to_string(),
            api,
            folder: None,
        }
    }

    /// The requests, for a test that looks at what the folder asked.
    #[cfg(test)]
    pub fn api(&mut self) -> &mut A {
        &mut self.api
    }

    fn folder(&self) -> Result<String> {
        self.folder
            .clone()
            .ok_or_else(|| anyhow!("the Graph drafts folder was used before it was selected"))
    }

    fn key(&self, head: &GraphDraftHead) -> Result<u32> {
        graph_draft_versions::key_of(self.store, &self.account, &head.id, &head.change_key)
    }

    /// The keys of `heads`, minted in the order the drafts were last
    /// modified, so of two versions first seen together the later edit gets
    /// the higher key; ascending.
    fn keys(&self, mut heads: Vec<GraphDraftHead>) -> Result<Vec<(u32, GraphDraftHead)>> {
        heads.sort_by(|a, b| {
            (a.last_modified.as_str(), a.id.as_str())
                .cmp(&(b.last_modified.as_str(), b.id.as_str()))
        });
        let mut keyed = heads
            .into_iter()
            .map(|head| Ok((self.key(&head)?, head)))
            .collect::<Result<Vec<_>>>()?;
        keyed.sort_by_key(|(key, _)| *key);
        Ok(keyed)
    }

    /// The draft id `key` names, when it names the version the folder saw
    /// last; `None` for a version an edit has superseded, which is gone from
    /// the folder as an expunged UID is.
    fn current(&self, key: u32) -> Result<Option<String>> {
        let Some(version) = graph_draft_versions::version(self.store, &self.account, key)? else {
            return Ok(None);
        };
        let latest =
            graph_draft_versions::latest_key(self.store, &self.account, &version.graph_id)?;
        Ok((latest == Some(key)).then_some(version.graph_id))
    }

    /// Whether `message_id` is a message a Graph send of this account has
    /// queued, whose staged draft sits in Drafts until `/send` moves it.
    fn staged_send(&self, message_id: &str) -> Result<bool> {
        let bare = bare_message_id(message_id);
        for spelling in [format!("<{bare}>"), bare.to_string()] {
            if crate::outbox::load_by_message_id(self.store, &self.account, &spelling)?.is_some() {
                return Ok(true);
            }
        }
        Ok(crate::outbox::server_cleanups(self.store, &self.account)?
            .iter()
            .any(|cleanup| bare_message_id(&cleanup.message_id) == bare))
    }
}

/// A header value on one line.
fn one_line(value: &str) -> String {
    value.replace(['\r', '\n'], " ").trim().to_string()
}

/// `text` as a header value: as it is when it is ASCII, an RFC 2047 encoded
/// word otherwise, which the reconcile's header parse decodes.
fn encoded(text: &str) -> String {
    let text = one_line(text);
    if text.is_ascii() {
        return text;
    }
    use base64::Engine;
    format!(
        "=?UTF-8?B?{}?=",
        base64::engine::general_purpose::STANDARD.encode(text.as_bytes())
    )
}

/// The header block a listing stands for: the fields of
/// [`super::folder::HEAD_FIELDS`] Graph lists, each when it has a value. The
/// draft-id marker is not among them: reading it costs a draft's whole
/// `internetMessageHeaders`, and the reconcile matches a copy by its
/// `Message-ID` first.
fn header_block(head: &GraphDraftHead) -> Vec<u8> {
    let mut block = String::new();
    let mut field = |name: &str, value: String| {
        if !value.is_empty() {
            block.push_str(&format!("{name}: {value}\r\n"));
        }
    };
    field(
        "Message-ID",
        head.internet_message_id
            .as_deref()
            .map(one_line)
            .unwrap_or_default(),
    );
    field("Subject", encoded(&head.subject));
    field("To", one_line(&head.to.join(", ")));
    field("Cc", one_line(&head.cc.join(", ")));
    field(
        "In-Reply-To",
        head.in_reply_to
            .as_deref()
            .map(one_line)
            .unwrap_or_default(),
    );
    block.push_str("\r\n");
    block.into_bytes()
}

impl<A: GraphDraftsApi> DraftsFolder for GraphDraftsFolder<'_, A> {
    /// Name the folder; nothing is asked of Graph until a call needs it.
    async fn select(&mut self, mailbox: &str) -> Result<FolderInfo> {
        self.folder = Some(mailbox.to_string());
        Ok(FolderInfo {
            uidvalidity: GRAPH_UIDVALIDITY,
            uidplus: true,
        })
    }

    /// Create the copy and key it. Graph keeps the `Message-ID` of a message
    /// created from MIME, which is what links the copy to its draft; a copy
    /// it gave one of its own could never be found again, so it is removed
    /// and the push fails.
    async fn append(&mut self, raw: &[u8]) -> Result<Option<u32>> {
        let created = self.api.create(raw).await?;
        let wanted = header_value(raw, "Message-ID").unwrap_or_default();
        let kept = created
            .internet_message_id
            .as_deref()
            .is_some_and(|mid| bare_message_id(mid) == bare_message_id(&wanted));
        if !kept {
            if let Err(e) = self.api.remove(&created.id).await {
                warn!(
                    "[drafts] could not remove the Graph copy {} that lost its Message-ID: {e:#}",
                    created.id
                );
            }
            return Err(anyhow!(
                "Graph gave the draft copy {wanted} the Message-ID {}, so the mirror could not \
                 find it again; the copy was removed",
                created.internet_message_id.as_deref().unwrap_or("(none)")
            ));
        }
        Ok(Some(self.key(&created)?))
    }

    async fn uids_of(&mut self, message_id: &str) -> Result<Vec<u32>> {
        let folder = self.folder()?;
        let wanted = bare_message_id(message_id).to_string();
        let found = self.api.find(&folder, message_id).await?;
        // `$filter` compares as Graph does; the exact id is compared here.
        let found = found
            .into_iter()
            .filter(|head| {
                head.internet_message_id
                    .as_deref()
                    .is_some_and(|mid| bare_message_id(mid) == wanted)
            })
            .collect();
        Ok(self.keys(found)?.into_iter().map(|(key, _)| key).collect())
    }

    /// Remove the copies `keys` names; a key an edit superseded, or a copy
    /// already gone, is removed already.
    async fn delete(&mut self, keys: &[u32]) -> Result<()> {
        for &key in keys {
            if let Some(id) = self.current(key)? {
                self.api.remove(&id).await?;
            }
        }
        Ok(())
    }

    /// Remove the copies `keys` names, answering those this session's
    /// `permanentDelete` removed. A copy another session removed first, or
    /// an edit superseded since it was found, is not claimed.
    async fn claim(&mut self, keys: &[u32]) -> Result<Vec<u32>> {
        let mut claimed = Vec::new();
        for &key in keys {
            let Some(id) = self.current(key)? else {
                continue;
            };
            if self.api.remove(&id).await? {
                claimed.push(key);
            }
        }
        claimed.sort_unstable();
        Ok(claimed)
    }

    /// Every draft of the folder, keyed, by the headers a listing carries;
    /// then every version no longer listed is forgotten. A staged send's
    /// draft is left out.
    async fn heads(&mut self) -> Result<Vec<ServerHead>> {
        let folder = self.folder()?;
        let listed = self.api.list(&folder).await?;
        let keyed = self.keys(listed)?;
        let all: Vec<u32> = keyed.iter().map(|(key, _)| *key).collect();
        graph_draft_versions::prune(self.store, &self.account, &all)
            .context("forgetting the Graph draft versions no longer listed")?;
        let mut heads = Vec::new();
        for (key, head) in keyed {
            if let Some(mid) = &head.internet_message_id {
                if self.staged_send(mid)? {
                    continue;
                }
            }
            heads.push(ServerHead {
                uid: key,
                header: header_block(&head),
            });
        }
        Ok(heads)
    }

    async fn fetch(&mut self, key: u32) -> Result<Option<Vec<u8>>> {
        match self.current(key)? {
            Some(id) => self.api.mime(&id).await,
            None => Ok(None),
        }
    }
}

/// An in-memory Graph mailbox's Drafts folder for the mirror's tests: drafts
/// created from MIME, edited in place, removed, and switches that make it
/// behave as the cases need.
#[cfg(test)]
pub mod fake {
    use super::*;

    /// One item of the fake folder.
    #[derive(Clone, Debug)]
    pub struct Item {
        pub id: String,
        /// Bumped by every edit.
        pub version: u32,
        pub raw: Vec<u8>,
        pub is_draft: bool,
        /// When it was last modified, on the fake's clock.
        pub modified: u32,
    }

    #[derive(Debug, Default)]
    pub struct FakeGraph {
        pub items: Vec<Item>,
        next_id: u32,
        clock: u32,
        /// `create`, `remove <id>`, in order.
        pub log: Vec<String>,
        /// Requests made, of any kind.
        pub requests: usize,
        /// Graph gives a created draft a `Message-ID` of its own.
        pub rewrites_message_id: bool,
        /// Another session removes what a `remove` names just before it: mp
        /// on the other machine claiming the same copy.
        pub removed_elsewhere: bool,
        /// Every call fails as the login gate fails a refused token.
        pub refused: bool,
        /// Every call fails as an unreachable server does.
        pub unreachable: bool,
    }

    impl FakeGraph {
        fn tick(&mut self) -> u32 {
            self.clock += 1;
            self.clock
        }

        fn check(&mut self) -> Result<()> {
            if self.refused {
                return Err(crate::login_gate::LoginRejected::new(
                    mp_protocol::account::RejectedLogin::Graph,
                    "invalid_grant: the refresh token was revoked".to_string(),
                )
                .into());
            }
            if self.unreachable {
                return Err(anyhow!("graph.microsoft.com did not answer"));
            }
            self.requests += 1;
            Ok(())
        }

        /// Put a draft in the folder as Outlook would, returning its id.
        pub fn put(&mut self, raw: &[u8]) -> String {
            self.next_id += 1;
            let id = format!("AAMk-{}", self.next_id);
            let modified = self.tick();
            self.items.push(Item {
                id: id.clone(),
                version: 1,
                raw: raw.to_vec(),
                is_draft: true,
                modified,
            });
            id
        }

        /// Edit the draft `id` in place, as Outlook does: same id, new
        /// version.
        pub fn edit(&mut self, id: &str, raw: &[u8]) {
            let modified = self.tick();
            let item = self
                .items
                .iter_mut()
                .find(|item| item.id == id)
                .expect("the draft to edit");
            item.raw = raw.to_vec();
            item.version += 1;
            item.modified = modified;
        }

        /// Remove the draft `id`, as a send or a delete in Outlook does.
        pub fn drop_item(&mut self, id: &str) {
            self.items.retain(|item| item.id != id);
        }

        /// The drafts.
        pub fn drafts(&self) -> Vec<&Item> {
            self.items.iter().filter(|item| item.is_draft).collect()
        }

        fn head(item: &Item) -> GraphDraftHead {
            let addresses = |name: &str| -> Vec<String> {
                header_value(&item.raw, name)
                    .and_then(|value| mailparse::addrparse(&value).ok())
                    .map(|list| {
                        list.iter()
                            .filter_map(|addr| match addr {
                                mailparse::MailAddr::Single(info) => Some(info.addr.clone()),
                                mailparse::MailAddr::Group(_) => None,
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            };
            GraphDraftHead {
                id: item.id.clone(),
                change_key: format!("ck{}", item.version),
                internet_message_id: header_value(&item.raw, "Message-ID"),
                subject: header_value(&item.raw, "Subject").unwrap_or_default(),
                to: addresses("To"),
                cc: addresses("Cc"),
                in_reply_to: header_value(&item.raw, "In-Reply-To"),
                last_modified: format!("2026-11-19T00:00:{:06}Z", item.modified),
                is_draft: item.is_draft,
            }
        }
    }

    /// One fake mailbox for two machines' folders, each with its own store.
    impl<A: GraphDraftsApi> GraphDraftsApi for &mut A {
        async fn list(&mut self, folder: &str) -> Result<Vec<GraphDraftHead>> {
            (**self).list(folder).await
        }

        async fn find(&mut self, folder: &str, message_id: &str) -> Result<Vec<GraphDraftHead>> {
            (**self).find(folder, message_id).await
        }

        async fn create(&mut self, raw: &[u8]) -> Result<GraphDraftHead> {
            (**self).create(raw).await
        }

        async fn mime(&mut self, id: &str) -> Result<Option<Vec<u8>>> {
            (**self).mime(id).await
        }

        async fn remove(&mut self, id: &str) -> Result<bool> {
            (**self).remove(id).await
        }
    }

    impl GraphDraftsApi for FakeGraph {
        async fn list(&mut self, folder: &str) -> Result<Vec<GraphDraftHead>> {
            self.check()?;
            assert_eq!(folder, crate::graph::drafts::DRAFTS_FOLDER);
            Ok(self.drafts().into_iter().map(Self::head).collect())
        }

        async fn find(&mut self, folder: &str, message_id: &str) -> Result<Vec<GraphDraftHead>> {
            self.check()?;
            assert_eq!(folder, crate::graph::drafts::DRAFTS_FOLDER);
            let wanted = bare_message_id(message_id).to_string();
            Ok(self
                .drafts()
                .into_iter()
                .map(Self::head)
                .filter(|head| {
                    head.internet_message_id
                        .as_deref()
                        .is_some_and(|mid| bare_message_id(mid) == wanted)
                })
                .collect())
        }

        async fn create(&mut self, raw: &[u8]) -> Result<GraphDraftHead> {
            self.check()?;
            self.log.push("create".to_string());
            let raw = if self.rewrites_message_id {
                let text = String::from_utf8_lossy(raw);
                let mid = header_value(raw, "Message-ID").unwrap_or_default();
                text.replacen(&mid, "<exchange.1@eurprd.outlook.com>", 1)
                    .into_bytes()
            } else {
                raw.to_vec()
            };
            let id = self.put(&raw);
            let item = self.items.iter().find(|item| item.id == id).unwrap();
            Ok(Self::head(item))
        }

        async fn mime(&mut self, id: &str) -> Result<Option<Vec<u8>>> {
            self.check()?;
            Ok(self
                .items
                .iter()
                .find(|item| item.id == id)
                .map(|item| item.raw.clone()))
        }

        async fn remove(&mut self, id: &str) -> Result<bool> {
            self.check()?;
            self.log.push(format!("remove {id}"));
            let present = self.items.iter().any(|item| item.id == id);
            self.drop_item(id);
            Ok(present && !self.removed_elsewhere)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeGraph;
    use super::*;

    fn block_on<F: Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(future)
    }

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("store.sqlite3")).unwrap();
        (dir, store)
    }

    fn message(mid: &str, subject: &str, body: &str) -> Vec<u8> {
        format!(
            "Message-ID: {mid}\r\nSubject: {subject}\r\nTo: Robin <robin@example.com>, \
             kim@example.com\r\nCc: chef@example.com\r\nIn-Reply-To: <parent@example.com>\r\n\
             MIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{body}\r\n"
        )
        .into_bytes()
    }

    /// A listing stands for the linking headers of each draft, a subject
    /// outside ASCII included, under keys that follow the drafts' order of
    /// modification; an edit in place is a new, higher key and the old one
    /// is gone from the folder.
    #[test]
    fn a_listing_keys_each_version_and_carries_the_linking_headers() {
        let (_dir, store) = store();
        let mut graph = FakeGraph::default();
        let first = graph.put(&message("<a@x>", "=?UTF-8?B?w4RuZGVydW5n?=", "eins"));
        graph.put(&message("<b@x>", "Angebot", "zwei"));
        let mut folder = GraphDraftsFolder::new(&store, "evoqs", graph);
        block_on(folder.select("drafts")).unwrap();
        let heads = block_on(folder.heads()).unwrap();
        assert_eq!(heads.len(), 2);
        assert!(heads[0].uid < heads[1].uid);
        let header = &heads[0].header;
        assert_eq!(header_value(header, "Message-ID").as_deref(), Some("<a@x>"));
        assert_eq!(header_value(header, "Subject").as_deref(), Some("Änderung"));
        assert_eq!(
            header_value(header, "To").as_deref(),
            Some("robin@example.com, kim@example.com")
        );
        assert_eq!(
            header_value(header, "Cc").as_deref(),
            Some("chef@example.com")
        );
        assert_eq!(
            header_value(header, "In-Reply-To").as_deref(),
            Some("<parent@example.com>")
        );
        let old = heads[0].uid;

        folder
            .api()
            .edit(&first, &message("<a@x>", "Änderung", "eins, bearbeitet"));
        let again = block_on(folder.heads()).unwrap();
        let edited = again
            .iter()
            .find(|h| header_value(&h.header, "Message-ID").as_deref() == Some("<a@x>"))
            .unwrap()
            .uid;
        assert!(edited > heads[1].uid, "an edit is the newest key");
        assert_eq!(block_on(folder.fetch(old)).unwrap(), None);
        assert!(
            String::from_utf8_lossy(&block_on(folder.fetch(edited)).unwrap().unwrap())
                .contains("bearbeitet")
        );
        assert_eq!(block_on(folder.uids_of("<a@x>")).unwrap(), vec![edited]);
    }

    /// A created copy is keyed and found under its `Message-ID`; a copy
    /// Graph gave a `Message-ID` of its own is removed and the push fails.
    #[test]
    fn a_created_copy_keeps_its_message_id_or_is_removed() {
        let (_dir, store) = store();
        let mut folder = GraphDraftsFolder::new(&store, "evoqs", FakeGraph::default());
        block_on(folder.select("drafts")).unwrap();
        let key = block_on(folder.append(&message("<mp-draft.1@evoqs.de>", "A", "x")))
            .unwrap()
            .expect("a key");
        assert_eq!(
            block_on(folder.uids_of("mp-draft.1@evoqs.de")).unwrap(),
            vec![key]
        );

        folder.api().rewrites_message_id = true;
        let error =
            block_on(folder.append(&message("<mp-draft.2@evoqs.de>", "B", "y"))).unwrap_err();
        assert!(
            error.to_string().contains("could not find it again"),
            "{error}"
        );
        assert_eq!(folder.api().drafts().len(), 1, "the stray copy went");
    }

    /// A claim answers the copies this session removed; one another session
    /// removed first, or one an edit superseded, is not claimed. A delete of
    /// a superseded version leaves the edit alone.
    #[test]
    fn a_claim_is_a_removal_this_session_made() {
        let (_dir, store) = store();
        let mut graph = FakeGraph::default();
        let id = graph.put(&message("<a@x>", "A", "x"));
        graph.put(&message("<b@x>", "B", "y"));
        let mut folder = GraphDraftsFolder::new(&store, "evoqs", graph);
        block_on(folder.select("drafts")).unwrap();
        let a = block_on(folder.uids_of("<a@x>")).unwrap();
        let b = block_on(folder.uids_of("<b@x>")).unwrap();

        folder.api().edit(&id, &message("<a@x>", "A", "edited"));
        assert_eq!(block_on(folder.uids_of("<a@x>")).unwrap().len(), 1);
        assert!(block_on(folder.claim(&a)).unwrap().is_empty());
        block_on(folder.delete(&a)).unwrap();
        assert_eq!(folder.api().drafts().len(), 2, "the edit stays");

        folder.api().removed_elsewhere = true;
        assert!(block_on(folder.claim(&b)).unwrap().is_empty());
        folder.api().removed_elsewhere = false;
        let a2 = block_on(folder.uids_of("<a@x>")).unwrap();
        assert_eq!(block_on(folder.claim(&a2)).unwrap(), a2);
        assert!(folder.api().drafts().is_empty());
    }

    /// A draft a Graph staged send created, under the outbox row's
    /// `Message-ID`, is not listed, so the reconcile never imports it.
    #[test]
    fn a_staged_send_draft_is_not_listed() {
        let (dir, store) = store();
        let blobs = crate::store::BlobStore::new(dir.path().join("blobs"));
        crate::outbox::enqueue(
            &store,
            &blobs,
            "evoqs",
            None,
            "<send.1@evoqs.de>",
            b"Message-ID: <send.1@evoqs.de>\r\n\r\nx",
            &crate::outbox::Envelope::default(),
        )
        .unwrap();
        let mut graph = FakeGraph::default();
        graph.put(&message("<send.1@evoqs.de>", "Big", "x"));
        graph.put(&message("<outlook.1@evoqs.de>", "Mine", "y"));
        let mut folder = GraphDraftsFolder::new(&store, "evoqs", graph);
        block_on(folder.select("drafts")).unwrap();
        let heads = block_on(folder.heads()).unwrap();
        assert_eq!(heads.len(), 1);
        assert_eq!(
            header_value(&heads[0].header, "Message-ID").as_deref(),
            Some("<outlook.1@evoqs.de>")
        );
    }

    /// The live requests sign in through the login gate: with a refusal held
    /// for the account, every call fails at once with that refusal, and no
    /// token is asked for.
    #[test]
    fn the_live_requests_go_through_the_login_gate() {
        let _data = crate::config::test_env::TestDataDir::new();
        let account = "gated-drafts";
        let login = crate::login_gate::Login::Graph(account.to_string());
        let rejected = crate::login_gate::LoginRejected::new(
            mp_protocol::account::RejectedLogin::Graph,
            "invalid_grant".to_string(),
        );
        crate::login_gate::record(&login, "", &rejected);
        let mut live = LiveGraphDrafts::new(GraphConfig {
            client_id: "client".to_string(),
            tenant_id: "tenant".to_string(),
            username: "me@evoqs.de".to_string(),
            account_name: account.to_string(),
            scopes: String::new(),
        });
        let error = block_on(live.list(crate::graph::drafts::DRAFTS_FOLDER)).unwrap_err();
        assert!(
            crate::login_gate::login_rejected(&error).is_some(),
            "{error:#}"
        );
        assert!(live.client.is_none());
        crate::login_gate::forget(&login);
    }
}
