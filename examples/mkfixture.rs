//! Deterministic offline fixture for the pre-daemon benchmark workloads.
//!
//! Builds a self-contained data root plus the `config.toml` that names its
//! accounts, so the installed `mp` can be pointed at it with two environment
//! variables and nothing else. Every message goes in through the real
//! [`ingest_message`] API, so the rows are the rows a sync writes.
//!
//! Determinism is the whole point: a fixed seed, fixed dates, fixed message
//! ids and a fixed body vocabulary, so two runs into two directories produce
//! byte-identical `mp dump-mailbox --json` output and a measurement taken
//! today can be compared with one taken after the daemon lands. The command
//! line and the workloads that consume the fixture are documented in
//! `docs/baselines/pre-daemon/workloads.md`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result};
use chrono::{FixedOffset, TimeZone};
use clap::Parser;
use mailypoppins::ingest::{ingest_message, IngestInput};
use mailypoppins::parse::{AttachmentData, FetchedEmail};
use mailypoppins::store::{BlobStore, Store};
use mailypoppins::types::MessageFlags;

/// Seed of the one generator. Changing it changes every body, so it is a
/// deliberate edit that invalidates every recorded measurement.
const SEED: u64 = 0x6D61696C_79706F70;

/// First message date; message `n` of a mailbox is this plus `n` minutes.
const EPOCH: &str = "2026-01-01T09:00:00+01:00";

/// Rare token planted in every 250th bulk body, so W7 has a stable hit count.
const NEEDLE: &str = "zolvertrix";

/// Fixed vocabulary the bodies are drawn from.
const WORDS: [&str; 24] = [
    "invoice", "schedule", "review", "tender", "concrete", "survey", "permit", "handover",
    "budget", "revision", "contractor", "milestone", "sample", "defect", "warranty", "site",
    "drawing", "estimate", "quantity", "steel", "insulation", "inspection", "signature", "ledger",
];

#[derive(Parser)]
#[command(about = "Generate the deterministic pre-daemon benchmark fixture")]
struct Args {
    /// Directory to build the fixture in; must not already hold one, since an
    /// existing store is reused rather than replaced.
    #[arg(long)]
    out: PathBuf,
    /// Messages in the large `Bulk` mailbox.
    #[arg(long, default_value_t = 5000)]
    rows: usize,
    /// Size of the one oversized body, in MiB.
    #[arg(long, default_value_t = 10)]
    big_mb: usize,
}

/// The whole generator's randomness: a 64-bit LCG, so the fixture depends on
/// no crate that is not already a dependency and cannot drift with one.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 33) as u32
    }

    fn pick<'a>(&mut self, from: &[&'a str]) -> &'a str {
        from[self.next() as usize % from.len()]
    }

    fn range(&mut self, lo: usize, hi: usize) -> usize {
        lo + self.next() as usize % (hi - lo)
    }
}

/// One mailbox to fill: its store key and how many messages it gets.
struct Plan { account: &'static str, mailbox: &'static str, count: usize }

fn main() -> Result<()> {
    let args = Args::parse();
    let started = Instant::now();

    let data = args.out.join("data");
    let config = args.out.join("config");
    fs::create_dir_all(&config).context("creating the config dir")?;
    write_config(&config.join("config.toml"))?;

    // N accounts, M mailboxes, K messages, fixed so the fixture is one artifact
    // rather than a family of them; only `--rows` and `--big-mb` move.
    let plans = [
        Plan { account: "alpha", mailbox: "inbox", count: 200 },
        Plan { account: "alpha", mailbox: "sent", count: 50 },
        Plan { account: "alpha", mailbox: "archive", count: 100 },
        Plan { account: "alpha", mailbox: "Bulk", count: args.rows },
        Plan { account: "beta", mailbox: "inbox", count: 100 },
        Plan { account: "beta", mailbox: "archive", count: 50 },
    ];

    // One store handle per account, opened in a fixed order.
    let mut stores: BTreeMap<&str, (Store, BlobStore)> = BTreeMap::new();
    let mut rng = Lcg(SEED);
    let mut total = 0usize;
    for plan in &plans {
        use std::collections::btree_map::Entry;
        let entry = match stores.entry(plan.account) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => {
                let dir = data.join("accounts").join(plan.account);
                fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
                let store = Store::open(dir.join("store.sqlite3"))
                    .with_context(|| format!("opening the store of {}", plan.account))?;
                e.insert((store, BlobStore::new(dir.join("blobs"))))
            }
        };
        let (store, blobs) = (&entry.0, &entry.1);
        for n in 0..plan.count {
            let uid = n as i64 + 1;
            let email = message(&mut rng, plan, n);
            ingest_message(
                store,
                blobs,
                &IngestInput { account: plan.account, mailbox: plan.mailbox, uid, email: &email, raw: None },
            )
            .with_context(|| format!("ingesting {}/{} uid {uid}", plan.account, plan.mailbox))?;
            total += 1;
        }
    }

    // The oversized body, last so its uid does not shift when --rows changes.
    let (store, blobs) = stores.get("alpha").expect("alpha was planned");
    let big = big_message(args.big_mb);
    ingest_message(
        store,
        blobs,
        &IngestInput { account: "alpha", mailbox: "inbox", uid: 900_001, email: &big, raw: None },
    )
    .context("ingesting the oversized body")?;
    total += 1;
    drop(stores);

    let elapsed = started.elapsed();
    let bytes = tree_size(&data);
    println!("fixture written to {}", args.out.display());
    println!("  accounts:  2 (alpha, beta)");
    println!("  mailboxes: {}", plans.len());
    println!("  messages:  {total} ({} in alpha/Bulk, 1 body of {} MiB)", args.rows, args.big_mb);
    println!("  needles:   {} bodies carry '{NEEDLE}'", args.rows / 250);
    println!("  build:     {:.2} s", elapsed.as_secs_f64());
    println!("  store:     {bytes} bytes ({:.1} MiB)", bytes as f64 / (1024.0 * 1024.0));
    println!();
    println!("export MAILYPOPPINS_CONFIG_DIR={}", config.display());
    println!("export MAILYPOPPINS_DATA_DIR={}", data.display());
    Ok(())
}

