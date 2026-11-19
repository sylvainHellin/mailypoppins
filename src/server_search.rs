//! The two passes of a server search over several mailboxes (PERSO-175).
//!
//! `limit` is the number of hits the whole search answers with, not a slice of
//! it per mailbox. Splitting it dropped a busy mailbox's newest hits for an
//! idle one's older ones, and asking every mailbox for `limit` whole messages
//! downloaded as many times the answer as there are mailboxes. So the search
//! runs in two passes:
//!
//! 1. Each mailbox is asked for its newest `limit` matches as
//!    [`SearchStub`]s, or its oldest under [`SearchOrder::Oldest`]
//!    (PERSO-273): the key the second pass downloads by, the `Date` the
//!    merge orders by and the `Message-ID` the caller filters on.
//! 2. Every mailbox's stubs are merged in that order and cut to `limit`
//!    ([`select`]), and only the survivors are downloaded whole, mailbox by
//!    mailbox.
//!
//! A mail server gives no relevance score, so [`SearchOrder::Relevance`] runs
//! as [`SearchOrder::Newest`] here ([`SearchOrder::on_server`]).
//!
//! So a search downloads at most `limit` messages however many mailboxes it
//! spans, and it knows when the answer is cut: a mailbox matched more than it
//! listed, or the merge dropped stubs.
//!
//! A survivor the second pass cannot download (Graph throttled it past its
//! retries, the message moved or was expunged between the passes) is not
//! dropped without a word: the mailbox is reported as having fallen short,
//! with how many and why, beside the hits that did land.
//!
//! Graph can also search every folder of the account in one query
//! ([`Scope::EveryFolder`]), which is what a search that names no mailbox
//! does there: one request rather than one per configured folder, and mail
//! in a folder the account does not configure (an Archive subfolder) still
//! matches. Each of its stubs then carries the label of the folder it sits
//! in, which is what its hit is listed under.
//!
//! A mailbox whose server refuses the search for its charset, as TUM's
//! Exchange answers `CHARSET UTF-8` with `NO [BADCHARSET]`, is searched in
//! the local store instead ([`StoreFallback`], PERSO-214): its stubs are the
//! stored rows that match, they compete in the same merge, and the sink is
//! told the store answered that mailbox and why, so the answer never reads
//! as "nothing found" while the mail sits in the store.
//!
//! The backend is a [`SearchBackend`]: one IMAP session ([`ImapSearch`]) or
//! one Graph client ([`GraphSearch`]). What the hits become is the caller's
//! [`SearchSink`], which is where the daemon's dedup, attachment post-filter
//! and event stream live (`src/daemon/methods/message_server.rs`).

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;

use anyhow::Result;

use crate::graph::GraphClient;
use crate::imap_client::ImapSession;
use crate::parse::FetchedEmail;
pub use mp_protocol::listing::SearchOrder;

/// One hit of a search's first pass: what the merge across mailboxes reads,
/// and the key the second pass downloads the message by.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchStub<K> {
    /// The UID (IMAP) or the message id (Graph) the second pass fetches.
    pub key: K,
    /// The `Date` header, parsed as the downloaded message's is, so the merge
    /// orders the stubs the way the hits sort.
    pub date: String,
    /// The `Message-ID` header, which the dedup and the attachment
    /// post-filter read.
    pub message_id: Option<String>,
    /// The folder the hit sits in, when the search spanned every folder at
    /// once ([`Scope::EveryFolder`]): Graph's `parentFolderId` as
    /// [`GraphClient::search_stubs`] reads it, and the label the hit is
    /// listed under once [`GraphSearch`] has resolved it. `None` when the
    /// search ran in one mailbox, whose own label the hit is listed under.
    pub folder: Option<String>,
}

/// Where one first-pass search runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scope {
    /// One mailbox, by its server name.
    Mailbox(String),
    /// Every folder of the account in one query, which only Graph answers
    /// (`/me/messages`); each stub names the folder it sits in.
    EveryFolder,
}

/// One mailbox's first-pass answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MailboxStubs<K> {
    /// Its newest matches, or its oldest, at most the limit it was asked
    /// for.
    pub stubs: Vec<SearchStub<K>>,
    /// The server matched more than `stubs` holds.
    pub more: bool,
    /// Microsoft Graph's `$search` cap stopped it short of the limit.
    pub capped: bool,
    /// Why the local store answered in the server's stead, when it did
    /// ([`StoreFallback`], PERSO-214); `None` for the server's own answer.
    pub local: Option<String>,
}

/// The stubs of the scope at an index, as the merge takes and hands them
/// back.
pub type StubGroup<K> = (usize, Vec<SearchStub<K>>);

/// What a second pass brought back.
#[derive(Debug)]
pub struct Fetched<K> {
    /// Each downloaded message, with the key it was asked for by.
    pub emails: Vec<(K, FetchedEmail)>,
    /// Each key that did not come back, with why.
    pub missing: Vec<(K, String)>,
}

impl<K> Default for Fetched<K> {
    fn default() -> Self {
        Fetched {
            emails: Vec::new(),
            missing: Vec::new(),
        }
    }
}

/// Whether the answer is cut, and by what.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cut {
    /// More matched than the hits hold.
    pub truncated: bool,
    /// Graph's cap stopped a mailbox, which no larger limit gets past.
    pub capped: bool,
}

/// One server a search runs against.
pub trait SearchBackend {
    /// What a stub names its message by.
    type Key: Clone + Eq + std::hash::Hash + Send + Sync;

