//! The test-selection guard: the workspace conversion must not deselect a
//! single test (#0120, unit P2-U1a).
//!
//! `cargo test` on a single-package tree runs everything under `src/`. The
//! moment the root `Cargo.toml` grows a `[workspace]` table, or a module moves
//! into `crates/`, the same command can silently stop compiling a whole file of
//! `#[cfg(test)]` modules, and nothing fails: the summary line just gets
//! smaller, and nobody reads it. This file is landed *before* the workspace
//! exists so the numbers below are a true "before".
//!
//! Counting is by **source scan**, not by what the harness selected: the guard
//! reads the `.rs` files under `src/tui/` and counts `#[test]` /
//! `#[tokio::test]` attributes, so it reports the same numbers whether or not
//! those tests are part of the current selection. A guard that counted
//! *executed* tests would disappear along with the tests it is meant to
//! defend.
//!
//! The three floors started as the counts on the pre-workspace tree and are
//! raised to the actual counts as the tree grows: a floor left far below what
//! the tree carries stops defending anything, because a whole file of tests
//! can vanish without crossing it. They are floors, not equalities: adding
//! tests must never fail CI. To lower one deliberately (a module genuinely
//! deleted), edit the constant in the same commit that deletes the tests, and
//! say so in the commit message.

use std::fs;
use std::path::{Path, PathBuf};

/// Every `.rs` file below here is scanned for test attributes.
const TUI_ROOT: &str = "src/tui";
/// The golden-frame suite: the TUI's only end-to-end rendering coverage, and
/// the first thing a bad workspace layout drops.
const GOLDEN_FRAMES: &str = "src/tui/ui/golden_frames.rs";
/// `insta` snapshots backing the golden frames and the widget tests.
const SNAPSHOT_DIR: &str = "src/tui/ui/snapshots";

/// `#[test]` / `#[tokio::test]` attributes under [`TUI_ROOT`].
///
/// The plan's floor was 367 and the pre-workspace tree carried 368; the Phase
/// 5 tree carries 464, which is the count CI defends.
const MIN_TUI_TESTS: usize = 464;
/// `#[test]` functions in [`GOLDEN_FRAMES`]. Plan floor and actual both 20.
const MIN_GOLDEN_FRAME_TESTS: usize = 20;
/// `.snap` files in [`SNAPSHOT_DIR`]. Plan floor and pre-workspace actual both
/// 18; the daemon-backed frames of P5-U1 added the two `…_daemon.snap` scenes
/// that have no hand-built pair, so the actual is 20.
const MIN_SNAPSHOT_FILES: usize = 20;

/// The sentence every failure here must contain, so that a CI log search for
/// it finds the guard regardless of which of the three counts moved.
const DROPPED: &str = "the workspace test selection has dropped tests";

#[test]
fn tui_test_count_has_not_dropped() {
    let root = repo_root().join(TUI_ROOT);
    let found = test_attributes_under(&root);
    assert!(
        found >= MIN_TUI_TESTS,
        "{DROPPED}: {TUI_ROOT} declares {found} `#[test]`/`#[tokio::test]` attributes, \
         expected at least {MIN_TUI_TESTS}"
    );
}

#[test]
fn golden_frame_test_count_has_not_dropped() {
    let file = repo_root().join(GOLDEN_FRAMES);
    let source =
        fs::read_to_string(&file).unwrap_or_else(|e| panic!("{DROPPED}: read {file:?}: {e}"));
    let found = test_attributes(&source);
    assert!(
        found >= MIN_GOLDEN_FRAME_TESTS,
        "{DROPPED}: {GOLDEN_FRAMES} declares {found} `#[test]` functions, \
         expected at least {MIN_GOLDEN_FRAME_TESTS}"
    );
}

#[test]
fn snapshot_file_count_has_not_dropped() {
    let dir = repo_root().join(SNAPSHOT_DIR);
    let found = snapshot_files(&dir);
    assert!(
        found >= MIN_SNAPSHOT_FILES,
        "{DROPPED}: {SNAPSHOT_DIR} holds {found} `.snap` files, \
         expected at least {MIN_SNAPSHOT_FILES}"
    );
}

