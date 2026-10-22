//! Submitting a built message over Microsoft Graph, durably (PERSO-144
//! follow-up, #0063 item 3).
//!
//! ## Two routes, one set of headers
//!
//! A message whose base64 MIME fits one request goes through `/me/sendMail`
//! as MIME, exactly as PERSO-144 shipped it. A larger one used to fall back to
//! the JSON message resource, which carries no `In-Reply-To`, no `References`
//! and not our `Message-ID`, so a large reply landed outside its conversation.
//! It now goes as a *staged* send: a draft created from the same MIME with its
//! largest attachments taken out (`POST /me/messages`, the same MIME
//! conversion `sendMail` runs), the attachments added to that draft one
//! request each (a POST when small, an upload session when large), then
//! `POST /me/messages/{id}/send`. The headers are the ones the small path
//! sends, so the reply threads in Outlook the way the small one does.
//!
//! The route was chosen over `createReply` on the parent because it keeps our
//! `Message-ID` (the outbox row, the local Sent copy and the resume's lookup
//! all key on it), needs no Graph id of the parent (which may have moved or
//! been deleted), and serves forwards and new messages as well as replies.
//! Only a message whose *body* alone exceeds the cap still goes as JSON,
//! staged the same way, and that one cannot carry threading headers at all:
//! Graph accepts only `x-` names in `internetMessageHeaders`.
//!
//! ## Exactly once across a crash
//!
//! The outbox marker (`submission_started_at`) is committed immediately before
//! the one request that can deliver (`sendMail` or the draft's `send`), never
//! before creating or filling a draft, which deliver nothing. The draft's
//! Graph id is committed to the row's envelope as soon as it exists. A resume
//! ([`resume_rows`]) therefore finds every row in one of two shapes:
//!
//! - no marker: nothing can have gone out. Whatever an interrupted attempt left
//!   in Drafts under our `Message-ID`, or under the recorded id, is discarded,
//!   and the message is submitted afresh from the row's bytes. A copy already
//!   in Sent Items (an operator `retry` of a message that did arrive) is
//!   recorded as sent instead.
//! - a marker: the delivering request went out and its answer was lost. The
//!   mailbox settles it: a sent copy means delivered; the recorded draft,
//!   read by its id, still in Drafts [`SEND_SETTLE_SECS`] later means the
//!   send never took, and that same draft is sent; neither means nobody can
//!   tell, and the row is parked in `failed` for a human, as SMTP parks it.
//!
//! A live send whose delivering request times out, or is answered with a 5xx,
//! leaves the marker set and the row `pending_send` for that same
//! reconciliation, rather than parking it at once: `sendMail` and `/send`
//! answer 202 and finish later, so a 5xx can follow a send Exchange took, and
//! only a 4xx proves nothing went out ([`crate::graph`]).
//!
//! A JSON draft gets an Exchange `Message-ID`, so it carries ours in
//! [`crate::graph::MESSAGE_ID_PROPERTY`], which the lookups read as well.
//!
//! A resume that records the message as sent also retires the draft file it
//! was built from, as the live send does, so `mp send-approved` cannot send
//! it a second time ([`super::retire_draft_of_row`]).

use std::future::Future;

use anyhow::{anyhow, Context, Result};
use log::{debug, info, warn};

use crate::graph::{
    fits_mime_send, mime_send_body, DraftState, OutgoingAttachment, ServerCopies,
    MIME_SEND_MAX_BASE64_BYTES,
};
use crate::outbox::{self, OutboxRow, SubmitOutcome};
use crate::send::{graph_mime, BuiltMessage, RecipientRole};
use crate::store::{BlobStore, Store};

/// How long after its delivering request a Graph submission with no verdict
/// is given to show up in Sent Items before the resume decides about it.
///
/// `sendMail` and `send` both answer 202 and finish asynchronously, so a copy
/// missing right after a restart is not yet evidence of anything.
pub const SEND_SETTLE_SECS: i64 = 600;

/// How long a recorded draft found in the Outbox, a send Exchange has taken
/// and not finished, is waited for before the row is parked for a human.
pub const SENDING_SETTLE_SECS: i64 = 6 * SEND_SETTLE_SECS;

