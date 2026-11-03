//! The server calendar through the CLI (PERSO-200, phase 1).
//!
//! Every row runs a real `mp daemon run` against the calendar fixture with the
//! calendar fake armed through `MAILYPOPPINS_DAEMON_FAKE_CALENDAR` and the
//! fake transport through `MAILYPOPPINS_DAEMON_FAKE_TRANSPORT`, and runs the
//! `mp` binary against it:
//!
//! - `mp calendar rebuild` reports the server resync beside the reply fold;
//! - `mp calendar list` prints the fake's calendars and the invitation
//!   calendars, and `--json` the `calendar.list` answer;
//! - `mp calendar view` places a timed, an all-day and a recurring event (two
//!   occurrences) on their days in the agenda, day and week layouts, filters
//!   by account and calendar, and `--json` prints the `calendar.view` answer;
//! - `mp calendar hide` and `mp calendar show` take a calendar out of the
//!   views and back;
//! - `mp event show` renders the card, and `mp event respond` reaches the fake
//!   with each answer, its comment and its send choice;
//! - `mp invite` answers through `event.respond`: through the server on an
//!   account whose calendar holds the invitation's event, by iMIP reply on an
//!   account without a backend.
//!
//! The views run in `--zone UTC`, the zone the fixture dates its events in.

mod support;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Output;

use chrono::{Datelike, Days, NaiveDate, Utc};
use serde_json::Value;
use tempfile::TempDir;

use mailypoppins::calendar_backend::fake::{RecordedResponse, FAKE_CALENDAR_ENV};
use mailypoppins::calendar_backend::Response;
use mailypoppins::calendar_model::{Event, EventTime};

use support::calendar_fixture::{at, event, fake_spec_with, request_ics, seed, IMIP, SERVER};
use support::parity::DaemonFixture;
use support::{read_fixture, send_fixture};

/// The invitation mail of the account with a backend, about the server's
/// meeting.
const SERVER_INVITE: &str = "mp://evoqs/inbox/meeting@example.com";
/// The invitation mail of the account without one.
const IMIP_INVITE: &str = "mp://perso/inbox/loc-day@example.com";
/// A Graph account that opts out of the server calendar.
const OUTLOOK: &str = "outlook";
/// Its invitation mail.
const OUTLOOK_INVITE: &str = "mp://outlook/inbox/outlook-invite@example.com";

/// [`OUTLOOK`]'s configuration, after the fixture's two accounts.
const OUTLOOK_CONFIG: &str = r#"
[[accounts]]
name = "outlook"
default_from = "me@outlook.example"
auth_method = "graph"
calendar_backend = "none"

[accounts.imap]
sync_interval_secs = 0

[accounts.oauth2]
client_id = "11111111-1111-1111-1111-111111111111"
tenant_id = "22222222-2222-2222-2222-222222222222"
"#;

/// The fixture, plus [`OUTLOOK`] holding an invitation mail.
fn seed_outlook(root: &Path) {
    seed(root);
    let config = root.join("config.toml");
    let text = std::fs::read_to_string(&config).unwrap();
    std::fs::write(&config, text + OUTLOOK_CONFIG).unwrap();
    let mut mail = read_fixture::email(
        "Chair <chair@tum.de>",
        "me@outlook.example",
        "Invitation: Board meeting",
        "Mon, 6 Jul 2026 10:00:00 +0200",
        "agenda to follow\n",
    );
    mail.message_id = Some("<outlook-invite@example.com>".into());
    mail.calendar_ics = Some(request_ics(
        "board@tum.de",
        "Board meeting",
        at(2, 10),
        "me@outlook.example",
    ));
    read_fixture::ingest(root, OUTLOOK, "inbox", 1, &mail);
}

/// The two occurrences of a weekly series, one and eight days out.
fn weekly() -> Vec<Event> {
    [(1, "ev-weekly-1"), (8, "ev-weekly-2")]
        .into_iter()
        .map(|(days, id)| {
            let mut occurrence = event(
                id,
                "cal-main",
                "weekly@example.com",
                "Weekly sync",
                EventTime::timed(at(days, 14), at(days, 15), "Europe/Berlin").unwrap(),
            );
            occurrence.series_master_id = Some("ev-weekly".into());
            occurrence
        })
        .collect()
}

