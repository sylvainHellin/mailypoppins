//! The server Drafts folder as the mirror sees it: six operations behind a
//! trait, so the push, the delete and the reconcile run offline against a fake
//! and against IMAP in the daemon (#0143).

use std::fmt;
use std::future::Future;

use anyhow::{anyhow, Context, Result};
use async_imap::types::UnsolicitedResponse;
use futures::io::{AsyncRead, AsyncWrite};
use futures::TryStreamExt;

use crate::config::ImapConfig;
use crate::imap_client::PooledSession;

/// What a `SELECT` of the folder says about it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FolderInfo {
    /// The folder's UIDVALIDITY, without which a UID means nothing.
    pub uidvalidity: u32,
    /// Whether the server offers `UIDPLUS`, which is what lets a delete
    /// expunge one UID; without it a copy is only flagged `\Deleted`, since a
    /// plain `EXPUNGE` would also take what another client flagged.
    pub uidplus: bool,
}

/// One message of the folder, by its header: what the reconcile reads to
/// link copies to files before it downloads any body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerHead {
    pub uid: u32,
    /// The [`HEAD_FIELDS`] of the header block, raw.
    pub header: Vec<u8>,
}

/// The headers [`DraftsFolder::heads`] carries: those the reconcile links a
/// copy to a file by, and none of the `X-Mailypoppins-Draft-Source` chunks,
/// up to 64 KiB per copy, which only [`DraftsFolder::fetch`] brings.
pub const HEAD_FIELDS: [&str; 6] = [
    "Message-ID",
    crate::send::DRAFT_ID_HEADER,
    "Subject",
    "To",
    "Cc",
    "In-Reply-To",
];

/// The `UID FETCH` items of [`DraftsFolder::heads`] over IMAP.
fn heads_query() -> String {
    format!(
        "(UID BODY.PEEK[HEADER.FIELDS ({})])",
        HEAD_FIELDS.join(" ").to_uppercase()
    )
}

/// The server Drafts folder of one account.
///
/// [`DraftsFolder::select`] comes first and names the folder every later call
/// acts on. A message flagged `\Deleted` is not there for any of them: a copy
/// a delete without `UIDPLUS` left flagged is gone as far as the mirror cares.
pub trait DraftsFolder {
    /// `SELECT` the folder.
    fn select(&mut self, mailbox: &str) -> impl Future<Output = Result<FolderInfo>>;

    /// `APPEND` a copy with `(\Draft \Seen)`. Success is the server's
    /// acknowledgement; the new UID is read back with
    /// [`DraftsFolder::uids_of`], since `async-imap` drops `APPENDUID`.
    fn append(&mut self, raw: &[u8]) -> impl Future<Output = Result<()>>;

    /// The UIDs of every copy carrying `message_id`, ascending.
    fn uids_of(&mut self, message_id: &str) -> impl Future<Output = Result<Vec<u32>>>;

    /// Remove the copies `uids` names: `\Deleted` and, with `UIDPLUS`,
    /// `UID EXPUNGE` of exactly those.
    fn delete(&mut self, uids: &[u32]) -> impl Future<Output = Result<()>>;

    /// Remove the copies `uids` names as a claim, and answer which of them
    /// this session removed: the UIDs whose untagged `EXPUNGE` it saw in
    /// answer to its own `UID EXPUNGE`. A copy another session removed first
    /// is not in the answer. Needs `UIDPLUS`, and refuses without it, since a
    /// plain `EXPUNGE` would also take what another client flagged.
    ///
    /// An error may come after the copies were flagged or removed, so it says
    /// nothing about whether they are still there.
    ///
    /// What a send of a mirrored draft takes the draft's server copy with
    /// before it submits ([`super::send_guard`]), so mp on two machines
    /// sending one draft cannot both go ahead.
    fn claim(&mut self, uids: &[u32]) -> impl Future<Output = Result<Vec<u32>>>;

    /// Every message of the folder, by its [`HEAD_FIELDS`].
    fn heads(&mut self) -> impl Future<Output = Result<Vec<ServerHead>>>;

    /// One message whole, `None` when the UID is gone.
    fn fetch(&mut self, uid: u32) -> impl Future<Output = Result<Option<Vec<u8>>>>;
}

