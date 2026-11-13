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
//! Each asks the gate first ([`dial`]), and the gate keeps a row per
//! refused secret. What each promise below rests on is named after it.
//!
//! # What a row is
//!
//! A row is a login identity and a hash of the refused secret, not a login.
//! The identity of a password login is its user name, ASCII-lowercased, and
//! not its host or its account, so these share one row:
//!
//! - an account's IMAP and SMTP logins presenting one user and one password
//!   on two hosts, the TUM's case (`one_user_and_password_on_two_hosts_is_one_row`);
//! - two configured accounts logging in as one user with one password
//!   (`two_accounts_as_one_user_are_one_row`).
//!
//! Two user names are two rows even with one password, so a typo in one
//! neither holds nor resets the other (`two_users_with_one_password_are_two_rows`),
//! and two passwords are two rows (`a_password_changed_at_one_login_follows_its_own_row`).
//! A Graph sign-in's identity is its account, its secret the refresh token.
//! The case this keying gets wrong, and is accepted: two unrelated servers
//! with one user name and one password share a row, so a refusal at one
//! holds the other and a success at one resets the other's count. Mail user
//! names are nearly always full addresses, and keying by host would split
//! the TUM's two hosts, the case the schedule exists for.
//!
//! # The hold
//!
//! While a row's hold runs, every login of the row fails at once with a
//! [`LoginRejected`] without dialling, the SMTP login after an IMAP refusal
//! included, and the answer names the login the server refused it at and the
//! time the server is asked again
//! (`a_password_refused_at_the_imap_login_holds_the_smtp_login`,
//! `a_held_refusal_names_the_time_the_server_is_asked_again`). The hold of a
//! password is 2 minutes after its first refusal in a row, 10 after the
//! second and an hour from the third on, counted at every login of the row
//! (`a_password_is_held_two_minutes_then_ten_then_an_hour`,
//! `a_refusal_at_either_login_counts_for_both`): a server that refuses a
//! correct password for a minute (the TUM server answered `NO LOGIN failed.`
//! to one twice 30 s apart, and took it on the attempts around) stops the
//! mail for minutes. A Graph sign-in is held an hour from its first refusal,
//! since `invalid_grant` is Entra's final word on the refresh token
//! (`a_graph_sign_in_is_held_an_hour_from_the_first_refusal`).
//!
//! When a hold ends, the first dial of the row marks it in flight and every
//! other dial of the row is answered as held until it answers, told that a
//! login with the password is being tried right now rather than a time
//! (`a_dial_held_by_one_in_flight_says_a_login_is_being_tried`); the mark is a
//! guard ([`Dial`]) that clears on a success, a refusal, any other error, a
//! timeout and a cancelled task alike (`a_dial_in_flight_holds_the_others_of_its_row`,
//! `one_dial_asks_when_a_hold_ends_and_a_cancelled_one_frees_the_row`). No
//! dial keeps its mark past [`LOGIN_DEADLINE`] after its connection opened:
//! the IMAP dial and the Graph token refresh fail at it as no answer, which
//! counts as no refusal (`imap_client::pool`'s
//! `a_login_that_never_answers_ends_at_the_deadline_and_frees_the_row`,
//! `crate::graph`'s `a_refresh_that_never_answers_ends_at_the_deadline_and_frees_the_row`),
//! and an SMTP submission lets go of its mark at it and goes on
//! (`crate::send`'s `an_smtp_login_that_never_answers_lets_go_of_its_mark_at_the_deadline`);
//! a mark older than `MARK_LIFETIME` holds nothing whatever left it
//! (`a_mark_older_than_any_dial_is_free`). So
//! a wrong password reaches the server 3 times in the first hour after its
//! first refusal and once an hour after, with the sync tick, the IDLE watcher
//! and the outbox drain racing at every hold end, until the user acts
//! (`one_wrong_password_is_asked_three_times_in_the_first_hour_and_once_an_hour_after`).
//! A secret never refused carries no mark and is dialled by as many tasks
//! as ask, as the pool opens its sessions (`a_secret_never_refused_is_dialled_concurrently`),
//! so the first refusal of a new password may come back from every dial
//! under way then, at most one per task that dials (the sync tick, the IDLE
//! watcher, the outbox drain); they count as one refusal, and the next step
//! is one dial (`the_first_refusal_of_a_new_password_is_met_by_the_dials_under_way`).
//!
//! The hold ends early when the user acts, through [`forget`] or
//! [`forget_account`]: stores a password (`config.set_password`), signs in
//! again (`config.oauth2_login`), or starts what needs the login (a sync for
//! the IMAP login and the Graph sign-in, a send for the SMTP login and the
//! Graph sign-in). That ends the hold of the secret the login dials with,
//! read where its dial reads it, for every login of its row, and of no other
//! secret: a send lets an IMAP password that differs from the SMTP one stay
//! held (`the_user_acting_on_a_login_not_seen_yet_ends_only_its_own_secrets_hold`).
//! It lets go of a dial of the row in flight as well, so the user's own dial
//! asks the server at once, beside it, at the cost of one more login per
//! user action, and the superseded dial's late end leaves the user's mark
//! alone (`the_user_acting_gets_past_a_dial_in_flight`, and end to end
//! `imap_client::pool`'s `a_sync_the_user_starts_gets_past_a_dial_in_flight`).
//! The count stays, so a refusal right after is the next in the row
//! (`the_user_acting_ends_the_hold_for_both_logins_and_keeps_the_row`).
//!
//! # The end of a row
//!
//! A row, count and all, ends when a login of the row succeeds with the
//! secret, since the server just took it (`a_success_at_either_login_resets_both`;
//! an SMTP login counts as a success once the server answered past it, even
//! when every recipient is refused:
//! `crate::send`'s `a_login_taken_with_every_recipient_refused_resets_the_count`),
//! when no login is attempted with its identity and secret any more
//! (`a_replaced_password_drops_its_row`), and when the daemon restarts, since
//! the list lives in memory only. A new secret has no row, so it is asked at
//! once.
//!
//! # What the clients read
//!
//! The health check and `account.credentials` read no secret: they report a
//! login refused while the row of the secret it was last attempted with
//! holds, so the SMTP login of a shared password is reported once it was
//! attempted within the hold, and a dial in flight is not reported
//! (`a_dial_in_flight_holds_the_others_of_its_row`). `config.probe` dials
//! past the gate: a probe is the user asking the server.

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

