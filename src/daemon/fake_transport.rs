//! The daemon-side transport fake (P4-U12), in the shape of
//! [`FAKE_SYNC_OUTCOME_ENV`](super::sync_outcome::FAKE_SYNC_OUTCOME_ENV).
//!
//! `send::build_smtp_transport` is TLS-only on both of its branches, so a
//! plaintext `TcpListener` cannot serve it and a TLS one would need a
//! certificate generator this tree does not depend on. Without something in its
//! place the whole success half of the send slice - a message that goes out, a
//! draft that is retired, a Sent copy that is filed - would be unpinned, so
//! [`FAKE_TRANSPORT_ENV`] serves the SMTP submission and the Sent-mailbox
//! APPEND in process and writes one line per transport event to a log the test
//! reads.
//!
//! ```json
//! {
//!   "log": "<path>",
//!   "reject": {"carol@example.com": "550 5.1.1 unknown recipient"},
//!   "defer": {"dan@example.com": "451 4.7.1 try again later"},
//!   "down": "connection refused",
//!   "append": "ok" | "fail" | "swallow_ack",
//!   "append_delay_ms": 0
//! }
//! ```
//!
//! It is read from the environment of the process that submits, which after
//! P4-U12 is the daemon and only the daemon: the client owns no transport. The
//! pre-daemon oracle therefore cannot see it, which is why every row that uses
//! it is a routed-side assertion rather than a parity row.
//!
//! # The log is the ledger, and the server's memory
//!
//! Every event is appended with one `O_APPEND` write, so two concurrent drains
//! cannot lose a line, and [`FakeSentMailbox::search_message_id`] answers out of
//! the same file: a copy an earlier APPEND filed is found by a later search
//! even though the acknowledgement was swallowed, which is exactly the
//! ambiguity the dedup search exists for (#0116).

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::Value;

use crate::send::{RecipientResult, RecipientVerdict, SendResult};

/// The hook. Set on the daemon, never on a client.
pub const FAKE_TRANSPORT_ENV: &str = "MAILYPOPPINS_DAEMON_FAKE_TRANSPORT";

/// What the fake's APPEND does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppendMode {
    /// Files the copy and acknowledges it.
    Ok,
    /// Files nothing and reports a failure.
    Fail,
    /// Files the copy and reports a failure: the acknowledgement is lost.
    SwallowAck,
}

/// One armed fake transport.
#[derive(Debug, Clone)]
pub struct FakeTransport {
    log: Option<PathBuf>,
    rejected: Vec<(String, String)>,
    /// The recipients the server refuses for now (a 4xx), each left
    /// retryable on the outbox row.
    deferred: Vec<(String, String)>,
    /// The error every submission fails with before any recipient is
    /// asked, as a server that cannot be reached does; `None` serves them.
    down: Option<String>,
    append: AppendMode,
    append_delay: std::time::Duration,
}

