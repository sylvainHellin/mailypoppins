//! Soak rows: sustained client churn, syncs, searches, draft writes, holds and
//! overflow against one live daemon (#0125, plan unit P6-U9).
//!
//! The Phase 6 exit gate asks for two things no functional test proves: that
//! *the daemon can run unattended across client churn*, and that *memory
//! remains bounded under slow clients and repeated syncs*. Both are statements
//! about what a process accumulates while it keeps answering, so every row
//! here measures the daemon from the outside - `/proc/<pid>/fd` for
//! descriptors, `/proc/<pid>/status` `VmRSS` for memory, `diagnostic.health`
//! for what the daemon says about itself - takes a reading before the work and
//! one after it, and asserts the difference against a pinned ceiling.
//!
//! # The rows
//!
//! | row | work | what it pins |
//! |---|---|---|
//! | a | 200 connect/bootstrap/disconnect cycles from 8 threads | descriptors and `clients` return to their baseline |
//! | b | one client that never reads, under an event burst, while four drain | RSS bounded, exactly one `resync`, every `sync.completed` delivered |
//! | c | 200 `sync.quick` passes on one account | RSS bounded, `operations.active` back to 0, `store.size_bytes` stable |
//! | d | 300 `message.list` / `message.search` calls from 4 clients | no error frame, p95 under a ceiling, RSS bounded |
//! | e | 100 draft files written then deleted under the watched directory | one event per draft either way, descriptors stable |
//! | f | 50 held sends armed and cancelled from alternating clients | `holds` back to 0, the ledger empty, the draft still approved |
//! | g | all of the above at once for `MAILYPOPPINS_SOAK_SECS` | RSS and descriptors stable, and a clean `daemon.stop` |
//!
//! # Bounded by default, long on request
//!
//! Every row is sized so the ordinary suite pays seconds rather than minutes:
//! the counts above are what a plain `cargo test` runs, and row (g) runs for
//! ten seconds. `MAILYPOPPINS_SOAK_SECS=<n>` is the owner's knob: row (g)
//! runs for `n` seconds and every other row multiplies its counts by
//! `n / 10`, so one variable turns a three-minute file into the hour-long run
//! a release wants. The command is in `docs/daemon-operations.md`.
//!
//! A soak that only fails is worth little, so every row **prints** what it
//! measured - descriptors before and after, RSS before and after, the p95, the
//! counts - and `cargo test --test daemon_soak -- --nocapture` is how a run
//! leaves evidence. The numbers from the run that landed this file are in
//! `docs/baselines/phase6-soak.md`.
//!
//! # The fixture
//!
//! All seven rows run against an `examples/mkfixture --rows 5000` root: two
//! accounts, six mailboxes, 5501 messages, one 10 MiB body. The generator is
//! `include!`d rather than copied, so the soak reads the same store the
//! benchmark workloads do (`docs/baselines/pre-daemon/workloads.md`) and
//! cannot drift from it. Building it costs about twenty seconds, so it is
//! built **once per machine** into `$TMPDIR/mp-soak-fixture-v1`, under a
//! `flock` so parallel rows build it once, and each row copies that template
//! into its own temporary root: two daemons cannot share one, since the socket
//! lives under the data directory.
//!
//! Two things are added to what `mkfixture` writes, both baked into the
//! template so every row sees the same root:
//!
//! - an `[accounts.imap]` host for `alpha`, because an account that configures
//!   no server is `local_only` and `sync.quick` refuses it with `-32006`
//!   before any pass runs. With a host that does not resolve the pass runs,
//!   fails on the missing IMAP secret in about ten milliseconds, and commits
//!   the `sync.completed` outcome row (c) and row (b) count. That is the real
//!   sync path, not the `MAILYPOPPINS_DAEMON_FAKE_SYNC_OUTCOME` hook: the hook
//!   fires once per `state.bootstrap` and cannot be driven 200 times without
//!   200 bootstraps, each of which would also fire the event burst.
//! - `[email] send_hold_secs`, since row (f) needs a window to arm, plus one
//!   approved draft for it to arm on.
//!
//! # Why an overflow row needs the burst hook
//!
//! Row (b)'s slow client has to overflow its outbound queue, and the queue
//! holds 512 events or 4 MiB (`tests/daemon_events.rs`), behind a kernel
//! socket buffer that absorbs another thousand or so. Fifty sync outcomes
//! never reach either bound, so the row arms
//! `MAILYPOPPINS_DAEMON_FAKE_EVENT_BURST`, whose changes fill the queue with
//! domain events the way a real sync storm would. The assertion the row is
//! about is what the daemon does *then*: discard the domain events, poison the
//! queue once, and keep delivering the lifecycle events - which is what a
//! `sync.completed` is - to everybody, the poisoned client included.

mod support;

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use mp_client::{ClientInfo, ClientKind, Connection, Identity};

use support::parity::{socket_path, DaemonFixture};

// ---------------------------------------------------------------------------
// The knobs
// ---------------------------------------------------------------------------

/// The owner's knob: row (g) runs this many seconds and every other row scales
/// its counts by `secs / DEFAULT_SOAK_SECS`.
const SOAK_SECS_ENV: &str = "MAILYPOPPINS_SOAK_SECS";

/// What row (g) costs when nobody asked for more.
const DEFAULT_SOAK_SECS: u64 = 10;

/// How much the daemon's resident set may grow across one row.
///
/// What it catches is a leak, which is unbounded, not a working set, which is
/// not. Measured on this host (`docs/baselines/phase6-soak.md`): rows (a),
/// (c), (e) and (f) move it by under 3 MiB, row (b) by 3 to 13, row (d) by 30
/// whether it runs 300 reads or 5400, and row (g) by 59 to 96 whether it runs
/// ten seconds or three minutes. The ceiling is twice the worst of those.
///
/// A ceiling alone is a poor leak detector, which is why every row prints its
/// growth: the one leak this unit found - an operation registry that never
/// forgot a settled operation, `src/daemon/operations.rs` - showed up as row
/// (c)'s growth rising with the number of passes (0.6 MiB at 200, 2.7 MiB at
/// 3600) long before any absolute number looked wrong. Read the numbers across
/// two scales; the assertion is the backstop.
const RSS_CEILING_BYTES: u64 = 192 * 1024 * 1024;

/// How many descriptors a row may leave behind, once the daemon is warm.
///
/// Not zero: an epoll registration or a connection being torn down can
/// legitimately differ by one or two between two readings taken at different
/// moments. A leak per cycle shows up as hundreds, never as four.
///
/// The number is small because every row takes its baseline through
/// [`warmed_baseline`]. A daemon that has just bound its socket is **not** at
/// its working set: its account runtimes are still coming up, and SQLite opens
/// a further handle per concurrent reader and keeps it. Measured on this host,
/// a fresh daemon holds 22 descriptors, the first few dozen concurrent clients
/// take it to 37, and four thousand more leave it at 37. Against a warm
/// baseline the rows here drift by two at two hundred cycles and by six at
/// twelve hundred, so the margin is twice the worst of those and a per-cycle
/// leak would be two hundred.
const FD_MARGIN: usize = 12;

/// The p95 ceiling for one read call in row (d).
///
/// A read of this fixture is a millisecond or two; the ceiling is two orders
/// of magnitude above it because what it is looking for is a lock convoy - one
/// client's read waiting behind three others - and a convoy costs hundreds of
/// milliseconds, not tens.
const P95_CEILING: Duration = Duration::from_millis(250);

/// How far `store.size_bytes` may move across a row that ingests nothing.
///
/// SQLite's WAL and its freelist move a little on any open; a row of 200
/// failing syncs must not move it at all beyond that.
const STORE_MARGIN_BYTES: i64 = 4 * 1024 * 1024;

/// Upper bound on any wait in this file. A ceiling, never a sleep.
const DEADLINE: Duration = Duration::from_secs(60);

/// Poll interval for the bounded waits.
const TICK: Duration = Duration::from_millis(25);

/// The account every row drives.
const ACCOUNT: &str = "alpha";

/// The second account, so a row can move between stores.
const OTHER_ACCOUNT: &str = "beta";

/// The approved draft the template seeds for row (f).
const HOLD_DRAFT: &str = "50a4000000000001";

