//! The client/engine boundary, recorded so it cannot widen (#0118, #0123).
//!
//! Two boundaries, two halves.
//!
//! **The TUI's imports.** The TUI called the library directly: it opened the
//! store, drove the IMAP client, read secrets and queued server ops itself. The
//! daemon migration had to drive that set to zero, and the only way to know it
//! was shrinking was to have written down what it was. The first half walks the
//! TUI's sources, collects every `use` of an engine module, and compares the
//! result against `tests/fixtures/tui-engine-imports.txt`. **The fixture is
//! empty since #0126 (P5-U10e)**, and the scan roots at `crates/mp-tui/src`
//! since P5-U10f, where the answer is structural: that crate's manifest names
//! `mp-core`, `mp-client` and `mp-protocol` and no `mailypoppins`, so an engine
//! import does not compile. The scan stays as the belt to that braces: it fails
//! the day someone adds the dependency back, naming the file and the module,
//! which a resolver error a hundred lines long would not.
//!
//! **The CLI's engine touches.** Phase 4's exit gate reads "no command that
//! touches domain state opens a store, a secret backend, a network backend, or
//! an engine lock". The second half walks the client-side sources - `src/main.rs`,
//! `src/*_cmd.rs`, `src/cutover.rs`, `src/config_cmd/` - for the symbols that
//! do exactly that, and compares the result against [`CLI_ENGINE_RESIDUE`], an
//! inline table whose third column is the reason each survivor is still there.
//! P4-U15 drove it from every migrated command down to what that table names,
//! and the table is a record too: a residue that goes away must be struck from
//! it in the same commit.
//!
//! **The TUI's engine calls.** The import scan above reads `use` statements,
//! and P5-U10c-I2 measured what that misses: most of what stands between
//! `src/tui/` and `crates/mp-tui` is spelled as a fully-qualified path
//! (`crate::outbox::counts_for_account(…)`, `crate::store::read::thread_messages(…)`),
//! which no `use` line mentions, so the import allow-list read six while the
//! work was six *groups* of call sites it could not see. The third half walks
//! the same tree for those paths, over production code only, and compares the
//! result against `tests/fixtures/tui-engine-paths.txt`. It is the same
//! mechanism as the import allow-list and it is a record in the same sense: a
//! row that goes away is struck in the commit that removes the call.
//!
//! Nothing here is feature-gated: it passes on the pre-daemon tree, which is
//! the point. `engine_imports` takes the client source root as an argument,
//! which is what let P5-U10f re-point it at `crates/mp-tui/src` without a
//! rewrite.
//!
//! To re-record the TUI allow-lists after a deliberate change, run
//! `UPDATE_TUI_ENGINE_IMPORTS=1 cargo test --test architecture_boundaries`
//! and commit the diff; the same switch rewrites both fixtures. The CLI residue
//! has no such switch on purpose: an entry is added by hand, with its reason,
//! or it is not added.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// The library modules that make up the engine: everything the TUI had to stop
/// touching directly when it started talking to the daemon instead. Modules
/// that stay shared between client and engine (`types`, `config`, `parse`,
/// `search`, `selector`, ...) are deliberately absent.
///
/// Nine names, not eleven: `secrets` and `oauth2` left for `mp-core` in
/// P5-U10a and were carried on this list unchanged for four units, because no
/// file under `src/tui/` imported either and striking them was never the unit's
/// brief. P5-U10f is the unit where keeping them would have been wrong rather
/// than merely untidy: `crates/mp-tui` depends on `mp-core`, so a `use
/// mp_core::secrets::…` in the TUI would be a real engine reach that this
/// scan - which looks for `crate::` and `mailypoppins::` - could not see, and a
/// list naming two modules the scan cannot reach reads as coverage it does not
/// have. The two are covered instead by [`MP_CORE_ENGINE_PATHS`], which scans
/// for the paths they are actually spelled with now.
pub const ENGINE_MODULES: [&str; 9] = [
    "graph",
    "imap_client",
    "ingest",
    "ops",
    "outbox",
    "pending_ops",
    "send",
    "store",
    "sync",
];

/// The two `mp-core` modules a client crate may link and must not use:
/// the secret backend and the OAuth2 device flow.
///
/// They are the engine's half of the shared crate (#0126, P5-U10a moved them
/// there), and the daemon is what opens a keyring or runs a device-code flow.
/// The allow-list scan above cannot see them, because they are `mp_core::`
/// paths rather than `crate::` ones, so [`mp_core_engine_reaches`] scans the
/// whole TUI crate for them, tests included.
///
/// A `use` is read as a path and not as text, by the same
/// [`brace_group_roots`] / [`leading_ident`] machinery the import allow-list
/// uses: a braced group has no canonical order, so `use mp_core::{config,
/// secrets::SecretBackend};` is the same reach as `use mp_core::{secrets,
/// config};` and a substring match only sees the second one.
pub const MP_CORE_ENGINE_MODULES: [&str; 2] = ["oauth2", "secrets"];

/// The same two modules as the fully-qualified paths a call spells them with
/// when no `use` line mentions them, which is what [`TUI_ENGINE_PATHS`] scans
/// for on the `crate::` side.
pub const MP_CORE_ENGINE_PATHS: [&str; 2] = ["mp_core::oauth2", "mp_core::secrets"];

