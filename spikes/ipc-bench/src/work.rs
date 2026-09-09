//! The four workloads, as direct in-process library calls.
//!
//! Both sides of the comparison run exactly this code: the server calls it
//! inside its dispatch, the harness calls it on its own store handle for the
//! direct figure, so the only difference the numbers can express is transport.

use anyhow::{anyhow, Context, Result};
use mailypoppins::store::read::{
    find_by_id, find_by_message_id, list_account, list_mailbox, load_body, CALENDAR_SIDECAR_NAME,
};
use mailypoppins::store::{BlobStore, Store};
use std::path::Path;
use std::str::FromStr;

use crate::proto::{CompactEnvelope, Envelope, WorkResult, PAGE_ROWS};

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

/// Where `message.jump_to_date` lands: the middle of the 5000 rows, so the
/// answer is neither the first page (which paging holds already) nor the last.
pub const JUMP_OFFSET: usize = 2500;

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
    /// P1a-U3, option A: the whole 5000-row list in the compact encoding.
    Whole,
    /// P1a-U3, option B: one 200-row page of the same list, compact.
    Page,
    /// Option B's total count, which the client can no longer compute itself.
    Count,
    /// Option B's `message.jump_to_date`: the index of the first row at or
    /// before a date, plus the page the cursor lands in.
    Jump,
    /// Option B's `message.filter`: the metadata filter (unread), answered as a
    /// match count plus the first page of matches.
    Filter,
    /// Option B's `message.select_all`: every row id of the current view.
    SelectAll,
}

impl Workload {
    pub fn as_str(self) -> &'static str {
        match self {
            Workload::W1 => "w1",
            Workload::W2 => "w2",
            Workload::W3 => "w3",
            Workload::W4 => "w4",
            Workload::Whole => "whole",
            Workload::Page => "page",
            Workload::Count => "count",
            Workload::Jump => "jump",
            Workload::Filter => "filter",
            Workload::SelectAll => "select_all",
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
            "whole" => Ok(Workload::Whole),
            "page" => Ok(Workload::Page),
            "count" => Ok(Workload::Count),
            "jump" => Ok(Workload::Jump),
            "filter" => Ok(Workload::Filter),
            "select_all" => Ok(Workload::SelectAll),
            other => Err(anyhow!(
                "unknown workload {other:?}, expected w1..w4 or whole|page|count|jump|filter|select_all"
            )),
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
    /// The `date_sort` `message.jump_to_date` asks for, resolved once so the
    /// jump is a fixed point of the fixture and not a moving target.
    jump_date_sort: i64,
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
        let jump_date_sort = jump_target(&store)?;
        Ok(Self { store, blobs, w1_row, w4_row, jump_date_sort })
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
            Workload::Whole => {
                let rows = list_mailbox(&self.store, ACCOUNT, W2_MAILBOX)
                    .context("listing the Bulk mailbox")?;
                let compact: Vec<CompactEnvelope> =
                    rows.iter().map(CompactEnvelope::from).collect();
                Ok(WorkResult {
                    row_count: compact.len() as u32,
                    compact,
                    ..WorkResult::default()
                })
            }
            Workload::Page => self.page(0, None),
            Workload::Count => {
                let total = self.count(None)?;
                Ok(WorkResult { total: Some(total), ..WorkResult::default() })
            }
            Workload::Jump => {
                // The index is what the client needs to place the cursor, and
                // with paging it is a server-side count: the client no longer
                // holds the rows above the target.
                let index: i64 = self.store.conn().query_row(
                    "SELECT COUNT(*) FROM messages \
                     WHERE account = ?1 AND mailbox = ?2 AND date_sort > ?3",
                    (ACCOUNT, W2_MAILBOX, self.jump_date_sort),
                    |row| row.get(0),
                )?;
                let offset = (index as usize / PAGE_ROWS) * PAGE_ROWS;
                let mut result = self.page(offset, None)?;
                result.index = Some(index as u32);
                Ok(result)
            }
            Workload::Filter => {
                let matches = self.count(Some(UNREAD))?;
                let mut result = self.page(0, Some(UNREAD))?;
                result.total = Some(matches);
                Ok(result)
            }
            Workload::SelectAll => {
                let mut stmt = self.store.conn().prepare(
                    "SELECT id FROM messages WHERE account = ?1 AND mailbox = ?2 \
                     ORDER BY date_sort DESC, id DESC",
                )?;
                let mut ids = Vec::with_capacity(5000);
                let mut rows = stmt.query((ACCOUNT, W2_MAILBOX))?;
                while let Some(row) = rows.next()? {
                    ids.push(row.get::<_, i64>(0)?);
                }
                Ok(WorkResult {
                    row_count: ids.len() as u32,
                    ids,
                    ..WorkResult::default()
                })
            }
        }
    }

    /// One page of the mailbox listing, newest first, in the compact encoding.
    ///
    /// The SQL mirrors `list_mailbox_sql` (same columns, same invite join, same
    /// `messages_list`-served ORDER BY) with a `LIMIT`/`OFFSET` added, because
    /// the product read path has no paged variant and the spike may not add one
    /// to `src/`. Reading the columns in `CompactEnvelope` order avoids a
    /// `MessageRow` hop the paged option would not make either.
    fn page(&self, offset: usize, predicate: Option<&str>) -> Result<WorkResult> {
        let sql = format!(
            "{} {} ORDER BY messages.date_sort DESC, messages.id DESC LIMIT ?3 OFFSET ?4",
            page_select(),
            predicate.map(|p| format!("AND {p}")).unwrap_or_default()
        );
        let mut stmt = self.store.conn().prepare_cached(&sql)?;
        let mut rows = stmt.query((ACCOUNT, W2_MAILBOX, PAGE_ROWS as i64, offset as i64))?;
        let mut compact = Vec::with_capacity(PAGE_ROWS);
        while let Some(row) = rows.next()? {
            compact.push(CompactEnvelope(
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
                row.get(8)?,
                row.get(9)?,
                row.get(10)?,
                row.get(11)?,
                row.get::<_, i64>(12)? != 0,
                row.get(13)?,
                row.get::<_, i64>(14)? != 0,
            ));
        }
        Ok(WorkResult {
            row_count: compact.len() as u32,
            compact,
            ..WorkResult::default()
        })
    }

    /// Rows in the current view, optionally under a filter predicate.
    fn count(&self, predicate: Option<&str>) -> Result<u32> {
        let sql = format!(
            "SELECT COUNT(*) FROM messages WHERE account = ?1 AND mailbox = ?2 {}",
            predicate.map(|p| format!("AND {p}")).unwrap_or_default()
        );
        let total: i64 =
            self.store.conn().query_row(&sql, (ACCOUNT, W2_MAILBOX), |row| row.get(0))?;
        Ok(total as u32)
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
            row_count: 1,
            ..WorkResult::default()
        })
    }
}

