//! Pure presentation over a typed event payload (P3b-U6).
//!
//! The daemon publishes data; a client decides how to say it. These functions
//! are that decision for the one payload two surfaces already render today, and
//! they take the payload and nothing else: no `App`, no config, no colour. A
//! glyph's colour is a terminal's business and a GUI has neither, so the
//! callers keep their own colouring around these strings.
//!
//! Every literal below is lifted from the current tree, so the daemon-era line
//! is the line a user reads now: [`sync_status_line`] is
//! `src/tui/helpers.rs::finish_sync` clause for clause, with the suffix
//! `drain_pending_ops` appends, and [`sync_cli_lines`] is the `println!`
//! sequence of `src/main.rs`'s `sync_one_account`, glyph included. Two lines
//! have no CLI original because today's `mp sync` cannot reach the state - it
//! passes no body-fetch deadline and it reports a mutation drain with a
//! `completed` count the payload does not carry - and they take the TUI's
//! wording at the glyph their severity contribution earns.

use mp_protocol::events::SyncCompleted;

/// What a drain report line carries when it came from the tail of a tick.
///
/// `mp sync` drains at both ends (#0114) and both ends report in the same
/// words, so without this the four lines of a tick that had work queued at both
/// ends would be two indistinguishable pairs. The client derives it from the
/// phase the daemon reported
/// (`mailypoppins::daemon::runtime::account::Phase::as_str`) and from nothing
/// else.
pub const TAIL_LABEL: &str = " (after sync)";

/// The prefix every line of a `--dry-run` pass carries, after its glyph.
const DRY_RUN: &str = "[dry-run] ";

/// `mp sync`'s report of one outbox drain: what went out, and what is still
/// owed.
///
/// `still_pending` is the rows this drain left behind, whatever the reason;
/// splitting "open" from "awaiting submission" is the outbox's own vocabulary
/// and not a distinction a sync summary makes.
pub fn outbox_drain_line(tail: bool, completed: u64, still_pending: u64) -> String {
    format!(
        "  \u{21bb} outbox{}: {completed} completed, {still_pending} still pending",
        label(tail)
    )
}

/// `mp sync`'s report of one mutation-queue drain.
pub fn mutations_drain_line(tail: bool, completed: u64, failed: u64) -> String {
    format!(
        "  \u{21bb} mutations{}: {completed} completed, {failed} failed",
        label(tail)
    )
}

/// A drain that could not run at all.
///
/// A warning and not a failure: the queue is retried on the next tick, so a
/// queue that could not be drained must not turn a sync that worked into a
/// failed command. It carries no [`TAIL_LABEL`] because only the head drain
/// reports this way; by the tail the outcome is already decided.
pub fn drain_failed_line(error: &str) -> String {
    format!("  \u{26a0} mutations: drain failed: {error}")
}

/// The status line of a `mutations.rolled_back` event: a drain outside a tick
/// rolled `failed` queued mutations back (#0133).
///
/// The words of [`sync_status_line`]'s rollback suffix, so a rollback reads
/// the same whichever drain found it, and they carry the substring the TUI
/// reads to show the line as a warning.
pub fn mutations_rolled_back_line(failed: u64) -> String {
    format!("{failed} mutation(s) failed and were rolled back (see the log)")
}

/// The line a contact index build the cache guard (#0067) refused earns:
/// `saved` is `refused_empty` or `refused_shrunk`, `contacts` what the build
/// found and `kept` what the kept index holds; `None` for any other verdict.
///
/// One wording for a `contact.rebuild` settle and for the build a
/// `contact.search` reports in its `rebuild`, in every client.
pub fn refused_rebuild_line(saved: &str, contacts: u64, kept: u64) -> Option<String> {
    match saved {
        "refused_empty" => Some(format!("Contacts rebuild found none, kept {kept} cached")),
        "refused_shrunk" => Some(format!(
            "Contacts rebuild found only {contacts}, kept {kept} cached"
        )),
        _ => None,
    }
}

/// The line under a server search the limit cut (PERSO-175): more matched
/// than the `shown` hits, and `remedy`, the client's own way to see them
/// (`raise -n to see them` in `mp search`, a narrower search in the TUI and
/// the desktop, which take no limit).
pub fn more_results_line(shown: usize, remedy: &str) -> String {
    format!("More results than the {shown} shown: {remedy}.")
}

