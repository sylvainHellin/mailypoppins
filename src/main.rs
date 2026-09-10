use mailypoppins::types::*;
use mailypoppins::config::*;
use mailypoppins::parse::*;
use mailypoppins::imap_client::{self, *};
use mailypoppins::config_cmd::*;
use mailypoppins::graph;
use mailypoppins::pending_ops;
use mailypoppins::selector::{Namespace, Selector};
use mailypoppins::store::Store;

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand};
use colored::*;
use log::{error, info, warn};
use std::io::{self, Write};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "mailypoppins")]
#[command(about = "A terminal email client: Markdown drafts on disk, received mail in a local store")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Draft selector to preview (dry-run mode)
    #[arg(value_name = "SELECTOR")]
    selector: Option<String>,

    /// Signature to use (overrides config default)
    #[arg(short, long, global = true)]
    signature: Option<String>,

    /// Skip signature entirely
    #[arg(long, global = true)]
    no_signature: bool,

    /// Account to use (default: first in config)
    #[arg(short = 'A', long, global = true)]
    account: Option<String>,

    /// Answer this command from the local daemon instead of in process.
    ///
    /// Debug reach while the migration is under way (P2-U11):
    /// `mp account list` and `mp list-messages` are routed, everything else
    /// still runs in process. Hidden, because `mp --help` may not move until
    /// the cutover slices make the daemon the default. It never falls back: a
    /// routed command that cannot reach a daemon exits 4 rather than answering
    /// from this process.
    #[arg(long, global = true, hide = true)]
    daemon: bool,
}

/// The long help for `mp search`: one grammar, every backend, with examples.
const SEARCH_LONG_ABOUT: &str = "\
Search emails with one grammar that every backend speaks.

Fields (combine freely, implicit AND between them):
  from:  to:  cc:  subject:  body:  filename:   match a field
  has:attachment                                 only mail with an attachment
  before:YYYY-MM-DD  after:YYYY-MM-DD             a date range (since: aliases after:)
  in:MAILBOX                                      scope to one mailbox
  message-id:<id>                                 exact Message-ID lookup

Operators:
  \"a phrase\"        a quoted phrase is one term
  a OR b            either term
  (a OR b)          a parenthesised OR group, AND-ed with the rest

Examples:
  mp search 'from:boss@corp.com (invoice OR receipt) has:attachment'
  mp search --from boss@corp.com --has-attachment 'invoice OR receipt'
  mp search 'subject:\"quarterly report\" after:2026-01-01 before:2026-07-01'
  mp search --local 'from:ada ledger'

Backend honesty: on Gmail and Exchange every term runs server-side. On plain
IMAP has:attachment has no server key, so it is answered from the local store
(synced mail only) and the run prints a warning. filename: is Gmail/Exchange/
--local only.";

#[derive(Subcommand)]
enum Commands {
    /// Send a single approved email, or (with --invite) a calendar invitation
    Send {
        /// Draft selector: mp://<account>/drafts/<id>, drafts/<id> or <id>
        /// (omit when using --invite)
        #[arg(value_name = "SELECTOR")]
        selector: Option<String>,
        /// Skip confirmation prompt
        #[arg(short = 'y', long)]
        yes: bool,
        /// Send an iMIP calendar invitation (METHOD:REQUEST) instead of a draft.
        /// Attendees come from --to/--cc; the subject is used as the event
        /// summary. Requires --to, --start, --subject, and one of --end/--duration.
        #[arg(long)]
        invite: bool,
        /// Invite recipient(s), comma-separated (invite mode; ATTENDEE + To).
        #[arg(long)]
        to: Option<String>,
        /// Invite CC recipient(s), comma-separated (invite mode; ATTENDEE + Cc).
        #[arg(long)]
        cc: Option<String>,
        /// Event subject / summary (invite mode).
        #[arg(long)]
        subject: Option<String>,
        /// Event start. Local time (2026-07-20T14:00 or "2026-07-20 14:00") or
        /// RFC3339 with offset (2026-07-20T14:00:00+02:00, ...Z). Invite mode.
        #[arg(long)]
        start: Option<String>,
        /// Event end (same formats as --start). Provide this or --duration.
        #[arg(long)]
        end: Option<String>,
        /// Event duration instead of --end: ISO8601 (PT1H30M) or short (1h30m).
        #[arg(long)]
        duration: Option<String>,
        /// Optional event location (invite mode).
        #[arg(long)]
        location: Option<String>,
        /// Optional event description / body (invite mode).
        #[arg(long)]
        description: Option<String>,
    },
    /// Send every approved draft of the account
    SendApproved {
        /// Send the approved drafts of every configured account
        #[arg(long)]
        all_accounts: bool,
        /// Skip confirmation prompt
        #[arg(short = 'y', long)]
        yes: bool,
    },
    /// List the account's drafts from the drafts index
    List {
        /// Only list drafts with this status
        #[arg(long, value_name = "STATUS")]
        status: Option<DraftStatusFilter>,
    },
    /// Validate a draft's frontmatter (default: every draft of the account)
    Validate {
        /// Draft selector: mp://<account>/drafts/<id>, drafts/<id> or <id>
        #[arg(value_name = "SELECTOR")]
        selector: Option<String>,
    },
    /// Mark a draft as approved
    MarkApproved {
        /// Draft selector: mp://<account>/drafts/<id>, drafts/<id> or <id>
        #[arg(value_name = "SELECTOR")]
        selector: String,
    },
    /// Demote an approved draft back to `draft` status (reverse of `mark-approved`)
    MarkDraft {
        /// Draft selector: mp://<account>/drafts/<id>, drafts/<id> or <id>
        #[arg(value_name = "SELECTOR")]
        selector: String,
    },
    /// Create a new email draft from template and print its selector
    New {
        /// Name for the new draft file
        name: String,
    },
    /// Print the filesystem path of a draft (the only selector-to-path edge)
    Path {
        /// Draft selector: mp://<account>/drafts/<id>, drafts/<id> or <id>
        #[arg(value_name = "SELECTOR")]
        selector: String,
    },
    /// Open a draft in $EDITOR
    Edit {
        /// Draft selector: mp://<account>/drafts/<id>, drafts/<id> or <id>
        #[arg(value_name = "SELECTOR")]
        selector: String,
    },
    /// Create a reply draft from a received email
    Reply {
        /// Received selector: mp://<account>/<mailbox>/<message-id>,
        /// <mailbox>/<message-id> or <message-id>
        #[arg(value_name = "SELECTOR")]
        selector: String,
        /// Reply to all recipients
        #[arg(long)]
        all: bool,
        /// Mailbox to resolve the selector in
        #[arg(long)]
        mailbox: Option<String>,
    },
    /// Forward an email to new recipients
    Forward {
        /// Received selector: mp://<account>/<mailbox>/<message-id>,
        /// <mailbox>/<message-id> or <message-id>
        #[arg(value_name = "SELECTOR")]
        selector: String,
        /// Mailbox to resolve the selector in
        #[arg(long)]
        mailbox: Option<String>,
    },
    /// RSVP to a received calendar invitation (iMIP REPLY over SMTP)
    Invite {
        #[command(subcommand)]
        action: InviteAction,
    },
    /// List available IMAP mailboxes/folders
    ListMailboxes,

    /// Fetch emails from IMAP server
    Fetch {
        /// Filter by sender address
        #[arg(long)]
        from: Option<String>,
        /// Filter by recipient address
        #[arg(long)]
        to: Option<String>,
        /// Filter by CC address
        #[arg(long)]
        cc: Option<String>,
        /// Subject contains
        #[arg(long)]
        subject: Option<String>,
        /// Body contains
        #[arg(long)]
        body: Option<String>,
        /// Emails since date (YYYY-MM-DD)
        #[arg(long)]
        since: Option<String>,
        /// Emails before date (YYYY-MM-DD)
        #[arg(long)]
        before: Option<String>,
        /// Max results (default: 10)
        #[arg(short = 'n', long, default_value = "10")]
        limit: usize,
        /// Show full body instead of preview
        #[arg(long)]
        full: bool,
        /// Mailbox name (default: INBOX)
        #[arg(long, default_value = "INBOX")]
        mailbox: String,
    },
    /// Sync mailboxes from the server into the local store
    Sync {
        /// Max messages per mailbox (default: 50)
        #[arg(short = 'n', long, default_value = "50")]
        limit: usize,
        /// Mailboxes to sync (default: INBOX, Archive, Sent)
        #[arg(long)]
        mailbox: Option<Vec<String>>,
        /// Show what would be ingested without writing anything
        #[arg(long)]
        dry_run: bool,
        /// Sync every configured account (failures are named at the end;
        /// exit code 1 if any account failed)
        //
        // Conflicts with `-A/--account`: the two answer the same question, and
        // silently ignoring the selector is how a cron line ends up syncing
        // accounts it never named. A second doc-comment paragraph would turn
        // this into clap's long help and reformat the whole subcommand's
        // `--help`, so the rationale stays a plain comment.
        #[arg(long, conflicts_with = "account")]
        all_accounts: bool,
    },
    /// Watch a mailbox for changes using IMAP IDLE
    Watch {
        /// Mailbox to watch (default: INBOX)
        #[arg(long, default_value = "INBOX")]
        mailbox: String,
        /// Timeout in seconds (exits with code 2 on timeout)
        #[arg(long)]
        timeout: Option<u64>,
    },
    /// Archive a received email (server + local)
    Archive {
        /// Received selector: mp://<account>/<mailbox>/<message-id>
        #[arg(value_name = "SELECTOR")]
        selector: String,
        /// Mailbox to resolve the selector in
        #[arg(long)]
        mailbox: Option<String>,
    },
    /// Delete a received email (server + local) or a local draft
    //
    // Kept to one doc line on purpose: a second paragraph flips clap into its
    // long-help layout and reformats the whole subcommand's `--help`, the same
    // footgun `Sync` guards against. The drafts vs received split and the
    // --force/--sent rules are carried by the argument help below.
    Delete {
        /// Received (mp://<acct>/<mbox>/<id>) or drafts (mp://<acct>/drafts/<id>) selector; omit with --sent
        #[arg(value_name = "SELECTOR", required_unless_present = "sent")]
        selector: Option<String>,
        /// Mailbox to resolve the selector in
        #[arg(long)]
        mailbox: Option<String>,
        /// Delete an approved draft (a queued send) anyway
        #[arg(long)]
        force: bool,
        /// Clear every sent draft of the account (takes no selector)
        #[arg(long, conflicts_with_all = ["selector", "mailbox", "force"])]
        sent: bool,
    },
    /// Open a received email's attachment in the default application
    Open {
        /// Received selector: mp://<account>/<mailbox>/<message-id>
        #[arg(value_name = "SELECTOR")]
        selector: String,
        /// Mailbox to resolve the selector in
        #[arg(long)]
        mailbox: Option<String>,
    },
    /// Save a received email's attachment(s) to a directory
    Save {
        /// Received selector: mp://<account>/<mailbox>/<message-id>
        #[arg(value_name = "SELECTOR")]
        selector: String,
        /// Output directory (default: current directory)
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Mailbox to resolve the selector in
        #[arg(long)]
        mailbox: Option<String>,
    },
    /// Print one received message from the local store (offline)
    Show {
        /// Received selector: mp://<account>/<mailbox>/<message-id>,
        /// <mailbox>/<message-id> or <message-id>
        #[arg(value_name = "SELECTOR")]
        selector: String,
        /// Mailbox to resolve the selector in
        #[arg(long)]
        mailbox: Option<String>,
        /// Emit one JSON object (headers, attachments and body) instead
        #[arg(long)]
        json: bool,
    },
    /// List received messages from the local store (offline)
    ListMessages {
        /// Mailbox to list (role, slug or sidebar label).
        /// Default: every mailbox of the account, grouped.
        #[arg(long)]
        mailbox: Option<String>,
        /// Max messages per mailbox listed (default: 20)
        #[arg(short = 'n', long, default_value = "20")]
        limit: usize,
    },
    /// Search emails on the IMAP server, or locally with --local
    #[command(long_about = SEARCH_LONG_ABOUT)]
    Search {
        /// Search query. One grammar for every backend: fields from: to: cc:
        /// subject: body: filename:, the flag has:attachment, dates
        /// before:YYYY-MM-DD / after:YYYY-MM-DD (since: aliases after:),
        /// quoted "phrases", OR and (a OR b) groups, plus in: and message-id:.
        /// Combine with the flags below; they build the same query.
        #[arg(default_value = "")]
        query: String,
        /// Mailbox to search (default: all the account's mailboxes)
        #[arg(long)]
        mailbox: Option<String>,
        /// Match the sender (from:)
        #[arg(long)]
        from: Option<String>,
        /// Match a recipient (to:)
        #[arg(long)]
        to: Option<String>,
        /// Match a Cc recipient (cc:)
        #[arg(long)]
        cc: Option<String>,
        /// Match the subject (subject:)
        #[arg(long)]
        subject: Option<String>,
        /// Match the body (body:)
        #[arg(long)]
        body: Option<String>,
        /// Match an attachment filename (Gmail/Exchange/--local only)
        #[arg(long)]
        filename: Option<String>,
        /// Only mail carrying an attachment (has:attachment)
        #[arg(long)]
        has_attachment: bool,
        /// On or after this date, YYYY-MM-DD (after:)
        #[arg(long)]
        after: Option<String>,
        /// Strictly before this date, YYYY-MM-DD (before:)
        #[arg(long)]
        before: Option<String>,
        /// Max results (default: 20)
        #[arg(short = 'n', long, default_value = "20")]
        limit: usize,
        /// Show full body instead of preview
        #[arg(long)]
        full: bool,
        /// Search the local store's full-text index instead of the server
        /// (offline, ranked, covers every synced mailbox at once)
        #[arg(long)]
        local: bool,
    },
    /// Manage configuration
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// Contact index operations
    Contacts {
        #[command(subcommand)]
        action: ContactsAction,
    },
    /// Calendar / iMIP invite operations
    Calendar {
        #[command(subcommand)]
        action: CalendarAction,
    },
    /// Inspect and unblock the durable send queue
    Outbox {
        #[command(subcommand)]
        action: OutboxAction,
    },
    /// Local store maintenance (retention garbage collection)
    Store {
        #[command(subcommand)]
        action: StoreAction,
    },
    /// Report what is left of the file-era `.md` tree, and import its drafts.
    ///
    /// Assigns an `id:` frontmatter field to any draft that has none, so a
    /// file-era draft becomes addressable by selector, then names the
    /// file-era mailbox directories nothing reads any more and prints the
    /// command that removes them. It deletes nothing itself. `--dry-run`
    /// writes not even the `id:` field.
    Cutover {
        /// Account name (default: all configured accounts)
        #[arg(long)]
        account: Option<String>,
        /// Report only; write nothing at all
        #[arg(long)]
        dry_run: bool,
    },
    /// Dump the TUI key bindings from the single KEYMAP source of truth.
    ///
    /// Markdown by default; `--json` emits the section-grouped shape the
    /// website consumes. Regenerate the site data with:
    /// `mp dump-keys --json > website/src/data/tui-keys.json`
    /// (see scripts/regen-website-keys.sh).
    DumpKeys {
        /// Emit JSON grouped by section instead of Markdown.
        #[arg(long)]
        json: bool,
    },
    /// Dump message envelopes from the local message store as NDJSON.
    ///
    /// Offline: reads the local store only, never the network. One compact
    /// JSON object per line, with the fields account, mailbox, message_id,
    /// from, to, cc, subject, date_sort, flags, attachments (name + size) and
    /// invite. No filesystem paths appear in the output.
    ///
    /// Records are sorted by account, mailbox, date_sort, message_id and
    /// subject (the message's uid breaks remaining ties without being
    /// emitted), so two runs over an unchanged store are byte-identical.
    ///
    /// Dumps every configured account by default; `-A/--account` restricts it
    /// to one, `--mailbox` to the named mailboxes.
    DumpMailbox {
        /// Emit newline-delimited JSON. Currently the only output format, and
        /// required, so a later default cannot silently change this one.
        #[arg(long, required = true)]
        json: bool,
        /// Mailbox to dump (role, slug or sidebar label; repeatable).
        /// Default: every mailbox of every selected account.
        #[arg(long)]
        mailbox: Option<Vec<String>>,
    },
    /// Inspect the configured accounts.
    ///
    /// Hidden for the same reason as `mp daemon` below: it is the oracle
    /// `mp --daemon account list` must match, so it becomes visible with the
    /// cutover rather than before it.
    #[command(hide = true)]
    Account {
        #[command(subcommand)]
        action: AccountAction,
    },
    /// Manage the local mailypoppins daemon (run, start, status, stop, restart).
    ///
    /// Hidden until the cutover slices make the daemon the default:
    /// `tests/cli_help_snapshot.rs` pins `mp --help` byte-identical to the
    /// pre-daemon baseline, and a visible subcommand would move it. P4-U1
    /// removed the `daemon` cargo feature; the `hide` stays until the surface
    /// is meant to move, and the snapshot moves once with it.
    #[command(hide = true)]
    Daemon {
        #[command(subcommand)]
        action: mailypoppins::daemon::lifecycle::DaemonAction,
    },
}

/// `mp account <action>`: what a client may ask about the accounts themselves.
#[derive(Clone, Debug, Subcommand)]
enum AccountAction {
    /// List the configured accounts, their backend and their state
    List,
}

/// Operator commands for the durable outbox (#0037).
///
/// The outbox drives itself: queued messages are submitted and their Sent copy
/// appended on the next startup or sync. These are for the two cases it cannot
/// decide alone, a submission that died without a verdict and may or may not
/// have been delivered, and one that a recipient was refused (#0063), which
/// nothing but a human can close.
#[derive(Subcommand)]
enum OutboxAction {
    /// List every queued, retrying, failed or partly delivered submission
    List,
    /// Send a failed submission again (only after checking it did not arrive)
    Retry {
        /// Outbox row id, as shown by `mp outbox list`
        id: i64,
    },
    /// Drop a submission and release the message bytes it holds
    Discard {
        /// Outbox row id, as shown by `mp outbox list`
        id: i64,
    },
}

/// Local store maintenance (#0060). Today just the retention garbage
/// collector; the sweep that also runs automatically after every sync.
#[derive(Subcommand)]
enum StoreAction {
    /// Run the retention sweep now: evict cached blobs over the disk cap.
    ///
    /// The first over-cap run only warns and records a marker; run it again to
    /// evict. `--dry-run` prints what would go without touching anything. A run
    /// that would reclaim more than half the store's blob bytes is refused
    /// without `--force` (a fat-finger guard while on-demand re-fetch, #0085,
    /// does not yet exist).
    Gc {
        /// Print what would be evicted and stop; change nothing.
        #[arg(long)]
        dry_run: bool,
        /// Evict even when the plan would reclaim more than half the store.
        #[arg(long)]
        force: bool,
        /// Sweep every configured account rather than just the default / `-A`.
        #[arg(long)]
        all_accounts: bool,
    },
}

