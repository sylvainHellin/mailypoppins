use mailypoppins::config::*;
use mailypoppins::config_cmd::*;
use mailypoppins::parse::*;
use mailypoppins::selector::Namespace;
use mailypoppins::types::*;

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand};
use colored::*;
use log::{error, info, warn};
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "mailypoppins")]
#[command(
    about = "A terminal email client: Markdown drafts on disk, received mail in a local store"
)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Draft selector to preview (dry-run mode)
    #[arg(value_name = "SELECTOR")]
    selector: Option<String>,

    /// Account to use
    //
    // Global, because `mp -A proton contacts search` before the subcommand is
    // how it is written. The commands it means nothing for shadow it with a
    // hidden [`NoAccount`] and refuse it ([`refuse_account_where_meaningless`]).
    #[arg(short = 'A', long, global = true)]
    account: Option<String>,
}

/// The signature a draft writer splices into the new draft: `-s/--signature`
/// and `--no-signature`, on the four commands that write one (`mp new`,
/// `mp reply`, `mp forward`, and `mp send --invite`, whose invitation has no
/// draft body to carry it).
#[derive(clap::Args, Clone, Debug, Default)]
struct SignatureFlags {
    /// Signature to use (overrides config default)
    #[arg(short, long)]
    signature: Option<String>,

    /// Skip signature entirely
    #[arg(long)]
    no_signature: bool,
}

/// The global `-A` shadowed on a command that names no account, hidden from
/// its help and refused by [`refuse_account_where_meaningless`], so
/// `mp daemon status -A work` is an error rather than an account nobody reads.
///
/// It shares the id `account` with the global flag, which is what makes clap
/// skip propagating the global one here, and what carries a value written
/// after the subcommand up into [`Cli::account`].
#[derive(clap::Args, Clone, Debug, Default)]
struct NoAccount {
    #[arg(
        short = 'A',
        long = "account",
        id = "account",
        value_name = "ACCOUNT",
        global = true,
        hide = true
    )]
    account: Option<String>,
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
(synced mail only): the run warns on stderr, the result header on stdout says
\"local store only: has:attachment\", and --json sets coverage to \"local\" with
the term in local_terms. filename: is Gmail/Exchange/--local only.

The daemon runs a server search, the same one the TUI and the desktop app run:
each mailbox is asked for its newest --limit matches, those are merged newest
first and cut to --limit, and only the survivors are downloaded. A mailbox the
server refuses does not stop the others; a \"Search in <mailbox> failed\" line
on stderr follows the results.";

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
        /// summary. Requires --to, --start and --subject; a timed event also
        /// needs one of --end/--duration, an all-day event (a bare date
        /// --start) does not.
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
        /// RFC3339 with offset (2026-07-20T14:00:00+02:00, ...Z). A bare date
        /// (2026-12-24) makes an all-day event. Invite mode.
        #[arg(long)]
        start: Option<String>,
        /// Event end, the same kind as --start. For an all-day event it is the
        /// last day, inclusive (--start 2026-12-24 --end 2027-01-08 covers 24 Dec
        /// through 8 Jan); omit it for a single day. A timed event needs this
        /// or --duration.
        #[arg(long)]
        end: Option<String>,
        /// Event duration instead of --end: ISO8601 (PT1H30M) or short (1h30m);
        /// whole days (P3D, 3d) for an all-day event.
        #[arg(long)]
        duration: Option<String>,
        /// Optional event location (invite mode).
        #[arg(long)]
        location: Option<String>,
        /// Optional event description / body (invite mode).
        #[arg(long)]
        description: Option<String>,
        /// Re-send an invitation already sent, as an update: give its UID (the
        /// `UID:` line of its preview or its invite.ics) and a higher
        /// --sequence. Calendar clients replace the event only when the UID is
        /// the same and the SEQUENCE is higher than the one they hold; without
        /// --uid a fresh UID is minted (a new event).
        #[arg(long, value_name = "UID", requires = "invite")]
        uid: Option<String>,
        /// The update's SEQUENCE, higher than the last one sent for this UID
        /// (the first invitation goes out as 0, so a first update is 1).
        /// Needs --uid.
        #[arg(long, value_name = "N", requires = "uid")]
        sequence: Option<u32>,
        /// Signature to use in the invitation (overrides config default)
        #[arg(short, long, requires = "invite")]
        signature: Option<String>,
        /// Skip the signature in the invitation
        #[arg(long, requires = "invite")]
        no_signature: bool,
    },
    /// Send every approved draft of the account
    SendApproved {
        /// Send the approved drafts of every configured account
        #[arg(long, conflicts_with = "account")]
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
        /// Print the listing as JSON on stdout (warnings stay on stderr)
        #[arg(long)]
        json: bool,
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
        #[command(flatten)]
        signature: SignatureFlags,
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
        #[command(flatten)]
        signature: SignatureFlags,
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
        /// Attach the original as an .eml (message/rfc822) instead of its attachments
        #[arg(long)]
        as_attachment: bool,
        #[command(flatten)]
        signature: SignatureFlags,
    },
    /// RSVP to a received calendar invitation (iMIP REPLY over SMTP)
    Invite {
        #[command(subcommand)]
        action: InviteAction,
    },
    /// List every folder on the server with its sync policy and role
    ///
    /// The policy is how the daemon keeps the folder: sync, status-only or
    /// ignore; an ignored folder says why.
    ListMailboxes {
        /// Print the listing as JSON on stdout
        #[arg(long)]
        json: bool,
    },

    /// Sync mailboxes from the server into the local store
    Sync {
        /// Max messages per mailbox
        #[arg(short = 'n', long, default_value = "50")]
        limit: usize,
        /// Mailboxes to sync, by role, label or server name; any folder the
        /// server lists is accepted (default: every folder the account syncs)
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
        /// Mailbox to watch
        #[arg(long, default_value = "INBOX")]
        mailbox: String,
        /// Timeout in seconds (exits with code 2 on timeout)
        #[arg(long)]
        timeout: Option<u64>,
    },
    /// Archive a received email (server + local)
    Archive {
        /// Received selector: mp://<account>/<mailbox>/<message-id>[/<uid>]
        #[arg(value_name = "SELECTOR")]
        selector: String,
        /// Mailbox to resolve the selector in
        #[arg(long)]
        mailbox: Option<String>,
    },
    /// Move a received email to another mailbox (server + local)
    Move {
        /// Received selector: mp://<account>/<mailbox>/<message-id>[/<uid>]
        #[arg(value_name = "SELECTOR")]
        selector: String,
        /// Destination mailbox: role, slug or label (inbox, Junk, archive, ...)
        #[arg(value_name = "MAILBOX")]
        destination: String,
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
        /// Received (mp://<acct>/<mbox>/<id>[/<uid>]) or drafts (mp://<acct>/drafts/<id>) selector; omit with --sent
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
    /// Write a received email's raw RFC 822 message to <date>_<subject>.eml
    Export {
        /// Received selector: mp://<account>/<mailbox>/<message-id>,
        /// <mailbox>/<message-id> or <message-id>
        #[arg(value_name = "SELECTOR")]
        selector: String,
        /// Output directory (default: current directory)
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Mailbox to resolve the selector in
        #[arg(long)]
        mailbox: Option<String>,
    },
    /// Print one received message from the local store, fetched first if missing
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
        /// Max messages per mailbox listed
        #[arg(short = 'n', long, default_value = "20")]
        limit: usize,
        /// Emit one JSON object {account, mailboxes: [{mailbox, label, total,
        /// shown}], messages: [...]} on stdout; each message carries selector
        /// (ending in /<uid> for one of two copies its mailbox holds), mailbox,
        /// message_id, from, to, cc, subject, date (ISO 8601, UTC),
        /// date_header, flags, has_attachments, invite
        #[arg(long)]
        json: bool,
    },
    /// Search emails on the mail server (IMAP or Exchange), or locally with --local
    #[command(long_about = SEARCH_LONG_ABOUT)]
    Search {
        /// Search query. One grammar for every backend: fields from: to: cc:
        /// subject: body: filename:, the flag has:attachment, dates
        /// before:YYYY-MM-DD / after:YYYY-MM-DD (since: aliases after:),
        /// quoted "phrases", OR and (a OR b) groups, plus in: and message-id:.
        /// Combine with the flags below; they build the same query.
        #[arg(default_value = "")]
        query: String,
        /// Mailbox to search: by role, label or server name, any folder the
        /// server lists (default: every folder the account syncs, and on
        /// Exchange every folder; --local: every mailbox the store holds)
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
        /// Max results across every searched mailbox, the newest winning
        /// wherever they sit; a server search cut by it ends with a
        /// "raise -n" line. Exchange stops a search at 1000 results per
        /// mailbox, which a line says instead
        #[arg(short = 'n', long, default_value = "20")]
        limit: usize,
        /// Show full body instead of preview
        #[arg(long)]
        full: bool,
        /// Search the local store's full-text index instead of the server
        /// (offline, ranked, covers every synced mailbox at once)
        #[arg(long)]
        local: bool,
        /// Emit one JSON object {account, query, source: local|server,
        /// coverage: server|local, local_terms: [...], truncated: bool,
        /// messages: [...]} on stdout, messages as in `mp list-messages
        /// --json` plus body (--full) or preview (server hit); coverage is
        /// local when the store answered any term, which local_terms names
        /// (empty under --local, where the store answered the whole query);
        /// truncated is true when more matched than --limit let through;
        /// warnings stay on stderr
        #[arg(long)]
        json: bool,
    },
    /// Manage configuration
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// Contact index operations
    #[command(after_help = "\
The index is a cache per account, `contacts-cache.json` in the account's data
directory, ranked from the From, To, Cc and Bcc headers of the stored mail.
Every sync and every send folds the mail it saw into it. The first search or
stats of an account builds it before answering, and rebuilds it first when
another mp version's extractor built it, so a reinstall never keeps an index
the current rules would not produce; the CLI, the TUI and the desktop app
read it the same way. `mp contacts rebuild` rebuilds it on demand.")]
    Contacts {
        #[command(subcommand)]
        action: ContactsAction,
    },
    /// Manage the signatures drafts carry, and each account's default
    #[command(after_help = "\
A signature is one Markdown file the daemon keeps under the configuration
directory; `mp new -s <name>` and the TUI's and the desktop app's compose
dialogs splice it into a draft. The default is per account: `-A` names the
account, the first configured one otherwise.

Examples:
  mp signature list
  mp signature new work && mp signature edit work
  mp -A tum signature default work")]
    Signature {
        #[command(subcommand)]
        action: SignatureAction,
    },
    /// The calendar: list, view, show and hide calendars, rebuild
    Calendar {
        #[command(subcommand)]
        action: CalendarAction,
    },
    /// One calendar event: show it, answer it
    #[command(after_help = "\
Examples:
  mp event show AAMkADev2 -A evoqs
  mp event respond AAMkADev2 accept --comment \"See you there\" -A evoqs
  mp event respond imip:42 decline -A perso")]
    Event {
        #[command(subcommand)]
        action: EventAction,
    },
    /// Inspect, dry-run and replay the mail hooks of `[[accounts.hooks]]`
    #[command(after_help = "\
The daemon runs a hook's command once for every message that arrives after
the hook was first seen and passes its `match` table. See
docs/daemon-operations.md for the configuration and the JSON on stdin.

Examples:
  mp hooks list
  mp hooks test pi-remote 'mp://assistant/inbox/abc@example.com'
  mp hooks replay pi-remote 'mp://assistant/inbox/abc@example.com'")]
    Hooks {
        #[command(subcommand)]
        action: HooksAction,
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
    /// writes not even the `id:` field. Covers every configured account
    /// unless `-A` names one.
    Cutover {
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
        #[command(flatten)]
        no_account: NoAccount,
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
    /// Print a shell completion script (eval or save under the shell's completion dir)
    #[command(after_help = "\
Examples:
  mp completions zsh > \"${fpath[1]}/_mp\"
  eval \"$(mp completions bash)\"")]
    Completions {
        /// Shell to generate the script for
        #[arg(value_enum)]
        shell: clap_complete::Shell,
        #[command(flatten)]
        no_account: NoAccount,
    },
    /// Inspect the configured accounts
    Account {
        #[command(subcommand)]
        action: AccountAction,
        #[command(flatten)]
        no_account: NoAccount,
    },
    /// Manage the local mailypoppins daemon (run, start, status, stop, restart)
    Daemon {
        #[command(subcommand)]
        action: mailypoppins::daemon::lifecycle::DaemonAction,
        #[command(flatten)]
        no_account: NoAccount,
    },
}

/// `mp account <action>`: what a client may ask about the accounts themselves.
#[derive(Clone, Debug, Subcommand)]
enum AccountAction {
    /// List the configured accounts, their backend and their state
    List {
        /// Answer from the daemon's `account.list` instead of in process.
        ///
        /// The parity oracle of P2-U11: `mp account list` reads the
        /// configuration and probes each store read-only in this process, and
        /// the flag asks the daemon the same question, so the two outputs can
        /// be compared byte for byte. It never falls back: with no daemon to
        /// reach it exits 4. Hidden, and scoped to the one command that reads
        /// it, since every other command reaches the daemon regardless.
        #[arg(long, hide = true)]
        daemon: bool,
    },
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
    List {
        /// Print the listing as JSON on stdout
        #[arg(long)]
        json: bool,
    },
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
        #[arg(long, conflicts_with = "account")]
        all_accounts: bool,
    },
}

/// The mail hooks of `[[accounts.hooks]]` (#0135).
#[derive(Subcommand)]
enum HooksAction {
    /// List the hooks with their cursor and last run (every configured
    /// account unless `-A` names one)
    List {
        /// Print the daemon's answer as JSON
        #[arg(long)]
        json: bool,
    },
    /// Check a hook against a stored message; runs nothing, moves no cursor
    Test {
        /// The hook's name
        hook: String,
        /// The message: an `mp://` selector or a Message-ID
        selector: String,
        /// Narrow the selector to one mailbox
        #[arg(long)]
        mailbox: Option<String>,
        /// Print the verdict and the payload the command would read, as JSON
        #[arg(long)]
        json: bool,
    },
    /// Run a hook for a stored message now, whatever its cursor says (the
    /// `match` table still has to pass)
    Replay {
        /// The hook's name
        hook: String,
        /// The message: an `mp://` selector or a Message-ID
        selector: String,
        /// Narrow the selector to one mailbox
        #[arg(long)]
        mailbox: Option<String>,
    },
}

