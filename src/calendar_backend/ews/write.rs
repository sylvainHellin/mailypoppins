//! The EWS calendar writes (PERSO-256, `.agents/plans/calendar.md`, phase 3,
//! units 6 and 7).
//!
//! The spike ran no write against the server
//! (`.agents/research/2026-11-13-ews-spike.md`, "Still unknown"): every
//! envelope here is built from Microsoft's EWS schema documentation and is
//! unverified live, each marked so where it is built.
//!
//! - An answer to an invitation is a response object (`AcceptItem`,
//!   `TentativelyAcceptItem`, `DeclineItem`) made through `CreateItem` on the
//!   event's `ItemId`, with the comment as its body. `SendAndSaveCopy` tells
//!   the organizer, `SaveOnly` records the answer without telling anybody.
//!   A tentative or a decline may carry `ProposedStart` and `ProposedEnd`, a
//!   new time the organizer receives as a proposal (`Exchange2013` and
//!   later).
//!
//! Every text the user typed goes out escaped ([`xml::escape`]).

use super::fetch::EwsFetch;
use super::{answer_error, parse, utc_text, xml, EwsCalendarBackend};
use crate::calendar_backend::{BackendResult, Response};
use crate::calendar_write::ProposedTime;

/// A plain-text `Body` holding `text`, escaped.
fn body_element(text: &str) -> String {
    format!("<t:Body BodyType=\"Text\">{}</t:Body>", xml::escape(text))
}

/// The `CreateItem` that answers the invitation `event`.
///
/// Built from the schema documentation, unverified live: the response
/// object's children go in the schema's order (`Body`, `ReferenceItemId`,
/// then `ProposedStart` and `ProposedEnd`), and a proposal rides on a
/// tentative or a decline only, as the schema puts it on
/// `MeetingRegistrationResponseObjectType` and Graph takes it.
pub(super) fn response_body(
    event: &str,
    response: Response,
    comment: Option<&str>,
    send_response: bool,
    proposed: Option<&ProposedTime>,
) -> String {
    let element = match response {
        Response::Accept => "AcceptItem",
        Response::Tentative => "TentativelyAcceptItem",
        Response::Decline => "DeclineItem",
    };
    let disposition = if send_response {
        "SendAndSaveCopy"
    } else {
        "SaveOnly"
    };
    let body = comment
        .filter(|comment| !comment.trim().is_empty())
        .map(body_element)
        .unwrap_or_default();
    let proposal = proposed
        .filter(|_| response != Response::Accept)
        .map(|proposed| {
            format!(
                "<t:ProposedStart>{}</t:ProposedStart><t:ProposedEnd>{}</t:ProposedEnd>",
                utc_text(proposed.slot.start),
                utc_text(proposed.slot.end)
            )
        })
        .unwrap_or_default();
    format!(
        "<m:CreateItem MessageDisposition=\"{disposition}\"><m:Items><t:{element}>{body}\
         <t:ReferenceItemId Id=\"{}\"/>{proposal}</t:{element}></m:Items></m:CreateItem>",
        xml::escape(event)
    )
}

impl<F: EwsFetch> EwsCalendarBackend<F> {
    /// Answer the invitation `event` ([`response_body`]): the server's one
    /// response message, or its refusal as a sentence.
    pub(super) async fn send_response(
        &self,
        event: &str,
        response: Response,
        comment: Option<&str>,
        send_response: bool,
        proposed: Option<&ProposedTime>,
    ) -> BackendResult<()> {
        let body = response_body(event, response, comment, send_response, proposed);
        let answer = self.call("CreateItem", &body).await?;
        parse::only_message(&answer, "CreateItem")
            .map_err(|error| answer_error(&error, "CreateItem"))?;
        Ok(())
    }
}
