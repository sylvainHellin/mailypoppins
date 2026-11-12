//! The logins the IMAP server refused, and the gate that keeps the daemon
//! from asking again (PERSO-296).
//!
//! A LOGIN answered with `NO` is the server's answer about the password, not
//! a blip: asking again with the same password cannot succeed, and a provider
//! may lock the account over a run of failed logins. Before this module the
//! daemon asked about six times a minute, through the pool's quick retries,
//! every sync tick, the drafts reconcile and the IDLE watcher, and its health
//! check still said the account was fine.
//!
//! Every login goes through [`super::open_imap_session`], which consults the
//! list kept here: a refusal is recorded under the server, the user and a hash
//! of the refused password, and while the entry is fresh the same login fails
//! at once with the same [`LoginRejected`] without dialling. The entry ends
//!
//! - after [`REFUSAL_HOLD`], so the server is asked again at most once an hour
//!   per login, which recovers on its own from a lockout that lifted and stays
//!   far below any lockout threshold;
//! - when a login to the same server and user succeeds;
//! - when the user stores a password (`config.set_password`) or starts a sync
//!   (`sync.quick`, `sync.full`), through [`forget`];
//! - when the daemon restarts, since the list lives in memory only.
//!
//! A new password is not on the list, because the hash is part of the entry,
//! so it is asked at once. `config.probe` dials past the gate
//! ([`super::dial_imap_session`]): a probe is the user asking the server.
//!
//! What counts as a refusal is [`is_rejection`]: a `NO` to LOGIN, unless its
//! response code is one RFC 5530 makes temporary.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use crate::config::{ImapConfig, ImapLogin};

/// How long a refused login stays refused before the server is asked again.
pub const REFUSAL_HOLD: Duration = Duration::from_secs(60 * 60);

/// The RFC 5530 response codes that make a `NO` to LOGIN a passing state of
/// the server rather than an answer about the password: it is down for
/// maintenance, the account is in use elsewhere, a connection limit was hit,
/// or the server failed. A login refused with one of these is retried as any
/// failed connection is.
const TEMPORARY_CODES: [&str; 4] = ["[UNAVAILABLE]", "[INUSE]", "[LIMIT]", "[SERVERBUG]"];

/// Whether a `NO` to LOGIN, as async-imap renders it, refuses the password.
///
/// The imap-proto parser in the tree does not know the RFC 5530 codes, so
/// they reach this point as the start of the answer's text
/// (`code: None, info: Some("[UNAVAILABLE] ...")`) and are matched there,
/// ignoring case as IMAP atoms do. A bare `NO`, `[AUTHENTICATIONFAILED]`,
/// `[AUTHORIZATIONFAILED]` and `[EXPIRED]` all refuse.
pub(super) fn is_rejection(answer: &str) -> bool {
    let answer = answer.to_ascii_uppercase();
    !TEMPORARY_CODES.iter().any(|code| answer.contains(code))
}

/// A LOGIN the server refused, either just now or within the hold.
///
/// Its text is the server's answer behind the words `IMAP login failed`, the
/// error a refused login has always surfaced as; one the gate answered from
/// the list says so, and says when the server is asked again.
#[derive(Clone, Debug)]
pub struct LoginRejected {
    /// The server's `NO`, as async-imap renders it.
    answer: String,
    /// Which entry of the list this refusal is, `None` until it is recorded.
    entry: Option<u64>,
    /// Whether the gate answered without dialling.
    from_list: bool,
}

impl LoginRejected {
    /// The refusal the server just sent.
    pub(crate) fn new(answer: String) -> Self {
        Self {
            answer,
            entry: None,
            from_list: false,
        }
    }

    /// The list entry this refusal belongs to: two errors with the same entry
    /// are one refusal, which a caller that logs every failure logs once.
    pub fn entry(&self) -> Option<u64> {
        self.entry
    }
}

impl fmt::Display for LoginRejected {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "IMAP login failed: {}", self.answer)?;
        if self.from_list {
            write!(
                f,
                " (the server refused this password earlier and is asked again within the \
                 hour, or at once after the password is set again or a sync is started)"
            )?;
        }
        Ok(())
    }
}