/// The line under a server search Microsoft Graph's `$search` cap cut
/// (`ServerSearchSettled::capped`), where no larger limit helps.
pub fn graph_search_cap_line() -> String {
    format!(
        "Microsoft Graph stops a search at {} results: narrow the query \
         (after:, before:, in:) to reach the rest.",
        mp_protocol::listing::GRAPH_SEARCH_CAP
    )
}

/// [`TAIL_LABEL`] for a tail phase, nothing for the head's.
fn label(tail: bool) -> &'static str {
    if tail {
        TAIL_LABEL
    } else {
        ""
    }
}

/// The TUI's one-line status message for a finished tick.
///
/// A failed tick reports the failure and nothing else: counts from a pass that
/// did not finish would read as a sync that happened. The account is always
/// named, because a pure function cannot count a client's accounts.
pub fn sync_status_line(outcome: &SyncCompleted) -> String {
    if let Some(error) = &outcome.error {
        return format!("Fetch failed ({}): {error}", outcome.account);
    }

    let mut line = format!(
        "Synced: {} new, {} existing",
        outcome.saved, outcome.skipped
    );
    if outcome.flags_updated > 0 {
        line.push_str(&format!(", {} status updated", outcome.flags_updated));
    }
    if outcome.uid_rebound > 0 {
        line.push_str(&format!(", {} renumbered", outcome.uid_rebound));
    }
    if outcome.pruned > 0 {
        line.push_str(&format!(", {} no longer in this mailbox", outcome.pruned));
    }
    if outcome.prunes_deferred > 0 {
        line.push_str(&format!(
            ", {} removal(s) held back (incomplete pass, run a full sync)",
            outcome.prunes_deferred
        ));
    }
    if outcome.bodies_truncated > 0 {
        line.push_str(&format!(
            ", {} mailbox(es) stopped at the fetch deadline (resuming next sync)",
            outcome.bodies_truncated
        ));
    }
    if !outcome.non_converging.is_empty() {
        line.push_str(&format!(
            ", fetch not converging on {} (see the log)",
            outcome.non_converging.join(", ")
        ));
    }
    if outcome.failed_mutations > 0 {
        line.push_str(&format!(
            "; {} mutation(s) failed and were rolled back (see the log)",
            outcome.failed_mutations
        ));
    }
    line
}

/// `mp sync`'s lines for a finished tick, one per state it reached, in the
/// order `sync_one_account` prints them and uncoloured.
pub fn sync_cli_lines(outcome: &SyncCompleted) -> Vec<String> {
    cli_lines(outcome, false)
}

/// The same lines for a `--dry-run` pass: every one of them prefixed after its
/// glyph, and the ingest count phrased as what *would* be downloaded.
pub fn sync_cli_lines_dry_run(outcome: &SyncCompleted) -> Vec<String> {
    cli_lines(outcome, true)
}

fn cli_lines(outcome: &SyncCompleted, dry_run: bool) -> Vec<String> {
    let at = if dry_run { DRY_RUN } else { "" };
    if let Some(error) = &outcome.error {
        return vec![format!("✗ {}: {error}", outcome.account)];
    }

    let mut lines = Vec::new();
    if outcome.skipped > 0 {
        lines.push(format!(
            "✓ {at}Synced: {} new, {} already present",
            outcome.saved, outcome.skipped
        ));
    } else {
        lines.push(format!(
            "✓ {at}Synced: {} email(s) {}",
            outcome.saved,
            if dry_run { "to download" } else { "ingested" }
        ));
    }
    if outcome.flags_updated > 0 {
        lines.push(format!(
            "ℹ {at}Status updated on {} message(s)",
            outcome.flags_updated
        ));
    }
    if outcome.uid_rebound > 0 {
        lines.push(format!(
            "ℹ {at}Matched {} moved or renumbered message(s) to their new server UIDs",
            outcome.uid_rebound
        ));
    }
    if outcome.pruned > 0 {
        lines.push(format!(
            "ℹ {at}{} message(s) left their mailbox on the server",
            outcome.pruned
        ));
    }
    if outcome.prunes_deferred > 0 {
        lines.push(format!(
            "⚠ {at}{} removal(s) held back: this pass did not see every message, \
             run a full sync to apply them",
            outcome.prunes_deferred
        ));
    }
    // No CLI original: `mp sync` is the explicit recovery path and carries no
    // body-fetch deadline, so it never sees this. The TUI's wording at `ℹ`,
    // because a deadline stop is progress (#0113).
    if outcome.bodies_truncated > 0 {
        lines.push(format!(
            "ℹ {at}{} mailbox(es) stopped at the fetch deadline (resuming next sync)",
            outcome.bodies_truncated
        ));
    }
    for name in &outcome.non_converging {
        lines.push(format!(
            "⚠ {at}'{name}' downloaded the same messages again: the fetch is not converging, \
             see the log"
        ));
    }
    // No CLI original either: today's `mp sync` prints a `completed`/`failed`
    // pair the payload cannot rebuild. The TUI's wording at `⚠`, which is the
    // severity a rolled-back mutation earns (#0039).
    if outcome.failed_mutations > 0 {
        lines.push(format!(
            "⚠ {at}{} mutation(s) failed and were rolled back (see the log)",
            outcome.failed_mutations
        ));
    }
    lines
}

