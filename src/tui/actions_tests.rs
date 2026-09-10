//! The action-to-command contract (P5-U5, ticket #0124).
//!
//! This file is a **contract test**: it is written before the TUI can mutate
//! anything through the daemon, against the surface P5-U6 has to supply
//! (`.agents/workflow/native-gui-daemon/plan.md` section 3.7, P5-U5/P5-U6),
//! the rows `docs/parity-matrix.md` classifies for the TUI's actions
//! (`MSG-01`..`MSG-08`, `DFT-01`..`DFT-11`, `SND-01`..`SND-04`, `SYN-01`,
//! `LST-08`, `LST-09`) and the method table of `docs/daemon-protocol.md`. It
//! does not compile against today's tree, which has no `crate::tui::commands`;
//! that failure *is* the proof the contract has no stub behind it. The
//! implementer does not edit this file, they make it pass.
//!
//! # Contract
//!
//! ```text
//! mailypoppins::tui::commands                                    // the module
//!
//! enum commands::ActionRoute: Debug + PartialEq {
//!     Daemon(&'static [&'static str]),  // the methods this action issues, in issue order
//!     ClientOnly(&'static str),         // the reason: editor, browser, clipboard, picker, terminal
//!     Local,                            // pure UI state; nothing leaves the process
//! }
//! fn commands::route(action: &Action) -> ActionRoute             // exhaustive, no wildcard arm
//!
//! fn commands::dispatch(
//!     app: &mut App,
//!     commands: &dyn crate::tui::queries::Queries,
//!     action: &Action,
//! ) -> bool
//! ```
//!
//! Three names, and nothing else. Every other type used below is the TUI's own
//! or the daemon's own and exists today.
//!
//! ## Why `Queries` is the door and not a second trait
//!
//! [`Session::call`](crate::tui::session::Session::call) is the only way to the
//! daemon, and [`Queries`](crate::tui::queries::Queries) is already the
//! object-safe wrapper over it that P5-U3 contracted and P5-U4 built, for both
//! `Session` and `QueryHandle`. A `Commands` trait with the identical method
//! would be a second name for one thing and a second place to implement it. The
//! trait's own doc calls it "something that answers a daemon method call and
//! blocks for the answer", which is what a command does too.
//!
//! ## Why `dispatch` returns a `bool` and takes the door as an argument
//!
//! `true` means "this action was daemon-routed and is handled; `handle_action`
//! owes it nothing". `false` means "not mine": either a `ClientOnly`/`Local`
//! action, or one of the daemon-routed actions that still owns a background
//! thread and a `BgResult` (see below). A refusal from the daemon is *not* a
//! `false`: it lands on the status line exactly as a refused store mutation
//! does today, which is why nothing here returns a `Result` the caller would
//! have to invent a second presentation for.
//!
//! The door is an argument rather than read off `app.session`, because
//! `dispatch` needs `&mut App` and a borrow of the session inside it would
//! collide. `Session::handle()` (P5-U4) already exists for exactly this: a
//! cheap clone that owns no borrow of the `App`.
//!
//! ## What `dispatch` does *not* handle, and why
//!
//! The seven actions whose route names an **operation**-kind method
//! (`sync.quick`, `sync.full`, `send.draft`, `send.approved`, `calendar.rsvp`,
//! `message.search`, `message.list_server`) keep the arm they have in
//! `handle_action`: each already owns a `std::thread::spawn` and a
//! `BgResult` reply channel, and an operation answers `{operation_id}` at once
//! and finishes later, so its arm has to wait somewhere and post a result. That
//! wait is P5-U8's to turn into an event subscription. Their contract here is
//! therefore [`ACTION_ROUTING`] plus [`TUI_ACTION_ENGINE_RESIDUE`]: the table
//! says which method they issue, the residue scan says they no longer reach
//! `crate::sync`, `crate::send` or `imap_client` to do it. There is no third
//! way to pin them, because `handle_action` takes a
//! `Terminal<CrosstermBackend<Stdout>>` and no test can build one.
//!
//! # The oracle
//!
//! An in-process [`Dispatcher`](crate::daemon::dispatch::Dispatcher) over a
//! seeded store, the fixture pattern P5-U1 established and P5-U3 reused, with
//! every call recorded on the way through. Each row below asserts three things
//! about one action: the **method** it issued, the **parameters** it resolved
//! (the account, and the row the cursor was actually on), and the **effect**
//! the daemon's own method body had on the store. The third is what makes the
//! first two more than a spelling check: the assertion is not "the TUI said
//! `message.archive`", it is "the row moved, and the server op it owes is
//! queued", read back through `crate::store::read` and `crate::pending_ops`.
//!
//! # What P5-U6 has to decide, and this file deliberately does not
//!
//! - **Three methods do not exist yet.** `message.set_read`, `message.set_flag`
//!   and `message.move` are the daemon surfaces `docs/parity-matrix.md` names
//!   for `MSG-03`, `MSG-04` and `MSG-05`, and nothing registers them today.
//!   [`every_daemon_route_names_a_registered_method`] is the gate; the rows
//!   below fix what they must do and not how they are spelled beyond
//!   `{account, row_id, …}`.
//! - **A TUI mutation may not wait for the server.** `message.archive` and
//!   `message.delete` (P4-U8) resolve the backend *before* the store is
//!   touched and then drain the owed op synchronously, which is what gives
//!   `mp archive` its blocking UX. The TUI has never done either: it commits
//!   the row change and the owed op in one transaction and lets the next sync
//!   tick drain it (`src/tui/mutations.rs`, #0039), which is why `u` on a
//!   thousand-message selection costs no network at all and why the mark-read
//!   of an explicit open cannot stall a frame. Every row below asserts the
//!   mutation **succeeded over an account with no credentials**, which is
//!   reachable only if the call queued rather than settled. How that is
//!   spelled is P5-U6's: a `settle: false` parameter defaulting to `true` so
//!   `mp archive` does not move, a separate durability, or a client-scoped
//!   variant. No row asserts the parameter's name.
//! - **Addressing.** Every row asserts `row_id`, the address P5-U4 added to
//!   `message.get` for exactly this reason: the TUI holds a
//!   [`MessageRef`](crate::tui::app::MessageRef) and nothing else (#0050), and
//!   `"<mailbox>/<uid>"` would make it carry a second identity for every row.
//!   `address` (`src/daemon/methods/message.rs`) already accepts it, so the two
//!   existing mutations need no change on this axis.
//! - **A batch is one call per message.** `MSG-06` says "batch forms", and this
//!   file pins the plain form instead: today a reference to a row that is gone
//!   is skipped with a log line while the rest of the selection proceeds
//!   (`mutations::message_id_of`), which one call per row gives for free and a
//!   plural address would have to re-invent as a partial-failure shape. A
//!   plural address is a follow-up worth taking the day a selection's round
//!   trips show up in a measurement.
//! - **`src/tui/mutations.rs` has no caller left** once the five message
//!   mutations are routed, and its four `queue_*` functions are what the three
//!   new methods need. Moving it into the daemon rather than deleting it keeps
//!   its eight unit tests, which are the only assertions anywhere that a
//!   flag change queues exactly one `ServerOp` of the right shape.
//!
//! # Determinism
//!
//! Every test owns a per-thread data root
//! ([`crate::config::test_env::TestDataDir`], #0077) held alive for as long as
//! the fixture, so the store, the drafts directory and the daemon's runtime
//! directory all resolve under it and no test can reach the developer's tree.
//! Rows are ingested through the real [`crate::ingest::ingest_message`]. Dates
//! are frozen literals. Nothing renders, so no theme is pinned and no snapshot
//! is minted.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use serde_json::{json, Value};

use mp_protocol::{Request, RequestId, JSONRPC_VERSION};

use crate::config::{AccountConfig, GlobalConfig};
use crate::daemon::config::{ConfigState, ConfigStore};
use crate::daemon::dispatch::{ClientCtx, ClientKind};
use crate::daemon::runtime::InstanceMeta;
use crate::daemon::server::DaemonState;
use crate::parse::FetchedEmail;
use crate::selector::DRAFTS_MAILBOX;
use crate::tui::app::{
    build_mailboxes, Action, App, ComposeMode, MailboxKind, MessageRef, RsvpChoice, SearchTarget,
};
use crate::tui::commands::{dispatch, route, ActionRoute};
use crate::tui::queries::{list_emails, Queries};

/// The one account every fixture configures, and the one every call names.
const ACCOUNT: &str = "alice";

