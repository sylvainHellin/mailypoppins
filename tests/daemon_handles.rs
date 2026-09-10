//! Materialised-handle lifetime versus the retention sweep (#0122, plan unit
//! P3b-U11).
//!
//! This file is a **contract test**: it is written before
//! `src/daemon/handles.rs`, the three `message.*` handle methods and the
//! sweep's pin seam exist, against the contract fixed in
//! `.agents/workflow/native-gui-daemon/plan.md` section 3.5 (unit P3b-U11) and
//! the source plan's prose ("Materialize files through daemon operations and
//! return constrained handles or paths with explicit lifetime … A materialized
//! handle keeps its backing blob alive until the client releases it or the
//! handle expires, so the retention sweep cannot evict a file a client just
//! opened … The sweep skips blobs backing a materialized handle that a client
//! still holds", anomalies `ANO-5` and `ANO-6`). It does not compile under
//! `--features daemon` today, and that failure *is* the proof the contract has
//! no stub behind it. An implementer (P3b-U12) does not edit this file; they
//! make it pass.
//!
//! # The three layers, and why the split
//!
//! **(a) In-process, against [`HandleTable`].** A handle's lifetime is a
//! statement about time, and a test that proved expiry by sleeping would be
//! slow where it is not flaky. The table therefore takes its clock as a
//! parameter - `pinned_blobs(now)`, `expire(now)`, `materialise(.., now)` - so
//! a test drives time by hand and every expiry assertion is deterministic.
//! Everything that is a property of the *bookkeeping* lives here: what a handle
//! pins, what a release does, what an expiry does, and the fact that two
//! handles over one blob keep it alive until the last one goes.
//!
//! **(b) In-process, against [`sweep_pinned`] over a seeded store.** The pin is
//! only worth anything if the sweep honours it *and* still refuses what
//! `ANO-5` says it must refuse. Both are properties of the planner over a real
//! store, so this layer seeds `store.sqlite3` plus a blob directory in a
//! tempdir - exactly as `src/store/sweep.rs`'s own unit tests do - and drives
//! the two together: the pinned set comes out of a [`HandleTable`] and goes
//! straight into the sweep, which is the seam this unit exists to pin.
//!
//! **(c) Over the socket, against a spawned `mp daemon run`.** Whether the
//! three methods are served, advertised, shaped as documented, and whether a
//! handle outlives the connection that opened it are properties of the daemon
//! process and of nothing smaller. Each socket test spawns its own daemon in
//! its own tempdir sandbox, with a store seeded in-process *before* the spawn,
//! killed on drop, with every wait bounded.
//!
//! # Surface under test
//!
//! ```rust,ignore
//! // src/daemon/handles.rs  ->  mailypoppins::daemon::handles
//! pub const DEFAULT_HANDLE_TTL: Duration = Duration::from_secs(600);
//! pub const HANDLE_TTL_ENV: &str = "MAILYPOPPINS_DAEMON_HANDLE_TTL_MS";
//! pub const HANDLES_DIR: &str = "handles";
//!
//! #[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
//! pub struct HandleId(pub String);
//! impl HandleId { pub fn as_str(&self) -> &str; }
//!
//! #[derive(Clone, Copy, Debug, PartialEq, Eq)]
//! pub enum HandleKind { Attachment, Html }
//! impl HandleKind { pub fn as_str(self) -> &'static str; }   // "attachment" / "html"
//!
//! #[derive(Clone, Debug, PartialEq, Eq)]
//! pub struct Handle {
//!     pub id: HandleId,
//!     pub kind: HandleKind,
//!     pub account: String,
//!     pub path: PathBuf,
//!     pub bytes: u64,
//!     pub blobs: BTreeSet<String>,
//!     pub expires_at: DateTime<Utc>,
//! }
//!
//! pub struct HandleTable;
//! impl HandleTable {
//!     pub fn new(ttl: Duration) -> HandleTable;
//!     pub fn from_env() -> HandleTable;
//!     pub fn ttl(&self) -> Duration;
//!     pub fn materialise(
//!         &self,
//!         kind: HandleKind,
//!         account: &str,
//!         path: PathBuf,
//!         bytes: u64,
//!         blobs: &[String],
//!         now: DateTime<Utc>,
//!     ) -> Handle;
//!     pub fn release(&self, id: &HandleId) -> bool;
//!     pub fn pinned_blobs(&self, now: DateTime<Utc>) -> BTreeSet<String>;
//!     pub fn expire(&self, now: DateTime<Utc>) -> Vec<HandleId>;
//! }
//!
//! // src/store/sweep.rs  ->  mailypoppins::store::sweep
//! pub fn sweep_pinned(
//!     store: &Store,
//!     blobs: &BlobStore,
//!     policy: &RetentionPolicy,
//!     opts: SweepOptions,
//!     pinned: &BTreeSet<String>,
//! ) -> Result<SweepOutcome>;
//!
//! // src/daemon/methods/message.rs  ->  mailypoppins::daemon::methods::message
//! pub const MESSAGE_HANDLE_METHOD_SPECS: [MethodSpec; 3];
//! ```
//!
//! # The table's semantics, pinned
//!
//! 1. **The table takes `&self` everywhere.** A [`Method`] is called through
//!    `&'a self` behind an `Arc` shared by every connection task, so the table
//!    carries its own interior mutability rather than forcing the dispatcher to
//!    hold a lock it cannot see into.
//! 2. **A handle id is opaque, unique and filesystem-safe**: non-empty and
//!    drawn from `[A-Za-z0-9_-]`, because it is also the name of the directory
//!    the materialised file lives in. That is what lets `release_handle` take
//!    nothing but the id: the daemon reconstructs the directory from it.
//! 3. **`expires_at` is `now + ttl`, and expiry is `expires_at <= now`.** A
//!    handle is live strictly *before* its expiry instant, so the boundary
//!    belongs to the dead: a client that must not lose its file asks for
//!    another handle rather than betting on a comparison.
//! 4. **Expiry is lazy and needs no reaper to be true.** `pinned_blobs(now)`
//!    ignores an expired handle whether or not `expire(now)` has been called,
//!    because a sweep that had to be preceded by a reaper tick would keep a
//!    dead handle's blob alive for as long as the tick was late. `expire(now)`
//!    is the *deletion* of the same fact, and returns the ids it dropped so the
//!    caller can unlink their directories.
//! 5. **`release` is the truth of "was this a live handle".** It returns `true`
//!    exactly once per handle; an unknown id, a released id and an expired id
//!    are all `false`, which is what makes `-32602` the wire answer to all
//!    three without the daemon having to tell them apart.
//! 6. **A blob stays pinned while any live handle names it.** Two handles over
//!    one blob release independently, and the blob is unpinned when the last
//!    one goes: a union, not a flag.
//!
//! # The sweep seam, pinned
//!
//! - **`sweep_pinned` is `sweep` plus a pin set**, and
//!   `sweep(a, b, c, d) == sweep_pinned(a, b, c, d, &BTreeSet::new())`. A new
//!   entry point rather than a `pinned` field on [`SweepOptions`]: the options
//!   are `Copy + Default` and travel through `src/main.rs` and the post-sync
//!   sweep, and a borrowed set inside them would put a lifetime on every one of
//!   those call sites for the benefit of the one caller that has pins. The
//!   daemon calls `sweep_pinned`; `mp store gc` keeps calling `sweep`.
//! - **A pinned blob is dropped from the eviction plan**, not from the store's
//!   size. `before_bytes` counts it, because it is resident disk and the cap is
//!   a statement about disk; what the pin changes is only who may be chosen as
//!   a victim.
//! - **The two `ANO-5` rules are computed after the pin, not before.** The
//!   warn-then-evict marker is untouched (a first over-cap sweep warns whether
//!   or not anything is pinned), and the half-store guard compares the
//!   *pinned-free* plan against `before_bytes`. Pinning can only shrink a plan,
//!   so it can only make the guard less likely to fire, and a sweep that
//!   refuses is still refusing on what it would really have deleted.
//! - **`--force` bypasses the guard and never the pin.** The guard is a
//!   fat-finger rule about volume and the user can overrule it; the pin is a
//!   correctness rule about a file another process has open, and no flag on
//!   this command line knows better.
//! - **A sweep that cannot reach the cap because of pins leaves the marker
//!   set**, which falls straight out of the existing "clear the marker only
//!   when `after <= cap`" rule: the store is still over its cap, and the next
//!   sweep, after the handle is gone, evicts.
//!
//! # The three methods, pinned
//!
//! - `message.materialise_attachment`, params `{account, id, part}`,
//!   `ClientIntegration`, `since` 1, `Durable`.
//! - `message.materialise_html`, params `{account, id}`, `ClientIntegration`,
//!   `since` 1, `Durable`.
//! - `message.release_handle`, params `{handle}`, `Query`, `since` 1,
//!   `Durable`.
//!
//! All three answer the documented shapes and nothing else:
//! `{handle, path, name, bytes, expires_at}` for the two materialisers and
//! `{}` for the release.
//!
//! # Contract points this file pins beyond the plan text
//!
//! - **A message is addressed as `"<mailbox>/<uid>"`.** The plan's params say
//!   `{account, id}` without saying what an `id` is, and the daemon has no
//!   message identifier yet: `message.list` puts `uid` and `message_id` on the
//!   wire, the store's own key is `UNIQUE (account, mailbox, uid)`, and
//!   `docs/daemon-protocol.md` already spells a message resource
//!   `message:work/inbox/41`. So `id` is the last two thirds of that resource,
//!   which a client composes from the mailbox it listed and the `uid` of the
//!   row it is holding, and `account:id` reassembles the resource. The store's
//!   row id was the other candidate and is the wrong one: it is a rebuild away
//!   from meaning a different message, and `mp` rebuilds stores on a schema
//!   bump. The `message_id` header was the third and is ambiguous: the Inbox
//!   and the Sent copy of one message share it.
//! - **The mailbox in an id is not resolved through the account's mailbox
//!   roles.** The daemon queries `(account, mailbox, uid)` directly, so a
//!   message in a mailbox the configuration no longer lists is still
//!   materialisable. A handle is about bytes in the store, not about what the
//!   sidebar shows.
//! - **`part` is the zero-based index into the message's user-facing
//!   attachment list**, which is `store::read::attachments_for`'s order
//!   (`ORDER BY ordinal`, iMIP sidecar excluded). It is the index of the row a
//!   client is looking at, and it is dense: addressing by the store's raw
//!   `ordinal` would leak the hidden sidecar's position into a list it is
//!   deliberately absent from. Out of range is `-32602`.
//! - **The materialised file lives at
//!   `<data_dir>/runtime/handles/<handle>/<name>`**, one directory per handle,
//!   under the runtime directory the plan already fixes at mode 0700. One
//!   directory per handle is what makes the filename the sender's own
//!   (`vertrag.pdf`, not a hash) without two handles colliding, and what makes
//!   the release a directory removal derivable from the id alone. `<name>` is
//!   the sanitised attachment filename for an attachment - the same
//!   `sanitize_attachment_filename` rule `store::read::materialise_attachments`
//!   applies, so a hostile `Content-Disposition` cannot escape the directory -
//!   and `message.html` for a rendition, which is the name
//!   `tui::actions::html_temp_file` already gives it.
//! - **The HTML rendition is the browser rendition, not the raw markup**:
//!   `store::read::load_html`, then the charset and CSP injection the TUI's
//!   `b` binding applies before handing a `file://` URL to a browser. Serving
//!   unhardened markup through a new door would undo a fix (#0037) at the
//!   moment the GUI starts using it.
//! - **`bytes` is the length of the file at `path`**, not the size of the
//!   backing blob. For an attachment the two agree; for a rendition they do
//!   not, because the CSP meta tag is added after the blob is read, and a
//!   client that preallocates a buffer from `bytes` reads a file.
//! - **A handle pins every blob its materialisation read.** For an attachment
//!   that is one blob; for a rendition it is the `html` blob, plus the `raw`
//!   blob when the markup carried `cid:` references and the inline-image scan
//!   had to parse it. Hence a set rather than a single hash.
//! - **The two materialisers are `ClientIntegration`, the release is a
//!   `Query`.** The source plan lists "opening or revealing a materialized
//!   attachment" under the work only the client's process can do, and the
//!   daemon's half of that is precisely "prepare the file and hand back the
//!   instruction". The release changes no canonical state, publishes no event
//!   and moves no revision - handles are per-client scratch and appear in no
//!   snapshot - so `Command`, whose outcome must carry the revision it moved
//!   to, would have to invent one. `Query` is the kind whose outcome carries
//!   neither, which is the honest description even though the call does delete
//!   a file.
//! - **All three are `CancelScope::Durable`, which is how "a handle outlives
//!   its client" is *declared* rather than discovered.** A GUI that opens an
//!   attachment in a viewer and then loses its connection must not have the
//!   file pulled from under the viewer; expiry, not disconnection, is what ends
//!   a handle, and the plan says so ("Expiry, not immediate deletion, reclaims
//!   an unreleased handle").
//! - **No `Change` variant, no event.** Nothing about a handle belongs in the
//!   `state.bootstrap` snapshot: it is scratch owned by one client, and a
//!   second client that learned about it could only misuse it. This is why
//!   this unit adds no arm to the exhaustive `reduce` of
//!   `tests/daemon_bootstrap.rs`.
//! - **The daemon opens the store the way `message.list` does** - by path,
//!   through `crate::config::store_path` - and takes no engine lock of its own.
//!   Materialising is a read plus a write into the daemon's own runtime
//!   directory, and neither makes the daemon an account's engine.
//! - **The TTL default is ten minutes**, long enough for a person to look at
//!   what they opened and short enough that a crashed client's scratch is gone
//!   within one coffee, overridable through `MAILYPOPPINS_DAEMON_HANDLE_TTL_MS`
//!   for tests in the style of the watcher's two hooks. The value is a
//!   recommendation the plan does not fix; it is pinned here so the T and I
//!   units agree.

