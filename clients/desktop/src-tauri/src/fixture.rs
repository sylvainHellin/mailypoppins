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
//! frontend's connection and resync screens are testable.
//!
//! The HTML bodies get the daemon's CSP meta tag prepended like a rendition
//! does, but their `<meta http-equiv="refresh">` is deliberately **not**
//! stripped: the hostile fixture exercises the reader's own defences.

use std::collections::BTreeMap;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};

use mp_client::events::Incoming;
use mp_protocol::state::Bootstrap;
use mp_protocol::EventEnvelope;

use crate::reader::MESSAGE_CSP;

const BOOTSTRAP: &str = include_str!("../../fixtures/bootstrap.json");
const ACCOUNTS: &str = include_str!("../../fixtures/accounts.json");
const MESSAGES: &str = include_str!("../../fixtures/messages.json");
const DRAFTS: &str = include_str!("../../fixtures/drafts.json");
const HTML: &str = include_str!("../../fixtures/html.json");

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
];

struct State {
    bootstrap: Value,
    accounts: Value,
    /// account -> mailbox slug -> rows, newest first.
    messages: BTreeMap<String, BTreeMap<String, Vec<Value>>>,
    drafts: BTreeMap<String, Value>,
    html: BTreeMap<i64, String>,
    instance: u32,
    revision: u64,
    down: bool,
    next_op: u64,
    next_row: i64,
    operations: BTreeMap<String, Value>,
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
            .map_or(0, |d| d["drafts"].as_array().map_or(0, Vec::len) as u64);
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
        b
    }
}

/// The daemon stand-in.
pub struct Fixture {
    state: Mutex<State>,
    events: Mutex<Sender<Incoming>>,
}

fn refused(method: &str, code: i32, message: &str) -> anyhow::Error {
    anyhow!("{method}: the daemon refused the call: {message} ({code})")
}

fn param_str<'a>(method: &str, params: &'a Value, key: &str) -> Result<&'a str> {
    params[key]
        .as_str()
        .ok_or_else(|| refused(method, -32602, &format!("missing string param `{key}`")))
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
        let drafts: BTreeMap<String, Value> =
            serde_json::from_str(DRAFTS).context("fixtures/drafts.json")?;
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
        Ok(Fixture {
            state: Mutex::new(State {
                bootstrap,
                accounts,
                messages,
                drafts,
                html,
                instance: 1,
                revision,
                down: false,
                next_op: 1,
                next_row,
                operations: BTreeMap::new(),
            }),
            events: Mutex::new(events),
        })
    }

    fn state(&self) -> MutexGuard<'_, State> {
        match self.state.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        }
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
        let envelope = {
            let mut s = self.state();
            s.revision += 1;
            EventEnvelope {
                instance_id: s.instance_id(),
                revision: s.revision,
                kind: kind.to_string(),
                payload,
            }
        };
        self.post(Incoming::Event(envelope));
    }

    /// Answer one wire method.
    pub fn call(self: &Arc<Self>, method: &str, params: Value) -> Result<Value> {
        let s = self.state();
        if s.down {
            return Err(anyhow!("{method}: the daemon is not reachable"));
        }
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
                s.drafts
                    .get(account)
                    .cloned()
                    .ok_or_else(|| refused(method, -32005, &format!("account_unknown: {account}")))
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
            other => Err(refused(other, -32601, "method not found")),
        }
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
        std::thread::spawn(move || {
            let total = hits.len();
            for hit in hits {
                std::thread::sleep(Duration::from_millis(150));
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

    /// Drive one connection state; see [`SIMULATIONS`].
    pub fn simulate(&self, what: &str) -> Result<()> {
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
                    "unknown simulation `{other}`; one of {}",
                    SIMULATIONS.join(", ")
                ))
            }
        }
        Ok(())
    }
}

/// A fixture row as `message.list` sends it.
fn wire_row(row: &Value) -> Value {
    let mut row = row.clone();
    if let Some(obj) = row.as_object_mut() {
        for key in FIXTURE_ONLY_KEYS {
            obj.remove(*key);
        }
    }
    row
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
        "message_id": row["message_id"], "from": row["from"], "to": row["to"], "cc": row["cc"],
        "subject": row["subject"], "date": row["date_display"], "flags": flags,
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
        "row_id": row["id"], "selector": row["selector"], "from": row["from"], "to": row["to"],
        "cc": row["cc"], "reply_to": null, "bcc": null, "subject": row["subject"],
        "date_display": row["date_display"], "date_sort": row["date_sort"], "flags": row["flags"],
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
        assert_eq!(messages, 20);
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
}