// ---------------------------------------------------------------------------
// (a) the classification table
// ---------------------------------------------------------------------------

/// Every [`Action`] variant, what it costs the daemon, and whether handling it
/// hands the terminal to a child process.
///
/// `(variant, route, suspends_terminal)`, in the declaration order of the enum
/// in `src/tui/app/types.rs` so the two read side by side. It is the plan's
/// P5-U5 sentence made executable: *"every `Action` variant maps to a daemon
/// method or is documented as client-only (editor, browser, clipboard, file
/// picker, terminal suspend)"*.
///
/// Three routes and no fourth:
///
/// - [`ActionRoute::Daemon`] names the methods the action's execution issues,
///   in issue order. A variant that issues one call per selected message names
///   that method once.
/// - [`ActionRoute::ClientOnly`] is an action whose whole effect is in this
///   process: an editor session, a browser launch, a clipboard write, a file
///   the user picked, a path already materialised. The reason is the column
///   the plan asks to be *documented*, so an empty one fails
///   [`every_client_only_route_gives_a_reason`].
/// - [`ActionRoute::Local`] is pure UI state: an overlay opens, a cursor moves,
///   a wizard closes. Nothing leaves the process and nothing needs a reason.
///
/// The third column is `Action::suspends_terminal`, pinned here for all fifty
/// variants at once. `src/tui/app/types.rs` has two tests over it already
/// (`suspending_actions_are_all_flagged`,
/// `ordinary_actions_do_not_suspend_the_terminal`); each lists a subset, and
/// neither fails when a *new* variant is classified wrongly. This column does,
/// because [`every_action_variant_is_in_the_routing_table`] is exhaustive by
/// construction. Neither of those two tests is touched.
const ACTION_ROUTING: &[(&str, ActionRoute, bool)] = &[
    (
        "EditCurrent",
        // The read-only Markdown rendition of a received row (#0075) and the
        // writable file of a drafts row (#0052) are both opened in `$EDITOR`
        // by this process. The mark-read it performs on the way in is
        // `MarkAsRead`'s method, queued and routed as its own action.
        ActionRoute::ClientOnly("$EDITOR, over a rendition or a draft file this process opens"),
        true,
    ),
    ("Reply", ActionRoute::Daemon(&["draft.reply"]), true),
    ("Send", ActionRoute::Daemon(&["send.draft"]), false),
    (
        "SendApproved",
        ActionRoute::Daemon(&["send.approved"]),
        false,
    ),
    ("NewDraft", ActionRoute::Daemon(&["draft.create"]), true),
    ("Approve", ActionRoute::Daemon(&["draft.approve"]), false),
    (
        "BatchApprove",
        ActionRoute::Daemon(&["draft.approve"]),
        false,
    ),
    ("MarkDraft", ActionRoute::Daemon(&["draft.demote"]), false),
    (
        "BatchMarkDraft",
        ActionRoute::Daemon(&["draft.demote"]),
        false,
    ),
    ("Archive", ActionRoute::Daemon(&["message.archive"]), false),
    (
        "Delete",
        // One key over two kinds of row: a received row is a store mutation,
        // a drafts row is a local file removal (#0073), and a parse-skipped
        // draft (#0080) is the same removal by path. `draft.discard` addresses
        // all three of the draft cases.
        ActionRoute::Daemon(&["message.delete", "draft.discard"]),
        false,
    ),
    (
        "BatchArchive",
        ActionRoute::Daemon(&["message.archive"]),
        false,
    ),
    (
        "BatchDelete",
        ActionRoute::Daemon(&["message.delete"]),
        false,
    ),
    (
        "BatchDeleteDrafts",
        ActionRoute::Daemon(&["draft.discard"]),
        false,
    ),
    (
        "MoveToMailbox",
        ActionRoute::Daemon(&["message.move"]),
        false,
    ),
    (
        "ToggleRead",
        ActionRoute::Daemon(&["message.set_read"]),
        false,
    ),
    (
        "MarkAsRead",
        ActionRoute::Daemon(&["message.set_read"]),
        false,
    ),
    (
        "BatchToggleRead",
        ActionRoute::Daemon(&["message.set_read"]),
        false,
    ),
    (
        "ToggleFlag",
        ActionRoute::Daemon(&["message.set_flag"]),
        false,
    ),
    (
        "BatchToggleFlag",
        ActionRoute::Daemon(&["message.set_flag"]),
        false,
    ),
    (
        "CopyMessageRef",
        ActionRoute::ClientOnly("the system clipboard"),
        false,
    ),
    (
        "OpenLogFile",
        ActionRoute::ClientOnly("$EDITOR, over this process's own log file"),
        true,
    ),
    (
        "OpenConfigFile",
        ActionRoute::ClientOnly("$EDITOR, over the configuration file the client resolves"),
        true,
    ),
    (
        "OpenAttachment",
        // The materialisation is `message.materialise_attachment` and happens
        // when the key resolves the part list; the action carries the path it
        // produced, so what is left is the desktop's file opener.
        ActionRoute::ClientOnly("the system file opener, over an already-materialised path"),
        false,
    ),
    (
        "SaveAttachments",
        ActionRoute::ClientOnly("a copy into a directory only this process can name (ANO-15)"),
        false,
    ),
    ("Fetch", ActionRoute::Daemon(&["sync.quick"]), false),
    (
        "LoadMailbox",
        // Routed by P5-U4 already; here so the table is the whole enum.
        ActionRoute::Daemon(&["message.list", "draft.list"]),
        false,
    ),
    ("FetchAccount", ActionRoute::Daemon(&["sync.quick"]), false),
    ("Sync", ActionRoute::Daemon(&["sync.full"]), false),
    (
        "ServerSearch",
        // The local pass first, then the server leg (LST-08).
        ActionRoute::Daemon(&["message.search", "message.list_server"]),
        false,
    ),
    (
        "SearchResultOpen",
        ActionRoute::ClientOnly("$EDITOR, over a rendition this process opens"),
        true,
    ),
    ("SearchResultJump", ActionRoute::Local, false),
    (
        "SearchResultYankPath",
        ActionRoute::ClientOnly("the system clipboard"),
        false,
    ),
    (
        "SearchResultFetch",
        // LST-09's `message.fetch` is not built and nothing else ingests a
        // server-only hit; the two functions that do it are the last two rows
        // of `TUI_ACTION_ENGINE_RESIDUE`.
        ActionRoute::ClientOnly(
            "no method ingests a server-only hit; see TUI_ACTION_ENGINE_RESIDUE",
        ),
        false,
    ),
    (
        "SearchResultReply",
        ActionRoute::Daemon(&["draft.reply"]),
        true,
    ),
    (
        "SearchResultForward",
        ActionRoute::Daemon(&["draft.forward"]),
        true,
    ),
    (
        "SearchResultArchive",
        ActionRoute::Daemon(&["message.archive"]),
        false,
    ),
    (
        "SearchResultOpenInBrowser",
        ActionRoute::Daemon(&["message.materialise_html"]),
        false,
    ),
    (
        "OpenHtmlInBrowser",
        ActionRoute::ClientOnly("the system browser, over an already-materialised path"),
        false,
    ),
    ("OpenComposeWizard", ActionRoute::Local, false),
    (
        "ComposeWizardSubmit",
        ActionRoute::Daemon(&["draft.create"]),
        true,
    ),
    ("ComposeWizardCancel", ActionRoute::Local, false),
    (
        "ComposeEditSignature",
        ActionRoute::ClientOnly("$EDITOR, over a signature file or a temporary copy of one"),
        true,
    ),
    ("Rsvp", ActionRoute::Daemon(&["calendar.rsvp"]), false),
    ("ComposeToContact", ActionRoute::Local, false),
    (
        "SendContactVcard",
        ActionRoute::Daemon(&["draft.create"]),
        true,
    ),
    (
        "CopyContactEmail",
        ActionRoute::ClientOnly("the system clipboard"),
        false,
    ),
    (
        "OpenEventSource",
        ActionRoute::ClientOnly("$EDITOR, over the invite the agenda row came from"),
        true,
    ),
    (
        "EditSignatureFile",
        ActionRoute::ClientOnly("$EDITOR, over a signature file"),
        true,
    ),
    (
        "AttachFileToDraft",
        // ATT-03's `draft.attach` is not built; the append is a write to a
        // file the user already picked.
        ActionRoute::ClientOnly(
            "a frontmatter append to a file the user picked; draft.attach is not built",
        ),
        false,
    ),
];

