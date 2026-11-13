//! The outbox's IMAP adapter: the Sent mailbox as [`SentMailbox`] sees it.
//!
//! Two operations, both driven by [`crate::outbox::drain`]: the Message-ID
//! dedup search that makes a retry safe, and the APPEND itself.
//!
//! One session is opened on first use and reused for the whole drain, so a
//! backlog of three pending rows costs one login rather than three.
//!
//! ## APPENDUID
//!
//! The tagged `OK` is the acknowledgement the state machine rests on, and on a
//! server with UIDPLUS it carries the new message's UID as `APPENDUID`, which
//! [`super::uidplus::append`] reads off it and stores on the row (PERSO-202).
//! A server without UIDPLUS answers a plain `OK`, and the UID is then recovered
//! with the same `UID SEARCH HEADER MESSAGE-ID` the dedup path runs.
//!
//! ## A refusal is not a failure to reach the server
//!
//! A `NO` or `BAD` the server answers to the `SELECT`, the `UID SEARCH` or
//! the `APPEND` comes back carrying [`SentRefused`], and every other error
//! (no connection, a login that failed, a timeout, a connection that broke
//! mid-command) does not: the outbox gives up a discarded row's copy only on
//! the server's own refusal (PERSO-304).

use anyhow::{anyhow, Result};
use log::{info, warn};

use super::{open_imap_session, ImapSession};
use crate::config::ImapConfig;
use crate::outbox::{SentMailbox, SentRefused};
use crate::timing::TimingSpan;

/// A live IMAP Sent mailbox for one account.
pub struct ImapSentMailbox {
    config: ImapConfig,
    session: Option<ImapSession>,
}

impl ImapSentMailbox {
    pub fn new(config: ImapConfig) -> Self {
        Self {
            config,
            session: None,
        }
    }

    /// The shared session, opened on first use.
    async fn session(&mut self) -> Result<&mut ImapSession> {
        if self.session.is_none() {
            self.session = Some(open_imap_session(&self.config).await?);
        }
        Ok(self.session.as_mut().expect("session just opened"))
    }

    /// Log out if a session was ever opened. Errors are ignored: the server
    /// closes an abandoned connection by itself.
    pub async fn close(mut self) {
        if let Some(mut session) = self.session.take() {
            session.logout().await.ok();
        }
    }

    /// Drop the session so the next call reconnects. Used after a failure,
    /// where the connection may be half-dead.
    fn invalidate(&mut self) {
        self.session = None;
    }
}

impl SentMailbox for ImapSentMailbox {
    async fn search_message_id(&mut self, mailbox: &str, message_id: &str) -> Result<Vec<u32>> {
        let mut span = TimingSpan::with_context("outbox_sent_search", mailbox.to_string());
        let result = async {
            let session = self.session().await?;
            session
                .select(mailbox)
                .await
                .map_err(|e| answered(e, format!("Failed to select {mailbox}")))?;
            let query = format!("HEADER Message-ID \"{}\"", message_id);
            let uids = session
                .uid_search(&query)
                .await
                .map_err(|e| answered(e, format!("UID SEARCH in {mailbox} failed")))?;
            let mut uids: Vec<u32> = uids.into_iter().collect();
            uids.sort_unstable();
            Ok(uids)
        }
        .await;
        if result.is_err() {
            self.invalidate();
        }
        span.mark("searched");
        result
    }

    async fn append(&mut self, mailbox: &str, raw: &[u8]) -> Result<Option<u32>> {
        let mut span = TimingSpan::with_context("outbox_append", mailbox.to_string());
        let message_id = crate::send::message_id_of(raw);
        let appended = async {
            let session = self.session().await?;
            super::uidplus::append(session, mailbox, "(\\Seen)", raw)
                .await
                .map_err(|e| refused_append(e, mailbox))
        }
        .await;
        let appended = match appended {
            Ok(appended) => appended,
            Err(e) => {
                self.invalidate();
                return Err(e);
            }
        };
        span.mark("appended");
        info!("[outbox] appended {message_id} to '{mailbox}'");
        if let Some(new) = appended {
            return Ok(Some(new.uid));
        }

        // The tagged OK above is the acknowledgement; without UIDPLUS this
        // recovers the UID that APPENDUID would have carried.
        match self.search_message_id(mailbox, &message_id).await {
            Ok(uids) => Ok(uids.last().copied()),
            Err(e) => {
                warn!("[outbox] appended to '{mailbox}' but could not read back the UID: {e:#}");
                Ok(None)
            }
        }
    }
}

