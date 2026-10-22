//! Desktop notifications for new mail (#0009).
//!
//! Fired from the TUI when a background quick-sync saves genuinely new
//! inbox emails (opt-in via the top-level `notifications = true` key in
//! config.toml). Shells out to platform tools instead of pulling in a
//! notification crate: `osascript` on macOS, `notify-send` on Linux.
//! Both degrade silently when the tool is missing or fails -- a lost
//! notification is never worth an error in the TUI.
//!
//! SECURITY: notification text contains attacker-controlled email
//! subjects and sender names. Text is passed to the helper tools as
//! separate argv entries -- never interpolated into a shell string or an
//! AppleScript literal (the macOS path hands the text to the script via
//! `on run argv`). `sanitize_notification_text` additionally strips
//! control characters, caps the length, and neutralizes a leading `-`
//! so the text can never be parsed as a CLI option by the helper tool.

/// Sender + subject of one newly saved email. Produced by the sync
/// backends (IMAP and Graph) for inbox saves only, so the notification
/// body can show "sender: subject" when exactly one email arrived.
#[derive(Debug, Clone)]
pub struct NewMailMeta {
    pub from: String,
    pub subject: String,
}

/// Maximum characters kept per sanitized field (sender, subject).
const MAX_FIELD_CHARS: usize = 120;

impl NewMailMeta {
    /// Metadata for one newly-ingested message. Ingest reports exactly the
    /// messages it wrote a row for, so there is no phantom-notification
    /// filtering left to do here.
    pub fn new(from: &str, subject: &str) -> Self {
        Self {
            from: from.to_string(),
            subject: subject.to_string(),
        }
    }
}

/// Strip control characters, collapse surrounding whitespace, cap the
/// length at `max_chars` (appending an ellipsis when truncated), and
/// prefix a space when the result would start with `-` (so it can never
/// be mistaken for a CLI option by `notify-send`/`osascript`).
pub fn sanitize_notification_text(s: &str, max_chars: usize) -> String {
    let cleaned: String = s.chars().filter(|c| !c.is_control()).collect();
    let trimmed = cleaned.trim();
    let mut out: String = trimmed.chars().take(max_chars).collect();
    if trimmed.chars().count() > max_chars {
        out.push('\u{2026}'); // …
    }
    if out.starts_with('-') {
        out.insert(0, ' ');
    }
    out
}

/// Build the `(title, body)` pair for a new-mail notification. Returns
/// `None` when there is nothing to notify about. One email shows
/// "sender: subject"; several are grouped into a single count line
/// (this is also the anti-spam grouping: one notification per completed
/// sync, however many emails it saved).
pub fn format_new_mail_notification(
    account: &str,
    new: &[NewMailMeta],
) -> Option<(String, String)> {
    let account = sanitize_notification_text(account, MAX_FIELD_CHARS);
    let title = if account.is_empty() {
        "New email".to_string()
    } else {
        format!("New email ({account})")
    };
    match new {
        [] => None,
        [one] => {
            let from = sanitize_notification_text(&one.from, MAX_FIELD_CHARS);
            let subject = sanitize_notification_text(&one.subject, MAX_FIELD_CHARS);
            let subject = if subject.is_empty() {
                "(no subject)".to_string()
            } else {
                subject
            };
            let body = if from.is_empty() {
                subject
            } else {
                format!("{from}: {subject}")
            };
            Some((title, body))
        }
        many => Some((title, format!("{} new emails", many.len()))),
    }
}

/// Fire a desktop notification for newly saved inbox emails. No-op when
/// `new` is empty. Non-blocking: the platform tool runs on a detached
/// thread (which also reaps the child process), and any failure is
/// logged at debug level and otherwise ignored.
///
/// In a test build (`cfg(test)` or the `test-support` feature every test
/// suite turns on) the notification goes to [`test_sink`] instead and never
/// reaches the host's notification centre; a test that did not open a
/// capture panics here rather than post one.
pub fn notify_new_mail(account: &str, new: &[NewMailMeta]) {
    let Some((title, body)) = format_new_mail_notification(account, new) else {
        return;
    };
    #[cfg(any(test, feature = "test-support"))]
    {
        test_sink::record(title, body);
    }
    #[cfg(not(any(test, feature = "test-support")))]
    std::thread::spawn(move || {
        if let Err(e) = send_desktop_notification(&title, &body) {
            log::debug!("Desktop notification failed: {e}");
        }
    });
}

