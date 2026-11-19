//! The Graph requests of the drafts mirror (#0143 slice 3, PERSO-182): list
//! the Drafts folder's drafts, find them by `Message-ID`, create a copy from
//! MIME, read one's MIME, and remove one for good.
//!
//! Every request asks for immutable ids ([`PREFER_IMMUTABLE_ID`]), so the id a
//! listing answers with is the one a later `$value` or `permanentDelete`
//! names, wherever Outlook moved the item in between. The mirror over these
//! requests is [`crate::draft_mirror::graph_folder`].

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

use super::{
    internet_id_url, message_action_url, GraphClient, GraphExtendedProperty, GraphRecipient,
    GRAPH_BASE, IN_REPLY_TO_PROPERTY,
};

/// The Drafts folder by Graph's well-known name, the folder every mirrored
/// copy lives in and the name the mirror records it under.
pub const DRAFTS_FOLDER: &str = "drafts";

/// What a listing reads of each draft: its identity and version, and the
/// headers the reconcile links a copy to a file by.
const HEAD_SELECT: &str =
    "id,changeKey,internetMessageId,subject,toRecipients,ccRecipients,lastModifiedDateTime,isDraft";

/// The `$expand` that brings a draft's `In-Reply-To`, `PidTagInReplyToId`,
/// percent-encoded; a draft without one carries no
/// `singleValueExtendedProperties` at all.
const IN_REPLY_TO_EXPAND: &str =
    "$expand=singleValueExtendedProperties($filter=id%20eq%20'String%200x1042')";

/// Drafts per listing page.
const LIST_PAGE_SIZE: usize = 100;

/// Pages a listing follows before it gives up: 5 000 drafts. A listing that
/// stops short is an error, never a short answer, since the reconcile reads a
/// copy missing from it as one sent or deleted elsewhere.
const MAX_LIST_PAGES: usize = 50;

/// One draft of the Drafts folder, as a listing or a create answers with it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GraphDraftHead {
    /// The immutable id.
    pub id: String,
    /// The version Exchange gives the item, which every edit changes.
    pub change_key: String,
    /// The RFC 5322 `Message-ID`, brackets included as Graph spells it.
    pub internet_message_id: Option<String>,
    pub subject: String,
    /// The `To` addresses.
    pub to: Vec<String>,
    /// The `Cc` addresses.
    pub cc: Vec<String>,
    /// `In-Reply-To`, read from `PidTagInReplyToId`.
    pub in_reply_to: Option<String>,
    /// `lastModifiedDateTime`, ISO 8601 in UTC as Graph writes it, which
    /// orders versions as text.
    pub last_modified: String,
    /// Whether the item is still a draft.
    pub is_draft: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DraftEntry {
    id: String,
    #[serde(default)]
    change_key: Option<String>,
    #[serde(default)]
    internet_message_id: Option<String>,
    #[serde(default)]
    subject: Option<String>,
    #[serde(default)]
    to_recipients: Vec<GraphRecipient>,
    #[serde(default)]
    cc_recipients: Vec<GraphRecipient>,
    #[serde(default)]
    last_modified_date_time: Option<String>,
    #[serde(default)]
    is_draft: Option<bool>,
    #[serde(default)]
    single_value_extended_properties: Vec<GraphExtendedProperty>,
}

#[derive(Debug, Deserialize)]
struct DraftPage {
    #[serde(default)]
    value: Vec<DraftEntry>,
    #[serde(rename = "@odata.nextLink", default)]
    next_link: Option<String>,
}

fn addresses(recipients: &[GraphRecipient]) -> Vec<String> {
    recipients
        .iter()
        .filter_map(|r| r.email_address.address.clone())
        .filter(|a| !a.trim().is_empty())
        .collect()
}

impl From<DraftEntry> for GraphDraftHead {
    fn from(entry: DraftEntry) -> Self {
        GraphDraftHead {
            change_key: entry.change_key.unwrap_or_default(),
            internet_message_id: entry
                .internet_message_id
                .filter(|mid| !mid.trim().is_empty()),
            subject: entry.subject.unwrap_or_default(),
            to: addresses(&entry.to_recipients),
            cc: addresses(&entry.cc_recipients),
            in_reply_to: entry
                .single_value_extended_properties
                .iter()
                .find(|p| p.id.eq_ignore_ascii_case(IN_REPLY_TO_PROPERTY))
                .and_then(|p| p.value.clone())
                .filter(|v| !v.trim().is_empty()),
            last_modified: entry.last_modified_date_time.unwrap_or_default(),
            // A create answers with the whole resource; an item that does not
            // say is read as a draft, which is what the folder holds.
            is_draft: entry.is_draft.unwrap_or(true),
            id: entry.id,
        }
    }
}