const CLIENT_ROOT: &str = "crates/mp-tui/src";
const ALLOW_LIST: &str = "tests/fixtures/tui-engine-imports.txt";
const PATH_ALLOW_LIST: &str = "tests/fixtures/tui-engine-paths.txt";

/// Every engine module imported under `root`, as `(path relative to root,
/// module)` pairs.
///
/// Both `use crate::…` and `use mailypoppins::…` are collected, in plain,
/// braced (`use crate::{store::Store, sync};`), aliased and `pub use` forms,
/// anywhere in the file: an import inside a `#[cfg(test)] mod tests` block is
/// still a dependency of the client crate on the engine, and dedup by
/// `(file, module)` keeps it from double-counting a module the file already
/// imports at the top.
pub fn engine_imports(root: &Path) -> BTreeSet<(String, String)> {
    let mut found = BTreeSet::new();
    for file in rust_files(root) {
        let rel = relative(root, &file);
        let source = fs::read_to_string(&file).unwrap_or_else(|e| panic!("read {file:?}: {e}"));
        for module in imported_crate_roots(&strip_line_comments(&source)) {
            if ENGINE_MODULES.contains(&module.as_str()) {
                found.insert((rel.clone(), module));
            }
        }
    }
    found
}

/// Every engine reach into `mp-core` from under `root`, as `(path relative to
/// root, `mp_core::<module>`)` pairs.
///
/// Two passes over one file, which between them cover both spellings and count
/// a file that uses both once, because both report the same pair:
///
/// - the `use` lines, parsed to their module segment, so a braced group is read
///   whatever order its items are in and however deeply they nest;
/// - everything that is not a `use` line, scanned for the fully-qualified path,
///   which is how a call reaches a module no import mentions.
///
/// Test modules are scanned along with production code: a test that opened a
/// secret backend would be opening the developer's own keyring.
pub fn mp_core_engine_reaches(root: &Path) -> BTreeSet<(String, String)> {
    let mut found = BTreeSet::new();
    for file in rust_files(root) {
        let rel = relative(root, &file);
        let source = fs::read_to_string(&file).unwrap_or_else(|e| panic!("read {file:?}: {e}"));
        let source = strip_line_comments(&source);
        for module in imported_roots(&source, &["mp_core::"]) {
            if MP_CORE_ENGINE_MODULES.contains(&module.as_str()) {
                found.insert((rel.clone(), format!("mp_core::{module}")));
            }
        }
        let called = strip_use_statements(&source);
        for path in MP_CORE_ENGINE_PATHS {
            if called.contains(path) {
                found.insert((rel.clone(), path.to_string()));
            }
        }
    }
    found
}

/// `file` as a `/`-joined path relative to `root`.
fn relative(root: &Path, file: &Path) -> String {
    file.strip_prefix(root)
        .unwrap_or(file)
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// Every `*.rs` file under `root`, recursively, in no particular order.
fn rust_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = fs::read_dir(&dir).unwrap_or_else(|e| panic!("read_dir {dir:?}: {e}"));
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

/// Drop `//` comments, so that a doc comment quoting an import does not count
/// as one. Block comments are left alone: stripping them without a real lexer
/// would mangle any `/*` inside a string literal, and the tree has none.
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

/// The first path segment of every `use crate::…` / `use mailypoppins::…` in
/// `source`, with braced groups expanded one level.
fn imported_crate_roots(source: &str) -> Vec<String> {
    imported_roots(source, &["crate::", "mailypoppins::"])
}

/// The first path segment of every `use <prefix>…` in `source`, for any of
/// `prefixes`, with braced groups expanded one level.
fn imported_roots(source: &str, prefixes: &[&str]) -> Vec<String> {
    let bytes = source.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(at) = source[i..].find("use ") {
        let start = i + at;
        i = start + 4;
        // Only a `use` that starts a statement counts, not the tail of an
        // identifier such as `reuse ` or a word inside a string.
        if start > 0 && is_ident_byte(bytes[start - 1]) {
            continue;
        }
        let rest = source[i..].trim_start();
        let Some(tail) = prefixes.iter().find_map(|prefix| rest.strip_prefix(prefix)) else {
            continue;
        };
        match tail.trim_start().strip_prefix('{') {
            Some(group) => out.extend(brace_group_roots(group)),
            None => out.extend(leading_ident(tail)),
        }
    }
    out
}

/// The first segment of each comma-separated item in a braced `use` group,
/// given the text just after the opening brace.
fn brace_group_roots(group: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 1usize;
    let mut item = String::new();
    for ch in group.chars() {
        match ch {
            '{' => {
                depth += 1;
                item.push(ch);
            }
            '}' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
                item.push(ch);
            }
            ',' if depth == 1 => {
                out.extend(leading_ident(&item));
                item.clear();
            }
            _ => item.push(ch),
        }
    }
    out.extend(leading_ident(&item));
    out
}

