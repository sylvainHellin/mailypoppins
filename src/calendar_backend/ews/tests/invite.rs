//! The event an invitation mail stands for (`event_for_message`): the mail
//! found by its Message-ID, and its `AssociatedCalendarItemId` read, on the
//! spike's meeting mails from Deleted Items.

use super::super::fetch::envelope;
use super::super::EwsCalendarBackend;
use super::{built_answer, success_message, ScriptedFetch};
use crate::calendar_backend::CalendarBackend;
use crate::config::EwsPasswordSource;

const FOUND: &str = include_str!("../../../../tests/fixtures/ews/finditem_meeting_request.xml");
const REQUEST: &str = include_str!("../../../../tests/fixtures/ews/getitem_meeting_request.xml");
const CANCELLATION: &str =
    include_str!("../../../../tests/fixtures/ews/getitem_meeting_cancellation.xml");
const RESPONSE: &str = include_str!("../../../../tests/fixtures/ews/getitem_meeting_response.xml");
/// TUM's answer of 2026-11-18 to the Message-ID restriction over the Inbox
/// and Deleted Items: one response message per folder, the invitation in the
/// first (ids sanitised).
const TWO_FOLDERS: &str =
    include_str!("../../../../tests/fixtures/ews/finditem_message_id_two_folders.xml");

fn backend(fetch: ScriptedFetch) -> EwsCalendarBackend<ScriptedFetch> {
    EwsCalendarBackend::with_fetch("tum", EwsPasswordSource::Imap, fetch)
}

/// The text of the first `Id` attribute after `tag` in `xml`.
fn id_after<'a>(xml: &'a str, tag: &str) -> &'a str {
    let at = xml.find(tag).expect("the tag") + tag.len();
    let rest = &xml[at..];
    let start = rest.find("Id=\"").unwrap() + 4;
    &rest[start..start + rest[start..].find('"').unwrap()]
}

