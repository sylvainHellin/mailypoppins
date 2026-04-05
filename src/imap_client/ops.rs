use std::fs;
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use futures::TryStreamExt;
use gray_matter::{engine::YAML, Matter};
use log::info;

use super::open_imap_session;
use crate::config::ImapConfig;
use crate::parse::attachments_dir_for;
use crate::types::InboxFrontmatter;

pub async fn append_to_sent_folder(imap_config: &ImapConfig, raw_message: &[u8], sent_mailbox: &str) -> Result<()> {
    info!("Appending sent email to IMAP folder '{}'", sent_mailbox);

    let mut session = open_imap_session(imap_config).await?;

    session.append(sent_mailbox, Some("(\\Seen)"), None, raw_message)
        .await
        .map_err(|e| anyhow!("Failed to APPEND to '{}': {}", sent_mailbox, e))?;

    session.logout().await.ok();
    info!("Successfully appended to '{}'", sent_mailbox);
    Ok(())
}

pub async fn archive_email_on_server(imap_config: &ImapConfig, message_id: &str, archive_mailbox: &str) -> Result<()> {
    info!("Archiving email on server: Message-ID={} -> {}", message_id, archive_mailbox);
    let mut session = open_imap_session(imap_config).await?;

    session
        .select("INBOX")
        .await
        .map_err(|e| anyhow!("Failed to select INBOX: {}", e))?;

    let query = format!("HEADER Message-ID \"{}\"", message_id);
    let uids = session
        .uid_search(&query)
        .await
        .map_err(|e| anyhow!("IMAP search failed: {}", e))?;

    if uids.is_empty() {
        session.logout().await.ok();
        return Err(anyhow!(
            "Email with Message-ID {} not found in INBOX on server",
            message_id
        ));
    }

    let uid = *uids.iter().next().expect("uids verified non-empty");
    let uid_str = uid.to_string();

    session
        .uid_copy(&uid_str, archive_mailbox)
        .await
        .map_err(|e| anyhow!("Failed to copy email to {}: {}", archive_mailbox, e))?;

    session
        .uid_store(&uid_str, "+FLAGS (\\Deleted)")
        .await
        .map_err(|e| anyhow!("Failed to mark email as deleted: {}", e))?
        .try_collect::<Vec<_>>()
        .await
        .map_err(|e| anyhow!("Failed to collect store response: {}", e))?;

    session
        .expunge()
        .await
        .map_err(|e| anyhow!("Failed to expunge: {}", e))?
        .try_collect::<Vec<_>>()
        .await
        .map_err(|e| anyhow!("Failed to collect expunge response: {}", e))?;

    session.logout().await.ok();
    Ok(())
}

pub async fn archive_email_locally(imap_config: &ImapConfig, archive_dir: &Path, file_path: &Path, archive_mailbox: &str) -> Result<()> {
    info!("Archiving email locally: {}", file_path.display());
    let content = fs::read_to_string(file_path)
        .with_context(|| format!("Failed to read file: {}", file_path.display()))?;

    let matter = Matter::<YAML>::new();
    let parsed = matter.parse(&content);
    let inbox_fm: InboxFrontmatter = parsed
        .data
        .ok_or_else(|| anyhow!("No frontmatter found in {}", file_path.display()))?
        .deserialize()
        .context("Failed to parse frontmatter")?;

    let message_id = inbox_fm.message_id.as_ref().ok_or_else(|| {
        anyhow!("No message_id in frontmatter -- cannot archive on server")
    })?;

    fs::create_dir_all(archive_dir)?;

    let new_content = content.replacen("status: inbox", "status: archived", 1);

    let file_name = file_path
        .file_name()
        .ok_or_else(|| anyhow!("Invalid file path"))?;
    let dest = archive_dir.join(file_name);

    fs::write(&dest, &new_content)?;
    fs::remove_file(file_path)?;

    let html_companion = file_path.with_extension("html");
    let html_dest = dest.with_extension("html");
    let had_html = html_companion.exists();
    if had_html {
        fs::copy(&html_companion, &html_dest)?;
        fs::remove_file(&html_companion)?;
    }

    let att_source = attachments_dir_for(file_path);
    let att_dest = attachments_dir_for(&dest);
    let had_att_dir = att_source.is_dir();
    if had_att_dir {
        fs::rename(&att_source, &att_dest)?;
    }

    if let Err(e) = archive_email_on_server(imap_config, message_id, archive_mailbox).await {
        info!("IMAP archive failed, rolling back local changes: {}", e);
        fs::write(file_path, &content)?;
        fs::remove_file(&dest)?;
        if had_html {
            fs::copy(&html_dest, &html_companion)?;
            fs::remove_file(&html_dest)?;
        }
        if had_att_dir {
            fs::rename(&att_dest, &att_source).ok();
        }
        return Err(e.context("IMAP archive failed; local changes have been rolled back"));
    }

    Ok(())
}

