//! The logins a server refused, and the gate that keeps the daemon from
//! asking again (PERSO-296).
//!
//! A login the server answered about the credential is not a blip: asking
//! again with the same password, or the same refresh token, cannot succeed,
//! and a provider may lock the account over a run of failed logins. Before
//! this module the daemon asked about six times a minute, through the IMAP
//! pool's quick retries, every sync tick, the drafts reconcile, the IDLE
//! watcher and the outbox drain, and its health check still said the account
//! was fine.
//!
//! Three logins go through the gate, each keyed by a [`Login`]:
//!
//! - the IMAP login, in [`crate::imap_client::open_imap_session`], refused by
//!   a `NO` about the password;
//! - the SMTP login, in [`crate::send::submit`], refused by a `535` or a reply
//!   carrying the enhanced code `5.7.8`;
//! - a Graph account's token refresh, in [`crate::graph::access_token`],
//!   refused by `invalid_grant` (the refresh token is revoked or expired).
//!
//! Each consults the list kept here: a refusal is recorded under the login
//! and a hash of the refused secret (the password, or the refresh token), and
//! while the entry is fresh the same login fails at once with the same
//! [`LoginRejected`] without dialling. The entry ends
//!
//! - after [`REFUSAL_HOLD`], so the server is asked again at most once an hour
//!   per login, which recovers on its own from a lockout that lifted and stays
//!   far below any lockout threshold;
//! - when the same login succeeds;
//! - when the user acts, through [`forget`]: stores a password
//!   (`config.set_password`), signs in again (`config.oauth2_login`), or
//!   starts what needs the login (a sync for the IMAP login and the Graph
//!   sign-in, a send for the SMTP login and the Graph sign-in);
//! - when the daemon restarts, since the list lives in memory only.
//!
//! A new secret is not on the list, because the hash is part of the entry,
//! so it is asked at once. `config.probe` dials past the gate: a probe is the
//! user asking the server.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use mp_protocol::account::RejectedLogin;
use sha2::{Digest, Sha256};

use crate::config::{AccountConfig, AuthMethod, ServerLogin};

/// How long a refused login stays refused before the server is asked again.
pub const REFUSAL_HOLD: Duration = Duration::from_secs(60 * 60);

/// One login an account makes, without its secret: what the list is keyed by,
/// and what the health check and `account.credentials` look a refusal up by
/// without reading a secret.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Login {
    /// The IMAP login of a password account.
    Imap(ServerLogin),
    /// The SMTP login of a password account.
    Smtp(ServerLogin),
    /// A Graph account's sign-in, by the account's name, which its token
    /// cache is kept under.
    Graph(String),
}

impl Login {
    /// What the user is told was refused.
    pub fn kind(&self) -> RejectedLogin {
        match self {
            Login::Imap(_) => RejectedLogin::Imap,
            Login::Smtp(_) => RejectedLogin::Smtp,
            Login::Graph(_) => RejectedLogin::Graph,
        }
    }

    /// `account`'s login of `kind`, `None` when the account makes no such
    /// login: a local-only account makes none, a password account no Graph
    /// sign-in, a Graph account no IMAP or SMTP login, and an account naming
    /// the retired `oauth2` mode none at all.
    pub fn of(account: &AccountConfig, kind: RejectedLogin) -> Option<Login> {
        match (&account.auth_method, kind) {
            _ if account.is_local_only() => None,
            (AuthMethod::Password, RejectedLogin::Imap) => {
                Some(Login::Imap(ServerLogin::imap(account)))
            }
            (AuthMethod::Password, RejectedLogin::Smtp) => {
                Some(Login::Smtp(ServerLogin::smtp(account)))
            }
            (AuthMethod::Graph, RejectedLogin::Graph) => Some(Login::Graph(account.name.clone())),
            _ => None,
        }
    }

    /// The login `account`'s mail goes through, which its sync, its IDLE
    /// watcher and its queued changes need: the IMAP login of a password
    /// account, the sign-in of a Graph one.
    pub fn of_mail(account: &AccountConfig) -> Option<Login> {
        let kind = match account.auth_method {
            AuthMethod::Graph => RejectedLogin::Graph,
            _ => RejectedLogin::Imap,
        };
        Login::of(account, kind)
    }

