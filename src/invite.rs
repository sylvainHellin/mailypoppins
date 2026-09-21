//! The send-side half of iMIP that reads an account (#0126, P5-U10a).
//!
//! The ICS building, the RSVP reply and the date/duration grammar are
//! [`mp_core::invite`] and are re-exported here, so `crate::invite::Rsvp`,
//! `crate::invite::build_invite_ics` and the rest resolve as they did when
//! this file held all of it. What stayed is the one function that needs an
//! account: `plan_invite` reads the account's auth method to refuse Graph, its
//! `default_from` for the `ORGANIZER`, and `send::split_addresses` for the
//! attendee list.

use anyhow::{anyhow, Result};

pub use mp_core::invite::*;

/// One sentence, refused in two places: the client, which must not preview an
/// invitation it is going to refuse, and `send.invite`, which refuses it before
/// it looks at anything else so a GUI can disable the action with its reason.
pub const GRAPH_REFUSAL: &str = "`mp send --invite` is not supported for Graph accounts yet \
                                (Graph calendar send is tracked by #0036, blocked on #0035). \
                                Use an SMTP-configured account.";

/// The flags `mp send --invite` takes, unresolved.
#[derive(Debug, Clone, Default)]
pub struct InviteRequest {
    pub to: Option<String>,
    pub cc: Option<String>,
    pub subject: Option<String>,
    pub start: Option<String>,
    pub end: Option<String>,
    pub duration: Option<String>,
    pub location: Option<String>,
    pub description: Option<String>,
}

/// One validated invitation: the preview the client renders and the `VEVENT`
/// the daemon builds, from one resolution of the same flags.
#[derive(Debug, Clone)]
pub struct InvitePlan {
    /// The event `SUMMARY`, which is also the message subject.
    pub subject: String,
    /// The `To:` header, exactly as it was typed.
    pub to_field: Option<String>,
    /// The `Cc:` header, likewise.
    pub cc_field: Option<String>,
    /// The spec the ICS is built from.
    pub spec: InviteSpec,
    /// The human-readable body: the description, or a one-line summary.
    pub body: String,
}

/// Validate one invitation request against the account that would send it.
///
/// The order of the refusals is the contract, because it is the order the user
/// has always met them in: the account first (`ANO-4`, decided before anything
/// else about the invitation is looked at), then the subject, the start, the
/// times, the recipients and the organizer.
///
/// `uid` is the client's when it has already previewed one, so the UID the user
/// read is the UID that goes out, and minted here otherwise.
pub fn plan_invite(
    account: &crate::config::AccountConfig,
    request: &InviteRequest,
    uid: Option<&str>,
) -> Result<InvitePlan> {
    if account.auth_method == crate::config::AuthMethod::Graph {
        return Err(anyhow!("{GRAPH_REFUSAL}"));
    }
    let subject = request
        .subject
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("--invite requires --subject (used as the event summary)"))?;
    let start = request
        .start
        .as_deref()
        .ok_or_else(|| anyhow!("--invite requires --start"))?;
    let (start_dt, end_dt) = resolve_times(
        start,
        request.end.as_deref(),
        request.duration.as_deref(),
    )?;

    // Attendees from --to/--cc (deduplicated, bare addresses). The To/Cc
    // headers keep the full form; ATTENDEE lines take the extracted address.
    let to_field = request.to.as_deref().filter(|s| !s.trim().is_empty());
    let cc_field = request.cc.as_deref().filter(|s| !s.trim().is_empty());
    let mut attendees: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for field in [to_field, cc_field].into_iter().flatten() {
        for raw in crate::send::split_addresses(field) {
            let address = crate::parse::extract_email_address(&raw);
            if !address.is_empty() && seen.insert(address.to_lowercase()) {
                attendees.push(address);
            }
        }
    }
    if attendees.is_empty() {
        return Err(anyhow!(
            "--invite requires at least one recipient via --to/--cc"
        ));
    }

    // ORGANIZER = the sending account's primary address (it must match, or
    // Exchange silently drops the invitation).
    let organizer = crate::parse::extract_email_address(&account.default_from);
    if organizer.is_empty() {
        return Err(anyhow!(
            "Account has no usable primary address (default_from); cannot set ORGANIZER"
        ));
    }

    let body = request
        .description
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("You are invited to: {subject}"));

    Ok(InvitePlan {
        subject: subject.to_string(),
        to_field: to_field.map(str::to_string),
        cc_field: cc_field.map(str::to_string),
        spec: InviteSpec {
            uid: uid
                .map(str::to_string)
                .unwrap_or_else(|| generate_uid(&organizer)),
            organizer,
            attendees,
            summary: subject.to_string(),
            start: start_dt,
            end: end_dt,
            location: request.location.clone().filter(|s| !s.trim().is_empty()),
            description: request.description.clone().filter(|s| !s.trim().is_empty()),
        },
        body,
    })
}