/// Where a test build sends a notification: a per-thread record, so a test
/// asserts the title and body the user would have seen and the host's
/// notification centre is never touched.
///
/// Per thread because each test runs on its own thread and the TUI calls
/// [`notify_new_mail`] on the thread that applies the event, which in a test
/// is the test's. A notification with no capture open panics: the
/// alternative, a silent drop, would let a test enable notifications without
/// saying what it expects, and before this sink such a test ran `osascript`
/// on the developer's machine.
#[cfg(any(test, feature = "test-support"))]
pub mod test_sink {
    use std::cell::RefCell;

    thread_local! {
        static SHOWN: RefCell<Option<Vec<(String, String)>>> = const { RefCell::new(None) };
    }

    /// An open capture; dropping it closes it.
    #[must_use = "the capture closes when dropped"]
    pub struct Capture(());

    /// Start recording this thread's notifications.
    pub fn capture() -> Capture {
        SHOWN.with(|shown| *shown.borrow_mut() = Some(Vec::new()));
        Capture(())
    }

    impl Capture {
        /// Every `(title, body)` recorded since [`capture`], oldest first.
        pub fn shown(&self) -> Vec<(String, String)> {
            SHOWN.with(|shown| shown.borrow().clone().unwrap_or_default())
        }
    }

    impl Drop for Capture {
        fn drop(&mut self) {
            SHOWN.with(|shown| *shown.borrow_mut() = None);
        }
    }

    pub(super) fn record(title: String, body: String) {
        SHOWN.with(|shown| match shown.borrow_mut().as_mut() {
            Some(list) => list.push((title, body)),
            None => panic!(
                "a test reached the host's notification centre with {title:?} / {body:?}; \
                 open mp_core::notify::test_sink::capture() and assert what it records"
            ),
        });
    }
}

/// macOS: `osascript` with the text passed through `on run argv` --
/// the attacker-controlled strings never touch AppleScript source, so
/// no quoting or escaping is involved at all.
#[cfg(target_os = "macos")]
#[cfg_attr(any(test, feature = "test-support"), allow(dead_code))]
fn send_desktop_notification(title: &str, body: &str) -> std::io::Result<()> {
    use std::process::{Command, Stdio};
    Command::new("osascript")
        .arg("-e")
        .arg("on run argv")
        .arg("-e")
        .arg("display notification (item 1 of argv) with title (item 2 of argv)")
        .arg("-e")
        .arg("end run")
        .arg(body)
        .arg(title)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|_| ())
}

/// Linux (incl. WSL with a notification daemon): `notify-send`, with
/// `--` terminating option parsing before the attacker-influenced args.
#[cfg(target_os = "linux")]
#[cfg_attr(any(test, feature = "test-support"), allow(dead_code))]
fn send_desktop_notification(title: &str, body: &str) -> std::io::Result<()> {
    use std::process::{Command, Stdio};
    Command::new("notify-send")
        .arg("--app-name=mailypoppins")
        .arg("--")
        .arg(title)
        .arg(body)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|_| ())
}