/// The undo-send window the template configures, in seconds.
///
/// Long enough that no armed hold in row (f) can fire before the row cancels
/// it, so a fired send is a failure rather than a race.
const HOLD_SECS: u64 = 300;

/// `MAILYPOPPINS_DAEMON_FAKE_EVENT_BURST`, row (b)'s only hook.
const FAKE_EVENT_BURST_ENV: &str = "MAILYPOPPINS_DAEMON_FAKE_EVENT_BURST";

/// `MAILYPOPPINS_DAEMON_WATCH_POLL_MS` and its debounce, so row (e) waits
/// milliseconds for a draft rather than the 1.3 s the defaults cost.
const WATCH_POLL_ENV: &str = "MAILYPOPPINS_DAEMON_WATCH_POLL_MS";
const WATCH_DEBOUNCE_ENV: &str = "MAILYPOPPINS_DAEMON_WATCH_DEBOUNCE_MS";

/// Changes one bootstrap commits in row (b).
///
/// Above the 512-event queue plus the kernel socket buffer by a wide margin,
/// which is what makes the overflow deterministic. It overflows the readers
/// too, and that is not a defect of the number: the hook commits its changes
/// in a tight loop off the bootstrap's path, so the daemon queues them faster
/// than any client drains them, at 2000 as at 4000. What the row is about is
/// what the daemon does with a queue that cannot be kept up with, and the
/// answer has to be the same for a reader as for a stalled client: discard the
/// domain events, ask for a resync once, and keep delivering the lifecycle
/// events. That a stalled reader does not delay another client's round trip is
/// `tests/daemon_events.rs`'s row, not this one's.
const BURST: u64 = 4_000;

/// How many seconds row (g) runs, and the multiplier every other row applies
/// to its counts.
fn soak_secs() -> u64 {
    std::env::var(SOAK_SECS_ENV)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|secs| *secs > 0)
        .unwrap_or(DEFAULT_SOAK_SECS)
}

/// `1` for an ordinary run, `n / 10` for an owner's run.
fn scale() -> usize {
    (soak_secs() / DEFAULT_SOAK_SECS).max(1) as usize
}

// ---------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------

/// The benchmark fixture generator, in process.
///
/// A faithful copy of `examples/mkfixture.rs`: the same seed, the same epoch,
/// the same six mailbox plans, the same vocabulary, the same needle, the same
/// `config.toml`, so the soak reads the store the benchmark workloads read
/// (`docs/baselines/pre-daemon/workloads.md`) rather than a shape of its own.
///
/// A copy and not the file itself because neither way of reusing it compiles:
/// `include!` refuses the example's leading `//!` block (`E0753`, an inner doc
/// comment cannot come out of a macro expansion), and `#[path]` would make
/// every item of it private to a module this one cannot reach into. Spawning
/// the built example is no better - `cargo test --test daemon_soak` does not
/// build examples, and a nested `cargo` blocks on the build lock the outer
/// `cargo test` holds.
///
/// [`assert_fixture_shape`] is the guard against the two drifting: it asserts
/// the counts and the needle hits this module produced, which is everything
/// the rows depend on.
///
/// The lint allowance is there for the same reason: `seq % 17 == 0` is what
/// the example writes, and rewriting it as `is_multiple_of` here would make
/// the two files diff noisily for no behavioural gain.
#[allow(clippy::manual_is_multiple_of)]
mod mkfixture {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::Path;

    use chrono::{FixedOffset, TimeZone};

    use mailypoppins::ingest::{ingest_message, IngestInput};
    use mailypoppins::parse::{AttachmentData, FetchedEmail};
    use mailypoppins::store::{BlobStore, Store};
    use mailypoppins::types::MessageFlags;

    /// Seed of the one generator, the example's.
    const SEED: u64 = 0x6D61696C_79706F70;

    /// First message date; message `n` of a mailbox is this plus `n` minutes.
    const EPOCH: &str = "2026-01-01T09:00:00+01:00";

    /// Rare token planted in every 250th bulk body.
    pub const NEEDLE: &str = "zolvertrix";

    /// Fixed vocabulary the bodies are drawn from.
    const WORDS: [&str; 24] = [
        "invoice",
        "schedule",
        "review",
        "tender",
        "concrete",
        "survey",
        "permit",
        "handover",
        "budget",
        "revision",
        "contractor",
        "milestone",
        "sample",
        "defect",
        "warranty",
        "site",
        "drawing",
        "estimate",
        "quantity",
        "steel",
        "insulation",
        "inspection",
        "signature",
        "ledger",
    ];

    /// A 64-bit LCG, so the fixture depends on no crate that is not already a
    /// dependency and cannot drift with one.
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
    struct Plan {
        account: &'static str,
        mailbox: &'static str,
        count: usize,
    }

    /// `mkfixture --out <out> --rows <rows> --big-mb <big_mb>`.
    pub fn build(out: &Path, rows: usize, big_mb: usize) {
        use std::collections::btree_map::Entry;

        let data = out.join("data");
        let config = out.join("config");
        fs::create_dir_all(&config).unwrap_or_else(|e| panic!("create the config dir: {e}"));
        fs::write(config.join("config.toml"), CONFIG)
            .unwrap_or_else(|e| panic!("write the fixture config.toml: {e}"));

        let plans = [
            Plan {
                account: "alpha",
                mailbox: "inbox",
                count: 200,
            },
            Plan {
                account: "alpha",
                mailbox: "sent",
                count: 50,
            },
            Plan {
                account: "alpha",
                mailbox: "archive",
                count: 100,
            },
            Plan {
                account: "alpha",
                mailbox: "Bulk",
                count: rows,
            },
            Plan {
                account: "beta",
                mailbox: "inbox",
                count: 100,
            },
            Plan {
                account: "beta",
                mailbox: "archive",
                count: 50,
            },
        ];

        let mut stores: BTreeMap<&str, (Store, BlobStore)> = BTreeMap::new();
        let mut rng = Lcg(SEED);
        for plan in &plans {
            let entry = match stores.entry(plan.account) {
                Entry::Occupied(e) => e.into_mut(),
                Entry::Vacant(e) => {
                    let dir = data.join("accounts").join(plan.account);
                    fs::create_dir_all(&dir)
                        .unwrap_or_else(|e| panic!("create {}: {e}", dir.display()));
                    let store = Store::open(dir.join("store.sqlite3"))
                        .unwrap_or_else(|e| panic!("open the store of {}: {e:#}", plan.account));
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
                    &IngestInput {
                        account: plan.account,
                        mailbox: plan.mailbox,
                        uid,
                        email: &email,
                        raw: None,
                    },
                )
                .unwrap_or_else(|e| {
                    panic!("ingest {}/{} uid {uid}: {e:#}", plan.account, plan.mailbox)
                });
            }
        }

        // The oversized body, last so its uid does not shift when `rows` does.
        let (store, blobs) = stores.get("alpha").expect("alpha was planned");
        ingest_message(
            store,
            blobs,
            &IngestInput {
                account: "alpha",
                mailbox: "inbox",
                uid: 900_001,
                email: &big_message(big_mb),
                raw: None,
            },
        )
        .unwrap_or_else(|e| panic!("ingest the oversized body: {e:#}"));
    }

    /// One generated message, a function of the plan and the sequence number.
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
    fn date_of(n: usize) -> String {
        let base = chrono::DateTime::parse_from_rfc3339(EPOCH).expect("EPOCH parses");
        let tz = FixedOffset::east_opt(3600).expect("+01:00");
        tz.from_utc_datetime(&(base.naive_utc() + chrono::Duration::minutes(n as i64)))
            .to_rfc2822()
    }

    fn title(word: &str) -> String {
        let mut c = word.chars();
        c.next()
            .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
            .unwrap_or_default()
    }

    /// The config the fixture accounts need to be visible to `mp`, verbatim
    /// from the example.
    const CONFIG: &str = r#"# Generated by examples/mkfixture.rs. Offline fixture: no server is ever
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
}

/// Bumped whenever [`build_template`] would produce a different tree, so a
/// cache from an older checkout is never reused.
const FIXTURE_VERSION: &str = "v1";

