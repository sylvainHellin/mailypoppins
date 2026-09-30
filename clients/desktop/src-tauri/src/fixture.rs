//! Fixture mode: a daemon stand-in in this process, for the frontend and the
//! reader tests, with no daemon and no mail server.
//!
//! `MP_DESKTOP_FIXTURE=1` (or `--fixture`) swaps the session's door for this.
//! It answers the same wire methods with the same shapes the daemon does, from
//! `clients/desktop/fixtures/*.json` compiled in, so every command, the event
//! pump and the `mpmsg` scheme run their real code over it: a refusal is the
//! text `mp_client::session` produces for a daemon refusal, and an event is an
//! [`Incoming`] on the same channel type the session thread posts to.
//!
//! [`Fixture::simulate`] drives the connection states a real daemon produces
//! (disconnect, reconnect, restart, resync, new mail, shutdown) so the
//! frontend's connection and resync screens are testable, plus a rolled-back
//! drain (`rollback`) and a send hold another client armed (`hold`).
//!
//! The five message mutations change the rows in memory and publish what the
//! daemon publishes for them: nothing with the answer, then, once the
//! account's mutations have been quiet for [`DRAIN_DELAY`], one
//! `state.invalidate` per mailbox whose counts moved (the drainer, #0133).
//! Every mutation is journaled, so `rollback` can put the rows back the way a
//! server refusal does.
//!
//! The HTML bodies get the daemon's CSP meta tag prepended like a rendition
//! does, but their `<meta http-equiv="refresh">` is deliberately **not**
//! stripped: the hostile fixture exercises the reader's own defences.
//!
//! Drafts are real Markdown-with-frontmatter files in a per-run directory,
//! `<temp>/mp-desktop-fixture-<pid>/drafts/<account>/`, written through the
//! same `mp_core::draft` builders the daemon uses; `drafts.json` (whose rows
//! the frontend's mock reads as they are) and `draft-bodies.json` seed them.
//! Every call rescans that directory, as the daemon's `draft.*` queries do,
//! so a file changed behind the fixture's back (a client-side recipient
//! rewrite, a simulated editor save) is what the next answer reads. The
//! fixture names every file it builds `<id>.md` (a created draft keeps its
//! given name), so a draft whose frontmatter breaks is still addressed by
//! its id, the file stem the daemon's watcher falls back to.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};

use mp_client::events::Incoming;
use mp_core::draft::{DraftRecipientEdit, SourceMessage};
use mp_core::selector::{message_key, Selector};
use mp_protocol::draft::{
    DraftCreated, DraftEntry, DraftKind, DraftListing, DraftLocation, DraftMessage, DraftPreview,
    DraftReport, DraftSkip, DraftSource, DraftValidation,
};
use mp_protocol::events::{
    Diagnostic, DraftInvalid, KIND_DRAFT_CHANGED, KIND_DRAFT_INVALID, KIND_MUTATIONS_ROLLED_BACK,
    KIND_OPERATION_FINISHED, KIND_SEND_HOLD_CANCELLED, KIND_SEND_HOLD_FIRED,
    KIND_SEND_HOLD_STARTED, KIND_SEND_HOLD_TICK, KIND_SYNC_COMPLETED,
};
use mp_protocol::send::{HoldListing, HoldStatus};
use mp_protocol::state::Bootstrap;
use mp_protocol::EventEnvelope;

use crate::reader::MESSAGE_CSP;

const BOOTSTRAP: &str = include_str!("../../fixtures/bootstrap.json");
const ACCOUNTS: &str = include_str!("../../fixtures/accounts.json");
const MESSAGES: &str = include_str!("../../fixtures/messages.json");
const DRAFTS: &str = include_str!("../../fixtures/drafts.json");
const HTML: &str = include_str!("../../fixtures/html.json");
const DRAFT_BODIES: &str = include_str!("../../fixtures/draft-bodies.json");
const SIGNATURES: &str = include_str!("../../fixtures/signatures.json");

/// The address every fixture account sends from.
const FIXTURE_FROM: &str = "Me <me@example.com>";

/// The line `editor_save` appends.
pub const EDITOR_SAVE_LINE: &str = "A line the fixture's editor added.";

/// The default gap between two server-search hits.
const HIT_DELAY: Duration = Duration::from_millis(150);

/// How long an account's mutations stay quiet before the drain runs, the
/// daemon's `DRAIN_DEBOUNCE`.
pub const DRAIN_DELAY: Duration = Duration::from_millis(1500);

/// How long a sync pass runs before it completes.
const SYNC_DELAY: Duration = Duration::from_millis(800);

/// The window a `hold` simulation arms, in seconds.
pub const SIMULATED_HOLD_SECS: u64 = 10;

/// The mailbox `message.archive` moves into, the daemon's `ARCHIVE_MAILBOX`.
const ARCHIVE_MAILBOX: &str = "archive";

/// Keys a fixture row carries that a `message.list` row does not.
const FIXTURE_ONLY_KEYS: &[&str] = &["body", "attachments"];

/// What [`Fixture::simulate`] can do.
pub const SIMULATIONS: &[&str] = &[
    "disconnect",
    "reconnect",
    "restart",
    "resync",
    "new_mail",
    "shutdown",
    "rollback",
    "hold",
    "editor_save",
    "editor_invalid",
];

/// One `editor_open` the fixture stubbed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditorOpen {
    pub path: String,
    /// The argument vector the editor would have run with.
    pub command: Vec<String>,
}

/// One row as it was before a mutation changed it.
struct Journaled {
    account: String,
    mailbox: String,
    index: usize,
    row: Value,
}

/// One armed send hold.
struct Hold {
    status: HoldStatus,
    deadline: Instant,
}

struct State {
    bootstrap: Value,
    accounts: Value,
    /// account -> mailbox slug -> rows, newest first.
    messages: BTreeMap<String, BTreeMap<String, Vec<Value>>>,
    /// account -> the listing of the last rescan of its drafts directory.
    drafts: BTreeMap<String, DraftListing>,
    /// The per-run directory the draft files live under.
    root: PathBuf,
    signatures: Value,
    /// Every stubbed `editor_open`, oldest first.
    editor_opens: Vec<EditorOpen>,
    next_draft: u64,
    html: BTreeMap<i64, String>,
    instance: u32,
    revision: u64,
    down: bool,
    next_op: u64,
    next_row: i64,
    operations: BTreeMap<String, Value>,
    /// The gap before each server-search hit, so a user sees them stream.
    hit_delay: Duration,
    /// Every row change a mutation made, oldest first.
    journal: Vec<Journaled>,
    /// account -> mailbox -> the `(total, unread)` clients were last told.
    published: BTreeMap<String, BTreeMap<String, (u64, u64)>>,
    /// account -> the number of the latest drain request.
    drain_requests: BTreeMap<String, u64>,
    drain_delay: Duration,
    sync_delay: Duration,
    /// The armed holds, in arm order.
    holds: Vec<Hold>,
    /// How long one second of a hold lasts; a test shortens it.
    hold_second: Duration,
}

impl State {
    fn instance_id(&self) -> String {
        format!("fixture-instance-{}", self.instance)
    }

    fn row(&self, account: &str, row_id: i64) -> Option<(&str, &Value)> {
        self.messages
            .get(account)?
            .iter()
            .find_map(|(mailbox, rows)| {
                rows.iter()
                    .find(|r| r["id"].as_i64() == Some(row_id))
                    .map(|r| (mailbox.as_str(), r))
            })
    }

    fn mailbox_rows(&self, account: &str) -> Vec<Value> {
        let seeds = self.bootstrap["snapshot"]["mailboxes"][account]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let drafts = self
            .drafts
            .get(account)
            .map_or(0, |d| d.drafts.len() as u64);
        seeds
            .into_iter()
            .map(|mut seed| {
                let slug = seed["slug"].as_str().unwrap_or_default().to_string();
                let (total, unread) = if slug == "drafts" {
                    (drafts, 0)
                } else {
                    let rows = self
                        .messages
                        .get(account)
                        .and_then(|m| m.get(&slug))
                        .map_or(&[][..], Vec::as_slice);
                    let unread = rows.iter().filter(|r| r["flags"]["seen"] != true).count();
                    (rows.len() as u64, unread as u64)
                };
                seed["total"] = json!(total);
                seed["unread"] = json!(unread);
                seed["badge"] = json!(total);
                seed
            })
            .collect()
    }

    fn bootstrap(&self) -> Value {
        let mut b = self.bootstrap.clone();
        b["instance_id"] = json!(self.instance_id());
        b["revision"] = json!(self.revision);
        let accounts: Vec<String> = self.messages.keys().cloned().collect();
        for account in accounts {
            b["snapshot"]["mailboxes"][&account] = Value::Array(self.mailbox_rows(&account));
        }
        b["snapshot"]["holds"] = json!(self.hold_listing(None).holds);
        for (account, listing) in &self.drafts {
            let mut rows: Vec<Value> = listing.drafts.iter().map(snapshot_row).collect();
            rows.extend(listing.skipped.iter().map(|skip| {
                json!({
                    "id": stem_of(&skip.path), "path": skip.path, "to": null, "subject": "",
                    "status": "invalid", "valid": false, "ready": false
                })
            }));
            b["snapshot"]["drafts"][account] = Value::Array(rows);
        }
        b
    }

