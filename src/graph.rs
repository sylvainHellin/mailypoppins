// Microsoft Graph API REST client for Exchange Online mail operations.
//
// Provides a stateless HTTP client that wraps `reqwest::Client` + OAuth2 access token.
// Each function corresponds to a Graph API endpoint and returns domain types
// (`FetchedEmail`, folder lists, etc.). Those are what `src/ingest.rs` writes
// into the per-account store and blob store, the same landing point the IMAP
// backend uses; this module never touches the filesystem itself.

use std::collections::{HashMap, HashSet};

use anyhow::{anyhow, Context, Result};
use log::{debug, info, warn};
use mp_protocol::listing::GRAPH_SEARCH_CAP;
use serde::de::DeserializeOwned;
use serde::Deserialize;

use crate::config::GraphConfig;
use crate::ingest::pass_may_prune;
use crate::parse::{sanitize_attachment_filename, AttachmentData, FetchedEmail};
use crate::server_search::{Fetched, MailboxStubs, SearchStub};
use crate::sync::{stops_at, FreshObservation, Stop, StopPoint, SyncResult, SyncTarget};
use crate::timing::TimingSpan;
use crate::types::MailboxRole;

const GRAPH_BASE: &str = "https://graph.microsoft.com/v1.0";

/// The `$select` a full message fetch needs, shared by every endpoint that
/// returns whole messages so they all hydrate the same [`FetchedEmail`].
const MESSAGE_SELECT: &str =
    "id,internetMessageId,subject,from,toRecipients,ccRecipients,body,receivedDateTime,hasAttachments,isRead";

/// The MAPI property behind `In-Reply-To`, `PidTagInReplyToId`, as Graph
/// names a single-value extended property.
const IN_REPLY_TO_PROPERTY: &str = "String 0x1042";

/// The MAPI property behind `References`, `PidTagInternetReferences`.
const REFERENCES_PROPERTY: &str = "String 0x1039";

/// The MAPI property Outlook records a message's last reply or forward in,
/// `PidTagLastVerbExecuted` (PERSO-171): see [`LastVerb`].
const LAST_VERB_PROPERTY: &str = "Integer 0x1081";

/// When that verb ran, `PidTagLastVerbExecutionTime`.
const LAST_VERB_TIME_PROPERTY: &str = "SystemTime 0x1082";

/// The icon Outlook draws for a message, `PidTagIconIndex`: what shows the
/// replied or forwarded arrow in the message list.
const ICON_INDEX_PROPERTY: &str = "Integer 0x1080";

/// The `$expand` that brings a message's `In-Reply-To` and `References`
/// (PERSO-147) and its last reply or forward (PERSO-171), percent-encoded for
/// a URL and for a `/$batch` sub-request.
///
/// The two MAPI properties rather than `internetMessageHeaders`: measured on
/// the EVOQS inbox on 2026-10-22, the whole header block costs about 9.3 KB per
/// message on a listing page and 5.9 KB on a single GET, the two properties
/// about 90 and 390 bytes. A message without either header carries no
/// `singleValueExtendedProperties` at all. `/messages/delta` refuses this
/// `$expand` (HTTP 400), which costs nothing: the sync downloads by id and the
/// re-thread walks the folder listing, and both accept it. A `$search` and the
/// Message-ID `$filter` accept it too (measured on the EVOQS inbox on
/// 2026-10-22), so a search hit `message.fetch` lands is threaded at ingest.
/// The last-verb property rides the same filter for about 6 bytes a message
/// that carries it, so a message downloaded after a reply in Outlook lands
/// answered.
const MESSAGE_EXPAND: &str = "$expand=singleValueExtendedProperties($filter=id%20eq%20'String%200x1042'%20or%20id%20eq%20'String%200x1039'%20or%20id%20eq%20'Integer%200x1081')";

/// The `$expand` the folder enumeration carries (PERSO-171): the last verb
/// alone, so the listing that restates `isRead` on every held row restates
/// the reply and forward history beside it. Measured on the EVOQS inbox on
/// 2026-10-22: 1.2 KB more on a 200-message page of 74 KB. `/messages/delta`
/// refuses it like it refuses [`MESSAGE_EXPAND`], so a quick pass that walks
/// the delta leaves the history to the next enumeration.
const LAST_VERB_EXPAND: &str =
    "$expand=singleValueExtendedProperties($filter=id%20eq%20'Integer%200x1081')";

/// The reply or forward Outlook last recorded on a message, the value of
/// `PidTagLastVerbExecuted` (PERSO-171).
///
/// Graph has no answered flag; Exchange keeps this MAPI property instead, and
/// Outlook draws its replied and forwarded arrows from it and from
/// `PidTagIconIndex`. It holds the *last* verb only: a reply followed by a
/// forward reads as a forward, which is why the sync merge only ever adds a
/// history bit and never clears one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LastVerb {
    Reply,
    ReplyAll,
    Forward,
}

impl LastVerb {
    /// The verb a sent draft records: a forward when it was not an answer.
    pub fn of_send(answered: bool, reply_all: bool) -> Self {
        match (answered, reply_all) {
            (false, _) => LastVerb::Forward,
            (true, false) => LastVerb::Reply,
            (true, true) => LastVerb::ReplyAll,
        }
    }

    /// `PidTagLastVerbExecuted`: `NOTEIVERB_REPLYTOSENDER`, `_REPLYTOALL`,
    /// `_FORWARD`.
    pub fn code(self) -> i64 {
        match self {
            LastVerb::Reply => 102,
            LastVerb::ReplyAll => 103,
            LastVerb::Forward => 104,
        }
    }

    /// `PidTagIconIndex`: 261 the replied mail icon, 262 the forwarded one.
    pub fn icon(self) -> i64 {
        match self {
            LastVerb::Reply | LastVerb::ReplyAll => 261,
            LastVerb::Forward => 262,
        }
    }

    /// The verb a value of the property names, `None` for any other verb
    /// (a reply to a public folder, a voting button, an open).
    fn from_code(code: i64) -> Option<Self> {
        match code {
            102 => Some(LastVerb::Reply),
            103 => Some(LastVerb::ReplyAll),
            104 => Some(LastVerb::Forward),
            _ => None,
        }
    }

    /// The verb among a message's expanded extended properties. Graph sends
    /// an integer property's value as a string.
    fn from_properties(properties: &[GraphExtendedProperty]) -> Option<Self> {
        properties
            .iter()
            .find(|p| p.id.eq_ignore_ascii_case(LAST_VERB_PROPERTY))
            .and_then(|p| p.value.as_deref())
            .and_then(|v| v.trim().parse().ok())
            .and_then(Self::from_code)
    }

    /// `flags` with the history bit this verb stands for set: a reply or a
    /// reply to all is `\Answered`, a forward `$Forwarded`.
    pub fn mark(self, flags: crate::types::MessageFlags) -> crate::types::MessageFlags {
        match self {
            LastVerb::Reply | LastVerb::ReplyAll => crate::types::MessageFlags {
                answered: true,
                ..flags
            },
            LastVerb::Forward => crate::types::MessageFlags {
                forwarded: true,
                ..flags
            },
        }
    }
}

/// The PATCH body that records `verb` on a message the way Outlook does: the
/// verb, when it ran, and the icon (PERSO-171). Exchange Online accepted all
/// three through Graph on the EVOQS account on 2026-10-22 (HTTP 200, read
/// back unchanged).
fn last_verb_patch_body(verb: LastVerb, at: chrono::DateTime<chrono::Utc>) -> serde_json::Value {
    serde_json::json!({
        "singleValueExtendedProperties": [
            {"id": LAST_VERB_PROPERTY, "value": verb.code().to_string()},
            {"id": LAST_VERB_TIME_PROPERTY, "value": at.format("%Y-%m-%dT%H:%M:%SZ").to_string()},
            {"id": ICON_INDEX_PROPERTY, "value": verb.icon().to_string()},
        ]
    })
}

/// Graph's hard ceiling on the number of requests in one `/$batch` call.
const BATCH_MAX_REQUESTS: usize = 20;

/// How many sub-request failures one `/$batch` pass tolerates before it stops
/// asking for the rest.
///
/// A failing sub-request is skipped, so the message stays new and is retried
/// next pass; without a budget a systematically failing id set (a revoked
/// permission, a mailbox mid-migration) spends the whole sync issuing requests
/// that cannot succeed and one `warn!` for each. Giving up after this many
/// leaves the pass's successful downloads in place and the rest new.
const BATCH_FAILURE_BUDGET: usize = 50;

/// How many individual sub-request failures are logged before the pass falls
/// back to a single summary line.
const BATCH_FAILURE_WARN_LIMIT: usize = 5;

/// Ceiling on a `Retry-After` a throttled sub-response asks for, so a hostile
/// or mistaken header cannot park a sync for hours.
const MAX_RETRY_AFTER_SECS: u64 = 120;

/// How many times a server search's second pass asks for one hit Graph keeps
/// throttling before it reports the hit as not downloaded (PERSO-175).
const SEARCH_FETCH_ATTEMPTS: u32 = 3;

/// The pause before asking again for a hit Graph throttled without a
/// `Retry-After`.
const SEARCH_THROTTLE_PAUSE_SECS: u64 = 2;

/// Pages the folder enumeration will walk before giving up.
///
/// At `$top=200` this covers a 50 000-message folder. The guard exists because
/// a `@odata.nextLink` loop has no other termination proof; hitting it marks
/// the enumeration incomplete rather than truncating silently, because a short
/// enumeration read as truth is a mass prune (see [`FolderEnumeration`]).
const MAX_ENUMERATION_PAGES: usize = 250;

/// Pages one `/messages/delta` walk will follow before giving up (#0042).
///
/// Same ceiling and same reasoning as [`MAX_ENUMERATION_PAGES`], with a
/// stricter consequence: an enumeration that hits its cap merely declines to
/// prune, while a delta walk that hits its cap has no `@odata.deltaLink` to
/// resume from and no way to know what it did not see, so it throws the token
/// away and the pass falls back to the full enumeration.
const MAX_DELTA_PAGES: usize = 250;

/// Messages per delta page, asked for with `Prefer: odata.maxpagesize`.
///
/// The delta endpoint takes its page size from the header rather than from
/// `$top`, which it rejects.
const DELTA_PAGE_SIZE: usize = 200;

/// The characters a Graph message id must not carry unescaped into a `/$batch`
/// sub-request URL.
///
/// A direct `reqwest` GET hands the id to `Url`, which escapes what a path
/// segment cannot hold; inside `/$batch` the URL is a JSON string that Graph
/// parses itself, so the escaping has to happen here. v1.0 REST ids are
/// base64url plus `=`, all legal in a path segment, so in practice this changes
/// nothing; immutable ids and the standard base64 alphabet are why it is not
/// nothing in principle (#0065).
const ID_ENCODE_SET: &percent_encoding::AsciiSet = &percent_encoding::CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'&')
    .add(b'+')
    .add(b'/')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'`')
    .add(b'{')
    .add(b'}');

// ---------------------------------------------------------------------------
// GraphClient
// ---------------------------------------------------------------------------

/// How long a Graph request may take to open its connection.
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// How long a Graph response may go without a single byte arriving.
///
/// An idle bound rather than a whole-request one, so a large attachment list
/// on a slow link keeps downloading as long as it moves, while a connection
/// that died under it (a laptop that slept mid-request) errors out instead of
/// holding the account's tick forever. Graph's own gateway gives up on a
/// request after about 100 seconds, so a live server never trips this.
const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// The HTTP client every Graph call goes through, with the two bounds above.
fn http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(READ_TIMEOUT)
        .build()
        .context("building the Graph HTTP client")
}

/// A Graph API client holding a reqwest client and a valid access token.
pub struct GraphClient {
    client: reqwest::Client,
    access_token: String,
    #[allow(dead_code)]
    username: String,
    /// The mailbox's top-level folders, listed once per client the first time
    /// a configured name is neither a well-known folder nor an id (see
    /// [`GraphClient::folder_path`]).
    top_folders: std::sync::Mutex<Option<Vec<GraphMailFolder>>>,
}

impl GraphClient {
    /// Create a new GraphClient, loading/refreshing the OAuth2 token synchronously.
    /// Panics if called from inside an async runtime (use `new_async` instead).
    pub fn new(config: &GraphConfig) -> Result<Self> {
        let access_token = crate::oauth2::load_or_refresh_token_blocking(
            &config.account_name,
            &config.client_id,
            &config.tenant_id,
            crate::oauth2::GRAPH_SCOPES,
        )?;
        Ok(Self {
            client: http_client()?,
            access_token,
            username: config.username.clone(),
            top_folders: std::sync::Mutex::new(None),
        })
    }

    /// Create a new GraphClient from within an async context.
    pub async fn new_async(config: &GraphConfig) -> Result<Self> {
        let access_token = crate::oauth2::load_or_refresh_token(
            &config.account_name,
            &config.client_id,
            &config.tenant_id,
            crate::oauth2::GRAPH_SCOPES,
        )
        .await?;
        Ok(Self {
            client: http_client()?,
            access_token,
            username: config.username.clone(),
            top_folders: std::sync::Mutex::new(None),
        })
    }

    fn bearer(&self) -> String {
        self.access_token.clone()
    }

    /// Refresh the access token in place, keeping the reqwest client and its
    /// connection pool.
    ///
    /// For the long-lived callers (the TUI watcher): an access token expires
    /// after about an hour, so a client built once and never touched starts
    /// returning 401 mid-session, but rebuilding the whole client per poll
    /// throws away the connection pool as well. `load_or_refresh_token` hits
    /// the network only when the cached token has actually expired.
    pub async fn refresh_token(&mut self, config: &GraphConfig) -> Result<()> {
        self.access_token = crate::oauth2::load_or_refresh_token(
            &config.account_name,
            &config.client_id,
            &config.tenant_id,
            crate::oauth2::GRAPH_SCOPES,
        )
        .await?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Graph API response types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphMailFolder {
    pub id: String,
    pub display_name: String,
    #[serde(default)]
    pub total_item_count: i64,
    #[serde(default)]
    pub unread_item_count: i64,
    /// How many folders sit under this one, which is what tells the folder
    /// tree walk (#0141) whether to ask for its children.
    #[serde(default)]
    pub child_folder_count: i64,
}

#[derive(Debug, Deserialize)]
struct GraphMailFolderList {
    value: Vec<GraphMailFolder>,
    #[serde(rename = "@odata.nextLink", default)]
    next_link: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphMessage {
    id: String,
    #[serde(default)]
    internet_message_id: Option<String>,
    #[serde(default)]
    subject: Option<String>,
    #[serde(default)]
    from: Option<GraphRecipient>,
    #[serde(default)]
    to_recipients: Vec<GraphRecipient>,
    #[serde(default)]
    cc_recipients: Vec<GraphRecipient>,
    #[serde(default)]
    body: Option<GraphBody>,
    #[serde(default)]
    received_date_time: Option<String>,
    #[serde(default)]
    has_attachments: bool,
    #[serde(default)]
    is_read: bool,
    #[serde(default)]
    conversation_id: Option<String>,
    /// The extended properties [`MESSAGE_EXPAND`] asks for, absent when the
    /// message carries none of them.
    #[serde(default)]
    single_value_extended_properties: Vec<GraphExtendedProperty>,
}

/// One single-value extended property: a MAPI property id and its value.
#[derive(Debug, Clone, Deserialize)]
struct GraphExtendedProperty {
    id: String,
    #[serde(default)]
    value: Option<String>,
}

/// The threading signals of one message as Graph answered them.
///
/// `conversationId` is always written, the empty string when Graph named none,
/// so the row reads as asked (see `messages.conversation_id` in the schema).
fn thread_hints(
    conversation_id: Option<&str>,
    properties: &[GraphExtendedProperty],
) -> crate::ingest::ThreadHints {
    let property = |id: &str| {
        properties
            .iter()
            .find(|p| p.id.eq_ignore_ascii_case(id))
            .and_then(|p| p.value.as_deref())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    };
    crate::ingest::ThreadHints {
        in_reply_to: property(IN_REPLY_TO_PROPERTY),
        references: property(REFERENCES_PROPERTY),
        conversation_id: Some(conversation_id.unwrap_or_default().trim().to_string()),
    }
}

/// A downloaded Graph message and the threading signals ingest needs beside
/// it, which a [`FetchedEmail`] has no field for.
pub struct GraphFetched {
    pub email: FetchedEmail,
    pub threading: crate::ingest::ThreadHints,
}

/// One entry of the re-thread's folder listing (PERSO-147).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphThreadingEntry {
    #[serde(default)]
    internet_message_id: Option<String>,
    #[serde(default)]
    conversation_id: Option<String>,
    #[serde(default)]
    single_value_extended_properties: Vec<GraphExtendedProperty>,
}

#[derive(Debug, Deserialize)]
struct GraphThreadingList {
    value: Vec<GraphThreadingEntry>,
    #[serde(rename = "@odata.nextLink")]
    next_link: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphRecipient {
    email_address: GraphEmailAddress,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphEmailAddress {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    address: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphBody {
    #[serde(default)]
    content_type: Option<String>,
    #[serde(default)]
    content: Option<String>,
}

/// One page of a message collection: a folder listing, or a search page of
/// whole messages or of [`GraphSearchStub`]s.
#[derive(Debug, Deserialize)]
struct GraphPage<T> {
    value: Vec<T>,
    /// The next page, which a search follows up to its limit.
    #[serde(rename = "@odata.nextLink", default)]
    next_link: Option<String>,
}

/// One hit of a search's first pass (PERSO-175): what the merge across
/// mailboxes reads, the id the second pass downloads it by, and the folder it
/// sits in, which names the hit of a search across every folder.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphSearchStub {
    id: String,
    #[serde(default)]
    internet_message_id: Option<String>,
    #[serde(default)]
    received_date_time: Option<String>,
    #[serde(default)]
    parent_folder_id: Option<String>,
}

/// One folder as a search names its hits: its id and its display name.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphFolderName {
    id: String,
    display_name: String,
}

/// One entry of a message's `/attachments` collection.
///
/// The collection mixes `fileAttachment`, `itemAttachment` (an attached
/// message or event) and `referenceAttachment` (a cloud link); only the first
/// carries `contentBytes`. Every field is optional so an item or reference
/// attachment, whose properties differ and may come back `null`, can never
/// fail the parse of the whole list.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphAttachment {
    #[serde(rename = "@odata.type", default)]
    odata_type: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    content_type: Option<String>,
    #[serde(default)]
    content_bytes: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    is_inline: Option<bool>,
    #[serde(default)]
    content_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GraphAttachmentList {
    value: Vec<GraphAttachment>,
}

/// Lightweight response for enumerating a folder: everything the sync needs
/// about a message without downloading it.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphMessageIdEntry {
    id: String,
    #[serde(default)]
    internet_message_id: Option<String>,
    #[serde(default)]
    is_read: bool,
    #[serde(default)]
    received_date_time: Option<String>,
    /// [`LAST_VERB_EXPAND`]'s answer, absent when Outlook recorded no verb.
    #[serde(default)]
    single_value_extended_properties: Vec<GraphExtendedProperty>,
}

#[derive(Debug, Deserialize)]
struct GraphMessageIdList {
    value: Vec<GraphMessageIdEntry>,
    #[serde(rename = "@odata.nextLink")]
    next_link: Option<String>,
}

/// What a folder enumeration knows about one message it listed.
///
/// Keyed by `internetMessageId` (the identity the store uses, see
/// [`crate::ingest::graph_uid`]); this is the rest of the row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderEntry {
    /// Graph's own message id: the only handle that fetches *this* message,
    /// rather than whatever currently sits in the folder's recency window.
    pub graph_id: String,
    /// The server's `\Seen` equivalent.
    pub is_read: bool,
    /// The reply or forward Outlook last recorded (PERSO-171). Only a folder
    /// enumeration reads it; a delta entry carries `None`, which the flag
    /// merge reads as "nothing to add", never as "not answered".
    pub last_verb: Option<LastVerb>,
    /// `receivedDateTime` verbatim (ISO 8601, always UTC from Graph), used to
    /// order a capped download newest-first. Sorts as a string, which is why
    /// the UTC form matters; `None` sorts oldest.
    pub received: Option<String>,
}

/// A whole folder as one enumeration saw it, and whether it saw all of it.
///
/// The flag is the prune's licence. `known − enumerated` is only a set of
/// vanished messages if the enumeration actually covered the folder; an
/// enumeration cut short by [`MAX_ENUMERATION_PAGES`] would make every message
/// past the cut look deleted, so an incomplete pass is a pass that may fetch
/// and re-flag but must not delete (#0065).
#[derive(Debug, Clone, Default)]
pub struct FolderEnumeration {
    /// Every message the folder listed, keyed by its trimmed
    /// `internetMessageId`.
    pub entries: HashMap<String, FolderEntry>,
    /// False when the page guard stopped the walk before the server ran out of
    /// pages.
    pub complete: bool,
}

/// What one target's fetch pass learned: the messages it downloaded, the ones
/// it skipped as already known, the folder as enumerated, and whether the
/// download covered everything the enumeration found.
pub struct FolderFetch {
    pub new_emails: Vec<GraphFetched>,
    pub skipped: usize,
    pub enumeration: FolderEnumeration,
    /// True when this pass did not download every new message it found, so the
    /// leftovers are queued for a later pass. See [`sync_mailboxes_graph`] for
    /// why that suspends the prune.
    pub download_incomplete: bool,
    /// True when `limit` alone left new messages behind, after a download that
    /// brought everything it asked for: the next pass's window takes the next
    /// ones, so the folder is worth another pass soon ([`backlog_owed`]).
    pub backlog: bool,
}

/// What one `/$batch` download run returned, and whether it returned all of it.
///
/// The count matters as much as the messages: a chunk whose sub-responses
/// failed, a parse that did not yield a [`FetchedEmail`], and a run that spent
/// [`BATCH_FAILURE_BUDGET`] all hand back fewer emails than ids asked for, and
/// all three mean the same thing to the prune: this pass does not know what the
/// server holds (#0065 follow-up).
pub struct BatchFetch {
    /// How many ids the run was asked for.
    pub requested: usize,
    pub emails: Vec<GraphFetched>,
    /// True when [`BATCH_FAILURE_BUDGET`] stopped the run before it had issued
    /// every chunk.
    pub gave_up: bool,
}

impl BatchFetch {
    /// Whether fewer messages came back than were asked for.
    fn fell_short(&self) -> bool {
        batch_fell_short(self.requested, self.emails.len(), self.gave_up)
    }
}

/// The rule behind [`BatchFetch::fell_short`], as counts: a run that handed
/// back fewer messages than ids, or gave up before issuing every chunk, did not
/// download what the enumeration found.
fn batch_fell_short(requested: usize, returned: usize, gave_up: bool) -> bool {
    gave_up || returned < requested
}

/// One entry of a `/$batch` response.
#[derive(Debug, Deserialize)]
struct GraphBatchEntry {
    id: String,
    status: u16,
    /// The sub-response's own headers. Only `Retry-After` is read, and only on
    /// a throttle: Graph throttles per sub-request, so the outer POST can be a
    /// 200 while individual messages are being told to slow down.
    ///
    /// The values are `serde_json::Value`, not `String`, because this is a
    /// whole-batch parse: a single sub-response header serialised as a number
    /// or an array (nothing in the protocol forbids it) would otherwise fail
    /// the deserialization of *every* entry in the chunk, which means zero
    /// downloads for that folder on every pass and, since #0065, a prune
    /// suspended for as long as it lasts. A header this code does not read
    /// should not be able to do that (#0065 follow-up).
    #[serde(default)]
    headers: HashMap<String, serde_json::Value>,
    #[serde(default)]
    body: Option<serde_json::Value>,
}

/// A header value as a whole number of seconds, whether Graph sent it as a
/// JSON string (the normal shape) or as a number.
fn header_secs(value: &serde_json::Value) -> Option<u64> {
    match value {
        serde_json::Value::String(s) => s.trim().parse::<u64>().ok(),
        serde_json::Value::Number(n) => n.as_u64(),
        _ => None,
    }
}

impl GraphBatchEntry {
    /// This sub-response's `Retry-After` in seconds, whatever its status.
    /// Header names are matched case-insensitively because Graph does not
    /// promise a casing.
    fn retry_after_header(&self) -> Option<u64> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("retry-after"))
            .and_then(|(_, v)| header_secs(v))
            .map(|secs| secs.min(MAX_RETRY_AFTER_SECS))
    }

    /// The pause this sub-response asks for, in seconds, when it is a throttle.
    ///
    /// 429 and 503 both count: Graph's throttling guidance names the two
    /// together, and a 503 carrying a `Retry-After` is back-pressure in the
    /// same sense as a 429 (#0065 follow-up).
    fn retry_after_secs(&self) -> Option<u64> {
        if !matches!(self.status, 429 | 503) {
            return None;
        }
        self.retry_after_header()
    }

    /// Whether this sub-response is Graph asking the client to slow down rather
    /// than reporting a failure.
    ///
    /// A throttle does not spend [`BATCH_FAILURE_BUDGET`]: the budget exists to
    /// stop a pass that cannot succeed (a revoked permission, a mailbox
    /// mid-migration), and back-pressure is not that. Spending it meant a
    /// throttled first sync gave up after 50 sub-responses and reported a short
    /// download. A bare 503 with no `Retry-After` stays a failure, because
    /// nothing distinguishes it from the service being down.
    fn is_throttled(&self) -> bool {
        self.status == 429 || (self.status == 503 && self.retry_after_header().is_some())
    }

    /// What this sub-response says about the message it asked for.
    fn into_sub_response(self) -> SubResponse<GraphMessage> {
        if self.is_throttled() {
            return SubResponse::Throttled(self.retry_after_secs());
        }
        if self.status != 200 {
            return SubResponse::Failed(match &self.body {
                Some(body) => format!("HTTP {}: {}", self.status, graph_error_text(body)),
                None => format!("HTTP {}", self.status),
            });
        }
        let Some(body) = self.body else {
            return SubResponse::Failed("an empty answer".to_string());
        };
        match serde_json::from_value::<GraphMessage>(body) {
            Ok(msg) => SubResponse::Message(msg),
            Err(e) => SubResponse::Failed(format!("did not parse: {e}")),
        }
    }
}

/// One `/$batch` sub-response, classified.
#[derive(Debug)]
enum SubResponse<T> {
    /// The message asked for.
    Message(T),
    /// Graph asked to slow down, with the pause its `Retry-After` named.
    Throttled(Option<u64>),
    /// A failure that asking again does not mend, and why.
    Failed(String),
}

/// A Graph error body as one line: its `error.code` and `error.message` when
/// it carries them, the raw JSON otherwise.
fn graph_error_text(body: &serde_json::Value) -> String {
    let error = &body["error"];
    match (error["code"].as_str(), error["message"].as_str()) {
        (Some(code), Some(message)) => format!("{code}: {message}"),
        (Some(code), None) => code.to_string(),
        _ => body.to_string(),
    }
}

/// What a batch download with retries brought back.
struct Retried<T> {
    /// Each message downloaded, with the id it was asked for by.
    landed: Vec<(String, T)>,
    /// Each id that did not come back, with why.
    missing: Vec<(String, String)>,
}

/// Download `ids` a [`BATCH_MAX_REQUESTS`] chunk at a time through `round`,
/// asking again for an id Graph throttled, up to `attempts` times in all.
///
/// A throttle's `Retry-After` (else [`SEARCH_THROTTLE_PAUSE_SECS`]) paces the
/// next chunk, and the throttled ids go to the back of the queue. Every id
/// ends either landed or missing with its reason: a failure that is not a
/// throttle is not asked again, an id the batch did not answer at all is
/// missing, and one still throttled after `attempts` is missing as such. An
/// error from `round` is the whole download failing.
async fn fetch_with_retries<T, F, Fut>(
    ids: &[String],
    attempts: u32,
    mut round: F,
) -> Result<Retried<T>>
where
    F: FnMut(Vec<String>) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<(usize, SubResponse<T>)>>>,
{
    let mut queue: std::collections::VecDeque<(String, u32)> =
        ids.iter().map(|id| (id.clone(), 1)).collect();
    let mut retried = Retried {
        landed: Vec::new(),
        missing: Vec::new(),
    };
    let mut pause: Option<u64> = None;
    while !queue.is_empty() {
        if let Some(secs) = pause.take() {
            info!("Graph asked for {secs}s before the next search download");
            tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
        }
        let take = queue.len().min(BATCH_MAX_REQUESTS);
        let chunk: Vec<(String, u32)> = queue.drain(..take).collect();
        let mut answers: Vec<Option<SubResponse<T>>> = chunk.iter().map(|_| None).collect();
        for (index, answer) in round(chunk.iter().map(|(id, _)| id.clone()).collect()).await? {
            if let Some(slot) = answers.get_mut(index) {
                *slot = Some(answer);
            }
        }
        for ((id, attempt), answer) in chunk.into_iter().zip(answers) {
            match answer {
                Some(SubResponse::Message(msg)) => retried.landed.push((id, msg)),
                Some(SubResponse::Throttled(secs)) if attempt < attempts => {
                    let secs = secs.unwrap_or(SEARCH_THROTTLE_PAUSE_SECS);
                    pause = Some(pause.map_or(secs, |held| held.max(secs)));
                    queue.push_back((id, attempt + 1));
                }
                Some(SubResponse::Throttled(_)) => retried
                    .missing
                    .push((id, format!("throttled by Microsoft Graph {attempts} times"))),
                Some(SubResponse::Failed(why)) => retried.missing.push((id, why)),
                None => retried
                    .missing
                    .push((id, "not answered by the batch".to_string())),
            }
        }
    }
    Ok(retried)
}

#[derive(Debug, Deserialize)]
struct GraphBatchResponse {
    #[serde(default)]
    responses: Vec<GraphBatchEntry>,
}

/// Lightweight response for finding a message by internet message ID.
#[derive(Debug, Deserialize)]
struct GraphMessageIdLookup {
    value: Vec<GraphMessageIdLookupEntry>,
}

#[derive(Debug, Deserialize)]
struct GraphMessageIdLookupEntry {
    id: String,
}

// ---------------------------------------------------------------------------
// Format helpers
// ---------------------------------------------------------------------------

fn format_recipient(r: &GraphRecipient) -> String {
    let addr = r.email_address.address.as_deref().unwrap_or("");
    match r.email_address.name.as_deref() {
        Some(name) if !name.is_empty() => format!("{} <{}>", name, addr),
        _ => addr.to_string(),
    }
}

fn format_recipients(recipients: &[GraphRecipient]) -> String {
    recipients
        .iter()
        .map(format_recipient)
        .collect::<Vec<_>>()
        .join(", ")
}

/// The JSON body of a `/$batch` call naming one folder per path segment, the
/// request id being the index into `segments`.
fn folder_batch_body(segments: &[String]) -> serde_json::Value {
    let requests: Vec<serde_json::Value> = segments
        .iter()
        .enumerate()
        .map(|(i, segment)| {
            let encoded = percent_encoding::utf8_percent_encode(segment, ID_ENCODE_SET);
            serde_json::json!({
                "id": i.to_string(),
                "method": "GET",
                "url": format!("/me/mailFolders/{encoded}?$select=id,displayName"),
            })
        })
        .collect();
    serde_json::json!({ "requests": requests })
}

/// The hit folders `configured` already names by id, labelled, and the ids
/// left to ask Graph about.
fn known_folder_labels(
    configured: &[(String, String)],
    ids: &[String],
) -> (HashMap<String, String>, Vec<String>) {
    let by_id: HashMap<&str, &str> = configured
        .iter()
        .filter(|(_, segment)| looks_like_folder_id(segment))
        .map(|(label, id)| (id.as_str(), label.as_str()))
        .collect();
    let mut labelled = HashMap::new();
    let mut unknown = Vec::new();
    for id in ids {
        match by_id.get(id.as_str()) {
            Some(label) => {
                labelled.insert(id.clone(), label.to_string());
            }
            None => unknown.push(id.clone()),
        }
    }
    (labelled, unknown)
}