/// `--rows`, the one number `docs/baselines/pre-daemon/workloads.md` pins.
const FIXTURE_ROWS: usize = 5000;

/// `--big-mb`, the oversized body's size.
const FIXTURE_BIG_MB: usize = 10;

/// The built-once template every row copies, as a single-root layout: data
/// directory, config directory and `HOME` are all this one path, which is what
/// [`DaemonFixture`] points a daemon at.
fn template_root() -> &'static Path {
    static TEMPLATE: OnceLock<PathBuf> = OnceLock::new();
    TEMPLATE.get_or_init(|| {
        let cache = std::env::temp_dir().join(format!("mp-soak-fixture-{FIXTURE_VERSION}"));
        fs::create_dir_all(&cache).unwrap_or_else(|e| panic!("create {}: {e}", cache.display()));
        let root = cache.join("root");
        let ready = cache.join("READY");
        if ready.is_file() {
            return root;
        }
        let _lock = BuildLock::acquire(&cache.join("build.lock"));
        // Another row may have built it while this one waited for the lock.
        if ready.is_file() {
            return root;
        }
        let started = Instant::now();
        build_template(&cache, &root);
        fs::write(&ready, FIXTURE_VERSION)
            .unwrap_or_else(|e| panic!("write the READY marker: {e}"));
        println!(
            "[soak] built the {FIXTURE_ROWS}-row fixture template at {} in {:.1} s",
            root.display(),
            started.elapsed().as_secs_f64()
        );
        root
    })
}

/// Generate the fixture into `cache`, then fold it into the one-root layout
/// and add what the soak rows need on top of it.
fn build_template(cache: &Path, root: &Path) {
    let staging = cache.join("staging");
    if staging.exists() {
        fs::remove_dir_all(&staging).unwrap_or_else(|e| panic!("clear the staging tree: {e}"));
    }
    if root.exists() {
        fs::remove_dir_all(root).unwrap_or_else(|e| panic!("clear a partial template: {e}"));
    }
    mkfixture::build(&staging, FIXTURE_ROWS, FIXTURE_BIG_MB);
    assert_fixture_shape(&staging.join("data"));

    // One root: the generated `data` tree becomes the template, and the
    // generated `config.toml` moves into it beside the accounts.
    fs::rename(staging.join("data"), root).unwrap_or_else(|e| panic!("moving the data tree: {e}"));
    let generated = fs::read_to_string(staging.join("config").join("config.toml"))
        .unwrap_or_else(|e| panic!("reading the generated config.toml: {e}"));
    fs::write(root.join("config.toml"), soak_config(&generated))
        .unwrap_or_else(|e| panic!("writing the soak config.toml: {e}"));

    let drafts = root.join("accounts").join(ACCOUNT).join("drafts");
    fs::create_dir_all(&drafts).unwrap_or_else(|e| panic!("create {}: {e}", drafts.display()));
    fs::write(drafts.join("hold.md"), hold_draft())
        .unwrap_or_else(|e| panic!("writing the approved draft: {e}"));
}

/// What the rows read out of the generated store, asserted once at build time
/// so a generator that drifted from `examples/mkfixture.rs` fails here rather
/// than as a mystifying count in a row.
fn assert_fixture_shape(data: &Path) {
    use mailypoppins::store::read;
    use mailypoppins::store::Store;

    let store = Store::open(data.join("accounts").join(ACCOUNT).join("store.sqlite3"))
        .expect("the generated store of alpha opens");
    let bulk = read::list_mailbox(&store, ACCOUNT, "Bulk").expect("alpha/Bulk lists");
    assert_eq!(
        bulk.len(),
        FIXTURE_ROWS,
        "the generator no longer produces the {FIXTURE_ROWS} rows the workloads pin"
    );
    let inbox = read::list_mailbox(&store, ACCOUNT, "inbox").expect("alpha/inbox lists");
    assert_eq!(
        inbox.len(),
        201,
        "alpha/inbox is 200 generated messages plus the oversized body"
    );
}

/// The generated configuration plus the two things the rows need.
///
/// The `[email]` table is **prepended**, for the reason
/// `tests/daemon_send_hold.rs` records: `config.toml` is a list of
/// `[[accounts]]` array tables, and a top-level table written after one of
/// them belongs to the last account instead of to the document.
fn soak_config(generated: &str) -> String {
    let with_imap = generated.replacen(
        "[accounts.mailboxes.inbox]",
        "[accounts.imap]\nhost = \"imap.fixture.invalid\"\nusername = \"alpha@fixture.invalid\"\n\n\
         [accounts.mailboxes.inbox]",
        1,
    );
    assert!(
        with_imap.contains("[accounts.imap]"),
        "the generated config.toml no longer has an inbox mailbox to anchor the imap table to"
    );
    format!("[email]\nsend_hold_secs = {HOLD_SECS}\n\n{with_imap}")
}

/// The one approved draft row (f) arms its holds on.
fn hold_draft() -> String {
    format!(
        "---\n\
         id: {HOLD_DRAFT}\n\
         to: ivana@example.com\n\
         cc:\n\
         bcc:\n\
         subject: \"Soak hold\"\n\
         status: approved\n\
         from: alpha@fixture.invalid\n\
         date: 2026-07-01 09:00\n\
         ---\n\
         \n\
         The draft row (f) arms and cancels fifty times.\n"
    )
}

/// A private copy of the template, dropped with the test.
struct SoakRoot {
    tmp: tempfile::TempDir,
}

impl SoakRoot {
    /// Copy the template into a fresh temporary directory.
    fn new() -> SoakRoot {
        let template = template_root();
        let tmp = tempfile::TempDir::new().expect("a temporary soak root");
        let status = std::process::Command::new("cp")
            .arg("-a")
            .arg(format!("{}/.", template.display()))
            .arg(tmp.path())
            .status()
            .expect("run cp -a");
        assert!(
            status.success(),
            "copying the fixture template into {} failed with {status}",
            tmp.path().display()
        );
        SoakRoot { tmp }
    }

    fn path(&self) -> &Path {
        self.tmp.path()
    }

    /// `<root>/accounts/<account>/drafts`, the directory the watcher walks.
    fn drafts_dir(&self, account: &str) -> PathBuf {
        self.tmp
            .path()
            .join("accounts")
            .join(account)
            .join("drafts")
    }
}