/// `me/mailFolders/{folder}/messages`, every draft of the folder by page.
fn list_url(folder: &str) -> Result<reqwest::Url> {
    let base = format!("{GRAPH_BASE}/me/mailFolders/{folder}/messages");
    let mut url = reqwest::Url::parse(&base).with_context(|| format!("parsing {base}"))?;
    url.set_query(Some(&format!(
        "$select={HEAD_SELECT}&$top={LIST_PAGE_SIZE}&{IN_REPLY_TO_EXPAND}"
    )));
    Ok(url)
}

/// The drafts of `folder` carrying `message_id`.
fn find_url(folder: &str, message_id: &str) -> Result<reqwest::Url> {
    let mut url = internet_id_url(
        &format!("me/mailFolders/{folder}/messages"),
        message_id,
        HEAD_SELECT,
    )?;
    let query = format!("{}&{IN_REPLY_TO_EXPAND}", url.query().unwrap_or_default());
    url.set_query(Some(&query));
    Ok(url)
}

/// The error a draft request's non-2xx answer becomes.
async fn status_error(resp: reqwest::Response, what: &str) -> anyhow::Error {
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    anyhow!("{what} failed (HTTP {status}): {body}")
}

impl GraphClient {
    /// Every draft `folder` holds, by its [`GraphDraftHead`]; an item that is
    /// no longer a draft is left out.
    pub async fn list_drafts(&self, folder: &str) -> Result<Vec<GraphDraftHead>> {
        self.draft_pages(list_url(folder)?, "Listing the Graph drafts")
            .await
    }

    /// The drafts of `folder` carrying `message_id`.
    pub async fn find_drafts(&self, folder: &str, message_id: &str) -> Result<Vec<GraphDraftHead>> {
        self.draft_pages(
            find_url(folder, message_id)?,
            "Looking a Graph draft up by Message-ID",
        )
        .await
    }

    /// Follow a draft listing from `url` to its last page.
    async fn draft_pages(&self, url: reqwest::Url, what: &str) -> Result<Vec<GraphDraftHead>> {
        let mut heads = Vec::new();
        let mut next = Some(url);
        for _ in 0..MAX_LIST_PAGES {
            let Some(url) = next.take() else {
                return Ok(heads);
            };
            let resp = self
                .item_request(reqwest::Method::GET, url)
                .send()
                .await
                .with_context(|| what.to_string())?;
            if !resp.status().is_success() {
                return Err(status_error(resp, what).await);
            }
            let page: DraftPage = resp
                .json()
                .await
                .with_context(|| format!("{what}: reading the answer"))?;
            heads.extend(
                page.value
                    .into_iter()
                    .map(GraphDraftHead::from)
                    .filter(|head| head.is_draft),
            );
            next = page
                .next_link
                .map(|link| reqwest::Url::parse(&link))
                .transpose()
                .context("parsing the next page of the drafts listing")?;
        }
        match next {
            None => Ok(heads),
            Some(_) => Err(anyhow!(
                "{what}: more than {MAX_LIST_PAGES} pages, the listing was not finished"
            )),
        }
    }

    /// Create a draft in Drafts from a whole RFC 5322 message and answer with
    /// what Graph made of it: `POST /me/messages` with the base64 MIME, the
    /// request the staged send creates its MIME drafts with.
    pub async fn create_draft_copy(&self, mime: &[u8]) -> Result<GraphDraftHead> {
        let what = "Creating the Graph draft copy";
        let resp = self
            .create_draft_mime_request(super::mime_send_body(mime))
            .send()
            .await
            .context(what)?;
        if !resp.status().is_success() {
            return Err(status_error(resp, what).await);
        }
        let entry: DraftEntry = resp
            .json()
            .await
            .with_context(|| format!("{what}: reading the answer"))?;
        Ok(entry.into())
    }

    /// The RFC 822 bytes of the draft `id`, `None` when no item has the id.
    pub async fn draft_mime(&self, id: &str) -> Result<Option<Vec<u8>>> {
        let what = "Reading a Graph draft's MIME content";
        let resp = self.draft_mime_request(id).send().await.context(what)?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !resp.status().is_success() {
            return Err(status_error(resp, what).await);
        }
        let bytes = resp
            .bytes()
            .await
            .with_context(|| format!("{what}: reading the answer"))?;
        Ok(Some(bytes.to_vec()))
    }

    fn draft_mime_request(&self, id: &str) -> reqwest::RequestBuilder {
        self.item_request(reqwest::Method::GET, message_action_url(id, "$value"))
    }