    /// The newest `limit` matches in `scope` as stubs, or the oldest when
    /// `order` is [`SearchOrder::Oldest`]; any other order reads as newest.
    fn stubs(
        &mut self,
        scope: &Scope,
        limit: usize,
        order: SearchOrder,
    ) -> impl Future<Output = Result<MailboxStubs<Self::Key>>> + Send;

    /// The whole messages behind `keys` in `scope`, and the keys that did
    /// not come back. An error is the whole download failing.
    fn fetch(
        &mut self,
        scope: &Scope,
        keys: &[Self::Key],
    ) -> impl Future<Output = Result<Fetched<Self::Key>>> + Send;
}

/// What the caller does with a search's hits.
pub trait SearchSink {
    /// Whether a first-pass hit carrying `message_id` competes for the limit:
    /// the caller's dedup and its store post-filter run here, before anything
    /// is downloaded.
    fn admit(&mut self, message_id: Option<&str>) -> bool;
    /// The downloaded hits of the `mailbox`-th scope, sitting in `folder`
    /// when the scope spanned every folder ([`SearchStub::folder`]).
    fn land(&mut self, mailbox: usize, folder: Option<&str>, emails: Vec<FetchedEmail>);
    /// The `mailbox`-th mailbox failed, or some of its hits could not be
    /// downloaded, which does not fail the search.
    fn refused(&mut self, mailbox: usize, error: &anyhow::Error);
    /// The local store searched the `mailbox`-th mailbox in the server's
    /// stead, for `reason` ([`MailboxStubs::local`], PERSO-214).
    fn answered_locally(&mut self, mailbox: usize, reason: &str);
}

/// Search `scopes` for the first `limit` hits in `order` across all of them
/// (newest, or oldest; relevance runs newest), handing each scope's
/// downloaded hits to `sink` in scope order, folder by folder within one that
/// spans every folder. Arranging the landed hits in `order` is the sink's.
pub async fn two_phase<B: SearchBackend, S: SearchSink>(
    backend: &mut B,
    scopes: &[Scope],
    limit: usize,
    order: SearchOrder,
    sink: &mut S,
) -> Cut {
    let order = order.on_server();
    let mut cut = Cut::default();
    let mut groups: Vec<(usize, Vec<SearchStub<B::Key>>)> = Vec::new();
    for (index, scope) in scopes.iter().enumerate() {
        match backend.stubs(scope, limit, order).await {
            Ok(found) => {
                log::info!(
                    "[server_search] {scope:?} matched {}{}",
                    found.stubs.len(),
                    if found.more { " and more" } else { "" }
                );
                cut.truncated |= found.more;
                cut.capped |= found.capped;
                if let Some(reason) = &found.local {
                    sink.answered_locally(index, reason);
                }
                let admitted = found
                    .stubs
                    .into_iter()
                    .filter(|stub| sink.admit(stub.message_id.as_deref()))
                    .collect();
                groups.push((index, admitted));
            }
            Err(error) => sink.refused(index, &error),
        }
    }

    let (picked, dropped) = select(groups, limit, order);
    cut.truncated |= dropped;
    for (index, stubs) in picked {
        let keys: Vec<B::Key> = stubs.iter().map(|stub| stub.key.clone()).collect();
        match backend.fetch(&scopes[index], &keys).await {
            Ok(fetched) => {
                for (folder, emails) in by_folder(&stubs, fetched.emails) {
                    sink.land(index, folder, emails);
                }
                if !fetched.missing.is_empty() {
                    sink.refused(index, &shortfall(&fetched.missing, keys.len()));
                }
            }
            Err(error) => sink.refused(index, &error),
        }
    }
    cut
}

/// The downloaded `emails` of one scope grouped by the folder their stubs
/// name, each group in download order and the groups in the order a folder
/// first appears; one group under `None` for a scope that is one mailbox.
fn by_folder<K: Eq + std::hash::Hash>(
    stubs: &[SearchStub<K>],
    emails: Vec<(K, FetchedEmail)>,
) -> Vec<(Option<&str>, Vec<FetchedEmail>)> {
    let folder_of: std::collections::HashMap<&K, Option<&str>> = stubs
        .iter()
        .map(|stub| (&stub.key, stub.folder.as_deref()))
        .collect();
    let mut groups: Vec<(Option<&str>, Vec<FetchedEmail>)> = Vec::new();
    for (key, email) in emails {
        let folder = folder_of.get(&key).copied().flatten();
        match groups.iter_mut().find(|(seen, _)| *seen == folder) {
            Some((_, group)) => group.push(email),
            None => groups.push((folder, vec![email])),
        }
    }
    groups
}

/// The error a mailbox whose second pass fell short is reported with: how
/// many of the `asked` hits did not come back, and why, each reason once with
/// its count.
fn shortfall<K>(missing: &[(K, String)], asked: usize) -> anyhow::Error {
    let mut reasons: Vec<(&str, usize)> = Vec::new();
    for (_, why) in missing {
        match reasons.iter_mut().find(|(seen, _)| *seen == why.as_str()) {
            Some((_, count)) => *count += 1,
            None => reasons.push((why, 1)),
        }
    }
    let reasons = reasons
        .iter()
        .map(|(why, count)| format!("{count} {why}"))
        .collect::<Vec<_>>()
        .join(", ");
    anyhow::anyhow!(
        "{} of {asked} hits could not be downloaded ({reasons})",
        missing.len()
    )
}