/// An exclusive advisory lock, so parallel rows build the template once.
///
/// The file is never read: holding it open *is* the lock, and the kernel
/// releases it when the handle drops, however this process ends.
struct BuildLock(#[allow(dead_code)] fs::File);

impl BuildLock {
    fn acquire(path: &Path) -> Self {
        let file = fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)
            .unwrap_or_else(|e| panic!("open the fixture build lock {}: {e}", path.display()));
        let start = Instant::now();
        loop {
            // Safety: `flock` on a descriptor this process owns; released by
            // the kernel when the file is closed, however this process dies.
            let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if rc == 0 {
                return BuildLock(file);
            }
            assert!(
                start.elapsed() < Duration::from_secs(600),
                "another row has held the fixture build lock {} for over ten minutes",
                path.display()
            );
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

// ---------------------------------------------------------------------------
// Measuring the daemon from the outside
// ---------------------------------------------------------------------------

/// Open descriptors, or `None` where there is no `/proc`.
///
/// The count includes the descriptor `read_dir` itself holds, which is why it
/// is only ever compared with another reading taken the same way.
fn fd_count(pid: u32) -> Option<usize> {
    let dir = PathBuf::from(format!("/proc/{pid}/fd"));
    let entries = fs::read_dir(dir).ok()?;
    Some(entries.filter(|entry| entry.is_ok()).count())
}

/// What each of those descriptors points at, folded into a count per target,
/// so a failure names the leak instead of leaving a number to interpret.
///
/// Sockets and pipes read as `socket:[<inode>]` and `pipe:[<inode>]`, which
/// would be a hundred distinct strings, so the inode is dropped and the class
/// is what gets counted.
fn fd_targets(pid: u32) -> std::collections::BTreeMap<String, usize> {
    let mut counts = std::collections::BTreeMap::new();
    let Ok(entries) = fs::read_dir(format!("/proc/{pid}/fd")) else {
        return counts;
    };
    for entry in entries.flatten() {
        let target = fs::read_link(entry.path())
            .map(|path| path.display().to_string())
            .unwrap_or_else(|_| "<unreadable>".to_string());
        let class = match target.split_once(":[") {
            Some((kind, _)) => format!("{kind}:[…]"),
            None => target,
        };
        *counts.entry(class).or_insert(0) += 1;
    }
    counts
}

/// `VmRSS` in bytes, or `None` where there is no `/proc`.
fn rss_bytes(pid: u32) -> Option<u64> {
    let status = fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let line = status.lines().find(|line| line.starts_with("VmRSS:"))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kib * 1024)
}

/// Whether this host lets a row read the two numbers above.
///
/// A row that cannot read them says so and returns rather than passing
/// silently: a green soak on a host that measured nothing is worse than a
/// skipped one.
fn proc_readable(pid: u32) -> bool {
    if fd_count(pid).is_some() && rss_bytes(pid).is_some() {
        return true;
    }
    eprintln!(
        "[soak] skipped: /proc/{pid} does not answer, so descriptors and RSS cannot be measured \
         on this host"
    );
    false
}

/// Assert that RSS grew by less than [`RSS_CEILING_BYTES`], and answer the
/// growth for the row to print.
fn assert_rss_bounded(label: &str, before: u64, after: u64) -> i64 {
    let growth = after as i64 - before as i64;
    assert!(
        growth < RSS_CEILING_BYTES as i64,
        "{label}: the daemon's RSS grew by {:.1} MiB ({before} -> {after} bytes), ceiling \
         {:.0} MiB",
        growth as f64 / (1024.0 * 1024.0),
        RSS_CEILING_BYTES as f64 / (1024.0 * 1024.0)
    );
    growth
}

/// Bring a freshly bound daemon to its working set and answer the descriptor
/// and RSS readings the row measures against.
///
/// A daemon that has just accepted its first connection is still starting: the
/// account runtimes come up off the startup path, the read pool opens its
/// connections on the first read that needs them, and SQLite keeps a handle
/// per concurrent reader. Measuring "before" at that moment and "after" at the
/// end of a row would report the daemon finishing its own startup as a leak.
/// So every row pays a short burst of concurrent clients first, waits for the
/// count to stop moving, and only then takes the numbers it will compare.
fn warmed_baseline(pid: u32, root: &Path) -> (usize, u64) {
    let cold = fd_count(pid).expect("a readable /proc");
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let path = root.to_path_buf();
            scope.spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("a current-thread runtime");
                rt.block_on(async {
                    for _ in 0..20 {
                        let mut conn = subscribed(&path).await;
                        for account in [ACCOUNT, OTHER_ACCOUNT] {
                            conn.call(
                                "message.list",
                                json!({"account": account, "mailbox": "inbox", "limit": 10}),
                            )
                            .await
                            .expect("message.list answers a warm-up client");
                        }
                        drop(conn);
                    }
                });
            });
        }
    });

    // Settled means "unchanged across a second of readings": the teardown of
    // the warm-up's own connections is asynchronous too.
    let start = Instant::now();
    let mut stable = 0;
    let mut last = usize::MAX;
    loop {
        let seen = fd_count(pid).expect("a readable /proc");
        stable = if seen == last { stable + 1 } else { 0 };
        last = seen;
        if stable >= 10 {
            return (seen, rss_bytes(pid).expect("a readable /proc"));
        }
        assert!(
            start.elapsed() < DEADLINE,
            "the daemon's descriptor count never settled: it was {cold} cold and is {seen} after \
             {DEADLINE:?} of warm-up; it holds {:?}",
            fd_targets(pid)
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Wait until the daemon's descriptor count stops moving, assert it came back
/// within [`FD_MARGIN`] of `before`, and answer the settled reading and the
/// difference.
///
/// It waits for stability rather than for the assertion to pass, so the number
/// it prints is the daemon's real working set rather than the first reading
/// that happened to be inside the margin. Closing is asynchronous on both
/// sides - a client's `close` returns before the daemon's read sees the
/// end-of-file, and the connection task that owns the descriptor is reaped
/// after that - so a reading taken the instant a row ends counts sockets that
/// are already going. A leak never settles, so the deadline is an assertion
/// too.
fn settle_fds(pid: u32, label: &str, before: usize) -> (usize, i64) {
    let start = Instant::now();
    let mut stable = 0;
    let mut last = usize::MAX;
    let after = loop {
        let seen = fd_count(pid).expect("a readable /proc");
        stable = if seen == last { stable + 1 } else { 0 };
        last = seen;
        if stable >= 10 {
            break seen;
        }
        assert!(
            start.elapsed() < DEADLINE,
            "{label}: the daemon's descriptor count never settled; it was {before} before the \
             row, reads {seen} after {DEADLINE:?} and holds {:?}",
            fd_targets(pid)
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    let growth = after as i64 - before as i64;
    assert!(
        growth <= FD_MARGIN as i64,
        "{label}: the daemon settled on {after} descriptors where it held {before}, margin \
         {FD_MARGIN}; it holds {:?}",
        fd_targets(pid)
    );
    (after, growth)
}

// ---------------------------------------------------------------------------
// Clients
// ---------------------------------------------------------------------------

/// One runtime per row, built once and kept for the whole row.
///
/// Not a `block_on` free function: a [`Connection`] holds a tokio `UnixStream`
/// registered with the runtime that created it, so a row that took two
/// runtimes would find its first client unusable inside the second ("a Tokio
/// 1.x context was found, but it is being shutdown"). Every row that measures
/// before and after needs exactly that, a client that outlives one block.
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("a multi-thread runtime")
}

fn client_info() -> ClientInfo {
    ClientInfo {
        kind: ClientKind::Gui,
        app_version: env!("CARGO_PKG_VERSION").to_string(),
    }
}

fn identity(root: &Path) -> Identity {
    Identity {
        data_dir: root.to_path_buf(),
        config_dir: root.to_path_buf(),
    }
}

/// A handshaken connection that is **not** subscribed to events, which is what
/// a row measuring call latency wants: no notification competes with a reply.
async fn connect(root: &Path) -> Connection {
    let mut conn = Connection::connect(&socket_path(root))
        .await
        .expect("connecting to a live daemon socket succeeds");
    conn.initialize(client_info(), identity(root), &[], &[])
        .await
        .expect("a compatible handshake succeeds");
    conn
}

/// A handshaken, bootstrapped connection: the daemon fans events at it from
/// the bootstrap on.
async fn subscribed(root: &Path) -> Connection {
    let mut conn = connect(root).await;
    conn.call("state.bootstrap", json!({}))
        .await
        .expect("a bootstrap registers this connection for events");
    conn
}

/// `diagnostic.health`, the daemon's statement about itself.
async fn health(conn: &mut Connection) -> Value {
    conn.call("diagnostic.health", json!({}))
        .await
        .expect("diagnostic.health answers")
}

fn health_u64(report: &Value, pointer: &str) -> u64 {
    report
        .pointer(pointer)
        .and_then(Value::as_u64)
        .unwrap_or_else(|| panic!("diagnostic.health carries a number at {pointer}: {report}"))
}

/// Poll `diagnostic.health` until `pointer` reads `want`, or fail naming what
/// it read instead.
async fn await_health(conn: &mut Connection, pointer: &str, want: u64, label: &str) {
    let start = Instant::now();
    loop {
        let seen = health_u64(&health(conn).await, pointer);
        if seen == want {
            return;
        }
        assert!(
            start.elapsed() < DEADLINE,
            "{label}: {pointer} is {seen} rather than {want} after {DEADLINE:?}"
        );
        tokio::time::sleep(TICK).await;
    }
}

/// One `state.event` notification, as `(kind, payload)`.
///
/// `state.resync_required` is the daemon's control message rather than an
/// event, and arrives as the kind `resync` so a row can count it.
async fn next_event(conn: &mut Connection, label: &str) -> (String, Value) {
    let notification = tokio::time::timeout(DEADLINE, conn.next_notification())
        .await
        .unwrap_or_else(|_| panic!("{label}: no notification within {DEADLINE:?}"))
        .unwrap_or_else(|| panic!("{label}: the daemon closed the connection"));
    match notification.method.as_str() {
        "state.event" => (
            notification.params["kind"]
                .as_str()
                .unwrap_or_else(|| panic!("{label}: an event carries a kind: {notification:?}"))
                .to_string(),
            notification.params["payload"].clone(),
        ),
        "state.resync_required" => ("resync".to_string(), notification.params.clone()),
        other => panic!("{label}: the daemon sent {other}, which nothing subscribed to"),
    }
}

/// Fire one `sync.quick` and wait for the `sync.completed` it commits.
///
/// Sequential on purpose: a second tick arriving while one runs *joins* it and
/// reports no outcome of its own, so overlapping passes would commit fewer
/// events than the row fired and the count would prove nothing.
async fn sync_once(conn: &mut Connection, account: &str) -> Value {
    let answer = conn
        .call("sync.quick", json!({"account": account}))
        .await
        .expect("sync.quick answers with an operation id");
    assert!(
        answer.get("operation_id").and_then(Value::as_str).is_some(),
        "sync.quick answers with an operation id: {answer}"
    );
    loop {
        let (kind, payload) = next_event(conn, "sync.quick").await;
        if kind == "sync.completed" {
            return payload;
        }
    }
}

// ---------------------------------------------------------------------------
// (a) Client churn
// ---------------------------------------------------------------------------

/// 200 connect/bootstrap/disconnect cycles from eight threads leave the daemon
/// holding the descriptors and the client count it started with.
///
/// This is the gate sentence "the daemon can run unattended across client
/// churn", measured rather than asserted: a connection task that never ended,
/// a socket that was never closed or a subscriber that was never unregistered
/// all show up here and nowhere else in the suite.
#[test]
fn client_churn_returns_every_descriptor_and_every_client_slot() {
    let root = SoakRoot::new();
    let daemon = DaemonFixture::start_with(root.path(), None, &[]);
    let pid = daemon.pid();
    if !proc_readable(pid) {
        return;
    }

    let cycles = 200 * scale();
    let threads = 8;
    let per_thread = cycles / threads;

    let rt = runtime();
    let (fds_before, rss_before) = warmed_baseline(pid, root.path());
    let mut probe = rt.block_on(connect(root.path()));
    let clients_before = rt.block_on(async { health_u64(&health(&mut probe).await, "/clients") });

    let started = Instant::now();
    std::thread::scope(|scope| {
        for _ in 0..threads {
            let path = root.path().to_path_buf();
            scope.spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("a current-thread runtime");
                rt.block_on(async {
                    for _ in 0..per_thread {
                        let mut conn = subscribed(&path).await;
                        // One call after the bootstrap, so the cycle is a
                        // client that did something rather than one that
                        // connected and left.
                        conn.call("account.list", json!({}))
                            .await
                            .expect("account.list answers a churned client");
                        drop(conn);
                    }
                });
            });
        }
    });
    let elapsed = started.elapsed();

    // The disconnects are asynchronous: the daemon reaps a connection when its
    // read returns end-of-file, which is after the client's `close`. Wait for
    // the count to come back rather than racing it.
    rt.block_on(await_health(
        &mut probe,
        "/clients",
        clients_before,
        "client churn",
    ));
    let (fds_after, fd_growth) = settle_fds(pid, "client churn", fds_before);
    let rss_after = rss_bytes(pid).expect("a readable /proc");
    let clients_after = rt.block_on(async { health_u64(&health(&mut probe).await, "/clients") });

    let rss_growth = assert_rss_bounded("client churn", rss_before, rss_after);
    assert_eq!(
        clients_after, clients_before,
        "the daemon still counts the connections it reaped"
    );

    println!(
        "[soak a] {cycles} connect/bootstrap/disconnect cycles on {threads} threads in {:.1} s: \
         fds {fds_before} -> {fds_after} ({fd_growth:+}), clients {clients_before} -> \
         {clients_after}, rss {:.1} -> {:.1} MiB ({:+.1})",
        elapsed.as_secs_f64(),
        rss_before as f64 / 1048576.0,
        rss_after as f64 / 1048576.0,
        rss_growth as f64 / 1048576.0,
    );

    drop(probe);
    daemon.stop();
}

// ---------------------------------------------------------------------------
// (b) A slow client under a sync burst
// ---------------------------------------------------------------------------

/// One client that never reads, four that do, and fifty syncs: the daemon's
/// memory stays bounded, every client is told to resync exactly once, and
/// every reader still receives every `sync.completed`.
///
/// The gate sentence is "memory remains bounded under slow clients and
/// repeated syncs". The bound is structural - a queue of 512 events or 4 MiB
/// per connection - and this row is what proves the structure holds when the
/// daemon is the one under pressure rather than a unit test's `Outbound`.
///
/// The second assertion is the one that would break silently: **no sync
/// outcome is lost to the overflow**. A `sync.completed` is a lifecycle event,
/// and the discard path keeps those while it throws the domain events around
/// them away, so a client whose queue overflowed in the middle of fifty syncs
/// still learns about all fifty. A regression that made outcomes coalescible,
/// or that discarded them with the rest, would pass every other test in the
/// suite and lose a user's failed sync here.
#[test]
fn a_slow_client_under_a_sync_burst_keeps_memory_bounded_and_loses_no_outcome() {
    let root = SoakRoot::new();
    let daemon = DaemonFixture::start_with(
        root.path(),
        None,
        &[(FAKE_EVENT_BURST_ENV, &BURST.to_string())],
    );
    let pid = daemon.pid();
    if !proc_readable(pid) {
        return;
    }

    let syncs = 50 * scale();
    let readers = 4;
    let (_, rss_before) = warmed_baseline(pid, root.path());
    let started = Instant::now();

    let (resyncs, delivered, rss_peak) = runtime().block_on(async {
        // The slow client first, so every burst any other bootstrap arms is
        // one it is registered for and never reads.
        let mut slow = subscribed(root.path()).await;

        // Then the readers, each bootstrapped before the driver starts, so
        // every outcome the driver commits is one they are registered for.
        // Their loops run while the syncs fire, which is what makes them fast
        // clients rather than four more stalled ones.
        let mut reading = Vec::new();
        for i in 0..readers {
            let conn = subscribed(root.path()).await;
            reading.push(tokio::spawn(async move {
                let mut conn = conn;
                let mut outcomes = 0_usize;
                let mut events = 0_u64;
                let mut resyncs = 0_usize;
                while outcomes < syncs {
                    let (kind, _) = next_event(&mut conn, &format!("fast reader {i}")).await;
                    events += 1;
                    match kind.as_str() {
                        "sync.completed" => outcomes += 1,
                        "resync" => resyncs += 1,
                        _ => {}
                    }
                }
                (outcomes, events, resyncs)
            }));
        }

        let mut driver = subscribed(root.path()).await;
        for _ in 0..syncs {
            let outcome = sync_once(&mut driver, ACCOUNT).await;
            assert_eq!(
                outcome["account"],
                json!(ACCOUNT),
                "the outcome names the account the pass ran against: {outcome}"
            );
        }
        let rss_peak = rss_bytes(pid).expect("a readable /proc");

        // Every reader sees every outcome the driver committed, whether or not
        // its own queue overflowed on the way: a `sync.completed` is a
        // lifecycle event, and the overflow path keeps those while it discards
        // the domain events around them.
        let mut delivered = Vec::new();
        for (i, task) in reading.into_iter().enumerate() {
            delivered.push(
                tokio::time::timeout(DEADLINE, task)
                    .await
                    .unwrap_or_else(|_| panic!("fast reader {i} never saw its {syncs} outcomes"))
                    .expect("a fast reader finishes"),
            );
        }

        // And the slow client, reading for the first time, finds exactly one
        // control message: the poison is set once and only a re-bootstrap
        // clears it.
        let mut resyncs = 0_usize;
        let mut seen = 0_u64;
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_millis(500), slow.next_notification()).await {
                Ok(Some(notification)) => {
                    seen += 1;
                    if notification.method == "state.resync_required" {
                        resyncs += 1;
                        assert_eq!(
                            notification.params["reason"],
                            json!("event_queue_overflow"),
                            "the control message says why: {:?}",
                            notification.params
                        );
                    }
                }
                // Nothing more is coming: the queue is drained and poisoned.
                Ok(None) | Err(_) => break,
            }
        }
        assert!(seen > 0, "the slow client was sent nothing at all");
        (resyncs, delivered, rss_peak)
    });
    let elapsed = started.elapsed();

    let rss_after = rss_bytes(pid).expect("a readable /proc");
    let rss_growth = assert_rss_bounded("slow client", rss_before, rss_after.max(rss_peak));

    assert_eq!(
        resyncs, 1,
        "a stalled client is asked to resync once and then left alone until it bootstraps again"
    );
    for (i, (outcomes, _, resyncs)) in delivered.iter().enumerate() {
        assert_eq!(
            *outcomes, syncs,
            "reader {i} received {outcomes} of the {syncs} sync outcomes the driver committed"
        );
        assert!(
            *resyncs <= 1,
            "reader {i} was asked to resync {resyncs} times without ever bootstrapping again"
        );
    }

    println!(
        "[soak b] {syncs} syncs, {readers} readers, one stalled client, burst {BURST}/bootstrap, \
         in {:.1} s: stalled-client resyncs {resyncs}, per-reader (events, resyncs) {:?}, rss \
         {:.1} -> {:.1} MiB ({:+.1})",
        elapsed.as_secs_f64(),
        delivered
            .iter()
            .map(|(_, events, resyncs)| (*events, *resyncs))
            .collect::<Vec<_>>(),
        rss_before as f64 / 1048576.0,
        rss_after as f64 / 1048576.0,
        rss_growth as f64 / 1048576.0,
    );

    daemon.stop();
}