pub async fn delete_email_on_server(imap_config: &ImapConfig, message_id: &str) -> Result<()> {
    info!("Deleting email on server: Message-ID={}", message_id);
    let mut session = open_imap_session(imap_config).await?;

    session
        .select("INBOX")
        .await
        .map_err(|e| anyhow!("Failed to select INBOX: {}", e))?;

    let query = format!("HEADER Message-ID \"{}\"", message_id);
    let uids = session
        .uid_search(&query)
        .await
        .map_err(|e| anyhow!("IMAP search failed: {}", e))?;

    if uids.is_empty() {
        session.logout().await.ok();
        return Err(anyhow!(
            "Email with Message-ID {} not found in INBOX on server",
            message_id
        ));
    }

    let uid = *uids.iter().next().expect("uids verified non-empty");
    let uid_str = uid.to_string();

    session
        .uid_store(&uid_str, "+FLAGS (\\Deleted)")
        .await
        .map_err(|e| anyhow!("Failed to mark email as deleted: {}", e))?
        .try_collect::<Vec<_>>()
        .await
        .map_err(|e| anyhow!("Failed to collect store response: {}", e))?;

    session
        .expunge()
        .await
        .map_err(|e| anyhow!("Failed to expunge: {}", e))?
        .try_collect::<Vec<_>>()
        .await
        .map_err(|e| anyhow!("Failed to collect expunge response: {}", e))?;

    session.logout().await.ok();
    Ok(())
}

pub async fn delete_email_locally(imap_config: &ImapConfig, file_path: &Path) -> Result<()> {
    info!("Deleting email locally: {}", file_path.display());
    let content = fs::read_to_string(file_path)
        .with_context(|| format!("Failed to read file: {}", file_path.display()))?;

    let matter = Matter::<YAML>::new();
    let parsed = matter.parse(&content);
    let message_id: Option<String> = parsed
        .data
        .and_then(|d| d.deserialize::<std::collections::HashMap<String, serde_yaml::Value>>().ok())
        .and_then(|map| map.get("message_id").and_then(|v| v.as_str().map(|s| s.to_string())));

    let html_companion = file_path.with_extension("html");
    let html_content = if html_companion.exists() {
        Some(fs::read_to_string(&html_companion)?)
    } else {
        None
    };

    let att_dir = attachments_dir_for(file_path);
    let att_backup = att_dir.with_extension("deleting");
    let had_att_dir = att_dir.is_dir();
    if had_att_dir {
        fs::rename(&att_dir, &att_backup)?;
    }

    fs::remove_file(file_path)
        .with_context(|| format!("Failed to remove local file: {}", file_path.display()))?;

    if html_content.is_some() {
        fs::remove_file(&html_companion).ok();
    }

    if let Some(ref mid) = message_id {
        if let Err(e) = delete_email_on_server(imap_config, mid).await {
            info!("IMAP delete failed, rolling back local changes: {}", e);
            fs::write(file_path, &content)?;
            if let Some(ref html) = html_content {
                fs::write(&html_companion, html)?;
            }
            if had_att_dir {
                fs::rename(&att_backup, &att_dir).ok();
            }
            return Err(e.context("IMAP delete failed; local changes have been rolled back"));
        }
    } else {
        info!(
            "No message_id in frontmatter -- skipping server deletion for {}",
            file_path.display()
        );
    }

    if had_att_dir && att_backup.exists() {
        fs::remove_dir_all(&att_backup).ok();
    }

    Ok(())
}