/// The RFC 5530 response codes that make a server's `NO` say nothing about
/// the message or the mailbox: the server is busy or broken (`UNAVAILABLE`,
/// `INUSE`, `LIMIT`, `SERVERBUG`), or the session is the matter
/// (`CONTACTADMIN`, `AUTHENTICATIONFAILED`, `AUTHORIZATIONFAILED`, `EXPIRED`,
/// `PRIVACYREQUIRED`). Such a `NO` counts as a server not reached, never as
/// a refusal of the copy (PERSO-304).
const NOT_ABOUT_THE_COPY: [&str; 9] = [
    "UNAVAILABLE",
    "INUSE",
    "LIMIT",
    "SERVERBUG",
    "CONTACTADMIN",
    "AUTHENTICATIONFAILED",
    "AUTHORIZATIONFAILED",
    "EXPIRED",
    "PRIVACYREQUIRED",
];

/// Whether the server's answer `answer` carries a response code of
/// [`NOT_ABOUT_THE_COPY`]: the first bracketed atom of the text, which is
/// where RFC 5530's codes stay, imap-proto not parsing them (async-imap
/// renders a `NO` as `code: None, info: Some("[UNAVAILABLE] ...")`, and
/// [`super::uidplus::AppendRefused::answer`] keeps the server's words).
/// Ignores case, as IMAP atoms do.
fn not_about_the_copy(answer: &str) -> bool {
    let Some((_, rest)) = answer.split_once('[') else {
        return false;
    };
    let code = rest.split([']', ' ']).next().unwrap_or_default();
    NOT_ABOUT_THE_COPY
        .iter()
        .any(|temporary| code.eq_ignore_ascii_case(temporary))
}

/// An `async-imap` error of the Sent search as the outbox reads it: the
/// server's `NO` or `BAD` is a [`SentRefused`], unless its response code
/// says it is not about the copy ([`not_about_the_copy`]); anything else a
/// plain error, a server not reached.
fn answered(error: async_imap::error::Error, what: String) -> anyhow::Error {
    match error {
        async_imap::error::Error::No(why) | async_imap::error::Error::Bad(why)
            if !not_about_the_copy(&why) =>
        {
            anyhow::Error::new(SentRefused(format!("{what}: {why}")))
        }
        other => anyhow!("{what}: {other}"),
    }
}

/// An APPEND error as the outbox reads it: the server's own refusal
/// ([`super::uidplus::AppendRefused`]) is a [`SentRefused`], unless its
/// response code says it is not about the copy; anything else a plain
/// error.
fn refused_append(error: anyhow::Error, mailbox: &str) -> anyhow::Error {
    if let Some(refused) = error.downcast_ref::<super::uidplus::AppendRefused>() {
        if !not_about_the_copy(&refused.answer) {
            return anyhow::Error::new(SentRefused(format!("{error}")));
        }
    }
    anyhow!("Failed to APPEND to '{mailbox}': {error}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outbox::is_sent_refusal;

    /// The server's `NO` or `BAD` to the search or the APPEND is a refusal;
    /// a broken connection, an I/O error, any other failure, and a `NO`
    /// whose RFC 5530 code says the server is busy or the session is the
    /// matter are not.
    #[test]
    fn only_the_servers_own_answer_about_the_copy_is_a_refusal() {
        use async_imap::error::Error;
        let what = || "Failed to select Sent".to_string();
        let no = |text: &str| Error::No(format!("code: None, info: Some(\"{text}\")"));
        assert!(is_sent_refusal(&answered(
            no("Mailbox doesn't exist"),
            what()
        )));
        assert!(is_sent_refusal(&answered(no("[NONEXISTENT] no"), what())));
        assert!(is_sent_refusal(&answered(
            Error::Bad("syntax".into()),
            what()
        )));
        for temporary in NOT_ABOUT_THE_COPY {
            let answer = no(&format!("[{}] later", temporary.to_lowercase()));
            assert!(!is_sent_refusal(&answered(answer, what())), "{temporary}");
        }
        assert!(!is_sent_refusal(&answered(Error::ConnectionLost, what())));
        let io = std::io::Error::new(std::io::ErrorKind::TimedOut, "timed out");
        assert!(!is_sent_refusal(&answered(Error::Io(io), what())));

        let append = |answer: &str| {
            refused_append(
                anyhow::Error::new(super::super::uidplus::AppendRefused {
                    mailbox: "[Gmail]/Sent Mail".into(),
                    answer: answer.into(),
                }),
                "[Gmail]/Sent Mail",
            )
        };
        assert!(is_sent_refusal(&append("[OVERQUOTA] mailbox full")));
        assert!(is_sent_refusal(&append("APPEND refused")));
        assert!(!is_sent_refusal(&append("[UNAVAILABLE] try again later")));
        assert!(!is_sent_refusal(&append("[INUSE] mailbox locked")));
        let broken = anyhow!("the connection closed during the APPEND to 'Sent'");
        assert!(!is_sent_refusal(&refused_append(broken, "Sent")));
    }
}