/// One of every [`Action`] variant, for the exhaustiveness of the table above.
///
/// The payloads are the cheapest that type-checks: nothing here is executed,
/// these values only carry a discriminant to [`variant_name`] and to
/// [`route`]. Adding a variant to the enum does not break this function, which
/// is why [`variant_name`] and not this list is where the compiler enforces
/// completeness.
fn one_of_each_action() -> Vec<Action> {
    vec![
        Action::EditCurrent,
        Action::Reply(false),
        Action::Send,
        Action::SendApproved,
        Action::NewDraft,
        Action::Approve,
        Action::BatchApprove(Vec::new()),
        Action::MarkDraft,
        Action::BatchMarkDraft(Vec::new()),
        Action::Archive,
        Action::Delete,
        Action::BatchArchive(Vec::new()),
        Action::BatchDelete(Vec::new()),
        Action::BatchDeleteDrafts(Vec::new()),
        Action::MoveToMailbox {
            msgs: Vec::new(),
            dest_idx: 0,
        },
        Action::ToggleRead,
        Action::MarkAsRead(MessageRef::new(1)),
        Action::BatchToggleRead(Vec::new()),
        Action::ToggleFlag,
        Action::BatchToggleFlag(Vec::new()),
        Action::CopyMessageRef,
        Action::OpenLogFile,
        Action::OpenConfigFile,
        Action::OpenAttachment("/tmp/a.pdf".into()),
        Action::SaveAttachments {
            sources: Vec::new(),
            dest_dir: "/tmp".into(),
        },
        Action::Fetch,
        Action::LoadMailbox {
            mailbox_idx: 0,
            generation: 0,
        },
        Action::FetchAccount(0),
        Action::Sync,
        Action::ServerSearch {
            query: crate::search::Query::default(),
            targets: Vec::<SearchTarget>::new(),
            local_mailbox: None,
        },
        Action::SearchResultOpen,
        Action::SearchResultJump,
        Action::SearchResultYankPath,
        Action::SearchResultFetch,
        Action::SearchResultReply(false),
        Action::SearchResultForward,
        Action::SearchResultArchive,
        Action::SearchResultOpenInBrowser,
        Action::OpenHtmlInBrowser("/tmp/a.html".into()),
        Action::OpenComposeWizard(ComposeMode::New),
        Action::ComposeWizardSubmit,
        Action::ComposeWizardCancel,
        Action::ComposeEditSignature,
        Action::Rsvp {
            msg: MessageRef::new(1),
            choice: RsvpChoice::Accept,
        },
        Action::ComposeToContact {
            to: "a@example.com".to_string(),
        },
        Action::SendContactVcard {
            contact: a_contact(),
        },
        Action::CopyContactEmail {
            address: "a@example.com".to_string(),
        },
        Action::OpenEventSource {
            msg: MessageRef::new(1),
        },
        Action::EditSignatureFile {
            name: "work".to_string(),
        },
        Action::AttachFileToDraft {
            path: "/tmp/a.pdf".to_string(),
        },
    ]
}

/// The name of an action's variant, as [`ACTION_ROUTING`]'s first column
/// spells it.
///
/// **This match has no wildcard arm, on purpose.** It is where a new `Action`
/// variant fails to compile, which is the compile-time half of the
/// completeness claim: the runtime half
/// ([`every_action_variant_is_in_the_routing_table`]) then fails until the
/// variant has a row in the table, a route and a `suspends_terminal` value.
/// The same device `Action::suspends_terminal` itself uses, for the same
/// reason: a new action must be classified rather than defaulting to whatever
/// the wildcard said.
fn variant_name(action: &Action) -> &'static str {
    match action {
        Action::EditCurrent => "EditCurrent",
        Action::Reply(_) => "Reply",
        Action::Send => "Send",
        Action::SendApproved => "SendApproved",
        Action::NewDraft => "NewDraft",
        Action::Approve => "Approve",
        Action::BatchApprove(_) => "BatchApprove",
        Action::MarkDraft => "MarkDraft",
        Action::BatchMarkDraft(_) => "BatchMarkDraft",
        Action::Archive => "Archive",
        Action::Delete => "Delete",
        Action::BatchArchive(_) => "BatchArchive",
        Action::BatchDelete(_) => "BatchDelete",
        Action::BatchDeleteDrafts(_) => "BatchDeleteDrafts",
        Action::MoveToMailbox { .. } => "MoveToMailbox",
        Action::ToggleRead => "ToggleRead",
        Action::MarkAsRead(_) => "MarkAsRead",
        Action::BatchToggleRead(_) => "BatchToggleRead",
        Action::ToggleFlag => "ToggleFlag",
        Action::BatchToggleFlag(_) => "BatchToggleFlag",
        Action::CopyMessageRef => "CopyMessageRef",
        Action::OpenLogFile => "OpenLogFile",
        Action::OpenConfigFile => "OpenConfigFile",
        Action::OpenAttachment(_) => "OpenAttachment",
        Action::SaveAttachments { .. } => "SaveAttachments",
        Action::Fetch => "Fetch",
        Action::LoadMailbox { .. } => "LoadMailbox",
        Action::FetchAccount(_) => "FetchAccount",
        Action::Sync => "Sync",
        Action::ServerSearch { .. } => "ServerSearch",
        Action::SearchResultOpen => "SearchResultOpen",
        Action::SearchResultJump => "SearchResultJump",
        Action::SearchResultYankPath => "SearchResultYankPath",
        Action::SearchResultFetch => "SearchResultFetch",
        Action::SearchResultReply(_) => "SearchResultReply",
        Action::SearchResultForward => "SearchResultForward",
        Action::SearchResultArchive => "SearchResultArchive",
        Action::SearchResultOpenInBrowser => "SearchResultOpenInBrowser",
        Action::OpenHtmlInBrowser(_) => "OpenHtmlInBrowser",
        Action::OpenComposeWizard(_) => "OpenComposeWizard",
        Action::ComposeWizardSubmit => "ComposeWizardSubmit",
        Action::ComposeWizardCancel => "ComposeWizardCancel",
        Action::ComposeEditSignature => "ComposeEditSignature",
        Action::Rsvp { .. } => "Rsvp",
        Action::ComposeToContact { .. } => "ComposeToContact",
        Action::SendContactVcard { .. } => "SendContactVcard",
        Action::CopyContactEmail { .. } => "CopyContactEmail",
        Action::OpenEventSource { .. } => "OpenEventSource",
        Action::EditSignatureFile { .. } => "EditSignatureFile",
        Action::AttachFileToDraft { .. } => "AttachFileToDraft",
    }
}

/// The table covers the enum, once each, in the enum's own order.
///
/// Fails in both directions: a variant with no row is unclassified, a row
/// naming no variant is a rename nobody followed. [`one_of_each_action`] is
/// checked against [`variant_name`] rather than trusted, so a value list that
/// forgot a variant fails here too.
#[test]
fn every_action_variant_is_in_the_routing_table() {
    let listed: Vec<&str> = one_of_each_action().iter().map(variant_name).collect();
    let tabled: Vec<&str> = ACTION_ROUTING.iter().map(|(name, _, _)| *name).collect();

    let unique: BTreeSet<&str> = listed.iter().copied().collect();
    assert_eq!(
        unique.len(),
        listed.len(),
        "one_of_each_action lists a variant twice"
    );
    assert_eq!(
        listed, tabled,
        "ACTION_ROUTING and the Action enum disagree; every variant needs a route and a \
         suspends_terminal value, in the enum's declaration order"
    );
}

/// [`route`] agrees with the table, variant by variant.
///
/// The table is the reviewed document; `route` is the production function the
/// rest of the TUI branches on. This is the row that makes the first one worth
/// writing.
#[test]
fn route_agrees_with_the_routing_table() {
    for action in one_of_each_action() {
        let name = variant_name(&action);
        let (_, expected, _) = ACTION_ROUTING
            .iter()
            .find(|(row, _, _)| *row == name)
            .unwrap_or_else(|| panic!("{name} has no row in ACTION_ROUTING"));
        assert_eq!(
            &route(&action),
            expected,
            "tui::commands::route disagrees with ACTION_ROUTING about {name}"
        );
    }
}

