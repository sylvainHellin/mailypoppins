//! The wire shapes: JSON-RPC 2.0, one message per line, `\n` terminated.
//!
//! The constants are the ones the plan fixes in section 3.0. The request cap
//! is enforced; the response cap is deliberately *not*, because three of the
//! four workloads exceed it by design and deciding what to do about that is
//! P1a-U4's job, not this harness's.

use serde::{Deserialize, Serialize};

/// Request frame cap fixed by the plan. Enforced on the server's read side.
pub const MAX_REQUEST_FRAME: usize = 1024 * 1024;

/// Envelopes per streamed chunk frame. Sized so a chunk of the 5000-row list
/// stays under the 1 MiB frame cap (a row serialises to roughly 400 bytes).
pub const CHUNK_ROWS: usize = 200;

/// Rows in one page of the paging option (P1a-U3). One screenful is 40 rows at
/// most, so 200 is five screens of scroll headroom, and it is the same number
/// `CHUNK_ROWS` uses so the two options are not compared at different
/// granularities.
pub const PAGE_ROWS: usize = 200;

/// The one method the spike server answers.
pub const METHOD_RUN: &str = "bench.run";

/// The notification the streamed workload sends before its response.
pub const METHOD_CHUNK: &str = "bench.chunk";

#[derive(Debug, Serialize, Deserialize)]
pub struct Request {
    pub jsonrpc: String,
    pub id: u64,
    pub method: String,
    pub params: Params,
}

impl Request {
    pub fn new(id: u64, workload: &str, stream: bool) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            method: METHOD_RUN.into(),
            params: Params { workload: workload.into(), stream },
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Params {
    pub workload: String,
    /// True: the result arrives as `bench.chunk` notifications followed by an
    /// empty response. False: one response frame carries the whole result.
    pub stream: bool,
}

/// One `messages` row on the wire.
///
/// A hand-written mirror of [`mailypoppins::store::read::MessageRow`], which is not
/// `Serialize`: the spike must not add a derive to product code, and the
/// mirror also pins the field set a daemon-era `list_mailbox` would send.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    pub id: i64,
    pub mailbox: String,
    pub uid: i64,
    pub message_id: String,
    pub from: Option<String>,
    pub to: Option<String>,
    pub cc: Option<String>,
    pub reply_to: Option<String>,
    pub bcc: Option<String>,
    pub subject: Option<String>,
    pub date_display: Option<String>,
    pub flags: Option<String>,
    pub has_attachments: bool,
    pub thread_id: Option<String>,
    pub is_invite: bool,
}

impl From<&mailypoppins::store::read::MessageRow> for Envelope {
    fn from(r: &mailypoppins::store::read::MessageRow) -> Self {
        Self {
            id: r.id,
            mailbox: r.mailbox.clone(),
            uid: r.uid,
            message_id: r.message_id.clone(),
            from: r.from.clone(),
            to: r.to.clone(),
            cc: r.cc.clone(),
            reply_to: r.reply_to.clone(),
            bcc: r.bcc.clone(),
            subject: r.subject.clone(),
            date_display: r.date_display.clone(),
            flags: r.flags.clone(),
            has_attachments: r.has_attachments,
            thread_id: r.thread_id.clone(),
            is_invite: r.is_invite,
        }
    }
}

/// The same fifteen fields as [`Envelope`], encoded positionally.
///
/// This is the "compact encoding" P1a-U3 measures the whole-list option under:
/// a JSON array per row instead of an object, which drops the key names
/// (roughly 150 bytes a row) and nothing else. The field set, the order and the
/// null handling are identical to `Envelope`, so the difference between the two
/// encodings is the encoding and not the payload.
///
/// A tuple struct with more than one field serialises as a JSON array, which is
/// why this is a tuple struct and not a rename-annotated record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactEnvelope(
    pub i64,
    pub String,
    pub i64,
    pub String,
    pub Option<String>,
    pub Option<String>,
    pub Option<String>,
    pub Option<String>,
    pub Option<String>,
    pub Option<String>,
    pub Option<String>,
    pub Option<String>,
    pub bool,
    pub Option<String>,
    pub bool,
);

impl From<&mailypoppins::store::read::MessageRow> for CompactEnvelope {
    fn from(r: &mailypoppins::store::read::MessageRow) -> Self {
        Self(
            r.id,
            r.mailbox.clone(),
            r.uid,
            r.message_id.clone(),
            r.from.clone(),
            r.to.clone(),
            r.cc.clone(),
            r.reply_to.clone(),
            r.bcc.clone(),
            r.subject.clone(),
            r.date_display.clone(),
            r.flags.clone(),
            r.has_attachments,
            r.thread_id.clone(),
            r.is_invite,
        )
    }
}

/// One shape for every workload's answer, so the client deserialises into a
/// concrete type (the realistic cost) without knowing which workload it asked
/// for at the type level.
///
/// Absent members are skipped on the wire: an answer that carries ids must not
/// pay for an empty `rows` key, or the byte comparison the list-transfer
/// decision rests on would charge each option for the other one's fields.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct WorkResult {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rows: Vec<Envelope>,
    /// Rows in the positional encoding, used by the P1a-U3 workloads.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub compact: Vec<CompactEnvelope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// Number of `bench.chunk` frames that carried `rows`, 0 when unstreamed.
    #[serde(default)]
    pub chunks: u32,
    /// Rows the server produced, whether or not they travelled in the response.
    #[serde(default)]
    pub row_count: u32,
    /// `message.select_all`: every row id of the current view.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ids: Vec<i64>,
    /// `message.jump_to_date`: the position the cursor lands on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<u32>,
    /// A total the client cannot compute because it does not hold every row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<u32>,
}

/// Server-side stage timings, carried back on every response.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize)]
pub struct Meta {
    /// Store read plus row conversion: the work a direct call also does.
    pub dispatch_us: u64,
    /// Serialising the result payload, response side.
    pub serialize_us: u64,
    /// Bytes of the result payload (all frames, delimiters excluded).
    pub bytes: u64,
}

/// The response frame, as the server builds it: `result` is already-encoded
/// JSON, spliced in rather than re-encoded.
#[derive(Debug, Serialize)]
pub struct RawResponse<'a> {
    pub jsonrpc: &'static str,
    pub id: u64,
    pub result: &'a serde_json::value::RawValue,
    pub meta: Meta,
}

/// The response frame as the client reads it, fully typed.
#[derive(Debug, Deserialize)]
pub struct Response {
    #[allow(dead_code)]
    pub jsonrpc: String,
    #[allow(dead_code)]
    pub id: u64,
    pub result: WorkResult,
    pub meta: Meta,
}

/// A streamed chunk notification, both directions.
#[derive(Debug, Serialize, Deserialize)]
pub struct ChunkFrame {
    pub jsonrpc: String,
    pub method: String,
    pub params: ChunkParams,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ChunkParams {
    pub id: u64,
    pub seq: u32,
    pub rows: Vec<Envelope>,
}