/// `<id>` without its brackets and surrounding space, which is how two
/// spellings of one `Message-ID` compare.
pub fn bare_message_id(message_id: &str) -> &str {
    message_id
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .trim()
}

/// A header's value in a raw header block, unfolded, `None` when absent.
pub fn header_value(header: &[u8], name: &str) -> Option<String> {
    let (headers, _) = mailparse::parse_headers(header).ok()?;
    headers
        .iter()
        .find(|h| h.get_key().eq_ignore_ascii_case(name))
        .map(|h| h.get_value().trim().to_string())
        .filter(|v| !v.is_empty())
}

/// The folder over a session borrowed from the account's IMAP pool.
///
/// One session for the whole run, borrowed on first use and returned when the
/// folder is dropped; a failed command poisons it, as every pooled op does,
/// because a half-read answer would be the next borrower's.
pub struct ImapDraftsFolder {
    config: ImapConfig,
    session: Option<PooledSession>,
    mailbox: Option<String>,
    uidplus: bool,
}

impl ImapDraftsFolder {
    pub fn new(config: ImapConfig) -> ImapDraftsFolder {
        ImapDraftsFolder {
            config,
            session: None,
            mailbox: None,
            uidplus: false,
        }
    }

    async fn pooled(&mut self) -> Result<&mut PooledSession> {
        if self.session.is_none() {
            self.session = Some(crate::imap_client::checkout(&self.config).await?);
        }
        Ok(self.session.as_mut().expect("checked out just now"))
    }

    fn mailbox(&self) -> Result<String> {
        self.mailbox
            .clone()
            .ok_or_else(|| anyhow!("the drafts folder was used before it was selected"))
    }

    /// Run one command, poisoning and dropping the session when it fails.
    fn settle<T>(&mut self, result: Result<T>) -> Result<T> {
        if result.is_err() {
            if let Some(mut session) = self.session.take() {
                session.poison();
            }
        }
        result
    }
}

impl DraftsFolder for ImapDraftsFolder {
    async fn select(&mut self, mailbox: &str) -> Result<FolderInfo> {
        let result = async {
            let pooled = self.pooled().await?;
            let uidplus = pooled.caps().uidplus;
            let selected = pooled
                .session()
                .select(mailbox)
                .await
                .map_err(|e| anyhow!("Failed to select {mailbox}: {e}"))?;
            Ok(FolderInfo {
                uidvalidity: selected.uid_validity.unwrap_or_default(),
                uidplus,
            })
        }
        .await;
        let info = self.settle(result)?;
        self.mailbox = Some(mailbox.to_string());
        self.uidplus = info.uidplus;
        Ok(info)
    }

    async fn append(&mut self, raw: &[u8]) -> Result<()> {
        let mailbox = self.mailbox()?;
        let result = async {
            self.pooled()
                .await?
                .session()
                .append(&mailbox, Some("(\\Draft \\Seen)"), None, raw)
                .await
                .map_err(|e| anyhow!("Failed to APPEND to '{mailbox}': {e}"))
        }
        .await;
        self.settle(result)
    }

    async fn uids_of(&mut self, message_id: &str) -> Result<Vec<u32>> {
        let query = format!(
            "UNDELETED HEADER Message-ID \"{}\"",
            bare_message_id(message_id).replace('"', "")
        );
        let result = async {
            let found = self
                .pooled()
                .await?
                .session()
                .uid_search(&query)
                .await
                .map_err(|e| anyhow!("UID SEARCH in the drafts folder failed: {e}"))?;
            let mut uids: Vec<u32> = found.into_iter().collect();
            uids.sort_unstable();
            Ok(uids)
        }
        .await;
        self.settle(result)
    }

    async fn delete(&mut self, uids: &[u32]) -> Result<()> {
        if uids.is_empty() {
            return Ok(());
        }
        let set = uids
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let uidplus = self.uidplus;
        let result = async {
            let session = self.pooled().await?.session();
            session
                .uid_store(&set, "+FLAGS (\\Deleted)")
                .await
                .map_err(|e| anyhow!("Failed to flag {set} deleted: {e}"))?
                .try_collect::<Vec<_>>()
                .await
                .map_err(|e| anyhow!("Failed to collect the store response: {e}"))?;
            if uidplus {
                session
                    .uid_expunge(&set)
                    .await
                    .map_err(|e| anyhow!("Failed to expunge {set}: {e}"))?
                    .try_collect::<Vec<_>>()
                    .await
                    .map_err(|e| anyhow!("Failed to collect the expunge response: {e}"))?;
            }
            Ok(())
        }
        .await;
        self.settle(result)
    }

