//! The server side of a mutation, at library layer.
//!
//! A flag, a move, an archive or a delete is one local store write and one
//! remote op. The local half lives in [`crate::store::write`]; this module owns
//! the remote half: [`ServerOp`] names what the server has to do, and
//! [`run_op`] executes it against IMAP or Graph.
//!
//! It sits in the library rather than in `tui/` on purpose. The TUI's mutation
//! pairing ([`crate::tui::mutations`]) built these types first, but the durable
//! `pending_ops` queue ([`crate::pending_ops`], #0039) drains the same ops from
//! a background engine that has no terminal, and the CLI issues them too. A
//! remote op is email logic, which the TUI-implements-no-email-logic invariant
//! keeps out of `tui/`; the queue and the TUI both reference it here, and
//! `tui::mutations` re-exports it so the TUI call sites read unchanged.
//!
//! Every op names the message by `Message-ID`, the one handle both backends
//! share and the one that survives the local row moving. A move, a delete and
//! the two flag toggles also carry the [`ServerCopy`] the store row stood for
//! when the op was queued (PERSO-238): a folder can hold two copies under one
//! `Message-ID`, and the IMAP arm acts on the row's own UID rather than on
//! whichever copy a `HEADER Message-ID` search lists first. Graph has no UIDs
//! and ignores it; a Graph move, delete and read toggle carry the row's Graph
//! item id instead ([`GraphCopy`], PERSO-245), and act on that item.

use std::error::Error as StdError;
use std::fmt;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::config::{AccountConfig, AuthMethod, GraphConfig, ImapConfig};

/// A backend op found no message to act on: the server's copy is already gone.
///
/// A *typed* not-found signal, so the durable queue's drain
/// ([`crate::pending_ops`], #0039 review) can tell "the target is already in
/// the desired state" from a real transport failure without matching on error
/// strings. A move or delete that succeeded on the server, then crashed before
/// its queue row retired, replays against a message the source folder no longer
/// holds; the backend reports that as this error and the drain treats it as a
/// converged op rather than parking a success as `failed`.
///
/// Direct CLI and TUI call sites keep seeing it as an ordinary error through
/// [`std::fmt::Display`], so a user acting on a message the server no longer
/// holds still gets the same message they always did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotFoundOnServer {
    /// The `Message-ID` the op named.
    pub message_id: String,
    /// The source folder searched, when the backend names one: IMAP, and a
    /// Graph move or delete (PERSO-245). The Graph read toggle and post-send
    /// mark leave it out.
    pub mailbox: Option<String>,
}

impl fmt::Display for NotFoundOnServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.mailbox {
            Some(mailbox) => write!(
                f,
                "Email with Message-ID {} not found in {} on server",
                self.message_id, mailbox
            ),
            None => write!(f, "Message {} not found on server", self.message_id),
        }
    }
}

impl StdError for NotFoundOnServer {}

impl NotFoundOnServer {
    /// True when `err` is, or wraps, a not-found signal from either backend.
    /// The drain uses this to converge a replay instead of failing it.
    pub fn is_in(err: &anyhow::Error) -> bool {
        err.downcast_ref::<NotFoundOnServer>().is_some()
    }
}

/// The one server copy a per-message op acts on (PERSO-238).
///
/// A folder can hold two copies of a message under one `Message-ID` (a move
/// that ran twice, PERSO-236, or a client that copied it), and the store keeps
/// one row per copy. The row's UID is what tells them apart, so the op carries
/// it, with the folder's UIDVALIDITY when a sync had recorded one: a UID only
/// names a message within one UIDVALIDITY.
///
/// Absent from an op whose row had no server UID yet (the `-id` sentinel of a
/// local move, the 63-bit synthetic uid of a Graph row or of an APPEND without
/// `APPENDUID`), and from a payload queued before the field existed: the IMAP
/// arm then searches by `Message-ID` alone
/// (`crate::imap_client::ops::resolve_copy` has the whole rule).
///
/// `siblings` are the UIDs the store bound to its *other* rows in the same
/// folder under the same `Message-ID` when the op was queued. A UID that went
/// stale (another client moved the message out and back) leaves the op's own
/// UID naming nothing, and the IMAP arm may then take the folder's one copy of
/// the message for it, but never a copy one of these rows stands for. Empty,
/// and absent from the payload, when the row has no twin, and in a payload
/// queued before the field existed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerCopy {
    pub uid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uidvalidity: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub siblings: Vec<u32>,
}

