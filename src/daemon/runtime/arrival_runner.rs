//! The per-account arrival runner (PERSO-186): after every tick, tell the
//! clients about the new inbox mail worth a notification, once the post-tick
//! judges are done with it.
//!
//! A tick queues the mail its pass announced on the runtime
//! ([`AccountRuntime::take_arrivals`]) before it moves
//! [`AccountRuntime::judging`], the record this runner, the blocked senders
//! and the mail hooks all wait on; a guarded `mp sync --mailbox` that
//! announced mail does the same ([`AccountRuntime::note_pass`]) and leaves
//! the scheduler's [`AccountRuntime::completed_ticks`] alone. Either judge may move a message
//! that tick brought in to Junk, so the runner waits until every judge the
//! account runs has reported that tick done ([`AccountRuntime::judged`]), then
//! reads the queued rows again ([`crate::daemon::arrivals::worth_notifying`])
//! and publishes what survives as one `mail.arrived` per tick, naming the
//! connection that presents it ([`CanonicalState::presenter`]), as a reminder
//! does. A hook that runs long holds the notification back for as long; its
//! own timeout bounds that.
//!
//! # What gets a runner
//!
//! A ready runtime of an account with a server: the runtimes the blocked
//! senders judge. A blocked runtime ingests nothing, and a local-only account
//! has no mail arriving.
//!
//! # Lifetime
//!
//! The scheduler's rules: spawned for one runtime, held weakly, stopped by its
//! retirement or its drop.

use std::sync::{Arc, Weak};

use futures::future::{BoxFuture, FutureExt};
use log::{debug, info, warn};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use mp_protocol::events::{Arrival, MailArrived, KIND_MAIL_ARRIVED};

use crate::config::AccountConfig;
use crate::daemon::senders;
use crate::daemon::state::events::Event as DaemonEvent;
use crate::daemon::state::{CanonicalState, ConnectionId};
use crate::sync::InboxArrival;

use super::account::{AccountRuntime, Judge, Readiness};

/// The post-tick judges an account runs, which its new mail waits for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Judges {
    /// Whether the blocked senders' runner runs for the account.
    pub senders: bool,
    /// Whether the mail hooks' runner runs for the account.
    pub hooks: bool,
}

impl Judges {
    fn running(self) -> impl Iterator<Item = Judge> {
        [(self.senders, Judge::Senders), (self.hooks, Judge::Hooks)]
            .into_iter()
            .filter_map(|(runs, judge)| runs.then_some(judge))
    }
}

/// What the runner does with one tick's judged arrivals: the production one
/// reads them again and publishes `mail.arrived`, a test's records them.
pub type Announce = Arc<dyn Fn(Vec<InboxArrival>) -> BoxFuture<'static, ()> + Send + Sync>;

/// Start the runner for `runtime`, if its account can receive mail.
pub fn spawn(
    runtime: &Arc<AccountRuntime>,
    canonical: Arc<CanonicalState>,
    cfg: &AccountConfig,
    judges: Judges,
) -> Option<JoinHandle<()>> {
    if cfg.is_local_only() || runtime.readiness() != Readiness::Ready {
        return None;
    }
    let cfg = Arc::new(cfg.clone());
    let announce: Announce = Arc::new(move |due| {
        let canonical = Arc::clone(&canonical);
        let cfg = Arc::clone(&cfg);
        async move { announce(&canonical, &cfg, due).await }.boxed()
    });
    Some(tokio::spawn(run(
        Arc::downgrade(runtime),
        runtime.retired(),
        runtime.judging(),
        judges,
        announce,
    )))
}

/// Resolve once `retired` goes `true` or its runtime is dropped.
async fn stopped(retired: &mut watch::Receiver<bool>) {
    let _ = retired.wait_for(|retired| *retired).await;
}

/// Resolve once every judge in `judges` is done with `tick`; `false` when the
/// runtime went away first.
async fn judged(runtime: &Weak<AccountRuntime>, judges: Judges, tick: Instant) -> bool {
    for judge in judges.running() {
        let Some(mut through) = runtime.upgrade().map(|live| live.judged_through(judge)) else {
            return false;
        };
        if through.wait_for(|done| *done >= tick).await.is_err() {
            return false;
        }
    }
    true
}

/// One account's runner, until the runtime it was spawned for retires.
async fn run(
    runtime: Weak<AccountRuntime>,
    mut retired: watch::Receiver<bool>,
    mut ticks: watch::Receiver<Instant>,
    judges: Judges,
    announce: Announce,
) {
    let mut account = String::new();
    loop {
        let tick = *ticks.borrow_and_update();
        if *retired.borrow() {
            break;
        }
        let ready = tokio::select! {
            biased;
            () = stopped(&mut retired) => break,
            ready = judged(&runtime, judges, tick) => ready,
        };
        let Some(live) = runtime.upgrade().filter(|_| ready) else {
            break;
        };
        account = live.account().to_string();
        let due = live.take_arrivals(tick);
        drop(live);
        if !due.is_empty() {
            announce(due).await;
        }
        tokio::select! {
            biased;
            () = stopped(&mut retired) => break,
            changed = ticks.changed() => {
                if changed.is_err() {
                    break;
                }
            }
        }
    }
    info!("[arrivals] {account}'s runtime retired; stopped announcing its mail");
}

