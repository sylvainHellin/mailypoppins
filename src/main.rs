use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use clap::{Parser, Subcommand};
use colored::*;
use gray_matter::{engine::YAML, Matter};
use lettre::{
    message::{header::ContentType, Attachment, Mailbox, MultiPart, SinglePart},
    transport::smtp::authentication::Credentials,
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
};
use pulldown_cmark::{html, Options, Parser as MdParser};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Email configuration from config.toml
#[derive(Debug, Deserialize, Default)]
struct EmailConfig {
    #[serde(default)]
    email: EmailSettings,
    #[serde(default)]
    signatures: SignaturesConfig,
}

#[derive(Debug, Deserialize)]
struct EmailSettings {
    #[serde(default = "default_font_family")]
    font_family: String,
    #[serde(default = "default_font_size")]
    font_size: String,
    #[serde(default = "default_true")]
    include_signature: bool,
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
struct SignaturesConfig {
    #[serde(default)]
    default: Option<String>,
    #[serde(flatten)]
    entries: HashMap<String, SignatureEntry>,
}

#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
struct SignatureEntry {
    #[serde(default)]
    name: Option<String>,
    path: String,
}

#[derive(Parser)]
#[command(name = "email-cli")]
#[command(about = "A CLI tool for sending emails from Markdown drafts with YAML frontmatter")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// File to preview (dry-run mode)
    #[arg(value_name = "FILE")]
    file: Option<PathBuf>,

    /// Signature to use (overrides config default)
    #[arg(short, long, global = true)]
    signature: Option<String>,