impl ServerCopy {
    /// The copy a store row names: its `uid`, when that is a UID a server
    /// issued, and the UIDVALIDITY its mailbox was last synced under.
    pub fn of_row(uid: i64, uidvalidity: Option<i64>) -> Option<ServerCopy> {
        let uid = server_uid(uid)?;
        Some(ServerCopy {
            uid,
            uidvalidity: uidvalidity.and_then(|v| u32::try_from(v).ok()),
            siblings: Vec::new(),
        })
    }

    /// The same copy, with the store's other rows of the message in its
    /// folder bound to `uids`; a uid no server issued is left out.
    pub fn with_siblings(self, uids: impl IntoIterator<Item = i64>) -> ServerCopy {
        let mut siblings: Vec<u32> = uids
            .into_iter()
            .filter_map(server_uid)
            .filter(|uid| *uid != self.uid)
            .collect();
        siblings.sort_unstable();
        siblings.dedup();
        ServerCopy { siblings, ..self }
    }
}

/// A store `uid` as the UID a server issued, `None` for a sentinel or a
/// synthetic uid.
fn server_uid(uid: i64) -> Option<u32> {
    u32::try_from(uid).ok().filter(|uid| *uid > 0)
}

/// The one Graph item a per-message op acts on (PERSO-245): the Graph analogue
/// of [`ServerCopy`].
///
/// A Graph row has no UID (its `uid` is a hash of the `Message-ID`), so the
/// sync records the Graph id of the folder's item on the row instead
/// (`messages.graph_id`, [`crate::ingest::bind_graph_ids`]) and the op carries
/// it. The id is Graph's default one, the kind every listing returns, which
/// names one item in one folder and changes when the item moves; a row a local
/// move took elsewhere has none until the destination is listed
/// ([`crate::store::write::move_row`]).
///
/// `siblings` are the ids the store bound to its other rows of the message in
/// the same folder, never taken for this one. Absent from an op whose row had
/// no id, and from a payload queued before the field existed: the Graph arm
/// then acts only on a folder that holds one copy
/// (`crate::graph_ops::resolve_item` has the whole rule).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphCopy {
    pub id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub siblings: Vec<String>,
}

impl GraphCopy {
    /// The item a row names by `id`, with its twins in the folder bound to
    /// `siblings`; the row's own id is no sibling.
    pub fn of_row(id: String, siblings: impl IntoIterator<Item = String>) -> GraphCopy {
        let mut siblings: Vec<String> = siblings.into_iter().filter(|s| *s != id).collect();
        siblings.sort_unstable();
        siblings.dedup();
        GraphCopy { id, siblings }
    }
}

/// What the background thread (or the queue drain) will ask the server to do.
///
/// Serializable so the durable [`crate::pending_ops`] queue can persist it in a
/// row's payload and replay it after a crash (#0039).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerOp {
    /// Move between server folders. Archiving is this with the archive folder
    /// as destination, exactly as `move_email_on_server` has always framed it.
    ///
    /// `copy`, here and on the three ops below, is the copy in the source
    /// folder the store row stood for ([`ServerCopy`], PERSO-238), and
    /// `graph_copy`, here and on the two ops below, the same on a Graph
    /// account ([`GraphCopy`], PERSO-245).
    Move {
        message_id: String,
        source_mailbox: String,
        dest_mailbox: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        copy: Option<ServerCopy>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        graph_copy: Option<GraphCopy>,
    },
    Delete {
        message_id: String,
        source_mailbox: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        copy: Option<ServerCopy>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        graph_copy: Option<GraphCopy>,
    },
    SetRead {
        message_id: String,
        mailbox: String,
        read: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        copy: Option<ServerCopy>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        graph_copy: Option<GraphCopy>,
    },
    /// Toggle the `\Flagged` star (#0007). Like [`ServerOp::SetRead`], the
    /// server is truth on the IMAP path, so a local toggle needs a UID STORE
    /// write the next sync restates.
    SetFlagged {
        message_id: String,
        mailbox: String,
        flagged: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        copy: Option<ServerCopy>,
    },
    /// The post-send bookkeeping bit on the source of a reply or a forward:
    /// `\Answered` when `answered`, `$Forwarded` otherwise (#TKT-0051, #0076).
    ///
    /// The one multi-mailbox op, because a source is one store row per mailbox
    /// (inbox, archive, sent) and the same server message in each: naming the
    /// whole list lets the drain write them over a single IMAP session instead
    /// of one login per mailbox, which is the cost #0076 exists to remove.
    ///
    /// Idempotent by construction (`+FLAGS` on a flag that may already be set)
    /// and not-found tolerant per mailbox, so a replay after a crash converges
    /// rather than failing.
    ///
    /// On Graph the op writes Outlook's MAPI history properties on every copy
    /// of the message instead (PERSO-171), and `reply_all` picks the verb it
    /// records: Exchange tells a reply from a reply to all, IMAP does not, so
    /// the IMAP arm ignores it. Absent from a payload queued before the field
    /// existed, which reads as a plain reply.
    SetAnswered {
        message_id: String,
        mailboxes: Vec<String>,
        answered: bool,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        reply_all: bool,
    },
    /// Replace a draft's copy in the server's Drafts folder with the file as
    /// it is when the drain runs (#0143).
    ///
    /// It names the draft and carries no bytes: the drain reads the file, so a
    /// burst of saves is one APPEND of the last state, and there is no blob to
    /// write before the queue row. The run is [`crate::draft_mirror`]'s, not
    /// [`run_op`]'s, because it reads the file and writes the store's
    /// `draft_sync` row.
    ///
    /// `hash` is the hash of the file the push was queued for, kept up to
    /// date while it waits: a push parked `failed` is not queued again for
    /// the same bytes, so only the next edit retries it. Absent from a row
    /// queued before the field existed.
    PushDraft {
        draft_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hash: Option<String>,
    },
    /// Remove every copy of one draft from a server folder (#0143): after a
    /// send that reached every recipient, after `draft.discard`, and for a
    /// copy whose draft was retired. `uid` and `uidvalidity` are the copy the
    /// draft's `draft_sync` row named, kept for the log; the delete finds the
    /// copies by `message_id`, which every copy of the draft carries.
    DeleteDraft {
        mailbox: String,
        message_id: String,
        #[serde(default)]
        uid: Option<u32>,
        #[serde(default)]
        uidvalidity: Option<u32>,
    },
}