/// The label of every hit folder, from the answers to
/// [`GraphClient::folder_labels`]'s batch: `asked[i]` is the `i`-th path
/// segment asked for with its configured label, `None` for a hit's folder id,
/// and `answers[i]` what Graph said that segment is.
///
/// A hit folder some configured mailbox resolves to is listed under that
/// mailbox's label, whatever the folder is called on the server; any other
/// is listed under its display name, and one Graph did not name is left out.
fn label_folders(
    asked: &[(Option<&str>, &str)],
    answers: &[Option<GraphFolderName>],
) -> HashMap<String, String> {
    // A configured folder is known by the segment it was asked under, which
    // is already its id unless it is a well-known name, and by the id Graph
    // answered with.
    let mut configured: HashMap<&str, &str> = HashMap::new();
    for ((label, segment), answer) in asked.iter().zip(answers) {
        let Some(label) = label else { continue };
        configured.insert(segment, label);
        if let Some(folder) = answer {
            configured.insert(&folder.id, label);
        }
    }
    asked
        .iter()
        .zip(answers)
        .filter(|((label, _), _)| label.is_none())
        .filter_map(|((_, id), answer)| {
            let label = configured
                .get(id)
                .or_else(|| configured.get(answer.as_ref()?.id.as_str()))
                .map(|label| label.to_string())
                .or_else(|| Some(answer.as_ref()?.display_name.clone()))?;
            Some((id.to_string(), label))
        })
        .collect()
}

/// The JSON body of a `/$batch` call fetching one message per id.
///
/// The request id is the index into `ids`, which is how the caller matches a
/// response back to the message it asked for. The message id is percent-encoded
/// ([`ID_ENCODE_SET`]) because nothing else on this path will do it.
fn batch_request_body(ids: &[&str]) -> serde_json::Value {
    let requests: Vec<serde_json::Value> = ids
        .iter()
        .enumerate()
        .map(|(i, id)| {
            let encoded = percent_encoding::utf8_percent_encode(id, ID_ENCODE_SET);
            serde_json::json!({
                "id": i.to_string(),
                "method": "GET",
                "url": format!(
                    "/me/messages/{}?$select={},conversationId&{}",
                    encoded, MESSAGE_SELECT, MESSAGE_EXPAND
                ),
            })
        })
        .collect();
    serde_json::json!({ "requests": requests })
}

/// Convert an ISO 8601 datetime (e.g. "2024-01-15T10:30:00Z") to RFC 2822 format.
fn iso_to_rfc2822(iso: &str) -> String {
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(iso) {
        dt.to_rfc2822()
    } else if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(iso, "%Y-%m-%dT%H:%M:%SZ") {
        let utc = chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(dt, chrono::Utc);
        utc.to_rfc2822()
    } else {
        iso.to_string()
    }
}

/// The Graph well-known folder name a configured folder stands for, if any.
///
/// A `/me/mailFolders/{x}` URL takes a well-known name or a folder id, never a
/// display name: "Sent Items" in the URL is read as a malformed id (HTTP 400
/// `ErrorInvalidIdMalformed`), so Sent Items and Junk Email never synced.
/// Both the well-known names and Exchange's English default display names map
/// here, matched case-insensitively, and so does mp's own role name `sent`,
/// which `in:sent` passes when the account configures no sent mailbox
/// (PERSO-173). A localized display name ("Gesendete
/// Elemente") is not listed: [`GraphClient::folder_path`] resolves it through
/// the folder list.
pub(crate) fn well_known_folder(folder: &str) -> Option<&'static str> {
    let lower = folder.trim().to_lowercase();
    Some(match lower.as_str() {
        "inbox" => "inbox",
        "archive" => "archive",
        "sentitems" | "sent items" | "sent" => "sentitems",
        "drafts" => "drafts",
        "deleteditems" | "deleted items" => "deleteditems",
        "junkemail" | "junk email" | "junk e-mail" => "junkemail",
        "outbox" => "outbox",
        _ => return None,
    })
}

/// How deep [`GraphClient::list_folder_tree`] walks the folder tree.
const MAX_FOLDER_DEPTH: usize = 8;

/// Whether a configured folder already is a Graph folder id.
///
/// Mail folder ids are base64 strings of well over a hundred characters with
/// no spaces; no display name a person types looks like one.
fn looks_like_folder_id(folder: &str) -> bool {
    folder.len() >= 64 && !folder.contains(char::is_whitespace)
}

/// The id of the folder whose display name is `name`, matched
/// case-insensitively against one level of the folder tree.
fn folder_id_by_display_name<'a>(folders: &'a [GraphMailFolder], name: &str) -> Option<&'a str> {
    let name = name.trim();
    folders
        .iter()
        .find(|f| f.display_name == name)
        .or_else(|| {
            folders
                .iter()
                .find(|f| f.display_name.to_lowercase() == name.to_lowercase())
        })
        .map(|f| f.id.as_str())
}

// ---------------------------------------------------------------------------
// Phase 2: List folders
// ---------------------------------------------------------------------------

impl GraphClient {
    /// List all mail folders in the user's mailbox.
    pub async fn list_folders(&self) -> Result<Vec<GraphMailFolder>> {
        self.list_folder_level(&format!("{}/me/mailFolders?$top=100", GRAPH_BASE))
            .await
    }

    /// Every folder on one level of the tree, following `@odata.nextLink`.
    async fn list_folder_level(&self, first_url: &str) -> Result<Vec<GraphMailFolder>> {
        let mut url = first_url.to_string();
        let mut all = Vec::new();
        for _ in 0..MAX_ENUMERATION_PAGES {
            let resp = self
                .client
                .get(&url)
                .bearer_auth(self.bearer())
                .send()
                .await
                .context("Failed to list mail folders")?;

            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                return Err(anyhow!("List folders failed (HTTP {}): {}", status, body));
            }

            let page: GraphMailFolderList =
                resp.json().await.context("Failed to parse folder list")?;
            all.extend(page.value);
            match page.next_link {
                Some(next) => url = next,
                None => break,
            }
        }
        Ok(all)
    }

    /// Every mail folder of the mailbox, the whole tree (#0141), each with
    /// the `Parent/Child` path [`Self::folder_path`] resolves back to it.
    ///
    /// Breadth-first, one request per level of each folder that has children,
    /// and bounded at [`MAX_FOLDER_DEPTH`] levels, so a pathological tree
    /// cannot keep a tick listing forever.
    pub async fn list_folder_tree(&self) -> Result<Vec<(String, GraphMailFolder)>> {
        let mut out = Vec::new();
        let mut level: Vec<(String, GraphMailFolder)> = self
            .list_folders()
            .await?
            .into_iter()
            .map(|folder| (folder.display_name.clone(), folder))
            .collect();
        for _ in 0..MAX_FOLDER_DEPTH {
            let mut next = Vec::new();
            for (path, folder) in &level {
                if folder.child_folder_count > 0 {
                    let children = self
                        .list_folder_level(&format!(
                            "{}/me/mailFolders/{}/childFolders?$top=100",
                            GRAPH_BASE, folder.id
                        ))
                        .await?;
                    next.extend(
                        children
                            .into_iter()
                            .map(|child| (format!("{path}/{}", child.display_name), child)),
                    );
                }
            }
            out.append(&mut level);
            if next.is_empty() {
                break;
            }
            level = next;
        }
        out.append(&mut level);
        Ok(out)
    }

    /// The id of a well-known folder (`sentitems`, `drafts`, ...), or `None`
    /// when the mailbox has none, which Graph answers with a 404 (#0141).
    pub async fn well_known_folder_id(&self, name: &str) -> Result<Option<String>> {
        let url = format!("{GRAPH_BASE}/me/mailFolders/{name}?$select=id");
        let resp = self
            .client
            .get(&url)
            .bearer_auth(self.bearer())
            .send()
            .await
            .with_context(|| format!("Failed to read the {name} folder"))?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "Read the {name} folder failed (HTTP {status}): {body}"
            ));
        }
        let folder: GraphFolderId = resp.json().await.context("Failed to parse the folder")?;
        Ok(Some(folder.id))
    }

    /// The URL path segment that names `folder` in `/me/mailFolders/{x}`.
    ///
    /// A well-known folder (by its Graph name or its English display name)
    /// maps to the well-known name and an id passes through, both without a
    /// request. Anything else is a display name, localized or custom, and is
    /// looked up: first among the top-level folders (listed once per client),
    /// then, for a `Parent/Child` path, one level at a time with the first
    /// segment resolved the same way. A name the lookup cannot place passes
    /// through unchanged, so the request reports Graph's own error.
    async fn folder_path(&self, folder: &str) -> Result<String> {
        if let Some(known) = well_known_folder(folder) {
            return Ok(known.to_string());
        }
        if looks_like_folder_id(folder) {
            return Ok(folder.to_string());
        }
        if let Some(id) = self.top_level_folder_id(folder).await? {
            return Ok(id);
        }
        if let Some((head, rest)) = folder.split_once('/') {
            let mut parent = match well_known_folder(head) {
                Some(known) => Some(known.to_string()),
                None => self.top_level_folder_id(head).await?,
            };
            for segment in rest.split('/') {
                let Some(parent_id) = parent else { break };
                let children = self
                    .list_folder_level(&format!(
                        "{}/me/mailFolders/{}/childFolders?$top=100",
                        GRAPH_BASE, parent_id
                    ))
                    .await?;
                parent = folder_id_by_display_name(&children, segment).map(str::to_string);
            }
            if let Some(id) = parent {
                return Ok(id);
            }
        }
        debug!("Graph folder '{folder}' matches no display name; using it as an id");
        Ok(folder.to_string())
    }

    /// The id of the top-level folder displayed as `name`, from a folder list
    /// fetched once per client.
    async fn top_level_folder_id(&self, name: &str) -> Result<Option<String>> {
        let cached = self
            .top_folders
            .lock()
            .map_err(|_| anyhow!("folder cache poisoned"))?
            .as_ref()
            .map(|folders| folder_id_by_display_name(folders, name).map(str::to_string));
        if let Some(found) = cached {
            return Ok(found);
        }
        let folders = self.list_folders().await?;
        let found = folder_id_by_display_name(&folders, name).map(str::to_string);
        *self
            .top_folders
            .lock()
            .map_err(|_| anyhow!("folder cache poisoned"))? = Some(folders);
        Ok(found)
    }
}

// ---------------------------------------------------------------------------
// Phase 3: Fetch messages
// ---------------------------------------------------------------------------

impl GraphClient {
    /// Fetch messages by their Graph ids, in batches of
    /// [`BATCH_MAX_REQUESTS`].
    ///
    /// This is what makes a Graph sync converge. Asking a folder for its `$top`
    /// most recent messages can never return a message that is new to the store
    /// but old on the server (one moved into Archive, say), so detection kept
    /// reporting it and the download kept missing it; naming each message by id
    /// downloads exactly what detection found.
    ///
    /// A message that fails inside the batch is logged and skipped: the rest of
    /// the pass still lands, and a message that was not ingested is simply new
    /// again next sync. Two guards keep that from becoming a treadmill (#0065):
    /// a throttled sub-response's `Retry-After` paces the remaining chunks, and
    /// after [`BATCH_FAILURE_BUDGET`] failures the pass stops asking and
    /// reports what it got, so a systematically failing id set costs one pass
    /// rather than one request and one log line per message.
    ///
    /// Whatever the reason, a short return is *reported* rather than absorbed:
    /// the caller folds it into the prune gate, because a message that was
    /// asked for and did not arrive is exactly the copy whose absence would
    /// make another mailbox's row look deleted (#0065 follow-up).
    pub async fn fetch_messages_by_ids(&self, ids: &[&str]) -> Result<BatchFetch> {
        let url = format!("{}/$batch", GRAPH_BASE);
        let mut emails = Vec::with_capacity(ids.len());
        let mut failures = 0usize;
        let mut gave_up = false;
        // The longest pause a sub-response asked for, applied at the top of the
        // next chunk. Pacing before a chunk rather than after one is what keeps
        // a throttle in the *last* chunk from sleeping up to `MAX_RETRY_AFTER_
        // SECS` with nothing left to pace, and keeps a pass that is about to
        // give up from sleeping first (#0065 follow-up).
        let mut pending_retry_after: Option<u64> = None;

        for chunk in ids.chunks(BATCH_MAX_REQUESTS) {
            if failures >= BATCH_FAILURE_BUDGET {
                warn!(
                    "Giving up on this pass's remaining message downloads after {failures} failed \
                     sub-requests; {} message(s) landed and the rest stay new",
                    emails.len(),
                );
                gave_up = true;
                break;
            }
            if let Some(secs) = pending_retry_after.take() {
                info!("Graph asked for {secs}s before the next batch; pacing the rest of the pass");
                tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
            }
            let resp = self
                .client
                .post(&url)
                .bearer_auth(self.bearer())
                .json(&batch_request_body(chunk))
                .send()
                .await
                .context("Failed to fetch messages by id")?;

            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                return Err(anyhow!(
                    "Batch message fetch failed (HTTP {}): {}",
                    status,
                    body
                ));
            }

            let batch: GraphBatchResponse = resp
                .json()
                .await
                .context("Failed to parse the batch response")?;

            for entry in batch.responses {
                // Graph is free to reorder the responses, so the request id
                // (the index into `chunk`) is the only correlation.
                let requested = entry
                    .id
                    .parse::<usize>()
                    .ok()
                    .and_then(|i| chunk.get(i).copied())
                    .unwrap_or("<unknown>");
                let status = entry.status;
                match entry.into_sub_response() {
                    SubResponse::Throttled(secs) => {
                        pending_retry_after = pending_retry_after.max(secs);
                        debug!(
                            "Graph throttled the batch fetch of message {requested} (HTTP {status})"
                        );
                    }
                    SubResponse::Failed(why) => {
                        failures += 1;
                        if failures <= BATCH_FAILURE_WARN_LIMIT {
                            warn!("Batch fetch of message {requested} failed: {why}");
                        }
                    }
                    SubResponse::Message(msg) => emails.push(GraphFetched {
                        threading: thread_hints(
                            msg.conversation_id.as_deref(),
                            &msg.single_value_extended_properties,
                        ),
                        email: self.hydrate(&msg).await,
                    }),
                }
            }
        }

        if failures > BATCH_FAILURE_WARN_LIMIT {
            warn!(
                "{failures} of {} batch sub-requests failed this pass (first \
                 {BATCH_FAILURE_WARN_LIMIT} logged above); those messages stay new",
                ids.len(),
            );
        }

        Ok(BatchFetch {
            requested: ids.len(),
            emails,
            gave_up,
        })
    }

    /// Turn one Graph message into a [`FetchedEmail`], pulling its attachments
    /// and lifting an iMIP invite out of them. Attachment failures are logged
    /// and leave the email otherwise intact.
    async fn hydrate(&self, msg: &GraphMessage) -> FetchedEmail {
        let mut email = graph_message_to_fetched_email(msg);
        if lists_attachments(msg) {
            match self.fetch_attachments(&msg.id).await {
                Ok(attachments) => email.attachments = attachments,
                Err(e) => warn!("Failed to fetch attachments for {}: {}", msg.id, e),
            }
            populate_calendar_from_attachments(&mut email);
        }
        email
    }

    /// Fetch attachments for a specific message.
    async fn fetch_attachments(&self, message_id: &str) -> Result<Vec<AttachmentData>> {
        let url = attachments_url(message_id);

        let resp = self
            .client
            .get(&url)
            .bearer_auth(self.bearer())
            .send()
            .await
            .context("Failed to fetch attachments")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "Fetch attachments failed (HTTP {}): {}",
                status,
                body
            ));
        }

        let att_list: GraphAttachmentList = resp
            .json()
            .await
            .context("Failed to parse attachment list")?;

        Ok(attachments_from_list(att_list))
    }
}

/// The URL listing a message's attachments.
///
/// It carries no `$select`: `contentBytes` exists only on the derived
/// `fileAttachment` type, and Graph rejects the whole request with HTTP 400
/// ("Could not find a property named 'contentBytes' on type
/// 'microsoft.graph.attachment'") when a `$select` on the base collection
/// names it. Without one, each entry comes back with every property of its
/// own type, so file attachments carry their bytes.
fn attachments_url(message_id: &str) -> String {
    format!("{}/me/messages/{}/attachments", GRAPH_BASE, message_id)
}

/// The URL listing a message's attachments as names and content ids only.
///
/// `contentId` is a property of the derived `fileAttachment` type, and Graph
/// refuses it by its bare name in a `$select` on the base collection (HTTP
/// 400, as it refuses `contentBytes`; a type-cast path segment is refused the
/// same way), but accepts it qualified with its type. About 1.3 KB a message
/// against the whole content of every attachment, often megabytes (measured
/// on evoqs, PERSO-165).
fn attachment_metadata_url(message_id: &str) -> String {
    format!(
        "{GRAPH_BASE}/me/messages/{message_id}/attachments?\
         $select=id,name,isInline,microsoft.graph.fileAttachment/contentId"
    )
}

/// The entries of a metadata listing, in order, as the inline backfill pairs
/// them with the stored references: an item or reference attachment was
/// never stored ([`attachments_from_list`] skips it), so it is flagged not a
/// file, which the backfill skips when pairing and counts as a real
/// attachment for the paperclip.
fn server_attachments(att_list: GraphAttachmentList) -> Vec<crate::ingest::ServerAttachment> {
    att_list
        .value
        .into_iter()
        .map(|att| crate::ingest::ServerAttachment {
            is_file: att
                .odata_type
                .as_deref()
                .is_none_or(|t| t.eq_ignore_ascii_case("#microsoft.graph.fileAttachment")),
            name: att.name.unwrap_or_else(|| "attachment".to_string()),
            content_id: att.content_id,
        })
        .collect()
}

/// The local half of the PERSO-165 inline backfill of one Graph account:
/// queue its inline-only rows for the attachment repair (once per store) and
/// classify a batch of the rows that carry a raw message. Failures are logged
/// and leave the work for the next pass.
fn classify_stored_inline_locally(
    store: &crate::store::Store,
    blobs: &crate::store::BlobStore,
    account: &str,
) {
    match crate::ingest::mark_inline_only_rows(store, blobs, account) {
        Ok(0) => {}
        Ok(n) => info!("Queued {n} inline-only message(s) for the attachment repair"),
        Err(e) => warn!("Could not queue the inline-only messages: {e:#}"),
    }
    match crate::ingest::classify_inline_from_raw(
        store,
        blobs,
        account,
        crate::ingest::INLINE_CLASSIFICATIONS_PER_PASS,
    ) {
        Ok(0) => {}
        Ok(n) => info!("Classified the attachments of {n} stored message(s) from their raw"),
        Err(e) => warn!("Could not classify the stored attachments from their raw: {e:#}"),
    }
}

/// Turn a parsed `/attachments` page into the attachments the store keeps.
///
/// Only `fileAttachment` entries carry content; an item attachment (an
/// attached message or event) or a reference attachment (a cloud link) is
/// skipped rather than failing the message, as is a zero-byte placeholder.
fn attachments_from_list(att_list: GraphAttachmentList) -> Vec<AttachmentData> {
    let mut result = Vec::new();
    for att in att_list.value {
        let is_file = att
            .odata_type
            .as_deref()
            .is_none_or(|t| t.eq_ignore_ascii_case("#microsoft.graph.fileAttachment"));
        if !is_file {
            debug!(
                "Skipping Graph {} '{}': only file attachments carry content",
                att.odata_type.as_deref().unwrap_or_default(),
                att.name.as_deref().unwrap_or_default(),
            );
            continue;
        }
        // Server-provided name is untrusted: strip path separators so it cannot
        // escape the attachments directory (the IMAP path does the same).
        let filename =
            sanitize_attachment_filename(&att.name.unwrap_or_else(|| "attachment".to_string()));
        let content = if let Some(b64) = att.content_bytes {
            use base64::Engine;
            base64::engine::general_purpose::STANDARD
                .decode(&b64)
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        // Skip zero-byte attachments (placeholder entries)
        if content.is_empty() {
            continue;
        }

        result.push(AttachmentData {
            filename,
            content,
            content_id: att.content_id,
        });
    }
    result
}

/// Whether the message's `/attachments` list is worth fetching.
///
/// Graph's `hasAttachments` is false for a message whose only attachments are
/// inline (the images an HTML body shows by `cid:` URL), so the flag alone
/// leaves such a mail with broken images. An HTML body that references `cid:`
/// is taken as a claim that the list holds something too.
fn lists_attachments(msg: &GraphMessage) -> bool {
    msg.has_attachments || references_inline_parts(msg)
}

/// Whether the message's HTML body references an inline part by `cid:` URL.
fn references_inline_parts(msg: &GraphMessage) -> bool {
    let Some(body) = msg.body.as_ref() else {
        return false;
    };
    let is_html = body
        .content_type
        .as_deref()
        .is_some_and(|ct| ct.eq_ignore_ascii_case("html"));
    is_html
        && body
            .content
            .as_deref()
            .is_some_and(|c| c.to_ascii_lowercase().contains("cid:"))
}

fn graph_message_to_fetched_email(msg: &GraphMessage) -> FetchedEmail {
    let from = msg.from.as_ref().map(format_recipient).unwrap_or_default();

    let to = format_recipients(&msg.to_recipients);
    let cc = if msg.cc_recipients.is_empty() {
        None
    } else {
        Some(format_recipients(&msg.cc_recipients))
    };

    let subject = msg.subject.clone().unwrap_or_default();

    let date = msg
        .received_date_time
        .as_deref()
        .map(iso_to_rfc2822)
        .unwrap_or_default();

    let (body_text, html_body) = if let Some(ref body) = msg.body {
        let content = body.content.as_deref().unwrap_or("");
        let is_html = body
            .content_type
            .as_deref()
            .map(|ct| ct.eq_ignore_ascii_case("html"))
            .unwrap_or(false);
        if is_html {
            // Same flatten as the IMAP path: no hard wrap, the preview
            // re-wraps to the pane width (#0111).
            let plain = crate::parse::html_to_plain(content);
            (plain, Some(content.to_string()))
        } else {
            (content.to_string(), None)
        }
    } else {
        (String::new(), None)
    };

    let message_id = msg.internet_message_id.clone();

    FetchedEmail {
        from,
        to,
        cc,
        // The Graph message shape this build deserializes carries neither
        // Reply-To nor Bcc, so the header pane shows them only for IMAP mail
        // (#0096); wiring the Graph fields is a follow-up if the path returns.
        reply_to: None,
        bcc: None,
        subject,
        date,
        body_text,
        html_body,
        // Raised for an inline-only message too, so a row whose inline parts
        // the first fetch could not bring stays an attachment repair
        // candidate; one the repair finds nothing storable for is settled
        // once, never retried.
        has_attachments: lists_attachments(msg),
        // The server's own claim, apart: true for an attached Outlook item
        // or cloud link the store never holds, false for an inline-only
        // message, which is what the paperclip needs (PERSO-165).
        server_has_attachments: Some(msg.has_attachments),
        message_id,
        attachments: Vec::new(), // filled by caller if has_attachments
        // `isRead`, and the reply or forward Outlook recorded in its MAPI
        // property (PERSO-171). The flag merge in `ingest::apply_graph_flags`
        // only ever adds a history bit, so a Graph pass never erases one a
        // user set here. Ingest itself writes the set verbatim, so a
        // *re-download* of a row this build flagged would drop a bit the
        // server does not hold; the Graph fetch only downloads Message-IDs the
        // store does not hold (`ingest::known_message_ids`), which is what
        // keeps that out of reach.
        flags: {
            let seen = crate::types::MessageFlags::seen(msg.is_read);
            LastVerb::from_properties(&msg.single_value_extended_properties)
                .map_or(seen, |verb| verb.mark(seen))
        },
        calendar_ics: None, // filled by caller once attachments are fetched
        event: None,
    }
}

/// Detect an iMIP calendar payload among fetched Graph attachments and populate
/// `calendar_ics` + `event` on the email. The Graph API delivers `text/calendar`
/// invites as a named `.ics` attachment, so both fetch paths converge on the
/// same `parse.rs`/`calendar.rs` machinery. Best-effort: parse failures still
/// leave the raw sidecar bytes so the email save writes the `.ics`.
fn populate_calendar_from_attachments(email: &mut FetchedEmail) {
    if let Some(ics) = lift_invite(&mut email.attachments) {
        email.event =
            crate::calendar::parse_ics(&ics).map(|ev| crate::calendar::event_frontmatter(&ev));
        email.calendar_ics = Some(ics);
    }
}

/// Take the iMIP invite out of `attachments`, if there is one.
///
/// Lifts only the FIRST `.ics` attachment that is an actual iMIP invite (its
/// payload parses as a VCALENDAR carrying a METHOD property). Every other
/// `.ics` (e.g. a plain calendar export the user shared) stays a regular
/// attachment with its original filename -- matching the IMAP path.
fn lift_invite(attachments: &mut Vec<AttachmentData>) -> Option<Vec<u8>> {
    let idx = attachments.iter().position(|att| {
        crate::calendar::is_ics_filename(&att.filename)
            && crate::calendar::is_imip_invite(&att.content)
    })?;
    Some(attachments.remove(idx).content)
}

/// What one attachment repair learnt about a message.
#[derive(Debug)]
enum RepairVerdict {
    /// The server listed attachment content: the sidecar invite, if any, and
    /// the remaining attachments, ready for the store.
    Attach {
        calendar_ics: Option<Vec<u8>>,
        attachments: Vec<AttachmentData>,
    },
    /// The server answered and has nothing the store can keep: only attached
    /// Outlook items, cloud links or empty placeholders. Asking again would
    /// get the same answer, so the message is settled.
    NothingToStore,
}

/// Turn a successfully fetched attachment list into what the repair writes.
fn repair_verdict(mut attachments: Vec<AttachmentData>) -> RepairVerdict {
    let calendar_ics = lift_invite(&mut attachments);
    if calendar_ics.is_none() && attachments.is_empty() {
        RepairVerdict::NothingToStore
    } else {
        RepairVerdict::Attach {
            calendar_ics,
            attachments,
        }
    }
}

// ---------------------------------------------------------------------------
// Phase 4: Sync support
// ---------------------------------------------------------------------------

impl GraphClient {
    /// Enumerate a folder: every message it holds, keyed by
    /// `internetMessageId`, with the handle and the metadata the sync needs to
    /// decide what to download, what to re-flag and what has gone.
    ///
    /// Messages without an `internetMessageId` are dropped, because the store
    /// identifies a Graph message by exactly that header (see
    /// [`crate::ingest::graph_uid`]) and an entry that cannot be matched to a
    /// row would look like a permanent new message. Since #0055 the sync also
    /// prunes from this enumeration, so such a message is not merely never
    /// downloaded but dropped locally if an older sync did download it;
    /// Exchange stamps the header on everything that goes through transport,
    /// which is what makes that acceptable.
    ///
    /// The walk is ordered by `receivedDateTime desc`, an `$orderby` Graph
    /// supports on this endpoint without a `$filter`. Unordered paging lets a concurrent
    /// arrival shift the skiptoken window and drop a message from the middle of
    /// the walk, and a dropped message is indistinguishable from a deleted one:
    /// with newest-first ordering an arrival lands on a page the walk has
    /// already passed instead (#0065).
    ///
    /// If the server rejects the `$orderby` outright (a 4xx on the first page,
    /// which some tenant or folder shapes may do and which no live Graph call
    /// has yet ruled out), the walk retries once without it. Unordered paging
    /// is a degradation, not a loss: a fully-paged enumeration still lists the
    /// whole folder, so `complete` stays honest and the prune stays open; what
    /// comes back is the pre-#0065 churn of an occasional message re-downloaded
    /// after a concurrent arrival shifted the window. A folder that never
    /// enumerates at all would be the worse failure, and that is what this
    /// avoids (#0065 follow-up).
    pub async fn enumerate_folder(&self, folder: &str) -> Result<FolderEnumeration> {
        let folder_path = self.folder_path(folder).await?;
        let mut result = HashMap::new();
        let mut complete = false;
        let mut pages = 0usize;
        let mut ordered = true;
        let mut url = enumeration_url(&folder_path, ordered);

        loop {
            if pages >= MAX_ENUMERATION_PAGES {
                warn!(
                    "Enumeration of '{folder}' stopped at {pages} pages ({} messages); \
                     this pass will not prune",
                    result.len(),
                );
                break;
            }
            pages += 1;
            let resp = self
                .client
                .get(&url)
                .bearer_auth(self.bearer())
                .send()
                .await
                .context("Failed to fetch message IDs")?;

            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                if ordered && pages == 1 && status.is_client_error() {
                    warn!(
                        "Graph rejected the ordered enumeration of '{folder}' (HTTP {status}); \
                         retrying unordered, which pages this folder correctly but may re-download \
                         a message a concurrent arrival shifts out of the walk"
                    );
                    ordered = false;
                    pages = 0;
                    url = enumeration_url(&folder_path, ordered);
                    continue;
                }
                return Err(anyhow!(
                    "Fetch message IDs from '{}' failed (HTTP {}): {}",
                    folder,
                    status,
                    body
                ));
            }

            let page: GraphMessageIdList = resp
                .json()
                .await
                .context("Failed to parse message ID list")?;

            absorb_page(&mut result, page.value);

            match page.next_link {
                Some(next) => url = next,
                None => {
                    complete = true;
                    break;
                }
            }
        }

        Ok(FolderEnumeration {
            entries: result,
            complete,
        })
    }

    /// Two-pass fetch: enumerate the folder, then download by id the messages
    /// the store does not hold.
    ///
    /// The enumeration in the returned [`FolderFetch`] covers every message in
    /// the folder, so the caller can both apply read-status changes to rows it
    /// already holds and see what has gone.
    ///
    /// `download_incomplete` counts both ways a pass can come up short: `limit`
    /// leaving new messages unfetched, and the batch handing back fewer
    /// messages than were asked for. The second was the gap the first shipped
    /// with (#0065 follow-up): a throttled-out archive target returned an empty
    /// vector, reported a complete download, and opened the prune gate on inbox
    /// rows whose archive copies had never landed.
    ///
    /// `window` is a `status-only` target's recency window (#0141): only the
    /// folder's newest `window` messages are candidates, and what lies below
    /// them is never a backlog ([`backlog_owed`]).
    pub async fn fetch_new_messages(
        &self,
        folder: &str,
        limit: usize,
        window: Option<usize>,
        known_ids: &HashSet<String>,
    ) -> Result<FolderFetch> {
        let enumeration = self.enumerate_folder(folder).await?;
        let (new, found) = select_for_download(&enumeration.entries, known_ids, limit, window);
        let capped = found > new.len();
        let skipped = enumeration.entries.len() - found;

        if new.is_empty() {
            return Ok(FolderFetch {
                new_emails: Vec::new(),
                skipped,
                enumeration,
                download_incomplete: capped,
                backlog: false,
            });
        }

        let graph_ids: Vec<&str> = new.iter().map(|entry| entry.graph_id.as_str()).collect();
        let batch = self.fetch_messages_by_ids(&graph_ids).await?;
        let download_incomplete = capped || batch.fell_short();
        let backlog = backlog_owed(window, found, graph_ids.len(), batch.fell_short());

        Ok(FolderFetch {
            new_emails: batch.emails,
            skipped,
            enumeration,
            download_incomplete,
            backlog,
        })
    }
}

/// The first-page URL of a folder enumeration, with or without the `$orderby`.
///
/// Split out so the ordered form and the fallback
/// ([`GraphClient::enumerate_folder`] retries once unordered on a 4xx) are one
/// definition rather than two literals that can drift apart.
fn enumeration_url(folder_path: &str, ordered: bool) -> String {
    let orderby = if ordered {
        "$orderby=receivedDateTime%20desc&"
    } else {
        ""
    };
    format!(
        "{}/me/mailFolders/{}/messages?\
         $select=id,internetMessageId,isRead,receivedDateTime&\
         {}{LAST_VERB_EXPAND}&$top=200",
        GRAPH_BASE, folder_path, orderby
    )
}

// ---------------------------------------------------------------------------
// #0042: /messages/delta
// ---------------------------------------------------------------------------

