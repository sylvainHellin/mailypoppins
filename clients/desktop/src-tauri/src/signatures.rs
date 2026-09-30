//! Signatures management (ACC-10, #0131): the TUI's `cs` overlay.
//!
//! The daemon serves no `signature.*` method. A signature is a file,
//! `config_dir()/signatures/<name>.md`, and the account's default lives in
//! the app state file, so over a daemon these commands call
//! `mp_core::signatures` themselves, as the TUI does and as
//! [`crate::commands::signature_list_on`] already did for the compose
//! wizard; the desktop's config dir is the daemon's (the handshake's
//! `Identity.config_dir`). A refusal is `mp_core`'s own sentence (a name
//! [`mp_core::signatures::validate_name`] refuses, a name already taken, a
//! name that names nothing) as a `protocol` error, or `not_found` for the
//! last. The fixture answers the same calls as pseudo-methods
//! ([`crate::fixture::FIXTURE_ONLY_METHODS`]) with the same sentences.
//!
//! No command writes a signature's content: that is the editor's, through
//! `editor_open` on [`SignatureFile::path`]. The daemon's watcher publishes
//! `signature.changed` for a file written or created; nothing reports a
//! delete (no `signature.removed`), so the dialog reads the listing again
//! after each of its own changes.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::State;

use crate::commands::{decode, with_door, SignatureListing};
use crate::error::{refusal_sentence, rpc_code, Addressing, GuiError};
use crate::session::{Door, SessionHandle};

/// One signature call: a file read or written, or the fixture's memory.
const SIGNATURE_BUDGET: Duration = Duration::from_secs(10);

/// One signature: its name, its file and what the file holds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
pub struct SignatureFile {
    /// The file stem, which is the name the compose wizard offers.
    pub name: String,
    /// The absolute path of the file, what the editor opens.
    pub path: String,
    /// The raw Markdown; empty for a signature just created.
    pub content: String,
}

/// A sentence of `mp_core::signatures` as the webview gets it: a name that
/// names no signature is `not_found`, every other refusal `protocol`.
fn refusal(sentence: &str) -> GuiError {
    if sentence.starts_with("no signature named") {
        GuiError::not_found(sentence)
    } else {
        GuiError::protocol(sentence)
    }
}

/// An `mp_core::signatures` error, with its context chain.
fn core_error(e: anyhow::Error) -> GuiError {
    refusal(&format!("{e:#}"))
}

/// A fixture pseudo-method: its `-32602` refusal carries `mp_core`'s
/// sentence, which reaches the webview as the daemon door's does.
fn fixture_call<T: serde::de::DeserializeOwned>(
    door: &Door,
    method: &str,
    params: Value,
) -> Result<T, GuiError> {
    let answer = door
        .call_within(method, params, SIGNATURE_BUDGET)
        .map_err(|e| {
            let text = format!("{e:#}");
            match (rpc_code(&text), refusal_sentence(&text)) {
                (Some(-32602), Some(sentence)) => refusal(sentence),
                _ => GuiError::from_call_text(&text, Addressing::Params),
            }
        })?;
    decode(method, answer)
}

/// The listing of `account` as `mp_core` reads it now.
fn core_listing(account: &str) -> SignatureListing {
    SignatureListing {
        account: account.to_string(),
        names: mp_core::signatures::list(),
        default: mp_core::signatures::default_signature_name(account),
    }
}

fn core_file(name: &str, content: String) -> SignatureFile {
    SignatureFile {
        name: name.to_string(),
        path: mp_core::signatures::signature_file(name)
            .display()
            .to_string(),
        content,
    }
}

/// The signature `name` and its content.
pub fn signature_read_on(door: &Door, name: &str) -> Result<SignatureFile, GuiError> {
    match door {
        Door::Fixture(_) => fixture_call(door, "signature.read", json!({"name": name})),
        Door::Daemon(_) => {
            mp_core::signatures::validate_name(name).map_err(core_error)?;
            let content = mp_core::signatures::read(name)
                .ok_or_else(|| refusal(&format!("no signature named '{name}'")))?;
            Ok(core_file(name, content))
        }
    }
}

/// A new, empty signature `name`; a name already taken is refused, so a
/// create never blanks a signature. The frontend opens its file next.
pub fn signature_create_on(door: &Door, name: &str) -> Result<SignatureFile, GuiError> {
    match door {
        Door::Fixture(_) => fixture_call(door, "signature.create", json!({"name": name})),
        Door::Daemon(_) => {
            mp_core::signatures::create(name).map_err(core_error)?;
            Ok(core_file(name, String::new()))
        }
    }
}

/// Rename `old` to `new`, carrying every account default that named it;
/// answers `account`'s listing after it.
pub fn signature_rename_on(
    door: &Door,
    account: &str,
    old: &str,
    new: &str,
) -> Result<SignatureListing, GuiError> {
    match door {
        Door::Fixture(_) => fixture_call(
            door,
            "signature.rename",
            json!({"account": account, "old": old, "new": new}),
        ),
        Door::Daemon(_) => {
            mp_core::signatures::rename(old, new).map_err(core_error)?;
            Ok(core_listing(account))
        }
    }
}