/// The stubs each mailbox downloads whole, and whether the cut dropped any.
///
/// Every mailbox's stubs are merged in `order` by `Date` (newest first, or
/// oldest first; relevance reads as newest), an unparsable date last, and cut
/// to `limit`. The survivors are handed back grouped by the mailbox index
/// they came with, in that order, in `order` within each; a mailbox none of
/// whose stubs survived is left out, so the second pass selects nothing it
/// downloads nothing from.
pub fn select<K>(
    groups: Vec<StubGroup<K>>,
    limit: usize,
    order: SearchOrder,
) -> (Vec<StubGroup<K>>, bool) {
    let order = order.on_server();
    let mut all: Vec<(usize, SearchStub<K>)> = groups
        .into_iter()
        .flat_map(|(index, stubs)| stubs.into_iter().map(move |stub| (index, stub)))
        .collect();
    all.sort_by(|(_, a), (_, b)| order.by_date(header_date(&a.date), header_date(&b.date)));
    let dropped = all.len() > limit;
    all.truncate(limit);

    let mut picked: Vec<(usize, Vec<SearchStub<K>>)> = Vec::new();
    all.sort_by_key(|(index, _)| *index);
    for (index, stub) in all {
        match picked.last_mut() {
            Some((last, stubs)) if *last == index => stubs.push(stub),
            _ => picked.push((index, vec![stub])),
        }
    }
    (picked, dropped)
}

/// A `Date` header value as the instant the merge orders by, `None` when it
/// does not parse.
fn header_date(date: &str) -> Option<chrono::DateTime<chrono::FixedOffset>> {
    chrono::DateTime::parse_from_rfc2822(date).ok()
}

/// One IMAP session, searching with one rendered `SEARCH`.
pub struct ImapSearch<'a> {
    pub session: &'a mut ImapSession,
    /// The `SEARCH` this server runs (`crate::search::to_imap`, or
    /// `to_gmail_search_command` for Gmail's `X-GM-RAW`).
    pub search: String,
    /// The Message-ID the query named, which the server matches as a
    /// substring and the stubs then match exactly.
    pub message_id: Option<String>,
}

impl SearchBackend for ImapSearch<'_> {
    type Key = u32;

    async fn stubs(
        &mut self,
        scope: &Scope,
        limit: usize,
        order: SearchOrder,
    ) -> Result<MailboxStubs<u32>> {
        crate::imap_client::search_stubs_on_session(
            self.session,
            &self.search,
            self.message_id.as_deref(),
            imap_mailbox(scope)?,
            limit,
            order,
        )
        .await
    }

    async fn fetch(&mut self, scope: &Scope, uids: &[u32]) -> Result<Fetched<u32>> {
        crate::imap_client::fetch_uids_on_session(self.session, imap_mailbox(scope)?, uids).await
    }
}

/// What a [`StoreFallback`] stub names its message by: the server's own key,
/// or the store row of a mailbox the store answered.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum FallbackKey<K> {
    Server(K),
    Stored(i64),
}

/// A backend whose mailbox the server cannot search for its charset is
/// searched in the account's local store instead (PERSO-214).
///
/// TUM's Exchange answers a search under `CHARSET UTF-8` with `NO
/// [BADCHARSET (US-ASCII)]` in every folder, so `from:Förster` found nothing
/// there while the mail sat in the store. The refusal
/// ([`crate::imap_client::search::BadCharset`]) is answered here, once per
/// mailbox and without a second server search in another charset: the stubs
/// are the stored rows of that mailbox that match the query, newest or
/// oldest first as asked, and they compete in the merge like the server's.
/// The mailbox's stubs carry the refusal's sentence in
/// [`MailboxStubs::local`], which the sink reports. Un-synced mail cannot be
/// among them, as for a plain IMAP `has:attachment`.
///
/// Any other failure is the server's and passes through, and so does the
/// refusal when the account has no store to stand in.
pub struct StoreFallback<'a, B> {
    pub inner: B,
    /// The account the store is searched for.
    pub account: &'a str,
    /// The account's directory, which holds its store and its blobs.
    pub account_dir: PathBuf,
    /// The query the store runs, the one `inner` rendered for the server.
    pub query: &'a crate::search::Query,
    /// The store key of each mailbox, by the server name its scope carries;
    /// a name it lacks is its own key.
    pub mailboxes: HashMap<String, String>,
}

impl<B> SearchBackend for StoreFallback<'_, B>
where
    B: SearchBackend + Send,
{
    type Key = FallbackKey<B::Key>;

    async fn stubs(
        &mut self,
        scope: &Scope,
        limit: usize,
        order: SearchOrder,
    ) -> Result<MailboxStubs<Self::Key>> {
        let error = match self.inner.stubs(scope, limit, order).await {
            Ok(found) => {
                return Ok(MailboxStubs {
                    stubs: found
                        .stubs
                        .into_iter()
                        .map(|stub| SearchStub {
                            key: FallbackKey::Server(stub.key),
                            date: stub.date,
                            message_id: stub.message_id,
                            folder: stub.folder,
                        })
                        .collect(),
                    more: found.more,
                    capped: found.capped,
                    local: found.local,
                })
            }
            Err(error) => error,
        };
        let Some(refusal) = error.downcast_ref::<crate::imap_client::search::BadCharset>() else {
            return Err(error);
        };
        let reason = refusal.to_string();
        match self.stored_stubs(scope, limit, order) {
            None => Err(error),
            Some(Err(e)) => Err(anyhow::anyhow!(
                "{error}, and the local store could not search it either: {e:#}"
            )),
            Some(Ok(mut found)) => {
                log::info!(
                    "[server_search] {scope:?} searched in the local store, {} match(es): {reason}",
                    found.stubs.len()
                );
                found.local = Some(reason);
                Ok(found)
            }
        }
    }

    async fn fetch(&mut self, scope: &Scope, keys: &[Self::Key]) -> Result<Fetched<Self::Key>> {
        let mut server = Vec::new();
        let mut stored = Vec::new();
        for key in keys {
            match key {
                FallbackKey::Server(key) => server.push(key.clone()),
                FallbackKey::Stored(id) => stored.push(*id),
            }
        }
        // The store's part first and whole, so no store is held across the
        // server's await.
        let mut answer = self.stored_emails(&stored);
        if !server.is_empty() {
            let fetched = self.inner.fetch(scope, &server).await?;
            answer.emails.extend(
                fetched
                    .emails
                    .into_iter()
                    .map(|(key, email)| (FallbackKey::Server(key), email)),
            );
            answer.missing.extend(
                fetched
                    .missing
                    .into_iter()
                    .map(|(key, why)| (FallbackKey::Server(key), why)),
            );
        }
        Ok(answer)
    }
}