/// `Action::suspends_terminal` agrees with the table, variant by variant.
///
/// The invariant P5-U6 preserves rather than changes (#0108): the pre-draw
/// drain stops on a queued action that hands the terminal to `$EDITOR`,
/// because an event that has left the kernel tty buffer cannot be put back.
/// Routing an action through the daemon must not move it into or out of that
/// set, and a *new* action must be classified in both columns at once.
#[test]
fn suspends_terminal_agrees_with_the_routing_table() {
    for action in one_of_each_action() {
        let name = variant_name(&action);
        let (_, _, expected) = ACTION_ROUTING
            .iter()
            .find(|(row, _, _)| *row == name)
            .unwrap_or_else(|| panic!("{name} has no row in ACTION_ROUTING"));
        assert_eq!(
            action.suspends_terminal(),
            *expected,
            "Action::{name}::suspends_terminal moved; the #0108 drain stops on exactly the \
             actions that reach helpers::edit_file"
        );
    }
}

/// Every method any route names is registered on a real daemon.
///
/// The dispatcher is asked rather than the `MethodSpec` arrays being listed
/// here, so a family whose array grows is covered without this file moving,
/// and so a method declared in an array but never registered still fails.
///
/// Fails on the tree as committed for `message.set_read`, `message.set_flag`
/// and `message.move`, which is the point: they are `MSG-03`, `MSG-04` and
/// `MSG-05`'s daemon surfaces and P5-U6 is the unit that registers them.
#[test]
fn every_daemon_route_names_a_registered_method() {
    let fixture = Fixture::new();
    let registered: BTreeSet<String> = fixture
        .daemon
        .dispatcher
        .specs()
        .into_iter()
        .map(|spec| spec.name.to_string())
        .collect();

    let mut missing: Vec<String> = Vec::new();
    for (name, route, _) in ACTION_ROUTING {
        if let ActionRoute::Daemon(methods) = route {
            assert!(
                !methods.is_empty(),
                "ACTION_ROUTING routes {name} to the daemon and names no method"
            );
            for method in *methods {
                if !registered.contains(*method) {
                    missing.push(format!("{name} -> {method}"));
                }
            }
        }
    }
    assert!(
        missing.is_empty(),
        "ACTION_ROUTING names methods no daemon serves: {missing:?}\n\
         A route is a promise that a client can make the call. Register the method or \
         re-route the action."
    );
}

