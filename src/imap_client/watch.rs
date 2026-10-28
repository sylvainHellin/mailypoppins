//! One IMAP IDLE round over a mailbox: what `mp watch` and the daemon's
//! per-account watcher wait on.

use anyhow::{anyhow, Result};
use async_imap::extensions::idle::IdleResponse;
use log::{info, warn};

use super::open_imap_session;
use crate::config::ImapConfig;
use crate::sync::MailboxState;

/// Wait for `mailbox` to change, for at most `timeout` seconds.
///
/// Returns 0 when it changed, 2 when a bounded round expired and 1 when an
/// unbounded one did.
pub async fn watch_mailbox(
    imap_config: &ImapConfig,
    mailbox: &str,
    timeout: Option<u64>,
) -> Result<i32> {
    watch_mailbox_since(imap_config, mailbox, timeout, None)
        .await
        .map(|(code, _)| code)
}

/// [`watch_mailbox`], for a caller that keeps watching round after round and
/// acts on every change in between (PERSO-240).
///
/// IDLE reports what happens while it runs, and nothing that happened before
/// the round's SELECT. A watcher that ticks on a change and then opens its
/// next round therefore never hears about a change that landed while it was
/// ticking: the second leg of two back-to-back moves, or a message arriving
/// mid-tick, waits for the next scheduled tick. So the round also answers with
/// the mailbox's state as of the change it reports (a fresh SELECT taken once
/// IDLE has returned), or as of its own SELECT when it expired, and a caller
/// that hands that back as `since` gets a round that reports a change at once
/// when the mailbox no longer looks the way it did.
///
/// The state is `EXISTS`, `UIDNEXT` and `UIDVALIDITY`, which every arrival,
/// expunge and renumbering moves. A change landing between the fresh SELECT
/// and the tick's own listing is reported once more, which costs one quick
/// tick that finds nothing to do and then compares equal.
pub async fn watch_mailbox_since(
    imap_config: &ImapConfig,
    mailbox: &str,
    timeout: Option<u64>,
    since: Option<&MailboxState>,
) -> Result<(i32, MailboxState)> {
    let mut session = open_imap_session(imap_config).await?;

    let selected = session
        .select(mailbox)
        .await
        .map_err(|e| anyhow!("Failed to select mailbox '{}': {}", mailbox, e))?;
    let at_select = state_of(&selected);

    if changed_since(since, &at_select) {
        info!(
            "Mailbox '{}' changed since the last round ({:?} -> {:?})",
            mailbox, since, at_select
        );
        session.logout().await.ok();
        return Ok((0, at_select));
    }

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

    // A round that expired saw nothing move, so its SELECT still describes
    // the mailbox. One that saw a change reads the state the change left: a
    // failed read hands back the SELECT's, which only costs the next round
    // one tick that finds nothing new.
    let state = if exit_code == 0 {
        match session.select(mailbox).await {
            Ok(after) => state_of(&after),
            Err(e) => {
                warn!("Could not re-read '{}' after its change: {}", mailbox, e);
                at_select
            }
        }
    } else {
        at_select
    };

    session.logout().await.ok();

    Ok((exit_code, state))
}

/// Whether the mailbox no longer looks the way the previous round left it.
/// With no previous round there is nothing to compare, and nothing missed.
fn changed_since(since: Option<&MailboxState>, now: &MailboxState) -> bool {
    since.is_some_and(|since| since != now)
}

/// The part of a SELECT response a watch compares between rounds.
fn state_of(mailbox: &async_imap::types::Mailbox) -> MailboxState {
    MailboxState {
        uid_validity: mailbox.uid_validity,
        uid_next: mailbox.uid_next,
        exists: mailbox.exists,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(exists: u32, uid_next: u32) -> MailboxState {
        MailboxState {
            uid_validity: Some(7),
            uid_next: Some(uid_next),
            exists,
        }
    }

    /// PERSO-240: the second leg of two back-to-back moves expunges the copy
    /// the first leg delivered while the tick the first leg woke is running.
    /// The next round's SELECT has one message fewer than the state the
    /// first round left, which is a change no IDLE was running to report.
    #[test]
    fn an_expunge_during_the_tick_is_a_change() {
        assert!(changed_since(Some(&state(3, 291)), &state(2, 291)));
    }

    /// An arrival and an expunge between two rounds leave `EXISTS` alone and
    /// move `UIDNEXT`; a renumbering moves `UIDVALIDITY`.
    #[test]
    fn uidnext_and_uidvalidity_count_as_much_as_exists() {
        assert!(changed_since(Some(&state(3, 291)), &state(3, 292)));
        let renumbered = MailboxState {
            uid_validity: Some(8),
            ..state(3, 291)
        };
        assert!(changed_since(Some(&state(3, 291)), &renumbered));
    }

    /// A mailbox that looks as it did is no change, and the first round has
    /// nothing to compare against.
    #[test]
    fn an_unchanged_mailbox_and_a_first_round_report_nothing() {
        assert!(!changed_since(Some(&state(3, 291)), &state(3, 291)));
        assert!(!changed_since(None, &state(3, 291)));
    }
}