impl std::error::Error for LoginRejected {}

/// The refused login behind `error`, wherever in its chain it sits.
pub fn login_rejected(error: &anyhow::Error) -> Option<&LoginRejected> {
    error.chain().find_map(|cause| cause.downcast_ref())
}

/// Whether the list holds a fresh refusal of `login`, whatever the password:
/// what the health check and `account.credentials` read, neither of which
/// may read a secret.
pub fn is_refused(login: &ImapLogin) -> bool {
    refusals().holds(login, Instant::now(), REFUSAL_HOLD)
}

/// Drop the refusal of `login`, so its next login asks the server.
pub fn forget(login: &ImapLogin) {
    refusals().forget(login);
}

/// The refusal a login with `config` would get without dialling, if any.
pub(super) fn refused(config: &ImapConfig) -> Option<LoginRejected> {
    refusals().refused(
        &config.login(),
        &fingerprint(&config.password),
        Instant::now(),
        REFUSAL_HOLD,
    )
}

/// Record that the server refused `config`'s login, and answer the refusal
/// with its entry.
pub(super) fn record(config: &ImapConfig, rejected: &LoginRejected) -> LoginRejected {
    refusals().record(
        config.login(),
        fingerprint(&config.password),
        rejected,
        Instant::now(),
        REFUSAL_HOLD,
    )
}

/// A login to `config`'s server and user succeeded, so whatever the list
/// held about it no longer describes the server.
pub(super) fn accepted(config: &ImapConfig) {
    refusals().forget(&config.login());
}

/// The refused password, as the list compares it: never the password itself.
fn fingerprint(password: &str) -> [u8; 32] {
    Sha256::digest(password.as_bytes()).into()
}

fn refusals() -> MutexGuard<'static, Refusals> {
    static REFUSALS: OnceLock<Mutex<Refusals>> = OnceLock::new();
    // A panic while the lock was held cannot leave the map half written: every
    // critical section is one map operation.
    REFUSALS
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// One refused login.
struct Refusal {
    password: [u8; 32],
    at: Instant,
    entry: u64,
    answer: String,
}

/// The list itself, with the clock and the hold passed in so a test can step
/// past the hour without waiting for it.
#[derive(Default)]
struct Refusals {
    entries: HashMap<ImapLogin, Refusal>,
    next_entry: u64,
}

impl Refusals {
    fn fresh(refusal: &Refusal, now: Instant, hold: Duration) -> bool {
        now.saturating_duration_since(refusal.at) < hold
    }

    fn refused(
        &mut self,
        login: &ImapLogin,
        password: &[u8; 32],
        now: Instant,
        hold: Duration,
    ) -> Option<LoginRejected> {
        let refusal = self.entries.get(login)?;
        if !Self::fresh(refusal, now, hold) {
            self.entries.remove(login);
            return None;
        }
        (refusal.password == *password).then(|| LoginRejected {
            answer: refusal.answer.clone(),
            entry: Some(refusal.entry),
            from_list: true,
        })
    }

    fn record(
        &mut self,
        login: ImapLogin,
        password: [u8; 32],
        rejected: &LoginRejected,
        now: Instant,
        hold: Duration,
    ) -> LoginRejected {
        // Two dials that raced each other past the gate were refused once: the
        // second keeps the first's entry, so a caller logs the refusal once.
        let entry = match self.entries.get(&login) {
            Some(refusal) if refusal.password == password && Self::fresh(refusal, now, hold) => {
                refusal.entry
            }
            _ => {
                self.next_entry += 1;
                self.entries.insert(
                    login,
                    Refusal {
                        password,
                        at: now,
                        entry: self.next_entry,
                        answer: rejected.answer.clone(),
                    },
                );
                self.next_entry
            }
        };
        LoginRejected {
            entry: Some(entry),
            ..rejected.clone()
        }
    }

    fn forget(&mut self, login: &ImapLogin) {
        self.entries.remove(login);
    }