// ---------------------------------------------------------------------------
// The send slice (P4-U12)
// ---------------------------------------------------------------------------

use mp_protocol::send::{
    ApprovedOutcome, OutboxListing, OutboxRetryOutcome, OutboxRow, SendOutcome,
};

/// What either confirmation prompt prints when the answer is not yes.
///
/// One constant because two commands print it and the parity rows compare it
/// literally; a second copy in the CLI would be a second thing to get wrong.
pub const CANCELLED_LINE: &str = "Cancelled.";

/// Every line `mp outbox list` prints for one account, in order, glyphs
/// included and colourless.
///
/// Three shapes, and the client picks between them from the payload rather than
/// from a store: an account that never queued anything, an account whose outbox
/// is clear, and the rows with their annotations and the counts line under them.
pub fn outbox_cli_lines(listing: &OutboxListing) -> Vec<String> {
    if !listing.ever_used {
        return vec![format!(
            "  \u{b7} nothing has been queued for {} yet",
            listing.account
        )];
    }
    if listing.rows.is_empty() {
        return vec![format!(
            "  \u{2713} the outbox for {} is clear",
            listing.account
        )];
    }
    let mut lines = Vec::new();
    for row in &listing.rows {
        lines.extend(outbox_row_lines(row));
    }
    lines.push(format!(
        "  \u{21bb} {} working, {} failed, {} partly delivered",
        listing.counts.open, listing.counts.failed, listing.counts.partial
    ));
    lines
}

/// One row and everything indented under it.
fn outbox_row_lines(row: &OutboxRow) -> Vec<String> {
    // The state column is padded to a fixed width, so a listing reads as a
    // table; the caller may colour the word inside it, which is what costs the
    // padding on a terminal, exactly as it did before the daemon.
    let state = if row.partial { "partial" } else { &row.state };
    let mut lines = vec![format!(
        "  {:>4}  {:<20} {}  {}",
        row.id,
        state,
        local_minute(row.updated),
        row.message_id
    )];
    if let Some(target) = row.target_mailbox.as_deref() {
        lines.push(format!("        sent copy -> {target}"));
    }
    if row.never_submitted {
        lines.push("        never submitted; the next sync sends it".to_string());
    }
    for (address, reason) in &row.rejected {
        lines.push(format!("        never delivered to: {address} ({reason})"));
    }
    // A `done` row owes nobody anything more, whatever its envelope still lists:
    // the submission is over and the note above says who missed out.
    if !row.outstanding.is_empty() && row.state != "done" {
        lines.push(format!(
            "        still to deliver to: {}",
            row.outstanding.join(", ")
        ));
    }
    if let Some(error) = row.last_error.as_deref() {
        let label = if row.partial {
            "outcome:"
        } else {
            "last error:"
        };
        lines.push(format!("        {label} {error}"));
    }
    lines
}