    /// Remove the draft `id` for good (`permanentDelete`, so it does not land
    /// in Deleted Items): `true` when this request removed it, `false` when no
    /// item had the id any more.
    pub async fn remove_draft(&self, id: &str) -> Result<bool> {
        let resp = self
            .discard_draft_request(id)
            .send()
            .await
            .context(REMOVE_WHAT)?;
        removed(resp).await
    }

    /// The `changeKey` the draft `id` holds now, `None` when no item has the
    /// id: `GET /me/messages/{id}?$select=changeKey`.
    pub async fn draft_change_key(&self, id: &str) -> Result<Option<String>> {
        let what = "Reading a Graph draft's change key";
        let resp = self.change_key_request(id)?.send().await.context(what)?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !resp.status().is_success() {
            return Err(status_error(resp, what).await);
        }
        let version: ChangeKey = resp
            .json()
            .await
            .with_context(|| format!("{what}: reading the answer"))?;
        Ok(Some(version.change_key))
    }

    fn change_key_request(&self, id: &str) -> Result<reqwest::RequestBuilder> {
        let mut url = reqwest::Url::parse(&format!("{GRAPH_BASE}/me/messages"))
            .context("parsing the Graph messages URL")?;
        url.path_segments_mut()
            .map_err(|()| anyhow!("the Graph messages URL cannot take a path"))?
            .push(id);
        url.set_query(Some("$select=changeKey"));
        Ok(self.item_request(reqwest::Method::GET, url))
    }

    /// Remove the version `change_key` of the draft `id` for good, and no
    /// other: `permanentDelete` with `If-Match: W/"<changeKey>"`. `true` when
    /// this request removed it; `false` when the item is gone (404) or holds
    /// another version (412), an edit saved since, which stays.
    pub async fn remove_draft_version(&self, id: &str, change_key: &str) -> Result<bool> {
        let resp = self
            .remove_version_request(id, change_key)
            .send()
            .await
            .context(REMOVE_WHAT)?;
        removed(resp).await
    }

    fn remove_version_request(&self, id: &str, change_key: &str) -> reqwest::RequestBuilder {
        self.discard_draft_request(id)
            .header(reqwest::header::IF_MATCH, if_match(change_key))
    }
}

const REMOVE_WHAT: &str = "Removing a Graph draft";

/// The `If-Match` value naming one version: the weak entity tag Graph
/// answers as a message's `@odata.etag`, `W/"<changeKey>"`.
fn if_match(change_key: &str) -> String {
    format!("W/\"{change_key}\"")
}

