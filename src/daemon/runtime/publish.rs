//! What the runtime's background tasks publish after they touched an account:
//! a tick's outcome and the mailbox counts that moved.
//!
//! The watcher, the scheduler and the drainer each end their work the same
//! way, so they share it here rather than each keeping a copy. A tick commits
//! its `sync.completed` through [`commit_tick`] and then the counts; a drain,
//! which ran no sync, commits the counts alone.
//!
//! Counts are compared against the canonical state rather than a memory of the
//! caller's own, because every one of these tasks and a client's own fetch
//! publish counts: a private map would miss a mailbox that moved away and back
//! between two of its own runs, and would republish on its first run what the
//! state already holds.

use std::collections::HashMap;
use std::sync::Arc;

use log::warn;

use crate::catalogue::SidebarRow;
use crate::config::AccountConfig;
use crate::daemon::dispatch::ResourceId;
use crate::daemon::server::commit_tick;
use crate::daemon::state::events::Event;
use crate::daemon::state::{CanonicalState, Change};

use super::account::{AccountRuntime, TickKind, TickOutcome};

/// Run one tick of `kind` on `runtime`, commit its outcome, and publish the
/// mailbox counts it moved.
///
/// A tick that was refused (blocked or retired) ran nothing and publishes
/// nothing. A joiner publishes no `sync.completed`, because the runner commits
/// that once, but it does publish counts: whatever the runner has not
/// published yet by the time the joiner reads the store is the same set, and
/// one the runner did publish compares equal and is skipped.
///
/// A tick that failed is logged here (PERSO-170): nobody waits on a scheduled
/// or a watcher tick, so its error would otherwise reach no one but the
/// `sync.completed` it carries.
pub async fn tick_and_publish(
    runtime: &AccountRuntime,
    canonical: &Arc<CanonicalState>,
    kind: TickKind,
) -> TickOutcome {
    let outcome = commit_tick(runtime, canonical, kind).await;
    if let Some(line) = failure_line(runtime.account(), &outcome) {
        warn!("{line}");
    }
    if !outcome.blocked {
        publish_changed_counts(canonical, runtime.config()).await;
    }
    outcome
}

/// The warning a failed tick leaves in the log, naming the account and the
/// error; `None` for a tick that did not fail, was refused, or joined one
/// already running, whose runner reports the error once.
fn failure_line(account: &str, outcome: &TickOutcome) -> Option<String> {
    if outcome.blocked || outcome.joined {
        return None;
    }
    let kind = match outcome.kind {
        TickKind::Quick => "quick",
        TickKind::Full => "full",
    };
    outcome
        .error
        .as_ref()
        .map(|error| format!("[daemon] a background {kind} tick of {account} failed: {error}"))
}

/// Bring the canonical state's view of `account` up to date after a pass:
/// the sidebar hierarchy its catalogue now resolves to, then one
/// [`Change::MailboxCounts`] per mailbox whose counts differ from what the
/// state holds.
///
/// The hierarchy comes first (#0141): a pass that listed a folder the server
/// just created re-seeds the account, announces every new or changed row as
/// a counts change (which is what makes both clients re-read the sidebar,
/// even for an empty folder whose counts did not move from zero), and every
/// row that went as a `state.remove`.
///
/// A `status-only` folder's counts are the server's, read by the tick's
/// `STATUS`; every other mailbox's are the store's.
pub async fn publish_changed_counts(canonical: &Arc<CanonicalState>, account: &AccountConfig) {
    let cfg = account.clone();
    let read = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        let store = crate::store::Store::open(crate::config::store_path(&cfg.name))?;
        let stored = crate::store::read::mailbox_read_counts(&store, &cfg.name)?;
        let listed = crate::catalogue::listing::load(&store, &cfg.name)?;
        let catalogue = crate::catalogue::Catalogue::resolve(&cfg, &listed);
        let seed = crate::daemon::state::seed_with(&cfg, &catalogue);
        let rows = crate::catalogue::sidebar(&cfg, &catalogue);
        let stored: HashMap<String, (u64, u64)> = stored
            .into_iter()
            .map(|(mailbox, counts)| (mailbox, (counts.total as u64, counts.unread as u64)))
            .collect();
        Ok((seed, effective_counts(&rows, stored)))
    })
    .await;
    let name = &account.name;
    let (seed, fresh) = match read {
        Ok(Ok(read)) => read,
        Ok(Err(e)) => return warn!("[daemon] counting the mailboxes of {name}: {e:#}"),
        Err(e) => return warn!("[daemon] the count task for {name} {e}"),
    };
    let (touched, removed) = canonical.reseed_account(seed);
    for slug in removed {
        canonical.publish(Event::Remove {
            resource: ResourceId::new(format!("mailbox:{name}/{slug}")),
        });
    }
    commit_changed_counts(canonical, name, fresh, &touched);
}

/// The counts the sidebar shows per slug: each row's ([`SidebarRow::counts`])
/// but the local Drafts view's, which no store row counts, then every other
/// mailbox the store holds rows under, by its store counts.
fn effective_counts(
    rows: &[SidebarRow],
    mut stored: HashMap<String, (u64, u64)>,
) -> Vec<(String, (u64, u64))> {
    let mut fresh: Vec<(String, (u64, u64))> = rows
        .iter()
        .filter(|row| row.slug != crate::selector::DRAFTS_MAILBOX)
        .map(|row| (row.slug.clone(), row.counts(stored.remove(&row.slug))))
        .collect();
    let mut rest: Vec<(String, (u64, u64))> = stored.into_iter().collect();
    rest.sort();
    fresh.extend(rest);
    fresh
}