use std::collections::BTreeSet;
use std::fs;
use std::future::Future;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use chrono::{DateTime, TimeZone, Utc};
use serde_json::{json, Value};
use tempfile::TempDir;
use tokio::net::UnixStream;

use mp_client::{ClientError, ClientInfo, ClientKind, Connection, Identity};
use mp_protocol::RpcError;

use mailypoppins::config::RetentionPolicy;
use mailypoppins::daemon::dispatch::{CancelScope, MethodKind};
use mailypoppins::daemon::handles::{
    Handle, HandleId, HandleKind, HandleTable, DEFAULT_HANDLE_TTL, HANDLES_DIR, HANDLE_TTL_ENV,
};
use mailypoppins::daemon::methods::message::MESSAGE_HANDLE_METHOD_SPECS;
use mailypoppins::store::schema;
use mailypoppins::store::sweep::{
    sweep, sweep_pinned, SweepDecision, SweepOptions, RETENTION_OVER_CAP_MARKER,
};
use mailypoppins::store::{BlobHash, BlobStore, Store};

/// The binary the socket layer spawns.
const MP: &str = env!("CARGO_BIN_EXE_mp");

/// Every bounded wait in the socket layer.
const DEADLINE: Duration = Duration::from_secs(20);

/// How often a socket test re-checks something it is waiting for.
const TICK: Duration = Duration::from_millis(25);

/// The handle lifetime a sandboxed daemon runs with, in milliseconds. Long
/// enough that no test races its own handle away.
const SANDBOX_TTL_MS: u64 = 60_000;

/// The lifetime the one expiry test runs with, and the wait that outlives it.
const SHORT_TTL_MS: u64 = 400;
const AFTER_SHORT_TTL: Duration = Duration::from_millis(1_100);

/// The three methods this unit pins, in the order
/// [`MESSAGE_HANDLE_METHOD_SPECS`] declares them.
const HANDLE_METHODS: [&str; 3] = [
    "message.materialise_attachment",
    "message.materialise_html",
    "message.release_handle",
];

/// The keys a materialisation answers with, sorted.
const MATERIALISED_KEYS: [&str; 5] = ["bytes", "expires_at", "handle", "name", "path"];

/// JSON-RPC's own "invalid params", the answer to an unknown handle.
const INVALID_PARAMS: i32 = -32602;

/// The daemon's `account_unknown`.
const ACCOUNT_UNKNOWN: i32 = -32005;

// ---------------------------------------------------------------------------
// Layer (a) - the vocabulary
// ---------------------------------------------------------------------------

#[test]
fn the_three_methods_declare_their_names_kinds_since_and_cancel_scope() {
    let names: Vec<&str> = MESSAGE_HANDLE_METHOD_SPECS
        .iter()
        .map(|spec| spec.name)
        .collect();
    assert_eq!(
        names,
        HANDLE_METHODS.to_vec(),
        "the family is exactly the three methods the plan names, in name order"
    );

    let attachment = MESSAGE_HANDLE_METHOD_SPECS[0];
    let html = MESSAGE_HANDLE_METHOD_SPECS[1];
    let release = MESSAGE_HANDLE_METHOD_SPECS[2];

    assert_eq!(
        attachment.kind,
        MethodKind::ClientIntegration,
        "the daemon prepares the file and the client opens it"
    );
    assert_eq!(html.kind, MethodKind::ClientIntegration);
    assert_eq!(
        release.kind,
        MethodKind::Query,
        "a release moves no revision and invalidates no resource"
    );

    for spec in MESSAGE_HANDLE_METHOD_SPECS {
        assert_eq!(spec.since, 1, "{} is version 1 surface", spec.name);
        assert_eq!(
            spec.cancel_scope,
            CancelScope::Durable,
            "{}: a handle outlives the connection that asked for it",
            spec.name
        );
    }
}