/// The identifier a path starts with, if it starts with one.
fn leading_ident(path: &str) -> Option<String> {
    let path = path.trim_start();
    let ident: String = path.chars().take_while(|c| is_ident_char(*c)).collect();
    (!ident.is_empty()).then_some(ident)
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn is_ident_byte(b: u8) -> bool {
    (b as char).is_ascii_alphanumeric() || b == b'_'
}

/// The repository root, derived from the manifest directory so the test does
/// not depend on the working directory cargo happens to use.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn render(entries: &BTreeSet<(String, String)>) -> String {
    let mut out = String::new();
    for (file, module) in entries {
        out.push_str(file);
        out.push(' ');
        out.push_str(module);
        out.push('\n');
    }
    out
}

fn parse_allow_list(text: &str) -> BTreeSet<(String, String)> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let (file, module) = line
                .split_once(' ')
                .unwrap_or_else(|| panic!("allow-list line is not `<file> <module>`: {line:?}"));
            (file.to_string(), module.trim().to_string())
        })
        .collect()
}

#[test]
fn tui_engine_imports_match_the_allow_list() {
    let root = repo_root().join(CLIENT_ROOT);
    assert!(root.is_dir(), "client root {root:?} does not exist");
    assert!(
        !rust_files(&root).is_empty(),
        "no .rs files under {root:?}: the walk found nothing to check"
    );

    let actual = engine_imports(&root);
    let fixture = repo_root().join(ALLOW_LIST);

    if std::env::var_os("UPDATE_TUI_ENGINE_IMPORTS").is_some() {
        fs::write(&fixture, render(&actual)).expect("rewrite allow-list");
        return;
    }

    let expected = parse_allow_list(&fs::read_to_string(&fixture).expect("read allow-list"));

    let added: Vec<_> = actual.difference(&expected).collect();
    let removed: Vec<_> = expected.difference(&actual).collect();
    if added.is_empty() && removed.is_empty() {
        return;
    }

    let mut report = String::new();
    for (file, module) in &added {
        report.push_str(&format!(
            "  new engine import: {CLIENT_ROOT}/{file} uses `{module}`\n"
        ));
    }
    for (file, module) in &removed {
        report.push_str(&format!(
            "  engine import gone: {CLIENT_ROOT}/{file} no longer uses `{module}`\n"
        ));
    }
    panic!(
        "the TUI's engine imports moved ({} in the tree, {} in {ALLOW_LIST}):\n{report}\n\
         The client/engine boundary must not widen. If the change is deliberate, re-record it \
         with `UPDATE_TUI_ENGINE_IMPORTS=1 cargo test --test architecture_boundaries`.",
        actual.len(),
        expected.len(),
    );
}

#[test]
fn the_allow_list_is_sorted_deduped_and_names_only_engine_modules() {
    let fixture = repo_root().join(ALLOW_LIST);
    let text = fs::read_to_string(&fixture).expect("read allow-list");
    let entries = parse_allow_list(&text);
    assert_eq!(
        render(&entries),
        text,
        "{ALLOW_LIST} is not sorted, is not deduped, or is not `<file> <module>` per line"
    );
    for (file, module) in &entries {
        assert!(
            ENGINE_MODULES.contains(&module.as_str()),
            "{ALLOW_LIST} names `{module}` (from {file}), which is not an engine module"
        );
    }
}

#[test]
fn engine_imports_reads_every_use_form_and_ignores_shared_modules() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let root = tmp.path();
    fs::create_dir_all(root.join("ui")).expect("mkdir");
    fs::write(
        root.join("plain.rs"),
        "use crate::store::open_store;\nuse crate::config::AccountConfig;\n",
    )
    .expect("write");
    fs::write(
        root.join("ui/grouped.rs"),
        "use crate::{store::{BlobStore, Store}, sync, parse::FetchedEmail};\n\
         pub use mailypoppins::ops::ServerOp as Op;\n\
         // use crate::secrets::load;\n\
         fn f() { use crate::pending_ops; }\n",
    )
    .expect("write");
    fs::write(
        root.join("ui/multiline.rs"),
        "use crate::send::{\n    SendReport,\n};\nuse crate::store::drafts;\n",
    )
    .expect("write");

    let found = engine_imports(root);
    let expected: BTreeSet<(String, String)> = [
        ("plain.rs", "store"),
        ("ui/grouped.rs", "ops"),
        ("ui/grouped.rs", "pending_ops"),
        ("ui/grouped.rs", "store"),
        ("ui/grouped.rs", "sync"),
        ("ui/multiline.rs", "send"),
        ("ui/multiline.rs", "store"),
    ]
    .into_iter()
    .map(|(f, m)| (f.to_string(), m.to_string()))
    .collect();
    assert_eq!(found, expected);
}

// ---------------------------------------------------------------------------
// The TUI's engine calls (#0126, P5-U10d)
// ---------------------------------------------------------------------------

