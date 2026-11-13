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
//! while its hold runs the same login fails at once with the same
//! [`LoginRejected`] without dialling. The hold of a password grows with the
//! refusals in a row ([`hold`]): 2 minutes after the first, 10 after the
//! second, an hour from the third on, so a server that refuses a correct
//! password for a minute (the TUM server answered `NO LOGIN failed.` to one
//! twice 30 s apart, and took it on the attempts around) stops the mail for
//! minutes, while a wrong password is asked at most three times in the first
//! hour and once an hour after, far below any lockout threshold. A Graph
//! sign-in is held an hour from the first refusal, since `invalid_grant` is
//! Entra's final word on the refresh token. The hold ends
//!
//! - when it runs out, and the next attempt asks the server;
//! - when the user acts, through [`forget`]: stores a password
//!   (`config.set_password`), signs in again (`config.oauth2_login`), or
//!   starts what needs the login (a sync for the IMAP login and the Graph
//!   sign-in, a send for the SMTP login and the Graph sign-in).
//!
//! Neither resets the count: a refusal of the same secret after either is the
//! next in the row. The entry, count and all, ends
//!
//! - when the same login succeeds;
//! - when the daemon restarts, since the list lives in memory only.
//!
//! A new secret is not on the list, because the hash is part of the entry,
//! so it is asked at once, and its first refusal is the first of a new row.
//! `config.probe` dials past the gate: a probe is the user asking the server.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use mp_protocol::account::RejectedLogin;
use sha2::{Digest, Sha256};

use crate::config::{AccountConfig, AuthMethod, ServerLogin};

/// How long a password refused once in a row is held.
pub const FIRST_HOLD: Duration = Duration::from_secs(2 * 60);

/// How long a password refused twice in a row is held.
pub const SECOND_HOLD: Duration = Duration::from_secs(10 * 60);

/// How long a password refused three times or more in a row is held, and a
/// Graph sign-in from its first refusal.
pub const LONG_HOLD: Duration = Duration::from_secs(60 * 60);

/// How long `login`, refused `in_a_row` times in a row, is held before the
/// server is asked again.
///
/// A password is held 2 minutes, then 10, then an hour: a server may refuse
/// a correct password for a minute, and a long first hold would stop the
/// account's mail for that long. A Graph sign-in is held the hour from the
/// first refusal, since `invalid_grant` says the refresh token is revoked or
/// expired and asking again sooner cannot change the answer.
pub fn hold(login: &Login, in_a_row: u32) -> Duration {
    match (login, in_a_row) {
        (Login::Graph(_), _) => LONG_HOLD,
        (_, 0 | 1) => FIRST_HOLD,
        (_, 2) => SECOND_HOLD,
        _ => LONG_HOLD,
    }
}

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
    /// When the hold ends, set when the gate answered without dialling.
    asked_again_at: Option<SystemTime>,
}