/// The calendar: the server calendar of an account with a
/// `calendar_backend` and the invitation mails of every account (PERSO-200),
/// and the organizer-side reply fold (#0030).
#[derive(Subcommand)]
enum CalendarAction {
    /// List the calendars: account, name, id, backend, visibility, colour
    ///
    /// Every account unless -A names one. An account with a server calendar
    /// lists the calendars of its last sync; an account holding invitation
    /// mails lists its `imip` calendar too.
    List {
        /// Print the `calendar.list` answer as JSON on stdout
        #[arg(long)]
        json: bool,
    },
    /// Show the events of an agenda, a day or a week
    #[command(after_help = "\
Dates: YYYY-MM-DD, today, tomorrow, yesterday, or +N / -N for N days from
today. The agenda runs from --on (default today) through 14 days later, or
from --from through --to; a day is --on; a week is the ISO week holding --on.
Each event line carries your answer: \u{2713} accepted, ~ tentative, \u{2717} declined,
? not answered, \u{2605} organizer.

Examples:
  mp calendar view
  mp calendar view week --on +7
  mp calendar view day --on tomorrow -A evoqs
  mp calendar view --from 2026-08-01 --to 2026-08-31 --json")]
    View {
        /// The layout
        #[arg(value_enum, default_value_t = ViewLayout::Agenda)]
        layout: ViewLayout,
        /// The day shown, or the first day of the agenda (default today)
        #[arg(long, value_name = "DATE", allow_hyphen_values = true, conflicts_with_all = ["from", "to"])]
        on: Option<String>,
        /// The agenda's first day
        #[arg(long, value_name = "DATE", allow_hyphen_values = true, requires = "to")]
        from: Option<String>,
        /// The agenda's last day, included
        #[arg(
            long,
            value_name = "DATE",
            allow_hyphen_values = true,
            requires = "from"
        )]
        to: Option<String>,
        /// Only these accounts (repeatable; default every account)
        #[arg(
            short = 'A',
            long = "account",
            id = "account",
            value_name = "ACCOUNT",
            global = true,
            action = clap::ArgAction::Append
        )]
        accounts: Vec<String>,
        /// Only these calendar ids, as `mp calendar list` prints them (repeatable)
        #[arg(long = "calendar", value_name = "ID")]
        calendars: Vec<String>,
        /// Include the hidden calendars' events
        #[arg(long)]
        hidden: bool,
        /// The IANA zone to read the times in (default the local zone)
        #[arg(long, value_name = "ZONE")]
        zone: Option<String>,
        /// Print the `calendar.view` answer as JSON on stdout
        #[arg(long)]
        json: bool,
    },
    /// Make a calendar's events visible in the views again
    Show {
        /// The calendar id, as `mp calendar list` prints it
        #[arg(value_name = "CALENDAR")]
        calendar: String,
    },
    /// Hide a calendar's events from the views (a local preference)
    Hide {
        /// The calendar id, as `mp calendar list` prints it
        #[arg(value_name = "CALENDAR")]
        calendar: String,
    },
    /// Report what the stored attendee REPLY emails resolve on the stored
    /// invitations, and resync the server calendar of an account that has
    /// one. The fold writes nothing: attendee statuses are derived from the
    /// `invite.ics` payloads wherever they are displayed, so there is no
    /// cached copy to rebuild. Covers every configured account unless `-A`
    /// names one.
    Rebuild,
}

/// The layouts of `mp calendar view`.
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum ViewLayout {
    /// Day by day, the days with events
    Agenda,
    /// One day
    Day,
    /// The ISO week, Monday to Sunday
    Week,
}

impl From<ViewLayout> for mailypoppins::calendar_cmd::Layout {
    fn from(layout: ViewLayout) -> Self {
        match layout {
            ViewLayout::Agenda => Self::Agenda,
            ViewLayout::Day => Self::Day,
            ViewLayout::Week => Self::Week,
        }
    }
}

/// One event of the calendar (PERSO-200), addressed by the id
/// `mp calendar view --json` prints.
#[derive(Subcommand)]
enum EventAction {
    /// Show one event: when, where, organizer, attendees, your answer
    Show {
        /// The event id: a server id, or imip:<row_id> for an invitation mail's event
        #[arg(value_name = "EVENT")]
        event: String,
        /// Print the `event.get` answer as JSON on stdout
        #[arg(long)]
        json: bool,
    },
    /// Answer an invitation: through the server for a server event, by iMIP
    /// reply for an invitation mail's event
    Respond {
        /// The event id: a server id, or imip:<row_id> for an invitation mail's event
        #[arg(value_name = "EVENT")]
        event: String,
        /// The answer
        #[arg(value_enum)]
        response: EventResponse,
        #[command(flatten)]
        answer: AnswerFlags,
        /// Print the settled `event.respond` result as JSON on stdout
        #[arg(long)]
        json: bool,
    },
}

/// The three answers to an invitation.
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum EventResponse {
    Accept,
    Tentative,
    Decline,
}

impl EventResponse {
    /// The word `event.respond` takes.
    fn word(self) -> &'static str {
        match self {
            EventResponse::Accept => "accept",
            EventResponse::Tentative => "tentative",
            EventResponse::Decline => "decline",
        }
    }

    /// The verb of the reply's subject, `Accepted: <summary>`.
    fn verb(self) -> &'static str {
        match self {
            EventResponse::Accept => "Accepted",
            EventResponse::Tentative => "Tentative",
            EventResponse::Decline => "Declined",
        }
    }
}

/// What an answer carries besides its word, on `mp event respond` and
/// `mp invite`.
#[derive(clap::Args, Clone, Debug, Default)]
struct AnswerFlags {
    /// A comment for the organizer
    #[arg(long, value_name = "TEXT")]
    comment: Option<String>,
    /// Record the answer without telling the organizer (server events only:
    /// an iMIP reply is the answer itself)
    #[arg(long)]
    no_send_response: bool,
}

/// RSVP actions for `mp invite <accept|tentative|decline> <selector>`.
/// Whole-series only (v1); the target is a received message whose store row
/// carries an `invite.ics` blob. The answer is `event.respond`'s: through the
/// server when the account's calendar holds the invitation's event, else the
/// iMIP reply.
#[derive(Subcommand)]
enum InviteAction {
    /// Accept the invitation (PARTSTAT=ACCEPTED)
    Accept(InviteAnswer),
    /// Tentatively accept the invitation (PARTSTAT=TENTATIVE)
    Tentative(InviteAnswer),
    /// Decline the invitation (PARTSTAT=DECLINED)
    Decline(InviteAnswer),
}

/// The invitation `mp invite` answers, and how.
#[derive(clap::Args)]
struct InviteAnswer {
    /// Received selector of the invitation: mp://<account>/<mailbox>/<message-id>
    #[arg(value_name = "SELECTOR")]
    selector: String,
    /// Mailbox to resolve the selector in
    #[arg(long)]
    mailbox: Option<String>,
    #[command(flatten)]
    answer: AnswerFlags,
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
        /// Emit one JSON object {account, query, contacts: [{address,
        /// display_name, sent_to, sent_cc, received}]}, best match first
        #[arg(long, conflicts_with = "parsable")]
        json: bool,
        /// Max number of results
        #[arg(short = 'n', long, default_value = "20")]
        limit: usize,
    },
    /// Rebuild the contact index from the local message store now (every
    /// configured account unless `-A` names one)
    Rebuild,
    /// Show index statistics
    Stats,
}

/// `mp signature <action>`, over the daemon's `signature.*` methods.
#[derive(Subcommand)]
enum SignatureAction {
    /// List the signatures, marking the account's default
    List {
        /// Print the daemon's answer as JSON {account, names, default}
        #[arg(long)]
        json: bool,
    },
    /// Print a signature's content
    Show {
        /// The signature's name
        name: String,
        /// Print the path of its file instead
        #[arg(long)]
        path: bool,
    },
    /// Create an empty signature and print the path of its file
    New {
        /// The signature's name
        name: String,
    },
    /// Open a signature in $EDITOR
    Edit {
        /// The signature's name
        name: String,
    },
    /// Rename a signature
    Rename {
        /// The signature's current name
        old: String,
        /// Its new name
        new: String,
    },
    /// Delete a signature
    Delete {
        /// The signature's name
        name: String,
    },
    /// Print the account's default signature, or set it, or clear it with --none
    Default {
        /// The signature to make the default
        name: Option<String>,
        /// Clear the default, so drafts carry no signature unless one is named
        #[arg(long, conflicts_with = "name")]
        none: bool,
    },
}

#[derive(Subcommand)]
enum ConfigAction {
    /// Interactive setup wizard
    Init {
        #[command(flatten)]
        no_account: NoAccount,
    },
    /// Show current configuration
    Show {
        #[command(flatten)]
        no_account: NoAccount,
    },
    /// Store a password in the active secrets backend (for the first account
    /// unless `-A` names one)
    SetPassword {
        /// Which password to set: "smtp" or "imap"
        which: String,
    },
    /// Wipe the encrypted secrets file (and Graph token caches) and re-prompt
    /// for credentials. Use this after a Time Machine restore to a new
    /// machine, or whenever the secrets file can no longer be decrypted.
    ResetSecrets {
        #[command(flatten)]
        no_account: NoAccount,
    },
    /// Add a new account to the existing config
    AddAccount {
        #[command(flatten)]
        no_account: NoAccount,
    },
    /// Sign a Microsoft 365 (Graph) account in with the OAuth2 device code
    /// flow and cache its token (for the first Graph account unless `-A`
    /// names one)
    Oauth2Login,
    /// Print config file path
    Path {
        #[command(flatten)]
        no_account: NoAccount,
    },
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
    uid: Option<String>,
    sequence: Option<u32>,
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
    if let OutboxAction::List { json: true } = action {
        println!("{}", serde_json::to_string_pretty(&listing)?);
        return Ok(());
    }
    if !listing.ever_used {
        print_lines(&mp_client::format::outbox_cli_lines(&listing));
        return Ok(());
    }