#[test]
fn the_ttl_default_and_its_env_hook_are_the_documented_values() {
    assert_eq!(
        DEFAULT_HANDLE_TTL,
        Duration::from_secs(600),
        "ten minutes, pinned here so the T and I units agree"
    );
    assert_eq!(HANDLE_TTL_ENV, "MAILYPOPPINS_DAEMON_HANDLE_TTL_MS");
    assert_eq!(
        HANDLES_DIR, "handles",
        "the runtime subdirectory materialised files live in"
    );
}

#[test]
fn a_table_reports_the_lifetime_it_was_built_with() {
    let table = HandleTable::new(Duration::from_secs(42));
    assert_eq!(table.ttl(), Duration::from_secs(42));
}

#[test]
fn the_env_hook_overrides_the_default_lifetime_and_an_absent_one_does_not() {
    // The only test in this file that touches the process environment; nothing
    // else reads this variable in-process.
    std::env::remove_var(HANDLE_TTL_ENV);
    assert_eq!(
        HandleTable::from_env().ttl(),
        DEFAULT_HANDLE_TTL,
        "no variable means the default"
    );

    std::env::set_var(HANDLE_TTL_ENV, "1500");
    assert_eq!(
        HandleTable::from_env().ttl(),
        Duration::from_millis(1_500),
        "the hook is milliseconds"
    );

    std::env::set_var(HANDLE_TTL_ENV, "not a number");
    assert_eq!(
        HandleTable::from_env().ttl(),
        DEFAULT_HANDLE_TTL,
        "an unreadable value is the default, not a panic in a daemon's startup"
    );
    std::env::remove_var(HANDLE_TTL_ENV);
}

#[test]
fn the_two_handle_kinds_travel_as_the_words_the_store_uses() {
    assert_eq!(HandleKind::Attachment.as_str(), "attachment");
    assert_eq!(HandleKind::Html.as_str(), "html");
}

// ---------------------------------------------------------------------------
// Layer (a) - helpers
// ---------------------------------------------------------------------------

/// A fixed instant, so every expiry assertion is arithmetic rather than a race
/// with the wall clock.
fn t0() -> DateTime<Utc> {
    Utc.timestamp_opt(1_700_000_000, 0).single().expect("t0")
}

fn after(base: DateTime<Utc>, seconds: i64) -> DateTime<Utc> {
    base + chrono::Duration::seconds(seconds)
}

/// A table whose handles live one minute.
fn table() -> HandleTable {
    HandleTable::new(Duration::from_secs(60))
}

/// Materialise one handle over `blobs`, at `now`, with a path that need not
/// exist: layer (a) is bookkeeping and touches no filesystem.
fn pin(table: &HandleTable, account: &str, blobs: &[&str], now: DateTime<Utc>) -> Handle {
    let owned: Vec<String> = blobs.iter().map(|hash| hash.to_string()).collect();
    table.materialise(
        HandleKind::Attachment,
        account,
        PathBuf::from("/nonexistent/handle/file.pdf"),
        7,
        &owned,
        now,
    )
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|item| item.to_string()).collect()
}

// ---------------------------------------------------------------------------
// Layer (a) - the table
// ---------------------------------------------------------------------------

#[test]
fn a_materialised_handle_carries_what_it_was_given_and_expires_one_lifetime_ahead() {
    let table = table();
    let handle = table.materialise(
        HandleKind::Html,
        "alpha",
        PathBuf::from("/tmp/handles/x/message.html"),
        1234,
        &["deadbeef".to_string()],
        t0(),
    );

    assert_eq!(handle.kind, HandleKind::Html);
    assert_eq!(handle.account, "alpha");
    assert_eq!(handle.path, PathBuf::from("/tmp/handles/x/message.html"));
    assert_eq!(handle.bytes, 1234);
    assert_eq!(handle.blobs, set(&["deadbeef"]));
    assert_eq!(
        handle.expires_at,
        after(t0(), 60),
        "expires_at is now plus the table's lifetime"
    );
}

#[test]
fn a_handle_id_is_unique_and_usable_as_a_directory_name() {
    let table = table();
    let mut seen = BTreeSet::new();
    for _ in 0..64 {
        let handle = pin(&table, "alpha", &["aa"], t0());
        let id = handle.id.as_str().to_string();
        assert!(!id.is_empty(), "a handle id is never empty");
        assert!(
            id.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "a handle id is a directory name, got {id:?}"
        );
        assert!(seen.insert(id.clone()), "{id} was minted twice");
        assert_eq!(HandleId(id.clone()).as_str(), id, "the newtype round-trips");
    }
}

#[test]
fn a_live_handle_pins_every_blob_it_read() {
    let table = table();
    pin(&table, "alpha", &["aa", "bb"], t0());
    assert_eq!(table.pinned_blobs(t0()), set(&["aa", "bb"]));
    assert_eq!(
        table.pinned_blobs(after(t0(), 59)),
        set(&["aa", "bb"]),
        "still live one second before it expires"
    );
}

#[test]
fn an_empty_table_pins_nothing() {
    assert!(table().pinned_blobs(t0()).is_empty());
}

#[test]
fn releasing_a_handle_unpins_it_and_the_second_release_says_no() {
    let table = table();
    let handle = pin(&table, "alpha", &["aa"], t0());

    assert!(table.release(&handle.id), "the first release finds it");
    assert!(
        table.pinned_blobs(t0()).is_empty(),
        "a released handle pins nothing"
    );
    assert!(
        !table.release(&handle.id),
        "the second release finds nothing, which is what makes it -32602"
    );
}

#[test]
fn an_unknown_handle_is_never_released() {
    let table = table();
    pin(&table, "alpha", &["aa"], t0());
    assert!(!table.release(&HandleId("no-such-handle".to_string())));
    assert_eq!(
        table.pinned_blobs(t0()),
        set(&["aa"]),
        "a failed release changes nothing"
    );
}

#[test]
fn a_blob_two_handles_share_stays_pinned_until_the_last_one_goes() {
    let table = table();
    let first = pin(&table, "alpha", &["shared"], t0());
    let second = pin(&table, "alpha", &["shared"], t0());

    assert!(table.release(&first.id));
    assert_eq!(
        table.pinned_blobs(t0()),
        set(&["shared"]),
        "the second handle still holds it"
    );
    assert!(table.release(&second.id));
    assert!(table.pinned_blobs(t0()).is_empty());
}

#[test]
fn an_expired_handle_pins_nothing_from_its_expiry_instant_on() {
    let table = table();
    let handle = pin(&table, "alpha", &["aa"], t0());

    assert_eq!(
        table.pinned_blobs(handle.expires_at - chrono::Duration::seconds(1)),
        set(&["aa"]),
        "live strictly before the instant"
    );
    assert!(
        table.pinned_blobs(handle.expires_at).is_empty(),
        "the boundary belongs to the dead"
    );
    assert!(
        table.pinned_blobs(after(t0(), 600)).is_empty(),
        "and it does not come back"
    );
}

#[test]
fn expiry_needs_no_reaper_to_be_true() {
    // `pinned_blobs` answers "is this blob spoken for *now*", never "was the
    // last tick recent enough": a sweep must not depend on a reaper having run.
    let table = table();
    pin(&table, "alpha", &["aa"], t0());
    assert!(table.pinned_blobs(after(t0(), 61)).is_empty());
}

#[test]
fn expire_drops_the_dead_returns_their_ids_sorted_and_keeps_the_live() {
    let table = table();
    let early_one = pin(&table, "alpha", &["aa"], t0());
    let early_two = pin(&table, "alpha", &["bb"], t0());
    let late = pin(&table, "alpha", &["cc"], after(t0(), 30));

    let dropped = table.expire(after(t0(), 61));
    let mut expected = vec![early_one.id.clone(), early_two.id.clone()];
    expected.sort();
    assert_eq!(
        dropped, expected,
        "expire returns exactly the handles it dropped, sorted"
    );
    assert_eq!(
        table.pinned_blobs(after(t0(), 61)),
        set(&["cc"]),
        "the handle minted later is still live"
    );
    assert!(
        table.expire(after(t0(), 61)).is_empty(),
        "a second expire at the same instant drops nothing"
    );
    assert!(
        !table.release(&early_one.id),
        "an expired handle is gone, so releasing it is -32602"
    );
    assert!(table.release(&late.id), "the live one is still releasable");
}

#[test]
fn expire_leaves_a_table_whose_handles_are_all_live_alone() {
    let table = table();
    let handle = pin(&table, "alpha", &["aa"], t0());
    assert!(table.expire(after(t0(), 30)).is_empty());
    assert_eq!(table.pinned_blobs(after(t0(), 30)), set(&["aa"]));
    assert!(table.release(&handle.id));
}

