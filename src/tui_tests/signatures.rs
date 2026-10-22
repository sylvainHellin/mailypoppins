//! The TUI's signatures overlay (`cs`, #0107) on the daemon's `signature.*`
//! family (the daemon leftovers, 2026-10-22).
//!
//! The overlay used to call `mp_core::signatures` itself; every listing and
//! every change is a daemon call now, so these run against an in-process
//! daemon ([`TestDaemon`]) over a fixture config directory, and the files the
//! daemon writes are checked on disk.

use crossterm::event::{KeyCode, KeyEvent};

use super::daemon::TestDaemon;
use crate::tui::app::{Action, App, Overlay, SignaturesMode};

/// Both path layers redirected into one tempdir, armed before the daemon is
/// built so its threads inherit them: the config directory for the signature
/// files, the data directory for the state file that records the default.
struct Fixture {
    _config: crate::config::test_env::ConfigDirOverride,
    _data: crate::config::test_env::TestDataDir,
    _dir: tempfile::TempDir,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = crate::config::test_env::ConfigDirOverride::new(dir.path());
    config.set_config_dir(&dir.path().join("config"));
    Fixture {
        _data: crate::config::test_env::TestDataDir::new(),
        _config: config,
        _dir: dir,
    }
}

/// An app on account `work`, holding a daemon session, with `names` on disk.
fn app_with_signatures(names: &[&str]) -> App {
    for name in names {
        crate::signatures::write(name, &format!("-- \n{name}")).expect("seed a signature");
    }
    let mut app = App::default_for_tests();
    app.account_config.name = "work".to_string();
    app.session = Some(TestDaemon::new(&["work", "home"]).session());
    app
}

fn press(app: &mut App, c: char) {
    app.handle_key(KeyEvent::from(KeyCode::Char(c)));
}

fn key(app: &mut App, code: KeyCode) {
    app.handle_key(KeyEvent::from(code));
}

fn type_text(app: &mut App, text: &str) {
    for c in text.chars() {
        press(app, c);
    }
}

fn open(app: &mut App) {
    press(app, 'c');
    press(app, 's');
}

fn overlay(app: &mut App) -> &mut crate::tui::app::SignaturesOverlay {
    app.signatures_overlay_mut()
        .expect("the signatures overlay")
}

/// `cs` lists the daemon's signatures for the account, `Esc` closes it.
#[test]
fn cs_lists_the_daemons_signatures_and_esc_closes_it() {
    let _fx = fixture();
    let mut app = app_with_signatures(&["work", "casual"]);
    open(&mut app);
    let listed = overlay(&mut app);
    assert_eq!(listed.names, ["casual", "work"]);
    assert_eq!(listed.account, "work");
    assert_eq!(listed.default, None);
    key(&mut app, KeyCode::Esc);
    assert!(matches!(app.overlay, Overlay::None));
}

/// The overlay opens on an empty directory too: it is where the first
/// signature gets created.
#[test]
fn the_overlay_opens_with_no_signatures_at_all() {
    let _fx = fixture();
    let mut app = app_with_signatures(&[]);
    open(&mut app);
    assert!(overlay(&mut app).names.is_empty());
    assert_eq!(overlay(&mut app).selected_name(), None);
}

/// Enter records the cursor signature as the account default through
/// `signature.set_default`; Enter on the one that already is clears it.
#[test]
fn enter_sets_and_then_clears_the_account_default() {
    let _fx = fixture();
    let mut app = app_with_signatures(&["casual", "work"]);
    open(&mut app);
    press(&mut app, 'j');
    key(&mut app, KeyCode::Enter);
    assert_eq!(
        crate::signatures::default_signature_name("work").as_deref(),
        Some("work")
    );
    assert_eq!(overlay(&mut app).default.as_deref(), Some("work"));
    key(&mut app, KeyCode::Enter);
    assert_eq!(crate::signatures::default_signature_name("work"), None);
    assert_eq!(overlay(&mut app).default, None);
}

/// `n` creates the file through `signature.create`, puts the cursor on it and
/// queues `$EDITOR`.
#[test]
fn n_creates_a_signature_and_opens_it_in_the_editor() {
    let _fx = fixture();
    let mut app = app_with_signatures(&["work"]);
    open(&mut app);
    press(&mut app, 'n');
    assert_eq!(overlay(&mut app).mode, SignaturesMode::New);
    type_text(&mut app, "casual");
    key(&mut app, KeyCode::Enter);

    assert!(crate::signatures::exists("casual"));
    assert_eq!(overlay(&mut app).mode, SignaturesMode::Browse);
    assert_eq!(overlay(&mut app).selected_name(), Some("casual"));
    match app.pending_actions.pop_front() {
        Some(Action::EditSignatureFile { name }) => assert_eq!(name, "casual"),
        other => panic!("expected EditSignatureFile, got {other:?}"),
    }
}

/// A name the daemon refuses is reported in its words and leaves the prompt
/// open with the text intact.
#[test]
fn an_invalid_new_name_is_reported_and_keeps_the_prompt_open() {
    let _fx = fixture();
    let mut app = app_with_signatures(&["work"]);
    open(&mut app);
    press(&mut app, 'n');
    type_text(&mut app, "../evil");
    key(&mut app, KeyCode::Enter);

    let status = app.status_message.clone().unwrap_or_default();
    assert!(status.starts_with("Cannot create"), "{status}");
    assert!(status.contains("cannot start with a dot"), "{status}");
    assert_eq!(overlay(&mut app).mode, SignaturesMode::New);
    assert_eq!(overlay(&mut app).input, "../evil");
    assert_eq!(crate::signatures::list(), ["work"]);
    assert!(app.pending_actions.is_empty());
}

