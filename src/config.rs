use anyhow::{Context, Result};
use colored::*;
use log::debug;
use serde::Deserialize;
use simplelog::{CombinedLogger, Config as LogConfig, LevelFilter, WriteLogger};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;

/// Email configuration from config.toml
#[derive(Debug, Deserialize, Default)]
pub(crate) struct EmailConfig {
    #[serde(default)]
    pub(crate) email: EmailSettings,
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
pub(crate) struct SignaturesConfig {
    #[serde(default)]
    pub(crate) default: Option<String>,
    #[serde(flatten)]
    pub(crate) entries: HashMap<String, SignatureEntry>,
}

#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
pub(crate) struct SignatureEntry {
    #[serde(default)]
    pub(crate) name: Option<String>,
    pub(crate) path: String,
}

pub(crate) struct SmtpConfig {
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) username: String,
    pub(crate) password: String,
    pub(crate) default_from: String,
}

impl SmtpConfig {
    pub(crate) fn from_env(path_hint: Option<&Path>) -> Result<Self> {
        load_env_from_path(path_hint);

        Ok(Self {
            host: std::env::var("SMTP_HOST").context("SMTP_HOST not set in environment")?,
            port: std::env::var("SMTP_PORT")
                .unwrap_or_else(|_| "587".to_string())
                .parse()
                .context("Invalid SMTP_PORT")?,
            username: std::env::var("SMTP_USERNAME")
                .context("SMTP_USERNAME not set in environment")?,
            password: std::env::var("SMTP_PASSWORD")
                .context("SMTP_PASSWORD not set in environment")?,
            default_from: std::env::var("DEFAULT_FROM")
                .context("DEFAULT_FROM not set in environment")?,
        })
    }
}

#[derive(Clone)]
pub(crate) struct ImapConfig {
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) username: String,
    pub(crate) password: String,
}

impl ImapConfig {
    pub(crate) fn from_env(path_hint: Option<&Path>) -> Result<Self> {
        load_env_from_path(path_hint);

        Ok(Self {
            host: std::env::var("IMAP_HOST").context("IMAP_HOST not set in environment")?,
            port: std::env::var("IMAP_PORT")
                .unwrap_or_else(|_| "993".to_string())
                .parse()
                .context("Invalid IMAP_PORT")?,
            username: std::env::var("IMAP_USERNAME")
                .or_else(|_| std::env::var("SMTP_USERNAME"))
                .context("Neither IMAP_USERNAME nor SMTP_USERNAME set")?,
            password: std::env::var("IMAP_PASSWORD")
                .or_else(|_| std::env::var("SMTP_PASSWORD"))
                .context("Neither IMAP_PASSWORD nor SMTP_PASSWORD set")?,
        })
    }
}

/// Initialize file-based logging to ~/.email-cli/logs/email-cli-YYYY-MM-DD.log.
/// Non-fatal: prints a warning and continues if setup fails.
pub(crate) fn init_logging() {
    let log_dir = dirs_next().join("logs");
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
fn dirs_next() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".email-cli")
}

pub(crate) fn resolve_sent_mailbox() -> String {
    std::env::var("SENT_MAILBOX")
        .unwrap_or_else(|_| "Sent".to_string())
}

/// Load email config from config.toml
pub(crate) fn load_email_config(path_hint: Option<&Path>) -> EmailConfig {
    // Normalize path hint by stripping trailing slashes to ensure parent() works correctly
    // Path::new("foo/bar/").parent() returns "foo/bar", not "foo"
    let normalized_hint: Option<PathBuf> = path_hint.map(|p| {
        let s = p.to_string_lossy();
        let trimmed = s.trim_end_matches('/').trim_end_matches('\\');
        PathBuf::from(trimmed)
    });
    let path_hint = normalized_hint.as_deref();

    // Build list of locations to search
    let mut config_locations: Vec<PathBuf> = Vec::new();

    // 1. Try relative to path hint (file or directory being processed)
    if let Some(hint) = path_hint {
        if let Some(parent) = hint.parent() {
            config_locations.push(parent.join("config.toml"));
            if let Some(grandparent) = parent.parent() {
                config_locations.push(grandparent.join("config.toml"));
                config_locations.push(grandparent.join("email/config.toml"));
                if let Some(great) = grandparent.parent() {
                    config_locations.push(great.join("config.toml"));
                    config_locations.push(great.join("email/config.toml"));
                }
            }
        }
    }

    // 2. Try relative to current working directory (for running from any location)
    if let Ok(cwd) = std::env::current_dir() {
        config_locations.push(cwd.join("config.toml"));
        config_locations.push(cwd.join("email/config.toml"));
        config_locations.push(cwd.join("../config.toml"));
        config_locations.push(cwd.join("../email/config.toml"));
        config_locations.push(cwd.join("../../config.toml"));
        config_locations.push(cwd.join("../../email/config.toml"));
    }

    for location in config_locations {
        if location.exists() {
            if let Ok(content) = fs::read_to_string(&location) {
                if let Ok(mut config) = toml::from_str::<EmailConfig>(&content) {
                    debug!("Loaded config from {}", location.display());
                    // Resolve relative signature paths to absolute
                    let config_dir = location.parent().unwrap_or(Path::new("."));
                    for entry in config.signatures.entries.values_mut() {
                        if !Path::new(&entry.path).is_absolute() {
                            entry.path = config_dir.join(&entry.path).to_string_lossy().to_string();
                        }
                    }
                    return config;
                }
            }
        }
    }

    debug!("No config.toml found, using defaults");
    EmailConfig::default()
}

/// Load signature HTML content
pub(crate) fn load_signature(config: &EmailConfig, signature_name: Option<&str>) -> Option<String> {
    let sig_name = signature_name
        .map(|s| s.to_string())
        .or_else(|| config.signatures.default.clone())?;

    let entry = config.signatures.entries.get(&sig_name)?;
    let path = Path::new(&entry.path);

    if path.exists() {
        fs::read_to_string(path).ok()
    } else {
        eprintln!("{} Signature file not found: {}", "⚠".yellow(), entry.path);
        None
    }
}

/// Try to load .env from multiple locations
pub(crate) fn load_env_from_path(path: Option<&Path>) {
    // First try standard dotenvy (current dir and parents)
    dotenvy::dotenv().ok();

    // Normalize path by stripping trailing slashes
    let normalized: Option<PathBuf> = path.map(|p| {
        let s = p.to_string_lossy();
        let trimmed = s.trim_end_matches('/').trim_end_matches('\\');
        PathBuf::from(trimmed)
    });
    let path = normalized.as_deref();

    // Build list of locations to search for .env
    let mut env_locations: Vec<PathBuf> = Vec::new();

    // 1. Try relative to path hint (file or directory being processed)
    if let Some(p) = path {
        if let Some(parent) = p.parent() {
            env_locations.push(parent.join(".env"));
            if let Some(grandparent) = parent.parent() {
                env_locations.push(grandparent.join(".env"));
                if let Some(great) = grandparent.parent() {
                    env_locations.push(great.join(".env"));
                }
            }
        }
    }

    // 2. Try relative to current working directory (for running from any location)
    if let Ok(cwd) = std::env::current_dir() {
        env_locations.push(cwd.join(".env"));
        env_locations.push(cwd.join("../.env"));
        env_locations.push(cwd.join("../../.env"));
    }

    // Load from all found .env files (later ones override earlier)
    for env_path in env_locations {
        if env_path.exists() {
            dotenvy::from_path(&env_path).ok();
        }
    }
}