/// How long a dial of a login may wait for the server once its connection is
/// open, through the TLS handshake, STARTTLS and the login itself, before it
/// is given up on as no answer, which is no refusal (PERSO-296).
///
/// The IMAP dial fails at it; an SMTP submission, which cannot tell its login
/// from the first recipient's upload, lets go of its mark at it and goes on;
/// a Graph token refresh fails at it. Generous on purpose, as lettre's own
/// 60 s command timeout is: a slow server on a bad link must not lose its
/// login to it.
pub const LOGIN_DEADLINE: Duration = Duration::from_secs(60);

/// How long a dial's in-flight mark holds the other dials of its row: past
/// it the mark is a dial that outlived every deadline above (the IMAP
/// connect's 30 s and [`LOGIN_DEADLINE`] after it), and the row is free
/// again, so no hung dial can stop the mail of its row for longer.
const MARK_LIFETIME: Duration = Duration::from_secs(LOGIN_DEADLINE.as_secs() + 2 * 60);

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
/// asked again, or that another login with it is being tried right now.
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
    /// Why the gate answered without dialling, `None` for an answer the
    /// server sent.
    held: Option<Held>,
}

/// Why the gate answered a dial without dialling.
#[derive(Clone, Copy, Debug)]
enum Held {
    /// The hold runs, and the server is asked again at this time.
    Until(SystemTime),
    /// The hold ended and another dial of the row is asking the server now.
    Trying,
}