    /// The login `account`'s sends go through: the SMTP login of a password
    /// account, the sign-in of a Graph one.
    pub fn of_send(account: &AccountConfig) -> Option<Login> {
        let kind = match account.auth_method {
            AuthMethod::Graph => RejectedLogin::Graph,
            _ => RejectedLogin::Smtp,
        };
        Login::of(account, kind)
    }
}

/// A login the server refused, either just now or within the hold.
///
/// Its text is the server's answer behind the words the failure has always
/// surfaced as (`IMAP login failed`, `SMTP login failed`); one the gate
/// answered from the list says so, and says when the server is asked again.
#[derive(Clone, Debug)]
pub struct LoginRejected {
    /// Which login was refused.
    kind: RejectedLogin,
    /// The server's answer.
    answer: String,
    /// Which entry of the list this refusal is, `None` until it is recorded.
    entry: Option<u64>,
    /// Whether the gate answered without dialling.
    from_list: bool,
}

impl LoginRejected {
    /// The refusal of a `kind` login the server just sent.
    pub fn new(kind: RejectedLogin, answer: String) -> Self {
        Self {
            kind,
            answer,
            entry: None,
            from_list: false,
        }
    }

    /// Which login was refused.
    pub fn kind(&self) -> RejectedLogin {
        self.kind
    }

    /// The list entry this refusal belongs to: two errors with the same entry
    /// are one refusal, which a caller that logs every failure logs once.
    pub fn entry(&self) -> Option<u64> {
        self.entry
    }

    /// Whether the gate answered from the list, without asking the server.
    pub fn from_list(&self) -> bool {
        self.from_list
    }
}