/// A unix timestamp as local `YYYY-MM-DD HH:MM`, or `-` when it is unset.
///
/// Local, because the operator reading the listing is in one place and the row
/// is about something that happened to them.
fn local_minute(ts: i64) -> String {
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

/// The line `mp outbox discard` prints: the row and the message, because an
/// operator who just dropped a message is owed both.
pub fn outbox_discard_line(row_id: i64, message_id: &str) -> String {
    format!("  \u{2713} discarded row {row_id} ({message_id}); its bytes are released")
}

/// The two lines a retry prints, and the third it prints only when a copy was
/// filed.
pub fn outbox_retry_lines(outcome: &OutboxRetryOutcome) -> Vec<String> {
    let mut lines = vec![format!(
        "  \u{21bb} row {} is queued again; sending it now",
        outcome.row_id
    )];
    lines.push(match outcome.state.as_deref() {
        Some(state) => format!("  \u{2713} row {} is now {state}", outcome.row_id),
        None => format!("  \u{2713} row {} is gone", outcome.row_id),
    });
    if outcome.completed > 0 {
        lines.push(format!(
            "  \u{2713} {} sent copy/copies filed",
            outcome.completed
        ));
    }
    lines
}

/// Every line `mp send` prints about an outcome, in order.
///
/// A send nobody received produces the recipient lines and nothing else: the
/// sentence that names it a failure is the command's exit code talking, and
/// that stays the client's decision.
pub fn send_cli_lines(outcome: &SendOutcome) -> Vec<String> {
    let mut lines: Vec<String> = outcome.recipients.iter().map(recipient_line).collect();
    let delivered = outcome.recipients.iter().filter(|r| r.delivered).count();
    if delivered == 0 {
        return lines;
    }
    if let Some(error) = outcome.settle_error.as_deref() {
        lines.push(format!(
            "\u{26a0} (sent but failed to retire draft: {error})"
        ));
    }
    let failed = outcome.recipients.len() - delivered;
    if failed == 0 {
        lines.push(format!(
            "\u{2713} Email sent successfully to all {} recipient(s) [{}]",
            outcome.recipients.len(),
            outcome.status_line
        ));
    } else {
        lines.push(format!(
            "\u{26a0} Partial send: {delivered} succeeded, {failed} failed [{}] \
             (marked as sent -- see logs for details)",
            outcome.status_line
        ));
    }
    lines
}

/// One recipient's own line, as `mp send` and `mp event` print it.
pub fn recipient_line(recipient: &mp_protocol::send::RecipientOutcome) -> String {
    if recipient.delivered {
        format!("  \u{2713} {} ({})", recipient.address, recipient.role)
    } else {
        format!(
            "  \u{2717} {} ({}): {}",
            recipient.address,
            recipient.role,
            recipient.error.as_deref().unwrap_or("unknown error")
        )
    }
}

/// The one line `mp send-approved` prints per draft, after the
/// `Sending to …` prefix it is appended to.
///
/// A send that never reached the transport at all carries no recipients, and
/// then the line is the reason it did not.
pub fn send_approved_line(outcome: &SendOutcome) -> String {
    if outcome.recipients.is_empty() {
        return format!("\u{2717} {}", outcome.status_line);
    }
    let delivered = outcome.recipients.iter().filter(|r| r.delivered).count();
    if delivered == 0 {
        return match outcome.recipients.iter().find_map(|r| r.error.as_deref()) {
            Some(reason) => format!(
                "\u{2717} all recipients failed: {reason} [{}]",
                outcome.status_line
            ),
            None => format!("\u{2717} all recipients failed [{}]", outcome.status_line),
        };
    }
    if let Some(error) = outcome.settle_error.as_deref() {
        return format!("\u{26a0} (sent but failed to update status: {error})");
    }
    if delivered == outcome.recipients.len() {
        format!("\u{2713} [{}]", outcome.status_line)
    } else {
        format!(
            "\u{26a0} (partial: {delivered}/{} recipients) [{}]",
            outcome.recipients.len(),
            outcome.status_line
        )
    }
}

/// The summary line the batch ends each account with.
pub fn send_approved_summary(outcome: &ApprovedOutcome) -> String {
    format!(
        "Summary {}: {} sent, {} failed",
        outcome.account, outcome.sent, outcome.failed
    )
}

// ---------------------------------------------------------------------------
// The admin slice (P4-U14)
// ---------------------------------------------------------------------------

/// The line `mp config oauth2-login` prints before the flow starts, for the
/// Graph account it signs in.
pub fn oauth2_start_line(account: &str) -> String {
    format!("\u{2139} Starting OAuth2 device code flow for account '{account}' (Graph API)")
}

/// The block a device-code login shows a human, blank lines included.
///
/// A pure function of the two values `config.oauth2_login` reports as its first
/// `operation.progress`, so a GUI reproduces the terminal's block without
/// knowing anything about the flow that produced it.
pub fn oauth2_device_code_lines(verification_uri: &str, user_code: &str) -> String {
    format!(
        "\n  To sign in, open a browser and go to:\n\n    {verification_uri}\n\n  \
         Enter the code: {user_code}\n\n"
    )
}

/// The line a finished login prints.
pub fn oauth2_stored_line(account: &str) -> String {
    format!("\u{2713} OAuth2 token acquired and cached for account '{account}'")
}

// ---------------------------------------------------------------------------
// The server calendar (PERSO-200)
// ---------------------------------------------------------------------------

use mp_protocol::calendar::{CalendarEvent, CalendarInfo};
use mp_protocol::reminder::CalendarReminder;

/// The calendars' names by account and id, for the tag an event line
/// carries in `mp calendar view` and in the TUI's agenda.
pub type CalendarNames = std::collections::HashMap<(String, String), String>;

/// [`CalendarNames`] from a `calendar.list` answer.
pub fn calendar_names(calendars: &[CalendarInfo]) -> CalendarNames {
    calendars
        .iter()
        .map(|c| ((c.account.clone(), c.id.clone()), c.name.clone()))
        .collect()
}

/// The `account/calendar` tag of an event, the calendar by name when the
/// list names it.
pub fn calendar_tag(event: &CalendarEvent, names: &CalendarNames) -> String {
    let name = names
        .get(&(event.account.clone(), event.calendar.clone()))
        .map(String::as_str)
        .unwrap_or(&event.calendar);
    format!("{}/{}", event.account, name)
}

/// A span of RFC 3339 instants read in `zone`: `Tue 2026-07-07 09:00-10:00`,
/// or its two ends when it spans days; the raw values when one does not
/// parse. The timed line of an event card, and an attendee's proposal.
pub fn time_span<Z: chrono::TimeZone>(start: &str, end: &str, zone: &Z) -> String
where
    Z::Offset: std::fmt::Display,
{
    let instant = |raw: &str| chrono::DateTime::parse_from_rfc3339(raw).ok();
    let (Some(from), Some(to)) = (instant(start), instant(end)) else {
        return format!("{start} to {end}");
    };
    let (from, to) = (from.with_timezone(zone), to.with_timezone(zone));
    if from.date_naive() == to.date_naive() {
        format!(
            "{}-{}",
            from.format("%a %Y-%m-%d %H:%M"),
            to.format("%H:%M")
        )
    } else {
        format!(
            "{} to {}",
            from.format("%a %Y-%m-%d %H:%M"),
            to.format("%a %Y-%m-%d %H:%M")
        )
    }
}

/// How an event card says where an event sits in its series: the rule in
/// words, or that it is one occurrence when the rule is unknown, with a
/// changed occurrence (`exception`) saying so; `None` for an event in no
/// series.
pub fn series_line(event: &CalendarEvent) -> Option<String> {
    let changed = event.kind == "exception";
    let instance = changed
        || event.kind == "occurrence"
        || event.series_master_id.is_some()
        || event.recurrence_id.is_some();
    match (event.recurrence.as_deref(), changed) {
        (Some(rule), false) => Some(rule.to_string()),
        (Some(rule), true) => Some(format!("{rule}, this occurrence changed")),
        (None, true) => Some("one occurrence of a series, changed".to_string()),
        (None, false) if instance => Some("one occurrence of a series".to_string()),
        (None, false) => None,
    }
}

/// How an event card names its online meeting: the service (`Teams
/// meeting`), then the join link when there is one; `None` for an event
/// that is no online meeting.
pub fn online_meeting_line(event: &CalendarEvent) -> Option<String> {
    let url = event
        .online_meeting_url
        .as_deref()
        .filter(|url| !url.trim().is_empty());
    if !event.is_online_meeting && url.is_none() {
        return None;
    }
    let label = match event.online_meeting_provider.as_deref() {
        Some("teams-for-business") => "Teams meeting",
        Some("skype-for-business") => "Skype for Business meeting",
        Some("skype-for-consumer") => "Skype meeting",
        _ => "Online meeting",
    };
    Some(match url {
        Some(url) => format!("{label}  {url}"),
        None => format!("{label}, no join link yet"),
    })
}

/// When a reminder's event starts, from `now` and read in `zone`: `in 15 min
/// (09:00)`, `now (09:00)`, `started 5 min ago (09:00)`, or for an all-day
/// event `all day today`, `all day tomorrow` or `all day Mon 9 Nov`. The line
/// the CLI, the TUI and the desktop notification show under the summary.
pub fn reminder_when<Z: chrono::TimeZone>(
    reminder: &CalendarReminder,
    now: chrono::DateTime<chrono::Utc>,
    zone: &Z,
) -> String
where
    Z::Offset: std::fmt::Display,
{
    let Ok(start) = chrono::DateTime::parse_from_rfc3339(&reminder.start) else {
        return reminder.start.clone();
    };
    let start = start.with_timezone(zone);
    if reminder.all_day {
        let today = now.with_timezone(zone).date_naive();
        let day = start.date_naive();
        return match (day - today).num_days() {
            0 => "all day today".to_string(),
            1 => "all day tomorrow".to_string(),
            _ => format!("all day {}", start.format("%a %-d %b")),
        };
    }
    let clock = start.format("%H:%M");
    let minutes = (start.with_timezone(&chrono::Utc) - now).num_seconds() / 60;
    let span = |minutes: i64| match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m} min"),
    };
    match minutes {
        0 => format!("now ({clock})"),
        m if m > 0 => format!("in {} ({clock})", span(m)),
        m => format!("started {} ago ({clock})", span(-m)),
    }
}