/// A seeded root, its daemon, and the logs of the two fakes.
struct Slice {
    tmp: TempDir,
    ledger: TempDir,
    daemon: Option<DaemonFixture>,
}

impl Slice {
    fn start() -> Slice {
        Slice::start_rejecting(&[])
    }

    /// A slice whose SMTP fake refuses `rejected` for good.
    fn start_rejecting(rejected: &[(&str, &str)]) -> Slice {
        let tmp = tempfile::tempdir().unwrap();
        let ledger = tempfile::tempdir().unwrap();
        seed_outlook(tmp.path());
        let spec = fake_spec_with(&ledger.path().join("calendar.jsonl"), false, weekly());
        let transport =
            send_fixture::fake_transport_rejecting(&ledger.path().join("transport.log"), rejected);
        let env = [
            (FAKE_CALENDAR_ENV, spec.as_str()),
            (send_fixture::FAKE_TRANSPORT_ENV, transport.as_str()),
        ];
        let daemon = DaemonFixture::start_with(tmp.path(), None, &env);
        Slice {
            tmp,
            ledger,
            daemon: Some(daemon),
        }
    }

    fn root(&self) -> &Path {
        self.tmp.path()
    }

    fn run(&self, args: &[&str]) -> Output {
        self.daemon.as_ref().unwrap().mp_routed(args)
    }

    /// `mp <args>`, which must succeed; its stdout without colour.
    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "mp {} failed:\n{}\n{}",
            args.join(" "),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        plain(&String::from_utf8_lossy(&out.stdout))
    }

    /// `mp <args>`, which must fail; its stderr.
    fn err(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            !out.status.success(),
            "mp {} succeeded:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&out.stdout)
        );
        plain(&String::from_utf8_lossy(&out.stderr))
    }

    fn json(&self, args: &[&str]) -> Value {
        serde_json::from_str(&self.ok(args)).expect("JSON on stdout")
    }

    /// What the calendar fake was answered, in order.
    fn answers(&self) -> Vec<RecordedResponse> {
        std::fs::read_to_string(self.ledger.path().join("calendar.jsonl"))
            .map(|text| {
                text.lines()
                    .map(|line| serde_json::from_str(line).unwrap())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn transport_log(&self) -> PathBuf {
        self.ledger.path().join("transport.log")
    }
}

impl Drop for Slice {
    fn drop(&mut self) {
        if let Some(daemon) = self.daemon.take() {
            daemon.stop();
        }
    }
}

/// `text` without its ANSI colour sequences.
fn plain(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            chars.by_ref().find(|c| *c == 'm');
        } else {
            out.push(c);
        }
    }
    out
}

/// The day `days` from today, in UTC.
fn date(days: u64) -> NaiveDate {
    Utc::now().date_naive() + Days::new(days)
}

/// The text of `--on`/`--from` for the day `days` from today.
fn on(days: u64) -> String {
    format!("+{days}")
}

/// A view's days: each heading's date, and the event lines under it.
fn by_day(text: &str) -> BTreeMap<NaiveDate, Vec<String>> {
    let mut days = BTreeMap::new();
    let mut current = None;
    for line in text.lines() {
        if let Some(day) = line
            .get(4..14)
            .and_then(|raw| NaiveDate::parse_from_str(raw, "%Y-%m-%d").ok())
            .filter(|_| !line.starts_with(' '))
        {
            current = Some(day);
            days.insert(day, Vec::new());
        } else if let (Some(day), true) = (current, line.starts_with("  ")) {
            days.get_mut(&day).unwrap().push(line.trim().to_string());
        }
    }
    days
}

/// The summaries of a day's event lines, in order.
fn summaries(lines: &[String]) -> Vec<&str> {
    [
        "Weekly sync",
        "Project review",
        "LOC Day planning",
        "Holiday",
        "Board meeting",
    ]
    .into_iter()
    .flat_map(|summary| {
        lines
            .iter()
            .enumerate()
            .filter(move |(_, line)| line.contains(summary))
            .map(move |(n, _)| (n, summary))
    })
    .collect::<BTreeMap<_, _>>()
    .into_values()
    .collect()
}

fn row_of(root: &Path, account: &str) -> i64 {
    read_fixture::store(root, account)
        .conn()
        .query_row("SELECT id FROM messages LIMIT 1", [], |row| row.get(0))
        .unwrap()
}

