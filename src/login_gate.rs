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
//! Three logins go through the gate, each named by a [`Login`]:
//!
//! - the IMAP login, in [`crate::imap_client::open_imap_session`], refused by
//!   a `NO` about the password;
//! - the SMTP login, in [`crate::send::submit`], refused by a `535` or a reply
//!   carrying the enhanced code `5.7.8`;
//! - a Graph account's token refresh, in [`crate::graph::access_token`],
//!   refused by `invalid_grant` (the refresh token is revoked or expired).
//!
//! Each consults the list kept here. A refusal is recorded under the
//! account and a hash of the refused secret (the password, or the refresh
//! token), not under the login: an account's IMAP and SMTP logins that use
//! one password share one row, whatever their hosts (the TUM's IMAP and SMTP
//! servers are two hosts checking one password). While the row's hold runs,
//! every login of the account with that secret fails at once with a
//! [`LoginRejected`] without dialling, the SMTP login after an IMAP refusal
//! included, and the answer says at which login the server refused it.
//!
//! The hold of a password grows with the refusals of it in a row, at either
//! login ([`hold`]): 2 minutes after the first, 10 after the second, an hour
//! from the third on. A server that refuses a correct password for a minute
//! (the TUM server answered `NO LOGIN failed.` to one twice 30 s apart, and
//! took it on the attempts around) stops the mail for minutes; a wrong
//! password is sent to the server at most 3 times in the first hour and once
//! an hour after, by the sync and the outbox drain together, until the user
//! acts. A Graph sign-in is held an hour from the first refusal, since
//! `invalid_grant` is Entra's final word on the refresh token. Two passwords
//! (an account whose IMAP and SMTP passwords differ, or one changed at one
//! login only) are two rows with a schedule each. Dials that race each other
//! past the gate as a hold ends are counted as one refusal.
//!
//! The hold ends
//!
//! - when it runs out, and the next attempt asks the server;
//! - when the user acts, through [`forget`] or [`forget_account`]: stores a
//!   password (`config.set_password`), signs in again
//!   (`config.oauth2_login`), or starts what needs the login (a sync for the
//!   IMAP login and the Graph sign-in, a send for the SMTP login and the Graph
//!   sign-in), which ends it for every login of the account sharing the
//!   secret.
//!
//! Neither resets the count: a refusal of the same secret after either is the
//! next in the row. The row, count and all, ends
//!
//! - when a login of the account succeeds with the secret, since the server
//!   just took it, for the other login as well;
//! - when no login of the account is attempted with the secret any more;
//! - when the daemon restarts, since the list lives in memory only.
//!
//! A new secret is not on the list, so it is asked at once, and its first
//! refusal is the first of a new row. The health check and
//! `account.credentials` read no secret: they report a login refused while
//! the row of the secret it was last attempted with holds, so the SMTP login
//! of a shared password is reported once it was attempted within the hold.
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
    /// The account the login is made for, which a refused secret is kept
    /// under.
    pub fn account(&self) -> &str {
        match self {
            Login::Imap(server) | Login::Smtp(server) => &server.account,
            Login::Graph(account) => account,
        }
    }

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
/// answered from the list says so, says at which login the server refused the
/// password when it was the account's other one, and says when the server is
/// asked again.
#[derive(Clone, Debug)]
pub struct LoginRejected {
    /// Which login was refused.
    kind: RejectedLogin,
    /// The login the server refused the secret at: `kind` itself, or the
    /// account's other password login that shares the password.
    refused_at: RejectedLogin,
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
            refused_at: kind,
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
            let elsewhere = match self.refused_at {
                at if at == self.kind => "",
                RejectedLogin::Imap => " at the IMAP login",
                RejectedLogin::Smtp => " at the SMTP login",
                RejectedLogin::Graph => "",
            };
            let set = match self.kind {
                RejectedLogin::Graph => "the account signs in again",
                _ => "the password is set again",
            };
            write!(
                f,
                " ({who} refused {what}{elsewhere} earlier and is asked again at {at}, or at \
                 once after {set} or {again})"
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

/// Whether the list holds a running refusal of the secret `login` was last
/// attempted with: what the health check and `account.credentials` read,
/// neither of which may read a secret. A login not yet attempted with a
/// refused password is not reported, even when its next attempt would be
/// held.
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

/// The logins of `account` the list holds a running refusal of, in the order
/// of [`RejectedLogin`].
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

/// End the hold of the secret `login` was last attempted with, for every
/// login of the account that shares it, so the next attempt asks the server;
/// the count of refusals in a row stays, since the user acting is no
/// success. A login the gate has not seen attempted yet ends the hold of
/// every secret of its kind (a password, or a sign-in) on its account.
pub fn forget(login: &Login) {
    refusals().release(login, Instant::now());
}

/// End the hold of every login `account` makes, which a stored password
/// asks for: the IMAP login falls back to the SMTP password, so either
/// password may be the one a refused login needed.
pub fn forget_account(account: &AccountConfig) {
    refusals().release_account(&account.name, Instant::now());
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

/// `login` succeeded with `secret`, so the server takes it: whatever the list
/// held about it, the count of refusals in a row included, no longer
/// describes the server, for the account's other login with it too.
pub fn accepted(login: &Login, secret: &str) {
    refusals().accept(login, &fingerprint(secret));
}

/// The refused secret, as the list compares it: never the secret itself.
fn fingerprint(secret: &str) -> [u8; 32] {
    Sha256::digest(secret.as_bytes()).into()
}

fn refusals() -> MutexGuard<'static, Refusals> {
    static REFUSALS: OnceLock<Mutex<Refusals>> = OnceLock::new();
    // A panic while the lock was held cannot leave the maps half written:
    // every critical section leaves them consistent between its writes.
    REFUSALS
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// What a refused secret is kept under: the account and the secret, not the
/// login, so an account's IMAP and SMTP logins that use one password share
/// one row, whatever their hosts. `sign_in` keeps a Graph refresh token apart
/// from a password.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct RowKey {
    account: String,
    sign_in: bool,
    secret: [u8; 32],
}

impl RowKey {
    fn of(login: &Login, secret: &[u8; 32]) -> Self {
        Self {
            account: login.account().to_string(),
            sign_in: matches!(login, Login::Graph(_)),
            secret: *secret,
        }
    }

    fn is_family_of(&self, login: &Login) -> bool {
        self.account == login.account() && self.sign_in == matches!(login, Login::Graph(_))
    }
}

/// One refused secret of one account: its schedule, shared by every login of
/// the account that uses it.
struct Row {
    /// When the hold ends and the server is asked again.
    until: Instant,
    /// The refusals of the secret in a row, at any of the account's logins,
    /// this one included.
    in_a_row: u32,
    entry: u64,
    /// The login the server last refused the secret at, and its answer.
    refused_at: RejectedLogin,
    answer: String,
}

impl Row {
    fn holds(&self, now: Instant) -> bool {
        now < self.until
    }
}

/// The list itself, with the clock passed in so a test can step past a hold
/// without waiting for it.
#[derive(Default)]
struct Refusals {
    /// The refused secrets, by account and secret.
    rows: HashMap<RowKey, Row>,
    /// The secret each login was last attempted with, which a caller that
    /// reads no secret (the health check) and the user's action find the
    /// login's row by.
    seen: HashMap<Login, [u8; 32]>,
    next_entry: u64,
}

impl Refusals {
    /// Note that `login` is attempted with `secret`, and drop the rows of its
    /// account no login is attempted with any more: a password replaced is
    /// a row nothing reads.
    fn see(&mut self, login: &Login, secret: &[u8; 32]) {
        if self.seen.get(login) == Some(secret) {
            return;
        }
        self.seen.insert(login.clone(), *secret);
        let account = login.account().to_string();
        let live: Vec<RowKey> = self
            .seen
            .iter()
            .filter(|(seen, _)| seen.account() == account)
            .map(|(seen, secret)| RowKey::of(seen, secret))
            .collect();
        self.rows
            .retain(|key, _| key.account != account || live.contains(key));
    }

    /// The answer the gate gives a `login` with `secret` at `now`, if the hold
    /// of the secret runs, whichever of the account's logins it was refused
    /// at; `wall` is the wall clock at `now`, which the answer's time is told
    /// in.
    fn refused(
        &mut self,
        login: &Login,
        secret: &[u8; 32],
        now: Instant,
        wall: SystemTime,
    ) -> Option<LoginRejected> {
        self.see(login, secret);
        let row = self.rows.get(&RowKey::of(login, secret))?;
        row.holds(now).then(|| LoginRejected {
            kind: login.kind(),
            refused_at: row.refused_at,
            answer: row.answer.clone(),
            entry: Some(row.entry),
            asked_again_at: Some(wall + row.until.saturating_duration_since(now)),
        })
    }

    fn record(
        &mut self,
        login: Login,
        secret: [u8; 32],
        rejected: &LoginRejected,
        now: Instant,
    ) -> LoginRejected {
        self.see(&login, &secret);
        let key = RowKey::of(&login, &secret);
        let entry = match self.rows.get(&key) {
            // Two dials that raced each other past the gate, at one login or
            // at both, were refused once: the second keeps the first's entry,
            // so a caller logs the refusal once, and its hold.
            Some(row) if row.holds(now) => row.entry,
            previous => {
                let in_a_row = previous.map_or(1, |row| row.in_a_row + 1);
                self.next_entry += 1;
                self.rows.insert(
                    key,
                    Row {
                        until: now + hold(&login, in_a_row),
                        in_a_row,
                        entry: self.next_entry,
                        refused_at: login.kind(),
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

    /// End at `now` the hold of the secret `login` was last attempted with,
    /// keeping its count; for a login not seen yet, of every row of its
    /// family on its account.
    fn release(&mut self, login: &Login, now: Instant) {
        match self.seen.get(login) {
            Some(secret) => {
                if let Some(row) = self.rows.get_mut(&RowKey::of(login, secret)) {
                    row.until = row.until.min(now);
                }
            }
            None => {
                for (_, row) in self
                    .rows
                    .iter_mut()
                    .filter(|(key, _)| key.is_family_of(login))
                {
                    row.until = row.until.min(now);
                }
            }
        }
    }

    /// End at `now` the hold of every row of `account`, keeping the counts.
    fn release_account(&mut self, account: &str, now: Instant) {
        for (_, row) in self
            .rows
            .iter_mut()
            .filter(|(key, _)| key.account == account)
        {
            row.until = row.until.min(now);
        }
    }

    /// `login` succeeded with `secret`: drop its row, count and all, for
    /// every login of the account that shares it.
    fn accept(&mut self, login: &Login, secret: &[u8; 32]) {
        self.see(login, secret);
        self.rows.remove(&RowKey::of(login, secret));
    }

    fn holds(&self, login: &Login, now: Instant) -> bool {
        self.seen
            .get(login)
            .and_then(|secret| self.rows.get(&RowKey::of(login, secret)))
            .is_some_and(|row| row.holds(now))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The IMAP login of `account` on `host`.
    fn imap_of(account: &str, host: &str) -> Login {
        Login::Imap(ServerLogin {
            account: account.into(),
            host: host.into(),
            port: 993,
            username: "alpha@example.com".into(),
        })
    }

    /// The SMTP login of `account` on `host`.
    fn smtp_of(account: &str, host: &str) -> Login {
        Login::Smtp(ServerLogin {
            account: account.into(),
            host: host.into(),
            port: 465,
            username: "alpha@example.com".into(),
        })
    }

    /// The IMAP and the SMTP login of the account `tum`, on two hosts as the
    /// TUM's are.
    fn imap() -> Login {
        imap_of("tum", "imap.tum.test")
    }

    fn smtp() -> Login {
        smtp_of("tum", "smtp.tum.test")
    }

    fn rejected(login: &Login) -> LoginRejected {
        LoginRejected::new(login.kind(), "no response: LOGIN failed.".into())
    }

    const SECOND: Duration = Duration::from_secs(1);
    const MINUTE: Duration = Duration::from_secs(60);

    fn wall() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000)
    }

    /// Refuse `login` with `secret` at `at` and answer how long the gate
    /// then holds it: the first instant from `at` on, to the second, when
    /// the same login with the same secret is asked again.
    fn held_for(list: &mut Refusals, login: &Login, secret: &str, at: Instant) -> Duration {
        list.record(login.clone(), fingerprint(secret), &rejected(login), at);
        let mut held = Duration::ZERO;
        while list
            .refused(login, &fingerprint(secret), at + held, wall())
            .is_some()
        {
            held += SECOND;
        }
        held
    }

    /// A refusal holds the secret it refused for its hold and no longer;
    /// another secret, or another account's login, is asked.
    #[test]
    fn a_refusal_holds_its_password_for_the_hold_and_then_lapses() {
        let mut list = Refusals::default();
        let now = Instant::now();
        let (wrong, other) = (fingerprint("wrong"), fingerprint("other"));
        let recorded = list.record(imap(), wrong, &rejected(&imap()), now);

        let held = list
            .refused(&imap(), &wrong, now + SECOND, wall())
            .expect("the same password is refused from the list");
        assert_eq!(held.entry(), recorded.entry());
        assert!(held.from_list());
        assert!(list.holds(&imap(), now + SECOND));
        assert!(list.refused(&imap(), &other, now, wall()).is_none());
        let elsewhere = imap_of("home", "imap.tum.test");
        assert!(list.refused(&elsewhere, &wrong, now, wall()).is_none());

        assert!(list
            .refused(&imap(), &wrong, now + FIRST_HOLD, wall())
            .is_none());
        assert!(!list.holds(&imap(), now + FIRST_HOLD));
    }

    /// The answer from the list names the time the server is asked again,
    /// the end of the hold on the wall clock, not a stretch it may not be.
    #[test]
    fn a_held_refusal_names_the_time_the_server_is_asked_again() {
        let mut list = Refusals::default();
        let now = Instant::now();
        list.record(imap(), fingerprint("wrong"), &rejected(&imap()), now);
        let held = list
            .refused(&imap(), &fingerprint("wrong"), now + 30 * SECOND, wall())
            .expect("held");
        let again = chrono::DateTime::<chrono::Local>::from(wall() + FIRST_HOLD - 30 * SECOND)
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

    /// The hold of a password grows with its refusals in a row: 2 minutes
    /// after the first, 10 after the second, an hour from the third on.
    #[test]
    fn a_password_is_held_two_minutes_then_ten_then_an_hour() {
        let mut list = Refusals::default();
        let mut at = Instant::now();
        let mut holds = Vec::new();
        for _ in 0..5 {
            let held = held_for(&mut list, &imap(), "wrong", at);
            holds.push(held);
            at += held;
        }
        assert_eq!(
            holds,
            [FIRST_HOLD, SECOND_HOLD, LONG_HOLD, LONG_HOLD, LONG_HOLD]
        );
        assert_eq!(FIRST_HOLD, 2 * MINUTE);
        assert_eq!(SECOND_HOLD, 10 * MINUTE);
        assert_eq!(LONG_HOLD, 60 * MINUTE);
    }

    /// One password the IMAP and the SMTP login of an account share is one
    /// row: refused at the IMAP login, the SMTP login with it fails from the
    /// list without dialling, on another host too, and says it was the IMAP
    /// login the server refused it at.
    #[test]
    fn a_password_refused_at_the_imap_login_holds_the_smtp_login() {
        let mut list = Refusals::default();
        let now = Instant::now();
        let wrong = fingerprint("wrong");
        list.record(imap(), wrong, &rejected(&imap()), now);
        assert!(
            !list.holds(&smtp(), now),
            "a login not yet attempted with the password is not reported refused"
        );

        let held = list
            .refused(&smtp(), &wrong, now + SECOND, wall())
            .expect("the SMTP login with the refused password is not dialled");
        assert_eq!(held.kind(), RejectedLogin::Smtp);
        let text = held.to_string();
        assert!(
            text.starts_with(
                "SMTP login failed: no response: LOGIN failed. (the server refused \
                 this password at the IMAP login earlier and is asked again at "
            ),
            "{text}"
        );
        assert!(text.ends_with("or a message is sent)"), "{text}");
        assert!(
            list.holds(&smtp(), now + SECOND),
            "once attempted, the SMTP login is reported refused beside the IMAP one"
        );
        assert!(list
            .refused(&smtp(), &fingerprint("its own"), now + SECOND, wall())
            .is_none());
    }

    /// A refusal at either login is the next in the one row, and the hold it
    /// sets holds both.
    #[test]
    fn a_refusal_at_either_login_counts_for_both() {
        let mut list = Refusals::default();
        let at = Instant::now();
        let first = held_for(&mut list, &imap(), "wrong", at);
        assert_eq!(first, FIRST_HOLD);
        let second = held_for(&mut list, &smtp(), "wrong", at + first);
        assert_eq!(
            second, SECOND_HOLD,
            "the SMTP refusal is the second in the row"
        );
        let then = at + first + SECOND;
        assert!(
            list.refused(&imap(), &fingerprint("wrong"), then, wall())
                .is_some(),
            "the SMTP refusal holds the IMAP login too"
        );
        assert_eq!(
            held_for(&mut list, &imap(), "wrong", at + first + second),
            LONG_HOLD
        );
    }

    /// A success at either login resets the row for both, since the server
    /// just took that password.
    #[test]
    fn a_success_at_either_login_resets_both() {
        let mut list = Refusals::default();
        let at = Instant::now();
        let first = held_for(&mut list, &imap(), "wrong", at);
        let second = held_for(&mut list, &smtp(), "wrong", at + first);
        let now = at + first + second;
        list.accept(&smtp(), &fingerprint("wrong"));
        assert!(list
            .refused(&imap(), &fingerprint("wrong"), now, wall())
            .is_none());
        assert_eq!(held_for(&mut list, &imap(), "wrong", now), FIRST_HOLD);
    }

    /// A password changed at one login only is a row of its own: the other
    /// login keeps its row and its count.
    #[test]
    fn a_password_changed_at_one_login_follows_its_own_row() {
        let mut list = Refusals::default();
        let at = Instant::now();
        let first = held_for(&mut list, &imap(), "old", at);
        let now = at + first;
        assert_eq!(held_for(&mut list, &smtp(), "new", now), FIRST_HOLD);
        assert!(list
            .refused(&imap(), &fingerprint("old"), now + SECOND, wall())
            .is_none());
        assert_eq!(
            held_for(&mut list, &imap(), "old", now + SECOND),
            SECOND_HOLD,
            "the old password's row kept its count"
        );
        assert!(list.holds(&smtp(), now + SECOND));
    }

    /// The user acting ends the hold at once, for both logins of the
    /// password, and keeps the row, so a refusal right after is the next in
    /// it rather than the first of a new one.
    #[test]
    fn the_user_acting_ends_the_hold_for_both_logins_and_keeps_the_row() {
        let mut list = Refusals::default();
        let at = Instant::now();
        list.record(imap(), fingerprint("wrong"), &rejected(&imap()), at);
        let now = at + SECOND;
        list.release(&imap(), now);
        assert!(!list.holds(&imap(), now));
        for login in [imap(), smtp()] {
            assert!(list
                .refused(&login, &fingerprint("wrong"), now, wall())
                .is_none());
        }
        assert_eq!(held_for(&mut list, &smtp(), "wrong", now), SECOND_HOLD);

        // A login the gate has not seen yet lets go of every password row
        // of its account: a sync started for an account whose SMTP login
        // alone was refused asks the server at once.
        let mut list = Refusals::default();
        list.record(smtp(), fingerprint("wrong"), &rejected(&smtp()), at);
        list.release(&imap(), now);
        assert!(list
            .refused(&imap(), &fingerprint("wrong"), now, wall())
            .is_none());
    }

    /// A Graph sign-in is held an hour from its first refusal: Entra's
    /// `invalid_grant` does not change by being asked again.
    #[test]
    fn a_graph_sign_in_is_held_an_hour_from_the_first_refusal() {
        let graph = Login::Graph("hines".into());
        assert_eq!(hold(&graph, 1), LONG_HOLD);
        assert_eq!(hold(&graph, 2), LONG_HOLD);
        assert_eq!(hold(&imap(), 1), FIRST_HOLD);
        assert_eq!(hold(&smtp(), 2), SECOND_HOLD);
        assert_eq!(hold(&smtp(), 3), LONG_HOLD);
        let mut list = Refusals::default();
        let at = Instant::now();
        assert_eq!(held_for(&mut list, &graph, "revoked", at), LONG_HOLD);
    }

    /// The worst case the module doc promises: one wrong password shared by
    /// the IMAP and the SMTP login, with a sync tick dialling IMAP every
    /// minute and the outbox drain dialling SMTP every 30 s, reaches the
    /// server 3 times in the first hour and once an hour after.
    #[test]
    fn one_wrong_password_is_asked_three_times_in_the_first_hour_and_once_an_hour_after() {
        let mut list = Refusals::default();
        let start = Instant::now();
        let wrong = fingerprint("wrong");
        let mut dials_per_hour = [0u32; 5];
        let mut t = Duration::ZERO;
        while t < 5 * LONG_HOLD {
            let now = start + t;
            let mut attempts = vec![smtp()];
            if t.as_secs().is_multiple_of(60) {
                attempts.insert(0, imap());
            }
            for login in attempts {
                if list.refused(&login, &wrong, now, wall()).is_none() {
                    dials_per_hour[(t.as_secs() / 3600) as usize] += 1;
                    list.record(login.clone(), wrong, &rejected(&login), now);
                }
            }
            t += 30 * SECOND;
        }
        assert_eq!(dials_per_hour, [3, 1, 1, 1, 1]);
    }

    /// Two refusals of one secret inside the hold are one entry, a new
    /// refusal after the hold was let go is a new one.
    #[test]
    fn a_second_refusal_of_the_same_password_keeps_its_entry() {
        let mut list = Refusals::default();
        let now = Instant::now();
        let wrong = fingerprint("wrong");
        let first = list.record(imap(), wrong, &rejected(&imap()), now);
        let raced = list.record(imap(), wrong, &rejected(&imap()), now);
        let sibling = list.record(smtp(), wrong, &rejected(&smtp()), now);
        assert_eq!(first.entry(), raced.entry());
        assert_eq!(first.entry(), sibling.entry());
        assert!(
            !list.holds(&imap(), now + FIRST_HOLD),
            "the race kept the first refusal's hold"
        );
        list.release(&imap(), now);
        assert!(!list.holds(&imap(), now));
        let again = list.record(imap(), wrong, &rejected(&imap()), now);
        assert_ne!(first.entry(), again.entry());
    }

    /// The chain is searched, so a context added on the way up does not hide
    /// the refusal from the caller that has to act on it.
    #[test]
    fn a_refusal_is_found_under_a_context() {
        use anyhow::Context;
        let error = Err::<(), _>(rejected(&imap()))
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