/// One entry of a `/messages/delta` page: either a message whose state
/// changed, or a removal.
///
/// A removal carries `id` and `@removed` and *nothing else*, which is the
/// whole reason the prune below does not consume it: see [`FolderDelta`].
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphDeltaEntry {
    id: String,
    #[serde(default)]
    internet_message_id: Option<String>,
    #[serde(default)]
    is_read: bool,
    #[serde(default)]
    received_date_time: Option<String>,
    #[serde(rename = "@removed", default)]
    removed: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct GraphDeltaPage {
    value: Vec<GraphDeltaEntry>,
    #[serde(rename = "@odata.nextLink")]
    next_link: Option<String>,
    #[serde(rename = "@odata.deltaLink")]
    delta_link: Option<String>,
}

/// What one delta walk saw: the messages whose state changed since the stored
/// token, how many removals it was told about, and the token that resumes
/// after it.
///
/// `changed` is keyed like [`FolderEnumeration::entries`], on the trimmed
/// `internetMessageId`, but it is **not** a folder listing: it is a change set,
/// so `known − changed` is meaningless and the prune must never be computed
/// from it.
#[derive(Debug, Clone, Default)]
pub struct FolderDelta {
    pub changed: HashMap<String, FolderEntry>,
    /// How many `@removed` entries the walk reported.
    pub removed: usize,
    /// The `@odata.deltaLink` the last page carried. A walk that ended without
    /// one never becomes a [`FolderDelta`]; it is a [`DeltaVerdict::Discard`].
    pub delta_link: String,
    /// Pages walked, for the log line and the timing evidence.
    pub pages: usize,
}

impl FolderDelta {
    /// Whether this delta hands the pass back to the full enumeration.
    ///
    /// **The `@removed` decision (#0042).** A removal entry names the message
    /// by Graph's own `id`, and the store does not hold that id: a Graph row's
    /// identity is [`crate::ingest::graph_uid`] of the `internetMessageId`,
    /// which a removal entry does not carry and which the server will not sell
    /// back for a message it has just deleted. So the delta cannot map a
    /// removal onto a row, and the prune keeps its existing source of truth,
    /// the full folder listing (`known − enumerated`, [`vanished_graph_uids`]).
    ///
    /// A pass whose delta reports any removal therefore escalates: it throws
    /// the change set away, enumerates the folder in full and prunes exactly as
    /// a pre-#0042 pass did. That keeps the #0072/#0074 coverage and
    /// deferred-prune gates working on unchanged inputs, and it costs the
    /// enumeration only on passes that would have had something to delete.
    ///
    /// The rejected alternative was to persist Graph's message id per row so a
    /// removal could be resolved directly. That is a schema column and a store
    /// rebuild for every account, and it buys latency on deletions rather than
    /// correctness; it is recorded as a follow-up on the ticket instead.
    fn forces_full_enumeration(&self) -> bool {
        self.removed > 0
    }
}

/// Why a pass took the full enumeration instead of the delta, or what it did
/// with a token it could not use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeltaVerdict {
    /// Use the stored token.
    Use,
    /// No token stored: this is the bootstrap pass.
    NoToken,
    /// A full sync (`limit == usize::MAX`) always relists the folder, so the
    /// prune and the token both get a fresh, whole-folder observation.
    FullSync,
    /// The folder's Graph id is not the one the token was minted against.
    FolderChanged,
    /// Something about the token or the walk is in doubt; the token is dropped
    /// and this pass enumerates.
    Discard(DeltaDiscard),
}

/// The reasons a delta token is thrown away rather than merely unused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeltaDiscard {
    /// Graph answered 410: the sync state behind the token is gone.
    Expired,
    /// The stored string is not a Graph delta URL.
    Malformed,
    /// The page chain hit [`MAX_DELTA_PAGES`].
    PageCap,
    /// The walk ran out of pages without an `@odata.deltaLink`, so there is no
    /// resume point and no proof the chain was complete.
    NoResumePoint,
    /// The request failed for any other reason.
    Failed,
}

/// Whether this pass may use the stored delta token, decided before any
/// request is issued.
///
/// The cardinal rule of #0042, and the same one #0041 wrote for CONDSTORE: the
/// delta may only ever be *faster*, never the only thing that looked. Every
/// branch that is not an exact match on a token this client minted for this
/// folder falls back to the full enumeration, which is the pre-#0042 pass and
/// misses nothing.
///
/// `identity_matches` is the Graph analogue of UIDVALIDITY: the token is bound
/// to a folder id, so a folder deleted and recreated under the same well-known
/// name (or a config that repoints a role at a different folder) invalidates
/// it. Graph would answer such a token with a 404 or a 410 anyway, which the
/// walk would discard on; this check makes the invalidation the client's own
/// rather than a server behaviour we depend on.
fn delta_verdict(limit: usize, stored: Option<&str>, identity_matches: bool) -> DeltaVerdict {
    let Some(token) = stored else {
        return DeltaVerdict::NoToken;
    };
    if !token.starts_with(GRAPH_BASE) || !token.contains("/delta") {
        return DeltaVerdict::Discard(DeltaDiscard::Malformed);
    }
    if !identity_matches {
        return DeltaVerdict::FolderChanged;
    }
    // A full sync is the periodic whole-folder observation everything else
    // leans on: it is what reopens the prune on removals the delta declined to
    // resolve, and what re-mints the token from a listing rather than from a
    // chain of increments.
    if limit == usize::MAX {
        return DeltaVerdict::FullSync;
    }
    DeltaVerdict::Use
}

/// What an HTTP status on the delta endpoint means for the token.
///
/// 410 is the documented expiry (`resyncRequired`); 404 is the folder having
/// gone out from under the token. Everything else that is not a success is a
/// plain failure, and all three land in the same place, because a delta that
/// did not complete cannot be told apart from one that skipped a message.
fn delta_status_discard(status: reqwest::StatusCode) -> Option<DeltaDiscard> {
    if status.is_success() {
        None
    } else if status == reqwest::StatusCode::GONE || status == reqwest::StatusCode::NOT_FOUND {
        Some(DeltaDiscard::Expired)
    } else {
        Some(DeltaDiscard::Failed)
    }
}

/// Fold one delta page into the change set, counting removals separately.
///
/// An entry with `@removed` is counted and dropped: it names a message by
/// Graph id, which the store cannot resolve (see
/// [`FolderDelta::forces_full_enumeration`]). An entry with no usable
/// `internetMessageId` is dropped for the same reason [`absorb_page`] drops
/// one.
fn absorb_delta_page(
    changed: &mut HashMap<String, FolderEntry>,
    removed: &mut usize,
    page: Vec<GraphDeltaEntry>,
) {
    for entry in page {
        if entry.removed.is_some() {
            *removed += 1;
            continue;
        }
        let Some(mid) = entry.internet_message_id else {
            continue;
        };
        let mid = mid.trim();
        if mid.is_empty() {
            continue;
        }
        changed.insert(
            mid.to_string(),
            FolderEntry {
                graph_id: entry.id,
                is_read: entry.is_read,
                received: entry.received_date_time,
                // The delta refuses the `$expand` that would carry it.
                last_verb: None,
            },
        );
    }
}

/// The first-page URL of a delta walk, and the `$deltatoken=latest` form that
/// mints a resume point without enumerating anything.
fn delta_url(folder_path: &str, latest: bool) -> String {
    let latest = if latest { "&$deltatoken=latest" } else { "" };
    format!(
        "{}/me/mailFolders/{}/messages/delta?\
         $select=id,internetMessageId,isRead,receivedDateTime{}",
        GRAPH_BASE, folder_path, latest
    )
}

/// A `$select=id` folder response.
///
/// Not [`GraphMailFolder`]: that type requires `displayName`, which a
/// `$select=id` projection does not return, so reusing it would fail the parse
/// of every identity check.
#[derive(Debug, Deserialize)]
struct GraphFolderId {
    id: String,
}

/// The folder id as the 63-bit hash the `uidvalidity` column can hold.
///
/// Graph's folder id is the analogue of IMAP's UIDVALIDITY (it is what a
/// resume token is bound to) so it is stored in the analogous column, hashed
/// because the column is an integer. Same hash as
/// [`crate::ingest::graph_uid`], which is a stable 63-bit digest of a string
/// and carries no message-specific meaning.
fn folder_identity_hash(folder_id: &str) -> i64 {
    crate::ingest::graph_uid(folder_id)
}

impl GraphClient {
    /// The folder's Graph id: the identity a delta token is bound to.
    async fn folder_identity(&self, folder: &str) -> Result<String> {
        let url = format!(
            "{}/me/mailFolders/{}?$select=id",
            GRAPH_BASE,
            self.folder_path(folder).await?
        );
        let resp = self
            .client
            .get(&url)
            .bearer_auth(self.bearer())
            .send()
            .await
            .context("Failed to read the folder id")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "Read folder id of '{}' failed (HTTP {}): {}",
                folder,
                status,
                body
            ));
        }
        let folder: GraphFolderId = resp.json().await.context("Failed to parse the folder")?;
        Ok(folder.id)
    }

    /// Mint a delta resume point for `folder` describing the folder *now*,
    /// without listing it (`$deltatoken=latest`).
    ///
    /// Called **before** the enumeration it will be stored alongside, never
    /// after: a token taken after the listing would silently cover the window
    /// between the two, and a message that arrived in it would never be
    /// reported by any pass. Taken first, that window is replayed by the next
    /// delta at worst.
    ///
    /// A tenant that rejects `$deltatoken=latest` gets `None` and keeps the
    /// pre-#0042 behaviour for good; no token is ever guessed.
    async fn mint_delta_token(&self, folder: &str) -> Option<String> {
        let folder_path = match self.folder_path(folder).await {
            Ok(p) => p,
            Err(e) => {
                warn!("Could not mint a delta token for '{folder}': {e}");
                return None;
            }
        };
        let url = delta_url(&folder_path, true);
        let resp = match self
            .client
            .get(&url)
            .bearer_auth(self.bearer())
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                warn!("Could not mint a delta token for '{folder}': {e}");
                return None;
            }
        };
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            warn!(
                "Graph refused a delta token for '{folder}' (HTTP {status}): {body}; \
                 this account keeps enumerating the folder in full"
            );
            return None;
        }
        match resp.json::<GraphDeltaPage>().await {
            Ok(page) => page.delta_link,
            Err(e) => {
                warn!("Could not parse the delta token for '{folder}': {e}");
                None
            }
        }
    }

    /// Walk `/messages/delta` from a stored token.
    ///
    /// `Ok(Ok(delta))` is a complete chain ending in a fresh resume point;
    /// `Ok(Err(discard))` is every doubt there is, and every one of them means
    /// the same thing to the caller: drop the token, enumerate the folder.
    async fn walk_delta(&self, link: &str) -> Result<FolderDelta, DeltaDiscard> {
        let mut changed = HashMap::new();
        let mut removed = 0usize;
        let mut pages = 0usize;
        let mut url = link.to_string();

        loop {
            if pages >= MAX_DELTA_PAGES {
                return Err(DeltaDiscard::PageCap);
            }
            pages += 1;
            let resp = match self
                .client
                .get(&url)
                .bearer_auth(self.bearer())
                .header("Prefer", format!("odata.maxpagesize={DELTA_PAGE_SIZE}"))
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    warn!("Graph delta request failed: {e}");
                    return Err(DeltaDiscard::Failed);
                }
            };
            if let Some(discard) = delta_status_discard(resp.status()) {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                warn!("Graph delta answered HTTP {status}: {body}");
                return Err(discard);
            }
            let page: GraphDeltaPage = match resp.json().await {
                Ok(p) => p,
                Err(e) => {
                    warn!("Could not parse a Graph delta page: {e}");
                    return Err(DeltaDiscard::Failed);
                }
            };
            absorb_delta_page(&mut changed, &mut removed, page.value);

            match (page.next_link, page.delta_link) {
                (Some(next), _) => url = next,
                (None, Some(delta_link)) => {
                    return Ok(FolderDelta {
                        changed,
                        removed,
                        delta_link,
                        pages,
                    })
                }
                (None, None) => return Err(DeltaDiscard::NoResumePoint),
            }
        }
    }

    /// The delta twin of [`GraphClient::fetch_new_messages`]: walk the change
    /// set, then download by id the changed messages the store does not hold.
    ///
    /// Returns `Ok(None)` when the delta reported removals, which hands the
    /// pass to the full enumeration with the token left in place
    /// ([`FolderDelta::forces_full_enumeration`]).
    async fn fetch_delta(
        &self,
        folder: &str,
        limit: usize,
        window: Option<usize>,
        known_ids: &HashSet<String>,
        link: &str,
    ) -> Result<Option<DeltaFetch>, DeltaDiscard> {
        let delta = self.walk_delta(link).await?;
        info!(
            "Graph delta for '{folder}': {} page(s), {} changed, {} removed",
            delta.pages,
            delta.changed.len(),
            delta.removed,
        );
        if delta.forces_full_enumeration() {
            return Ok(None);
        }

        let (new, found) = select_for_download(&delta.changed, known_ids, limit, window);
        let capped = found > new.len();
        if new.is_empty() {
            return Ok(Some(DeltaFetch {
                new_emails: Vec::new(),
                changed: delta.changed,
                delta_link: delta.delta_link,
                download_incomplete: capped,
                backlog: false,
            }));
        }
        let graph_ids: Vec<&str> = new.iter().map(|entry| entry.graph_id.as_str()).collect();
        let batch = match self.fetch_messages_by_ids(&graph_ids).await {
            Ok(b) => b,
            Err(e) => {
                warn!("Graph delta download failed for '{folder}': {e:#}");
                return Err(DeltaDiscard::Failed);
            }
        };
        let download_incomplete = capped || batch.fell_short();
        let backlog = backlog_owed(window, found, graph_ids.len(), batch.fell_short());
        Ok(Some(DeltaFetch {
            new_emails: batch.emails,
            changed: delta.changed,
            delta_link: delta.delta_link,
            download_incomplete,
            backlog,
        }))
    }
}

/// What one delta pass hands the orchestrator.
///
/// The shape deliberately differs from [`FolderFetch`]: there is no
/// `enumeration`, because a change set is not a folder listing and nothing may
/// diff against it, and no `skipped`, because a delta does not walk past the
/// messages it already holds.
struct DeltaFetch {
    new_emails: Vec<GraphFetched>,
    /// The changed messages, for the read-flag pass.
    changed: HashMap<String, FolderEntry>,
    /// The resume point, stored only if this pass covered and wrote
    /// everything.
    delta_link: String,
    download_incomplete: bool,
    /// [`FolderFetch::backlog`], for the change set.
    backlog: bool,
}

/// Whether a folder's pass left a backlog that another quick pass can drain at
/// once (PERSO-172).
///
/// A capped pass downloads the newest `limit` messages the store lacks *by id*,
/// so the next pass lists the leftovers as new again and takes the next window:
/// a backlog drains one window per pass with no full sync. At one quick tick per
/// `sync_interval_secs` that took hours for a new account of a few thousand
/// messages, so the runtime asks for the next tick right away while this holds
/// (see `scheduler::BACKLOG_FOLLOW_UP`).
///
/// Only the cap counts. A batch that fell short is a throttle or a failure,
/// which an immediate retry would only make worse, and a pass that asked for
/// nothing (`-n 0`) made no progress a retry would repeat; both wait for the
/// regular interval. A stored delta token cannot hide the backlog: a capped
/// pass never mints one ([`may_record_delta_token`]), so the next pass
/// enumerates again or replays the same change set from the older token.
///
/// A windowed (`status-only`) target never owes one (#0141): what it leaves
/// below its newest `window` messages is its policy, and draining it pass
/// after pass would download the whole trash or junk folder.
fn backlog_owed(window: Option<usize>, found: usize, asked: usize, fell_short: bool) -> bool {
    window.is_none() && found > asked && asked > 0 && !fell_short
}

/// What one folder's ingest loop left behind, for the decisions taken after it.
#[derive(Debug, Default, PartialEq, Eq)]
struct WindowIngest {
    /// Messages of the window the store now holds, newly inserted or not.
    written: usize,
    /// A message the store refused and still owes a retry
    /// ([`crate::ingest::note_ingest_failure`] answered true): the pass did
    /// not write everything it downloaded.
    owed_failure: bool,
}

/// Whether the folder is worth another quick pass at once (PERSO-172), given
/// that its fetch left a backlog ([`backlog_owed`]) and what the window wrote.
///
/// A message the store refused and still owes a retry is the same doubt as a
/// short batch: the regular interval retries it, an immediate tick would only
/// repeat it.
///
/// A window that wrote nothing owes nothing either. A message given up on
/// after [`crate::ingest::MAX_INGEST_ATTEMPTS`] counts against nothing, yet it
/// never enters `known_message_ids`, so every pass lists it as new and puts it
/// in the window again, newest first. A window made only of such messages
/// would otherwise owe a follow-up for ever: the same bodies downloaded every
/// few seconds, none of them ever stored. Progress is the test because it is
/// what a follow-up buys; a window with one given-up message among written
/// ones still moves the drain on.
fn owes_follow_up(backlog: bool, window: &WindowIngest) -> bool {
    backlog && !window.owed_failure && window.written > 0
}

/// Write one folder's downloaded window into the store, counting into
/// `result` what it inserted.
///
/// An inserted inbox message is announced as new mail (the desktop
/// notification of #0009) only when it is newer than every row the account
/// held before the pass ([`crate::ingest::arrived_after`] over `newest_held`).
/// The Graph pull is by id, newest first, so a backlog drain and a first sync
/// insert hundreds of messages that are old to their owner; a message that
/// arrives during the drain is the newest of its window and is still
/// announced.
fn ingest_window(
    store: &crate::store::Store,
    blobs: &crate::store::BlobStore,
    account_name: &str,
    target: &SyncTarget,
    new_emails: &[GraphFetched],
    newest_held: Option<i64>,
    result: &mut SyncResult,
) -> WindowIngest {
    let mut window = WindowIngest::default();
    for fetched in new_emails {
        let email = &fetched.email;
        let message_id = crate::ingest::resolve_message_id(email, None);
        let uid = crate::ingest::graph_uid(&message_id);
        match crate::ingest::ingest_message_threaded(
            store,
            blobs,
            &crate::ingest::IngestInput {
                account: account_name,
                mailbox: target.role.as_str(),
                uid,
                email,
                raw: None,
            },
            &crate::ingest::RebindPolicy::Always,
            Some(&fetched.threading),
        ) {
            Ok(outcome) => {
                window.written += 1;
                crate::ingest::clear_ingest_failure(store, account_name, target.role.as_str(), uid);
                if outcome.inserted {
                    result.saved += 1;
                    if target.role.is_inbox()
                        && crate::ingest::arrived_after(newest_held, &email.date)
                    {
                        result
                            .new_inbox_mail
                            .push(crate::notify::NewMailMeta::new(&email.from, &email.subject));
                    }
                }
                if outcome.uid_rebound {
                    result.uid_rebound += 1;
                }
                result.fresh_observations.push(FreshObservation {
                    role: target.role.clone(),
                    from: email.from.clone(),
                    to: email.to.clone(),
                    cc: email.cc.clone(),
                    date: email.date.clone(),
                });
            }
            Err(e) => {
                warn!(
                    "Failed to ingest {} from {}: {:#}",
                    message_id, target.role, e
                );
                window.owed_failure |= crate::ingest::note_ingest_failure(
                    store,
                    account_name,
                    target.role.as_str(),
                    &target.server_name,
                    uid,
                    &format!("{e:#} (message {message_id})"),
                );
            }
        }
    }
    window
}

/// Whether a pass has earned the right to store its delta resume point.
///
/// The token means "at this point the store held every message the folder
/// listed", and that is the whole safety argument for using it later: a delta
/// from a token with that property reports every subsequent change, so the
/// property survives. A pass that left a message undownloaded, or downloaded
/// one it could not write, does not have the property and must not mint the
/// claim; the older token stays and the next pass replays from it.
///
/// This is the same rule #0041 wrote for the CONDSTORE resume point, for the
/// same reason, and it is what keeps the #0074 ingest bound honest on this
/// path: `ingest_failed` is false once a poisoned message has been given up
/// on, so a message the store will never accept cannot wedge the delta chain
/// for good.
fn may_record_delta_token(covered: bool, download_incomplete: bool, ingest_failed: bool) -> bool {
    covered && !download_incomplete && !ingest_failed
}

/// Fold one page of an enumeration into the map.
///
/// The key is `internetMessageId` **trimmed**, because that is the identity
/// [`crate::ingest::resolve_message_id`] stores and [`crate::ingest::graph_uid`]
/// hashes. Keying on the header verbatim made a single padded id look both new
/// (no matching row) and vanished (no matching enumeration key) on every pass:
/// a delete-and-re-download loop, with the flags never applying because the uid
/// was computed from the untrimmed string (#0065).
///
/// A message with no usable `internetMessageId` is dropped; see
/// [`GraphClient::enumerate_folder`] for why that is acceptable.
fn absorb_page(result: &mut HashMap<String, FolderEntry>, page: Vec<GraphMessageIdEntry>) {
    for entry in page {
        let Some(mid) = entry.internet_message_id else {
            continue;
        };
        let mid = mid.trim();
        if mid.is_empty() {
            continue;
        }
        result.insert(
            mid.to_string(),
            FolderEntry {
                graph_id: entry.id,
                is_read: entry.is_read,
                received: entry.received_date_time,
                last_verb: LastVerb::from_properties(&entry.single_value_extended_properties),
            },
        );
    }
}

/// The flags the server states for one held row: `isRead`, and the history
/// bit its last verb stands for (PERSO-171). What the merge does with them is
/// [`crate::ingest::apply_graph_flags`]'s business.
fn server_flags(entry: &FolderEntry) -> crate::types::MessageFlags {
    let seen = crate::types::MessageFlags::seen(entry.is_read);
    entry.last_verb.map_or(seen, |verb| verb.mark(seen))
}

/// The messages this pass will download, and how many new ones it found in all.
///
/// The two differ exactly when `limit` left some behind, and that difference is
/// what suspends the prune: a pass that could not fetch everything it found
/// cannot also be trusted to say what has been deleted (see [`pass_may_prune`]).
///
/// A windowed target (#0141) selects among the folder's newest `window`
/// messages, held or not, the way the IMAP window takes the newest UIDs of the
/// listing. Capping the new ones instead would take the next older 50 on every
/// pass, since the store lacks them by id, and download the whole folder over
/// a run of passes. `found` still counts every new message of the folder, so a
/// windowed pass that left history on the server reports itself capped and
/// mints no delta token ([`may_record_delta_token`]).
fn select_for_download<'a>(
    server: &'a HashMap<String, FolderEntry>,
    known_ids: &HashSet<String>,
    limit: usize,
    window: Option<usize>,
) -> (Vec<&'a FolderEntry>, usize) {
    let found = server
        .keys()
        .filter(|mid| !known_ids.contains(mid.as_str()))
        .count();
    let new = newest_first(server)
        .into_iter()
        .take(window.unwrap_or(usize::MAX))
        .filter(|(mid, _)| !known_ids.contains(*mid))
        .map(|(_, entry)| entry)
        .take(limit)
        .collect();
    (new, found)
}

/// Every message of the listing, newest first.
///
/// Newest first is what makes a capped sync useful (the quick sync passes
/// `limit = 100`): the pass downloads the most recent arrivals, and because it
/// downloads them *by id* the leftovers are known next pass, so a backlog
/// drains one window at a time instead of the same window repeating. Ties and
/// missing dates fall back to the message id so the order is total and a
/// truncated pass is reproducible.
fn newest_first(server: &HashMap<String, FolderEntry>) -> Vec<(&str, &FolderEntry)> {
    let mut all: Vec<(&str, &FolderEntry)> = server
        .iter()
        .map(|(mid, entry)| (mid.as_str(), entry))
        .collect();
    all.sort_by(|(a_mid, a), (b_mid, b)| {
        b.received.cmp(&a.received).then_with(|| a_mid.cmp(b_mid))
    });
    all
}

/// The rows the store holds for a mailbox that the folder no longer lists, as
/// the synthetic UIDs [`crate::ingest::graph_uid`] gives them.
///
/// The Graph counterpart of [`crate::imap_client::vanished_uids`], and simpler:
/// the enumeration covers the whole folder rather than a recency window, so
/// there is no range to clamp the diff to. An enumeration that came back empty
/// means an empty folder, and emptying a folder on the server does empty it
/// locally.
///
/// What replaces the IMAP clamp is the caller's two gates, because both of the
/// cases the clamp defends against exist here too (#0065): a pass that did not
/// see the whole folder or could not download its whole backlog does not prune
/// at all ([`sync_mailboxes_graph`]), and a row too freshly dated to be
/// anything but a local ingest the server has yet to file is held back
/// ([`crate::ingest::prunable_uids`]).
fn vanished_graph_uids(
    known_ids: &HashSet<String>,
    server: &HashMap<String, FolderEntry>,
) -> Vec<i64> {
    known_ids
        .iter()
        .filter(|mid| !server.contains_key(mid.as_str()))
        .map(|mid| crate::ingest::graph_uid(mid))
        .collect()
}

// ---------------------------------------------------------------------------
// Phase 4: sync_mailboxes_graph
// ---------------------------------------------------------------------------

/// What one attachment repair pass over a mailbox did, for its log line.
#[derive(Debug, Default, PartialEq, Eq)]
struct RepairTally {
    repaired: usize,
    settled: usize,
    failed: usize,
}

impl GraphClient {
    /// Fetch the attachments the first ingest of a message could not.
    ///
    /// A message whose attachment fetch failed is ingested anyway, with its
    /// body and without its attachments, and from then on the store knows it,
    /// so no later pass downloads it again. This is the pass that closes the
    /// gap: it takes at most [`crate::ingest::ATTACHMENT_REPAIRS_PER_PASS`]
    /// messages of the mailbox that Graph marked `hasAttachments` and the store
    /// holds no attachment for, newest first, and asks the server again.
    ///
    /// Three outcomes, each recorded so the candidate set converges: the
    /// attachments are written and the message leaves the set; the server has
    /// nothing storable (only attached items or links) and the message is
    /// settled for good; or the fetch fails and the message waits out a
    /// doubling backoff capped at a day, never given up on, because the
    /// failure may be a bug a later build fixes.
    ///
    /// `server` is whatever listing this pass holds for the folder, a full
    /// enumeration or a delta change set; a message it does not name is looked
    /// up by its `Message-ID`.
    async fn repair_missing_attachments(
        &self,
        store: &crate::store::Store,
        blobs: &crate::store::BlobStore,
        account: &str,
        mailbox: &str,
        server: &HashMap<String, FolderEntry>,
        now: i64,
    ) -> RepairTally {
        let mut tally = RepairTally::default();
        let candidates = match crate::ingest::attachment_repair_candidates(
            store,
            account,
            mailbox,
            now,
            crate::ingest::ATTACHMENT_REPAIRS_PER_PASS,
        ) {
            Ok(candidates) => candidates,
            Err(e) => {
                warn!("Could not list the messages of '{mailbox}' owed their attachments: {e:#}");
                return tally;
            }
        };
        for candidate in candidates {
            let outcome = match self.repair_verdict_for(&candidate, server).await {
                Ok(RepairVerdict::Attach {
                    calendar_ics,
                    attachments,
                }) => crate::ingest::attach_missing_attachments(
                    store,
                    blobs,
                    candidate.row_id,
                    calendar_ics.as_deref(),
                    &attachments,
                )
                .map(|_| true),
                Ok(RepairVerdict::NothingToStore) => {
                    crate::ingest::settle_attachment_repair(store, candidate.row_id).map(|_| false)
                }
                Err(e) => Err(e),
            };
            match outcome {
                Ok(true) => tally.repaired += 1,
                Ok(false) => tally.settled += 1,
                Err(e) => {
                    tally.failed += 1;
                    let error = format!("{e:#}");
                    match crate::ingest::note_attachment_repair_failure(
                        store,
                        candidate.row_id,
                        &error,
                        now,
                    ) {
                        Ok(attempts) => warn!(
                            "Attachment repair of {} in '{mailbox}' failed (attempt {attempts}, \
                             retried in {}s): {error}",
                            candidate.message_id,
                            crate::ingest::attachment_repair_backoff_secs(attempts),
                        ),
                        Err(record) => warn!(
                            "Attachment repair of {} in '{mailbox}' failed ({error}) and could \
                             not be recorded: {record:#}",
                            candidate.message_id
                        ),
                    }
                }
            }
        }
        if tally != RepairTally::default() {
            info!(
                "Graph attachment repair in '{mailbox}': {} repaired, {} with nothing to store, \
                 {} failed",
                tally.repaired, tally.settled, tally.failed
            );
        }
        tally
    }

    /// Mark the attachments an older build stored for the rows of `mailbox`
    /// that have no raw message, from the server's attachment metadata
    /// (PERSO-165).
    ///
    /// At most [`crate::ingest::INLINE_LOOKUPS_PER_PASS`] rows a pass, newest
    /// first, one metadata request each (names and content ids, no content).
    /// A row the server no longer lists is marked from what the store holds,
    /// every reference not inline, so it leaves the set too. A failed request
    /// ends the pass for this mailbox and leaves the rest for the next one:
    /// a failure that persists would otherwise cost every candidate a request
    /// on every pass.
    async fn classify_stored_inline(
        &self,
        store: &crate::store::Store,
        blobs: &crate::store::BlobStore,
        account: &str,
        mailbox: &str,
        server: &HashMap<String, FolderEntry>,
    ) -> usize {
        let candidates = match crate::ingest::inline_lookup_candidates(
            store,
            account,
            mailbox,
            crate::ingest::INLINE_LOOKUPS_PER_PASS,
        ) {
            Ok(candidates) => candidates,
            Err(e) => {
                warn!("Could not list the rows of '{mailbox}' owed an inline mark: {e:#}");
                return 0;
            }
        };
        let mut classified = 0;
        for candidate in &candidates {
            let listing = match self.inline_metadata_for(candidate, server).await {
                Ok(listing) => listing,
                Err(e) => {
                    warn!(
                        "Inline mark of {} in '{mailbox}' failed, the rest wait for the next \
                         pass: {e:#}",
                        candidate.message_id
                    );
                    break;
                }
            };
            match crate::ingest::apply_inline_metadata(
                store,
                blobs,
                candidate.row_id,
                listing.as_deref(),
            ) {
                Ok(()) => classified += 1,
                Err(e) => {
                    warn!(
                        "Could not record the inline mark of {}: {e:#}",
                        candidate.message_id
                    );
                    break;
                }
            }
        }
        if classified > 0 {
            info!(
                "Graph inline mark in '{mailbox}': {classified} of {} row(s) classified",
                candidates.len()
            );
        }
        classified
    }

    /// The attachment metadata of one inline candidate, `None` when the
    /// server lists no message with its `Message-ID`.
    async fn inline_metadata_for(
        &self,
        candidate: &crate::ingest::InlineLookupCandidate,
        server: &HashMap<String, FolderEntry>,
    ) -> Result<Option<Vec<crate::ingest::ServerAttachment>>> {
        let graph_id = match server.get(&candidate.message_id) {
            Some(entry) => entry.graph_id.clone(),
            None => match self
                .find_message_by_internet_id(&candidate.message_id)
                .await?
            {
                Some(id) => id,
                None => return Ok(None),
            },
        };
        self.fetch_attachment_metadata(&graph_id).await.map(Some)
    }

