//! App-managed signature files (#0107).
//!
//! A signature is one Markdown file at `config_dir()/signatures/<name>.md`.
//! The file stem is both the key and the display name, so the filesystem is
//! the whole index: there is nothing to keep in sync, and creating a signature
//! is creating a file.
//!
//! The per-account *selection* of a default lives in [`crate::app_state`],
//! not in `config.toml`: content is content, and which one an account starts
//! with is app state the TUI rewrites without asking.
//!
//! Before #0107 both halves lived in `[accounts.signatures.*]`, inline `text`
//! or a `path` to a file. [`migrate_config_signatures`] copies such tables into
//! this layer once and then tells the user the tables are dead; it never
//! rewrites `config.toml` (the #0022 precedent: warn, do not edit the user's
//! file).

use anyhow::{bail, Context, Result};
use colored::*;
use std::fs;
use std::path::{Path, PathBuf};

use crate::app_state::AppState;
use crate::config::GlobalConfig;

/// Longest signature name we accept. Long enough for "work-external-german",
/// short enough that the name plus `.md` is a sane filename everywhere.
pub const MAX_NAME_LEN: usize = 64;

/// `config_dir()/signatures/`.
pub fn signatures_dir() -> PathBuf {
    crate::config::config_dir().join("signatures")
}

/// The file a named signature lives in. The name must have passed
/// [`validate_name`]; callers that take a name from the user validate first.
pub fn signature_file(name: &str) -> PathBuf {
    signatures_dir().join(format!("{name}.md"))
}

/// Reject a name that is not a safe, unambiguous file stem.
///
/// Rejecting beats sanitising: a silently rewritten name means the user asks
/// for `../evil` and gets `evil`, and then cannot find the file they think
/// they made. The rules are the file-system ones (no separators, no `..`, no
/// leading dot so the file is never hidden) plus a conservative charset, since
/// the name is also displayed in the TUI and typed on a shell.
pub fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("a signature name cannot be empty");
    }
    if name.len() > MAX_NAME_LEN {
        bail!("signature name '{name}' is longer than {MAX_NAME_LEN} characters");
    }
    if name != name.trim() {
        bail!("signature name '{name}' has leading or trailing whitespace");
    }
    if name.starts_with('.') {
        bail!("signature name '{name}' cannot start with a dot");
    }
    if name.contains("..") {
        bail!("signature name '{name}' cannot contain '..'");
    }
    if name.contains('/') || name.contains('\\') {
        bail!("signature name '{name}' cannot contain a path separator");
    }
    if let Some(bad) = name
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ' ')))
    {
        bail!(
            "signature name '{name}' contains '{bad}'; use letters, digits, '-', '_', '.' or spaces"
        );
    }
    Ok(())
}

/// Every signature name, sorted. Empty when the directory does not exist.
///
/// Files whose stem would not pass [`validate_name`] are skipped rather than
/// listed: they cannot have been created through this module, and offering a
/// name the rest of the API refuses is worse than not showing it.
pub fn list() -> Vec<String> {
    let dir = signatures_dir();
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|e| e.path().is_file())
        .filter_map(|e| {
            let path = e.path();
            if path.extension().and_then(|x| x.to_str()) != Some("md") {
                return None;
            }
            let stem = path.file_stem()?.to_str()?.to_string();
            validate_name(&stem).ok().map(|()| stem)
        })
        .collect();
    names.sort();
    names
}

/// Whether a signature file exists under this name.
pub fn exists(name: &str) -> bool {
    validate_name(name).is_ok() && signature_file(name).is_file()
}

/// The raw Markdown of a signature, or `None` when it does not exist (or the
/// name is not one we would ever have written).
pub fn read(name: &str) -> Option<String> {
    validate_name(name).ok()?;
    fs::read_to_string(signature_file(name)).ok()
}