    /// The store counts of every mailbox of `account`, Drafts aside.
    fn counts(&self, account: &str) -> BTreeMap<String, (u64, u64)> {
        self.messages
            .get(account)
            .map(|boxes| {
                boxes
                    .iter()
                    .map(|(mailbox, rows)| {
                        let unread = rows.iter().filter(|r| r["flags"]["seen"] != true).count();
                        (mailbox.clone(), (rows.len() as u64, unread as u64))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Where `row_id` sits in `account`: its mailbox and its index there.
    fn locate(&self, account: &str, row_id: i64) -> Option<(String, usize)> {
        self.messages
            .get(account)?
            .iter()
            .find_map(|(mailbox, rows)| {
                rows.iter()
                    .position(|r| r["id"].as_i64() == Some(row_id))
                    .map(|i| (mailbox.clone(), i))
            })
    }

    /// A hold as a client reads it now.
    fn hold_at(&self, hold: &Hold, now: Instant) -> HoldStatus {
        HoldStatus {
            remaining_secs: remaining(hold.deadline, now, self.hold_second),
            ..hold.status.clone()
        }
    }

    fn hold_listing(&self, account: Option<&str>) -> HoldListing {
        let now = Instant::now();
        HoldListing {
            holds: self
                .holds
                .iter()
                .filter(|h| account.is_none_or(|a| h.status.account == a))
                .map(|h| self.hold_at(h, now))
                .collect(),
        }
    }

    /// Take the hold `id` names out of the table, its window emptied.
    fn take_hold(&mut self, id: &str) -> Option<HoldStatus> {
        let at = self
            .holds
            .iter()
            .position(|h| h.status.operation_id == id)?;
        let hold = self.holds.remove(at);
        Some(HoldStatus {
            remaining_secs: 0,
            ..hold.status
        })
    }

    /// Arm one hold `secs` long and answer its status.
    fn arm_hold(&mut self, mut status: HoldStatus, secs: u64) -> HoldStatus {
        let second = self.hold_second;
        status.hold_secs = status.hold_secs.max(secs);
        status.remaining_secs = secs;
        status.fires_at = rfc3339_in(Duration::from_secs(secs));
        self.holds.push(Hold {
            status: status.clone(),
            deadline: Instant::now() + second * secs as u32,
        });
        status
    }

    fn next_operation_id(&mut self, prefix: &str) -> String {
        let id = format!("{prefix}-{}", self.next_op);
        self.next_op += 1;
        id
    }

    fn account_known(&self, method: &str, account: &str) -> Result<()> {
        if self.messages.contains_key(account) {
            Ok(())
        } else {
            Err(refused(
                method,
                -32005,
                &format!("account_unknown: {account}"),
            ))
        }
    }

    /// A mailbox a move may land in, by slug or label, the way the daemon's
    /// `resolve_mailbox` resolves one: never Drafts.
    fn destination(&self, method: &str, account: &str, wanted: &str) -> Result<String> {
        let rows = self.mailbox_rows(account);
        let candidates: Vec<(&str, &str)> = rows
            .iter()
            .filter_map(|r| Some((r["slug"].as_str()?, r["label"].as_str()?)))
            .filter(|(slug, _)| *slug != "drafts")
            .collect();
        if let Some((slug, _)) = candidates.iter().find(|(slug, label)| {
            wanted.eq_ignore_ascii_case(slug) || wanted.eq_ignore_ascii_case(label)
        }) {
            return Ok((*slug).to_string());
        }
        let known: Vec<&str> = candidates.iter().map(|(slug, _)| *slug).collect();
        Err(refused(
            method,
            -32602,
            &format!(
                "'{wanted}' is not a mailbox of {account} (known: {})",
                known.join(", ")
            ),
        ))
    }

    /// One of the five message mutations: the row change, journaled, and the
    /// answer the daemon's `mutate` gives.
    fn mutate(&mut self, method: &str, params: &Value) -> Result<Value> {
        let account = param_str(method, params, "account")?.to_string();
        self.account_known(method, &account)?;
        let destination = match method {
            "message.archive" => Some(ARCHIVE_MAILBOX.to_string()),
            "message.move" => {
                let wanted = param_str(method, params, "destination")?;
                Some(self.destination(method, &account, wanted)?)
            }
            _ => None,
        };
        let state_param = match method {
            "message.set_read" => Some("read"),
            "message.set_flag" => Some("flagged"),
            _ => None,
        };
        let new_state = match state_param {
            Some(name) => Some(params[name].as_bool().ok_or_else(|| {
                refused(
                    method,
                    -32602,
                    &format!(
                        "{name} is a required boolean: the state to set, not the state to toggle"
                    ),
                )
            })?),
            None => None,
        };
        if !matches!(
            params.get("settle"),
            None | Some(Value::Null | Value::Bool(_))
        ) {
            return Err(refused(method, -32602, "settle is a boolean"));
        }
        let row_id = match params.get("row_id") {
            None | Some(Value::Null) => {
                return Err(refused(
                    method,
                    -32602,
                    "a message is addressed by id, by row_id or by selector; send exactly one",
                ))
            }
            Some(v) => v.as_i64().ok_or_else(|| {
                refused(
                    method,
                    -32602,
                    "row_id is a messages.id, which is an integer",
                )
            })?,
        };
        let Some((mailbox, index)) = self.locate(&account, row_id) else {
            return Err(refused(
                method,
                -32602,
                &format!("{account} holds no message with row id {row_id}"),
            ));
        };
        let boxes = self
            .messages
            .get_mut(&account)
            .expect("the account was checked");
        let rows = boxes.get_mut(&mailbox).expect("the row was located");
        let before = rows[index].clone();
        let message_id = before["message_id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let mut answer = json!({
            "account": account,
            "id": format!("{mailbox}/{}", before["uid"]),
            "selector": before["selector"],
            "mailbox": mailbox,
        });
        match (method, destination, new_state) {
            (_, Some(destination), _) => {
                let mut row = rows.remove(index);
                // The row's own selector is `Selector::for_message`'s, the
                // bare Message-ID; `moved_to` carries the stored Message-ID,
                // brackets and all, which is what the daemon answers.
                row["selector"] =
                    json!(
                        Selector::new(&account, &destination, message_key(&message_id)).to_string()
                    );
                let selector = Selector::new(&account, &destination, &message_id).to_string();
                let target = boxes.entry(destination.clone()).or_default();
                let at = target
                    .iter()
                    .position(|r| r["date_sort"].as_str() < row["date_sort"].as_str())
                    .unwrap_or(target.len());
                target.insert(at, row);
                answer["moved_to"] = json!({"mailbox": destination, "selector": selector});
            }
            ("message.delete", None, _) => {
                rows.remove(index);
            }
            (_, None, Some(value)) => {
                let (flag, key) = if method == "message.set_read" {
                    ("seen", "read")
                } else {
                    ("flagged", "flagged")
                };
                rows[index]["flags"][flag] = json!(value);
                answer[key] = json!(value);
            }
            _ => return Err(refused(method, -32601, "method not found")),
        }
        self.journal.push(Journaled {
            account,
            mailbox,
            index,
            row: before,
        });
        Ok(answer)
    }

    /// Put the last `n` journaled rows back, newest first, and answer how
    /// many each account got back.
    fn roll_back(&mut self, n: usize) -> BTreeMap<String, u64> {
        let from = self.journal.len().saturating_sub(n);
        let undone: Vec<Journaled> = self.journal.drain(from..).rev().collect();
        let mut failed = BTreeMap::new();
        for entry in undone {
            let id = entry.row["id"].as_i64();
            if let Some(boxes) = self.messages.get_mut(&entry.account) {
                for rows in boxes.values_mut() {
                    rows.retain(|r| r["id"].as_i64() != id);
                }
                let rows = boxes.entry(entry.mailbox).or_default();
                let at = entry.index.min(rows.len());
                rows.insert(at, entry.row);
            }
            *failed.entry(entry.account).or_insert(0) += 1;
        }
        failed
    }

    /// The counts invalidations a drain of `account` owes: one per mailbox
    /// whose counts differ from what clients were last told.
    fn moved_counts(&mut self, account: &str) -> Vec<Value> {
        let fresh = self.counts(account);
        let told = self.published.entry(account.to_string()).or_default();
        let mut out = Vec::new();
        for (mailbox, pair) in fresh {
            if told.get(&mailbox) == Some(&pair) {
                continue;
            }
            told.insert(mailbox.clone(), pair);
            out.push(json!({
                "resource": format!("mailbox:{account}/{mailbox}"),
                "scope": {"query": "counts"}
            }));
        }
        out
    }
}

/// What a draft method answers and the watcher events it owes.
type Answered = (Value, Vec<(&'static str, Value)>);

/// The draft files: the `draft.*` family and `signature.list`.
impl State {
    fn drafts_dir(&self, account: &str) -> PathBuf {
        self.root.join("drafts").join(account)
    }

    /// Re-read every account's drafts directory into [`State::drafts`].
    fn rescan(&mut self) {
        let accounts: Vec<String> = self.messages.keys().cloned().collect();
        for account in accounts {
            let (drafts, skipped) = scan_drafts(&account, &self.drafts_dir(&account));
            let listing = DraftListing {
                account: account.clone(),
                drafts,
                skipped,
                collisions: Vec::new(),
            };
            self.drafts.insert(account, listing);
        }
    }

    /// The listed draft `id` names, or the daemon's `-32602`.
    fn draft(&self, method: &str, account: &str, id: &str) -> Result<&DraftEntry> {
        self.drafts
            .get(account)
            .and_then(|l| l.drafts.iter().find(|d| d.id == id))
            .ok_or_else(|| {
                refused(
                    method,
                    -32602,
                    &format!("no draft matches {}", Selector::for_draft(account, id)),
                )
            })
    }

    /// The draft `id` names, or, for a file that will not parse, the
    /// `-32010` refusal the daemon answers when the id is the file's stem.
    fn parseable(&self, method: &str, account: &str, id: &str) -> Result<DraftEntry> {
        if let Some(skip) = self
            .drafts
            .get(account)
            .and_then(|l| l.skipped.iter().find(|s| stem_of(&s.path) == id))
        {
            return Err(refused(method, -32010, &skip.error));
        }
        self.draft(method, account, id).cloned()
    }

    /// The watcher's event for the file at `path`, after a rescan.
    fn watch_events(&self, path: &Path) -> Vec<(&'static str, Value)> {
        let shown = path.display().to_string();
        let account = path
            .parent()
            .and_then(Path::file_name)
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let Some(listing) = self.drafts.get(&account) else {
            return Vec::new();
        };
        if let Some(entry) = listing.drafts.iter().find(|d| d.path == shown) {
            let mut payload = snapshot_row(entry);
            payload["account"] = json!(account);
            return vec![(KIND_DRAFT_CHANGED, payload)];
        }
        if let Some(skip) = listing.skipped.iter().find(|s| s.path == shown) {
            let payload = DraftInvalid {
                account,
                id: stem_of(&skip.path),
                path: skip.path.clone(),
                diagnostics: vec![Diagnostic {
                    line: None,
                    message: skip.error.clone(),
                }],
            };
            return vec![(
                KIND_DRAFT_INVALID,
                serde_json::to_value(payload).unwrap_or_default(),
            )];
        }
        Vec::new()
    }

    fn mint_id(&mut self) -> String {
        self.next_draft += 1;
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as u64);
        format!("{:016x}", nanos ^ (self.next_draft << 56))
    }

    /// The signature a written draft carries: the one named, none, or the
    /// account's default.
    fn signature_for(&self, account: &str, params: &Value) -> Option<String> {
        if params["no_signature"] == true {
            return None;
        }
        let name = params["signature"]
            .as_str()
            .or_else(|| self.signatures["defaults"][account].as_str())?;
        self.signatures["signatures"][name]
            .as_str()
            .map(str::to_string)
    }

    /// The received message `source` addresses, as a reply or a forward reads
    /// it. A forward carries the attachments, written as small files.
    fn source_of(
        &self,
        method: &str,
        account: &str,
        source: &Value,
        forward: bool,
    ) -> Result<(SourceMessage, DraftSource)> {
        let row_id = source["row_id"].as_i64().ok_or_else(|| {
            refused(
                method,
                -32602,
                "the fixture addresses a source by row_id only",
            )
        })?;
        let (mailbox, row) = self.row(account, row_id).ok_or_else(|| {
            refused(
                method,
                -32602,
                &format!("{account} holds no message with row id {row_id}"),
            )
        })?;
        let text = |key: &str| row[key].as_str().unwrap_or_default().to_string();
        let opt = |key: &str| row[key].as_str().map(str::to_string);
        let mut attachments = Vec::new();
        if forward {
            let dir = self
                .root
                .join("attachments")
                .join(account)
                .join(row_id.to_string());
            for part in row["attachments"].as_array().into_iter().flatten() {
                let name = part["name"].as_str().unwrap_or("part");
                fs::create_dir_all(&dir)?;
                let file = dir.join(name);
                fs::write(&file, format!("fixture attachment {name}\n"))?;
                attachments.push(file);
            }
        }
        let message = SourceMessage {
            from: text("from"),
            to: text("to"),
            cc: opt("cc"),
            subject: text("subject"),
            message_id: opt("message_id"),
            date: opt("date_display"),
            body: text("body").trim().to_string(),
            attachments,
            html: self.html.get(&row_id).cloned(),
            reply_to: opt("reply_to"),
        };
        let source = DraftSource {
            id: format!("{mailbox}/{}", row["uid"]),
            selector: text("selector"),
        };
        Ok((message, source))
    }

    /// A built file renamed to `<id>.md` with its id minted, and the headers
    /// override applied.
    fn finish_built(
        &mut self,
        built: &Path,
        headers: Option<&DraftRecipientEdit>,
    ) -> Result<(String, PathBuf)> {
        let id = self.mint_id();
        mp_core::draft::set_draft_id(built, &id)?;
        let path = built.with_file_name(format!("{id}.md"));
        fs::rename(built, &path)?;
        let html = built.with_extension("html");
        if html.exists() {
            fs::rename(&html, path.with_extension("html"))?;
        }
        if let Some(headers) = headers {
            mp_core::draft::rewrite_draft_recipients(&path, headers)?;
        }
        Ok((id, path))
    }

    /// The answer of the four methods that write a draft, and its watcher
    /// event.
    fn created(
        &mut self,
        account: &str,
        id: &str,
        path: &Path,
        source: Option<DraftSource>,
    ) -> Result<Answered> {
        self.rescan();
        let answer = DraftCreated {
            account: account.to_string(),
            id: id.to_string(),
            selector: Selector::for_draft(account, id).to_string(),
            path: path.display().to_string(),
            source,
        };
        Ok((serde_json::to_value(answer)?, self.watch_events(path)))
    }

    fn draft_method(&mut self, method: &str, params: &Value) -> Result<Answered> {
        let account = param_str(method, params, "account")?.to_string();
        self.account_known(method, &account)?;
        let dir = self.drafts_dir(&account);
        match method {
            "signature.list" => {
                let mut names: Vec<&String> = self.signatures["signatures"]
                    .as_object()
                    .map(|o| o.keys().collect())
                    .unwrap_or_default();
                names.sort();
                let answer = json!({
                    "account": account, "names": names,
                    "default": self.signatures["defaults"][&account]
                });
                Ok((answer, Vec::new()))
            }
            "draft.create" => {
                let name = param_str(method, params, "name")?;
                let file_name = match Path::new(name).extension() {
                    Some(_) => name.to_string(),
                    None => format!("{name}.md"),
                };
                fs::create_dir_all(&dir)?;
                let path = dir.join(file_name);
                if path.exists() {
                    return Err(refused(
                        method,
                        -32602,
                        &format!("A draft already exists at {}", path.display()),
                    ));
                }
                let id = self.mint_id();
                let skeleton = mp_core::draft::new_draft_skeleton_with_id(
                    FIXTURE_FROM,
                    &rfc3339_in(Duration::ZERO),
                    &id,
                    self.signature_for(&account, params).as_deref(),
                );
                fs::write(&path, skeleton)?;
                self.created(&account, &id, &path, None)
            }
            "draft.reply" | "draft.forward" => {
                let reply = method == "draft.reply";
                let headers = recipient_headers(method, params)?;
                let (message, source) =
                    self.source_of(method, &account, &params["source"], !reply)?;
                let signature = self.signature_for(&account, params);
                fs::create_dir_all(&dir)?;
                let built = if reply {
                    mp_core::draft::create_reply_draft_from(
                        &message,
                        params["all"] == true,
                        FIXTURE_FROM,
                        Some(&dir),
                        signature.as_deref(),
                    )?
                } else {
                    mp_core::draft::create_forward_draft_from(
                        &message,
                        FIXTURE_FROM,
                        Some(&dir),
                        signature.as_deref(),
                    )?
                };
                let (id, path) = self.finish_built(&built, headers.as_ref())?;
                self.created(&account, &id, &path, Some(source))
            }
            "draft.create_from_message" => {
                let kind: DraftKind =
                    serde_json::from_value(params["kind"].clone()).map_err(|_| {
                        refused(method, -32602, "kind is one of reply, reply_all or forward")
                    })?;
                let message: DraftMessage = match &params["message"] {
                    value @ Value::Object(_) => {
                        serde_json::from_value(value.clone()).map_err(|e| {
                            refused(
                                method,
                                -32602,
                                &format!("message is not a quotable message: {e}"),
                            )
                        })?
                    }
                    _ => {
                        return Err(refused(
                            method,
                            -32602,
                            "message is the object a reply quotes",
                        ))
                    }
                };
                let source = SourceMessage {
                    from: message.from,
                    to: message.to,
                    cc: message.cc,
                    subject: message.subject,
                    message_id: message.message_id,
                    date: Some(message.date_display),
                    body: message.body_text.trim().to_string(),
                    attachments: Vec::new(),
                    html: message.html_body,
                    reply_to: message.reply_to,
                };
                let signature = self.signature_for(&account, params);
                fs::create_dir_all(&dir)?;
                let built = match kind {
                    DraftKind::Reply | DraftKind::ReplyAll => {
                        mp_core::draft::create_reply_draft_from(
                            &source,
                            kind == DraftKind::ReplyAll,
                            FIXTURE_FROM,
                            Some(&dir),
                            signature.as_deref(),
                        )?
                    }
                    DraftKind::Forward => mp_core::draft::create_forward_draft_from(
                        &source,
                        FIXTURE_FROM,
                        Some(&dir),
                        signature.as_deref(),
                    )?,
                };
                let (id, path) = self.finish_built(&built, None)?;
                self.created(&account, &id, &path, None)
            }
            "draft.path" => {
                let id = param_str(method, params, "id")?;
                let entry = self.draft(method, &account, id)?;
                let answer = DraftLocation {
                    account: account.clone(),
                    id: entry.id.clone(),
                    selector: entry.selector.clone(),
                    path: entry.path.clone(),
                    status: entry.status.clone(),
                };
                Ok((serde_json::to_value(answer)?, Vec::new()))
            }
            "draft.approve" | "draft.demote" => {
                let approve = method == "draft.approve";
                let id = param_str(method, params, "id")?;
                let entry = self.parseable(method, &account, id)?;
                if entry.status == "sent" {
                    return Err(refused(
                        method,
                        -32602,
                        if approve {
                            "Cannot approve an already sent email"
                        } else {
                            "Cannot revert a sent email back to draft"
                        },
                    ));
                }
                let path = PathBuf::from(&entry.path);
                if approve {
                    mp_core::draft::mark_as_approved(&path)?;
                } else {
                    mp_core::draft::mark_as_draft(&path)?;
                }
                self.rescan();
                let answer = json!({
                    "account": account, "id": id,
                    "status": if approve { "approved" } else { "draft" },
                    "path": entry.path,
                });
                Ok((answer, self.watch_events(&path)))
            }
            "draft.validate" => {
                let rows: Vec<DraftEntry> = match params["id"].as_str() {
                    Some(id) => vec![self.draft(method, &account, id)?.clone()],
                    None => self
                        .drafts
                        .get(&account)
                        .map(|l| l.drafts.clone())
                        .unwrap_or_default(),
                };
                let answer = DraftValidation {
                    account: account.clone(),
                    reports: rows.iter().map(|row| report(&account, row)).collect(),
                };
                Ok((serde_json::to_value(answer)?, Vec::new()))
            }
            "draft.preview" => {
                let id = param_str(method, params, "id")?;
                let entry = self.parseable(method, &account, id)?;
                let draft = mp_core::draft::parse_email_draft(Path::new(&entry.path))
                    .map_err(|e| refused(method, -32010, &one_line(&e)))?;
                let outcome = mp_core::draft::validate_draft(&draft);
                let fm = &draft.frontmatter;
                let answer = DraftPreview {
                    account: account.clone(),
                    id: entry.id.clone(),
                    selector: entry.selector.clone(),
                    path: entry.path.clone(),
                    from: fm.from.clone().unwrap_or_else(|| FIXTURE_FROM.to_string()),
                    to: fm.to.clone(),
                    cc: fm.cc.clone(),
                    bcc: fm.bcc.clone(),
                    subject: fm.subject.clone(),
                    body: draft.body_markdown.chars().take(500).collect(),
                    body_truncated: draft.body_markdown.len() > 500,
                    status: fm.status.to_string(),
                    valid: outcome.is_ok(),
                    error: outcome.as_ref().err().map(|e| e.to_string()),
                    warnings: outcome.unwrap_or_default(),
                    font_family: "Calibri".into(),
                    font_size: "11pt".into(),
                    signature: None,
                };
                Ok((serde_json::to_value(answer)?, Vec::new()))
            }
            other => Err(refused(other, -32601, "method not found")),
        }
    }
}

/// The daemon stand-in.
pub struct Fixture {
    state: Mutex<State>,
    events: Mutex<Sender<Incoming>>,
    /// Every call, method and params, for a test to read what went out.
    #[cfg(test)]
    calls: Mutex<Vec<(String, Value)>>,
}

fn refused(method: &str, code: i32, message: &str) -> anyhow::Error {
    anyhow!("{method}: the daemon refused the call: {message} ({code})")
}

fn param_str<'a>(method: &str, params: &'a Value, key: &str) -> Result<&'a str> {
    params[key]
        .as_str()
        .ok_or_else(|| refused(method, -32602, &format!("missing string param `{key}`")))
}

/// Refuse a parameter outside `allowed`, as the daemon's `only` does.
fn only(method: &str, params: &Value, allowed: &[&str]) -> Result<()> {
    if let Some(extra) = params
        .as_object()
        .and_then(|o| o.keys().find(|k| !allowed.contains(&k.as_str())))
    {
        return Err(refused(
            method,
            -32602,
            &format!("{method} has no {extra} parameter"),
        ));
    }
    Ok(())
}

/// Whole hold seconds left before `deadline`, rounded up like the daemon's.
fn remaining(deadline: Instant, now: Instant, second: Duration) -> u64 {
    let left = deadline.saturating_duration_since(now);
    let nanos = second.as_nanos().max(1);
    left.as_nanos().div_ceil(nanos) as u64
}

/// The RFC3339 UTC instant `after` from now, to the second.
fn rfc3339_in(after: Duration) -> String {
    let secs = (SystemTime::now() + after)
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs()) as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

impl Fixture {
    /// Load the compiled-in fixtures; events go to `events`.
    pub fn load(events: Sender<Incoming>) -> Result<Fixture> {
        let bootstrap: Value =
            serde_json::from_str(BOOTSTRAP).context("fixtures/bootstrap.json")?;
        // Refuse a fixture the real decoder would refuse.
        serde_json::from_value::<Bootstrap>(bootstrap.clone())
            .context("fixtures/bootstrap.json does not decode as a Bootstrap")?;
        let accounts: Value = serde_json::from_str(ACCOUNTS).context("fixtures/accounts.json")?;
        let messages: BTreeMap<String, BTreeMap<String, Vec<Value>>> =
            serde_json::from_str(MESSAGES).context("fixtures/messages.json")?;
        let seeds: BTreeMap<String, DraftListing> =
            serde_json::from_str(DRAFTS).context("fixtures/drafts.json")?;
        let bodies: BTreeMap<String, String> =
            serde_json::from_str(DRAFT_BODIES).context("fixtures/draft-bodies.json")?;
        let signatures: Value =
            serde_json::from_str(SIGNATURES).context("fixtures/signatures.json")?;
        let root = fixture_root();
        seed_drafts(&root, &seeds, &bodies).context("writing the fixture drafts")?;
        let html_by_key: BTreeMap<String, String> =
            serde_json::from_str(HTML).context("fixtures/html.json")?;
        let mut html = BTreeMap::new();
        for (key, body) in html_by_key {
            let id: i64 = key.parse().context("fixtures/html.json keys are row ids")?;
            html.insert(id, body);
        }
        let revision = bootstrap["revision"].as_u64().unwrap_or(1);
        let next_row = messages
            .values()
            .flat_map(|m| m.values().flatten())
            .filter_map(|r| r["id"].as_i64())
            .max()
            .unwrap_or(1000)
            + 1;
        let seeds: Vec<HoldStatus> = serde_json::from_value(bootstrap["snapshot"]["holds"].clone())
            .context("fixtures/bootstrap.json's holds")?;
        let mut state = State {
            bootstrap,
            accounts,
            messages,
            drafts: BTreeMap::new(),
            root,
            signatures,
            editor_opens: Vec::new(),
            next_draft: 0,
            html,
            instance: 1,
            revision,
            down: false,
            next_op: 1,
            next_row,
            operations: BTreeMap::new(),
            hit_delay: HIT_DELAY,
            journal: Vec::new(),
            published: BTreeMap::new(),
            drain_requests: BTreeMap::new(),
            drain_delay: DRAIN_DELAY,
            sync_delay: SYNC_DELAY,
            holds: Vec::new(),
            hold_second: Duration::from_secs(1),
        };
        state.rescan();
        let names: Vec<String> = state.messages.keys().cloned().collect();
        for name in names {
            let counts = state.counts(&name);
            state.published.insert(name, counts);
        }
        // The seeded holds are armed now with the remainder the file gives
        // them; their countdown starts with [`Fixture::start_clock`].
        for seed in seeds {
            let secs = seed.remaining_secs;
            state.arm_hold(seed, secs);
        }
        Ok(Fixture {
            state: Mutex::new(state),
            events: Mutex::new(events),
            #[cfg(test)]
            calls: Mutex::new(Vec::new()),
        })
    }

    /// Start the countdown of the holds the bootstrap fixture seeds.
    ///
    /// Apart from [`Fixture::load`] so a test's event stream carries no tick
    /// it did not ask for; the app calls it once the fixture door is up.
    pub fn start_clock(self: &Arc<Self>) {
        let ids: Vec<String> = self
            .state()
            .holds
            .iter()
            .map(|h| h.status.operation_id.clone())
            .collect();
        for id in ids {
            self.spawn_hold(id);
        }
    }

    /// Drain after `delay` of quiet; a test sets zero.
    #[cfg(test)]
    pub fn set_drain_delay(&self, delay: Duration) {
        self.state().drain_delay = delay;
    }

    /// Complete a sync pass after `delay`; a test sets zero.
    #[cfg(test)]
    pub fn set_sync_delay(&self, delay: Duration) {
        self.state().sync_delay = delay;
    }

    /// Every call so far, method and params.
    #[cfg(test)]
    pub fn calls(&self) -> Vec<(String, Value)> {
        self.calls.lock().map(|c| c.clone()).unwrap_or_default()
    }

    /// How long one second of a hold lasts; a test shortens it.
    #[cfg(test)]
    pub fn set_hold_second(&self, second: Duration) {
        self.state().hold_second = second;
    }

    fn state(&self) -> MutexGuard<'_, State> {
        match self.state.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Stream server-search hits `delay` apart; a test sets zero.
    #[cfg(test)]
    pub fn set_hit_delay(&self, delay: Duration) {
        self.state().hit_delay = delay;
    }

    /// The instance id the fixture answers as.
    pub fn instance_id(&self) -> String {
        self.state().instance_id()
    }

    fn post(&self, incoming: Incoming) {
        if let Ok(tx) = self.events.lock() {
            let _ = tx.send(incoming);
        }
    }

    /// Commit one event at the next revision and post it.
    fn emit(&self, kind: &str, payload: Value) {
        let mut s = self.state();
        self.emit_locked(&mut s, kind, payload);
    }

    /// [`Fixture::emit`] under a lock the caller holds, so what it decided
    /// under that lock and the event it posts are one step: posting under the
    /// state lock keeps the channel in revision order.
    fn emit_locked(&self, s: &mut State, kind: &str, payload: Value) {
        s.revision += 1;
        let envelope = EventEnvelope {
            instance_id: s.instance_id(),
            revision: s.revision,
            kind: kind.to_string(),
            payload,
        };
        self.post(Incoming::Event(envelope));
    }

    /// Answer one wire method.
    pub fn call(self: &Arc<Self>, method: &str, params: Value) -> Result<Value> {
        #[cfg(test)]
        if let Ok(mut calls) = self.calls.lock() {
            calls.push((method.to_string(), params.clone()));
        }
        let mut s = self.state();
        if s.down {
            return Err(anyhow!("{method}: the daemon is not reachable"));
        }
        // The daemon's draft queries answer from a fresh directory scan.
        s.rescan();
        match method {
            "state.bootstrap" => Ok(s.bootstrap()),
            "account.list" => Ok(s.accounts.clone()),
            "mailbox.list" => {
                let account = param_str(method, &params, "account")?;
                if !s.messages.contains_key(account) {
                    return Err(refused(
                        method,
                        -32005,
                        &format!("account_unknown: {account}"),
                    ));
                }
                Ok(json!({"account": account, "mailboxes": s.mailbox_rows(account)}))
            }
            "message.list" => {
                let account = param_str(method, &params, "account")?;
                let mailbox = param_str(method, &params, "mailbox")?;
                let Some(boxes) = s.messages.get(account) else {
                    return Err(refused(
                        method,
                        -32005,
                        &format!("account_unknown: {account}"),
                    ));
                };
                if mailbox.eq_ignore_ascii_case("drafts") {
                    return Err(refused(method, -32602, "drafts are listed by draft.list"));
                }
                let known = s.mailbox_rows(account).iter().any(|r| r["slug"] == mailbox);
                if !known {
                    return Err(refused(method, -32602, &format!("no mailbox `{mailbox}`")));
                }
                let rows: Vec<Value> = boxes
                    .get(mailbox)
                    .map(|rows| rows.iter().map(wire_row).collect())
                    .unwrap_or_default();
                Ok(
                    json!({"account": account, "mailbox": mailbox, "total": rows.len(), "messages": rows}),
                )
            }
            "draft.list" => {
                let account = param_str(method, &params, "account")?;
                let listing = s.drafts.get(account).cloned().ok_or_else(|| {
                    refused(method, -32005, &format!("account_unknown: {account}"))
                })?;
                Ok(serde_json::to_value(listing)?)
            }
            "draft.create"
            | "draft.reply"
            | "draft.forward"
            | "draft.create_from_message"
            | "draft.path"
            | "draft.approve"
            | "draft.demote"
            | "draft.validate"
            | "draft.preview"
            | "signature.list" => {
                let (answer, events) = s.draft_method(method, &params)?;
                for (kind, payload) in events {
                    self.emit_locked(&mut s, kind, payload);
                }
                Ok(answer)
            }
            "message.get" => {
                let account = param_str(method, &params, "account")?;
                let row_id = params["row_id"].as_i64().unwrap_or(-1);
                let (mailbox, row) = s.row(account, row_id).ok_or_else(|| {
                    refused(method, -32602, &format!("no message has row_id {row_id}"))
                })?;
                Ok(shown(account, mailbox, row, params["body"] != false))
            }
            "message.html" => {
                let account = param_str(method, &params, "account")?;
                let row_id = params["row_id"].as_i64().unwrap_or(-1);
                if s.row(account, row_id).is_none() {
                    return Err(refused(
                        method,
                        -32602,
                        &format!("no message has row_id {row_id}"),
                    ));
                }
                let Some(body) = s.html.get(&row_id) else {
                    return Err(refused(method, -32602, "the message has no HTML part"));
                };
                let html = rendition(body);
                Ok(json!({"account": account, "row_id": row_id, "bytes": html.len(), "html": html}))
            }
            "message.search" => {
                let account = param_str(method, &params, "account")?;
                let query = params["query"]
                    .as_str()
                    .unwrap_or_default()
                    .trim()
                    .to_lowercase();
                if query.chars().all(|c| c.is_ascii_punctuation()) && !query.is_empty() {
                    return Err(refused(method, -32602, "the query has no searchable term"));
                }
                let limit = params["limit"]
                    .as_u64()
                    .map(|l| l as usize)
                    .unwrap_or(usize::MAX);
                let only = params["mailbox"].as_str();
                let Some(boxes) = s.messages.get(account) else {
                    return Err(refused(
                        method,
                        -32005,
                        &format!("account_unknown: {account}"),
                    ));
                };
                let hits: Vec<Value> = boxes
                    .iter()
                    .filter(|(mailbox, _)| only.is_none_or(|m| m == mailbox.as_str()))
                    .flat_map(|(mailbox, rows)| rows.iter().map(move |r| (mailbox, r)))
                    .filter(|(_, r)| matches(r, &query))
                    .take(limit)
                    .map(|(mailbox, r)| {
                        let mut hit = wire_row(r);
                        hit["mailbox"] = json!(mailbox);
                        hit
                    })
                    .collect();
                Ok(json!({"account": account, "query": params["query"], "hits": hits}))
            }
            "message.search_server" => {
                let account = param_str(method, &params, "account")?.to_string();
                let query = params["query"]
                    .as_str()
                    .unwrap_or_default()
                    .trim()
                    .to_lowercase();
                let limit = params["limit"].as_u64().unwrap_or(50) as usize;
                let exclude: Vec<String> = params["exclude_message_ids"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                let Some(boxes) = s.messages.get(&account) else {
                    return Err(refused(
                        method,
                        -32005,
                        &format!("account_unknown: {account}"),
                    ));
                };
                let mut hits: Vec<Value> = boxes
                    .iter()
                    .flat_map(|(mailbox, rows)| rows.iter().map(move |r| (mailbox, r)))
                    .filter(|(_, r)| matches(r, &query))
                    .filter(|(_, r)| !exclude.iter().any(|e| r["message_id"] == e.as_str()))
                    .map(|(mailbox, r)| server_hit(&account, mailbox, r))
                    .collect();
                hits.push(server_only_hit(&account, &query));
                hits.truncate(limit);
                drop(s);
                Ok(
                    json!({"operation_id": self.start_server_search(account, params["query"].clone(), hits)}),
                )
            }
            "operation.status" => {
                let id = param_str(method, &params, "operation_id")?;
                s.operations
                    .get(id)
                    .cloned()
                    .ok_or_else(|| refused(method, -32602, &format!("unknown operation {id}")))
            }
            "operation.cancel" => {
                let id = param_str(method, &params, "operation_id")?.to_string();
                let state = s
                    .operations
                    .get(&id)
                    .map(|o| o["state"].as_str().unwrap_or_default().to_string())
                    .ok_or_else(|| refused(method, -32602, &format!("unknown operation {id}")))?;
                if state != "queued" && state != "running" {
                    return Err(refused(
                        method,
                        -32602,
                        &format!("operation {id} is already {state}"),
                    ));
                }
                drop(s);
                self.settle(&id, "cancelled", None);
                Ok(json!({"operation_id": id, "state": "cancelled"}))
            }
            "message.archive" | "message.delete" | "message.move" | "message.set_flag"
            | "message.set_read" => {
                let answer = s.mutate(method, &params)?;
                drop(s);
                let account = answer["account"].as_str().unwrap_or_default().to_string();
                // `settle: false` asks for the debounced drain; any other
                // value is `mp archive`'s blocking contract, drained before
                // the answer.
                if params["settle"] == false {
                    self.request_drain(&account);
                } else {
                    self.drain(&account);
                }
                Ok(answer)
            }
            "draft.discard" => {
                let account = param_str(method, &params, "account")?.to_string();
                let id = param_str(method, &params, "id")?.to_string();
                let force = params["force"] == true;
                s.account_known(method, &account)?;
                let selector = Selector::for_draft(&account, &id).to_string();
                let entry = s.draft(method, &account, &id)?.clone();
                let status = entry.status.clone();
                if status == "approved" && !force {
                    return Err(refused(
                        method,
                        -32602,
                        &format!(
                            "{selector} is approved, a queued send; deleting it drops that send. \
                             Re-run with --force, or demote it first with `mp mark-draft`."
                        ),
                    ));
                }
                mp_core::draft::remove_draft_files(Path::new(&entry.path))
                    .with_context(|| format!("removing {}", entry.path))?;
                s.rescan();
                drop(s);
                // The daemon's draft watcher sees the file go.
                self.emit(
                    "state.remove",
                    json!({"resource": format!("draft:{account}/{id}")}),
                );
                Ok(json!({"account": account, "id": id, "selector": selector, "status": status}))
            }
            "send.hold_status" => {
                only(method, &params, &["account"])?;
                let listing = s.hold_listing(params["account"].as_str());
                Ok(serde_json::to_value(listing)?)
            }
            "send.cancel_hold" => {
                only(method, &params, &["operation_id"])?;
                let id = param_str(method, &params, "operation_id")?.to_string();
                let Some(status) = s.take_hold(&id) else {
                    return Err(refused(
                        method,
                        -32602,
                        &format!("no hold is running for {id}"),
                    ));
                };
                self.emit_locked(
                    &mut s,
                    KIND_SEND_HOLD_CANCELLED,
                    serde_json::to_value(status)?,
                );
                let revision = s.revision;
                Ok(json!({"cancelled": true, "operation_id": id, "revision": revision}))
            }
            "sync.quick" | "sync.full" => {
                let allowed: &[&str] = if method == "sync.quick" {
                    &["account", "limit", "mailbox", "dry_run"]
                } else {
                    &["account", "mailbox", "dry_run"]
                };
                only(method, &params, allowed)?;
                let account = param_str(method, &params, "account")?.to_string();
                s.account_known(method, &account)?;
                let id = s.next_operation_id("fixture-op");
                s.operations.insert(
                    id.clone(),
                    json!({
                        "operation_id": id, "method": method, "state": "running",
                        "scope": "durable", "progress": null, "result": null, "error": null
                    }),
                );
                let delay = s.sync_delay;
                drop(s);
                let fixture = Arc::clone(self);
                let op = id.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(delay);
                    if fixture.running(&op) {
                        fixture.complete_sync(&op, &account);
                    }
                });
                Ok(json!({"operation_id": id}))
            }
            other => Err(refused(other, -32601, "method not found")),
        }
    }

    /// Ask for a drain of `account` once its mutations have been quiet for
    /// the drain delay, the daemon's `request_drain`: every request restarts
    /// the wait.
    fn request_drain(self: &Arc<Self>, account: &str) {
        let (ticket, delay) = {
            let mut s = self.state();
            let ticket = s.drain_requests.entry(account.to_string()).or_insert(0);
            *ticket += 1;
            (*ticket, s.drain_delay)
        };
        let fixture = Arc::clone(self);
        let account = account.to_string();
        std::thread::spawn(move || {
            std::thread::sleep(delay);
            let latest = fixture.state().drain_requests.get(&account).copied();
            if latest == Some(ticket) {
                fixture.drain(&account);
            }
        });
    }

    /// A drain whose ops all landed: the counts that moved, and nothing else.
    fn drain(&self, account: &str) {
        let moved = self.state().moved_counts(account);
        for payload in moved {
            self.emit("state.invalidate", payload);
        }
    }

    /// Finish a sync pass the way the runtime's tick does: the queued
    /// mutations drain, `sync.completed` is committed, then the operation
    /// settles. The `home` fixture account fails its login, as its sync
    /// health says.
    fn complete_sync(&self, op: &str, account: &str) {
        let failed_login = self.state().bootstrap["snapshot"]["accounts"]
            .as_array()
            .and_then(|a| a.iter().find(|x| x["name"] == account))
            .is_some_and(|x| x["sync_health"]["state"] == "failed");
        let error = failed_login.then(|| "the server refused the login".to_string());
        self.drain(account);
        let outcome = json!({
            "account": account,
            "severity": if error.is_some() { "error" } else { "ok" },
            "saved": 0, "skipped": 0, "flags_updated": 0, "pruned": 0,
            "prunes_deferred": 0, "uid_rebound": 0, "uidvalidity_resets": 0,
            "bodies_truncated": 0, "non_converging": [], "failed_mutations": 0,
            "error": error, "new_inbox_mail": []
        });
        self.emit(KIND_SYNC_COMPLETED, outcome.clone());
        match error {
            None => self.settle(
                op,
                "succeeded",
                Some(json!({"blocked": false, "outcome": outcome, "new_inbox_mail": []})),
            ),
            Some(message) => self.fail(op, &message),
        }
    }

    /// Settle `id` as failed with an internal error.
    fn fail(&self, id: &str, message: &str) {
        let payload = {
            let mut s = self.state();
            let Some(op) = s.operations.get_mut(id) else {
                return;
            };
            let error = json!({"code": -32603, "message": message});
            op["state"] = json!("failed");
            op["error"] = error.clone();
            json!({"operation_id": id, "state": "failed", "error": error})
        };
        self.emit(KIND_OPERATION_FINISHED, payload);
    }

    /// Run one hold's countdown off its deadline, the daemon's `run_held`: a
    /// tick each time the remainder drops a second, down to 1, then the fire.
    /// A hold that left the table (cancelled, or a restart) stops it.
    fn spawn_hold(self: &Arc<Self>, id: String) {
        let fixture = Arc::clone(self);
        std::thread::spawn(move || loop {
            let (deadline, second) = {
                let s = fixture.state();
                let Some(hold) = s.holds.iter().find(|h| h.status.operation_id == id) else {
                    return;
                };
                (hold.deadline, s.hold_second)
            };
            let left = remaining(deadline, Instant::now(), second);
            if left <= 1 {
                std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
                let mut s = fixture.state();
                if let Some(status) = s.take_hold(&id) {
                    let payload = serde_json::to_value(status).unwrap_or_default();
                    fixture.emit_locked(&mut s, KIND_SEND_HOLD_FIRED, payload);
                }
                return;
            }
            let next = deadline - second * (left - 1) as u32;
            std::thread::sleep(next.saturating_duration_since(Instant::now()));
            let mut s = fixture.state();
            let now = Instant::now();
            let Some(status) = s
                .holds
                .iter()
                .find(|h| h.status.operation_id == id)
                .map(|h| s.hold_at(h, now))
            else {
                return;
            };
            // A wake past the deadline is the fire's, not a tick of 0.
            if status.remaining_secs == 0 {
                continue;
            }
            let payload = serde_json::to_value(status).unwrap_or_default();
            fixture.emit_locked(&mut s, KIND_SEND_HOLD_TICK, payload);
        });
    }

    fn start_server_search(
        self: &Arc<Self>,
        account: String,
        query: Value,
        hits: Vec<Value>,
    ) -> String {
        let id = {
            let mut s = self.state();
            let id = format!("fixture-op-{}", s.next_op);
            s.next_op += 1;
            s.operations.insert(
                id.clone(),
                json!({
                    "operation_id": id, "method": "message.search_server", "state": "running",
                    "scope": "durable", "progress": null, "result": null, "error": null
                }),
            );
            id
        };
        let fixture = Arc::clone(self);
        let op = id.clone();
        let delay = self.state().hit_delay;
        std::thread::spawn(move || {
            let total = hits.len();
            for hit in hits {
                std::thread::sleep(delay);
                if !fixture.running(&op) {
                    return;
                }
                fixture.emit(
                    "message.server_hit",
                    json!({"operation_id": op, "hit": hit}),
                );
            }
            if fixture.running(&op) {
                let result = json!({
                    "account": account, "query": query, "hits": total,
                    "deduplicated": 0, "unreachable": []
                });
                fixture.settle(&op, "succeeded", Some(result));
            }
        });
        id
    }

    fn running(&self, id: &str) -> bool {
        let s = self.state();
        !s.down
            && s.operations
                .get(id)
                .is_some_and(|o| o["state"] == "running")
    }

    fn settle(&self, id: &str, state: &str, result: Option<Value>) {
        let payload = {
            let mut s = self.state();
            let Some(op) = s.operations.get_mut(id) else {
                return;
            };
            op["state"] = json!(state);
            match &result {
                Some(r) => {
                    op["result"] = r.clone();
                    json!({"operation_id": id, "state": state, "result": r})
                }
                None => {
                    let error = json!({"code": -32008, "message": "operation_cancelled", "data": {"operation_id": id}});
                    op["error"] = error.clone();
                    json!({"operation_id": id, "state": state, "error": error})
                }
            }
        };
        self.emit("operation.finished", payload);
    }

    /// Drive one connection state; see [`SIMULATIONS`]. `rollback:<n>`
    /// reverts only the last `n` mutations.
    pub fn simulate(self: &Arc<Self>, what: &str) -> Result<()> {
        if let Some(n) = what.strip_prefix("rollback:") {
            let n: usize = n
                .parse()
                .map_err(|_| anyhow!("`rollback:<n>` takes a count, not `{n}`"))?;
            return self.simulate_rollback(n);
        }
        match what {
            "rollback" => return self.simulate_rollback(usize::MAX),
            "hold" => {
                self.simulate_hold();
                return Ok(());
            }
            "editor_save" => return self.simulate_editor(false),
            "editor_invalid" => return self.simulate_editor(true),
            _ => {}
        }
        match what {
            "disconnect" => {
                self.state().down = true;
                self.post(Incoming::Disconnected {
                    reason: "fixture: simulated disconnect".into(),
                });
            }
            "reconnect" => {
                let id = {
                    let mut s = self.state();
                    s.down = false;
                    s.instance_id()
                };
                self.post(Incoming::Reconnected { instance_id: id });
            }
            "restart" => {
                self.post(Incoming::Disconnected {
                    reason: "fixture: simulated daemon restart".into(),
                });
                let id = {
                    let mut s = self.state();
                    s.instance += 1;
                    s.revision = 1;
                    s.operations.clear();
                    // A daemon's holds and queued ops live in its memory and
                    // its drain; neither survives it here.
                    s.holds.clear();
                    s.journal.clear();
                    s.drain_requests.clear();
                    s.down = false;
                    s.instance_id()
                };
                self.post(Incoming::Reconnected { instance_id: id });
            }
            "resync" => {
                let id = self.state().instance_id();
                self.post(Incoming::Resync {
                    instance_id: id,
                    reason: "event_queue_overflow".into(),
                });
            }
            "new_mail" => {
                {
                    let mut s = self.state();
                    let id = s.next_row;
                    s.next_row += 1;
                    let row = json!({
                        "id": id, "uid": id, "message_id": format!("<new-{id}@fixture.example>"),
                        "from": "Fixture <fixture@example.com>", "to": "me@example.com",
                        "cc": null, "reply_to": null, "bcc": null,
                        "subject": format!("New fixture message {id}"),
                        "date_sort": "2026-09-30T12:00:00", "date_display": "Wed, 30 Sep 2026 12:00:00 +0200",
                        "flags": {"seen": false, "answered": false, "forwarded": false, "flagged": false},
                        "has_attachments": false, "is_invite": false,
                        "selector": format!("mp://work/inbox/new-{id}@fixture.example"),
                        "body": "A message the fixture delivered.\n", "attachments": []
                    });
                    if let Some(inbox) = s.messages.get_mut("work").and_then(|m| m.get_mut("inbox"))
                    {
                        inbox.insert(0, row);
                    }
                }
                self.emit(
                    "state.invalidate",
                    json!({"resource": "mailbox:work/inbox", "scope": {"query": "counts"}}),
                );
            }
            "shutdown" => {
                self.emit(
                    "daemon.shutting_down",
                    json!({"grace_secs": 5, "pending": []}),
                );
            }
            other => {
                return Err(anyhow!(
                    "unknown simulation `{other}`; one of {}, or rollback:<n>",
                    SIMULATIONS.join(", ")
                ))
            }
        }
        Ok(())
    }

    /// A drain the server refused: the last `n` mutations are put back, each
    /// account hears `mutations.rolled_back` with its count, then the counts
    /// that moved, in the drainer's order. Nothing to roll back is an error,
    /// since the daemon publishes nothing for a drain that failed nothing.
    fn simulate_rollback(&self, n: usize) -> Result<()> {
        let failed = self.state().roll_back(n);
        if failed.is_empty() {
            return Err(anyhow!("no fixture mutation is left to roll back"));
        }
        for (account, failed) in failed {
            self.emit(
                KIND_MUTATIONS_ROLLED_BACK,
                json!({"account": account, "failed": failed}),
            );
            self.drain(&account);
        }
        Ok(())
    }

    /// A send another client armed on `work`'s first draft, with a
    /// [`SIMULATED_HOLD_SECS`] window: `send.hold_started` now, a
    /// `send.hold_tick` a second, `send.hold_fired` at the end unless
    /// `send.cancel_hold` comes first. The fire sends nothing.
    fn simulate_hold(self: &Arc<Self>) {
        let status = {
            let mut s = self.state();
            let id = s.next_operation_id("fixture-hold");
            let draft = s.drafts.get("work").and_then(|l| l.drafts.first());
            let status = HoldStatus {
                operation_id: id,
                account: "work".into(),
                draft_id: draft.map_or("fixture-draft", |d| d.id.as_str()).into(),
                subject: draft.and_then(|d| d.subject.clone()).unwrap_or_default(),
                hold_secs: SIMULATED_HOLD_SECS,
                remaining_secs: SIMULATED_HOLD_SECS,
                fires_at: String::new(),
                origin: "tui".into(),
            };
            s.arm_hold(status, SIMULATED_HOLD_SECS)
        };
        let id = status.operation_id.clone();
        self.emit(
            KIND_SEND_HOLD_STARTED,
            serde_json::to_value(status).unwrap_or_default(),
        );
        self.spawn_hold(id);
    }

    /// Journal an `editor_open` instead of spawning anything.
    pub fn record_editor(&self, path: &str, command: Vec<String>) {
        self.state().editor_opens.push(EditorOpen {
            path: path.to_string(),
            command,
        });
    }

    /// Every stubbed `editor_open`, oldest first.
    pub fn editor_opens(&self) -> Vec<EditorOpen> {
        self.state().editor_opens.clone()
    }

    /// A client wrote the draft file at `path` (a recipient rewrite): publish
    /// what the daemon's watcher would, `draft.changed` or `draft.invalid`.
    pub fn file_written(&self, path: &Path) {
        let mut s = self.state();
        s.rescan();
        for (kind, payload) in s.watch_events(path) {
            self.emit_locked(&mut s, kind, payload);
        }
    }

    /// The editor saved the newest draft `editor_open` named: a line appended
    /// (`editor_save`), or a frontmatter that no longer parses
    /// (`editor_invalid`), and the watcher's event for it.
    fn simulate_editor(&self, broken: bool) -> Result<()> {
        let mut s = self.state();
        let open = s.editor_opens.last().cloned().ok_or_else(|| {
            anyhow!("no draft was opened in the editor yet; editor_open names the file")
        })?;
        let path = PathBuf::from(&open.path);
        let content =
            fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let next = if broken {
            content.replacen("---\n", "---\nto: [unclosed\n", 1)
        } else {
            format!("{}\n{EDITOR_SAVE_LINE}\n", content.trim_end_matches('\n'))
        };
        fs::write(&path, next).with_context(|| format!("writing {}", path.display()))?;
        s.rescan();
        let events = s.watch_events(&path);
        if events.is_empty() {
            return Err(anyhow!(
                "{} is not a fixture draft any more",
                path.display()
            ));
        }
        for (kind, payload) in events {
            self.emit_locked(&mut s, kind, payload);
        }
        Ok(())
    }
}

/// `<temp>/mp-desktop-fixture-<pid>`, with a `-<n>` suffix for every fixture
/// after the first in one process (the tests load many).
fn fixture_root() -> PathBuf {
    static RUNS: AtomicU32 = AtomicU32::new(0);
    let n = RUNS.fetch_add(1, Ordering::SeqCst);
    let base = format!("mp-desktop-fixture-{}", std::process::id());
    std::env::temp_dir().join(if n == 0 { base } else { format!("{base}-{n}") })
}

/// Write `drafts.json`'s rows as files under `root`, with the bodies of
/// `draft-bodies.json`, the first of each account the newest, so the listing
/// keeps the file's order. The rows' own paths are ignored.
fn seed_drafts(
    root: &Path,
    seeds: &BTreeMap<String, DraftListing>,
    bodies: &BTreeMap<String, String>,
) -> Result<()> {
    // A directory left by an earlier run under a reused pid.
    let _ = fs::remove_dir_all(root);
    let now = SystemTime::now();
    for (account, listing) in seeds {
        let dir = root.join("drafts").join(account);
        fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        for (i, row) in listing.drafts.iter().enumerate() {
            let id = row.id.as_str();
            let path = dir.join(format!("{id}.md"));
            let skeleton = mp_core::draft::new_draft_skeleton_with_id(
                FIXTURE_FROM,
                row.date.as_deref().unwrap_or_default(),
                id,
                None,
            );
            let body = bodies.get(id).map_or("", String::as_str);
            fs::write(&path, format!("{skeleton}{body}"))?;
            let text = |v: &Option<String>| v.clone().unwrap_or_default();
            mp_core::draft::rewrite_draft_recipients(
                &path,
                &DraftRecipientEdit {
                    to: text(&row.to),
                    cc: text(&row.cc),
                    bcc: String::new(),
                    subject: text(&row.subject),
                },
            )?;
            if row.status == "approved" {
                mp_core::draft::mark_as_approved(&path)?;
            }
            fs::File::options()
                .write(true)
                .open(&path)?
                .set_modified(now - Duration::from_secs(60 * (i as u64 + 1)))?;
        }
    }
    Ok(())
}

/// A file stem, the id the daemon's watcher names an unparseable draft by.
fn stem_of(path: &str) -> String {
    Path::new(path)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn one_line(error: &anyhow::Error) -> String {
    format!("{error:#}").replace('\n', " ")
}

/// The daemon's `index_dir` over one directory: the rows that parse, newest
/// file first then by id, and the files that do not.
fn scan_drafts(account: &str, dir: &Path) -> (Vec<DraftEntry>, Vec<DraftSkip>) {
    let mut rows: Vec<(SystemTime, DraftEntry)> = Vec::new();
    let mut skipped = Vec::new();
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if !path.is_file() || path.extension().is_none_or(|x| x != "md") {
            continue;
        }
        let shown = path.display().to_string();
        let mtime = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(UNIX_EPOCH);
        match mp_core::draft::parse_email_draft(&path) {
            Ok(draft) => {
                let fm = &draft.frontmatter;
                let id = fm.id.clone().unwrap_or_else(|| stem_of(&shown));
                let filled = |v: &Option<String>| v.clone().filter(|v| !v.trim().is_empty());
                rows.push((
                    mtime,
                    DraftEntry {
                        selector: Selector::for_draft(account, &id).to_string(),
                        id,
                        path: shown,
                        status: fm.status.to_string(),
                        to: filled(&fm.to),
                        cc: filled(&fm.cc),
                        subject: Some(fm.subject.clone()),
                        date: fm.date.clone(),
                        valid: true,
                        ready: mp_core::draft::validate_draft(&draft).is_ok(),
                    },
                ));
            }
            Err(e) => skipped.push(DraftSkip {
                path: shown,
                error: one_line(&e),
            }),
        }
    }
    rows.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.id.cmp(&b.1.id)));
    skipped.sort_by(|a, b| a.path.cmp(&b.path));
    (rows.into_iter().map(|(_, row)| row).collect(), skipped)
}