    async fn claim(&mut self, uids: &[u32]) -> Result<Vec<u32>> {
        if uids.is_empty() {
            return Ok(Vec::new());
        }
        if !self.uidplus {
            return Err(anyhow!(
                "the server does not offer UIDPLUS, so a draft's server copy cannot be \
                 removed by its UID alone"
            ));
        }
        let result = async {
            let session = self.pooled().await?.session();
            claim_on(session, uids).await
        }
        .await;
        self.settle(result)
    }

    async fn heads(&mut self) -> Result<Vec<ServerHead>> {
        let result = async {
            let session = self.pooled().await?.session();
            let undeleted = session
                .uid_search("UNDELETED")
                .await
                .map_err(|e| anyhow!("UID SEARCH in the drafts folder failed: {e}"))?;
            if undeleted.is_empty() {
                return Ok(Vec::new());
            }
            let mut uids: Vec<u32> = undeleted.into_iter().collect();
            uids.sort_unstable();
            let set = uids
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(",");
            let fetched: Vec<_> = session
                .uid_fetch(&set, heads_query())
                .await
                .map_err(|e| anyhow!("Failed to fetch the drafts' headers: {e}"))?
                .try_collect()
                .await
                .map_err(|e| anyhow!("Failed to collect the drafts' headers: {e}"))?;
            let mut heads: Vec<ServerHead> = fetched
                .iter()
                .filter_map(|msg| {
                    Some(ServerHead {
                        uid: msg.uid?,
                        header: msg.header()?.to_vec(),
                    })
                })
                .collect();
            heads.sort_by_key(|head| head.uid);
            Ok(heads)
        }
        .await;
        self.settle(result)
    }

    async fn fetch(&mut self, uid: u32) -> Result<Option<Vec<u8>>> {
        let result = async {
            let fetched: Vec<_> = self
                .pooled()
                .await?
                .session()
                .uid_fetch(uid.to_string(), "(UID BODY.PEEK[])")
                .await
                .map_err(|e| anyhow!("Failed to fetch draft {uid}: {e}"))?
                .try_collect()
                .await
                .context("collecting a draft's bytes")?;
            Ok(fetched
                .iter()
                .find(|msg| msg.uid == Some(uid))
                .and_then(|msg| msg.body())
                .map(<[u8]>::to_vec))
        }
        .await;
        self.settle(result)
    }
}

/// How many `UID FETCH`es a claim reads its copies' sequence numbers with
/// before it gives up on a folder another client keeps expunging in.
const CLAIM_FETCH_ATTEMPTS: usize = 4;

/// Empty `session`'s unsolicited responses, answering whether an `EXPUNGE`
/// was among them.
///
/// `async-imap` sends an untagged `EXPUNGE` that arrives during any command
/// but `UID EXPUNGE` there, out of order with the command's own answer, and
/// drops what arrives once 100 are queued, which a pooled session nobody
/// drains reaches.
fn drain_unsolicited<T>(session: &async_imap::Session<T>) -> bool
where
    T: AsyncRead + AsyncWrite + Unpin + fmt::Debug,
{
    let mut expunged = false;
    while let Ok(response) = session.unsolicited_responses.try_recv() {
        expunged |= matches!(response, UnsolicitedResponse::Expunge(_));
    }
    expunged
}