/// Write a signature's content, creating the directory and the file as needed.
pub fn write(name: &str, content: &str) -> Result<()> {
    validate_name(name)?;
    let dir = signatures_dir();
    fs::create_dir_all(&dir)
        .with_context(|| format!("creating signatures directory at {}", dir.display()))?;
    let path = signature_file(name);
    fs::write(&path, content)
        .with_context(|| format!("writing signature at {}", path.display()))?;
    Ok(())
}

/// Create a new, empty signature and return its file path.
///
/// Fails when one of that name already exists: creating is not a way to
/// silently blank an existing signature.
pub fn create(name: &str) -> Result<PathBuf> {
    validate_name(name)?;
    if exists(name) {
        bail!("a signature named '{name}' already exists");
    }
    write(name, "")?;
    Ok(signature_file(name))
}

/// Rename a signature, carrying any account default that pointed at it.
///
/// Fails when `old` does not exist or `new` already does, so a rename can
/// never destroy the target.
pub fn rename(old: &str, new: &str) -> Result<()> {
    validate_name(old)?;
    validate_name(new)?;
    if old == new {
        return Ok(());
    }
    if !exists(old) {
        bail!("no signature named '{old}'");
    }
    if exists(new) {
        bail!("a signature named '{new}' already exists");
    }
    fs::rename(signature_file(old), signature_file(new)).with_context(|| {
        format!(
            "renaming signature {} to {}",
            signature_file(old).display(),
            signature_file(new).display()
        )
    })?;
    retarget_defaults(old, Some(new))
}

/// Delete a signature and clear any account default that pointed at it.
pub fn delete(name: &str) -> Result<()> {
    validate_name(name)?;
    if !exists(name) {
        bail!("no signature named '{name}'");
    }
    let path = signature_file(name);
    fs::remove_file(&path)
        .with_context(|| format!("deleting signature at {}", path.display()))?;
    retarget_defaults(name, None)
}

/// Point every account default that named `old` at `new` (or clear it).
fn retarget_defaults(old: &str, new: Option<&str>) -> Result<()> {
    let mut state = AppState::load();
    let accounts: Vec<String> = state
        .accounts
        .iter()
        .filter(|(_, a)| a.default_signature.as_deref() == Some(old))
        .map(|(name, _)| name.clone())
        .collect();
    if accounts.is_empty() {
        return Ok(());
    }
    for account in &accounts {
        state.set_default_signature(account, new);
    }
    state.save()
}

// ---------------------------------------------------------------------------
// Per-account default selection (thin wrappers over the app state file)
// ---------------------------------------------------------------------------

/// The account's default signature name, if it is recorded *and* the file it
/// names still exists. A default pointing at a deleted file is no default.
pub fn default_signature_name(account: &str) -> Option<String> {
    let state = AppState::load();
    let name = state.default_signature(account)?.to_string();
    exists(&name).then_some(name)
}

/// Record (or clear, with `None`) the account's default signature.
pub fn set_default_signature(account: &str, name: Option<&str>) -> Result<()> {
    if let Some(name) = name {
        validate_name(name)?;
        if !exists(name) {
            bail!("no signature named '{name}'");
        }
    }
    let mut state = AppState::load();
    state.set_default_signature(account, name);
    state.save()
}

// ---------------------------------------------------------------------------
// Migration off `[accounts.signatures.*]` (#0107)
// ---------------------------------------------------------------------------

