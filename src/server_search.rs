//! The two passes of a server search over several mailboxes (PERSO-175).
//!
//! `limit` is the number of hits the whole search answers with, not a slice of
//! it per mailbox. Splitting it dropped a busy mailbox's newest hits for an
//! idle one's older ones, and asking every mailbox for `limit` whole messages
//! downloaded as many times the answer as there are mailboxes. So the search
//! runs in two passes:
//!
//! 1. Each mailbox is asked for its newest `limit` matches as
//!    [`SearchStub`]s: the key the second pass downloads by, the `Date` the
//!    merge orders by and the `Message-ID` the caller filters on.
//! 2. Every mailbox's stubs are merged newest first and cut to `limit`
//!    ([`select`]), and only the survivors are downloaded whole, mailbox by
//!    mailbox.
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
//! The backend is a [`SearchBackend`]: one IMAP session ([`ImapSearch`]) or
//! one Graph client ([`GraphSearch`]). What the hits become is the caller's
//! [`SearchSink`], which is where the daemon's dedup, attachment post-filter
//! and event stream live (`src/daemon/methods/message_server.rs`).

use std::future::Future;

use anyhow::Result;

use crate::graph::GraphClient;
use crate::imap_client::ImapSession;
use crate::parse::FetchedEmail;

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
    /// Its newest matches, at most the limit it was asked for.
    pub stubs: Vec<SearchStub<K>>,
    /// The server matched more than `stubs` holds.
    pub more: bool,
    /// Microsoft Graph's `$search` cap stopped it short of the limit.
    pub capped: bool,
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

    /// The newest `limit` matches in `scope` as stubs.
    fn stubs(
        &mut self,
        scope: &Scope,
        limit: usize,
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
}

/// Search `scopes` for the newest `limit` hits across all of them, handing
/// each scope's downloaded hits to `sink` in scope order, folder by folder
/// within one that spans every folder.
pub async fn two_phase<B: SearchBackend, S: SearchSink>(
    backend: &mut B,
    scopes: &[Scope],
    limit: usize,
    sink: &mut S,
) -> Cut {
    let mut cut = Cut::default();
    let mut groups: Vec<(usize, Vec<SearchStub<B::Key>>)> = Vec::new();
    for (index, scope) in scopes.iter().enumerate() {
        match backend.stubs(scope, limit).await {
            Ok(found) => {
                log::info!(
                    "[server_search] {scope:?} matched {}{}",
                    found.stubs.len(),
                    if found.more { " and more" } else { "" }
                );
                cut.truncated |= found.more;
                cut.capped |= found.capped;
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

    let (picked, dropped) = select(groups, limit);
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
/// Every mailbox's stubs are merged newest first by `Date`, an unparsable
/// date last, and cut to `limit`. The survivors are handed back grouped by
/// the mailbox index they came with, in that order, newest first within each;
/// a mailbox none of whose stubs survived is left out, so the second pass
/// selects nothing it downloads nothing from.
pub fn select<K>(groups: Vec<StubGroup<K>>, limit: usize) -> (Vec<StubGroup<K>>, bool) {
    let mut all: Vec<(usize, SearchStub<K>)> = groups
        .into_iter()
        .flat_map(|(index, stubs)| stubs.into_iter().map(move |stub| (index, stub)))
        .collect();
    all.sort_by(|(_, a), (_, b)| newest_first(&a.date, &b.date));
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

/// Two `Date` header values, newest first, an unparsable one last.
fn newest_first(a: &str, b: &str) -> std::cmp::Ordering {
    let a = chrono::DateTime::parse_from_rfc2822(a).ok();
    let b = chrono::DateTime::parse_from_rfc2822(b).ok();
    b.cmp(&a)
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

    async fn stubs(&mut self, scope: &Scope, limit: usize) -> Result<MailboxStubs<u32>> {
        crate::imap_client::search_stubs_on_session(
            self.session,
            &self.search,
            self.message_id.as_deref(),
            imap_mailbox(scope)?,
            limit,
        )
        .await
    }

    async fn fetch(&mut self, scope: &Scope, uids: &[u32]) -> Result<Fetched<u32>> {
        crate::imap_client::fetch_uids_on_session(self.session, imap_mailbox(scope)?, uids).await
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

    async fn stubs(&mut self, scope: &Scope, limit: usize) -> Result<MailboxStubs<String>> {
        match scope {
            Scope::Mailbox(folder) => {
                let mut found = self
                    .client
                    .search_stubs(self.query, Some(folder), limit)
                    .await?;
                // One folder's hits are listed under its own label.
                for stub in &mut found.stubs {
                    stub.folder = None;
                }
                Ok(found)
            }
            Scope::EveryFolder => {
                let mut found = self.client.search_stubs(self.query, None, limit).await?;
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
        let (picked, dropped) = select(groups.clone(), 3);
        assert_eq!(keys_of(picked), vec![(0, vec![80]), (1, vec![7, 6])]);
        assert!(dropped, "three of six were cut");

        let (picked, dropped) = select(groups, 6);
        assert_eq!(
            keys_of(picked),
            vec![(0, vec![80, 90]), (1, vec![7, 6, 5]), (3, vec![1])],
            "an unparsable date sorts last"
        );
        assert!(!dropped, "every stub fits");
    }

    /// A scripted server: each mailbox's matches newest first, and a record
    /// of what each pass asked for.
    #[derive(Default)]
    struct Script {
        matches: HashMap<String, Vec<SearchStub<u32>>>,
        refuse: HashSet<String>,
        /// UIDs the second pass does not bring back, and why.
        vanish: HashMap<u32, &'static str>,
        asked: Vec<(String, usize)>,
        fetched: Vec<(String, Vec<u32>)>,
    }

    impl SearchBackend for Script {
        type Key = u32;

        async fn stubs(&mut self, scope: &Scope, limit: usize) -> Result<MailboxStubs<u32>> {
            let mailbox = name(scope);
            self.asked.push((mailbox.clone(), limit));
            if self.refuse.contains(&mailbox) {
                anyhow::bail!("NO no such mailbox");
            }
            let all = self.matches.get(&mailbox).cloned().unwrap_or_default();
            Ok(MailboxStubs {
                more: all.len() > limit,
                stubs: all.into_iter().take(limit).collect(),
                capped: false,
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

        let cut = two_phase(&mut script, &scopes(&["INBOX"]), 4, &mut sink).await;

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

        let cut = two_phase(&mut script, &[Scope::EveryFolder], 3, &mut sink).await;

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

        let cut = two_phase(&mut script, &scopes(&["INBOX", "Archive"]), 5, &mut sink).await;

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

        let cut = two_phase(&mut script, &scopes(&["Spesen", "INBOX"]), 50, &mut sink).await;

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
        let cut = two_phase(&mut script, &scopes(&["INBOX"]), 2, &mut sink).await;
        assert_eq!(script.fetched, vec![("INBOX".to_string(), vec![9])]);
        assert!(cut.truncated);
    }
}
