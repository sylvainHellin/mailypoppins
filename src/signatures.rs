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
use std::collections::HashMap;
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

/// One legacy entry the migration could not copy out, for the notice.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SkippedEntry {
    account: String,
    entry: String,
    reason: String,
}

/// What one migration run found and what it could not carry over.
struct MigrationReport {
    /// Whether any account still carries a `[accounts.*.signatures]` table.
    legacy_present: bool,
    /// Entries that reached no file, so their only copy is still in
    /// `config.toml`.
    skipped: Vec<SkippedEntry>,
}

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
/// The legacy layer was per-account and this one is flat, so two accounts can
/// arrive with the same entry key and different content. The second one is
/// migrated under `<account>-<name>` and its default points there, rather than
/// silently inheriting the first account's signature; identical content is not
/// a collision and keeps sharing one file. See [`migration_target_name`].
///
/// Errors are per-entry and non-fatal: a missing `path` source or an
/// unwritable file warns and skips. Failing startup over a signature would be
/// out of proportion to what a signature is. Skipped entries are named in the
/// notice, because their content exists nowhere but the tables the notice is
/// otherwise telling the user to delete.
pub fn migrate_config_signatures(config: &GlobalConfig) -> Result<()> {
    let report = run_config_signature_migration(config);
    if report.legacy_present {
        eprintln!("{}", dead_signature_tables_notice(&report.skipped));
        log::warn!(
            "[signatures] legacy [accounts.*.signatures] tables in {} are dead; \
             signatures now live in {}",
            crate::config::config_path().display(),
            signatures_dir().display()
        );
    }
    Ok(())
}

/// The migration proper, split out so tests can inspect what was skipped
/// without capturing stderr.
fn run_config_signature_migration(config: &GlobalConfig) -> MigrationReport {
    let mut report = MigrationReport {
        legacy_present: false,
        skipped: Vec::new(),
    };
    let mut state = AppState::load();
    let mut state_dirty = false;
    // Signature name -> the account that claimed it in *this* run. A name that
    // merely exists on disk is not claimed: that is the rerun / user-edited
    // case, which keeps the pre-existing "leave it alone" behaviour.
    let mut claimed: HashMap<String, String> = HashMap::new();

    for account in &config.accounts {
        let legacy = &account.signatures;
        if legacy.entries.is_empty() && legacy.default.is_none() {
            continue;
        }
        report.legacy_present = true;
        // Entry key -> the name it actually landed under, when they differ.
        let mut renamed: HashMap<String, String> = HashMap::new();
        // Every name this account resolved to, shared ones included, so its
        // default is never pointed at a file that belongs to someone else.
        let mut mine: Vec<String> = Vec::new();

        let mut names: Vec<&String> = legacy.entries.keys().collect();
        names.sort();
        for name in names {
            let entry = &legacy.entries[name];
            if let Err(e) = validate_name(name) {
                log::warn!(
                    "[signatures] skipping legacy signature '{name}' of account '{}': \
                     not a usable file name",
                    account.name
                );
                report.skipped.push(SkippedEntry {
                    account: account.name.clone(),
                    entry: name.clone(),
                    reason: format!("{e}"),
                });
                continue;
            }
            let Some(content) = legacy_entry_content(&account.name, name, entry) else {
                report.skipped.push(SkippedEntry {
                    account: account.name.clone(),
                    entry: name.clone(),
                    reason: match entry.path.as_deref() {
                        Some(p) => format!("its path {p} could not be read"),
                        None => "it has neither 'text' nor 'path'".to_string(),
                    },
                });
                continue;
            };
            let Some(target) = migration_target_name(&account.name, name, &content, &claimed)
            else {
                log::warn!(
                    "[signatures] account '{}': no usable file name for signature '{name}'",
                    account.name
                );
                report.skipped.push(SkippedEntry {
                    account: account.name.clone(),
                    entry: name.clone(),
                    reason: "no usable file name was free for it".to_string(),
                });
                continue;
            };
            claimed
                .entry(target.clone())
                .or_insert_with(|| account.name.clone());
            mine.push(target.clone());
            if target != *name {
                renamed.insert(name.clone(), target.clone());
            }
            // An existing file is never overwritten: it is either this entry
            // from an earlier run, the same content shared with another
            // account, or something the user has since edited.
            if exists(&target) {
                continue;
            }
            match write(&target, &content) {
                Ok(()) => log::info!(
                    "[signatures] migrated '{name}' of account '{}' to {}",
                    account.name,
                    signature_file(&target).display()
                ),
                Err(e) => {
                    log::warn!("[signatures] could not migrate '{name}': {e:#}");
                    report.skipped.push(SkippedEntry {
                        account: account.name.clone(),
                        entry: name.clone(),
                        reason: format!("it could not be written out ({e})"),
                    });
                }
            }
        }

        // The old per-account default, kept only if nothing has recorded one,
        // and pointed at the disambiguated name when the entry was moved.
        if let Some(default) = legacy.default.as_deref() {
            if state.default_signature(&account.name).is_none() {
                let target = renamed
                    .get(default)
                    .cloned()
                    .unwrap_or_else(|| default.to_string());
                // A default whose entry this account did not migrate (it was
                // skipped) must not inherit whatever another account wrote
                // under that name: no default beats the wrong identity's.
                let anothers = !mine.contains(&target)
                    && claimed
                        .get(&target)
                        .is_some_and(|owner| *owner != account.name);
                if anothers {
                    log::warn!(
                        "[signatures] account '{}': default '{default}' was not migrated, \
                         and '{target}' belongs to another account; leaving it unset",
                        account.name
                    );
                } else if validate_name(&target).is_ok() {
                    state.set_default_signature(&account.name, Some(&target));
                    state_dirty = true;
                }
            }
        }
    }

    if state_dirty {
        if let Err(e) = state.save() {
            log::warn!("[signatures] could not record migrated defaults: {e:#}");
        }
    }
    report
}