/// The Graph requests a submission is made of, and nothing else.
///
/// A trait so the staged send and its resume are tested against an in-memory
/// mailbox: every crash window is a dropped future at a chosen step, which no
/// live tenant reproduces offline.
pub trait GraphSendApi {
    /// `/me/sendMail` with the base64 MIME. Delivers.
    fn send_mime(&self, mime_base64: String) -> impl Future<Output = Result<()>> + Send;
    /// `POST /me/messages` with the base64 MIME; the new draft's Graph id.
    fn create_draft_mime(&self, mime_base64: String)
        -> impl Future<Output = Result<String>> + Send;
    /// `POST /me/messages` with a JSON message; the new draft's Graph id.
    fn create_draft_json(
        &self,
        message: &serde_json::Value,
    ) -> impl Future<Output = Result<String>> + Send;
    /// Add one file to a draft.
    fn add_attachment(
        &self,
        draft_id: &str,
        attachment: &OutgoingAttachment,
    ) -> impl Future<Output = Result<()>> + Send;
    /// `POST /me/messages/{id}/send`. Delivers.
    fn send_draft(&self, draft_id: &str) -> impl Future<Output = Result<()>> + Send;
    /// Remove a draft for good; one already gone counts as removed.
    fn discard_draft(&self, draft_id: &str) -> impl Future<Output = Result<()>> + Send;
    /// What the mailbox holds under one `Message-ID`.
    fn server_copies(
        &self,
        internet_message_id: &str,
    ) -> impl Future<Output = Result<ServerCopies>> + Send;
    /// Where the draft with this Graph id is now.
    fn draft_state(&self, draft_id: &str) -> impl Future<Output = Result<DraftState>> + Send;
}

impl GraphSendApi for crate::graph::GraphClient {
    fn send_mime(&self, mime_base64: String) -> impl Future<Output = Result<()>> + Send {
        crate::graph::GraphClient::send_mime(self, mime_base64)
    }
    fn create_draft_mime(
        &self,
        mime_base64: String,
    ) -> impl Future<Output = Result<String>> + Send {
        crate::graph::GraphClient::create_draft_mime(self, mime_base64)
    }
    fn create_draft_json(
        &self,
        message: &serde_json::Value,
    ) -> impl Future<Output = Result<String>> + Send {
        crate::graph::GraphClient::create_draft_json(self, message)
    }
    fn add_attachment(
        &self,
        draft_id: &str,
        attachment: &OutgoingAttachment,
    ) -> impl Future<Output = Result<()>> + Send {
        crate::graph::GraphClient::add_attachment(self, draft_id, attachment)
    }
    fn send_draft(&self, draft_id: &str) -> impl Future<Output = Result<()>> + Send {
        crate::graph::GraphClient::send_draft(self, draft_id)
    }
    fn discard_draft(&self, draft_id: &str) -> impl Future<Output = Result<()>> + Send {
        crate::graph::GraphClient::discard_draft(self, draft_id)
    }
    fn server_copies(
        &self,
        internet_message_id: &str,
    ) -> impl Future<Output = Result<ServerCopies>> + Send {
        crate::graph::GraphClient::server_copies(self, internet_message_id)
    }
    fn draft_state(&self, draft_id: &str) -> impl Future<Output = Result<DraftState>> + Send {
        crate::graph::GraphClient::draft_state(self, draft_id)
    }
}

// ---------------------------------------------------------------------------
// The plan
// ---------------------------------------------------------------------------

/// What a staged send creates its draft from.
#[derive(Debug, Clone, PartialEq)]
pub enum DraftBody {
    /// The message's own MIME, base64, minus the attachments moved out: every
    /// header, threading included, as `sendMail` would have sent it.
    Mime(String),
    /// The JSON message resource, for a body too large for any MIME request.
    /// Carries no threading headers and gets an Exchange `Message-ID`; ours
    /// rides along in [`crate::graph::MESSAGE_ID_PROPERTY`].
    Json(serde_json::Value),
}

/// How one built message is submitted over Graph.
#[derive(Debug, Clone, PartialEq)]
pub enum GraphPlan {
    /// One `/me/sendMail` with the whole base64 MIME.
    Mime(String),
    /// A draft, its attachments added one request each, then `send`.
    Draft {
        body: DraftBody,
        attachments: Vec<OutgoingAttachment>,
    },
}

/// The base64 length of `n` bytes.
fn base64_len(n: usize) -> usize {
    n.div_ceil(3) * 4
}

/// One top-level attachment part of the built MIME: the byte range that
/// removes it (its boundary line through its last byte) and its file.
struct AttachmentPart {
    range: std::ops::Range<usize>,
    attachment: OutgoingAttachment,
}

