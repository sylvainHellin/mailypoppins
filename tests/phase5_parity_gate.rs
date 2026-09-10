//! The Phase 5 parity-gate oracle suite (P5-U9, ticket #0124).
//!
//! The plan's Phase 5 gate line is *"the complete TUI parity gate passes,
//! oracles included"*, and section 3.7 names five oracles. Four of them are
//! here; the fifth, the undo-send-hold criterion, is
//! `tests/phase5_undo_send_hold.rs`, because it needs a fake transport and a
//! live session rather than a byte comparison.
//!
//! | oracle | rows in this file |
//! |---|---|
//! | (a) the eight integration suites, rerun through a live daemon and byte-diffed against `pre-daemon` | [`the_eight_legacy_suites_answer_through_a_live_daemon`], [`the_roll_up_names_every_suite_the_phase_four_gate_named`] |
//! | (b) the daemon-backed golden frames | [`the_daemon_golden_frames_module_carries_at_least_twenty_two_tests`], [`every_store_backed_golden_frame_has_a_daemon_twin_pinned_to_its_snapshot`], [`the_daemon_only_snapshots_are_the_two_scenes_that_have_no_hand_built_pair`] |
//! | (c) `mp --help` recursive and `mp dump-keys --json`, from a binary of this run | [`the_help_walk_of_this_runs_binary_is_the_phase_zero_capture`], [`dump_keys_json_of_this_runs_binary_is_the_phase_zero_capture`] |
//! | (d) the `KeyAction::Manual` checklist | [`every_manual_key_has_a_row_in_the_pre_daemon_checklist`], [`the_phase_five_manual_checklist_is_complete_and_carries_no_failure`] |
//!
//! # (a) Why a roll-up rather than eight new comparisons
//!
//! Phase 4 already compares these surfaces command by command: the six slice
//! suites (`tests/daemon_{read,draft,mutation,sync,send,admin}_slice.rs`) each
//! run their commands through `DaemonFixture::mp_routed` - which sets
//! `MAILYPOPPINS_DAEMON_REQUIRE=1`, so a command answered in the client's own
//! process fails rather than passes - and byte-diff them against the
//! `pre-daemon` binary over the same seeded root. Duplicating that here would
//! be a second definition of parity that can drift from the first.
//!
//! What Phase 4 never wrote down in one place is the *list*: which eight
//! legacy suites the migration had to keep honest.
//! `docs/baselines/phase4-gate-evidence.md` names them in prose. This file
//! turns that prose into a table with one routed, oracle-compared row per
//! suite, so a suite that quietly stops being daemon-backed fails a test
//! instead of a paragraph.
//!
//! The roll-up needs the `pre-daemon` oracle and **fails** when there is
//! none: a gate row that returns green because it found nothing to compare
//! against is worse than no row at all, since the summary line then says the
//! parity gate passed. It does not build one either (a 30-minute build inside
//! a gate run is indistinguishable from a hang), so the failure message names
//! `MP_ORACLE_BIN`, the cache path and `docs/baselines/pre-daemon/README.md`.
//! `MP_PARITY_ALLOW_NO_ORACLE=1` turns that failure back into a skip, for the
//! machine that genuinely cannot host an oracle; a gate run does not set it.
//!
//! Seven rows are byte-parity rows. The eighth, `engine_lock_ingest_cli`,
//! cannot be one and says so in its own row: the property that suite pins is
//! *"another process holds the engine lock, so `mp sync` skips"*, and since
//! P5-U8 the other process is the daemon itself. The pre-daemon oracle
//! therefore prints the skip line where the routed client does the sync, which
//! is the intended difference rather than a regression. Its row asserts both
//! halves: the client routes, and the oracle sees the lock held.
//!
//! # (b) Why a source and snapshot comparison
//!
//! `src/tui/ui/golden_frames_daemon.rs` lives inside the library, so its tests
//! are not reachable from here as tests. What is reachable is what they leave
//! on disk: the module source and `src/tui/ui/snapshots/`. The oracle the
//! module chose (P5-U1) is stronger than a second snapshot family - each
//! daemon-built frame is asserted byte-identical to the hand-built frame of the
//! same fixture, through `same_frame`, so it inherits the reviewed snapshot
//! instead of minting one to be approved on trust. The rows below pin exactly
//! that: every store-backed scene has a `…_daemon` twin, every twin is pinned
//! either by `same_frame` or by a `…_daemon.snap` whose body equals the
//! store-backed one, and the only `…_daemon.snap` files are the two scenes that
//! have no hand-built counterpart.
//!
//! # (c) Why the binary of this run
//!
//! The gate says *"byte-identical to the Phase 0 capture from a binary
//! reinstalled in the same run"*. `env!("CARGO_BIN_EXE_mp")` is that binary:
//! cargo builds it for this test target, from this working tree, before the
//! test runs. An installed `mp` on `PATH` is whatever the developer last
//! installed and would let a stale binary pass the gate.
//!
//! # (d) Where the Manual keys come from
//!
//! `KeyAction::Manual` rows are catalogued but hand-dispatched: `mp dump-keys`
//! prints them, the resolver never returns them, and no automated test can
//! press them, which is why the plan asks for a *checklist* rather than a test.
//! The set is taken from `KEYMAP` itself (`mailypoppins::tui::app::KEYMAP`,
//! public since long before the daemon) and cross-checked against the
//! `dump-keys --json` of this run's binary, so neither source can drift alone.
//!
//! The `docs/baselines/phase5-manual-keys.md` row **fails as committed**. That
//! file is P5-U11's to write, when someone has actually pressed the twenty keys
//! against a daemon-backed TUI; until then this oracle has no evidence and must
//! say so. The failure message states the exact format expected.

