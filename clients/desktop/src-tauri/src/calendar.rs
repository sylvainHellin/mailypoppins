//! The calendar agenda (CAL-02, CAL-03, #0131).
//!
//! `calendar.events` answers an account's agenda already deduped, folded and
//! sorted, so this layer passes it through; the past/upcoming filter is the
//! frontend's, as it is the TUI's. An entry's source is its `invite.ics`
//! blob, which `message.ics` answers as bytes: the TUI writes it to a temp
//! file and hands that to `$EDITOR` (`Action::OpenEventSource`), and this
//! writes it under the app's cache directory, readable by its owner only,
//! before the external editor opens it. Edits to that copy reach nothing:
//! the file is an artifact to read, not a draft.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tauri::{AppHandle, State};

use mp_client::queries;
use mp_protocol::calendar::AgendaEvent;

use crate::attachments::{cache_dir, write_private, RENDITIONS};
use crate::commands::with_door;
use crate::editor::{self, EditorLaunch, Lookup};
use crate::error::{Addressing, GuiError};
use crate::session::{Budgeted, Door, SessionHandle};

/// One agenda read: a store scan and the reply fold, daemon-side.
const AGENDA_BUDGET: Duration = Duration::from_secs(15);

/// One `invite.ics` blob read.
const ICS_BUDGET: Duration = Duration::from_secs(10);

/// What the TUI says when a row has no `invite.ics` (`Action::OpenEventSource`).
pub const NO_ICS: &str = "That event has no ics source in the store";

/// `account`'s agenda, every row: past events included, the filter is the
/// frontend's.
pub fn calendar_events_on(door: &Door, account: &str) -> Result<Vec<AgendaEvent>, GuiError> {
    let q = Budgeted {
        door,
        budget: AGENDA_BUDGET,
    };
    queries::calendar_events(&q, account).map_err(|e| GuiError::from_call(&e, Addressing::Resource))
}

/// Where row `row_id`'s `invite.ics` is written: one file per row, so the
/// same entry opened twice rewrites one file.
pub fn invite_source_path(cache_dir: &Path, account: &str, row_id: i64) -> PathBuf {
    let account: String = account
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    cache_dir
        .join(RENDITIONS)
        .join(format!("invite-{account}-{row_id}.ics"))
}

/// Write row `row_id`'s `invite.ics` under `cache_dir` (0700 directory, 0600
/// file) and answer its path; a row with no ics is [`NO_ICS`].
pub fn invite_source_write_on(
    door: &Door,
    cache_dir: &Path,
    account: &str,
    row_id: i64,
) -> Result<PathBuf, GuiError> {
    let q = Budgeted {
        door,
        budget: ICS_BUDGET,
    };
    let bytes = queries::message_ics(&q, account, row_id)
        .map_err(|e| GuiError::from_call(&e, Addressing::Resource))?
        .ok_or_else(|| GuiError::not_found(NO_ICS))?;
    let path = invite_source_path(cache_dir, account, row_id);
    write_private(cache_dir, &path, &bytes)?;
    Ok(path)
}

/// Open row `row_id`'s `invite.ics` in the external editor (the TUI's agenda
/// Enter and `e`); with a fixture door the editor is journaled, not run.
pub fn invite_source_open_on(
    door: &Door,
    cache_dir: &Path,
    lookup: &Lookup,
    account: &str,
    row_id: i64,
    window: Duration,
) -> Result<EditorLaunch, GuiError> {
    let path = invite_source_write_on(door, cache_dir, account, row_id)?;
    let fixture = match door {
        Door::Fixture(fixture) => Some(fixture.as_ref()),
        Door::Daemon(_) => None,
    };
    editor::open_on(fixture, lookup, &path.to_string_lossy(), window)
}

// ---------------------------------------------------------------------------
// The commands
// ---------------------------------------------------------------------------

#[tauri::command(rename_all = "snake_case")]
pub async fn calendar_events(
    session: State<'_, SessionHandle>,
    account: String,
) -> Result<Vec<AgendaEvent>, GuiError> {
    with_door(&session, move |_, door| calendar_events_on(door, &account)).await
}

