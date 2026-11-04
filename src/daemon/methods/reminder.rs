//! The calendar reminders (PERSO-255): the pending list and the two
//! decisions on a reminder.
//!
//! ```text
//! calendar.reminders        {accounts?}
//!                               -> {reminders:[CalendarReminder]}
//! calendar.dismiss_reminder {account, event}
//!                               -> {account, event}
//! calendar.snooze_reminder  {account, event, minutes}
//!                               -> {account, event, until}
//! ```
//!
//! `calendar.reminders` is a query over the reminder record and the calendar
//! cache ([`crate::daemon::reminders::pending`]): the reminders that fired and
//! wait for the user, which a client that connects later shows.
//!
//! The two decisions are commands: each records the decision locally, wakes
//! the reminder task, publishes `calendar.reminders_changed` so every client
//! clears or moves the reminder together, and answers with the revision that
//! moved. A decision may come before the reminder fired, so a dismissal ahead
//! of time keeps it from firing. On a backend that serves `server-reminders`
//! the decision then goes to the server (Graph's `dismissReminder` and
//! `snoozeReminder`), so Outlook stops reminding too; that call runs after the
//! answer, and a failure is logged and leaves the local record in place
//! (decision log, 2026-11-03).
//!
//! The refusals are `-32602`: an event the cache does not hold, one that does
//! not remind (no reminder, cancelled or declined), a snooze outside 1 to
//! [`MAX_SNOOZE_MINUTES`] minutes, and a snooze of a dismissed reminder.

use std::sync::Arc;

use chrono::{Duration, SecondsFormat};
use futures::future::BoxFuture;
use log::{info, warn};
use serde_json::Value;

use mp_protocol::reminder::{
    CalendarRemindersParams, CalendarRemindersResult, ReminderDismissed, ReminderSnoozed,
};
use mp_protocol::RpcError;

use crate::calendar_backend::{backend_for, CalendarBackend};
use crate::calendar_model::CalendarFeature;
use crate::config::AccountConfig;
use crate::store::Store;

use super::super::config::ConfigStore;
use super::super::dispatch::{
    CancelToken, ClientCtx, Dispatcher, DomainError, Method, MethodKind, MethodSpec, Outcome,
    ResourceId,
};
use super::super::reminders::{
    self as engine, reminders_changed, reminding_accounts, Decision, Refusal, Reminders,
    MAX_SNOOZE_MINUTES,
};
use super::super::runtime::account::off_thread;
use super::super::state::CanonicalState;
use super::{internal, invalid_params, only_params, server_error, string_param};

/// The family, in method-name order.
pub const REMINDER_METHOD_SPECS: [MethodSpec; 3] = [
    MethodSpec::new("calendar.dismiss_reminder", MethodKind::Command, 1),
    MethodSpec::new("calendar.reminders", MethodKind::Query, 1),
    MethodSpec::new("calendar.snooze_reminder", MethodKind::Command, 1),
];

/// Register the family on `dispatcher`.
pub fn register(
    dispatcher: &mut Dispatcher,
    config: Arc<ConfigStore>,
    canonical: Arc<CanonicalState>,
    reminders: Arc<Reminders>,
) {
    for spec in REMINDER_METHOD_SPECS {
        dispatcher.register(Arc::new(ReminderMethod {
            spec,
            config: Arc::clone(&config),
            canonical: Arc::clone(&canonical),
            reminders: Arc::clone(&reminders),
        }));
    }
}

/// One method of the family, selected by its own [`MethodSpec`].
pub struct ReminderMethod {
    spec: MethodSpec,
    config: Arc<ConfigStore>,
    canonical: Arc<CanonicalState>,
    reminders: Arc<Reminders>,
}

impl Method for ReminderMethod {
    fn spec(&self) -> MethodSpec {
        self.spec
    }

    fn call<'a>(
        &'a self,
        _ctx: &'a ClientCtx,
        params: Value,
        _cancel: CancelToken,
    ) -> BoxFuture<'a, Result<Outcome, DomainError>> {
        Box::pin(async move {
            let accounts = self.config.accounts();
            let answer = match self.spec.name {
                "calendar.reminders" => self.list(&params, &accounts).map(Outcome::query),
                "calendar.dismiss_reminder" => self.decide(&params, &accounts, false),
                _ => self.decide(&params, &accounts, true),
            };
            answer.map_err(DomainError::from)
        })
    }
}

impl ReminderMethod {
    /// `calendar.reminders`: the pending reminders of every reminding account
    /// or of those named, the earliest start first, each with the presenter
    /// as it stands now.
    fn list(&self, params: &Value, accounts: &[AccountConfig]) -> Result<Value, RpcError> {
        only_params("calendar.reminders", params, &["accounts"])?;
        let request: CalendarRemindersParams = serde_json::from_value(params.clone())
            .map_err(|e| invalid_params(format!("calendar.reminders: {e}")))?;
        let reminding = reminding_accounts(accounts);
        let selected: Vec<&AccountConfig> = match &request.accounts {
            Some(names) => names
                .iter()
                .map(|name| super::account::ready_account(accounts, name))
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .filter(|account| reminding.iter().any(|r| r.name == account.name))
                .collect(),
            None => reminding.iter().collect(),
        };
        let now = self.reminders.now();
        let viewer = crate::calendar_view::local_zone();
        let presenter = self.canonical.presenter();
        let mut reminders = Vec::new();
        for account in selected {
            let store = open_store(&account.name)?;
            let listed = engine::pending(&store, &account.name, now, viewer)
                .map_err(|e| server_error(&account.name, &e))?;
            reminders.extend(listed);
        }
        reminders.sort_by(|a, b| a.start.cmp(&b.start).then_with(|| a.event.cmp(&b.event)));
        for reminder in &mut reminders {
            engine::present(reminder, presenter);
        }
        serde_json::to_value(CalendarRemindersResult { reminders })
            .map_err(|e| internal(format!("serialising an answer: {e}")))
    }