/// Read `due` again and publish what is still worth a notification.
async fn announce(canonical: &CanonicalState, cfg: &Arc<AccountConfig>, due: Vec<InboxArrival>) {
    let account = cfg.name.clone();
    let judge_cfg = Arc::clone(cfg);
    let judged = tokio::task::spawn_blocking(move || {
        let Some(store) = crate::store::open_store(&judge_cfg.name) else {
            return anyhow::Ok(Vec::new());
        };
        // A blocked-senders file that does not parse blocks nobody here: the
        // scan has already said so, and losing the notification of every
        // message over it would hide the account's mail as well.
        let blocked = senders::load(&senders::list_path()).unwrap_or_else(|e| {
            warn!("[arrivals] {}: {e:#}", judge_cfg.name);
            Vec::new()
        });
        crate::daemon::arrivals::worth_notifying(&store, &judge_cfg, &blocked, &due)
    })
    .await;
    let arrivals = match judged {
        Ok(Ok(arrivals)) => arrivals,
        Ok(Err(e)) => return warn!("[arrivals] reading the new mail of {account}: {e:#}"),
        Err(e) => return warn!("[arrivals] the read task of {account} {e}"),
    };
    if arrivals.is_empty() {
        return debug!("[arrivals] {account}: nothing new worth a notification");
    }
    let presenter = canonical.presenter();
    info!(
        "[arrivals] {account}: {} new message(s) (presenter {})",
        arrivals.len(),
        presenter.map_or("none".to_string(), |(conn, kind)| format!(
            "{kind} connection {}",
            conn.0
        ))
    );
    if let Some(event) = mail_arrived(account, arrivals, presenter) {
        canonical.publish(event);
    }
}