/// The body of a reminder's notification: when its event starts and, when
/// it has one, where.
pub fn reminder_body<Z: chrono::TimeZone>(
    reminder: &CalendarReminder,
    now: chrono::DateTime<chrono::Utc>,
    zone: &Z,
) -> String
where
    Z::Offset: std::fmt::Display,
{
    let mut body = reminder_when(reminder, now, zone);
    if let Some(location) = reminder
        .location
        .as_deref()
        .filter(|l| !l.trim().is_empty())
    {
        body.push_str(&format!(", {location}"));
    }
    body
}

/// A reminder's notification now, read in the local zone: its summary as the
/// title (`(no title)` when it has none) and [`reminder_body`].
pub fn reminder_notification(reminder: &CalendarReminder) -> (String, String) {
    let title = if reminder.summary.trim().is_empty() {
        "(no title)".to_string()
    } else {
        reminder.summary.clone()
    };
    (
        title,
        reminder_body(reminder, chrono::Utc::now(), &chrono::Local),
    )
}

/// One reminder as a line: the summary, when, and where.
pub fn reminder_line<Z: chrono::TimeZone>(
    reminder: &CalendarReminder,
    now: chrono::DateTime<chrono::Utc>,
    zone: &Z,
) -> String
where
    Z::Offset: std::fmt::Display,
{
    let summary = if reminder.summary.trim().is_empty() {
        "(no title)"
    } else {
        reminder.summary.as_str()
    };
    format!("{summary}, {}", reminder_body(reminder, now, zone))
}

