//! The calendar reminders through the CLI (PERSO-255, calendar phase 2).
//!
//! A real `mp daemon run` against the calendar fixture, with the calendar
//! fake armed through `MAILYPOPPINS_DAEMON_FAKE_CALENDAR` and holding a
//! standup whose reminder is overdue, and the `mp` binary against it:
//!
//! - `mp calendar reminders` lists the fired reminder with its account and
//!   event id, and `--json` prints the `calendar.reminders` answer;
//! - `mp calendar reminder snooze` and `dismiss` take it off the list;
//! - a dismissal of an event without a reminder is refused in the daemon's
//!   words.

mod support;

use std::process::Output;
use std::time::{Duration, Instant};

use chrono::{Duration as Minutes, Utc};
use tempfile::TempDir;

use mailypoppins::calendar_backend::fake::FAKE_CALENDAR_ENV;
use mailypoppins::calendar_model::{EventTime, OwnResponse};

use support::calendar_fixture::{event, fake_spec_with, seed, SERVER};
use support::parity::DaemonFixture;

struct Slice {
    _tmp: TempDir,
    daemon: Option<DaemonFixture>,
}

impl Slice {
    fn start() -> Slice {
        let tmp = tempfile::tempdir().unwrap();
        seed(tmp.path());
        let now = Utc::now();
        let mut standup = event(
            "ev-standup",
            "cal-main",
            "standup@example.com",
            "Standup",
            EventTime::timed(now - Minutes::minutes(5), now + Minutes::hours(6), "UTC").unwrap(),
        );
        standup.reminder = Some(10);
        standup.own_response = OwnResponse::Accepted;
        let spec = fake_spec_with(&tmp.path().join("calendar.jsonl"), false, vec![standup]);
        let daemon = DaemonFixture::start_with(tmp.path(), None, &[(FAKE_CALENDAR_ENV, &spec)]);
        Slice {
            _tmp: tmp,
            daemon: Some(daemon),
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        self.daemon.as_ref().unwrap().mp_routed(args)
    }

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

    fn err(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(!out.status.success(), "mp {} succeeded", args.join(" "));
        plain(&String::from_utf8_lossy(&out.stderr))
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

#[test]
fn reminders_list_snooze_and_dismiss_through_the_cli() {
    let slice = Slice::start();
    assert_eq!(
        slice.ok(&["calendar", "reminders"]).trim(),
        "No reminders waiting"
    );
    slice.ok(&["calendar", "rebuild", "-A", SERVER]);
    let start = Instant::now();
    let listed = loop {
        let answer: serde_json::Value =
            serde_json::from_str(&slice.ok(&["calendar", "reminders", "--json"])).unwrap();
        if !answer["reminders"].as_array().unwrap().is_empty() {
            break answer;
        }
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "the reminder never fired"
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(listed["reminders"][0]["event"], "ev-standup");
    assert_eq!(listed["reminders"][0]["minutes_before"], 10);

    let text = slice.ok(&["calendar", "reminders", "-A", SERVER]);
    assert!(text.contains("Standup, started "), "{text}");
    assert!(text.contains(&format!("{SERVER}  ev-standup")), "{text}");

    let snoozed = slice.ok(&[
        "calendar",
        "reminder",
        "snooze",
        "ev-standup",
        "--minutes",
        "10",
        "-A",
        SERVER,
    ]);
    assert!(
        snoozed.contains("Snoozed the reminder of ev-standup for 10 min"),
        "{snoozed}"
    );
    assert_eq!(
        slice.ok(&["calendar", "reminders"]).trim(),
        "No reminders waiting"
    );
    let dismissed = slice.ok(&[
        "calendar",
        "reminder",
        "dismiss",
        "ev-standup",
        "-A",
        SERVER,
    ]);
    assert!(
        dismissed.contains("Dismissed the reminder of ev-standup"),
        "{dismissed}"
    );
    let refused = slice.err(&[
        "calendar",
        "reminder",
        "dismiss",
        "ev-meeting",
        "-A",
        SERVER,
    ]);
    assert!(refused.contains("ev-meeting has no reminder"), "{refused}");
}
