//! The client/engine import boundary, recorded so it cannot widen (#0118).
//!
//! The TUI calls the library directly today: it opens the store, drives the
//! IMAP client, reads secrets and queues server ops itself. The daemon
//! migration has to drive that set to zero, and the only way to know it is
//! shrinking is to have written down what it is. This file walks `src/tui/`,
//! collects every `use` of an engine module, and compares the result against
//! `tests/fixtures/tui-engine-imports.txt`. A new engine import in the TUI
//! fails the test naming the file and the module; a removed one fails it too,
//! because the allow-list is a record, not a ceiling, and the number in it is
//! the migration's progress bar.
//!
//! Nothing here is feature-gated: it passes on the pre-daemon tree, which is
//! the point. `engine_imports` takes the client source root as an argument so
//! that Phase 5 can re-point it at `crates/mp-tui/` without a rewrite.
//!
//! To re-record the allow-list after a deliberate change, run
//! `UPDATE_TUI_ENGINE_IMPORTS=1 cargo test --test architecture_boundaries`
//! and commit the diff.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// The library modules that make up the engine: everything the TUI must stop
/// touching directly once it talks to the daemon instead. Modules that stay
/// shared between client and engine (`types`, `config`, `parse`, `search`,
/// `selector`, ...) are deliberately absent.
pub const ENGINE_MODULES: [&str; 11] = [
    "graph",
    "imap_client",
    "ingest",
    "oauth2",
    "ops",
    "outbox",
    "pending_ops",
    "secrets",
    "send",
    "store",
    "sync",
];

const CLIENT_ROOT: &str = "src/tui";
const ALLOW_LIST: &str = "tests/fixtures/tui-engine-imports.txt";

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
        let rel = file
            .strip_prefix(root)
            .unwrap_or(&file)
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        let source = fs::read_to_string(&file).unwrap_or_else(|e| panic!("read {file:?}: {e}"));
        for module in imported_crate_roots(&strip_line_comments(&source)) {
            if ENGINE_MODULES.contains(&module.as_str()) {
                found.insert((rel.clone(), module));
            }
        }
    }
    found
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
        let Some(tail) = rest
            .strip_prefix("crate::")
            .or_else(|| rest.strip_prefix("mailypoppins::"))
        else {
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
