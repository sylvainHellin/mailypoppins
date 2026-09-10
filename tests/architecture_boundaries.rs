//! The client/engine boundary, recorded so it cannot widen (#0118, #0123).
//!
//! Two boundaries, two halves.
//!
//! **The TUI's imports.** The TUI calls the library directly today: it opens
//! the store, drives the IMAP client, reads secrets and queues server ops
//! itself. The daemon migration has to drive that set to zero, and the only way
//! to know it is shrinking is to have written down what it is. The first half
//! walks `src/tui/`, collects every `use` of an engine module, and compares the
//! result against `tests/fixtures/tui-engine-imports.txt`. A new engine import
//! in the TUI fails the test naming the file and the module; a removed one
//! fails it too, because the allow-list is a record, not a ceiling, and the
//! number in it is the migration's progress bar. **The TUI is out of scope of
//! the CLI allow-list below until Phase 5** (ticket #0124), which is the unit
//! that takes it off the direct path; until then its residue is this fixture's
//! business and nothing else's.
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
//! Nothing here is feature-gated: it passes on the pre-daemon tree, which is
//! the point. `engine_imports` takes the client source root as an argument so
//! that Phase 5 can re-point it at `crates/mp-tui/` without a rewrite.
//!
//! To re-record the TUI allow-list after a deliberate change, run
//! `UPDATE_TUI_ENGINE_IMPORTS=1 cargo test --test architecture_boundaries`
//! and commit the diff. The CLI residue has no such switch on purpose: an entry
//! is added by hand, with its reason, or it is not added.

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
const CLIENT_SOURCES: [&str; 3] = ["src/main.rs", "src/cutover.rs", "src/config_cmd"];

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
    // (d) The two wizards. `config.init` and `config.add_account` exist and are
    //     what `mp config init` asks for its path and its account list, but the
    //     prompting, the connection tests between the prompts and the write are
    //     one interactive transaction; splitting it needs a wizard protocol,
    //     which no unit of Phase 4 contracted.
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
    // (a) The server leg of `mp search` (LST-06), which no slice migrated.
    //     `docs/parity-matrix.md` carries it as "not started": the read slice
    //     (P4-U3/U4) contracted `mp search --local` and nothing else, and the
    //     `message.*` family is closed by `tests/daemon_read_slice.rs` and
    //     `tests/daemon_sync_slice.rs`.
    (
        "src/main.rs",
        "GraphClient::",
        "mp search (server leg): LST-06 is unmigrated, no message.search_server exists",
    ),
    (
        "src/main.rs",
        "GraphConfig::load",
        "mp search (server leg): LST-06 is unmigrated, no message.search_server exists",
    ),
    (
        "src/main.rs",
        "ImapConfig::load",
        "mp search (server leg): LST-06 is unmigrated, no message.search_server exists",
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
        "mp search (server leg): LST-06 is unmigrated, no message.search_server exists",
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