/// Sum of [`test_attributes`] over every `.rs` file below `root`.
fn test_attributes_under(root: &Path) -> usize {
    rust_files(root)
        .iter()
        .map(|file| {
            let source = fs::read_to_string(file)
                .unwrap_or_else(|e| panic!("{DROPPED}: read {file:?}: {e}"));
            test_attributes(&source)
        })
        .sum()
}

/// Count `#[test]` and `#[tokio::test]` attributes in `source`.
///
/// Attribute arguments are ignored, so `#[tokio::test(flavor = "multi_thread")]`
/// counts; sibling attributes that merely mention the word do not
/// (`#[cfg(test)]`, `#[should_panic]`). Line comments are stripped first, so a
/// commented-out test does not inflate the count.
fn test_attributes(source: &str) -> usize {
    let source = strip_line_comments(source);
    let mut count = 0;
    let mut from = 0;
    while let Some(at) = source[from..].find("#[") {
        let start = from + at + 2;
        from = start;
        let Some(end) = closing_bracket(&source, start) else {
            break;
        };
        if is_test_attribute(source[start..end].trim()) {
            count += 1;
        }
    }
    count
}

/// Index of the `]` closing the `#[` whose body starts at `start`, honouring
/// nested brackets.
fn closing_bracket(source: &str, start: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (offset, ch) in source[start..].char_indices() {
        match ch {
            '[' => depth += 1,
            ']' if depth == 0 => return Some(start + offset),
            ']' => depth -= 1,
            _ => {}
        }
    }
    None
}

/// Whether an attribute body (the text between `#[` and `]`) declares a test.
fn is_test_attribute(body: &str) -> bool {
    let path = body.split('(').next().unwrap_or(body).trim();
    path == "test" || path == "tokio::test"
}

/// `.snap` files directly in `dir`. `.snap.new` files, which `cargo insta`
/// leaves behind for unreviewed diffs, have extension `new` and are skipped.
fn snapshot_files(dir: &Path) -> usize {
    let entries = fs::read_dir(dir).unwrap_or_else(|e| panic!("{DROPPED}: read_dir {dir:?}: {e}"));
    entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            let path = entry.path();
            path.is_file() && path.extension().is_some_and(|e| e == "snap")
        })
        .count()
}

/// Every `.rs` file below `root`, recursively.
fn rust_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries =
            fs::read_dir(&dir).unwrap_or_else(|e| panic!("{DROPPED}: read_dir {dir:?}: {e}"));
        for entry in entries {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    out
}

/// Drop `//` comments, so a doc comment quoting an attribute is not counted.
/// Block comments are left alone, matching `tests/architecture_boundaries.rs`:
/// stripping them without a real lexer would mangle a `/*` inside a string
/// literal, and the tree has none.
fn strip_line_comments(source: &str) -> String {
    source
        .lines()
        .map(|line| match line.find("//") {
            Some(at) => &line[..at],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn test_attributes_counts_both_forms_and_ignores_look_alikes() {
    let source = "\
#[test]
fn a() {}

#[tokio::test]
async fn b() {}

#[tokio::test(flavor = \"multi_thread\", worker_threads = 2)]
async fn c() {}

#[test]
#[should_panic]
fn d() {}

#[cfg(test)]
mod tests {
    #[test]
    fn e() {}
    // #[test]
    // fn commented_out() {}
}

#[allow(dead_code)]
fn f() {}
";
    assert_eq!(test_attributes(source), 5);
}

#[test]
fn counters_walk_nested_dirs_and_skip_non_snapshots() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let root = tmp.path();
    fs::create_dir_all(root.join("ui/snapshots")).expect("mkdir");
    fs::write(root.join("top.rs"), "#[test]\nfn a() {}\n").expect("write");
    fs::write(
        root.join("ui/nested.rs"),
        "#[tokio::test]\nasync fn b() {}\n#[test]\nfn c() {}\n",
    )
    .expect("write");
    fs::write(root.join("ui/notrust.txt"), "#[test]\n").expect("write");
    for name in ["one.snap", "two.snap", "three.snap.new", "notes.md"] {
        fs::write(root.join("ui/snapshots").join(name), "x").expect("write");
    }

    assert_eq!(test_attributes_under(root), 3);
    assert_eq!(snapshot_files(&root.join("ui/snapshots")), 2);
}