/// Other platforms: silently do nothing.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
#[cfg_attr(any(test, feature = "test-support"), allow(dead_code))]
fn send_desktop_notification(_title: &str, _body: &str) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(from: &str, subject: &str) -> NewMailMeta {
        NewMailMeta {
            from: from.to_string(),
            subject: subject.to_string(),
        }
    }

    // -- sanitize_notification_text ------------------------------------

    #[test]
    fn sanitize_strips_control_characters() {
        assert_eq!(
            sanitize_notification_text("a\nb\tc\x1b[31md\re", 100),
            "abc[31mde"
        );
    }

    #[test]
    fn sanitize_trims_whitespace() {
        assert_eq!(sanitize_notification_text("  hello  ", 100), "hello");
    }

    #[test]
    fn sanitize_truncates_with_ellipsis() {
        assert_eq!(sanitize_notification_text("abcdef", 4), "abcd\u{2026}");
    }

    #[test]
    fn sanitize_truncation_counts_chars_not_bytes() {
        // Multi-byte chars must not be split (would panic on byte slicing).
        assert_eq!(sanitize_notification_text("ééééé", 3), "ééé\u{2026}");
    }

    #[test]
    fn sanitize_exact_length_is_untouched() {
        assert_eq!(sanitize_notification_text("abcd", 4), "abcd");
    }

    #[test]
    fn sanitize_neutralizes_leading_dash() {
        // A subject like "--urgent" must not be parseable as a CLI option.
        assert_eq!(sanitize_notification_text("--urgent", 100), " --urgent");
    }

    #[test]
    fn sanitize_leading_dash_after_control_strip() {
        // Control chars hiding the dash are stripped first, then the
        // now-leading dash is still neutralized.
        assert_eq!(sanitize_notification_text("\x00-e evil", 100), " -e evil");
    }

    // -- format_new_mail_notification ----------------------------------

    #[test]
    fn format_none_for_empty_list() {
        assert!(format_new_mail_notification("work", &[]).is_none());
    }

    #[test]
    fn format_single_email_shows_sender_and_subject() {
        let (title, body) =
            format_new_mail_notification("work", &[meta("Alice <a@example.com>", "Hi there")])
                .unwrap();
        assert_eq!(title, "New email (work)");
        assert_eq!(body, "Alice <a@example.com>: Hi there");
    }

    #[test]
    fn format_single_email_without_subject() {
        let (_, body) = format_new_mail_notification("work", &[meta("a@example.com", "")]).unwrap();
        assert_eq!(body, "a@example.com: (no subject)");
    }

    #[test]
    fn format_single_email_without_sender() {
        let (_, body) = format_new_mail_notification("work", &[meta("", "Hello")]).unwrap();
        assert_eq!(body, "Hello");
    }

    #[test]
    fn format_multiple_emails_grouped_into_count() {
        let batch = vec![meta("a", "1"), meta("b", "2"), meta("c", "3")];
        let (title, body) = format_new_mail_notification("work", &batch).unwrap();
        assert_eq!(title, "New email (work)");
        assert_eq!(body, "3 new emails");
    }

    #[test]
    fn format_sanitizes_attacker_controlled_fields() {
        let (_, body) = format_new_mail_notification(
            "work",
            &[meta("-e 'do shell script'\n", "evil\x07subject")],
        )
        .unwrap();
        assert_eq!(body, " -e 'do shell script': evilsubject");
    }

    #[test]
    fn format_empty_account_omits_parenthetical() {
        let (title, _) = format_new_mail_notification("", &[meta("a", "s")]).unwrap();
        assert_eq!(title, "New email");
    }

    #[test]
    fn format_long_subject_is_capped() {
        let long = "x".repeat(500);
        let (_, body) = format_new_mail_notification("work", &[meta("a", &long)]).unwrap();
        // 120 chars + ellipsis + "a: " prefix
        assert_eq!(body.chars().count(), 3 + 120 + 1);
        assert!(body.ends_with('\u{2026}'));
    }

    // -- test_sink ------------------------------------------------------

    #[test]
    fn a_test_build_records_the_notification_instead_of_posting_it() {
        let capture = test_sink::capture();
        notify_new_mail("work", &[meta("Alice <a@example.com>", "Hi there")]);
        notify_new_mail("work", &[]);
        assert_eq!(
            capture.shown(),
            vec![(
                "New email (work)".to_string(),
                "Alice <a@example.com>: Hi there".to_string()
            )]
        );
    }

    #[test]
    #[should_panic(expected = "a test reached the host's notification centre")]
    fn a_notification_with_no_capture_open_panics() {
        notify_new_mail("work", &[meta("a", "s")]);
    }

    // -- NewMailMeta::new ----------------------------------------------

    #[test]
    fn new_mail_meta_carries_sender_and_subject() {
        let m = NewMailMeta::new("alice", "saved");
        assert_eq!(m.from, "alice");
        assert_eq!(m.subject, "saved");
    }
}