// ---------------------------------------------------------------------------
// Layer (b) - a seeded store
// ---------------------------------------------------------------------------

/// A store and a blob directory in a tempdir, seeded row by row, which is how
/// `src/store/sweep.rs`'s own tests build one.
struct StoreFixture {
    _dir: TempDir,
    store: Store,
    blobs: BlobStore,
}

impl StoreFixture {
    fn new() -> StoreFixture {
        let dir = TempDir::new().expect("tempdir");
        let blobs = BlobStore::new(dir.path().join("blobs"));
        let store = Store::open(dir.path().join("store.sqlite3")).expect("open store");
        StoreFixture {
            _dir: dir,
            store,
            blobs,
        }
    }

    /// A store at an explicit path, for the socket layer's sandbox.
    fn at(store_path: PathBuf, blobs_dir: PathBuf) -> StoreFixture {
        let dir = TempDir::new().expect("tempdir");
        let blobs = BlobStore::new(blobs_dir);
        let store = Store::open(store_path).expect("open store");
        StoreFixture {
            _dir: dir,
            store,
            blobs,
        }
    }

    fn message(&self, account: &str, mailbox: &str, uid: i64, date_sort: i64) -> i64 {
        self.store
            .conn()
            .execute(
                "INSERT INTO messages (account, mailbox, uid, message_id, subject, date_sort, \
                 has_attachments) VALUES (?1, ?2, ?3, ?4, 'Angebot', ?5, 1)",
                (
                    account,
                    mailbox,
                    uid,
                    format!("<m{uid}@example.com>"),
                    date_sort,
                ),
            )
            .expect("insert message");
        self.store.conn().last_insert_rowid()
    }

    /// Write a blob, acquire one reference for `row`, and record the
    /// `message_blobs` row a read path resolves through.
    fn blob(
        &self,
        row: i64,
        kind: &str,
        ordinal: i64,
        filename: Option<&str>,
        bytes: &[u8],
    ) -> BlobHash {
        let hash = self.blobs.write(bytes).expect("write blob");
        let conn = self.store.conn();
        self.blobs
            .acquire(conn, &hash, bytes.len() as u64)
            .expect("acquire blob");
        conn.execute(
            "INSERT INTO message_blobs (message_row, kind, ordinal, hash, filename, size) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            (
                row,
                kind,
                ordinal,
                hash.as_str(),
                filename,
                bytes.len() as i64,
            ),
        )
        .expect("insert message_blobs");
        hash
    }

    /// A body blob of `size` bytes on a message dated `date_sort`, the shape
    /// the eviction plan orders by.
    fn body_of(&self, uid: i64, date_sort: i64, size: usize, seed: u8) -> BlobHash {
        let row = self.message("alpha", "inbox", uid, date_sort);
        self.blob(row, "body", 0, None, &vec![seed; size])
    }

    fn marker_set(&self) -> bool {
        schema::get_meta(self.store.conn(), RETENTION_OVER_CAP_MARKER)
            .expect("reading the marker")
            .is_some()
    }

    fn message_count(&self) -> i64 {
        self.store
            .conn()
            .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
            .expect("counting messages")
    }

    fn references(&self, hash: &BlobHash) -> i64 {
        self.store
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM message_blobs WHERE hash = ?1",
                [hash.as_str()],
                |row| row.get(0),
            )
            .expect("counting references")
    }
}

/// A cap-only policy, horizons off, so a test's cap is the only thing deciding.
fn policy(cap: u64) -> RetentionPolicy {
    RetentionPolicy {
        metadata_horizon_days: 0,
        body_horizon_days: 0,
        attachment_horizon_days: 0,
        max_disk_bytes: cap,
    }
}

/// The pin set a live handle over `hash` produces, which is the seam under
/// test: the daemon's table feeds the store's planner and nothing in between
/// reshapes it.
fn pinned_set(table: &HandleTable, now: DateTime<Utc>) -> BTreeSet<String> {
    table.pinned_blobs(now)
}

// ---------------------------------------------------------------------------
// Layer (b) - the sweep honours the pin (ANO-6)
// ---------------------------------------------------------------------------

#[test]
fn a_sweep_with_an_open_handle_evicts_nothing_that_handle_references() {
    let f = StoreFixture::new();
    // Five 1000-byte bodies, oldest first; a cap of 3000 wants the oldest two.
    let mut hashes = Vec::new();
    for uid in 1..=5 {
        hashes.push(f.body_of(uid, uid * 1000, 1000, uid as u8));
    }
    let pol = policy(3000);

    // A client is holding the oldest blob, which is exactly the first victim.
    let table = table();
    let handle = table.materialise(
        HandleKind::Attachment,
        "alpha",
        PathBuf::from("/tmp/handles/h/vertrag.pdf"),
        1000,
        &[hashes[0].to_string()],
        t0(),
    );
    let pinned = pinned_set(&table, t0());

    let warned = sweep_pinned(&f.store, &f.blobs, &pol, SweepOptions::default(), &pinned).unwrap();
    assert_eq!(
        warned.decision,
        SweepDecision::WarnedFirstBreach,
        "the marker rule comes first, pins or no pins"
    );

    let out = sweep_pinned(&f.store, &f.blobs, &pol, SweepOptions::default(), &pinned).unwrap();
    assert_eq!(out.decision, SweepDecision::Evicted);
    assert!(
        !out.evicted
            .iter()
            .any(|blob| blob.hash == hashes[0].to_string()),
        "the pinned blob is not in the plan, got {:?}",
        out.evicted
    );
    assert!(
        f.blobs.contains(&hashes[0]),
        "the file a client has open survives the sweep (ANO-6)"
    );
    // The eviction still happens, on the next-oldest victims instead.
    assert!(!f.blobs.contains(&hashes[1]));
    assert!(!f.blobs.contains(&hashes[2]));
    assert!(f.blobs.contains(&hashes[3]));
    assert_eq!(
        f.message_count(),
        5,
        "a sweep never touches a message row, pinned or not"
    );
    assert_eq!(
        f.references(&hashes[1]),
        1,
        "nor the reference of an evicted blob"
    );
    assert!(table.release(&handle.id));
}

#[test]
fn a_pinned_blob_still_counts_towards_the_store_size() {
    let f = StoreFixture::new();
    let hash = f.body_of(1, 1000, 1000, 1);
    let pol = policy(500);

    let table = table();
    table.materialise(
        HandleKind::Attachment,
        "alpha",
        PathBuf::from("/tmp/handles/h/file"),
        1000,
        &[hash.to_string()],
        t0(),
    );
    let pinned = pinned_set(&table, t0());

    let out = sweep_pinned(&f.store, &f.blobs, &pol, SweepOptions::default(), &pinned).unwrap();
    assert_eq!(
        out.before_bytes, 1000,
        "the pin hides a blob from the plan, never from the store's size"
    );
    assert_eq!(
        out.decision,
        SweepDecision::WarnedFirstBreach,
        "the store is over its cap even though nothing may be evicted"
    );
}

#[test]
fn a_sweep_that_cannot_reach_the_cap_because_of_a_pin_keeps_the_marker() {
    let f = StoreFixture::new();
    let pinned_hash = f.body_of(1, 1000, 1000, 1);
    let free_hash = f.body_of(2, 2000, 500, 2);
    // Cap 800 over 1500 resident bytes: without the pin, evicting the oldest
    // 1000 would be enough; with it, everything evictable still leaves 1000.
    let pol = policy(800);

    let table = table();
    table.materialise(
        HandleKind::Attachment,
        "alpha",
        PathBuf::from("/tmp/handles/h/file"),
        1000,
        &[pinned_hash.to_string()],
        t0(),
    );
    let pinned = pinned_set(&table, t0());

    sweep_pinned(&f.store, &f.blobs, &pol, SweepOptions::default(), &pinned).unwrap();
    let out = sweep_pinned(&f.store, &f.blobs, &pol, SweepOptions::default(), &pinned).unwrap();
    assert_eq!(out.decision, SweepDecision::Evicted);
    assert!(!f.blobs.contains(&free_hash), "the unpinned blob went");
    assert!(f.blobs.contains(&pinned_hash));
    assert!(
        out.after_bytes > pol.max_disk_bytes,
        "still over the cap, because the rest is spoken for"
    );
    assert!(
        f.marker_set(),
        "the marker stays set while the store is over its cap, so the next \
         sweep after the release evicts"
    );
}

// ---------------------------------------------------------------------------
// Layer (b) - the two ANO-5 rules survive the pin
// ---------------------------------------------------------------------------