/// Copy any legacy `[accounts.<acct>.signatures.*]` tables into the signatures
/// directory and the app state file, then say so once.
///
/// Idempotent by construction: a name whose `.md` file already exists is left
/// alone (so a signature the user has since edited is never clobbered), and a
/// default is only recorded when the account has none yet. The notice is
/// re-emitted on every run while the dead tables are still in `config.toml`,
/// which is the only nudge the user gets to delete them, since nothing here
/// rewrites their file.
///
/// Errors are per-entry and non-fatal: a missing `path` source or an
/// unwritable file warns and skips. Failing startup over a signature would be
/// out of proportion to what a signature is.
pub fn migrate_config_signatures(config: &GlobalConfig) -> Result<()> {
    let mut legacy_present = false;
    let mut state = AppState::load();
    let mut state_dirty = false;

    for account in &config.accounts {
        let legacy = &account.signatures;
        if legacy.entries.is_empty() && legacy.default.is_none() {
            continue;
        }
        legacy_present = true;

        let mut names: Vec<&String> = legacy.entries.keys().collect();
        names.sort();
        for name in names {
            let entry = &legacy.entries[name];
            if validate_name(name).is_err() {
                log::warn!(
                    "[signatures] skipping legacy signature '{name}' of account '{}': \
                     not a usable file name",
                    account.name
                );
                continue;
            }
            if exists(name) {
                continue;
            }
            let Some(content) = legacy_entry_content(&account.name, name, entry) else {
                continue;
            };
            match write(name, &content) {
                Ok(()) => log::info!(
                    "[signatures] migrated '{name}' to {}",
                    signature_file(name).display()
                ),
                Err(e) => log::warn!("[signatures] could not migrate '{name}': {e:#}"),
            }
        }

        // The old per-account default, kept only if nothing has recorded one.
        if let Some(default) = legacy.default.as_deref() {
            if state.default_signature(&account.name).is_none() && validate_name(default).is_ok() {
                state.set_default_signature(&account.name, Some(default));
                state_dirty = true;
            }
        }
    }

    if state_dirty {
        if let Err(e) = state.save() {
            log::warn!("[signatures] could not record migrated defaults: {e:#}");
        }
    }

    if legacy_present {
        warn_about_dead_signature_tables();
    }
    Ok(())
}

/// The content a legacy entry should be written out as: inline `text`
/// verbatim, else the `path` file's bytes. `None` (with a warning) for an
/// entry that carries neither, or whose `path` cannot be read.
fn legacy_entry_content(
    account: &str,
    name: &str,
    entry: &crate::config::SignatureEntry,
) -> Option<String> {
    if let Some(text) = entry.text.as_deref() {
        return Some(text.to_string());
    }
    let path_str = entry.path.as_deref()?;
    let expanded = shellexpand::tilde(path_str).into_owned();
    match fs::read_to_string(Path::new(&expanded)) {
        Ok(content) => Some(content),
        Err(e) => {
            log::warn!(
                "[signatures] account '{account}': signature '{name}' points at {expanded}, \
                 which could not be read ({e}); skipped"
            );
            None
        }
    }
}