impl<B: SearchBackend> StoreFallback<'_, B> {
    /// The stored rows of `scope`'s mailbox that match the query, as stubs,
    /// or `None` when there is no store to search or the scope is not one
    /// mailbox.
    fn stored_stubs(
        &self,
        scope: &Scope,
        limit: usize,
        order: SearchOrder,
    ) -> Option<Result<MailboxStubs<FallbackKey<B::Key>>>> {
        let Scope::Mailbox(server) = scope else {
            return None;
        };
        let mailbox = self.mailboxes.get(server).unwrap_or(server);
        let store = crate::store::open_store_in(&self.account_dir)?;
        // One past the limit, to know whether the mailbox matched more.
        let hits = crate::store::search::search_ast(
            &store,
            self.account,
            self.query,
            Some(mailbox),
            limit.saturating_add(1),
            order,
        );
        Some(hits.map(|hits| {
            MailboxStubs {
                more: hits.len() > limit,
                stubs: hits
                    .into_iter()
                    .take(limit)
                    .map(|hit| SearchStub {
                        key: FallbackKey::Stored(hit.row.id),
                        date: hit.row.date_display.unwrap_or_default(),
                        message_id: Some(hit.row.message_id),
                        folder: None,
                    })
                    .collect(),
                capped: false,
                local: None,
            }
        }))
    }

    /// The stored messages behind `ids`, as a download brings them back; a
    /// row gone since the first pass is missing, with why.
    fn stored_emails(&self, ids: &[i64]) -> Fetched<FallbackKey<B::Key>> {
        let mut answer = Fetched::default();
        if ids.is_empty() {
            return answer;
        }
        let Some(store) = crate::store::open_store_in(&self.account_dir) else {
            answer.missing = ids
                .iter()
                .map(|id| {
                    (
                        FallbackKey::Stored(*id),
                        "the local store is gone".to_string(),
                    )
                })
                .collect();
            return answer;
        };
        // `crate::config::blobs_dir`, for the directory already resolved.
        let blobs = crate::store::BlobStore::new(self.account_dir.join("blobs"));
        for id in ids {
            match crate::store::read::find_by_id(&store, *id) {
                Ok(Some(row)) => {
                    let body = crate::store::read::load_body(&store, &blobs, row.id);
                    answer
                        .emails
                        .push((FallbackKey::Stored(*id), stored_email(row, body)));
                }
                Ok(None) => answer.missing.push((
                    FallbackKey::Stored(*id),
                    "no longer in the local store".to_string(),
                )),
                Err(e) => answer
                    .missing
                    .push((FallbackKey::Stored(*id), format!("{e:#}"))),
            }
        }
        answer
    }
}

/// A stored row as the message a download would have brought back: its
/// headers, flags and body as the store holds them.
fn stored_email(row: crate::store::read::MessageRow, body: Option<String>) -> FetchedEmail {
    let flags = row.flags();
    FetchedEmail {
        from: row.from.unwrap_or_default(),
        to: row.to.unwrap_or_default(),
        cc: row.cc,
        reply_to: row.reply_to,
        bcc: row.bcc,
        subject: row.subject.unwrap_or_default(),
        date: row.date_display.unwrap_or_default(),
        body_text: body.unwrap_or_default(),
        html_body: None,
        has_attachments: row.has_attachments,
        server_has_attachments: None,
        message_id: Some(row.message_id),
        attachments: Vec::new(),
        flags,
        calendar_ics: None,
        event: None,
    }
}

/// The mailbox an IMAP search selects: IMAP has no query across mailboxes.
fn imap_mailbox(scope: &Scope) -> Result<&str> {
    match scope {
        Scope::Mailbox(mailbox) => Ok(mailbox),
        Scope::EveryFolder => anyhow::bail!("an IMAP search runs in one mailbox at a time"),
    }
}

/// One Graph client, searching a folder at a time or every folder at once
/// over HTTP.
pub struct GraphSearch<'a> {
    client: &'a GraphClient,
    query: &'a crate::search::Query,
    /// The account's configured mailboxes as `(label, server name)`, whose
    /// labels a hit of a [`Scope::EveryFolder`] search is listed under.
    configured: &'a [(String, String)],
    /// The label of every folder id resolved so far in this search.
    labels: std::collections::HashMap<String, String>,
}

