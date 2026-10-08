//! TypeScript bindings for the desktop client, generated with ts-rs.
//!
//! Every wire type derives `ts_rs::TS` behind the `ts` feature, and the
//! committed bindings live in `clients/desktop/src/protocol/generated/`.
//! `export_bindings` rewrites them (`pnpm gen:types` in `clients/desktop`);
//! `generated_bindings_are_current` fails when a wire type changed and they
//! were not regenerated.
//!
//! The export goes through an explicit [`Config`] rather than `#[ts(export)]`:
//! the tests `#[ts(export)]` generates read `TS_RS_LARGE_INT` and
//! `TS_RS_EXPORT_DIR` from the environment and default to `bigint` and
//! `./bindings`, and `serde_json` hands a `u64` to JavaScript as a `number`.
#![cfg(feature = "ts")]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use mp_protocol::{
    calendar, diagnostic, draft, events, listing, operation, rendition, send, state,
};
use ts_rs::{Config, TS};

/// Exports each listed type, and its dependencies, into `$cfg`'s directory.
macro_rules! export {
    ($cfg:expr; $($ty:ty),* $(,)?) => {{
        let mut count = 0usize;
        $(
            <$ty as TS>::export_all($cfg).expect(concat!("export ", stringify!($ty)));
            count += 1;
        )*
        count
    }};
}

/// The command a developer runs when the bindings are stale.
const REGENERATE: &str = "cd clients/desktop && pnpm gen:types";

fn committed_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../clients/desktop/src/protocol/generated")
}

fn config(dir: &Path) -> Config {
    Config::new().with_large_int("number").with_out_dir(dir)
}

/// Writes every wire type into `dir` and answers how many types were listed.
fn export_into(dir: &Path) -> usize {
    let cfg = config(dir);
    let cfg = &cfg;
    export!(cfg;
        calendar::EventAttendee,
        calendar::EventFrontmatter,
        calendar::AgendaEvent,
        diagnostic::CheckStatus,
        diagnostic::HealthCheck,
        draft::DraftSource,
        draft::DraftCreated,
        draft::DraftEntry,
        draft::DraftSkip,
        draft::DraftCollision,
        draft::DraftListing,
        draft::DraftReport,
        draft::DraftValidation,
        draft::DraftLocation,
        draft::DraftPreview,
        draft::DraftKind,
        draft::DraftMessage,
        events::Arrival,
        events::Severity,
        events::SyncCompleted,
        events::MutationsRolledBack,
        events::ConfigChanged,
        events::Diagnostic,
        events::DraftChanged,
        events::DraftInvalid,
        events::SignatureChanged,
        events::ConfigInvalid,
        listing::MessageFlags,
        listing::MessageListRow,
        listing::MessageListing,
        listing::MessageListStreamStarted,
        listing::MessageRowsChunk,
        listing::ThreadMessage,
        listing::ThreadListing,
        listing::ServerSearchHit,
        mp_protocol::RequestId,
        mp_protocol::Request,
        mp_protocol::Response,
        mp_protocol::ErrorResponse,
        mp_protocol::RpcError,
        mp_protocol::Notification,
        mp_protocol::EventEnvelope,
        operation::OperationState,
        operation::CancelScope,
        operation::Progress,
        operation::OperationStatus,
        rendition::MessageHtmlParams,
        rendition::MessageHtml,
        rendition::InlineHtmlRefusal,
        send::RecipientOutcome,
        send::SentCopy,
        send::SendOutcome,
        send::ApprovedOutcome,
        send::OutboxRow,
        send::OutboxCounts,
        send::OutboxListing,
        send::HoldStatus,
        send::HoldListing,
        send::OutboxRetryOutcome,
        state::AccountState,
        state::SyncHealthState,
        state::SyncHealth,
        state::AccountSnapshot,
        state::MailboxRow,
        state::DraftRow,
        state::OutboxCounts,
        state::Snapshot,
        state::Bootstrap,
    )
}