#[test]
fn a_pinned_sweep_still_warns_once_before_it_evicts_anything() {
    let f = StoreFixture::new();
    let pinned_hash = f.body_of(1, 1000, 1000, 1);
    let free_one = f.body_of(2, 2000, 1000, 2);
    let free_two = f.body_of(3, 3000, 1000, 3);
    let pol = policy(2500);

    let table = table();
    table.materialise(
        HandleKind::Attachment,
        "alpha",
        PathBuf::from("/tmp/handles/h/file"),
        1000,
        &[pinned_hash.to_string()],
        t0(),
    );
    let pinned = pinned_set(&table, t0());

    let first = sweep_pinned(&f.store, &f.blobs, &pol, SweepOptions::default(), &pinned).unwrap();
    assert_eq!(first.decision, SweepDecision::WarnedFirstBreach);
    assert!(first.evicted.is_empty(), "the first breach evicts nothing");
    assert!(f.marker_set(), "and it persists the marker");
    assert!(f.blobs.contains(&free_one));
    assert!(f.blobs.contains(&free_two));
    assert!(f.blobs.contains(&pinned_hash));

    let second = sweep_pinned(&f.store, &f.blobs, &pol, SweepOptions::default(), &pinned).unwrap();
    assert_eq!(second.decision, SweepDecision::Evicted);
    assert_eq!(
        second.evicted.len(),
        1,
        "the second over-cap sweep evicts, skipping the pin"
    );
    assert_eq!(second.evicted[0].hash, free_one.to_string());
    assert!(f.blobs.contains(&pinned_hash));
}

#[test]
fn a_pinned_sweep_still_refuses_a_plan_over_half_the_store_without_force() {
    let f = StoreFixture::new();
    // 4000 bytes: one pinned, three free. A cap of 500 wants all four; with the
    // pin the plan is 3000, which is still more than half of 4000, so the guard
    // fires on what the sweep would really have deleted.
    let pinned_hash = f.body_of(1, 1000, 1000, 1);
    let free = [
        f.body_of(2, 2000, 1000, 2),
        f.body_of(3, 3000, 1000, 3),
        f.body_of(4, 4000, 1000, 4),
    ];
    let pol = policy(500);

    let table = table();
    let handle = table.materialise(
        HandleKind::Attachment,
        "alpha",
        PathBuf::from("/tmp/handles/h/file"),
        1000,
        &[pinned_hash.to_string()],
        t0(),
    );
    let pinned = pinned_set(&table, t0());

    sweep_pinned(&f.store, &f.blobs, &pol, SweepOptions::default(), &pinned).unwrap(); // warn
    let refused = sweep_pinned(&f.store, &f.blobs, &pol, SweepOptions::default(), &pinned).unwrap();
    match refused.decision {
        SweepDecision::RefusedTooMuch { would_evict_bytes } => assert_eq!(
            would_evict_bytes, 3000,
            "the refusal reports the pinned-free plan, not the plan it never had"
        ),
        other => panic!("the half-store guard must still refuse, got {other:?}"),
    }
    assert!(refused.evicted.is_empty(), "a refusal evicts nothing");
    for hash in &free {
        assert!(f.blobs.contains(hash), "nothing went");
    }
    assert!(f.blobs.contains(&pinned_hash));
    assert!(table.release(&handle.id));
}

#[test]
fn force_overrules_the_half_store_guard_and_never_the_pin() {
    let f = StoreFixture::new();
    let pinned_hash = f.body_of(1, 1000, 1000, 1);
    let free = [
        f.body_of(2, 2000, 1000, 2),
        f.body_of(3, 3000, 1000, 3),
        f.body_of(4, 4000, 1000, 4),
    ];
    let pol = policy(500);

    let table = table();
    table.materialise(
        HandleKind::Attachment,
        "alpha",
        PathBuf::from("/tmp/handles/h/file"),
        1000,
        &[pinned_hash.to_string()],
        t0(),
    );
    let pinned = pinned_set(&table, t0());
    let forced = SweepOptions {
        dry_run: false,
        force: true,
    };

    sweep_pinned(&f.store, &f.blobs, &pol, SweepOptions::default(), &pinned).unwrap(); // warn
    let out = sweep_pinned(&f.store, &f.blobs, &pol, forced, &pinned).unwrap();
    assert_eq!(out.decision, SweepDecision::Evicted);
    assert_eq!(out.evicted.len(), 3, "every unpinned blob went");
    for hash in &free {
        assert!(!f.blobs.contains(hash));
    }
    assert!(
        f.blobs.contains(&pinned_hash),
        "--force is a fat-finger override, not a licence to pull a file out \
         from under a viewer"
    );
}

#[test]
fn a_pinned_dry_run_reports_the_pinned_free_plan_and_changes_nothing() {
    let f = StoreFixture::new();
    let pinned_hash = f.body_of(1, 1000, 1000, 1);
    let free = f.body_of(2, 2000, 1000, 2);
    let pol = policy(1500);

    let table = table();
    table.materialise(
        HandleKind::Attachment,
        "alpha",
        PathBuf::from("/tmp/handles/h/file"),
        1000,
        &[pinned_hash.to_string()],
        t0(),
    );
    let pinned = pinned_set(&table, t0());

    sweep_pinned(&f.store, &f.blobs, &pol, SweepOptions::default(), &pinned).unwrap(); // warn
    let out = sweep_pinned(
        &f.store,
        &f.blobs,
        &pol,
        SweepOptions {
            dry_run: true,
            force: false,
        },
        &pinned,
    )
    .unwrap();
    assert_eq!(out.decision, SweepDecision::Evicted);
    assert_eq!(out.evicted.len(), 1);
    assert_eq!(out.evicted[0].hash, free.to_string());
    assert!(f.blobs.contains(&free), "a dry run deletes nothing");
    assert!(f.blobs.contains(&pinned_hash));
}

// ---------------------------------------------------------------------------
// Layer (b) - the pin ends, the bytes come back
// ---------------------------------------------------------------------------

#[test]
fn releasing_the_handle_then_sweeping_reclaims_the_blob() {
    let f = StoreFixture::new();
    let hash = f.body_of(1, 1000, 1000, 1);
    let other = f.body_of(2, 2000, 1000, 2);
    // A cap low enough that evicting the unpinned blob does not end the
    // breach, so the store is still over its cap when the handle goes.
    let pol = policy(500);

    let table = table();
    let handle = table.materialise(
        HandleKind::Attachment,
        "alpha",
        PathBuf::from("/tmp/handles/h/file"),
        1000,
        &[hash.to_string()],
        t0(),
    );

    sweep_pinned(
        &f.store,
        &f.blobs,
        &pol,
        SweepOptions::default(),
        &pinned_set(&table, t0()),
    )
    .unwrap(); // warn
    let held = sweep_pinned(
        &f.store,
        &f.blobs,
        &pol,
        SweepOptions::default(),
        &pinned_set(&table, t0()),
    )
    .unwrap();
    assert_eq!(held.evicted.len(), 1, "only the unpinned one");
    assert_eq!(held.evicted[0].hash, other.to_string());
    assert!(f.blobs.contains(&hash));

    assert!(table.release(&handle.id));
    let after_release = sweep_pinned(
        &f.store,
        &f.blobs,
        &pol,
        SweepOptions {
            dry_run: false,
            force: true,
        },
        &pinned_set(&table, t0()),
    )
    .unwrap();
    assert_eq!(after_release.decision, SweepDecision::Evicted);
    assert_eq!(after_release.evicted[0].hash, hash.to_string());
    assert!(
        !f.blobs.contains(&hash),
        "the released blob is reclaimable again"
    );
    assert_eq!(f.message_count(), 2, "and the rows are still there");
}

#[test]
fn an_expired_handle_stops_pinning_and_the_next_sweep_reclaims_it() {
    let f = StoreFixture::new();
    let hash = f.body_of(1, 1000, 1000, 1);
    let other = f.body_of(2, 2000, 1000, 2);
    // As above: still over the cap once the unpinned blob is gone.
    let pol = policy(500);

    let table = table();
    table.materialise(
        HandleKind::Attachment,
        "alpha",
        PathBuf::from("/tmp/handles/h/file"),
        1000,
        &[hash.to_string()],
        t0(),
    );

    sweep_pinned(
        &f.store,
        &f.blobs,
        &pol,
        SweepOptions::default(),
        &pinned_set(&table, t0()),
    )
    .unwrap(); // warn
    sweep_pinned(
        &f.store,
        &f.blobs,
        &pol,
        SweepOptions::default(),
        &pinned_set(&table, t0()),
    )
    .unwrap();
    assert!(f.blobs.contains(&hash), "held while the handle is live");
    assert!(!f.blobs.contains(&other));

    // Nobody released it and nobody reaped it; the lifetime simply ran out.
    let later = after(t0(), 61);
    let out = sweep_pinned(
        &f.store,
        &f.blobs,
        &pol,
        SweepOptions {
            dry_run: false,
            force: true,
        },
        &pinned_set(&table, later),
    )
    .unwrap();
    assert_eq!(out.decision, SweepDecision::Evicted);
    assert_eq!(out.evicted[0].hash, hash.to_string());
    assert!(
        !f.blobs.contains(&hash),
        "expiry, not deletion, is what reclaims an unreleased handle"
    );
}