/// A client-only route says why, and the reason is a sentence rather than a
/// shrug.
///
/// The plan's word is *documented*: "maps to a daemon method or is documented
/// as client-only (editor, browser, clipboard, file picker, terminal suspend)".
/// A row with an empty reason is a row nobody thought about, which is the thing
/// this column exists to prevent.
#[test]
fn every_client_only_route_gives_a_reason() {
    for (name, route, _) in ACTION_ROUTING {
        if let ActionRoute::ClientOnly(reason) = route {
            assert!(
                reason.len() > 12,
                "ACTION_ROUTING's client-only entry for {name} has no usable reason: {reason:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// (b) what each daemon-routed mutation issues
// ---------------------------------------------------------------------------

/// Archiving the cursor row calls `message.archive` for *that* row, and the
/// daemon really moves it.
///
/// `MSG-01`. Three assertions in one: the method, the address (the row the
/// cursor was on, by `row_id`), and the effect - the store row is in `archive`
/// and one `ServerOp::Move` is owed to the server, which is what
/// `mutations::queue_move` produces today and what the sync tick drains.
///
/// The account has no credentials, so a call that resolved a backend and
/// settled the op would refuse and leave the row where it was. That it
/// succeeds is the assertion that a TUI mutation still queues rather than
/// waiting for the server (see the module header).
#[test]
fn archiving_the_cursor_row_calls_message_archive_for_that_row() {
    let fixture = Fixture::new();
    seed_inbox();
    let mut app = app_on_inbox(&fixture);
    app.list_index = 1;
    let msg = app.selected_email_ref().expect("a row under the cursor");
    fixture.forget();

    assert!(
        dispatch(&mut app, &fixture, &Action::Archive),
        "Action::Archive is daemon-routed, so dispatch owns it"
    );

    let params = fixture.only_call("message.archive");
    assert_eq!(params["account"], json!(ACCOUNT));
    assert_eq!(
        params["row_id"],
        json!(msg.row_id()),
        "the archive addressed a row other than the one under the cursor"
    );
    assert_eq!(mailbox_of(msg).as_deref(), Some("archive"));
    assert_eq!(queued_ops().len(), 1, "the server move was not queued");
}

/// Deleting the cursor row calls `message.delete` for that row, and the row is
/// gone from the store afterwards.
///
/// `MSG-02`, the received-mail half.
#[test]
fn deleting_the_cursor_row_calls_message_delete_for_that_row() {
    let fixture = Fixture::new();
    seed_inbox();
    let mut app = app_on_inbox(&fixture);
    let msg = app.selected_email_ref().expect("a row under the cursor");
    fixture.forget();

    assert!(dispatch(&mut app, &fixture, &Action::Delete));

    let params = fixture.only_call("message.delete");
    assert_eq!(params["account"], json!(ACCOUNT));
    assert_eq!(params["row_id"], json!(msg.row_id()));
    assert!(mailbox_of(msg).is_none(), "the row survived the delete");
    assert_eq!(queued_ops().len(), 1, "the server delete was not queued");
}

/// A batch is one call per selected message, in selection order, and every one
/// of them lands.
///
/// `MSG-06`. The plural address the parity matrix sketches is not what this
/// pins; the reason is in the module header.
#[test]
fn a_batch_archive_calls_the_method_once_per_message() {
    let fixture = Fixture::new();
    seed_inbox();
    let mut app = app_on_inbox(&fixture);
    let msgs: Vec<MessageRef> = app
        .emails
        .iter()
        .filter_map(|entry| entry.msg)
        .take(2)
        .collect();
    assert_eq!(msgs.len(), 2, "the fixture seeds more than two inbox rows");
    fixture.forget();

    assert!(dispatch(
        &mut app,
        &fixture,
        &Action::BatchArchive(msgs.clone())
    ));

    let addressed: Vec<i64> = fixture
        .calls_to("message.archive")
        .iter()
        .map(|params| params["row_id"].as_i64().expect("a row_id"))
        .collect();
    assert_eq!(
        addressed,
        msgs.iter().map(|m| m.row_id()).collect::<Vec<_>>(),
        "a batch issues one call per message, in the selection's order"
    );
    for msg in &msgs {
        assert_eq!(mailbox_of(*msg).as_deref(), Some("archive"));
    }
    assert_eq!(queued_ops().len(), 2);
}

/// The read toggle calls `message.set_read` with the new state, and the row
/// carries `\Seen` afterwards.
///
/// `MSG-03`. The state is the *new* one and it is resolved from the row, not
/// from a second read of the store: the cursor row is unread here, so the call
/// asks for `read: true`.
#[test]
fn the_read_toggle_calls_message_set_read_with_the_new_state() {
    let fixture = Fixture::new();
    seed_inbox();
    let mut app = app_on_inbox(&fixture);
    let msg = app
        .emails
        .iter()
        .find(|entry| !entry.read)
        .and_then(|entry| entry.msg)
        .expect("an unread row");
    app.list_index = app
        .visible
        .iter()
        .position(|&i| app.emails[i].msg == Some(msg))
        .expect("the unread row is visible");
    fixture.forget();

    assert!(dispatch(&mut app, &fixture, &Action::ToggleRead));

    let params = fixture.only_call("message.set_read");
    assert_eq!(params["account"], json!(ACCOUNT));
    assert_eq!(params["row_id"], json!(msg.row_id()));
    assert_eq!(params["read"], json!(true));
    assert!(row(msg).expect("the row is still there").is_read());
    assert_eq!(queued_ops().len(), 1, "the server flag was not queued");
}

/// The flag toggle calls `message.set_flag`, and flagging leaves the read bit
/// alone.
///
/// `MSG-04` (#0007). The second half is the one a shared "set flags" method
/// would get wrong.
#[test]
fn the_star_toggle_calls_message_set_flag_and_leaves_read_alone() {
    let fixture = Fixture::new();
    seed_inbox();
    let mut app = app_on_inbox(&fixture);
    let msg = app
        .emails
        .iter()
        .find(|entry| !entry.read && !entry.flagged)
        .and_then(|entry| entry.msg)
        .expect("an unread, unflagged row");
    app.list_index = app
        .visible
        .iter()
        .position(|&i| app.emails[i].msg == Some(msg))
        .expect("the row is visible");
    fixture.forget();

    assert!(dispatch(&mut app, &fixture, &Action::ToggleFlag));

    let params = fixture.only_call("message.set_flag");
    assert_eq!(params["account"], json!(ACCOUNT));
    assert_eq!(params["row_id"], json!(msg.row_id()));
    assert_eq!(params["flagged"], json!(true));
    let row = row(msg).expect("the row is still there");
    assert!(row.is_flagged());
    assert!(!row.is_read(), "flagging must not touch the read bit");
}

/// A quick move calls `message.move` and the row lands in the mailbox the
/// picker chose.
///
/// `MSG-05` (#0018). The destination is asserted through the store rather than
/// through a parameter name, because which key carries it is P5-U6's to spell:
/// `mailbox` is already the *narrowing* parameter of a selector address, so a
/// destination cannot reuse it.
#[test]
fn a_quick_move_calls_message_move_and_lands_the_row() {
    let fixture = Fixture::new();
    seed_inbox();
    let mut app = app_on_inbox(&fixture);
    let msg = app.selected_email_ref().expect("a row under the cursor");
    let dest_idx = app
        .find_mailbox_by_kind(MailboxKind::Sent)
        .expect("the Sent mailbox is one of the four roles");
    fixture.forget();

    assert!(dispatch(
        &mut app,
        &fixture,
        &Action::MoveToMailbox {
            msgs: vec![msg],
            dest_idx,
        }
    ));

    let params = fixture.only_call("message.move");
    assert_eq!(params["account"], json!(ACCOUNT));
    assert_eq!(params["row_id"], json!(msg.row_id()));
    assert_eq!(mailbox_of(msg).as_deref(), Some("sent"));
    assert_eq!(queued_ops().len(), 1, "the server move was not queued");
}

/// The mark-read of an explicit open carries the row the open resolved, not
/// the row the cursor ended on.
///
/// #0110, and the half of it that only a daemon-backed path can get wrong. The
/// queueing half is already pinned in `src/tui/app/keys.rs`
/// (`tab_into_the_body_pane_queues_one_mark_read` and its neighbours) and is
/// not touched here; what is new is that the *action* carries a
/// [`MessageRef`] and the command must address that one. The cursor is moved
/// to another row between the resolution and the dispatch, which is exactly
/// what the #0108 coalescing does when a `Tab` and a `J` land in one batch.
#[test]
fn mark_as_read_addresses_the_row_the_open_resolved() {
    let fixture = Fixture::new();
    seed_inbox();
    let mut app = app_on_inbox(&fixture);
    let opened = app
        .emails
        .iter()
        .find(|entry| !entry.read)
        .and_then(|entry| entry.msg)
        .expect("an unread row");
    // The cursor moves on before the queue is drained. The fixture's unread
    // row is its oldest, so the newest row is the one a `J` would leave the
    // cursor on.
    app.list_index = 0;
    let elsewhere = app.selected_email_ref().expect("another row");
    assert_ne!(opened, elsewhere, "the fixture needs two distinct rows");
    fixture.forget();

    assert!(dispatch(&mut app, &fixture, &Action::MarkAsRead(opened)));

    let params = fixture.only_call("message.set_read");
    assert_eq!(
        params["row_id"],
        json!(opened.row_id()),
        "the mark followed the cursor instead of the MessageRef the open resolved (#0110)"
    );
    assert_eq!(params["read"], json!(true));
    assert!(row(opened).expect("the row is still there").is_read());
}

/// Moving the cursor issues no command at all.
///
/// The other half of #0110, which reversed #0087: showing a row in the preview
/// is not reading it. A daemon-backed TUI makes the failure mode louder than
/// it was - a mark per cursor move is a write per keystroke - so it is pinned
/// as a call count rather than only as an absent `Action`.
#[test]
fn a_cursor_move_issues_no_command() {
    let fixture = Fixture::new();
    seed_inbox();
    let mut app = app_on_inbox(&fixture);
    fixture.forget();

    for _ in 0..3 {
        app.handle_key(crossterm::event::KeyEvent::from(
            crossterm::event::KeyCode::Char('j'),
        ));
    }
    let queued: Vec<&Action> = app.pending_actions.iter().collect();
    for action in queued {
        assert!(
            !matches!(action, Action::MarkAsRead(_)),
            "a cursor move queued a mark-read (#0110)"
        );
    }
    assert!(
        fixture.calls().is_empty(),
        "moving the cursor called the daemon: {:?}",
        fixture.calls()
    );
}

/// Approving the cursor draft calls `draft.approve` with the indexed id, and
/// the file says `approved` afterwards.
///
/// `DFT-04`. Draft ids rather than [`MessageRef`]s because a draft has no
/// `messages` row to name it by, which is the same reason `Action::BatchApprove`
/// carries ids (#0052).
#[test]
fn approving_the_cursor_draft_calls_draft_approve_with_its_id() {
    let fixture = Fixture::new();
    let mut app = app_on_drafts(&fixture);
    let id = app
        .selected_email()
        .and_then(|entry| entry.draft_id.clone())
        .expect("the cursor is on an indexed draft");
    fixture.forget();

    assert!(dispatch(&mut app, &fixture, &Action::Approve));

    let params = fixture.only_call("draft.approve");
    assert_eq!(params["account"], json!(ACCOUNT));
    assert_eq!(params["id"], json!(id));
    assert!(
        draft_file_says("status: approved"),
        "the draft file was not approved"
    );
}

/// Deleting a drafts row calls `draft.discard`, not `message.delete`.
///
/// `MSG-02`'s other half (#0073): one key over two kinds of row, and the row
/// under the cursor decides which method it is. The received branch is
/// [`deleting_the_cursor_row_calls_message_delete_for_that_row`].
#[test]
fn deleting_a_drafts_row_calls_draft_discard() {
    let fixture = Fixture::new();
    let mut app = app_on_drafts(&fixture);
    let id = app
        .selected_email()
        .and_then(|entry| entry.draft_id.clone())
        .expect("the cursor is on an indexed draft");
    fixture.forget();

    assert!(dispatch(&mut app, &fixture, &Action::Delete));

    let params = fixture.only_call("draft.discard");
    assert_eq!(params["account"], json!(ACCOUNT));
    assert_eq!(params["id"], json!(id));
    assert!(
        fixture.calls_to("message.delete").is_empty(),
        "a drafts row went to the message mutation"
    );
    assert!(
        !crate::config::drafts_dir(ACCOUNT).join("one.md").exists(),
        "the draft file survived the discard"
    );
}

/// An action `dispatch` does not own is handed back rather than swallowed.
///
/// The seven operation-kind actions keep the arm they have in `handle_action`,
/// which owns the thread and the `BgResult` their answer arrives on (see the
/// module header). A `dispatch` that returned `true` for one of them would
/// drop a sync on the floor.
#[test]
fn an_operation_and_a_client_only_action_are_not_dispatchs_business() {
    let fixture = Fixture::new();
    seed_inbox();
    let mut app = app_on_inbox(&fixture);
    fixture.forget();

    for action in [
        Action::Fetch,
        Action::Sync,
        Action::Send,
        Action::CopyMessageRef,
        Action::OpenComposeWizard(ComposeMode::New),
    ] {
        assert!(
            !dispatch(&mut app, &fixture, &action),
            "dispatch claimed {action:?}, which handle_action still owns"
        );
    }
    assert!(
        fixture.calls().is_empty(),
        "dispatch called the daemon for an action it does not own: {:?}",
        fixture.calls()
    );
}

// ---------------------------------------------------------------------------
// (c) the invariants P5-U6 preserves
// ---------------------------------------------------------------------------

/// The two bounds on the pre-draw drain are unchanged (#0108).
///
/// Read from `src/tui/mod.rs`'s own constants, so this fails on the value and
/// not on a copy of it. A child module sees its parent's private items, which
/// is why this file lives beside the loop rather than under `app/`.
#[test]
fn the_drain_bounds_are_unchanged() {
    assert_eq!(
        super::MAX_COALESCED_EVENTS,
        64,
        "the batch cap sizes one paint's worth of a held key"
    );
    assert_eq!(
        super::COALESCE_BUDGET,
        std::time::Duration::from_millis(50),
        "the wall-clock ceiling is about three frames at 60 Hz"
    );
}

/// The drain's stop condition still has its four clauses, in order.
///
/// A source scan, because `run_loop` owns a
/// `Terminal<CrosstermBackend<Stdout>>` and no test can build one: what can be
/// asserted is that the loop still reads the way #0108 wrote it, and that an
/// edit to any of the four clauses fails a test rather than passing quietly.
///
/// The four, in the order they must be evaluated:
///
/// 1. `!app.running` - a quit ends the drain, so a queued `e` after `q` never
///    reaches `handle_action` and never dispatches an editor on the way out.
/// 2. the batch cap, so a paste cannot starve the paint.
/// 3. the wall-clock budget, so 64 slow events cannot either.
/// 4. a queued action that suspends the terminal, so the keystrokes still in
///    the tty buffer reach `$EDITOR` rather than being swallowed here.
///
/// And the poll that follows them: the drain is incremental, which is
/// load-bearing, because whether a key yields a suspending action is a property
/// of `pending_actions` *after* `app.update`.
#[test]
fn the_pre_draw_drain_still_stops_on_its_four_clauses() {
    let source = read_source("src/tui/mod.rs");
    let (start, _) = source
        .match_indices("let drain_started")
        .next()
        .expect("the drain still starts by stamping a clock");
    let end = start
        + source[start..]
            .find("dirty = true;")
            .expect("the drain still ends by marking the frame dirty");
    let drain = &source[start..end];

    let clauses = [
        "!app.running",
        "batched >= MAX_COALESCED_EVENTS",
        "drain_started.elapsed() >= COALESCE_BUDGET",
        ".any(app::Action::suspends_terminal)",
    ];
    let mut at = 0usize;
    for clause in clauses {
        let found = drain[at..].find(clause).unwrap_or_else(|| {
            panic!(
                "the pre-draw drain no longer stops on {clause:?}; the four clauses of #0108 are \
                 {clauses:?}, in that order"
            )
        });
        at += found + clause.len();
    }
    assert!(
        drain[at..].contains("event::poll_pending_event()"),
        "the drain stopped being incremental: the next event must be polled *after* the stop \
         condition, because an event out of the kernel tty buffer cannot be put back"
    );
}

/// Quitting stops the loop, which is the first of those four clauses.
///
/// The behavioural half of the row above: a `q` that left `running` true would
/// make clause 1 dead and let a queued editor action through. Pinned here
/// rather than in `src/tui/app/types.rs`, whose
/// `quit_refuses_while_a_send_is_holding` covers the *refusal* and not this.
#[test]
fn a_quit_clears_running_so_the_drain_stops() {
    let mut app = App::default_for_tests();
    assert!(app.running);
    app.update(crate::tui::app::Message::Quit);
    assert!(
        !app.running,
        "the drain's first clause reads app.running, so quit must clear it"
    );
    assert!(
        Action::EditCurrent.suspends_terminal(),
        "the action a queued `e` produces is the one clause 1 exists to keep out"
    );
}

// ---------------------------------------------------------------------------
// (d) the gate over the direct path
// ---------------------------------------------------------------------------

/// The engine calls P5-U6 may keep in `src/tui/{actions,mutations}.rs`, and
/// why.
///
/// `(file, function, needle, reason)`, sorted. Read it as the answer to "why is
/// this gate not at zero": six functions read or write something no registered
/// method answers, and one is the undo-send hold the plan holds back for Phase
/// 6 (`SND-04`, `.agents/workflow/native-gui-daemon/plan.md` P6-U1/U2).
///
/// The table is a record as much as a gate, and
/// [`the_actions_that_could_be_routed_were`] fails in both directions: a call
/// site still there belongs behind a method, a site gone belongs struck from
/// the table in the same commit. It follows `tests/architecture_boundaries.rs`'s
/// `CLI_ENGINE_RESIDUE` and `queries_tests::TUI_APP_STORE_RESIDUE` in shape and
/// in intent, with a needle column those two do not have: `handle_action` is a
/// thousand lines and a whole-function exemption there would exempt forty
/// arms, so what is permitted is one *symbol* in one function.
///
/// It lives in this module rather than in `tests/architecture_boundaries.rs`
/// for the reason P5-U3 gave: it only becomes true when P5-U6 lands, and the
/// plan requires `cargo test --workspace` to be green on a T unit's commit.
/// This module is the target that does not compile, so a failing gate inside it
/// costs the rest of the tree nothing.
const TUI_ACTION_ENGINE_RESIDUE: &[(&str, &str, &str, &str)] = &[
    (
        "src/tui/actions.rs",
        "fetch_search_hit",
        "imap_client::",
        "the raw fetch by Message-ID behind a server-only hit: message.list_server lists \
         envelopes and no method fetches one message's bytes (LST-09)",
    ),
    (
        "src/tui/actions.rs",
        "handle_action",
        "store_for_mutation(",
        "the OpenEventSource arm's invite.ics blob, the same read app/mod.rs keeps as \
         load_message_ics: no message.* method hands out an attachment blob inline",
    ),
    (
        "src/tui/actions.rs",
        "handle_search_result_action",
        "store_for_mutation(",
        "the read-only Markdown rendition of a search hit, which is readonly_view_for_row's \
         read under another caller",
    ),
    (
        "src/tui/actions.rs",
        "ingest_search_hit",
        "open_store(",
        "ingesting a server-only hit into the local store: LST-09's message.fetch is not built \
         and mail enters the store through a sync (#0037)",
    ),
    (
        "src/tui/actions.rs",
        "readonly_view_for_row",
        "store_for_mutation(",
        "the read-only Markdown rendition (#0075, RD-06): nothing registered renders a stored \
         message as Markdown, and the parity matrix's message.materialize is not built",
    ),
    (
        "src/tui/actions.rs",
        "selected_selector",
        "open_store(",
        "the mp:// selector of the cursor row (RD-07): no listing carries one, and message.get \
         would be a whole-message read to answer a clipboard copy",
    ),
    (
        "src/tui/actions.rs",
        "send_one_draft",
        "send_draft(",
        "the undo-send hold's fire path (#0090, SND-04): the plan holds the hold in the TUI \
         until P6-U1/U2 moves it and the send to send.draft together",
    ),
    (
        "src/tui/actions.rs",
        "store_for_mutation",
        "open_store(",
        "the helper the three renditions above share; it dies with the last of them",
    ),
];

/// The two files this unit is accountable for.
///
/// `src/tui/mutations.rs` has no caller left once the five message mutations
/// are routed, so the expected end state is a file that is gone (moved into the
/// daemon, which is where its four `queue_*` functions are needed). The scan
/// treats a missing file as a file with no residue rather than failing, so
/// deleting it is not a reason to edit this table.
const ACTION_SOURCES: [&str; 2] = ["src/tui/actions.rs", "src/tui/mutations.rs"];

/// The symbols that mean "this function is on the direct path".
///
/// Each is a call, not an import: `tests/architecture_boundaries.rs` already
/// gates the TUI's *imports* file by file against
/// `tests/fixtures/tui-engine-imports.txt`, and what that cannot say is which
/// function still opens a store. A match on the line that declares a function
/// is ignored, so `fn store_for_mutation(` does not report itself.
const ENGINE_NEEDLES: [&str; 11] = [
    "ImapConfig::load",
    "SmtpConfig::load",
    "Store::open(",
    "imap_client::",
    "lib_do_sync",
    "mutations::queue_",
    "open_store(",
    "pending_ops::",
    "send_draft(",
    "send_rsvp(",
    "store_for_mutation(",
];

/// P5-U5's gate: every action that a registered method can carry goes through
/// one, and what is left is the eight entries of
/// [`TUI_ACTION_ENGINE_RESIDUE`].
///
/// Fails on the tree as committed (thirty-three call sites against eight),
/// which is the point: it is P5-U6's gate, and P5-U6 passes it by routing the
/// mutations, the drafts, the sync and the search legs through
/// [`crate::tui::commands`] and `crate::tui::queries`.
#[test]
fn the_actions_that_could_be_routed_were() {
    let actual = engine_calls();
    let expected: BTreeSet<(String, String, String)> = TUI_ACTION_ENGINE_RESIDUE
        .iter()
        .map(|(file, function, needle, _)| {
            (file.to_string(), function.to_string(), needle.to_string())
        })
        .collect();

    let added: Vec<_> = actual.difference(&expected).collect();
    let removed: Vec<_> = expected.difference(&actual).collect();
    if added.is_empty() && removed.is_empty() {
        return;
    }

    let mut report = String::new();
    for (file, function, needle) in &added {
        report.push_str(&format!(
            "  still on the direct path: {file}::{function} -> {needle}\n"
        ));
    }
    for (file, function, needle) in &removed {
        report.push_str(&format!(
            "  no longer on the direct path: {file}::{function} -> {needle}\n"
        ));
    }
    panic!(
        "the TUI's action layer's engine calls moved ({} in the tree, {} in \
         TUI_ACTION_ENGINE_RESIDUE):\n{report}\n\
         Phase 5's gate is that an action reaches the store, the mail server or the send path \
         through a daemon method rather than through the library. A site that is still there \
         belongs behind a method; a site that went away belongs struck from \
         TUI_ACTION_ENGINE_RESIDUE in the same commit.",
        actual.len(),
        expected.len(),
    );
}

/// Every residue entry names a function that exists, in a scanned file, with a
/// needle the scanner looks for and a usable reason, and the table is sorted
/// and deduplicated.
#[test]
fn every_residue_entry_names_a_real_function_and_a_reason() {
    let mut previous: Option<(&str, &str, &str)> = None;
    for (file, function, needle, reason) in TUI_ACTION_ENGINE_RESIDUE {
        assert!(
            ACTION_SOURCES.contains(file),
            "TUI_ACTION_ENGINE_RESIDUE names {file}, which this unit does not scan"
        );
        assert!(
            ENGINE_NEEDLES.contains(needle),
            "TUI_ACTION_ENGINE_RESIDUE names the needle {needle:?}, which the scanner does not \
             look for"
        );
        let source = read_source(file);
        assert!(
            source.contains(&format!("fn {function}(")),
            "TUI_ACTION_ENGINE_RESIDUE names {file}::{function}, which is not a function of that \
             file"
        );
        assert!(
            reason.len() > 20,
            "TUI_ACTION_ENGINE_RESIDUE's entry for {file}::{function} -> {needle} has no usable \
             reason: {reason:?}"
        );
        if let Some(previous) = previous {
            assert!(
                previous < (file, function, needle),
                "TUI_ACTION_ENGINE_RESIDUE is not sorted: {previous:?} precedes {:?}",
                (file, function, needle)
            );
        }
        previous = Some((file, function, needle));
    }
}

/// The scanner finds a call in production code, attributes it to its enclosing
/// function, and ignores one inside a `#[cfg(test)]` module, one in a line
/// comment, and the line that declares the function itself.
///
/// A test for the test, because a source scan that silently found nothing
/// would pass the gate above for the wrong reason.
#[test]
fn the_scanner_attributes_a_call_and_ignores_tests_comments_and_declarations() {
    let source = "\
fn real() {\n    let s = open_store(\"a\");\n}\n\
/// See open_store(\"x\") for the reason.\n\
pub(crate) fn documented() {\n    let _ = 1;\n}\n\
fn store_for_mutation(app: &mut App) {\n    let _ = 1;\n}\n\
#[cfg(test)]\nmod tests {\n    fn t() { open_store(\"a\"); }\n}\n";
    assert_eq!(
        engine_calls_in(source),
        vec![("real".to_string(), "open_store(".to_string())],
        "the scanner found something other than the one production call"
    );
}

/// Every `(file, function, needle)` on the direct path, over the production
/// code of [`ACTION_SOURCES`].
fn engine_calls() -> BTreeSet<(String, String, String)> {
    let mut found = BTreeSet::new();
    for file in ACTION_SOURCES {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(file);
        // A file P5-U6 deleted has no residue, which is a pass and not a
        // failure: `src/tui/mutations.rs` is expected to go.
        let Ok(source) = std::fs::read_to_string(&path) else {
            continue;
        };
        for (function, needle) in engine_calls_in(&source) {
            found.insert((file.to_string(), function, needle));
        }
    }
    found
}

/// The `(function, needle)` pairs of `source`, with `#[cfg(test)]` modules and
/// line comments removed first and function declarations skipped.
///
/// A unit test may open a store: it is the module's own test, and it is not a
/// path a keypress takes. A doc comment may name `open_store` too, and several
/// of them do.
fn engine_calls_in(source: &str) -> Vec<(String, String)> {
    let source = strip_test_modules(&strip_line_comments(source));
    let mut current = String::new();
    let mut found = Vec::new();
    for line in source.lines() {
        if let Some(name) = function_name(line) {
            current = name;
            continue;
        }
        for needle in ENGINE_NEEDLES {
            let pair = (current.clone(), needle.to_string());
            if line.contains(needle) && !found.contains(&pair) {
                found.push(pair);
            }
        }
    }
    found
}

/// The name a line declares, for a line that declares a function.
fn function_name(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let rest = ["pub(crate) ", "pub(super) ", "pub(self) ", "pub "]
        .iter()
        .find_map(|prefix| trimmed.strip_prefix(prefix))
        .unwrap_or(trimmed);
    let rest = rest.strip_prefix("async ").unwrap_or(rest);
    let rest = rest.strip_prefix("fn ")?;
    let name: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    (!name.is_empty()).then_some(name)
}

/// Drop everything from `//` to the end of the line, string literals included:
/// a scan over source text is not a parser, and a doc comment naming
/// `open_store` is not a call to it.
fn strip_line_comments(source: &str) -> String {
    source
        .lines()
        .map(|line| match line.find("//") {
            Some(at) => &line[..at],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Drop every `#[cfg(test)] mod … { … }` block, braces matched.
///
/// The same helper `tests/architecture_boundaries.rs` and
/// `app/queries_tests.rs` use, for the same reason and with the same
/// limitation: braces inside string literals inside a test module would
/// confuse it, and neither scanned file has one.
fn strip_test_modules(source: &str) -> String {
    let mut out = source.to_string();
    loop {
        let Some(at) = out.find("#[cfg(test)]") else {
            return out;
        };
        let after = &out[at + "#[cfg(test)]".len()..];
        let trimmed = after.trim_start();
        if !trimmed.starts_with("mod ") && !trimmed.starts_with("pub mod ") {
            out.replace_range(at..at + 1, " ");
            continue;
        }
        let Some(open) = out[at..].find('{').map(|i| at + i) else {
            return out;
        };
        let mut depth = 0usize;
        let mut end = out.len();
        for (i, ch) in out[open..].char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = open + i + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        out.replace_range(at..end, "");
    }
}

/// One source file of the crate, read from the repo root the build ran in.
fn read_source(relative: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

// ---------------------------------------------------------------------------
// The fixture: one seeded store, one in-process daemon over it, every call
// recorded
// ---------------------------------------------------------------------------

/// A daemon assembled over a fixture data root, reachable as a [`Queries`],
/// remembering what it was asked.
///
/// The data-root override is held here rather than dropped at the end of the
/// builder because every path in sight (the store, the blob store, the drafts
/// directory, the daemon's runtime directory) resolves under it, and a dropped
/// tempdir would point the next resolution at the developer's own tree.
struct Fixture {
    daemon: DaemonState,
    runtime: tokio::runtime::Runtime,
    calls: RefCell<Vec<(String, Value)>>,
    _data: crate::config::test_env::TestDataDir,
}

impl Fixture {
    /// A daemon over a fresh data root with an empty store for [`ACCOUNT`].
    ///
    /// The store is created before the daemon is assembled because
    /// `account::state_of` probes the store *file*: an account with no file is
    /// `blocked`, and the message methods refuse a blocked account. Seeding
    /// rows afterwards is fine, the probe runs per call.
    fn new() -> Fixture {
        let data = crate::config::test_env::TestDataDir::new();
        let root = crate::config::mailypoppins_data_dir();
        std::fs::create_dir_all(crate::config::account_dir(ACCOUNT)).expect("an account dir");
        drop(crate::store::Store::open(crate::config::store_path(ACCOUNT)).expect("a store"));

        let store = Arc::new(ConfigStore::new(
            root.join("config.toml"),
            ConfigState::Ok,
            global_config(),
            // No account runtimes: every method under test reads the store for
            // itself, so nothing here depends on a runtime having come up.
            false,
        ));
        let daemon = DaemonState::new(
            InstanceMeta {
                app_version: "0.0.0-p5u5".to_string(),
                protocol_min: mp_protocol::PROTOCOL_MIN,
                protocol_max: mp_protocol::PROTOCOL_MAX,
                instance_id: "action-commands".to_string(),
                pid: 42,
                started_at: "2026-07-28T09:00:00Z".to_string(),
                data_dir: root.clone(),
                config_dir: root,
            },
            store,
        );
        Fixture {
            daemon,
            runtime: {
                // Every thread this runtime starts, the blocking pool's
                // included, is pointed at the fixture's data root. The
                // override of #0077 is thread-local, so without this a method
                // that hops to `spawn_blocking` - which the message mutations
                // do, because a `Store` is not `Sync` and the commit and the
                // owed op are one unit - resolves `store_path` against the
                // developer's own tree and refuses with `-32006`. The guard is
                // forgotten rather than held because the thread it belongs to
                // dies with this runtime, which dies with the fixture.
                let root = crate::config::mailypoppins_data_dir();
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .on_thread_start(move || {
                        std::mem::forget(crate::config::test_env::DataDirOverride::set(&root));
                    })
                    .build()
                    .expect("a current-thread runtime")
            },
            calls: RefCell::new(Vec::new()),
            _data: data,
        }
    }

    /// The TUI's own connection context: a `tui` client at protocol 1.
    fn ctx() -> ClientCtx {
        ClientCtx {
            connection_id: 1,
            kind: ClientKind::Tui,
            protocol: 1,
            capabilities: Vec::new(),
        }
    }

    /// Everything the TUI has asked for so far, in order.
    fn calls(&self) -> Vec<(String, Value)> {
        self.calls.borrow().clone()
    }

    /// Forget what has been asked so far, so a row asserts about its own
    /// action and not about the listing that seeded it.
    fn forget(&self) {
        self.calls.borrow_mut().clear();
    }

    /// The parameters of every call to `method`, in order.
    fn calls_to(&self, method: &str) -> Vec<Value> {
        self.calls
            .borrow()
            .iter()
            .filter(|(name, _)| name == method)
            .map(|(_, params)| params.clone())
            .collect()
    }

    /// The parameters of the one call to `method`, asserting it was the only
    /// call of any kind: an action that issued a second one is doing something
    /// this file has not been told about.
    fn only_call(&self, method: &str) -> Value {
        let calls = self.calls();
        assert_eq!(
            calls.len(),
            1,
            "expected one call to {method}, got {:?}",
            calls
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>()
        );
        assert_eq!(calls[0].0, method, "the action called the wrong method");
        calls[0].1.clone()
    }
}

impl Queries for Fixture {
    fn call(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        self.calls
            .borrow_mut()
            .push((method.to_string(), params.clone()));
        let request = Request {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: Some(RequestId::Num(1)),
            method: method.to_string(),
            params,
        };
        let outcome = self
            .runtime
            .block_on(self.daemon.dispatcher.dispatch(&Fixture::ctx(), request))
            .map_err(|e| anyhow::anyhow!("{method}: {e}"))?;
        Ok(outcome.result)
    }
}

/// The one-account configuration every fixture is built from.
fn global_config() -> GlobalConfig {
    GlobalConfig {
        accounts: vec![AccountConfig {
            name: ACCOUNT.to_string(),
            ..Default::default()
        }],
        ..Default::default()
    }
}

/// One fixture message, everything about it derived from `subject` so a
/// failure names the row it is about.
fn fixture_email(subject: &str, date: &str, read: bool) -> FetchedEmail {
    FetchedEmail {
        from: format!("Sender {subject} <s@example.com>"),
        to: "me@example.com".into(),
        cc: None,
        reply_to: None,
        bcc: None,
        subject: subject.into(),
        date: date.into(),
        body_text: format!("body of {subject}"),
        html_body: None,
        has_attachments: false,
        message_id: Some(format!("<{subject}@example.com>")),
        attachments: Vec::new(),
        flags: crate::types::MessageFlags::seen(read),
        calendar_ics: None,
        event: None,
    }
}

/// Write a fixture message through the real ingest API, so the rows under test
/// are the rows the sync path actually produces.
fn ingest_fixture(mailbox: &str, uid: i64, email: &FetchedEmail) {
    let store = crate::store::Store::open(crate::config::store_path(ACCOUNT)).unwrap();
    let blobs = crate::store::BlobStore::for_account(ACCOUNT);
    crate::ingest::ingest_message(
        &store,
        &blobs,
        &crate::ingest::IngestInput {
            account: ACCOUNT,
            mailbox,
            uid,
            email,
            raw: None,
        },
    )
    .unwrap();
}

/// Three inbox messages, the first unread, which is the shape every mutation
/// row below wants.
fn seed_inbox() {
    ingest_fixture(
        "inbox",
        1,
        &fixture_email("a", "Mon, 01 Jan 2024 09:00:00 +0000", false),
    );
    ingest_fixture(
        "inbox",
        2,
        &fixture_email("b", "Mon, 01 Jan 2024 10:00:00 +0000", true),
    );
    ingest_fixture(
        "inbox",
        3,
        &fixture_email("c", "Mon, 01 Jan 2024 11:00:00 +0000", true),
    );
}

/// An `App` on [`ACCOUNT`]'s inbox, with the list the daemon lists and the
/// cursor on the first row.
///
/// The rows come through [`list_emails`] rather than through the store, so the
/// `MessageRef`s a dispatch resolves are the ones a real frame is holding.
fn app_on_inbox(fixture: &Fixture) -> App {
    let mut app = App::default_for_tests();
    app.account_config = AccountConfig {
        name: ACCOUNT.to_string(),
        ..Default::default()
    };
    app.mailboxes = build_mailboxes(&app.account_config);
    app.active_mailbox = app
        .find_mailbox_by_kind(MailboxKind::Inbox)
        .expect("the Inbox mailbox is one of the four roles");
    app.emails = Arc::new(list_emails(fixture, ACCOUNT, "inbox").expect("the daemon lists inbox"));
    app.rebuild_visible();
    app.list_index = 0;
    app
}

/// An `App` on [`ACCOUNT`]'s Drafts mailbox, with one draft in it.
fn app_on_drafts(fixture: &Fixture) -> App {
    let dir = crate::config::drafts_dir(ACCOUNT);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("one.md"),
        "---\nid: one\nfrom: me@example.com\nto: you@example.com\nsubject: One\n\
         status: draft\ndate: 2024-01-01T09:00:00+00:00\n---\n\nBody.\n",
    )
    .unwrap();

    let mut app = App::default_for_tests();
    app.account_config = AccountConfig {
        name: ACCOUNT.to_string(),
        ..Default::default()
    };
    app.mailboxes = build_mailboxes(&app.account_config);
    app.active_mailbox = app
        .find_mailbox_by_kind(MailboxKind::Drafts)
        .expect("the Drafts mailbox is one of the four roles");
    app.emails = Arc::new(
        list_emails(fixture, ACCOUNT, DRAFTS_MAILBOX).expect("the daemon lists the drafts"),
    );
    app.rebuild_visible();
    app.list_index = 0;
    assert_eq!(app.emails.len(), 1, "the drafts fixture writes one draft");
    app
}

/// True when the account's one draft file contains `needle`.
fn draft_file_says(needle: &str) -> bool {
    let path = crate::config::drafts_dir(ACCOUNT).join("one.md");
    std::fs::read_to_string(path)
        .map(|text| text.contains(needle))
        .unwrap_or(false)
}

/// The store row behind a reference, or `None` once it is gone.
fn row(msg: MessageRef) -> Option<crate::store::read::MessageRow> {
    let store = crate::store::Store::open(crate::config::store_path(ACCOUNT)).unwrap();
    crate::store::read::find_by_id(&store, msg.row_id()).unwrap()
}

/// The mailbox a reference's row is in, or `None` once it is gone.
fn mailbox_of(msg: MessageRef) -> Option<String> {
    row(msg).map(|row| row.mailbox)
}

/// The server operations [`ACCOUNT`] currently owes.
fn queued_ops() -> Vec<crate::pending_ops::PendingOp> {
    let store = crate::store::Store::open(crate::config::store_path(ACCOUNT)).unwrap();
    crate::pending_ops::queued_ops(&store, ACCOUNT).unwrap()
}

/// One contact, for the one action that carries one.
fn a_contact() -> crate::contacts::Contact {
    crate::contacts::Contact {
        address: "a@example.com".to_string(),
        display_name: "A".to_string(),
        sent_to: 0,
        sent_cc: 0,
        received: 0,
        first_seen: String::new(),
        last_seen: String::new(),
        source: Default::default(),
    }
}

/// The one compile-time assertion in this file: a [`Session`] is a command
/// door, so the dispatch the rows above drive over an in-process dispatcher is
/// the same code the TUI drives over the socket.
///
/// Never called. If it were removed the suite would still pass over the
/// fixture and the TUI would issue no command at all, which is the failure this
/// line exists to make impossible.
#[allow(dead_code)]
fn a_session_dispatches_commands(
    app: &mut App,
    session: &crate::tui::session::Session,
    action: &Action,
) -> bool {
    dispatch(app, session, action)
}