/// The text of the element `name` in `xml`, unescaped of `&lt;` and `&gt;`.
fn text_of(xml: &str, name: &str) -> String {
    let open = format!("<t:{name}>");
    let start = xml.find(&open).unwrap() + open.len();
    xml[start..start + xml[start..].find('<').unwrap()]
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

/// The FindItem fixture cut to its first meeting request, the one a
/// restriction on its Message-ID finds.
fn found_one() -> String {
    let second = FOUND.match_indices("<t:MeetingRequest>").nth(1).unwrap().0;
    let end = FOUND.find("</t:Items>").unwrap();
    format!("{}{}", &FOUND[..second], &FOUND[end..])
}

/// An invitation mail is found by its Message-ID, angle brackets added when
/// the caller left them out and escaped in the restriction, in the Inbox
/// and Deleted Items, then read for the calendar item it is about.
#[tokio::test]
async fn an_invitation_mail_names_its_event_through_its_associated_item() {
    let backend = backend(
        ScriptedFetch::default()
            .answer("FindItem", &found_one())
            .answer("GetItem", REQUEST),
    );
    let message_id = text_of(REQUEST, "InternetMessageId");
    let bare = message_id.trim_start_matches('<').trim_end_matches('>');
    let event = backend.event_for_message(bare).await.unwrap();
    assert_eq!(
        event.as_deref(),
        Some(id_after(REQUEST, "<t:AssociatedCalendarItemId"))
    );
    let escaped = message_id.replace('<', "&lt;").replace('>', "&gt;");
    let mail = id_after(REQUEST, "<t:MeetingRequest>");
    assert_eq!(
        backend.fetch().sent(),
        [
            envelope(&format!(
                "<m:FindItem Traversal=\"Shallow\"><m:ItemShape><t:BaseShape>IdOnly</t:BaseShape>\
                 </m:ItemShape><m:Restriction><t:IsEqualTo>\
                 <t:FieldURI FieldURI=\"message:InternetMessageId\"/><t:FieldURIOrConstant>\
                 <t:Constant Value=\"{escaped}\"/></t:FieldURIOrConstant></t:IsEqualTo>\
                 </m:Restriction><m:ParentFolderIds><t:DistinguishedFolderId Id=\"inbox\"/>\
                 <t:DistinguishedFolderId Id=\"deleteditems\"/></m:ParentFolderIds></m:FindItem>"
            )),
            envelope(&format!(
                "<m:GetItem><m:ItemShape><t:BaseShape>AllProperties</t:BaseShape>\
                 </m:ItemShape><m:ItemIds><t:ItemId Id=\"{mail}\"/></m:ItemIds></m:GetItem>"
            )),
        ]
    );
}

/// The server answers a FindItem over two folders with one response message
/// per folder, as it did live, and the mail is found in whichever holds it.
#[tokio::test]
async fn a_mail_is_found_in_either_folder_of_the_two_messages() {
    let found = backend(
        ScriptedFetch::default()
            .answer("FindItem", TWO_FOLDERS)
            .answer("GetItem", REQUEST),
    );
    let event = found.event_for_message("<a@b>").await.unwrap();
    assert_eq!(
        event.as_deref(),
        Some(id_after(REQUEST, "<t:AssociatedCalendarItemId"))
    );
    let mail = id_after(TWO_FOLDERS, "<t:MeetingRequest>");
    assert!(found.fetch().sent()[1].contains(&format!("<t:ItemId Id=\"{mail}\"/>")));
    // The same answer with the folders the other way round.
    let first = TWO_FOLDERS.find("<m:FindItemResponseMessage").unwrap();
    let second = TWO_FOLDERS.rfind("<m:FindItemResponseMessage").unwrap();
    let end = TWO_FOLDERS.find("</m:ResponseMessages>").unwrap();
    let swapped = format!(
        "{}{}{}{}",
        &TWO_FOLDERS[..first],
        &TWO_FOLDERS[second..end],
        &TWO_FOLDERS[first..second],
        &TWO_FOLDERS[end..]
    );
    let found = backend(
        ScriptedFetch::default()
            .answer("FindItem", &swapped)
            .answer("GetItem", REQUEST),
    );
    assert!(found.event_for_message("<a@b>").await.unwrap().is_some());
    assert!(found.fetch().sent()[1].contains(&format!("<t:ItemId Id=\"{mail}\"/>")));
}

/// A cancellation and a response name their event the same way.
#[tokio::test]
async fn a_cancellation_and_a_response_name_their_event_too() {
    for (mail, tag) in [
        (CANCELLATION, "<t:MeetingCancellation>"),
        (RESPONSE, "<t:MeetingResponse>"),
    ] {
        let found = found_one().replace(
            id_after(&found_one(), "<t:MeetingRequest>"),
            id_after(mail, tag),
        );
        let backend = backend(
            ScriptedFetch::default()
                .answer("FindItem", &found)
                .answer("GetItem", mail),
        );
        let event = backend
            .event_for_message(&text_of(mail, "InternetMessageId"))
            .await
            .unwrap();
        assert_eq!(
            event.as_deref(),
            Some(id_after(mail, "<t:AssociatedCalendarItemId")),
            "{tag}"
        );
    }
}

/// A mail the server does not find, and one that is no meeting mail, stand
/// for no event; the first asks nothing more.
#[tokio::test]
async fn a_mail_not_found_or_no_meeting_mail_stands_for_nothing() {
    let none = built_answer(
        "FindItem",
        &success_message(
            "FindItem",
            "<m:RootFolder TotalItemsInView=\"0\" IncludesLastItemInRange=\"true\"><t:Items/>\
             </m:RootFolder>",
        ),
    );
    let plain = built_answer(
        "GetItem",
        &success_message(
            "GetItem",
            "<m:Items><t:Message><t:ItemId Id=\"plain\" ChangeKey=\"k\"/></t:Message></m:Items>",
        ),
    );
    let backend = backend(
        ScriptedFetch::default()
            .answer("FindItem", &none)
            .answer("FindItem", &found_one())
            .answer("GetItem", &plain),
    );
    assert_eq!(backend.event_for_message("<a@b>").await.unwrap(), None);
    assert_eq!(backend.fetch().sent().len(), 1);
    assert_eq!(backend.event_for_message("<c@d>").await.unwrap(), None);
    assert!(backend.fetch().exhausted());
}