/// The `.ts` files directly in `dir`, by name. The `gui/` subfolder belongs
/// to the desktop crate's own export and is left out.
fn read_bindings(dir: &Path) -> BTreeMap<String, String> {
    let Ok(entries) = fs::read_dir(dir) else {
        return BTreeMap::new();
    };
    entries
        .map(|e| e.expect("read_dir entry").path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "ts"))
        .map(|p| {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            (name, fs::read_to_string(&p).expect("read binding"))
        })
        .collect()
}

/// A fresh scratch directory, unique to this process and `purpose`.
fn scratch(purpose: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "mp-protocol-ts-{purpose}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// Every binding, generated fresh, with the trailing space ts-rs leaves after
/// each `{` trimmed so the committed files pass `git diff --check`, plus an
/// `index.ts` that re-exports them all.
fn generate() -> BTreeMap<String, String> {
    let dir = scratch("gen");
    export_into(&dir);
    let mut bindings = read_bindings(&dir);
    let index: String = std::iter::once(
        "// Generated by crates/mp-protocol/tests/ts_bindings.rs. Do not edit this file manually.\n"
            .to_owned(),
    )
    .chain(bindings.keys().map(|name| {
        let stem = name.trim_end_matches(".ts");
        format!("export type {{ {stem} }} from \"./{stem}\";\n")
    }))
    .collect();
    bindings.insert("index.ts".to_owned(), index);
    bindings
        .into_iter()
        .map(|(name, body)| {
            let mut trimmed: String = body
                .lines()
                .map(|l| l.trim_end().to_owned() + "\n")
                .collect();
            trimmed.truncate(trimmed.trim_end().len());
            trimmed.push('\n');
            (name, trimmed)
        })
        .collect()
}

/// Rewrites the committed bindings. Ignored in a plain `cargo test` because it
/// writes into the source tree; `pnpm gen:types` runs it.
#[test]
#[ignore = "writes clients/desktop/src/protocol/generated; run it through `pnpm gen:types`"]
fn export_bindings() {
    let fresh = generate();
    let target = committed_dir();
    fs::create_dir_all(&target).expect("create generated dir");
    for name in read_bindings(&target).keys() {
        if !fresh.contains_key(name) {
            fs::remove_file(target.join(name)).expect("remove a binding no type exports");
        }
    }
    for (name, body) in &fresh {
        fs::write(target.join(name), body).expect("write binding");
    }
}

#[test]
fn generated_bindings_are_current() {
    let fresh = generate();
    let committed = read_bindings(&committed_dir());
    let stale: Vec<&String> = fresh
        .iter()
        .filter(|(name, body)| committed.get(*name) != Some(body))
        .map(|(name, _)| name)
        .collect();
    let orphaned: Vec<&String> = committed
        .keys()
        .filter(|n| !fresh.contains_key(*n))
        .collect();
    assert!(
        stale.is_empty() && orphaned.is_empty(),
        "the TypeScript bindings in clients/desktop/src/protocol/generated are stale \
         (missing or changed: {stale:?}; no longer exported: {orphaned:?}); run `{REGENERATE}`"
    );
}

/// A new wire type that derives `TS` and is not listed in [`export_into`]
/// would only be exported if another listed type happened to reference it.
#[test]
fn every_ts_type_is_listed() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let derived: usize = fs::read_dir(&src)
        .expect("read src")
        .map(|e| e.expect("read_dir entry").path())
        .filter(|p| p.extension().is_some_and(|x| x == "rs"))
        .map(|p| {
            fs::read_to_string(&p)
                .expect("read source")
                .matches("derive(ts_rs::TS)")
                .count()
        })
        .sum();
    let listed = export_into(&scratch("count"));
    assert_eq!(
        derived, listed,
        "{derived} types derive ts_rs::TS and {listed} are listed in export_into"
    );
}

/// Two types with one TypeScript name would overwrite each other's file.
#[test]
fn every_type_gets_its_own_file() {
    let listed = export_into(&scratch("files"));
    let files = generate().keys().filter(|n| *n != "index.ts").count();
    assert_eq!(files, listed);
}