/// [`DraftsFolder::claim`] on a session with the folder selected and
/// `UIDPLUS`: `UID STORE +FLAGS.SILENT (\Deleted)`, a `UID FETCH` of each
/// copy's sequence number, then `UID EXPUNGE`, whose untagged `EXPUNGE`s
/// [`claimed_by_expunges`] matches against those numbers.
///
/// The numbers must be the ones the server holds when the `UID EXPUNGE`
/// starts. An `EXPUNGE` another client caused during the `UID FETCH` reaches
/// the unsolicited channel rather than the fetch's answer, with nothing to
/// say whether it came before or after the copies' `FETCH` lines; a fetch
/// that saw one is read again, until one completes without any, so a
/// renumbering below a copy is never missed and its own `EXPUNGE` taken for
/// someone else's. The channel is emptied before the `STORE` so what earlier
/// commands left queued neither counts nor fills it.
async fn claim_on<T>(session: &mut async_imap::Session<T>, uids: &[u32]) -> Result<Vec<u32>>
where
    T: AsyncRead + AsyncWrite + Unpin + fmt::Debug + Send,
{
    let set = uids
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    drain_unsolicited(session);
    session
        .uid_store(&set, "+FLAGS.SILENT (\\Deleted)")
        .await
        .map_err(|e| anyhow!("Failed to flag {set} deleted: {e}"))?
        .try_collect::<Vec<_>>()
        .await
        .map_err(|e| anyhow!("Failed to collect the store response: {e}"))?;
    let mut pending: Option<Vec<(u32, u32)>> = None;
    for _ in 0..CLAIM_FETCH_ATTEMPTS {
        // An expunge reported during the STORE is in the numbers this fetch
        // reads, so it does not count.
        drain_unsolicited(session);
        let fetched: Vec<_> = session
            .uid_fetch(&set, "(UID)")
            .await
            .map_err(|e| anyhow!("Failed to fetch {set}: {e}"))?
            .try_collect()
            .await
            .map_err(|e| anyhow!("Failed to collect the fetch response: {e}"))?;
        if drain_unsolicited(session) {
            continue;
        }
        pending = Some(
            fetched
                .iter()
                .filter_map(|msg| Some((msg.message, msg.uid?)))
                .filter(|(_, uid)| uids.contains(uid))
                .collect(),
        );
        break;
    }
    let Some(mut pending) = pending else {
        return Err(anyhow!(
            "another client expunged in the folder during each of {CLAIM_FETCH_ATTEMPTS} reads \
             of {set}'s sequence numbers, which were flagged deleted but not expunged"
        ));
    };
    let expunged: Vec<u32> = session
        .uid_expunge(&set)
        .await
        .map_err(|e| anyhow!("Failed to expunge {set}: {e}"))?
        .try_collect()
        .await
        .map_err(|e| anyhow!("Failed to collect the expunge response: {e}"))?;
    Ok(claimed_by_expunges(&mut pending, &expunged))
}