/// The comparison and the commits of [`publish_changed_counts`], apart from
/// the store read so a test can drive it. A slug in `touched` is committed
/// even when its counts did not move: it is a row the hierarchy just gained
/// or changed, and the commit is what tells a client to read it.
fn commit_changed_counts(
    canonical: &CanonicalState,
    account: &str,
    fresh: impl IntoIterator<Item = (String, (u64, u64))>,
    touched: &[String],
) {
    for (mailbox, pair) in fresh {
        if canonical.mailbox_counts(account, &mailbox) == Some(pair) && !touched.contains(&mailbox)
        {
            continue;
        }
        canonical.apply(Change::MailboxCounts {
            account: account.to_string(),
            mailbox,
            total: pair.0,
            unread: pair.1,
            // What the sidebar prints beside the label, which is the total
            // (`src/daemon/methods/mailbox.rs`).
            badge: pair.0,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded_state() -> CanonicalState {
        use crate::daemon::state::{AccountSeed, MailboxSeed};
        CanonicalState::new(
            crate::daemon::state::InstanceId::new("test"),
            vec![AccountSeed {
                name: "alpha".to_string(),
                mailboxes: ["inbox", "archive"]
                    .into_iter()
                    .map(|slug| MailboxSeed {
                        role: slug.to_string(),
                        slug: slug.to_string(),
                        label: slug.to_string(),
                        ..Default::default()
                    })
                    .collect(),
            }],
        )
    }

    fn outcome(error: Option<&str>) -> TickOutcome {
        TickOutcome {
            kind: TickKind::Quick,
            phases: Vec::new(),
            joined: false,
            blocked: false,
            body_deadline: None,
            status: String::new(),
            error: error.map(str::to_string),
            sync: None,
            contacts: None,
            calendar_changed: false,
        }
    }

    /// A tick that listed a new folder re-seeds the account's hierarchy and
    /// announces the new row even at 0/0, which is what makes both clients
    /// read the sidebar again; a folder the server stopped listing is
    /// reported as dropped, and the counts of a slug that stays are kept
    /// (#0141).
    #[test]
    fn a_new_folder_reseeds_the_account_and_is_announced() {
        use crate::daemon::state::{AccountSeed, MailboxSeed};
        let state = seeded_state();
        commit_changed_counts(&state, "alpha", vec![("inbox".to_string(), (4, 1))], &[]);
        let seed = |slugs: &[&str]| AccountSeed {
            name: "alpha".to_string(),
            mailboxes: slugs
                .iter()
                .map(|slug| MailboxSeed {
                    role: slug.to_string(),
                    slug: slug.to_string(),
                    label: slug.to_string(),
                    ..Default::default()
                })
                .collect(),
        };

        let (touched, removed) = state.reseed_account(seed(&["inbox", "archive", "Spesen"]));
        assert_eq!(touched, ["Spesen"]);
        assert!(removed.is_empty());
        assert_eq!(
            state.mailbox_counts("alpha", "inbox"),
            Some((4, 1)),
            "carried"
        );
        let before = state.revision().get();
        commit_changed_counts(
            &state,
            "alpha",
            vec![
                ("inbox".to_string(), (4, 1)),
                ("Spesen".to_string(), (0, 0)),
            ],
            &touched,
        );
        assert_eq!(
            state.revision().get(),
            before + 1,
            "only the new row is announced"
        );

        let (touched, removed) = state.reseed_account(seed(&["inbox", "Spesen"]));
        assert!(touched.is_empty());
        assert_eq!(removed, ["archive"]);
        assert_eq!(
            state.reseed_account(seed(&["inbox", "Spesen"])),
            (Vec::new(), Vec::new()),
            "an unchanged hierarchy is no change"
        );
    }

    /// A failed tick's warning names the account and carries the error
    /// verbatim, the line a silent scheduled tick never left (PERSO-170).
    #[test]
    fn a_failed_tick_is_logged_with_its_account_and_error() {
        let error = "secrets backend not initialized -- call secrets::init() first";
        assert_eq!(
            failure_line("tum", &outcome(Some(error))).as_deref(),
            Some(
                "[daemon] a background quick tick of tum failed: \
                 secrets backend not initialized -- call secrets::init() first"
            )
        );
        assert_eq!(failure_line("tum", &outcome(None)), None, "a clean tick");

        let mut joined = outcome(Some(error));
        joined.joined = true;
        assert_eq!(failure_line("tum", &joined), None, "the runner logs it");
        let mut blocked = outcome(Some(error));
        blocked.blocked = true;
        assert_eq!(
            failure_line("tum", &blocked),
            None,
            "a refused tick ran nothing"
        );
    }

    /// Only a mailbox whose counts differ from the state's is committed, so a
    /// drain or a tick that moved nothing publishes nothing.
    #[test]
    fn only_moved_counts_are_committed() {
        let state = seeded_state();
        let before = state.revision().get();
        let fresh = || {
            vec![
                ("inbox".to_string(), (4, 1)),
                ("archive".to_string(), (0, 0)),
            ]
        };
        commit_changed_counts(&state, "alpha", fresh(), &[]);
        assert_eq!(
            state.revision().get(),
            before + 1,
            "the archive was already 0/0"
        );
        assert_eq!(state.mailbox_counts("alpha", "inbox"), Some((4, 1)));

        commit_changed_counts(&state, "alpha", fresh(), &[]);
        assert_eq!(
            state.revision().get(),
            before + 1,
            "nothing moved the second time"
        );

        commit_changed_counts(&state, "alpha", fresh(), &["archive".to_string()]);
        assert_eq!(
            state.revision().get(),
            before + 2,
            "a row the hierarchy just gained is announced even at 0/0"
        );
    }
}