/// A listed draft as the snapshot and `draft.changed` carry it, less the
/// account.
fn snapshot_row(entry: &DraftEntry) -> Value {
    json!({
        "id": entry.id, "path": entry.path, "to": entry.to,
        "subject": entry.subject.clone().unwrap_or_default(), "status": entry.status,
        "valid": entry.valid, "ready": entry.ready
    })
}

/// One draft's diagnostics, as `draft.validate` reports them.
fn report(account: &str, row: &DraftEntry) -> DraftReport {
    let outcome = mp_core::draft::parse_email_draft(Path::new(&row.path))
        .and_then(|draft| mp_core::draft::validate_draft(&draft));
    DraftReport {
        id: row.id.clone(),
        selector: Selector::for_draft(account, &row.id).to_string(),
        valid: outcome.is_ok(),
        error: outcome.as_ref().err().map(|e| e.to_string()),
        warnings: outcome.unwrap_or_default(),
    }
}

/// The `headers` override of `draft.reply` and `draft.forward`: all four
/// keys once present, as the daemon requires.
fn recipient_headers(method: &str, params: &Value) -> Result<Option<DraftRecipientEdit>> {
    let headers = match &params["headers"] {
        Value::Null => return Ok(None),
        Value::Object(headers) => headers,
        _ => {
            return Err(refused(
                method,
                -32602,
                "headers is an object of to, cc, bcc and subject",
            ))
        }
    };
    let field = |name: &str| {
        headers
            .get(name)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| {
                refused(
                    method,
                    -32602,
                    &format!(
                        "headers overrides all four of to, cc, bcc and subject; {name} is missing"
                    ),
                )
            })
    };
    Ok(Some(DraftRecipientEdit {
        to: field("to")?,
        cc: field("cc")?,
        bcc: field("bcc")?,
        subject: field("subject")?,
    }))
}