impl ServerOp {
    /// The `pending_ops.kind` string for this op (#0039). The five kinds the
    /// ticket names map onto these four variants: archive is a `move` with the
    /// archive folder as destination, and mark-read / mark-unread are one
    /// `SetRead` with the `read` bit.
    pub fn kind(&self) -> &'static str {
        match self {
            ServerOp::Move { .. } => "move",
            ServerOp::Delete { .. } => "delete",
            ServerOp::SetRead { .. } => "set_read",
            ServerOp::SetFlagged { .. } => "set_flagged",
            ServerOp::SetAnswered { .. } => "set_answered",
            ServerOp::PushDraft { .. } => "push_draft",
            ServerOp::DeleteDraft { .. } => "delete_draft",
        }
    }

    /// Whether the op is the drafts mirror's (#0143), which the drain runs
    /// through [`crate::draft_mirror`] rather than [`run_op`].
    pub fn is_draft_op(&self) -> bool {
        matches!(
            self,
            ServerOp::PushDraft { .. } | ServerOp::DeleteDraft { .. }
        )
    }

    /// The message this op names: a draft push names its draft's id, since
    /// its copy has no `Message-ID` before the first push.
    pub fn message_id(&self) -> &str {
        match self {
            ServerOp::Move { message_id, .. }
            | ServerOp::Delete { message_id, .. }
            | ServerOp::SetRead { message_id, .. }
            | ServerOp::SetFlagged { message_id, .. }
            | ServerOp::SetAnswered { message_id, .. }
            | ServerOp::DeleteDraft { message_id, .. } => message_id,
            ServerOp::PushDraft { draft_id, .. } => draft_id,
        }
    }
}

/// The backend a batch of ops runs against, resolved before the spawn so a
/// missing config never leaves a half-applied mutation behind.
pub enum Backend {
    Imap(Box<ImapConfig>),
    Graph(Box<GraphConfig>),
}

impl Backend {
    /// The backend an account's owed ops run against, loaded from its config.
    ///
    /// The account's `auth_method` decides the transport, not whichever config
    /// happens to be present: a Graph account drains over Graph or not at all.
    /// This is the resolver the durable queue's background drain
    /// ([`crate::pending_ops::resume_account`]) uses, the config-side twin of
    /// the TUI's `backend_for_mutation`, so a drain from `mp sync` or a
    /// terminal-less engine builds the same backend the interactive path does.
    pub fn resolve(account: &AccountConfig) -> Result<Backend> {
        if account.auth_method == AuthMethod::Graph {
            Ok(Backend::Graph(Box::new(GraphConfig::load(account)?)))
        } else {
            Ok(Backend::Imap(Box::new(ImapConfig::load(account)?)))
        }
    }
}