    match action {
        OutboxAction::List { .. } => print_lines(&mp_client::format::outbox_cli_lines(&listing)),
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

/// What a command that hands back a selector or a path prints: `decorated`
/// (`✓ <selector>`, possibly over several lines) on a terminal, and only the
/// bare `value` when stdout is a pipe or a file (#0144).
///
/// The bare form is what makes `sel=$(mp new x) && mp path "$sel"` work: a
/// captured check mark is not part of any selector, and `mp path` refused the
/// whole string as one with an empty segment.
fn hand_back(decorated: &str, value: &str) {
    println!(
        "{}",
        handed_back(std::io::stdout().is_terminal(), decorated, value)
    );
}

/// The text [`hand_back`] prints, with the terminal check made by the caller.
fn handed_back<'a>(terminal: bool, decorated: &'a str, value: &'a str) -> &'a str {
    if terminal {
        decorated
    } else {
        value
    }
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
        sequence: args.sequence,
    };
    let plan = mailypoppins::invite::plan_invite(account_config, &request, args.uid.as_deref())?;
    let spec = &plan.spec;
    // Connected before the preview: an invitation the user declines is still a
    // run that answered from the daemon, and `ANO-4` is refused above this line
    // so a Graph account costs no session.
    let mut connection = daemon_connection().await;

    println!("{}", "--- Invite Preview ---".bold());
    println!("  {} {}", "Summary:".yellow(), plan.subject);
    println!("  {} {}", "Organizer:".green(), spec.organizer);
    println!("  {} {}", "Attendees:".green(), spec.attendees.join(", "));
    println!("  {} {}", "When:".blue(), spec.span.describe());
    if let Some(loc) = spec.location.as_deref() {
        println!("  {} {}", "Location:".blue(), loc);
    }
    println!("  {} {}", "UID:".dimmed(), spec.uid);
    if args.uid.is_some() {
        println!(
            "  {} {} (update of the invitation with this UID)",
            "Sequence:".dimmed(),
            spec.sequence
        );
    }
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
    // Only an update names a sequence, so a new invitation's call is the one a
    // daemon from before `sequence` existed also takes.
    if let Some(sequence) = args.sequence {
        params["sequence"] = serde_json::json!(sequence);
    }
    params = with_params(params, signature);

    daemon_call(&mut connection, "state.bootstrap", serde_json::json!({})).await;
    let started = daemon_try_call(&mut connection, "send.invite", params)
        .await
        .map_err(|e| refusal(&account_config.name, e))?;
    let operation = wire_str(&started["operation_id"]).to_string();
    let outcome: mp_protocol::send::SendOutcome =
        match await_operation(&mut connection, &operation, |_| {}).await {
            Settled::Done(result) => {
                serde_json::from_value(result).context("reading the daemon's send.invite answer")?
            }
            Settled::Failed(message) => return Err(anyhow!("{message}")),
        };

    for recipient in &outcome.recipients {
        if recipient.delivered {
            println!(
                "  {} {} ({})",
                "\u{2713}".green(),
                recipient.address,
                recipient.role
            );
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
    let query = mailypoppins::selector::parse_in(selector, Namespace::Drafts, account, None)?;
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
        println!(
            "  {} {}",
            "To:".green(),
            preview.to.as_deref().unwrap_or("(none)")
        );
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
            println!(
                "{} (sent but failed to retire draft: {error})",
                "\u{26a0}".yellow()
            );
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
    // A Graph account has no SMTP transport by design, so there is nothing to
    // warn about.
    if account_config.auth_method != AuthMethod::Graph {
        if let Err(e) = SmtpConfig::load(account_config) {
            eprintln!("{} Could not load SMTP config: {}", "\u{26a0}".yellow(), e);
        }
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

    println!(
        "\n{} approved email(s) found:\n",
        approved.len().to_string().bold()
    );
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

/// The refusal of a read against an account with nothing to read, which a
/// routed command raises when the daemon answers `account_not_ready`.
fn no_received_store(account: &str) -> anyhow::Error {
    anyhow!(
        "{account} has no local store yet, so no received mail can be addressed; \
         run `mp sync` first"
    )
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

/// The account's mailboxes as the daemon's `mailbox.list` answers them: the
/// sidebar hierarchy every client reads, with every folder the server lists
/// (#0141), so a `--mailbox` the config never names still resolves.
async fn daemon_mailboxes(
    connection: &mut mp_client::Connection,
    account: &str,
) -> Result<Vec<mp_protocol::state::MailboxRow>> {
    let answer = daemon_try_call(
        connection,
        "mailbox.list",
        mp_client::queries::mailbox_list_params(account),
    )
    .await
    .map_err(|e| refusal(account, e))?;
    Ok(mp_client::queries::decode_mailbox_rows(&answer))
}

/// The mailboxes a listing covers: the one `--mailbox` names, or every mailbox
/// of the account but drafts.
///
/// A name matches a store key, then a sidebar label, then a server name,
/// case-insensitively ([`mp_protocol::state::pick_mailbox`], as the daemon
/// resolves it), and an unknown one is an error naming what it could have been
/// rather than an empty result.
fn select_mailboxes(
    account: &str,
    rows: Vec<mp_protocol::state::MailboxRow>,
    mailbox: Option<&str>,
) -> Result<Vec<mp_protocol::state::MailboxRow>> {
    let mailboxes: Vec<_> = rows
        .into_iter()
        .filter(|m| m.slug != mailypoppins::selector::DRAFTS_MAILBOX)
        .collect();
    let Some(want) = mailbox else {
        return Ok(mailboxes);
    };
    if let Some(hit) =
        mp_protocol::state::pick_mailbox(&mailboxes, want, mp_protocol::state::MailboxRow::names)
    {
        return Ok(vec![hit.clone()]);
    }
    let known = mailboxes
        .iter()
        .map(|m| m.slug.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    Err(anyhow!(
        "'{want}' is not a mailbox of {account} (known: {known})"
    ))
}

/// How long a routed command waits for the daemon, per call; the same budget as
/// one connect-and-handshake ([`mailypoppins::daemon::client::CONNECT_TIMEOUT`]),
/// which bounds the session setup rather than a call on it.
const DAEMON_TIMEOUT: std::time::Duration = mailypoppins::daemon::client::CONNECT_TIMEOUT;

/// How long a routed *mutation* waits for the daemon.
///
/// Ten seconds is right for a store read and wrong for `message.archive`: the
/// daemon commits the row change and drains the owed server op before it
/// answers (#0039), so the call spans a round trip to the mail server. A client
/// that gave up at ten seconds would exit 4 over work the daemon went on to
/// finish, which is the one answer a mutation may never give.
const DAEMON_MUTATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// How long a routed *server query* waits for the daemon: `mp list-mailboxes`
/// and `mp search`, whose work is a session on the mail server rather than a
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
    match connection.call_within(method, params, budget).await {
        Ok(result) => Ok(result),
        Err(mp_client::ClientError::Rpc(error)) => Err(error),
        Err(e) => unanswered(method, e),
    }
}

/// The exit-4 diagnostic for a call that got no answer at all, as opposed to a
/// refusal: `ClientError::Timeout` renders as "<method> went unanswered for
/// Ns", the sentence this path has always printed, and anything else is the
/// transport's own words after the method.
fn unanswered(method: &str, error: mp_client::ClientError) -> ! {
    match error {
        e @ mp_client::ClientError::Timeout { .. } => daemon_unavailable(&e.to_string()),
        e => daemon_unavailable(&format!("{method}: {e}")),
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

/// `mp account list --daemon`: the daemon's answer, rendered locally.
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
/// resolved through [`select_mailboxes`] over the daemon's `mailbox.list`, so
/// every folder the server lists is reachable (#0141) and an unknown one is
/// refused in the words it has always been refused in.
async fn routed_list_messages(
    account: &AccountConfig,
    mailbox: Option<&str>,
    limit: usize,
    json: bool,
) -> Result<()> {
    let mut connection = daemon_connection().await;
    let rows = daemon_mailboxes(&mut connection, &account.name).await?;
    let selected = select_mailboxes(&account.name, rows, mailbox)?;
    let mut groups = Vec::new();
    let mut listing = mailypoppins::read_cmd::MessageListingJson {
        account: account.name.clone(),
        mailboxes: Vec::new(),
        messages: Vec::new(),
    };
    for info in selected {
        let result = daemon_try_call(
            &mut connection,
            "message.list",
            serde_json::json!({
                "account": account.name,
                "mailbox": info.slug,
                "limit": limit,
            }),
        )
        .await
        .map_err(|e| refusal(&account.name, e))?;
        let total = result["total"].as_u64().unwrap_or_default() as usize;
        if json {
            let messages = result["messages"].as_array().cloned().unwrap_or_default();
            listing
                .mailboxes
                .push(mailypoppins::read_cmd::ListedMailbox {
                    mailbox: info.slug.clone(),
                    label: info.label.clone(),
                    total,
                    shown: messages.len(),
                });
            listing.messages.extend(messages.iter().map(|message| {
                mailypoppins::read_cmd::ListedMessage::from_wire(
                    &account.name,
                    &info.slug,
                    &mp_client::queries::row_from_wire(message),
                    None,
                )
            }));
            continue;
        }
        let rows = result["messages"]
            .as_array()
            .map(|messages| {
                messages
                    .iter()
                    .map(|message| row_from_wire(&account.name, message, &info.slug))
                    .collect()
            })
            .unwrap_or_default();
        groups.push((info.label.clone(), total, rows));
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&listing)?);
        return Ok(());
    }
    print!(
        "{}",
        mailypoppins::read_cmd::render_list(&account.name, &groups)
    );
    Ok(())
}

/// One `message.list` entry as the row the renderer takes, with the selector
/// the daemon rendered for it (PERSO-244).
///
/// Decoded by `mp_client::queries::row_from_wire`, the one wire-row decoder
/// every client shares, and mapped onto the store row `render_list` takes.
/// Everything a listing prints comes off the wire, `date_display` included, so
/// the routed path opens no store of its own. The fields the mapping leaves
/// empty are the ones nothing in a listing reads: the store id, the other
/// recipients, the body blob, the thread. `flags` is rebuilt as the token
/// string the store holds, so `MessageRow::flags` parses it back into the same
/// three bits; the star is left off, as it always was on this path.
fn row_from_wire(
    account: &str,
    message: &serde_json::Value,
    mailbox: &str,
) -> mailypoppins::read_cmd::SelectedRow {
    selected_row(account, mp_client::queries::row_from_wire(message), mailbox)
}

/// [`store_row`], beside the selector the daemon rendered for the row: the
/// one that names its copy when its mailbox holds a twin (PERSO-244).
fn selected_row(
    account: &str,
    row: mp_protocol::listing::MessageListRow,
    mailbox: &str,
) -> mailypoppins::read_cmd::SelectedRow {
    let selector = mailypoppins::read_cmd::wire_selector(account, mailbox, &row);
    (selector, store_row(row, mailbox))
}

/// [`row_from_wire`]'s mapping, from a row already decoded.
fn store_row(
    row: mp_protocol::listing::MessageListRow,
    mailbox: &str,
) -> mailypoppins::store::read::MessageRow {
    let text = |value: String| Some(value).filter(|value| !value.is_empty());
    mailypoppins::store::read::MessageRow {
        id: 0,
        mailbox: mailbox.to_string(),
        uid: row.uid,
        message_id: row.message_id,
        from: text(row.from),
        to: None,
        cc: None,
        reply_to: None,
        bcc: None,
        subject: text(row.subject),
        date_display: text(row.date_display),
        flags: Some(
            MessageFlags {
                seen: row.flags.seen,
                answered: row.flags.answered,
                forwarded: row.flags.forwarded,
                flagged: false,
            }
            .to_flag_string(),
        ),
        has_attachments: row.has_attachments,
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
    account: &AccountConfig,
    selector: &str,
    mailbox: Option<&str>,
) -> Result<mailypoppins::read_cmd::ShownMessage> {
    let mut connection = daemon_connection().await;
    let result = message_get_or_fetch(&mut connection, account, selector, mailbox, true).await?;
    serde_json::from_value(result).context("reading the daemon's message.get answer")
}

/// `message.get` for a selector, fetching the message first when the store
/// lacks it ([`fetch_on_miss`]).
async fn message_get_or_fetch(
    connection: &mut mp_client::Connection,
    account_config: &AccountConfig,
    selector: &str,
    mailbox: Option<&str>,
    body: bool,
) -> Result<serde_json::Value> {
    let account = account_config.name.as_str();
    fetch_on_miss(
        account_config,
        selector,
        mailbox,
        StandIn::Allowed,
        async |selector: &str, mailbox: Option<&str>| {
            let mut params = serde_json::json!({"account": account, "selector": selector});
            if let Some(mailbox) = mailbox {
                params["mailbox"] = serde_json::json!(mailbox);
            }
            if !body {
                params["body"] = serde_json::json!(false);
            }
            daemon_try_call(connection, "message.get", params)
                .await
                .map_err(|e| refusal(account, e))
        },
    )
    .await
}

/// Whether a command may act on the copy the store holds in another mailbox
/// than the one its selector names.
///
/// A read, a reply, a forward and an RSVP may: the copy is the same message
/// by `Message-ID`, so what they read or quote is the same. An archive, a move
/// and a delete may not, because the Inbox and Sent copies of a message sent to
/// oneself are two messages on the server, and moving or deleting the other
/// one is not what the selector asked for.
#[derive(Clone, Copy)]
enum StandIn {
    Allowed,
    Refused { verb: &'static str },
}

/// Run a command on a received selector, and when the store holds no row for
/// a selector that names its mailbox, `message.fetch` the message from the
/// server and run the command once more on the row the fetch settled on
/// (PERSO-155).
///
/// `mp search` without `--local` lists what the server holds, so the
/// selectors it prints include messages no sync stored (the reported case was
/// a sent message eleven months old), and every command that takes a received
/// selector refused exactly those with "no match". A miss is recognised by
/// the daemon's own sentence for it, minted by the same [`selector::not_found`]
/// the resolver raises, so any other refusal (an ambiguity, a malformed
/// selector, a message that is no invitation) is reported unchanged and
/// fetches nothing. A selector with no mailbox fetches nothing either: the
/// server has to be told which mailbox to search. One that names a mailbox is
/// handed to `message.fetch` whatever the mailbox, and a name the server does
/// not list is refused there, in the daemon's words (#0141).
///
/// The retry names the selector the fetch settled on, which is a copy the
/// store already held in another mailbox when there was one (`already_present`,
/// found by `Message-ID` with no server session); [`StandIn`] says whether the
/// command may act on that copy. The stderr notice says which of the two
/// happened, a download from the server or a copy the store holds elsewhere.
///
/// [`selector::not_found`]: mailypoppins::selector::not_found
async fn fetch_on_miss<T>(
    account_config: &AccountConfig,
    selector: &str,
    mailbox: Option<&str>,
    stand_in: StandIn,
    mut run: impl AsyncFnMut(&str, Option<&str>) -> Result<T>,
) -> Result<T> {
    let refused = match run(selector, mailbox).await {
        Ok(answer) => return Ok(answer),
        Err(refused) => refused,
    };
    let Some((mailbox_key, message_id)) =
        missing_from_store(account_config, selector, mailbox, &refused.to_string())
    else {
        return Err(refused);
    };

    // Which mailboxes a fetch may file a row under is the daemon's call, made
    // from the server's folder list (#0141): a hit from a folder the sidebar
    // leaves out (Proton's All Mail) is fetchable, as its `.eml` export and
    // its forward already are. A refusal carries the daemon's own reason,
    // whichever daemon answers: an older one that only knew the configured
    // mailboxes says so in its own words.
    let account = account_config.name.as_str();
    let mut session = operation_session().await;
    let fetched = match session
        .run_operation(
            "message.fetch",
            serde_json::json!({
                "account": account,
                "mailbox": mailbox_key,
                "message_id": message_id,
            }),
            DAEMON_TIMEOUT,
            |_| {},
        )
        .await
    {
        Ok(Settled::Done(result)) => result,
        Ok(Settled::Failed(message)) => {
            bail!("{refused}, and fetching it from the server failed: {message}")
        }
        Err(mp_client::ClientError::Rpc(error))
            if error.code != mp_protocol::ErrorCode::AccountNotReady.code() =>
        {
            bail!(
                "{refused}, and the daemon will not fetch it: {}",
                error.message
            )
        }
        Err(mp_client::ClientError::Rpc(error)) => return Err(refusal(account, error)),
        Err(mp_client::ClientError::Closed) => {
            daemon_unavailable("the daemon closed the connection with an operation still running")
        }
        Err(e) => unanswered("message.fetch", e),
    };
    let settled = wire_str(&fetched["selector"]).to_string();
    if fetched["already_present"].as_bool() == Some(true) {
        let held_in = wire_str(&fetched["mailbox"]);
        if let StandIn::Refused { verb } = stand_in {
            bail!(
                "{refused}; the store holds that message in {held_in} as {settled}, \
                 so {verb} that selector to act on that copy"
            );
        }
        eprintln!(
            "{} {} is not in the local store under that mailbox; the store holds it in {}",
            "\u{2139}".blue(),
            selector,
            held_in
        );
    } else {
        eprintln!(
            "{} {} was not in the local store; fetched it from the server",
            "\u{2139}".blue(),
            selector
        );
    }
    run(&settled, None).await
}

/// The mailbox and the `Message-ID` to fetch when `refused` is the store's
/// "no match" for a received `selector` that names its mailbox, else `None`.
///
/// The `Message-ID` goes back in angle brackets, the form a server and a sync
/// both carry: Graph keys the row's synthetic uid off that exact string, so the
/// bare selector key would land a row the next sync duplicates.
fn missing_from_store(
    account: &AccountConfig,
    selector: &str,
    mailbox: Option<&str>,
    refused: &str,
) -> Option<(String, String)> {
    let query =
        mailypoppins::selector::parse_in(selector, Namespace::Received, &account.name, mailbox)
            .ok()?;
    // A selector ending in a UID names one copy the store listed (PERSO-239):
    // a fetch by Message-ID cannot promise that copy, so its miss stands.
    if query.uid.is_some() || refused != mailypoppins::selector::not_found(&query).to_string() {
        return None;
    }
    // The selector's mailbox segment as typed: `message.fetch` resolves it on
    // the daemon, against the same catalogue a sync does (#0141).
    let mailbox = query.mailbox.clone()?;
    let key = mailypoppins::selector::message_key(&query.key);
    Some((mailbox, format!("<{key}>")))
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

/// `mp search --local`: the ranked hits of the daemon's index read, which the
/// text listing maps onto store rows and `--json` prints as they came.
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
) -> Result<Vec<mp_client::queries::SearchHit>> {
    let mut params = search_params(account, query, flags, limit);
    params["body"] = serde_json::json!(full);
    if let Some(mailbox) = mailbox {
        params["mailbox"] = serde_json::json!(mailbox);
    }

    let mut connection = daemon_connection().await;
    let result = daemon_try_call(&mut connection, "message.search", params)
        .await
        .map_err(|e| refusal(account, e))?;
    Ok(mp_client::queries::decode_search_hits(&result))
}

/// The parameters `message.search` and `message.search_server` share: the
/// query as typed, `mp search`'s flags under their wire names, and the limit.
///
/// A flag the user did not give is left out rather than sent empty or
/// `false`: a daemon older than the field filters on `message.search_server`
/// refuses a key it does not know, so an `mp search` with no filter keeps
/// working against it until it restarts.
fn search_params(
    account: &str,
    query: &str,
    flags: &mailypoppins::search::Flags,
    limit: usize,
) -> serde_json::Value {
    let mut params = serde_json::json!({
        "account": account,
        "query": query,
        "limit": limit,
    });
    if flags.has_attachment {
        params["has_attachment"] = serde_json::json!(true);
    }
    for (key, value) in [
        ("from", &flags.from),
        ("to", &flags.to),
        ("cc", &flags.cc),
        ("subject", &flags.subject),
        ("body_query", &flags.body),
        ("filename", &flags.filename),
        ("after", &flags.after),
        ("before", &flags.before),
    ] {
        if let Some(value) = value {
            params[key] = serde_json::json!(value);
        }
    }
    params
}

/// `message.search_server`'s parameters: [`search_params`], and the one
/// mailbox `--mailbox` names. Without it no `mailboxes` key travels, so the
/// daemon runs its account-wide search (one query across every folder on
/// Graph), the request the TUI overlay and the desktop search bar send for
/// the same scope.
fn server_search_params(
    account: &str,
    query: &str,
    mailbox: Option<&str>,
    flags: &mailypoppins::search::Flags,
    limit: usize,
) -> serde_json::Value {
    let mut params = search_params(account, query, flags, limit);
    if let Some(mailbox) = mailbox {
        params["mailboxes"] = serde_json::json!([mailbox]);
    }
    params
}

/// The event kind one hit of a `message.search_server` streams as.
const KIND_SERVER_SEARCH_HIT: &str = "message.server_hit";

/// `mp search` without `--local`: the daemon's `message.search_server`, which
/// the TUI overlay and the desktop search bar call too (`LST-06`, PERSO-175).
///
/// `--limit` is the number of hits the whole search answers with: the daemon
/// asks every mailbox for its newest `limit`, merges them newest first, cuts
/// them to `limit` and downloads only those. The hits stream as events and
/// are listed newest first once the search settles; what the settle says
/// (the store-coverage warning, the cut, the mailboxes the server refused)
/// is printed around them by [`mailypoppins::read_cmd::server_search_lines`].
///
/// `--mailbox` names any folder the server lists; without it the daemon
/// reads the query's own `in:`, and without that searches every folder the
/// account syncs on IMAP (#0141) and every folder at once on Graph, each hit
/// listed under its folder.
async fn routed_server_search(
    account_config: &AccountConfig,
    query: &str,
    mailbox: Option<&str>,
    flags: &mailypoppins::search::Flags,
    limit: usize,
    full: bool,
    json: bool,
) -> Result<()> {
    let account = account_config.name.as_str();
    let params = server_search_params(account, query, mailbox, flags, limit);

    let mut connection = daemon_connection().await;
    // What a hit's mailbox label resolves to for its selector; an account
    // with no store yet has no sidebar to resolve against, and its hits are
    // listed without one.
    let rows = if json {
        daemon_mailboxes(&mut connection, account)
            .await
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    // A client that follows its own operation has to be a subscriber first:
    // the hits and the finish reach bootstrapped connections only.
    daemon_call(&mut connection, "state.bootstrap", serde_json::json!({})).await;
    // The daemon's words: a server search refuses an account with no server
    // as a sync of it does, which is not the store-less read `refusal` names.
    let started = daemon_try_call(&mut connection, "message.search_server", params)
        .await
        .map_err(|e| anyhow!("{}", e.message))?;

    let id = wire_str(&started["operation_id"]).to_string();
    let mut hits: Vec<mp_protocol::listing::ServerSearchHit> = Vec::new();
    let settled = connection
        .await_operation_events(&id, |kind, payload| {
            if kind != KIND_SERVER_SEARCH_HIT {
                return;
            }
            match serde_json::from_value(payload["hit"].clone()) {
                Ok(hit) => hits.push(hit),
                Err(e) => warn!("[search] a message.server_hit did not decode: {e}"),
            }
        })
        .await
        .unwrap_or_else(|_| {
            daemon_unavailable("the daemon closed the connection with a search still running")
        });
    let settled: mp_protocol::listing::ServerSearchSettled = match settled {
        Settled::Done(result) => serde_json::from_value(result)
            .context("reading the daemon's message.search_server settle")?,
        Settled::Failed(message) => bail!("{message}"),
    };

    let hits = mailypoppins::read_cmd::server_hits_newest_first(hits);
    let lines = mailypoppins::read_cmd::server_search_lines(&settled, &hits, json);
    for line in &lines.before {
        eprintln!("{} {line}", "\u{26a0}".yellow());
    }
    if json {
        let results = mailypoppins::read_cmd::SearchResultsJson::server(
            account,
            query,
            &settled.local_terms,
            settled.truncated,
            hits.iter()
                .map(|hit| {
                    // The hit names the mailbox by its sidebar label; the
                    // selector wants the store's id, which every folder the
                    // sidebar lists has since #0141.
                    let key = select_mailboxes(account, rows.clone(), Some(&hit.mailbox))
                        .ok()
                        .and_then(|hit| hit.into_iter().next())
                        .map(|row| row.slug);
                    mailypoppins::read_cmd::ListedMessage::from_server_hit(
                        account,
                        key.as_deref(),
                        hit,
                        full,
                    )
                })
                .collect(),
        );
        println!("{}", serde_json::to_string_pretty(&results)?);
    } else if hits.is_empty() {
        println!(
            "{}",
            mailypoppins::read_cmd::no_server_results(&settled).yellow()
        );
    } else {
        let emails: Vec<FetchedEmail> = hits
            .iter()
            .map(mailypoppins::read_cmd::fetched_from_hit)
            .collect();
        display_fetched_emails_with_note(&emails, full, settled.local_store_note().as_deref());
    }
    if !json {
        if let Some(trailer) = &lines.trailer {
            println!("{trailer}");
        }
    }
    for line in &lines.after {
        eprintln!("{} {line}", "\u{26a0}".yellow());
    }
    Ok(())
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
/// the daemon splices the signature into the body, from the signatures
/// directory and the account's default.
fn signature_params(no_signature: bool, signature: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "no_signature": no_signature,
        "signature": signature,
    })
}

impl SignatureFlags {
    /// The flags as the `draft.*` writers take them.
    fn params(&self) -> serde_json::Value {
        signature_params(self.no_signature, self.signature.as_deref())
    }
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
        true => (
            "draft.approve",
            "approved",
            "is already approved",
            "approved",
        ),
        false => ("draft.demote", "draft", "is already a draft", "demoted"),
    };
    if location.status == target {
        hand_back(
            &format!("{} {} {already}", "\u{2139}".blue(), location.selector),
            &location.selector,
        );
        return Ok(());
    }
    daemon_try_call(
        &mut connection,
        method,
        serde_json::json!({"account": account, "id": location.id}),
    )
    .await
    .map_err(|e| refusal(account, e))?;
    hand_back(
        &format!("{} {done} {}", "\u{2713}".green(), location.selector),
        &location.selector,
    );
    Ok(())
}

/// `mp reply` and `mp forward`: the draft the daemon built from a stored
/// message, and the message it names.
///
/// `as_attachment` is `mp forward --as-attachment` (PERSO-174): the daemon
/// attaches the source's `.eml`, which for a Graph row is a round trip to the
/// server, so that call waits as long as a server query does.
async fn routed_from_source(
    account: &str,
    method: &str,
    selector: &str,
    mailbox: Option<&str>,
    all: bool,
    as_attachment: bool,
    signature: serde_json::Value,
) -> Result<mp_protocol::draft::DraftCreated> {
    let mut source = serde_json::json!({"selector": selector});
    if let Some(mailbox) = mailbox {
        source["mailbox"] = serde_json::json!(mailbox);
    }
    let mut params = with_params(
        serde_json::json!({"account": account, "source": source, "all": all}),
        signature,
    );
    if !as_attachment {
        return draft_call(account, method, params).await;
    }
    params["as_attachment"] = serde_json::json!(true);
    let mut connection = daemon_connection().await;
    let result = daemon_try_call_within(&mut connection, method, params, DAEMON_SERVER_TIMEOUT)
        .await
        .map_err(|e| refusal(account, e))?;
    serde_json::from_value(result).with_context(|| format!("reading the daemon's {method} answer"))
}

// ---------------------------------------------------------------------------
// The message-mutation slice, routed (P4-U8)
// ---------------------------------------------------------------------------

/// A string field of a daemon answer, which the shape says is there.
fn wire_str(value: &serde_json::Value) -> &str {
    value.as_str().unwrap_or_default()
}

/// `mp archive`, `mp move` and `mp delete` of received mail.
///
/// One call: the daemon resolves the selector, refuses in the command's own
/// words when it cannot, loads the account's credentials *before* it touches
/// the store, and then commits the row change and drains the op it owes the
/// server in one go (#0039). The synchronous UX is preserved because the
/// answer is the settled outcome rather than an acknowledgement.
///
/// `extra` carries the parameters one method needs beyond the address, which
/// is `destination` for `message.move`.
async fn routed_received_mutation(
    account: &str,
    method: &str,
    selector: &str,
    mailbox: Option<&str>,
    extra: &[(&str, &str)],
) -> Result<serde_json::Value> {
    let mut params = serde_json::json!({"account": account, "selector": selector});
    if let Some(mailbox) = mailbox {
        params["mailbox"] = serde_json::json!(mailbox);
    }
    for (key, value) in extra {
        params[*key] = serde_json::json!(value);
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

/// The message `mp open`, `mp save` and `mp export` were pointed at.
///
/// One `message.get` with no body: it resolves the selector, so the client
/// learns the canonical selector its refusals and its materialisation calls are
/// phrased in, and the attachment list whose length is how many parts there are
/// to ask for.
async fn routed_attachment_target(
    connection: &mut mp_client::Connection,
    account: &AccountConfig,
    selector: &str,
    mailbox: Option<&str>,
) -> Result<mailypoppins::read_cmd::ShownMessage> {
    let result = message_get_or_fetch(connection, account, selector, mailbox, false).await?;
    serde_json::from_value(result).context("reading the daemon's message.get answer")
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

/// The message's raw RFC 822 bytes, materialised by the daemon as
/// `<date>_<subject>.eml` (PERSO-174): the handle, the file and its name.
///
/// The server timeout rather than the store one, because a row with no stored
/// raw message (every Graph row) is answered by a round trip to the server.
async fn routed_export(
    connection: &mut mp_client::Connection,
    account: &str,
    selector: &str,
) -> Result<(String, PathBuf, String)> {
    let result = daemon_try_call_within(
        connection,
        "message.materialise_eml",
        serde_json::json!({"account": account, "selector": selector}),
        DAEMON_SERVER_TIMEOUT,
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
                    if known.is_empty() {
                        "none"
                    } else {
                        known.as_str()
                    }
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

// ---------------------------------------------------------------------------
// The sync/watch slice, routed (P4-U10)
// ---------------------------------------------------------------------------

/// The only mailbox the daemon's watcher watches (PERSO-184).
const WATCHED_MAILBOX: &str = mailypoppins::daemon::methods::sync::WATCHED_MAILBOX;

/// What one operation settled as.
use mp_client::operation::Settled;

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
/// way: `mp_client`'s [`Connection::await_operation`](mp_client::Connection::await_operation),
/// with a daemon that goes away first ending the run with exit 4.
///
/// There is no budget: a full sync takes as long as the mailbox does, exactly
/// as it did in process.
async fn await_operation(
    connection: &mut mp_client::Connection,
    id: &str,
    on_progress: impl FnMut(&serde_json::Value),
) -> Settled {
    match connection.await_operation(id, on_progress).await {
        Ok(settled) => settled,
        Err(_) => {
            daemon_unavailable("the daemon closed the connection with an operation still running")
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
        Some((
            mp_client::format::outbox_drain_line(tail, done, total),
            false,
        ))
    } else if phase.ends_with("_mutations") {
        Some((
            mp_client::format::mutations_drain_line(tail, done, total),
            false,
        ))
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
                // start deleting cached blobs. The sweep is the daemon's since
                // P4-U15; only the report below it is this process's.
                if !dry_run {
                    retention_sweep_after_sync(&mut connection, account_config).await;
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
            println!(
                "{}",
                paint(&format!("- {}: local-only, skipped", account.name))
            );
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
/// mailbox that was dropped and the reason. The
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
             {WATCHED_MAILBOX}-only, watching another mailbox is not supported yet",
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
async fn routed_list_mailboxes(account: &AccountConfig, json: bool) -> Result<()> {
    let mut connection = daemon_connection().await;
    let result = daemon_try_call_within(
        &mut connection,
        "mailbox.list_server",
        serde_json::json!({"account": account.name}),
        DAEMON_SERVER_TIMEOUT,
    )
    .await
    .map_err(|e| refusal(&account.name, e))?;
    if json {
        // The daemon's own result shape: `account`, `source` and the
        // `mailboxes` rows (`name`, `label`, `delimiter`, `attributes`,
        // `total`, `unread`, `role`, `policy`, `key`, `reason`, `null` where
        // the transport or the catalogue says nothing).
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }

    let graph = wire_str(&result["source"]) == "graph";
    println!(
        "{} Available {}:",
        "ℹ".blue(),
        if graph { "folders" } else { "mailboxes" }
    );
    for line in mailypoppins::read_cmd::server_mailbox_lines(&result) {
        println!("{line}");
    }
    Ok(())
}

/// Run the automatic post-sync retention sweep for one account, best-effort.
///
/// The sweep is `diagnostic.store_gc`'s from P4-U15, on the connection the sync
/// is already following, with the options the post-sync sweep has always used
/// (`SweepOptions::default()`, which is neither a dry run nor forced). Only the
/// report is this process's, and `manual = false` keeps it as quiet as it was.
///
/// A sweep failure never fails the sync it rides on: the store is a cache and
/// an unswept cache is merely too big, not broken, so the error is logged and
/// the sync still reports success. An account the daemon calls storeless is the
/// silent case the file check used to be.
async fn retention_sweep_after_sync(
    connection: &mut mp_client::Connection,
    account: &AccountConfig,
) {
    let params = serde_json::json!({"account": account.name, "dry_run": false, "force": false});
    let started = match daemon_try_call(connection, "diagnostic.store_gc", params).await {
        Ok(started) => started,
        Err(error) if is_storeless(&error) => return,
        Err(error) => {
            warn!(
                "[retention] skipping sweep for '{}': {}",
                account.name, error.message
            );
            return;
        }
    };
    match settle(connection, &started).await {
        Ok(result) => report_sweep_outcome(&account.name, &sweep_outcome(&result, false), false),
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
                if outcome.dry_run {
                    "would evict"
                } else {
                    "evicted"
                },
                outcome.evicted.len(),
                human_bytes(outcome.reclaimed_bytes()),
                if outcome.dry_run { "would be" } else { "now" },
            );
            if outcome.dry_run {
                for e in &outcome.evicted {
                    let horizon = if e.past_horizon {
                        " [past horizon]"
                    } else {
                        ""
                    };
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
    json: bool,
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
    // The build this search ran because the account had no index, refused by
    // the cache guard: the answer is the kept index, and stderr says so, which
    // leaves `--parsable` and `--json` on stdout as they are.
    if let Ok(refused) =
        serde_json::from_value::<mp_protocol::contact::RefusedRebuild>(result["rebuild"].clone())
    {
        if let Some(line) =
            mp_client::format::refused_rebuild_line(&refused.saved, refused.contacts, refused.kept)
        {
            eprintln!("{} {line}", "\u{26a0}".yellow());
        }
    }
    let rows = contact_rows(&result["contacts"]);
    if json {
        println!(
            "{}",
            mailypoppins::contacts_cmd::search_json(account, query, &rows)?
        );
    } else {
        mailypoppins::contacts_cmd::print_search(&rows, query, parsable);
    }
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
        if let Some(server) = result.get("server") {
            mailypoppins::calendar_cmd::print_server_report(server);
        }
    }
    Ok(())
}

/// The account a hook command means: the one `-A` names, or else the one
/// account that configures a hook of that name.
fn account_for_hook(global: &GlobalConfig, named: Option<&str>, hook: &str) -> Result<String> {
    if named.is_some() {
        return Ok(pick_account_named(global, named)?.name.clone());
    }
    let owners: Vec<&str> = global
        .accounts
        .iter()
        .filter(|account| account.hooks.iter().any(|h| h.name == hook))
        .map(|account| account.name.as_str())
        .collect();
    match owners.as_slice() {
        [one] => Ok(one.to_string()),
        [] => Err(anyhow!("no account configures a hook named {hook:?}")),
        many => Err(anyhow!(
            "{} accounts configure a hook named {hook:?} ({}); name one with -A",
            many.len(),
            many.join(", ")
        )),
    }
}

/// `mp hooks list`: one block per account, in configuration order.
async fn routed_hooks_list(accounts: &[String], json: bool) -> Result<()> {
    let mut connection = daemon_connection().await;
    let mut answers = Vec::new();
    for account in accounts {
        let result = daemon_try_call(
            &mut connection,
            "hook.list",
            serde_json::json!({"account": account}),
        )
        .await
        .map_err(|error| refusal(account, error))?;
        answers.push(result);
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&answers)?);
        return Ok(());
    }
    let mut any = false;
    for answer in &answers {
        let hooks = answer["hooks"].as_array().cloned().unwrap_or_default();
        if hooks.is_empty() {
            continue;
        }
        any = true;
        println!("{}", wire_str(&answer["account"]).bold());
        for hook in &hooks {
            print!("{}", render_hook(hook));
        }
    }
    if !any {
        println!("No hooks configured");
    }
    Ok(())
}

/// One `hook.list` entry as `mp hooks list` prints it.
fn render_hook(hook: &serde_json::Value) -> String {
    let strings = |value: &serde_json::Value| -> Vec<String> {
        value
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .map(|item| wire_str(item).to_string())
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut out = format!(
        "  {}  {}  {}  (timeout {}s)\n",
        wire_str(&hook["name"]),
        wire_str(&hook["mailbox"]),
        strings(&hook["exec"]).join(" "),
        hook["timeout_secs"].as_u64().unwrap_or_default(),
    );
    let criteria = &hook["match"];
    let senders = strings(&criteria["authenticated_from"]);
    if !senders.is_empty() {
        out.push_str(&format!(
            "    from     {} ({} checked by {})\n",
            senders.join(", "),
            if criteria["accept_spf"].as_bool().unwrap_or(false) {
                "DKIM or SPF"
            } else {
                "DKIM"
            },
            wire_str(&criteria["authserv_id"])
        ));
    }
    let to = strings(&criteria["to"]);
    if !to.is_empty() {
        out.push_str(&format!("    to       {}\n", to.join(", ")));
    }
    if let Some(subject) = criteria["subject"].as_str() {
        out.push_str(&format!("    subject  {subject}\n"));
    }
    if let Some(headers) = criteria["headers"].as_object() {
        for (name, pattern) in headers {
            out.push_str(&format!("    header   {name}: {}\n", wire_str(pattern)));
        }
    }
    if hook["mailbox_key"].is_null() {
        out.push_str(&format!(
            "    {} mailbox {:?} is not configured for this account\n",
            "\u{2717}".red(),
            wire_str(&hook["mailbox"])
        ));
    }
    match hook["cursor"].as_object() {
        Some(cursor) => out.push_str(&format!(
            "    cursor   armed {}, last uid {}, fired {}\n",
            wire_str(&cursor["armed_at"]),
            cursor["last_uid"].as_i64().unwrap_or_default(),
            cursor["fired"].as_u64().unwrap_or_default(),
        )),
        None => out.push_str("    cursor   not armed yet (arms at the next sync)\n"),
    }
    if let Some(run) = hook["last_run"].as_object() {
        let mark = if run["ok"].as_bool().unwrap_or(false) {
            "\u{2713}".green()
        } else {
            "\u{2717}".red()
        };
        out.push_str(&format!(
            "    last run {mark} {} {} {} in {}ms\n",
            wire_str(&run["at"]),
            wire_str(&run["message_id"]),
            wire_str(&run["outcome"]),
            run["duration_ms"].as_u64().unwrap_or_default(),
        ));
    }
    out
}

/// The parameters `hook.test` and `hook.replay` share.
fn hook_params(
    account: &str,
    hook: &str,
    selector: &str,
    mailbox: Option<&str>,
) -> serde_json::Value {
    let mut params = serde_json::json!({"account": account, "hook": hook, "selector": selector});
    if let Some(mailbox) = mailbox {
        params["mailbox"] = serde_json::json!(mailbox);
    }
    params
}

/// `mp hooks test`: every criterion with its verdict, and whether it would run.
async fn routed_hooks_test(
    account: &str,
    hook: &str,
    selector: &str,
    mailbox: Option<&str>,
    json: bool,
) -> Result<()> {
    let mut connection = daemon_connection().await;
    let result = daemon_try_call(
        &mut connection,
        "hook.test",
        hook_params(account, hook, selector, mailbox),
    )
    .await
    .map_err(|error| refusal(account, error))?;
    if json {
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }
    for check in result["checks"].as_array().cloned().unwrap_or_default() {
        let mark = if check["passed"].as_bool().unwrap_or(false) {
            "\u{2713}".green()
        } else {
            "\u{2717}".red()
        };
        println!(
            "  {mark} {}  {}",
            wire_str(&check["criterion"]),
            wire_str(&check["detail"])
        );
    }
    let matched = result["matched"].as_bool().unwrap_or(false);
    let fired_before = result["fired_before"].as_bool().unwrap_or(false);
    println!(
        "{}",
        match (matched, fired_before) {
            (true, false) => format!("{hook} matches this message"),
            (true, true) => format!("{hook} matches this message and has already fired for it"),
            (false, _) => format!("{hook} does not match this message"),
        }
    );
    Ok(())
}

/// `mp hooks replay`: run it in the daemon and print what the command did.
async fn routed_hooks_replay(
    account: &str,
    hook: &str,
    selector: &str,
    mailbox: Option<&str>,
) -> Result<()> {
    let mut connection = operation_session().await;
    let started = daemon_try_call(
        &mut connection,
        "hook.replay",
        hook_params(account, hook, selector, mailbox),
    )
    .await
    .map_err(|error| refusal(account, error))?;
    let result = settle(&mut connection, &started).await?;
    let ok = result["ok"].as_bool().unwrap_or(false);
    let mark = if ok {
        "\u{2713}".green()
    } else {
        "\u{2717}".red()
    };
    println!(
        "{mark} {hook} for {}: {} in {}ms",
        wire_str(&result["message_id"]),
        wire_str(&result["outcome"]),
        result["duration_ms"].as_u64().unwrap_or_default()
    );
    for stream in ["stdout", "stderr"] {
        let text = wire_str(&result[stream]);
        if !text.is_empty() {
            println!("{stream}:\n{text}");
        }
    }
    if !ok {
        std::process::exit(1);
    }
    Ok(())
}

/// `mp invite accept|tentative|decline`: the answer is `event.respond`'s,
/// through the server when the account's calendar holds the invitation's
/// event (`message.invite`'s `event_id`, PERSO-200), else the iMIP reply the
/// daemon builds, submits and files; the one line it prints is this
/// process's.
///
/// The selector resolves first: a Graph account answers through its server
/// calendar, so `ANO-4` is now the refusal of a Graph account's invitation
/// with no server event to answer, which `event.respond` raises.
async fn routed_rsvp(
    account_config: &AccountConfig,
    invite: &InviteAnswer,
    response: EventResponse,
) -> Result<()> {
    let account = account_config.name.as_str();
    let mut connection = operation_session().await;
    let (selector, card) = fetch_on_miss(
        account_config,
        &invite.selector,
        invite.mailbox.as_deref(),
        StandIn::Allowed,
        async |selector: &str, mailbox: Option<&str>| {
            let mut params = serde_json::json!({"account": account, "selector": selector});
            if let Some(mailbox) = mailbox {
                params["mailbox"] = serde_json::json!(mailbox);
            }
            let card = daemon_try_call(&mut connection, "message.invite", params)
                .await
                .map_err(|e| refusal(account, e))?;
            Ok((selector.to_string(), card))
        },
    )
    .await?;
    let row_id = card["row_id"].as_i64();
    let event = match (card["event"].is_null(), card["event_id"].as_str(), row_id) {
        (true, _, _) | (false, None, None) => {
            bail!("{selector} carries no invitation to reply to")
        }
        (false, Some(server), _) => server.to_string(),
        (false, None, Some(row_id)) => mailypoppins::calendar_view::imip_event_id(row_id),
    };
    let shown = event_get(&mut connection, account, &event).await?;
    let settled = respond(&mut connection, account, &event, response, &invite.answer).await?;
    let organizer = shown
        .organizer
        .as_ref()
        .map(|organizer| organizer.email.clone())
        .unwrap_or_else(|| "the organizer".to_string());
    if settled.via == "imip" && !settled.delivered {
        return Err(anyhow!("Failed to send the RSVP to {organizer}"));
    }
    let summary = if shown.summary.is_empty() {
        "(no subject)"
    } else {
        &shown.summary
    };
    if settled.delivered {
        println!(
            "{} {}: {} \u{2014} replied to {}",
            "\u{2713}".green(),
            response.verb(),
            summary,
            organizer
        );
    } else {
        println!(
            "{} {}: {}, without telling {}",
            "\u{2713}".green(),
            response.verb(),
            summary,
            organizer
        );
    }
    Ok(())
}

/// `event.get`, decoded.
async fn event_get(
    connection: &mut mp_client::Connection,
    account: &str,
    event: &str,
) -> Result<mp_protocol::calendar::CalendarEvent> {
    let answer = daemon_try_call(
        connection,
        "event.get",
        serde_json::json!({"account": account, "event": event}),
    )
    .await
    .map_err(|e| refusal(account, e))?;
    let answer: mp_protocol::calendar::EventGetResult =
        serde_json::from_value(answer).context("reading the daemon's event.get answer")?;
    Ok(answer.event)
}

/// `event.respond`, run to its settled result.
async fn respond(
    connection: &mut mp_client::Connection,
    account: &str,
    event: &str,
    response: EventResponse,
    answer: &AnswerFlags,
) -> Result<mp_protocol::calendar::EventRespondResult> {
    let params = mp_client::queries::event_respond_params(
        account,
        event,
        response.word(),
        answer.comment.as_deref(),
        !answer.no_send_response,
    );
    let result = run_admin_operation(connection, "event.respond", params, account).await?;
    serde_json::from_value(result).context("reading the settled event.respond result")
}

/// `calendar.list` for one account or every one, decoded.
async fn calendar_list(
    connection: &mut mp_client::Connection,
    account: Option<&str>,
) -> Result<serde_json::Value> {
    let params = match account {
        Some(account) => serde_json::json!({"account": account}),
        None => serde_json::json!({}),
    };
    daemon_try_call(connection, "calendar.list", params)
        .await
        .map_err(|e| refusal(account.unwrap_or_default(), e))
}

fn calendar_infos(answer: &serde_json::Value) -> Result<Vec<mp_protocol::calendar::CalendarInfo>> {
    let list: mp_protocol::calendar::CalendarListResult = serde_json::from_value(answer.clone())
        .context("reading the daemon's calendar.list answer")?;
    Ok(list.calendars)
}

/// `mp calendar list`.
async fn routed_calendar_list(account: Option<&str>, json: bool) -> Result<()> {
    let mut connection = daemon_connection().await;
    let answer = calendar_list(&mut connection, account).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&answer)?);
        return Ok(());
    }
    print!(
        "{}",
        mailypoppins::calendar_cmd::render_calendars(&calendar_infos(&answer)?)
    );
    Ok(())
}

/// What `mp calendar view` was asked, before the dates are read.
struct ViewRequest {
    layout: mailypoppins::calendar_cmd::Layout,
    on: Option<String>,
    range: Option<(String, String)>,
    accounts: Vec<String>,
    calendars: Vec<String>,
    hidden: bool,
    zone: Option<String>,
}

/// The zone `--zone` names, else the local one.
fn viewer_zone(zone: Option<&str>) -> Result<chrono_tz::Tz> {
    match zone {
        Some(name) => mailypoppins::calendar_view::parse_zone(name).ok_or_else(|| {
            anyhow!("unknown time zone {name:?}; use an IANA name like Europe/Berlin")
        }),
        None => Ok(mailypoppins::calendar_view::local_zone()),
    }
}

/// `mp calendar view`: the days are read here, in the viewer's zone, and the
/// events are `calendar.view`'s.
async fn routed_calendar_view(request: ViewRequest, json: bool) -> Result<()> {
    use mailypoppins::calendar_cmd as cal;
    let zone = viewer_zone(request.zone.as_deref())?;
    let today = chrono::Utc::now().with_timezone(&zone).date_naive();
    let date = |raw: &str| cal::parse_date(raw, today).map_err(|e| anyhow!(e));
    let on = request.on.as_deref().map(date).transpose()?;
    let range = match &request.range {
        Some((from, to)) => Some((date(from)?, date(to)?)),
        None => None,
    };
    let days = cal::view_days(request.layout, on, range, today).map_err(|e| anyhow!(e))?;
    let params = cal::view_params(
        days,
        zone,
        (!request.accounts.is_empty()).then_some(request.accounts.clone()),
        (!request.calendars.is_empty()).then_some(request.calendars),
        request.hidden,
    );
    let mut connection = daemon_connection().await;
    let answer = daemon_try_call(
        &mut connection,
        "calendar.view",
        serde_json::to_value(&params)?,
    )
    .await
    .map_err(|e| refusal(request.accounts.first().map_or("", String::as_str), e))?;
    if json {
        println!("{}", serde_json::to_string_pretty(&answer)?);
        return Ok(());
    }
    let view: mp_protocol::calendar::CalendarViewResult =
        serde_json::from_value(answer).context("reading the daemon's calendar.view answer")?;
    let names = match request.accounts.as_slice() {
        [one] => calendar_list(&mut connection, Some(one)).await?,
        _ => calendar_list(&mut connection, None).await?,
    };
    let names = cal::calendar_names(&calendar_infos(&names)?);
    print!(
        "{}",
        cal::render_view(request.layout, days, today, zone, &view.events, &names)
    );
    Ok(())
}

/// `mp calendar show|hide`.
async fn routed_calendar_visibility(account: &str, calendar: &str, visible: bool) -> Result<()> {
    let mut connection = daemon_connection().await;
    daemon_try_call(
        &mut connection,
        "calendar.set_visibility",
        serde_json::json!({"account": account, "calendar": calendar, "visible": visible}),
    )
    .await
    .map_err(|e| refusal(account, e))?;
    println!(
        "{} {account}/{calendar} is {}",
        "\u{2713}".green(),
        if visible { "visible" } else { "hidden" }
    );
    Ok(())
}

/// `mp event show`.
async fn routed_event_show(account: &str, event: &str, json: bool) -> Result<()> {
    let mut connection = daemon_connection().await;
    if json {
        let answer = daemon_try_call(
            &mut connection,
            "event.get",
            serde_json::json!({"account": account, "event": event}),
        )
        .await
        .map_err(|e| refusal(account, e))?;
        println!("{}", serde_json::to_string_pretty(&answer)?);
        return Ok(());
    }
    let shown = event_get(&mut connection, account, event).await?;
    let names = calendar_list(&mut connection, Some(account)).await?;
    let names = mailypoppins::calendar_cmd::calendar_names(&calendar_infos(&names)?);
    print!(
        "{}",
        mailypoppins::calendar_cmd::render_event(
            &shown,
            mailypoppins::calendar_view::local_zone(),
            &names
        )
    );
    Ok(())
}

/// `mp event respond`: the settled answer, and how it reached the organizer.
async fn routed_event_respond(
    account: &str,
    event: &str,
    response: EventResponse,
    answer: &AnswerFlags,
    json: bool,
) -> Result<()> {
    let mut connection = operation_session().await;
    let settled = respond(&mut connection, account, event, response, answer).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&settled)?);
        return Ok(());
    }
    let how = match (settled.via.as_str(), settled.delivered) {
        ("imip", true) => "iMIP reply sent to the organizer",
        ("imip", false) => "the iMIP reply was not delivered",
        (_, true) => "through the server, organizer told",
        (_, false) => "through the server, organizer not told",
    };
    println!(
        "{} {} {}: {how}",
        "\u{2713}".green(),
        response.verb(),
        settled.event
    );
    if let Some(comment) = settled.comment.as_deref() {
        println!("  comment: {comment}");
    }
    if settled.via == "imip" && !settled.delivered {
        std::process::exit(1);
    }
    Ok(())
}

/// `mp store gc`: the sweep is the daemon's, the report is
/// [`report_sweep_outcome`]'s, and an account with no store is a note.
async fn routed_store_gc(account: &str, dry_run: bool, force: bool) -> Result<()> {
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
    report_sweep_outcome(account, &sweep_outcome(&result, dry_run), true);
    Ok(())
}

/// One settled `diagnostic.store_gc` result, back as the outcome
/// [`report_sweep_outcome`] renders.
///
/// `dry_run` is the flag the caller asked for, used only when the answer does
/// not carry one of its own.
fn sweep_outcome(
    result: &serde_json::Value,
    dry_run: bool,
) -> mailypoppins::store::sweep::SweepOutcome {
    use mailypoppins::store::sweep::{BlobKind, EvictedBlob, SweepDecision, SweepOutcome};

    let number = |key: &str| result[key].as_u64().unwrap_or_default();
    let decision = &result["decision"];
    SweepOutcome {
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
    }
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

/// `mp config show`: the effective configuration, rendered from `config.get`,
/// and whether each password and token is stored, from `account.credentials`.
///
/// Each account's `[folders]` come from `mailbox.list` with `ignored: true`,
/// every folder the server last listed with its policy (#0141); an account
/// the daemon cannot list yet says why instead.
async fn routed_config_show() -> Result<()> {
    let (path, _, config) = routed_config_state().await?;
    let mut connection = daemon_connection().await;
    let answer = daemon_try_call(
        &mut connection,
        "account.credentials",
        serde_json::json!({}),
    )
    .await
    .map_err(|error| anyhow!("{}", error.message))?;
    let credentials: mp_protocol::account::AccountCredentials = serde_json::from_value(answer)
        .context("reading the credential presence the daemon reported")?;
    let mut signatures = mailypoppins::config_cmd::Signatures::default();
    for account in &config.accounts {
        let answer = daemon_try_call(
            &mut connection,
            "signature.list",
            serde_json::json!({"account": account.name}),
        )
        .await
        .map_err(|error| anyhow!("{}", error.message))?;
        let listing: mp_protocol::signature::SignatureListing = serde_json::from_value(answer)
            .context("reading the signature listing the daemon reported")?;
        if signatures.files.is_empty() {
            for name in &listing.names {
                let answer = daemon_try_call(
                    &mut connection,
                    "signature.read",
                    serde_json::json!({"name": name}),
                )
                .await
                .map_err(|error| anyhow!("{}", error.message))?;
                signatures.files.push(
                    serde_json::from_value(answer)
                        .context("reading the signature the daemon reported")?,
                );
            }
        }
        signatures
            .defaults
            .insert(account.name.clone(), listing.default);
    }
    let mut folders = mailypoppins::config_cmd::show::AccountFolders::new();
    for account in &config.accounts {
        let mut params = mp_client::queries::mailbox_list_params(&account.name);
        params["ignored"] = serde_json::json!(true);
        let rows = daemon_try_call(&mut connection, "mailbox.list", params)
            .await
            .map(|answer| mp_client::queries::decode_mailbox_rows(&answer))
            .map_err(|error| error.message);
        folders.insert(account.name.clone(), rows);
    }
    cmd_config_show(&config, &path, &credentials, &signatures, &folders)
}

/// One `signature.*` call, its refusal raised as the daemon worded it.
async fn signature_call(
    connection: &mut mp_client::Connection,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value> {
    daemon_try_call(connection, method, params)
        .await
        .map_err(|error| anyhow!("{}", error.message))
}

/// `mp signature list`: the names, the account's default marked.
fn render_signature_listing(listing: &mp_protocol::signature::SignatureListing) -> String {
    if listing.names.is_empty() {
        return "No signatures (create one with `mp signature new <name>`)\n".to_string();
    }
    let mut out = String::new();
    for name in &listing.names {
        let marker = if listing.default.as_deref() == Some(name.as_str()) {
            "  (default)"
        } else {
            ""
        };
        out.push_str(&format!("{name}{marker}\n"));
    }
    out
}

/// `mp signature <action>`: every listing and every change is the daemon's
/// `signature.*`, the family the TUI's `cs` overlay and the desktop's
/// Signatures dialog call; this process only prints, and runs `$EDITOR` on
/// the file the daemon names.
async fn routed_signature(account: &str, action: SignatureAction) -> Result<()> {
    use mp_protocol::signature::{SignatureFile, SignatureListing};
    let mut connection = daemon_connection().await;
    let listing = |answer: serde_json::Value| -> Result<SignatureListing> {
        serde_json::from_value(answer).context("reading the signature listing the daemon reported")
    };
    let file = |answer: serde_json::Value| -> Result<SignatureFile> {
        serde_json::from_value(answer).context("reading the signature the daemon reported")
    };
    match action {
        SignatureAction::List { json } => {
            let answer = signature_call(
                &mut connection,
                "signature.list",
                serde_json::json!({"account": account}),
            )
            .await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&answer)?);
            } else {
                print!("{}", render_signature_listing(&listing(answer)?));
            }
        }
        SignatureAction::Show { name, path } => {
            let read = file(
                signature_call(
                    &mut connection,
                    "signature.read",
                    serde_json::json!({"name": name}),
                )
                .await?,
            )?;
            if path {
                println!("{}", read.path);
            } else if read.content.ends_with('\n') || read.content.is_empty() {
                print!("{}", read.content);
            } else {
                println!("{}", read.content);
            }
        }
        SignatureAction::New { name } => {
            let created = file(
                signature_call(
                    &mut connection,
                    "signature.create",
                    serde_json::json!({"name": name}),
                )
                .await?,
            )?;
            println!("{}", created.path);
        }
        SignatureAction::Edit { name } => {
            // The daemon names the file and this process runs the editor on
            // it, as `mp edit` does for a draft.
            let read = file(
                signature_call(
                    &mut connection,
                    "signature.read",
                    serde_json::json!({"name": name}),
                )
                .await?,
            )?;
            let editor = std::env::var("EDITOR").unwrap_or_else(|_| "hx".to_string());
            let status = std::process::Command::new(&editor)
                .arg(&read.path)
                .status()
                .with_context(|| format!("running {editor}"))?;
            if !status.success() {
                return Err(anyhow!("{editor} exited with {status}"));
            }
            println!("{} Signature '{name}' edited", "\u{2713}".green());
        }
        SignatureAction::Rename { old, new } => {
            signature_call(
                &mut connection,
                "signature.rename",
                serde_json::json!({"account": account, "old": old, "new": new}),
            )
            .await?;
            println!(
                "{} Renamed signature '{old}' to '{new}'",
                "\u{2713}".green()
            );
        }
        SignatureAction::Delete { name } => {
            signature_call(
                &mut connection,
                "signature.delete",
                serde_json::json!({"account": account, "name": name}),
            )
            .await?;
            println!("{} Deleted signature '{name}'", "\u{2713}".green());
        }
        SignatureAction::Default {
            name: None,
            none: false,
        } => {
            let answer = signature_call(
                &mut connection,
                "signature.list",
                serde_json::json!({"account": account}),
            )
            .await?;
            match listing(answer)?.default {
                Some(name) => println!("{name}"),
                None => println!("(none)"),
            }
        }
        SignatureAction::Default { name, .. } => {
            signature_call(
                &mut connection,
                "signature.set_default",
                serde_json::json!({"account": account, "name": name}),
            )
            .await?;
            match name {
                Some(name) => println!(
                    "{} Default signature of {account}: '{name}'",
                    "\u{2713}".green()
                ),
                None => println!("{} {account} has no default signature", "\u{2713}".green()),
            }
        }
    }
    Ok(())
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
    println!("  - For Graph accounts, you will need to re-run `mp config oauth2-login`");
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
            AuthMethod::Graph => {
                println!(
                    "{} Account '{}' (Graph auth) -- run `mp config oauth2-login --account {}` to re-acquire token",
                    "\u{2139}".blue(),
                    account.name.bold(),
                    account.name
                );
            }
            AuthMethod::RetiredOAuth2 => {
                println!(
                    "{} {}",
                    "\u{26a0}".yellow(),
                    mailypoppins::config::retired_oauth2_message(&account.name)
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
    // The default is the client's: the first Graph account, else the first
    // account at all, which the daemon then refuses with the reason.
    let name = match account {
        Some(name) => name.to_string(),
        None => global
            .accounts
            .iter()
            .find(|a| a.auth_method == AuthMethod::Graph)
            .or_else(|| global.accounts.first())
            .ok_or_else(|| anyhow!("No accounts configured"))?
            .name
            .clone(),
    };

    let mut connection = operation_session().await;
    sign_in(&mut connection, serde_json::json!({"account": name}), &name).await
}

/// One `config.oauth2_login`, its start line, the device code as the daemon
/// reports it and the stored line: `mp config oauth2-login`'s rendering, and
/// the setup wizards' sign-in of a candidate account.
async fn sign_in(
    connection: &mut mp_client::Connection,
    params: serde_json::Value,
    name: &str,
) -> Result<()> {
    let started = daemon_try_call(connection, "config.oauth2_login", params)
        .await
        .map_err(|error| anyhow!("{}", error.message))?;
    println!("{}", paint(&mp_client::format::oauth2_start_line(name)));

    let id = wire_str(&started["operation_id"]).to_string();
    let settled = await_operation(connection, &id, |payload| {
        if payload["phase"].as_str()
            == Some(mailypoppins::daemon::methods::config::DEVICE_CODE_PHASE)
        {
            if let Some((uri, code)) = payload["message"].as_str().and_then(|m| m.split_once(' ')) {
                print!("{}", mp_client::format::oauth2_device_code_lines(uri, code));
            }
        }
    })
    .await;
    match settled {
        Settled::Done(_) => {
            println!("{}", paint(&mp_client::format::oauth2_stored_line(name)));
            Ok(())
        }
        Settled::Failed(message) => Err(anyhow!("{message}")),
    }
}

/// How long one connection test or mailbox listing may take: it dials a
/// server, so the store-read budget is too short for a slow one.
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// The setup wizards' door onto the daemon (`mailypoppins::config_cmd`).
struct DaemonWizard {
    connection: mp_client::Connection,
}

impl mailypoppins::config_cmd::WizardDaemon for DaemonWizard {
    async fn probe(
        &mut self,
        candidate: &serde_json::Value,
        check: &str,
        password: Option<&str>,
    ) -> Result<mailypoppins::config_cmd::Probe> {
        let mut params = serde_json::json!({"account": candidate, "check": check});
        if let Some(password) = password {
            params["password"] = serde_json::json!(password);
        }
        let answer =
            daemon_try_call_within(&mut self.connection, "config.probe", params, PROBE_TIMEOUT)
                .await
                .map_err(|error| anyhow!("{}", error.message))?;
        Ok(mailypoppins::config_cmd::Probe::from_answer(&answer))
    }

    async fn sign_in(&mut self, candidate: &serde_json::Value) -> Result<()> {
        let name = wire_str(&candidate["name"]).to_string();
        sign_in(
            &mut self.connection,
            serde_json::json!({"account": name, "candidate": candidate}),
            &name,
        )
        .await
    }

    async fn init(&mut self, candidate: &serde_json::Value, replace: bool) -> Result<PathBuf> {
        // The two globals the wizard has always written, so a file it makes
        // reads the same as before the daemon wrote it.
        let answer = daemon_try_call(
            &mut self.connection,
            "config.init",
            serde_json::json!({
                "account": candidate,
                "replace": replace,
                "theme": mailypoppins::tui::theme::DEFAULT_THEME_NAME,
                "notifications": false,
            }),
        )
        .await
        .map_err(|error| anyhow!("{}", error.message))?;
        Ok(PathBuf::from(wire_str(&answer["path"])))
    }

    async fn add_account(&mut self, candidate: &serde_json::Value) -> Result<()> {
        daemon_try_call(
            &mut self.connection,
            "config.add_account",
            serde_json::json!({"account": candidate}),
        )
        .await
        .map_err(|error| anyhow!("{}", error.message))?;
        Ok(())
    }

    async fn set_password(&mut self, account: &str, kind: &str, value: &str) -> Result<()> {
        daemon_try_call(
            &mut self.connection,
            "config.set_password",
            serde_json::json!({"account": account, "kind": kind, "value": value}),
        )
        .await
        .map_err(|error| anyhow!("{}", error.message))?;
        Ok(())
    }
}

/// A daemon session that will follow an operation.
///
/// A client only receives `operation.progress` and `operation.finished` if it
/// subscribed first, which `state.bootstrap` is: a session that skipped it
/// waits forever for a notification nobody addressed to it.
async fn operation_session() -> mp_client::Connection {
    let mut connection = daemon_connection().await;
    match connection.subscribe_within(DAEMON_TIMEOUT).await {
        Ok(_) => connection,
        Err(mp_client::ClientError::Rpc(error)) => {
            eprintln!("{} {}", "\u{2717}".red(), error.message);
            std::process::exit(1);
        }
        Err(e) => unanswered("state.bootstrap", e),
    }
}

/// Start one operation of the admin slice and wait for its result, raising the
/// refusal the daemon spelled out.
async fn run_admin_operation(
    connection: &mut mp_client::Connection,
    method: &str,
    params: serde_json::Value,
    account: &str,
) -> Result<serde_json::Value> {
    match connection
        .run_operation(method, params, DAEMON_TIMEOUT, |_| {})
        .await
    {
        Ok(Settled::Done(result)) => Ok(result),
        Ok(Settled::Failed(message)) => Err(anyhow!("{message}")),
        Err(mp_client::ClientError::Rpc(error)) => Err(refusal(account, error)),
        Err(mp_client::ClientError::Closed) => {
            daemon_unavailable("the daemon closed the connection with an operation still running")
        }
        Err(e) => unanswered(method, e),
    }
}

/// Wait for an operation this slice started, with no progress to render.
async fn settle(
    connection: &mut mp_client::Connection,
    started: &serde_json::Value,
) -> Result<serde_json::Value> {
    match connection.settle(started).await {
        Ok(Settled::Done(result)) => Ok(result),
        Ok(Settled::Failed(message)) => Err(anyhow!("{message}")),
        Err(_) => {
            daemon_unavailable("the daemon closed the connection with an operation still running")
        }
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

/// `--all-accounts` beside `-A`, wherever `-A` was written.
///
/// The `conflicts_with = "account"` on each `--all-accounts` catches `-A` only
/// when it follows the subcommand: clap checks a subcommand's conflicts against
/// its own matches, and a global flag written before the subcommand is
/// propagated into them after that check. This is the same refusal for the
/// other spelling, so `mp -A work send-approved --all-accounts` cannot send
/// every account's drafts.
fn refuse_all_accounts_with_selector(cli: &Cli) -> std::result::Result<(), clap::Error> {
    let all_accounts = match &cli.command {
        Some(Commands::SendApproved { all_accounts, .. }) => *all_accounts,
        Some(Commands::Sync { all_accounts, .. }) => *all_accounts,
        Some(Commands::Store {
            action: StoreAction::Gc { all_accounts, .. },
        }) => *all_accounts,
        _ => false,
    };
    if all_accounts && cli.account.is_some() {
        return Err(conflict(
            "the argument '--all-accounts' cannot be used with '--account <ACCOUNT>'",
        ));
    }
    Ok(())
}

/// A usage conflict clap's own checks cannot see, reported the way clap
/// reports one, under the binary's name rather than the crate's.
fn conflict(message: impl std::fmt::Display) -> clap::Error {
    <Cli as clap::CommandFactory>::command()
        .bin_name("mp")
        .error(clap::error::ErrorKind::ArgumentConflict, message)
}

/// `-A` on a command that names no account, wherever `-A` was written.
///
/// The daemon's lifecycle, the shell completions, the key dump, the account
/// listing and the five `mp config` commands that are about the whole file
/// (`init`, `show`, `add-account`, `reset-secrets`, `path`) act on no account,
/// so an `-A` there is a mistake to report rather than a value to ignore:
/// `mp daemon status -A alpha` used to be accepted and mean nothing. Each of
/// them shadows the global flag with a hidden [`NoAccount`], which keeps it
/// out of their help, and this refuses it, whether it was written before the
/// subcommand or after it.
fn refuse_account_where_meaningless(
    cli: &Cli,
    command_label: &str,
) -> std::result::Result<(), clap::Error> {
    let shadowed = match &cli.command {
        Some(Commands::Daemon { no_account, .. })
        | Some(Commands::Completions { no_account, .. })
        | Some(Commands::DumpKeys { no_account, .. })
        | Some(Commands::Account { no_account, .. }) => no_account,
        Some(Commands::Config { action }) => match action {
            ConfigAction::Init { no_account }
            | ConfigAction::Show { no_account }
            | ConfigAction::ResetSecrets { no_account }
            | ConfigAction::AddAccount { no_account }
            | ConfigAction::Path { no_account } => no_account,
            ConfigAction::SetPassword { .. } | ConfigAction::Oauth2Login => return Ok(()),
        },
        _ => return Ok(()),
    };
    if shadowed.account.is_some() || cli.account.is_some() {
        return Err(conflict(format!(
            "the argument '--account <ACCOUNT>' cannot be used with 'mp {command_label}', \
             which acts on no account"
        )));
    }
    Ok(())
}

/// Write the `shell` completion script for `mp` to `out`.
fn write_completions(shell: clap_complete::Shell, out: &mut dyn Write) -> Result<()> {
    let mut cmd = <Cli as clap::CommandFactory>::command();
    clap_complete::generate(shell, &mut cmd, "mp", out);
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    init_logging();
    // Two steps rather than `Cli::parse()`, which is these two: the matches
    // carry the typed command name that the daemon policy is written in.
    // `get_matches` answers `--help` and `--version` and exits, exactly as
    // `parse` does, so the help surface does not move.
    let matches = <Cli as clap::CommandFactory>::command().get_matches();
    let cli =
        <Cli as clap::FromArgMatches>::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());
    refuse_all_accounts_with_selector(&cli).unwrap_or_else(|e| e.exit());
    let (command_name, subcommand_name) = invocation(&matches);
    let command_label = match (command_name, subcommand_name) {
        (Some(command), Some(sub)) => format!("{command} {sub}"),
        (Some(command), None) => command.to_string(),
        _ => "(no subcommand)".to_string(),
    };
    refuse_account_where_meaningless(&cli, &command_label).unwrap_or_else(|e| e.exit());

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

    info!(
        "mailypoppins started: {:?}",
        std::env::args().collect::<Vec<_>>()
    );

    // The daemon commands own their startup order (MIG-04, config load, socket)
    // and must not run the client preamble below, which loads secrets and SMTP
    // credentials this process has no use for.
    if let Some(Commands::Daemon { action, .. }) = &cli.command {
        let code = mailypoppins::daemon::lifecycle::dispatch(action.clone()).await;
        std::process::exit(code);
    }

    // A completion script is generated from the clap definition alone: no
    // config, no secrets, no daemon, and nothing on stderr to pollute an eval.
    if let Some(Commands::Completions { shell, .. }) = &cli.command {
        // Rendered into a buffer first: clap_complete panics on a write error,
        // and `mp completions zsh | head` closing the pipe is not one.
        let mut script = Vec::new();
        write_completions(*shell, &mut script)?;
        match io::stdout().write_all(&script) {
            Err(e) if e.kind() == io::ErrorKind::BrokenPipe => {}
            other => other?,
        }
        return Ok(());
    }

    // Move a pre-#0022 ~/.config/email directory before anything reads config
    // or secrets out of it. A failure here is fatal: the alternative is running
    // against an empty config and silently re-prompting for every password.
    if let Err(e) = mailypoppins::config::migrate_legacy_config_dir() {
        eprintln!("{} {}", "\u{2717}".red(), e);
        std::process::exit(1);
    }

    // Load global config from ~/.config/mailypoppins/config.toml
    // A warning (a key the load ignored, PERSO-118) goes to stderr as well as
    // the log, because a config typo otherwise surfaces only as a command
    // that cannot find what the user believes they configured.
    let global_config = match mailypoppins::config::load_global_config_with_warnings() {
        Ok((config, warnings)) => {
            for warning in &warnings {
                eprintln!("{} {}: {warning}", "⚠".yellow(), config_path().display());
            }
            config
        }
        Err(e) => {
            eprintln!("{} {}", "⚠".yellow(), e);
            eprintln!("  Some commands may not work without proper configuration.");
            GlobalConfig::default()
        }
    };

    // Copy any legacy `[accounts.*.signatures]` tables into the app-managed
    // signatures directory + state file (#0107), before anything resolves a
    // signature out of them. Non-fatal: a signature that fails to migrate
    // costs a warning, not a startup.
    if let Err(e) = mailypoppins::signatures::migrate_config_signatures(&global_config) {
        eprintln!("{} signature migration: {e:#}", "⚠".yellow());
    }

    // The TUI opens no secret: whether an account has its passwords and token
    // is the daemon's `account.credentials`, and every send and sync is a
    // daemon call. So a launch of it skips the two secret reads below, the
    // backend's opening and the SMTP credential load, which every other
    // command still runs for the lines they print.
    let launches_tui = cli.command.is_none() && cli.selector.is_none();

    // Initialize the secrets backend (encrypted file by default, or OS keyring
    // if the user opted in via `secrets_backend = "keyring"` in config.toml).
    let secrets = if launches_tui {
        Ok(())
    } else {
        mailypoppins::config::init_secrets_backend(&global_config)
    };
    if let Err(e) = secrets {
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
                eprintln!(
                    "{} Could not initialize secrets backend: {}",
                    "\u{26a0}".yellow(),
                    err
                );
            }
        }
    }

    // Resolve which account to use
    let account_config: mailypoppins::config::AccountConfig = if let Some(ref name) = cli.account {
        global_config
            .accounts
            .iter()
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
    // A Graph account sends through Graph and has no SMTP transport by design,
    // so it skips the load and its warning.
    let _smtp_config = if launches_tui || account_config.auth_method == AuthMethod::Graph {
        None
    } else {
        Some(SmtpConfig::load(&account_config).unwrap_or_else(|e| {
            eprintln!("{} Could not load SMTP config: {}", "⚠".yellow(), e);
            eprintln!("  Some commands may not work without proper configuration.");
            SmtpConfig {
                host: "localhost".to_string(),
                port: 465,
                username: String::new(),
                password: String::new(),
                default_from: "user@example.com".to_string(),
                accept_invalid_certs: false,
            }
        }))
    };

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
            uid,
            sequence,
            signature,
            no_signature,
        }) => {
            if invite {
                // The daemon resolves and splices the signature, since an
                // invitation has no draft body the user could have edited.
                run_send_invite(
                    &account_config,
                    signature_params(no_signature, signature.as_deref()),
                    InviteArgs {
                        to,
                        cc,
                        subject,
                        start,
                        end,
                        duration,
                        location,
                        description,
                        uid,
                        sequence,
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

        Some(Commands::List { status, json }) => {
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
            if json {
                println!("{}", serde_json::to_string_pretty(&listing)?);
            } else {
                print!("{}", mailypoppins::draft_cmd::render_list(&listing));
            }
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

        Some(Commands::New { name, signature }) => {
            // The `.md` suffixing rule is the daemon's, because the daemon owns
            // the directory the file lands in.
            let created: mp_protocol::draft::DraftCreated = draft_call(
                &account_config.name,
                "draft.create",
                with_params(
                    serde_json::json!({"account": account_config.name, "name": name}),
                    signature.params(),
                ),
            )
            .await?;
            hand_back(
                &format!("{} {}", "\u{2713}".green(), created.selector),
                &created.selector,
            );
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
            hand_back(
                &format!("{} {}", "\u{2713}".green(), location.selector),
                &location.selector,
            );
        }

        Some(Commands::Reply {
            selector,
            all,
            mailbox,
            signature,
        }) => {
            let account_config = account_for_selector(&selector, &account_config, &global_config)?;
            let created = fetch_on_miss(
                &account_config,
                &selector,
                mailbox.as_deref(),
                StandIn::Allowed,
                async |selector: &str, mailbox: Option<&str>| {
                    routed_from_source(
                        &account_config.name,
                        "draft.reply",
                        selector,
                        mailbox,
                        all,
                        false,
                        signature.params(),
                    )
                    .await
                },
            )
            .await?;
            hand_back(
                &format!(
                    "{} reply to {}\n{}",
                    "\u{2713}".green(),
                    created.source.map(|s| s.selector).unwrap_or_default(),
                    created.selector
                ),
                &created.selector,
            );
        }

        Some(Commands::Forward {
            selector,
            mailbox,
            as_attachment,
            signature,
        }) => {
            let account_config = account_for_selector(&selector, &account_config, &global_config)?;
            let created = fetch_on_miss(
                &account_config,
                &selector,
                mailbox.as_deref(),
                StandIn::Allowed,
                async |selector: &str, mailbox: Option<&str>| {
                    routed_from_source(
                        &account_config.name,
                        "draft.forward",
                        selector,
                        mailbox,
                        false,
                        as_attachment,
                        signature.params(),
                    )
                    .await
                },
            )
            .await?;
            hand_back(
                &format!(
                    "{} forward of {}\n{}",
                    "\u{2713}".green(),
                    created.source.map(|s| s.selector).unwrap_or_default(),
                    created.selector
                ),
                &created.selector,
            );
        }

        // The invitation, the reply it builds and the submission are the
        // daemon's from P4-U14, `ANO-4`'s refusal included; the one line the
        // command prints is still this process's.
        Some(Commands::Invite { action }) => {
            let (invite, response) = match action {
                InviteAction::Accept(invite) => (invite, EventResponse::Accept),
                InviteAction::Tentative(invite) => (invite, EventResponse::Tentative),
                InviteAction::Decline(invite) => (invite, EventResponse::Decline),
            };
            // The invitation's own account answers, as for every other
            // received selector: a cross-account selector names its account.
            let account_config =
                account_for_selector(&invite.selector, &account_config, &global_config)?;
            routed_rsvp(&account_config, &invite, response).await?;
        }

        Some(Commands::ListMailboxes { json }) => {
            // Which transport answers, the credentials it needs and the session
            // it opens are all the daemon's (P4-U10); what stays here is the
            // wording of the listing.
            routed_list_mailboxes(&account_config, json).await?;
        }

        Some(Commands::Sync {
            limit,
            mailbox,
            dry_run,
            all_accounts,
        }) => {
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
            routed_sync(&accounts, limit, mailbox.as_deref(), dry_run).await?;
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
            let moved = fetch_on_miss(
                &account_config,
                &selector,
                mailbox.as_deref(),
                StandIn::Refused { verb: "archive" },
                async |selector: &str, mailbox: Option<&str>| {
                    routed_received_mutation(
                        &account_config.name,
                        "message.archive",
                        selector,
                        mailbox,
                        &[],
                    )
                    .await
                },
            )
            .await?;
            hand_back(
                &format!(
                    "{} archived {}\n  {} {}",
                    "\u{2713}".green(),
                    wire_str(&moved["selector"]),
                    "now".dimmed(),
                    wire_str(&moved["moved_to"]["selector"])
                ),
                wire_str(&moved["moved_to"]["selector"]),
            );
        }

        Some(Commands::Move {
            selector,
            destination,
            mailbox,
        }) => {
            // `mp archive` with a destination of the user's choosing
            // (PERSO-235): the same daemon call the TUI's `M` makes, resolved
            // against the selector's own account, and settled before it
            // answers. The destination is resolved by the daemon, as a role,
            // slug or label, so an unknown name is refused there with the
            // account's mailboxes listed.
            let account_config = account_for_selector(&selector, &account_config, &global_config)?;
            let moved = fetch_on_miss(
                &account_config,
                &selector,
                mailbox.as_deref(),
                StandIn::Refused { verb: "move" },
                async |selector: &str, mailbox: Option<&str>| {
                    routed_received_mutation(
                        &account_config.name,
                        "message.move",
                        selector,
                        mailbox,
                        &[("destination", destination.as_str())],
                    )
                    .await
                },
            )
            .await?;
            hand_back(
                &format!(
                    "{} moved {}\n  {} {}",
                    "\u{2713}".green(),
                    wire_str(&moved["selector"]),
                    "now".dimmed(),
                    wire_str(&moved["moved_to"]["selector"])
                ),
                wire_str(&moved["moved_to"]["selector"]),
            );
        }

        Some(Commands::Delete {
            selector,
            mailbox,
            force,
            sent,
        }) => {
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
                    fetch_on_miss(
                        &account_config,
                        &selector,
                        mailbox.as_deref(),
                        StandIn::Refused { verb: "delete" },
                        async |selector: &str, mailbox: Option<&str>| {
                            routed_received_mutation(
                                &account_config.name,
                                "message.delete",
                                selector,
                                mailbox,
                                &[],
                            )
                            .await
                        },
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
                &account_config,
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
            // the handle's own lifetime is what ends it. The body's inline
            // images open only when the message has no other attachment
            // (PERSO-165), so a signed PDF does not open its logos too.
            for part in mailypoppins::read_cmd::parts_to_open(&message.attachments) {
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

        Some(Commands::Save {
            selector,
            output,
            mailbox,
        }) => {
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
                &account_config,
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
            // rather than growing a copy per run. The body's inline images
            // are saved too, after the real attachments (PERSO-165): a
            // pasted screenshot or a phone photo is sent as one.
            let mut written: Vec<String> = Vec::new();
            for part in mailypoppins::read_cmd::parts_in_reading_order(&message.attachments) {
                let (handle, path, name) = routed_materialise(
                    &mut connection,
                    &account_config.name,
                    &message.selector,
                    part,
                )
                .await?;
                let name = mailypoppins::daemon::client::unique_name(name, &written);
                let bytes =
                    std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
                let out = dest.join(&name);
                std::fs::write(&out, &bytes)
                    .with_context(|| format!("writing {}", out.display()))?;
                release_handle(&mut connection, &handle).await;
                written.push(name);
            }
            for name in &written {
                let path = spelled.join(name).display().to_string();
                hand_back(&format!("{} {path}", "\u{2713}".green()), &path);
            }
        }

        // The `.eml` export (PERSO-174): `mp save`'s shape with one file, the
        // message itself. The daemon reads the stored raw message, or asks
        // the server for it when the row has none (every Graph row), and the
        // copy into the user's directory is this process's, for the reason
        // `mp save` gives.
        Some(Commands::Export {
            selector,
            output,
            mailbox,
        }) => {
            let account_config = account_for_selector(&selector, &account_config, &global_config)?;
            let spelled = output.unwrap_or_else(|| PathBuf::from("."));
            let dest = mailypoppins::daemon::client::absolutise(&spelled);

            let mut connection = daemon_connection().await;
            let message = routed_attachment_target(
                &mut connection,
                &account_config,
                &selector,
                mailbox.as_deref(),
            )
            .await?;
            let (handle, path, name) =
                routed_export(&mut connection, &account_config.name, &message.selector).await?;
            std::fs::create_dir_all(&dest)
                .with_context(|| format!("creating {}", dest.display()))?;
            let bytes =
                std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
            let out = dest.join(&name);
            std::fs::write(&out, &bytes).with_context(|| format!("writing {}", out.display()))?;
            release_handle(&mut connection, &handle).await;
            let shown = spelled.join(&name).display().to_string();
            hand_back(&format!("{} {shown}", "\u{2713}".green()), &shown);
        }

        // The read surface over the store (#0062): both offline, both reusing
        // the queries `store::read` already had. `mp show` is the single-message
        // half, addressed by the same selector grammar every other received
        // command takes, and resolved through `account_for_selector` first so a
        // cross-account selector opens the right store (the #0073 follow-up).
        Some(Commands::Show {
            selector,
            mailbox,
            json,
        }) => {
            let account_config = account_for_selector(&selector, &account_config, &global_config)?;
            let message = routed_show(&account_config, &selector, mailbox.as_deref()).await?;
            if json {
                println!("{}", mailypoppins::read_cmd::to_json(&message)?);
            } else {
                print!("{}", mailypoppins::read_cmd::render_show(&message));
            }
        }

        Some(Commands::ListMessages {
            mailbox,
            limit,
            json,
        }) => {
            routed_list_messages(&account_config, mailbox.as_deref(), limit, json).await?;
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
            json,
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
            let query_ast =
                mailypoppins::search::from_cli(&query, &flags).map_err(|e| anyhow!("{e}"))?;

            if local {
                // Resolve mailbox scope: --mailbox flag > in: directive > all.
                let mailbox_name = mailbox.or_else(|| query_ast.in_mailbox.clone());
                // The daemon's `message.search` resolves the name against the
                // account's sidebar (#0141), so a folder only the server names
                // is found and an unknown one is refused in its words.
                let mailbox_key = mailbox_name;
                // One hit past the limit tells a cut answer from one that
                // had exactly `limit` hits (PERSO-175).
                let mut hits = routed_search(
                    &account_config.name,
                    &query,
                    mailbox_key.as_deref(),
                    &flags,
                    limit.saturating_add(1),
                    full,
                )
                .await?;
                let truncated = hits.len() > limit;
                hits.truncate(limit);
                if json {
                    let results = mailypoppins::read_cmd::SearchResultsJson::local(
                        &account_config.name,
                        &query,
                        truncated,
                        hits.iter()
                            .map(|hit| {
                                mailypoppins::read_cmd::ListedMessage::from_wire(
                                    &account_config.name,
                                    &hit.mailbox,
                                    &hit.row,
                                    hit.body.clone(),
                                )
                            })
                            .collect(),
                    );
                    println!("{}", serde_json::to_string_pretty(&results)?);
                } else {
                    let rows: Vec<_> = hits
                        .into_iter()
                        .map(|hit| {
                            (
                                selected_row(&account_config.name, hit.row, &hit.mailbox),
                                hit.body,
                            )
                        })
                        .collect();
                    print!(
                        "{}",
                        mailypoppins::read_cmd::render_search(&account_config.name, &query, &rows)
                    );
                }
                // An early return leaves the check at the bottom of `main`
                // unrun, so it happens here instead. Every early return out of
                // a routed command owes this line.
                mailypoppins::daemon::client::enforce_routing(&command_label);
                return Ok(());
            }

            routed_server_search(
                &account_config,
                &query,
                mailbox.as_deref(),
                &flags,
                limit,
                full,
                json,
            )
            .await?;
        }

        // The index, the ranking and the cache are all the daemon's from
        // P4-U14; the glyphs, the tab-delimited projection and the
        // all-accounts loop stay here.
        Some(Commands::Contacts { action }) => match action {
            ContactsAction::Search {
                query,
                parsable,
                json,
                limit,
            } => {
                let acct = cli.account.clone();
                let name = pick_account_named(&global_config, acct.as_deref())?
                    .name
                    .clone();
                routed_contacts_search(&name, query.as_deref(), parsable, json, limit).await?;
            }
            ContactsAction::Rebuild => {
                let acct = cli.account.clone();
                routed_contacts_rebuild(&accounts_for(&global_config, acct.as_deref())?).await?;
            }
            ContactsAction::Stats => {
                let acct = cli.account.clone();
                let name = pick_account_named(&global_config, acct.as_deref())?
                    .name
                    .clone();
                routed_contacts_stats(&name).await?;
            }
        },

        Some(Commands::Signature { action }) => {
            let acct = cli.account.clone();
            let account = pick_account_named(&global_config, acct.as_deref())?
                .name
                .clone();
            routed_signature(&account, action).await?;
        }

        Some(Commands::Calendar { action }) => match action {
            CalendarAction::Rebuild => {
                let acct = cli.account.clone();
                routed_calendar_rebuild(&accounts_for(&global_config, acct.as_deref())?).await?;
            }
            CalendarAction::List { json } => {
                let account = match cli.account.as_deref() {
                    Some(named) => Some(
                        pick_account_named(&global_config, Some(named))?
                            .name
                            .clone(),
                    ),
                    None => None,
                };
                routed_calendar_list(account.as_deref(), json).await?;
            }
            CalendarAction::View {
                layout,
                on,
                from,
                to,
                accounts,
                calendars,
                hidden,
                zone,
                json,
            } => {
                let accounts = accounts
                    .iter()
                    .map(|named| {
                        Ok(pick_account_named(&global_config, Some(named))?
                            .name
                            .clone())
                    })
                    .collect::<Result<Vec<_>>>()?;
                let request = ViewRequest {
                    layout: layout.into(),
                    on,
                    range: from.zip(to),
                    accounts,
                    calendars,
                    hidden,
                    zone,
                };
                routed_calendar_view(request, json).await?;
            }
            CalendarAction::Show { calendar } => {
                let account = pick_account_named(&global_config, cli.account.as_deref())?;
                routed_calendar_visibility(&account.name, &calendar, true).await?;
            }
            CalendarAction::Hide { calendar } => {
                let account = pick_account_named(&global_config, cli.account.as_deref())?;
                routed_calendar_visibility(&account.name, &calendar, false).await?;
            }
        },

        Some(Commands::Event { action }) => {
            let account = pick_account_named(&global_config, cli.account.as_deref())?
                .name
                .clone();
            match action {
                EventAction::Show { event, json } => {
                    routed_event_show(&account, &event, json).await?;
                }
                EventAction::Respond {
                    event,
                    response,
                    answer,
                    json,
                } => {
                    routed_event_respond(&account, &event, response, &answer, json).await?;
                }
            }
        }

        Some(Commands::Hooks { action }) => {
            let acct = cli.account.clone();
            match action {
                HooksAction::List { json } => {
                    routed_hooks_list(&accounts_for(&global_config, acct.as_deref())?, json)
                        .await?;
                }
                HooksAction::Test {
                    hook,
                    selector,
                    mailbox,
                    json,
                } => {
                    let account = account_for_hook(&global_config, acct.as_deref(), &hook)?;
                    routed_hooks_test(&account, &hook, &selector, mailbox.as_deref(), json).await?;
                }
                HooksAction::Replay {
                    hook,
                    selector,
                    mailbox,
                } => {
                    let account = account_for_hook(&global_config, acct.as_deref(), &hook)?;
                    routed_hooks_replay(&account, &hook, &selector, mailbox.as_deref()).await?;
                }
            }
        }

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
                    println!(
                        "\n{}",
                        format!("\u{2500}\u{2500} {} \u{2500}\u{2500}", account.name).bold()
                    );
                }
                routed_store_gc(&account.name, dry_run, force).await?;
            }
        }

        Some(Commands::Cutover { dry_run }) => {
            let acct = cli.account.clone();
            routed_cutover(&accounts_for(&global_config, acct.as_deref())?, dry_run).await?;
        }

        Some(Commands::DumpKeys { json, .. }) => {
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
                ConfigAction::Init { .. } => {
                    let (path, exists, _) = routed_config_state().await?;
                    let mut wizard = DaemonWizard {
                        connection: operation_session().await,
                    };
                    cmd_config_init(&mut wizard, &path, exists).await?;
                }
                ConfigAction::Show { .. } => routed_config_show().await?,
                ConfigAction::SetPassword { which } => {
                    let acct_name = cli
                        .account
                        .clone()
                        .or_else(|| global_config.accounts.first().map(|a| a.name.clone()))
                        .unwrap_or_else(|| "main".to_string());
                    routed_set_password(&which, &acct_name).await?;
                }
                ConfigAction::AddAccount { .. } => {
                    let (path, exists, config) = routed_config_state().await?;
                    let names: Vec<String> =
                        config.accounts.iter().map(|a| a.name.clone()).collect();
                    let mut wizard = DaemonWizard {
                        connection: operation_session().await,
                    };
                    cmd_config_add_account(&mut wizard, &path, exists, &names).await?;
                }
                ConfigAction::Oauth2Login => {
                    let acct_name = cli.account.clone();
                    routed_oauth2_login(&global_config, acct_name.as_deref()).await?;
                }

                ConfigAction::ResetSecrets { .. } => routed_reset_secrets().await?,
                ConfigAction::Path { .. } => cmd_config_path(),
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
                // No selector, no subcommand: the TUI, on the account `-A`
                // names when it names one.
                let account = match cli.account.as_deref() {
                    Some(name) => {
                        Some(pick_account_named(&global_config, Some(name))?.name.clone())
                    }
                    None => None,
                };
                mailypoppins::tui::run(
                    mailypoppins::daemon::client::tui_connector(),
                    account.as_deref(),
                )?;
            }
        }
        Some(Commands::Account { action, .. }) => match action {
            AccountAction::List { daemon } => {
                let entries = if daemon {
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
        Some(Commands::Completions { .. }) => unreachable!("completions exit before dispatch"),
    }

    // The parity hook (P4-U2): with MAILYPOPPINS_DAEMON_REQUIRE set, a command
    // that answered from this process fails here instead of producing output a
    // parity test would credit to the daemon.
    mailypoppins::daemon::client::enforce_routing(&command_label);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{handed_back, is_drafts_selector, search_params, server_search_params};

    /// A server search over every mailbox names none, so the daemon runs its
    /// account-wide search, as the TUI and the desktop ask for it; `--mailbox`
    /// names its one mailbox.
    #[test]
    fn a_server_search_over_every_mailbox_names_none() {
        let every = server_search_params("tum", "invoice", None, &Default::default(), 50);
        assert_eq!(
            every,
            serde_json::json!({"account": "tum", "query": "invoice", "limit": 50})
        );
        let one = server_search_params("tum", "invoice", Some("Sent"), &Default::default(), 50);
        assert_eq!(one["mailboxes"], serde_json::json!(["Sent"]));
    }

    /// A search with no filter sends only the three keys every daemon
    /// accepts, so one that predates the field filters does not refuse it;
    /// a filter the user gave travels under its wire name (PERSO-175).
    #[test]
    fn a_search_sends_only_the_filters_the_user_gave() {
        let plain = search_params("tum", "invoice", &Default::default(), 20);
        assert_eq!(
            plain,
            serde_json::json!({"account": "tum", "query": "invoice", "limit": 20})
        );

        let flags = mailypoppins::search::Flags {
            has_attachment: true,
            from: Some("borrmann".into()),
            ..Default::default()
        };
        let filtered = search_params("tum", "", &flags, 20);
        assert_eq!(filtered["has_attachment"], true);
        assert_eq!(filtered["from"], "borrmann");
    }

    /// A terminal keeps the decorated lines and a pipe gets the bare value
    /// alone, however many lines the decoration spans (#0144).
    #[test]
    fn a_pipe_gets_the_bare_value_and_a_terminal_the_decorated_lines() {
        let decorated = "\u{2713} reply to mp://tum/inbox/a\nmp://tum/drafts/b";
        assert_eq!(handed_back(true, decorated, "mp://tum/drafts/b"), decorated);
        assert_eq!(
            handed_back(false, decorated, "mp://tum/drafts/b"),
            "mp://tum/drafts/b"
        );
    }

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

    /// `--all-accounts` and `-A` answer the same question, so naming both is
    /// refused rather than silently widening a send or a sweep to every account.
    #[test]
    fn all_accounts_conflicts_with_an_account_selector() {
        use clap::Parser;
        for args in [
            &["mp", "send-approved", "--all-accounts", "-A", "work", "-y"][..],
            &["mp", "store", "gc", "--all-accounts", "-A", "work"][..],
            &["mp", "sync", "--all-accounts", "--account", "work"][..],
            &["mp", "-A", "work", "sync", "--all-accounts"][..],
            &["mp", "-A", "work", "send-approved", "--all-accounts", "-y"][..],
            &["mp", "--account", "work", "store", "gc", "--all-accounts"][..],
        ] {
            let err = super::Cli::try_parse_from(args)
                .and_then(|cli| super::refuse_all_accounts_with_selector(&cli))
                .err();
            assert_eq!(
                err.map(|e| e.kind()),
                Some(clap::error::ErrorKind::ArgumentConflict),
                "{args:?} must be refused"
            );
        }
        assert!(
            super::Cli::try_parse_from(["mp", "send-approved", "--all-accounts", "-y"]).is_ok()
        );
    }

    /// `-A` on a command that acts on no account is refused, written before or
    /// after the subcommand, and is not in that command's help; on a command
    /// that takes an account it still parses either way (2026-10-22).
    #[test]
    fn the_account_selector_is_refused_where_it_names_no_account() {
        use clap::{CommandFactory, Parser};
        for (args, label) in [
            (&["mp", "daemon", "status", "-A", "x"][..], "daemon status"),
            (&["mp", "-A", "x", "daemon", "stop"][..], "daemon stop"),
            (&["mp", "daemon", "-A", "x", "status"][..], "daemon status"),
            (&["mp", "completions", "zsh", "-A", "x"][..], "completions"),
            (&["mp", "--account", "x", "dump-keys"][..], "dump-keys"),
            (&["mp", "account", "list", "-A", "x"][..], "account list"),
            (&["mp", "config", "path", "-A", "x"][..], "config path"),
            (&["mp", "-A", "x", "config", "show"][..], "config show"),
            (&["mp", "config", "init", "-A", "x"][..], "config init"),
            (
                &["mp", "config", "add-account", "-A", "x"][..],
                "config add-account",
            ),
            (
                &["mp", "config", "reset-secrets", "-A", "x"][..],
                "config reset-secrets",
            ),
        ] {
            let cli = super::Cli::try_parse_from(args)
                .unwrap_or_else(|e| panic!("{args:?} must parse before the refusal: {e}"));
            let err = super::refuse_account_where_meaningless(&cli, label).err();
            assert_eq!(
                err.map(|e| e.kind()),
                Some(clap::error::ErrorKind::ArgumentConflict),
                "{args:?} must be refused"
            );
        }
        for args in [
            &["mp", "daemon", "status"][..],
            &["mp", "-A", "x", "config", "set-password", "smtp"][..],
            &["mp", "contacts", "stats", "-A", "x"][..],
        ] {
            let cli = super::Cli::try_parse_from(args).expect("parses");
            assert!(
                super::refuse_account_where_meaningless(&cli, "x").is_ok(),
                "{args:?} is accepted"
            );
        }
        let mut command = super::Cli::command();
        let status = command
            .find_subcommand_mut("daemon")
            .and_then(|daemon| daemon.find_subcommand_mut("status"))
            .expect("mp daemon status")
            .render_help()
            .to_string();
        assert!(!status.contains("--account"), "{status}");
    }

    /// `-s` and `--no-signature` belong to the commands that write a
    /// signature: `new`, `reply`, `forward`, and `send` with `--invite`.
    #[test]
    fn the_signature_flags_are_only_on_the_draft_writers() {
        use clap::Parser;
        for args in [
            &["mp", "new", "memo", "-s", "work"][..],
            &["mp", "reply", "msg@example.com", "--no-signature"][..],
            &["mp", "forward", "msg@example.com", "--signature", "work"][..],
            &["mp", "send", "--invite", "-s", "work"][..],
        ] {
            assert!(super::Cli::try_parse_from(args).is_ok(), "{args:?} parses");
        }
        for args in [
            &["mp", "-s", "work", "new", "memo"][..],
            &["mp", "list", "--no-signature"][..],
            &["mp", "sync", "-s", "work"][..],
            &["mp", "send", "memo", "-s", "work"][..],
        ] {
            assert!(
                super::Cli::try_parse_from(args).is_err(),
                "{args:?} is refused"
            );
        }
    }

    /// `mp completions zsh` renders the whole command tree as a zsh completion
    /// function named for the `mp` binary, not the clap `mailypoppins` name.
    #[test]
    fn completions_render_a_zsh_script_for_mp() {
        use clap::Parser;
        let cli = super::Cli::try_parse_from(["mp", "completions", "zsh"]).expect("parses");
        assert!(matches!(
            cli.command,
            Some(super::Commands::Completions {
                shell: clap_complete::Shell::Zsh,
                ..
            })
        ));
        let mut out = Vec::new();
        super::write_completions(clap_complete::Shell::Zsh, &mut out).unwrap();
        let script = String::from_utf8(out).expect("UTF-8");
        assert!(script.contains("#compdef mp"), "{script:.200}");
        assert!(script.contains("_mp"));
        assert!(script.contains("daemon"));
        assert!(script.contains("completions"));
    }

    /// The global `-A` / `--account` reaches every subcommand, including the
    /// ones that used to declare a local `--account` and so rejected `-A`.
    #[test]
    fn the_global_account_selector_works_on_every_per_account_subcommand() {
        use clap::Parser;
        for args in [
            &["mp", "contacts", "stats", "-A", "x"][..],
            &["mp", "contacts", "search", "ada", "-A", "x"][..],
            &["mp", "contacts", "rebuild", "--account", "x"][..],
            &["mp", "calendar", "rebuild", "-A", "x"][..],
            &["mp", "calendar", "list", "-A", "x"][..],
            &["mp", "calendar", "hide", "cal-1", "-A", "x"][..],
            &["mp", "event", "show", "ev-1", "-A", "x"][..],
            &["mp", "event", "respond", "ev-1", "accept", "-A", "x"][..],
            &["mp", "cutover", "--dry-run", "-A", "x"][..],
            &["mp", "config", "set-password", "smtp", "-A", "x"][..],
            &["mp", "config", "oauth2-login", "--account", "x"][..],
        ] {
            let cli = super::Cli::try_parse_from(args)
                .unwrap_or_else(|e| panic!("{args:?} must parse: {e}"));
            assert_eq!(cli.account.as_deref(), Some("x"), "{args:?}");
        }
    }

    /// `mp calendar view` takes `-A` more than once, before or after the
    /// subcommand, and every account it names reaches the view.
    #[test]
    fn calendar_view_takes_several_accounts() {
        use clap::Parser;
        let accounts = |args: &[&str]| match super::Cli::try_parse_from(args)
            .unwrap_or_else(|e| panic!("{args:?} must parse: {e}"))
            .command
        {
            Some(super::Commands::Calendar {
                action: super::CalendarAction::View { accounts, .. },
            }) => accounts,
            _ => panic!("{args:?} is a calendar view"),
        };
        assert_eq!(
            accounts(&["mp", "calendar", "view", "-A", "a", "-A", "b"]),
            ["a", "b"]
        );
        assert_eq!(accounts(&["mp", "-A", "a", "calendar", "view"]), ["a"]);
        assert!(accounts(&["mp", "calendar", "view", "week"]).is_empty());
        assert_eq!(
            accounts(&["mp", "calendar", "view", "day", "--on", "-1", "-A", "a"]),
            ["a"]
        );
    }
}