mod support;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use mailypoppins::tui::app::{KeyAction, KEYMAP};

use support::admin_fixture;
use support::parity::{
    assert_byte_identical, mp_command, oracle, oracle_cache_dir, sandbox_env, DaemonFixture,
    ORACLE_BIN_ENV, ORACLE_TAG,
};
use support::send_fixture;

/// The repository root, which is where the baselines and the scripts are.
fn repo() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// The binary cargo built for this test target: the "reinstalled in the same
/// run" the gate line asks for.
const MP: &str = env!("CARGO_BIN_EXE_mp");

// ---------------------------------------------------------------------------
// (a) The eight integration suites, through a live daemon
// ---------------------------------------------------------------------------

/// One legacy suite, and the command that stands for it.
///
/// Every row is a parity row: routed under `MAILYPOPPINS_DAEMON_REQUIRE=1`, so
/// a command answered in the client's own process fails instead of passing,
/// and byte-identical to the `pre-daemon` oracle over the same seeded root.
struct Suite {
    /// The file under `tests/`, without the extension.
    name: &'static str,
    /// The command, as `mp` takes it.
    args: Vec<String>,
}

fn parity(name: &'static str, args: &[&str]) -> Suite {
    Suite {
        name,
        args: args.iter().map(|a| (*a).to_string()).collect(),
    }
}

/// The eight suites `docs/baselines/phase4-gate-evidence.md` names under
/// "Existing CLI tests and help snapshots pass", in that order.
///
/// The command chosen for each is the read-only surface that suite is about,
/// so the roll-up can run all eight against one seeded root in one daemon
/// lifetime without any of them changing what the next one sees.
fn suites() -> Vec<Suite> {
    let draft = send_fixture::selector(send_fixture::ACCOUNT, send_fixture::VALID);
    vec![
        parity(
            "cli_read_surface_integration",
            &["list-messages", "-A", "alpha", "--mailbox", "inbox"],
        ),
        parity("cli_selector_contract", &["path", &draft]),
        parity("draft_integration", &["list"]),
        parity(
            "dump_mailbox_integration",
            &["-A", "alpha", "dump-mailbox", "--json"],
        ),
        parity(
            "store_search_integration",
            &["search", "--local", "ledger", "-n", "10"],
        ),
        parity("outbox_integration", &["outbox", "list"]),
        parity("imip_integration", &["calendar", "rebuild", "-A", "alpha"]),
        parity("engine_lock_ingest_cli", &["sync", "-A", "alpha"]),
    ]
}

