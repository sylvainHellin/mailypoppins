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

use std::collections::BTreeMap;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};

use mp_client::events::Incoming;
use mp_core::selector::{message_key, Selector};
use mp_protocol::events::{
    KIND_MUTATIONS_ROLLED_BACK, KIND_OPERATION_FINISHED, KIND_SEND_HOLD_CANCELLED,
    KIND_SEND_HOLD_FIRED, KIND_SEND_HOLD_STARTED, KIND_SEND_HOLD_TICK, KIND_SYNC_COMPLETED,
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
];

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
    drafts: BTreeMap<String, Value>,
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
        b["snapshot"]["holds"] = json!(self.hold_listing(None).holds);
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
        let seeds: Vec<HoldStatus> = serde_json::from_value(bootstrap["snapshot"]["holds"].clone())
            .context("fixtures/bootstrap.json's holds")?;
        let mut state = State {
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
            hit_delay: HIT_DELAY,
            journal: Vec::new(),
            published: BTreeMap::new(),
            drain_requests: BTreeMap::new(),
            drain_delay: DRAIN_DELAY,
            sync_delay: SYNC_DELAY,
            holds: Vec::new(),
            hold_second: Duration::from_secs(1),
        };
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
                let Some(listing) = s.drafts.get_mut(&account) else {
                    return Err(refused(
                        method,
                        -32005,
                        &format!("account_unknown: {account}"),
                    ));
                };
                let selector = Selector::for_draft(&account, &id).to_string();
                let drafts = listing["drafts"]
                    .as_array_mut()
                    .ok_or_else(|| anyhow!("fixtures/drafts.json: no drafts array"))?;
                let Some(at) = drafts.iter().position(|d| d["id"] == id.as_str()) else {
                    return Err(refused(
                        method,
                        -32602,
                        &format!("no draft matches {selector}"),
                    ));
                };
                let status = drafts[at]["status"].clone();
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
                drafts.remove(at);
                if let Some(seeded) = s.bootstrap["snapshot"]["drafts"][&account].as_array_mut() {
                    seeded.retain(|d| d["id"] != id.as_str());
                }
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
            let draft = s.drafts["work"]["drafts"][0].clone();
            let status = HoldStatus {
                operation_id: id,
                account: "work".into(),
                draft_id: draft["id"].as_str().unwrap_or("fixture-draft").into(),
                subject: draft["subject"].as_str().unwrap_or_default().into(),
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

        f.state().drafts.get_mut("work").expect("work")["drafts"][0]["status"] = json!("approved");
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