/// The one-character mark of the user's own answer to an event.
pub fn response_mark(own_response: &str) -> char {
    match own_response {
        "accepted" => '\u{2713}',
        "tentative" => '~',
        "declined" => '\u{2717}',
        "needs-action" => '?',
        "organizer" => '\u{2605}',
        _ => ' ',
    }
}

/// What each free/busy glyph means, the legend printed beside a row.
pub const AVAILABILITY_LEGEND: &str =
    "\u{b7} free  \u{2592} tentative  \u{2588} busy  \u{2593} away  \u{2591} elsewhere  ? unknown";

/// The glyph a slot of a `calendar.freebusy` view shows, by its digit: `·`
/// free, `▒` tentative, `█` busy, `▓` away, `░` working elsewhere, `?` for
/// anything else, a slot past the view's end included.
pub fn availability_glyph(digit: char) -> char {
    match digit {
        '0' => '\u{b7}',
        '1' => '\u{2592}',
        '2' => '\u{2588}',
        '3' => '\u{2593}',
        '4' => '\u{2591}',
        _ => '?',
    }
}

/// The glyphs of `count` slots of `view` from slot `first`; a slot before
/// the view's start or past its end is unknown.
pub fn availability_row(view: &str, first: i64, count: usize) -> String {
    let digits: Vec<char> = view.chars().collect();
    (0..count as i64)
        .map(|offset| {
            let at = first + offset;
            let digit = usize::try_from(at)
                .ok()
                .and_then(|at| digits.get(at).copied())
                .unwrap_or('?');
            availability_glyph(digit)
        })
        .collect()
}