/// One notice that the `[accounts.*.signatures]` tables no longer do anything.
///
/// Same shape as [`crate::config`]'s self-reference warning: stderr for the
/// user in front of a terminal, `log::warn!` for the one who ran the TUI and
/// never saw stderr at all.
fn warn_about_dead_signature_tables() {
    let dir = signatures_dir();
    eprintln!(
        "{} Signatures now live as Markdown files in {} (#0107). The \
         [accounts.*.signatures] tables in {} are no longer read and can be deleted; \
         your signatures were copied out of them.",
        "⚠".yellow(),
        dir.display(),
        crate::config::config_path().display(),
    );
    log::warn!(
        "[signatures] legacy [accounts.*.signatures] tables in {} are dead; \
         signatures now live in {}",
        crate::config::config_path().display(),
        dir.display()
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::test_env::{ConfigDirOverride, TestDataDir};
    use crate::config::{AccountConfig, SignatureEntry, SignaturesConfig};
    use std::collections::HashMap;

    /// Both path layers redirected into one tempdir: `config_dir()` for the
    /// signature files, `mailypoppins_data_dir()` for `state.json`.
    struct Fixture {
        _dir: tempfile::TempDir,
        _config: ConfigDirOverride,
        _data: TestDataDir,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = ConfigDirOverride::new(dir.path());
        config.set_config_dir(&dir.path().join("config"));
        Fixture {
            _dir: dir,
            _config: config,
            _data: TestDataDir::new(),
        }
    }

    fn account_with(default: Option<&str>, entries: &[(&str, SignatureEntry)]) -> AccountConfig {
        let mut map = HashMap::new();
        for (name, entry) in entries {
            map.insert((*name).to_string(), entry.clone());
        }
        AccountConfig {
            name: "work".to_string(),
            signatures: SignaturesConfig {
                default: default.map(str::to_string),
                entries: map,
            },
            ..Default::default()
        }
    }

    fn config_with(accounts: Vec<AccountConfig>) -> GlobalConfig {
        GlobalConfig {
            accounts,
            ..Default::default()
        }
    }

    // -- CRUD ---------------------------------------------------------------

    #[test]
    fn write_read_list_round_trip() {
        let _fx = fixture();
        assert!(list().is_empty());

        write("work", "-- \nAlice").unwrap();
        write("casual", "Cheers").unwrap();

        assert_eq!(list(), vec!["casual".to_string(), "work".to_string()]);
        assert_eq!(read("work").as_deref(), Some("-- \nAlice"));
        assert!(read("nope").is_none());
        assert!(exists("work"));
    }

    #[test]
    fn create_makes_an_empty_file_and_refuses_a_duplicate() {
        let _fx = fixture();
        let path = create("work").unwrap();
        assert!(path.is_file());
        assert_eq!(read("work").as_deref(), Some(""));

        let err = create("work").unwrap_err().to_string();
        assert!(err.contains("already exists"), "{err}");
    }

    #[test]
    fn rename_moves_the_file_and_delete_removes_it() {
        let _fx = fixture();
        write("work", "content").unwrap();

        rename("work", "office").unwrap();
        assert!(!exists("work"));
        assert_eq!(read("office").as_deref(), Some("content"));

        delete("office").unwrap();
        assert!(list().is_empty());
        assert!(delete("office").is_err());
    }

    #[test]
    fn rename_refuses_to_overwrite_an_existing_signature() {
        let _fx = fixture();
        write("work", "a").unwrap();
        write("home", "b").unwrap();

        let err = rename("work", "home").unwrap_err().to_string();
        assert!(err.contains("already exists"), "{err}");
        assert_eq!(read("home").as_deref(), Some("b"));
    }

    /// A rename carries the default with it; a delete clears it.
    #[test]
    fn rename_and_delete_keep_the_default_selection_honest() {
        let _fx = fixture();
        write("work", "a").unwrap();
        set_default_signature("acct", Some("work")).unwrap();

        rename("work", "office").unwrap();
        assert_eq!(default_signature_name("acct").as_deref(), Some("office"));

        delete("office").unwrap();
        assert_eq!(default_signature_name("acct"), None);
    }

    #[test]
    fn a_default_naming_a_missing_file_is_no_default() {
        let _fx = fixture();
        let mut state = AppState::load();
        state.set_default_signature("acct", Some("ghost"));
        state.save().unwrap();

        assert_eq!(default_signature_name("acct"), None);
    }

    #[test]
    fn setting_a_default_requires_the_signature_to_exist() {
        let _fx = fixture();
        assert!(set_default_signature("acct", Some("ghost")).is_err());

        write("work", "a").unwrap();
        set_default_signature("acct", Some("work")).unwrap();
        assert_eq!(default_signature_name("acct").as_deref(), Some("work"));

        set_default_signature("acct", None).unwrap();
        assert_eq!(default_signature_name("acct"), None);
    }

    // -- Name validation ----------------------------------------------------

    #[test]
    fn names_that_are_not_safe_file_stems_are_rejected() {
        for bad in [
            "",
            ".hidden",
            "../escape",
            "..",
            "a/b",
            "a\\b",
            " leading",
            "trailing ",
            "semi;colon",
            "quote\"d",
            "new\nline",
        ] {
            assert!(validate_name(bad).is_err(), "accepted {bad:?}");
        }
        let long = "x".repeat(MAX_NAME_LEN + 1);
        assert!(validate_name(&long).is_err(), "accepted an over-long name");
    }

    #[test]
    fn ordinary_names_are_accepted() {
        for good in ["work", "work-external", "work_de", "v1.2", "with space", "A1"] {
            validate_name(good).unwrap_or_else(|e| panic!("rejected {good:?}: {e}"));
        }
    }

    /// A rejected name never reaches the filesystem.
    #[test]
    fn a_traversing_name_writes_nothing() {
        let _fx = fixture();
        assert!(write("../escape", "x").is_err());
        assert!(create("../escape").is_err());
        assert!(list().is_empty());
    }

    /// A stray file in the directory is not a signature.
    #[test]
    fn list_ignores_non_markdown_files() {
        let _fx = fixture();
        write("work", "a").unwrap();
        fs::write(signatures_dir().join("notes.txt"), "x").unwrap();
        fs::create_dir_all(signatures_dir().join("sub.md")).unwrap();

        assert_eq!(list(), vec!["work".to_string()]);
    }

    // -- Migration ----------------------------------------------------------

    #[test]
    fn migration_writes_inline_text_to_a_file() {
        let _fx = fixture();
        let config = config_with(vec![account_with(
            Some("default"),
            &[(
                "default",
                SignatureEntry {
                    name: None,
                    text: Some("-- \nAlice".to_string()),
                    path: None,
                },
            )],
        )]);

        migrate_config_signatures(&config).unwrap();

        assert_eq!(read("default").as_deref(), Some("-- \nAlice"));
        assert_eq!(default_signature_name("work").as_deref(), Some("default"));
    }

    #[test]
    fn migration_copies_a_path_entrys_file() {
        let _fx = fixture();
        let src = tempfile::tempdir().unwrap();
        let file = src.path().join("robin.md");
        fs::write(&file, "-- \nRobin\n").unwrap();

        let config = config_with(vec![account_with(
            None,
            &[(
                "robin",
                SignatureEntry {
                    name: None,
                    text: None,
                    path: Some(file.to_string_lossy().into_owned()),
                },
            )],
        )]);

        migrate_config_signatures(&config).unwrap();

        assert_eq!(read("robin").as_deref(), Some("-- \nRobin\n"));
        // No legacy default, so nothing is recorded.
        assert_eq!(default_signature_name("work"), None);
    }

    /// A `path` that no longer resolves is skipped, and the rest still
    /// migrates.
    #[test]
    fn migration_skips_an_unreadable_path_entry() {
        let _fx = fixture();
        let config = config_with(vec![account_with(
            None,
            &[
                (
                    "gone",
                    SignatureEntry {
                        name: None,
                        text: None,
                        path: Some("/no/such/signature.md".to_string()),
                    },
                ),
                (
                    "kept",
                    SignatureEntry {
                        name: None,
                        text: Some("here".to_string()),
                        path: None,
                    },
                ),
            ],
        )]);

        migrate_config_signatures(&config).unwrap();

        assert_eq!(list(), vec!["kept".to_string()]);
    }

    /// Running it twice must not clobber a signature the user has edited in
    /// the meantime, nor move a default they have since changed.
    #[test]
    fn migration_is_idempotent() {
        let _fx = fixture();
        let config = config_with(vec![account_with(
            Some("default"),
            &[(
                "default",
                SignatureEntry {
                    name: None,
                    text: Some("original".to_string()),
                    path: None,
                },
            )],
        )]);

        migrate_config_signatures(&config).unwrap();
        write("default", "edited since").unwrap();
        write("newer", "made in the app").unwrap();
        set_default_signature("work", Some("newer")).unwrap();

        migrate_config_signatures(&config).unwrap();

        assert_eq!(read("default").as_deref(), Some("edited since"));
        assert_eq!(default_signature_name("work").as_deref(), Some("newer"));
        assert_eq!(list(), vec!["default".to_string(), "newer".to_string()]);
    }

    /// Nothing to migrate: no files, no state, no notice.
    #[test]
    fn migration_is_a_no_op_without_legacy_tables() {
        let _fx = fixture();
        let config = config_with(vec![AccountConfig {
            name: "work".to_string(),
            ..Default::default()
        }]);

        migrate_config_signatures(&config).unwrap();

        assert!(list().is_empty());
        assert!(!crate::app_state::state_path().exists());
    }
}