    /// The file attachments of one message as names and content ids, in the
    /// server's order, without their content (PERSO-165).
    async fn fetch_attachment_metadata(
        &self,
        message_id: &str,
    ) -> Result<Vec<crate::ingest::ServerAttachment>> {
        let resp = self
            .client
            .get(attachment_metadata_url(message_id))
            .bearer_auth(self.bearer())
            .send()
            .await
            .context("Failed to fetch the attachment metadata")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "Fetch attachment metadata failed (HTTP {status}): {body}"
            ));
        }
        let list: GraphAttachmentList = resp
            .json()
            .await
            .context("Failed to parse the attachment metadata")?;
        Ok(server_attachments(list))
    }

    /// Ask the server what attachments one repair candidate has.
    async fn repair_verdict_for(
        &self,
        candidate: &crate::ingest::AttachmentRepairCandidate,
        server: &HashMap<String, FolderEntry>,
    ) -> Result<RepairVerdict> {
        let graph_id = match server.get(&candidate.message_id) {
            Some(entry) => entry.graph_id.clone(),
            None => self
                .find_message_by_internet_id(&candidate.message_id)
                .await?
                .ok_or_else(|| anyhow!("the server lists no message with this Message-ID"))?,
        };
        let attachments = self.fetch_attachments(&graph_id).await?;
        Ok(repair_verdict(attachments))
    }

    /// Every message of `folder` with the threading signals the sync download
    /// now asks for, keyed by trimmed `internetMessageId`, and whether the walk
    /// reached the last page (PERSO-147).
    ///
    /// The listing carries `conversationId` and the two MAPI threading
    /// properties ([`MESSAGE_EXPAND`]) and nothing else, about 510 bytes a
    /// message, so a folder of a few thousand messages is a few requests and
    /// about a megabyte, and no body is downloaded again.
    async fn list_folder_threading(
        &self,
        folder: &str,
    ) -> Result<(HashMap<String, crate::ingest::ThreadHints>, bool)> {
        let folder_path = self.folder_path(folder).await?;
        let mut url = threading_listing_url(&folder_path);
        let mut hints = HashMap::new();
        for _ in 0..MAX_ENUMERATION_PAGES {
            let resp = self
                .client
                .get(&url)
                .bearer_auth(self.bearer())
                .send()
                .await
                .context("Failed to list the folder's threading signals")?;
            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                return Err(anyhow!(
                    "Listing the threading signals of '{folder}' failed (HTTP {status}): {body}"
                ));
            }
            let page: GraphThreadingList = resp
                .json()
                .await
                .context("Failed to parse the threading listing")?;
            absorb_threading_page(&mut hints, page.value);
            match page.next_link {
                Some(next) => url = next,
                None => return Ok((hints, true)),
            }
        }
        warn!(
            "The threading listing of '{folder}' stopped at {MAX_ENUMERATION_PAGES} pages; \
             the rows it did not reach stay owed a re-thread"
        );
        Ok((hints, false))
    }

    /// Give the rows of `mailbox` an older build ingested without threading
    /// signals the ones the server holds, and re-thread them (PERSO-147).
    ///
    /// A no-op once every row of the mailbox has been asked: ingest writes the
    /// signals of every new message, so the candidates are only ever the rows
    /// stored before the fix, and the walk runs once per mailbox. A failure is
    /// logged and leaves the candidates for the next pass.
    async fn rethread_stored(
        &self,
        store: &crate::store::Store,
        account: &str,
        target: &SyncTarget,
    ) -> crate::ingest::RethreadTally {
        let mailbox = target.role.as_str();
        let candidates = match crate::ingest::rethread_candidates(store, account, mailbox) {
            Ok(c) if c.is_empty() => return crate::ingest::RethreadTally::default(),
            Ok(c) => c,
            Err(e) => {
                warn!("Could not list the rows of '{mailbox}' owed a re-thread: {e:#}");
                return crate::ingest::RethreadTally::default();
            }
        };
        let (hints, complete) = match self.list_folder_threading(&target.server_name).await {
            Ok(listing) => listing,
            Err(e) => {
                warn!(
                    "Graph re-thread of '{mailbox}' skipped this pass, {} row(s) still owed: {e:#}",
                    candidates.len()
                );
                return crate::ingest::RethreadTally::default();
            }
        };
        match crate::ingest::rethread_rows(store, account, &candidates, &hints, complete) {
            Ok(tally) => {
                info!(
                    "Graph re-thread of '{mailbox}': {} of {} row(s) given their threading \
                     signals, {} moved to another conversation, {} not on the server",
                    tally.updated,
                    candidates.len(),
                    tally.merged,
                    tally.settled,
                );
                tally
            }
            Err(e) => {
                warn!("Graph re-thread of '{mailbox}' failed: {e:#}");
                crate::ingest::RethreadTally::default()
            }
        }
    }
}

/// The first-page URL of the re-thread's folder listing.
fn threading_listing_url(folder_path: &str) -> String {
    format!(
        "{GRAPH_BASE}/me/mailFolders/{folder_path}/messages?\
         $select=internetMessageId,conversationId&{MESSAGE_EXPAND}&$top=200"
    )
}

/// Fold one page of the threading listing into `hints`, keyed by the trimmed
/// `internetMessageId` the store keys rows by ([`absorb_page`] says why).
fn absorb_threading_page(
    hints: &mut HashMap<String, crate::ingest::ThreadHints>,
    page: Vec<GraphThreadingEntry>,
) {
    for entry in page {
        let Some(mid) = entry.internet_message_id else {
            continue;
        };
        let mid = mid.trim();
        if mid.is_empty() {
            continue;
        }
        hints.insert(
            mid.to_string(),
            thread_hints(
                entry.conversation_id.as_deref(),
                &entry.single_value_extended_properties,
            ),
        );
    }
}

/// Sync orchestrator for Graph accounts, store-only. Mirrors the IMAP
/// `sync_mailboxes`, with two documented differences forced by the API:
///
/// - Graph never returns the original RFC822, so rows get `raw_blob` NULL and
///   the body/attachment blobs are the only copies of the content;
/// - Graph has no UID, so the row's `uid` is [`crate::ingest::graph_uid`] of
///   the message's `Message-ID`, which is stable per message and keeps the
///   `UNIQUE (account, mailbox, uid)` identity meaningful.
///
/// The orchestration mirrors [`crate::imap_client::sync_mailboxes`] line for
/// line, prune second pass included: the prunes are held back until every
/// target has been ingested, so a message archived in Outlook web has its
/// archive row before its inbox row goes and never spends a window with no row
/// anywhere (#0055).
///
/// Three conditions gate that prune, standing in for the IMAP window clamp
/// (#0065): the pass must have enumerated every target in full, it must have
/// landed every new message it found (a quick sync's `limit`, a batch
/// sub-response failure, a throttle that ran the pass out of budget and an
/// ingest error all count against this), and a row dated within
/// [`crate::ingest::PRUNE_MIN_AGE_SECS`] of now is left alone. Without the
/// first two, a message moved between two folders loses its source row in the
/// pass that could not fetch its destination copy, and holds no row anywhere
/// until the backlog drains.
///
/// Since #0042 a quick sync may replace the enumeration with a
/// `/messages/delta` walk from the token in `sync_cursors.deltalink`. The
/// delta is strictly an accelerator: [`delta_verdict`] refuses it on anything
/// unusual, any doubt during the walk throws the token away
/// ([`DeltaDiscard`]), a delta that reports removals hands the pass back to the
/// enumeration ([`FolderDelta::forces_full_enumeration`]), a full sync always
/// relists, and a token is only ever minted by a pass that saw the whole
/// folder and wrote every message in it ([`may_record_delta_token`]).
///
/// `stop` is asked before each target (PERSO-127), and only there: a Graph
/// target fetches and ingests in one step, so its mailbox boundary is the one
/// point at which nothing is half done. A stopped pass returns what the
/// targets before the stop wrote, and runs no prune.
pub async fn sync_mailboxes_graph(
    config: &GraphConfig,
    account_name: &str,
    targets: &[SyncTarget],
    limit: usize,
    dry_run: bool,
    stop: Option<&Stop>,
) -> Result<SyncResult> {
    info!(
        "sync_mailboxes_graph: account={account_name}, {} targets, limit={limit}, dry_run={dry_run}",
        targets.len(),
    );
    let span_label = if limit < usize::MAX {
        "sync_mailboxes_graph:quick"
    } else {
        "sync_mailboxes_graph:full"
    };
    let mut span = TimingSpan::with_context(span_label, format!("{} targets", targets.len()));

    let client = GraphClient::new_async(config).await?;
    let store = crate::store::Store::open_for_pass(account_name, dry_run)?;
    let blobs = crate::store::BlobStore::for_account(account_name);
    let mut result = SyncResult::default();
    span.mark("client_open");

    // Every prune this run will apply, collected here and applied after the
    // loop: see the second pass below for why it cannot run per target.
    let mut prunes: Vec<(MailboxRole, Vec<i64>)> = Vec::new();
    // `(enumeration complete, download truncated)` per target, which decides
    // whether the prunes above may be applied at all: see `pass_may_prune`.
    let mut coverage: Vec<(bool, bool)> = Vec::with_capacity(targets.len());
    // What "new" means for the notification: newer than anything the store
    // held before this pass, read once so a target synced earlier in the pass
    // does not raise the bar for the inbox (see `ingest_window`).
    let newest_held = crate::ingest::newest_held_date(&store, account_name);

    // The inline images of rows an older build stored (PERSO-165), the local
    // half: the inline-only rows queued for the attachment repair once per
    // store, and the rows that carry a raw message (the outbox's Sent copies)
    // classified from it, a bounded batch per pass.
    if !dry_run {
        classify_stored_inline_locally(&store, &blobs, account_name);
        span.mark("inline_local");
    }

    for (index, target) in targets.iter().enumerate() {
        if stops_at(stop, StopPoint::Mailbox(index)) {
            info!(
                "[sync] {account_name}: stopped before '{}', {index} of {} mailbox(es) synced",
                target.server_name,
                targets.len()
            );
            return Ok(result);
        }
        let known = crate::ingest::known_message_ids(&store, account_name, target.role.as_str())?;
        let mailbox = target.role.as_str();
        let stored = crate::ingest::load_mailbox_cursor(&store, account_name, mailbox)?;
        let stored_token = stored.as_ref().and_then(|c| c.deltalink.clone());
        let stored_identity = stored.as_ref().and_then(|c| c.uidvalidity);

        // The folder id, read fresh. An id we could not read is not evidence
        // that the folder is the same one, so it counts as a mismatch and the
        // pass enumerates; the *stored* identity is kept, because a failed
        // lookup is no evidence of a change either.
        let observed_identity = if dry_run {
            None
        } else {
            match client.folder_identity(&target.server_name).await {
                Ok(id) => Some(folder_identity_hash(&id)),
                Err(e) => {
                    warn!("Could not read the folder id of {}: {e:#}", target.role);
                    None
                }
            }
        };
        let identity_matches = observed_identity.is_some() && observed_identity == stored_identity;
        let identity = observed_identity.or(stored_identity);

        // A dry run writes nothing, and every delta branch below is a write:
        // it either drops a token or mints one. So a dry run takes the
        // pre-#0042 pass, unchanged.
        let verdict = if dry_run {
            DeltaVerdict::FullSync
        } else {
            delta_verdict(limit, stored_token.as_deref(), identity_matches)
        };
        if let DeltaVerdict::Discard(reason) = verdict {
            info!("Graph delta token for {} dropped ({reason:?})", target.role);
            crate::ingest::clear_mailbox_deltalink(&store, account_name, mailbox);
        }
        if verdict == DeltaVerdict::FolderChanged {
            info!(
                "Graph folder id for {} is not the one its delta token was minted against; \
                 dropping the token and enumerating",
                target.role
            );
            crate::ingest::clear_mailbox_deltalink(&store, account_name, mailbox);
        }

        // The delta half. `None` here means this pass enumerates, for any of
        // the reasons above or because the walk itself was in doubt.
        let mut delta_fetch: Option<DeltaFetch> = None;
        if verdict == DeltaVerdict::Use {
            let link = stored_token.clone().unwrap_or_default();
            match client
                .fetch_delta(&target.server_name, limit, target.window, &known, &link)
                .await
            {
                Ok(Some(d)) => delta_fetch = Some(d),
                Ok(None) => info!(
                    "Graph delta for {} reported removals; enumerating the folder so the \
                     prune can resolve them",
                    target.role
                ),
                Err(reason) => {
                    info!(
                        "Graph delta for {} abandoned ({reason:?}); dropping the token and \
                         enumerating",
                        target.role
                    );
                    crate::ingest::clear_mailbox_deltalink(&store, account_name, mailbox);
                }
            }
        }

        // What the pass saw, in the two shapes it can come in. `server` is a
        // whole folder listing and may be diffed against the store; `changed`
        // is a change set and may not.
        let (new_emails, server, complete, download_incomplete, backlog, token, used_delta) =
            match delta_fetch {
                Some(d) => (
                    d.new_emails,
                    d.changed,
                    // A delta pass covers the folder in the sense the prune gate
                    // asks about: its token asserts the store held everything the
                    // folder listed at token time, and this walk brought in every
                    // change since, so nothing another target's prune might need
                    // is missing. It contributes no prunes of its own.
                    true,
                    d.download_incomplete,
                    d.backlog,
                    Some(d.delta_link),
                    true,
                ),
                None => {
                    // Minted before the enumeration, never after: a token taken
                    // afterwards would silently swallow the window between the two.
                    let minted = if dry_run {
                        None
                    } else {
                        client.mint_delta_token(&target.server_name).await
                    };
                    let fetch = match client
                        .fetch_new_messages(&target.server_name, limit, target.window, &known)
                        .await
                    {
                        Ok(v) => v,
                        Err(e) => {
                            warn!("Graph sync failed for {}: {}", target.role, e);
                            // A target that did not sync at all is the strongest
                            // form of partial pass: the copy that would justify
                            // another target's deletion may be exactly what this
                            // fetch failed to bring in.
                            coverage.push((false, false));
                            continue;
                        }
                    };
                    let FolderFetch {
                        new_emails,
                        skipped,
                        enumeration,
                        download_incomplete,
                        backlog,
                    } = fetch;
                    result.skipped += skipped;
                    (
                        new_emails,
                        enumeration.entries,
                        enumeration.complete,
                        download_incomplete,
                        backlog,
                        minted,
                        false,
                    )
                }
            };
        span.mark(&format!("fetch:{}", target.role));

        // What a `status-only` window leaves on the server is its policy, not a
        // short pass (#0141, [`SyncTarget::short_download_counts`]).
        let short = download_incomplete && target.short_download_counts();
        if dry_run {
            coverage.push((complete, short));
            result.saved += new_emails.len();
            continue;
        }

        // A message that was downloaded but not written is as absent from the
        // store as one that was never fetched, so it counts against this
        // target's coverage too (#0065 follow-up).
        //
        // Bounded the same way the IMAP path is (#0074 review): without the
        // bound, one message this store rejects every time would set
        // `truncated` on every pass and suspend the account's prune for good.
        // `ingest::note_ingest_failure` counts the attempts per
        // `(account, mailbox, uid)` and gives up loudly after
        // `MAX_INGEST_ATTEMPTS`; a success clears the count, so transient
        // failures never accumulate towards it. The arrival mark has no Graph
        // half -- the pull is by id, with no positional window (see the cursor
        // below) -- so the give-up only releases the prune gate.
        let window = ingest_window(
            &store,
            &blobs,
            account_name,
            target,
            &new_emails,
            newest_held,
            &mut result,
        );
        let ingest_failed = window.owed_failure;
        coverage.push((complete, short || ingest_failed));
        // A window that moved the drain on leaves the next one for the next
        // pass, which the runtime runs at once rather than an interval from now.
        if owes_follow_up(backlog, &window) {
            info!(
                "Graph sync of {} left older new messages for the next pass",
                target.role
            );
            result.backlogged.push(target.server_name.clone());
        }
        span.mark(&format!("ingest:{}", target.role));

        // Read status, and the reply or forward Outlook recorded (PERSO-171),
        // for messages the store already holds, one transaction for the whole
        // folder.
        result.flags_updated += crate::ingest::apply_graph_flags(
            &store,
            account_name,
            target.role.as_str(),
            server
                .iter()
                .map(|(mid, entry)| (crate::ingest::graph_uid(mid), server_flags(entry))),
        );
        span.mark(&format!("flags:{}", target.role));

        // Messages stored without the attachments their first fetch failed
        // to bring, a bounded handful per pass.
        client
            .repair_missing_attachments(
                &store,
                &blobs,
                account_name,
                mailbox,
                &server,
                crate::outbox::unix_now(),
            )
            .await;
        span.mark(&format!("attachment_repair:{}", target.role));

        // Rows an older build stored without threading signals, once.
        client.rethread_stored(&store, account_name, target).await;
        span.mark(&format!("rethread:{}", target.role));

        // Attachments an older build stored without their inline mark, a
        // bounded batch per pass (PERSO-165).
        client
            .classify_stored_inline(&store, &blobs, account_name, mailbox, &server)
            .await;
        span.mark(&format!("inline:{}", target.role));

        // The other half of the same diff: what the store holds here and the
        // server does not. Held back until every target has been ingested (see
        // the second pass below).
        //
        // Only a full enumeration may compute it. `server` on a delta pass is
        // a change set, so `known − server` would name every message that did
        // not happen to change since the token: the entire mailbox (#0042).
        if !used_delta {
            let vanished = vanished_graph_uids(&known, &server);
            if !vanished.is_empty() {
                prunes.push((target.role.clone(), vanished));
            }
        }

        // The resume point is only minted by a pass that saw the whole folder
        // and wrote every message it found, and only alongside the folder id
        // it is bound to: a token with no identity could never be validated
        // again (#0042).
        let earned = may_record_delta_token(complete, download_incomplete, ingest_failed);
        let minted_a_token = token.is_some();
        let token_to_store = if earned { token } else { None };

        if used_delta {
            // A delta pass observed no folder listing, so it writes the token
            // and nothing else: `exists` and the rest are observations it did
            // not make, and `record_mailbox_cursor` would null them.
            if let (Some(identity), Some(link)) = (identity, token_to_store.as_deref()) {
                crate::ingest::record_delta_token(&store, account_name, mailbox, identity, link);
            }
        } else if complete {
            // Only a complete enumeration knows how many messages the folder
            // holds; a short one would record a count the UI shows as truth,
            // so the last known-good stays instead.
            crate::ingest::record_mailbox_cursor(
                &store,
                account_name,
                mailbox,
                &crate::ingest::MailboxCursor {
                    // The Graph analogue of UIDVALIDITY: the folder id its
                    // delta token is bound to, hashed into the column.
                    uidvalidity: identity,
                    last_uid: None,
                    uidnext: None,
                    exists: Some(server.len() as i64),
                    highest_modseq: None,
                    deltalink: if identity.is_some() {
                        token_to_store
                    } else {
                        None
                    },
                    // IMAP-only: the Graph pull downloads by id, so it has no
                    // positional window that can leave an arrival behind.
                    arrival_mark: None,
                },
            )?;
            if earned && stored_token.is_some() && !minted_a_token {
                // This pass covered the folder but could not mint a token, so
                // the stored one is older than a listing that has just been
                // fully consumed. Keeping it would replay the same changes
                // (and, if they included removals, escalate to this same
                // enumeration) on every pass for good.
                crate::ingest::clear_mailbox_deltalink(&store, account_name, mailbox);
            }
        }
    }

    // Second pass: every prune, after every target has been ingested. The
    // ordering argument is [`crate::imap_client::sync_mailboxes`]'s, and holds
    // here for the same reason: targets are synced inbox, archive, sent, so
    // pruning inside the loop would delete the inbox row of a message archived
    // in Outlook web before the archive pass ingests it.
    if pass_may_prune(&coverage) {
        let now = crate::outbox::unix_now();
        for (role, vanished) in &prunes {
            // The age guard is the other half: a row the outbox has just
            // ingested locally is not something the server ever listed under
            // that identity, so it is in every vanished set until the server's
            // own copy shows up.
            let prunable =
                crate::ingest::prunable_uids(&store, account_name, role.as_str(), vanished, now);
            result.pruned += crate::ingest::prune_vanished(
                &store,
                &blobs,
                account_name,
                role.as_str(),
                &prunable,
            );
        }
    } else {
        result.prunes_deferred = prunes.iter().map(|(_, v)| v.len()).sum();
        if result.prunes_deferred > 0 {
            info!(
                "Graph sync: {} pending prune(s) deferred; this pass did not see every message",
                result.prunes_deferred,
            );
        }
    }
    span.mark("prune");

    Ok(result)
}

// ---------------------------------------------------------------------------
// Phase 5: Send via Graph
// ---------------------------------------------------------------------------

/// The largest base64 MIME body sent in one Graph request, a `/me/sendMail`
/// or a `/me/messages` draft (PERSO-144).
///
/// Graph caps a request at about 4 MB (a 413 or "maximum request length
/// supported is 4MB" above it); 3.5 MB of base64, about 2.6 MB of raw MIME,
/// leaves headroom for the request line and headers and for the cap being
/// counted in MiB or MB. A message above it goes as a draft whose attachments
/// are added one request each ([`crate::send::graph_submit`]).
pub const MIME_SEND_MAX_BASE64_BYTES: usize = 3_500_000;

/// The request body `/me/sendMail` takes for a MIME send: the whole
/// message, standard base64, unwrapped.
pub fn mime_send_body(mime: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(mime)
}

/// Whether a base64 MIME body of `base64_len` bytes is small enough for one
/// MIME request; see [`MIME_SEND_MAX_BASE64_BYTES`].
pub fn fits_mime_send(base64_len: usize) -> bool {
    base64_len <= MIME_SEND_MAX_BASE64_BYTES
}

/// The largest attachment added to a draft in one `POST .../attachments`.
///
/// Graph's guidance is one POST under 3 MB and an upload session from 3 MB to
/// 150 MB; the POST carries the bytes as base64 inside JSON, so 2.5 MB of file
/// is about 3.4 MB of request, under the same 4 MB cap with the same headroom
/// [`MIME_SEND_MAX_BASE64_BYTES`] keeps.
pub const ATTACHMENT_POST_MAX_BYTES: usize = 2_500_000;

/// The byte range one upload-session `PUT` carries: Graph asks for less than
/// 4 MB per request.
pub const UPLOAD_CHUNK_BYTES: usize = 3 * 1024 * 1024;

/// One file attachment added to a Graph draft after the draft is created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutgoingAttachment {
    pub name: String,
    pub content_type: String,
    pub bytes: Vec<u8>,
}

/// The MAPI named property a JSON-created draft carries our `Message-ID` in
/// (#0063 review).
///
/// Exchange gives a draft created from the JSON resource a `Message-ID` of its
/// own, and `internetMessageHeaders` takes only `x-` names and cannot be read
/// by `$filter` anyway. A `singleValueExtendedProperty` can, so this is how
/// the resume finds a JSON draft an interrupted create left behind, and its
/// sent copy if the property stays on the item when it is sent (a live check
/// #0063 still owes).
pub const MESSAGE_ID_PROPERTY: &str =
    "String {5b0bd1a4-8b35-4c39-9a4e-2f7d3c1e6a90} Name MailypoppinsMessageId";

/// The `singleValueExtendedProperties` entry stamping a JSON draft with our
/// `Message-ID` ([`MESSAGE_ID_PROPERTY`]).
pub fn message_id_property(internet_message_id: &str) -> serde_json::Value {
    serde_json::json!([{ "id": MESSAGE_ID_PROPERTY, "value": internet_message_id }])
}

/// The `Prefer` value every request of a staged send carries, so the draft id
/// recorded on the outbox row is the item's immutable id (#0063 review).
///
/// A default Outlook id changes whenever the item moves between folders, so
/// the id a draft was created under answers 404 once `/send` has moved it to
/// the Outbox or Sent Items, and the resume could never tell a send under way
/// from a draft deleted. An immutable id stays with the item through both
/// moves. Sent on every request that returns or takes such an id (the
/// creates, the attachments, `/send`, `permanentDelete`, the lookups and the
/// Drafts folder id their `parentFolderId` is compared with), so the ids stay
/// one kind.
pub const PREFER_IMMUTABLE_ID: &str = "IdType=\"ImmutableId\"";

/// What the mailbox holds under one RFC 5322 `Message-ID`, for the outbox's
/// resume (#0063 item 3).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerCopies {
    /// A copy that is not a draft exists: Exchange has sent the message and
    /// filed it (Sent Items, or wherever a rule moved it).
    pub sent: bool,
    /// The Graph ids of the copies still sitting in Drafts.
    pub drafts: Vec<String>,
}

/// Where one recorded draft is now, read by its Graph id (#0063 review).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DraftState {
    /// A draft, in the Drafts folder: its send never took.
    InDrafts,
    /// Still a draft, outside Drafts: a send under way (the Outbox).
    Sending,
    /// No longer a draft: the message was sent and kept its id.
    Sent,
    /// No item has this id any more. A sent draft gets a new id when it is
    /// filed in Sent Items, so this is no proof either way.
    Gone,
}

/// The JSON body of `POST /me/messages/{id}/attachments` for one small file.
pub fn attachment_post_body(attachment: &OutgoingAttachment) -> serde_json::Value {
    use base64::Engine;
    serde_json::json!({
        "@odata.type": "#microsoft.graph.fileAttachment",
        "name": attachment.name,
        "contentType": attachment.content_type,
        "contentBytes": base64::engine::general_purpose::STANDARD.encode(&attachment.bytes),
    })
}

/// The JSON body of `POST /me/messages/{id}/attachments/createUploadSession`.
pub fn upload_session_body(attachment: &OutgoingAttachment) -> serde_json::Value {
    serde_json::json!({
        "AttachmentItem": {
            "attachmentType": "file",
            "name": attachment.name,
            "contentType": attachment.content_type,
            "size": attachment.bytes.len(),
        }
    })
}

/// The `[start, end)` byte ranges an upload session sends, in order.
pub fn upload_ranges(total: usize, chunk: usize) -> Vec<(usize, usize)> {
    let chunk = chunk.max(1);
    (0..total)
        .step_by(chunk)
        .map(|start| (start, (start + chunk).min(total)))
        .collect()
}

/// The `Content-Range` header of one upload-session `PUT`: inclusive bounds,
/// then the file size.
pub fn content_range(start: usize, end: usize, total: usize) -> String {
    format!("bytes {start}-{}/{total}", end.saturating_sub(1))
}

/// `/me/messages/{id}/{action}`.
fn message_action_url(message_id: &str, action: &str) -> String {
    format!("{GRAPH_BASE}/me/messages/{message_id}/{action}")
}

/// A request that may deliver the message failed: say whether it can have.
///
/// A connect or builder failure never reached the API, so nothing was sent;
/// anything else (a timeout, a dropped response) leaves it unknown, and the
/// outbox must settle it by asking the mailbox rather than by sending again.
/// See [`crate::outbox::AmbiguousSubmission`].
fn delivering_request_error(e: reqwest::Error, what: &str) -> anyhow::Error {
    if e.is_connect() || e.is_builder() {
        anyhow!(e).context(format!("{what} via Graph API"))
    } else {
        anyhow!(crate::outbox::AmbiguousSubmission(e.to_string()))
            .context(format!("{what} via Graph API"))
    }
}

/// A request that may deliver the message was answered with an error status:
/// say whether it can have delivered (#0063 review).
///
/// `sendMail` and `/send` answer 202 and finish later, so a 5xx from Graph's
/// front end (a 504 from the gateway, a 500) can arrive after Exchange took
/// the message. Only a 4xx proves nothing went out, along with Graph's two
/// throttling answers, a 429 and a 503 carrying `Retry-After`, which it gives
/// before it processes anything. Every other status is wrapped in
/// [`crate::outbox::AmbiguousSubmission`], so the outbox settles it by asking
/// the mailbox rather than by sending again.
///
/// `gone_is_ambiguous` is for `/send` on a draft: a 404 there means the item
/// left Drafts, which is what a send already under way does to it.
pub(crate) fn delivering_status_error(
    status: u16,
    retry_after: bool,
    gone_is_ambiguous: bool,
    message: String,
) -> anyhow::Error {
    let throttled = status == 429 || (status == 503 && retry_after);
    let refused = (400..500).contains(&status) && !(gone_is_ambiguous && status == 404);
    if throttled || refused {
        anyhow!(message)
    } else {
        anyhow!(crate::outbox::AmbiguousSubmission(message))
    }
}

/// The error a delivering request's non-2xx answer becomes, read off the
/// response ([`delivering_status_error`]).
async fn delivering_response_error(
    resp: reqwest::Response,
    gone_is_ambiguous: bool,
    what: &str,
) -> anyhow::Error {
    let status = resp.status();
    let retry_after = resp.headers().contains_key(reqwest::header::RETRY_AFTER);
    let body = resp.text().await.unwrap_or_default();
    delivering_status_error(
        status.as_u16(),
        retry_after,
        gone_is_ambiguous,
        format!("{what} (HTTP {status}): {body}"),
    )
}

/// The id of the message a `201 Created` answered with.
#[derive(Debug, Deserialize)]
struct CreatedMessage {
    id: String,
}

/// The `uploadUrl` of a new upload session.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UploadSession {
    upload_url: String,
}

/// One copy of a message, as the resume's lookup selects it.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CopyEntry {
    #[serde(default)]
    id: String,
    #[serde(default)]
    is_draft: bool,
    #[serde(default)]
    parent_folder_id: Option<String>,
}

/// The Graph id of a folder.
#[derive(Debug, Deserialize)]
struct FolderId {
    id: String,
}

/// The copies two lookups found, folded into what the resume asks.
fn fold_copies(drafts_folder: &str, found: Vec<CopyEntry>) -> ServerCopies {
    let mut copies = ServerCopies::default();
    for copy in found {
        if !copy.is_draft {
            copies.sent = true;
        } else if copy.parent_folder_id.as_deref() == Some(drafts_folder)
            && !copies.drafts.contains(&copy.id)
        {
            copies.drafts.push(copy.id);
        }
    }
    copies
}

/// Read where a draft is from its `isDraft` and `parentFolderId`.
fn draft_state_of(drafts_folder: &str, entry: &CopyEntry) -> DraftState {
    if !entry.is_draft {
        DraftState::Sent
    } else if entry.parent_folder_id.as_deref() == Some(drafts_folder) {
        DraftState::InDrafts
    } else {
        DraftState::Sending
    }
}

#[derive(Debug, Deserialize)]
struct CopyList {
    value: Vec<CopyEntry>,
}

impl GraphClient {
    /// Send a whole RFC 5322 message through `/me/sendMail` (PERSO-144).
    ///
    /// `mime_base64` is [`mime_send_body`] of the bytes: Graph takes the
    /// message base64-encoded as a `text/plain` body, parses recipients
    /// (`Bcc:` included) from its headers and keeps its `Message-ID`,
    /// `In-Reply-To` and `References`, which the JSON form cannot carry.
    /// Saved to Sent Items, the endpoint's default.
    pub async fn send_mime(&self, mime_base64: String) -> Result<()> {
        let url = format!("{}/me/sendMail", GRAPH_BASE);
        let resp = self
            .client
            .post(&url)
            .header(reqwest::header::CONTENT_TYPE, "text/plain")
            .body(mime_base64)
            .bearer_auth(self.bearer())
            .send()
            .await
            .map_err(|e| delivering_request_error(e, "Failed to send mail"))?;
        if resp.status().is_success() {
            info!("Graph sendMail succeeded");
            Ok(())
        } else {
            Err(delivering_response_error(resp, false, "Graph sendMail failed").await)
        }
    }

    /// Create a draft from a whole RFC 5322 message, `POST /me/messages` with
    /// the base64 MIME as a `text/plain` body, and return its Graph id.
    ///
    /// The same MIME conversion `/me/sendMail` runs, so the draft keeps the
    /// headers a MIME send keeps; the difference is that nothing goes out
    /// until [`GraphClient::send_draft`].
    pub async fn create_draft_mime(&self, mime_base64: String) -> Result<String> {
        self.create_draft(self.create_draft_mime_request(mime_base64))
            .await
    }

    fn create_draft_mime_request(&self, mime_base64: String) -> reqwest::RequestBuilder {
        self.item_request(reqwest::Method::POST, format!("{GRAPH_BASE}/me/messages"))
            .header(reqwest::header::CONTENT_TYPE, "text/plain")
            .body(mime_base64)
    }

    /// Create a draft from a JSON message resource and return its Graph id.
    pub async fn create_draft_json(&self, message: &serde_json::Value) -> Result<String> {
        self.create_draft(self.create_draft_json_request(message))
            .await
    }

    fn create_draft_json_request(&self, message: &serde_json::Value) -> reqwest::RequestBuilder {
        self.item_request(reqwest::Method::POST, format!("{GRAPH_BASE}/me/messages"))
            .json(message)
    }