/// Organizer-side calendar operations (#0030).
#[derive(Subcommand)]
enum CalendarAction {
    /// Report what the stored attendee REPLY emails resolve on the stored
    /// invitations. Writes nothing: attendee statuses are derived from the
    /// `invite.ics` payloads wherever they are displayed, so there is no
    /// cached copy to rebuild.
    Rebuild {
        /// Account name (default: all configured accounts)
        #[arg(long)]
        account: Option<String>,
    },
}

/// RSVP actions for `mp invite <accept|tentative|decline> <selector>`.
/// Whole-series only (v1); the target is a received message whose store row
/// carries an `invite.ics` blob.
#[derive(Subcommand)]
enum InviteAction {
    /// Accept the invitation (PARTSTAT=ACCEPTED)
    Accept {
        /// Received selector of the invitation: mp://<account>/<mailbox>/<message-id>
        #[arg(value_name = "SELECTOR")]
        selector: String,
        /// Mailbox to resolve the selector in
        #[arg(long)]
        mailbox: Option<String>,
    },
    /// Tentatively accept the invitation (PARTSTAT=TENTATIVE)
    Tentative {
        /// Received selector of the invitation: mp://<account>/<mailbox>/<message-id>
        #[arg(value_name = "SELECTOR")]
        selector: String,
        /// Mailbox to resolve the selector in
        #[arg(long)]
        mailbox: Option<String>,
    },
    /// Decline the invitation (PARTSTAT=DECLINED)
    Decline {
        /// Received selector of the invitation: mp://<account>/<mailbox>/<message-id>
        #[arg(value_name = "SELECTOR")]
        selector: String,
        /// Mailbox to resolve the selector in
        #[arg(long)]
        mailbox: Option<String>,
    },
}

#[derive(Subcommand)]
enum ContactsAction {
    /// Search the contact index
    Search {
        /// Query string (fuzzy-matched against name and email)
        query: Option<String>,
        /// Emit tab-delimited `email\tname` lines (for mutt/aerc/vim integration)
        #[arg(long)]
        parsable: bool,
        /// Max number of results
        #[arg(short = 'n', long, default_value = "20")]
        limit: usize,
        /// Account name (default: first configured account)
        #[arg(long)]
        account: Option<String>,
    },
    /// Rebuild the contact index from the local message store
    Rebuild {
        /// Account name (default: all configured accounts)
        #[arg(long)]
        account: Option<String>,
    },
    /// Show index statistics
    Stats {
        /// Account name (default: first configured account)
        #[arg(long)]
        account: Option<String>,
    },
}

#[derive(Subcommand)]
enum ConfigAction {
    /// Interactive setup wizard
    Init,
    /// Show current configuration
    Show,
    /// Store a password in the active secrets backend
    SetPassword {
        /// Which password to set: "smtp" or "imap"
        which: String,
        /// Account name (required if multiple accounts)
        #[arg(long)]
        account: Option<String>,
    },
    /// Wipe the encrypted secrets file (and OAuth2 token caches) and re-prompt
    /// for credentials. Use this after a Time Machine restore to a new
    /// machine, or whenever the secrets file can no longer be decrypted.
    ResetSecrets,
    /// Add a new account to the existing config
    AddAccount,
    /// Run OAuth2 device code flow to acquire and cache a token
    Oauth2Login {
        /// Account name (default: first OAuth2 account)
        #[arg(long)]
        account: Option<String>,
    },
    /// Print config file path
    Path,
}

/// Sort fetched emails by date descending (newest first).
fn sort_fetched_by_date(emails: &mut [FetchedEmail]) {
    emails.sort_by(|a, b| {
        let da = chrono::DateTime::parse_from_rfc2822(&a.date).ok();
        let db = chrono::DateTime::parse_from_rfc2822(&b.date).ok();
        db.cmp(&da)
    });
}

fn prompt_confirmation(message: &str) -> bool {
    print!("{} [y/N] ", message);
    let _ = io::stdout().flush();

    let mut input = String::new();
    if io::stdin().read_line(&mut input).is_err() {
        return false;
    }

    matches!(input.trim().to_lowercase().as_str(), "y" | "yes")
}

/// CLI arguments for `mp send --invite`.
struct InviteArgs {
    to: Option<String>,
    cc: Option<String>,
    subject: Option<String>,
    start: Option<String>,
    end: Option<String>,
    duration: Option<String>,
    location: Option<String>,
    description: Option<String>,
    yes: bool,
}

/// `mp outbox <list|retry|discard>`: the operator surface of the durable send
/// queue (#0037 item 5).
///
/// Everything here is deliberately manual. A submission that died without a
/// verdict may or may not have been delivered, and no automatic rule can tell
/// the difference, so the row waits in `failed` until a human has looked in the
/// recipient's mailbox and decided between `retry` and `discard`.
async fn cmd_outbox(
    account_config: &mailypoppins::config::AccountConfig,
    action: OutboxAction,
) -> Result<()> {
    let account = account_config.name.as_str();
    let mut connection = daemon_connection().await;

    // The listing first, whatever was asked: the pre-daemon command looked for
    // a store file before it read the action, so an account that has never
    // queued anything gets one sentence and exit 0 from all three subcommands.
    let listing: mp_protocol::send::OutboxListing = typed_call(
        &mut connection,
        account,
        "send.outbox_list",
        serde_json::json!({"account": account}),
    )
    .await?;
    if !listing.ever_used {
        print_lines(&mp_client::format::outbox_cli_lines(&listing));
        return Ok(());
    }

    match action {
        OutboxAction::List => print_lines(&mp_client::format::outbox_cli_lines(&listing)),
        OutboxAction::Discard { id } => {
            let result = daemon_try_call(
                &mut connection,
                "send.outbox_discard",
                serde_json::json!({"account": account, "row_id": id}),
            )
            .await
            .map_err(|e| refusal(account, e))?;
            print_lines(&[mp_client::format::outbox_discard_line(
                id,
                wire_str(&result["message_id"]),
            )]);
        }
        OutboxAction::Retry { id } => {
            // A client that follows its own operation has to be a subscriber
            // first: the finished event reaches bootstrapped connections only.
            daemon_call(&mut connection, "state.bootstrap", serde_json::json!({})).await;
            let started = daemon_try_call(
                &mut connection,
                "send.outbox_retry",
                serde_json::json!({"account": account, "row_id": id}),
            )
            .await
            .map_err(|e| refusal(account, e))?;
            let operation = wire_str(&started["operation_id"]).to_string();
            let outcome: mp_protocol::send::OutboxRetryOutcome =
                match await_operation(&mut connection, &operation, |_| {}).await {
                    Settled::Done(result) => serde_json::from_value(result)
                        .context("reading the daemon's send.outbox_retry answer")?,
                    Settled::Failed(message) => return Err(anyhow!("{message}")),
                };
            print_lines(&mp_client::format::outbox_retry_lines(&outcome));
        }
    }
    Ok(())
}

/// Print rendered lines, putting back the colour `mp_client::format` does not
/// carry: a glyph's colour is a terminal's business and a GUI has neither.
fn print_lines(lines: &[String]) {
    for line in lines {
        println!("{}", paint(line));
    }
}

/// A unix timestamp as local `YYYY-MM-DD HH:MM`, or `-` when it is unset.
// Unused from P4-U12, when `mp outbox list` started rendering its time column
// through `mp_client::format`, and deleted with the rest of the direct engine
// paths by P4-U15.
#[allow(dead_code)]
fn format_unix_time(ts: i64) -> String {
    if ts <= 0 {
        return "-".to_string();
    }
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|dt| {
            dt.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| "-".to_string())
}

/// `mp send --invite`: an iMIP calendar invitation (`METHOD:REQUEST`).
///
/// The preview, the `UID` in it and the confirmation are the client's, because
/// a daemon has no stdin and a user who reads `UID: x` must be sent `UID: x`;
/// the `VEVENT`, the MIME tree and the durable submission are `send.invite`'s.
/// Both sides validate through one [`mailypoppins::invite::plan_invite`], so
/// the refusals arrive in the order the user has always met them in and the
/// Graph refusal (`ANO-4`) is made before anything is previewed.
async fn run_send_invite(
    account_config: &mailypoppins::config::AccountConfig,
    signature: serde_json::Value,
    args: InviteArgs,
) -> Result<()> {
    let request = mailypoppins::invite::InviteRequest {
        to: args.to.clone(),
        cc: args.cc.clone(),
        subject: args.subject.clone(),
        start: args.start.clone(),
        end: args.end.clone(),
        duration: args.duration.clone(),
        location: args.location.clone(),
        description: args.description.clone(),
    };
    let plan = mailypoppins::invite::plan_invite(account_config, &request, None)?;
    let spec = &plan.spec;
    // Connected before the preview: an invitation the user declines is still a
    // run that answered from the daemon, and `ANO-4` is refused above this line
    // so a Graph account costs no session.
    let mut connection = daemon_connection().await;

    println!("{}", "--- Invite Preview ---".bold());
    println!("  {} {}", "Summary:".yellow(), plan.subject);
    println!("  {} {}", "Organizer:".green(), spec.organizer);
    println!("  {} {}", "Attendees:".green(), spec.attendees.join(", "));
    println!(
        "  {} {}  \u{2192}  {}",
        "When:".blue(),
        spec.start.to_rfc3339(),
        spec.end.to_rfc3339()
    );
    if let Some(loc) = spec.location.as_deref() {
        println!("  {} {}", "Location:".blue(), loc);
    }
    println!("  {} {}", "UID:".dimmed(), spec.uid);
    println!("{}", "---".dimmed());

    if !args.yes && !prompt_confirmation("Send this invitation?") {
        println!("{}", mp_client::format::CANCELLED_LINE);
        return Ok(());
    }
    println!("Sending invitation...");

    let mut params = serde_json::json!({
        "account": account_config.name,
        "to": args.to,
        "cc": args.cc,
        "subject": args.subject,
        "start": args.start,
        "end": args.end,
        "duration": args.duration,
        "location": args.location,
        "description": args.description,
        // The UID the preview above printed, so what the user read is what goes
        // out; the daemon mints one only when a client previewed nothing.
        "uid": spec.uid,
    });
    params = with_params(params, signature);

    daemon_call(&mut connection, "state.bootstrap", serde_json::json!({})).await;
    let started = daemon_try_call(&mut connection, "send.invite", params)
        .await
        .map_err(|e| refusal(&account_config.name, e))?;
    let operation = wire_str(&started["operation_id"]).to_string();
    let outcome: mp_protocol::send::SendOutcome =
        match await_operation(&mut connection, &operation, |_| {}).await {
            Settled::Done(result) => serde_json::from_value(result)
                .context("reading the daemon's send.invite answer")?,
            Settled::Failed(message) => return Err(anyhow!("{message}")),
        };

    for recipient in &outcome.recipients {
        if recipient.delivered {
            println!("  {} {} ({})", "\u{2713}".green(), recipient.address, recipient.role);
        } else {
            println!(
                "  {} {} ({}): {}",
                "\u{2717}".red(),
                recipient.address,
                recipient.role,
                recipient.error.as_deref().unwrap_or("unknown error")
            );
        }
    }
    let delivered = outcome.recipients.iter().filter(|r| r.delivered).count();
    if delivered == 0 {
        return Err(anyhow!(
            "Failed to send invitation to all {} recipient(s)",
            outcome.recipients.len()
        ));
    }
    if delivered == outcome.recipients.len() {
        println!(
            "{} Invitation sent to all {} recipient(s) [{}]",
            "\u{2713}".green().bold(),
            outcome.recipients.len(),
            outcome.status_line
        );
    } else {
        println!(
            "{} Partial send: {} succeeded, {} failed",
            "\u{26a0}".yellow().bold(),
            delivered,
            outcome.recipients.len() - delivered
        );
    }
    Ok(())
}

/// `mp send <selector>`: the preview and the prompt here, the send there.
///
/// `draft.preview` is what resolves the selector, so the echoed line and the
/// refusal of a draft that does not validate are the daemon's answers rendered
/// locally, exactly as the bare-selector dry run is (P4-U6).
async fn routed_send(
    account_config: &mailypoppins::config::AccountConfig,
    selector: &str,
    yes: bool,
) -> Result<()> {
    let account = account_config.name.as_str();
    // Parsed here, because the sentence a selector with no account earns is the
    // parser's and predates the daemon: sending an empty account name across
    // the socket would answer it with `account_unknown` instead.
    let query =
        mailypoppins::selector::parse_in(selector, Namespace::Drafts, account, None)?;
    let mut connection = daemon_connection().await;
    let preview: mp_protocol::draft::DraftPreview = typed_call(
        &mut connection,
        account,
        "draft.preview",
        serde_json::json!({"account": account, "id": query.key}),
    )
    .await?;
    println!("{} {}", "\u{2192}".dimmed(), preview.selector);
    if let Some(error) = preview.error.as_deref() {
        return Err(anyhow!("{error}"));
    }

    // Which transport carries it is the account's business (#0058); what is
    // decided here is only how the message is shown before it goes.
    let is_graph = account_config.auth_method == AuthMethod::Graph;
    if is_graph {
        // Simplified: the Graph path needs no SMTP configuration to preview.
        println!("{}", "--- Email Preview ---".bold());
        println!("  {} {}", "To:".green(), preview.to.as_deref().unwrap_or("(none)"));
        if let Some(cc) = preview.cc.as_deref() {
            println!("  {} {}", "Cc:".blue(), cc);
        }
        if let Some(bcc) = preview.bcc.as_deref() {
            println!("  {} {}", "Bcc:".blue(), bcc);
        }
        println!("  {} {}", "Subject:".yellow(), preview.subject);
        println!("{}", "---".dimmed());
    } else {
        print!("{}", mailypoppins::draft_cmd::render_send_preview(&preview));
    }

    if !yes && !prompt_confirmation("Send this email?") {
        println!("{}", mp_client::format::CANCELLED_LINE);
        return Ok(());
    }
    println!(
        "{}",
        if is_graph {
            "Sending via Graph API..."
        } else {
            "Sending email..."
        }
    );

    daemon_call(&mut connection, "state.bootstrap", serde_json::json!({})).await;
    let started = daemon_try_call(
        &mut connection,
        "send.draft",
        serde_json::json!({"account": account, "id": preview.id}),
    )
    .await
    .map_err(|e| refusal(account, e))?;
    let operation = wire_str(&started["operation_id"]).to_string();
    let outcome: mp_protocol::send::SendOutcome =
        match await_operation(&mut connection, &operation, |_| {}).await {
            Settled::Done(result) => {
                serde_json::from_value(result).context("reading the daemon's send.draft answer")?
            }
            Settled::Failed(message) => return Err(anyhow!("{message}")),
        };

    let delivered = outcome.recipients.iter().filter(|r| r.delivered).count();
    if is_graph {
        if delivered == 0 {
            return Err(anyhow!(
                "{}",
                outcome
                    .recipients
                    .iter()
                    .find_map(|r| r.error.clone())
                    .unwrap_or_else(|| "Graph send failed".to_string())
            ));
        }
        if let Some(error) = outcome.settle_error.as_deref() {
            println!("{} (sent but failed to retire draft: {error})", "\u{26a0}".yellow());
        }
        println!(
            "{} Email sent successfully via Graph API [{}]",
            "\u{2713}".green().bold(),
            outcome.status_line
        );
        return Ok(());
    }

    print_lines(&mp_client::format::send_cli_lines(&outcome));
    if delivered == 0 {
        return Err(anyhow!(
            "Failed to send to all {} recipient(s)",
            outcome.recipients.len()
        ));
    }
    Ok(())
}

