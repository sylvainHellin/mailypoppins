use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use futures::TryStreamExt;
use gray_matter::{engine::YAML, Matter};
use log::info;

use super::{ImapSession, open_imap_session};
use crate::config::ImapConfig;
use crate::parse::attachments_dir_for;
use crate::types::InboxFrontmatter;

/// SEARCH + UID COPY + UID STORE \Deleted on an existing session.
/// Does NOT EXPUNGE -- caller batches a single EXPUNGE at the end.
async fn archive_single_on_session(
    session: &mut ImapSession,
    message_id: &str,
    archive_mailbox: &str,
) -> Result<()> {
    let query = format!("HEADER Message-ID \"{}\"", message_id);
    let uids = session
        .uid_search(&query)
        .await
        .map_err(|e| anyhow!("IMAP search failed: {}", e))?;

    if uids.is_empty() {
        info!(
            "Message-ID {} not found in INBOX (already moved on server), skipping server-side archive",
            message_id
        );
        return Ok(());
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

    Ok(())
}

/// SEARCH + UID STORE \Deleted on an existing session.
/// Does NOT EXPUNGE -- caller batches a single EXPUNGE at the end.
async fn delete_single_on_session(
    session: &mut ImapSession,
    message_id: &str,
) -> Result<()> {
    let query = format!("HEADER Message-ID \"{}\"", message_id);
    let uids = session
        .uid_search(&query)
        .await
        .map_err(|e| anyhow!("IMAP search failed: {}", e))?;

    if uids.is_empty() {
        info!(
            "Message-ID {} not found in INBOX (already removed on server), skipping server-side delete",
            message_id
        );
        return Ok(());
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

    Ok(())
}

/// Archive multiple emails using a single IMAP connection.
pub async fn batch_archive_emails_locally(
    imap_config: &ImapConfig,
    archive_dir: &Path,
    paths: &[PathBuf],
    archive_mailbox: &str,
) -> Vec<(PathBuf, Result<()>)> {
    let mut results: Vec<(PathBuf, Result<()>)> = Vec::with_capacity(paths.len());

    if let Err(e) = fs::create_dir_all(archive_dir) {
        return paths
            .iter()
            .map(|p| (p.clone(), Err(anyhow!("Failed to create archive dir: {}", e))))
            .collect();
    }

    let matter = Matter::<YAML>::new();

    struct LocalPrep {
        idx: usize,
        message_id: String,
        original_content: String,
        dest: PathBuf,
        had_html: bool,
        html_companion: PathBuf,
        html_dest: PathBuf,
        had_att_dir: bool,
        att_source: PathBuf,
        att_dest: PathBuf,
    }

    let mut prepared: Vec<LocalPrep> = Vec::new();

    // Phase 1: Local operations
    for (i, path) in paths.iter().enumerate() {
        let local_result: Result<LocalPrep> = (|| {
            let content = fs::read_to_string(path)
                .with_context(|| format!("Failed to read file: {}", path.display()))?;

            let parsed = matter.parse(&content);
            let inbox_fm: InboxFrontmatter = parsed
                .data
                .ok_or_else(|| anyhow!("No frontmatter found in {}", path.display()))?
                .deserialize()
                .context("Failed to parse frontmatter")?;

            let message_id = inbox_fm.message_id.ok_or_else(|| {
                anyhow!("No message_id in frontmatter -- cannot archive on server")
            })?;

            let new_content = content.replacen("status: inbox", "status: archived", 1);
            let file_name = path
                .file_name()
                .ok_or_else(|| anyhow!("Invalid file path"))?;
            let dest = archive_dir.join(file_name);

            fs::write(&dest, &new_content)?;
            fs::remove_file(path)?;

            let html_companion = path.with_extension("html");
            let html_dest = dest.with_extension("html");
            let had_html = html_companion.exists();
            if had_html {
                fs::copy(&html_companion, &html_dest)?;
                fs::remove_file(&html_companion)?;
            }

            let att_source = attachments_dir_for(path);
            let att_dest = attachments_dir_for(&dest);
            let had_att_dir = att_source.is_dir();
            if had_att_dir {
                fs::rename(&att_source, &att_dest)?;
            }

            Ok(LocalPrep {
                idx: i,
                message_id,
                original_content: content,
                dest,
                had_html,
                html_companion,
                html_dest,
                had_att_dir,
                att_source,
                att_dest,
            })
        })();

        match local_result {
            Ok(prep) => prepared.push(prep),
            Err(e) => results.push((path.clone(), Err(e))),
        }
    }

    if prepared.is_empty() {
        return results;
    }

    // Phase 2: IMAP operations (single connection)
    let mut imap_results: Vec<Result<()>> = Vec::with_capacity(prepared.len());

    match open_imap_session(imap_config).await {
        Ok(mut session) => {
            match session
                .select("INBOX")
                .await
                .map_err(|e| anyhow!("Failed to select INBOX: {}", e))
            {
                Ok(_) => {
                    for prep in &prepared {
                        imap_results.push(archive_single_on_session(
                            &mut session,
                            &prep.message_id,
                            archive_mailbox,
                        ).await);
                    }
                    if imap_results.iter().any(Result::is_ok) {
                        match session.expunge().await {
                            Ok(stream) => {
                                if let Err(e) = stream.try_collect::<Vec<_>>().await {
                                    info!("Batch EXPUNGE collect failed (non-fatal): {}", e);
                                }
                            }
                            Err(e) => {
                                info!("Batch EXPUNGE failed (non-fatal): {}", e);
                            }
                        }
                    }
                }
                Err(e) => {
                    let msg = format!("{}", e);
                    for _ in &prepared {
                        imap_results.push(Err(anyhow!("{}", msg)));
                    }
                }
            }
            session.logout().await.ok();
        }
        Err(e) => {
            let msg = format!("IMAP connection failed: {}", e);
            for _ in &prepared {
                imap_results.push(Err(anyhow!("{}", msg)));
            }
        }
    }

    // Phase 3: Process IMAP results, rollback failures
    for (prep, imap_result) in prepared.into_iter().zip(imap_results) {
        let path = paths[prep.idx].clone();
        match imap_result {
            Ok(()) => results.push((path, Ok(()))),
            Err(e) => {
                info!(
                    "IMAP archive failed for {}, rolling back: {}",
                    path.display(),
                    e
                );
                let _ = fs::write(&path, &prep.original_content);
                let _ = fs::remove_file(&prep.dest);
                if prep.had_html {
                    let _ = fs::copy(&prep.html_dest, &prep.html_companion);
                    let _ = fs::remove_file(&prep.html_dest);
                }
                if prep.had_att_dir {
                    let _ = fs::rename(&prep.att_dest, &prep.att_source);
                }
                results.push((
                    path,
                    Err(e.context("IMAP archive failed; local changes rolled back")),
                ));
            }
        }
    }

    results
}

/// Delete multiple emails using a single IMAP connection.
pub async fn batch_delete_emails_locally(
    imap_config: &ImapConfig,
    paths: &[PathBuf],
) -> Vec<(PathBuf, Result<()>)> {
    let mut results: Vec<(PathBuf, Result<()>)> = Vec::with_capacity(paths.len());

    let matter = Matter::<YAML>::new();

    struct LocalPrep {
        idx: usize,
        message_id: Option<String>,
        original_content: String,
        html_content: Option<String>,
        html_companion: PathBuf,
        had_att_dir: bool,
        att_dir: PathBuf,
        att_backup: PathBuf,
    }

    let mut prepared: Vec<LocalPrep> = Vec::new();

    // Phase 1: Local operations
    for (i, path) in paths.iter().enumerate() {
        let local_result: Result<LocalPrep> = (|| {
            let content = fs::read_to_string(path)
                .with_context(|| format!("Failed to read file: {}", path.display()))?;

            let parsed = matter.parse(&content);
            let message_id: Option<String> = parsed
                .data
                .and_then(|d| {
                    d.deserialize::<std::collections::HashMap<String, serde_yaml::Value>>()
                        .ok()
                })
                .and_then(|map| {
                    map.get("message_id")
                        .and_then(|v| v.as_str().map(|s| s.to_string()))
                });

            let html_companion = path.with_extension("html");
            let html_content = if html_companion.exists() {
                Some(fs::read_to_string(&html_companion)?)
            } else {
                None
            };

            let att_dir = attachments_dir_for(path);
            let att_backup = att_dir.with_extension("deleting");
            let had_att_dir = att_dir.is_dir();
            if had_att_dir {
                fs::rename(&att_dir, &att_backup)?;
            }

            fs::remove_file(path)
                .with_context(|| format!("Failed to remove local file: {}", path.display()))?;

            if html_content.is_some() {
                fs::remove_file(&html_companion).ok();
            }

            Ok(LocalPrep {
                idx: i,
                message_id,
                original_content: content,
                html_content,
                html_companion,
                had_att_dir,
                att_dir,
                att_backup,
            })
        })();

        match local_result {
            Ok(prep) => prepared.push(prep),
            Err(e) => results.push((path.clone(), Err(e))),
        }
    }

    if prepared.is_empty() {
        return results;
    }

    let has_any_imap = prepared.iter().any(|p| p.message_id.is_some());

    if !has_any_imap {
        for prep in prepared {
            info!(
                "No message_id -- skipping server deletion for {}",
                paths[prep.idx].display()
            );
            results.push((paths[prep.idx].clone(), Ok(())));
        }
        return results;
    }

    // Phase 2: IMAP operations (single connection)
    let mut imap_results: Vec<Result<()>> = Vec::with_capacity(prepared.len());

    match open_imap_session(imap_config).await {
        Ok(mut session) => {
            match session
                .select("INBOX")
                .await
                .map_err(|e| anyhow!("Failed to select INBOX: {}", e))
            {
                Ok(_) => {
                    for prep in &prepared {
                        if let Some(ref mid) = prep.message_id {
                            imap_results.push(delete_single_on_session(&mut session, mid).await);
                        } else {
                            imap_results.push(Ok(()));
                        }
                    }
                    if imap_results.iter().any(Result::is_ok) {
                        match session.expunge().await {
                            Ok(stream) => {
                                if let Err(e) = stream.try_collect::<Vec<_>>().await {
                                    info!("Batch EXPUNGE collect failed (non-fatal): {}", e);
                                }
                            }
                            Err(e) => {
                                info!("Batch EXPUNGE failed (non-fatal): {}", e);
                            }
                        }
                    }
                }
                Err(e) => {
                    let msg = format!("{}", e);
                    for prep in &prepared {
                        if prep.message_id.is_some() {
                            imap_results.push(Err(anyhow!("{}", msg)));
                        } else {
                            imap_results.push(Ok(()));
                        }
                    }
                }
            }
            session.logout().await.ok();
        }
        Err(e) => {
            let msg = format!("IMAP connection failed: {}", e);
            for prep in &prepared {
                if prep.message_id.is_some() {
                    imap_results.push(Err(anyhow!("{}", msg)));
                } else {
                    imap_results.push(Ok(()));
                }
            }
        }
    }

    // Phase 3: Process results, rollback failures
    for (prep, imap_result) in prepared.into_iter().zip(imap_results) {
        let path = paths[prep.idx].clone();
        match imap_result {
            Ok(()) => {
                if prep.message_id.is_none() {
                    info!(
                        "No message_id -- skipping server deletion for {}",
                        path.display()
                    );
                }
                if prep.had_att_dir && prep.att_backup.exists() {
                    let _ = fs::remove_dir_all(&prep.att_backup);
                }
                results.push((path, Ok(())));
            }
            Err(e) => {
                info!(
                    "IMAP delete failed for {}, rolling back: {}",
                    path.display(),
                    e
                );
                let _ = fs::write(&path, &prep.original_content);
                if let Some(ref html) = prep.html_content {
                    let _ = fs::write(&prep.html_companion, html);
                }
                if prep.had_att_dir {
                    let _ = fs::rename(&prep.att_backup, &prep.att_dir);
                }
                results.push((
                    path,
                    Err(e.context("IMAP delete failed; local changes rolled back")),
                ));
            }
        }
    }

    results
}