/// Decide how a built message goes over Graph (PERSO-144, and its large-message
/// follow-up).
///
/// Deterministic in the bytes and the recipients, so a resume that has only
/// the outbox row plans exactly what the live send planned.
pub fn plan(built: &BuiltMessage) -> Result<GraphPlan> {
    let mime = graph_mime(built)?;
    let whole = mime_send_body(&mime);
    if fits_mime_send(whole.len()) {
        return Ok(GraphPlan::Mime(whole));
    }

    let parsed = mailparse::parse_mail(&mime).context("parsing the built message")?;
    let parts = attachment_parts(&mime, &parsed)?;

    // The largest attachments leave first, so as many as possible stay inside
    // the MIME and the draft needs as few extra requests as possible.
    let mut order: Vec<usize> = (0..parts.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(parts[i].range.len()));
    let mut moved = vec![false; parts.len()];
    let mut remaining = mime.len();
    for i in order {
        if fits_mime_send(base64_len(remaining)) {
            break;
        }
        moved[i] = true;
        remaining -= parts[i].range.len();
    }

    if fits_mime_send(base64_len(remaining)) {
        let mut stripped = Vec::with_capacity(remaining);
        let mut at = 0;
        let mut attachments = Vec::new();
        for (part, moved) in parts.into_iter().zip(moved) {
            if moved {
                stripped.extend_from_slice(&mime[at..part.range.start]);
                at = part.range.end;
                attachments.push(part.attachment);
            }
        }
        stripped.extend_from_slice(&mime[at..]);
        info!(
            "[send] {} is {} bytes of base64 MIME, over Graph's request limit of {}; \
             sending it as a draft with {} attachment(s) added separately",
            built.message_id,
            whole.len(),
            MIME_SEND_MAX_BASE64_BYTES,
            attachments.len(),
        );
        return Ok(GraphPlan::Draft {
            body: DraftBody::Mime(mime_send_body(&stripped)),
            attachments,
        });
    }

    // The body alone does not fit a MIME request. The JSON resource carries
    // the HTML part once instead of the plain and HTML alternatives in base64,
    // at the price of the threading headers.
    let message = json_message(built, &parsed)?;
    let size = serde_json::to_vec(&message)
        .context("serialising the Graph message")?
        .len();
    if size > MIME_SEND_MAX_BASE64_BYTES {
        return Err(anyhow!(
            "the message body is {size} bytes even without its attachments, over the {} \
             bytes one Graph request can carry; shorten it or send the content as an attachment",
            MIME_SEND_MAX_BASE64_BYTES
        ));
    }
    warn!(
        "[send] {}'s body alone is over Graph's MIME request limit; sending it as JSON, \
         which carries no In-Reply-To or References",
        built.message_id
    );
    Ok(GraphPlan::Draft {
        body: DraftBody::Json(message),
        attachments: parts.into_iter().map(|p| p.attachment).collect(),
    })
}

/// The top-level attachment parts of a `multipart/mixed` message, in order.
fn attachment_parts(raw: &[u8], parsed: &mailparse::ParsedMail<'_>) -> Result<Vec<AttachmentPart>> {
    let Some(boundary) = parsed
        .ctype
        .mimetype
        .starts_with("multipart/")
        .then(|| parsed.ctype.params.get("boundary"))
        .flatten()
    else {
        return Ok(Vec::new());
    };
    let delimiter = format!("--{boundary}");
    let mut out = Vec::new();
    for part in &parsed.subparts {
        if part.get_content_disposition().disposition != mailparse::DispositionType::Attachment {
            continue;
        }
        let start = part.raw_bytes.as_ptr() as usize - raw.as_ptr() as usize;
        let end = start + part.raw_bytes.len();
        let line = raw[..start]
            .windows(delimiter.len())
            .rposition(|w| w == delimiter.as_bytes())
            .context("an attachment part with no boundary line before it")?;
        if !raw[line + delimiter.len()..start]
            .iter()
            .all(u8::is_ascii_whitespace)
        {
            return Err(anyhow!(
                "an attachment part not opened by its boundary line"
            ));
        }
        out.push(AttachmentPart {
            range: line..end,
            attachment: outgoing_attachment(part)?,
        });
    }
    Ok(out)
}