/// The paths a `crates/mp-tui` could not resolve, as they are spelled in the
/// tree, and what each group is about.
///
/// Prefixes rather than a parse, for the reason [`ENGINE_SYMBOLS`] is a list of
/// substrings: a test that needed a Rust front end to attribute a call would be
/// a second compiler to maintain. The cost is that a comment naming one would
/// count, which [`strip_line_comments`] removes, and that a `use` line would
/// count twice, which [`strip_use_statements`] removes.
///
/// Three groups:
///
/// - **the engine modules**, the same [`ENGINE_MODULES`] names reached through
///   a path instead of an import, plus the two that left for `mp-core` and are
///   spelled `crate::secrets::` / `crate::oauth2::` no longer;
/// - **the root crate's own halves of the shared modules**, which live beside
///   the engine because they need a store, a row or an index: the agenda
///   loader and the five `draft` operations. `crate::draft::` on its own would
///   be wrong, because most of that module is `mp_core`'s and a client may
///   reach it, so the five are named as the symbols they are spelled by. They
///   are symbols rather than paths for a second reason too: the TUI imported
///   them and called them bare, so there was no `crate::` path to look for;
/// - **`crate::daemon::`**, which is the daemon itself. The TUI reached it for
///   one thing, the connect helper, and a client crate that linked the daemon
///   would have no boundary at all. Since P5-U10f it reaches it for nothing:
///   the binary hands `mp_tui::run` a `session::Connector` of two function
///   pointers, so the exit-4 diagnostic stays in the process that owns the
///   terminal and the fixture is empty.
///
/// The whole list is a belt over the braces the manifest now provides. It is
/// kept because a list that went to zero and was deleted would have to be
/// rewritten from memory the day someone reaches for the engine again, and
/// because `crate::` here means `mp_tui` since the move: a row appearing is a
/// module of the TUI crate that has grown an engine of its own.
const TUI_ENGINE_PATHS: [&str; 18] = [
    "crate::agenda::",
    "crate::daemon::",
    "crate::graph::",
    "crate::imap_client::",
    "crate::ingest::",
    "crate::oauth2::",
    "crate::ops::",
    "crate::outbox::",
    "crate::pending_ops::",
    "crate::secrets::",
    "crate::send::",
    "crate::store::",
    "crate::sync::",
    "create_draft_from_source(",
    "delete_indexed_draft(",
    "new_draft_skeleton(",
    "settle_sent_draft(",
    "source_from_row(",
];

/// Every engine path reached from the **production** code under `root`, as
/// `(path relative to root, path prefix)` pairs.
///
/// Production only, which is the difference from [`engine_imports`]: a test
/// module under the TUI moves with the code it tests, and a file that is
/// nothing but a test module (declared `#[cfg(test)] mod x;`) moves whole. What
/// blocks the crate move is the paths a *frame* takes.
pub fn engine_paths(root: &Path) -> BTreeSet<(String, String)> {
    let test_only = test_only_files(root);
    let mut found = BTreeSet::new();
    for file in rust_files(root) {
        if test_only.contains(&file) {
            continue;
        }
        let rel = relative(root, &file);
        let source = fs::read_to_string(&file).unwrap_or_else(|e| panic!("read {file:?}: {e}"));
        let source = strip_use_statements(&strip_test_modules(&strip_line_comments(&source)));
        for path in TUI_ENGINE_PATHS {
            if source.contains(path) {
                found.insert((rel.clone(), path.to_string()));
            }
        }
    }
    found
}

/// Every file under `root` that a parent module declares `#[cfg(test)]`.
///
/// Derived rather than listed by name: the TUI carries several such files and a
/// suffix convention would be a second rule to keep in step with the `mod`
/// lines that decide it.
fn test_only_files(root: &Path) -> BTreeSet<PathBuf> {
    let mut found = BTreeSet::new();
    for file in rust_files(root) {
        let source = fs::read_to_string(&file).unwrap_or_else(|e| panic!("read {file:?}: {e}"));
        // The directory the `mod` lines of this file name modules in.
        let dir = match file.file_stem().and_then(|s| s.to_str()) {
            Some("mod") | Some("lib") | Some("main") => file.parent().map(Path::to_path_buf),
            Some(stem) => file.parent().map(|parent| parent.join(stem)),
            None => None,
        };
        let Some(dir) = dir else { continue };
        for name in test_only_module_names(&source) {
            found.insert(dir.join(format!("{name}.rs")));
            found.insert(dir.join(&name).join("mod.rs"));
        }
    }
    found
}

/// The module names `source` declares behind `#[cfg(test)]`.
fn test_only_module_names(source: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = source;
    while let Some(at) = rest.find("#[cfg(test)]") {
        rest = &rest[at + "#[cfg(test)]".len()..];
        let head = rest.trim_start();
        let head = head
            .strip_prefix("pub mod ")
            .or_else(|| head.strip_prefix("mod "));
        let Some(head) = head else { continue };
        let name: String = head.chars().take_while(|c| is_ident_char(*c)).collect();
        // A `#[cfg(test)] mod tests { … }` in the same file declares no file.
        if !name.is_empty() && head[name.len()..].trim_start().starts_with(';') {
            out.push(name);
        }
    }
    out
}

