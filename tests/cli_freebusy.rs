//! `mp calendar freebusy` through the CLI (PERSO-255, phase 2, slice 7).
//!
//! A real `mp daemon run` against the calendar fixture, the calendar fake
//! armed with `free-busy` and the busy blocks of the account and a colleague
//! ([`fake_spec_free_busy`]), and the `mp` binary against it:
//!
//! - the day's rows: the account first, the colleague's away and tentative
//!   blocks, and an outside address reading "no free/busy";
//! - `--json` prints the settled `calendar.freebusy` result;
//! - an account without a calendar backend is refused with the daemon's
//!   sentence.

mod support;

use std::process::Output;

use serde_json::Value;
use tempfile::TempDir;

use mailypoppins::calendar_backend::fake::FAKE_CALENDAR_ENV;

use support::calendar_fixture::{fake_spec_free_busy, seed, COLLEAGUE, IMIP, OWN_ADDRESS, SERVER};
use support::parity::DaemonFixture;

struct Slice {
    _tmp: TempDir,
    daemon: Option<DaemonFixture>,
}

impl Slice {
    fn start() -> Slice {
        let tmp = tempfile::tempdir().unwrap();
        seed(tmp.path());
        let spec = fake_spec_free_busy(&tmp.path().join("calendar.jsonl"), false, true);
        let daemon =
            DaemonFixture::start_with(tmp.path(), None, &[(FAKE_CALENDAR_ENV, spec.as_str())]);
        Slice {
            _tmp: tmp,
            daemon: Some(daemon),
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        self.daemon.as_ref().unwrap().mp_routed(args)
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
fn plain(text: &[u8]) -> String {
    let text = String::from_utf8_lossy(text);
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

/// The day's rows in UTC, the account first, then the colleague and the
/// outside address; `--json` is the settled result; an iMIP-only account
/// is refused.
#[test]
fn calendar_freebusy_through_the_cli() {
    let slice = Slice::start();
    let out = slice.run(&[
        "calendar",
        "freebusy",
        "-A",
        SERVER,
        "--on",
        "+3",
        "--zone",
        "UTC",
        COLLEAGUE,
        "someone@gmail.com",
    ]);
    assert!(out.status.success(), "{}", plain(&out.stderr));
    let text = plain(&out.stdout);
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines[0].starts_with("Free/busy, "), "{text}");
    assert!(lines[0].ends_with("(UTC), 30 min slots"), "{text}");
    let at = |needle: &str| {
        lines
            .iter()
            .position(|line| *line == needle)
            .unwrap_or_else(|| panic!("no line {needle:?} in\n{text}"))
    };
    let (own, ada, outside) = (at(OWN_ADDRESS), at(COLLEAGUE), at("someone@gmail.com"));
    assert!(own < ada && ada < outside, "{text}");
    let row = |header: usize| lines[header + 2].split("  ").last().unwrap().to_string();
    let dots = |n: usize| "\u{b7}".repeat(n);
    assert_eq!(
        row(own),
        format!("{}{}{}", dots(18), "\u{2588}".repeat(2), dots(28))
    );
    assert_eq!(
        row(ada),
        format!(
            "{}{}{}{}{}",
            dots(18),
            "\u{2593}".repeat(4),
            dots(4),
            "\u{2592}".repeat(2),
            dots(20)
        )
    );
    assert!(
        lines[own + 3].contains("busy") && lines[own + 3].contains("09:00-10:00  Project review")
    );
    assert!(lines[ada + 3].contains("away") && lines[ada + 3].contains("09:00-11:00  Away"));
    assert!(lines[ada + 4].contains("tentative") && lines[ada + 4].contains("13:00-14:00"));
    assert_eq!(
        lines[outside + 1].trim(),
        "no free/busy: Proxy web request failed (5016)"
    );

    let out = slice.run(&[
        "calendar", "freebusy", "-A", SERVER, "--on", "+3", "--zone", "UTC", COLLEAGUE, "--json",
    ]);
    assert!(out.status.success(), "{}", plain(&out.stderr));
    let json: Value = serde_json::from_slice(&out.stdout).expect("JSON on stdout");
    assert_eq!(json["account"], SERVER);
    assert_eq!(json["interval_minutes"], 30);
    assert_eq!(json["schedules"][0]["address"], OWN_ADDRESS);
    assert_eq!(json["schedules"][1]["address"], COLLEAGUE);
    assert_eq!(json["schedules"][1]["items"][0]["status"], "oof");

    let out = slice.run(&["calendar", "freebusy", "-A", IMIP, COLLEAGUE]);
    assert!(!out.status.success());
    let err = plain(&out.stderr);
    assert!(err.contains("no calendar backend"), "{err}");
}
