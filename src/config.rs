use anyhow::{Context, Result};
use colored::*;
use log::debug;
use serde::Deserialize;
use simplelog::{CombinedLogger, Config as LogConfig, LevelFilter, WriteLogger};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;

// ---------------------------------------------------------------------------
// Global config (loaded from ~/.config/email/config.toml)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, Default)]
pub(crate) struct GlobalConfig {
    #[serde(default)]
    pub(crate) email: EmailSettings,
    #[serde(default)]
    pub(crate) smtp: SmtpSettings,
    #[serde(default)]
    pub(crate) imap: ImapSettings,
    #[serde(default)]
    pub(crate) directories: DirectorySettings,
    #[serde(default)]
    pub(crate) mailboxes: MailboxesConfig,
    #[serde(default)]
    pub(crate) signatures: SignaturesConfig,
}

#[derive(Debug, Deserialize)]
pub(crate) struct EmailSettings {
    #[serde(default = "default_font_family")]
    pub(crate) font_family: String,
    #[serde(default = "default_font_size")]
    pub(crate) font_size: String,
    #[serde(default = "default_true")]
    pub(crate) include_signature: bool,
}

impl Default for EmailSettings {
    fn default() -> Self {
        Self {
            font_family: default_font_family(),
            font_size: default_font_size(),
            include_signature: true,
        }
    }
}

fn default_font_family() -> String {
    "Helvetica, Arial, sans-serif".to_string()
}

fn default_font_size() -> String {
    "12pt".to_string()
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct SmtpSettings {
    #[serde(default)]
    pub(crate) host: String,
    #[serde(default = "default_smtp_port")]
    pub(crate) port: u16,
    #[serde(default)]
    pub(crate) username: String,
    #[serde(default)]
    pub(crate) default_from: String,
}

fn default_smtp_port() -> u16 {
    465
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct ImapSettings {
    #[serde(default)]
    pub(crate) host: String,
    #[serde(default = "default_imap_port")]
    pub(crate) port: u16,
    #[serde(default)]
    pub(crate) username: String,
}

fn default_imap_port() -> u16 {
    993
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct DirectorySettings {
    #[serde(default)]
    pub(crate) root: Option<String>,
    #[serde(default)]
    pub(crate) drafts: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub(crate) struct MailboxMapping {
    pub(crate) server: String,
    pub(crate) local: String,
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct MailboxesConfig {
    #[serde(default)]
    pub(crate) inbox: Option<MailboxMapping>,
    #[serde(default)]
    pub(crate) archive: Option<MailboxMapping>,
    #[serde(default)]
    pub(crate) sent: Option<MailboxMapping>,
    #[serde(default)]
    pub(crate) extra: Option<Vec<MailboxMapping>>,
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct SignaturesConfig {
    #[serde(default)]
    pub(crate) default: Option<String>,
    #[serde(flatten)]
    pub(crate) entries: HashMap<String, SignatureEntry>,
}

#[derive(Debug, Deserialize, Clone)]
pub(crate) struct SignatureEntry {
    #[serde(default)]
    pub(crate) name: Option<String>,
    pub(crate) path: String,
}

// ---------------------------------------------------------------------------
// Resolved runtime configs (include secrets from keyring)
// ---------------------------------------------------------------------------

pub(crate) struct SmtpConfig {
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) username: String,
    pub(crate) password: String,
    pub(crate) default_from: String,
}

#[derive(Clone)]
pub(crate) struct ImapConfig {
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) username: String,
    pub(crate) password: String,
}

// ---------------------------------------------------------------------------
// Keyring helpers
// ---------------------------------------------------------------------------

const KEYRING_SERVICE: &str = "email-cli";

pub(crate) fn get_keyring_password(key: &str) -> Result<String> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, key)
        .context("Failed to create keyring entry")?;
    entry
        .get_password()
        .with_context(|| format!("Password '{}' not found in keyring. Run `email config set-password` to set it.", key))
}

pub(crate) fn set_keyring_password(key: &str, password: &str) -> Result<()> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, key)
        .context("Failed to create keyring entry")?;
    entry
        .set_password(password)
        .with_context(|| format!("Failed to store password '{}' in keyring", key))
}

// ---------------------------------------------------------------------------
// Config loading
// ---------------------------------------------------------------------------

/// Return the path to the global config file: ~/.config/email/config.toml
pub(crate) fn config_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home)
        .join(".config")
        .join("email")
        .join("config.toml")
}