#[test]
fn sweeping_with_no_pins_is_the_unpinned_sweep() {
    // The delegation seam: `sweep` is `sweep_pinned` with an empty set, so
    // `mp store gc` keeps its signature and the daemon adds the one argument.
    let plain = StoreFixture::new();
    let pinned = StoreFixture::new();
    for uid in 1..=4 {
        plain.body_of(uid, uid * 1000, 1000, uid as u8);
        pinned.body_of(uid, uid * 1000, 1000, uid as u8);
    }
    let pol = policy(3000);
    let empty: BTreeSet<String> = BTreeSet::new();

    sweep(&plain.store, &plain.blobs, &pol, SweepOptions::default()).unwrap();
    sweep_pinned(
        &pinned.store,
        &pinned.blobs,
        &pol,
        SweepOptions::default(),
        &empty,
    )
    .unwrap();

    let a = sweep(&plain.store, &plain.blobs, &pol, SweepOptions::default()).unwrap();
    let b = sweep_pinned(
        &pinned.store,
        &pinned.blobs,
        &pol,
        SweepOptions::default(),
        &empty,
    )
    .unwrap();

    assert_eq!(a.decision, b.decision);
    assert_eq!(a.before_bytes, b.before_bytes);
    assert_eq!(a.after_bytes, b.after_bytes);
    assert_eq!(
        a.evicted
            .iter()
            .map(|blob| blob.hash.clone())
            .collect::<Vec<_>>(),
        b.evicted
            .iter()
            .map(|blob| blob.hash.clone())
            .collect::<Vec<_>>(),
        "same store, same plan"
    );
}

#[test]
fn a_pin_over_a_blob_no_plan_wanted_changes_nothing() {
    let f = StoreFixture::new();
    let newest = f.body_of(9, 9000, 1000, 9);
    for uid in 1..=3 {
        f.body_of(uid, uid * 1000, 1000, uid as u8);
    }
    let pol = policy(3000);

    let table = table();
    table.materialise(
        HandleKind::Attachment,
        "alpha",
        PathBuf::from("/tmp/handles/h/file"),
        1000,
        &[newest.to_string()],
        t0(),
    );
    let pinned = pinned_set(&table, t0());

    sweep_pinned(&f.store, &f.blobs, &pol, SweepOptions::default(), &pinned).unwrap(); // warn
    let out = sweep_pinned(&f.store, &f.blobs, &pol, SweepOptions::default(), &pinned).unwrap();
    assert_eq!(out.decision, SweepDecision::Evicted);
    assert_eq!(out.evicted.len(), 1, "the oldest, as ever");
    assert!(f.blobs.contains(&newest));
}

// ---------------------------------------------------------------------------
// Layer (c) - the sandbox
// ---------------------------------------------------------------------------

/// A private `HOME`, config directory and data directory, with a seeded store
/// and a daemon that can be talked to.
///
/// Dropping it kills whatever daemon `daemon.pid` names, so nothing outlives
/// the test that started it, even when the test panics half way through.
struct Sandbox {
    root: TempDir,
    ttl_ms: u64,
}

impl Sandbox {
    fn with_account(account: &str) -> Sandbox {
        Sandbox::with_account_at(account, SANDBOX_TTL_MS)
    }