/// Row keys a listing sends as `""` when the header was absent, where the
/// fixtures (like the store) may hold `null` or nothing.
const LISTED_STRINGS: &[&str] = &["from", "to", "subject", "date_sort", "date_display"];

/// A fixture row as `message.list` sends it.
fn wire_row(row: &Value) -> Value {
    let mut row = row.clone();
    if let Some(obj) = row.as_object_mut() {
        for key in FIXTURE_ONLY_KEYS {
            obj.remove(*key);
        }
        for key in LISTED_STRINGS {
            let value = obj.get(*key).and_then(Value::as_str).unwrap_or_default();
            let value = json!(value);
            obj.insert((*key).to_string(), value);
        }
    }
    row
}

/// A header as a listing or a server hit sends it: the string, or `""`.
fn listed(row: &Value, key: &str) -> Value {
    json!(row[key].as_str().unwrap_or_default())
}

/// A header as `message.get` sends it (`read_cmd::present`): the string, or
/// `null` when it is absent or empty.
fn present(row: &Value, key: &str) -> Value {
    row[key]
        .as_str()
        .filter(|v| !v.is_empty())
        .map_or(Value::Null, |v| json!(v))
}

fn matches(row: &Value, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    ["subject", "from", "body"].iter().any(|k| {
        row[*k]
            .as_str()
            .is_some_and(|v| v.to_lowercase().contains(query))
    })
}