/// `mp calendar rebuild`, `list`, `view`, `hide` and `show`.
#[test]
fn calendar_list_view_hide_and_show_through_the_cli() {
    let slice = Slice::start();

    let rebuilt = slice.ok(&["calendar", "rebuild", "-A", SERVER]);
    assert!(
        rebuilt.contains(
            "server calendar: 2 calendar(s), 4 event(s) synced, 0 removed, 2 baseline(s)"
        ),
        "{rebuilt}"
    );
    let rebuilt = slice.ok(&["calendar", "rebuild", "-A", IMIP]);
    assert!(!rebuilt.contains("server calendar"), "{rebuilt}");

    // The list: one line per calendar, the default marked.
    let listed = slice.ok(&["calendar", "list"]);
    let rows: Vec<Vec<&str>> = listed
        .lines()
        .map(|line| line.split_whitespace().collect())
        .collect();
    assert_eq!(
        rows,
        [
            vec![
                SERVER,
                "Calendar",
                "cal-main",
                "graph",
                "visible",
                "#3b82f6",
                "(default)"
            ],
            vec![
                SERVER,
                "Holidays",
                "cal-second",
                "graph",
                "visible",
                "#3b82f6"
            ],
            vec![SERVER, "Invitations", "imip", "none", "visible", "-"],
            vec![IMIP, "Invitations", "imip", "none", "visible", "-"],
            vec![OUTLOOK, "Invitations", "imip", "none", "visible", "-"],
        ]
    );
    let one = slice.json(&["calendar", "list", "-A", IMIP, "--json"]);
    assert_eq!(one["calendars"].as_array().unwrap().len(), 1, "{one}");
    assert_eq!(one["calendars"][0]["id"], "imip");

    // The agenda: today through fourteen days out, only the days with events,
    // the server's copy of the meeting and not its invitation mail.
    let agenda = by_day(&slice.ok(&["calendar", "view", "--zone", "UTC"]));
    let expected: BTreeMap<NaiveDate, Vec<&str>> = [
        (date(1), vec!["Weekly sync"]),
        (date(2), vec!["Board meeting"]),
        (date(3), vec!["Project review"]),
        (date(4), vec!["LOC Day planning"]),
        (date(5), vec!["Holiday"]),
        (date(6), vec!["Holiday"]),
        (date(8), vec!["Weekly sync"]),
    ]
    .into_iter()
    .collect();
    let seen: BTreeMap<NaiveDate, Vec<&str>> = agenda
        .iter()
        .map(|(day, lines)| (*day, summaries(lines)))
        .collect();
    assert_eq!(seen, expected, "{agenda:?}");
    let meeting = &agenda[&date(3)][0];
    assert!(
        meeting.starts_with("09:00-10:00  ? Project review"),
        "{meeting}"
    );
    assert!(meeting.ends_with("[evoqs/Calendar]"), "{meeting}");
    assert!(
        agenda[&date(5)][0].starts_with("all day"),
        "{:?}",
        agenda[&date(5)]
    );
    assert!(agenda[&date(4)][0].contains("@ Room 4.12  [perso/Invitations]"));

    // One account, and an agenda range of its own.
    let evoqs = by_day(&slice.ok(&[
        "calendar",
        "view",
        "--zone",
        "UTC",
        "-A",
        SERVER,
        "--from",
        &on(2),
        "--to",
        &on(5),
    ]));
    assert_eq!(
        evoqs.keys().copied().collect::<Vec<_>>(),
        [date(3), date(5)]
    );

    // A day: its events by start.
    let day = slice.ok(&["calendar", "view", "day", "--on", &on(3), "--zone", "UTC"]);
    let day = by_day(&day);
    assert_eq!(day.len(), 1);
    assert_eq!(summaries(&day[&date(3)]), ["Project review"]);
    let empty = slice.ok(&["calendar", "view", "day", "--on", &on(7), "--zone", "UTC"]);
    assert!(empty.contains("  no events"), "{empty}");

    // A week: Monday to Sunday around the second occurrence, every day listed.
    let week_text = slice.ok(&["calendar", "view", "week", "--on", &on(8), "--zone", "UTC"]);
    let week = by_day(&week_text);
    let monday = date(8) - Days::new(u64::from(date(8).weekday().num_days_from_monday()));
    assert_eq!(
        week.keys().copied().collect::<Vec<_>>(),
        (0..7).map(|n| monday + Days::new(n)).collect::<Vec<_>>(),
        "{week_text}"
    );
    assert!(
        summaries(&week[&date(8)]).contains(&"Weekly sync"),
        "{week_text}"
    );
    assert!(week_text.starts_with(&format!("Week {}", monday.iso_week().week())));

    // The raw answer.
    let raw = slice.json(&["calendar", "view", "--zone", "UTC", "-A", SERVER, "--json"]);
    assert_eq!(raw["zone"], "UTC");
    assert!(
        raw["range"]["start"].is_string() && raw["window"]["end"].is_string(),
        "{raw}"
    );
    let ids: Vec<&str> = raw["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        ["ev-weekly-1", "ev-meeting", "ev-holiday", "ev-weekly-2"]
    );
    let only = slice.json(&[
        "calendar",
        "view",
        "--zone",
        "UTC",
        "--calendar",
        "cal-second",
        "--json",
    ]);
    assert_eq!(only["events"].as_array().unwrap().len(), 1, "{only}");

    // Hidden: out of the views, back with --hidden, and shown again.
    let hidden = slice.ok(&["calendar", "hide", "cal-second", "-A", SERVER]);
    assert!(hidden.contains("evoqs/cal-second is hidden"), "{hidden}");
    let agenda = slice.ok(&["calendar", "view", "--zone", "UTC"]);
    assert!(!agenda.contains("Holiday"), "{agenda}");
    let all = slice.ok(&["calendar", "view", "--zone", "UTC", "--hidden"]);
    assert!(all.contains("Holiday"), "{all}");
    assert!(slice
        .ok(&["calendar", "list", "-A", SERVER])
        .contains("cal-second  graph  hidden"));
    let shown = slice.ok(&["calendar", "show", "cal-second", "-A", SERVER]);
    assert!(shown.contains("evoqs/cal-second is visible"), "{shown}");
    assert!(slice
        .ok(&["calendar", "view", "--zone", "UTC"])
        .contains("Holiday"));

    let unknown = slice.err(&["calendar", "hide", "nope", "-A", SERVER]);
    assert!(unknown.contains("nope"), "{unknown}");
    let bad_date = slice.err(&["calendar", "view", "--on", "someday"]);
    assert!(bad_date.contains("YYYY-MM-DD"), "{bad_date}");
}