/// Delete `name` and clear every account default that named it; answers
/// `account`'s listing after it.
pub fn signature_delete_on(
    door: &Door,
    account: &str,
    name: &str,
) -> Result<SignatureListing, GuiError> {
    match door {
        Door::Fixture(_) => fixture_call(
            door,
            "signature.delete",
            json!({"account": account, "name": name}),
        ),
        Door::Daemon(_) => {
            mp_core::signatures::delete(name).map_err(core_error)?;
            Ok(core_listing(account))
        }
    }
}

/// Make `name` the default of `account`, or clear it with `None`; answers
/// `account`'s listing after it.
pub fn signature_set_default_on(
    door: &Door,
    account: &str,
    name: Option<&str>,
) -> Result<SignatureListing, GuiError> {
    match door {
        Door::Fixture(_) => fixture_call(
            door,
            "signature.set_default",
            json!({"account": account, "name": name}),
        ),
        Door::Daemon(_) => {
            mp_core::signatures::set_default_signature(account, name).map_err(core_error)?;
            Ok(core_listing(account))
        }
    }
}

// ---------------------------------------------------------------------------
// Tauri commands
// ---------------------------------------------------------------------------

#[tauri::command(rename_all = "snake_case")]
pub async fn signature_read(
    session: State<'_, SessionHandle>,
    name: String,
) -> Result<SignatureFile, GuiError> {
    with_door(&session, move |_, door| signature_read_on(door, &name)).await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn signature_create(
    session: State<'_, SessionHandle>,
    name: String,
) -> Result<SignatureFile, GuiError> {
    with_door(&session, move |_, door| signature_create_on(door, &name)).await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn signature_rename(
    session: State<'_, SessionHandle>,
    account: String,
    old: String,
    new: String,
) -> Result<SignatureListing, GuiError> {
    with_door(&session, move |_, door| {
        signature_rename_on(door, &account, &old, &new)
    })
    .await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn signature_delete(
    session: State<'_, SessionHandle>,
    account: String,
    name: String,
) -> Result<SignatureListing, GuiError> {
    with_door(&session, move |_, door| {
        signature_delete_on(door, &account, &name)
    })
    .await
}

/// `name` absent or null clears the default.
#[tauri::command(rename_all = "snake_case")]
pub async fn signature_set_default(
    session: State<'_, SessionHandle>,
    account: String,
    name: Option<String>,
) -> Result<SignatureListing, GuiError> {
    with_door(&session, move |_, door| {
        signature_set_default_on(door, &account, name.as_deref())
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::signature_list_on;
    use crate::fixture::{Fixture, SIGNATURE_EDIT_LINE};
    use mp_client::events::Incoming;
    use std::sync::mpsc::Receiver;
    use std::sync::Arc;

    fn fixture_door() -> (Door, Arc<Fixture>, Receiver<Incoming>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let fixture = Arc::new(Fixture::load(tx).expect("fixture"));
        (Door::Fixture(Arc::clone(&fixture)), fixture, rx)
    }

    /// The `(kind, payload)` of every event posted so far.
    fn drained(rx: &Receiver<Incoming>) -> Vec<(String, Value)> {
        std::iter::from_fn(|| rx.try_recv().ok())
            .filter_map(|i| match i {
                Incoming::Event(e) => Some((e.kind, e.payload)),
                _ => None,
            })
            .collect()
    }

    /// What `mp_core::signatures::validate_name` says of `name`.
    fn core_sentence(name: &str) -> String {
        format!(
            "{:#}",
            mp_core::signatures::validate_name(name).expect_err("refused")
        )
    }

    #[test]
    fn a_bad_name_is_refused_with_mp_cores_sentence() {
        let (d, _f, _rx) = fixture_door();
        for bad in ["../evil", "", ".hidden", "a/b", "semi;colon"] {
            let want = core_sentence(bad);
            for got in [
                signature_create_on(&d, bad),
                signature_read_on(&d, bad),
                signature_rename_on(&d, "work", "work", bad).map(|l| SignatureFile {
                    name: l.account,
                    path: String::new(),
                    content: String::new(),
                }),
            ] {
                match got {
                    Err(GuiError::Protocol { message, code }) => {
                        assert_eq!(message, want, "{bad:?}");
                        assert_eq!(code, None);
                    }
                    other => panic!("{bad:?}: {other:?}"),
                }
            }
        }
    }

    #[test]
    fn read_answers_the_file_and_a_missing_name_is_not_found() {
        let (d, f, _rx) = fixture_door();
        let work = signature_read_on(&d, "work").expect("read");
        assert_eq!(work.name, "work");
        assert_eq!(work.content, "Kind regards,\nMe\nFixture GmbH");
        assert!(work.path.ends_with("signatures/work.md"));
        assert!(work.path.starts_with(&f.root().display().to_string()));
        assert_eq!(
            std::fs::read_to_string(&work.path).expect("mirrored"),
            work.content
        );
        match signature_read_on(&d, "missing") {
            Err(GuiError::NotFound { message, .. }) => {
                assert_eq!(message, "no signature named 'missing'")
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn create_writes_an_empty_file_and_refuses_a_name_taken() {
        let (d, _f, rx) = fixture_door();
        let fresh = signature_create_on(&d, "fresh start").expect("created");
        assert_eq!(fresh.name, "fresh start");
        assert_eq!(fresh.content, "");
        assert_eq!(std::fs::read_to_string(&fresh.path).expect("file"), "");
        let events = drained(&rx);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, "signature.changed");
        assert_eq!(events[0].1["name"], "fresh start");
        assert_eq!(events[0].1["path"], fresh.path.as_str());
        assert_eq!(
            signature_list_on(&d, "work").expect("list").names,
            ["fresh start", "short", "work"]
        );
        match signature_create_on(&d, "work") {
            Err(GuiError::Protocol { message, .. }) => {
                assert_eq!(message, "a signature named 'work' already exists")
            }
            other => panic!("{other:?}"),
        }
        assert!(drained(&rx).is_empty(), "a refusal publishes nothing");
        assert_eq!(
            signature_read_on(&d, "work").expect("kept").content,
            "Kind regards,\nMe\nFixture GmbH",
            "a create never blanks a signature"
        );
    }

    #[test]
    fn rename_carries_the_default_and_moves_the_file() {
        let (d, _f, rx) = fixture_door();
        let old_path = signature_read_on(&d, "work").expect("read").path;
        let listing = signature_rename_on(&d, "work", "work", "office").expect("renamed");
        assert_eq!(listing.account, "work");
        assert_eq!(listing.names, ["office", "short"]);
        assert_eq!(listing.default.as_deref(), Some("office"));
        let office = signature_read_on(&d, "office").expect("moved");
        assert_eq!(office.content, "Kind regards,\nMe\nFixture GmbH");
        assert!(std::path::Path::new(&office.path).is_file());
        assert!(!std::path::Path::new(&old_path).exists());
        let events = drained(&rx);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, "signature.changed");
        assert_eq!(events[0].1["name"], "office");
        // The other account's listing names the same files and its own default.
        assert_eq!(signature_list_on(&d, "home").expect("home").default, None);
        match signature_rename_on(&d, "work", "short", "office") {
            Err(GuiError::Protocol { message, .. }) => {
                assert_eq!(message, "a signature named 'office' already exists")
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            signature_rename_on(&d, "work", "gone", "other"),
            Err(GuiError::NotFound { .. })
        ));
        // A rename to itself is nothing, as `mp_core`'s.
        let same = signature_rename_on(&d, "work", "short", "short").expect("same");
        assert_eq!(same.names, ["office", "short"]);
        assert!(drained(&rx).is_empty());
    }

    #[test]
    fn deleting_the_default_clears_it_and_publishes_nothing() {
        let (d, _f, rx) = fixture_door();
        let path = signature_read_on(&d, "work").expect("read").path;
        let listing = signature_delete_on(&d, "work", "work").expect("deleted");
        assert_eq!(listing.names, ["short"]);
        assert_eq!(listing.default, None);
        assert!(!std::path::Path::new(&path).exists());
        assert!(drained(&rx).is_empty(), "no signature.removed exists");
        assert!(matches!(
            signature_delete_on(&d, "work", "work"),
            Err(GuiError::NotFound { .. })
        ));
    }

    #[test]
    fn set_default_sets_and_clears_the_accounts_default() {
        let (d, _f, _rx) = fixture_door();
        let set = signature_set_default_on(&d, "home", Some("short")).expect("set");
        assert_eq!(set.account, "home");
        assert_eq!(set.default.as_deref(), Some("short"));
        assert_eq!(
            signature_list_on(&d, "work")
                .expect("work")
                .default
                .as_deref(),
            Some("work"),
            "per account"
        );
        let cleared = signature_set_default_on(&d, "work", None).expect("cleared");
        assert_eq!(cleared.default, None);
        assert!(matches!(
            signature_set_default_on(&d, "work", Some("missing")),
            Err(GuiError::NotFound { .. })
        ));
        assert!(matches!(
            signature_set_default_on(&d, "nobody", None),
            Err(GuiError::NotFound { .. })
        ));
    }

    #[test]
    fn the_signature_changed_simulation_edits_work_and_says_so() {
        let (d, f, rx) = fixture_door();
        f.simulate("signature_changed").expect("simulated");
        let work = signature_read_on(&d, "work").expect("read");
        assert!(work.content.ends_with(SIGNATURE_EDIT_LINE));
        assert_eq!(
            std::fs::read_to_string(&work.path).expect("file"),
            work.content
        );
        let events = drained(&rx);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, "signature.changed");
        assert_eq!(events[0].1["name"], "work");
        assert_eq!(events[0].1["path"], work.path.as_str());
    }

    #[test]
    fn a_refusal_names_the_sentence_only() {
        assert!(matches!(
            refusal("no signature named 'x'"),
            GuiError::NotFound { .. }
        ));
        assert!(matches!(
            core_error(anyhow::anyhow!("a signature named 'x' already exists")),
            GuiError::Protocol { code: None, .. }
        ));
    }
}