    fn with_account_at(account: &str, ttl_ms: u64) -> Sandbox {
        let root = TempDir::new().expect("tempdir");
        for sub in ["home", "config", "data"] {
            fs::create_dir_all(root.path().join(sub)).expect("sandbox subdir");
        }
        let sandbox = Sandbox { root, ttl_ms };
        fs::write(sandbox.config_path(), account_toml(account)).expect("write config.toml");
        fs::create_dir_all(sandbox.account_dir(account)).expect("account dir");
        sandbox
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    fn config_dir(&self) -> PathBuf {
        self.root.path().join("config")
    }

    fn data_dir(&self) -> PathBuf {
        self.root.path().join("data")
    }

    fn config_path(&self) -> PathBuf {
        self.config_dir().join("config.toml")
    }

    fn account_dir(&self, account: &str) -> PathBuf {
        self.data_dir().join("accounts").join(account)
    }

    fn store_path(&self, account: &str) -> PathBuf {
        self.account_dir(account).join("store.sqlite3")
    }

    fn blobs_dir(&self, account: &str) -> PathBuf {
        self.account_dir(account).join("blobs")
    }

    /// Where a handle's file must land: one directory per handle under the
    /// runtime directory.
    fn handle_dir(&self, handle: &str) -> PathBuf {
        self.data_dir()
            .join("runtime")
            .join(HANDLES_DIR)
            .join(handle)
    }

    fn socket(&self) -> PathBuf {
        self.data_dir().join("runtime").join("daemon.sock")
    }

    fn pid_file(&self) -> PathBuf {
        self.data_dir().join("runtime").join("daemon.pid")
    }

    fn identity(&self) -> Identity {
        Identity {
            data_dir: self.data_dir(),
            config_dir: self.config_dir(),
        }
    }

    /// Seed the account's store in this process, before any daemon exists, so
    /// the daemon opens a store that is already there.
    fn seed(&self, account: &str) -> StoreFixture {
        StoreFixture::at(self.store_path(account), self.blobs_dir(account))
    }

    /// An `mp` invocation pointed at this sandbox, with the lifetime hook set
    /// and every other test-only hook explicitly cleared so an inherited
    /// variable cannot change an outcome.
    fn cmd(&self) -> Command {
        let mut cmd = Command::new(MP);
        cmd.env("HOME", self.home())
            .env("MAILYPOPPINS_DATA_DIR", self.data_dir())
            .env("MAILYPOPPINS_CONFIG_DIR", self.config_dir())
            .env(HANDLE_TTL_ENV, self.ttl_ms.to_string())
            .env_remove("MAILYPOPPINS_DAEMON_FAIL_START")
            .env_remove("MAILYPOPPINS_DAEMON_FAKE_READY_AFTER_MS")
            .env_remove("MAILYPOPPINS_DAEMON_FAKE_EVENT_BURST")
            .env_remove("MAILYPOPPINS_DAEMON_FAKE_OPERATIONS")
            .env_remove("MAILYPOPPINS_DAEMON_FAKE_SYNC_OUTCOME");
        cmd
    }

    /// Spawn `mp daemon run`, killed on drop, and wait until its socket
    /// accepts a connection. Its stdio goes nowhere.
    ///
    /// Materialising is a store read and a write into the daemon's own runtime
    /// directory, so it takes no engine lock of its own, exactly as
    /// `message.list` does not.
    async fn start_daemon(&self) -> Proc {
        let child = self
            .cmd()
            .args(["daemon", "run"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn mp daemon run");
        let proc = Proc(Some(child));
        self.wait_socket_live().await;
        proc
    }

    async fn wait_socket_live(&self) {
        let start = Instant::now();
        loop {
            if UnixStream::connect(self.socket()).await.is_ok() {
                return;
            }
            assert!(
                start.elapsed() < DEADLINE,
                "the daemon socket {} never accepted a connection within {DEADLINE:?}",
                self.socket().display()
            );
            tokio::time::sleep(TICK).await;
        }
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        if let Ok(raw) = fs::read_to_string(self.pid_file()) {
            if let Ok(pid) = raw.trim().parse::<i32>() {
                if pid > 1 {
                    // Safety: a pid read from a pid file we own.
                    unsafe { libc::kill(pid, libc::SIGKILL) };
                }
            }
        }
    }
}

/// A spawned `mp` process, killed and reaped on drop, so a panicking assertion
/// never leaves a daemon behind.
struct Proc(Option<Child>);

impl Drop for Proc {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

// ---------------------------------------------------------------------------
// Layer (c) - helpers
// ---------------------------------------------------------------------------

/// One account's `[[accounts]]` block, as a user would write it. No IMAP or
/// SMTP host: nothing in this file connects to anything.
fn account_toml(name: &str) -> String {
    format!(
        "[[accounts]]\n\
         name = \"{name}\"\n\
         default_from = \"{name}@example.com\"\n\
         \n\
         [accounts.mailboxes.inbox]\n\
         server = \"INBOX\"\n\
         \n"
    )
}

/// Await `fut` under [`DEADLINE`], failing the test rather than the suite's
/// patience if the daemon never answers.
async fn within<T>(label: &str, fut: impl Future<Output = T>) -> T {
    match tokio::time::timeout(DEADLINE, fut).await {
        Ok(value) => value,
        Err(_) => panic!("{label} did not finish within {DEADLINE:?}"),
    }
}

fn client_info() -> ClientInfo {
    ClientInfo {
        kind: ClientKind::Gui,
        app_version: env!("CARGO_PKG_VERSION").to_string(),
    }
}

/// Connect and handshake.
async fn connected(sandbox: &Sandbox) -> Connection {
    let mut conn = within(
        "Connection::connect",
        Connection::connect(&sandbox.socket()),
    )
    .await
    .expect("connecting to a live daemon socket succeeds");
    within(
        "Connection::initialize",
        conn.initialize(client_info(), sandbox.identity(), &[], &[]),
    )
    .await
    .expect("a compatible handshake succeeds");
    conn
}

async fn call_ok(conn: &mut Connection, method: &str, params: Value) -> Value {
    within(method, conn.call(method, params))
        .await
        .unwrap_or_else(|e| panic!("{method} was expected to succeed, got {e:?}"))
}

async fn call_err(conn: &mut Connection, method: &str, params: Value) -> RpcError {
    let error = within(method, conn.call(method, params))
        .await
        .expect_err("the call was expected to fail");
    match error {
        ClientError::Rpc(error) => error,
        other => {
            panic!("{method} failed for a transport reason rather than a domain one: {other:?}")
        }
    }
}

fn sorted_keys(value: &Value) -> Vec<String> {
    let mut keys: Vec<String> = value
        .as_object()
        .unwrap_or_else(|| panic!("expected a JSON object, got {value}"))
        .keys()
        .cloned()
        .collect();
    keys.sort();
    keys
}

fn assert_keys(value: &Value, expected: &[&str], what: &str) {
    assert_eq!(
        sorted_keys(value),
        expected.iter().map(|k| k.to_string()).collect::<Vec<_>>(),
        "{what} has exactly the documented keys, got {value}"
    );
}

/// The `handle` and `path` of a materialisation, with every documented
/// invariant of the shape checked on the way through.
fn materialised(sandbox: &Sandbox, answer: &Value, expected_name: &str) -> (String, PathBuf) {
    assert_keys(answer, &MATERIALISED_KEYS, "a materialisation");

    let handle = answer["handle"]
        .as_str()
        .unwrap_or_else(|| panic!("handle is a string, got {answer}"))
        .to_string();
    assert!(
        handle
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            && !handle.is_empty(),
        "the handle is a directory-safe name, got {handle:?}"
    );

    assert_eq!(
        answer["name"].as_str(),
        Some(expected_name),
        "name is the file's own name under the handle directory, got {answer}"
    );

    let path = PathBuf::from(
        answer["path"]
            .as_str()
            .unwrap_or_else(|| panic!("path is a string, got {answer}")),
    );
    assert!(path.is_absolute(), "a client opens an absolute path");
    assert_eq!(
        path,
        sandbox.handle_dir(&handle).join(expected_name),
        "the file lands in this handle's own directory under the runtime dir"
    );
    assert!(path.exists(), "the file is on disk when the answer arrives");

    let bytes = answer["bytes"]
        .as_u64()
        .unwrap_or_else(|| panic!("bytes is an unsigned integer, got {answer}"));
    assert_eq!(
        bytes,
        fs::metadata(&path).expect("stat the handle file").len(),
        "bytes is the length of the file at path"
    );

    let expires_at = answer["expires_at"]
        .as_str()
        .unwrap_or_else(|| panic!("expires_at is a string, got {answer}"));
    let parsed = DateTime::parse_from_rfc3339(expires_at)
        .unwrap_or_else(|e| panic!("expires_at is RFC 3339: {e}; got {expires_at:?}"))
        .with_timezone(&Utc);
    let ahead = parsed.signed_duration_since(Utc::now());
    assert!(
        ahead > chrono::Duration::zero()
            && ahead <= chrono::Duration::milliseconds(sandbox.ttl_ms as i64),
        "expires_at is at most one lifetime ahead and has not already passed, got {expires_at}"
    );

    (handle, path)
}

/// A message with one attachment and one HTML rendition, the row every socket
/// test addresses as `inbox/41`.
fn seed_one_message(sandbox: &Sandbox, account: &str) -> (Vec<u8>, String) {
    let fixture = sandbox.seed(account);
    let row = fixture.message(account, "inbox", 41, 1_700_000_000);
    let attachment = b"%PDF-1.7 a contract that is not really a pdf".to_vec();
    fixture.blob(row, "attachment", 0, Some("vertrag.pdf"), &attachment);
    let html = "<html><head><title>Angebot</title></head><body><p>Guten Tag</p></body></html>";
    fixture.blob(row, "html", 0, None, html.as_bytes());
    fixture.blob(row, "body", 0, None, b"Guten Tag\n");
    (attachment, html.to_string())
}

// ---------------------------------------------------------------------------
// Layer (c) - the methods over the socket
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_three_methods_are_advertised_at_the_handshake() {
    let sandbox = Sandbox::with_account("alpha");
    seed_one_message(&sandbox, "alpha");
    let _daemon = sandbox.start_daemon().await;

    let mut conn = within(
        "Connection::connect",
        Connection::connect(&sandbox.socket()),
    )
    .await
    .expect("connect");
    let hello = within(
        "Connection::initialize",
        conn.initialize(client_info(), sandbox.identity(), &[], &[]),
    )
    .await
    .expect("handshake");

    for name in HANDLE_METHODS {
        assert!(
            hello.capabilities.iter().any(|cap| cap == name),
            "{name} is advertised, got {:?}",
            hello.capabilities
        );
    }
}

#[tokio::test]
async fn materialising_an_attachment_answers_a_handle_a_path_and_an_expiry() {
    let sandbox = Sandbox::with_account("alpha");
    let (attachment, _) = seed_one_message(&sandbox, "alpha");
    let _daemon = sandbox.start_daemon().await;
    let mut conn = connected(&sandbox).await;

    let answer = call_ok(
        &mut conn,
        "message.materialise_attachment",
        json!({"account": "alpha", "id": "inbox/41", "part": 0}),
    )
    .await;
    let (_handle, path) = materialised(&sandbox, &answer, "vertrag.pdf");

    assert_eq!(
        fs::read(&path).expect("read the materialised attachment"),
        attachment,
        "the file is the attachment's bytes, under the name the sender gave it"
    );
}

#[tokio::test]
async fn materialising_the_html_rendition_answers_the_same_shape_and_a_hardened_file() {
    let sandbox = Sandbox::with_account("alpha");
    seed_one_message(&sandbox, "alpha");
    let _daemon = sandbox.start_daemon().await;
    let mut conn = connected(&sandbox).await;

    let answer = call_ok(
        &mut conn,
        "message.materialise_html",
        json!({"account": "alpha", "id": "inbox/41"}),
    )
    .await;
    let (_handle, path) = materialised(&sandbox, &answer, "message.html");

    let rendition = fs::read_to_string(&path).expect("read the rendition");
    assert!(
        rendition.contains("Guten Tag"),
        "the rendition is the message's markup, got {rendition}"
    );
    assert!(
        rendition.contains("Content-Security-Policy"),
        "a file:// rendition carries the CSP meta tag (#0037), got {rendition}"
    );
    assert!(
        rendition.to_ascii_lowercase().contains("charset"),
        "and a charset, so umlauts survive the browser's guess, got {rendition}"
    );
}

#[tokio::test]
async fn two_materialisations_of_one_message_are_two_handles_in_two_directories() {
    let sandbox = Sandbox::with_account("alpha");
    seed_one_message(&sandbox, "alpha");
    let _daemon = sandbox.start_daemon().await;
    let mut conn = connected(&sandbox).await;

    let params = json!({"account": "alpha", "id": "inbox/41", "part": 0});
    let first = call_ok(&mut conn, "message.materialise_attachment", params.clone()).await;
    let second = call_ok(&mut conn, "message.materialise_attachment", params).await;
    let (first_handle, first_path) = materialised(&sandbox, &first, "vertrag.pdf");
    let (second_handle, second_path) = materialised(&sandbox, &second, "vertrag.pdf");

    assert_ne!(
        first_handle, second_handle,
        "each call mints its own handle"
    );
    assert_ne!(first_path, second_path);

    // Releasing one leaves the other's file where it is: one directory per
    // handle is what makes that true.
    call_ok(
        &mut conn,
        "message.release_handle",
        json!({"handle": first_handle}),
    )
    .await;
    assert!(!first_path.exists());
    assert!(second_path.exists(), "the other handle is untouched");
}

#[tokio::test]
async fn a_hostile_attachment_name_cannot_escape_its_handle_directory() {
    let sandbox = Sandbox::with_account("alpha");
    let fixture = sandbox.seed("alpha");
    let row = fixture.message("alpha", "inbox", 41, 1_700_000_000);
    fixture.blob(
        row,
        "attachment",
        0,
        Some("../../escaped.pdf"),
        b"not where you wanted me",
    );
    drop(fixture);
    let _daemon = sandbox.start_daemon().await;
    let mut conn = connected(&sandbox).await;

    let answer = call_ok(
        &mut conn,
        "message.materialise_attachment",
        json!({"account": "alpha", "id": "inbox/41", "part": 0}),
    )
    .await;
    let handle = answer["handle"].as_str().expect("a handle").to_string();
    let path = PathBuf::from(answer["path"].as_str().expect("a path"));

    assert!(
        path.starts_with(sandbox.handle_dir(&handle)),
        "the sanitised name keeps the file inside its handle directory, got {}",
        path.display()
    );
    assert!(
        !sandbox.data_dir().join("escaped.pdf").exists()
            && !sandbox.root.path().join("escaped.pdf").exists(),
        "nothing was written outside the handle directory"
    );
}

#[tokio::test]
async fn releasing_a_handle_answers_an_empty_object_and_removes_the_file() {
    let sandbox = Sandbox::with_account("alpha");
    seed_one_message(&sandbox, "alpha");
    let _daemon = sandbox.start_daemon().await;
    let mut conn = connected(&sandbox).await;

    let answer = call_ok(
        &mut conn,
        "message.materialise_attachment",
        json!({"account": "alpha", "id": "inbox/41", "part": 0}),
    )
    .await;
    let (handle, path) = materialised(&sandbox, &answer, "vertrag.pdf");

    let released = call_ok(
        &mut conn,
        "message.release_handle",
        json!({"handle": handle}),
    )
    .await;
    assert_eq!(released, json!({}), "the release answers an empty object");
    assert!(
        !path.exists(),
        "the release takes the scratch file with it, {} is still there",
        path.display()
    );
    assert!(
        !sandbox.handle_dir(&handle).exists(),
        "and its directory too"
    );
}

#[tokio::test]
async fn releasing_an_unknown_handle_is_invalid_params() {
    let sandbox = Sandbox::with_account("alpha");
    seed_one_message(&sandbox, "alpha");
    let _daemon = sandbox.start_daemon().await;
    let mut conn = connected(&sandbox).await;

    let error = call_err(
        &mut conn,
        "message.release_handle",
        json!({"handle": "no-such-handle"}),
    )
    .await;
    assert_eq!(error.code, INVALID_PARAMS, "{}", error.message);

    let missing = call_err(&mut conn, "message.release_handle", json!({})).await;
    assert_eq!(
        missing.code, INVALID_PARAMS,
        "a missing handle names the parameter: {}",
        missing.message
    );
}

#[tokio::test]
async fn releasing_the_same_handle_twice_is_invalid_params_the_second_time() {
    let sandbox = Sandbox::with_account("alpha");
    seed_one_message(&sandbox, "alpha");
    let _daemon = sandbox.start_daemon().await;
    let mut conn = connected(&sandbox).await;

    let answer = call_ok(
        &mut conn,
        "message.materialise_html",
        json!({"account": "alpha", "id": "inbox/41"}),
    )
    .await;
    let handle = answer["handle"].as_str().expect("a handle").to_string();

    call_ok(
        &mut conn,
        "message.release_handle",
        json!({"handle": handle.clone()}),
    )
    .await;
    let again = call_err(
        &mut conn,
        "message.release_handle",
        json!({"handle": handle}),
    )
    .await;
    assert_eq!(again.code, INVALID_PARAMS, "{}", again.message);
}

#[tokio::test]
async fn a_handle_outlives_the_client_that_opened_it() {
    let sandbox = Sandbox::with_account("alpha");
    seed_one_message(&sandbox, "alpha");
    let _daemon = sandbox.start_daemon().await;

    let (handle, path) = {
        let mut opener = connected(&sandbox).await;
        let answer = call_ok(
            &mut opener,
            "message.materialise_attachment",
            json!({"account": "alpha", "id": "inbox/41", "part": 0}),
        )
        .await;
        materialised(&sandbox, &answer, "vertrag.pdf")
        // `opener` is dropped here: the connection that asked for the handle
        // is gone, and a viewer somewhere still has the file open.
    };

    // Give the daemon time to notice the disconnect, so this is not a race the
    // test wins by being fast.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        path.exists(),
        "a disconnect is not a release: {} was taken away",
        path.display()
    );

    let mut other = connected(&sandbox).await;
    let released = call_ok(
        &mut other,
        "message.release_handle",
        json!({"handle": handle}),
    )
    .await;
    assert_eq!(
        released,
        json!({}),
        "the handle was still live on another connection"
    );
    assert!(!path.exists());
}

#[tokio::test]
async fn an_expired_handle_is_no_longer_releasable() {
    let sandbox = Sandbox::with_account_at("alpha", SHORT_TTL_MS);
    seed_one_message(&sandbox, "alpha");
    let _daemon = sandbox.start_daemon().await;
    let mut conn = connected(&sandbox).await;

    let answer = call_ok(
        &mut conn,
        "message.materialise_attachment",
        json!({"account": "alpha", "id": "inbox/41", "part": 0}),
    )
    .await;
    let handle = answer["handle"].as_str().expect("a handle").to_string();

    tokio::time::sleep(AFTER_SHORT_TTL).await;

    let error = call_err(
        &mut conn,
        "message.release_handle",
        json!({"handle": handle}),
    )
    .await;
    assert_eq!(
        error.code, INVALID_PARAMS,
        "an expired handle is gone, not a live one: {}",
        error.message
    );

    // The daemon is still serving: expiry reclaims a handle, not the family.
    let again = call_ok(
        &mut conn,
        "message.materialise_attachment",
        json!({"account": "alpha", "id": "inbox/41", "part": 0}),
    )
    .await;
    assert!(again["handle"].is_string());
}

// ---------------------------------------------------------------------------
// Layer (c) - what the methods refuse
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_unknown_account_is_account_unknown() {
    let sandbox = Sandbox::with_account("alpha");
    seed_one_message(&sandbox, "alpha");
    let _daemon = sandbox.start_daemon().await;
    let mut conn = connected(&sandbox).await;

    let error = call_err(
        &mut conn,
        "message.materialise_attachment",
        json!({"account": "beta", "id": "inbox/41", "part": 0}),
    )
    .await;
    assert_eq!(error.code, ACCOUNT_UNKNOWN, "{}", error.message);
    assert_eq!(
        error.data.as_ref().and_then(|data| data.get("account")),
        Some(&json!("beta")),
        "the payload names the account, as the code's shape says"
    );

    let html = call_err(
        &mut conn,
        "message.materialise_html",
        json!({"account": "beta", "id": "inbox/41"}),
    )
    .await;
    assert_eq!(html.code, ACCOUNT_UNKNOWN, "{}", html.message);
}

#[tokio::test]
async fn a_message_that_is_not_there_is_invalid_params() {
    let sandbox = Sandbox::with_account("alpha");
    seed_one_message(&sandbox, "alpha");
    let _daemon = sandbox.start_daemon().await;
    let mut conn = connected(&sandbox).await;

    for id in ["inbox/99", "archive/41", "41", "inbox/not-a-number", ""] {
        let error = call_err(
            &mut conn,
            "message.materialise_html",
            json!({"account": "alpha", "id": id}),
        )
        .await;
        assert_eq!(
            error.code, INVALID_PARAMS,
            "id {id:?} is not a message of this account: {}",
            error.message
        );
    }
}

#[tokio::test]
async fn a_part_that_is_not_there_is_invalid_params() {
    let sandbox = Sandbox::with_account("alpha");
    seed_one_message(&sandbox, "alpha");
    let _daemon = sandbox.start_daemon().await;
    let mut conn = connected(&sandbox).await;

    for part in [json!(1), json!(7), json!(-1), json!("0"), json!(null)] {
        let error = call_err(
            &mut conn,
            "message.materialise_attachment",
            json!({"account": "alpha", "id": "inbox/41", "part": part}),
        )
        .await;
        assert_eq!(
            error.code, INVALID_PARAMS,
            "part {part} is not an attachment of this message: {}",
            error.message
        );
    }

    let missing = call_err(
        &mut conn,
        "message.materialise_attachment",
        json!({"account": "alpha", "id": "inbox/41"}),
    )
    .await;
    assert_eq!(
        missing.code, INVALID_PARAMS,
        "an absent part names the parameter: {}",
        missing.message
    );
}

#[tokio::test]
async fn a_message_with_no_markup_has_no_rendition_to_materialise() {
    let sandbox = Sandbox::with_account("alpha");
    let fixture = sandbox.seed("alpha");
    let row = fixture.message("alpha", "inbox", 41, 1_700_000_000);
    fixture.blob(row, "body", 0, None, b"Plain text only.\n");
    drop(fixture);
    let _daemon = sandbox.start_daemon().await;
    let mut conn = connected(&sandbox).await;

    let error = call_err(
        &mut conn,
        "message.materialise_html",
        json!({"account": "alpha", "id": "inbox/41"}),
    )
    .await;
    assert_eq!(
        error.code, INVALID_PARAMS,
        "a sender who wrote no markup is not a daemon failure: {}",
        error.message
    );
}