/// `mp event show` and `mp event respond`, and `mp invite` on an account
/// whose calendar holds the invitation's event.
#[test]
fn event_show_and_respond_through_the_cli() {
    let slice = Slice::start();
    slice.ok(&["calendar", "rebuild", "-A", SERVER]);

    let card = slice.ok(&["event", "show", "ev-meeting", "-A", SERVER]);
    for line in [
        "Project review",
        "  when        ",
        "  calendar    evoqs/Calendar",
        "  organizer   Ada Lovelace <ada@example.com>",
        "  attendees   sylvain@example.com  required, needs-action",
        "  response    ? needs-action",
        "  id          ev-meeting",
    ] {
        assert!(card.contains(line), "{line:?} in\n{card}");
    }
    let occurrence = slice.ok(&["event", "show", "ev-weekly-2", "-A", SERVER]);
    assert!(
        occurrence.contains("recurrence  one occurrence of a series"),
        "{occurrence}"
    );
    let raw = slice.json(&["event", "show", "ev-meeting", "-A", SERVER, "--json"]);
    assert_eq!(raw["account"], SERVER);
    assert_eq!(raw["event"]["id"], "ev-meeting");
    assert_eq!(raw["event"]["time"]["kind"], "timed");

    let accepted = slice.ok(&[
        "event",
        "respond",
        "ev-meeting",
        "accept",
        "--comment",
        "running late",
        "-A",
        SERVER,
    ]);
    assert!(
        accepted.contains("Accepted ev-meeting: through the server, organizer told"),
        "{accepted}"
    );
    assert!(accepted.contains("comment: running late"), "{accepted}");
    let tentative = slice.json(&[
        "event",
        "respond",
        "ev-meeting",
        "tentative",
        "--no-send-response",
        "-A",
        SERVER,
        "--json",
    ]);
    assert_eq!(tentative["via"], "server");
    assert_eq!(tentative["response"], "tentative");
    assert_eq!(tentative["send_response"], false);
    assert_eq!(tentative["delivered"], false);
    assert_eq!(tentative["comment"], Value::Null);
    slice.ok(&["event", "respond", "ev-meeting", "decline", "-A", SERVER]);

    // `mp invite` on the meeting's invitation mail answers the server event.
    let invited = slice.ok(&["invite", "accept", SERVER_INVITE, "--comment", "on my way"]);
    assert!(
        invited.contains("Accepted: Project review \u{2014} replied to ada@example.com"),
        "{invited}"
    );

    let answer = |response, comment: Option<&str>, send_response| RecordedResponse {
        event: "ev-meeting".into(),
        response,
        comment: comment.map(str::to_string),
        send_response,
    };
    assert_eq!(
        slice.answers(),
        [
            answer(Response::Accept, Some("running late"), true),
            answer(Response::Tentative, None, false),
            answer(Response::Decline, None, true),
            answer(Response::Accept, Some("on my way"), true),
        ]
    );

    let unknown = slice.err(&["event", "show", "nope", "-A", SERVER]);
    assert!(unknown.contains("nope"), "{unknown}");
}