impl fmt::Display for LoginRejected {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            RejectedLogin::Imap => write!(f, "IMAP login failed: {}", self.answer)?,
            RejectedLogin::Smtp => write!(f, "SMTP login failed: {}", self.answer)?,
            RejectedLogin::Graph => write!(f, "OAuth2 sign-in refused: {}", self.answer)?,
        }
        if self.from_list {
            let (who, what, again) = match self.kind {
                RejectedLogin::Imap => ("the server", "this password", "a sync is started"),
                RejectedLogin::Smtp => ("the server", "this password", "a message is sent"),
                RejectedLogin::Graph => {
                    ("Microsoft", "this sign-in", "a sync or a send is started")
                }
            };
            let set = match self.kind {
                RejectedLogin::Graph => "the account signs in again",
                _ => "the password is set again",
            };
            write!(
                f,
                " ({who} refused {what} earlier and is asked again within the hour, or at once \
                 after {set} or {again})"
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

/// Whether the list holds a fresh refusal of `login`, whatever the secret:
/// what the health check and `account.credentials` read, neither of which
/// may read a secret.
pub fn is_refused(login: &Login) -> bool {
    refusals().holds(login, Instant::now(), REFUSAL_HOLD)
}

/// The logins of `account` the list holds a fresh refusal of, in the order of
/// [`RejectedLogin`].
pub fn rejected_logins(account: &AccountConfig) -> Vec<RejectedLogin> {
    [
        RejectedLogin::Imap,
        RejectedLogin::Smtp,
        RejectedLogin::Graph,
    ]
    .into_iter()
    .filter_map(|kind| Login::of(account, kind))
    .filter(is_refused)
    .map(|login| login.kind())
    .collect()
}

/// Drop the refusal of `login`, so its next attempt asks the server.
pub fn forget(login: &Login) {
    refusals().forget(login);
}

/// Drop the refusal of every login `account` makes, which a stored password
/// asks for: the IMAP login falls back to the SMTP password, so either
/// password may be the one a refused login needed.
pub fn forget_account(account: &AccountConfig) {
    for kind in [
        RejectedLogin::Imap,
        RejectedLogin::Smtp,
        RejectedLogin::Graph,
    ] {
        if let Some(login) = Login::of(account, kind) {
            forget(&login);
        }
    }
}

/// Record a refusal of `login`, as a refused dial does, for a test outside
/// this module that needs the gate to hold one.
#[cfg(test)]
pub(crate) fn refuse_for_test(login: Login) {
    let rejected = LoginRejected::new(login.kind(), "no response: LOGIN failed.".into());
    record(&login, "refused", &rejected);
}

/// The refusal a `login` with `secret` would get without dialling, if any.
pub fn refused(login: &Login, secret: &str) -> Option<LoginRejected> {
    refusals().refused(login, &fingerprint(secret), Instant::now(), REFUSAL_HOLD)
}

/// Record that the server refused `login` with `secret`, and answer the
/// refusal with its entry.
pub fn record(login: &Login, secret: &str, rejected: &LoginRejected) -> LoginRejected {
    refusals().record(
        login.clone(),
        fingerprint(secret),
        rejected,
        Instant::now(),
        REFUSAL_HOLD,
    )
}

/// `login` succeeded, so whatever the list held about it no longer describes
/// the server.
pub fn accepted(login: &Login) {
    refusals().forget(login);
}

/// The refused secret, as the list compares it: never the secret itself.
fn fingerprint(secret: &str) -> [u8; 32] {
    Sha256::digest(secret.as_bytes()).into()
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
    secret: [u8; 32],
    at: Instant,
    entry: u64,
    answer: String,
}

/// The list itself, with the clock and the hold passed in so a test can step
/// past the hour without waiting for it.
#[derive(Default)]
struct Refusals {
    entries: HashMap<Login, Refusal>,
    next_entry: u64,
}

impl Refusals {
    fn fresh(refusal: &Refusal, now: Instant, hold: Duration) -> bool {
        now.saturating_duration_since(refusal.at) < hold
    }

    fn refused(
        &mut self,
        login: &Login,
        secret: &[u8; 32],
        now: Instant,
        hold: Duration,
    ) -> Option<LoginRejected> {
        let refusal = self.entries.get(login)?;
        if !Self::fresh(refusal, now, hold) {
            self.entries.remove(login);
            return None;
        }
        (refusal.secret == *secret).then(|| LoginRejected {
            kind: login.kind(),
            answer: refusal.answer.clone(),
            entry: Some(refusal.entry),
            from_list: true,
        })
    }

    fn record(
        &mut self,
        login: Login,
        secret: [u8; 32],
        rejected: &LoginRejected,
        now: Instant,
        hold: Duration,
    ) -> LoginRejected {
        // Two dials that raced each other past the gate were refused once: the
        // second keeps the first's entry, so a caller logs the refusal once.
        let entry = match self.entries.get(&login) {
            Some(refusal) if refusal.secret == secret && Self::fresh(refusal, now, hold) => {
                refusal.entry
            }
            _ => {
                self.next_entry += 1;
                self.entries.insert(
                    login,
                    Refusal {
                        secret,
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

    fn forget(&mut self, login: &Login) {
        self.entries.remove(login);
    }

    fn holds(&self, login: &Login, now: Instant, hold: Duration) -> bool {
        self.entries
            .get(login)
            .is_some_and(|refusal| Self::fresh(refusal, now, hold))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(port: u16) -> ServerLogin {
        ServerLogin {
            host: "imap.example.com".into(),
            port,
            username: "alpha@example.com".into(),
        }
    }

    fn login(port: u16) -> Login {
        Login::Imap(server(port))
    }

    fn rejected() -> LoginRejected {
        LoginRejected::new(RejectedLogin::Imap, "no".into())
    }

    /// A refusal holds the secret it refused for the hold and no longer;
    /// another secret, or the same one after the hold, is asked.
    #[test]
    fn a_refusal_holds_its_password_for_the_hold_and_then_lapses() {
        let mut list = Refusals::default();
        let (now, hold) = (Instant::now(), Duration::from_secs(3600));
        let (wrong, other) = (fingerprint("wrong"), fingerprint("other"));
        let recorded = list.record(login(993), wrong, &rejected(), now, hold);

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

    /// The IMAP and the SMTP login of one server and user are two logins: a
    /// refusal of one holds nothing of the other, and the answer from the
    /// list names the login it holds.
    #[test]
    fn the_imap_and_the_smtp_login_are_held_apart() {
        let mut list = Refusals::default();
        let (now, hold) = (Instant::now(), Duration::from_secs(3600));
        let wrong = fingerprint("wrong");
        let smtp = Login::Smtp(server(993));
        list.record(
            smtp.clone(),
            wrong,
            &LoginRejected::new(RejectedLogin::Smtp, "535 5.7.8".into()),
            now,
            hold,
        );
        assert!(list.refused(&login(993), &wrong, now, hold).is_none());
        let held = list.refused(&smtp, &wrong, now, hold).expect("held");
        assert_eq!(held.kind(), RejectedLogin::Smtp);
        assert!(held.from_list());
        assert!(
            held.to_string()
                .starts_with("SMTP login failed: 535 5.7.8 ("),
            "{held}"
        );
        assert!(held.to_string().contains("a message is sent"), "{held}");
    }

    /// Two refusals of one secret inside the hold are one entry, a new
    /// refusal after the entry was forgotten is a new one.
    #[test]
    fn a_second_refusal_of_the_same_password_keeps_its_entry() {
        let mut list = Refusals::default();
        let (now, hold) = (Instant::now(), Duration::from_secs(3600));
        let first = list.record(login(993), fingerprint("wrong"), &rejected(), now, hold);
        let raced = list.record(login(993), fingerprint("wrong"), &rejected(), now, hold);
        assert_eq!(first.entry(), raced.entry());
        list.forget(&login(993));
        assert!(!list.holds(&login(993), now, hold));
        let again = list.record(login(993), fingerprint("wrong"), &rejected(), now, hold);
        assert_ne!(first.entry(), again.entry());
    }

    /// The chain is searched, so a context added on the way up does not hide
    /// the refusal from the caller that has to act on it.
    #[test]
    fn a_refusal_is_found_under_a_context() {
        use anyhow::Context;
        let error = Err::<(), _>(rejected())
            .context("listing the folders")
            .unwrap_err();
        assert!(login_rejected(&error).is_some());
        assert!(login_rejected(&anyhow::anyhow!("IMAP login failed: no")).is_none());
    }

    /// A refused SMTP login is what `account.credentials` and the health
    /// check read for the account, and a stored password lets go of it.
    #[test]
    fn a_refused_smtp_login_is_the_accounts_rejected_login_until_forgotten() {
        let config: crate::config::GlobalConfig = toml::from_str(
            "[[accounts]]\nname = \"held\"\n[accounts.imap]\nhost = \"imap.held-smtp.test\"\n\
             [accounts.smtp]\nhost = \"smtp.held-smtp.test\"\nusername = \"held\"\n",
        )
        .unwrap();
        let account = &config.accounts[0];
        assert!(rejected_logins(account).is_empty());
        refuse_for_test(Login::of_send(account).unwrap());
        assert_eq!(rejected_logins(account), [RejectedLogin::Smtp]);
        forget_account(account);
        assert!(rejected_logins(account).is_empty());
    }

    /// A password account makes an IMAP and an SMTP login, a Graph account
    /// its sign-in, and a local-only account none.
    #[test]
    fn each_kind_of_account_makes_its_own_logins() {
        let config: crate::config::GlobalConfig = toml::from_str(
            "[[accounts]]\nname = \"tum\"\n[accounts.imap]\nhost = \"imap.tum.test\"\n\
             [accounts.smtp]\nhost = \"smtp.tum.test\"\n\n\
             [[accounts]]\nname = \"hines\"\nauth_method = \"graph\"\n\n\
             [[accounts]]\nname = \"notes\"\n",
        )
        .unwrap();
        let [tum, hines, notes] = &config.accounts[..] else {
            panic!("three accounts");
        };
        assert!(matches!(Login::of_mail(tum), Some(Login::Imap(_))));
        assert!(matches!(Login::of_send(tum), Some(Login::Smtp(_))));
        assert_eq!(Login::of(tum, RejectedLogin::Graph), None);
        assert_eq!(Login::of_mail(hines), Some(Login::Graph("hines".into())));
        assert_eq!(Login::of_send(hines), Some(Login::Graph("hines".into())));
        assert_eq!(Login::of(hines, RejectedLogin::Imap), None);
        assert_eq!(Login::of_mail(notes), None);
        assert_eq!(Login::of_send(notes), None);
    }
}