/// `mp send-approved`: the batch listing and the prompt here, the sends there.
///
/// The listing comes from `draft.list`, which is the family whose subject is
/// the drafts directory, and the file name it prints is the one field of that
/// listing a send path has any business with.
async fn routed_send_approved(
    account_config: &mailypoppins::config::AccountConfig,
    yes: bool,
) -> Result<()> {
    let account = account_config.name.as_str();
    // The pre-daemon loop loaded a transport per account and said so when it
    // could not. The line is the user's; the load behind it goes with the rest
    // of the client's transport handling in P4-U15.
    if let Err(e) = SmtpConfig::load(account_config) {
        eprintln!("{} Could not load SMTP config: {}", "\u{26a0}".yellow(), e);
    }
    let mut connection = daemon_connection().await;
    let listing: mp_protocol::draft::DraftListing = typed_call(
        &mut connection,
        account,
        "draft.list",
        serde_json::json!({"account": account, "status": "approved"}),
    )
    .await?;
    let approved: Vec<&mp_protocol::draft::DraftEntry> =
        listing.drafts.iter().filter(|entry| entry.valid).collect();
    if approved.is_empty() {
        println!("No approved drafts for {account}");
        return Ok(());
    }

    println!("\n{} approved email(s) found:\n", approved.len().to_string().bold());
    for entry in &approved {
        println!(
            "  {} -> {}",
            std::path::Path::new(&entry.path)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy(),
            entry.to.as_deref().unwrap_or("(bcc only)")
        );
    }
    if !yes
        && !prompt_confirmation(&format!(
            "\nSend all {} emails for {account}?",
            approved.len()
        ))
    {
        println!("{}", mp_client::format::CANCELLED_LINE);
        return Ok(());
    }

    daemon_call(&mut connection, "state.bootstrap", serde_json::json!({})).await;
    let started = daemon_try_call(
        &mut connection,
        "send.approved",
        serde_json::json!({"account": account}),
    )
    .await
    .map_err(|e| refusal(account, e))?;
    let operation = wire_str(&started["operation_id"]).to_string();
    let outcome: mp_protocol::send::ApprovedOutcome =
        match await_operation(&mut connection, &operation, |_| {}).await {
            Settled::Done(result) => serde_json::from_value(result)
                .context("reading the daemon's send.approved answer")?,
            Settled::Failed(message) => return Err(anyhow!("{message}")),
        };

    for result in &outcome.results {
        // The recipient is the listing's, because it is the listing the user
        // just read: the outcome names addresses, not the `to:` line.
        let to = result
            .selector
            .as_deref()
            .and_then(|selector| approved.iter().find(|entry| entry.selector == selector))
            .and_then(|entry| entry.to.as_deref())
            .unwrap_or("(bcc only)");
        print!("Sending to {to}... ");
        io::stdout().flush()?;
        println!("{}", paint(&mp_client::format::send_approved_line(result)));
    }
    println!(
        "\n{}",
        paint(&mp_client::format::send_approved_summary(&outcome))
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// The selector edge (#0050)
// ---------------------------------------------------------------------------

/// The store's mailbox key for the archive folder. `mp archive` is a move with
/// a fixed destination, exactly as the TUI frames it.
// Unused from P4-U8, when `mp archive` started answering from the daemon and
// the daemon started naming the destination mailbox itself, and deleted with
// the rest of the direct engine paths by P4-U15.
#[allow(dead_code)]
const ARCHIVE_MAILBOX: &str = "archive";

/// `--status` values for `mp list`. A closed set rather than a free string, so
/// a typo is a clap error instead of an empty listing.
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum DraftStatusFilter {
    Draft,
    Approved,
    Sent,
}

impl DraftStatusFilter {
    fn as_str(self) -> &'static str {
        match self {
            DraftStatusFilter::Draft => "draft",
            DraftStatusFilter::Approved => "approved",
            DraftStatusFilter::Sent => "sent",
        }
    }
}

/// Open the account's store for a *received* lookup.
///
/// A missing file means the account has never synced, which is a different
/// answer from "no such message" and is worth saying: resolving a selector
/// against an empty index would otherwise report the message as unknown.
fn received_store(account: &str) -> Result<Store> {
    let path = mailypoppins::config::store_path(account);
    if !path.exists() {
        return Err(no_received_store(account));
    }
    Store::open(&path).with_context(|| format!("opening the store of {account}"))
}

/// The refusal of a read against an account with nothing to read.
///
/// One sentence for the two routes: [`received_store`] raises it when it finds
/// no store file, and a routed command raises the identical one when the daemon
/// answers `account_not_ready`, because a user may not be able to tell which
/// process looked.
fn no_received_store(account: &str) -> anyhow::Error {
    anyhow!(
        "{account} has no local store yet, so no received mail can be addressed; \
         run `mp sync` first"
    )
}

/// Open the account's store and refresh its drafts index.
///
/// This is the engine-start refresh of #0050 scope item 5, paid by every
/// draft-facing command before it reads the table: a draft an agent wrote a
/// second ago is in the index by the time the command lists or resolves it.
// Unused from P4-U12: the daemon owns the drafts directory every send path
// reads. Deleted with the rest of the direct engine paths by P4-U15.
#[allow(dead_code)]
fn drafts_store(account: &str) -> Result<Store> {
    Ok(drafts_store_reporting(account)?.0)
}

/// [`drafts_store`], additionally handing back the files the refresh skipped
/// for a parse failure, so `mp list` can name them after its listing instead
/// of letting a broken draft vanish from the output (#0080).
// Unused from P4-U12: the daemon owns the drafts directory every send path
// reads. Deleted with the rest of the direct engine paths by P4-U15.
#[allow(dead_code)]
fn drafts_store_reporting(
    account: &str,
) -> Result<(Store, Vec<mailypoppins::store::drafts::SkippedDraft>)> {
    let store = Store::open(mailypoppins::config::store_path(account))
        .with_context(|| format!("opening the store of {account}"))?;
    let dir = mailypoppins::config::drafts_dir(account);
    let (_, collisions, skipped) =
        mailypoppins::store::drafts::refresh_reporting(&store, account, &dir)
            .with_context(|| format!("refreshing the drafts index of {account}"))?;
    // Two files claiming one id means one of them is unaddressable. The index
    // cannot decide which the user meant, so it says so rather than dropping
    // the loser in silence.
    for collision in &collisions {
        eprintln!("{} {collision}", "⚠".yellow());
    }
    Ok((store, skipped))
}

/// Print the warning block `mp list` shows after its listing when the refresh
/// skipped one or more drafts for a parse failure (#0080).
///
/// A skipped file is a draft the index cannot see: no `id:`, no row, absent
/// from the listing above. Naming it here, with its one-line parse error, is
/// what turns "my draft disappeared" into a fixable line. The exit code stays
/// 0: the listing itself succeeded, and the broken file is a warning about the
/// directory, not a failure of the command.
// Unused from P4-U6, when `mp list` started answering from the daemon and
// `draft_cmd::render_skipped` started rendering the block from the wire, and
// deleted with the rest of the direct engine paths by P4-U15.
#[allow(dead_code)]
fn print_skipped_drafts(skipped: &[mailypoppins::store::drafts::SkippedDraft]) {
    if skipped.is_empty() {
        return;
    }
    let n = skipped.len();
    let noun = if n == 1 { "draft" } else { "drafts" };
    eprintln!(
        "\n{} {n} {noun} skipped (frontmatter would not parse; fix the YAML to list them):",
        "⚠".yellow()
    );
    for skip in skipped {
        eprintln!("  {} - {}", skip.path.display().to_string().yellow(), skip.error);
    }
}

/// Re-index the drafts directory after a command wrote a draft, so the next
/// reader (the TUI, `mp list`, the next command) sees it without waiting for
/// the one-second scan. Best-effort: the write already happened, and the scan
/// would pick it up anyway.
// Unused from P4-U12: the daemon owns the drafts directory every send path
// reads. Deleted with the rest of the direct engine paths by P4-U15.
#[allow(dead_code)]
fn reindex_drafts(account: &str) {
    if let Err(e) = drafts_store(account) {
        warn!("could not refresh the drafts index of {account}: {e:#}");
    }
}

/// Resolve a draft selector to its indexed row plus the canonical selector.
// Unused from P4-U12: the daemon owns the drafts directory every send path
// reads. Deleted with the rest of the direct engine paths by P4-U15.
#[allow(dead_code)]
fn resolve_draft_arg(
    store: &Store,
    selector: &str,
    account: &str,
) -> Result<(mailypoppins::store::drafts::DraftRow, Selector)> {
    let query = mailypoppins::selector::parse_in(selector, Namespace::Drafts, account, None)?;
    mailypoppins::selector::resolve_draft(store, &query)
}

/// Whether a `mp delete` argument names a draft rather than received mail.
///
/// Dispatch is on the selector shape (#0073 scope item 1), not a second
/// command: a fully qualified drafts selector carries the reserved `drafts`
/// mailbox segment, and `--mailbox drafts` names it beside an elided selector.
/// Anything else is received mail, whose namespace `resolve_received_arg`
/// enforces.
fn is_drafts_selector(selector: &str, mailbox: Option<&str>) -> Result<bool> {
    if mailbox == Some(mailypoppins::selector::DRAFTS_MAILBOX) {
        return Ok(true);
    }
    let parts = mailypoppins::selector::parse(selector)?;
    Ok(parts.mailbox.as_deref() == Some(mailypoppins::selector::DRAFTS_MAILBOX))
}

/// Resolve a received selector to its message row plus the canonical selector.
/// The mailbox key (`MailboxRole` id) a `--mailbox` argument names.
///
/// Matches a role id or a sidebar label case-insensitively, the same rule
/// `mp dump-mailbox` and `mp list-messages` apply, and an unknown name is an
/// error naming what it could have been rather than an empty result.
fn resolve_mailbox_key(account: &AccountConfig, want: &str) -> Result<String> {
    let mailboxes: Vec<_> = mailypoppins::tui::app::build_mailboxes(account)
        .into_iter()
        .filter(|m| m.id != mailypoppins::selector::DRAFTS_MAILBOX)
        .collect();
    if let Some(hit) = mailboxes
        .iter()
        .find(|m| want.eq_ignore_ascii_case(&m.id) || want.eq_ignore_ascii_case(&m.label))
    {
        return Ok(hit.id.clone());
    }
    let known = mailboxes
        .iter()
        .map(|m| m.id.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    Err(anyhow!(
        "'{want}' is not a mailbox of {} (known: {known})",
        account.name
    ))
}

/// The `(label, total, first `limit` rows)` groups `mp list-messages` prints.
///
/// One group per configured mailbox, in sidebar order, so a whole-account
/// listing reads like the sidebar rather than like a merged stream; the drafts
/// pseudo-mailbox is skipped, because it is local truth and `mp list` owns it.
/// The limit is per mailbox for the same reason: a shared budget would let a
/// busy inbox hide every other mailbox entirely.
///
/// `--mailbox` matches a role id or a sidebar label case-insensitively, the
/// same rule `mp dump-mailbox` applies, and an unknown name is an error naming
/// what it could have been rather than an empty listing.
// Unused from P4-U4, when `mp list-messages` started answering from the daemon,
// and deleted with the rest of the direct engine paths by P4-U15. Kept until
// then so one unit moves the callers and another removes what they left.
#[allow(dead_code)]
fn list_message_groups(
    store: &Store,
    account: &AccountConfig,
    mailbox: Option<&str>,
    limit: usize,
) -> Result<Vec<(String, usize, Vec<mailypoppins::store::read::MessageRow>)>> {
    let selected = select_mailboxes(account, mailbox)?;

    let mut groups = Vec::new();
    for info in selected {
        let mut rows = mailypoppins::store::read::list_mailbox(store, &account.name, &info.id)?;
        let total = rows.len();
        rows.truncate(limit);
        groups.push((info.label.clone(), total, rows));
    }
    Ok(groups)
}

/// The mailboxes a listing covers: the one `--mailbox` names, or every mailbox
/// of the account but drafts.
///
/// Split out of [`list_message_groups`] so the in-process listing and the one
/// routed through the daemon resolve a mailbox name, and refuse an unknown one,
/// through the same code and with the same message.
fn select_mailboxes(
    account: &AccountConfig,
    mailbox: Option<&str>,
) -> Result<Vec<mailypoppins::tui::app::MailboxInfo>> {
    let mailboxes: Vec<_> = mailypoppins::tui::app::build_mailboxes(account)
        .into_iter()
        .filter(|m| m.id != mailypoppins::selector::DRAFTS_MAILBOX)
        .collect();
    match mailbox {
        Some(want) => {
            let hit: Vec<_> = mailboxes
                .iter()
                .filter(|m| want.eq_ignore_ascii_case(&m.id) || want.eq_ignore_ascii_case(&m.label))
                .cloned()
                .collect();
            if hit.is_empty() {
                let known = mailboxes
                    .iter()
                    .map(|m| m.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(anyhow!(
                    "'{want}' is not a mailbox of {} (known: {known})",
                    account.name
                ));
            }
            Ok(hit)
        }
        None => Ok(mailboxes),
    }
}

/// How long a routed command waits for the daemon, per call.
const DAEMON_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// How long a routed *mutation* waits for the daemon.
///
/// Ten seconds is right for a store read and wrong for `message.archive`: the
/// daemon commits the row change and drains the owed server op before it
/// answers (#0039), so the call spans a round trip to the mail server. A client
/// that gave up at ten seconds would exit 4 over work the daemon went on to
/// finish, which is the one answer a mutation may never give.
const DAEMON_MUTATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// How long a routed *server query* waits for the daemon: `mp list-mailboxes`
/// and `mp fetch`, whose work is a session on the mail server rather than a
/// store read (P4-U10). The pre-daemon binary waited as long as the server
/// took; this is the same order of magnitude as a mutation's round trip.
const DAEMON_SERVER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// Connect to the local daemon and complete the handshake, or end the run.
///
/// One line, on purpose: the policy lives in
/// [`mailypoppins::daemon::client::client_session`], which is the single door
/// every command reaching the daemon goes through from P4-U4 on. It starts a
/// daemon on demand when none is listening, waits a bounded time for it, and
/// exits 4 naming the socket and `mp daemon run` when that fails. It never
/// falls back: a command that asked for the daemon and quietly answered from
/// this process would let the user believe the daemon did the work.
async fn daemon_connection() -> mp_client::Connection {
    mailypoppins::daemon::client::client_session().await
}

/// One call on a routed command's connection.
///
/// A refusal the daemon spelled out (an unknown account, a mailbox that is not
/// one) is an ordinary command failure and exits 1; a daemon that stops
/// answering is exit 4, the same code as one that was never there.
async fn daemon_call(
    connection: &mut mp_client::Connection,
    method: &str,
    params: serde_json::Value,
) -> serde_json::Value {
    match daemon_try_call(connection, method, params).await {
        Ok(result) => result,
        Err(error) => {
            eprintln!("{} {}", "\u{2717}".red(), error.message);
            std::process::exit(1);
        }
    }
}

/// One call whose refusal the caller answers for.
///
/// A daemon that stops answering is still exit 4 here, the same code as one that
/// was never there, because that is a fact about the daemon rather than about
/// the command. Everything the daemon spelled out comes back typed, so a
/// migrated command can raise the error its pre-daemon self raised instead of
/// printing a refusal in a shape no user has seen before.
async fn daemon_try_call(
    connection: &mut mp_client::Connection,
    method: &str,
    params: serde_json::Value,
) -> std::result::Result<serde_json::Value, mp_protocol::RpcError> {
    daemon_try_call_within(connection, method, params, DAEMON_TIMEOUT).await
}

/// [`daemon_try_call`] under a budget of the caller's choosing, for the calls
/// whose work is not a store read.
async fn daemon_try_call_within(
    connection: &mut mp_client::Connection,
    method: &str,
    params: serde_json::Value,
    budget: std::time::Duration,
) -> std::result::Result<serde_json::Value, mp_protocol::RpcError> {
    match tokio::time::timeout(budget, connection.call(method, params)).await {
        Ok(Ok(result)) => Ok(result),
        Ok(Err(mp_client::ClientError::Rpc(error))) => Err(error),
        Ok(Err(e)) => daemon_unavailable(&format!("{method}: {e}")),
        Err(_) => daemon_unavailable(&format!(
            "{method} went unanswered for {}s",
            budget.as_secs()
        )),
    }
}

/// A refusal the daemon spelled out, as the error the command raises.
///
/// The bytes a user sees may not depend on which process did the looking, so
/// `account_not_ready` becomes the sentence a store-less read has always
/// produced and everything else travels as the daemon worded it. Both leave
/// through `main`'s own error path, which is where the pre-daemon binary
/// reported them.
fn refusal(account: &str, error: mp_protocol::RpcError) -> anyhow::Error {
    if error.code == mp_protocol::ErrorCode::AccountNotReady.code() {
        return no_received_store(account);
    }
    anyhow!("{}", error.message)
}

/// The exit-4 diagnostic of a routed command: why, where, and how to fix it.
fn daemon_unavailable(why: &str) -> ! {
    mailypoppins::daemon::client::unavailable(why, &mailypoppins::daemon::runtime::socket_path())
}

/// `mp account list`, printed identically whether the entries were built here
/// or came off the wire.
fn render_accounts(entries: &[mailypoppins::daemon::methods::account::AccountEntry]) -> String {
    if entries.is_empty() {
        return "No accounts configured\n".to_string();
    }
    let width = entries.iter().map(|e| e.name.len()).max().unwrap_or(0);
    let mut out = String::new();
    for entry in entries {
        out.push_str(&format!(
            "{:width$}  {:5}  {}{}\n",
            entry.name,
            entry.backend,
            entry.state,
            if entry.default { "  (default)" } else { "" },
        ));
    }
    out
}

/// `mp --daemon account list`: the daemon's answer, rendered locally.
async fn routed_account_list() -> Vec<mailypoppins::daemon::methods::account::AccountEntry> {
    let mut connection = daemon_connection().await;
    let result = daemon_call(&mut connection, "account.list", serde_json::json!({})).await;
    result["accounts"]
        .as_array()
        .map(|accounts| {
            accounts
                .iter()
                .filter_map(mailypoppins::daemon::methods::account::from_json)
                .collect()
        })
        .unwrap_or_default()
}

/// `mp list-messages`: which messages, in which order, and how many the mailbox
/// holds all come from the daemon.
///
/// One `message.list` per listed mailbox, because the method answers about one
/// mailbox and the grouping is the client's presentation. The mailbox name is
/// resolved here, through the same [`select_mailboxes`] the in-process listing
/// used, so an unknown one is refused in the words it has always been refused
/// in and without a round trip.
async fn routed_list_messages(
    account: &AccountConfig,
    mailbox: Option<&str>,
    limit: usize,
) -> Result<()> {
    let selected = select_mailboxes(account, mailbox)?;
    let mut connection = daemon_connection().await;
    let mut groups = Vec::new();
    for info in selected {
        let result = daemon_try_call(
            &mut connection,
            "message.list",
            serde_json::json!({
                "account": account.name,
                "mailbox": info.id,
                "limit": limit,
            }),
        )
        .await
        .map_err(|e| refusal(&account.name, e))?;
        let total = result["total"].as_u64().unwrap_or_default() as usize;
        let rows = result["messages"]
            .as_array()
            .map(|messages| {
                messages
                    .iter()
                    .map(|message| row_from_wire(message, &info.id))
                    .collect()
            })
            .unwrap_or_default();
        groups.push((info.label.clone(), total, rows));
    }
    print!(
        "{}",
        mailypoppins::read_cmd::render_list(&account.name, &groups)
    );
    Ok(())
}

/// One `message.list` entry as the row the renderer takes.
///
/// Everything a listing prints comes off the wire, `date_display` included, so
/// the routed path opens no store of its own. The fields the wire does not
/// carry are the ones nothing in a listing reads: the store id, the other
/// recipients, the body blob, the thread. `flags` is rebuilt as the token
/// string the store holds, so `MessageRow::flags` parses it back into the same
/// three bits.
fn row_from_wire(
    message: &serde_json::Value,
    mailbox: &str,
) -> mailypoppins::store::read::MessageRow {
    let uid = message["uid"].as_i64().unwrap_or_default();
    let text = |key: &str| {
        message[key]
            .as_str()
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    let flag = |key: &str| message["flags"][key].as_bool().unwrap_or(false);
    mailypoppins::store::read::MessageRow {
        id: 0,
        mailbox: mailbox.to_string(),
        uid,
        message_id: message["message_id"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        from: text("from"),
        to: None,
        cc: None,
        reply_to: None,
        bcc: None,
        subject: text("subject"),
        date_display: text("date_display"),
        flags: Some(
            MessageFlags {
                seen: flag("seen"),
                answered: flag("answered"),
                forwarded: flag("forwarded"),
                flagged: false,
            }
            .to_flag_string(),
        ),
        has_attachments: message["has_attachments"].as_bool().unwrap_or(false),
        body_blob: None,
        thread_id: None,
        is_invite: false,
    }
}

/// `mp show`: the record the daemon read, which is the record `--json` prints
/// and the record the text layout renders.
///
/// The selector crosses the socket unresolved, because resolving one needs the
/// store the client no longer has; which *account* it names stays a client-side
/// decision, since `Selector::parse` needs no store.
async fn routed_show(
    account: &str,
    selector: &str,
    mailbox: Option<&str>,
) -> Result<mailypoppins::read_cmd::ShownMessage> {
    let mut params = serde_json::json!({"account": account, "selector": selector});
    if let Some(mailbox) = mailbox {
        params["mailbox"] = serde_json::json!(mailbox);
    }
    let mut connection = daemon_connection().await;
    let result = daemon_try_call(&mut connection, "message.get", params)
        .await
        .map_err(|e| refusal(account, e))?;
    serde_json::from_value(result).context("reading the daemon's message.get answer")
}

/// `mp dump-mailbox --json`: every selected account's envelope records, in the
/// dump's own order.
///
/// The accounts are called in ascending name order, which is the client's only
/// ordering duty: the dump's sort key opens with the account name, so
/// concatenating the answers reproduces the whole order. An account the daemon
/// cannot read contributes nothing rather than failing the run, exactly as
/// `dump::collect_records` skipped a store it could not open.
async fn routed_dump(
    accounts: &[AccountConfig],
    filter: &[String],
) -> Result<Vec<mailypoppins::dump::EnvelopeRecord>> {
    let mut names: Vec<&str> = accounts.iter().map(|a| a.name.as_str()).collect();
    names.sort_unstable();
    let mailbox = if filter.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::json!(filter)
    };

    let mut connection = daemon_connection().await;
    let mut records = Vec::new();
    for name in names {
        let params = serde_json::json!({
            "account": name,
            "projection": "envelope",
            "mailbox": mailbox,
        });
        match daemon_try_call(&mut connection, "message.list", params).await {
            Ok(result) => {
                let answered: Vec<mailypoppins::dump::EnvelopeRecord> =
                    serde_json::from_value(result["records"].clone())
                        .context("reading the daemon's envelope records")?;
                records.extend(answered);
            }
            Err(e) if e.code == mp_protocol::ErrorCode::AccountNotReady.code() => continue,
            Err(e) => return Err(refusal(name, e)),
        }
    }
    Ok(records)
}

/// `mp search --local`: the ranked hits of the daemon's index read, as the rows
/// the search listing prints.
///
/// The flags travel as the user typed them and the daemon builds the query with
/// `search::from_cli`, so one parser serves every backend. `body` is `--full`;
/// `body_query` is `--body`, because one key may not mean two things.
async fn routed_search(
    account: &str,
    query: &str,
    mailbox: Option<&str>,
    flags: &mailypoppins::search::Flags,
    limit: usize,
    full: bool,
) -> Result<Vec<(mailypoppins::store::read::MessageRow, Option<String>)>> {
    let mut params = serde_json::json!({
        "account": account,
        "query": query,
        "limit": limit,
        "body": full,
        "has_attachment": flags.has_attachment,
    });
    for (key, value) in [
        ("mailbox", mailbox.map(str::to_string)),
        ("from", flags.from.clone()),
        ("to", flags.to.clone()),
        ("cc", flags.cc.clone()),
        ("subject", flags.subject.clone()),
        ("body_query", flags.body.clone()),
        ("filename", flags.filename.clone()),
        ("after", flags.after.clone()),
        ("before", flags.before.clone()),
    ] {
        if let Some(value) = value {
            params[key] = serde_json::json!(value);
        }
    }

    let mut connection = daemon_connection().await;
    let result = daemon_try_call(&mut connection, "message.search", params)
        .await
        .map_err(|e| refusal(account, e))?;
    Ok(result["hits"]
        .as_array()
        .map(|hits| {
            hits.iter()
                .map(|hit| {
                    let mailbox = hit["mailbox"].as_str().unwrap_or_default();
                    let body = hit["body"].as_str().map(str::to_string);
                    (row_from_wire(hit, mailbox), body)
                })
                .collect()
        })
        .unwrap_or_default())
}

/// One typed answer from the daemon, on an existing connection.
///
/// The refusal comes back as the error the command has always raised (see
/// [`refusal`]), so a routed failure reads exactly like the in-process one it
/// replaced.
async fn typed_call<T: serde::de::DeserializeOwned>(
    connection: &mut mp_client::Connection,
    account: &str,
    method: &str,
    params: serde_json::Value,
) -> Result<T> {
    let result = daemon_try_call(connection, method, params)
        .await
        .map_err(|e| refusal(account, e))?;
    serde_json::from_value(result).with_context(|| format!("reading the daemon's {method} answer"))
}

/// One typed answer from the daemon, on a connection of its own.
async fn draft_call<T: serde::de::DeserializeOwned>(
    account: &str,
    method: &str,
    params: serde_json::Value,
) -> Result<T> {
    let mut connection = daemon_connection().await;
    typed_call(&mut connection, account, method, params).await
}

/// The two signature flags, as the `draft.*` writers take them.
///
/// They travel rather than being resolved here: the daemon writes the file, so
/// the daemon splices the signature into the body, from the same configuration
/// [`resolve_body_signature`] reads.
fn signature_params(no_signature: bool, signature: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "no_signature": no_signature,
        "signature": signature,
    })
}

/// Merge `extra`'s keys into `params`, which is how a call adds the signature
/// flags to its own parameters.
fn with_params(mut params: serde_json::Value, extra: serde_json::Value) -> serde_json::Value {
    if let (Some(target), Some(source)) = (params.as_object_mut(), extra.as_object()) {
        for (key, value) in source {
            target.insert(key.clone(), value.clone());
        }
    }
    params
}

/// `mp mark-approved` and `mp mark-draft`: resolve, then rewrite.
///
/// Two calls on one connection, because `draft.path` is what tells the client
/// the *previous* status and the rewrite's own four keys are frozen. A draft
/// already in the target status is left alone and reported, which is the `ℹ`
/// line; anything the daemon refuses (a sent draft) leaves through the error
/// path the command has always used.
async fn routed_mark(account: &str, selector: &str, approve: bool) -> Result<()> {
    let mut connection = daemon_connection().await;
    let location: mp_protocol::draft::DraftLocation = typed_call(
        &mut connection,
        account,
        "draft.path",
        serde_json::json!({"account": account, "selector": selector}),
    )
    .await?;

    let (method, target, already, done) = match approve {
        true => ("draft.approve", "approved", "is already approved", "approved"),
        false => ("draft.demote", "draft", "is already a draft", "demoted"),
    };
    if location.status == target {
        println!("{} {} {already}", "\u{2139}".blue(), location.selector);
        return Ok(());
    }
    daemon_try_call(
        &mut connection,
        method,
        serde_json::json!({"account": account, "id": location.id}),
    )
    .await
    .map_err(|e| refusal(account, e))?;
    println!("{} {done} {}", "\u{2713}".green(), location.selector);
    Ok(())
}

/// `mp reply` and `mp forward`: the draft the daemon built from a stored
/// message, and the message it names.
async fn routed_from_source(
    account: &str,
    method: &str,
    selector: &str,
    mailbox: Option<&str>,
    all: bool,
    signature: serde_json::Value,
) -> Result<mp_protocol::draft::DraftCreated> {
    let mut source = serde_json::json!({"selector": selector});
    if let Some(mailbox) = mailbox {
        source["mailbox"] = serde_json::json!(mailbox);
    }
    let params = with_params(
        serde_json::json!({"account": account, "source": source, "all": all}),
        signature,
    );
    draft_call(account, method, params).await
}

// ---------------------------------------------------------------------------
// The message-mutation slice, routed (P4-U8)
// ---------------------------------------------------------------------------

/// A string field of a daemon answer, which the shape says is there.
fn wire_str(value: &serde_json::Value) -> &str {
    value.as_str().unwrap_or_default()
}

/// `mp archive` and `mp delete` of received mail.
///
/// One call: the daemon resolves the selector, refuses in the command's own
/// words when it cannot, loads the account's credentials *before* it touches
/// the store, and then commits the row change and drains the op it owes the
/// server in one go (#0039). The synchronous UX is preserved because the
/// answer is the settled outcome rather than an acknowledgement.
async fn routed_received_mutation(
    account: &str,
    method: &str,
    selector: &str,
    mailbox: Option<&str>,
) -> Result<serde_json::Value> {
    let mut params = serde_json::json!({"account": account, "selector": selector});
    if let Some(mailbox) = mailbox {
        params["mailbox"] = serde_json::json!(mailbox);
    }
    let mut connection = daemon_connection().await;
    daemon_try_call_within(&mut connection, method, params, DAEMON_MUTATION_TIMEOUT)
        .await
        .map_err(|e| refusal(account, e))
}

/// `mp delete <drafts selector>`: local-only, so one call and no backend.
async fn routed_discard(account: &str, selector: &str, force: bool) -> Result<serde_json::Value> {
    let mut connection = daemon_connection().await;
    daemon_try_call(
        &mut connection,
        "draft.discard",
        serde_json::json!({"account": account, "selector": selector, "force": force}),
    )
    .await
    .map_err(|e| refusal(account, e))
}

/// `mp delete --sent`: the sweep, and the two lines it prints.
///
/// The sweep keeps going past a draft it cannot remove, so the answer counts
/// what went and lists what stayed; one `⚠ keeping …` line per survivor, then
/// the summary. An account with nothing to sweep is a number rather than a
/// refusal, and prints the sentence it always printed.
async fn routed_sweep(account: &str) -> Result<()> {
    let mut connection = daemon_connection().await;
    let result = daemon_try_call(
        &mut connection,
        "draft.discard",
        serde_json::json!({"account": account, "sent": true}),
    )
    .await
    .map_err(|e| refusal(account, e))?;

    let cleared = result["cleared"].as_u64().unwrap_or_default();
    let kept = result["kept"].as_array().cloned().unwrap_or_default();
    if cleared == 0 && kept.is_empty() {
        println!("No sent drafts to clear on {account}");
        return Ok(());
    }
    for row in &kept {
        eprintln!(
            "{} keeping {}: {}",
            "\u{26a0}".yellow(),
            wire_str(&row["selector"]),
            wire_str(&row["error"])
        );
    }
    println!(
        "{} cleared {cleared} sent draft{} on {account}",
        "\u{2713}".green(),
        if cleared == 1 { "" } else { "s" }
    );
    Ok(())
}

/// The message `mp open` and `mp save` were pointed at.
///
/// One `message.get` with no body: it resolves the selector, so the client
/// learns the canonical selector its refusals and its materialisation calls are
/// phrased in, and the attachment list whose length is how many parts there are
/// to ask for.
async fn routed_attachment_target(
    connection: &mut mp_client::Connection,
    account: &str,
    selector: &str,
    mailbox: Option<&str>,
) -> Result<mailypoppins::read_cmd::ShownMessage> {
    let mut params =
        serde_json::json!({"account": account, "selector": selector, "body": false});
    if let Some(mailbox) = mailbox {
        params["mailbox"] = serde_json::json!(mailbox);
    }
    typed_call(connection, account, "message.get", params).await
}

/// One materialised attachment: the file the daemon wrote, and the name it was
/// sent under.
///
/// The daemon never renames a part - two parts sent under one name come back as
/// two handles carrying that one name, in two directories - so the file lives
/// at `<data_dir>/runtime/handles/<handle>/<name>` and stays there until it is
/// released or its ten minutes are up.
async fn routed_materialise(
    connection: &mut mp_client::Connection,
    account: &str,
    selector: &str,
    part: usize,
) -> Result<(String, PathBuf, String)> {
    let result = daemon_try_call(
        connection,
        "message.materialise_attachment",
        serde_json::json!({"account": account, "selector": selector, "part": part}),
    )
    .await
    .map_err(|e| refusal(account, e))?;
    Ok((
        wire_str(&result["handle"]).to_string(),
        PathBuf::from(wire_str(&result["path"])),
        wire_str(&result["name"]).to_string(),
    ))
}

/// Give a handle back, best-effort: the file has been copied where the user
/// wanted it, and a release the daemon refuses is a handle its own expiry will
/// collect.
async fn release_handle(connection: &mut mp_client::Connection, handle: &str) {
    if let Err(e) = daemon_try_call(
        connection,
        "message.release_handle",
        serde_json::json!({ "handle": handle }),
    )
    .await
    {
        warn!("releasing handle {handle}: {}", e.message);
    }
}

// Unused from P4-U14, when `mp invite` was the last command to resolve a
// received selector in this process. Deleted with the rest of the direct engine
// paths by P4-U15.
#[allow(dead_code)]
fn resolve_received_arg(
    store: &Store,
    selector: &str,
    account: &str,
    mailbox: Option<&str>,
) -> Result<(mailypoppins::store::read::MessageRow, Selector)> {
    let query = mailypoppins::selector::parse_in(selector, Namespace::Received, account, mailbox)?;
    mailypoppins::selector::resolve_received(store, &query)
}

/// The account a selector operates on: its own `mp://<account>/…` segment when
/// present, otherwise the `-A`/default account already resolved. The selector's
/// account overrides the flag because naming it in the selector is the more
/// specific statement, exactly as `parse_in` lets it override `--mailbox`.
///
/// Every selector command must call this *before* opening a store or loading a
/// transport, so a cross-account selector opens the right account's store and
/// server credentials instead of resolving against the default and reporting a
/// wrong-store miss (the #0073 follow-up bug). A selector naming an
/// unconfigured account fails here, loudly, rather than as a phantom miss.
fn account_for_selector(
    selector: &str,
    default: &AccountConfig,
    global: &GlobalConfig,
) -> Result<AccountConfig> {
    let parts = mailypoppins::selector::parse(selector)?;
    match parts.account {
        Some(name) if name != default.name => global
            .accounts
            .iter()
            .find(|a| a.name == name)
            .cloned()
            .ok_or_else(|| {
                let known = global
                    .accounts
                    .iter()
                    .map(|a| a.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                anyhow!(
                    "selector names account '{name}', which is not configured (known: {})",
                    if known.is_empty() { "none" } else { known.as_str() }
                )
            }),
        _ => Ok(default.clone()),
    }
}

/// Guard for a command whose transport is loaded before the selector is parsed
/// (`mp send`, `mp invite`): the SMTP/Graph credentials and the signature
/// already belong to `bound`, so a cross-account selector cannot be honoured
/// without reloading them. Rather than send from the wrong account silently, or
/// widen the send path to rebind mid-command, fail loudly and point at `-A`.
fn ensure_selector_account_matches(selector: &str, bound: &AccountConfig) -> Result<()> {
    let parts = mailypoppins::selector::parse(selector)?;
    if let Some(name) = parts.account {
        if name != bound.name {
            bail!(
                "selector names account '{name}', but this command is bound to '{}' \
                 (its transport is already configured); re-run with `-A {name}`",
                bound.name
            );
        }
    }
    Ok(())
}

/// The account's Markdown signature for a draft body (#0099), honouring the
/// global `--no-signature` / `--signature <name>` flags and the
/// `include_signature` config. Returns `None` (no signature block) when the
/// user opted out or nothing is configured.
fn resolve_body_signature(
    account: &AccountConfig,
    no_signature: bool,
    signature_name: Option<&str>,
    email: &EmailSettings,
) -> Option<String> {
    mailypoppins::config::body_signature(account, no_signature, signature_name, email)
}

/// The mailboxes an account is configured for, as a human-readable list for
/// the error a `--mailbox` typo produces.
///
/// The list itself moved into the library with the refusal that carries it
/// (P4-U10): the daemon resolves a sync's targets now, so the sentence has to be
/// reachable from both processes.
#[allow(dead_code)]
fn configured_mailbox_names(account: &AccountConfig) -> String {
    mailypoppins::config::configured_mailbox_names(account)
}

/// One end of a `mp sync` tick: the outbox, then the mutation queue (#0114).
///
/// Dead since P4-U10: `mp sync` drains inside the daemon's pass and renders the
/// report lines from the `operation.progress` events it publishes. The direct
/// path is deleted by P4-U15 with the rest of them.
#[allow(dead_code)]
///
/// The outbox goes first so a message that reached the server before the last
/// crash gets its Sent copy before this sync reads the mailbox it belongs in
/// (#0037 item 5); the mutation queue follows (#0039), so a move, delete or
/// flag toggle enqueued locally (by the TUI, or by a CLI invocation that
/// crashed before its op ran) is retired against the same mailboxes. Nothing is
/// drained and nothing is printed when nothing is owed, so a clean account adds
/// no traffic and no output at either end.
///
/// The returned suffix is always empty: this path reports on stdout rather than
/// in a status line, and only the shape of
/// [`mailypoppins::sync::tick::run_tick_with_drains`] is borrowed.
/// `label` distinguishes the tail's report lines from the head's, since
/// both print above the `✓ Synced` summary and would otherwise be four
/// identical `↻` lines when work was queued at both ends.
async fn drain_queues_cli(account_config: &AccountConfig, dry_run: bool, label: &str) -> String {
    if dry_run {
        return String::new();
    }
    let drained = mailypoppins::send::resume_outbox(account_config).await;
    if drained.completed > 0 || drained.still_open > 0 {
        println!(
            "  {} outbox{label}: {} completed, {} still pending",
            "↻".dimmed(),
            drained.completed,
            drained.still_open + drained.awaiting_submission
        );
    }
    match pending_ops::resume_account(account_config).await {
        Ok(Some(ops)) if ops.completed > 0 || ops.failed > 0 => {
            println!(
                "  {} mutations{label}: {} completed, {} failed",
                "↻".dimmed(),
                ops.completed,
                ops.failed
            );
        }
        Ok(_) => {}
        // Loud but not fatal: the tail drain must not turn a sync that worked
        // into a failed command, and the queue is retried on the next tick.
        Err(e) => {
            eprintln!("  {} mutations: drain failed: {e:#}", "⚠".yellow());
            log::warn!(
                "[pending_ops] draining {} at the sync tick failed: {e:#}",
                account_config.name
            );
        }
    }
    String::new()
}

/// One account's `mp sync`: the outbox drain, the sync itself, the contacts
/// hook, and the per-account summary lines.
///
/// Factored out of the `Sync` arm so `--all-accounts` is a loop over exactly
/// the single-account body (#0071). `Err` is an account-level failure, a
/// refused login above all; the caller names it and keeps going.
///
/// Dead since P4-U10: the pass is `sync.quick`'s and the rendering is
/// [`sync_one_routed`]'s. Deleted by P4-U15.
#[allow(dead_code)]
async fn sync_one_account(
    account_config: &AccountConfig,
    limit: usize,
    mailbox: Option<&[String]>,
    dry_run: bool,
) -> Result<()> {
    let targets: Vec<imap_client::SyncTarget> = if let Some(user_mailboxes) = mailbox {
        // Both halves of a target come from one configured mapping: building
        // the role from the typed string files an extra mailbox's rows under
        // `projects` while the rest of the product reads `Projects` (#0064).
        user_mailboxes
            .iter()
            .map(|mb| {
                let (role, server_name) = find_sync_target(account_config, mb)
                    .ok_or_else(|| {
                        anyhow!(
                            "account '{}' has no mailbox '{}' configured; it knows {}",
                            account_config.name,
                            mb,
                            configured_mailbox_names(account_config)
                        )
                    })?;
                Ok(imap_client::SyncTarget { role, server_name })
            })
            .collect::<Result<Vec<_>>>()?
    } else {
        all_configured_mailboxes(account_config)
            .iter()
            .map(|(role, mapping)| imap_client::SyncTarget {
                role: role.clone(),
                server_name: mapping.server.clone(),
            })
            .collect()
    };

    // The tick drains at both ends (#0114): once before the read, so the
    // server has converged by the time this sync looks at it, and once after,
    // so anything queued while the tick was in flight (a TUI running alongside
    // this one) is retired now instead of waiting for the next tick. Both ends
    // run whether the sync itself succeeded or failed, because the ticks that
    // fail are the long ones.
    let (_suffix, result) = mailypoppins::sync::tick::run_tick_with_drains(
        || drain_queues_cli(account_config, dry_run, ""),
        || async {
            if account_config.auth_method == AuthMethod::Graph {
                let graph_config = GraphConfig::load(account_config)?;
                // The Graph path is not guarded yet (#0122 covers the IMAP
                // ingest), so it always reports a pass that ran.
                graph::sync_mailboxes_graph(
                    &graph_config,
                    &account_config.name,
                    &targets,
                    limit,
                    dry_run,
                )
                .await
                .map(Some)
            } else {
                let imap_config = ImapConfig::load(account_config)?;
                // No body deadline (#0113): `mp sync` is the explicit recovery
                // path, and the pass a user runs to make the store converge is
                // the one pass that must not stop early.
                sync_mailboxes(
                    &imap_config,
                    &account_config.name,
                    &targets,
                    limit,
                    dry_run,
                    None,
                )
                .await
            }
        },
        || drain_queues_cli(account_config, dry_run, " (after sync)"),
    )
    .await;
    // Another process is this account's engine, so this run ingested nothing
    // and opened no session (#0122). That is a success, not a failure: the
    // holder is doing the work. Say so instead of printing a summary of a pass
    // that never ran.
    let Some(result) = result? else {
        println!(
            "{} Sync skipped: another engine is syncing '{}'; leaving the ingest to it",
            "ℹ".blue(),
            account_config.name,
        );
        return Ok(());
    };

    if !dry_run {
        // Incremental contacts-index update (best-effort).
        mailypoppins::contacts::hooks::bump_after_sync(account_config, &result.fresh_observations);
    }

    let prefix = if dry_run { "[dry-run] " } else { "" };

    if result.skipped > 0 {
        println!(
            "{} {}Synced: {} new, {} already present",
            "✓".green(),
            prefix,
            result.saved,
            result.skipped,
        );
    } else {
        println!(
            "{} {}Synced: {} email(s) {}",
            "✓".green(),
            prefix,
            result.saved,
            if dry_run { "to download" } else { "ingested" },
        );
    }

    if result.flags_updated > 0 {
        println!(
            "{} {}Status updated on {} message(s)",
            "ℹ".blue(),
            prefix,
            result.flags_updated,
        );
    }
    if result.uid_rebound > 0 {
        println!(
            "{} {}Rebound {} message(s) to new UIDs after a UIDVALIDITY reset",
            "ℹ".blue(),
            prefix,
            result.uid_rebound,
        );
    }
    if result.pruned > 0 {
        println!(
            "{} {}{} message(s) left their mailbox on the server",
            "ℹ".blue(),
            prefix,
            result.pruned,
        );
    }
    if result.prunes_deferred > 0 {
        println!(
            "{} {}{} removal(s) held back: this pass did not see every message, \
             run a full sync to apply them",
            "⚠".yellow(),
            prefix,
            result.prunes_deferred,
        );
    }
    // #0115: one line per mailbox that downloaded the same mail it downloaded
    // last pass. The exit code is unchanged, because nothing failed; what is
    // wrong is that the work repeats.
    {
        let mut names = result.non_converging.clone();
        names.sort();
        names.dedup();
        for name in names {
            println!(
                "{} {}'{}' downloaded the same messages again: the fetch is not converging, \
                 see the log and docs/tickets/0115-warn-on-a-non-converging-fetch.md",
                "⚠".yellow(),
                prefix,
                name,
            );
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// The sync/watch slice, routed (P4-U10)
// ---------------------------------------------------------------------------

/// The only mailbox the daemon's watcher watches (BACKLOG.md).
const WATCHED_MAILBOX: &str = mailypoppins::daemon::methods::sync::WATCHED_MAILBOX;

/// What one operation settled as.
enum Settled {
    /// The `result` a succeeded operation produced.
    Done(serde_json::Value),
    /// The message a failed or cancelled operation stopped with.
    Failed(String),
}

/// How one account's sync ended, which is all the caller's summary needs.
enum Synced {
    /// The account had nothing to sync and is out of the denominator.
    Skipped,
    /// A pass ran, or was left to the engine that holds the lock.
    Ran,
    /// It failed, in the daemon's own words.
    Failed(String),
}

/// Follow one operation to its end, rendering the reports it publishes on the
/// way.
///
/// Events rather than polling: a drain report lives on `operation.progress`, and
/// a poll of `operation.status` only ever sees the newest one. Both kinds are
/// lifecycle events, so neither is coalesced away nor dropped when a slow client
/// overflows its queue. There is no budget: a full sync takes as long as the
/// mailbox does, exactly as it did in process.
async fn await_operation(
    connection: &mut mp_client::Connection,
    id: &str,
    mut on_progress: impl FnMut(&serde_json::Value),
) -> Settled {
    use mailypoppins::daemon::operations::{KIND_OPERATION_FINISHED, KIND_OPERATION_PROGRESS};
    loop {
        let Some(notification) = connection.next_notification().await else {
            daemon_unavailable("the daemon closed the connection with an operation still running")
        };
        let params = notification.params;
        if params["payload"]["operation_id"].as_str() != Some(id) {
            continue;
        }
        match params["kind"].as_str() {
            Some(KIND_OPERATION_PROGRESS) => on_progress(&params["payload"]),
            Some(KIND_OPERATION_FINISHED) => {
                let payload = &params["payload"];
                return match payload["state"].as_str() {
                    Some("succeeded") => Settled::Done(payload["result"].clone()),
                    _ => Settled::Failed(wire_str(&payload["error"]["message"]).to_string()),
                };
            }
            _ => {}
        }
    }
}

/// One `operation.progress` report of a sync as the line `mp sync` prints for
/// it, and whether that line belongs on stderr.
///
/// The tail label is derived from the phase name and from nothing else, which is
/// why the five names are on the wire at all
/// ([`Phase::as_str`](mailypoppins::daemon::runtime::account::Phase::as_str)).
/// A phase with nothing to report carries a null `total` and prints no line, so
/// a clean account is as quiet as it always was.
fn drain_line(payload: &serde_json::Value) -> Option<(String, bool)> {
    let phase = payload["phase"].as_str()?;
    let tail = phase.starts_with("tail_");
    if let Some(error) = payload["message"].as_str() {
        return Some((mp_client::format::drain_failed_line(error), true));
    }
    let done = payload["done"].as_u64().unwrap_or_default();
    let total = payload["total"].as_u64()?;
    if phase.ends_with("_outbox") {
        Some((mp_client::format::outbox_drain_line(tail, done, total), false))
    } else if phase.ends_with("_mutations") {
        Some((mp_client::format::mutations_drain_line(tail, done, total), false))
    } else {
        None
    }
}

/// One rendered line with its glyph coloured the way this command has always
/// coloured it.
///
/// `mp_client::format` is deliberately colourless - a glyph's colour is a
/// terminal's business and a GUI has neither - so the CLI puts the colour back
/// on. The glyph is the only thing that decides it, so this needs to know
/// nothing about which line it is looking at.
fn paint(line: &str) -> String {
    let trimmed = line.trim_start();
    let indent = &line[..line.len() - trimmed.len()];
    let Some((glyph, rest)) = trimmed.split_once(' ') else {
        return line.to_string();
    };
    let painted = match glyph {
        "\u{2713}" => glyph.green(),
        "\u{2139}" => glyph.blue(),
        "\u{26a0}" => glyph.yellow(),
        "\u{2717}" => glyph.red(),
        "\u{21bb}" | "-" => glyph.dimmed(),
        _ => return line.to_string(),
    };
    format!("{indent}{painted} {rest}")
}

/// Whether a refusal is the daemon saying this account has no server at all.
fn is_local_only(error: &mp_protocol::RpcError) -> bool {
    error.code == mp_protocol::ErrorCode::AccountNotReady.code()
        && error
            .data
            .as_ref()
            .map(|data| data["state"] == "local_only")
            .unwrap_or(false)
}

/// `mp sync`: one operation per account, in configuration order.
///
/// `--all-accounts` stays a loop in the client (#0071): the per-account header,
/// the denominator that counts only what was attempted and the exit code are all
/// rendering of a per-account result, and a `^C` stops the account currently
/// running rather than every account at once.
async fn routed_sync(
    global_config: &GlobalConfig,
    accounts: &[AccountConfig],
    limit: usize,
    mailbox: Option<&[String]>,
    dry_run: bool,
) -> Result<()> {
    let mut connection = daemon_connection().await;
    // A client that follows its own operation has to be a subscriber first: the
    // progress and finished events reach bootstrapped connections only.
    daemon_call(&mut connection, "state.bootstrap", serde_json::json!({})).await;

    // One account's failure does not abort the others: the run continues and
    // every failure is named at the end (#0071). The seven-week outage in #0068
    // was a failure nothing named.
    let mut attempted = 0usize;
    let mut failed: Vec<String> = Vec::new();
    for account_config in accounts {
        if accounts.len() > 1 {
            println!("\n{}", format!("── {} ──", account_config.name).bold());
        }
        match sync_one_routed(&mut connection, account_config, limit, mailbox, dry_run).await {
            // A drafts-only account has nothing to sync and is not a failure;
            // counting it as one exits 1 on every run of a config that
            // legitimately holds one (#0071 review).
            Synced::Skipped => {}
            Synced::Ran => {
                attempted += 1;
                // The retention sweep rides on every real sync (#0060): a
                // dry-run touches nothing, and a failed sync is not a moment to
                // start deleting cached blobs. Still the client's, and the one
                // store this handler still opens; P4-U15 owns moving it.
                if !dry_run {
                    retention_sweep_after_sync(global_config, account_config);
                }
            }
            Synced::Failed(message) => {
                attempted += 1;
                error!("[sync] account '{}' failed: {message}", account_config.name);
                eprintln!("{} {}: {}", "✗".red(), account_config.name, message);
                failed.push(account_config.name.clone());
            }
        }
    }

    // Skipped accounts are out of the denominator too: "1 of 2" when the second
    // was never synced would be a claim about an account this run said nothing
    // about.
    if let Some(summary) = mailypoppins::sync_health::failure_summary(attempted, &failed) {
        eprintln!("{} {}", "✗".red(), summary);
    }
    let code = mailypoppins::sync_health::exit_code(&failed);
    if code != 0 {
        std::process::exit(code);
    }
    Ok(())
}

/// One account's pass: the operation, its reports, and the lines they render to.
///
/// `sync.quick` and not `sync.full`, always: `mp sync -n N` is a quick pass with
/// an explicit bound, and `-n` has a default, so the CLI has no unbounded form.
async fn sync_one_routed(
    connection: &mut mp_client::Connection,
    account: &AccountConfig,
    limit: usize,
    mailbox: Option<&[String]>,
    dry_run: bool,
) -> Synced {
    let mut params = serde_json::json!({
        "account": account.name,
        "limit": limit,
        "dry_run": dry_run,
    });
    if let Some(mailbox) = mailbox {
        params["mailbox"] = serde_json::json!(mailbox);
    }
    let started = match daemon_try_call(connection, "sync.quick", params).await {
        Ok(started) => started,
        Err(error) if is_local_only(&error) => {
            println!("{}", paint(&format!("- {}: local-only, skipped", account.name)));
            return Synced::Skipped;
        }
        Err(error) => return Synced::Failed(error.message),
    };

    let id = wire_str(&started["operation_id"]).to_string();
    let settled = await_operation(connection, &id, |payload| {
        if let Some((line, stderr)) = drain_line(payload) {
            if stderr {
                eprintln!("{}", paint(&line));
            } else {
                println!("{}", paint(&line));
            }
        }
    })
    .await;
    let result = match settled {
        Settled::Done(result) => result,
        Settled::Failed(message) => return Synced::Failed(message),
    };

    // Another process is this account's engine, so this run ingested nothing and
    // opened no session (#0122). That is a success, not a failure: the holder is
    // doing the work. Say so instead of printing a summary of a pass that never
    // ran.
    if result["blocked"].as_bool().unwrap_or(false) {
        println!(
            "{}",
            paint(&format!(
                "ℹ Sync skipped: another engine is syncing '{}'; leaving the ingest to it",
                account.name
            ))
        );
        return Synced::Ran;
    }
    if let Ok(outcome) =
        serde_json::from_value::<mp_protocol::events::SyncCompleted>(result["outcome"].clone())
    {
        let lines = if dry_run {
            mp_client::format::sync_cli_lines_dry_run(&outcome)
        } else {
            mp_client::format::sync_cli_lines(&outcome)
        };
        for line in lines {
            println!("{}", paint(&line));
        }
    }
    Synced::Ran
}

/// `mp watch`: the daemon holds the IDLE, the client holds the patience.
///
/// The narrowing to INBOX is announced before the call rather than discovered
/// afterwards, so a user who typed `--mailbox Archive` and got INBOX is told the
/// mailbox that was dropped, the reason and where the decision is recorded. The
/// daemon refuses anything but INBOX as well, so a client that skipped the
/// narrowing is told rather than quietly watched the wrong mailbox.
async fn routed_watch(account: &AccountConfig, mailbox: &str, timeout: Option<u64>) -> Result<()> {
    let mut connection = daemon_connection().await;
    daemon_call(&mut connection, "state.bootstrap", serde_json::json!({})).await;

    let watched = if mailbox.eq_ignore_ascii_case(WATCHED_MAILBOX) {
        mailbox.to_string()
    } else {
        eprintln!(
            "{} watching {WATCHED_MAILBOX} instead of '{mailbox}': the daemon's watcher is \
             {WATCHED_MAILBOX}-only (BACKLOG.md, `mp watch --mailbox` is narrowed to \
             {WATCHED_MAILBOX})",
            "⚠".yellow()
        );
        WATCHED_MAILBOX.to_string()
    };

    let started = daemon_try_call(
        &mut connection,
        "sync.watch",
        serde_json::json!({"account": account.name, "mailbox": watched}),
    )
    .await
    .map_err(|e| refusal(&account.name, e))?;
    let id = wire_str(&started["operation_id"]).to_string();
    println!("Watching {} for changes...", mailbox);

    let waited = match timeout {
        None => Some(await_operation(&mut connection, &id, |_| {}).await),
        Some(seconds) => tokio::time::timeout(
            std::time::Duration::from_secs(seconds),
            await_operation(&mut connection, &id, |_| {}),
        )
        .await
        .ok(),
    };
    match waited {
        Some(Settled::Done(_)) => {
            println!("{} Mailbox changed.", "✓".green());
            Ok(())
        }
        Some(Settled::Failed(message)) => Err(anyhow!("{message}")),
        // The timeout stays client-side: a daemon-side timer would be a second
        // place that knows about one client's patience. The watch is stopped
        // rather than abandoned, so the daemon drops the IDLE with us.
        None => {
            let _ = daemon_try_call(
                &mut connection,
                "operation.cancel",
                serde_json::json!({"operation_id": id}),
            )
            .await;
            println!("{} Timed out.", "ℹ".blue());
            std::process::exit(2);
        }
    }
}

/// `mp list-mailboxes`: what the server says it holds.
///
/// The two transports report different things about a mailbox, so `source` says
/// which answered: Graph carries the item counts this listing prints and IMAP's
/// `LIST` carries none.
async fn routed_list_mailboxes(account: &AccountConfig) -> Result<()> {
    let mut connection = daemon_connection().await;
    let result = daemon_try_call_within(
        &mut connection,
        "mailbox.list_server",
        serde_json::json!({"account": account.name}),
        DAEMON_SERVER_TIMEOUT,
    )
    .await
    .map_err(|e| refusal(&account.name, e))?;

    let graph = wire_str(&result["source"]) == "graph";
    println!(
        "{} Available {}:",
        "ℹ".blue(),
        if graph { "folders" } else { "mailboxes" }
    );
    let empty = Vec::new();
    for mailbox in result["mailboxes"].as_array().unwrap_or(&empty) {
        let name = wire_str(&mailbox["name"]);
        if !graph {
            println!("  {}", name);
            continue;
        }
        let unread = match mailbox["unread"].as_u64() {
            Some(unread) if unread > 0 => format!(" ({})", format!("{unread} unread").yellow()),
            _ => String::new(),
        };
        println!(
            "  {} {} total{}",
            name.green(),
            mailbox["total"].as_u64().unwrap_or_default(),
            unread,
        );
    }
    Ok(())
}

/// The `mp fetch` filters that become IMAP search terms.
struct FetchFilters {
    from: Option<String>,
    to: Option<String>,
    cc: Option<String>,
    subject: Option<String>,
    body: Option<String>,
    since: Option<String>,
    before: Option<String>,
}

/// `mp fetch`: what the server has, printed and not written (#0037).
///
/// `--full` never crosses the socket: it selects how much of a body the listing
/// prints, and the answer already carries all of it.
async fn routed_fetch(
    account: &AccountConfig,
    mailbox: &str,
    limit: usize,
    filters: FetchFilters,
    full: bool,
) -> Result<()> {
    let mut criteria = serde_json::Map::new();
    for (key, value) in [
        ("from", filters.from),
        ("to", filters.to),
        ("cc", filters.cc),
        ("subject", filters.subject),
        ("body", filters.body),
        ("since", filters.since),
        ("before", filters.before),
    ] {
        if let Some(value) = value {
            criteria.insert(key.to_string(), serde_json::json!(value));
        }
    }
    let mut connection = daemon_connection().await;
    let result = daemon_try_call_within(
        &mut connection,
        "message.list_server",
        serde_json::json!({
            "account": account.name,
            "mailbox": mailbox,
            "limit": limit,
            "criteria": criteria,
        }),
        DAEMON_SERVER_TIMEOUT,
    )
    .await
    .map_err(|e| refusal(&account.name, e))?;

    let emails: Vec<FetchedEmail> = result["messages"]
        .as_array()
        .map(|messages| messages.iter().map(fetched_from_wire).collect())
        .unwrap_or_default();
    display_fetched_emails(&emails, full);
    Ok(())
}

/// One `message.list_server` entry as the record the listing renders.
///
/// The fields left empty are the ones a fetch listing never reads: it prints six
/// headers, the attachment marker and the body, and it writes nothing, so there
/// is nowhere for a parsed attachment or a calendar part to go.
fn fetched_from_wire(message: &serde_json::Value) -> FetchedEmail {
    let text = |key: &str| message[key].as_str().map(str::to_string);
    FetchedEmail {
        from: text("from").unwrap_or_default(),
        to: text("to").unwrap_or_default(),
        cc: text("cc"),
        reply_to: None,
        bcc: None,
        subject: text("subject").unwrap_or_default(),
        date: text("date").unwrap_or_default(),
        body_text: text("body").unwrap_or_default(),
        html_body: None,
        has_attachments: message["has_attachments"].as_bool().unwrap_or(false),
        message_id: None,
        attachments: Vec::new(),
        flags: Default::default(),
        calendar_ics: None,
        event: None,
    }
}

/// Run the retention sweep for one account and report it, the body of
/// `mp store gc`.
///
/// A store file that does not exist yet has nothing to sweep, which is the
/// common case for a freshly configured or drafts-only account.
// Unused from P4-U14, when `mp store gc` started answering from
// `diagnostic.store_gc`. Deleted with the rest of the direct engine paths by
// P4-U15.
#[allow(dead_code)]
fn run_store_gc(
    global_config: &GlobalConfig,
    account: &AccountConfig,
    dry_run: bool,
    force: bool,
) -> Result<()> {
    let path = mailypoppins::config::store_path(&account.name);
    if !path.exists() {
        println!(
            "  {} {} has no store yet; nothing to sweep",
            "\u{b7}".dimmed(),
            account.name
        );
        return Ok(());
    }
    let policy = mailypoppins::config::retention_for(global_config, account)?;
    let store = mailypoppins::store::Store::open(&path)?;
    let blobs = mailypoppins::store::BlobStore::for_account(&account.name);
    let outcome = mailypoppins::store::sweep::sweep(
        &store,
        &blobs,
        &policy,
        mailypoppins::store::sweep::SweepOptions { dry_run, force },
    )?;
    report_sweep_outcome(&account.name, &outcome, true);
    Ok(())
}

/// Run the automatic post-sync retention sweep for one account, best-effort.
///
/// A sweep failure never fails the sync it rides on: the store is a cache and
/// an unswept cache is merely too big, not broken, so the error is logged and
/// the sync still reports success.
fn retention_sweep_after_sync(global_config: &GlobalConfig, account: &AccountConfig) {
    let policy = match mailypoppins::config::retention_for(global_config, account) {
        Ok(p) => p,
        Err(e) => {
            warn!("[retention] skipping sweep for '{}': {e:#}", account.name);
            return;
        }
    };
    let path = mailypoppins::config::store_path(&account.name);
    if !path.exists() {
        return;
    }
    let store = match mailypoppins::store::Store::open(&path) {
        Ok(s) => s,
        Err(e) => {
            warn!("[retention] sweep could not open the store of '{}': {e:#}", account.name);
            return;
        }
    };
    let blobs = mailypoppins::store::BlobStore::for_account(&account.name);
    match mailypoppins::store::sweep::sweep(
        &store,
        &blobs,
        &policy,
        mailypoppins::store::sweep::SweepOptions::default(),
    ) {
        Ok(outcome) => report_sweep_outcome(&account.name, &outcome, false),
        Err(e) => warn!("[retention] sweep of '{}' failed: {e:#}", account.name),
    }
}

/// Print a retention sweep outcome. `manual` distinguishes `mp store gc` (which
/// speaks even when there is nothing to do) from the post-sync sweep (which
/// stays quiet unless it warned, evicted, or cleared a stale marker).
fn report_sweep_outcome(
    account: &str,
    outcome: &mailypoppins::store::sweep::SweepOutcome,
    manual: bool,
) {
    use mailypoppins::store::sweep::{human_bytes, SweepDecision};
    let at = human_bytes(outcome.before_bytes);
    let cap = human_bytes(outcome.cap_bytes);
    match &outcome.decision {
        SweepDecision::UnderCap { cleared_marker } => {
            if *cleared_marker {
                println!(
                    "{} {}: store back under budget ({at} / {cap}); over-cap marker cleared",
                    "\u{2713}".green(),
                    account
                );
            } else if manual {
                println!(
                    "{} {}: store at {at} / cap {cap}, under budget (nothing to evict)",
                    "\u{2713}".green(),
                    account
                );
            }
        }
        SweepDecision::WarnedFirstBreach => {
            println!(
                "{} {}: store at {at} / cap {cap}, will prune on next run",
                "\u{26a0}".yellow(),
                account
            );
        }
        SweepDecision::RefusedTooMuch { would_evict_bytes } => {
            println!(
                "{} {}: pruning would reclaim {} ({} > half of {at}); refusing. \
                 Re-run `mp store gc --force` to proceed \
                 (on-demand re-fetch of an evicted body is not built yet, #0085).",
                "\u{26a0}".yellow(),
                account,
                human_bytes(*would_evict_bytes),
                human_bytes(*would_evict_bytes),
            );
        }
        SweepDecision::Evicted => {
            let prefix = if outcome.dry_run { "[dry-run] " } else { "" };
            let now = human_bytes(outcome.after_bytes);
            println!(
                "{} {}{}: {} {} blob(s) ({}), store {} at {now} / cap {cap}",
                "\u{2713}".green(),
                prefix,
                account,
                if outcome.dry_run { "would evict" } else { "evicted" },
                outcome.evicted.len(),
                human_bytes(outcome.reclaimed_bytes()),
                if outcome.dry_run { "would be" } else { "now" },
            );
            if outcome.dry_run {
                for e in &outcome.evicted {
                    let horizon = if e.past_horizon { " [past horizon]" } else { "" };
                    println!(
                        "    {} {} {} ({}){}",
                        "\u{2192}".dimmed(),
                        &e.hash[..12],
                        e.kind.as_str(),
                        human_bytes(e.size),
                        horizon,
                    );
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The admin slice, routed (P4-U14)
// ---------------------------------------------------------------------------

/// The account a `--account` flag names, or the first configured one.
///
/// The default and the lookup are the client's, because every all-accounts loop
/// of this slice is too: a method that quietly served "the first configured
/// account" would make the CLI's default and the GUI's default two rules that
/// can drift. The two sentences are the ones these commands have always
/// refused with.
fn pick_account_named<'a>(
    global: &'a GlobalConfig,
    name: Option<&str>,
) -> Result<&'a AccountConfig> {
    match name {
        Some(wanted) => global
            .accounts
            .iter()
            .find(|a| a.name.eq_ignore_ascii_case(wanted))
            .ok_or_else(|| anyhow!("no account named '{}'", wanted)),
        None => global
            .accounts
            .first()
            .ok_or_else(|| anyhow!("no accounts configured")),
    }
}

/// The accounts one pass walks: the named one, or every configured one in
/// configuration order.
fn accounts_for(global: &GlobalConfig, name: Option<&str>) -> Result<Vec<String>> {
    match name {
        Some(_) => Ok(vec![pick_account_named(global, name)?.name.clone()]),
        None => {
            if global.accounts.is_empty() {
                return Err(anyhow!("no accounts configured"));
            }
            Ok(global.accounts.iter().map(|a| a.name.clone()).collect())
        }
    }
}

/// One contact row off the wire.
fn contact_row(value: &serde_json::Value) -> mailypoppins::contacts_cmd::ContactRow {
    let count = |key: &str| value[key].as_u64().unwrap_or_default() as u32;
    mailypoppins::contacts_cmd::ContactRow {
        address: wire_str(&value["address"]).to_string(),
        display_name: wire_str(&value["display_name"]).to_string(),
        sent_to: count("sent_to"),
        sent_cc: count("sent_cc"),
        received: count("received"),
    }
}

/// Every contact row of one answer.
fn contact_rows(value: &serde_json::Value) -> Vec<mailypoppins::contacts_cmd::ContactRow> {
    value
        .as_array()
        .map(|rows| rows.iter().map(contact_row).collect())
        .unwrap_or_default()
}

/// `mp contacts search`: the ranking is the daemon's, the glyphs are this
/// process's.
async fn routed_contacts_search(
    account: &str,
    query: Option<&str>,
    parsable: bool,
    limit: usize,
) -> Result<()> {
    let mut connection = daemon_connection().await;
    let query = query.unwrap_or_default();
    let result = daemon_try_call(
        &mut connection,
        "contact.search",
        serde_json::json!({"account": account, "query": query, "limit": limit}),
    )
    .await
    .map_err(|error| refusal(account, error))?;
    mailypoppins::contacts_cmd::print_search(&contact_rows(&result["contacts"]), query, parsable);
    Ok(())
}

/// `mp contacts stats`.
async fn routed_contacts_stats(account: &str) -> Result<()> {
    let mut connection = daemon_connection().await;
    let result = daemon_try_call(
        &mut connection,
        "contact.stats",
        serde_json::json!({"account": account}),
    )
    .await
    .map_err(|error| refusal(account, error))?;
    let number = |key: &str| result[key].as_u64().unwrap_or_default();
    mailypoppins::contacts_cmd::print_stats(
        account,
        number("total") as usize,
        number("sent_to"),
        number("sent_cc"),
        number("received"),
        wire_str(&result["cache_path"]),
        wire_str(&result["built_at"]),
        &contact_rows(&result["top"]),
    );
    Ok(())
}

/// `mp contacts rebuild`: one operation per account, in configuration order,
/// with the header printed before each so a batch is legible while it runs.
async fn routed_contacts_rebuild(accounts: &[String]) -> Result<()> {
    use mailypoppins::contacts::CacheSave;

    let mut connection = operation_session().await;
    for account in accounts {
        mailypoppins::contacts_cmd::print_rebuild_header(account);
        let result = run_admin_operation(
            &mut connection,
            "contact.rebuild",
            serde_json::json!({"account": account}),
            account,
        )
        .await?;
        let contacts = result["contacts"].as_u64().unwrap_or_default() as usize;
        let kept = result["kept"].as_u64().unwrap_or_default() as usize;
        let saved = match wire_str(&result["saved"]) {
            "refused_empty" => CacheSave::RefusedEmpty { kept },
            "refused_shrunk" => CacheSave::RefusedShrunk {
                kept,
                rebuilt: contacts,
            },
            _ => CacheSave::Written,
        };
        mailypoppins::contacts_cmd::print_rebuild_outcome(
            &saved,
            contacts,
            std::path::Path::new(wire_str(&result["cache_path"])),
        );
    }
    Ok(())
}

/// `mp calendar rebuild`: the organiser-side fold, one account at a time, and
/// an account with no store is a note rather than the end of the pass.
async fn routed_calendar_rebuild(accounts: &[String]) -> Result<()> {
    let mut connection = operation_session().await;
    for account in accounts {
        mailypoppins::calendar_cmd::print_header(account);
        let started = daemon_try_call(
            &mut connection,
            "calendar.rebuild",
            serde_json::json!({"account": account}),
        )
        .await;
        let started = match started {
            Ok(started) => started,
            Err(error) if is_storeless(&error) => {
                mailypoppins::calendar_cmd::print_no_store(account);
                continue;
            }
            Err(error) => return Err(anyhow!("{}", error.message)),
        };
        let result = settle(&mut connection, &started).await?;
        let count = |key: &str| result[key].as_u64().unwrap_or_default() as usize;
        mailypoppins::calendar_cmd::print_report(
            count("resolved"),
            count("invites_seen"),
            count("replies_seen"),
            count("cancelled"),
        );
    }
    Ok(())
}

/// `mp invite accept|tentative|decline`: the reply is built, submitted and
/// filed by the daemon; the one line it prints is this process's.
async fn routed_rsvp(
    account: &str,
    selector: &str,
    mailbox: Option<&str>,
    response: &str,
) -> Result<()> {
    let mut connection = operation_session().await;
    let params = serde_json::json!({
        "account": account,
        "selector": selector,
        "mailbox": mailbox,
        "response": response,
    });
    let result = run_admin_operation(&mut connection, "calendar.rsvp", params, account).await?;
    let organizer = wire_str(&result["organizer"]).to_string();
    if !result["delivered"].as_bool().unwrap_or(false) {
        return Err(anyhow!("Failed to send the RSVP to {organizer}"));
    }
    println!(
        "{} {} \u{2014} replied to {}",
        "\u{2713}".green(),
        wire_str(&result["subject"]),
        organizer
    );
    Ok(())
}

/// `mp store gc`: the sweep is the daemon's, the report is
/// [`report_sweep_outcome`]'s, and an account with no store is a note.
async fn routed_store_gc(account: &str, dry_run: bool, force: bool) -> Result<()> {
    use mailypoppins::store::sweep::{BlobKind, EvictedBlob, SweepDecision, SweepOutcome};

    let mut connection = operation_session().await;
    let params = serde_json::json!({"account": account, "dry_run": dry_run, "force": force});
    let started = daemon_try_call(&mut connection, "diagnostic.store_gc", params).await;
    let started = match started {
        Ok(started) => started,
        Err(error) if is_storeless(&error) => {
            println!(
                "  {} {} has no store yet; nothing to sweep",
                "\u{b7}".dimmed(),
                account
            );
            return Ok(());
        }
        Err(error) => return Err(anyhow!("{}", error.message)),
    };
    let result = settle(&mut connection, &started).await?;
    let number = |key: &str| result[key].as_u64().unwrap_or_default();
    let decision = &result["decision"];
    let outcome = SweepOutcome {
        cap_bytes: number("cap_bytes"),
        before_bytes: number("before_bytes"),
        after_bytes: number("after_bytes"),
        evicted: result["evicted"]
            .as_array()
            .map(|blobs| {
                blobs
                    .iter()
                    .map(|blob| EvictedBlob {
                        hash: wire_str(&blob["hash"]).to_string(),
                        kind: BlobKind::from_wire(wire_str(&blob["kind"])),
                        size: blob["size"].as_u64().unwrap_or_default(),
                        newest_date: blob["newest_date"].as_i64().unwrap_or_default(),
                        past_horizon: blob["past_horizon"].as_bool().unwrap_or(false),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        decision: match wire_str(&decision["kind"]) {
            "warned_first_breach" => SweepDecision::WarnedFirstBreach,
            "refused_too_much" => SweepDecision::RefusedTooMuch {
                would_evict_bytes: decision["would_evict_bytes"].as_u64().unwrap_or_default(),
            },
            "evicted" => SweepDecision::Evicted,
            _ => SweepDecision::UnderCap {
                cleared_marker: decision["cleared_marker"].as_bool().unwrap_or(false),
            },
        },
        dry_run: result["dry_run"].as_bool().unwrap_or(dry_run),
    };
    report_sweep_outcome(account, &outcome, true);
    Ok(())
}

/// `mp cutover`: the drafts import and the file-era scan are the daemon's, and
/// the `rm -rf` block is printed once for every account that named a remnant.
async fn routed_cutover(accounts: &[String], dry_run: bool) -> Result<()> {
    use mailypoppins::cutover::{CutoverReport, LegacyRemnant};

    let mut connection = operation_session().await;
    if dry_run {
        mailypoppins::cutover::print_dry_run_notice();
    }
    let mut reports: Vec<CutoverReport> = Vec::new();
    for account in accounts {
        mailypoppins::cutover::print_account_header(account);
        let started = daemon_try_call(
            &mut connection,
            "config.cutover",
            serde_json::json!({"account": account, "dry_run": dry_run}),
        )
        .await;
        let started = match started {
            Ok(started) => started,
            Err(error) if is_storeless(&error) => {
                mailypoppins::cutover::print_no_store();
                continue;
            }
            Err(error) => return Err(anyhow!("{}", error.message)),
        };
        let result = settle(&mut connection, &started).await?;
        let strings = |value: &serde_json::Value| -> Vec<String> {
            value
                .as_array()
                .map(|rows| rows.iter().map(|row| wire_str(row).to_string()).collect())
                .unwrap_or_default()
        };
        let report = CutoverReport {
            account: account.clone(),
            imported: strings(&result["drafts"]["imported"])
                .into_iter()
                .map(PathBuf::from)
                .collect(),
            already_indexed: result["drafts"]["already_indexed"]
                .as_u64()
                .unwrap_or_default() as usize,
            skipped: strings(&result["drafts"]["skipped"]),
            collisions: strings(&result["drafts"]["collisions"]),
            remnants: result["remnants"]
                .as_array()
                .map(|rows| {
                    rows.iter()
                        .map(|row| LegacyRemnant {
                            path: PathBuf::from(wire_str(&row["path"])),
                            md_files: row["md_files"].as_u64().unwrap_or_default() as usize,
                            bytes: row["bytes"].as_u64().unwrap_or_default(),
                        })
                        .collect()
                })
                .unwrap_or_default(),
        };
        mailypoppins::cutover::print_report(
            &report,
            &mailypoppins::config::account_dir(account),
            dry_run,
        );
        reports.push(report);
    }
    let remnants: Vec<&LegacyRemnant> = reports.iter().flat_map(|r| &r.remnants).collect();
    mailypoppins::cutover::print_footer(&remnants);
    Ok(())
}

/// The configuration as the daemon holds it: where it lives, whether there is
/// one, and what it says.
///
/// `mp config init` and `mp config add-account` ask this **before** they
/// prompt, which is how the client learns the two facts each command's first
/// line prints, and what lets the declined branch satisfy
/// `MAILYPOPPINS_DAEMON_REQUIRE`.
async fn routed_config_state() -> Result<(PathBuf, bool, GlobalConfig)> {
    let mut connection = daemon_connection().await;
    let result = daemon_call(&mut connection, "config.get", serde_json::json!({})).await;
    let config: GlobalConfig = serde_json::from_value(result["config"].clone())
        .context("reading the effective configuration the daemon reported")?;
    Ok((
        PathBuf::from(wire_str(&result["path"])),
        wire_str(&result["state"]) != "absent",
        config,
    ))
}

/// Tell the daemon to re-read a `config.toml` a wizard just wrote.
///
/// Best-effort and silent: the wizard's own output is what the user reads, and
/// a reload that failed leaves the daemon on the previous snapshot, which the
/// next command reports for itself.
async fn reload_config_quietly() {
    let mut connection = daemon_connection().await;
    if let Err(e) = daemon_try_call(&mut connection, "config.reload", serde_json::json!({})).await {
        warn!("[client] the daemon could not reload the new configuration: {}", e.message);
    }
}

/// `mp config show`: the effective configuration, rendered from `config.get`.
async fn routed_config_show() -> Result<()> {
    let (path, _, config) = routed_config_state().await?;
    cmd_config_show(&config, &path)
}

/// `mp config set-password`: the prompt is this process's terminal's, the value
/// crosses the socket once, and nothing else about it is written here.
async fn routed_set_password(which: &str, account: &str) -> Result<()> {
    mailypoppins::config_cmd::password::check_kind(which);
    let value = mailypoppins::config_cmd::password::prompt(which, account)?;
    let mut connection = daemon_connection().await;
    daemon_try_call(
        &mut connection,
        "config.set_password",
        serde_json::json!({"account": account, "kind": which, "value": value}),
    )
    .await
    .map_err(|error| anyhow!("{}", error.message))?;
    println!(
        "{}",
        mailypoppins::config_cmd::password::stored_line(which, account)
    );
    Ok(())
}

/// `mp config reset-secrets`: the confirmation and every password prompt are
/// this process's; the unlinking and the writes are the daemon's.
async fn routed_reset_secrets() -> Result<()> {
    let (_, _, config) = routed_config_state().await?;
    let secrets_file = mailypoppins::secrets::secrets_path();
    let token_dir = mailypoppins::config::tokens_dir();

    println!("{}", "=== Reset Secrets ===".bold().cyan());
    println!();
    println!("This will:");
    println!("  - Delete {}", secrets_file.display());
    println!("  - Delete {}/*.enc", token_dir.display());
    println!("  - Prompt you to re-enter SMTP/IMAP passwords for each account");
    println!("  - For OAuth2/Graph accounts, you will need to re-run `mp config oauth2-login`");
    println!();
    print!("Continue? [y/N] ");
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    if !matches!(answer.trim().to_lowercase().as_str(), "y" | "yes") {
        println!("{}", mp_client::format::CANCELLED_LINE);
        return Ok(());
    }

    let mut connection = daemon_connection().await;
    let removed = daemon_try_call(
        &mut connection,
        "config.reset_secrets",
        serde_json::json!({}),
    )
    .await
    .map_err(|error| anyhow!("{}", error.message))?;
    for path in removed["removed"].as_array().unwrap_or(&Vec::new()) {
        println!("{} Removed {}", "\u{2713}".green(), wire_str(path));
    }

    if config.accounts.is_empty() {
        println!();
        println!(
            "{} No accounts configured. Run `mp config init` to create one.",
            "\u{26a0}".yellow()
        );
        return Ok(());
    }

    println!();
    for account in &config.accounts {
        match account.auth_method {
            AuthMethod::Password => {
                println!(
                    "{} Account '{}' (Password auth)",
                    "\u{25b6}".cyan(),
                    account.name.bold()
                );
                let smtp_pw = dialoguer::Password::new()
                    .with_prompt(format!("  SMTP password for '{}'", account.name))
                    .interact()
                    .context("SMTP password input cancelled")?;
                store_password(&mut connection, &account.name, "smtp", &smtp_pw).await?;
                println!("    {} SMTP password stored", "\u{2713}".green());

                print!("  Use a separate IMAP password? [y/N] ");
                io::stdout().flush()?;
                let mut sep = String::new();
                io::stdin().read_line(&mut sep)?;
                if matches!(sep.trim().to_lowercase().as_str(), "y" | "yes") {
                    let imap_pw = dialoguer::Password::new()
                        .with_prompt(format!("  IMAP password for '{}'", account.name))
                        .interact()
                        .context("IMAP password input cancelled")?;
                    store_password(&mut connection, &account.name, "imap", &imap_pw).await?;
                    println!("    {} IMAP password stored", "\u{2713}".green());
                }
            }
            AuthMethod::OAuth2 | AuthMethod::Graph => {
                println!(
                    "{} Account '{}' ({:?} auth) -- run `mp config oauth2-login --account {}` to re-acquire token",
                    "\u{2139}".blue(),
                    account.name.bold(),
                    account.auth_method,
                    account.name
                );
            }
        }
    }

    println!();
    println!("{} Secrets reset complete.", "\u{2713}".green().bold());
    Ok(())
}

/// One password across the socket, and nothing about it anywhere else.
async fn store_password(
    connection: &mut mp_client::Connection,
    account: &str,
    kind: &str,
    value: &str,
) -> Result<()> {
    daemon_try_call(
        connection,
        "config.set_password",
        serde_json::json!({"account": account, "kind": kind, "value": value}),
    )
    .await
    .map(|_| ())
    .map_err(|error| anyhow!("{}", error.message))
}

/// `mp config oauth2-login`: the daemon runs the device flow and reports the
/// verification URL and the user code; this process renders them (`INT-04`).
async fn routed_oauth2_login(global: &GlobalConfig, account: Option<&str>) -> Result<()> {
    // The default is the client's, exactly as it was: the first OAuth2 or Graph
    // account, else the first account at all.
    let name = match account {
        Some(name) => name.to_string(),
        None => global
            .accounts
            .iter()
            .find(|a| a.auth_method == AuthMethod::OAuth2 || a.auth_method == AuthMethod::Graph)
            .or_else(|| global.accounts.first())
            .ok_or_else(|| anyhow!("No accounts configured"))?
            .name
            .clone(),
    };

    let mut connection = operation_session().await;
    let started = daemon_try_call(
        &mut connection,
        "config.oauth2_login",
        serde_json::json!({"account": name}),
    )
    .await
    .map_err(|error| anyhow!("{}", error.message))?;

    let graph = global
        .accounts
        .iter()
        .find(|a| a.name == name)
        .is_some_and(|a| a.auth_method == AuthMethod::Graph);
    println!("{}", paint(&mp_client::format::oauth2_start_line(&name, graph)));

    let id = wire_str(&started["operation_id"]).to_string();
    let settled = await_operation(&mut connection, &id, |payload| {
        if payload["phase"].as_str() == Some(mailypoppins::daemon::methods::config::DEVICE_CODE_PHASE)
        {
            if let Some((uri, code)) = payload["message"].as_str().and_then(|m| m.split_once(' ')) {
                print!("{}", mp_client::format::oauth2_device_code_lines(uri, code));
            }
        }
    })
    .await;
    match settled {
        Settled::Done(_) => {
            println!("{}", paint(&mp_client::format::oauth2_stored_line(&name)));
            Ok(())
        }
        Settled::Failed(message) => Err(anyhow!("{message}")),
    }
}

/// A daemon session that will follow an operation.
///
/// A client only receives `operation.progress` and `operation.finished` if it
/// subscribed first, which `state.bootstrap` is: a session that skipped it
/// waits forever for a notification nobody addressed to it.
async fn operation_session() -> mp_client::Connection {
    let mut connection = daemon_connection().await;
    daemon_call(&mut connection, "state.bootstrap", serde_json::json!({})).await;
    connection
}

/// Start one operation of the admin slice and wait for its result, raising the
/// refusal the daemon spelled out.
async fn run_admin_operation(
    connection: &mut mp_client::Connection,
    method: &str,
    params: serde_json::Value,
    account: &str,
) -> Result<serde_json::Value> {
    let started = daemon_try_call(connection, method, params)
        .await
        .map_err(|error| refusal(account, error))?;
    settle(connection, &started).await
}

/// Wait for an operation this slice started, with no progress to render.
async fn settle(
    connection: &mut mp_client::Connection,
    started: &serde_json::Value,
) -> Result<serde_json::Value> {
    let id = wire_str(&started["operation_id"]).to_string();
    match await_operation(connection, &id, |_| {}).await {
        Settled::Done(result) => Ok(result),
        Settled::Failed(message) => Err(anyhow!("{message}")),
    }
}

/// Whether a refusal is the daemon saying this account has no store at all,
/// which several commands of this slice report as a note and walk past.
fn is_storeless(error: &mp_protocol::RpcError) -> bool {
    error.code == mp_protocol::ErrorCode::AccountNotReady.code()
}

/// The command and, where the no-daemon list distinguishes one, the
/// subcommand, as they were typed.
///
/// Taken from clap's own [`ArgMatches`](clap::ArgMatches) rather than from a
/// match over [`Commands`], so the strings are the ones on the command line,
/// kebab-case and all, and a renamed variant cannot silently move a command on
/// or off [`mailypoppins::daemon::client::needs_daemon`]'s list.
fn invocation(matches: &clap::ArgMatches) -> (Option<&str>, Option<&str>) {
    let command = matches.subcommand_name();
    let subcommand = matches
        .subcommand()
        .and_then(|(_, inner)| inner.subcommand_name());
    (command, subcommand)
}

#[tokio::main]
async fn main() -> Result<()> {
    init_logging();
    // Two steps rather than `Cli::parse()`, which is these two: the matches
    // carry the typed command name that the daemon policy is written in.
    // `get_matches` answers `--help` and `--version` and exits, exactly as
    // `parse` does, so the help surface does not move.
    let matches = <Cli as clap::CommandFactory>::command().get_matches();
    let cli = <Cli as clap::FromArgMatches>::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());
    let (command_name, subcommand_name) = invocation(&matches);
    let command_label = match (command_name, subcommand_name) {
        (Some(command), Some(sub)) => format!("{command} {sub}"),
        (Some(command), None) => command.to_string(),
        _ => "(no subcommand)".to_string(),
    };

    // Asking a command that can never reach the daemon to prove it did is a
    // mistake in the test, not a failure of the command, and saying so here
    // costs less than debugging an exit 1 from the end of the run.
    if mailypoppins::daemon::client::routing_required()
        && !mailypoppins::daemon::client::needs_daemon(command_name, subcommand_name)
    {
        eprintln!(
            "{} `mp {command_label}` is on the no-daemon list, so {} cannot be satisfied",
            "\u{2717}".red(),
            mailypoppins::daemon::client::REQUIRE_ENV,
        );
        std::process::exit(1);
    }

    info!("mailypoppins started: {:?}", std::env::args().collect::<Vec<_>>());

    // The daemon commands own their startup order (MIG-04, config load, socket)
    // and must not run the client preamble below, which loads secrets and SMTP
    // credentials this process has no use for.
    if let Some(Commands::Daemon { action }) = &cli.command {
        let code = mailypoppins::daemon::lifecycle::dispatch(action.clone()).await;
        std::process::exit(code);
    }

    // Move a pre-#0022 ~/.config/email directory before anything reads config
    // or secrets out of it. A failure here is fatal: the alternative is running
    // against an empty config and silently re-prompting for every password.
    if let Err(e) = mailypoppins::config::migrate_legacy_config_dir() {
        eprintln!("{} {}", "\u{2717}".red(), e);
        std::process::exit(1);
    }

    // Load global config from ~/.config/mailypoppins/config.toml
    let global_config = load_global_config().unwrap_or_else(|e| {
        eprintln!("{} {}", "⚠".yellow(), e);
        eprintln!("  Some commands may not work without proper configuration.");
        GlobalConfig::default()
    });

    // Copy any legacy `[accounts.*.signatures]` tables into the app-managed
    // signatures directory + state file (#0107), before anything resolves a
    // signature out of them. Non-fatal: a signature that fails to migrate
    // costs a warning, not a startup.
    if let Err(e) = mailypoppins::signatures::migrate_config_signatures(&global_config) {
        eprintln!("{} signature migration: {e:#}", "⚠".yellow());
    }

    // Initialize the secrets backend (encrypted file by default, or OS keyring
    // if the user opted in via `secrets_backend = "keyring"` in config.toml).
    if let Err(e) = mailypoppins::config::init_secrets_backend(&global_config) {
        match &e {
            mailypoppins::secrets::SecretsError::NotInitialized(_) => {
                // Empty store at startup is fine -- `mp config init` or
                // `set-password` will populate it. The actual missing-key
                // error surfaces later when `SmtpConfig::load` is called.
            }
            mailypoppins::secrets::SecretsError::Undecryptable(_, _) => {
                eprintln!("{} {}", "\u{2717}".red(), e);
                std::process::exit(1);
            }
            mailypoppins::secrets::SecretsError::Other(err) => {
                eprintln!("{} Could not initialize secrets backend: {}", "\u{26a0}".yellow(), err);
            }
        }
    }

    // Resolve which account to use
    let account_config: mailypoppins::config::AccountConfig = if let Some(ref name) = cli.account {
        global_config.accounts.iter()
            .find(|a| a.name == *name)
            .cloned()
            .unwrap_or_else(|| {
                eprintln!("{} Account '{}' not found in config", "⚠".yellow(), name);
                mailypoppins::config::AccountConfig::default()
            })
    } else {
        global_config.accounts.first().cloned().unwrap_or_default()
    };

    // Load SMTP config from account config + keyring.
    //
    // Unused from P4-U14, when `mp invite` started submitting its reply through
    // `calendar.rsvp`, and deleted with the rest of the direct engine paths by
    // P4-U15. The call itself stays: the two warning lines it prints when there
    // are no credentials are part of what every command has always printed.
    let _smtp_config = SmtpConfig::load(&account_config).unwrap_or_else(|e| {
        eprintln!("{} Could not load SMTP config: {}", "⚠".yellow(), e);
        eprintln!("  Some commands may not work without proper configuration.");
        SmtpConfig {
            host: "localhost".to_string(),
            port: 465,
            username: String::new(),
            password: String::new(),
            default_from: "user@example.com".to_string(),
            accept_invalid_certs: false,
            auth_method: mailypoppins::config::AuthMethod::Password,
        }
    });

    // Signature for direct sends and invites, which have no editable draft to
    // carry it in the body (#0099). Draft sends (`mp send`) take None: their
    // signature was appended to the body at `mp reply`/`mp forward`/`mp new`
    // time.
    // Unused from P4-U12, when `mp send --invite` started resolving its
    // signature in the daemon, and deleted with the rest of the direct engine
    // paths by P4-U15.
    let _signature_content: Option<String> = resolve_body_signature(
        &account_config,
        cli.no_signature,
        cli.signature.as_deref(),
        &global_config.email,
    );

    // The same two flags, unresolved, for the draft writers the daemon owns:
    // it writes the file, so it splices the signature (P4-U6). Taken before the
    // match, which moves the command out of `cli`.
    let draft_signature = signature_params(cli.no_signature, cli.signature.as_deref());

    match cli.command {
        Some(Commands::Send {
            selector,
            yes,
            invite,
            to,
            cc,
            subject,
            start,
            end,
            duration,
            location,
            description,
        }) => {
            if invite {
                run_send_invite(
                    &account_config,
                    draft_signature.clone(),
                    InviteArgs {
                        to,
                        cc,
                        subject,
                        start,
                        end,
                        duration,
                        location,
                        description,
                        yes,
                    },
                )
                .await?;
                mailypoppins::daemon::client::enforce_routing(&command_label);
                return Ok(());
            }

            let selector = selector.ok_or_else(|| {
                anyhow!(
                    "`mp send` needs a draft selector, or use `--invite` to send a calendar \
                     invitation"
                )
            })?;
            // The account this command is bound to picked the transport before
            // the selector was read, so a cross-account selector fails loudly
            // rather than sending from the wrong account.
            ensure_selector_account_matches(&selector, &account_config)?;
            routed_send(&account_config, &selector, yes).await?;
        }

        // `--all-accounts` is a loop over the same body rather than a second
        // code path, in configuration order, and it keeps going past an account
        // with nothing to send (#0071).
        Some(Commands::SendApproved { all_accounts, yes }) => {
            let accounts: Vec<mailypoppins::config::AccountConfig> = if all_accounts {
                global_config.accounts.clone()
            } else {
                vec![account_config.clone()]
            };
            if accounts.is_empty() {
                return Err(anyhow!("No account configured (check `mp config show`)"));
            }
            for account_config in accounts {
                routed_send_approved(&account_config, yes).await?;
            }
        }

        Some(Commands::List { status }) => {
            let listing: mp_protocol::draft::DraftListing = draft_call(
                &account_config.name,
                "draft.list",
                serde_json::json!({
                    "account": account_config.name,
                    "status": status.map(DraftStatusFilter::as_str),
                }),
            )
            .await?;
            // The collisions first, because the index reported them before it
            // was read; the skipped files after the listing they are missing
            // from. Both are warnings about the directory: exit code 0.
            eprint!(
                "{}",
                mailypoppins::draft_cmd::render_collisions(&listing.collisions)
            );
            print!("{}", mailypoppins::draft_cmd::render_list(&listing));
            eprint!(
                "{}",
                mailypoppins::draft_cmd::render_skipped(&listing.skipped)
            );
        }

        Some(Commands::Validate { selector }) => {
            let account_config = match &selector {
                Some(sel) => account_for_selector(sel, &account_config, &global_config)?,
                None => account_config.clone(),
            };
            let validation: mp_protocol::draft::DraftValidation = draft_call(
                &account_config.name,
                "draft.validate",
                serde_json::json!({"account": account_config.name, "selector": selector}),
            )
            .await?;
            print!(
                "{}",
                mailypoppins::draft_cmd::render_validation(&validation)
            );
            // The daemon refuses nothing about an invalid draft: reporting one
            // is its answer, and the exit code is this process's decision.
            if mailypoppins::draft_cmd::invalid_count(&validation) > 0 {
                mailypoppins::daemon::client::enforce_routing(&command_label);
                std::process::exit(1);
            }
        }

        Some(Commands::MarkApproved { selector }) => {
            let account_config = account_for_selector(&selector, &account_config, &global_config)?;
            routed_mark(&account_config.name, &selector, true).await?;
        }

        Some(Commands::MarkDraft { selector }) => {
            let account_config = account_for_selector(&selector, &account_config, &global_config)?;
            routed_mark(&account_config.name, &selector, false).await?;
        }

        Some(Commands::New { name }) => {
            // The `.md` suffixing rule is the daemon's, because the daemon owns
            // the directory the file lands in.
            let created: mp_protocol::draft::DraftCreated = draft_call(
                &account_config.name,
                "draft.create",
                with_params(
                    serde_json::json!({"account": account_config.name, "name": name}),
                    draft_signature,
                ),
            )
            .await?;
            println!("{} {}", "\u{2713}".green(), created.selector);
        }

        Some(Commands::Path { selector }) => {
            let account_config = account_for_selector(&selector, &account_config, &global_config)?;
            let location: mp_protocol::draft::DraftLocation = draft_call(
                &account_config.name,
                "draft.path",
                serde_json::json!({"account": account_config.name, "selector": selector}),
            )
            .await?;
            println!("{}", location.path);
        }

        Some(Commands::Edit { selector }) => {
            // The daemon names the file and this process runs the editor on it:
            // an editor is a terminal session, which only the client has.
            let account_config = account_for_selector(&selector, &account_config, &global_config)?;
            let location: mp_protocol::draft::DraftLocation = draft_call(
                &account_config.name,
                "draft.path",
                serde_json::json!({"account": account_config.name, "selector": selector}),
            )
            .await?;
            let editor = std::env::var("EDITOR").unwrap_or_else(|_| "hx".to_string());
            let status = std::process::Command::new(&editor)
                .arg(&location.path)
                .status()
                .with_context(|| format!("running {editor}"))?;
            if !status.success() {
                return Err(anyhow!("{editor} exited with {status}"));
            }
            println!("{} {}", "\u{2713}".green(), location.selector);
        }

        Some(Commands::Reply { selector, all, mailbox }) => {
            let account_config = account_for_selector(&selector, &account_config, &global_config)?;
            let created = routed_from_source(
                &account_config.name,
                "draft.reply",
                &selector,
                mailbox.as_deref(),
                all,
                draft_signature,
            )
            .await?;
            println!(
                "{} reply to {}",
                "\u{2713}".green(),
                created.source.map(|s| s.selector).unwrap_or_default()
            );
            println!("{}", created.selector);
        }

        Some(Commands::Forward { selector, mailbox }) => {
            let account_config = account_for_selector(&selector, &account_config, &global_config)?;
            let created = routed_from_source(
                &account_config.name,
                "draft.forward",
                &selector,
                mailbox.as_deref(),
                false,
                draft_signature,
            )
            .await?;
            println!(
                "{} forward of {}",
                "\u{2713}".green(),
                created.source.map(|s| s.selector).unwrap_or_default()
            );
            println!("{}", created.selector);
        }

        // The invitation, the reply it builds and the submission are the
        // daemon's from P4-U14, `ANO-4`'s refusal included; the one line the
        // command prints is still this process's.
        Some(Commands::Invite { action }) => {
            let (selector, mailbox, response) = match action {
                InviteAction::Accept { selector, mailbox } => (selector, mailbox, "accept"),
                InviteAction::Tentative { selector, mailbox } => (selector, mailbox, "tentative"),
                InviteAction::Decline { selector, mailbox } => (selector, mailbox, "decline"),
            };
            routed_rsvp(
                &account_config.name,
                &selector,
                mailbox.as_deref(),
                response,
            )
            .await?;
        }

        Some(Commands::ListMailboxes) => {
            // Which transport answers, the credentials it needs and the session
            // it opens are all the daemon's (P4-U10); what stays here is the
            // wording of the listing.
            routed_list_mailboxes(&account_config).await?;
        }

        Some(Commands::Fetch {
            from,
            to,
            cc,
            subject,
            body,
            since,
            before,
            limit,
            full,
            mailbox,
        }) => {
            // `mp fetch` is a lookup, not an ingest: it prints what the server
            // has and writes nothing. Messages enter the store through
            // `mp sync` (#0037), which is the only path that fetches by UID
            // and can key a row.
            routed_fetch(
                &account_config,
                &mailbox,
                limit,
                FetchFilters {
                    from,
                    to,
                    cc,
                    subject,
                    body,
                    since,
                    before,
                },
                full,
            )
            .await?;
        }

        Some(Commands::Sync { limit, mailbox, dry_run, all_accounts }) => {
            // `--all-accounts` is a loop over the same body rather than a
            // second code path, like `send-approved --all-accounts`: each
            // account resolves its own transport and targets, so the
            // per-account sync is exactly what the single-account form does.
            let accounts: Vec<AccountConfig> = if all_accounts {
                global_config.accounts.clone()
            } else {
                vec![account_config.clone()]
            };
            // An empty config (or a `-A` that named nothing) resolves to
            // `AccountConfig::default()`, whose name is empty: without this the
            // run reports `✗ : <error>` for an account that does not exist
            // (#0071 review).
            if accounts.is_empty() || accounts.iter().all(|a| a.name.is_empty()) {
                return Err(anyhow!("No account to sync (check `mp config show`)"));
            }
            routed_sync(&global_config, &accounts, limit, mailbox.as_deref(), dry_run).await?;
        }

        Some(Commands::Watch { mailbox, timeout }) => {
            // The Graph refusal moved into the daemon with the rest of the
            // account knowledge; the timeout stayed here, because a client's
            // patience is the client's (P4-U10).
            routed_watch(&account_config, &mailbox, timeout).await?;
        }

        Some(Commands::Archive { selector, mailbox }) => {
            // The server move and the row rewrite both belong to the selector's
            // account: resolve it before asking the daemon, so a cross-account
            // selector is answered by its own account's store and credentials.
            let account_config = account_for_selector(&selector, &account_config, &global_config)?;
            let moved = routed_received_mutation(
                &account_config.name,
                "message.archive",
                &selector,
                mailbox.as_deref(),
            )
            .await?;
            println!(
                "{} archived {}",
                "\u{2713}".green(),
                wire_str(&moved["selector"])
            );
            println!(
                "  {} {}",
                "now".dimmed(),
                wire_str(&moved["moved_to"]["selector"])
            );
        }

        Some(Commands::Delete { selector, mailbox, force, sent }) => {
            if sent {
                // The upgrade path (#0073): a version that did not retire a
                // sent draft on send leaves a directory of `status: sent`
                // files with nothing left to do to them. Clear them in one
                // call, file and row alike.
                routed_sweep(&account_config.name).await?;
            } else {
                // `required_unless_present = "sent"` guarantees the selector.
                let selector = selector.expect("clap requires a selector without --sent");
                // A cross-account selector deletes from its own account, so the
                // store and (for received mail) the server credentials must be
                // the selector's, not `-A`'s (the #0073 follow-up bug).
                let account_config =
                    account_for_selector(&selector, &account_config, &global_config)?;
                let deleted = if is_drafts_selector(&selector, mailbox.as_deref())? {
                    // Drafts are local-only: no server op, just the file and
                    // the index row the rescan drops (#0073).
                    routed_discard(&account_config.name, &selector, force).await?
                } else {
                    // The durable queue again (#0039): the daemon commits the
                    // row delete and the owed server delete together, then
                    // drains it. A delete has nothing to roll back (the row is
                    // gone and the server still holds the message), so a
                    // refusal propagates verbatim and the next sync refetches
                    // the UID; the not-found message stays byte-identical.
                    routed_received_mutation(
                        &account_config.name,
                        "message.delete",
                        &selector,
                        mailbox.as_deref(),
                    )
                    .await?
                };
                println!(
                    "{} deleted {}",
                    "\u{2713}".green(),
                    wire_str(&deleted["selector"])
                );
            }
        }

        Some(Commands::Open { selector, mailbox }) => {
            let account_config = account_for_selector(&selector, &account_config, &global_config)?;
            let mut connection = daemon_connection().await;
            let message = routed_attachment_target(
                &mut connection,
                &account_config.name,
                &selector,
                mailbox.as_deref(),
            )
            .await?;
            if message.attachments.is_empty() {
                return Err(anyhow!("{} has no attachments", message.selector));
            }
            // Attachments are blobs; the system opener needs files, so the
            // daemon materialises each part into its own handle directory and
            // the client launches the opener - the one half of this only the
            // user's own session can do (ANO-15). The handles are deliberately
            // not released: the viewer just started is holding the file, and
            // the handle's own lifetime is what ends it.
            for part in 0..message.attachments.len() {
                let (_handle, path, _name) = routed_materialise(
                    &mut connection,
                    &account_config.name,
                    &message.selector,
                    part,
                )
                .await?;
                mailypoppins::parse::open_file_with_system(&path)?;
                println!("{} opened {}", "\u{2713}".green(), path.display());
            }
        }

        Some(Commands::Save { selector, output, mailbox }) => {
            let account_config = account_for_selector(&selector, &account_config, &global_config)?;
            // The destination is the user's, so this process resolves it twice
            // over: the absolute form is what the writes use, because the daemon
            // was started from somewhere else and its cwd means nothing
            // (ANO-15), and the spelling is what the user reads back.
            let spelled = output.unwrap_or_else(|| PathBuf::from("."));
            let dest = mailypoppins::daemon::client::absolutise(&spelled);

            let mut connection = daemon_connection().await;
            let message = routed_attachment_target(
                &mut connection,
                &account_config.name,
                &selector,
                mailbox.as_deref(),
            )
            .await?;
            if message.attachments.is_empty() {
                return Err(anyhow!("{} has no attachments", message.selector));
            }

            std::fs::create_dir_all(&dest)
                .with_context(|| format!("creating {}", dest.display()))?;
            // The daemon hands back the name the sender chose and never renames
            // a part; two parts sent under one name are two handles carrying
            // that one name, and the `_1` rule that makes them two files is
            // applied here, where the names become paths. Within this call
            // only: saving the same message twice writes the same two names
            // rather than growing a copy per run.
            let mut written: Vec<String> = Vec::new();
            for part in 0..message.attachments.len() {
                let (handle, path, name) = routed_materialise(
                    &mut connection,
                    &account_config.name,
                    &message.selector,
                    part,
                )
                .await?;
                let name = mailypoppins::daemon::client::unique_name(name, &written);
                let bytes = std::fs::read(&path)
                    .with_context(|| format!("reading {}", path.display()))?;
                let out = dest.join(&name);
                std::fs::write(&out, &bytes)
                    .with_context(|| format!("writing {}", out.display()))?;
                release_handle(&mut connection, &handle).await;
                written.push(name);
            }
            for name in &written {
                println!("{} {}", "\u{2713}".green(), spelled.join(name).display());
            }
        }

        // The read surface over the store (#0062): both offline, both reusing
        // the queries `store::read` already had. `mp show` is the single-message
        // half, addressed by the same selector grammar every other received
        // command takes, and resolved through `account_for_selector` first so a
        // cross-account selector opens the right store (the #0073 follow-up).
        Some(Commands::Show { selector, mailbox, json }) => {
            let account_config = account_for_selector(&selector, &account_config, &global_config)?;
            let message =
                routed_show(&account_config.name, &selector, mailbox.as_deref()).await?;
            if json {
                println!("{}", mailypoppins::read_cmd::to_json(&message)?);
            } else {
                print!("{}", mailypoppins::read_cmd::render_show(&message));
            }
        }

        Some(Commands::ListMessages { mailbox, limit }) => {
            routed_list_messages(&account_config, mailbox.as_deref(), limit).await?;
        }

        Some(Commands::Search {
            query,
            mailbox,
            from,
            to,
            cc,
            subject,
            body,
            filename,
            has_attachment,
            after,
            before,
            limit,
            full,
            local,
        }) => {
            // One parser, one AST (#0086a): the positional grammar and the
            // flags build the identical query. A malformed query is an error
            // with a caret, never a silent search for less.
            let flags = mailypoppins::search::Flags {
                from,
                to,
                cc,
                subject,
                body,
                filename,
                has_attachment,
                after,
                before,
            };
            let query_ast = mailypoppins::search::from_cli(&query, &flags)
                .map_err(|e| anyhow!("{e}"))?;

            // Resolve mailbox scope: --mailbox flag > in: directive > all.
            let mailbox_name = mailbox.or_else(|| query_ast.in_mailbox.clone());

            if local {
                // The mailbox is resolved here, as it was before the daemon
                // answered the search: the names come from the configuration
                // the client already holds, so an unknown one is refused in the
                // same words and without a round trip.
                let mailbox_key = match mailbox_name.as_deref() {
                    Some(want) => Some(resolve_mailbox_key(&account_config, want)?),
                    None => None,
                };
                let rows = routed_search(
                    &account_config.name,
                    &query,
                    mailbox_key.as_deref(),
                    &flags,
                    limit,
                    full,
                )
                .await?;
                print!(
                    "{}",
                    mailypoppins::read_cmd::render_search(&account_config.name, &query, &rows)
                );
                // An early return leaves the check at the bottom of `main`
                // unrun, so it happens here instead. Every early return out of
                // a routed command owes this line.
                mailypoppins::daemon::client::enforce_routing(&command_label);
                return Ok(());
            }

            if account_config.auth_method == AuthMethod::Graph {
                let graph_config = GraphConfig::load(&account_config)?;
                let client = graph::GraphClient::new_async(&graph_config).await?;
                let mut emails = client
                    .search_messages(&query_ast, mailbox_name.as_deref(), limit)
                    .await?;
                sort_fetched_by_date(&mut emails);
                if emails.is_empty() {
                    println!("{}", "No results found".yellow());
                } else {
                    display_fetched_emails(&emails, full);
                }
            } else {
                let imap_config = ImapConfig::load(&account_config)?;
                // Gmail is the only IMAP server with an attachment key
                // (`X-GM-RAW has:attachment`); a plain server has none, so its
                // has:attachment residue is post-filtered from the store below.
                let host_lc = imap_config.host.to_ascii_lowercase();
                let gmail = host_lc == "imap.gmail.com"
                    || host_lc.ends_with(".gmail.com")
                    || host_lc.ends_with("googlemail.com");
                let (imap_search, attachment_postfilter) = if gmail {
                    (mailypoppins::search::to_gmail_search_command(&query_ast), false)
                } else {
                    let r = mailypoppins::search::to_imap(&query_ast)
                        .map_err(|e| anyhow!("{e}"))?;
                    (r.search, r.attachment_postfilter)
                };
                let msg_id = query_ast.message_id.clone();

                // The target mailboxes: the scoped one, or all the account's.
                let targets: Vec<(String, String)> = match mailbox_name {
                    Some(ref mb) => vec![(mb.clone(), mb.clone())],
                    None => all_configured_mailboxes(&account_config)
                        .iter()
                        .map(|(role, mapping)| (role.to_string(), mapping.server.clone()))
                        .collect(),
                };

                let mut session = imap_client::open_imap_session(&imap_config).await?;
                let per_mb = (limit / targets.len().max(1)).max(5);
                let mut total = 0usize;
                let mut all_emails: Vec<FetchedEmail> = Vec::new();
                for (label, server) in &targets {
                    if total >= limit {
                        break;
                    }
                    let budget = if targets.len() == 1 {
                        limit
                    } else {
                        per_mb.min(limit - total)
                    };
                    match imap_client::search_on_session(
                        &mut session,
                        &imap_search,
                        msg_id.as_deref(),
                        server,
                        Some(budget),
                    )
                    .await
                    {
                        Ok(emails) => {
                            total += emails.len();
                            all_emails.extend(emails);
                        }
                        Err(e) => {
                            eprintln!("{} Search in {} failed: {}", "\u{26a0}".yellow(), label, e);
                        }
                    }
                }
                session.logout().await.ok();

                // Plain-IMAP has:attachment (#0086a, option b): keep only hits
                // the local store marks as carrying an attachment, and say that
                // un-synced mail is not covered.
                if attachment_postfilter {
                    let store = received_store(&account_config.name)?;
                    let with_att = mailypoppins::store::read::message_ids_with_attachments(
                        &store,
                        &account_config.name,
                    )?;
                    all_emails.retain(|e| {
                        e.message_id.as_deref().is_some_and(|m| {
                            with_att.contains(&mailypoppins::store::read::normalize_message_id_key(m))
                        })
                    });
                    eprintln!(
                        "{} has:attachment on plain IMAP is answered from the local store, so \
                         un-synced mail is not covered. Run `mp sync` for full coverage.",
                        "\u{26a0}".yellow()
                    );
                }

                sort_fetched_by_date(&mut all_emails);
                if all_emails.is_empty() {
                    println!("{}", "No results found".yellow());
                } else {
                    display_fetched_emails(&all_emails, full);
                }
            }
        }

        // The index, the ranking and the cache are all the daemon's from
        // P4-U14; the glyphs, the tab-delimited projection and the
        // all-accounts loop stay here.
        Some(Commands::Contacts { action }) => {
            match action {
                ContactsAction::Search { query, parsable, limit, account } => {
                    let acct = account.or_else(|| cli.account.clone());
                    let name = pick_account_named(&global_config, acct.as_deref())?
                        .name
                        .clone();
                    routed_contacts_search(&name, query.as_deref(), parsable, limit).await?;
                }
                ContactsAction::Rebuild { account } => {
                    let acct = account.or_else(|| cli.account.clone());
                    routed_contacts_rebuild(&accounts_for(&global_config, acct.as_deref())?)
                        .await?;
                }
                ContactsAction::Stats { account } => {
                    let acct = account.or_else(|| cli.account.clone());
                    let name = pick_account_named(&global_config, acct.as_deref())?
                        .name
                        .clone();
                    routed_contacts_stats(&name).await?;
                }
            }
        }

        Some(Commands::Calendar { action }) => match action {
            CalendarAction::Rebuild { account } => {
                let acct = account.or_else(|| cli.account.clone());
                routed_calendar_rebuild(&accounts_for(&global_config, acct.as_deref())?).await?;
            }
        },

        Some(Commands::Outbox { action }) => {
            cmd_outbox(&account_config, action).await?;
        }

        Some(Commands::Store { action }) => {
            let StoreAction::Gc {
                dry_run,
                force,
                all_accounts,
            } = action;
            let accounts: Vec<AccountConfig> = if all_accounts {
                global_config.accounts.clone()
            } else {
                vec![account_config.clone()]
            };
            if accounts.is_empty() || accounts.iter().all(|a| a.name.is_empty()) {
                return Err(anyhow!("No account to sweep (check `mp config show`)"));
            }
            for account in &accounts {
                if accounts.len() > 1 {
                    println!("\n{}", format!("\u{2500}\u{2500} {} \u{2500}\u{2500}", account.name).bold());
                }
                routed_store_gc(&account.name, dry_run, force).await?;
            }
        }

        Some(Commands::Cutover { account, dry_run }) => {
            let acct = account.or_else(|| cli.account.clone());
            routed_cutover(&accounts_for(&global_config, acct.as_deref())?, dry_run).await?;
        }

        Some(Commands::DumpKeys { json }) => {
            if json {
                print!("{}", mailypoppins::tui::dump_keys_json());
            } else {
                print!("{}", mailypoppins::tui::dump_keys());
            }
        }

        Some(Commands::DumpMailbox { json: _, mailbox }) => {
            // `--json` is `required = true`, so the format is already pinned.
            let accounts: Vec<mailypoppins::config::AccountConfig> = match cli.account {
                Some(ref name) => global_config
                    .accounts
                    .iter()
                    .filter(|a| a.name == *name)
                    .cloned()
                    .collect(),
                None => global_config.accounts.clone(),
            };
            if accounts.is_empty() {
                return Err(anyhow!("No account to dump (check `mp config show`)"));
            }
            let filter = mailbox.unwrap_or_default();
            let records = routed_dump(&accounts, &filter).await?;
            let stdout = io::stdout();
            let mut out = stdout.lock();
            out.write_all(mailypoppins::dump::to_ndjson(&records).as_bytes())?;
            out.flush()?;
        }

        Some(Commands::Config { action }) => {
            match action {
                // Both wizards ask the daemon where the configuration is and
                // whether there is one *before* they prompt, which is the pair
                // of facts their first line prints and what lets the declined
                // branch prove it routed (P4-U14).
                ConfigAction::Init => {
                    let (path, exists, _) = routed_config_state().await?;
                    cmd_config_init(&path, exists)?;
                    reload_config_quietly().await;
                }
                ConfigAction::Show => routed_config_show().await?,
                ConfigAction::SetPassword { which, account } => {
                    let acct_name = account
                        .or_else(|| cli.account.clone())
                        .or_else(|| global_config.accounts.first().map(|a| a.name.clone()))
                        .unwrap_or_else(|| "main".to_string());
                    routed_set_password(&which, &acct_name).await?;
                }
                ConfigAction::AddAccount => {
                    let (path, exists, config) = routed_config_state().await?;
                    let names: Vec<String> =
                        config.accounts.iter().map(|a| a.name.clone()).collect();
                    cmd_config_add_account(&path, exists, &names)?;
                    reload_config_quietly().await;
                }
                ConfigAction::Oauth2Login { account } => {
                    let acct_name = account.or_else(|| cli.account.clone());
                    routed_oauth2_login(&global_config, acct_name.as_deref()).await?;
                }

                ConfigAction::ResetSecrets => routed_reset_secrets().await?,
                ConfigAction::Path => cmd_config_path(),
            }
        }

        None => {
            if let Some(ref selector) = cli.selector {
                // Preview mode (dry run): a draft selector, never a path. The
                // daemon renders the record, including the body cut-offs, and
                // passes no signature: the body already carries it (#0099).
                let account_config =
                    account_for_selector(selector, &account_config, &global_config)?;
                let preview: mp_protocol::draft::DraftPreview = draft_call(
                    &account_config.name,
                    "draft.preview",
                    serde_json::json!({"account": account_config.name, "selector": selector}),
                )
                .await?;
                print!("{}", mailypoppins::draft_cmd::render_preview(&preview));
            } else {
                // No file, no subcommand -> launch TUI
                mailypoppins::tui::run()?;
            }
        }
        Some(Commands::Account { action }) => match action {
            AccountAction::List => {
                let entries = if cli.daemon {
                    routed_account_list().await
                } else {
                    mailypoppins::daemon::methods::account::entries(&global_config.accounts)
                };
                print!("{}", render_accounts(&entries));
            }
        },

        // Dispatched and exited before the client preamble above, so control
        // never arrives here.
        Some(Commands::Daemon { .. }) => unreachable!("daemon commands exit before dispatch"),
    }

    // The parity hook (P4-U2): with MAILYPOPPINS_DAEMON_REQUIRE set, a command
    // that answered from this process fails here instead of producing output a
    // parity test would credit to the daemon.
    mailypoppins::daemon::client::enforce_routing(&command_label);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::is_drafts_selector;

    /// `mp delete` dispatches on the selector shape, not a second command
    /// (#0073): the reserved `drafts` mailbox segment, or `--mailbox drafts`
    /// beside an elided selector, names a draft; anything else is received.
    #[test]
    fn a_drafts_selector_is_recognised_by_its_mailbox_segment() {
        assert!(is_drafts_selector("mp://tum/drafts/abc123", None).unwrap());
        assert!(is_drafts_selector("drafts/abc123", None).unwrap());
        // The flag names the mailbox beside an elided key.
        assert!(is_drafts_selector("abc123", Some("drafts")).unwrap());
    }

    #[test]
    fn a_received_selector_is_not_a_draft() {
        assert!(!is_drafts_selector("mp://tum/INBOX/msg@example.com", None).unwrap());
        assert!(!is_drafts_selector("mp://tum/Archive/msg@example.com", None).unwrap());
        // A bare key with no drafts flag is a received key by default scope.
        assert!(!is_drafts_selector("msg@example.com", None).unwrap());
    }
}