/// The slot of a view starting at `start` (RFC 3339) with slots of
/// `interval` minutes that `at` falls in; `None` for an unreadable start.
pub fn slot_of(start: &str, interval: u32, at: chrono::DateTime<chrono::Utc>) -> Option<i64> {
    let start = chrono::DateTime::parse_from_rfc3339(start).ok()?;
    let minutes = (at - start.with_timezone(&chrono::Utc)).num_minutes();
    Some(minutes.div_euclid(i64::from(interval.max(1))))
}

/// The word a schedule block's status reads as.
pub fn availability_word(status: &str) -> &'static str {
    match status {
        "free" => "free",
        "tentative" => "tentative",
        "busy" => "busy",
        "oof" => "away",
        "working-elsewhere" => "elsewhere",
        _ => "unknown",
    }
}

#[cfg(test)]
mod availability_tests {
    use super::{availability_row, availability_word, slot_of};
    use chrono::{TimeZone, Utc};

    /// Each digit has its glyph, and a slot outside the view is unknown.
    #[test]
    fn a_view_reads_as_one_glyph_per_slot() {
        assert_eq!(
            availability_row("012345", 0, 6),
            "\u{b7}\u{2592}\u{2588}\u{2593}\u{2591}?"
        );
        assert_eq!(availability_row("02", -1, 4), "?\u{b7}\u{2588}?");
        assert_eq!(availability_row("", 0, 2), "??");
        assert_eq!(availability_word("oof"), "away");
        assert_eq!(availability_word("working-elsewhere"), "elsewhere");
    }

    /// An instant falls in the slot its minutes from the start count to,
    /// one before the start in a negative slot.
    #[test]
    fn an_instant_falls_in_its_slot() {
        let start = "2026-11-03T23:00:00Z";
        let at = |h: u32, m: u32| Utc.with_ymd_and_hms(2026, 11, 4, h, m, 0).unwrap();
        assert_eq!(slot_of(start, 30, at(6, 0)), Some(14));
        assert_eq!(slot_of(start, 30, at(6, 29)), Some(14));
        assert_eq!(
            slot_of(
                start,
                30,
                Utc.with_ymd_and_hms(2026, 11, 3, 22, 59, 0).unwrap()
            ),
            Some(-1)
        );
        assert_eq!(slot_of("soon", 30, at(6, 0)), None);
    }
}

#[cfg(test)]
mod reminder_tests {
    use super::{reminder_body, reminder_line, reminder_notification, reminder_when};
    use chrono::{TimeZone, Utc};
    use mp_protocol::reminder::CalendarReminder;

    fn reminder(start: &str, all_day: bool) -> CalendarReminder {
        CalendarReminder {
            account: "evoqs".into(),
            event: "ev".into(),
            summary: "Standup".into(),
            start: start.into(),
            all_day,
            location: Some("Room 2.14".into()),
            online_meeting_url: None,
            minutes_before: 15,
            presenter: None,
            presenter_connection: None,
        }
    }

    /// Ahead, at, and past the start, in the viewer's zone, and an all-day
    /// event by its day.
    #[test]
    fn a_reminder_says_when_its_event_starts() {
        let berlin = chrono::FixedOffset::east_opt(3600).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 11, 4, 8, 45, 0).unwrap();
        let at = |start: &str| reminder_when(&reminder(start, false), now, &berlin);
        assert_eq!(at("2026-11-04T09:00:00Z"), "in 15 min (10:00)");
        assert_eq!(at("2026-11-04T10:30:00Z"), "in 1 h 45 min (11:30)");
        assert_eq!(at("2026-11-04T09:45:00Z"), "in 1 h (10:45)");
        assert_eq!(at("2026-11-04T08:45:30Z"), "now (09:45)");
        assert_eq!(at("2026-11-04T08:40:00Z"), "started 5 min ago (09:40)");
        let day = |start: &str| reminder_when(&reminder(start, true), now, &berlin);
        assert_eq!(day("2026-11-03T23:00:00Z"), "all day today");
        assert_eq!(day("2026-11-04T23:00:00Z"), "all day tomorrow");
        assert_eq!(day("2026-11-08T23:00:00Z"), "all day Mon 9 Nov");
        assert_eq!(
            reminder_line(&reminder("2026-11-04T09:00:00Z", false), now, &berlin),
            "Standup, in 15 min (10:00), Room 2.14"
        );
        let mut nowhere = reminder("2026-11-04T09:00:00Z", false);
        nowhere.location = Some(" ".into());
        assert_eq!(reminder_body(&nowhere, now, &berlin), "in 15 min (10:00)");
    }

    /// The notification is titled by the summary, or says it has none.
    #[test]
    fn a_reminder_notification_is_titled_by_its_summary() {
        let mut untitled = reminder("2026-11-04T09:00:00Z", false);
        assert_eq!(reminder_notification(&untitled).0, "Standup");
        untitled.summary = " ".into();
        let (title, body) = reminder_notification(&untitled);
        assert_eq!(title, "(no title)");
        assert!(body.ends_with(", Room 2.14"), "{body}");
    }
}