    fn holds(&self, login: &ImapLogin, now: Instant, hold: Duration) -> bool {
        self.entries
            .get(login)
            .is_some_and(|refusal| Self::fresh(refusal, now, hold))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The answers async-imap renders a tagged `NO` to LOGIN as: the code is
    /// `None` because imap-proto does not parse RFC 5530's, and they sit at
    /// the start of the text.
    fn no(info: &str) -> String {
        format!("code: None, info: Some({info:?})")
    }

    /// A bare `NO` and the RFC 5530 codes about the credential refuse the
    /// password; the four codes about the server's state do not, whatever
    /// their case.
    #[test]
    fn a_no_refuses_the_password_unless_its_code_says_the_server_is_unwell() {
        for refusal in [
            "LOGIN failed.",
            "[AUTHENTICATIONFAILED] LOGIN failed.",
            "[AUTHORIZATIONFAILED] No such authorization-ID",
            "[EXPIRED] That password isn't valid any more",
        ] {
            assert!(is_rejection(&no(refusal)), "{refusal}");
        }
        for passing in [
            "[UNAVAILABLE] User's backend down for maintenance",
            "[INUSE] Mailbox in use",
            "[LIMIT] Too many connections",
            "[SERVERBUG] Internal error",
            "[unavailable] try again later",
        ] {
            assert!(!is_rejection(&no(passing)), "{passing}");
        }
    }

    fn login(port: u16) -> ImapLogin {
        ImapLogin {
            host: "imap.example.com".into(),
            port,
            username: "alpha@example.com".into(),
        }
    }

    /// A refusal holds the password it refused for the hold and no longer;
    /// another password, or the same password after the hold, is asked.
    #[test]
    fn a_refusal_holds_its_password_for_the_hold_and_then_lapses() {
        let mut list = Refusals::default();
        let (now, hold) = (Instant::now(), Duration::from_secs(3600));
        let (wrong, other) = (fingerprint("wrong"), fingerprint("other"));
        let recorded = list.record(
            login(993),
            wrong,
            &LoginRejected::new("no".into()),
            now,
            hold,
        );

        let held = list
            .refused(&login(993), &wrong, now + hold / 2, hold)
            .expect("the same password is refused from the list");
        assert_eq!(held.entry(), recorded.entry());
        assert!(held.to_string().contains("asked again within the hour"));
        assert!(list.holds(&login(993), now + hold / 2, hold));
        assert!(list.refused(&login(993), &other, now, hold).is_none());
        assert!(list.refused(&login(143), &wrong, now, hold).is_none());

        assert!(!list.holds(&login(993), now + hold, hold));
        assert!(list
            .refused(&login(993), &wrong, now + hold, hold)
            .is_none());
        assert!(
            !list.holds(&login(993), now, hold),
            "a lapsed entry is dropped when it is found lapsed"
        );
    }

    /// Two refusals of one password inside the hold are one entry, a new
    /// refusal after the entry was forgotten is a new one.
    #[test]
    fn a_second_refusal_of_the_same_password_keeps_its_entry() {
        let mut list = Refusals::default();
        let (now, hold) = (Instant::now(), Duration::from_secs(3600));
        let rejected = LoginRejected::new("no".into());
        let first = list.record(login(993), fingerprint("wrong"), &rejected, now, hold);
        let raced = list.record(login(993), fingerprint("wrong"), &rejected, now, hold);
        assert_eq!(first.entry(), raced.entry());
        list.forget(&login(993));
        assert!(!list.holds(&login(993), now, hold));
        let again = list.record(login(993), fingerprint("wrong"), &rejected, now, hold);
        assert_ne!(first.entry(), again.entry());
    }

    /// The chain is searched, so a context added on the way up does not hide
    /// the refusal from the caller that has to act on it.
    #[test]
    fn a_refusal_is_found_under_a_context() {
        use anyhow::Context;
        let error = Err::<(), _>(LoginRejected::new("no".into()))
            .context("listing the folders")
            .unwrap_err();
        assert!(login_rejected(&error).is_some());
        assert!(login_rejected(&anyhow::anyhow!("IMAP login failed: no")).is_none());
    }
}