impl LoginRejected {
    /// The refusal of a `kind` login the server just sent.
    pub fn new(kind: RejectedLogin, answer: String) -> Self {
        Self {
            kind,
            answer,
            entry: None,
            asked_again_at: None,
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
        self.asked_again_at.is_some()
    }
}

impl fmt::Display for LoginRejected {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            RejectedLogin::Imap => write!(f, "IMAP login failed: {}", self.answer)?,
            RejectedLogin::Smtp => write!(f, "SMTP login failed: {}", self.answer)?,
            RejectedLogin::Graph => write!(f, "OAuth2 sign-in refused: {}", self.answer)?,
        }
        if let Some(at) = self.asked_again_at {
            let at = chrono::DateTime::<chrono::Local>::from(at).format("%H:%M");
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
                " ({who} refused {what} earlier and is asked again at {at}, or at once after \
                 {set} or {again})"
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
    refusals().holds(login, Instant::now())
}

/// Whether the password account named `account` logs in to IMAP and SMTP
/// with one password (PERSO-297): no IMAP password is stored (the IMAP login
/// falls back to the SMTP one), no SMTP one is, or the two are equal. A
/// password the backend cannot read counts as not stored. Read from the
/// secrets backend, compared here, and never handed out.
pub fn shared_password(account: &str) -> bool {
    let stored = |kind: &str| crate::config::get_secret(&format!("{kind}-password-{account}")).ok();
    match (stored("imap"), stored("smtp")) {
        (Some(imap), Some(smtp)) => imap == smtp,
        _ => true,
    }
}

/// What every surface says about `account`'s refused `login`
/// ([`mp_core::mp_command::login_rejected_detail`]), with the kind-less
/// set-password command when the account uses one password for both logins.
pub fn rejected_detail(account: &str, login: RejectedLogin) -> String {
    let shared = login != RejectedLogin::Graph && shared_password(account);
    mp_core::mp_command::login_rejected_detail(account, login, shared)
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

/// End the hold of `login`, so its next attempt asks the server; the count
/// of refusals in a row stays, since the user acting is no success.
pub fn forget(login: &Login) {
    refusals().release(login, Instant::now());
}

/// End the hold of every login `account` makes, which a stored password
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
    refusals().refused(
        login,
        &fingerprint(secret),
        Instant::now(),
        SystemTime::now(),
    )
}

/// Record that the server refused `login` with `secret`, and answer the
/// refusal with its entry.
pub fn record(login: &Login, secret: &str, rejected: &LoginRejected) -> LoginRejected {
    refusals().record(login.clone(), fingerprint(secret), rejected, Instant::now())
}

/// `login` succeeded, so whatever the list held about it, the count of
/// refusals in a row included, no longer describes the server.
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
    /// When the hold ends and the server is asked again.
    until: Instant,
    /// The refusals of `secret` in a row, this one included.
    in_a_row: u32,
    entry: u64,
    answer: String,
}

impl Refusal {
    fn holds(&self, now: Instant) -> bool {
        now < self.until
    }
}

/// The list itself, with the clock passed in so a test can step past a hold
/// without waiting for it.
#[derive(Default)]
struct Refusals {
    entries: HashMap<Login, Refusal>,
    next_entry: u64,
}

impl Refusals {
    /// The answer the gate gives a `login` with `secret` at `now`, if its hold
    /// runs; `wall` is the wall clock at `now`, which the answer's time is
    /// told in.
    fn refused(
        &self,
        login: &Login,
        secret: &[u8; 32],
        now: Instant,
        wall: SystemTime,
    ) -> Option<LoginRejected> {
        let refusal = self.entries.get(login)?;
        (refusal.secret == *secret && refusal.holds(now)).then(|| LoginRejected {
            kind: login.kind(),
            answer: refusal.answer.clone(),
            entry: Some(refusal.entry),
            asked_again_at: Some(wall + refusal.until.saturating_duration_since(now)),
        })
    }