/// The escape hatch for a machine that cannot host an oracle: set it to `1`
/// and the roll-up skips instead of failing.
const ALLOW_NO_ORACLE_ENV: &str = "MP_PARITY_ALLOW_NO_ORACLE";

/// The oracle binary, if one is already available, without building one.
///
/// `support::parity::oracle_bin` builds the `pre-daemon` tag when the cache is
/// cold, which is right for a slice suite that cannot mean anything without an
/// oracle and wrong for a gate roll-up: a 30-minute build inside a gate run is
/// indistinguishable from a hang. So the caller fails with a message that says
/// how to get one, unless [`ALLOW_NO_ORACLE_ENV`] says to skip.
fn oracle_if_present() -> Option<PathBuf> {
    if let Some(from_env) = std::env::var_os(ORACLE_BIN_ENV) {
        let path = PathBuf::from(from_env);
        if path.is_file() {
            return Some(path);
        }
    }
    let cached = oracle_cache_dir().join(ORACLE_TAG).join("mp");
    cached.is_file().then_some(cached)
}

#[test]
fn the_roll_up_names_every_suite_the_phase_four_gate_named() {
    let named: Vec<&str> = suites().iter().map(|s| s.name).collect();
    assert_eq!(
        named,
        [
            "cli_read_surface_integration",
            "cli_selector_contract",
            "draft_integration",
            "dump_mailbox_integration",
            "store_search_integration",
            "outbox_integration",
            "imip_integration",
            "engine_lock_ingest_cli",
        ],
        "the eight suites are the ones `docs/baselines/phase4-gate-evidence.md` lists; a suite \
         added or removed here is a change to what the parity gate covers and belongs in the \
         ticket, not in this list"
    );
    for suite in suites() {
        assert!(
            repo()
                .join("tests")
                .join(format!("{}.rs", suite.name))
                .is_file(),
            "the roll-up names `tests/{}.rs`, which is not in the tree",
            suite.name
        );
    }
}