/// Run a single op against `backend`. The durable queue drains one row at a
/// time, so it uses this rather than the batch form (#0039).
pub async fn run_op(backend: &Backend, op: &ServerOp) -> Result<()> {
    match backend {
        Backend::Graph(config) => run_graph(config, op).await,
        Backend::Imap(config) => run_imap(config, op).await,
    }
}

async fn run_imap(config: &ImapConfig, op: &ServerOp) -> Result<()> {
    match op {
        ServerOp::Move {
            message_id,
            source_mailbox,
            dest_mailbox,
            copy,
            ..
        } => {
            crate::imap_client::move_email_on_server(
                config,
                message_id,
                source_mailbox,
                copy.as_ref(),
                dest_mailbox,
            )
            .await
        }
        ServerOp::Delete {
            message_id,
            source_mailbox,
            copy,
            ..
        } => {
            crate::imap_client::delete_email_on_server(
                config,
                message_id,
                source_mailbox,
                copy.as_ref(),
            )
            .await
        }
        ServerOp::SetRead {
            message_id,
            mailbox,
            read,
            copy,
            ..
        } => {
            if *read {
                crate::imap_client::mark_read_on_server(config, message_id, mailbox, copy.as_ref())
                    .await
            } else {
                crate::imap_client::mark_unread_on_server(
                    config,
                    message_id,
                    mailbox,
                    copy.as_ref(),
                )
                .await
            }
        }
        ServerOp::SetFlagged {
            message_id,
            mailbox,
            flagged,
            copy,
        } => {
            let flag = crate::types::FLAG_FLAGGED;
            if *flagged {
                crate::imap_client::add_flag_on_server(
                    config,
                    message_id,
                    mailbox,
                    copy.as_ref(),
                    flag,
                )
                .await
            } else {
                crate::imap_client::remove_flag_on_server(
                    config,
                    message_id,
                    mailbox,
                    copy.as_ref(),
                    flag,
                )
                .await
            }
        }
        ServerOp::SetAnswered {
            message_id,
            mailboxes,
            answered,
            ..
        } => {
            let flag = if *answered {
                crate::types::FLAG_ANSWERED
            } else {
                crate::types::FLAG_FORWARDED
            };
            crate::imap_client::add_flag_in_mailboxes(config, message_id, mailboxes, flag).await
        }
        ServerOp::PushDraft { .. } | ServerOp::DeleteDraft { .. } => Err(draft_op_refusal(op)),
    }
}

/// A draft op handed to [`run_op`], which cannot run one: it needs the draft
/// file and the store, and the queue's drain routes it to
/// [`crate::draft_mirror`] instead.
fn draft_op_refusal(op: &ServerOp) -> anyhow::Error {
    anyhow::anyhow!(
        "{} is the drafts mirror's op and runs through the queue's drain",
        op.kind()
    )
}