/// One MIME attachment part as the file Graph attaches.
fn outgoing_attachment(part: &mailparse::ParsedMail<'_>) -> Result<OutgoingAttachment> {
    use mailparse::MailHeaderMap;
    let name = part
        .get_content_disposition()
        .params
        .get("filename")
        .or_else(|| part.ctype.params.get("name"))
        .cloned()
        .unwrap_or_else(|| "attachment".to_string());
    let mut bytes = part
        .get_body_raw()
        .with_context(|| format!("decoding attachment {name}"))?;
    // mailparse ends a part with the CRLF that opens the next boundary line;
    // it belongs to the delimiter (RFC 2046 5.1.1), and only an identity
    // encoding hands it back as content.
    let encoded = part
        .headers
        .get_first_value("Content-Transfer-Encoding")
        .is_some_and(|cte| {
            let cte = cte.trim();
            cte.eq_ignore_ascii_case("base64") || cte.eq_ignore_ascii_case("quoted-printable")
        });
    if !encoded && part.raw_bytes.ends_with(b"\r\n") && bytes.ends_with(b"\r\n") {
        bytes.truncate(bytes.len() - 2);
    }
    Ok(OutgoingAttachment {
        name,
        content_type: part.ctype.mimetype.clone(),
        bytes,
    })
}

/// The JSON message resource for a body too large for MIME: subject, the HTML
/// (or plain) body, the recipients by role, and our `Message-ID` in
/// [`crate::graph::MESSAGE_ID_PROPERTY`] so the resume can find the draft.
fn json_message(
    built: &BuiltMessage,
    parsed: &mailparse::ParsedMail<'_>,
) -> Result<serde_json::Value> {
    use mailparse::MailHeaderMap;
    fn body_part<'a>(
        part: &'a mailparse::ParsedMail<'a>,
        mimetype: &str,
    ) -> Option<&'a mailparse::ParsedMail<'a>> {
        if part.get_content_disposition().disposition == mailparse::DispositionType::Attachment {
            return None;
        }
        if part.ctype.mimetype.eq_ignore_ascii_case(mimetype) {
            return Some(part);
        }
        part.subparts
            .iter()
            .find_map(|sub| body_part(sub, mimetype))
    }
    let (content_type, part) = match body_part(parsed, "text/html") {
        Some(part) => ("HTML", part),
        None => (
            "Text",
            body_part(parsed, "text/plain").context("the message has no text body")?,
        ),
    };
    let content = part.get_body().context("decoding the message body")?;
    let recipients = |role: RecipientRole| -> Vec<serde_json::Value> {
        built
            .recipients
            .iter()
            .filter(|(_, r)| *r == role)
            .map(|(addr, _)| {
                let (name, address) = crate::send::parse_name_address(addr);
                serde_json::json!({"emailAddress": {"name": name, "address": address}})
            })
            .collect()
    };
    let mut message = serde_json::json!({
        "subject": parsed.headers.get_first_value("Subject").unwrap_or_default(),
        "body": {"contentType": content_type, "content": content},
        "toRecipients": recipients(RecipientRole::To),
        "singleValueExtendedProperties": crate::graph::message_id_property(&built.message_id),
    });
    let cc = recipients(RecipientRole::Cc);
    if !cc.is_empty() {
        message["ccRecipients"] = serde_json::Value::Array(cc);
    }
    let bcc = recipients(RecipientRole::Bcc);
    if !bcc.is_empty() {
        message["bccRecipients"] = serde_json::Value::Array(bcc);
    }
    Ok(message)
}

// ---------------------------------------------------------------------------
// Submitting
// ---------------------------------------------------------------------------

/// What one Graph submission did, from the outbox's point of view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphOutcome {
    /// Graph accepted the delivering request.
    Delivered,
    /// Nothing can have been delivered: the failure came before the
    /// delivering request, or that request was refused outright. The row
    /// stays submittable, under backoff.
    Clean(String),
    /// The delivering request went out and no answer came back. The marker
    /// stays set and [`resume_rows`] settles the row against the mailbox.
    Unconfirmed(String),
}

impl GraphOutcome {
    /// The transition this outcome commits, `None` for the one that commits
    /// nothing.
    pub fn submit_outcome(&self) -> Option<SubmitOutcome> {
        match self {
            GraphOutcome::Delivered => Some(SubmitOutcome::Accepted),
            GraphOutcome::Clean(e) => Some(SubmitOutcome::CleanPreSubmission(e.clone())),
            GraphOutcome::Unconfirmed(_) => None,
        }
    }