/// The config the fixture accounts need to be visible to `mp`.
fn write_config(path: &Path) -> Result<()> {
    let toml = r#"# Generated by examples/mkfixture.rs. Offline fixture: no server is ever
# contacted, the hosts below do not resolve.

[[accounts]]
name = "alpha"
default_from = "alpha@fixture.invalid"

[accounts.mailboxes.inbox]
server = "INBOX"

[accounts.mailboxes.sent]
server = "Sent"

[accounts.mailboxes.archive]
server = "Archive"

[[accounts.mailboxes.extra]]
server = "Bulk"

[[accounts]]
name = "beta"
default_from = "beta@fixture.invalid"

[accounts.mailboxes.inbox]
server = "INBOX"

[accounts.mailboxes.archive]
server = "Archive"
"#;
    fs::write(path, toml).with_context(|| format!("writing {}", path.display()))
}

/// One generated message. Everything about it is a function of the plan and
/// the sequence number, so the same call produces the same message forever.
fn message(rng: &mut Lcg, plan: &Plan, n: usize) -> FetchedEmail {
    let seq = n + 1;
    let subject = format!("{} {} {seq}", title(rng.pick(&WORDS)), rng.pick(&WORDS));
    let mut body = String::new();
    for _ in 0..rng.range(3, 9) {
        for _ in 0..rng.range(8, 24) {
            body.push_str(rng.pick(&WORDS));
            body.push(' ');
        }
        body.push_str("\n\n");
    }
    if plan.mailbox == "Bulk" && seq % 250 == 0 {
        body.push_str(NEEDLE);
        body.push('\n');
    }

    let attach = seq % 17 == 0;
    FetchedEmail {
        from: format!("Sender {seq} <sender{seq}@fixture.invalid>"),
        to: format!("{}@fixture.invalid", plan.account),
        cc: (seq % 11 == 0).then(|| "cc@fixture.invalid".to_string()),
        reply_to: None,
        bcc: None,
        subject,
        date: date_of(n),
        body_text: body,
        html_body: None,
        has_attachments: attach,
        message_id: Some(format!(
            "<{}-{}-{seq}@fixture.invalid>",
            plan.account,
            plan.mailbox.to_lowercase()
        )),
        attachments: if attach {
            vec![AttachmentData {
                filename: format!("attachment-{seq}.txt"),
                content: format!("attachment payload {seq}\n").into_bytes(),
                content_id: None,
            }]
        } else {
            Vec::new()
        },
        flags: MessageFlags::seen(seq % 3 == 0),
        calendar_ics: None,
        event: None,
    }
}

/// The W4 subject: one message whose body blob is `mb` MiB exactly.
fn big_message(mb: usize) -> FetchedEmail {
    let line = "The quick brown fox jumps over the lazy dog. 0123456789 abcdef\n";
    let want = mb * 1024 * 1024;
    let mut body = String::with_capacity(want);
    while body.len() + line.len() <= want {
        body.push_str(line);
    }
    while body.len() < want {
        body.push('.');
    }
    FetchedEmail {
        from: "Bulk Sender <big@fixture.invalid>".into(),
        to: "alpha@fixture.invalid".into(),
        cc: None,
        reply_to: None,
        bcc: None,
        subject: format!("Oversized body {mb} MiB"),
        date: date_of(900_000),
        body_text: body,
        html_body: None,
        has_attachments: false,
        message_id: Some("<big-body@fixture.invalid>".into()),
        attachments: Vec::new(),
        flags: MessageFlags::seen(true),
        calendar_ics: None,
        event: None,
    }
}

/// `EPOCH` plus `n` minutes, as an RFC 2822 `Date:` header.
///
/// Fixed offset rather than a local timezone: the header, and so the sort key
/// the dump prints, must not depend on where the fixture is built.
fn date_of(n: usize) -> String {
    let base = chrono::DateTime::parse_from_rfc3339(EPOCH).expect("EPOCH parses");
    let tz = FixedOffset::east_opt(3600).expect("+01:00");
    tz.from_utc_datetime(&(base.naive_utc() + chrono::Duration::minutes(n as i64)))
        .to_rfc2822()
}

fn title(word: &str) -> String {
    let mut c = word.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

fn tree_size(root: &Path) -> u64 {
    walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter_map(|e| e.metadata().ok())
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .sum()
}