/// The `message.get` record (`read_cmd::ShownMessage`).
fn shown(account: &str, mailbox: &str, row: &Value, body: bool) -> Value {
    let f = &row["flags"];
    let mut flags = Vec::new();
    if f["seen"] == true {
        flags.push("read");
    }
    if f["answered"] == true {
        flags.push("answered");
    }
    if f["flagged"] == true {
        flags.push("flagged");
    }
    let mut record = json!({
        "selector": row["selector"], "account": account, "mailbox": mailbox,
        "message_id": row["message_id"], "from": present(row, "from"),
        "to": present(row, "to"), "cc": present(row, "cc"),
        "subject": present(row, "subject"), "date": present(row, "date_display"), "flags": flags,
        "invite": row["is_invite"], "attachments": row["attachments"],
    });
    if body {
        record["body"] = row["body"].clone();
    }
    record
}

fn server_hit(account: &str, mailbox: &str, row: &Value) -> Value {
    json!({
        "account": account, "mailbox": mailbox, "message_id": row["message_id"],
        "row_id": row["id"], "selector": row["selector"], "from": listed(row, "from"),
        "to": listed(row, "to"), "cc": row["cc"], "reply_to": null, "bcc": null,
        "subject": listed(row, "subject"), "date_display": listed(row, "date_display"),
        "date_sort": listed(row, "date_sort"), "flags": row["flags"],
        "has_attachments": row["has_attachments"], "is_invite": row["is_invite"],
        "body_text": row["body"], "html_body": null
    })
}