impl<'a> GraphSearch<'a> {
    /// A search of `query` through `client`, on an account configuring
    /// `configured` as `(label, server name)`.
    pub fn new(
        client: &'a GraphClient,
        query: &'a crate::search::Query,
        configured: &'a [(String, String)],
    ) -> GraphSearch<'a> {
        GraphSearch {
            client,
            query,
            configured,
            labels: std::collections::HashMap::new(),
        }
    }

    /// Replace each stub's folder id by the label it is listed under,
    /// resolving the ids this search has not met yet in one go.
    ///
    /// A folder Graph cannot name is listed under its id, logged: the hit is
    /// worth more than a tidy label.
    async fn label_folders(&mut self, stubs: &mut [SearchStub<String>]) {
        let mut unknown: Vec<String> = Vec::new();
        for id in stubs.iter().filter_map(|stub| stub.folder.as_ref()) {
            if !self.labels.contains_key(id) && !unknown.contains(id) {
                unknown.push(id.clone());
            }
        }
        if !unknown.is_empty() {
            match self.client.folder_labels(self.configured, &unknown).await {
                Ok(found) => self.labels.extend(found),
                Err(e) => log::warn!("[server_search] naming the hits' folders: {e:#}"),
            }
        }
        for stub in stubs.iter_mut() {
            if let Some(id) = stub.folder.take() {
                stub.folder = Some(self.labels.get(&id).cloned().unwrap_or(id));
            }
        }
    }
}