/// `r` renames through `signature.rename`, and the default follows.
#[test]
fn r_renames_the_selected_signature_and_carries_the_default() {
    let _fx = fixture();
    let mut app = app_with_signatures(&["work"]);
    crate::signatures::set_default_signature("work", Some("work")).expect("default");
    open(&mut app);
    press(&mut app, 'r');
    assert_eq!(overlay(&mut app).input, "work");
    type_text(&mut app, "-external");
    key(&mut app, KeyCode::Enter);

    assert_eq!(crate::signatures::list(), ["work-external"]);
    assert_eq!(
        crate::signatures::default_signature_name("work").as_deref(),
        Some("work-external")
    );
    let renamed = overlay(&mut app);
    assert_eq!(renamed.mode, SignaturesMode::Browse);
    assert_eq!(renamed.selected_name(), Some("work-external"));
    assert_eq!(renamed.default.as_deref(), Some("work-external"));
}

/// A rename onto an existing name is refused in the daemon's words, and the
/// prompt stays open.
#[test]
fn a_rename_onto_an_existing_name_is_refused() {
    let _fx = fixture();
    let mut app = app_with_signatures(&["casual", "work"]);
    open(&mut app);
    press(&mut app, 'r');
    for _ in 0.."casual".len() {
        key(&mut app, KeyCode::Backspace);
    }
    type_text(&mut app, "work");
    key(&mut app, KeyCode::Enter);

    let status = app.status_message.clone().unwrap_or_default();
    assert!(
        status.contains("a signature named 'work' already exists"),
        "{status}"
    );
    assert_eq!(crate::signatures::list(), ["casual", "work"]);
    assert_eq!(overlay(&mut app).mode, SignaturesMode::Rename);
}

/// `d` asks first with the file the daemon names, and `y` deletes it through
/// `signature.delete`, clearing a default that named it.
#[test]
fn d_deletes_behind_the_confirm_dialog_and_clears_the_default() {
    let _fx = fixture();
    let mut app = app_with_signatures(&["casual", "work"]);
    crate::signatures::set_default_signature("work", Some("work")).expect("default");
    open(&mut app);
    press(&mut app, 'j');
    press(&mut app, 'd');
    let Overlay::Confirm(dialog) = &app.overlay else {
        panic!("`d` should raise the confirm dialog");
    };
    assert!(dialog.title.contains("work"), "{}", dialog.title);
    assert_eq!(
        dialog.detail,
        crate::signatures::signature_file("work")
            .display()
            .to_string()
    );

    press(&mut app, 'y');
    assert!(!crate::signatures::exists("work"));
    assert_eq!(crate::signatures::default_signature_name("work"), None);
    let Overlay::Signatures(listed) = &app.overlay else {
        panic!("the list comes back after the delete");
    };
    assert_eq!(listed.names, ["casual"]);
    assert_eq!(listed.default, None);
    assert_eq!(listed.selected_name(), Some("casual"));
}

/// Answering `n` to the delete keeps the file and returns to the list.
#[test]
fn a_cancelled_signature_delete_returns_to_the_list() {
    let _fx = fixture();
    let mut app = app_with_signatures(&["casual", "work"]);
    open(&mut app);
    press(&mut app, 'd');
    press(&mut app, 'n');
    assert!(crate::signatures::exists("casual"));
    assert_eq!(overlay(&mut app).names, ["casual", "work"]);
}

/// A compose wizard's signature block is the daemon's file, the named one or
/// the account's default, through the draft writers' Markdown conversion.
#[test]
fn a_compose_wizard_takes_its_signature_block_from_the_daemon() {
    let _fx = fixture();
    let app = app_with_signatures(&["work", "casual"]);
    crate::signatures::set_default_signature("work", Some("casual")).expect("default");
    let named = app
        .compose_signature_markdown(Some("work"))
        .expect("a block");
    assert!(named.contains("work"), "{named}");
    let default = app
        .compose_signature_markdown(None)
        .expect("the default's block");
    assert!(default.contains("casual"), "{default}");
    assert_eq!(app.compose_signature_markdown(Some("missing")), None);
}

/// Another client's delete reaches an open overlay: the watcher's
/// `signature.removed` makes it ask the daemon again.
#[test]
fn a_signature_removed_event_relists_the_open_overlay() {
    let _fx = fixture();
    let mut app = app_with_signatures(&["casual", "work"]);
    app.apply_bootstrap(&mp_protocol::state::Bootstrap {
        instance_id: "tui-test".to_string(),
        revision: 98,
        ..Default::default()
    });
    open(&mut app);
    crate::signatures::delete("work").expect("another client deletes");
    let event = mp_protocol::EventEnvelope {
        instance_id: "tui-test".to_string(),
        revision: 99,
        kind: mp_protocol::events::KIND_SIGNATURE_REMOVED.to_string(),
        payload: serde_json::json!({"name": "work"}),
    };
    app.apply_event(&event);
    assert_eq!(overlay(&mut app).names, ["casual"]);
}