// ---------------------------------------------------------------------------
// (c) Repeated syncs
// ---------------------------------------------------------------------------

/// 200 `sync.quick` passes leave no operation live, no memory behind and the
/// store where they found it.
#[test]
fn repeated_syncs_settle_every_operation_and_leave_the_store_alone() {
    let root = SoakRoot::new();
    let daemon = DaemonFixture::start_with(root.path(), None, &[]);
    let pid = daemon.pid();
    if !proc_readable(pid) {
        return;
    }

    let passes = 200 * scale();
    let (_, rss_before) = warmed_baseline(pid, root.path());
    let started = Instant::now();

    let (store_before, store_after, peak_active) = runtime().block_on(async {
        let mut conn = subscribed(root.path()).await;
        let before = health(&mut conn).await;
        let store_before = health_u64(&before, "/store/size_bytes");

        let mut peak_active = 0_u64;
        for n in 0..passes {
            sync_once(&mut conn, ACCOUNT).await;
            // Cheap, and it is the only way a row sees a registry that grows:
            // a leak shows as a rising floor rather than as a spike.
            if n % 25 == 0 {
                peak_active =
                    peak_active.max(health_u64(&health(&mut conn).await, "/operations/active"));
            }
        }

        await_health(&mut conn, "/operations/active", 0, "repeated syncs").await;
        let after = health(&mut conn).await;
        (
            store_before,
            health_u64(&after, "/store/size_bytes"),
            peak_active,
        )
    });
    let elapsed = started.elapsed();

    let rss_after = rss_bytes(pid).expect("a readable /proc");
    let rss_growth = assert_rss_bounded("repeated syncs", rss_before, rss_after);
    let store_growth = store_after as i64 - store_before as i64;
    assert!(
        store_growth.abs() < STORE_MARGIN_BYTES,
        "{passes} passes that ingested nothing moved the store by {store_growth} bytes \
         ({store_before} -> {store_after}), margin {STORE_MARGIN_BYTES}"
    );

    println!(
        "[soak c] {passes} sync.quick passes in {:.1} s: operations.active peak {peak_active}, \
         back to 0, store {store_before} -> {store_after} bytes ({store_growth:+}), rss \
         {:.1} -> {:.1} MiB ({:+.1})",
        elapsed.as_secs_f64(),
        rss_before as f64 / 1048576.0,
        rss_after as f64 / 1048576.0,
        rss_growth as f64 / 1048576.0,
    );

    daemon.stop();
}