impl LoginRejected {
    /// The refusal of a `kind` login the server just sent.
    pub fn new(kind: RejectedLogin, answer: String) -> Self {
        Self {
            kind,
            refused_at: kind,
            answer,
            entry: None,
            held: None,
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
        self.held.is_some()
    }
}

impl fmt::Display for LoginRejected {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            RejectedLogin::Imap => write!(f, "IMAP login failed: {}", self.answer)?,
            RejectedLogin::Smtp => write!(f, "SMTP login failed: {}", self.answer)?,
            RejectedLogin::Graph => write!(f, "OAuth2 sign-in refused: {}", self.answer)?,
        }
        if let Some(held) = self.held {
            let (who, what, attempt, again) = match self.kind {
                RejectedLogin::Imap => (
                    "the server",
                    "this password",
                    "a login",
                    "a sync is started",
                ),
                RejectedLogin::Smtp => (
                    "the server",
                    "this password",
                    "a login",
                    "a message is sent",
                ),
                RejectedLogin::Graph => (
                    "Microsoft",
                    "this sign-in",
                    "a sign-in",
                    "a sync or a send is started",
                ),
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
            match held {
                Held::Until(at) => {
                    let at = chrono::DateTime::<chrono::Local>::from(at).format("%H:%M");
                    write!(
                        f,
                        " ({who} refused {what}{elsewhere} earlier and is asked again at {at}, or \
                         at once after {set} or {again})"
                    )?;
                }
                Held::Trying => write!(
                    f,
                    " ({who} refused {what}{elsewhere} earlier and {attempt} with it is being \
                     tried right now; {who} is asked again at once after {set} or {again})"
                )?,
            }
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

/// End the hold of the secret `login` dials with, read from where the dial
/// reads it ([`stored_secret`]), for every login sharing its row, and let go
/// of a dial of the row in flight, so the next attempt asks the server; the
/// count of refusals in a row stays, since the user acting is no success. A
/// secret that cannot be read is the one `login` was last attempted with, if
/// any. The hold of any other secret
/// runs on: a send does not let go of a different IMAP password.
pub fn forget(login: &Login) {
    let stored = stored_secret(login).map(|secret| fingerprint(&secret));
    refusals().release(login, stored, Instant::now());
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

/// The secret `login` dials with, read as its dial reads it: the IMAP
/// password falling back to the SMTP one ([`crate::config::ImapConfig::load`]),
/// the SMTP password, or the refresh token of the Graph sign-in's token cache.
fn stored_secret(login: &Login) -> Option<String> {
    match login {
        Login::Imap(server) => crate::config::imap_password_of(&server.account).ok(),
        Login::Smtp(server) => {
            crate::config::get_secret(&format!("smtp-password-{}", server.account)).ok()
        }
        Login::Graph(account) => {
            crate::oauth2::load_token_cache(account).map(|cache| cache.refresh_token)
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

/// The refusals in a row of `login`'s `secret`, 0 when the list holds none,
/// for a test outside this module that needs the count.
#[cfg(test)]
pub(crate) fn in_a_row_for_test(login: &Login, secret: &str) -> u32 {
    refusals()
        .rows
        .get(&RowKey::of(login, &fingerprint(secret)))
        .map_or(0, |row| row.in_a_row)
}

/// Ask the gate to dial `login` with `secret`: the refusal to answer at once
/// when the row of the secret holds, or a dial in flight of a secret the
/// server refused before, and else a [`Dial`] to report the answer through.
///
/// A dial of a secret the server refused before marks its row in flight
/// until it answers or is dropped, so the tasks that come meanwhile (the sync
/// tick, the IDLE watcher, the outbox drain, a held hold ending for all of
/// them at once) are answered as held rather than dialling beside it. A
/// secret never refused is dialled by as many tasks at once as ask, as the
/// pool does.
pub fn dial(login: &Login, secret: &str) -> Result<Dial, LoginRejected> {
    let secret = fingerprint(secret);
    let mark = refusals().begin(login, &secret, Instant::now(), SystemTime::now())?;
    Ok(Dial {
        login: login.clone(),
        secret,
        mark,
    })
}

/// One dial the gate let through. Report its answer with [`Dial::accepted`]
/// or [`Dial::refused`]; dropped on any other outcome (another error, a
/// timeout, a cancelled task), it frees the row it marked in flight.
pub struct Dial {
    login: Login,
    secret: [u8; 32],
    /// The token of the in-flight mark on the row of the secret, when it had
    /// a row.
    mark: Option<u64>,
}

impl Dial {
    /// The server took the login, so whatever the list held about the
    /// secret, the count of refusals in a row included, no longer describes
    /// the server, for every login of its row.
    pub fn accepted(&mut self) {
        let mut list = refusals();
        list.accept(&self.login, &self.secret);
        if let Some(mark) = self.mark.take() {
            list.end(&self.login, &self.secret, mark);
        }
    }

    /// The server refused the login: record it, and answer the refusal with
    /// its entry.
    pub fn refused(&mut self, rejected: &LoginRejected) -> LoginRejected {
        let mut list = refusals();
        let recorded = list.record(self.login.clone(), self.secret, rejected, Instant::now());
        if let Some(mark) = self.mark.take() {
            list.end(&self.login, &self.secret, mark);
        }
        recorded
    }

    /// Whether the dial still marks its row in flight.
    pub fn marked(&self) -> bool {
        self.mark.is_some()
    }

    /// Free the row this dial marked without an answer, for a dial that goes
    /// on past [`LOGIN_DEADLINE`]; its answer, when it comes, is still
    /// reported.
    pub fn let_go(&mut self) {
        if let Some(mark) = self.mark.take() {
            refusals().end(&self.login, &self.secret, mark);
        }
    }
}

impl Drop for Dial {
    fn drop(&mut self) {
        self.let_go();
    }
}

/// Record that the server refused `login` with `secret`, as a refused dial
/// does, for a test outside this module.
#[cfg(test)]
pub(crate) fn record(login: &Login, secret: &str, rejected: &LoginRejected) -> LoginRejected {
    refusals().record(login.clone(), fingerprint(secret), rejected, Instant::now())
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

/// What a refused secret is kept under: the login identity and the secret,
/// not the login, so every login that presents the same user and password
/// shares one row, whatever its host and whichever account it is configured
/// in.
///
/// The identity of a password login is its user name, ASCII-lowercased (mail
/// servers compare it so); a Graph sign-in's is its account's name, which its
/// token cache is kept under, and `sign_in` keeps it apart from a password.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct RowKey {
    identity: String,
    sign_in: bool,
    secret: [u8; 32],
}

impl RowKey {
    fn of(login: &Login, secret: &[u8; 32]) -> Self {
        Self {
            identity: identity(login),
            sign_in: matches!(login, Login::Graph(_)),
            secret: *secret,
        }
    }
}

/// The identity `login` presents to its server, apart from its secret.
fn identity(login: &Login) -> String {
    match login {
        Login::Imap(server) | Login::Smtp(server) => server.username.to_ascii_lowercase(),
        Login::Graph(account) => account.clone(),
    }
}

/// One refused secret of one login identity: its schedule, shared by every
/// login that presents them.
struct Row {
    /// When the hold ends and the server is asked again.
    until: Instant,
    /// The refusals of the secret in a row, at any login of the row, this
    /// one included.
    in_a_row: u32,
    entry: u64,
    /// The login the server last refused the secret at, and its answer.
    refused_at: RejectedLogin,
    answer: String,
    /// The dial of the secret in flight since the hold ended, which holds
    /// every other dial of the row until it answers or its mark goes stale.
    in_flight: Option<Mark>,
}

/// A dial's in-flight mark on its row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Mark {
    /// Which dial took it.
    token: u64,
    /// When it was taken.
    taken: Instant,
}

impl Row {
    fn holds(&self, now: Instant) -> bool {
        now < self.until
    }

    /// Whether a dial of the row is in flight at `now`: a mark older than
    /// [`MARK_LIFETIME`] is a dial that hung past every deadline, and holds
    /// nothing.
    fn in_flight(&self, now: Instant) -> bool {
        self.in_flight
            .is_some_and(|mark| now.saturating_duration_since(mark.taken) < MARK_LIFETIME)
    }
}

/// The list itself, with the clock passed in so a test can step past a hold
/// without waiting for it.
#[derive(Default)]
struct Refusals {
    /// The refused secrets, by login identity and secret.
    rows: HashMap<RowKey, Row>,
    /// The secret each login was last attempted with, which a caller that
    /// reads no secret (the health check) and the user's action find the
    /// login's row by.
    seen: HashMap<Login, [u8; 32]>,
    next_entry: u64,
    next_mark: u64,
}

impl Refusals {
    /// Note that `login` is attempted with `secret`, and drop the rows no
    /// login is attempted with any more: a password replaced is a row nothing
    /// reads.
    fn see(&mut self, login: &Login, secret: &[u8; 32]) {
        if self.seen.get(login) == Some(secret) {
            return;
        }
        self.seen.insert(login.clone(), *secret);
        let live: Vec<RowKey> = self
            .seen
            .iter()
            .map(|(seen, secret)| RowKey::of(seen, secret))
            .collect();
        self.rows.retain(|key, _| live.contains(key));
    }

    /// The answer the gate gives a `login` with `secret` at `now`, if the hold
    /// of the secret runs, whichever login of the row it was refused at; `wall` is the wall clock at `now`, which the answer's time is told
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
        (row.holds(now) || row.in_flight(now)).then(|| LoginRejected {
            kind: login.kind(),
            refused_at: row.refused_at,
            answer: row.answer.clone(),
            entry: Some(row.entry),
            held: Some(if row.holds(now) {
                Held::Until(wall + row.until.saturating_duration_since(now))
            } else {
                // The hold ended, and a dial in flight is asking the server.
                Held::Trying
            }),
        })
    }

    /// Let a dial of `login` with `secret` through at `now`, or answer it
    /// as [`Refusals::refused`] does; a dial of a secret with a row marks the
    /// row in flight and answers the mark's token.
    fn begin(
        &mut self,
        login: &Login,
        secret: &[u8; 32],
        now: Instant,
        wall: SystemTime,
    ) -> Result<Option<u64>, LoginRejected> {
        if let Some(held) = self.refused(login, secret, now, wall) {
            return Err(held);
        }
        let Some(row) = self.rows.get_mut(&RowKey::of(login, secret)) else {
            return Ok(None);
        };
        self.next_mark += 1;
        row.in_flight = Some(Mark {
            token: self.next_mark,
            taken: now,
        });
        Ok(Some(self.next_mark))
    }

    /// The dial marked `mark` answered or was dropped: its row, if its mark
    /// is still this dial's, is free for the next dial. A later dial's mark,
    /// taken once this one's went stale, stays.
    fn end(&mut self, login: &Login, secret: &[u8; 32], mark: u64) {
        if let Some(row) = self.rows.get_mut(&RowKey::of(login, secret)) {
            if row.in_flight.is_some_and(|held| held.token == mark) {
                row.in_flight = None;
            }
        }
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
                        in_flight: None,
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

    /// End at `now` the hold of the row of `login`'s secret, keeping its
    /// count: `stored`, the secret its next dial reads, or the one it was
    /// last attempted with when that cannot be read. Nothing else is let go.
    ///
    /// The mark of a dial in flight goes too, so the user's own dial asks the
    /// server at once, beside it: one more login per user action. The
    /// superseded dial's end then finds another mark, or none, and leaves it
    /// ([`Refusals::end`]).
    fn release(&mut self, login: &Login, stored: Option<[u8; 32]>, now: Instant) {
        let Some(secret) = stored.or_else(|| self.seen.get(login).copied()) else {
            return;
        };
        if let Some(row) = self.rows.get_mut(&RowKey::of(login, &secret)) {
            row.until = row.until.min(now);
            row.in_flight = None;
        }
    }

    /// `login` succeeded with `secret`: drop its row, count and all, for
    /// every login that shares it.
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

    /// `login` as `user`.
    fn as_user(login: Login, user: &str) -> Login {
        match login {
            Login::Imap(server) => Login::Imap(ServerLogin {
                username: user.into(),
                ..server
            }),
            Login::Smtp(server) => Login::Smtp(ServerLogin {
                username: user.into(),
                ..server
            }),
            graph => graph,
        }
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
        let someone_else = as_user(imap(), "bob@example.com");
        assert!(list.refused(&someone_else, &wrong, now, wall()).is_none());

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

    /// The identity case (a): one account, one user and one password on two
    /// hosts, the university's IMAP and SMTP servers, is one row.
    #[test]
    fn one_user_and_password_on_two_hosts_is_one_row() {
        let mut list = Refusals::default();
        let at = Instant::now();
        assert_ne!(imap(), smtp());
        let first = held_for(&mut list, &imap(), "wrong", at);
        assert_eq!(
            held_for(&mut list, &smtp(), "wrong", at + first),
            SECOND_HOLD
        );
    }

    /// The identity case (b): one account whose IMAP and SMTP users differ
    /// (a typo in one) but share a password is two rows: the wrong user's
    /// refusal holds nothing of the right one, and the right one's success
    /// resets nothing of the wrong one.
    #[test]
    fn two_users_with_one_password_are_two_rows() {
        let mut list = Refusals::default();
        let at = Instant::now();
        let typo = as_user(imap(), "alpha@exmaple.com");
        let wrong = fingerprint("shared");
        let first = held_for(&mut list, &typo, "shared", at);
        assert!(
            list.refused(&smtp(), &wrong, at + SECOND, wall()).is_none(),
            "the right user is not held by the typo's refusal"
        );
        list.accept(&smtp(), &wrong);
        assert!(
            list.refused(&typo, &wrong, at + SECOND, wall()).is_some(),
            "the right user's success let go of nothing"
        );
        assert_eq!(
            held_for(&mut list, &typo, "shared", at + first),
            SECOND_HOLD,
            "the typo's count went on"
        );
    }

    /// The identity case (c): two configured accounts logging in as the
    /// same user on the same server with the same wrong password are one
    /// row, so together they reach the server 3 times in the first hour and
    /// once an hour after.
    #[test]
    fn two_accounts_as_one_user_are_one_row() {
        let mut list = Refusals::default();
        let start = Instant::now();
        let (work, copy) = (
            imap_of("work", "imap.tum.test"),
            imap_of("copy", "imap.tum.test"),
        );
        let wrong = fingerprint("wrong");
        let mut dials_per_hour = [0u32; 3];
        let mut t = Duration::ZERO;
        while t < 3 * LONG_HOLD {
            for login in [&work, &copy] {
                if list.refused(login, &wrong, start + t, wall()).is_none() {
                    dials_per_hour[(t.as_secs() / 3600) as usize] += 1;
                    list.record(login.clone(), wrong, &rejected(login), start + t);
                }
            }
            t += 30 * SECOND;
        }
        assert_eq!(dials_per_hour, [3, 1, 1]);
    }

    /// A password no login is attempted with any more is a row nothing
    /// reads, and it goes: the same password tried again later starts a new
    /// row.
    #[test]
    fn a_replaced_password_drops_its_row() {
        let mut list = Refusals::default();
        let at = Instant::now();
        let first = held_for(&mut list, &imap(), "old", at);
        held_for(&mut list, &imap(), "old", at + first);
        assert!(list
            .refused(&imap(), &fingerprint("new"), at + first, wall())
            .is_none());
        assert_eq!(
            held_for(&mut list, &imap(), "old", at + LONG_HOLD),
            FIRST_HOLD,
            "the old password's count went with its row"
        );
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
        list.release(&imap(), None, now);
        assert!(!list.holds(&imap(), now));
        for login in [imap(), smtp()] {
            assert!(list
                .refused(&login, &fingerprint("wrong"), now, wall())
                .is_none());
        }
        assert_eq!(held_for(&mut list, &smtp(), "wrong", now), SECOND_HOLD);
    }

    /// The user acting on a login the gate has not seen dialled yet ends the
    /// hold of the secret that login dials with, read where the dial reads
    /// it, and of no other: a send started while the IMAP password A is held
    /// for an hour and the SMTP password is B lets A's hold run, and one
    /// started when both use A ends it.
    #[test]
    fn the_user_acting_on_a_login_not_seen_yet_ends_only_its_own_secrets_hold() {
        let at = Instant::now();
        let now = at + SECOND;
        let a = fingerprint("password A");

        let mut list = Refusals::default();
        list.record(imap(), a, &rejected(&imap()), at);
        list.release(&smtp(), Some(fingerprint("password B")), now);
        assert!(
            list.refused(&imap(), &a, now, wall()).is_some(),
            "a send on another password lets the IMAP hold run"
        );
        list.release(&smtp(), None, now);
        assert!(
            list.refused(&imap(), &a, now, wall()).is_some(),
            "nor does a send whose password cannot be read"
        );
        list.release(&smtp(), Some(a), now);
        assert!(
            list.refused(&imap(), &a, now, wall()).is_none(),
            "a send on the same password ends the shared hold"
        );
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

    /// The worst case the module doc promises, under the race: one wrong
    /// password the IMAP and the SMTP login share, with the sync tick
    /// dialling IMAP every minute, the IDLE watcher every 30 s and the
    /// outbox drain dialling SMTP every 30 s, all three asking the gate
    /// before any of them hears back, reaches the server 3 times in the
    /// first hour after its first refusal and once an hour after.
    #[test]
    fn one_wrong_password_is_asked_three_times_in_the_first_hour_and_once_an_hour_after() {
        let mut list = Refusals::default();
        let start = Instant::now();
        let wrong = fingerprint("wrong");
        // The first refusal, which starts the row.
        list.record(imap(), wrong, &rejected(&imap()), start);
        let mut dials_per_hour = [1u32, 0, 0, 0, 0];
        let mut t = 30 * SECOND;
        while t < 5 * LONG_HOLD {
            let now = start + t;
            let mut racing = vec![imap(), smtp()];
            if t.as_secs().is_multiple_of(60) {
                racing.push(imap());
            }
            // Every task asks before any dial answers.
            let through: Vec<(Login, u64)> = racing
                .into_iter()
                .filter_map(|login| match list.begin(&login, &wrong, now, wall()) {
                    Ok(mark) => Some((login, mark.expect("the row is marked"))),
                    Err(_) => None,
                })
                .collect();
            for (login, mark) in through {
                dials_per_hour[(t.as_secs() / 3600) as usize] += 1;
                list.record(login.clone(), wrong, &rejected(&login), now);
                list.end(&login, &wrong, mark);
            }
            t += 30 * SECOND;
        }
        assert_eq!(dials_per_hour, [3, 1, 1, 1, 1]);
    }

    /// A dial of a secret refused before marks its row in flight: every
    /// other dial of the row, at either login, is held until it answers or
    /// is dropped, and then the next one is let through.
    #[test]
    fn a_dial_in_flight_holds_the_others_of_its_row() {
        let mut list = Refusals::default();
        let at = Instant::now();
        let wrong = fingerprint("wrong");
        list.record(imap(), wrong, &rejected(&imap()), at);
        let now = at + FIRST_HOLD;
        let mark = list
            .begin(&imap(), &wrong, now, wall())
            .expect("the hold ended")
            .expect("the row is marked");
        for login in [imap(), smtp()] {
            let held = list.begin(&login, &wrong, now, wall()).unwrap_err();
            assert!(held.from_list(), "{held}");
        }
        assert!(
            !list.holds(&imap(), now),
            "a dial in flight is no refusal the health check reports"
        );
        list.end(&imap(), &wrong, mark);
        assert!(list.begin(&smtp(), &wrong, now, wall()).is_ok());
    }

    /// The user acting gets past a dial in flight: the release clears its
    /// mark, so the user's own dial asks the server at once, and the late
    /// end of the superseded dial leaves the user's mark alone.
    #[test]
    fn the_user_acting_gets_past_a_dial_in_flight() {
        let mut list = Refusals::default();
        let at = Instant::now();
        let wrong = fingerprint("wrong");
        list.record(imap(), wrong, &rejected(&imap()), at);
        let now = at + FIRST_HOLD;
        let superseded = list
            .begin(&imap(), &wrong, now, wall())
            .expect("the hold ended")
            .expect("the row is marked");

        list.release(&smtp(), Some(wrong), now);
        let user = list
            .begin(&smtp(), &wrong, now, wall())
            .expect("the user's send asks the server at once")
            .expect("and marks the row");
        list.end(&imap(), &wrong, superseded);
        assert!(
            list.begin(&imap(), &wrong, now, wall()).is_err(),
            "the superseded dial's end left the user's mark"
        );
        list.end(&smtp(), &wrong, user);
        assert!(list.begin(&imap(), &wrong, now, wall()).is_ok());
    }

    /// A dial held only because another dial of its row is in flight is
    /// told so, not given a time the server is asked again at.
    #[test]
    fn a_dial_held_by_one_in_flight_says_a_login_is_being_tried() {
        let mut list = Refusals::default();
        let at = Instant::now();
        let wrong = fingerprint("wrong");
        list.record(imap(), wrong, &rejected(&imap()), at);
        let now = at + FIRST_HOLD;
        list.begin(&imap(), &wrong, now, wall()).unwrap();
        let held = list.begin(&smtp(), &wrong, now, wall()).unwrap_err();
        assert!(held.from_list());
        let text = held.to_string();
        assert!(
            text.ends_with(
                "(the server refused this password at the IMAP login earlier and a login with it \
                 is being tried right now; the server is asked again at once after the password \
                 is set again or a message is sent)"
            ),
            "{text}"
        );
    }

    /// A mark older than [`MARK_LIFETIME`] is a dial that hung past every
    /// deadline, and the row is free for the next dial; the hung dial's late
    /// end leaves the newer mark alone.
    #[test]
    fn a_mark_older_than_any_dial_is_free() {
        let mut list = Refusals::default();
        let at = Instant::now();
        let wrong = fingerprint("wrong");
        list.record(imap(), wrong, &rejected(&imap()), at);
        let now = at + FIRST_HOLD;
        let hung = list
            .begin(&imap(), &wrong, now, wall())
            .expect("the hold ended")
            .expect("the row is marked");
        let before = now + MARK_LIFETIME - SECOND;
        assert!(list.refused(&smtp(), &wrong, before, wall()).is_some());
        assert!(list.begin(&smtp(), &wrong, before, wall()).is_err());

        let then = now + MARK_LIFETIME;
        assert!(
            list.refused(&smtp(), &wrong, then, wall()).is_none(),
            "a stale mark holds nothing"
        );
        let next = list
            .begin(&smtp(), &wrong, then, wall())
            .expect("a stale mark is free")
            .expect("the row is marked again");
        list.end(&imap(), &wrong, hung);
        assert!(
            list.begin(&imap(), &wrong, then, wall()).is_err(),
            "the hung dial's end left the new mark"
        );
        list.end(&smtp(), &wrong, next);
        assert!(list.begin(&imap(), &wrong, then, wall()).is_ok());
    }

    /// A secret the server never refused is dialled by every task that
    /// asks, unmarked, as the pool opens its sessions; so is a secret whose
    /// row a success dropped.
    #[test]
    fn a_secret_never_refused_is_dialled_concurrently() {
        let mut list = Refusals::default();
        let now = Instant::now();
        let right = fingerprint("right");
        for _ in 0..4 {
            assert_eq!(list.begin(&imap(), &right, now, wall()).unwrap(), None);
        }
    }

    /// The race the mark cannot close: a password never refused before is
    /// dialled by the tasks that ask together, so its first refusal may come
    /// back from as many dials as were in flight. The step after is one dial.
    #[test]
    fn the_first_refusal_of_a_new_password_is_met_by_the_dials_under_way() {
        let mut list = Refusals::default();
        let now = Instant::now();
        let wrong = fingerprint("wrong");
        let through = [imap(), imap(), smtp()]
            .iter()
            .filter(|login| list.begin(login, &wrong, now, wall()).is_ok())
            .count();
        assert_eq!(through, 3);
        let first = list.record(imap(), wrong, &rejected(&imap()), now);
        let raced = list.record(smtp(), wrong, &rejected(&smtp()), now);
        assert_eq!(first.entry(), raced.entry(), "counted as one refusal");
        let then = now + FIRST_HOLD;
        let next = [imap(), smtp()]
            .iter()
            .filter(|login| list.begin(login, &wrong, then, wall()).is_ok())
            .count();
        assert_eq!(next, 1);
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
        list.release(&imap(), None, now);
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