/// `not_found` when the row has no `invite.ics`.
#[tauri::command(rename_all = "snake_case")]
pub async fn invite_source_open(
    app: AppHandle,
    session: State<'_, SessionHandle>,
    account: String,
    row_id: i64,
) -> Result<EditorLaunch, GuiError> {
    let settings = editor::settings_file(&app)?;
    with_door(&session, move |_, door| {
        let cache = cache_dir(&app, door)?;
        let setting = editor::read_setting(&settings).unwrap_or_else(|e| {
            tracing::warn!("[editor] ignoring the setting: {e}");
            None
        });
        invite_source_open_on(
            door,
            &cache,
            &editor::live_lookup(setting),
            &account,
            row_id,
            editor::EXIT_WINDOW,
        )
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::{Fixture, INVITE_ROW};
    use std::sync::Arc;

    fn fixture_door() -> (Door, Arc<Fixture>) {
        let (tx, rx) = std::sync::mpsc::channel();
        std::mem::forget(rx);
        let fixture = Arc::new(Fixture::load(tx).expect("fixture"));
        (Door::Fixture(Arc::clone(&fixture)), fixture)
    }

    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn any_file(p: &Path) -> bool {
        p.is_file()
    }

    fn lookup() -> Lookup<'static> {
        Lookup {
            env: &no_env,
            setting: Some("zed --wait".to_string()),
            is_file: &any_file,
            macos: true,
        }
    }

    #[test]
    fn calendar_events_on_decodes_the_fixture_agenda() {
        let (door, _f) = fixture_door();
        let work = calendar_events_on(&door, "work").expect("work");
        assert_eq!(work.len(), 5);
        let steering = work
            .iter()
            .find(|e| e.row_id == INVITE_ROW)
            .expect("row 1008");
        assert_eq!(
            steering.event.summary.as_deref(),
            Some("Steering committee")
        );
        assert_eq!(steering.event.method.as_deref(), Some("REQUEST"));
        assert_eq!(steering.event.attendees.len(), 3);
        assert_eq!(steering.start_display, "2099-10-14 10:00");
        assert!(matches!(
            calendar_events_on(&door, "nobody"),
            Err(GuiError::NotFound { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn invite_source_open_writes_a_private_file_and_journals_the_editor() {
        use std::os::unix::fs::PermissionsExt;
        let (door, f) = fixture_door();
        let cache = crate::test_support::scratch_dir("invite-source");
        let launch = invite_source_open_on(
            &door,
            &cache,
            &lookup(),
            "work",
            INVITE_ROW,
            Duration::from_millis(100),
        )
        .expect("opened");
        assert_eq!(launch.pid, None, "a fixture spawns nothing");
        let path = invite_source_path(&cache, "work", INVITE_ROW);
        assert!(
            path.ends_with("renditions/invite-work-1008.ics"),
            "{path:?}"
        );
        let text = std::fs::read_to_string(&path).expect("written");
        assert!(text.contains("SUMMARY:Steering committee"), "{text}");
        let mode = |p: &Path| std::fs::metadata(p).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(path.parent().expect("dir")), 0o700);
        let opens = f.editor_opens();
        assert_eq!(opens.len(), 1);
        assert_eq!(opens[0].path, path.to_string_lossy());
        assert_eq!(
            opens[0].command.last().map(String::as_str),
            Some(opens[0].path.as_str())
        );
    }

    #[test]
    fn a_row_without_an_ics_is_not_found_and_opens_nothing() {
        let (door, f) = fixture_door();
        let cache = crate::test_support::scratch_dir("invite-none");
        let err = invite_source_open_on(
            &door,
            &cache,
            &lookup(),
            "work",
            9104,
            Duration::from_millis(100),
        )
        .expect_err("no ics");
        assert_eq!(err, GuiError::not_found(NO_ICS));
        assert!(f.editor_opens().is_empty());
        assert!(!invite_source_path(&cache, "work", 9104).exists());
        assert!(matches!(
            invite_source_open_on(
                &door,
                &cache,
                &lookup(),
                "work",
                424_242,
                Duration::from_millis(100)
            ),
            Err(GuiError::NotFound { .. })
        ));
    }

    #[test]
    fn an_account_name_cannot_leave_the_renditions_directory() {
        let path = invite_source_path(Path::new("/c"), "../x y", 7);
        assert_eq!(path, Path::new("/c/renditions/invite-___x_y-7.ics"));
    }
}