// ---------------------------------------------------------------------------
// (d) Search churn
// ---------------------------------------------------------------------------

/// The five read calls row (d) rotates through, as `(method, params)`.
///
/// Two mailboxes of one account, one of another, a full-text hit and a
/// prefix-heavy one: different stores, different projections and different
/// result sizes, so a convoy has somewhere to form.
fn read_calls() -> Vec<(&'static str, Value)> {
    vec![
        (
            "message.list",
            json!({"account": ACCOUNT, "mailbox": "inbox", "limit": 50}),
        ),
        (
            "message.list",
            json!({"account": ACCOUNT, "mailbox": "Bulk", "limit": 200}),
        ),
        (
            "message.list",
            json!({"account": OTHER_ACCOUNT, "mailbox": "inbox", "limit": 50}),
        ),
        (
            "message.search",
            json!({"account": ACCOUNT, "query": mkfixture::NEEDLE, "limit": 50}),
        ),
        (
            "message.search",
            json!({"account": ACCOUNT, "query": "invoice", "limit": 100}),
        ),
    ]
}

/// 300 reads from four concurrent clients: none fails, and the p95 stays two
/// orders of magnitude under a convoy.
#[test]
fn search_churn_from_four_clients_answers_every_call_inside_the_ceiling() {
    let root = SoakRoot::new();
    let daemon = DaemonFixture::start_with(root.path(), None, &[]);
    let pid = daemon.pid();
    if !proc_readable(pid) {
        return;
    }

    let clients = 4;
    let calls = 300 * scale();
    let per_client = calls / clients;
    let (_, rss_before) = warmed_baseline(pid, root.path());
    let started = Instant::now();

    let mut latencies: Vec<Duration> = runtime().block_on(async {
        let mut tasks = Vec::new();
        for client in 0..clients {
            let path = root.path().to_path_buf();
            tasks.push(tokio::spawn(async move {
                let calls = read_calls();
                let mut conn = connect(&path).await;
                let mut taken = Vec::with_capacity(per_client);
                for n in 0..per_client {
                    let (method, params) = &calls[(n + client) % calls.len()];
                    let at = Instant::now();
                    let answer = conn.call(method, params.clone()).await.unwrap_or_else(|e| {
                        panic!("client {client} call {n} to {method} failed: {e}")
                    });
                    taken.push(at.elapsed());
                    assert!(answer.is_object(), "{method} answers an object: {answer}");
                }
                taken
            }));
        }
        let mut all = Vec::new();
        for task in tasks {
            all.extend(task.await.expect("a read client finishes"));
        }
        all
    });
    let elapsed = started.elapsed();

    let rss_after = rss_bytes(pid).expect("a readable /proc");
    let rss_growth = assert_rss_bounded("search churn", rss_before, rss_after);

    latencies.sort();
    assert_eq!(latencies.len(), per_client * clients);
    let p50 = latencies[latencies.len() / 2];
    let p95 = latencies[latencies.len() * 95 / 100];
    let worst = *latencies.last().expect("at least one call");
    assert!(
        p95 < P95_CEILING,
        "the p95 of {} reads from {clients} clients is {p95:?}, ceiling {P95_CEILING:?} \
         (p50 {p50:?}, max {worst:?})",
        latencies.len()
    );

    println!(
        "[soak d] {} reads from {clients} clients in {:.1} s: p50 {:.1} ms, p95 {:.1} ms, max \
         {:.1} ms, rss {:.1} -> {:.1} MiB ({:+.1})",
        latencies.len(),
        elapsed.as_secs_f64(),
        p50.as_secs_f64() * 1000.0,
        p95.as_secs_f64() * 1000.0,
        worst.as_secs_f64() * 1000.0,
        rss_before as f64 / 1048576.0,
        rss_after as f64 / 1048576.0,
        rss_growth as f64 / 1048576.0,
    );

    daemon.stop();
}

// ---------------------------------------------------------------------------
// (e) Draft-watch churn
// ---------------------------------------------------------------------------