/// Drop every `use` statement, so an import counted by the allow-list above is
/// not counted a second time here.
///
/// Line-based, and a braced multi-line `use` is covered by its first line: the
/// continuation lines carry item names, not crate paths.
fn strip_use_statements(source: &str) -> String {
    source
        .lines()
        .filter(|line| {
            let line = line.trim_start();
            !(line.starts_with("use ")
                || line.starts_with("pub use ")
                || line.starts_with("pub(crate) use ")
                || line.starts_with("pub(super) use "))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The move's real progress bar: every engine path the TUI's production code
/// takes is one the allow-list names, and every one it names is still taken.
#[test]
fn tui_engine_paths_match_the_allow_list() {
    let root = repo_root().join(CLIENT_ROOT);
    assert!(root.is_dir(), "client root {root:?} does not exist");

    let actual = engine_paths(&root);
    let fixture = repo_root().join(PATH_ALLOW_LIST);

    if std::env::var_os("UPDATE_TUI_ENGINE_IMPORTS").is_some() {
        fs::write(&fixture, render(&actual)).expect("rewrite the path allow-list");
        return;
    }

    let expected = parse_allow_list(&fs::read_to_string(&fixture).expect("read the path allow-list"));

    let added: Vec<_> = actual.difference(&expected).collect();
    let removed: Vec<_> = expected.difference(&actual).collect();
    if added.is_empty() && removed.is_empty() {
        return;
    }

    let mut report = String::new();
    for (file, path) in &added {
        report.push_str(&format!(
            "  new engine call: {CLIENT_ROOT}/{file} reaches `{path}`\n"
        ));
    }
    for (file, path) in &removed {
        report.push_str(&format!(
            "  engine call gone: {CLIENT_ROOT}/{file} no longer reaches `{path}`\n"
        ));
    }
    panic!(
        "the TUI's engine calls moved ({} in the tree, {} in {PATH_ALLOW_LIST}):\n{report}\n\
         The client/engine boundary must not widen, and a call that went away belongs struck \
         from the fixture in the same commit. If the change is deliberate, re-record it with \
         `UPDATE_TUI_ENGINE_IMPORTS=1 cargo test --test architecture_boundaries`.",
        actual.len(),
        expected.len(),
    );
}

/// The two `mp-core` modules a client may not use, scanned over the whole TUI
/// crate, tests included.
///
/// The crate boundary is the proof for the eleven engine modules: `mp-tui`'s
/// manifest names no `mailypoppins`, so `crate::store::…` does not resolve and
/// no scan is needed to know it. It is *not* the proof for these two, because
/// `mp-tui` does depend on `mp-core` and both live there: a client that reached
/// for a keyring or ran a device-code flow would compile. This is the gate that
/// says it does not, and it scans test modules as well as production code,
/// because a test that opened a secret backend would be opening the user's
/// keyring on the developer's machine.
#[test]
fn the_tui_crate_reaches_no_engine_module_of_the_shared_crate() {
    let root = repo_root().join(CLIENT_ROOT);
    assert!(!rust_files(&root).is_empty(), "no .rs files under {root:?}");
    let found: Vec<String> = mp_core_engine_reaches(&root)
        .into_iter()
        .map(|(file, path)| format!("  {CLIENT_ROOT}/{file} reaches `{path}`"))
        .collect();
    assert!(
        found.is_empty(),
        "the TUI crate reaches the engine half of `mp-core`:\n{}\n\
         `secrets` and `oauth2` are the daemon's: it owns the keyring and the device-code flow, \
         and a client that opened either would be the second engine the boundary exists to \
         prevent.",
        found.join("\n")
    );
}

/// The path allow-list is sorted, deduped and names only scanned paths.
#[test]
fn the_path_allow_list_is_sorted_deduped_and_names_only_scanned_paths() {
    let fixture = repo_root().join(PATH_ALLOW_LIST);
    let text = fs::read_to_string(&fixture).expect("read the path allow-list");
    let entries = parse_allow_list(&text);
    assert_eq!(
        render(&entries),
        text,
        "{PATH_ALLOW_LIST} is not sorted, is not deduped, or is not `<file> <path>` per line"
    );
    for (file, path) in &entries {
        assert!(
            TUI_ENGINE_PATHS.contains(&path.as_str()),
            "{PATH_ALLOW_LIST} names `{path}` (from {file}), which the scan does not look for"
        );
    }
}

/// The `mp-core` scanner reads a module segment wherever a `use` puts it, reads
/// a fully-qualified call, and reads neither out of a comment.
///
/// The row exists because the scan it replaced was four substrings, and
/// `use mp_core::{config, secrets::SecretBackend};` is none of them: it matched
/// `use mp_core::{secrets` and `mp_core::secrets`, so a `secrets` that was not
/// the first item of its brace group went through. A braced group has no
/// canonical order, and nothing makes a developer write the engine module first.
#[test]
fn the_shared_crate_scanner_reads_a_module_segment_wherever_a_use_puts_it() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let root = tmp.path();
    fs::create_dir_all(root.join("ui")).expect("mkdir");
    fs::write(
        root.join("first.rs"),
        "use mp_core::{secrets::SecretBackend, config};\n",
    )
    .expect("write");
    // The spelling the four-substring scan could not see.
    fs::write(
        root.join("later.rs"),
        "use mp_core::{config, secrets::SecretBackend};\n",
    )
    .expect("write");
    fs::write(
        root.join("ui/nested.rs"),
        "use mp_core::{\n    config::{store_path, AccountConfig},\n    oauth2,\n};\n",
    )
    .expect("write");
    fs::write(
        root.join("ui/called.rs"),
        "fn open() { let _ = mp_core::secrets::backend(); }\n",
    )
    .expect("write");
    // A file that both imports and calls reports the reach once.
    fs::write(
        root.join("ui/both.rs"),
        "use mp_core::oauth2::DeviceFlow;\nfn go() { mp_core::oauth2::start(); }\n",
    )
    .expect("write");
    fs::write(
        root.join("ui/clean.rs"),
        "use mp_core::{config, parse::FetchedEmail};\n\
         // use mp_core::secrets::SecretBackend;\n\
         // mp_core::oauth2::start() is named in a comment\n\
         fn f() {}\n",
    )
    .expect("write");

    let found = mp_core_engine_reaches(root);
    let expected: BTreeSet<(String, String)> = [
        ("first.rs", "mp_core::secrets"),
        ("later.rs", "mp_core::secrets"),
        ("ui/both.rs", "mp_core::oauth2"),
        ("ui/called.rs", "mp_core::secrets"),
        ("ui/nested.rs", "mp_core::oauth2"),
    ]
    .into_iter()
    .map(|(f, p)| (f.to_string(), p.to_string()))
    .collect();
    assert_eq!(found, expected);
}

/// The scanner reads a fully-qualified call, ignores a `use` of the same
/// module, ignores a comment, and ignores a `#[cfg(test)]` module both in a
/// file and as a whole file.
#[test]
fn the_path_scanner_reads_calls_and_ignores_imports_comments_and_tests() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let root = tmp.path();
    fs::create_dir_all(root.join("app")).expect("mkdir");
    fs::write(
        root.join("mod.rs"),
        "mod real;\n#[cfg(test)]\nmod only_tests;\npub mod app;\n",
    )
    .expect("write");
    fs::write(
        root.join("real.rs"),
        "use crate::store::open_store;\n\
         fn paint() { let _ = crate::outbox::counts_for_account(\"a\"); }\n\
         // crate::sync::tick() is named in a comment\n\
         #[cfg(test)]\n\
         mod tests {\n    fn t() { crate::ingest::ingest_message(); }\n}\n",
    )
    .expect("write");
    fs::write(
        root.join("only_tests.rs"),
        "fn t() { let _ = crate::send::send_draft(); }\n",
    )
    .expect("write");
    fs::write(
        root.join("app/mod.rs"),
        "fn body() { crate::store::read::thread_messages(); }\n\
         fn quote() { crate::draft::create_draft_from_source(); }\n\
         fn parse() { crate::draft::parse_email_draft(); }\n",
    )
    .expect("write");

    let found = engine_paths(root);
    let expected: BTreeSet<(String, String)> = [
        ("app/mod.rs", "create_draft_from_source("),
        ("app/mod.rs", "crate::store::"),
        ("real.rs", "crate::outbox::"),
    ]
    .into_iter()
    .map(|(f, p)| (f.to_string(), p.to_string()))
    .collect();
    assert_eq!(found, expected);
}

// ---------------------------------------------------------------------------
// The CLI's engine touches (#0123, P4-U15)
// ---------------------------------------------------------------------------

/// The client-side sources: the binary and the handler modules it calls.
///
/// `src/tui/` is deliberately absent, and stays absent until Phase 5 (#0124)
/// takes the TUI off the direct path; the first half of this file is what
/// records its residue meanwhile. The daemon (`src/daemon/`) and the engine
/// modules themselves are absent for the opposite reason: opening a store is
/// their job.
const CLIENT_SOURCES: [&str; 7] = [
    "src/main.rs",
    "src/calendar_cmd.rs",
    "src/contacts_cmd.rs",
    "src/cutover.rs",
    "src/draft_cmd.rs",
    "src/read_cmd.rs",
    "src/config_cmd",
];

/// The symbols that open a store, a secret backend, a network backend or an
/// engine lock, as they are spelled in the tree.
///
/// Substrings rather than a parse: `Store::open` is `Store::open` however it is
/// qualified, and a test that needed a Rust front end to say so would be a
/// second compiler to maintain. The cost is that a comment mentioning one would
/// count, which `strip_line_comments` and `strip_test_modules` between them
/// remove.
const ENGINE_SYMBOLS: [&str; 23] = [
    // the store
    "Store::open",
    "BlobStore::",
    "sweep::sweep(",
    "ingest::",
    "pending_ops::",
    "outbox::",
    "reconcile_account(",
    // the engine lock
    "EngineLock",
    // the secret backend
    "open_secrets(",
    "get_secret(",
    "set_secret(",
    "init_secrets_backend(",
    "secrets_path(",
    "token_cache_path(",
    "load_token_cache(",
    // the network backends
    "device_code_flow(",
    "SmtpConfig::load",
    "ImapConfig::load",
    "GraphConfig::load",
    "build_smtp_transport",
    "imap_client::",
    "GraphClient::",
    "SmtpTransport::",
];

/// Every engine touch left on the CLI's path after P4-U15, with the reason it
/// is still there.
///
/// `(file, symbol, reason)`, sorted by file then symbol. Read it as the answer
/// to "why is the Phase 4 gate not at literal zero": four groups, each with a
/// cause that outlives this unit.
const CLI_ENGINE_RESIDUE: [(&str, &str, &str); 17] = [
    // (d) The two wizards. They call `config.get` (`routed_config_state` in
    //     `src/main.rs`) for the path, whether the file exists and the account
    //     names, and do the rest themselves: the prompting, the connection
    //     tests between the prompts, the password writes and the file write are
    //     one interactive transaction. Splitting it needs a wizard protocol,
    //     which no unit of Phase 4 contracted. `config.init` and
    //     `config.add_account` are served and are what such a protocol would
    //     build on, but no wizard calls either; only `tests/daemon_config.rs`
    //     exercises them (`BACKLOG.md`).
    (
        "src/config_cmd/helpers.rs",
        "SmtpTransport::",
        "the wizards' SMTP connection test, run between two prompts",
    ),
    (
        "src/config_cmd/helpers.rs",
        "imap_client::",
        "the wizards' IMAP connection test, run between two prompts",
    ),
    (
        "src/config_cmd/init.rs",
        "GraphClient::",
        "the wizards' Graph connection test, run between two prompts",
    ),
    (
        "src/config_cmd/init.rs",
        "device_code_flow(",
        "the wizards' inline OAuth2 login, run between two prompts",
    ),
    (
        "src/config_cmd/init.rs",
        "imap_client::",
        "the wizards' mailbox listing, which the next prompt offers as its choices",
    ),
    (
        "src/config_cmd/init.rs",
        "set_secret(",
        "the wizards store the password they have just prompted for",
    ),
    // (c) `mp config show`'s three local probes. `config.get` is contracted
    //     *not* to look a secret up (`tests/daemon_config.rs`), its key set is
    //     pinned at four, and the config family is pinned at nine methods by
    //     `tests/daemon_admin_slice.rs`, so neither a field nor a method can
    //     carry them without editing a T unit's file.
    (
        "src/config_cmd/show.rs",
        "get_secret(",
        "mp config show's (not set)/**** column; config.get is contracted not to probe a secret",
    ),
    (
        "src/config_cmd/show.rs",
        "load_token_cache(",
        "mp config show's token = valid/expired/invalid line; the same contract",
    ),
    (
        "src/config_cmd/show.rs",
        "token_cache_path(",
        "mp config show's token = not cached line; the same contract",
    ),
    // (a) The server leg of `mp search` (LST-06). The method it wants exists
    //     since P5-U10c - `message.search_server`, which the TUI's overlay is
    //     routed through - and three differences stand between this leg and
    //     it, each user-visible and none of them a call-site change:
    //     `mp search` prints `Search in <mailbox> failed` to stderr per
    //     mailbox as it goes, where the operation reports `unreachable` at the
    //     settle; `--mailbox` here names the server mailbox directly, so a
    //     name the account does not configure is searched rather than refused;
    //     and the plain-IMAP `has:attachment` warning is a sentence about a
    //     post-filter the daemon now applies itself. Routing it is a unit of
    //     its own, recorded in `docs/tickets/0126-tui-crate-move.md`.
    (
        "src/main.rs",
        "GraphClient::",
        "mp search (server leg): message.search_server exists, but routing this leg moves three user-visible behaviours",
    ),
    (
        "src/main.rs",
        "GraphConfig::load",
        "mp search (server leg): message.search_server exists, but routing this leg moves three user-visible behaviours",
    ),
    (
        "src/main.rs",
        "ImapConfig::load",
        "mp search (server leg): message.search_server exists, but routing this leg moves three user-visible behaviours",
    ),
    // (b) The startup preamble, which runs before any socket and on the
    //     no-daemon list too (`mp config path`, `mp daemon *`). Routing it would
    //     move bytes on commands that must never need a daemon.
    (
        "src/main.rs",
        "SmtpConfig::load",
        "the preamble's `Could not load SMTP config` pair, and mp send-approved's, print before any daemon call",
    ),
    (
        "src/main.rs",
        "Store::open",
        "mp search (server leg): the plain-IMAP has:attachment post-filter reads the local index",
    ),
    (
        "src/main.rs",
        "imap_client::",
        "mp search (server leg): message.search_server exists, but routing this leg moves three user-visible behaviours",
    ),
    (
        "src/main.rs",
        "init_secrets_backend(",
        "the preamble's Undecryptable and Other branches print and exit before any daemon call",
    ),
    (
        "src/main.rs",
        "secrets_path(",
        "mp config reset-secrets prints the path; a path, and no secret read",
    ),
];

/// Every `(file, symbol)` pair the scan finds under `CLIENT_SOURCES`.
fn cli_engine_touches() -> BTreeSet<(String, String)> {
    let root = repo_root();
    let mut found = BTreeSet::new();
    for entry in CLIENT_SOURCES {
        let path = root.join(entry);
        assert!(path.exists(), "client source {path:?} does not exist");
        let files = if path.is_dir() {
            rust_files(&path)
        } else {
            vec![path]
        };
        for file in files {
            let rel = file
                .strip_prefix(&root)
                .unwrap_or(&file)
                .to_string_lossy()
                .replace('\\', "/");
            // A file that is nothing but a test module is one; `src/config_cmd/
            // tests.rs` is included from `mod.rs` under `#[cfg(test)]`.
            if file.file_stem().is_some_and(|s| s == "tests") {
                continue;
            }
            let source = fs::read_to_string(&file).unwrap_or_else(|e| panic!("read {file:?}: {e}"));
            let source = strip_test_modules(&strip_line_comments(&source));
            for symbol in ENGINE_SYMBOLS {
                if source.contains(symbol) {
                    found.insert((rel.clone(), symbol.to_string()));
                }
            }
        }
    }
    found
}

/// Drop every `#[cfg(test)] mod … { … }` block, braces matched.
///
/// A unit test may open a store: it is the engine's own test, running in the
/// same file for want of anywhere better, and it is not a CLI handler.
fn strip_test_modules(source: &str) -> String {
    let mut out = source.to_string();
    loop {
        let Some(at) = out.find("#[cfg(test)]") else {
            return out;
        };
        // The `mod` this attribute guards, if it guards one; anything else
        // (a `#[cfg(test)] use`, say) is left where it is, with the attribute
        // neutralised so the search moves on.
        let after = &out[at + "#[cfg(test)]".len()..];
        let trimmed = after.trim_start();
        if !trimmed.starts_with("mod ") && !trimmed.starts_with("pub mod ") {
            out.replace_range(at..at + 1, " ");
            continue;
        }
        let Some(open) = out[at..].find('{').map(|i| at + i) else {
            return out;
        };
        let mut depth = 0usize;
        let mut end = out.len();
        for (i, ch) in out[open..].char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = open + i + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        out.replace_range(at..end, "");
    }
}

/// The Phase 4 gate: every engine touch left on the CLI's path is one the
/// residue table names, and every entry the table names is still there.
#[test]
fn the_cli_opens_no_store_secret_network_or_engine_lock_outside_the_residue() {
    let actual = cli_engine_touches();
    let expected: BTreeSet<(String, String)> = CLI_ENGINE_RESIDUE
        .iter()
        .map(|(file, symbol, _)| (file.to_string(), symbol.to_string()))
        .collect();

    let added: Vec<_> = actual.difference(&expected).collect();
    let removed: Vec<_> = expected.difference(&actual).collect();
    if added.is_empty() && removed.is_empty() {
        return;
    }

    let mut report = String::new();
    for (file, symbol) in &added {
        report.push_str(&format!("  new engine touch: {file} uses `{symbol}`\n"));
    }
    for (file, symbol) in &removed {
        let reason = CLI_ENGINE_RESIDUE
            .iter()
            .find(|(f, s, _)| f == file && s == symbol)
            .map(|(_, _, reason)| *reason)
            .unwrap_or("");
        report.push_str(&format!(
            "  engine touch gone: {file} no longer uses `{symbol}` ({reason})\n"
        ));
    }
    panic!(
        "the CLI's engine touches moved ({} in the tree, {} in CLI_ENGINE_RESIDUE):\n{report}\n\
         Phase 4's gate is that a CLI handler opens no store, secret backend, network backend \
         or engine lock. A new touch belongs behind the daemon; a touch that went away belongs \
         struck from CLI_ENGINE_RESIDUE in the same commit.",
        actual.len(),
        expected.len(),
    );
}

/// The residue table is sorted, deduped, names only scanned symbols, and gives
/// every entry a reason. A row with an empty reason is a row nobody thought
/// about, which is the thing this table exists to prevent.
#[test]
fn every_residue_entry_names_a_scanned_symbol_and_a_reason() {
    let mut previous: Option<(&str, &str)> = None;
    for (file, symbol, reason) in CLI_ENGINE_RESIDUE {
        assert!(
            ENGINE_SYMBOLS.contains(&symbol),
            "CLI_ENGINE_RESIDUE names `{symbol}` (from {file}), which the scan does not look for"
        );
        assert!(
            CLIENT_SOURCES
                .iter()
                .any(|source| file == *source || file.starts_with(&format!("{source}/"))),
            "CLI_ENGINE_RESIDUE names {file}, which is not under CLIENT_SOURCES"
        );
        assert!(
            reason.len() > 20,
            "CLI_ENGINE_RESIDUE's entry for {file} `{symbol}` has no usable reason: {reason:?}"
        );
        if let Some(previous) = previous {
            assert!(
                previous < (file, symbol),
                "CLI_ENGINE_RESIDUE is not sorted: {previous:?} precedes {:?}",
                (file, symbol)
            );
        }
        previous = Some((file, symbol));
    }
}

/// `strip_test_modules` removes a `#[cfg(test)]` module whole, nested braces
/// and string literals included, and leaves the code around it alone.
#[test]
fn strip_test_modules_removes_the_block_and_nothing_else() {
    let source = "fn real() { let _ = 1; }\n\
                  #[cfg(test)]\n\
                  mod tests {\n    fn t() { Store::open(\"x\"); }\n}\n\
                  fn after() {}\n";
    let stripped = strip_test_modules(source);
    assert!(!stripped.contains("Store::open"), "{stripped}");
    assert!(stripped.contains("fn real()"), "{stripped}");
    assert!(stripped.contains("fn after()"), "{stripped}");
}