fn rows_result(rows: &[mailypoppins::store::read::MessageRow]) -> WorkResult {
    let envelopes: Vec<Envelope> = rows.iter().map(Envelope::from).collect();
    WorkResult {
        row_count: envelopes.len() as u32,
        rows: envelopes,
        ..WorkResult::default()
    }
}

/// The metadata filter the TUI applies over its in-memory list today: unread.
const UNREAD: &str = "(messages.flags IS NULL OR messages.flags NOT LIKE '%\\Seen%')";

/// The column list and the FROM/WHERE of a paged listing, in
/// [`CompactEnvelope`] order.
fn page_select() -> String {
    format!(
        "SELECT messages.id, messages.mailbox, messages.uid, messages.message_id, \
         messages.from_, messages.to_, messages.cc, messages.reply_to, messages.bcc, \
         messages.subject, messages.date_display, messages.flags, \
         messages.has_attachments, messages.thread_id, \
         (invite.message_row IS NOT NULL) \
         FROM messages \
         LEFT JOIN (SELECT DISTINCT message_row FROM message_blobs \
                     WHERE kind = 'attachment' \
                       AND filename = '{CALENDAR_SIDECAR_NAME}') invite \
           ON invite.message_row = messages.id \
         WHERE messages.account = ?1 AND messages.mailbox = ?2"
    )
}

/// The `date_sort` of the row at [`JUMP_OFFSET`], so `jump` addresses a date
/// the fixture actually holds.
fn jump_target(store: &Store) -> Result<i64> {
    let value: i64 = store.conn().query_row(
        "SELECT date_sort FROM messages WHERE account = ?1 AND mailbox = ?2 \
         ORDER BY date_sort DESC, id DESC LIMIT 1 OFFSET ?3",
        (ACCOUNT, W2_MAILBOX, JUMP_OFFSET as i64),
        |row| row.get(0),
    )?;
    Ok(value)
}

fn row_of(store: &Store, message_id: &str) -> Result<i64> {
    let rows = find_by_message_id(store, ACCOUNT, message_id)
        .with_context(|| format!("looking up {message_id}"))?;
    rows.first()
        .map(|r| r.id)
        .ok_or_else(|| anyhow!("the fixture has no message {message_id}"))
}