    /// Skip signature entirely
    #[arg(long, global = true)]
    no_signature: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Send a single approved email
    Send {
        /// Path to the email draft file
        file: PathBuf,
    },
    /// Send all approved emails in a directory
    SendApproved {
        /// Directory containing email drafts
        #[arg(default_value = ".")]
        dir: PathBuf,
    },
    /// List emails by status
    List {
        /// Directory to scan
        #[arg(default_value = ".")]
        dir: PathBuf,
    },
    /// Validate YAML frontmatter in files
    Validate {
        /// File or directory to validate
        path: PathBuf,
    },
    /// Mark an email as approved
    MarkApproved {
        /// File to mark as approved
        file: PathBuf,
    },
    /// Create a new email draft from template
    New {
        /// Name for the new draft file
        name: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum EmailStatus {
    Draft,
    Approved,
    Sent,
}

impl std::fmt::Display for EmailStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EmailStatus::Draft => write!(f, "draft"),
            EmailStatus::Approved => write!(f, "approved"),
            EmailStatus::Sent => write!(f, "sent"),
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct EmailFrontmatter {
    to: String,
    #[serde(default)]
    cc: Option<String>,
    #[serde(default)]
    bcc: Option<String>,
    subject: String,
    status: EmailStatus,
    #[serde(default)]
    from: Option<String>,
    #[serde(default)]
    reply_to: Option<String>,
    #[serde(default)]
    attachments: Option<Vec<String>>,
    #[serde(default)]
    sent_at: Option<String>,
    #[serde(default)]
    sent_via: Option<String>,
}

#[derive(Debug)]
struct EmailDraft {
    path: PathBuf,
    frontmatter: EmailFrontmatter,
    body_markdown: String,
}

struct SmtpConfig {
    host: String,
    port: u16,
    username: String,
    password: String,
    default_from: String,
}

/// Load email config from config.toml
fn load_email_config(path_hint: Option<&Path>) -> EmailConfig {
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

    EmailConfig::default()
}

/// Load signature HTML content
fn load_signature(config: &EmailConfig, signature_name: Option<&str>) -> Option<String> {
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
fn load_env_from_path(path: Option<&Path>) {
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

impl SmtpConfig {
    fn from_env(path_hint: Option<&Path>) -> Result<Self> {
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

fn markdown_to_html(markdown: &str, config: &EmailConfig, signature: Option<&str>) -> String {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TABLES);

    let parser = MdParser::new_ext(markdown, options);
    let mut html_output = String::new();
    html::push_html(&mut html_output, parser);

    // Add signature if provided
    let signature_html = signature.unwrap_or_default();

    // Wrap in basic HTML structure with styling from config
    format!(
        r#"<!DOCTYPE html>
<html>
<head>
<meta charset="UTF-8">
<style>
body {{ font-family: {font_family}; font-size: {font_size}; line-height: 1.6; color: #000; }}
a {{ color: #0066cc; }}
p {{ margin: 0 0 1em 0; }}
</style>
</head>
<body>
{body}
{signature}
</body>
</html>"#,
        font_family = config.email.font_family,
        font_size = config.email.font_size,
        body = html_output,
        signature = signature_html
    )
}

fn parse_email_draft(path: &Path) -> Result<EmailDraft> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("Failed to read file: {}", path.display()))?;

    let matter = Matter::<YAML>::new();
    let parsed = matter.parse(&content);

    let frontmatter: EmailFrontmatter = parsed
        .data
        .ok_or_else(|| anyhow!("No frontmatter found in file"))?
        .deserialize()
        .context("Failed to parse frontmatter")?;

    let body_markdown = parsed.content.trim().to_string();

    Ok(EmailDraft {
        path: path.to_path_buf(),
        frontmatter,
        body_markdown,
    })
}

fn validate_draft(draft: &EmailDraft) -> Result<Vec<String>> {
    let mut warnings = Vec::new();

    // Check required fields
    if draft.frontmatter.to.is_empty() {
        return Err(anyhow!("Missing 'to' field"));
    }

    if draft.frontmatter.subject.is_empty() {
        return Err(anyhow!("Missing 'subject' field"));
    }

    if draft.body_markdown.is_empty() {
        warnings.push("Email body is empty".to_string());
    }

    // Validate email format (basic check)
    for email in draft.frontmatter.to.split(',') {
        let email = email.trim();
        if !email.contains('@') || !email.contains('.') {
            return Err(anyhow!("Invalid email address: {}", email));
        }
    }

    // Check attachments exist
    if let Some(attachments) = &draft.frontmatter.attachments {
        for attachment in attachments {
            let expanded = shellexpand::tilde(attachment);
            if !Path::new(expanded.as_ref()).exists() {
                warnings.push(format!("Attachment not found: {}", attachment));
            }
        }
    }

    Ok(warnings)
}

fn preview_draft(
    draft: &EmailDraft,
    smtp_config: &SmtpConfig,
    email_config: &EmailConfig,
    signature: Option<&str>,
    is_dry_run: bool,
) -> Result<()> {
    println!("\n{}", "=== Email Draft Preview ===".bold().cyan());
    println!(
        "{}: {}",
        "From".bold(),
        draft
            .frontmatter
            .from
            .as_ref()
            .unwrap_or(&smtp_config.default_from)
    );
    println!("{}: {}", "To".bold(), draft.frontmatter.to);

    if let Some(cc) = &draft.frontmatter.cc {
        println!("{}: {}", "Cc".bold(), cc);
    }

    if let Some(bcc) = &draft.frontmatter.bcc {
        println!("{}: {}", "Bcc".bold(), bcc);
    }

    println!("{}: {}", "Subject".bold(), draft.frontmatter.subject);

    println!("\n{}\n", "--- Body Preview (first 500 chars) ---".dimmed());
    let preview: String = draft.body_markdown.chars().take(500).collect();
    println!("{}", preview);
    if draft.body_markdown.len() > 500 {
        println!("{}", "...".dimmed());
    }

    println!("\n{}", "--- Settings ---".dimmed());
    println!(
        "  Font: {} ({})",
        email_config.email.font_family, email_config.email.font_size
    );
    if let Some(sig) = signature {
        let sig_preview: String = sig.chars().take(50).collect();
        println!("  Signature: {} ...", sig_preview.replace('\n', " "));
    } else {
        println!("  Signature: {}", "none".dimmed());
    }

    println!("\n{}", "--- Status ---".dimmed());

    // Validate and show status
    match validate_draft(draft) {
        Ok(warnings) => {
            println!("{} Valid YAML frontmatter", "✓".green());
            println!(
                "{} Status: {}",
                "✓".green(),
                format!("{}", draft.frontmatter.status).yellow()
            );
            println!("{} All required fields present", "✓".green());

            for warning in warnings {
                println!("{} {}", "⚠".yellow(), warning);
            }
        }
        Err(e) => {
            println!("{} Validation error: {}", "✗".red(), e);
        }
    }

    if is_dry_run {
        println!(
            "\n{}\n",
            "[DRY RUN] Would send email (use 'send' subcommand to actually send)"
                .yellow()
                .bold()
        );
    }

    Ok(())
}

async fn send_email(
    draft: &EmailDraft,
    smtp_config: &SmtpConfig,
    email_config: &EmailConfig,
    signature: Option<&str>,
) -> Result<()> {
    // Check status
    if draft.frontmatter.status != EmailStatus::Approved {
        return Err(anyhow!(
            "Email not approved for sending. Current status: {}",
            draft.frontmatter.status
        ));
    }

    let from_address = draft
        .frontmatter
        .from
        .as_ref()
        .unwrap_or(&smtp_config.default_from);

    let from_mailbox: Mailbox = from_address
        .parse()
        .context("Invalid 'from' email address")?;

    // Build the message
    let mut builder = Message::builder()
        .from(from_mailbox)
        .subject(&draft.frontmatter.subject);

    // Add To recipients
    for to in draft.frontmatter.to.split(',') {
        let to_mailbox: Mailbox = to.trim().parse().context("Invalid 'to' email address")?;
        builder = builder.to(to_mailbox);
    }

    // Add Cc recipients
    if let Some(cc) = &draft.frontmatter.cc {
        for cc_addr in cc.split(',') {
            let cc_mailbox: Mailbox = cc_addr
                .trim()
                .parse()
                .context("Invalid 'cc' email address")?;
            builder = builder.cc(cc_mailbox);
        }
    }

    // Add Bcc recipients
    if let Some(bcc) = &draft.frontmatter.bcc {
        for bcc_addr in bcc.split(',') {
            let bcc_mailbox: Mailbox = bcc_addr
                .trim()
                .parse()
                .context("Invalid 'bcc' email address")?;
            builder = builder.bcc(bcc_mailbox);
        }
    }

    // Add Reply-To
    if let Some(reply_to) = &draft.frontmatter.reply_to {
        let reply_mailbox: Mailbox = reply_to
            .parse()
            .context("Invalid 'reply_to' email address")?;
        builder = builder.reply_to(reply_mailbox);
    }

    // Generate HTML with signature
    let body_html = markdown_to_html(&draft.body_markdown, email_config, signature);

    // Build the text/html alternative part
    let body_multipart = MultiPart::alternative()
        .singlepart(
            SinglePart::builder()
                .header(ContentType::TEXT_PLAIN)
                .body(draft.body_markdown.clone()),
        )
        .singlepart(
            SinglePart::builder()
                .header(ContentType::TEXT_HTML)
                .body(body_html),
        );

    // Build message with or without attachments
    let message = if let Some(attachments) = &draft.frontmatter.attachments {
        if !attachments.is_empty() {
            // Wrap body in mixed multipart and add attachments
            let mut mixed = MultiPart::mixed().multipart(body_multipart);

            for attachment_path in attachments {
                let expanded = shellexpand::tilde(attachment_path);
                let path = Path::new(expanded.as_ref());

                let file_content = fs::read(path)
                    .with_context(|| format!("Failed to read attachment: {}", attachment_path))?;

                let filename = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "attachment".to_string());

                let content_type = mime_guess::from_path(path)
                    .first_or_octet_stream()
                    .to_string();

                let attachment = Attachment::new(filename).body(file_content, content_type.parse().unwrap());
                mixed = mixed.singlepart(attachment);
            }

            builder.multipart(mixed).context("Failed to build email message")?
        } else {
            builder.multipart(body_multipart).context("Failed to build email message")?
        }
    } else {
        builder.multipart(body_multipart).context("Failed to build email message")?
    };

    // Create SMTP transport
    let creds = Credentials::new(smtp_config.username.clone(), smtp_config.password.clone());

    let mailer: AsyncSmtpTransport<Tokio1Executor> =
        AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&smtp_config.host)?
            .port(smtp_config.port)
            .credentials(creds)
            .build();

    // Send the email
    mailer.send(message).await.context("Failed to send email")?;

    Ok(())
}

fn update_status_to_sent(draft: &EmailDraft, sent_dir: Option<&Path>) -> Result<()> {
    let mut frontmatter = draft.frontmatter.clone();
    frontmatter.status = EmailStatus::Sent;
    frontmatter.sent_at = Some(Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string());
    frontmatter.sent_via = Some(format!("email-cli v{}", env!("CARGO_PKG_VERSION")));

    // Serialize the updated frontmatter
    let yaml = serde_yaml::to_string(&frontmatter)?;

    // Reconstruct the file content
    let new_content = format!("---\n{}---\n\n{}", yaml, draft.body_markdown);

    // Determine destination path
    let dest_path = if let Some(sent_dir) = sent_dir {
        fs::create_dir_all(sent_dir)?;
        // Prepend datetime to filename to avoid overwriting and make search easier
        let timestamp = Utc::now().format("%Y-%m-%d-%H-%M-%S").to_string();
        let original_name = draft.path.file_name().unwrap().to_string_lossy();
        let new_name = format!("{}_{}", timestamp, original_name);
        sent_dir.join(new_name)
    } else {
        draft.path.clone()
    };

    // Write the updated content
    fs::write(&dest_path, new_content)?;

    // If we moved the file, remove the original
    if sent_dir.is_some() && dest_path != draft.path {
        fs::remove_file(&draft.path)?;
    }

    Ok(())
}

fn find_drafts(dir: &Path, status_filter: Option<EmailStatus>) -> Result<Vec<EmailDraft>> {
    let mut drafts = Vec::new();

    for entry in WalkDir::new(dir)
        .max_depth(1)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if path.is_file() && path.extension().is_some_and(|ext| ext == "md") {
            match parse_email_draft(path) {
                Ok(draft) => {
                    if let Some(ref filter) = status_filter {
                        if &draft.frontmatter.status == filter {
                            drafts.push(draft);
                        }
                    } else {
                        drafts.push(draft);
                    }
                }
                Err(e) => {
                    eprintln!("{} Skipping {}: {}", "⚠".yellow(), path.display(), e);
                }
            }
        }
    }

    Ok(drafts)
}

fn prompt_confirmation(message: &str) -> bool {
    print!("{} [y/N] ", message);
    io::stdout().flush().unwrap();

    let mut input = String::new();
    io::stdin().read_line(&mut input).unwrap();

    matches!(input.trim().to_lowercase().as_str(), "y" | "yes")
}

fn mark_as_approved(path: &Path) -> Result<()> {
    let draft = parse_email_draft(path)?;

    if draft.frontmatter.status == EmailStatus::Approved {
        println!("{} Already approved: {}", "ℹ".blue(), path.display());
        return Ok(());
    }

    if draft.frontmatter.status == EmailStatus::Sent {
        return Err(anyhow!("Cannot approve an already sent email"));
    }

    let mut frontmatter = draft.frontmatter.clone();
    frontmatter.status = EmailStatus::Approved;

    let yaml = serde_yaml::to_string(&frontmatter)?;
    let new_content = format!("---\n{}---\n\n{}", yaml, draft.body_markdown);

    fs::write(path, new_content)?;

    println!("{} Marked as approved: {}", "✓".green(), path.display());

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Determine a path hint for finding .env file
    let path_hint: Option<PathBuf> = match &cli.command {
        Some(Commands::Send { file }) => Some(file.clone()),
        Some(Commands::SendApproved { dir }) => Some(dir.clone()),
        Some(Commands::List { dir }) => Some(dir.clone()),
        Some(Commands::Validate { path }) => Some(path.clone()),
        Some(Commands::MarkApproved { file }) => Some(file.clone()),
        Some(Commands::New { name }) => Some(PathBuf::from(name)),
        None => cli.file.clone(),
    };

    // Load SMTP config with path hint for finding .env
    let smtp_config = SmtpConfig::from_env(path_hint.as_deref()).unwrap_or_else(|e| {
        eprintln!("{} Could not load SMTP config: {}", "⚠".yellow(), e);
        eprintln!("  Some commands may not work without proper configuration.");
        SmtpConfig {
            host: "localhost".to_string(),
            port: 587,
            username: String::new(),
            password: String::new(),
            default_from: "user@example.com".to_string(),
        }
    });

    // Load email config (fonts, signatures)
    let email_config = load_email_config(path_hint.as_deref());

    // Determine signature to use
    let signature_content: Option<String> = if cli.no_signature {
        None
    } else if email_config.email.include_signature {
        load_signature(&email_config, cli.signature.as_deref())
    } else {
        // include_signature is false, but user can override with --signature
        cli.signature
            .as_deref()
            .and_then(|s| load_signature(&email_config, Some(s)))
    };

    // Get sent directory from env (now loaded from .env via path hint)
    // Strip quotes if present (some .env parsers keep them)
    let sent_dir: Option<PathBuf> = std::env::var("SENT_DIR").ok().map(|s| {
        let s = s.trim_matches('"').trim_matches('\'');
        PathBuf::from(shellexpand::tilde(s).into_owned())
    });

    match cli.command {
        Some(Commands::Send { file }) => {
            let draft = parse_email_draft(&file)?;
            validate_draft(&draft)?;

            preview_draft(
                &draft,
                &smtp_config,
                &email_config,
                signature_content.as_deref(),
                false,
            )?;

            if !prompt_confirmation("Send this email?") {
                println!("Cancelled.");
                return Ok(());
            }

            println!("Sending email...");
            send_email(
                &draft,
                &smtp_config,
                &email_config,
                signature_content.as_deref(),
            )
            .await?;
            update_status_to_sent(&draft, sent_dir.as_deref())?;

            println!(
                "{} Email sent successfully to {}",
                "✓".green().bold(),
                draft.frontmatter.to
            );
        }

        Some(Commands::SendApproved { dir }) => {
            let drafts = find_drafts(&dir, Some(EmailStatus::Approved))?;

            if drafts.is_empty() {
                println!("No approved emails found in {}", dir.display());
                return Ok(());
            }

            println!(
                "\n{} approved email(s) found:\n",
                drafts.len().to_string().bold()
            );

            for draft in &drafts {
                println!(
                    "  {} → {}",
                    draft.path.file_name().unwrap().to_string_lossy(),
                    draft.frontmatter.to
                );
            }

            if !prompt_confirmation(&format!("\nSend all {} emails?", drafts.len())) {
                println!("Cancelled.");
                return Ok(());
            }

            let mut sent_count = 0;
            let mut failed_count = 0;

            for draft in drafts {
                print!("Sending to {}... ", draft.frontmatter.to);
                io::stdout().flush()?;

                match send_email(
                    &draft,
                    &smtp_config,
                    &email_config,
                    signature_content.as_deref(),
                )
                .await
                {
                    Ok(()) => {
                        if let Err(e) = update_status_to_sent(&draft, sent_dir.as_deref()) {
                            println!("{} (sent but failed to update status: {})", "⚠".yellow(), e);
                        } else {
                            println!("{}", "✓".green());
                        }
                        sent_count += 1;
                    }
                    Err(e) => {
                        println!("{} {}", "✗".red(), e);
                        failed_count += 1;
                    }
                }
            }

            println!(
                "\n{}: {} sent, {} failed",
                "Summary".bold(),
                sent_count.to_string().green(),
                failed_count.to_string().red()
            );
        }

        Some(Commands::List { dir }) => {
            let drafts = find_drafts(&dir, None)?;

            if drafts.is_empty() {
                println!("No email drafts found in {}", dir.display());
                return Ok(());
            }

            let mut draft_count = 0;
            let mut approved_count = 0;
            let mut sent_count = 0;

            println!("\n{}", "Email Drafts:".bold());
            println!("{}", "─".repeat(60));

            for draft in &drafts {
                let status_colored = match draft.frontmatter.status {
                    EmailStatus::Draft => "draft".yellow(),
                    EmailStatus::Approved => "approved".green(),
                    EmailStatus::Sent => "sent".dimmed(),
                };

                match draft.frontmatter.status {
                    EmailStatus::Draft => draft_count += 1,
                    EmailStatus::Approved => approved_count += 1,
                    EmailStatus::Sent => sent_count += 1,
                }

                println!(
                    "[{}] {} → {}",
                    status_colored,
                    draft.path.file_name().unwrap().to_string_lossy(),
                    draft.frontmatter.to
                );
            }

            println!("{}", "─".repeat(60));
            println!(
                "Total: {} | Draft: {} | Approved: {} | Sent: {}",
                drafts.len(),
                draft_count.to_string().yellow(),
                approved_count.to_string().green(),
                sent_count.to_string().dimmed()
            );
        }

        Some(Commands::Validate { path }) => {
            let files: Vec<PathBuf> = if path.is_dir() {
                WalkDir::new(&path)
                    .max_depth(1)
                    .into_iter()
                    .filter_map(|e| e.ok())
                    .filter(|e| {
                        e.path().is_file() && e.path().extension().is_some_and(|ext| ext == "md")
                    })
                    .map(|e| e.path().to_path_buf())
                    .collect()
            } else {
                vec![path.clone()]
            };

            let mut valid_count = 0;
            let mut invalid_count = 0;

            for file in &files {
                match parse_email_draft(file) {
                    Ok(draft) => match validate_draft(&draft) {
                        Ok(warnings) => {
                            print!("{} {}", "✓".green(), file.display());
                            if !warnings.is_empty() {
                                print!(" ({})", warnings.join(", ").yellow());
                            }
                            println!();
                            valid_count += 1;
                        }
                        Err(e) => {
                            println!("{} {} - {}", "✗".red(), file.display(), e);
                            invalid_count += 1;
                        }
                    },
                    Err(e) => {
                        println!("{} {} - {}", "✗".red(), file.display(), e);
                        invalid_count += 1;
                    }
                }
            }

            println!(
                "\nValidation complete: {} valid, {} invalid",
                valid_count.to_string().green(),
                invalid_count.to_string().red()
            );

            if invalid_count > 0 {
                std::process::exit(1);
            }
        }

        Some(Commands::MarkApproved { file }) => {
            mark_as_approved(&file)?;
        }

        Some(Commands::New { name }) => {
            // Add .md extension if not present
            let file_name = if Path::new(&name).extension().is_some() {
                name.clone()
            } else {
                format!("{}.md", name)
            };
            let path = PathBuf::from(&file_name);

            // Check if file already exists
            if path.exists() {
                return Err(anyhow!("File already exists: {}", path.display()));
            }

            // Create skeleton content with all frontmatter fields
            let skeleton = r#"---
to:
cc:
bcc:
subject:
status: draft
from:
reply_to:
attachments: []
---

"#;

            fs::write(&path, skeleton)?;
            println!("{} Created new draft: {}", "✓".green(), path.display());
        }

        None => {
            // Default: preview mode (dry run)
            if let Some(file) = cli.file {
                let draft = parse_email_draft(&file)?;
                preview_draft(
                    &draft,
                    &smtp_config,
                    &email_config,
                    signature_content.as_deref(),
                    true,
                )?;
            } else {
                // No file provided, show help
                println!("email-cli - A CLI tool for sending emails from Markdown drafts\n");
                println!("Usage:");
                println!("  email-cli <file>              Preview a draft (dry-run)");
                println!("  email-cli send <file>         Send a single approved email");
                println!("  email-cli send-approved <dir> Send all approved emails");
                println!("  email-cli list <dir>          List emails by status");
                println!("  email-cli validate <path>     Validate YAML frontmatter");
                println!("  email-cli mark-approved <file> Mark as approved");
                println!("  email-cli new <name>          Create a new draft from template");
                println!("\nOptions:");
                println!("  -s, --signature <name>  Use a specific signature");
                println!("  --no-signature          Skip signature entirely");
                println!("\nRun 'email-cli --help' for more information.");
            }
        }
    }

    Ok(())
}