/// The `mail.arrived` event of `arrivals`, naming `presenter`, the connection
/// [`CanonicalState::presenter`] elected, as the one that notifies.
fn mail_arrived(
    account: String,
    arrivals: Vec<Arrival>,
    presenter: Option<(ConnectionId, &str)>,
) -> Option<DaemonEvent> {
    let payload = MailArrived {
        account,
        arrivals,
        presenter: presenter.map(|(_, kind)| kind.to_string()),
        presenter_connection: presenter.map(|(conn, _)| conn.0),
    };
    match serde_json::to_value(&payload) {
        Ok(payload) => Some(DaemonEvent::Lifecycle {
            kind: KIND_MAIL_ARRIVED,
            payload,
        }),
        Err(e) => {
            warn!("[arrivals] could not serialise a mail.arrived: {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::time::Duration;

    use super::super::account::{DrainHook, TickHooks, TickKind, TickReport};
    use crate::sync::SyncResult;

    /// A runtime whose every tick's body "syncs" the next batch of `bodies`.
    fn runtime(dir: &std::path::Path, bodies: Vec<Vec<InboxArrival>>) -> Arc<AccountRuntime> {
        let report = TickReport::default();
        let queue = Arc::new(Mutex::new(bodies));
        let quiet: DrainHook = Arc::new(|_ctx| async { String::new() }.boxed());
        let body_report = report.clone();
        let hooks = TickHooks {
            outbox: Arc::clone(&quiet),
            mutations: quiet,
            body: Arc::new(move |_ctx| {
                let mut queue = queue.lock().unwrap();
                let arrivals = if queue.is_empty() {
                    Vec::new()
                } else {
                    queue.remove(0)
                };
                body_report.record_sync(SyncResult {
                    new_inbox_mail: arrivals,
                    ..SyncResult::default()
                });
                async { Ok(()) }.boxed()
            }),
        };
        let cfg = AccountConfig {
            name: "alpha".to_string(),
            ..Default::default()
        };
        Arc::new(
            AccountRuntime::start_at_with_hooks(dir, cfg, 1, hooks)
                .unwrap()
                .with_report(report),
        )
    }

    fn arrival(subject: &str) -> InboxArrival {
        InboxArrival {
            row_id: 1,
            message_id: format!("<{subject}@example.com>"),
            from: "ada@example.com".to_string(),
            subject: subject.to_string(),
        }
    }

    /// The subjects of each announcement, in order.
    type Announced = Arc<Mutex<Vec<Vec<String>>>>;

    /// The runner over `runtime`, recording each announcement's subjects.
    fn runner(runtime: &Arc<AccountRuntime>, judges: Judges) -> (JoinHandle<()>, Announced) {
        let announced = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&announced);
        let announce: Announce = Arc::new(move |due: Vec<InboxArrival>| {
            record
                .lock()
                .unwrap()
                .push(due.into_iter().map(|a| a.subject).collect());
            async {}.boxed()
        });
        let task = tokio::spawn(run(
            Arc::downgrade(runtime),
            runtime.retired(),
            runtime.judging(),
            judges,
            announce,
        ));
        (task, announced)
    }

    async fn settle() {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    /// A tick's new mail is announced only once every judge the account runs
    /// has reported that tick done, and once; a tick that brought nothing
    /// announces nothing.
    #[tokio::test]
    async fn new_mail_waits_for_every_judge_and_is_announced_once() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime(dir.path(), vec![vec![arrival("one"), arrival("two")]]);
        let judges = Judges {
            senders: true,
            hooks: true,
        };
        let (task, announced) = runner(&runtime, judges);

        runtime.tick(TickKind::Quick).await;
        let tick = *runtime.completed_ticks().borrow();
        settle().await;
        assert!(announced.lock().unwrap().is_empty(), "no judge is done yet");

        runtime.judged(Judge::Senders, tick);
        settle().await;
        assert!(
            announced.lock().unwrap().is_empty(),
            "the hooks are still running"
        );

        runtime.judged(Judge::Hooks, tick);
        settle().await;
        assert_eq!(
            *announced.lock().unwrap(),
            vec![vec!["one".to_string(), "two".to_string()]]
        );

        runtime.tick(TickKind::Quick).await;
        let quiet = *runtime.completed_ticks().borrow();
        runtime.judged(Judge::Senders, quiet);
        runtime.judged(Judge::Hooks, quiet);
        settle().await;
        assert_eq!(
            announced.lock().unwrap().len(),
            1,
            "nothing new, nothing said"
        );

        drop(runtime);
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("the runner stops with its runtime")
            .unwrap();
    }

    /// The event names the elected presenter's connection and kind, and none
    /// when only the CLI is connected.
    #[test]
    fn the_event_names_the_presenter() {
        let arrivals = vec![Arrival {
            from: "Ada <ada@example.com>".to_string(),
            subject: "Engine".to_string(),
        }];
        let payload =
            |presenter| match mail_arrived("alpha".to_string(), arrivals.clone(), presenter) {
                Some(DaemonEvent::Lifecycle { kind, payload }) => {
                    assert_eq!(kind, KIND_MAIL_ARRIVED);
                    serde_json::from_value::<MailArrived>(payload).unwrap()
                }
                other => panic!("not a lifecycle event: {other:?}"),
            };
        let gui = payload(Some((
            ConnectionId(4),
            mp_protocol::reminder::PRESENTER_GUI,
        )));
        assert_eq!(gui.account, "alpha");
        assert_eq!(gui.arrivals, arrivals);
        assert_eq!(gui.presenter.as_deref(), Some("gui"));
        assert_eq!(gui.presenter_connection, Some(4));
        let nobody = payload(None);
        assert_eq!(
            (nobody.presenter, nobody.presenter_connection),
            (None, None)
        );
    }

    /// A guarded pass's arrivals (PERSO-186) wake the real blocked-senders
    /// runner through the judging record, and once it has judged them the
    /// runner announces them; the completed-tick record, the scheduler's,
    /// never moves.
    #[tokio::test]
    async fn a_noted_pass_is_judged_and_announced_without_a_tick() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime(dir.path(), Vec::new());
        let judges = Judges {
            senders: true,
            hooks: false,
        };
        let (_task, announced) = runner(&runtime, judges);
        let mut cfg = AccountConfig {
            // No store under this name, so the scan finds nothing to move and
            // reports the pass judged.
            name: "zz-perso186-noted-pass".to_string(),
            ..Default::default()
        };
        cfg.imap.host = "imap.example.com".to_string();
        let canonical = Arc::new(CanonicalState::new(
            crate::daemon::state::InstanceId::new("test"),
            Vec::new(),
        ));
        let _senders = super::super::sender_runner::spawn(&runtime, canonical, &cfg)
            .expect("a ready account with a server is judged");
        settle().await;
        let ticked = *runtime.completed_ticks().borrow();

        runtime.note_pass(vec![arrival("guarded")]);
        settle().await;
        assert_eq!(
            *announced.lock().unwrap(),
            vec![vec!["guarded".to_string()]]
        );
        assert_eq!(
            *runtime.completed_ticks().borrow(),
            ticked,
            "a pass is not a tick"
        );
    }

    /// An account that runs no hooks waits for the blocked senders alone, and
    /// a later tick's mail is not announced with an earlier tick's judges.
    #[tokio::test]
    async fn only_the_judges_that_run_are_waited_for() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime(
            dir.path(),
            vec![vec![arrival("first")], vec![arrival("second")]],
        );
        let (_task, announced) = runner(
            &runtime,
            Judges {
                senders: true,
                hooks: false,
            },
        );

        runtime.tick(TickKind::Quick).await;
        let first = *runtime.completed_ticks().borrow();
        runtime.judged(Judge::Senders, first);
        settle().await;
        assert_eq!(*announced.lock().unwrap(), vec![vec!["first".to_string()]]);

        runtime.tick(TickKind::Quick).await;
        settle().await;
        assert_eq!(
            announced.lock().unwrap().len(),
            1,
            "the second tick's mail waits for its own judgement"
        );
        let second = *runtime.completed_ticks().borrow();
        runtime.judged(Judge::Senders, second);
        settle().await;
        assert_eq!(
            *announced.lock().unwrap(),
            vec![vec!["first".to_string()], vec!["second".to_string()]]
        );
    }
}