/// What a removal's answer says: removed, or not there to remove (404, gone;
/// 412, another version).
async fn removed(resp: reqwest::Response) -> Result<bool> {
    let status = resp.status();
    if status == reqwest::StatusCode::NOT_FOUND
        || status == reqwest::StatusCode::PRECONDITION_FAILED
    {
        return Ok(false);
    }
    if !status.is_success() {
        return Err(status_error(resp, REMOVE_WHAT).await);
    }
    Ok(true)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChangeKey {
    change_key: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::PREFER_IMMUTABLE_ID;

    /// The listing reads each draft's identity, version and linking headers,
    /// `In-Reply-To` among them, from the Drafts folder by its well-known
    /// name, a page at a time.
    #[test]
    fn the_listing_asks_for_the_linking_fields_of_the_drafts_folder() {
        let url = list_url(DRAFTS_FOLDER).unwrap();
        assert_eq!(url.path(), "/v1.0/me/mailFolders/drafts/messages");
        let query = url.query().unwrap();
        for field in [
            "id",
            "changeKey",
            "internetMessageId",
            "subject",
            "toRecipients",
            "ccRecipients",
            "lastModifiedDateTime",
            "isDraft",
        ] {
            assert!(query.contains(field), "{field} in {query}");
        }
        assert!(query.contains("$top=100"), "{query}");
        assert!(query.contains("String%200x1042"), "{query}");
        assert!(!query.contains("internetMessageHeaders"), "{query}");
    }

    /// The lookup by `Message-ID` filters the Drafts folder, the id escaped
    /// as every Graph lookup escapes it.
    #[test]
    fn the_lookup_filters_the_drafts_folder_by_message_id() {
        let url = find_url(DRAFTS_FOLDER, "<mp-draft.1+x@evoqs.de>").unwrap();
        assert_eq!(url.path(), "/v1.0/me/mailFolders/drafts/messages");
        let query = url.query().unwrap();
        assert!(query.starts_with("$filter=internetMessageId"), "{query}");
        assert!(query.contains("mp-draft.1%2Bx%40evoqs.de"), "{query}");
        assert!(query.contains("changeKey"), "{query}");
        assert!(query.contains("String%200x1042"), "{query}");
    }

    /// Every request of the mirror asks for immutable ids and carries the
    /// token; the create posts base64 MIME, the read asks for `$value`, and
    /// the removal is a `permanentDelete` of one version, named by
    /// `If-Match`.
    #[test]
    fn every_mirror_request_asks_for_immutable_ids() {
        let client = GraphClient::for_tests();
        let create = client
            .create_draft_mime_request(crate::graph::mime_send_body(b"Subject: x\r\n\r\nbody"))
            .build()
            .unwrap();
        assert_eq!(create.method(), reqwest::Method::POST);
        assert_eq!(create.url().path(), "/v1.0/me/messages");
        assert_eq!(
            create.headers().get(reqwest::header::CONTENT_TYPE).unwrap(),
            "text/plain"
        );
        let mime = client.draft_mime_request("AAMk-1").build().unwrap();
        assert_eq!(mime.method(), reqwest::Method::GET);
        assert_eq!(mime.url().path(), "/v1.0/me/messages/AAMk-1/$value");
        let remove = client
            .remove_version_request("AAMk-1", "CQAB")
            .build()
            .unwrap();
        assert_eq!(
            remove.headers().get(reqwest::header::IF_MATCH).unwrap(),
            r#"W/"CQAB""#
        );
        let version = client
            .change_key_request("AAMk-1")
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(version.method(), reqwest::Method::GET);
        assert_eq!(version.url().path(), "/v1.0/me/messages/AAMk-1");
        assert_eq!(version.url().query(), Some("$select=changeKey"));
        assert_eq!(remove.method(), reqwest::Method::POST);
        assert_eq!(
            remove.url().path(),
            "/v1.0/me/messages/AAMk-1/permanentDelete"
        );
        let list = client
            .item_request(reqwest::Method::GET, list_url(DRAFTS_FOLDER).unwrap())
            .build()
            .unwrap();
        for (what, request) in [
            ("create", create),
            ("mime", mime),
            ("remove", remove),
            ("version", version),
            ("list", list),
        ] {
            assert_eq!(
                request.headers().get("Prefer").map(|v| v.to_str().unwrap()),
                Some(PREFER_IMMUTABLE_ID),
                "{what}"
            );
            assert!(
                request
                    .headers()
                    .contains_key(reqwest::header::AUTHORIZATION),
                "{what}"
            );
        }
    }

    /// A listing page as Graph answers it: recipients by address, the reply
    /// target from its MAPI property, a draft's version, and an item that is
    /// no longer a draft told apart.
    #[test]
    fn a_listing_page_reads_into_draft_heads() {
        let page: DraftPage = serde_json::from_value(serde_json::json!({
            "value": [
                {
                    "@odata.etag": "W/\"CQAAABYAAAA\"",
                    "id": "AAMk-1",
                    "changeKey": "CQAAABYAAAA",
                    "internetMessageId": "<mp-draft.1@evoqs.de>",
                    "subject": "Angebot",
                    "toRecipients": [
                        {"emailAddress": {"name": "Robin", "address": "robin@example.com"}}
                    ],
                    "ccRecipients": [],
                    "lastModifiedDateTime": "2026-11-19T10:00:00Z",
                    "isDraft": true,
                    "singleValueExtendedProperties": [
                        {"id": "String 0x1042", "value": "<parent@example.com>"}
                    ]
                },
                {
                    "id": "AAMk-2",
                    "changeKey": "CQAAABYAAAB",
                    "subject": null,
                    "isDraft": false
                }
            ],
            "@odata.nextLink": "https://graph.microsoft.com/v1.0/me/mailFolders/drafts/messages?$skip=100"
        }))
        .unwrap();
        assert!(page.next_link.is_some());
        let heads: Vec<GraphDraftHead> = page.value.into_iter().map(Into::into).collect();
        assert_eq!(
            heads[0],
            GraphDraftHead {
                id: "AAMk-1".to_string(),
                change_key: "CQAAABYAAAA".to_string(),
                internet_message_id: Some("<mp-draft.1@evoqs.de>".to_string()),
                subject: "Angebot".to_string(),
                to: vec!["robin@example.com".to_string()],
                cc: Vec::new(),
                in_reply_to: Some("<parent@example.com>".to_string()),
                last_modified: "2026-11-19T10:00:00Z".to_string(),
                is_draft: true,
            }
        );
        assert!(!heads[1].is_draft);
        assert_eq!(heads[1].subject, "");
        assert_eq!(heads[1].internet_message_id, None);
    }
}