    fn record(
        &mut self,
        login: Login,
        secret: [u8; 32],
        rejected: &LoginRejected,
        now: Instant,
    ) -> LoginRejected {
        let entry = match self.entries.get(&login) {
            // Two dials that raced each other past the gate were refused once:
            // the second keeps the first's entry, so a caller logs the refusal
            // once, and its hold.
            Some(refusal) if refusal.secret == secret && refusal.holds(now) => refusal.entry,
            previous => {
                let in_a_row = match previous {
                    Some(refusal) if refusal.secret == secret => refusal.in_a_row + 1,
                    _ => 1,
                };
                self.next_entry += 1;
                let until = now + hold(&login, in_a_row);
                self.entries.insert(
                    login,
                    Refusal {
                        secret,
                        until,
                        in_a_row,
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

    /// End the hold of `login` at `now`, keeping its count.
    fn release(&mut self, login: &Login, now: Instant) {
        if let Some(refusal) = self.entries.get_mut(login) {
            refusal.until = refusal.until.min(now);
        }
    }

    /// Drop `login`'s entry, count and all.
    fn forget(&mut self, login: &Login) {
        self.entries.remove(login);
    }

    fn holds(&self, login: &Login, now: Instant) -> bool {
        self.entries
            .get(login)
            .is_some_and(|refusal| refusal.holds(now))
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

    const SECOND: Duration = Duration::from_secs(1);

    /// A refusal holds the secret it refused for its hold and no longer;
    /// another secret, or the same one after the hold, is asked.
    #[test]
    fn a_refusal_holds_its_password_for_the_hold_and_then_lapses() {
        let mut list = Refusals::default();
        let now = Instant::now();
        let (wrong, other) = (fingerprint("wrong"), fingerprint("other"));
        let recorded = list.record(login(993), wrong, &rejected(), now);

        let wall = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let held = list
            .refused(&login(993), &wrong, now + SECOND, wall)
            .expect("the same password is refused from the list");
        assert_eq!(held.entry(), recorded.entry());
        assert!(held.from_list());
        assert!(list.holds(&login(993), now + SECOND));
        assert!(list.refused(&login(993), &other, now, wall).is_none());
        assert!(list.refused(&login(143), &wrong, now, wall).is_none());

        assert!(!list.holds(&login(993), now + FIRST_HOLD));
        assert!(list
            .refused(&login(993), &wrong, now + FIRST_HOLD, wall)
            .is_none());
    }

    /// The answer from the list names the time the server is asked again,
    /// the end of the hold on the wall clock, not a stretch it may not be.
    #[test]
    fn a_held_refusal_names_the_time_the_server_is_asked_again() {
        let mut list = Refusals::default();
        let now = Instant::now();
        list.record(login(993), fingerprint("wrong"), &rejected(), now);
        let wall = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let held = list
            .refused(&login(993), &fingerprint("wrong"), now + 30 * SECOND, wall)
            .expect("held");
        let again = chrono::DateTime::<chrono::Local>::from(wall + FIRST_HOLD - 30 * SECOND)
            .format("%H:%M")
            .to_string();
        assert!(
            held.to_string().contains(&format!(
                "(the server refused this password earlier and is asked again at {again}, \
                 or at once after the password is set again or a sync is started)"
            )),
            "{held}"
        );
        assert!(!held.to_string().contains("hour"), "{held}");
    }

    /// Refuse `secret` at `at` and answer how long the gate then holds it:
    /// the first instant from `at` on, to the second, when it asks again.
    fn held_for(list: &mut Refusals, secret: &str, at: Instant) -> Duration {
        list.record(login(993), fingerprint(secret), &rejected(), at);
        let mut held = Duration::ZERO;
        while list.holds(&login(993), at + held) {
            held += SECOND;
        }
        held
    }

    /// The hold of a password grows with its refusals in a row: 2 minutes
    /// after the first, 10 after the second, an hour from the third on, so a
    /// wrong password is asked at most three times in the first hour.
    #[test]
    fn a_password_is_held_two_minutes_then_ten_then_an_hour() {
        let mut list = Refusals::default();
        let mut at = Instant::now();
        let mut holds = Vec::new();
        for _ in 0..5 {
            let held = held_for(&mut list, "wrong", at);
            holds.push(held);
            at += held;
        }
        assert_eq!(
            holds,
            [FIRST_HOLD, SECOND_HOLD, LONG_HOLD, LONG_HOLD, LONG_HOLD]
        );
        assert_eq!(FIRST_HOLD, 2 * 60 * SECOND);
        assert_eq!(SECOND_HOLD, 10 * 60 * SECOND);
        assert_eq!(LONG_HOLD, 60 * 60 * SECOND);
        let in_the_first_hour = holds
            .iter()
            .scan(Duration::ZERO, |start, held| {
                let this = *start;
                *start += *held;
                Some(this)
            })
            .filter(|start| *start < LONG_HOLD)
            .count();
        assert_eq!(in_the_first_hour, 3);
    }

    /// A success ends the row: the next refusal of the same password is held
    /// 2 minutes again.
    #[test]
    fn a_success_resets_the_row() {
        let mut list = Refusals::default();
        let at = Instant::now();
        let first = held_for(&mut list, "wrong", at);
        let second = held_for(&mut list, "wrong", at + first);
        assert_eq!(second, SECOND_HOLD);
        list.forget(&login(993));
        assert_eq!(
            held_for(&mut list, "wrong", at + first + second),
            FIRST_HOLD
        );
    }

    /// A new password starts a row of its own, and asks at once.
    #[test]
    fn a_changed_password_starts_its_own_row() {
        let mut list = Refusals::default();
        let at = Instant::now();
        let first = held_for(&mut list, "wrong", at);
        held_for(&mut list, "wrong", at + first);
        let now = at + first + SECOND;
        assert!(list
            .refused(&login(993), &fingerprint("new"), now, SystemTime::now())
            .is_none());
        assert_eq!(held_for(&mut list, "new", now), FIRST_HOLD);
    }

    /// The user acting ends the hold at once and keeps the row, so a refusal
    /// right after is the next in it rather than the first of a new one.
    #[test]
    fn the_user_acting_ends_the_hold_and_keeps_the_row() {
        let mut list = Refusals::default();
        let at = Instant::now();
        list.record(login(993), fingerprint("wrong"), &rejected(), at);
        let now = at + SECOND;
        list.release(&login(993), now);
        assert!(!list.holds(&login(993), now));
        assert!(list
            .refused(&login(993), &fingerprint("wrong"), now, SystemTime::now())
            .is_none());
        assert_eq!(held_for(&mut list, "wrong", now), SECOND_HOLD);
    }

    /// A Graph sign-in is held the hour from its first refusal: Entra's
    /// `invalid_grant` does not change by being asked again.
    #[test]
    fn a_graph_sign_in_is_held_an_hour_from_the_first_refusal() {
        let graph = Login::Graph("hines".into());
        assert_eq!(hold(&graph, 1), LONG_HOLD);
        assert_eq!(hold(&graph, 2), LONG_HOLD);
        assert_eq!(hold(&login(993), 1), FIRST_HOLD);
        assert_eq!(hold(&Login::Smtp(server(465)), 1), FIRST_HOLD);
        assert_eq!(hold(&Login::Smtp(server(465)), 2), SECOND_HOLD);
        assert_eq!(hold(&Login::Smtp(server(465)), 3), LONG_HOLD);
        let mut list = Refusals::default();
        let at = Instant::now();
        list.record(graph.clone(), fingerprint("revoked"), &rejected(), at);
        assert!(list.holds(&graph, at + LONG_HOLD - SECOND));
        assert!(!list.holds(&graph, at + LONG_HOLD));
    }

    /// The IMAP and the SMTP login of one server and user are two logins: a
    /// refusal of one holds nothing of the other, and the answer from the
    /// list names the login it holds.
    #[test]
    fn the_imap_and_the_smtp_login_are_held_apart() {
        let mut list = Refusals::default();
        let (now, wall) = (Instant::now(), SystemTime::now());
        let wrong = fingerprint("wrong");
        let smtp = Login::Smtp(server(993));
        list.record(
            smtp.clone(),
            wrong,
            &LoginRejected::new(RejectedLogin::Smtp, "535 5.7.8".into()),
            now,
        );
        assert!(list.refused(&login(993), &wrong, now, wall).is_none());
        let held = list.refused(&smtp, &wrong, now, wall).expect("held");
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
        let now = Instant::now();
        let first = list.record(login(993), fingerprint("wrong"), &rejected(), now);
        let raced = list.record(login(993), fingerprint("wrong"), &rejected(), now);
        assert_eq!(first.entry(), raced.entry());
        assert!(
            !list.holds(&login(993), now + FIRST_HOLD),
            "the race kept the first refusal's hold"
        );
        list.release(&login(993), now);
        assert!(!list.holds(&login(993), now));
        let again = list.record(login(993), fingerprint("wrong"), &rejected(), now);
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