async fn run_graph(config: &GraphConfig, op: &ServerOp) -> Result<()> {
    match op {
        ServerOp::Move {
            message_id,
            source_mailbox,
            dest_mailbox,
            graph_copy,
            ..
        } => {
            crate::graph_ops::move_message(
                config,
                message_id,
                source_mailbox,
                graph_copy.as_ref(),
                dest_mailbox,
            )
            .await
        }
        ServerOp::Delete {
            message_id,
            source_mailbox,
            graph_copy,
            ..
        } => {
            crate::graph_ops::delete_message(
                config,
                message_id,
                source_mailbox,
                graph_copy.as_ref(),
            )
            .await
        }
        ServerOp::SetRead {
            message_id,
            mailbox,
            read,
            graph_copy,
            ..
        } => {
            crate::graph_ops::mark_read(config, message_id, mailbox, graph_copy.as_ref(), *read)
                .await
        }
        // Graph stays seen-only (#0007): the star is parked locally, honoured
        // by the store and the list, but never mirrored to Graph. A no-op Ok
        // rather than an error, so the optimistic local write stands.
        ServerOp::SetFlagged { message_id, .. } => {
            log::debug!("[graph] flag parked locally for {message_id}; Graph is seen-only");
            Ok(())
        }
        // Graph has no answered flag; Outlook keeps the reply and forward
        // history in MAPI properties, which Exchange accepts through a PATCH
        // (PERSO-171, measured on the EVOQS account on 2026-10-22). The
        // folder list is not read: every copy of the message is written.
        ServerOp::SetAnswered {
            message_id,
            answered,
            reply_all,
            ..
        } => {
            let verb = crate::graph::LastVerb::of_send(*answered, *reply_all);
            crate::graph::mark_last_verb_graph(config, message_id, verb).await
        }
        ServerOp::PushDraft { .. } | ServerOp::DeleteDraft { .. } => Err(draft_op_refusal(op)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mv(id: &str, source: &str, dest: &str) -> ServerOp {
        ServerOp::Move {
            message_id: id.to_string(),
            source_mailbox: source.to_string(),
            dest_mailbox: dest.to_string(),
            copy: None,
            graph_copy: None,
        }
    }

    fn del(id: &str, source: &str) -> ServerOp {
        ServerOp::Delete {
            message_id: id.to_string(),
            source_mailbox: source.to_string(),
            copy: None,
            graph_copy: None,
        }
    }

    /// The kind strings are the `pending_ops.kind` column values (#0039), and
    /// every op names its message.
    #[test]
    fn kind_and_message_id_are_stable() {
        assert_eq!(mv("<1@x>", "INBOX", "Archive").kind(), "move");
        assert_eq!(del("<1@x>", "INBOX").kind(), "delete");
        assert_eq!(
            ServerOp::SetRead {
                message_id: "<1@x>".to_string(),
                mailbox: "INBOX".to_string(),
                read: false,
                copy: None,
                graph_copy: None,
            }
            .kind(),
            "set_read"
        );
        assert_eq!(
            ServerOp::SetAnswered {
                message_id: "<1@x>".to_string(),
                mailboxes: vec!["INBOX".to_string()],
                answered: true,
                reply_all: false,
            }
            .kind(),
            "set_answered"
        );
        assert_eq!(mv("<abc@x>", "INBOX", "Archive").message_id(), "<abc@x>");
        let push = ServerOp::PushDraft {
            draft_id: "d1".to_string(),
            hash: None,
        };
        assert_eq!((push.kind(), push.message_id()), ("push_draft", "d1"));
        assert!(push.is_draft_op() && !mv("<1@x>", "INBOX", "Archive").is_draft_op());
        assert_eq!(
            ServerOp::SetAnswered {
                message_id: "<abc@x>".to_string(),
                mailboxes: Vec::new(),
                answered: false,
                reply_all: false,
            }
            .message_id(),
            "<abc@x>"
        );
    }

    /// The op round-trips through JSON, which is what the durable queue stores
    /// in the row payload and replays after a crash (#0039).
    #[test]
    fn a_server_op_round_trips_through_json() {
        for op in [
            mv("<1@x>", "INBOX", "Archive"),
            del("<2@x>", "Archive"),
            ServerOp::SetRead {
                message_id: "<3@x>".to_string(),
                mailbox: "INBOX".to_string(),
                read: true,
                copy: Some(ServerCopy {
                    uid: 286,
                    uidvalidity: Some(7),
                    siblings: Vec::new(),
                }),
                graph_copy: None,
            },
            ServerOp::SetFlagged {
                message_id: "<4@x>".to_string(),
                mailbox: "INBOX".to_string(),
                flagged: false,
                copy: Some(ServerCopy {
                    uid: 12,
                    uidvalidity: None,
                    siblings: Vec::new(),
                }),
            },
            ServerOp::SetAnswered {
                message_id: "<5@x>".to_string(),
                mailboxes: vec!["INBOX".to_string(), "Archive".to_string()],
                answered: true,
                reply_all: false,
            },
            ServerOp::SetAnswered {
                message_id: "<6@x>".to_string(),
                mailboxes: vec!["inbox".to_string()],
                answered: true,
                reply_all: true,
            },
            ServerOp::PushDraft {
                draft_id: "d1".to_string(),
                hash: Some("ab".repeat(32)),
            },
            ServerOp::DeleteDraft {
                mailbox: "Drafts".to_string(),
                message_id: "<mp.1@x>".to_string(),
                uid: Some(18564),
                uidvalidity: Some(7),
            },
        ] {
            let json = serde_json::to_string(&op).unwrap();
            assert_eq!(serde_json::from_str::<ServerOp>(&json).unwrap(), op);
        }
    }

    /// A `set_answered` payload a build before PERSO-171 queued carries no
    /// `reply_all` and still decodes, as a plain reply; a plain reply writes the
    /// payload it always did.
    #[test]
    fn an_answered_op_queued_before_reply_all_existed_still_decodes() {
        let old = r#"{"SetAnswered":{"message_id":"<7@x>","mailboxes":["INBOX"],"answered":true}}"#;
        let op = ServerOp::SetAnswered {
            message_id: "<7@x>".to_string(),
            mailboxes: vec!["INBOX".to_string()],
            answered: true,
            reply_all: false,
        };
        assert_eq!(serde_json::from_str::<ServerOp>(old).unwrap(), op);
        assert_eq!(serde_json::to_string(&op).unwrap(), old);
    }

    /// A move, delete or flag op queued before PERSO-238 carries no `copy` and
    /// still decodes, as an op that searches by `Message-ID`; an op whose row
    /// had no server UID writes the payload it always did, so nothing queued
    /// needs migrating either way.
    #[test]
    fn an_op_queued_before_the_copy_existed_still_decodes() {
        let old =
            r#"{"Move":{"message_id":"<8@x>","source_mailbox":"INBOX","dest_mailbox":"Archive"}}"#;
        let op = mv("<8@x>", "INBOX", "Archive");
        assert_eq!(serde_json::from_str::<ServerOp>(old).unwrap(), op);
        assert_eq!(serde_json::to_string(&op).unwrap(), old);

        let with_copy = ServerOp::Delete {
            message_id: "<9@x>".to_string(),
            source_mailbox: "Archive".to_string(),
            copy: Some(ServerCopy {
                uid: 285,
                uidvalidity: Some(1700000000),
                siblings: Vec::new(),
            }),
            graph_copy: None,
        };
        let json = serde_json::to_string(&with_copy).unwrap();
        assert!(
            json.contains(r#""copy":{"uid":285,"uidvalidity":1700000000}"#),
            "{json}"
        );
        assert_eq!(serde_json::from_str::<ServerOp>(&json).unwrap(), with_copy);
    }

    /// Only a UID a server issued names a copy: the `-id` sentinel of a local
    /// move and a 63-bit synthetic uid fall back to the `Message-ID` search.
    #[test]
    fn a_row_names_a_server_copy_only_by_a_server_uid() {
        assert_eq!(
            ServerCopy::of_row(285, Some(7)),
            Some(ServerCopy {
                uid: 285,
                uidvalidity: Some(7),
                siblings: Vec::new(),
            })
        );
        assert_eq!(
            ServerCopy::of_row(285, None).map(|c| c.uidvalidity),
            Some(None)
        );
        assert_eq!(
            ServerCopy::of_row(285, Some(7))
                .unwrap()
                .with_siblings([289, -12, 285, 288, 289, crate::ingest::graph_uid("<a@x>")])
                .siblings,
            [288, 289],
            "a twin's sentinel, synthetic uid or the copy's own UID is no sibling"
        );
        assert_eq!(ServerCopy::of_row(-5, Some(7)), None, "a local move");
        assert_eq!(ServerCopy::of_row(0, Some(7)), None);
        assert_eq!(
            ServerCopy::of_row(crate::ingest::graph_uid("<a@x>"), None),
            None,
            "a synthetic uid"
        );
    }

    /// An op queued before PERSO-245 carries no `graph_copy` and decodes as
    /// one that names no Graph item; one that names an item writes it beside
    /// the rest and reads it back, and its siblings leave out its own id.
    #[test]
    fn an_op_queued_before_the_graph_copy_existed_still_decodes() {
        let old = r#"{"SetRead":{"message_id":"<8@x>","mailbox":"Inbox","read":true}}"#;
        let op = ServerOp::SetRead {
            message_id: "<8@x>".to_string(),
            mailbox: "Inbox".to_string(),
            read: true,
            copy: None,
            graph_copy: None,
        };
        assert_eq!(serde_json::from_str::<ServerOp>(old).unwrap(), op);
        assert_eq!(serde_json::to_string(&op).unwrap(), old);

        let named = ServerOp::Move {
            message_id: "<9@x>".to_string(),
            source_mailbox: "Inbox".to_string(),
            dest_mailbox: "Archive".to_string(),
            copy: None,
            graph_copy: Some(GraphCopy::of_row(
                "AAMk-b".to_string(),
                ["AAMk-c", "AAMk-b", "AAMk-a", "AAMk-c"].map(String::from),
            )),
        };
        let json = serde_json::to_string(&named).unwrap();
        assert!(
            json.contains(r#""graph_copy":{"id":"AAMk-b","siblings":["AAMk-a","AAMk-c"]}"#),
            "{json}"
        );
        assert_eq!(serde_json::from_str::<ServerOp>(&json).unwrap(), named);
    }
}