    /// The error text, when there is one.
    pub fn error(&self) -> Option<&str> {
        match self {
            GraphOutcome::Delivered => None,
            GraphOutcome::Clean(e) | GraphOutcome::Unconfirmed(e) => Some(e),
        }
    }
}

/// The outbox row a submission writes its progress to: the store and the id.
/// `None` when the store could not be opened, which costs the durability of
/// this one send and nothing else.
pub type RowRef<'a> = Option<(&'a Store, i64)>;

fn mark_started(row: RowRef<'_>) {
    if let Some((store, id)) = row {
        if let Err(e) = outbox::mark_submission_started(store, id) {
            log::error!("[outbox] could not mark row {id} as entering submission: {e:#}");
        }
    }
}

fn note_draft(row: RowRef<'_>, draft: Option<&str>) {
    if let Some((store, id)) = row {
        if let Err(e) = outbox::set_graph_draft(store, id, draft) {
            // The draft carries our `Message-ID` when it was made from MIME,
            // so the resume still finds it by that.
            warn!("[outbox] could not record the Graph draft of row {id}: {e:#}");
        }
    }
}

/// Discard a draft this submission will not send, and forget it on the row.
/// A failure leaves it recorded, so the next resume discards it.
async fn discard<A: GraphSendApi>(row: RowRef<'_>, api: &A, draft_id: &str) {
    match api.discard_draft(draft_id).await {
        Ok(()) => note_draft(row, None),
        Err(e) => warn!("[send] could not discard the unsent Graph draft {draft_id}: {e:#}"),
    }
}

/// Read the error of a delivering request.
fn delivering(result: Result<()>) -> GraphOutcome {
    match result {
        Ok(()) => GraphOutcome::Delivered,
        Err(e) if e.chain().any(|c| c.is::<outbox::AmbiguousSubmission>()) => {
            GraphOutcome::Unconfirmed(format!("{e:#}"))
        }
        Err(e) => GraphOutcome::Clean(format!("{e:#}")),
    }
}

/// Run one submission along its plan, writing its progress to the row.
///
/// Records no verdict itself: the caller commits [`GraphOutcome::submit_outcome`]
/// (the live send through its `DurableSend`, the resume directly).
pub async fn submit<A: GraphSendApi>(row: RowRef<'_>, plan: GraphPlan, api: &A) -> GraphOutcome {
    let (body, attachments) = match plan {
        GraphPlan::Mime(body) => {
            mark_started(row);
            return delivering(api.send_mime(body).await);
        }
        GraphPlan::Draft { body, attachments } => (body, attachments),
    };

    // Creating and filling a draft delivers nothing, so none of it is behind
    // the marker, and any failure here is a clean one. A draft a failed or
    // interrupted create left behind carries our `Message-ID`, which is how
    // the resume finds and discards it.
    let created = match &body {
        DraftBody::Mime(mime) => api.create_draft_mime(mime.clone()).await,
        DraftBody::Json(message) => api.create_draft_json(message).await,
    };
    let draft_id = match created {
        Ok(id) => id,
        Err(e) => return GraphOutcome::Clean(format!("{e:#}")),
    };
    note_draft(row, Some(&draft_id));

    for attachment in &attachments {
        if let Err(e) = api.add_attachment(&draft_id, attachment).await {
            discard(row, api, &draft_id).await;
            return GraphOutcome::Clean(format!("{e:#}"));
        }
    }

    mark_started(row);
    let outcome = delivering(api.send_draft(&draft_id).await);
    if let GraphOutcome::Clean(_) = outcome {
        // Refused outright, so the draft is still a draft and nobody got it.
        discard(row, api, &draft_id).await;
    }
    outcome
}

// ---------------------------------------------------------------------------
// Resuming
// ---------------------------------------------------------------------------

/// Commit a resume's outcome on the row.
fn record(store: &Store, blobs: &BlobStore, id: i64, outcome: &GraphOutcome) -> Result<()> {
    if let Some(transition) = outcome.submit_outcome() {
        outbox::record_submission(store, blobs, id, &transition)?;
    }
    Ok(())
}