#[test]
fn the_eight_legacy_suites_answer_through_a_live_daemon() {
    let Some(bin) = oracle_if_present() else {
        let complaint = format!(
            "no `{ORACLE_TAG}` oracle at {} and {ORACLE_BIN_ENV} is unset, so the eight-suite \
             roll-up has nothing to compare against. Build one with the procedure in \
             docs/baselines/pre-daemon/README.md, or run any Phase 4 slice suite once, which \
             builds it into the cache. Set {ALLOW_NO_ORACLE_ENV}=1 to skip this row instead, \
             which a gate run may not do.",
            oracle_cache_dir().join(ORACLE_TAG).join("mp").display()
        );
        if std::env::var(ALLOW_NO_ORACLE_ENV).as_deref() == Ok("1") {
            eprintln!("skipping the eight-suite roll-up: {complaint}");
            return;
        }
        panic!("{complaint}");
    };
    assert!(bin.is_file(), "the oracle resolved to {}", bin.display());

    let tmp = tempfile::tempdir().expect("a temporary parity root");
    admin_fixture::seed(tmp.path());
    let daemon = DaemonFixture::start(tmp.path());

    let mut failures = Vec::new();
    for suite in suites() {
        let args: Vec<&str> = suite.args.iter().map(String::as_str).collect();
        let routed = daemon.mp_routed(&args);
        let complaint = String::from_utf8_lossy(&routed.stderr).to_string();
        if complaint.contains("MAILYPOPPINS_DAEMON_REQUIRE") {
            failures.push(format!(
                "{}: `mp {}` answered in the client's own process\n{complaint}",
                suite.name,
                args.join(" ")
            ));
            continue;
        }
        let theirs = oracle(&args, tmp.path());
        let compared = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            assert_byte_identical(&routed, &theirs)
        }));
        if let Err(payload) = compared {
            failures.push(format!(
                "{}: `mp {}` is not byte-identical to the `{ORACLE_TAG}` oracle\n{}",
                suite.name,
                args.join(" "),
                panic_message(payload)
            ));
        }
    }
    daemon.stop();

    assert!(
        failures.is_empty(),
        "{} of the eight legacy suites do not hold through a live daemon:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

/// The engine `tests/engine_lock_ingest_cli.rs` calls "another process" is the
/// daemon, and the client that syncs takes no lock of its own.
///
/// The parity row above cannot say this: every account of the roll-up fixture
/// is local-only, so both binaries skip the sync before any lock is reached,
/// which is why the row is byte-identical and why it is not the whole story.
/// The suite's premise - *some other process holds the lock, so `mp sync`
/// leaves the ingest to it* - is now a statement about ownership, and since
/// P5-U8 turned account runtimes on by default the owner is the daemon.
#[test]
fn the_engine_the_legacy_lock_suite_assumes_is_the_daemon() {
    use mailypoppins::engine_lock::EngineLock;
    use std::time::{Duration, Instant};

    let tmp = tempfile::tempdir().expect("a temporary lock root");
    let root = tmp.path();
    support::sync_fixture::seed(root);
    let account = support::sync_fixture::SERVER_ACCOUNT;
    let lock = support::sync_fixture::engine_lock_path(root, account);

    let free = |path: &Path| match EngineLock::try_acquire_at(path, account) {
        Ok(Some(held)) => {
            drop(held);
            true
        }
        Ok(None) => false,
        Err(e) => panic!("the engine lock {} is not readable: {e}", path.display()),
    };
    assert!(
        free(&lock),
        "nothing holds {} before a daemon is started",
        lock.display()
    );

    let daemon = DaemonFixture::start(root);
    let deadline = Instant::now() + Duration::from_secs(20);
    while free(&lock) {
        assert!(
            Instant::now() < deadline,
            "the daemon never took {}: an account runtime that holds no engine lock is a daemon \
             claiming to be an engine it is not, and `mp sync` would then have nobody to leave the \
             ingest to",
            lock.display()
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    daemon.stop();

    let deadline = Instant::now() + Duration::from_secs(20);
    while !free(&lock) {
        assert!(
            Instant::now() < deadline,
            "and it released {} when it stopped",
            lock.display()
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        return (*text).to_string();
    }
    if let Some(text) = payload.downcast_ref::<String>() {
        return text.clone();
    }
    "<a panic payload that is neither &str nor String>".to_string()
}

// ---------------------------------------------------------------------------
// (b) The daemon-backed golden frames
// ---------------------------------------------------------------------------

/// The library module P5-U1 wrote and P5-U2 made pass.
fn daemon_frames_source() -> String {
    let path = repo().join("src/tui/ui/golden_frames_daemon.rs");
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn store_frames_source() -> String {
    let path = repo().join("src/tui/ui/golden_frames.rs");
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Every `#[test] fn name` in a module source, in file order.
fn test_names(source: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut armed = false;
    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("#[test]") {
            armed = true;
            continue;
        }
        if !armed {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("fn ") {
            if let Some(name) = rest.split('(').next() {
                names.push(name.to_string());
                armed = false;
            }
        }
    }
    names
}

/// The scenes `src/tui/ui/snapshots/` holds for a module, by test name.
fn snapshot_files(module: &str) -> BTreeMap<String, PathBuf> {
    let dir = repo().join("src/tui/ui/snapshots");
    let prefix = format!("mailypoppins__tui__ui__{module}__");
    let mut found = BTreeMap::new();
    for entry in fs::read_dir(&dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display())) {
        let entry = entry.expect("a snapshot directory entry");
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(rest) = name.strip_prefix(&prefix) else {
            continue;
        };
        let Some(scene) = rest.strip_suffix(".snap") else {
            continue;
        };
        found.insert(scene.to_string(), entry.path());
    }
    found
}

/// A snapshot's frame, without the insta header (`---` … `---`), which carries
/// the source path and therefore differs between two modules by construction.
fn snapshot_body(path: &Path) -> String {
    let text = fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    match text.split_once("---\n") {
        Some((_, rest)) => match rest.split_once("---\n") {
            Some((_, body)) => body.to_string(),
            None => rest.to_string(),
        },
        None => text,
    }
}

/// The plan's count: the 20 hand-built frames each need a daemon variant, and
/// the module adds the two scenes a bootstrap can reach and a hand-built `App`
/// cannot.
const DAEMON_FRAME_TESTS: usize = 22;

#[test]
fn the_daemon_golden_frames_module_carries_at_least_twenty_two_tests() {
    let names = test_names(&daemon_frames_source());
    assert!(
        names.len() >= DAEMON_FRAME_TESTS,
        "`src/tui/ui/golden_frames_daemon.rs` carries {} tests, and the P5-U1 contract is at least \
         {DAEMON_FRAME_TESTS}: a daemon variant of each hand-built frame, plus the `opening` \
         account and the extra-mailbox bootstrap. Found: {names:?}",
        names.len()
    );
    let ui_mod =
        fs::read_to_string(repo().join("src/tui/ui/mod.rs")).expect("read src/tui/ui/mod.rs");
    assert!(
        ui_mod.contains("golden_frames_daemon"),
        "`src/tui/ui/mod.rs` no longer declares the daemon golden-frame module, so none of its \
         tests run at all"
    );
    assert!(
        store_frames_source().contains("fn frame_snapshot"),
        "the daemon module reuses `golden_frames::frame_snapshot`, which is what keeps the two \
         families from drifting; it is no longer there"
    );
}

#[test]
fn every_store_backed_golden_frame_has_a_daemon_twin_pinned_to_its_snapshot() {
    let store_scenes = snapshot_files("golden_frames");
    assert!(
        store_scenes.len() >= 18,
        "the store-backed family is 18 reviewed snapshots or more, found {}",
        store_scenes.len()
    );

    let daemon_source = daemon_frames_source();
    let daemon_tests: BTreeSet<String> = test_names(&daemon_source).into_iter().collect();
    let daemon_scenes = snapshot_files("golden_frames_daemon");

    let mut pairs = Vec::new();
    let mut failures = Vec::new();
    for (scene, store_path) in &store_scenes {
        let twin = format!("{scene}_daemon");
        if !daemon_tests.contains(&twin) {
            failures.push(format!(
                "{scene}: no `{twin}` in src/tui/ui/golden_frames_daemon.rs"
            ));
            continue;
        }
        match daemon_scenes.get(&twin) {
            // The twin minted a snapshot of its own: it must be the same frame,
            // or the two families have drifted and one of them is wrong.
            Some(daemon_path) => {
                if snapshot_body(store_path) == snapshot_body(daemon_path) {
                    pairs.push(format!("{scene} = {twin} (snapshot bodies equal)"));
                } else {
                    failures.push(format!(
                        "{scene}: {} and {} are different frames",
                        store_path.display(),
                        daemon_path.display()
                    ));
                }
            }
            // No snapshot of its own, which is the stronger form: the twin
            // asserts byte equality against the hand-built frame through
            // `same_frame`, so it is pinned by the reviewed snapshot itself.
            None => {
                if body_of(&daemon_source, &twin).contains("same_frame(") {
                    pairs.push(format!(
                        "{scene} = {twin} (same_frame against the hand-built app)"
                    ));
                } else {
                    failures.push(format!(
                        "{scene}: `{twin}` neither carries a `…_daemon` snapshot nor calls \
                         `same_frame`, so nothing pins the daemon-built frame to the reviewed one"
                    ));
                }
            }
        }
    }

    assert!(
        failures.is_empty(),
        "{} of {} shared scenes are not pinned:\n{}\n\npinned pairs:\n{}",
        failures.len(),
        store_scenes.len(),
        failures.join("\n"),
        pairs.join("\n")
    );
    assert_eq!(
        pairs.len(),
        store_scenes.len(),
        "every shared scene is named as a pair:\n{}",
        pairs.join("\n")
    );
}

/// The body of `fn name`, from its signature to the next top-level `}`.
fn body_of(source: &str, name: &str) -> String {
    let needle = format!("fn {name}(");
    let Some(start) = source.find(&needle) else {
        return String::new();
    };
    let rest = &source[start..];
    match rest.find("\n}\n") {
        Some(end) => rest[..end].to_string(),
        None => rest.to_string(),
    }
}

#[test]
fn the_daemon_only_snapshots_are_the_two_scenes_that_have_no_hand_built_pair() {
    let daemon_scenes: BTreeSet<String> =
        snapshot_files("golden_frames_daemon").into_keys().collect();
    let expected: BTreeSet<String> = [
        "golden_opening_account_daemon",
        "golden_extra_mailbox_daemon",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    assert_eq!(
        daemon_scenes, expected,
        "the only `…_daemon` snapshot files are the two scenes a bootstrap can reach and a \
         hand-built `App` cannot (an `opening` account, and one mailbox row more). A third one \
         means a shared scene minted a second snapshot instead of being compared against the \
         reviewed one, which is a frame approved on trust."
    );
}

// ---------------------------------------------------------------------------
// (c) The help walk and the key dump, from this run's binary
// ---------------------------------------------------------------------------

/// A sandbox for a `mp` invocation that must reach nothing: an empty root, no
/// daemon started, every daemon hook cleared.
fn quiet_sandbox(root: &Path) -> Command {
    let mut cmd = mp_command(root);
    cmd.env("MAILYPOPPINS_DAEMON_AUTOSTART", "0");
    cmd
}

#[test]
fn the_help_walk_of_this_runs_binary_is_the_phase_zero_capture() {
    let tmp = tempfile::tempdir().expect("a temporary help root");
    let script = repo().join("scripts/capture-cli-help.sh");
    assert!(script.is_file(), "{} is missing", script.display());

    let mut cmd = Command::new(&script);
    sandbox_env(&mut cmd, tmp.path());
    let out = cmd
        .env("MP", MP)
        .env("MAILYPOPPINS_DAEMON_AUTOSTART", "0")
        .current_dir(repo())
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|e| panic!("run {}: {e}", script.display()));
    assert!(
        out.status.success(),
        "the help walk failed with {}:\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );

    let baseline = repo().join("docs/baselines/pre-daemon/cli-help.txt");
    let expected =
        fs::read(&baseline).unwrap_or_else(|e| panic!("read {}: {e}", baseline.display()));
    assert_bytes_equal(&out.stdout, &expected, &baseline);
}

#[test]
fn dump_keys_json_of_this_runs_binary_is_the_phase_zero_capture() {
    let tmp = tempfile::tempdir().expect("a temporary keys root");
    let out = quiet_sandbox(tmp.path())
        .args(["dump-keys", "--json"])
        .output()
        .expect("run `mp dump-keys --json`");
    assert!(
        out.status.success(),
        "`mp dump-keys --json` failed with {}:\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );

    let baseline = repo().join("docs/baselines/pre-daemon/tui-keys.json");
    let expected =
        fs::read(&baseline).unwrap_or_else(|e| panic!("read {}: {e}", baseline.display()));
    assert_bytes_equal(&out.stdout, &expected, &baseline);
}

/// Byte comparison with a failure a reader can act on: the first differing
/// line, with three lines of context, rather than two walls of text.
fn assert_bytes_equal(got: &[u8], want: &[u8], baseline: &Path) {
    if got == want {
        return;
    }
    let (got, want) = (String::from_utf8_lossy(got), String::from_utf8_lossy(want));
    let got_lines: Vec<&str> = got.lines().collect();
    let want_lines: Vec<&str> = want.lines().collect();
    let first = (0..got_lines.len().max(want_lines.len()))
        .find(|&i| got_lines.get(i) != want_lines.get(i))
        .unwrap_or(0);
    let from = first.saturating_sub(3);
    let to = (first + 12).min(got_lines.len().max(want_lines.len()));
    let mut excerpt = vec![format!("@@ line {} @@", first + 1)];
    for i in from..to {
        if let Some(line) = got_lines.get(i) {
            excerpt.push(format!("-{line}"));
        }
        if let Some(line) = want_lines.get(i) {
            excerpt.push(format!("+{line}"));
        }
    }
    panic!(
        "the binary of this run does not reproduce {}\n(-this run, +baseline)\n{}",
        baseline.display(),
        excerpt.join("\n")
    );
}

// ---------------------------------------------------------------------------
// (d) The KeyAction::Manual checklist
// ---------------------------------------------------------------------------

/// One catalogued hand-dispatched key: the section `mp dump-keys` files it
/// under, and the spelling it prints.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ManualKey {
    section: &'static str,
    keys: &'static str,
}

/// Every `KeyAction::Manual` row that reaches the help overlay, the website and
/// `mp dump-keys`.
///
/// Rows with an empty `keys` are the dispatch-only leader rows (`Space`, the
/// `g` prefixes), which `help_sections` skips and which no one can press on
/// their own.
fn manual_keys() -> Vec<ManualKey> {
    KEYMAP
        .iter()
        .filter(|kb| kb.action == KeyAction::Manual && !kb.keys.is_empty())
        .map(|kb| ManualKey {
            section: kb.ctx.group_title(),
            keys: kb.keys,
        })
        .collect()
}

/// The `manual-keys.md` headings that describe a surface, by dump-keys section.
///
/// A key is looked for inside its own surface rather than anywhere in the file:
/// `Esc` appears in a dozen surfaces, so a whole-file search would pass for a
/// key nobody documented.
fn surface_headings(section: &str) -> &'static [&'static str] {
    match section {
        "SERVER SEARCH" => &["Server search overlay"],
        "ACTIVITY LOG" => &["Activity overlay"],
        other => panic!(
            "no `docs/baselines/pre-daemon/manual-keys.md` surface is mapped to the dump-keys \
             section {other:?}; a new KeyAction::Manual family needs a mapping here and a section \
             there"
        ),
    }
}

/// The atoms of a display spelling: `"gg / G"` is two keys, `"j/k"` is two,
/// `"/"` is one.
fn key_atoms(keys: &str) -> Vec<String> {
    if keys.trim() == "/" {
        return vec!["/".to_string()];
    }
    keys.split('/')
        .map(str::trim)
        .filter(|atom| !atom.is_empty())
        .map(str::to_string)
        .collect()
}

/// The sections of a Markdown file, keyed by `## ` heading.
fn markdown_sections(text: &str) -> Vec<(String, String)> {
    let mut sections: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        if let Some(title) = line.strip_prefix("## ") {
            sections.push((title.trim().to_string(), String::new()));
        } else if let Some(last) = sections.last_mut() {
            last.1.push_str(line);
            last.1.push('\n');
        }
    }
    sections
}

#[test]
fn the_manual_rows_are_exactly_what_this_runs_binary_dumps() {
    let derived: BTreeSet<(String, String)> = manual_keys()
        .into_iter()
        .map(|k| (k.section.to_string(), k.keys.to_string()))
        .collect();

    let tmp = tempfile::tempdir().expect("a temporary keys root");
    let out = quiet_sandbox(tmp.path())
        .args(["dump-keys", "--json"])
        .output()
        .expect("run `mp dump-keys --json`");
    let dumped: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("`mp dump-keys --json` is JSON");

    let mut printed = BTreeSet::new();
    for section in dumped.as_array().expect("dump-keys prints an array") {
        let title = section["title"]
            .as_str()
            .expect("a section title")
            .to_string();
        if !matches!(title.as_str(), "SERVER SEARCH" | "ACTIVITY LOG") {
            continue;
        }
        for binding in section["bindings"].as_array().expect("a bindings array") {
            printed.insert((
                title.clone(),
                binding["key"].as_str().expect("a key").to_string(),
            ));
        }
    }

    assert_eq!(
        derived, printed,
        "the `KeyAction::Manual` rows of `KEYMAP` and the two hand-dispatched sections of \
         `mp dump-keys --json` must be the same set: the checklist below is built from the first \
         and the gate is stated over the second"
    );
    assert_eq!(
        derived.len(),
        20,
        "Phase 0 counted 20 documented-only Manual rows (15 server search, 5 activity log); a new \
         one needs a `docs/baselines/pre-daemon/manual-keys.md` row and a checklist row"
    );
}

#[test]
fn every_manual_key_has_a_row_in_the_pre_daemon_checklist() {
    let path = repo().join("docs/baselines/pre-daemon/manual-keys.md");
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let sections = markdown_sections(&text);

    let mut missing = Vec::new();
    for key in manual_keys() {
        let headings = surface_headings(key.section);
        let surface: String = sections
            .iter()
            .filter(|(title, _)| headings.iter().any(|h| title.starts_with(h)))
            .map(|(_, body)| body.as_str())
            .collect();
        assert!(
            !surface.is_empty(),
            "`{}` has no section for {:?}",
            path.display(),
            headings
        );
        for atom in key_atoms(key.keys) {
            if !surface.contains(&format!("`{atom}`")) {
                missing.push(format!(
                    "{} `{}` (from the {} spelling {:?})",
                    key.section, atom, key.section, key.keys
                ));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "{} hand-dispatched key(s) reach `mp dump-keys` and no row of {} describes them:\n{}",
        missing.len(),
        path.display(),
        missing.join("\n")
    );
}

/// The companion file P5-U11 writes once the twenty keys have been pressed
/// against a daemon-backed TUI.
const PHASE5_CHECKLIST: &str = "docs/baselines/phase5-manual-keys.md";

/// The only two statuses a row may carry.
const PASS: &str = "pass";
const NOT_TAKEN: &str = "NOT TAKEN (owner-only)";

#[test]
fn the_phase_five_manual_checklist_is_complete_and_carries_no_failure() {
    let path = repo().join(PHASE5_CHECKLIST);
    let expected = manual_keys();
    let format = format!(
        "Expected format: one Markdown table row per key, in `mp dump-keys --json` order,\n\
         \n\
         | surface | key | status | evidence |\n\
         |---|---|---|---|\n\
         | SERVER SEARCH | j/k | {PASS} | … |\n\
         \n\
         with `status` exactly `{PASS}` or `{NOT_TAKEN}`, and the {} (surface, key) pairs of \
         `mp dump-keys --json` each present exactly once. `fail` is not a status a gate document \
         may carry: a failed key is a bug to fix before the gate closes.",
        expected.len()
    );

    let Ok(text) = fs::read_to_string(&path) else {
        panic!(
            "{PHASE5_CHECKLIST} does not exist. It is P5-U11's to write, from a real run of the \
             daemon-backed TUI against `docs/baselines/pre-daemon/manual-keys.md`: no automated \
             test can press a hand-dispatched key, which is why the plan asks for a checklist.\n\n\
             {format}"
        )
    };

    let mut rows: BTreeMap<(String, String), String> = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if !line.starts_with('|') {
            continue;
        }
        let cells: Vec<String> = line
            .trim_matches('|')
            .split('|')
            .map(|cell| cell.trim().trim_matches('`').trim().to_string())
            .collect();
        if cells.len() < 3 || cells[0].starts_with("---") || cells[0] == "surface" {
            continue;
        }
        rows.insert((cells[0].clone(), cells[1].clone()), cells[2].clone());
    }

    let mut problems = Vec::new();
    for key in &expected {
        match rows.get(&(key.section.to_string(), key.keys.to_string())) {
            None => problems.push(format!("{} `{}`: no row", key.section, key.keys)),
            Some(status) if status == PASS || status == NOT_TAKEN => {}
            Some(status) => problems.push(format!(
                "{} `{}`: status {status:?}, which is neither {PASS:?} nor {NOT_TAKEN:?}",
                key.section, key.keys
            )),
        }
    }
    assert!(
        !text.to_lowercase().contains("| fail"),
        "{PHASE5_CHECKLIST} carries a failing row, so the Phase 5 parity gate does not close"
    );
    assert!(
        problems.is_empty(),
        "{} of the {} hand-dispatched keys are not answered by {PHASE5_CHECKLIST}:\n{}\n\n{format}",
        problems.len(),
        expected.len(),
        problems.join("\n")
    );
}