/// Load the global config from ~/.config/email/config.toml
pub(crate) fn load_global_config() -> Result<GlobalConfig> {
    let path = config_path();
    if !path.exists() {
        return Err(anyhow::anyhow!(
            "Config file not found at {}. Run `email config init` to create it.",
            path.display()
        ));
    }
    let content = fs::read_to_string(&path)
        .with_context(|| format!("Failed to read config file: {}", path.display()))?;
    let config: GlobalConfig = toml::from_str(&content)
        .with_context(|| format!("Failed to parse config file: {}", path.display()))?;
    debug!("Loaded global config from {}", path.display());
    Ok(config)
}

/// Build SmtpConfig from GlobalConfig + keyring password
impl SmtpConfig {
    pub(crate) fn load(config: &GlobalConfig) -> Result<Self> {
        let password = get_keyring_password("smtp-password")?;
        Ok(Self {
            host: config.smtp.host.clone(),
            port: config.smtp.port,
            username: config.smtp.username.clone(),
            password,
            default_from: config.smtp.default_from.clone(),
        })
    }
}

/// Build ImapConfig from GlobalConfig + keyring password (with SMTP fallback)
impl ImapConfig {
    pub(crate) fn load(config: &GlobalConfig) -> Result<Self> {
        // Username falls back to SMTP username if empty
        let username = if config.imap.username.is_empty() {
            config.smtp.username.clone()
        } else {
            config.imap.username.clone()
        };

        // Password: try imap-password first, fall back to smtp-password
        let password = get_keyring_password("imap-password")
            .or_else(|_| get_keyring_password("smtp-password"))?;

        // Host falls back to SMTP host if empty
        let host = if config.imap.host.is_empty() {
            config.smtp.host.clone()
        } else {
            config.imap.host.clone()
        };

        Ok(Self {
            host,
            port: config.imap.port,
            username,
            password,
        })
    }
}

/// Expand a path string: resolve ~ and optionally resolve relative paths against root.
fn expand_path(s: &str, root: Option<&Path>) -> PathBuf {
    let s = s.trim_matches('"').trim_matches('\'');
    if s.starts_with('/') || s.starts_with('~') {
        PathBuf::from(shellexpand::tilde(s).into_owned())
    } else if let Some(root) = root {
        root.join(s)
    } else {
        PathBuf::from(shellexpand::tilde(s).into_owned())
    }
}

/// Resolve an optional directory path from config: expand ~ and return PathBuf
pub(crate) fn resolve_dir(dir: &Option<String>) -> Option<PathBuf> {
    dir.as_ref().map(|s| expand_path(s, None))
}

/// Resolve the root directory from config
pub(crate) fn resolve_root_dir(config: &GlobalConfig) -> Option<PathBuf> {
    resolve_dir(&config.directories.root)
}

/// Resolve a MailboxMapping's local path, relative to root if not absolute
pub(crate) fn resolve_mailbox_local_path(config: &GlobalConfig, mapping: &MailboxMapping) -> PathBuf {
    let root = resolve_root_dir(config);
    expand_path(&mapping.local, root.as_deref())
}

/// Resolve the sent mailbox server name from config
pub(crate) fn resolve_sent_mailbox(config: &GlobalConfig) -> String {
    config
        .mailboxes
        .sent
        .as_ref()
        .map(|m| m.server.clone())
        .unwrap_or_else(|| "Sent".to_string())
}

/// Resolve the drafts directory from config (relative to root or absolute)
pub(crate) fn resolve_drafts_dir_from_config(config: &GlobalConfig) -> Option<PathBuf> {
    let root = resolve_root_dir(config);
    config.directories.drafts.as_ref().map(|s| expand_path(s, root.as_deref()))
}

/// Find a mailbox mapping by server name (case-insensitive match against configured mailboxes)
fn find_mailbox_mapping<'a>(config: &'a GlobalConfig, mailbox: &str) -> Option<&'a MailboxMapping> {
    // Check the three special-role mailboxes
    if let Some(ref m) = config.mailboxes.inbox {
        if m.server.eq_ignore_ascii_case(mailbox) || mailbox.eq_ignore_ascii_case("inbox") {
            return Some(m);
        }
    }
    if let Some(ref m) = config.mailboxes.archive {
        if m.server.eq_ignore_ascii_case(mailbox) || mailbox.eq_ignore_ascii_case("archive") {
            return Some(m);
        }
    }
    if let Some(ref m) = config.mailboxes.sent {
        if m.server.eq_ignore_ascii_case(mailbox) || mailbox.eq_ignore_ascii_case("sent") {
            return Some(m);
        }
    }
    // Check extra mailboxes
    if let Some(ref extras) = config.mailboxes.extra {
        for m in extras {
            if m.server.eq_ignore_ascii_case(mailbox) {
                return Some(m);
            }
        }
    }
    None
}