/// How many drafts row (e) writes before it waits for their events.
///
/// Under the 512-event outbound queue, deliberately: a batch larger than the
/// queue overflows the subscriber, which is the daemon behaving exactly as
/// `docs/daemon-protocol.md` says it must - discard, ask for a resync, stop
/// sending domain events - and would leave this row waiting for events the
/// client is no longer entitled to. Six hundred drafts written at once is what
/// found that; the row now writes them a batch at a time and drains each one,
/// which is also what a user's editor does.
const DRAFT_BATCH: usize = 100;

/// One draft file, written to `<root>/accounts/<account>/drafts/<id>.md`.
fn soak_draft(id: &str, n: usize) -> String {
    format!(
        "---\n\
         id: {id}\n\
         to: robin@example.com\n\
         cc:\n\
         bcc:\n\
         subject: \"Soak draft {n}\"\n\
         status: draft\n\
         from: alpha@fixture.invalid\n\
         date: 2026-07-01 09:00\n\
         ---\n\
         \n\
         Draft {n} of the watcher churn row.\n"
    )
}

/// The id of draft `n`, in the sixteen-hex-digit shape the drafts carry.
fn soak_draft_id(n: usize) -> String {
    format!("{:016x}", 0x50a4_0000_0000_1000_u64 + n as u64)
}

/// 100 drafts appear and are then deleted: one event per draft either way, and
/// the watcher leaves no descriptor behind.
///
/// What the watcher guarantees, and therefore what this pins: **one event per
/// draft**, not one per write. Two writes to one file inside the debounce
/// coalesce, because a `draft.changed` is a `Replace` keyed by the draft's own
/// resource; a hundred *distinct* drafts are a hundred resources and coalesce
/// with nothing. So the row writes each file once and waits for the set of
/// ids, never for a count of notifications - and it writes them
/// [`DRAFT_BATCH`] at a time, because a batch bigger than the outbound queue
/// is entitled to a resync rather than to the events.
#[test]
fn draft_watch_churn_delivers_every_draft_and_leaks_no_descriptor() {
    let root = SoakRoot::new();
    let daemon = DaemonFixture::start_with(
        root.path(),
        None,
        &[(WATCH_POLL_ENV, "25"), (WATCH_DEBOUNCE_ENV, "100")],
    );
    let pid = daemon.pid();
    if !proc_readable(pid) {
        return;
    }

    let drafts = 100 * scale();
    let dir = root.drafts_dir(ACCOUNT);
    let (fds_before, rss_before) = warmed_baseline(pid, root.path());
    let started = Instant::now();

    let (upserts, removes) = runtime().block_on(async {
        let mut conn = subscribed(root.path()).await;
        let wanted: Vec<String> = (0..drafts).map(soak_draft_id).collect();

        let mut upserts = BTreeSet::new();
        for batch in wanted.chunks(DRAFT_BATCH) {
            for (n, id) in batch.iter().enumerate() {
                fs::write(dir.join(format!("{id}.md")), soak_draft(id, n))
                    .unwrap_or_else(|e| panic!("write draft {id}: {e}"));
            }
            while !batch.iter().all(|id| upserts.contains(id)) {
                let (kind, payload) = next_event(&mut conn, "draft watch").await;
                match kind.as_str() {
                    "draft.changed" => {
                        if let Some(id) = payload["id"].as_str() {
                            upserts.insert(id.to_string());
                        }
                    }
                    "resync" => panic!(
                        "a batch of {DRAFT_BATCH} drafts overflowed the subscriber's queue: \
                         {payload}"
                    ),
                    _ => {}
                }
            }
        }

        let mut removes = BTreeSet::new();
        for batch in wanted.chunks(DRAFT_BATCH) {
            for id in batch {
                fs::remove_file(dir.join(format!("{id}.md")))
                    .unwrap_or_else(|e| panic!("remove draft {id}: {e}"));
            }
            while !batch.iter().all(|id| removes.contains(id)) {
                let (kind, payload) = next_event(&mut conn, "draft watch").await;
                match kind.as_str() {
                    "state.remove" => {
                        if let Some(id) = payload["resource"].as_str().and_then(|resource| {
                            resource
                                .strip_prefix(&format!("draft:{ACCOUNT}/"))
                                .map(str::to_string)
                        }) {
                            removes.insert(id);
                        }
                    }
                    "resync" => panic!(
                        "a batch of {DRAFT_BATCH} deletions overflowed the subscriber's queue: \
                         {payload}"
                    ),
                    _ => {}
                }
            }
        }
        (upserts.len(), removes.len())
    });
    let elapsed = started.elapsed();

    // The watcher polls, so a reading taken the instant the last remove
    // arrived can still hold the descriptor of the walk that found it.
    let (fds_after, fd_growth) = settle_fds(pid, "draft watch churn", fds_before);
    let rss_after = rss_bytes(pid).expect("a readable /proc");
    let rss_growth = assert_rss_bounded("draft watch churn", rss_before, rss_after);

    println!(
        "[soak e] {drafts} drafts written and deleted in {:.1} s: {upserts} draft.changed, \
         {removes} state.remove, fds {fds_before} -> {fds_after} ({fd_growth:+}), rss {:.1} -> \
         {:.1} MiB ({:+.1})",
        elapsed.as_secs_f64(),
        rss_before as f64 / 1048576.0,
        rss_after as f64 / 1048576.0,
        rss_growth as f64 / 1048576.0,
    );

    daemon.stop();
}

// ---------------------------------------------------------------------------
// (f) Hold churn
// ---------------------------------------------------------------------------

/// 50 holds armed on one client and cancelled from another leave the scheduler
/// empty, the ledger empty and the draft approved.
///
/// Alternating on purpose: the plan's own sentence is that "cancellation from
/// a client other than the one that sent must work", and a row that armed and
/// cancelled on one connection would never exercise the table the scheduler
/// keeps across connections.
#[test]
fn hold_churn_cancels_every_hold_and_leaves_the_ledger_empty() {
    let root = SoakRoot::new();
    let daemon = DaemonFixture::start_with(root.path(), None, &[]);
    let pid = daemon.pid();
    if !proc_readable(pid) {
        return;
    }

    let holds = 50 * scale();
    let (_, rss_before) = warmed_baseline(pid, root.path());
    let started = Instant::now();

    let outbox = runtime().block_on(async {
        let mut a = subscribed(root.path()).await;
        let mut b = subscribed(root.path()).await;

        for n in 0..holds {
            let (arm, cancel): (&mut Connection, &mut Connection) = if n % 2 == 0 {
                (&mut a, &mut b)
            } else {
                (&mut b, &mut a)
            };
            let answer = arm
                .call(
                    "send.draft",
                    json!({"account": ACCOUNT, "id": HOLD_DRAFT, "hold": true}),
                )
                .await
                .unwrap_or_else(|e| panic!("arming hold {n} failed: {e}"));
            assert_eq!(
                answer.get("held").and_then(Value::as_bool),
                Some(true),
                "hold {n} was armed rather than sent: {answer}"
            );
            let operation_id = answer["operation_id"]
                .as_str()
                .unwrap_or_else(|| panic!("hold {n} answers with an operation id: {answer}"))
                .to_string();

            cancel
                .call("send.cancel_hold", json!({"operation_id": operation_id}))
                .await
                .unwrap_or_else(|e| {
                    panic!("cancelling hold {n} from the other client failed: {e}")
                });
        }

        await_health(&mut a, "/holds", 0, "hold churn").await;

        let listing = a
            .call("send.hold_status", json!({"account": ACCOUNT}))
            .await
            .expect("send.hold_status answers");
        assert_eq!(
            listing["holds"],
            json!([]),
            "the scheduler carries no hold once every one of them was cancelled: {listing}"
        );

        let drafts = a
            .call("draft.list", json!({"account": ACCOUNT}))
            .await
            .expect("draft.list answers");
        let status = drafts["drafts"]
            .as_array()
            .expect("a draft listing carries drafts")
            .iter()
            .find(|entry| entry["id"] == json!(HOLD_DRAFT))
            .map(|entry| entry["status"].clone())
            .unwrap_or_else(|| panic!("the held draft is still listed: {drafts}"));
        assert_eq!(
            status,
            json!("approved"),
            "a cancelled hold leaves its draft approved"
        );

        let outbox = a
            .call("send.outbox_list", json!({"account": ACCOUNT}))
            .await
            .expect("send.outbox_list answers");
        outbox
    });
    let elapsed = started.elapsed();

    let rss_after = rss_bytes(pid).expect("a readable /proc");
    let rss_growth = assert_rss_bounded("hold churn", rss_before, rss_after);
    let rows = outbox["rows"]
        .as_array()
        .unwrap_or_else(|| panic!("an outbox listing carries rows: {outbox}"))
        .len();
    assert_eq!(
        rows, 0,
        "no cancelled hold queued anything to send: {outbox}"
    );

    println!(
        "[soak f] {holds} holds armed and cancelled from alternating clients in {:.1} s: holds 0, \
         outbox {rows} rows, rss {:.1} -> {:.1} MiB ({:+.1})",
        elapsed.as_secs_f64(),
        rss_before as f64 / 1048576.0,
        rss_after as f64 / 1048576.0,
        rss_growth as f64 / 1048576.0,
    );

    daemon.stop();
}

