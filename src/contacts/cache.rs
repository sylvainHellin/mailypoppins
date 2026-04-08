//! JSON cache for the per-account contact index.
//!
//! Each account's index lives at `<directories.root>/.contacts-cache.json`.

use crate::contacts::types::ContactIndex;
use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// Cache file for a given account lives inside that account's root directory.
pub fn cache_path(account_root: &Path) -> PathBuf {
    account_root.join(".contacts-cache.json")
}

pub fn load_cache(account_root: &Path) -> Result<Option<ContactIndex>> {
    let path = cache_path(account_root);
    if !path.exists() {
        return Ok(None);
    }
    let data = fs::read_to_string(&path)
        .with_context(|| format!("reading contacts cache at {}", path.display()))?;
    let index: ContactIndex = serde_json::from_str(&data)
        .with_context(|| format!("parsing contacts cache at {}", path.display()))?;
    Ok(Some(index))
}

pub fn save_cache(account_root: &Path, index: &ContactIndex) -> Result<()> {
    let path = cache_path(account_root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating cache directory at {}", parent.display()))?;
    }
    let data = serde_json::to_string_pretty(index)?;
    fs::write(&path, data)
        .with_context(|| format!("writing contacts cache at {}", path.display()))?;
    Ok(())
}
