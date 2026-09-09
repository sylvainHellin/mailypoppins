//! The four workloads, as direct in-process library calls.
//!
//! Both sides of the comparison run exactly this code: the server calls it
//! inside its dispatch, the harness calls it on its own store handle for the
//! direct figure, so the only difference the numbers can express is transport.

use anyhow::{anyhow, Context, Result};
use mailypoppins::store::read::{find_by_id, find_by_message_id, list_account, list_mailbox, load_body};
use mailypoppins::store::{BlobStore, Store};
use std::path::Path;
use std::str::FromStr;

use crate::proto::{Envelope, WorkResult};

/// The account every workload reads. The fixture has two; `alpha` is the one
/// that carries the 5000-row `Bulk` mailbox and the oversized body.
pub const ACCOUNT: &str = "alpha";

/// The ordinary `alpha/inbox` row W1 previews, named by `Message-ID` so the
/// choice survives a fixture rebuild with a different `--rows`.
pub const W1_MESSAGE_ID: &str = "<alpha-inbox-42@fixture.invalid>";

/// The 10 MiB body W4 reads.
pub const W4_MESSAGE_ID: &str = "<big-body@fixture.invalid>";

/// The mailbox W2 lists; 5000 rows at the documented fixture size.
pub const W2_MAILBOX: &str = "Bulk";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Workload {
    /// Cursor-move preview: one envelope plus its body.
    W1,
    /// The 5000-row list refetch.
    W2,
    /// Whole-account `dump-mailbox`, streamed.
    W3,
    /// The 10 MiB body.
    W4,
}

impl Workload {
    pub fn as_str(self) -> &'static str {
        match self {
            Workload::W1 => "w1",
            Workload::W2 => "w2",
            Workload::W3 => "w3",
            Workload::W4 => "w4",
        }
    }

    /// W3 is the only one whose answer is a stream: it is the workload that
    /// cannot fit a single frame at any plausible cap.
    pub fn streams(self) -> bool {
        self == Workload::W3
    }
}

impl FromStr for Workload {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "w1" => Ok(Workload::W1),
            "w2" => Ok(Workload::W2),
            "w3" => Ok(Workload::W3),
            "w4" => Ok(Workload::W4),
            other => Err(anyhow!("unknown workload {other:?}, expected w1..w4")),
        }
    }
}

/// One open store, plus the two row ids the point workloads address.
///
/// The ids are resolved once at startup so no sample pays for a `Message-ID`
/// lookup the daemon-era client would not make either: a cursor move addresses
/// a row it already holds.
pub struct Fixture {
    store: Store,
    blobs: BlobStore,
    w1_row: i64,
    w4_row: i64,
}

impl Fixture {
    /// Open the account store of a fixture directory built by
    /// `cargo run --release --example mkfixture -- --out <dir> --rows 5000`.
    pub fn open(root: &Path) -> Result<Self> {
        let dir = root.join("data").join("accounts").join(ACCOUNT);
        let db = dir.join("store.sqlite3");
        if !db.exists() {
            return Err(anyhow!(
                "no fixture store at {} (build one with: cargo run --release --example mkfixture -- --out {} --rows 5000)",
                db.display(),
                root.display()
            ));
        }
        let store = Store::open(&db).with_context(|| format!("opening {}", db.display()))?;
        let blobs = BlobStore::new(dir.join("blobs"));
        let w1_row = row_of(&store, W1_MESSAGE_ID)?;
        let w4_row = row_of(&store, W4_MESSAGE_ID)?;
        Ok(Self { store, blobs, w1_row, w4_row })
    }

    /// Run one workload, producing exactly what the transport would carry.
    pub fn run(&self, workload: Workload) -> Result<WorkResult> {
        match workload {
            Workload::W1 => self.preview(self.w1_row),
            Workload::W2 => {
                let rows = list_mailbox(&self.store, ACCOUNT, W2_MAILBOX)
                    .context("listing the Bulk mailbox")?;
                Ok(rows_result(&rows))
            }
            Workload::W3 => {
                let rows =
                    list_account(&self.store, ACCOUNT).context("listing the whole account")?;
                Ok(rows_result(&rows))
            }
            Workload::W4 => self.preview(self.w4_row),
        }
    }

    /// One envelope and its body: the shape a preview needs.
    fn preview(&self, id: i64) -> Result<WorkResult> {
        let row = find_by_id(&self.store, id)
            .with_context(|| format!("reading row {id}"))?
            .ok_or_else(|| anyhow!("row {id} vanished from the fixture"))?;
        let body = load_body(&self.store, &self.blobs, id)
            .ok_or_else(|| anyhow!("row {id} has no readable body"))?;
        Ok(WorkResult {
            rows: vec![Envelope::from(&row)],
            body: Some(body),
            chunks: 0,
            row_count: 1,
        })
    }
}

fn rows_result(rows: &[mailypoppins::store::read::MessageRow]) -> WorkResult {
    let envelopes: Vec<Envelope> = rows.iter().map(Envelope::from).collect();
    WorkResult {
        row_count: envelopes.len() as u32,
        rows: envelopes,
        body: None,
        chunks: 0,
    }
}

fn row_of(store: &Store, message_id: &str) -> Result<i64> {
    let rows = find_by_message_id(store, ACCOUNT, message_id)
        .with_context(|| format!("looking up {message_id}"))?;
    rows.first()
        .map(|r| r.id)
        .ok_or_else(|| anyhow!("the fixture has no message {message_id}"))
}
