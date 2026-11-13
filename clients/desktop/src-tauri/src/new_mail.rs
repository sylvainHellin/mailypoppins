//! The new-mail notifications (PERSO-186).
//!
//! The daemon decides which mail deserves one: after each tick it publishes
//! `mail.arrived` with the inbox mail still unread once the blocked senders
//! and the mail hooks have had their turn, and names the presenter, the one
//! connection that raises the system notification (this app's whenever it is
//! connected, as for a reminder). This layer only shows it, from Rust through
//! `tauri-plugin-notification` as the reminders do ([`crate::reminders`]):
//! one notification per event, titled by the account, "sender: subject" for
//! one message and a count for several, the text the TUI shows
//! ([`mp_core::notify::format_new_mail_notification`]).
//!
//! A click on its body brings the main window forward and opens nothing
//! ([`is_new_mail`], which [`crate::reminders::install`] reads); macOS shows
//! no banner for the frontmost app, and its Focus modes and the app's
//! notification settings are the user's way to silence it.

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Runtime};
use tauri_plugin_notification::NotificationExt;

use mp_protocol::events::{MailArrived, KIND_MAIL_ARRIVED};
use mp_protocol::EventEnvelope;

/// The key of the extra payload that marks a new-mail notification.
const EXTRA: &str = "mail";

/// What a new-mail notification carries in its extra payload: the account
/// whose mail it announced.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct MailTarget {
    account: String,
}

/// Whether `arrived` is this app's to notify: the daemon names `connection`,
/// this app's, as its presenter.
fn presents(arrived: &MailArrived, connection: Option<u64>) -> bool {
    connection.is_some() && arrived.presenter_connection == connection
}

/// The notification's title and body, `None` for an event with no mail.
fn text(arrived: &MailArrived) -> Option<(String, String)> {
    mp_core::notify::format_new_mail_notification(&arrived.account, &arrived.arrivals)
}

/// A `mail.arrived` this app's `connection` presents is notified; every other
/// event, and new mail another connection presents, is not.
pub fn on_event<R: Runtime>(app: &AppHandle<R>, connection: Option<u64>, event: &EventEnvelope) {
    if event.kind != KIND_MAIL_ARRIVED {
        return;
    }
    let arrived = match serde_json::from_value::<MailArrived>(event.payload.clone()) {
        Ok(arrived) => arrived,
        Err(e) => return tracing::warn!("[mail] a mail.arrived did not decode: {e}"),
    };
    if !presents(&arrived, connection) {
        return;
    }
    let Some((title, body)) = text(&arrived) else {
        return;
    };
    let shown = app
        .notification()
        .builder()
        .title(title)
        .body(body)
        .extra(
            EXTRA,
            MailTarget {
                account: arrived.account.clone(),
            },
        )
        .show();
    match shown {
        Ok(()) => tracing::info!(
            "[mail] notified {} new message(s) of {}",
            arrived.arrivals.len(),
            arrived.account
        ),
        Err(e) => tracing::warn!(
            "[mail] the notification of {}'s new mail failed: {e}",
            arrived.account
        ),
    }
}

/// Whether a click names a new-mail notification.
pub fn is_new_mail(performed: &tauri_plugin_notification::ActionPerformed) -> bool {
    performed.action_id() == "tap"
        && performed
            .notification()
            .is_some_and(|notification| notification.extra().contains_key(EXTRA))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mp_protocol::events::Arrival;

    fn arrived(arrivals: &[(&str, &str)]) -> MailArrived {
        MailArrived {
            account: "evoqs".into(),
            arrivals: arrivals
                .iter()
                .map(|(from, subject)| Arrival {
                    from: (*from).into(),
                    subject: (*subject).into(),
                })
                .collect(),
            presenter: Some("gui".into()),
            presenter_connection: Some(4),
        }
    }

    /// Only the mail naming this app's connection is notified: mail a TUI or
    /// another connection presents is not, nor any before a bootstrap.
    #[test]
    fn only_this_apps_connection_presents() {
        let one = arrived(&[("Ada <ada@example.com>", "Engine")]);
        assert!(presents(&one, Some(4)));
        assert!(!presents(&one, Some(5)));
        assert!(!presents(&one, None));
        let unpresented = MailArrived {
            presenter: None,
            presenter_connection: None,
            ..one
        };
        assert!(!presents(&unpresented, None));
    }

    /// One message shows its sender and subject, several their count, under
    /// the account's name; no mail shows nothing.
    #[test]
    fn the_text_is_the_tuis() {
        assert_eq!(
            text(&arrived(&[("Ada <ada@example.com>", "Engine")])),
            Some((
                "New email (evoqs)".to_string(),
                "Ada <ada@example.com>: Engine".to_string()
            ))
        );
        assert_eq!(
            text(&arrived(&[("a", "1"), ("b", "2")])),
            Some(("New email (evoqs)".to_string(), "2 new emails".to_string()))
        );
        assert_eq!(text(&arrived(&[])), None);
    }

    /// A click on a new-mail notification's body is recognised; a dismissal
    /// and a click on a reminder's are not.
    #[test]
    fn a_click_on_new_mail_is_recognised() {
        let performed = |action: &str, extra: serde_json::Value| {
            serde_json::from_value::<tauri_plugin_notification::ActionPerformed>(
                serde_json::json!({
                    "actionId": action,
                    "notification": {"id": 1, "extra": extra},
                }),
            )
            .unwrap()
        };
        let mail = serde_json::json!({"mail": {"account": "evoqs"}});
        assert!(is_new_mail(&performed("tap", mail.clone())));
        assert!(!is_new_mail(&performed("dismiss", mail)));
        assert!(!is_new_mail(&performed(
            "tap",
            serde_json::json!({"reminder": {"account": "evoqs", "event": "e", "start": ""}})
        )));
    }
}