/// Resolve a mailbox name to its local directory from the config
pub(crate) fn resolve_mailbox_dir(config: &GlobalConfig, mailbox: &str) -> Result<PathBuf> {
    let mapping = find_mailbox_mapping(config, mailbox).with_context(|| {
        format!(
            "No mailbox configured for '{}'. Check [mailboxes] in {}",
            mailbox,
            config_path().display()
        )
    })?;
    Ok(resolve_mailbox_local_path(config, mapping))
}

/// Return all configured mailboxes: (role_or_server_name, mapping)
/// Roles: "inbox", "archive", "sent", plus extra server names.
pub(crate) fn all_configured_mailboxes(config: &GlobalConfig) -> Vec<(String, &MailboxMapping)> {
    let mut result = Vec::new();
    if let Some(ref m) = config.mailboxes.inbox {
        result.push(("inbox".to_string(), m));
    }
    if let Some(ref m) = config.mailboxes.archive {
        result.push(("archive".to_string(), m));
    }
    if let Some(ref m) = config.mailboxes.sent {
        result.push(("sent".to_string(), m));
    }
    if let Some(ref extras) = config.mailboxes.extra {
        for m in extras {
            result.push((m.server.clone(), m));
        }
    }
    result
}

/// Given a user-specified mailbox name (which might be a role like "inbox" or
/// a server name like "INBOX"), return the actual IMAP server name from config.
pub(crate) fn find_server_name_for_role(config: &GlobalConfig, name: &str) -> String {
    if let Some(mapping) = find_mailbox_mapping(config, name) {
        mapping.server.clone()
    } else {
        name.to_string()
    }
}

/// Slugify a mailbox name for use as a local directory name.
/// Lowercase, replace spaces/dots/slashes with hyphens, collapse consecutive hyphens.
pub(crate) fn slugify_mailbox_name(name: &str) -> String {
    let slug: String = name
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let mut result = String::new();
    let mut prev_hyphen = false;
    for c in slug.chars() {
        if c == '-' {
            if !prev_hyphen {
                result.push(c);
            }
            prev_hyphen = true;
        } else {
            prev_hyphen = false;
            result.push(c);
        }
    }
    result.trim_matches('-').to_string()
}

// ---------------------------------------------------------------------------
// Logging (unchanged)
// ---------------------------------------------------------------------------

/// Initialize file-based logging to ~/.email-cli/logs/email-cli-YYYY-MM-DD.log.
/// Non-fatal: prints a warning and continues if setup fails.
pub(crate) fn init_logging() {
    let log_dir = log_base_dir().join("logs");
    if let Err(e) = fs::create_dir_all(&log_dir) {
        eprintln!(
            "{} Could not create log directory {}: {}",
            "⚠".yellow(),
            log_dir.display(),
            e
        );
        return;
    }

    let filename = format!("email-cli-{}.log", Utc::now().format("%Y-%m-%d"));
    let log_path = log_dir.join(filename);

    let log_file = match fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!(
                "{} Could not open log file {}: {}",
                "⚠".yellow(),
                log_path.display(),
                e
            );
            return;
        }
    };

    if let Err(e) = CombinedLogger::init(vec![WriteLogger::new(
        LevelFilter::Debug,
        LogConfig::default(),
        log_file,
    )]) {
        eprintln!("{} Could not initialize logger: {}", "⚠".yellow(), e);
    }
}

/// Return the base directory for email-cli data: ~/.email-cli
fn log_base_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".email-cli")
}

// ---------------------------------------------------------------------------
// Signature loading
// ---------------------------------------------------------------------------

/// Load signature HTML content
pub(crate) fn load_signature(config: &GlobalConfig, signature_name: Option<&str>) -> Option<String> {
    let sig_name = signature_name
        .map(|s| s.to_string())
        .or_else(|| config.signatures.default.clone())?;

    let entry = config.signatures.entries.get(&sig_name)?;

    // Expand ~ in signature path
    let expanded = shellexpand::tilde(&entry.path).into_owned();
    let path = Path::new(&expanded);

    if path.exists() {
        fs::read_to_string(path).ok()
    } else {
        eprintln!("{} Signature file not found: {}", "⚠".yellow(), entry.path);
        None
    }
}