/// The file name a legacy entry's content should land under.
///
/// The entry's own name when nothing in this run has claimed it (free name,
/// or a file already on disk, which is the rerun case), and when another
/// account claimed it but the content is byte-identical, since one file then
/// serves both. Otherwise `<account>-<name>`, and `<account>-<name>-2`, `-3`
/// and so on if even that is taken by a third account with other content.
///
/// Deterministic given the account order in `config.toml`, so a rerun lands on
/// the same name and finds the file already there instead of making a second
/// copy. `None` when no candidate is a usable file stem.
fn migration_target_name(
    account: &str,
    name: &str,
    content: &str,
    claimed: &HashMap<String, String>,
) -> Option<String> {
    let free_for_us = |candidate: &str| match claimed.get(candidate) {
        None => true,
        Some(owner) => owner == account || read(candidate).as_deref() == Some(content),
    };
    if free_for_us(name) {
        return Some(name.to_string());
    }
    for attempt in 1..=99 {
        let Some(candidate) = disambiguated_name(account, name, attempt) else {
            continue;
        };
        if free_for_us(&candidate) {
            return Some(candidate);
        }
    }
    None
}

/// `<account>-<name>` for the first attempt, `<account>-<name>-<n>` after
/// that, with the account part reduced to a safe file stem and truncated so
/// the whole thing still fits [`MAX_NAME_LEN`]. `None` when nothing usable is
/// left of the account name.
fn disambiguated_name(account: &str, name: &str, attempt: usize) -> Option<String> {
    let suffix = if attempt <= 1 {
        String::new()
    } else {
        format!("-{attempt}")
    };
    let cleaned: String = account
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches(|c| c == '-' || c == '.');
    if cleaned.is_empty() {
        return None;
    }
    let budget = MAX_NAME_LEN.checked_sub(name.len() + 1 + suffix.len())?;
    if budget == 0 {
        return None;
    }
    let prefix: String = cleaned.chars().take(budget).collect();
    let candidate = format!("{prefix}-{name}{suffix}");
    validate_name(&candidate).ok()?;
    Some(candidate)
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

/// The notice that the `[accounts.*.signatures]` tables no longer do anything.
///
/// Same shape as [`crate::config`]'s self-reference warning: stderr for the
/// user in front of a terminal, `log::warn!` for the one who ran the TUI and
/// never saw stderr at all.
///
/// "Can be deleted" is only said when every entry reached a file. An entry the
/// migration skipped exists nowhere else, and `mp config init` rewrites
/// `config.toml` without these tables, so a blanket "we copied everything out"
/// would be the last thing the user read before losing it.
fn dead_signature_tables_notice(skipped: &[SkippedEntry]) -> String {
    let dir = signatures_dir();
    let config = crate::config::config_path();
    if skipped.is_empty() {
        return format!(
            "{} Signatures now live as Markdown files in {} (#0107). The \
             [accounts.*.signatures] tables in {} are no longer read and can be deleted; \
             your signatures were copied out of them.",
            "⚠".yellow(),
            dir.display(),
            config.display(),
        );
    }
    let mut out = format!(
        "{} Signatures now live as Markdown files in {} (#0107). The \
         [accounts.*.signatures] tables in {} are no longer read.\n\
         These entries could NOT be copied out, so keep them until you have saved \
         their content elsewhere:",
        "⚠".yellow(),
        dir.display(),
        config.display(),
    );
    for s in skipped {
        out.push_str(&format!(
            "\n  - account '{}', signature '{}': {}",
            s.account, s.entry, s.reason
        ));
    }
    out.push_str("\nEverything else was copied out; delete the tables only once these are handled.");
    out
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
        named_account_with("work", default, entries)
    }

    fn named_account_with(
        name: &str,
        default: Option<&str>,
        entries: &[(&str, SignatureEntry)],
    ) -> AccountConfig {
        let mut map = HashMap::new();
        for (entry_name, entry) in entries {
            map.insert((*entry_name).to_string(), entry.clone());
        }
        AccountConfig {
            name: name.to_string(),
            signatures: SignaturesConfig {
                default: default.map(str::to_string),
                entries: map,
            },
            ..Default::default()
        }
    }

    fn inline(text: &str) -> SignatureEntry {
        SignatureEntry {
            name: None,
            text: Some(text.to_string()),
            path: None,
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

    /// Two accounts, the same entry key, different content: the flat layer has
    /// one name and they need two files, so the second account's is migrated
    /// under `<account>-<name>` and its default follows it. Without this,
    /// account two silently signs with account one's identity.
    #[test]
    fn a_cross_account_name_collision_migrates_under_a_disambiguated_name() {
        let _fx = fixture();
        let config = config_with(vec![
            named_account_with("work", Some("default"), &[("default", inline("-- \nAlice"))]),
            named_account_with("home", Some("default"), &[("default", inline("-- \nAl"))]),
        ]);

        migrate_config_signatures(&config).unwrap();

        // Both contents survive, under two names.
        assert_eq!(read("default").as_deref(), Some("-- \nAlice"));
        assert_eq!(read("home-default").as_deref(), Some("-- \nAl"));
        // Each account resolves its own.
        assert_eq!(default_signature_name("work").as_deref(), Some("default"));
        assert_eq!(
            default_signature_name("home").as_deref(),
            Some("home-default")
        );

        // A rerun makes no second copy and moves nothing.
        migrate_config_signatures(&config).unwrap();
        assert_eq!(
            list(),
            vec!["default".to_string(), "home-default".to_string()]
        );
        assert_eq!(read("default").as_deref(), Some("-- \nAlice"));
        assert_eq!(read("home-default").as_deref(), Some("-- \nAl"));

        // And it does not clobber the disambiguated file the user has edited.
        write("home-default", "edited since").unwrap();
        migrate_config_signatures(&config).unwrap();
        assert_eq!(read("home-default").as_deref(), Some("edited since"));
        assert_eq!(
            list(),
            vec!["default".to_string(), "home-default".to_string()]
        );
    }

    /// The same entry key with byte-identical content is not a collision: one
    /// file serves both accounts.
    #[test]
    fn identical_content_under_one_name_stays_one_file() {
        let _fx = fixture();
        let config = config_with(vec![
            named_account_with("work", Some("default"), &[("default", inline("-- \nAlice"))]),
            named_account_with("home", Some("default"), &[("default", inline("-- \nAlice"))]),
        ]);

        migrate_config_signatures(&config).unwrap();

        assert_eq!(list(), vec!["default".to_string()]);
        assert_eq!(default_signature_name("work").as_deref(), Some("default"));
        assert_eq!(default_signature_name("home").as_deref(), Some("default"));
    }

    /// A default whose own entry could not be migrated is left unset rather
    /// than pointed at the file another account wrote under that name.
    #[test]
    fn a_default_never_inherits_another_accounts_file() {
        let _fx = fixture();
        let config = config_with(vec![
            named_account_with("work", Some("default"), &[("default", inline("-- \nAlice"))]),
            named_account_with(
                "home",
                Some("default"),
                &[(
                    "default",
                    SignatureEntry {
                        name: None,
                        text: None,
                        path: Some("/no/such/signature.md".to_string()),
                    },
                )],
            ),
        ]);

        migrate_config_signatures(&config).unwrap();

        assert_eq!(list(), vec!["default".to_string()]);
        assert_eq!(default_signature_name("work").as_deref(), Some("default"));
        assert_eq!(default_signature_name("home"), None);
    }

    /// An entry that reached no file is named on stderr, and the notice stops
    /// saying the tables can be deleted: they hold its only copy.
    #[test]
    fn the_notice_names_every_entry_it_could_not_copy_out() {
        let _fx = fixture();
        let config = config_with(vec![named_account_with(
            "work",
            None,
            &[
                ("../escape", inline("unsafe name")),
                (
                    "gone",
                    SignatureEntry {
                        name: None,
                        text: None,
                        path: Some("/no/such/signature.md".to_string()),
                    },
                ),
                ("kept", inline("here")),
            ],
        )]);

        let report = run_config_signature_migration(&config);
        let notice = dead_signature_tables_notice(&report.skipped);

        assert_eq!(list(), vec!["kept".to_string()]);
        assert_eq!(report.skipped.len(), 2, "{:?}", report.skipped);
        assert!(notice.contains("account 'work', signature '../escape'"), "{notice}");
        assert!(notice.contains("account 'work', signature 'gone'"), "{notice}");
        assert!(notice.contains("/no/such/signature.md"), "{notice}");
        assert!(!notice.contains("can be deleted"), "{notice}");
        assert!(!notice.contains("kept"), "{notice}");

        // With nothing skipped the old, unqualified wording is unchanged.
        let clean = dead_signature_tables_notice(&[]);
        assert!(clean.contains("can be deleted"), "{clean}");
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