/// `mp invite` on an account without a backend still sends the iMIP reply,
/// now through `event.respond`, and refuses `--no-send-response` there.
#[test]
fn mp_invite_still_answers_an_imip_invitation() {
    let slice = Slice::start();
    // Read before the reply is filed beside it.
    let event = format!("imip:{}", row_of(slice.root(), IMIP));

    let silent = slice.err(&[
        "-A",
        IMIP,
        "invite",
        "decline",
        "--no-send-response",
        IMIP_INVITE,
    ]);
    assert!(silent.contains("send_response"), "{silent}");

    let replied = slice.ok(&[
        "-A",
        IMIP,
        "invite",
        "accept",
        IMIP_INVITE,
        "--comment",
        "gladly",
    ]);
    assert_eq!(
        replied.trim(),
        "\u{2713} Accepted: LOC Day planning \u{2014} replied to chair@tum.de"
    );
    let submitted = send_fixture::transport_events(&slice.transport_log())
        .into_iter()
        .filter(|event| {
            matches!(event, send_fixture::TransportEvent::Submit { address, accepted: true, .. }
                if address == "chair@tum.de")
        })
        .count();
    assert_eq!(submitted, 1, "the reply reached the organizer");

    let result = slice.json(&[
        "event",
        "respond",
        &event,
        "tentative",
        "-A",
        IMIP,
        "--json",
    ]);
    assert_eq!(result["via"], "imip", "{result}");
    assert_eq!(result["delivered"], true, "{result}");
    assert_eq!(result["event"], event.as_str());
    assert!(slice.answers().is_empty(), "no server call for a mail");

    // A Graph account sends no iMIP reply: without a server calendar to answer
    // through, its invitation is refused once the selector has resolved.
    let graph = slice.err(&["-A", OUTLOOK, "invite", "accept", OUTLOOK_INVITE]);
    assert!(
        graph.contains("RSVP is not supported for Graph accounts yet"),
        "{graph}"
    );
}

/// An iMIP reply the organizer's server refused is a failure on both paths:
/// `--json` still prints the object, `"delivered": false`, and exits 1 with
/// the same error on stderr (PERSO-200).
#[test]
fn an_undelivered_imip_reply_fails_with_and_without_json() {
    let slice = Slice::start_rejecting(&[("chair@tum.de", "550 5.1.1 no such mailbox")]);
    let event = format!("imip:{}", row_of(slice.root(), IMIP));

    let out = slice.run(&["event", "respond", &event, "accept", "-A", IMIP, "--json"]);
    assert!(!out.status.success(), "--json exited 0");
    let result: Value = serde_json::from_slice(&out.stdout).expect("the object on stdout");
    assert_eq!(result["via"], "imip", "{result}");
    assert_eq!(result["delivered"], false, "{result}");
    let stderr = plain(&String::from_utf8_lossy(&out.stderr));
    assert!(stderr.contains("Failed to send the RSVP"), "{stderr}");

    let plain_err = slice.err(&["event", "respond", &event, "decline", "-A", IMIP]);
    assert!(plain_err.contains("Failed to send the RSVP"), "{plain_err}");
}