/// One hit the store has never ingested: `row_id` and `selector` are null.
fn server_only_hit(account: &str, query: &str) -> Value {
    json!({
        "account": account, "mailbox": "Archive", "message_id": "<server-only@fixture.example>",
        "row_id": null, "selector": null, "from": "Old Colleague <old@example.com>",
        "to": "me@example.com", "cc": null, "reply_to": null, "bcc": null,
        "subject": format!("Server-only match for \"{query}\""),
        "date_display": "Mon, 3 Mar 2025 09:00:00 +0100", "date_sort": "2025-03-03T08:00:00",
        "flags": {"seen": true, "answered": false, "forwarded": false, "flagged": false},
        "has_attachments": false, "is_invite": false,
        "body_text": "A message only the server holds.\n", "html_body": null
    })
}

/// A body as the daemon's rendition shapes it: the CSP meta tag first, after
/// a leading doctype, which ends at its first `>` (`parse.rs`'s
/// `insert_at_document_start`).
fn rendition(body: &str) -> String {
    let tag = format!("<meta http-equiv=\"Content-Security-Policy\" content=\"{MESSAGE_CSP}\">\n");
    let lead = body.len() - body.trim_start().len();
    let rest = &body[lead..];
    let doctype = rest
        .get(..9)
        .is_some_and(|s| s.eq_ignore_ascii_case("<!doctype"));
    match rest.find('>').filter(|_| doctype) {
        Some(gt) => {
            let at = lead + gt + 1;
            format!("{}{tag}{}", &body[..at], &body[at..])
        }
        None => format!("{tag}{body}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    fn fixture() -> (Arc<Fixture>, std::sync::mpsc::Receiver<Incoming>) {
        let (tx, rx) = channel();
        (Arc::new(Fixture::load(tx).expect("fixtures load")), rx)
    }

    #[test]
    fn the_fixtures_load_and_have_the_promised_size() {
        let (f, _rx) = fixture();
        let b: Bootstrap =
            serde_json::from_value(f.call("state.bootstrap", json!({})).expect("bootstrap"))
                .expect("decodes");
        assert_eq!(b.snapshot.accounts.len(), 2);
        let mailboxes: usize = b.snapshot.mailboxes.values().map(Vec::len).sum();
        assert_eq!(mailboxes, 6);
        let s = f.state();
        let messages: usize = s
            .messages
            .values()
            .flat_map(|m| m.values())
            .map(Vec::len)
            .sum();
        assert_eq!(messages, 21);
        assert_eq!(s.html.len(), 3);
        let hostile = s
            .html
            .values()
            .find(|h| h.contains("<script>"))
            .expect("a hostile body");
        for needle in [
            "http-equiv=\"refresh\"",
            "target=\"_blank\"",
            "src=\"https://",
            "<form",
        ] {
            assert!(
                hostile.contains(needle),
                "the hostile body carries {needle}"
            );
        }
    }

    #[test]
    fn counts_are_computed_from_the_rows() {
        let (f, _rx) = fixture();
        let answer = f
            .call("mailbox.list", json!({"account": "work"}))
            .expect("list");
        let inbox = &answer["mailboxes"][0];
        assert_eq!(inbox["slug"], "inbox");
        assert_eq!(inbox["total"], 8);
        assert_eq!(
            answer["mailboxes"][1]["total"], 2,
            "drafts count from the draft index"
        );
    }

    #[test]
    fn listing_rows_decode_as_protocol_rows() {
        let (f, _rx) = fixture();
        let answer = f
            .call(
                "message.list",
                json!({"account": "work", "mailbox": "inbox"}),
            )
            .expect("list");
        let rows = mp_client::queries::decode_message_rows(&answer);
        assert_eq!(rows.len(), 8);
        assert!(rows.iter().all(|r| r.id > 0 && !r.selector.is_empty()));
        assert!(answer["messages"][0].get("body").is_none());
    }

    #[test]
    fn refusals_read_like_daemon_refusals() {
        let (f, _rx) = fixture();
        let e = f
            .call("message.html", json!({"account": "work", "row_id": 1002}))
            .expect_err("no markup");
        assert_eq!(crate::error::rpc_code(&format!("{e:#}")), Some(-32602));
        let e = f
            .call("mailbox.list", json!({"account": "nobody"}))
            .expect_err("unknown");
        assert_eq!(crate::error::rpc_code(&format!("{e:#}")), Some(-32005));
    }

    #[test]
    fn a_server_search_streams_hits_and_finishes() {
        let (f, rx) = fixture();
        let started = f
            .call(
                "message.search_server",
                json!({"account": "work", "query": "ledger"}),
            )
            .expect("started");
        let op = started["operation_id"].as_str().expect("id").to_string();
        let mut hits = 0;
        loop {
            match rx.recv_timeout(Duration::from_secs(5)).expect("an event") {
                Incoming::Event(e) if e.kind == "message.server_hit" => hits += 1,
                Incoming::Event(e) if e.kind == "operation.finished" => {
                    assert_eq!(e.payload["operation_id"], op.as_str());
                    break;
                }
                other => panic!("unexpected {other:?}"),
            }
        }
        assert!(hits >= 2, "local matches plus one server-only hit");
    }

    /// The next event, within five seconds.
    fn next_event(rx: &std::sync::mpsc::Receiver<Incoming>) -> EventEnvelope {
        match rx.recv_timeout(Duration::from_secs(5)).expect("an event") {
            Incoming::Event(e) => e,
            other => panic!("expected an event, got {other:?}"),
        }
    }

    fn inbox_ids(f: &Arc<Fixture>, account: &str, mailbox: &str) -> Vec<i64> {
        f.state().messages[account]
            .get(mailbox)
            .map(|rows| rows.iter().filter_map(|r| r["id"].as_i64()).collect())
            .unwrap_or_default()
    }

    #[test]
    fn a_queued_mutation_answers_first_and_drains_the_counts_after() {
        let (f, rx) = fixture();
        f.set_drain_delay(Duration::from_millis(50));
        let answer = f
            .call(
                "message.archive",
                json!({"account": "work", "row_id": 1001, "settle": false}),
            )
            .expect("archived");
        assert_eq!(answer["mailbox"], "inbox");
        assert_eq!(answer["id"], "inbox/1");
        assert_eq!(answer["moved_to"]["mailbox"], "archive");
        assert_eq!(
            answer["moved_to"]["selector"],
            "mp://work/archive/%3Cquarterly-ledger-review-1001@fixture.example%3E",
            "the stored Message-ID, brackets percent-encoded, as the daemon answers"
        );
        assert!(
            rx.try_recv().is_err(),
            "the mutation itself publishes nothing"
        );
        let mut resources = vec![
            next_event(&rx).payload["resource"].clone(),
            next_event(&rx).payload["resource"].clone(),
        ];
        resources.sort_by_key(|r| r.to_string());
        assert_eq!(resources, ["mailbox:work/archive", "mailbox:work/inbox"]);
        assert!(!inbox_ids(&f, "work", "inbox").contains(&1001));
        assert!(inbox_ids(&f, "work", "archive").contains(&1001));
    }

    #[test]
    fn a_burst_of_mutations_is_one_drain() {
        let (f, rx) = fixture();
        f.set_drain_delay(Duration::from_millis(100));
        for row in [1001, 1002, 1003] {
            f.call(
                "message.set_read",
                json!({"account": "work", "row_id": row, "read": false, "settle": false}),
            )
            .expect("set");
        }
        let e = next_event(&rx);
        assert_eq!(
            (e.kind.as_str(), &e.payload["resource"]),
            ("state.invalidate", &json!("mailbox:work/inbox"))
        );
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
    }

    #[test]
    fn mutations_refuse_like_the_daemon() {
        let (f, _rx) = fixture();
        let code = |method: &str, params: Value| {
            let e = f.call(method, params).expect_err("refused");
            crate::error::rpc_code(&format!("{e:#}"))
        };
        assert_eq!(
            code(
                "message.archive",
                json!({"account": "work", "row_id": 424242})
            ),
            Some(-32602)
        );
        assert_eq!(
            code(
                "message.archive",
                json!({"account": "work", "row_id": 1015})
            ),
            Some(-32602),
            "a row of another account"
        );
        assert_eq!(
            code(
                "message.archive",
                json!({"account": "nobody", "row_id": 1001})
            ),
            Some(-32005)
        );
        assert_eq!(
            code(
                "message.move",
                json!({"account": "work", "row_id": 1001, "destination": "drafts"})
            ),
            Some(-32602)
        );
        assert_eq!(
            code(
                "message.set_flag",
                json!({"account": "work", "row_id": 1001})
            ),
            Some(-32602),
            "the new state is required"
        );
        assert_eq!(inbox_ids(&f, "work", "inbox").len(), 8, "nothing moved");
    }

    #[test]
    fn a_rollback_restores_the_rows_and_says_how_many() {
        let (f, rx) = fixture();
        f.set_drain_delay(Duration::from_secs(60));
        let before = f.state().messages.clone();
        for (method, params) in [
            ("message.archive", json!({"row_id": 1001})),
            ("message.delete", json!({"row_id": 1003})),
            (
                "message.move",
                json!({"row_id": 1004, "destination": "Sent"}),
            ),
            ("message.set_flag", json!({"row_id": 1005, "flagged": true})),
            ("message.set_read", json!({"row_id": 1002, "read": false})),
        ] {
            let mut params = params;
            params["account"] = json!("work");
            params["settle"] = json!(false);
            f.call(method, params).expect(method);
        }
        f.call(
            "message.set_read",
            json!({"account": "home", "row_id": 1015, "read": false, "settle": false}),
        )
        .expect("home");
        assert_ne!(f.state().messages, before);

        f.simulate("rollback:1").expect("one");
        let e = next_event(&rx);
        assert_eq!(e.kind, KIND_MUTATIONS_ROLLED_BACK);
        assert_eq!(e.payload, json!({"account": "home", "failed": 1}));

        f.simulate("rollback").expect("the rest");
        assert_eq!(f.state().messages, before, "every row is back where it was");
        let rolled: Vec<Value> = std::iter::from_fn(|| rx.try_recv().ok())
            .filter_map(|i| match i {
                Incoming::Event(e) if e.kind == KIND_MUTATIONS_ROLLED_BACK => Some(e.payload),
                _ => None,
            })
            .collect();
        assert_eq!(rolled, [json!({"account": "work", "failed": 5})]);
        assert!(f.simulate("rollback").is_err(), "nothing is left");
    }

    #[test]
    fn a_discarded_draft_is_removed_and_an_approved_one_refused() {
        let (f, rx) = fixture();
        let answer = f
            .call(
                "draft.discard",
                json!({"account": "work", "id": "offsite-note"}),
            )
            .expect("discarded");
        assert_eq!(answer["selector"], "mp://work/drafts/offsite-note");
        assert_eq!(answer["status"], "draft");
        let e = next_event(&rx);
        assert_eq!(e.kind, "state.remove");
        assert_eq!(e.payload["resource"], "draft:work/offsite-note");
        let listed = f
            .call("draft.list", json!({"account": "work"}))
            .expect("list");
        assert_eq!(listed["drafts"].as_array().map(Vec::len), Some(1));
        let b: Bootstrap =
            serde_json::from_value(f.call("state.bootstrap", json!({})).expect("b")).expect("b");
        assert_eq!(b.snapshot.drafts["work"].len(), 1);

        let path = f.state().drafts["work"].drafts[0].path.clone();
        mp_core::draft::mark_as_approved(Path::new(&path)).expect("approved");
        let e = f
            .call(
                "draft.discard",
                json!({"account": "work", "id": "angebot-antwort"}),
            )
            .expect_err("approved");
        assert!(format!("{e:#}").contains("is approved"));
    }

    #[test]
    fn the_seeded_hold_is_in_the_bootstrap_and_the_status() {
        let (f, _rx) = fixture();
        let b: Bootstrap =
            serde_json::from_value(f.call("state.bootstrap", json!({})).expect("b")).expect("b");
        assert_eq!(b.snapshot.holds.len(), 1);
        let seed = &b.snapshot.holds[0];
        assert_eq!(seed.operation_id, "fixture-hold-seed");
        assert!(seed.remaining_secs > 0 && seed.remaining_secs <= 60);
        assert!(seed.fires_at.ends_with('Z') && seed.fires_at.len() == 20);
        let work = f
            .call("send.hold_status", json!({"account": "work"}))
            .expect("status");
        assert_eq!(work["holds"].as_array().map(Vec::len), Some(1));
        let home = f
            .call("send.hold_status", json!({"account": "home"}))
            .expect("status");
        assert_eq!(home["holds"], json!([]));
        assert!(f
            .call("send.hold_status", json!({"account": "work", "x": 1}))
            .is_err());
    }

    #[test]
    fn a_simulated_hold_counts_down_and_fires() {
        let (f, rx) = fixture();
        f.set_hold_second(Duration::from_millis(50));
        f.simulate("hold").expect("hold");
        let started = next_event(&rx);
        assert_eq!(started.kind, KIND_SEND_HOLD_STARTED);
        assert_eq!(started.payload["remaining_secs"], SIMULATED_HOLD_SECS);
        let id = started.payload["operation_id"].clone();
        let mut ticks = Vec::new();
        loop {
            let e = next_event(&rx);
            assert_eq!(e.payload["operation_id"], id);
            match e.kind.as_str() {
                KIND_SEND_HOLD_TICK => ticks.push(e.payload["remaining_secs"].as_u64()),
                KIND_SEND_HOLD_FIRED => {
                    assert_eq!(e.payload["remaining_secs"], 0);
                    break;
                }
                other => panic!("unexpected {other}"),
            }
        }
        // Off the deadline, so a late wake skips a second rather than
        // drifting: strictly down, never 0, never the full window.
        let ticks: Vec<u64> = ticks.into_iter().map(Option::unwrap_or_default).collect();
        assert!(!ticks.is_empty());
        assert!(ticks.windows(2).all(|w| w[0] > w[1]), "{ticks:?}");
        assert!(
            ticks.iter().all(|t| (1..SIMULATED_HOLD_SECS).contains(t)),
            "{ticks:?}"
        );
        let listing = f.call("send.hold_status", json!({})).expect("status");
        let ids: Vec<&Value> = listing["holds"]
            .as_array()
            .expect("holds")
            .iter()
            .map(|h| &h["operation_id"])
            .collect();
        assert_eq!(ids, [&json!("fixture-hold-seed")], "the fired hold is gone");
    }

    #[test]
    fn a_cancelled_hold_stops_and_cannot_be_cancelled_twice() {
        let (f, rx) = fixture();
        // The fire is ten of these away, far past the cancel below.
        f.set_hold_second(Duration::from_millis(100));
        f.simulate("hold").expect("hold");
        let id = next_event(&rx).payload["operation_id"]
            .as_str()
            .expect("id")
            .to_string();
        assert_eq!(next_event(&rx).kind, KIND_SEND_HOLD_TICK);
        let answer = f
            .call("send.cancel_hold", json!({"operation_id": id}))
            .expect("cancelled");
        assert_eq!(answer["cancelled"], true);
        // A tick posted between the first one and the cancel is still queued.
        let cancelled = loop {
            let e = next_event(&rx);
            if e.kind != KIND_SEND_HOLD_TICK {
                break e;
            }
        };
        assert_eq!(cancelled.kind, KIND_SEND_HOLD_CANCELLED);
        assert_eq!(cancelled.payload["remaining_secs"], 0);
        assert_eq!(answer["revision"], cancelled.revision);
        assert!(
            rx.recv_timeout(Duration::from_millis(500)).is_err(),
            "no tick and no fire after the cancel"
        );
        let e = f
            .call("send.cancel_hold", json!({"operation_id": id}))
            .expect_err("gone");
        assert_eq!(crate::error::rpc_code(&format!("{e:#}")), Some(-32602));
    }

    #[test]
    fn a_sync_pass_drains_completes_and_settles() {
        let (f, rx) = fixture();
        f.set_sync_delay(Duration::ZERO);
        f.set_drain_delay(Duration::from_secs(60));
        f.call(
            "message.delete",
            json!({"account": "work", "row_id": 1009, "settle": false}),
        )
        .expect("deleted");
        let op = f
            .call("sync.quick", json!({"account": "work"}))
            .expect("started")["operation_id"]
            .clone();
        let kinds: Vec<String> = (0..3).map(|_| next_event(&rx).kind).collect();
        assert_eq!(
            kinds,
            [
                "state.invalidate",
                KIND_SYNC_COMPLETED,
                KIND_OPERATION_FINISHED
            ]
        );
        let status = f
            .call("operation.status", json!({"operation_id": op}))
            .expect("status");
        assert_eq!(status["state"], "succeeded");

        f.call("sync.full", json!({"account": "home"}))
            .expect("started");
        let completed = next_event(&rx);
        assert_eq!(completed.payload["severity"], "error");
        let finished = next_event(&rx);
        assert_eq!(finished.payload["state"], "failed");
        assert!(f
            .call("sync.full", json!({"account": "work", "limit": 5}))
            .is_err());
    }

    #[test]
    fn the_seeded_drafts_are_files_with_frontmatter_in_a_per_run_dir() {
        let (f, _rx) = fixture();
        let listed = f
            .call("draft.list", json!({"account": "work"}))
            .expect("list");
        let listing: DraftListing = serde_json::from_value(listed).expect("decodes");
        let ids: Vec<&str> = listing.drafts.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(ids, ["angebot-antwort", "offsite-note"], "the seed order");
        let root = f.state().root.clone();
        assert!(root
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with("mp-desktop-fixture-")));
        for draft in &listing.drafts {
            assert!(Path::new(&draft.path).starts_with(root.join("drafts/work")));
            let text = fs::read_to_string(&draft.path).expect("a real file");
            assert!(
                text.starts_with(&format!("---\nid: {}\n", draft.id)),
                "{text}"
            );
        }
        let angebot = &listing.drafts[0];
        assert_eq!(angebot.to.as_deref(), Some("robin@example.com"));
        assert!(angebot.valid && angebot.ready);
        assert!(!listing.drafts[1].ready, "no recipient, no subject");
        let b: Bootstrap =
            serde_json::from_value(f.call("state.bootstrap", json!({})).expect("b")).expect("b");
        assert_eq!(b.snapshot.drafts["work"][0].path, angebot.path);
    }

    #[test]
    fn a_broken_draft_is_published_invalid_skipped_and_refused() {
        let (f, rx) = fixture();
        assert!(
            f.simulate("editor_save").is_err(),
            "nothing was opened in the editor yet"
        );
        let path = f.state().drafts["work"].drafts[1].path.clone();
        f.record_editor(&path, vec!["zed".into(), path.clone()]);
        f.simulate("editor_invalid").expect("broken");
        let e = next_event(&rx);
        assert_eq!(e.kind, KIND_DRAFT_INVALID);
        let invalid: DraftInvalid = serde_json::from_value(e.payload).expect("decodes");
        assert_eq!(
            (
                invalid.account.as_str(),
                invalid.id.as_str(),
                invalid.path.as_str()
            ),
            ("work", "offsite-note", path.as_str())
        );
        assert!(!invalid.diagnostics.is_empty());

        let listed = f
            .call("draft.list", json!({"account": "work"}))
            .expect("list");
        assert_eq!(listed["drafts"].as_array().map(Vec::len), Some(1));
        assert_eq!(listed["skipped"][0]["path"], path.as_str());
        let b: Bootstrap =
            serde_json::from_value(f.call("state.bootstrap", json!({})).expect("b")).expect("b");
        let row = b.snapshot.drafts["work"]
            .iter()
            .find(|d| d.id == "offsite-note")
            .expect("the invalid row");
        assert_eq!((row.status.as_str(), row.valid), ("invalid", false));
        for method in ["draft.approve", "draft.preview"] {
            let e = f
                .call(method, json!({"account": "work", "id": "offsite-note"}))
                .expect_err("unparseable");
            assert_eq!(
                crate::error::rpc_code(&format!("{e:#}")),
                Some(-32010),
                "{method}"
            );
        }
        f.simulate("editor_save").expect("saved, still broken");
        assert_eq!(next_event(&rx).kind, KIND_DRAFT_INVALID);
    }

    #[test]
    fn a_created_draft_takes_its_name_and_a_discard_removes_the_file() {
        let (f, rx) = fixture();
        let created = f
            .call("draft.create", json!({"account": "home", "name": "note"}))
            .expect("created");
        let path = created["path"].as_str().expect("path").to_string();
        assert!(path.ends_with("/drafts/home/note.md"));
        assert_eq!(next_event(&rx).kind, KIND_DRAFT_CHANGED);
        let id = created["id"].as_str().expect("id");
        let discarded = f
            .call("draft.discard", json!({"account": "home", "id": id}))
            .expect("discarded");
        assert_eq!(discarded["status"], "draft");
        assert!(!Path::new(&path).exists());
        assert_eq!(next_event(&rx).kind, "state.remove");
    }

    #[test]
    fn a_restart_forgets_holds_and_the_journal() {
        let (f, _rx) = fixture();
        f.call(
            "message.set_flag",
            json!({"account": "work", "row_id": 1001, "flagged": true}),
        )
        .expect("flagged");
        f.simulate("restart").expect("restart");
        let b = f.call("state.bootstrap", json!({})).expect("b");
        assert_eq!(b["snapshot"]["holds"], json!([]));
        assert!(f.simulate("rollback").is_err());
    }
}
