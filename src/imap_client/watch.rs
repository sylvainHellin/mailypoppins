use anyhow::{anyhow, Result};
use async_imap::extensions::idle::IdleResponse;
use log::info;

use super::open_imap_session;
use crate::config::ImapConfig;

pub async fn watch_mailbox(imap_config: &ImapConfig, mailbox: &str, timeout: Option<u64>) -> Result<i32> {
    let mut session = open_imap_session(imap_config).await?;

    session
        .select(mailbox)
        .await
        .map_err(|e| anyhow!("Failed to select mailbox '{}': {}", mailbox, e))?;

    info!("Watching mailbox '{}' for changes (IMAP IDLE)", mailbox);

    let exit_code = match timeout {
        None => {
            let mut handle = session.idle();
            handle.init().await?;
            let (fut, _stop) = handle.wait_with_timeout(std::time::Duration::from_secs(29 * 60));
            let response = fut.await?;
            session = handle.done().await?;

            match response {
                IdleResponse::NewData(_) => 0,
                IdleResponse::Timeout | IdleResponse::ManualInterrupt => 1,
            }
        }
        Some(timeout_secs) => {
            let total_timeout = std::time::Duration::from_secs(timeout_secs);
            let idle_interval = std::time::Duration::from_secs(29.min(timeout_secs));
            let start = std::time::Instant::now();

            loop {
                let remaining = total_timeout.saturating_sub(start.elapsed());
                if remaining.is_zero() {
                    break 2;
                }

                let wait_duration = idle_interval.min(remaining);

                let mut handle = session.idle();
                handle.init().await?;
                let (fut, _stop) = handle.wait_with_timeout(wait_duration);
                let response = fut.await?;
                session = handle.done().await?;

                match response {
                    IdleResponse::NewData(_) => break 0,
                    IdleResponse::Timeout => continue,
                    IdleResponse::ManualInterrupt => continue,
                }
            }
        }
    };

    session.logout().await.ok();

    Ok(exit_code)
}