/// The fake this process is running under, or `None` when the hook is unset or
/// unparseable.
///
/// An unparseable value is the same as no hook, the rule every other numeric or
/// JSON hook in the daemon follows: a process may not refuse to run over an
/// environment variable it did not understand.
pub fn fake_transport() -> Option<FakeTransport> {
    let raw = std::env::var(FAKE_TRANSPORT_ENV).ok()?;
    if raw.trim().is_empty() {
        return None;
    }
    let value: Value = serde_json::from_str(&raw).ok()?;
    let refusals = |key: &str, fallback: &str| -> Vec<(String, String)> {
        value
            .get(key)
            .and_then(Value::as_object)
            .map(|map| {
                map.iter()
                    .map(|(address, reason)| {
                        (
                            address.clone(),
                            reason.as_str().unwrap_or(fallback).to_string(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    Some(FakeTransport {
        log: value.get("log").and_then(Value::as_str).map(PathBuf::from),
        rejected: refusals("reject", "rejected"),
        deferred: refusals("defer", "deferred"),
        down: value
            .get("down")
            .and_then(Value::as_str)
            .map(str::to_string),
        append: match value.get("append").and_then(Value::as_str) {
            Some("fail") => AppendMode::Fail,
            Some("swallow_ack") => AppendMode::SwallowAck,
            _ => AppendMode::Ok,
        },
        append_delay: std::time::Duration::from_millis(
            value
                .get("append_delay_ms")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        ),
    })
}

/// Whether this process submits through the fake rather than over SMTP.
pub fn armed() -> bool {
    fake_transport().is_some()
}

impl FakeTransport {
    /// The SMTP submission: one verdict per recipient, one ledger line each,
    /// or, the server down, its error and no line.
    pub fn submit(
        &self,
        message_id: &str,
        recipients: &[(String, crate::send::RecipientRole)],
    ) -> Result<SendResult> {
        if let Some(error) = &self.down {
            anyhow::bail!("{error}");
        }
        let named = |list: &[(String, String)], address: &str| {
            list.iter()
                .find(|(named, _)| named.eq_ignore_ascii_case(address.trim()))
                .map(|(_, reason)| reason.clone())
        };
        let results = recipients
            .iter()
            .map(|(address, role)| {
                let (verdict, error) = match named(&self.rejected, address) {
                    Some(reason) => (RecipientVerdict::Rejected, Some(reason)),
                    None => match named(&self.deferred, address) {
                        Some(reason) => (RecipientVerdict::Retryable, Some(reason)),
                        None => (RecipientVerdict::Delivered, None),
                    },
                };
                self.record(&format!(
                    "submit {message_id} {address} {}",
                    match verdict {
                        RecipientVerdict::Rejected => "rejected",
                        RecipientVerdict::Retryable => "deferred",
                        _ => "accepted",
                    }
                ));
                RecipientResult {
                    address: address.clone(),
                    role: *role,
                    success: verdict == RecipientVerdict::Delivered,
                    error,
                    verdict,
                }
            })
            .collect();
        Ok(SendResult { results })
    }

    /// One event, appended whole.
    fn record(&self, line: &str) {
        let Some(path) = self.log.as_deref() else {
            return;
        };
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(file, "{line}");
        }
    }

    /// Every line the ledger holds, for the search's own lookup.
    fn ledger(&self) -> Vec<String> {
        self.log
            .as_deref()
            .and_then(|path: &Path| std::fs::read_to_string(path).ok())
            .map(|text| text.lines().map(str::to_string).collect())
            .unwrap_or_default()
    }
}

/// The Sent mailbox the fake serves, in place of `ImapSentMailbox`.
pub struct FakeSentMailbox {
    transport: FakeTransport,
}

impl FakeSentMailbox {
    pub fn new(transport: FakeTransport) -> FakeSentMailbox {
        FakeSentMailbox { transport }
    }
}

impl crate::outbox::SentMailbox for FakeSentMailbox {
    async fn search_message_id(&mut self, mailbox: &str, message_id: &str) -> Result<Vec<u32>> {
        self.transport
            .record(&format!("search {mailbox} {message_id}"));
        let filed = format!("append {mailbox} {message_id}");
        Ok(self
            .transport
            .ledger()
            .iter()
            .filter(|line| line.trim() == filed)
            // A synthetic UID: the copy's identity is its Message-ID here, and
            // the number only has to be non-empty and stable.
            .map(|_| 1u32)
            .take(1)
            .collect())
    }

    async fn append(&mut self, mailbox: &str, raw: &[u8]) -> Result<Option<u32>> {
        let message_id = crate::send::message_id_of(raw);
        if self.transport.append == AppendMode::Fail {
            anyhow::bail!("the fake Sent mailbox refused the APPEND");
        }
        // Recorded before the park, because the server has the copy from the
        // moment it takes it: a drain that dies in the window below has already
        // filed it, and that is the state the dedup search must find.
        self.transport
            .record(&format!("append {mailbox} {message_id}"));
        if !self.transport.append_delay.is_zero() {
            tokio::time::sleep(self.transport.append_delay).await;
        }
        if self.transport.append == AppendMode::SwallowAck {
            anyhow::bail!("the fake Sent mailbox filed the copy and lost the acknowledgement");
        }
        Ok(Some(1))
    }
}