// ---------------------------------------------------------------------------
// (g) The mixed run
// ---------------------------------------------------------------------------

/// Everything at once for [`soak_secs`] seconds, ending in a clean stop.
///
/// The five workers run until the deadline rather than for a fixed count, so
/// the row costs what it was asked for on any machine, and the numbers it
/// prints say how much work that bought. The stop at the end is the point: a
/// daemon that soaked for an hour must still settle its operations, tell the
/// client that asked, and say `clean`.
#[test]
fn a_mixed_run_stays_bounded_and_ends_in_a_clean_stop() {
    let root = SoakRoot::new();
    let daemon = DaemonFixture::start_with(
        root.path(),
        None,
        &[(WATCH_POLL_ENV, "50"), (WATCH_DEBOUNCE_ENV, "100")],
    );
    let pid = daemon.pid();
    if !proc_readable(pid) {
        return;
    }

    let secs = soak_secs();
    let dir = root.drafts_dir(ACCOUNT);
    let (fds_before, rss_before) = warmed_baseline(pid, root.path());
    let started = Instant::now();

    let churned = Arc::new(AtomicU64::new(0));
    let synced = Arc::new(AtomicU64::new(0));
    let reads = Arc::new(AtomicU64::new(0));
    let written = Arc::new(AtomicU64::new(0));
    let holds = Arc::new(AtomicU64::new(0));
    let events = Arc::new(AtomicU64::new(0));

    let rt = runtime();
    rt.block_on(async {
        let deadline = Instant::now() + Duration::from_secs(secs);
        let mut workers: Vec<tokio::task::JoinHandle<()>> = Vec::new();

        // A reader that drains events for the whole run, so the other workers
        // are never the only subscribers and the daemon is fanning out while
        // it answers.
        workers.push({
            let path = root.path().to_path_buf();
            let events = Arc::clone(&events);
            tokio::spawn(async move {
                let mut conn = subscribed(&path).await;
                while Instant::now() < deadline {
                    if let Ok(Some(_)) =
                        tokio::time::timeout(Duration::from_millis(100), conn.next_notification())
                            .await
                    {
                        events.fetch_add(1, Ordering::Relaxed);
                    }
                }
            })
        });

        // Client churn.
        workers.push({
            let path = root.path().to_path_buf();
            let churned = Arc::clone(&churned);
            tokio::spawn(async move {
                while Instant::now() < deadline {
                    let mut conn = subscribed(&path).await;
                    conn.call("account.list", json!({}))
                        .await
                        .expect("account.list answers a churned client");
                    drop(conn);
                    churned.fetch_add(1, Ordering::Relaxed);
                }
            })
        });

        // Syncs.
        workers.push({
            let path = root.path().to_path_buf();
            let synced = Arc::clone(&synced);
            tokio::spawn(async move {
                let mut conn = subscribed(&path).await;
                while Instant::now() < deadline {
                    sync_once(&mut conn, ACCOUNT).await;
                    synced.fetch_add(1, Ordering::Relaxed);
                }
            })
        });

        // Reads.
        workers.push({
            let path = root.path().to_path_buf();
            let reads = Arc::clone(&reads);
            tokio::spawn(async move {
                let calls = read_calls();
                let mut conn = connect(&path).await;
                let mut n = 0_usize;
                while Instant::now() < deadline {
                    let (method, params) = &calls[n % calls.len()];
                    conn.call(method, params.clone())
                        .await
                        .unwrap_or_else(|e| panic!("the mixed run's {method} failed: {e}"));
                    reads.fetch_add(1, Ordering::Relaxed);
                    n += 1;
                }
            })
        });

        // Draft writes and deletes.
        workers.push({
            let written = Arc::clone(&written);
            tokio::spawn(async move {
                let mut n = 0_usize;
                while Instant::now() < deadline {
                    let id = soak_draft_id(n);
                    let path = dir.join(format!("{id}.md"));
                    fs::write(&path, soak_draft(&id, n))
                        .unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    fs::remove_file(&path)
                        .unwrap_or_else(|e| panic!("remove {}: {e}", path.display()));
                    written.fetch_add(1, Ordering::Relaxed);
                    n += 1;
                }
            })
        });

        // Holds, armed and cancelled on two connections.
        workers.push({
            let path = root.path().to_path_buf();
            let holds = Arc::clone(&holds);
            tokio::spawn(async move {
                let mut arm = subscribed(&path).await;
                let mut cancel = subscribed(&path).await;
                while Instant::now() < deadline {
                    let answer = arm
                        .call(
                            "send.draft",
                            json!({"account": ACCOUNT, "id": HOLD_DRAFT, "hold": true}),
                        )
                        .await
                        .expect("the mixed run arms a hold");
                    let operation_id = answer["operation_id"]
                        .as_str()
                        .unwrap_or_else(|| panic!("a held send answers with an id: {answer}"))
                        .to_string();
                    cancel
                        .call("send.cancel_hold", json!({"operation_id": operation_id}))
                        .await
                        .expect("the mixed run cancels its own hold");
                    holds.fetch_add(1, Ordering::Relaxed);
                }
            })
        });

        for worker in workers {
            worker.await.expect("a mixed-run worker finishes");
        }
    });
    let work_elapsed = started.elapsed();

    // Measured while the daemon is still serving: the stop is the next
    // assertion, not the one these two numbers belong to.
    let rss_after = rss_bytes(pid).expect("a readable /proc");
    let (fds_after, fd_growth) = settle_fds(pid, "mixed run", fds_before);
    let rss_growth = assert_rss_bounded("mixed run", rss_before, rss_after);

    let (clean, unsettled) = rt.block_on(async {
        let mut conn = connect(root.path()).await;
        let answer = conn
            .call("daemon.stop", json!({}))
            .await
            .expect("daemon.stop answers before it shuts anything down");
        assert_eq!(answer["stopping"], json!(true), "{answer}");
        let mut clean = None;
        let mut unsettled = json!([]);
        while let Some(notification) = tokio::time::timeout(DEADLINE, conn.next_notification())
            .await
            .expect("the daemon closes the connection rather than hanging")
        {
            if notification.method == "daemon.stopped" {
                clean = notification.params["clean"].as_bool();
                unsettled = notification.params["unsettled"].clone();
            }
        }
        (clean, unsettled)
    });

    assert_eq!(
        clean,
        Some(true),
        "a daemon that soaked for {secs}s still stops clean; unsettled: {unsettled}"
    );

    println!(
        "[soak g] {secs}s mixed run ({:.1} s wall): {} churn cycles, {} syncs, {} reads, {} \
         draft write/delete pairs, {} holds, {} events drained, fds {fds_before} -> {fds_after} \
         ({fd_growth:+}), rss {:.1} -> {:.1} MiB ({:+.1}), stop clean",
        work_elapsed.as_secs_f64(),
        churned.load(Ordering::Relaxed),
        synced.load(Ordering::Relaxed),
        reads.load(Ordering::Relaxed),
        written.load(Ordering::Relaxed),
        holds.load(Ordering::Relaxed),
        events.load(Ordering::Relaxed),
        rss_before as f64 / 1048576.0,
        rss_after as f64 / 1048576.0,
        rss_growth as f64 / 1048576.0,
    );

    daemon.stop();
}