/// Settle every `pending_send` row of a Graph account (#0063 item 3).
///
/// Never returns an error: it runs on startup and on the sync tick, where a
/// mailbox that cannot be reached must not fail the caller, and a row whose
/// lookup fails is simply left for the next pass. A row whose draft a send in
/// this process is still working on is left alone: its marker is not yet set
/// while the draft is being filled, and resubmitting it would send it twice.
///
/// A row this pass records as sent has its draft file in `drafts` retired
/// while its send slot is still held ([`super::retire_draft_of_row`]).
pub async fn resume_rows<A: GraphSendApi>(
    store: &Store,
    blobs: &BlobStore,
    account: &str,
    drafts: &std::path::Path,
    now: i64,
    api: &A,
) {
    let rows = match outbox::pending_send_rows(store, account) {
        Ok(rows) => rows,
        Err(e) => {
            warn!("[outbox] could not read the pending sends for {account}: {e:#}");
            return;
        }
    };
    for row in rows {
        let Some(_claim) = super::SendAdmission::claim_row(account, &row) else {
            debug!(
                "[outbox] row {} is being sent right now; leaving it",
                row.id
            );
            continue;
        };
        let result = match row.submission_started_at {
            None => {
                if row.updated + outbox::backoff_secs(row.attempts) > now {
                    continue;
                }
                resubmit(store, blobs, &row, api).await
            }
            Some(started) => settle_unconfirmed(store, blobs, &row, started, now, api).await,
        };
        if let Err(e) = result {
            warn!("[outbox] could not resume row {} over Graph: {e:#}", row.id);
        }
        super::retire_draft_of_row(drafts, store, row.id);
    }
}

/// Remove from the server the drafts an outbox row left behind, before the
/// row is discarded (#0063 review): every draft in Drafts under the row's
/// `Message-ID`, and the recorded draft when it is still a draft in Drafts.
///
/// The recorded id is read before it is deleted, because an item that has
/// left Drafts may be the message itself on its way out or already sent.
/// Fails on the first draft that will not go, so the caller keeps the row
/// that names it.
pub async fn discard_row_drafts<A: GraphSendApi>(api: &A, row: &OutboxRow) -> Result<()> {
    let mut drafts = api.server_copies(&row.message_id).await?.drafts;
    if let Some(id) = row.envelope.as_ref().and_then(|e| e.graph_draft.clone()) {
        if !drafts.contains(&id) && api.draft_state(&id).await? == DraftState::InDrafts {
            drafts.push(id);
        }
    }
    for id in &drafts {
        api.discard_draft(id)
            .await
            .with_context(|| format!("discarding the Graph draft {id}"))?;
    }
    if !drafts.is_empty() {
        info!(
            "[outbox] discarded {} Graph draft(s) of row {}",
            drafts.len(),
            row.id
        );
    }
    Ok(())
}

/// Discard drafts, best effort, logging what will not go.
async fn discard_all<A: GraphSendApi>(api: &A, drafts: &[String]) {
    for id in drafts {
        if let Err(e) = api.discard_draft(id).await {
            warn!("[outbox] could not discard the leftover Graph draft {id}: {e:#}");
        }
    }
}

/// A row with no marker: nothing went out. Clear what an interrupted attempt
/// left in Drafts, then submit from the row's bytes.
async fn resubmit<A: GraphSendApi>(
    store: &Store,
    blobs: &BlobStore,
    row: &OutboxRow,
    api: &A,
) -> Result<()> {
    let Some(envelope) = row.envelope.clone().filter(|e| e.is_submittable()) else {
        outbox::record_submission(
            store,
            blobs,
            row.id,
            &SubmitOutcome::Ambiguous(
                "the queued submission has no usable envelope; it cannot be resent \
                 automatically"
                    .to_string(),
            ),
        )?;
        return Ok(());
    };

    let copies = api.server_copies(&row.message_id).await?;
    if copies.sent {
        // An operator re-armed a row whose message did arrive after all.
        info!(
            "[outbox] row {} ({}) is already in Sent Items; recorded as sent, not sent again",
            row.id, row.message_id
        );
        discard_all(api, &copies.drafts).await;
        outbox::record_submission(store, blobs, row.id, &SubmitOutcome::Accepted)?;
        return Ok(());
    }

    let mut stale = copies.drafts;
    if let Some(id) = &envelope.graph_draft {
        if !stale.contains(id) {
            stale.push(id.clone());
        }
    }
    for id in &stale {
        // A draft that will not go away keeps the row waiting: creating
        // another would leave two.
        api.discard_draft(id)
            .await
            .with_context(|| format!("discarding the leftover draft {id}"))?;
    }
    if !stale.is_empty() {
        info!(
            "[outbox] discarded {} unsent Graph draft(s) of row {}",
            stale.len(),
            row.id
        );
    }
    outbox::set_graph_draft(store, row.id, None)?;

    let raw = blobs
        .read(&row.raw_blob)
        .with_context(|| format!("reading the queued message of outbox row {}", row.id))?;
    let built = BuiltMessage {
        raw,
        message_id: row.message_id.clone(),
        recipients: envelope.recipients.clone(),
        from: envelope.from.clone(),
        draft_key: envelope.draft_key.clone(),
    };
    let plan = match plan(&built) {
        Ok(plan) => plan,
        Err(e) => {
            // Nothing a later pass can change: the bytes are what they are.
            outbox::record_submission(
                store,
                blobs,
                row.id,
                &SubmitOutcome::Ambiguous(format!("cannot be sent over Graph: {e:#}")),
            )?;
            return Ok(());
        }
    };
    info!(
        "[outbox] resubmitting row {} ({}) over Graph",
        row.id, row.message_id
    );
    let outcome = submit(Some((store, row.id)), plan, api).await;
    record(store, blobs, row.id, &outcome)
}