    /// `calendar.dismiss_reminder` or, with `snooze`, `calendar.snooze_reminder`.
    fn decide(
        &self,
        params: &Value,
        accounts: &[AccountConfig],
        snooze: bool,
    ) -> Result<Outcome, RpcError> {
        let method = self.spec.name;
        if snooze {
            only_params(method, params, &["account", "event", "minutes"])?;
        } else {
            only_params(method, params, &["account", "event"])?;
        }
        let name = string_param(params, "account")?;
        let event = string_param(params, "event")?;
        let account = super::account::ready_account(accounts, &name)?.clone();
        let now = self.reminders.now();
        let decision = if snooze {
            let minutes = params
                .get("minutes")
                .and_then(Value::as_u64)
                .filter(|minutes| (1..=u64::from(MAX_SNOOZE_MINUTES)).contains(minutes))
                .ok_or_else(|| {
                    invalid_params(format!(
                        "minutes is a required whole number from 1 to {MAX_SNOOZE_MINUTES}"
                    ))
                })?;
            let minutes = i64::try_from(minutes).unwrap_or(i64::from(MAX_SNOOZE_MINUTES));
            Decision::Snooze(now + Duration::minutes(minutes))
        } else {
            Decision::Dismiss
        };
        let store = open_store(&name)?;
        let viewer = crate::calendar_view::local_zone();
        match engine::decide(&store, &name, &event, decision, viewer)
            .map_err(|e| server_error(&name, &e))?
        {
            Ok(()) => {}
            Err(Refusal::NoEvent) => {
                return Err(invalid_params(format!("{name} has no event {event}")))
            }
            Err(Refusal::NoReminder) => {
                return Err(invalid_params(format!(
                    "{event} has no reminder: it has none set, or it was cancelled or declined"
                )))
            }
            Err(Refusal::Dismissed) => {
                return Err(invalid_params(format!(
                    "the reminder of {event} was dismissed; there is nothing to snooze"
                )))
            }
        }
        self.reminders.forget(&name, &event);
        self.reminders.poke();
        let revision = self.canonical.publish(reminders_changed(&name));
        tell_the_server(account, event.clone(), decision);
        let result = match decision {
            Decision::Dismiss => serde_json::to_value(ReminderDismissed {
                account: name.clone(),
                event: event.clone(),
            }),
            Decision::Snooze(until) => serde_json::to_value(ReminderSnoozed {
                account: name.clone(),
                event: event.clone(),
                until: until.to_rfc3339_opts(SecondsFormat::Secs, true),
            }),
        }
        .map_err(|e| internal(format!("serialising an answer: {e}")))?;
        Ok(Outcome::command(
            result,
            revision.get(),
            vec![ResourceId::new(format!("reminder:{name}/{event}"))],
        ))
    }
}

/// Send `decision` to the account's server when its backend serves
/// `server-reminders`, after the answer: the local record is what the clients
/// read, and a failed call is logged and leaves it in place.
fn tell_the_server(account: AccountConfig, event: String, decision: Decision) {
    if !crate::calendar_backend::features(&account).contains(&CalendarFeature::ServerReminders) {
        return;
    }
    tokio::spawn(async move {
        let name = account.name.clone();
        let what = match decision {
            Decision::Dismiss => "dismissal",
            Decision::Snooze(_) => "snooze",
        };
        let sent = off_thread("the reminder decision", move || async move {
            let backend = backend_for(&account)
                .await?
                .ok_or_else(|| anyhow::anyhow!("{} has no calendar backend", account.name))?;
            match decision {
                Decision::Dismiss => backend.dismiss_reminder(&event).await,
                Decision::Snooze(until) => backend.snooze_reminder(&event, until).await,
            }
        })
        .await;
        match sent {
            Ok(Ok(())) => info!("[reminders] {name}: the server took the reminder {what}"),
            Ok(Err(e)) => warn!("[reminders] {name}: the server refused the reminder {what}: {e}"),
            Err(e) => warn!("[reminders] {name}: the reminder {what} did not run: {e:#}"),
        }
    });
}

fn open_store(name: &str) -> Result<Store, RpcError> {
    Store::open(crate::config::store_path(name)).map_err(|e| server_error(name, &e))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The family is in method-name order, one query and two commands.
    #[test]
    fn the_family_is_one_query_and_two_commands_in_name_order() {
        let names: Vec<&str> = REMINDER_METHOD_SPECS.iter().map(|spec| spec.name).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted);
        let kinds: Vec<MethodKind> = REMINDER_METHOD_SPECS.iter().map(|spec| spec.kind).collect();
        assert_eq!(
            kinds,
            [MethodKind::Command, MethodKind::Query, MethodKind::Command]
        );
    }
}
