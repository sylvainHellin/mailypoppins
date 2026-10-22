//! The server Drafts folder as the mirror sees it: six operations behind a
//! trait, so the push, the delete and the reconcile run offline against a fake
//! and against IMAP in the daemon (#0143).

use std::future::Future;

use anyhow::{anyhow, Context, Result};
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

#[cfg(test)]
mod tests {
    use super::*;

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