/// A row with a marker: the delivering request went out and its answer was
/// lost. Ask the mailbox.
async fn settle_unconfirmed<A: GraphSendApi>(
    store: &Store,
    blobs: &BlobStore,
    row: &OutboxRow,
    started: i64,
    now: i64,
    api: &A,
) -> Result<()> {
    let copies = api.server_copies(&row.message_id).await?;
    if copies.sent {
        info!(
            "[outbox] row {} ({}) is in Sent Items; its unconfirmed send went out",
            row.id, row.message_id
        );
        discard_all(api, &copies.drafts).await;
        outbox::record_submission(store, blobs, row.id, &SubmitOutcome::Accepted)?;
        return Ok(());
    }
    if now - started < SEND_SETTLE_SECS {
        debug!(
            "[outbox] row {} is not in Sent Items yet; asking again on a later pass",
            row.id
        );
        return Ok(());
    }

    // The recorded draft is read by its id, which works on both routes: a
    // JSON draft is not under our `Message-ID`, and one the lookup by property
    // missed is still this item.
    let recorded = row.envelope.as_ref().and_then(|e| e.graph_draft.clone());
    let state = match &recorded {
        Some(id) => Some(api.draft_state(id).await?),
        None => None,
    };
    let others: Vec<String> = copies
        .drafts
        .iter()
        .filter(|id| Some(*id) != recorded.as_ref())
        .cloned()
        .collect();
    match (recorded, state) {
        (_, Some(DraftState::Sent)) => {
            info!(
                "[outbox] row {} ({}): its draft is no longer a draft; the send went out",
                row.id, row.message_id
            );
            discard_all(api, &others).await;
            outbox::record_submission(store, blobs, row.id, &SubmitOutcome::Accepted)?;
            return Ok(());
        }
        (_, Some(DraftState::Sending)) if now - started < SENDING_SETTLE_SECS => {
            debug!(
                "[outbox] row {} is in the Outbox; asking again on a later pass",
                row.id
            );
            return Ok(());
        }
        (Some(draft_id), Some(DraftState::InDrafts)) => {
            // A draft that was sent leaves Drafts; this one is still there
            // long after its send, so the send never took. Sending the same
            // item, not a new one, is what keeps a late-arriving first send
            // from becoming a second message.
            discard_all(api, &others).await;
            info!(
                "[outbox] row {} ({}) is still in Drafts {}s after its send; sending that draft",
                row.id,
                row.message_id,
                now - started
            );
            outbox::mark_submission_started(store, row.id)?;
            let outcome = delivering(api.send_draft(&draft_id).await);
            if let GraphOutcome::Clean(_) = outcome {
                discard(Some((store, row.id)), api, &draft_id).await;
            }
            return record(store, blobs, row.id, &outcome);
        }
        _ => {}
    }

    // Neither sent nor waiting in Drafts: it may be in transit, or lost.
    // Nothing automatic is safe, so a human decides, as for SMTP.
    discard_all(api, &others).await;
    outbox::record_submission(
        store,
        blobs,
        row.id,
        &SubmitOutcome::Ambiguous(format!(
            "Graph never confirmed the send, and {}s later the message is neither in Sent \
             Items nor in Drafts; check Sent Items, then retry or discard the row",
            now - started
        )),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests;