    /// A request of the staged send: the bearer token and
    /// [`PREFER_IMMUTABLE_ID`], so every draft id it answers with or names is
    /// the immutable one.
    fn item_request(
        &self,
        method: reqwest::Method,
        url: impl reqwest::IntoUrl,
    ) -> reqwest::RequestBuilder {
        self.client
            .request(method, url)
            .bearer_auth(self.bearer())
            .header("Prefer", PREFER_IMMUTABLE_ID)
    }

    async fn create_draft(&self, request: reqwest::RequestBuilder) -> Result<String> {
        let resp = request
            .send()
            .await
            .context("Failed to create the Graph draft")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "Creating the Graph draft failed (HTTP {status}): {body}"
            ));
        }
        let created: CreatedMessage = resp
            .json()
            .await
            .context("Failed to read the created Graph draft")?;
        Ok(created.id)
    }

    /// Add one file to a draft: a single POST when it is small, an upload
    /// session in [`UPLOAD_CHUNK_BYTES`] ranges otherwise.
    pub async fn add_attachment(
        &self,
        draft_id: &str,
        attachment: &OutgoingAttachment,
    ) -> Result<()> {
        if attachment.bytes.len() <= ATTACHMENT_POST_MAX_BYTES {
            let resp = self
                .attachment_post_request(draft_id, attachment)
                .send()
                .await
                .with_context(|| format!("Failed to attach {}", attachment.name))?;
            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                return Err(anyhow!(
                    "Attaching {} failed (HTTP {status}): {body}",
                    attachment.name
                ));
            }
            return Ok(());
        }

        let resp = self
            .upload_session_request(draft_id, attachment)
            .send()
            .await
            .with_context(|| format!("Failed to open an upload session for {}", attachment.name))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "Opening an upload session for {} failed (HTTP {status}): {body}",
                attachment.name
            ));
        }
        let session: UploadSession = resp
            .json()
            .await
            .context("Failed to read the upload session")?;

        let total = attachment.bytes.len();
        for (start, end) in upload_ranges(total, UPLOAD_CHUNK_BYTES) {
            // The upload URL is pre-authenticated: Graph refuses a bearer
            // token on it.
            let resp = self
                .client
                .put(&session.upload_url)
                .header(
                    reqwest::header::CONTENT_RANGE,
                    content_range(start, end, total),
                )
                .body(attachment.bytes[start..end].to_vec())
                .send()
                .await
                .with_context(|| format!("Failed to upload {}", attachment.name))?;
            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                return Err(anyhow!(
                    "Uploading {} failed at byte {start} (HTTP {status}): {body}",
                    attachment.name
                ));
            }
        }
        Ok(())
    }

    fn attachment_post_request(
        &self,
        draft_id: &str,
        attachment: &OutgoingAttachment,
    ) -> reqwest::RequestBuilder {
        self.item_request(
            reqwest::Method::POST,
            message_action_url(draft_id, "attachments"),
        )
        .json(&attachment_post_body(attachment))
    }

    fn upload_session_request(
        &self,
        draft_id: &str,
        attachment: &OutgoingAttachment,
    ) -> reqwest::RequestBuilder {
        self.item_request(
            reqwest::Method::POST,
            message_action_url(draft_id, "attachments/createUploadSession"),
        )
        .json(&upload_session_body(attachment))
    }

    /// Send a draft, `POST /me/messages/{id}/send`. Exchange files it in Sent
    /// Items and it stops being a draft, keeping its immutable id.
    pub async fn send_draft(&self, draft_id: &str) -> Result<()> {
        let resp = self
            .send_draft_request(draft_id)
            .send()
            .await
            .map_err(|e| delivering_request_error(e, "Failed to send the draft"))?;
        if resp.status().is_success() {
            info!("Graph draft send succeeded");
            Ok(())
        } else {
            Err(delivering_response_error(resp, true, "Graph draft send failed").await)
        }
    }

    /// Remove a draft this client created and will not send, for good
    /// (`permanentDelete`, so it does not land in Deleted Items). A draft that
    /// is already gone counts as removed.
    pub async fn discard_draft(&self, draft_id: &str) -> Result<()> {
        let resp = self
            .discard_draft_request(draft_id)
            .send()
            .await
            .context("Failed to discard the Graph draft")?;
        if resp.status().is_success() || resp.status() == reqwest::StatusCode::NOT_FOUND {
            Ok(())
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            Err(anyhow!(
                "Discarding the Graph draft failed (HTTP {status}): {body}"
            ))
        }
    }

    /// What the mailbox holds under one `Message-ID`: whether a sent copy
    /// exists anywhere, and which copies still sit in Drafts.
    ///
    /// Looked up twice, by `internetMessageId` (a MIME send keeps ours) and by
    /// [`MESSAGE_ID_PROPERTY`] (a JSON draft carries ours only there).
    pub async fn server_copies(&self, internet_message_id: &str) -> Result<ServerCopies> {
        let drafts_folder = self.drafts_folder_id().await?;
        let select = "id,isDraft,parentFolderId";
        let by_header: CopyList = self
            .get_json(internet_id_url("me/messages", internet_message_id, select)?)
            .await?;
        let by_property: CopyList = self
            .get_json(property_lookup_url(
                "me/messages",
                internet_message_id,
                select,
            )?)
            .await?;
        Ok(fold_copies(
            &drafts_folder,
            by_header
                .value
                .into_iter()
                .chain(by_property.value)
                .collect(),
        ))
    }

    /// Where the draft with this Graph id is now:
    /// `GET /me/messages/{id}?$select=isDraft,parentFolderId`, a 404 being
    /// [`DraftState::Gone`] (#0063 review).
    pub async fn draft_state(&self, draft_id: &str) -> Result<DraftState> {
        let drafts_folder = self.drafts_folder_id().await?;
        let resp = self
            .draft_state_request(draft_id)?
            .send()
            .await
            .context("Failed to look the draft up")?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(DraftState::Gone);
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("Draft lookup failed (HTTP {status}): {body}"));
        }
        let entry: CopyEntry = resp
            .json()
            .await
            .context("Failed to parse the draft lookup")?;
        Ok(draft_state_of(&drafts_folder, &entry))
    }

    /// The Graph id of the Drafts folder, which `parentFolderId` is compared
    /// against.
    async fn drafts_folder_id(&self) -> Result<String> {
        let url = reqwest::Url::parse(&format!("{GRAPH_BASE}/me/mailFolders/drafts?$select=id"))
            .context("parsing the Drafts folder URL")?;
        let folder: FolderId = self.get_json(url).await?;
        Ok(folder.id)
    }

    fn send_draft_request(&self, draft_id: &str) -> reqwest::RequestBuilder {
        self.item_request(reqwest::Method::POST, message_action_url(draft_id, "send"))
            .header(reqwest::header::CONTENT_LENGTH, "0")
    }

    fn discard_draft_request(&self, draft_id: &str) -> reqwest::RequestBuilder {
        self.item_request(
            reqwest::Method::POST,
            message_action_url(draft_id, "permanentDelete"),
        )
        .header(reqwest::header::CONTENT_LENGTH, "0")
    }

    fn draft_state_request(&self, draft_id: &str) -> Result<reqwest::RequestBuilder> {
        let mut url = reqwest::Url::parse(&format!("{GRAPH_BASE}/me/messages"))
            .context("parsing the Graph messages URL")?;
        url.path_segments_mut()
            .map_err(|()| anyhow!("the Graph messages URL cannot take a path"))?
            .push(draft_id);
        url.set_query(Some("$select=isDraft,parentFolderId"));
        Ok(self.item_request(reqwest::Method::GET, url))
    }

    /// The GET behind [`GraphClient::server_copies`] and the Drafts folder id.
    async fn get_json<T: serde::de::DeserializeOwned>(&self, url: reqwest::Url) -> Result<T> {
        let resp = self
            .item_request(reqwest::Method::GET, url)
            .send()
            .await
            .context("Failed to look the message up")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("Message lookup failed (HTTP {status}): {body}"));
        }
        resp.json()
            .await
            .context("Failed to parse the message lookup")
    }
}

// ---------------------------------------------------------------------------
// Phase 6: Archive, delete, mark read via Graph
// ---------------------------------------------------------------------------

impl GraphClient {
    /// Find a message by its RFC822 Internet Message-ID.
    /// Searches across all folders.
    pub async fn find_message_by_internet_id(
        &self,
        internet_message_id: &str,
    ) -> Result<Option<String>> {
        Ok(self
            .find_messages_by_internet_id(internet_message_id)
            .await?
            .into_iter()
            .next())
    }

    /// Every copy of a message the mailbox holds, by its RFC822 Message-ID:
    /// one Graph id per folder it is filed in.
    pub async fn find_messages_by_internet_id(
        &self,
        internet_message_id: &str,
    ) -> Result<Vec<String>> {
        let url = message_lookup_url(internet_message_id)?;

        let resp = self
            .client
            .get(url)
            .bearer_auth(self.bearer())
            .send()
            .await
            .context("Failed to search for message by ID")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "Find message by ID failed (HTTP {}): {}",
                status,
                body
            ));
        }

        let result: GraphMessageIdLookup = resp
            .json()
            .await
            .context("Failed to parse message lookup")?;

        Ok(result.value.into_iter().map(|e| e.id).collect())
    }

    /// The RFC 822 bytes of one message, as Graph exports it through
    /// `/messages/{id}/$value` (PERSO-174).
    ///
    /// A Graph row is stored without a raw message, so this is the only place
    /// the `.eml` of `mp export` can come from for one.
    pub async fn fetch_mime(&self, message_id: &str) -> Result<Vec<u8>> {
        let resp = self
            .client
            .get(mime_url(message_id))
            .bearer_auth(self.bearer())
            .send()
            .await
            .context("Failed to fetch the message's MIME content")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "Fetching the message's MIME content failed (HTTP {status}): {body}"
            ));
        }
        let bytes = resp
            .bytes()
            .await
            .context("Failed to read the message's MIME content")?;
        Ok(bytes.to_vec())
    }

    /// Record a reply or a forward on a message the way Outlook does, through
    /// its MAPI history properties (PERSO-171).
    pub async fn set_last_verb(&self, message_id: &str, verb: LastVerb) -> Result<()> {
        let url = format!("{}/me/messages/{}", GRAPH_BASE, message_id);
        let resp = self
            .client
            .patch(&url)
            .bearer_auth(self.bearer())
            .json(&last_verb_patch_body(verb, chrono::Utc::now()))
            .send()
            .await
            .context("Failed to record the reply or forward")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "Recording the reply or forward failed (HTTP {status}): {body}"
            ));
        }
        Ok(())
    }

    /// Move a message to a different folder.
    pub async fn move_message(&self, message_id: &str, destination_folder: &str) -> Result<()> {
        let dest = self.folder_path(destination_folder).await?;
        let url = format!("{}/me/messages/{}/move", GRAPH_BASE, message_id);

        let resp = self
            .client
            .post(&url)
            .bearer_auth(self.bearer())
            .json(&serde_json::json!({
                "destinationId": dest
            }))
            .send()
            .await
            .context("Failed to move message")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("Move message failed (HTTP {}): {}", status, body));
        }

        Ok(())
    }

    /// Delete a message permanently.
    pub async fn delete_message(&self, message_id: &str) -> Result<()> {
        let url = format!("{}/me/messages/{}", GRAPH_BASE, message_id);

        let resp = self
            .client
            .delete(&url)
            .bearer_auth(self.bearer())
            .send()
            .await
            .context("Failed to delete message")?;

        if !resp.status().is_success() && resp.status().as_u16() != 204 {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("Delete message failed (HTTP {}): {}", status, body));
        }

        Ok(())
    }

    /// Update the read status of a message.
    pub async fn update_read_status(&self, message_id: &str, is_read: bool) -> Result<()> {
        let url = format!("{}/me/messages/{}", GRAPH_BASE, message_id);

        let resp = self
            .client
            .patch(&url)
            .bearer_auth(self.bearer())
            .json(&serde_json::json!({
                "isRead": is_read
            }))
            .send()
            .await
            .context("Failed to update read status")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "Update read status failed (HTTP {}): {}",
                status,
                body
            ));
        }

        Ok(())
    }
}

/// Move a message to another folder via Graph (#0018), naming it by its
/// `Message-ID` and touching nothing locally: the store row was already moved
/// optimistically by the caller (`crate::store::write`). Archiving is this with
/// the archive folder as destination.
///
/// A message the server does not have is not an error. Graph's copy may already
/// be gone (moved from another client), and the local row is what the user is
/// looking at.
pub async fn move_message_graph(
    config: &GraphConfig,
    internet_message_id: &str,
    dest_folder: &str,
) -> Result<()> {
    let client = GraphClient::new_async(config).await?;

    match client
        .find_message_by_internet_id(internet_message_id)
        .await?
    {
        Some(graph_id) => {
            client.move_message(&graph_id, dest_folder).await?;
            info!(
                "Graph: moved message {} to {}",
                internet_message_id, dest_folder
            );
        }
        None => warn!(
            "Graph: message {} not found on server, nothing to move",
            internet_message_id
        ),
    }
    Ok(())
}

/// Delete a message via Graph, naming it by its `Message-ID`. Same contract as
/// [`move_message_graph`]: server only, and a missing message is not an error.
pub async fn delete_message_graph(config: &GraphConfig, internet_message_id: &str) -> Result<()> {
    let client = GraphClient::new_async(config).await?;

    match client
        .find_message_by_internet_id(internet_message_id)
        .await?
    {
        Some(graph_id) => {
            client.delete_message(&graph_id).await?;
            info!("Graph: deleted message {} from server", internet_message_id);
        }
        None => warn!(
            "Graph: message {} not found on server, nothing to delete",
            internet_message_id
        ),
    }
    Ok(())
}

/// Record a reply or a forward on every server copy of a message, naming it by
/// its `Message-ID` (PERSO-171): the Graph half of the post-send
/// `set_answered` op.
///
/// Every copy, because the store flags every local copy of a source and the
/// server holds one item per folder the message is filed in. Idempotent: a
/// replay writes the same verb again, with a fresher time. A message the
/// mailbox no longer holds is the typed not-found the drain converges on.
pub async fn mark_last_verb_graph(
    config: &GraphConfig,
    internet_message_id: &str,
    verb: LastVerb,
) -> Result<()> {
    let client = GraphClient::new_async(config).await?;
    let copies = client
        .find_messages_by_internet_id(internet_message_id)
        .await?;
    if copies.is_empty() {
        return Err(crate::ops::NotFoundOnServer {
            message_id: internet_message_id.to_string(),
            mailbox: None,
        }
        .into());
    }
    for graph_id in &copies {
        client.set_last_verb(graph_id, verb).await?;
    }
    info!(
        "Graph: recorded {verb:?} on {} cop(ies) of {internet_message_id}",
        copies.len()
    );
    Ok(())
}

/// Mark read/unread via Graph API.
pub async fn mark_read_graph(
    config: &GraphConfig,
    internet_message_id: &str,
    is_read: bool,
) -> Result<()> {
    let client = GraphClient::new_async(config).await?;

    if let Some(graph_id) = client
        .find_message_by_internet_id(internet_message_id)
        .await?
    {
        client.update_read_status(&graph_id, is_read).await?;
        Ok(())
    } else {
        // Typed not-found so the durable queue's drain converges a replay whose
        // read toggle already landed (#0039 review); a direct caller still sees
        // it as an error through `Display`.
        Err(crate::ops::NotFoundOnServer {
            message_id: internet_message_id.to_string(),
            mailbox: None,
        }
        .into())
    }
}

// ---------------------------------------------------------------------------
// Phase 7: Search via Graph
// ---------------------------------------------------------------------------

/// What a Graph query value keeps literal: the RFC 3986 unreserved set. A
/// space goes out as `%20` rather than the form encoding's `+`, which OData
/// does not promise to read as a space.
const QUERY_VALUE_ENCODE_SET: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// The most messages one Graph page returns: `$top` takes 1 to 1000.
const GRAPH_MAX_TOP: usize = 1000;

/// The `$select` of a search's first pass: the id the second pass downloads
/// by, the Message-ID the dedup and the attachment post-filter read, the date
/// the merge orders by, and the folder a hit of a search across every folder
/// is listed under (PERSO-175).
const STUB_SELECT: &str = "id,internetMessageId,receivedDateTime,parentFolderId";

/// What a search does after one page of hits.
#[derive(Debug, PartialEq)]
enum SearchStep {
    /// Fetch this `@odata.nextLink`.
    Next(String),
    /// Stop: `truncated` when more matched than the search returns, `capped`
    /// when it is Graph's `$search` cap rather than the limit that stopped it.
    Done { truncated: bool, capped: bool },
}

/// The paging decision of a search that has `collected` hits after `pages`
/// pages, wants `limit` and was handed `next_link`.
///
/// It pages until the limit, which one page of at most [`GRAPH_MAX_TOP`]
/// cannot reach past 1000, and stops at [`MAX_ENUMERATION_PAGES`] because a
/// `@odata.nextLink` chain has no other termination proof. A `$search` that
/// runs out of pages at [`GRAPH_SEARCH_CAP`] hits under a larger limit was
/// cut by Graph, not finished, and says so.
fn after_search_page(
    collected: usize,
    limit: usize,
    next_link: Option<String>,
    is_search: bool,
    pages: usize,
) -> SearchStep {
    if collected >= limit {
        return SearchStep::Done {
            truncated: collected > limit || next_link.is_some(),
            capped: false,
        };
    }
    match next_link {
        Some(next) if pages < MAX_ENUMERATION_PAGES => SearchStep::Next(next),
        Some(_) => SearchStep::Done {
            truncated: true,
            capped: false,
        },
        None => {
            let capped = is_search && collected >= GRAPH_SEARCH_CAP;
            SearchStep::Done {
                truncated: capped,
                capped,
            }
        }
    }
}

/// A Graph search's hits, in Graph's order, and whether more matched than
/// they hold (PERSO-175).
struct SearchPages<T> {
    hits: Vec<T>,
    /// More matched than `hits` holds.
    truncated: bool,
    /// Graph's [`GRAPH_SEARCH_CAP`] stopped the search short of its limit,
    /// which no paging gets past: a narrower query is the only way on.
    capped: bool,
}

/// What a search asks Graph for per hit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SearchShape {
    /// The whole message and its threading signals, for the hit
    /// `message.fetch` ingests.
    Whole,
    /// A [`GraphSearchStub`], the first pass of a server search.
    Stub,
}

/// The query parameter a [`crate::search::GraphQuery`] travels as, for the URL
/// and for the error that names a refused search.
fn graph_query_param(query: &crate::search::GraphQuery) -> Option<(&'static str, String)> {
    use crate::search::GraphQuery;
    match query {
        GraphQuery::All => None,
        GraphQuery::Search(kql) => Some(("$search", format!("\"{kql}\""))),
        GraphQuery::Filter(filter) => Some(("$filter", filter.clone())),
    }
}

/// The message-search URL under `base`, with every query value
/// percent-encoded: a raw `+` in a plus-address would otherwise read as a
/// space, and a `&` or `#` in a subject would end the parameter or the query.
///
/// One of `$search` or `$filter` at most, never both (PERSO-173): Graph
/// answers `SearchWithFilter` to the pair and `SearchWithOrderBy` to
/// `$orderby` beside `$search`, and a `$filter` beside
/// `$orderby=receivedDateTime` that does not start with `receivedDateTime` is
/// `InefficientFilter`. Only the term-less listing is sorted server-side; a
/// `$search` comes back newest first on its own, and the Message-ID filter
/// matches a handful of copies at most.
///
/// A [`SearchShape::Whole`] search asks for the threading signals the sync
/// download asks for, `conversationId` and [`MESSAGE_EXPAND`] (PERSO-147), so
/// a hit `message.fetch` ingests threads at once instead of waiting for a
/// re-thread that lists its whole folder for it. Graph accepts the `$expand`
/// beside a `$search` and beside a `$filter`. A [`SearchShape::Stub`] search
/// asks for [`STUB_SELECT`] alone.
fn search_url(
    base: &str,
    limit: usize,
    query: &crate::search::GraphQuery,
    shape: SearchShape,
) -> Result<reqwest::Url> {
    let mut pairs: Vec<(&str, String)> = Vec::new();
    pairs.push(("$top", limit.clamp(1, GRAPH_MAX_TOP).to_string()));
    pairs.push((
        "$select",
        match shape {
            SearchShape::Whole => format!("{MESSAGE_SELECT},conversationId"),
            SearchShape::Stub => STUB_SELECT.to_string(),
        },
    ));
    match graph_query_param(query) {
        Some(param) => pairs.push(param),
        None => pairs.push(("$orderby", "receivedDateTime desc".to_string())),
    }
    let query = pairs
        .iter()
        .map(|(name, value)| {
            format!(
                "{name}={}",
                percent_encoding::utf8_percent_encode(value, QUERY_VALUE_ENCODE_SET)
            )
        })
        .collect::<Vec<_>>()
        .join("&");
    // Already percent-encoded, so it joins the query as it is.
    let query = match shape {
        SearchShape::Whole => format!("{query}&{MESSAGE_EXPAND}"),
        SearchShape::Stub => query,
    };
    let mut url = reqwest::Url::parse(base).with_context(|| format!("parsing {base}"))?;
    url.set_query(Some(&query));
    Ok(url)
}

/// The `$value` URL of one Graph message: its MIME export. The id goes into
/// the path as the move, delete and patch URLs put it.
fn mime_url(graph_id: &str) -> String {
    format!("{GRAPH_BASE}/me/messages/{graph_id}/$value")
}

/// The URL looking a message up by its `Message-ID` across every folder.
///
/// The id goes into an OData string literal, so a single quote in it is
/// doubled, and the whole `$filter` value is percent-encoded: a raw `+` would
/// read as a space, a `&` would end the parameter and a `#` the query, and the
/// lookup would quietly find nothing.
fn message_lookup_url(internet_message_id: &str) -> Result<reqwest::Url> {
    internet_id_url("me/messages", internet_message_id, "id")
}

/// `{collection}?$filter=internetMessageId eq '<id>'&$select={select}`, the
/// one lookup by RFC 5322 `Message-ID` Graph offers.
fn internet_id_url(
    collection: &str,
    internet_message_id: &str,
    select: &str,
) -> Result<reqwest::Url> {
    let clean_id = internet_message_id.trim_matches(|c| c == '<' || c == '>');
    let filter = format!("internetMessageId eq '<{}>'", clean_id.replace('\'', "''"));
    let query = format!(
        "$filter={}&$select={select}",
        percent_encoding::utf8_percent_encode(&filter, QUERY_VALUE_ENCODE_SET)
    );
    let base = format!("{GRAPH_BASE}/{collection}");
    let mut url = reqwest::Url::parse(&base).with_context(|| format!("parsing {base}"))?;
    url.set_query(Some(&query));
    Ok(url)
}

/// `{collection}?$filter=singleValueExtendedProperties/Any(...)&$select=...`:
/// the copies stamped with our `Message-ID` in [`MESSAGE_ID_PROPERTY`].
///
/// Escaped as [`internet_id_url`] escapes, and the value is matched exactly as
/// it was stamped, brackets included.
fn property_lookup_url(
    collection: &str,
    internet_message_id: &str,
    select: &str,
) -> Result<reqwest::Url> {
    let filter = format!(
        "singleValueExtendedProperties/Any(ep: ep/id eq '{MESSAGE_ID_PROPERTY}' and ep/value eq '{}')",
        internet_message_id.replace('\'', "''")
    );
    let query = format!(
        "$filter={}&$select={select}",
        percent_encoding::utf8_percent_encode(&filter, QUERY_VALUE_ENCODE_SET)
    );
    let base = format!("{GRAPH_BASE}/{collection}");
    let mut url = reqwest::Url::parse(&base).with_context(|| format!("parsing {base}"))?;
    url.set_query(Some(&query));
    Ok(url)
}

/// The error a refused Graph search answers with: the request it sent and
/// Graph's own reason, never a quieter search for less (PERSO-173).
fn search_refused(
    query: &crate::search::GraphQuery,
    status: reqwest::StatusCode,
    body: &str,
) -> anyhow::Error {
    let sent = graph_query_param(query)
        .map(|(name, value)| format!("{name}={value}"))
        .unwrap_or_else(|| "the folder listing".to_string());
    anyhow!("Microsoft Graph refused the search {sent} (HTTP {status}): {body}")
}

impl GraphClient {
    /// The first pass of a server search (PERSO-175): the newest `limit` hits
    /// of a KQL `$search` for every match term, or of the `$filter` of an
    /// exact Message-ID lookup, as [`STUB_SELECT`] stubs, paged through
    /// `@odata.nextLink` up to `limit`.
    ///
    /// The caller merges every folder's stubs and cuts them to `limit` before
    /// downloading anything, then downloads the survivors whole with
    /// [`Self::fetch_search_hits`] ([`crate::server_search`]). The stub date
    /// is the one [`graph_message_to_fetched_email`] writes, so the merge
    /// orders the stubs as the downloaded hits sort.
    ///
    /// A refused request is an error naming what was sent. There is no
    /// fallback: the one this replaced dropped the `$search` half and listed
    /// the folder's newest messages as if they were the hits (PERSO-173).
    pub async fn search_stubs(
        &self,
        query: &crate::search::Query,
        folder: Option<&str>,
        limit: usize,
    ) -> Result<MailboxStubs<String>> {
        let pages: SearchPages<GraphSearchStub> = self
            .search_pages(query, folder, limit, SearchShape::Stub)
            .await?;
        let stubs = pages
            .hits
            .into_iter()
            .filter(|stub| {
                is_exact_message_id(
                    stub.internet_message_id.as_deref(),
                    query.message_id.as_deref(),
                )
            })
            .map(|stub| SearchStub {
                key: stub.id,
                date: stub
                    .received_date_time
                    .as_deref()
                    .map(iso_to_rfc2822)
                    .unwrap_or_default(),
                message_id: stub.internet_message_id,
                folder: stub.parent_folder_id,
            })
            .collect();
        Ok(MailboxStubs {
            stubs,
            more: pages.truncated,
            capped: pages.capped,
        })
    }

    /// The label each folder of `ids` (a search hit's `parentFolderId`) is
    /// listed under: the label of the configured mailbox that is that folder,
    /// else the folder's display name (PERSO-175). An id Graph does not
    /// answer for is left out.
    ///
    /// `configured` is the account's folders as `(label, server name or
    /// folder id)`. A hit whose folder id is one of them is labelled with no
    /// request at all ([`known_folder_labels`]): the listing stores every
    /// folder's id (#0141), so on an account that has been listed only a
    /// folder created since needs asking. For the rest, each configured
    /// server name is resolved to the path segment [`Self::folder_path`]
    /// gives it (a well-known name, an id, or the id its display name was
    /// found under) and every segment, configured or hit, is asked for its
    /// canonical id and display name in one `/$batch` per
    /// [`BATCH_MAX_REQUESTS`]: a well-known name and a `parentFolderId` only
    /// compare once both are ids.
    pub async fn folder_labels(
        &self,
        configured: &[(String, String)],
        ids: &[String],
    ) -> Result<HashMap<String, String>> {
        let (mut labelled, unknown) = known_folder_labels(configured, ids);
        if unknown.is_empty() {
            return Ok(labelled);
        }
        let mut labels: Vec<Option<&str>> = Vec::new();
        let mut segments: Vec<String> = Vec::new();
        for (label, server_name) in configured {
            if looks_like_folder_id(server_name) {
                continue;
            }
            match self.folder_path(server_name).await {
                Ok(segment) => {
                    labels.push(Some(label));
                    segments.push(segment);
                }
                Err(e) => debug!("Graph folder '{server_name}' did not resolve: {e:#}"),
            }
        }
        for id in &unknown {
            labels.push(None);
            segments.push(id.clone());
        }
        let asked: Vec<(Option<&str>, &str)> = labels
            .into_iter()
            .zip(segments.iter().map(String::as_str))
            .collect();
        let mut answers: Vec<Option<GraphFolderName>> = Vec::with_capacity(segments.len());
        for chunk in segments.chunks(BATCH_MAX_REQUESTS) {
            let resp = self
                .client
                .post(format!("{GRAPH_BASE}/$batch"))
                .bearer_auth(self.bearer())
                .json(&folder_batch_body(chunk))
                .send()
                .await
                .context("Failed to name the search hits' folders")?;
            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                return Err(anyhow!(
                    "Naming the search hits' folders failed (HTTP {status}): {body}"
                ));
            }
            let batch: GraphBatchResponse = resp
                .json()
                .await
                .context("Failed to parse the folder batch response")?;
            let mut found: Vec<Option<GraphFolderName>> = chunk.iter().map(|_| None).collect();
            for entry in batch.responses {
                let Some(slot) = entry
                    .id
                    .parse::<usize>()
                    .ok()
                    .and_then(|i| found.get_mut(i))
                else {
                    continue;
                };
                if entry.status == 200 {
                    *slot = entry
                        .body
                        .and_then(|body| serde_json::from_value(body).ok());
                }
            }
            answers.extend(found);
        }
        labelled.extend(label_folders(&asked, &answers));
        Ok(labelled)
    }

    /// The second pass of a server search (PERSO-175): the messages behind
    /// the Graph ids [`Self::search_stubs`] kept, downloaded whole in
    /// `/$batch` chunks.
    ///
    /// Not [`Self::fetch_messages_by_ids`], the sync's download, which skips
    /// a throttled message for the next pass to find again: a search has no
    /// next pass, so a throttled hit is asked for again after its
    /// `Retry-After`, up to [`SEARCH_FETCH_ATTEMPTS`] times, and a hit that
    /// still did not come back, throttled, moved since the first pass or
    /// refused, is handed back in `missing` with why ([`fetch_with_retries`]).
    pub async fn fetch_search_hits(&self, ids: &[String]) -> Result<Fetched<String>> {
        let retried =
            fetch_with_retries(ids, SEARCH_FETCH_ATTEMPTS, |chunk| self.batch_round(chunk)).await?;
        let mut emails = Vec::with_capacity(retried.landed.len());
        for (id, msg) in &retried.landed {
            emails.push((id.clone(), self.hydrate(msg).await));
        }
        Ok(Fetched {
            emails,
            missing: retried.missing,
        })
    }

    /// One `/$batch` call for the messages behind `ids`, each sub-response
    /// classified and keyed by its index into `ids`.
    async fn batch_round(
        &self,
        ids: Vec<String>,
    ) -> Result<Vec<(usize, SubResponse<GraphMessage>)>> {
        let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
        let resp = self
            .client
            .post(format!("{GRAPH_BASE}/$batch"))
            .bearer_auth(self.bearer())
            .json(&batch_request_body(&ids))
            .send()
            .await
            .context("Failed to fetch the search hits")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "Batch fetch of the search hits failed (HTTP {status}): {body}"
            ));
        }
        let batch: GraphBatchResponse = resp
            .json()
            .await
            .context("Failed to parse the batch response")?;
        Ok(batch
            .responses
            .into_iter()
            .filter_map(|entry| {
                // Graph is free to reorder the responses, so the request id
                // (the index into `ids`) is the only correlation.
                let index = entry.id.parse::<usize>().ok()?;
                Some((index, entry.into_sub_response()))
            })
            .collect())
    }

    /// The messages matching `query` in `folder`, whole and with each hit's
    /// threading signals beside it (PERSO-147), for the caller that ingests a
    /// hit: `message.fetch`.
    pub async fn search_messages_threaded(
        &self,
        query: &crate::search::Query,
        folder: Option<&str>,
        limit: usize,
    ) -> Result<Vec<GraphFetched>> {
        let pages: SearchPages<GraphMessage> = self
            .search_pages(query, folder, limit, SearchShape::Whole)
            .await?;
        let mut hits = Vec::with_capacity(pages.hits.len());
        for msg in &pages.hits {
            if !is_exact_message_id(
                msg.internet_message_id.as_deref(),
                query.message_id.as_deref(),
            ) {
                continue;
            }
            hits.push(GraphFetched {
                email: self.hydrate(msg).await,
                threading: thread_hints(
                    msg.conversation_id.as_deref(),
                    &msg.single_value_extended_properties,
                ),
            });
        }
        Ok(hits)
    }

    /// The pages one search reads, in `shape`, cut to `limit`.
    ///
    /// One page used to be all a search read, with `$top` clamped to 1000,
    /// so `--limit 2500` quietly answered 1000; it now follows
    /// `@odata.nextLink` until the limit (see [`after_search_page`]).
    async fn search_pages<T: DeserializeOwned>(
        &self,
        query: &crate::search::Query,
        folder: Option<&str>,
        limit: usize,
        shape: SearchShape,
    ) -> Result<SearchPages<T>> {
        let graph_query = crate::search::to_graph(query).map_err(|e| anyhow!("{e}"))?;

        let base = if let Some(f) = folder {
            let fp = self.folder_path(f).await?;
            format!("{}/me/mailFolders/{}/messages", GRAPH_BASE, fp)
        } else {
            format!("{}/me/messages", GRAPH_BASE)
        };

        let is_search = matches!(graph_query, crate::search::GraphQuery::Search(_));
        let mut url = search_url(&base, limit, &graph_query, shape)?;
        let mut hits: Vec<T> = Vec::new();
        let mut pages = 0usize;
        let (truncated, capped) = loop {
            debug!("Graph search URL: {}", url);
            // No `ConsistencyLevel: eventual`: Graph wants it for advanced
            // queries on directory objects, not for `$search` on messages.
            let resp = self
                .client
                .get(url.clone())
                .bearer_auth(self.bearer())
                .send()
                .await
                .context("Failed to search messages")?;

            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                return Err(search_refused(&graph_query, status, &body));
            }

            let page: GraphPage<T> = resp
                .json()
                .await
                .context("Failed to parse search results")?;
            pages += 1;
            hits.extend(page.value);
            match after_search_page(hits.len(), limit, page.next_link, is_search, pages) {
                // The whole link, as Graph hands it: its `$skip` counts items
                // Graph walked, not hits, and is not ours to rebuild.
                SearchStep::Next(next) => {
                    url = reqwest::Url::parse(&next)
                        .with_context(|| format!("parsing the search's next page {next}"))?;
                }
                SearchStep::Done { truncated, capped } => break (truncated, capped),
            }
        };
        hits.truncate(limit);
        Ok(SearchPages {
            hits,
            truncated,
            capped,
        })
    }
}