/// The UIDs among `pending` (sequence number, UID) that the untagged
/// `EXPUNGE` responses `expunged` removed, in the order the server sent them.
///
/// Each `EXPUNGE n` renumbers every message above `n` down by one at once
/// (RFC 3501, 7.4.1), so a copy is matched by its sequence number as the
/// expunges before it left it, and an expunge of some other message below it
/// (another client's, reported during this command) moves it down rather
/// than being taken for it.
fn claimed_by_expunges(pending: &mut Vec<(u32, u32)>, expunged: &[u32]) -> Vec<u32> {
    let mut claimed = Vec::new();
    for &seq in expunged {
        if let Some(at) = pending.iter().position(|(s, _)| *s == seq) {
            claimed.push(pending.remove(at).1);
        }
        for (s, _) in pending.iter_mut() {
            if *s > seq {
                *s -= 1;
            }
        }
    }
    claimed.sort_unstable();
    claimed
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An expunge reported for another message below a copy moves the copy
    /// down one; the copy is claimed only by the expunge of its own number.
    #[test]
    fn a_claim_follows_the_renumbering_of_each_expunge() {
        // Copies 120 at 5 and 130 at 9; another client's message at 3 goes
        // first, then 5 (now 4), then 9 (now 8, then 7).
        let mut pending = vec![(5, 120), (9, 130)];
        assert_eq!(
            claimed_by_expunges(&mut pending, &[3, 4, 7]),
            vec![120, 130]
        );
        // Only another client's expunge, at the copy's old number after a
        // shift: not this session's claim.
        let mut pending = vec![(5, 120)];
        assert_eq!(
            claimed_by_expunges(&mut pending, &[2, 5]),
            Vec::<u32>::new()
        );
        let mut pending = vec![(5, 120)];
        assert_eq!(claimed_by_expunges(&mut pending, &[]), Vec::<u32>::new());
    }

    /// A server played from a script: each command line written takes the
    /// next reply, its `{tag}` replaced by the command's tag, and a read past
    /// the last reply is the connection closing.
    #[derive(Debug)]
    struct Scripted {
        replies: std::collections::VecDeque<String>,
        written: Vec<u8>,
        out: std::collections::VecDeque<u8>,
    }

    impl AsyncRead for Scripted {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            buf: &mut [u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            let n = buf.len().min(self.out.len());
            for (slot, byte) in buf.iter_mut().zip(self.out.drain(..n)) {
                *slot = byte;
            }
            std::task::Poll::Ready(Ok(n))
        }
    }

    impl AsyncWrite for Scripted {
        fn poll_write(
            mut self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            buf: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            self.written.extend_from_slice(buf);
            while let Some(end) = self.written.windows(2).position(|w| w == b"\r\n") {
                let line: Vec<u8> = self.written.drain(..end + 2).collect();
                let line = String::from_utf8_lossy(&line).to_string();
                let tag = line.split(' ').next().unwrap_or_default().to_string();
                if let Some(reply) = self.replies.pop_front() {
                    let reply = reply.replace("{tag}", &tag);
                    self.out.extend(reply.into_bytes());
                }
            }
            std::task::Poll::Ready(Ok(buf.len()))
        }

        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }

        fn poll_close(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    /// A logged-in session on a [`Scripted`] server whose replies after the
    /// `LOGIN`'s are `replies`.
    fn scripted(replies: &[&str]) -> async_imap::Session<Scripted> {
        let mut all = vec!["{tag} OK LOGIN completed\r\n".to_string()];
        all.extend(replies.iter().map(|r| r.to_string()));
        let stream = Scripted {
            replies: all.into(),
            written: Vec::new(),
            out: Default::default(),
        };
        futures::executor::block_on(async_imap::Client::new(stream).login("me", "pw"))
            .map_err(|(e, _)| e)
            .expect("the scripted login")
    }

    const STORED: &str = "{tag} OK STORE completed\r\n";

    /// Another client expunges a message below the copy while the claim
    /// reads the copy's number: the read is done again, so the copy's own
    /// `EXPUNGE`, one lower, is still recognised as this session's.
    #[test]
    fn an_expunge_below_the_copy_during_the_fetch_still_claims_it() {
        let mut session = scripted(&[
            STORED,
            "* 5 FETCH (UID 120)\r\n* 3 EXPUNGE\r\n{tag} OK FETCH completed\r\n",
            "* 4 FETCH (UID 120)\r\n{tag} OK FETCH completed\r\n",
            "* 4 EXPUNGE\r\n{tag} OK EXPUNGE completed\r\n",
        ]);
        let claimed = futures::executor::block_on(claim_on(&mut session, &[120])).unwrap();
        assert_eq!(claimed, vec![120]);
    }

    /// What earlier commands left queued, an `EXPUNGE` among them and enough
    /// to fill the channel, is neither taken for a renumbering during the
    /// claim nor stops one from being heard.
    #[test]
    fn a_claim_starts_from_an_empty_unsolicited_queue() {
        let mut noop = "* 2 EXPUNGE\r\n".to_string();
        for _ in 0..120 {
            noop.push_str("* 9 EXISTS\r\n");
        }
        noop.push_str("{tag} OK NOOP completed\r\n");
        let mut session = scripted(&[
            &noop,
            STORED,
            "* 5 FETCH (UID 120)\r\n* 3 EXPUNGE\r\n{tag} OK FETCH completed\r\n",
            "* 4 FETCH (UID 120)\r\n{tag} OK FETCH completed\r\n",
            "* 4 EXPUNGE\r\n{tag} OK EXPUNGE completed\r\n",
        ]);
        futures::executor::block_on(session.noop()).unwrap();
        let claimed = futures::executor::block_on(claim_on(&mut session, &[120])).unwrap();
        assert_eq!(claimed, vec![120]);

        // Only stale ones: one read is enough.
        let mut session = scripted(&[
            "* 2 EXPUNGE\r\n{tag} OK NOOP completed\r\n",
            STORED,
            "* 5 FETCH (UID 120)\r\n{tag} OK FETCH completed\r\n",
            "* 5 EXPUNGE\r\n{tag} OK EXPUNGE completed\r\n",
        ]);
        futures::executor::block_on(session.noop()).unwrap();
        let claimed = futures::executor::block_on(claim_on(&mut session, &[120])).unwrap();
        assert_eq!(claimed, vec![120]);
    }

    /// A folder that renumbers during every read gives no number to match,
    /// so the claim fails rather than guess; a connection that drops once the
    /// copy is flagged claims nothing (`async-imap` ends a stream at EOF
    /// without an error), which the guard refuses the same way.
    #[test]
    fn a_claim_without_a_steady_number_or_a_connection_fails() {
        let moving = "* 5 FETCH (UID 120)\r\n* 1 EXPUNGE\r\n{tag} OK FETCH completed\r\n";
        let mut replies = vec![STORED];
        replies.extend([moving; CLAIM_FETCH_ATTEMPTS]);
        let mut session = scripted(&replies);
        let failed = futures::executor::block_on(claim_on(&mut session, &[120])).unwrap_err();
        assert!(failed.to_string().contains("expunged"), "{failed:#}");

        let mut session = scripted(&[STORED]);
        let dropped = futures::executor::block_on(claim_on(&mut session, &[120]));
        assert!(dropped.as_ref().map_or(true, Vec::is_empty), "{dropped:?}");
    }

    /// The listing asks for the linking headers by name, so the draft source
    /// a copy carries never rides along.
    #[test]
    fn the_listing_fetches_only_the_linking_headers() {
        let query = heads_query();
        assert!(
            query.starts_with("(UID BODY.PEEK[HEADER.FIELDS ("),
            "{query}"
        );
        for field in HEAD_FIELDS {
            assert!(query.contains(&field.to_uppercase()), "{field} in {query}");
        }
        assert!(!query
            .to_uppercase()
            .contains(&super::super::carrier::SOURCE_HEADER.to_uppercase()));
    }
}

/// An in-memory Drafts folder for the mirror's tests: every operation the
/// trait has, a log of the mutating ones, and switches that make one fail.
#[cfg(test)]
pub mod fake {
    use super::*;

    /// One message of the fake folder.
    #[derive(Clone, Debug)]
    pub struct Copy {
        pub uid: u32,
        pub raw: Vec<u8>,
        pub deleted: bool,
    }

    #[derive(Debug)]
    pub struct FakeFolder {
        pub name: String,
        pub uidvalidity: u32,
        pub uidplus: bool,
        pub next_uid: u32,
        pub copies: Vec<Copy>,
        /// `append`, `delete <uids>`, in order.
        pub log: Vec<String>,
        pub fail_append: bool,
        pub fail_select: bool,
        /// UIDs a listing does not show, as a server that answered a
        /// `SEARCH` without them for one round would.
        pub hidden: Vec<u32>,
        /// A file and the text appended to it at the next `fetch`: a save in
        /// the editor while the mirror waits on the network.
        pub touch_on_fetch: Option<(std::path::PathBuf, String)>,
        /// Another session expunges the copies a `claim` names just before
        /// this one's `UID EXPUNGE`: mp on the other machine sending the
        /// same draft at the same moment.
        pub claimed_elsewhere: bool,
        /// A `claim` removes the copies it names and then fails: the
        /// connection dropping after the `UID EXPUNGE`.
        pub fail_claim: bool,
    }

    impl Default for FakeFolder {
        fn default() -> Self {
            FakeFolder {
                name: "Drafts".to_string(),
                uidvalidity: 7,
                uidplus: true,
                next_uid: 100,
                copies: Vec::new(),
                log: Vec::new(),
                fail_append: false,
                fail_select: false,
                hidden: Vec::new(),
                touch_on_fetch: None,
                claimed_elsewhere: false,
                fail_claim: false,
            }
        }
    }

    impl FakeFolder {
        /// Put a message in the folder as another client would, returning its
        /// UID.
        pub fn put(&mut self, raw: &[u8]) -> u32 {
            let uid = self.next_uid;
            self.next_uid += 1;
            self.copies.push(Copy {
                uid,
                raw: raw.to_vec(),
                deleted: false,
            });
            uid
        }

        /// The copies a client sees: not flagged deleted.
        pub fn live(&self) -> Vec<&Copy> {
            self.copies.iter().filter(|c| !c.deleted).collect()
        }

        /// The copies a listing shows: live and not hidden.
        fn listed(&self) -> Vec<&Copy> {
            self.live()
                .into_iter()
                .filter(|c| !self.hidden.contains(&c.uid))
                .collect()
        }

        pub fn remove(&mut self, uid: u32) {
            self.copies.retain(|c| c.uid != uid);
        }
    }

    impl DraftsFolder for FakeFolder {
        async fn select(&mut self, mailbox: &str) -> Result<FolderInfo> {
            if self.fail_select {
                return Err(anyhow!("the fake server refused SELECT"));
            }
            assert_eq!(mailbox, self.name, "the mirror selects the drafts folder");
            Ok(FolderInfo {
                uidvalidity: self.uidvalidity,
                uidplus: self.uidplus,
            })
        }

        async fn append(&mut self, raw: &[u8]) -> Result<()> {
            if self.fail_append {
                return Err(anyhow!("the fake server refused APPEND"));
            }
            self.log.push("append".to_string());
            self.put(raw);
            Ok(())
        }

        async fn uids_of(&mut self, message_id: &str) -> Result<Vec<u32>> {
            let wanted = bare_message_id(message_id).to_string();
            Ok(self
                .listed()
                .iter()
                .filter(|c| {
                    header_value(&c.raw, "Message-ID")
                        .is_some_and(|mid| bare_message_id(&mid) == wanted)
                })
                .map(|c| c.uid)
                .collect())
        }

        async fn delete(&mut self, uids: &[u32]) -> Result<()> {
            self.log.push(format!("delete {uids:?}"));
            let uidplus = self.uidplus;
            for copy in &mut self.copies {
                if uids.contains(&copy.uid) {
                    copy.deleted = true;
                }
            }
            if uidplus {
                self.copies
                    .retain(|c| !(c.deleted && uids.contains(&c.uid)));
            }
            Ok(())
        }

        async fn claim(&mut self, uids: &[u32]) -> Result<Vec<u32>> {
            if !self.uidplus {
                return Err(anyhow!("the fake server offers no UIDPLUS"));
            }
            self.log.push(format!("claim {uids:?}"));
            let present: Vec<u32> = self
                .live()
                .iter()
                .map(|c| c.uid)
                .filter(|uid| uids.contains(uid))
                .collect();
            self.copies.retain(|c| !uids.contains(&c.uid));
            if self.fail_claim {
                return Err(anyhow!("the fake server dropped the connection"));
            }
            if self.claimed_elsewhere {
                return Ok(Vec::new());
            }
            Ok(present)
        }

        /// The [`HEAD_FIELDS`] of every listed copy, as a server answers
        /// `BODY.PEEK[HEADER.FIELDS (...)]`: those header lines, folds kept,
        /// and the blank line.
        async fn heads(&mut self) -> Result<Vec<ServerHead>> {
            Ok(self
                .listed()
                .iter()
                .map(|c| {
                    let end = c
                        .raw
                        .windows(4)
                        .position(|w| w == b"\r\n\r\n")
                        .map(|at| at + 2)
                        .unwrap_or(c.raw.len());
                    let mut header = Vec::new();
                    let mut keep = false;
                    for line in c.raw[..end].split_inclusive(|b| *b == b'\n') {
                        if !line.starts_with(b" ") && !line.starts_with(b"\t") {
                            let name = line.split(|b| *b == b':').next().unwrap_or_default();
                            keep = HEAD_FIELDS
                                .iter()
                                .any(|f| f.as_bytes().eq_ignore_ascii_case(name));
                        }
                        if keep {
                            header.extend_from_slice(line);
                        }
                    }
                    header.extend_from_slice(b"\r\n");
                    ServerHead { uid: c.uid, header }
                })
                .collect())
        }

        async fn fetch(&mut self, uid: u32) -> Result<Option<Vec<u8>>> {
            if let Some((path, text)) = self.touch_on_fetch.take() {
                let mut bytes = std::fs::read(&path)?;
                bytes.extend_from_slice(text.as_bytes());
                std::fs::write(&path, bytes)?;
            }
            Ok(self
                .listed()
                .iter()
                .find(|c| c.uid == uid)
                .map(|c| c.raw.clone()))
        }
    }
}