impl SearchBackend for GraphSearch<'_> {
    type Key = String;

    async fn stubs(
        &mut self,
        scope: &Scope,
        limit: usize,
        order: SearchOrder,
    ) -> Result<MailboxStubs<String>> {
        match scope {
            Scope::Mailbox(folder) => {
                let mut found = self
                    .client
                    .search_stubs(self.query, Some(folder), limit, order)
                    .await?;
                // One folder's hits are listed under its own label.
                for stub in &mut found.stubs {
                    stub.folder = None;
                }
                Ok(found)
            }
            Scope::EveryFolder => {
                let mut found = self
                    .client
                    .search_stubs(self.query, None, limit, order)
                    .await?;
                self.label_folders(&mut found.stubs).await;
                Ok(found)
            }
        }
    }

    /// The ids are Graph's own and name a message in whichever folder it
    /// sits, so the scope only says whose hits these are.
    async fn fetch(&mut self, _scope: &Scope, ids: &[String]) -> Result<Fetched<String>> {
        self.client.fetch_search_hits(ids).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};

    fn stub(key: u32, date: &str, message_id: &str) -> SearchStub<u32> {
        SearchStub {
            key,
            date: date.to_string(),
            message_id: Some(message_id.to_string()),
            folder: None,
        }
    }

    fn scopes(names: &[&str]) -> Vec<Scope> {
        names
            .iter()
            .map(|name| Scope::Mailbox(name.to_string()))
            .collect()
    }

    /// The name a scope's matches are scripted under.
    fn name(scope: &Scope) -> String {
        match scope {
            Scope::Mailbox(mailbox) => mailbox.clone(),
            Scope::EveryFolder => "*".to_string(),
        }
    }

    fn keys_of(picked: Vec<(usize, Vec<SearchStub<u32>>)>) -> Vec<(usize, Vec<u32>)> {
        picked
            .into_iter()
            .map(|(index, stubs)| (index, stubs.into_iter().map(|s| s.key).collect()))
            .collect()
    }

    /// The merge picks the `limit` newest by `Date` across every mailbox,
    /// whatever their UIDs say, and each survivor stays with its mailbox.
    #[test]
    fn the_newest_stubs_across_mailboxes_survive_the_cut() {
        let groups = vec![
            (
                0,
                vec![
                    stub(90, "1 Feb 2026 10:00:00 +0000", "<a@x>"),
                    stub(80, "20 Feb 2026 10:00:00 +0000", "<b@x>"),
                ],
            ),
            (
                1,
                vec![
                    stub(7, "25 Feb 2026 10:00:00 +0000", "<c@x>"),
                    stub(6, "24 Feb 2026 10:00:00 +0000", "<d@x>"),
                    stub(5, "3 Feb 2026 10:00:00 +0000", "<e@x>"),
                ],
            ),
            (2, vec![]),
            (3, vec![stub(1, "not a date", "<f@x>")]),
        ];
        let (picked, dropped) = select(groups.clone(), 3, SearchOrder::Newest);
        assert_eq!(keys_of(picked), vec![(0, vec![80]), (1, vec![7, 6])]);
        assert!(dropped, "three of six were cut");

        let (picked, dropped) = select(groups, 6, SearchOrder::Newest);
        assert_eq!(
            keys_of(picked),
            vec![(0, vec![80, 90]), (1, vec![7, 6, 5]), (3, vec![1])],
            "an unparsable date sorts last"
        );
        assert!(!dropped, "every stub fits");
    }

    /// The oldest-first merge keeps the `limit` oldest by `Date` across every
    /// mailbox, an unparsable date still last, and relevance merges newest
    /// first, a server having no score (PERSO-273).
    #[test]
    fn the_oldest_stubs_survive_an_oldest_first_cut() {
        let groups = vec![
            (
                0,
                vec![
                    stub(80, "20 Feb 2026 10:00:00 +0000", "<b@x>"),
                    stub(90, "1 Feb 2026 10:00:00 +0000", "<a@x>"),
                ],
            ),
            (
                1,
                vec![
                    stub(5, "3 Feb 2026 10:00:00 +0000", "<e@x>"),
                    stub(7, "25 Feb 2026 10:00:00 +0000", "<c@x>"),
                ],
            ),
            (2, vec![stub(1, "not a date", "<f@x>")]),
        ];
        let (picked, dropped) = select(groups.clone(), 2, SearchOrder::Oldest);
        assert_eq!(keys_of(picked), vec![(0, vec![90]), (1, vec![5])]);
        assert!(dropped);

        let (picked, _) = select(groups.clone(), 5, SearchOrder::Oldest);
        assert_eq!(
            keys_of(picked),
            vec![(0, vec![90, 80]), (1, vec![5, 7]), (2, vec![1])],
            "an unparsable date sorts last oldest first too"
        );

        let (picked, _) = select(groups, 2, SearchOrder::Relevance);
        assert_eq!(keys_of(picked), vec![(0, vec![80]), (1, vec![7])]);
    }

    /// An oldest-first search asks every mailbox for its oldest matches and
    /// downloads the oldest across them; relevance asks for the newest
    /// (PERSO-273).
    #[tokio::test]
    async fn an_oldest_first_search_asks_each_mailbox_for_its_oldest() {
        let mut script = Script::default();
        script.matches.insert(
            "INBOX".into(),
            (0..6)
                .map(|i| stub(100 - i, &day(30 - i), &format!("<in{i}@x>")))
                .collect(),
        );
        script.matches.insert(
            "Archive".into(),
            (0..3)
                .map(|i| stub(10 - i, &day(20 - i), &format!("<ar{i}@x>")))
                .collect(),
        );
        let mut sink = Collect::default();

        let cut = two_phase(
            &mut script,
            &scopes(&["INBOX", "Archive"]),
            3,
            SearchOrder::Oldest,
            &mut sink,
        )
        .await;

        assert_eq!(script.orders, [SearchOrder::Oldest, SearchOrder::Oldest]);
        assert_eq!(
            script.fetched,
            vec![("Archive".to_string(), vec![8, 9, 10])],
            "the archive holds the three oldest"
        );
        assert!(cut.truncated);

        let mut script = Script::default();
        let mut sink = Collect::default();
        two_phase(
            &mut script,
            &scopes(&["INBOX"]),
            3,
            SearchOrder::Relevance,
            &mut sink,
        )
        .await;
        assert_eq!(script.orders, [SearchOrder::Newest]);
    }

    /// A scripted server: each mailbox's matches newest first, and a record
    /// of what each pass asked for.
    #[derive(Default)]
    struct Script {
        matches: HashMap<String, Vec<SearchStub<u32>>>,
        refuse: HashSet<String>,
        /// Mailboxes that answer `NO [BADCHARSET (US-ASCII)]`.
        badcharset: HashSet<String>,
        /// UIDs the second pass does not bring back, and why.
        vanish: HashMap<u32, &'static str>,
        asked: Vec<(String, usize)>,
        /// The order each first-pass query was asked in.
        orders: Vec<SearchOrder>,
        fetched: Vec<(String, Vec<u32>)>,
    }

    impl SearchBackend for Script {
        type Key = u32;

        async fn stubs(
            &mut self,
            scope: &Scope,
            limit: usize,
            order: SearchOrder,
        ) -> Result<MailboxStubs<u32>> {
            let mailbox = name(scope);
            self.asked.push((mailbox.clone(), limit));
            self.orders.push(order);
            if self.refuse.contains(&mailbox) {
                anyhow::bail!("NO no such mailbox");
            }
            if self.badcharset.contains(&mailbox) {
                return Err(anyhow::Error::new(crate::imap_client::search::BadCharset {
                    charsets: "US-ASCII".to_string(),
                }));
            }
            let mut all = self.matches.get(&mailbox).cloned().unwrap_or_default();
            if order == SearchOrder::Oldest {
                all.reverse();
            }
            Ok(MailboxStubs {
                more: all.len() > limit,
                stubs: all.into_iter().take(limit).collect(),
                capped: false,
                local: None,
            })
        }

        async fn fetch(&mut self, scope: &Scope, uids: &[u32]) -> Result<Fetched<u32>> {
            let mailbox = name(scope);
            self.fetched.push((mailbox.clone(), uids.to_vec()));
            let stubs = &self.matches[&mailbox];
            let (gone, here): (Vec<u32>, Vec<u32>) =
                uids.iter().partition(|uid| self.vanish.contains_key(uid));
            let missing = gone
                .into_iter()
                .map(|uid| (uid, self.vanish[&uid].to_string()))
                .collect();
            let emails = here
                .into_iter()
                .map(|uid| {
                    let stub = stubs.iter().find(|s| s.key == uid).unwrap();
                    let email = FetchedEmail {
                        from: String::new(),
                        to: String::new(),
                        cc: None,
                        reply_to: None,
                        bcc: None,
                        subject: String::new(),
                        date: stub.date.clone(),
                        body_text: String::new(),
                        html_body: None,
                        has_attachments: false,
                        server_has_attachments: None,
                        message_id: stub.message_id.clone(),
                        attachments: Vec::new(),
                        flags: Default::default(),
                        calendar_ics: None,
                        event: None,
                    };
                    (uid, email)
                })
                .collect();
            Ok(Fetched { emails, missing })
        }
    }

    /// A sink that drops one Message-ID and records the rest.
    #[derive(Default)]
    struct Collect {
        drop: Option<String>,
        landed: Vec<(usize, Vec<String>)>,
        /// The folder each `landed` entry sits in.
        landed_in: Vec<Option<String>>,
        refused: Vec<usize>,
        errors: Vec<String>,
        /// Each mailbox the store answered, with why.
        local: Vec<(usize, String)>,
    }

    impl SearchSink for Collect {
        fn admit(&mut self, message_id: Option<&str>) -> bool {
            message_id != self.drop.as_deref()
        }
        fn land(&mut self, mailbox: usize, folder: Option<&str>, emails: Vec<FetchedEmail>) {
            let ids = emails.into_iter().filter_map(|e| e.message_id).collect();
            self.landed.push((mailbox, ids));
            self.landed_in.push(folder.map(str::to_string));
        }
        fn refused(&mut self, mailbox: usize, error: &anyhow::Error) {
            self.refused.push(mailbox);
            self.errors.push(format!("{error}"));
        }
        fn answered_locally(&mut self, mailbox: usize, reason: &str) {
            self.local.push((mailbox, reason.to_string()));
        }
    }

    /// A survivor the second pass could not download is reported against its
    /// mailbox with how many and why, and the hits that did download still
    /// land: a throttled or moved hit never just goes missing (PERSO-175).
    #[tokio::test]
    async fn a_hit_the_second_pass_cannot_download_is_reported_not_dropped() {
        let mut script = Script::default();
        script.matches.insert(
            "INBOX".into(),
            (0..4)
                .map(|i| stub(10 - i, &day(10 - i), &format!("<m{i}@x>")))
                .collect(),
        );
        script.vanish.insert(9, "throttled by the server 3 times");
        script.vanish.insert(8, "throttled by the server 3 times");
        script.vanish.insert(7, "HTTP 404");
        let mut sink = Collect::default();

        let cut = two_phase(
            &mut script,
            &scopes(&["INBOX"]),
            4,
            SearchOrder::Newest,
            &mut sink,
        )
        .await;

        assert_eq!(sink.landed, vec![(0, vec!["<m0@x>".to_string()])]);
        assert_eq!(sink.refused, vec![0]);
        assert_eq!(
            sink.errors,
            ["3 of 4 hits could not be downloaded \
              (2 throttled by the server 3 times, 1 HTTP 404)"]
        );
        assert_eq!(cut, Cut::default(), "a shortfall is a failure, not a cut");
    }

    fn day(n: u32) -> String {
        format!("{n} Mar 2026 10:00:00 +0000")
    }

    /// PERSO-214: a mailbox whose server refuses the search for its charset
    /// is searched in the local store, once, and its stored matches compete
    /// in the merge with the server's own; the sink hears which mailbox the
    /// store answered and why. Another refusal stays the server's, and with
    /// no store the charset refusal does too.
    #[tokio::test]
    async fn a_mailbox_the_server_cannot_search_for_its_charset_is_searched_in_the_store() {
        use crate::ingest::{ingest_message, IngestInput};
        use crate::store::{BlobStore, Store};

        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("store.sqlite3")).unwrap();
        let blobs = BlobStore::new(dir.path().join("blobs"));
        let stored = |subject: &str, date: &str, id: &str| FetchedEmail {
            from: "Nick Förster <foerster@example.com>".to_string(),
            to: "sylvain@example.com".to_string(),
            cc: None,
            reply_to: None,
            bcc: None,
            subject: subject.to_string(),
            date: date.to_string(),
            body_text: "Antrag anbei\n".to_string(),
            html_body: None,
            has_attachments: false,
            server_has_attachments: None,
            message_id: Some(id.to_string()),
            attachments: Vec::new(),
            flags: Default::default(),
            calendar_ics: None,
            event: None,
        };
        for (uid, (mailbox, email)) in [
            ("inbox", stored("LOC Antrag BIM Lab", &day(19), "<bim@x>")),
            ("inbox", stored("Older", &day(1), "<older@x>")),
            ("archive", stored("Archived", &day(18), "<archived@x>")),
        ]
        .into_iter()
        .enumerate()
        {
            ingest_message(
                &store,
                &blobs,
                &IngestInput {
                    account: "tum",
                    mailbox,
                    uid: uid as i64 + 1,
                    email: &email,
                    raw: None,
                },
            )
            .unwrap();
        }
        drop(store);

        let mut script = Script::default();
        script.badcharset.insert("INBOX".into());
        script
            .matches
            .insert("Sent Items".into(), vec![stub(7, &day(10), "<sent@x>")]);
        script.refuse.insert("Spesen".into());
        let query = crate::search::parse("from:Förster").unwrap();
        let mut backend = StoreFallback {
            inner: script,
            account: "tum",
            account_dir: dir.path().to_path_buf(),
            query: &query,
            mailboxes: HashMap::from([
                ("INBOX".to_string(), "inbox".to_string()),
                ("Sent Items".to_string(), "sent".to_string()),
            ]),
        };
        let mut sink = Collect::default();

        let cut = two_phase(
            &mut backend,
            &scopes(&["INBOX", "Sent Items", "Spesen"]),
            2,
            SearchOrder::Newest,
            &mut sink,
        )
        .await;

        let badcharset =
            "the server cannot search for non-ASCII text, only in US-ASCII (BADCHARSET)";
        assert_eq!(sink.local, [(0, badcharset.to_string())]);
        assert_eq!(
            backend.inner.asked,
            [
                ("INBOX".to_string(), 2),
                ("Sent Items".to_string(), 2),
                ("Spesen".to_string(), 2)
            ],
            "one server search per mailbox, none again in another charset"
        );
        assert_eq!(
            sink.landed,
            [
                (0, vec!["<bim@x>".to_string()]),
                (1, vec!["<sent@x>".to_string()]),
            ],
            "the store's newest match and the server's, merged under the limit; \
             the archive's row is not the Inbox's"
        );
        assert_eq!(
            backend.inner.fetched,
            [("Sent Items".to_string(), vec![7])],
            "a stored hit is not downloaded"
        );
        assert_eq!(sink.refused, [2], "another refusal stays the server's");
        assert_eq!(sink.errors, ["NO no such mailbox"]);
        assert!(cut.truncated, "the Inbox's older row was cut");

        let mut sink = Collect::default();
        backend.account_dir = dir.path().join("no-store");
        two_phase(
            &mut backend,
            &scopes(&["INBOX"]),
            2,
            SearchOrder::Newest,
            &mut sink,
        )
        .await;
        assert!(sink.local.is_empty());
        assert_eq!(sink.errors, [badcharset], "no store, nothing stands in");
    }

    /// A search across every folder at once is one first-pass query, and its
    /// hits land under the folder each sits in, one configured or not
    /// (PERSO-175): the Graph default, which reaches an Archive subfolder
    /// the account does not configure.
    #[tokio::test]
    async fn a_search_across_every_folder_lands_each_hit_under_its_folder() {
        let in_folder = |key: u32, n: u32, folder: &str| SearchStub {
            folder: Some(folder.to_string()),
            ..stub(key, &day(n), &format!("<{key}@x>"))
        };
        let mut script = Script::default();
        script.matches.insert(
            "*".into(),
            vec![
                in_folder(4, 9, "Inbox"),
                in_folder(3, 8, "Archive/2023"),
                in_folder(2, 7, "Inbox"),
                in_folder(1, 6, "Archive/2023"),
            ],
        );
        let mut sink = Collect::default();

        let cut = two_phase(
            &mut script,
            &[Scope::EveryFolder],
            3,
            SearchOrder::Newest,
            &mut sink,
        )
        .await;

        assert_eq!(
            script.asked,
            [("*".to_string(), 3)],
            "one query, not one per folder"
        );
        assert_eq!(script.fetched, [("*".to_string(), vec![4, 3, 2])]);
        assert_eq!(
            sink.landed,
            [
                (0, vec!["<4@x>".to_string(), "<2@x>".to_string()]),
                (0, vec!["<3@x>".to_string()]),
            ]
        );
        assert_eq!(
            sink.landed_in,
            [Some("Inbox".to_string()), Some("Archive/2023".to_string())]
        );
        assert!(cut.truncated, "the fourth match was cut");
    }

    /// Every mailbox is asked for the whole limit, only the merged newest
    /// `limit` are downloaded, and the answer says it was cut (PERSO-175).
    #[tokio::test]
    async fn every_mailbox_is_asked_for_the_whole_limit_and_only_the_survivors_download() {
        let mut script = Script::default();
        // A busy inbox with the five newest, and an idle archive.
        script.matches.insert(
            "INBOX".into(),
            (0..8)
                .map(|i| stub(100 - i, &day(30 - i), &format!("<in{i}@x>")))
                .collect(),
        );
        script.matches.insert(
            "Archive".into(),
            (0..3)
                .map(|i| stub(10 - i, &day(20 - i), &format!("<ar{i}@x>")))
                .collect(),
        );
        let mut sink = Collect::default();

        let cut = two_phase(
            &mut script,
            &scopes(&["INBOX", "Archive"]),
            5,
            SearchOrder::Newest,
            &mut sink,
        )
        .await;

        assert_eq!(
            script.asked,
            vec![("INBOX".to_string(), 5), ("Archive".to_string(), 5)],
            "the limit is not split across the mailboxes"
        );
        assert_eq!(
            script.fetched,
            vec![("INBOX".to_string(), vec![100, 99, 98, 97, 96])],
            "the archive's older hits lost the merge and were never downloaded"
        );
        assert_eq!(sink.landed.len(), 1);
        assert_eq!(sink.landed[0].1.len(), 5);
        assert_eq!(
            cut,
            Cut {
                truncated: true,
                capped: false
            }
        );
    }

    /// A search that fits its limit is not cut, a refused mailbox goes to the
    /// sink and does not stop the others, and a hit the sink does not admit
    /// is neither counted nor downloaded.
    #[tokio::test]
    async fn a_refused_mailbox_and_a_dropped_hit_leave_the_rest_alone() {
        let mut script = Script::default();
        script.matches.insert(
            "INBOX".into(),
            vec![stub(2, &day(2), "<keep@x>"), stub(1, &day(1), "<shown@x>")],
        );
        script.refuse.insert("Spesen".into());
        let mut sink = Collect {
            drop: Some("<shown@x>".into()),
            ..Collect::default()
        };

        let cut = two_phase(
            &mut script,
            &scopes(&["Spesen", "INBOX"]),
            50,
            SearchOrder::Newest,
            &mut sink,
        )
        .await;

        assert_eq!(sink.refused, vec![0]);
        assert_eq!(script.fetched, vec![("INBOX".to_string(), vec![2])]);
        assert_eq!(sink.landed, vec![(1, vec!["<keep@x>".to_string()])]);
        assert_eq!(cut, Cut::default(), "nothing was cut");
    }

    /// A mailbox that matched more than it listed cuts the answer even when
    /// every stub it listed survives the merge.
    #[tokio::test]
    async fn a_mailbox_holding_more_than_it_listed_cuts_the_answer() {
        let mut script = Script::default();
        script.matches.insert(
            "INBOX".into(),
            (0..3)
                .map(|i| stub(10 - i, &day(10 - i), &format!("<m{i}@x>")))
                .collect(),
        );
        let mut sink = Collect {
            // The post-filter drops one of the two the mailbox listed.
            drop: Some("<m0@x>".into()),
            ..Collect::default()
        };
        let cut = two_phase(
            &mut script,
            &scopes(&["INBOX"]),
            2,
            SearchOrder::Newest,
            &mut sink,
        )
        .await;
        assert_eq!(script.fetched, vec![("INBOX".to_string(), vec![9])]);
        assert!(cut.truncated);
    }
}