#[cfg(test)]
mod tests {
    use super::refused_rebuild_line;
    use super::{online_meeting_line, series_line, time_span};
    use mp_protocol::calendar::{CalendarEvent, CalendarEventTime};

    fn event() -> CalendarEvent {
        serde_json::from_value(serde_json::json!({
            "account": "evoqs",
            "calendar": "cal",
            "id": "ev",
            "uid": "ev@example.com",
            "summary": "Jour fixe",
            "time": {"kind": "timed", "start": "2026-11-03T08:00:00Z",
                     "end": "2026-11-03T09:00:00Z", "zone": "UTC"},
            "own_response": "none",
            "sensitivity": "normal",
            "show_as": "busy"
        }))
        .unwrap()
    }

    #[test]
    fn a_span_is_one_day_or_two_ends_in_the_zone_given() {
        assert_eq!(
            time_span("2026-11-03T08:00:00Z", "2026-11-03T09:30:00Z", &chrono::Utc),
            "Tue 2026-11-03 08:00-09:30"
        );
        assert_eq!(
            time_span("2026-11-03T23:00:00Z", "2026-11-04T01:00:00Z", &chrono::Utc),
            "Tue 2026-11-03 23:00 to Wed 2026-11-04 01:00"
        );
        assert_eq!(time_span("soon", "later", &chrono::Utc), "soon to later");
    }

    #[test]
    fn the_series_line_names_the_rule_and_a_changed_occurrence() {
        let mut ev = event();
        assert!(matches!(ev.time, CalendarEventTime::Timed { .. }));
        assert_eq!(ev.kind, "single", "an absent kind reads as single");
        assert_eq!(series_line(&ev), None);
        ev.kind = "occurrence".into();
        assert_eq!(
            series_line(&ev).as_deref(),
            Some("one occurrence of a series")
        );
        ev.recurrence = Some("Weekly on Tuesday".into());
        assert_eq!(series_line(&ev).as_deref(), Some("Weekly on Tuesday"));
        ev.kind = "exception".into();
        assert_eq!(
            series_line(&ev).as_deref(),
            Some("Weekly on Tuesday, this occurrence changed")
        );
        ev.recurrence = None;
        assert_eq!(
            series_line(&ev).as_deref(),
            Some("one occurrence of a series, changed")
        );
    }

    #[test]
    fn the_online_line_names_the_service_and_the_link() {
        let mut ev = event();
        assert_eq!(online_meeting_line(&ev), None);
        ev.is_online_meeting = true;
        assert_eq!(
            online_meeting_line(&ev).as_deref(),
            Some("Online meeting, no join link yet")
        );
        ev.online_meeting_provider = Some("teams-for-business".into());
        ev.online_meeting_url = Some("https://teams.example.org/l/x".into());
        assert_eq!(
            online_meeting_line(&ev).as_deref(),
            Some("Teams meeting  https://teams.example.org/l/x")
        );
        ev.is_online_meeting = false;
        ev.online_meeting_provider = None;
        assert_eq!(
            online_meeting_line(&ev).as_deref(),
            Some("Online meeting  https://teams.example.org/l/x")
        );
    }

    /// The two refusals of the cache guard have one wording in every client,
    /// and a written index has none.
    #[test]
    fn a_refused_build_has_one_line_and_a_written_one_none() {
        assert_eq!(
            refused_rebuild_line("refused_empty", 0, 25).as_deref(),
            Some("Contacts rebuild found none, kept 25 cached")
        );
        assert_eq!(
            refused_rebuild_line("refused_shrunk", 3, 25).as_deref(),
            Some("Contacts rebuild found only 3, kept 25 cached")
        );
        assert_eq!(refused_rebuild_line("written", 30, 0), None);
    }
}