/// Whether a hit's `Message-ID` is the one the query named, case- and
/// bracket-insensitively: the `$search` and `$filter` matches are fuzzy, and
/// a query that named no Message-ID keeps every hit.
fn is_exact_message_id(candidate: Option<&str>, wanted: Option<&str>) -> bool {
    let Some(wanted) = wanted else {
        return true;
    };
    let wanted = crate::imap_client::normalize_message_id(wanted);
    candidate
        .is_some_and(|m| crate::imap_client::normalize_message_id(m).eq_ignore_ascii_case(wanted))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// A MIME send is allowed up to the threshold inclusive and refused one
    /// byte past it, where the caller stages the send through a draft
    /// (PERSO-144).
    #[test]
    fn the_mime_send_threshold_is_inclusive_and_under_four_megabytes() {
        assert!(fits_mime_send(0));
        assert!(fits_mime_send(MIME_SEND_MAX_BASE64_BYTES));
        assert!(!fits_mime_send(MIME_SEND_MAX_BASE64_BYTES + 1));
        assert!(!fits_mime_send(4 * 1024 * 1024));
        const { assert!(MIME_SEND_MAX_BASE64_BYTES < 4_000_000) };
    }

    /// An upload session sends the file in order, in ranges under 4 MB, and
    /// names each with inclusive bounds and the total (Graph large-attachment
    /// guide).
    #[test]
    fn an_upload_session_covers_the_file_in_ordered_ranges_under_four_megabytes() {
        const { assert!(UPLOAD_CHUNK_BYTES < 4_000_000) };
        assert_eq!(
            upload_ranges(7_000_000, UPLOAD_CHUNK_BYTES),
            [
                (0, 3_145_728),
                (3_145_728, 6_291_456),
                (6_291_456, 7_000_000)
            ]
        );
        assert_eq!(upload_ranges(10, 10), [(0, 10)]);
        assert!(upload_ranges(0, 10).is_empty());
        assert_eq!(
            content_range(0, 3_145_728, 7_000_000),
            "bytes 0-3145727/7000000"
        );
        assert_eq!(
            content_range(6_291_456, 7_000_000, 7_000_000),
            "bytes 6291456-6999999/7000000"
        );
    }

    /// A small attachment goes as one base64 `fileAttachment`, a large one
    /// opens a session naming its size; the POST cap keeps the base64 JSON
    /// under the request limit.
    #[test]
    fn attachment_requests_name_the_file_its_type_and_size() {
        use base64::Engine;
        const { assert!(ATTACHMENT_POST_MAX_BYTES.div_ceil(3) * 4 < MIME_SEND_MAX_BASE64_BYTES) };
        let file = OutgoingAttachment {
            name: "plans.pdf".to_string(),
            content_type: "application/pdf".to_string(),
            bytes: b"%PDF".to_vec(),
        };
        let post = attachment_post_body(&file);
        assert_eq!(post["@odata.type"], "#microsoft.graph.fileAttachment");
        assert_eq!(post["name"], "plans.pdf");
        assert_eq!(post["contentType"], "application/pdf");
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(post["contentBytes"].as_str().unwrap())
                .unwrap(),
            b"%PDF"
        );
        let session = upload_session_body(&file);
        assert_eq!(session["AttachmentItem"]["attachmentType"], "file");
        assert_eq!(session["AttachmentItem"]["name"], "plans.pdf");
        assert_eq!(session["AttachmentItem"]["size"], 4);
    }

    /// The resume's two lookups: every copy with its draft bit, and the
    /// copies in Drafts, both by the bracketed `Message-ID`.
    #[test]
    fn the_resume_looks_a_message_up_everywhere_and_in_drafts() {
        let all = internet_id_url("me/messages", "<a'b@x>", "id,isDraft").unwrap();
        assert_eq!(all.path(), "/v1.0/me/messages");
        let query: std::collections::HashMap<_, _> = all.query_pairs().collect();
        assert_eq!(query["$filter"], "internetMessageId eq '<a''b@x>'");
        assert_eq!(query["$select"], "id,isDraft");
        let drafts = internet_id_url("me/mailFolders/drafts/messages", "a@x", "id").unwrap();
        assert_eq!(drafts.path(), "/v1.0/me/mailFolders/drafts/messages");
        assert_eq!(
            message_action_url("AAMk=", "send"),
            format!("{GRAPH_BASE}/me/messages/AAMk=/send")
        );
    }

    /// The sendMail MIME body is the message in standard, unwrapped base64.
    #[test]
    fn the_mime_send_body_is_unwrapped_standard_base64() {
        use base64::Engine;
        let mime = "From: a@x\r\nSubject: hi\r\n\r\n".repeat(20);
        let body = mime_send_body(mime.as_bytes());
        assert!(!body.contains('\n') && !body.contains('\r'));
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(&body)
                .unwrap(),
            mime.as_bytes()
        );
    }

    fn att(filename: &str, content: &str) -> crate::parse::AttachmentData {
        crate::parse::AttachmentData {
            filename: filename.to_string(),
            content: content.as_bytes().to_vec(),
            content_id: None,
        }
    }

    fn email_with(attachments: Vec<crate::parse::AttachmentData>) -> FetchedEmail {
        FetchedEmail {
            from: "a@b.com".into(),
            to: "me@x.com".into(),
            cc: None,
            reply_to: None,
            bcc: None,
            subject: "s".into(),
            date: "d".into(),
            body_text: String::new(),
            html_body: None,
            has_attachments: true,
            server_has_attachments: None,
            message_id: None,
            attachments,
            flags: Default::default(),
            calendar_ics: None,
            event: None,
        }
    }

    const INVITE_ICS: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:graph-invite-1\r\nSUMMARY:Sync\r\nDTSTART:20260720T120000Z\r\nDTEND:20260720T130000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    const EXPORT_ICS: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:graph-export-1\r\nSUMMARY:Dentist\r\nDTSTART:20260801T090000Z\r\nDTEND:20260801T093000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

    /// The attachment repair settles a message whose list holds nothing the
    /// store can keep, and attaches everything else, the invite lifted into
    /// its sidecar as a first ingest would.
    #[test]
    fn the_repair_settles_only_a_message_with_nothing_storable() {
        let page = serde_json::json!({
            "value": [
                {
                    "@odata.type": "#microsoft.graph.itemAttachment",
                    "id": "A1",
                    "name": "Forwarded message",
                },
                {
                    "@odata.type": "#microsoft.graph.referenceAttachment",
                    "id": "A2",
                    "name": "Plan.docx",
                },
                {
                    "@odata.type": "#microsoft.graph.fileAttachment",
                    "id": "A3",
                    "name": "empty.txt",
                    "contentBytes": "",
                },
            ],
        });
        let list: GraphAttachmentList = serde_json::from_value(page).expect("page parses");
        assert!(matches!(
            repair_verdict(attachments_from_list(list)),
            RepairVerdict::NothingToStore
        ));
        assert!(matches!(
            repair_verdict(Vec::new()),
            RepairVerdict::NothingToStore
        ));

        match repair_verdict(vec![att("invite.ics", INVITE_ICS)]) {
            RepairVerdict::Attach {
                calendar_ics,
                attachments,
            } => {
                assert_eq!(calendar_ics.as_deref(), Some(INVITE_ICS.as_bytes()));
                assert!(attachments.is_empty());
            }
            other => panic!("an invite is storable: {other:?}"),
        }
        match repair_verdict(vec![att("report.pdf", "%PDF")]) {
            RepairVerdict::Attach {
                calendar_ics,
                attachments,
            } => {
                assert!(calendar_ics.is_none());
                assert_eq!(attachments.len(), 1);
                assert_eq!(attachments[0].filename, "report.pdf");
            }
            other => panic!("a file is storable: {other:?}"),
        }
    }

    #[test]
    fn graph_lifts_invite_and_keeps_shared_ics() {
        let mut email = email_with(vec![
            att("invite.ics", INVITE_ICS),
            att("my-calendar.ics", EXPORT_ICS),
        ]);
        populate_calendar_from_attachments(&mut email);
        // Invite lifted to sidecar + event block.
        assert_eq!(email.calendar_ics.as_deref(), Some(INVITE_ICS.as_bytes()));
        assert!(email.event.is_some());
        // The non-invite export survives as a regular attachment, filename kept.
        assert_eq!(email.attachments.len(), 1);
        assert_eq!(email.attachments[0].filename, "my-calendar.ics");
        assert_eq!(email.attachments[0].content, EXPORT_ICS.as_bytes());
    }

    #[test]
    fn graph_non_imip_ics_export_stays_a_plain_attachment() {
        let mut email = email_with(vec![att("schedule.ics", EXPORT_ICS)]);
        populate_calendar_from_attachments(&mut email);
        assert!(
            email.calendar_ics.is_none(),
            "no sidecar for a plain export"
        );
        assert!(email.event.is_none());
        assert_eq!(email.attachments.len(), 1);
        assert_eq!(email.attachments[0].filename, "schedule.ics");
    }

    // -----------------------------------------------------------------------
    // Sync diff: what a folder enumeration says to download and to prune
    // -----------------------------------------------------------------------

    fn entry(graph_id: &str, received: Option<&str>) -> FolderEntry {
        FolderEntry {
            graph_id: graph_id.to_string(),
            is_read: false,
            received: received.map(|s| s.to_string()),
            last_verb: None,
        }
    }

    /// A folder as the server enumerates it: `(internetMessageId, graph id,
    /// receivedDateTime)`.
    fn folder(rows: &[(&str, &str, Option<&str>)]) -> HashMap<String, FolderEntry> {
        rows.iter()
            .map(|(mid, gid, received)| (mid.to_string(), entry(gid, *received)))
            .collect()
    }

    fn known(ids: &[&str]) -> HashSet<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    /// The defect that made a Graph sync never converge: a message that is new
    /// to the store but old on the server. Detection always found it; the
    /// download asked the folder for its most recent messages and never got it
    /// back. Selection now hands the caller that message's own Graph id.
    #[test]
    fn an_old_message_new_to_the_store_is_selected_for_download() {
        let server = folder(&[
            ("<recent@x>", "AAA", Some("2026-08-06T10:00:00Z")),
            ("<moved-in@x>", "BBB", Some("2024-01-01T09:00:00Z")),
        ]);
        let selected = select_for_download(&server, &known(&["<recent@x>"]), usize::MAX, None).0;
        assert_eq!(
            selected
                .iter()
                .map(|e| e.graph_id.as_str())
                .collect::<Vec<_>>(),
            vec!["BBB"],
        );
    }

    /// Newest first, so a capped pass takes the arrivals a user is waiting for;
    /// no `received` sorts last rather than dropping out.
    #[test]
    fn new_ids_come_back_newest_first() {
        let server = folder(&[
            ("<old@x>", "OLD", Some("2026-01-01T00:00:00Z")),
            ("<new@x>", "NEW", Some("2026-08-06T00:00:00Z")),
            ("<undated@x>", "UND", None),
        ]);
        let selected = select_for_download(&server, &HashSet::new(), usize::MAX, None).0;
        assert_eq!(
            selected
                .iter()
                .map(|e| e.graph_id.as_str())
                .collect::<Vec<_>>(),
            vec!["NEW", "OLD", "UND"],
        );
    }

    /// Same timestamp on two messages must still give one order, or a truncated
    /// pass would pick a different pair each time and neither would land.
    #[test]
    fn ties_break_on_the_message_id_so_a_capped_pass_is_reproducible() {
        let server = folder(&[
            ("<b@x>", "B", Some("2026-08-06T00:00:00Z")),
            ("<a@x>", "A", Some("2026-08-06T00:00:00Z")),
        ]);
        let first = select_for_download(&server, &HashSet::new(), usize::MAX, None).0;
        let second = select_for_download(&server, &HashSet::new(), usize::MAX, None).0;
        assert_eq!(
            first
                .iter()
                .map(|e| e.graph_id.as_str())
                .collect::<Vec<_>>(),
            vec!["A", "B"],
        );
        assert_eq!(first, second);
    }

    /// The prune set: what the store holds for this mailbox and the server did
    /// not list, as the UIDs the rows carry.
    #[test]
    fn a_message_gone_from_the_folder_is_a_vanished_uid() {
        let server = folder(&[("<stays@x>", "AAA", None)]);
        let vanished = vanished_graph_uids(&known(&["<stays@x>", "<archived@x>"]), &server);
        assert_eq!(vanished, vec![crate::ingest::graph_uid("<archived@x>")]);
    }

    #[test]
    fn a_folder_the_store_matches_prunes_nothing() {
        let server = folder(&[("<a@x>", "AAA", None), ("<b@x>", "BBB", None)]);
        assert!(vanished_graph_uids(&known(&["<a@x>", "<b@x>"]), &server).is_empty());
    }

    /// #0065 item 2. The enumeration keys on the trimmed `internetMessageId`,
    /// which is what ingest stores. Keyed verbatim, a padded header made the
    /// same message look new (its trimmed row id is not the map key) *and*
    /// vanished (its row id is not in the map): a delete-and-re-download loop
    /// once the prune landed.
    #[test]
    fn a_padded_internet_message_id_neither_loops_nor_prunes() {
        let mut map = HashMap::new();
        absorb_page(
            &mut map,
            vec![GraphMessageIdEntry {
                id: "AAA".into(),
                internet_message_id: Some("  <padded@x>\r\n".into()),
                is_read: false,
                received_date_time: Some("2026-08-06T10:00:00Z".into()),
                single_value_extended_properties: Vec::new(),
            }],
        );
        assert_eq!(map.keys().collect::<Vec<_>>(), vec!["<padded@x>"]);

        // The store holds it under the id `resolve_message_id` produced, which
        // is the same trim.
        let stored = known(&["<padded@x>"]);
        assert!(
            select_for_download(&map, &stored, usize::MAX, None)
                .0
                .is_empty(),
            "a message the store holds must not be downloaded again",
        );
        assert!(
            vanished_graph_uids(&stored, &map).is_empty(),
            "a message the folder still lists must not be pruned",
        );
    }

    /// An entry with no usable id is dropped rather than keyed on an empty
    /// string, which would collide every such message onto one map entry.
    #[test]
    fn an_entry_without_a_message_id_is_dropped() {
        let mut map = HashMap::new();
        absorb_page(
            &mut map,
            vec![
                GraphMessageIdEntry {
                    id: "AAA".into(),
                    internet_message_id: None,
                    is_read: false,
                    received_date_time: None,
                    single_value_extended_properties: Vec::new(),
                },
                GraphMessageIdEntry {
                    id: "BBB".into(),
                    internet_message_id: Some("   ".into()),
                    is_read: false,
                    received_date_time: None,
                    single_value_extended_properties: Vec::new(),
                },
            ],
        );
        assert!(map.is_empty());
    }

    // -----------------------------------------------------------------------
    // #0065 item 4: a capped pass does not prune
    // -----------------------------------------------------------------------

    /// `limit` cuts the download, and the cut is reported: `found` is what the
    /// pass would have taken, the vector is what it took.
    #[test]
    fn a_capped_pass_reports_what_it_left_behind() {
        let server = folder(&[
            ("<a@x>", "A", Some("2026-08-06T03:00:00Z")),
            ("<b@x>", "B", Some("2026-08-06T02:00:00Z")),
            ("<c@x>", "C", Some("2026-08-06T01:00:00Z")),
        ]);
        let (selected, found) = select_for_download(&server, &HashSet::new(), 2, None);
        assert_eq!(found, 3);
        assert_eq!(
            selected
                .iter()
                .map(|e| e.graph_id.as_str())
                .collect::<Vec<_>>(),
            vec!["A", "B"],
        );
        let (all, found) = select_for_download(&server, &HashSet::new(), usize::MAX, None);
        assert_eq!((all.len(), found), (3, 3), "a full sync never truncates");
    }

    /// PERSO-172: a new account's quick passes drain the folder one window
    /// each, every pass but the last says a backlog is owed (what the runtime
    /// ticks again at once on), and none of the short ones mints the delta
    /// token that would let the next pass skip the listing.
    #[test]
    fn a_backlog_drains_one_window_per_pass_and_owes_a_pass_until_the_last() {
        const QUICK: usize = crate::daemon::runtime::account::QUICK_TICK_LIMIT;
        let rows: Vec<(String, String, String)> = (0..250)
            .map(|i| {
                (
                    format!("<m{i:03}@x>"),
                    format!("G{i:03}"),
                    format!("2026-08-06T{:02}:{:02}:00Z", i / 60, i % 60),
                )
            })
            .collect();
        let borrowed: Vec<(&str, &str, Option<&str>)> = rows
            .iter()
            .map(|(mid, id, date)| (mid.as_str(), id.as_str(), Some(date.as_str())))
            .collect();
        let server = folder(&borrowed);

        let mut known = HashSet::new();
        let mut token: Option<String> = None;
        let mut owed = Vec::new();
        loop {
            // What the next pass decides before it lists anything: no token
            // was minted by a short pass, so it enumerates again.
            assert_eq!(
                delta_verdict(QUICK, token.as_deref(), true),
                DeltaVerdict::NoToken
            );
            let (new, found) = select_for_download(&server, &known, QUICK, None);
            if found == 0 {
                break;
            }
            let capped = found > new.len();
            owed.push(backlog_owed(None, found, new.len(), false));
            if may_record_delta_token(true, capped, false) {
                token = Some(format!("{GRAPH_BASE}/me/mailFolders('A')/messages/delta?t"));
            }
            for entry in new {
                known.insert(format!("<m{}@x>", &entry.graph_id[1..]));
            }
            if token.is_some() {
                break;
            }
        }
        assert_eq!(known.len(), 250, "every message landed without a full sync");
        assert_eq!(owed, vec![true, true, false], "100, 100, then the last 50");
        assert!(token.is_some(), "the pass that finished mints the token");
    }

    /// #0141: a windowed (`status-only`) target takes the folder's newest
    /// `window` messages and stops there. Capping the *new* ones instead took
    /// the next older 50 on every pass and owed a follow-up each time, so back
    /// to back passes downloaded the whole trash.
    #[test]
    fn a_windowed_target_takes_the_newest_window_and_owes_no_backlog() {
        const QUICK: usize = crate::daemon::runtime::account::QUICK_TICK_LIMIT;
        const WINDOW: usize = crate::catalogue::STATUS_ONLY_WINDOW;
        let rows: Vec<(String, String, String)> = (0..250)
            .map(|i| {
                (
                    format!("<m{i:03}@x>"),
                    format!("G{i:03}"),
                    format!("2026-08-06T{:02}:{:02}:00Z", i / 60, i % 60),
                )
            })
            .collect();
        let borrowed: Vec<(&str, &str, Option<&str>)> = rows
            .iter()
            .map(|(mid, id, date)| (mid.as_str(), id.as_str(), Some(date.as_str())))
            .collect();
        let server = folder(&borrowed);

        let mut known = HashSet::new();
        for pass in 0..4 {
            let (new, found) = select_for_download(&server, &known, QUICK, Some(WINDOW));
            assert!(
                !backlog_owed(Some(WINDOW), found, new.len(), false),
                "pass {pass} owes no follow-up"
            );
            for entry in new {
                known.insert(format!("<m{}@x>", &entry.graph_id[1..]));
            }
        }
        let expected: HashSet<String> = (250 - WINDOW..250)
            .map(|i| format!("<m{i:03}@x>"))
            .collect();
        assert_eq!(known, expected, "the newest {WINDOW} and nothing older");

        // A message moved into the trash lands at the top of the window.
        let (new, found) = select_for_download(&server, &HashSet::new(), 10, Some(WINDOW));
        assert_eq!(found, 250, "the history below the window is still reported");
        assert_eq!(new.len(), 10, "the pass's own limit still applies");
        assert_eq!(new[0].graph_id, "G249");
    }

    /// Only the cap owes an immediate pass: a short batch (a throttle, a
    /// failed sub-response) and a pass that asked for nothing wait for the
    /// regular interval, and a pass that took everything owes nothing.
    #[test]
    fn only_a_window_that_landed_whole_owes_an_immediate_pass() {
        assert!(backlog_owed(None, 250, 100, false));
        assert!(
            !backlog_owed(None, 250, 100, true),
            "a throttled batch backs off"
        );
        assert!(
            !backlog_owed(None, 250, 0, false),
            "`-n 0` makes no progress"
        );
        assert!(!backlog_owed(None, 100, 100, false), "nothing left behind");
        assert!(!backlog_owed(None, 0, 0, false));
    }

    fn sync_target(role: MailboxRole, server_name: &str) -> SyncTarget {
        SyncTarget::new(role, server_name)
    }

    /// A message as the Graph download hands it over, dated by its
    /// `receivedDateTime`.
    fn received(mid: &str, iso: &str) -> GraphFetched {
        let mut email = email_with(Vec::new());
        email.has_attachments = false;
        email.message_id = Some(mid.into());
        email.subject = mid.into();
        email.body_text = format!("body of {mid}");
        email.date = iso_to_rfc2822(iso);
        GraphFetched {
            email,
            threading: thread_hints(None, &[]),
        }
    }

    /// One message as the batch download answers it with the PERSO-147
    /// `$select` and `$expand`: the two MAPI properties come back as
    /// single-value extended properties, absent when the message has neither.
    fn graph_message(json: serde_json::Value) -> GraphMessage {
        serde_json::from_value(json).expect("a Graph message")
    }

    #[test]
    fn the_threading_properties_become_the_ingest_hints() {
        let reply = graph_message(serde_json::json!({
            "id": "AAMk1",
            "internetMessageId": "<reply@x>",
            "conversationId": "AAQkConv=",
            "singleValueExtendedProperties": [
                {"id": "String 0x1042", "value": "<parent@x>"},
                {"id": "String 0x1039", "value": "<root@x> <parent@x>"}
            ]
        }));
        assert_eq!(
            thread_hints(
                reply.conversation_id.as_deref(),
                &reply.single_value_extended_properties
            ),
            crate::ingest::ThreadHints {
                in_reply_to: Some("<parent@x>".into()),
                references: Some("<root@x> <parent@x>".into()),
                conversation_id: Some("AAQkConv=".into()),
            }
        );

        // A root message: no properties at all, and the conversation alone.
        let root = graph_message(serde_json::json!({
            "id": "AAMk2",
            "internetMessageId": "<root@x>",
            "conversationId": "AAQkConv="
        }));
        assert_eq!(
            thread_hints(
                root.conversation_id.as_deref(),
                &root.single_value_extended_properties
            ),
            crate::ingest::ThreadHints {
                in_reply_to: None,
                references: None,
                conversation_id: Some("AAQkConv=".into()),
            }
        );

        // No conversation named: written as asked, the empty string.
        assert_eq!(thread_hints(None, &[]).conversation_id.as_deref(), Some(""));
    }

    #[test]
    fn the_threading_listing_keys_by_the_trimmed_message_id() {
        let page: GraphThreadingList = serde_json::from_value(serde_json::json!({
            "value": [
                {"internetMessageId": " <a@x> ", "conversationId": "C1"},
                {"conversationId": "C2"},
                {"internetMessageId": "<b@x>", "conversationId": "C1",
                 "singleValueExtendedProperties": [{"id": "String 0x1042", "value": "<a@x>"}]}
            ]
        }))
        .unwrap();
        assert!(page.next_link.is_none());
        let mut hints = HashMap::new();
        absorb_threading_page(&mut hints, page.value);
        assert_eq!(hints.len(), 2, "an entry with no Message-ID is dropped");
        assert_eq!(hints["<a@x>"].conversation_id.as_deref(), Some("C1"));
        assert_eq!(hints["<b@x>"].in_reply_to.as_deref(), Some("<a@x>"));
        assert_eq!(hints["<b@x>"].references, None);
    }

    #[test]
    fn the_download_and_the_listing_ask_for_the_threading_signals() {
        let body = batch_request_body(&["AAA"]);
        let url = body["requests"][0]["url"].as_str().unwrap();
        assert!(url.contains(",conversationId&"), "got {url}");
        assert!(url.contains(MESSAGE_EXPAND), "got {url}");
        let listing = threading_listing_url("inbox");
        assert!(
            listing.contains("$select=internetMessageId,conversationId&"),
            "got {listing}"
        );
        assert!(listing.contains(MESSAGE_EXPAND), "got {listing}");
        assert!(
            !MESSAGE_EXPAND.contains(' '),
            "a batch sub-request URL carries no raw space"
        );
    }

    /// PERSO-171: the enumeration that restates `isRead` asks for the last
    /// verb beside it, and only for that; the delta, which refuses an
    /// `$expand`, does not ask.
    #[test]
    fn the_enumeration_asks_for_the_last_verb_and_the_delta_does_not() {
        for ordered in [true, false] {
            let url = enumeration_url("inbox", ordered);
            assert!(url.contains(LAST_VERB_EXPAND), "got {url}");
            assert!(!url.contains("0x1042"), "got {url}");
        }
        assert!(!LAST_VERB_EXPAND.contains(' '));
        assert!(!delta_url("inbox", false).contains("$expand"));
        assert!(MESSAGE_EXPAND.contains("'Integer%200x1081'"));
    }

    /// The property's values map onto the history bits: a reply and a reply to
    /// all are answered, a forward is forwarded, any other verb is nothing.
    #[test]
    fn the_last_verb_property_maps_onto_the_history_bits() {
        let props = |value: &str| {
            vec![GraphExtendedProperty {
                id: "Integer 0x1081".into(),
                value: Some(value.into()),
            }]
        };
        assert_eq!(
            LastVerb::from_properties(&props("102")),
            Some(LastVerb::Reply)
        );
        assert_eq!(
            LastVerb::from_properties(&props("103")),
            Some(LastVerb::ReplyAll)
        );
        assert_eq!(
            LastVerb::from_properties(&props("104")),
            Some(LastVerb::Forward)
        );
        assert_eq!(LastVerb::from_properties(&props("105")), None);
        assert_eq!(LastVerb::from_properties(&props("junk")), None);
        assert_eq!(LastVerb::from_properties(&[]), None);
        // The threading properties beside it are not mistaken for it.
        let threading = vec![GraphExtendedProperty {
            id: "String 0x1042".into(),
            value: Some("102".into()),
        }];
        assert_eq!(LastVerb::from_properties(&threading), None);

        let none = crate::types::MessageFlags::default();
        let replied = LastVerb::ReplyAll.mark(none);
        assert!(replied.answered && !replied.forwarded);
        let forwarded = LastVerb::Forward.mark(none);
        assert!(forwarded.forwarded && !forwarded.answered);

        // A downloaded message carries the verb into its flags.
        let msg = graph_message(serde_json::json!({
            "id": "AAMk1",
            "internetMessageId": "<a@x>",
            "isRead": true,
            "singleValueExtendedProperties": [{"id": "Integer 0x1081", "value": "102"}]
        }));
        let flags = graph_message_to_fetched_email(&msg).flags;
        assert!(flags.seen && flags.answered && !flags.forwarded);

        // An enumerated entry too, and a delta entry never.
        let mut map = HashMap::new();
        absorb_page(
            &mut map,
            vec![GraphMessageIdEntry {
                id: "AAA".into(),
                internet_message_id: Some("<a@x>".into()),
                is_read: false,
                received_date_time: None,
                single_value_extended_properties: props("104"),
            }],
        );
        assert_eq!(map["<a@x>"].last_verb, Some(LastVerb::Forward));
        let flags = server_flags(&map["<a@x>"]);
        assert!(!flags.seen && flags.forwarded && !flags.answered);
    }

    /// The PATCH body records the verb, its time and the icon, as the three
    /// properties Exchange accepted on the live test (PERSO-171).
    #[test]
    fn the_last_verb_patch_names_the_verb_its_time_and_the_icon() {
        let at = chrono::DateTime::parse_from_rfc3339("2026-10-22T10:24:45Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let body = |verb| last_verb_patch_body(verb, at);
        assert_eq!(
            body(LastVerb::Reply),
            serde_json::json!({"singleValueExtendedProperties": [
                {"id": "Integer 0x1081", "value": "102"},
                {"id": "SystemTime 0x1082", "value": "2026-10-22T10:24:45Z"},
                {"id": "Integer 0x1080", "value": "261"},
            ]})
        );
        let all = body(LastVerb::ReplyAll);
        assert_eq!(all["singleValueExtendedProperties"][0]["value"], "103");
        assert_eq!(all["singleValueExtendedProperties"][2]["value"], "261");
        let forward = body(LastVerb::Forward);
        assert_eq!(forward["singleValueExtendedProperties"][0]["value"], "104");
        assert_eq!(forward["singleValueExtendedProperties"][2]["value"], "262");

        assert_eq!(LastVerb::of_send(true, false), LastVerb::Reply);
        assert_eq!(LastVerb::of_send(true, true), LastVerb::ReplyAll);
        assert_eq!(LastVerb::of_send(false, true), LastVerb::Forward);
    }

    /// The store half (PERSO-171): a sync row whose listing carries 0x1081=102
    /// lands answered, a held row a later enumeration names as replied turns
    /// answered, and an entry with no verb (the delta's) clears nothing.
    #[test]
    fn a_row_the_server_names_replied_lands_answered() {
        let tmp = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(tmp.path().join("store.sqlite3")).unwrap();
        let blobs = crate::store::BlobStore::new(tmp.path().join("blobs"));
        let inbox = sync_target(MailboxRole::Inbox, "Inbox");
        let answered = |mid: &str| -> bool {
            let flags: Option<String> = store
                .conn()
                .query_row(
                    "SELECT flags FROM messages WHERE message_id = ?1",
                    [mid],
                    |r| r.get(0),
                )
                .unwrap();
            crate::types::MessageFlags::parse(flags.as_deref().unwrap_or_default()).answered
        };
        let mut result = SyncResult::default();

        // Downloaded with the property: answered at ingest.
        let msg = graph_message(serde_json::json!({
            "id": "AAMk1",
            "internetMessageId": "<replied@x>",
            "receivedDateTime": "2026-10-21T20:12:00Z",
            "isRead": true,
            "singleValueExtendedProperties": [{"id": "Integer 0x1081", "value": "102"}]
        }));
        let mut fetched = received("<replied@x>", "2026-10-21T20:12:00Z");
        fetched.email.flags = graph_message_to_fetched_email(&msg).flags;
        let plain = received("<later@x>", "2026-10-21T20:13:00Z");
        ingest_window(
            &store,
            &blobs,
            "acct",
            &inbox,
            &[fetched, plain],
            None,
            &mut result,
        );
        assert!(answered("<replied@x>"));
        assert!(!answered("<later@x>"));

        // Replied in Outlook later: the next enumeration says so.
        let listed = |verb: Option<LastVerb>| FolderEntry {
            graph_id: "AAA".into(),
            is_read: true,
            received: None,
            last_verb: verb,
        };
        let updated = crate::ingest::apply_graph_flags(
            &store,
            "acct",
            "inbox",
            [(
                crate::ingest::graph_uid("<later@x>"),
                server_flags(&listed(Some(LastVerb::Reply))),
            )],
        );
        assert_eq!(updated, 1);
        assert!(answered("<later@x>"));

        // A delta entry, or a forward after the reply, clears nothing.
        crate::ingest::apply_graph_flags(
            &store,
            "acct",
            "inbox",
            [
                (
                    crate::ingest::graph_uid("<later@x>"),
                    server_flags(&listed(None)),
                ),
                (
                    crate::ingest::graph_uid("<replied@x>"),
                    server_flags(&listed(Some(LastVerb::Forward))),
                ),
            ],
        );
        assert!(answered("<later@x>"));
        assert!(answered("<replied@x>"));
    }

    /// A search, and the Message-ID lookup `message.fetch` downloads a hit
    /// through, ask for the threading signals the sync download asks for
    /// (PERSO-147), and the nested `$filter` stays inside the `$expand`.
    #[test]
    fn a_search_asks_for_the_threading_signals() {
        for query in ["from:amazon", "message-id:<a+b@x>", "after:2026-10-01"] {
            let pairs = url_pairs(query, 5);
            assert_eq!(
                pairs["$select"],
                format!("{MESSAGE_SELECT},conversationId"),
                "{query}"
            );
            assert_eq!(
                pairs["$expand"],
                "singleValueExtendedProperties($filter=id eq 'String 0x1042' or id eq 'String 0x1039' or id eq 'Integer 0x1081')",
                "{query}"
            );
        }
        let search = url_pairs("from:amazon", 5);
        assert!(!search.contains_key("$filter"), "{search:?}");
        let lookup = url_pairs("message-id:<a+b@x>", 1);
        assert_eq!(lookup["$filter"], "internetMessageId eq '<a+b@x>'");
    }

    /// PERSO-147: a Graph reply and its parent, downloaded in two windows in
    /// either order, land in one conversation; the headers thread the reply
    /// to a stored parent, the conversation threads a parent stored after it.
    #[test]
    fn graph_windows_thread_by_headers_and_by_conversation() {
        let tmp = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(tmp.path().join("store.sqlite3")).unwrap();
        let blobs = crate::store::BlobStore::new(tmp.path().join("blobs"));
        let inbox = sync_target(MailboxRole::Inbox, "Inbox");
        let with = |mid: &str, iso: &str, irt: Option<&str>, conversation: &str| {
            let mut fetched = received(mid, iso);
            fetched.threading = crate::ingest::ThreadHints {
                in_reply_to: irt.map(str::to_string),
                references: irt.map(str::to_string),
                conversation_id: Some(conversation.to_string()),
            };
            fetched
        };
        let thread_of = |mid: &str| -> String {
            store
                .conn()
                .query_row(
                    "SELECT thread_id FROM messages WHERE message_id = ?1",
                    [mid],
                    |r| r.get(0),
                )
                .unwrap()
        };
        let mut result = SyncResult::default();

        // Parent first, reply after: the headers.
        ingest_window(
            &store,
            &blobs,
            "acct",
            &inbox,
            &[with("<p1@x>", "2026-10-21T20:12:00Z", None, "C1")],
            None,
            &mut result,
        );
        ingest_window(
            &store,
            &blobs,
            "acct",
            &inbox,
            &[with(
                "<r1@x>",
                "2026-10-21T20:13:00Z",
                Some("<p1@x>"),
                "other",
            )],
            None,
            &mut result,
        );
        assert_eq!(thread_of("<r1@x>"), "<p1@x>", "the headers win");

        // Reply first, parent after: the conversation.
        ingest_window(
            &store,
            &blobs,
            "acct",
            &inbox,
            &[with("<r2@x>", "2026-10-21T20:13:00Z", Some("<p2@x>"), "C2")],
            None,
            &mut result,
        );
        assert_eq!(thread_of("<r2@x>"), "<r2@x>", "no parent stored yet");
        ingest_window(
            &store,
            &blobs,
            "acct",
            &inbox,
            &[with("<p2@x>", "2026-10-21T20:12:00Z", None, "C2")],
            None,
            &mut result,
        );
        assert_eq!(thread_of("<p2@x>"), "<r2@x>");

        // The stored signals, and an unrelated message that shares nothing.
        let (irt, conversation): (Option<String>, Option<String>) = store
            .conn()
            .query_row(
                "SELECT in_reply_to, conversation_id FROM messages WHERE message_id = '<r1@x>'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(irt.as_deref(), Some("<p1@x>"));
        assert_eq!(conversation.as_deref(), Some("other"));
        ingest_window(
            &store,
            &blobs,
            "acct",
            &inbox,
            &[with("<lone@x>", "2026-10-21T20:14:00Z", None, "")],
            None,
            &mut result,
        );
        assert_eq!(
            thread_of("<lone@x>"),
            "<lone@x>",
            "an empty conversation matches nothing"
        );
    }

    /// PERSO-172 review: a message given up on after three attempts never
    /// enters the store, so every pass lists it as new and downloads it again.
    /// A window made only of those must not owe the immediate pass that would
    /// re-download the same bodies every few seconds for ever.
    #[test]
    fn a_window_of_given_up_messages_owes_no_follow_up() {
        let tmp = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(tmp.path().join("store.sqlite3")).unwrap();
        // A blob root that is a file: every body write fails, deterministically.
        let not_a_dir = tmp.path().join("blobs");
        std::fs::write(&not_a_dir, b"").unwrap();
        let blobs = crate::store::BlobStore::new(&not_a_dir);
        let inbox = sync_target(MailboxRole::Inbox, "Inbox");
        let emails: Vec<GraphFetched> = (0..3)
            .map(|i| received(&format!("<poison{i}@x>"), "2026-08-06T10:00:00Z"))
            .collect();

        for pass in 1..=4 {
            let mut result = SyncResult::default();
            let window = ingest_window(&store, &blobs, "acct", &inbox, &emails, None, &mut result);
            assert_eq!(window.written, 0, "pass {pass} stored nothing");
            assert_eq!(
                window.owed_failure,
                pass < 3,
                "pass {pass}: owed until the third failure gives up"
            );
            assert!(
                !owes_follow_up(true, &window),
                "pass {pass} made no progress, so the backlog waits for the interval"
            );
            assert_eq!(result.saved, 0);
        }
    }

    /// Progress is what a follow-up buys: a given-up message among written ones
    /// still moves the drain on, an owed retry still waits for the interval.
    #[test]
    fn a_follow_up_is_owed_only_by_a_window_that_moved_the_drain_on() {
        let progressed = WindowIngest {
            written: 99,
            owed_failure: false,
        };
        assert!(owes_follow_up(true, &progressed));
        assert!(!owes_follow_up(false, &progressed), "no backlog, no hurry");
        let retrying = WindowIngest {
            written: 99,
            owed_failure: true,
        };
        assert!(!owes_follow_up(true, &retrying));
        assert!(!owes_follow_up(true, &WindowIngest::default()));
    }

    /// PERSO-172 review: a drain inserts a hundred old messages per pass, and
    /// every inserted inbox row used to be announced, so the desktop said "100
    /// new emails" every few seconds. Only a message newer than everything the
    /// account held before the pass is announced; a first sync announces
    /// nothing.
    #[test]
    fn only_mail_newer_than_the_store_is_announced() {
        let tmp = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(tmp.path().join("store.sqlite3")).unwrap();
        let blobs = crate::store::BlobStore::new(tmp.path().join("blobs"));
        let inbox = sync_target(MailboxRole::Inbox, "Inbox");
        let announced = |result: &SyncResult| -> Vec<String> {
            result
                .new_inbox_mail
                .iter()
                .map(|m| m.subject.clone())
                .collect()
        };

        // The first sync of an empty store: the newest window, all of it old
        // to its owner.
        assert_eq!(crate::ingest::newest_held_date(&store, "acct"), None);
        let first = vec![
            received("<m3@x>", "2026-08-06T12:00:00Z"),
            received("<m2@x>", "2026-08-06T11:00:00Z"),
        ];
        let mut result = SyncResult::default();
        ingest_window(&store, &blobs, "acct", &inbox, &first, None, &mut result);
        assert_eq!(result.saved, 2);
        assert!(
            announced(&result).is_empty(),
            "a first sync announces nothing"
        );

        // A drain pass: the next older window, plus one message that arrived
        // meanwhile and so leads the window.
        let newest_held = crate::ingest::newest_held_date(&store, "acct");
        assert!(newest_held.is_some());
        let drain = vec![
            received("<fresh@x>", "2026-08-06T12:00:01Z"),
            received("<m1@x>", "2026-08-06T10:00:00Z"),
            received("<m0@x>", "2026-08-06T09:00:00Z"),
        ];
        let mut result = SyncResult::default();
        ingest_window(
            &store,
            &blobs,
            "acct",
            &inbox,
            &drain,
            newest_held,
            &mut result,
        );
        assert_eq!(result.saved, 3, "the old ones are still stored");
        assert_eq!(announced(&result), vec!["<fresh@x>".to_string()]);

        // Only the inbox announces, whatever the date.
        let archive = sync_target(MailboxRole::Archive, "Archive");
        let newest_held = crate::ingest::newest_held_date(&store, "acct");
        let mut result = SyncResult::default();
        let later = vec![received("<filed@x>", "2026-08-07T08:00:00Z")];
        ingest_window(
            &store,
            &blobs,
            "acct",
            &archive,
            &later,
            newest_held,
            &mut result,
        );
        assert_eq!(result.saved, 1);
        assert!(announced(&result).is_empty());
    }

    /// The floor is the account's, not the inbox's: an inbox kept at zero
    /// holds no rows, and a per-folder floor would silence every arrival.
    #[test]
    fn an_empty_inbox_still_announces_what_arrives_in_it() {
        let tmp = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(tmp.path().join("store.sqlite3")).unwrap();
        let blobs = crate::store::BlobStore::new(tmp.path().join("blobs"));
        let mut result = SyncResult::default();
        let archived = vec![received("<old@x>", "2026-08-06T10:00:00Z")];
        let archive = sync_target(MailboxRole::Archive, "Archive");
        ingest_window(
            &store,
            &blobs,
            "acct",
            &archive,
            &archived,
            None,
            &mut result,
        );

        let newest_held = crate::ingest::newest_held_date(&store, "acct");
        let inbox = sync_target(MailboxRole::Inbox, "Inbox");
        let arrival = vec![received("<new@x>", "2026-08-06T10:05:00Z")];
        let mut result = SyncResult::default();
        ingest_window(
            &store,
            &blobs,
            "acct",
            &inbox,
            &arrival,
            newest_held,
            &mut result,
        );
        assert_eq!(result.new_inbox_mail.len(), 1);
        assert_eq!(
            crate::ingest::newest_held_date(&store, "other"),
            None,
            "another account's rows are not this one's floor"
        );
    }

    /// The gate itself. A quick sync that could not download its whole backlog
    /// must not delete anything, and it must not delete anything *anywhere*:
    /// the inbox row of a message archived elsewhere is only safe to drop
    /// because the archive pass ingested its copy, and the archive pass is the
    /// one that was capped.
    #[test]
    fn one_capped_target_suspends_the_prune_for_every_target() {
        // (enumeration complete, download truncated) per target.
        assert!(pass_may_prune(&[
            (true, false),
            (true, false),
            (true, false)
        ]));
        assert!(!pass_may_prune(&[
            (true, false),
            (true, true),
            (true, false)
        ]));
        assert!(!pass_may_prune(&[(false, false), (true, false)]));
        assert!(
            pass_may_prune(&[]),
            "a pass with no targets prunes nothing anyway"
        );
    }

    // -----------------------------------------------------------------------
    // #0065 items 5 and 6: the /$batch sub-requests
    // -----------------------------------------------------------------------

    /// Graph parses the sub-request URL out of the JSON itself, so nothing else
    /// on this path escapes the id.
    #[test]
    fn batch_sub_request_ids_are_percent_encoded() {
        let body = batch_request_body(&["AA/BB+CC=", "plain-_id="]);
        let url = body["requests"][0]["url"].as_str().unwrap();
        assert!(url.starts_with("/me/messages/AA%2FBB%2BCC=?"), "got {url}");
        // The base64url alphabet and `=` stay legible: the common case is
        // unchanged.
        let url = body["requests"][1]["url"].as_str().unwrap();
        assert!(url.starts_with("/me/messages/plain-_id=?"), "got {url}");
    }

    fn batch_entry(status: u16, headers: &[(&str, &str)]) -> GraphBatchEntry {
        GraphBatchEntry {
            id: "0".into(),
            status,
            headers: headers
                .iter()
                .map(|(k, v)| (k.to_string(), serde_json::Value::String(v.to_string())))
                .collect(),
            body: None,
        }
    }

    /// A 429 *sub*-response inside a 200 batch is the throttle a big first sync
    /// actually meets; its `Retry-After` paces the remaining chunks.
    #[test]
    fn a_throttled_sub_response_asks_for_a_pause() {
        assert_eq!(
            batch_entry(429, &[("Retry-After", "7")]).retry_after_secs(),
            Some(7),
        );
        assert_eq!(
            batch_entry(429, &[("retry-after", " 7 ")]).retry_after_secs(),
            Some(7),
            "Graph promises no casing",
        );
        assert_eq!(
            batch_entry(429, &[("Retry-After", "999999")]).retry_after_secs(),
            Some(MAX_RETRY_AFTER_SECS),
            "a hostile header cannot park the sync",
        );
        assert_eq!(batch_entry(429, &[]).retry_after_secs(), None);
        assert_eq!(
            batch_entry(404, &[("Retry-After", "7")]).retry_after_secs(),
            None,
            "only a throttle is a reason to wait",
        );
    }

    // -----------------------------------------------------------------------
    // #0065 follow-up: the batch's own failure modes
    // -----------------------------------------------------------------------

    /// A header value that is not a JSON string must not fail the parse of the
    /// whole chunk. It used to: `HashMap<String, String>` made one numeric or
    /// array-valued header on one sub-response into zero downloads for that
    /// folder, on every pass, with the prune suspended throughout.
    #[test]
    fn a_non_string_sub_response_header_does_not_fail_the_batch() {
        let raw = serde_json::json!({
            "responses": [
                {
                    "id": "0",
                    "status": 429,
                    "headers": {
                        "Retry-After": 9,
                        "Content-Length": 512,
                        "X-Weird": ["a", "b"],
                    },
                },
                { "id": "1", "status": 200, "headers": { "Content-Type": "application/json" } },
            ]
        });
        let parsed: GraphBatchResponse = serde_json::from_value(raw).expect(
            "a header shape this code does not read cannot \
                                                be allowed to fail the whole chunk",
        );
        assert_eq!(parsed.responses.len(), 2);
        assert_eq!(
            parsed.responses[0].retry_after_secs(),
            Some(9),
            "a numeric Retry-After is still a Retry-After",
        );
        assert_eq!(parsed.responses[1].retry_after_secs(), None);
    }

    /// Throttling is back-pressure, not failure: it paces the pass but does not
    /// spend the budget that exists to stop a pass that cannot succeed. A 503
    /// counts as a throttle only when it carries the header, because a bare one
    /// is indistinguishable from the service being down.
    #[test]
    fn a_throttle_is_not_a_failure_but_a_bare_503_is() {
        assert!(batch_entry(429, &[("Retry-After", "7")]).is_throttled());
        assert!(
            batch_entry(429, &[]).is_throttled(),
            "429 throttles with or without the header"
        );
        assert!(batch_entry(503, &[("Retry-After", "7")]).is_throttled());
        assert_eq!(
            batch_entry(503, &[("Retry-After", "7")]).retry_after_secs(),
            Some(7),
            "Graph's throttling guidance names 503 alongside 429",
        );
        assert!(!batch_entry(503, &[]).is_throttled());
        assert!(!batch_entry(404, &[("Retry-After", "7")]).is_throttled());
        assert!(!batch_entry(200, &[]).is_throttled());
    }

    /// A sub-response is a throttle with its pause, or a failure naming
    /// Graph's own code and message, never a silent skip.
    #[test]
    fn a_sub_response_is_classified_with_its_reason() {
        assert!(matches!(
            batch_entry(429, &[("Retry-After", "7")]).into_sub_response(),
            SubResponse::Throttled(Some(7))
        ));
        let mut moved = batch_entry(404, &[]);
        moved.body = Some(serde_json::json!({
            "error": {"code": "ErrorItemNotFound", "message": "The specified object was not found."}
        }));
        match moved.into_sub_response() {
            SubResponse::Failed(why) => assert_eq!(
                why,
                "HTTP 404: ErrorItemNotFound: The specified object was not found."
            ),
            other => panic!("a 404 is a failure, got {other:?}"),
        }
        assert!(matches!(
            batch_entry(200, &[]).into_sub_response(),
            SubResponse::Failed(why) if why == "an empty answer"
        ));
    }

    /// A scripted `/$batch` round for [`fetch_with_retries`]: each id answers
    /// with what `answer` says on its `n`-th request, and every chunk asked
    /// for is recorded.
    async fn run_retries(
        ids: &[&str],
        answer: impl Fn(&str, usize) -> Option<SubResponse<String>>,
    ) -> (Retried<String>, Vec<Vec<String>>) {
        let asked = std::cell::RefCell::new(Vec::<Vec<String>>::new());
        let ids: Vec<String> = ids.iter().map(|id| id.to_string()).collect();
        let retried = fetch_with_retries(&ids, SEARCH_FETCH_ATTEMPTS, |chunk: Vec<String>| {
            let mut asked = asked.borrow_mut();
            let answers: Vec<(usize, SubResponse<String>)> = chunk
                .iter()
                .enumerate()
                .filter_map(|(index, id)| {
                    let nth = asked.iter().filter(|c| c.contains(id)).count() + 1;
                    answer(id, nth).map(|a| (index, a))
                })
                .collect();
            asked.push(chunk);
            async move { Ok(answers) }
        })
        .await
        .expect("no round fails outright");
        (retried, asked.into_inner())
    }

    /// A hit Graph throttled is asked for again after the pause and lands,
    /// where the sync's download would have skipped it for a next pass a
    /// search does not have (PERSO-175).
    #[tokio::test]
    async fn a_throttled_search_hit_is_asked_again_and_lands() {
        let (retried, asked) = run_retries(&["a", "b"], |id, nth| {
            Some(match (id, nth) {
                ("b", 1) => SubResponse::Throttled(Some(0)),
                _ => SubResponse::Message(format!("msg {id}")),
            })
        })
        .await;
        assert_eq!(asked, [vec!["a", "b"], vec!["b"]]);
        let landed: Vec<&str> = retried.landed.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(landed, ["a", "b"]);
        assert!(retried.missing.is_empty());
    }

    /// Every hit ends landed or missing with why: one throttled on every
    /// attempt, one Graph refused, and one the batch never answered.
    #[tokio::test]
    async fn a_hit_that_never_comes_back_is_missing_with_why() {
        let (retried, asked) = run_retries(&["busy", "moved", "silent", "ok"], |id, _| match id {
            "busy" => Some(SubResponse::Throttled(Some(0))),
            "moved" => Some(SubResponse::Failed("HTTP 404".into())),
            "silent" => None,
            _ => Some(SubResponse::Message(id.to_string())),
        })
        .await;
        assert_eq!(
            asked
                .iter()
                .filter(|chunk| chunk.contains(&"busy".to_string()))
                .count(),
            SEARCH_FETCH_ATTEMPTS as usize,
            "a throttled hit is asked for a bounded number of times"
        );
        assert_eq!(retried.landed.len(), 1);
        assert_eq!(
            retried.missing,
            [
                ("moved".to_string(), "HTTP 404".to_string()),
                (
                    "silent".to_string(),
                    "not answered by the batch".to_string()
                ),
                (
                    "busy".to_string(),
                    "throttled by Microsoft Graph 3 times".to_string()
                ),
            ]
        );
    }

    /// The gap #0065 shipped with: `download_incomplete` was derived from
    /// `limit` alone, so a target whose batch was throttled out returned no
    /// messages and still reported a complete download. The prune gate then
    /// opened on inbox rows whose archive copies had never landed.
    #[test]
    fn a_short_batch_return_marks_the_pass_incomplete() {
        // (ids asked for, messages returned, the budget ran out).
        assert!(!batch_fell_short(20, 20, false));
        assert!(
            batch_fell_short(20, 19, false),
            "one failed sub-response is one message the store does not hold",
        );
        assert!(
            batch_fell_short(20, 0, false),
            "a wholly throttled-out target is the case that opened the gate",
        );
        assert!(
            batch_fell_short(0, 0, true),
            "a pass that spent its failure budget did not see the folder",
        );
        // And the gate is closed by it, whichever target came up short.
        assert!(!pass_may_prune(&[(true, false), (true, true)]));
    }

    /// The `$orderby` fallback: a tenant that rejects the ordered enumeration
    /// gets an unordered walk rather than a folder that never syncs.
    #[test]
    fn the_enumeration_url_can_drop_the_orderby() {
        let ordered = enumeration_url("inbox", true);
        assert!(
            ordered.contains("$orderby=receivedDateTime%20desc"),
            "got {ordered}"
        );
        let unordered = enumeration_url("inbox", false);
        assert!(!unordered.contains("$orderby"), "got {unordered}");
        for url in [&ordered, &unordered] {
            assert!(url.contains("/me/mailFolders/inbox/messages?"), "got {url}");
            assert!(url.contains("$select=id,internetMessageId,isRead,receivedDateTime"));
            assert!(url.contains("$top=200"));
        }
    }

    /// Graph answers HTTP 400 to `$select=...contentBytes` on the base
    /// attachment collection, which left every Graph message without its
    /// attachments; the list request must not narrow the properties at all.
    #[test]
    fn the_attachment_list_url_selects_nothing() {
        let url = attachments_url("AAMk=");
        assert_eq!(url, format!("{GRAPH_BASE}/me/messages/AAMk=/attachments"));
        assert!(!url.contains("$select"), "got {url}");
        assert!(!url.contains("contentBytes"), "got {url}");
    }

    /// A live `/attachments` page mixes file, item and reference attachments,
    /// with `null` where a type lacks a property: the file comes through, the
    /// others are skipped, and none of them fails the parse.
    #[test]
    fn item_and_reference_attachments_do_not_break_the_file_ones() {
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode(b"%PDF-1.4");
        let page = serde_json::json!({
            "@odata.context": "https://graph.microsoft.com/v1.0/$metadata#...",
            "value": [
                {
                    "@odata.type": "#microsoft.graph.fileAttachment",
                    "id": "A1",
                    "name": "../report.pdf",
                    "contentType": "application/pdf",
                    "size": 8,
                    "isInline": false,
                    "contentId": null,
                    "contentLocation": null,
                    "contentBytes": b64,
                },
                {
                    "@odata.type": "#microsoft.graph.itemAttachment",
                    "id": "A2",
                    "name": "Forwarded message",
                    "contentType": null,
                    "size": 4096,
                    "isInline": null,
                },
                {
                    "@odata.type": "#microsoft.graph.referenceAttachment",
                    "id": "A3",
                    "name": "Plan.docx",
                    "contentType": null,
                    "isInline": false,
                },
                {
                    "@odata.type": "#microsoft.graph.fileAttachment",
                    "id": "A4",
                    "name": "empty.txt",
                    "contentBytes": "",
                },
            ],
        });
        let list: GraphAttachmentList = serde_json::from_value(page).expect("page parses");
        let attachments = attachments_from_list(list);
        assert_eq!(attachments.len(), 1, "got {attachments:?}");
        assert_eq!(attachments[0].content, b"%PDF-1.4");
        assert!(!attachments[0].filename.contains('/'));
        assert!(attachments[0].filename.ends_with("report.pdf"));
    }

    /// The inline backfill's listing (PERSO-165): `contentId` qualified with
    /// its derived type, which Graph accepts where the bare name is a 400, no
    /// content; every entry in order, the file attachments ingest stored
    /// flagged as such.
    #[test]
    fn the_metadata_listing_names_file_attachments_and_their_ids() {
        let url = attachment_metadata_url("AAA");
        assert!(url.ends_with(
            "/me/messages/AAA/attachments?\
             $select=id,name,isInline,microsoft.graph.fileAttachment/contentId"
        ));
        assert!(!url.contains("contentBytes"));
        let page = serde_json::json!({
            "value": [
                {"@odata.type": "#microsoft.graph.fileAttachment", "name": "image001.png",
                 "isInline": true, "contentId": "image001.png@01DA"},
                {"@odata.type": "#microsoft.graph.itemAttachment", "name": "Forwarded"},
                {"@odata.type": "#microsoft.graph.fileAttachment", "name": "a/report.pdf",
                 "isInline": false, "contentId": null},
            ],
        });
        let list: GraphAttachmentList = serde_json::from_value(page).expect("page parses");
        assert_eq!(
            server_attachments(list),
            vec![
                crate::ingest::ServerAttachment {
                    name: "image001.png".into(),
                    content_id: Some("image001.png@01DA".into()),
                    is_file: true,
                },
                crate::ingest::ServerAttachment {
                    name: "Forwarded".into(),
                    content_id: None,
                    is_file: false,
                },
                crate::ingest::ServerAttachment {
                    name: "a/report.pdf".into(),
                    content_id: None,
                    is_file: true,
                },
            ]
        );
    }

    #[test]
    fn batch_body_names_one_get_per_id() {
        let body = batch_request_body(&["AAA", "BBB"]);
        let requests = body["requests"].as_array().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0]["id"], "0");
        assert_eq!(requests[1]["id"], "1");
        assert_eq!(requests[0]["method"], "GET");
        let url = requests[1]["url"].as_str().unwrap();
        assert!(url.starts_with("/me/messages/BBB?$select="), "got {url}");
        assert!(url.contains("internetMessageId"));
    }

    #[test]
    fn test_iso_to_rfc2822() {
        let rfc = iso_to_rfc2822("2024-01-15T10:30:00Z");
        assert!(rfc.contains("2024"));
        assert!(rfc.contains("10:30"));
    }

    #[test]
    fn test_iso_to_rfc2822_with_timezone() {
        let rfc = iso_to_rfc2822("2024-06-15T14:30:00+02:00");
        assert!(rfc.contains("2024"));
    }

    #[test]
    fn test_iso_to_rfc2822_invalid() {
        let result = iso_to_rfc2822("not-a-date");
        assert_eq!(result, "not-a-date");
    }

    #[test]
    fn test_format_recipient_with_name() {
        let r = GraphRecipient {
            email_address: GraphEmailAddress {
                name: Some("John Doe".to_string()),
                address: Some("john@example.com".to_string()),
            },
        };
        assert_eq!(format_recipient(&r), "John Doe <john@example.com>");
    }

    #[test]
    fn test_format_recipient_no_name() {
        let r = GraphRecipient {
            email_address: GraphEmailAddress {
                name: None,
                address: Some("john@example.com".to_string()),
            },
        };
        assert_eq!(format_recipient(&r), "john@example.com");
    }

    /// "Sent Items" in a `/me/mailFolders/{x}` URL is a malformed id to
    /// Graph; the well-known names and Exchange's English display names must
    /// both reach the URL as the well-known name.
    #[test]
    fn standard_folders_resolve_to_their_well_known_names() {
        for (configured, expected) in [
            ("Inbox", "inbox"),
            ("inbox", "inbox"),
            ("Archive", "archive"),
            ("SentItems", "sentitems"),
            ("Sent Items", "sentitems"),
            ("sentitems", "sentitems"),
            ("sent", "sentitems"),
            ("Drafts", "drafts"),
            ("Deleted Items", "deleteditems"),
            ("deleteditems", "deleteditems"),
            ("Junk Email", "junkemail"),
            ("Junk E-mail", "junkemail"),
            ("junkemail", "junkemail"),
        ] {
            assert_eq!(
                well_known_folder(configured),
                Some(expected),
                "{configured}"
            );
        }
        assert_eq!(well_known_folder("MyFolder"), None);
        assert_eq!(well_known_folder("Gesendete Elemente"), None);
    }

    /// A localized mailbox names its standard folders in its own language; the
    /// display name resolves to the folder id through the folder list.
    #[test]
    fn a_localized_display_name_resolves_through_the_folder_list() {
        let list: GraphMailFolderList = serde_json::from_value(serde_json::json!({
            "value": [
                {"id": "AAMkInbox", "displayName": "Posteingang", "totalItemCount": 3},
                {"id": "AAMkSent", "displayName": "Gesendete Elemente"},
                {"id": "AAMkJunk", "displayName": "Junk-E-Mail"},
            ],
            "@odata.nextLink": "https://graph.microsoft.com/v1.0/me/mailFolders?$skip=3",
        }))
        .expect("folder page parses");
        assert!(list.next_link.is_some());
        let folders = list.value;
        assert_eq!(
            folder_id_by_display_name(&folders, "Gesendete Elemente"),
            Some("AAMkSent")
        );
        assert_eq!(
            folder_id_by_display_name(&folders, "junk-e-mail"),
            Some("AAMkJunk")
        );
        assert_eq!(folder_id_by_display_name(&folders, "Projekte"), None);
    }

    #[test]
    fn only_an_id_shaped_name_skips_the_lookup() {
        let id = "AAMkADc0ZGFmYmIzLTE3NjAtNDQzMS04NTczLTk4MzU0NTJhM2JkZgAuAAAAAACtv1F197oaSaeGBAwG1kbSAQDOhGPffOUcTJWagX2l-iGKAAAAAAEJAAA=";
        assert!(looks_like_folder_id(id));
        assert!(!looks_like_folder_id("Gesendete Elemente"));
        assert!(!looks_like_folder_id("MyFolder"));
    }

    // The Graph rendering now goes through the shared grammar (#0086a):
    // `crate::search::parse` -> `to_graph`. These pin the same wiring the
    // search command uses, over the unified AST.
    /// A plus-address, an ampersand and a hash all reach Graph as the values
    /// they were: percent-encoded in the URL, and `$filter` still its own
    /// parameter.
    #[test]
    fn search_url_encodes_every_value() {
        let query = crate::search::parse(r#"from:a+b@x.com subject:"R&D #1""#).unwrap();
        let graph_query = crate::search::to_graph(&query).unwrap();
        let url = search_url(
            "https://graph.microsoft.com/v1.0/me/messages",
            10,
            &graph_query,
            SearchShape::Whole,
        )
        .unwrap();
        let raw = url.as_str();
        assert!(raw.contains("%2B"), "{raw}");
        assert!(raw.contains("%26"), "{raw}");
        assert!(raw.contains("%23"), "{raw}");
        assert!(url.fragment().is_none(), "{raw}");
        let pairs: std::collections::HashMap<String, String> =
            url.query_pairs().into_owned().collect();
        assert_eq!(
            pairs["$search"],
            r#""from:a+b@x.com AND subject:\"R&D #1\"""#
        );
        assert_eq!(pairs["$top"], "10");
    }

    /// A server search's first pass asks Graph for the stub fields alone,
    /// without the bodies and the threading `$expand` a whole hit carries
    /// (PERSO-175).
    #[test]
    fn a_stub_search_asks_for_the_stub_fields_alone() {
        let query = crate::search::parse("from:ivana ledger").unwrap();
        let graph_query = crate::search::to_graph(&query).unwrap();
        let base = "https://graph.microsoft.com/v1.0/me/messages";
        let pairs = |shape| -> std::collections::HashMap<String, String> {
            search_url(base, 50, &graph_query, shape)
                .unwrap()
                .query_pairs()
                .into_owned()
                .collect()
        };
        let stub = pairs(SearchShape::Stub);
        assert_eq!(stub["$select"], STUB_SELECT);
        assert!(!stub.contains_key("$expand"), "{stub:?}");
        assert_eq!(stub["$top"], "50");
        let whole = pairs(SearchShape::Whole);
        assert!(whole["$select"].contains("body"), "{whole:?}");
        assert!(whole.contains_key("$expand"), "{whole:?}");
    }

    /// A stub carries the folder it sits in, which is what a search across
    /// every folder lists its hit under (PERSO-175).
    #[test]
    fn a_stub_names_the_folder_it_sits_in() {
        assert!(STUB_SELECT
            .split(',')
            .any(|field| field == "parentFolderId"));
        let stub: GraphSearchStub = serde_json::from_value(serde_json::json!({
            "id": "AAMsg",
            "internetMessageId": "<a@x>",
            "receivedDateTime": "2026-10-22T10:00:00Z",
            "parentFolderId": "AAMkSub",
        }))
        .unwrap();
        assert_eq!(stub.parent_folder_id.as_deref(), Some("AAMkSub"));
    }

    /// A hit folder that is a configured mailbox is listed under its label,
    /// whatever Graph calls it, any other under its display name, and one
    /// Graph did not name is left to the caller (PERSO-175).
    #[test]
    fn a_hit_folder_is_labelled_by_its_mailbox_else_its_display_name() {
        let folder = |id: &str, name: &str| {
            Some(GraphFolderName {
                id: id.into(),
                display_name: name.into(),
            })
        };
        let asked = [
            (Some("Inbox"), "inbox"),
            (Some("Extra"), "AAMkReports"),
            (None, "AAMkInbox"),
            (None, "AAMkSub"),
            (None, "AAMkReports"),
            (None, "AAMkGone"),
        ];
        let answers = [
            folder("AAMkInbox", "Posteingang"),
            None,
            folder("AAMkInbox", "Posteingang"),
            folder("AAMkSub", "2023"),
            folder("AAMkReports", "Reports"),
            None,
        ];
        let labels = label_folders(&asked, &answers);
        assert_eq!(labels["AAMkInbox"], "Inbox", "the configured label wins");
        assert_eq!(labels["AAMkSub"], "2023");
        assert_eq!(
            labels["AAMkReports"], "Extra",
            "a mailbox configured by its id matches by that id"
        );
        assert!(!labels.contains_key("AAMkGone"));
        assert_eq!(labels.len(), 3);
    }

    /// #0141 follow-up: a hit from a folder the listing holds the id of is
    /// labelled with no request, an ignored child folder included, which is
    /// what lets the hit be fetched under the name the listing knows it by.
    /// Only an id the listing does not have is left to ask Graph about.
    #[test]
    fn a_hit_folder_the_listing_knows_is_labelled_without_a_request() {
        let id = |tail: &str| format!("AAMkAGI2{}{tail}", "x".repeat(64));
        let configured = vec![
            ("Inbox".to_string(), id("inbox")),
            ("Archive/2023".to_string(), id("old")),
            ("Spesen".to_string(), "Spesen".to_string()),
        ];
        let (labelled, unknown) =
            known_folder_labels(&configured, &[id("inbox"), id("old"), id("new")]);
        assert_eq!(labelled[&id("inbox")], "Inbox");
        assert_eq!(labelled[&id("old")], "Archive/2023");
        assert_eq!(unknown, vec![id("new")]);
    }

    /// A search page carries Graph's `@odata.nextLink`, which the search
    /// follows; the list used to drop it, so one page was all a search read.
    #[test]
    fn a_search_page_keeps_its_next_link() {
        let page: GraphPage<GraphMessage> = serde_json::from_value(serde_json::json!({
            "value": [],
            "@odata.nextLink": "https://graph.microsoft.com/v1.0/me/messages?$skip=1000"
        }))
        .unwrap();
        assert_eq!(
            page.next_link.as_deref(),
            Some("https://graph.microsoft.com/v1.0/me/messages?$skip=1000")
        );
        let last: GraphPage<GraphMessage> =
            serde_json::from_value(serde_json::json!({"value": []})).unwrap();
        assert_eq!(last.next_link, None);
    }

    /// `--limit 2500` pages past one page of 1000 instead of answering 1000
    /// (PERSO-173 review), and stops at the limit, saying more matched when
    /// Graph still holds a page.
    #[test]
    fn a_search_pages_until_its_limit() {
        let next = || Some("https://graph.microsoft.com/v1.0/next".to_string());
        assert_eq!(
            after_search_page(1000, 2500, next(), false, 1),
            SearchStep::Next("https://graph.microsoft.com/v1.0/next".into())
        );
        assert_eq!(
            after_search_page(2500, 2500, next(), false, 3),
            SearchStep::Done {
                truncated: true,
                capped: false
            }
        );
        assert_eq!(
            after_search_page(2500, 2500, None, false, 3),
            SearchStep::Done {
                truncated: false,
                capped: false
            }
        );
        assert_eq!(
            after_search_page(1800, 2500, None, false, 2),
            SearchStep::Done {
                truncated: false,
                capped: false
            },
            "a filter that runs out of pages found everything"
        );
        assert_eq!(
            after_search_page(30, 20, None, true, 1),
            SearchStep::Done {
                truncated: true,
                capped: false
            },
            "a page longer than the limit is cut"
        );
    }

    /// A `$search` answers 1000 results at most however it is paged, so one
    /// that runs out there under a larger limit was cut by Graph and says so;
    /// one that runs out below it found everything.
    #[test]
    fn a_search_that_runs_out_at_the_graph_cap_says_it_was_capped() {
        assert_eq!(
            after_search_page(GRAPH_SEARCH_CAP, 2500, None, true, 2),
            SearchStep::Done {
                truncated: true,
                capped: true
            }
        );
        assert_eq!(
            after_search_page(999, 2500, None, true, 2),
            SearchStep::Done {
                truncated: false,
                capped: false
            }
        );
        assert_eq!(
            after_search_page(GRAPH_SEARCH_CAP, GRAPH_SEARCH_CAP, None, true, 1),
            SearchStep::Done {
                truncated: false,
                capped: false
            },
            "a limit Graph can meet is not capped"
        );
    }

    /// A `@odata.nextLink` chain has no termination proof of its own, so the
    /// search stops at the page bound and calls the answer cut.
    #[test]
    fn a_search_stops_at_the_page_bound() {
        assert_eq!(
            after_search_page(
                10,
                2500,
                Some("https://graph.microsoft.com/v1.0/next".into()),
                true,
                MAX_ENUMERATION_PAGES
            ),
            SearchStep::Done {
                truncated: true,
                capped: false
            }
        );
    }

    fn url_pairs(query: &str, limit: usize) -> std::collections::HashMap<String, String> {
        let graph_query = crate::search::to_graph(&crate::search::parse(query).unwrap()).unwrap();
        search_url(
            "https://graph.microsoft.com/v1.0/me/messages",
            limit,
            &graph_query,
            SearchShape::Whole,
        )
        .unwrap()
        .query_pairs()
        .into_owned()
        .collect()
    }

    /// The parameter combinations Graph refuses never go out (PERSO-173):
    /// `$search` travels alone, without `$orderby` (`SearchWithOrderBy`) or
    /// `$filter` (`SearchWithFilter`), and a filter-only query no longer
    /// carries an `$orderby` its filter does not start with
    /// (`InefficientFilter`).
    #[test]
    fn a_search_url_never_pairs_parameters_graph_refuses() {
        for query in [
            "from:amazon",
            "from:ovh mailypoppins after:2026-10-01",
            "subject:PERSO-144",
            "after:2026-10-01 before:2026-10-15",
            "has:attachment",
            "invoice",
        ] {
            let pairs = url_pairs(query, 20);
            assert!(pairs.contains_key("$search"), "{query}: {pairs:?}");
            assert!(!pairs.contains_key("$orderby"), "{query}: {pairs:?}");
            assert!(!pairs.contains_key("$filter"), "{query}: {pairs:?}");
        }
        assert_eq!(url_pairs("from:amazon", 20)["$search"], "\"from:amazon\"");

        let lookup = url_pairs("message-id:<a@x>", 1);
        assert_eq!(lookup["$filter"], "internetMessageId eq '<a@x>'");
        assert!(!lookup.contains_key("$orderby"), "{lookup:?}");
        assert!(!lookup.contains_key("$search"), "{lookup:?}");

        // Only the term-less listing is sorted server-side.
        let all = url_pairs("", 20);
        assert_eq!(all["$orderby"], "receivedDateTime desc");
        assert!(!all.contains_key("$search") && !all.contains_key("$filter"));
    }

    #[test]
    fn the_page_size_stays_within_what_graph_accepts() {
        assert_eq!(url_pairs("x", 0)["$top"], "1");
        assert_eq!(url_pairs("x", 5000)["$top"], "1000");
    }

    /// A refused search is an error that names what was sent, never a quiet
    /// listing of the newest messages.
    #[test]
    fn a_refused_search_names_the_query_it_sent() {
        let graph_query =
            crate::search::to_graph(&crate::search::parse("subject:invoice").unwrap()).unwrap();
        let err = search_refused(
            &graph_query,
            reqwest::StatusCode::BAD_REQUEST,
            r#"{"error":{"code":"SearchWithOrderBy"}}"#,
        )
        .to_string();
        assert!(err.contains(r#"$search="subject:invoice""#), "{err}");
        assert!(err.contains("400"), "{err}");
        assert!(err.contains("SearchWithOrderBy"), "{err}");
    }

    /// A Message-ID carrying URL and OData metacharacters reaches Graph as
    /// exactly the literal it is: quotes doubled, nothing read as a separator.
    /// Graph says `hasAttachments: false` for a mail whose only attachments
    /// are inline images; its HTML body's `cid:` references are what get the
    /// list fetched and the stored flag raised, so a failed fetch is repaired.
    #[test]
    fn an_inline_only_message_still_lists_its_attachments() {
        let msg = |content_type: &str, content: &str, has: bool| -> GraphMessage {
            serde_json::from_value(serde_json::json!({
                "id": "AAMk",
                "internetMessageId": "<m@x>",
                "body": {"contentType": content_type, "content": content},
                "hasAttachments": has,
            }))
            .unwrap()
        };
        let inline = msg("html", r#"<p><img src="CID:image001.png@01DA"></p>"#, false);
        assert!(lists_attachments(&inline));
        assert!(graph_message_to_fetched_email(&inline).has_attachments);

        let plain_html = msg("html", "<p>no images</p>", false);
        assert!(!lists_attachments(&plain_html));
        assert!(!graph_message_to_fetched_email(&plain_html).has_attachments);

        // A text body that merely mentions the scheme is not a reference.
        let text = msg("text", "see cid:foo", false);
        assert!(!lists_attachments(&text));

        let flagged = msg("text", "hi", true);
        assert!(lists_attachments(&flagged));
    }

    /// The bounded client builds with this build's TLS stack.
    #[test]
    fn the_graph_http_client_builds_with_its_timeouts() {
        http_client().expect("the Graph client builds");
        assert!(READ_TIMEOUT > CONNECT_TIMEOUT);
    }

    #[test]
    fn the_message_lookup_url_escapes_the_id() {
        let url = message_lookup_url("<a+b&c#d'e%f=g?h@x.com>").unwrap();
        let raw = url.as_str();
        assert!(url.fragment().is_none(), "{raw}");
        assert!(raw.contains("%2B"), "{raw}");
        assert!(raw.contains("%26"), "{raw}");
        assert!(raw.contains("%23"), "{raw}");
        let pairs: Vec<(String, String)> = url.query_pairs().into_owned().collect();
        assert_eq!(
            pairs,
            vec![
                (
                    "$filter".to_string(),
                    "internetMessageId eq '<a+b&c#d''e%f=g?h@x.com>'".to_string()
                ),
                ("$select".to_string(), "id".to_string()),
            ]
        );
        assert_eq!(url.path(), "/v1.0/me/messages");
        // A bare id gets its brackets back.
        let bare = message_lookup_url("plain@x.com").unwrap();
        let pairs: Vec<(String, String)> = bare.query_pairs().into_owned().collect();
        assert_eq!(pairs[0].1, "internetMessageId eq '<plain@x.com>'");
    }

    /// Only a refusal Graph gives before it processes anything leaves a
    /// delivering request clean; any other 5xx may follow a send Exchange
    /// already took (#0063 review), and so may a 404 on a draft's `/send`.
    #[test]
    fn only_a_refusal_or_a_throttle_proves_a_delivering_request_sent_nothing() {
        let ambiguous = |status, retry_after, gone| {
            delivering_status_error(status, retry_after, gone, format!("HTTP {status}"))
                .chain()
                .any(|c| c.is::<crate::outbox::AmbiguousSubmission>())
        };
        for clean in [400, 401, 403, 413, 429] {
            assert!(!ambiguous(clean, false, false), "{clean}");
            assert!(!ambiguous(clean, false, true), "{clean} on /send");
        }
        assert!(!ambiguous(503, true, false), "a throttling 503");
        assert!(!ambiguous(404, false, false), "sendMail: no such mailbox");
        assert!(ambiguous(404, false, true), "/send: the draft left Drafts");
        for unknown in [500, 502, 504] {
            assert!(ambiguous(unknown, false, false), "{unknown}");
            assert!(ambiguous(unknown, true, true), "{unknown} on /send");
        }
        assert!(ambiguous(503, false, false), "a 503 that is not throttling");
    }

    /// A copy is sent when it is not a draft, and a draft counts only in the
    /// Drafts folder: one in the Outbox is a send under way.
    #[test]
    fn server_copies_fold_both_lookups_by_folder() {
        let entry = |id: &str, is_draft, folder: &str| CopyEntry {
            id: id.to_string(),
            is_draft,
            parent_folder_id: Some(folder.to_string()),
        };
        let copies = fold_copies(
            "DRAFTS",
            vec![
                entry("a", true, "DRAFTS"),
                entry("b", true, "OUTBOX"),
                entry("a", true, "DRAFTS"),
            ],
        );
        assert_eq!(
            copies,
            ServerCopies {
                sent: false,
                drafts: vec!["a".to_string()]
            }
        );
        assert!(fold_copies("DRAFTS", vec![entry("c", false, "SENT")]).sent);
        assert_eq!(
            draft_state_of("DRAFTS", &entry("a", true, "DRAFTS")),
            DraftState::InDrafts
        );
        assert_eq!(
            draft_state_of("DRAFTS", &entry("b", true, "OUTBOX")),
            DraftState::Sending
        );
        assert_eq!(
            draft_state_of("DRAFTS", &entry("c", false, "SENT")),
            DraftState::Sent
        );
    }

    #[test]
    fn the_property_lookup_filters_on_the_stamped_message_id() {
        let url = property_lookup_url("me/messages", "<a'b+c@x.com>", "id").unwrap();
        let pairs: Vec<(String, String)> = url.query_pairs().into_owned().collect();
        assert_eq!(
            pairs[0].1,
            format!(
                "singleValueExtendedProperties/Any(ep: ep/id eq '{MESSAGE_ID_PROPERTY}' \
                 and ep/value eq '<a''b+c@x.com>')"
            )
        );
        assert!(url.as_str().contains("%2B"));
    }

    /// Every request of the staged send asks for immutable ids, so the draft
    /// id the outbox records is the one `/send`, `permanentDelete` and the
    /// lookups name and answer with, wherever the item has moved since
    /// (#0063 review).
    #[test]
    fn every_staged_send_request_asks_for_immutable_ids() {
        let client = GraphClient {
            client: http_client().unwrap(),
            access_token: "token".to_string(),
            username: "me@example.com".to_string(),
            top_folders: std::sync::Mutex::new(None),
        };
        let attachment = OutgoingAttachment {
            name: "a.pdf".to_string(),
            content_type: "application/pdf".to_string(),
            bytes: vec![1, 2, 3],
        };
        let lookup = internet_id_url("me/messages", "<a@x.com>", "id").unwrap();
        let requests = [
            (
                "create (MIME)",
                client.create_draft_mime_request("TUlNRQ==".to_string()),
            ),
            (
                "create (JSON)",
                client.create_draft_json_request(&serde_json::json!({})),
            ),
            ("attach", client.attachment_post_request("d1", &attachment)),
            (
                "upload session",
                client.upload_session_request("d1", &attachment),
            ),
            ("send", client.send_draft_request("d1")),
            ("discard", client.discard_draft_request("d1")),
            ("state", client.draft_state_request("d1").unwrap()),
            ("lookup", client.item_request(reqwest::Method::GET, lookup)),
        ];
        for (what, request) in requests {
            let request = request.build().unwrap();
            assert_eq!(
                request.headers().get("Prefer").map(|v| v.to_str().unwrap()),
                Some(PREFER_IMMUTABLE_ID),
                "{what}"
            );
            assert!(
                request
                    .headers()
                    .contains_key(reqwest::header::AUTHORIZATION),
                "{what}"
            );
        }
        assert_eq!(PREFER_IMMUTABLE_ID, r#"IdType="ImmutableId""#);
    }

    #[test]
    fn the_mime_url_is_the_message_s_value_endpoint() {
        assert_eq!(
            mime_url("AAMkAD-x_y="),
            "https://graph.microsoft.com/v1.0/me/messages/AAMkAD-x_y=/$value"
        );
    }

    #[test]
    fn graph_params_text_only() {
        let query = crate::search::parse("hello world").unwrap();
        assert_eq!(
            crate::search::to_graph(&query).unwrap(),
            crate::search::GraphQuery::Search("hello AND world".into())
        );
    }

    #[test]
    fn graph_params_from_is_kql() {
        let query = crate::search::parse("from:alice@example.com").unwrap();
        assert_eq!(
            crate::search::to_graph(&query).unwrap(),
            crate::search::GraphQuery::Search("from:alice@example.com".into())
        );
    }

    #[test]
    fn graph_params_date_range() {
        let query = crate::search::parse("after:2024-01-01 before:2024-02-01").unwrap();
        assert_eq!(
            crate::search::to_graph(&query).unwrap(),
            crate::search::GraphQuery::Search(
                "received>=2024-01-01 AND received<2024-02-01".into()
            )
        );
    }

    // -----------------------------------------------------------------------
    // #0074 review: the Graph ingest-failure bound
    // -----------------------------------------------------------------------

    /// The Graph sync loop folds an ingest failure into `truncated`, which
    /// suspends the account's prune. Without a bound, a message this store
    /// rejects every time would do that on every pass, for good: the deadlock
    /// #0074 closed on the IMAP side.
    ///
    /// This walks the two calls the Graph loop makes per failed message, in
    /// production order (`note_ingest_failure` folded into `ingest_failed`,
    /// then `pass_may_prune` over the coverage tuple), over the real store.
    #[test]
    fn a_poisoned_graph_message_stops_holding_the_prune_after_three_passes() {
        let tmp = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(tmp.path().join("store.sqlite3")).unwrap();
        let uid = crate::ingest::graph_uid("<poison@example.com>");

        for pass in 1..=2 {
            let ingest_failed = crate::ingest::note_ingest_failure(
                &store,
                "acct",
                "inbox",
                "Inbox",
                uid,
                "the store will not take it",
            );
            assert!(ingest_failed, "pass {pass} must still retry");
            assert!(
                !crate::ingest::pass_may_prune(&[(true, ingest_failed)]),
                "pass {pass} still reports itself short, so the prune stays deferred"
            );
        }

        let ingest_failed = crate::ingest::note_ingest_failure(
            &store,
            "acct",
            "inbox",
            "Inbox",
            uid,
            "the store will not take it",
        );
        assert!(!ingest_failed, "the third failure gives up on the message");
        assert!(
            crate::ingest::pass_may_prune(&[(true, ingest_failed)]),
            "and the Graph account's prune runs again instead of being suspended for good"
        );
        assert_eq!(
            crate::ingest::ingest_failure_attempts(&store, "acct", "inbox", uid),
            3
        );
    }

    /// The other half of the bound: a success clears the count, so a message
    /// that fails transiently gets a full three attempts every time, not three
    /// for its lifetime.
    #[test]
    fn a_successful_graph_ingest_clears_the_failure_count() {
        let tmp = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(tmp.path().join("store.sqlite3")).unwrap();
        let uid = crate::ingest::graph_uid("<flaky@example.com>");

        assert!(crate::ingest::note_ingest_failure(
            &store, "acct", "inbox", "Inbox", uid, "locked"
        ));
        assert!(crate::ingest::note_ingest_failure(
            &store, "acct", "inbox", "Inbox", uid, "locked"
        ));
        assert_eq!(
            crate::ingest::ingest_failure_attempts(&store, "acct", "inbox", uid),
            2
        );

        // The success path of the loop's `match`.
        crate::ingest::clear_ingest_failure(&store, "acct", "inbox", uid);
        assert_eq!(
            crate::ingest::ingest_failure_attempts(&store, "acct", "inbox", uid),
            0
        );

        assert!(crate::ingest::note_ingest_failure(
            &store, "acct", "inbox", "Inbox", uid, "locked"
        ));
        assert_eq!(
            crate::ingest::ingest_failure_attempts(&store, "acct", "inbox", uid),
            1,
            "the next failure starts the count over"
        );
    }

    // -----------------------------------------------------------------
    // #0042: /messages/delta
    // -----------------------------------------------------------------

    fn delta_page(json: serde_json::Value) -> GraphDeltaPage {
        serde_json::from_value(json).expect("a delta page must parse")
    }

    #[test]
    fn the_delta_decision_matrix() {
        let token = format!("{GRAPH_BASE}/me/mailFolders('AAA')/messages/delta?$deltatoken=t1");

        // No token: the bootstrap pass enumerates and mints one.
        assert_eq!(delta_verdict(100, None, false), DeltaVerdict::NoToken);
        assert_eq!(delta_verdict(usize::MAX, None, true), DeltaVerdict::NoToken);

        // A full sync always relists: it is the periodic whole-folder
        // observation the prune and the token both lean on.
        assert_eq!(
            delta_verdict(usize::MAX, Some(&token), true),
            DeltaVerdict::FullSync
        );

        // A quick sync with a token minted against this folder is the only
        // branch that takes the delta.
        assert_eq!(delta_verdict(100, Some(&token), true), DeltaVerdict::Use);

        // The Graph analogue of a UIDVALIDITY change: a folder that is not the
        // one the token was minted against invalidates it, whatever the sync.
        assert_eq!(
            delta_verdict(100, Some(&token), false),
            DeltaVerdict::FolderChanged
        );
        assert_eq!(
            delta_verdict(usize::MAX, Some(&token), false),
            DeltaVerdict::FolderChanged
        );

        // Anything that is not a delta URL this client would have stored is
        // thrown away rather than sent to the server.
        for junk in ["", "not a url", "https://evil.example/me/messages/delta"] {
            assert_eq!(
                delta_verdict(100, Some(junk), true),
                DeltaVerdict::Discard(DeltaDiscard::Malformed),
                "{junk:?} is not a token"
            );
        }
        let no_delta = format!("{GRAPH_BASE}/me/mailFolders('AAA')/messages?$top=200");
        assert_eq!(
            delta_verdict(100, Some(&no_delta), true),
            DeltaVerdict::Discard(DeltaDiscard::Malformed)
        );
    }

    #[test]
    fn an_expired_delta_link_is_discarded_and_anything_else_unusual_with_it() {
        use reqwest::StatusCode;
        assert_eq!(delta_status_discard(StatusCode::OK), None);
        // The documented expiry: 410 Gone / resyncRequired.
        assert_eq!(
            delta_status_discard(StatusCode::GONE),
            Some(DeltaDiscard::Expired)
        );
        // The folder went out from under the token.
        assert_eq!(
            delta_status_discard(StatusCode::NOT_FOUND),
            Some(DeltaDiscard::Expired)
        );
        // Everything else is a failure, and a failed walk is indistinguishable
        // from one that skipped a message, so it drops the token too.
        for status in [
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::UNAUTHORIZED,
            StatusCode::INTERNAL_SERVER_ERROR,
            StatusCode::SERVICE_UNAVAILABLE,
        ] {
            assert_eq!(
                delta_status_discard(status),
                Some(DeltaDiscard::Failed),
                "{status} must not be trusted"
            );
        }
    }

    #[test]
    fn a_removed_entry_is_counted_and_never_mistaken_for_a_message() {
        let page = delta_page(serde_json::json!({
            "value": [
                {
                    "id": "AAA",
                    "internetMessageId": " <kept@example.com> ",
                    "isRead": true,
                    "receivedDateTime": "2026-01-01T00:00:00Z"
                },
                // A removal carries the Graph id and nothing else: no
                // internetMessageId, which is the identity the store keys on.
                { "id": "BBB", "@removed": { "reason": "deleted" } },
                { "id": "CCC", "@removed": { "reason": "changed" } },
                // No usable identity: dropped, as in a full enumeration.
                { "id": "DDD", "isRead": false }
            ],
            "@odata.deltaLink": "https://graph.microsoft.com/v1.0/x/delta?$deltatoken=t2"
        }));
        let mut changed = HashMap::new();
        let mut removed = 0usize;
        absorb_delta_page(&mut changed, &mut removed, page.value);

        assert_eq!(removed, 2, "both removal reasons count");
        assert_eq!(changed.len(), 1);
        let entry = changed
            .get("<kept@example.com>")
            .expect("keyed on the trimmed id");
        assert_eq!(entry.graph_id, "AAA");
        assert!(entry.is_read);

        let delta = FolderDelta {
            changed,
            removed,
            delta_link: page.delta_link.unwrap(),
            pages: 1,
        };
        assert!(
            delta.forces_full_enumeration(),
            "a delta that reports removals hands the pass to the enumeration, because the \
             prune resolves a deletion by listing the folder and not from @removed"
        );
        assert!(!FolderDelta::default().forces_full_enumeration());
    }

    #[test]
    fn a_change_set_is_never_a_folder_listing() {
        // Why the orchestrator only computes the prune on a full enumeration:
        // fed a delta's change set, the same diff calls every message that
        // simply did not change since the token a deletion.
        let known = known(&["<a@x>", "<b@x>", "<c@x>"]);
        let change_set = folder(&[("<b@x>", "BBB", Some("2026-01-01T00:00:00Z"))]);
        let vanished = vanished_graph_uids(&known, &change_set);
        assert_eq!(
            vanished.len(),
            2,
            "which would be a mass prune, so `used_delta` gates the call"
        );
    }

    #[test]
    fn a_resume_point_is_only_minted_by_a_pass_that_covered_the_folder() {
        // covered, nothing left behind, everything written.
        assert!(may_record_delta_token(true, false, false));
        // An enumeration that did not see the whole folder.
        assert!(!may_record_delta_token(false, false, false));
        // A pass that left new messages undownloaded.
        assert!(!may_record_delta_token(true, true, false));
        // A message downloaded and not written is as absent as one never
        // fetched, so the token would over-claim.
        assert!(!may_record_delta_token(true, false, true));
    }

    #[test]
    fn a_poisoned_message_cannot_wedge_the_delta_chain_for_good() {
        let tmp = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(tmp.path().join("store.sqlite3")).unwrap();
        let uid = crate::ingest::graph_uid("<poison@example.com>");

        // The #0074 bound applies identically on the delta path: while the
        // message is still owed the token does not advance, so the next pass
        // replays the same changes...
        for pass in 1..=2 {
            let ingest_failed = crate::ingest::note_ingest_failure(
                &store,
                "acct",
                "inbox",
                "Inbox",
                uid,
                "the store will not take it",
            );
            assert!(
                !may_record_delta_token(true, false, ingest_failed),
                "pass {pass} has not written everything, so it mints no token"
            );
        }
        // ...and once the message is given up on, the chain moves again.
        let ingest_failed = crate::ingest::note_ingest_failure(
            &store,
            "acct",
            "inbox",
            "Inbox",
            uid,
            "the store will not take it",
        );
        assert!(!ingest_failed);
        assert!(
            may_record_delta_token(true, false, ingest_failed),
            "a message the store will never accept must not freeze the delta for ever"
        );
    }

    #[test]
    fn the_delta_token_survives_a_full_window_pass_and_only_a_clear_removes_it() {
        let tmp = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(tmp.path().join("store.sqlite3")).unwrap();
        let link = format!("{GRAPH_BASE}/me/mailFolders('AAA')/messages/delta?$deltatoken=t1");
        let identity = folder_identity_hash("AAA");

        crate::ingest::record_delta_token(&store, "acct", "inbox", identity, &link);
        let stored = crate::ingest::load_mailbox_cursor(&store, "acct", "inbox")
            .unwrap()
            .unwrap();
        assert_eq!(stored.deltalink.as_deref(), Some(link.as_str()));
        assert_eq!(stored.uidvalidity, Some(identity));

        // The #0054 carry-forward hazard, pinned for `deltalink` the way #0041
        // pinned it for `highest_modseq`: a full-window pass says nothing about
        // the token and must not wipe it.
        crate::ingest::record_mailbox_cursor(
            &store,
            "acct",
            "inbox",
            &crate::ingest::MailboxCursor {
                uidvalidity: Some(identity),
                exists: Some(42),
                deltalink: None,
                ..Default::default()
            },
        )
        .unwrap();
        let stored = crate::ingest::load_mailbox_cursor(&store, "acct", "inbox")
            .unwrap()
            .unwrap();
        assert_eq!(
            stored.deltalink.as_deref(),
            Some(link.as_str()),
            "a pass with nothing to say about the token leaves it alone"
        );

        // A folder identity change is what makes the token meaningless, and
        // the only way out is the explicit clear.
        let observed = folder_identity_hash("BBB");
        assert_ne!(observed, identity);
        assert_eq!(
            delta_verdict(100, stored.deltalink.as_deref(), observed == identity),
            DeltaVerdict::FolderChanged
        );
        crate::ingest::clear_mailbox_deltalink(&store, "acct", "inbox");
        let stored = crate::ingest::load_mailbox_cursor(&store, "acct", "inbox")
            .unwrap()
            .unwrap();
        assert_eq!(stored.deltalink, None, "the token cannot survive the reset");
        assert_eq!(
            delta_verdict(100, stored.deltalink.as_deref(), true),
            DeltaVerdict::NoToken,
            "and the next pass enumerates and mints a fresh one"
        );
    }

    #[test]
    fn the_delta_url_has_a_listing_form_and_a_mint_form() {
        let listing = delta_url("inbox", false);
        assert!(listing.starts_with(&format!(
            "{GRAPH_BASE}/me/mailFolders/inbox/messages/delta?"
        )));
        assert!(listing.contains("$select=id,internetMessageId,isRead,receivedDateTime"));
        assert!(
            !listing.contains("$deltatoken"),
            "the listing form asks for the folder's state, not for a bare token"
        );
        // No `$top`: the delta endpoint takes its page size from the `Prefer`
        // header, which `walk_delta` sends.
        assert!(!listing.contains("$top"));
        assert!(delta_url("inbox", true).ends_with("&$deltatoken=latest"));
    }

    #[test]
    fn a_delta_page_chain_ends_in_a_resume_point() {
        // The two shapes `walk_delta` branches on: a page that continues, and
        // a page that closes the chain.
        let middle = delta_page(serde_json::json!({
            "value": [],
            "@odata.nextLink": "https://graph.microsoft.com/v1.0/next"
        }));
        assert_eq!(
            middle.next_link.as_deref(),
            Some("https://graph.microsoft.com/v1.0/next")
        );
        assert_eq!(middle.delta_link, None);

        let last = delta_page(serde_json::json!({
            "value": [],
            "@odata.deltaLink": "https://graph.microsoft.com/v1.0/d?$deltatoken=t2"
        }));
        assert_eq!(last.next_link, None);
        assert!(last.delta_link.is_some());

        // A chain that simply stops has no resume point and no proof it was
        // complete; `walk_delta` answers `NoResumePoint` and the token goes.
        let orphan = delta_page(serde_json::json!({ "value": [] }));
        assert!(orphan.next_link.is_none() && orphan.delta_link.is_none());
    }
}
