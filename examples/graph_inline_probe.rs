//! The Graph inline-image probe of PERSO-165 (#0145).
//!
//! Answers, against a live Graph account, which shape of the attachment list
//! returns `isInline` and `contentId` without `contentBytes`, and what it costs
//! in bytes beside the full list the ingest downloads.
//!
//! It links the library, loads the account from `config.toml` and takes its
//! access token through the cached OAuth2 token, the path the daemon takes. It
//! only reads from the server, opens no live store and takes no engine lock,
//! so it runs beside a live daemon.
//!
//! ```text
//! cargo run --example graph_inline_probe -- <account> [folder] [messages]
//! cargo run --example graph_inline_probe -- <account> --backfill <store-copy> <blobs-dir> <mailbox>=<folder>...
//! cargo run --example graph_inline_probe -- <account> --check <store-copy> <blobs-dir> <row-id>
//! ```
//!
//! `--check` prints what the readers make of one row of a store copy: its
//! attachment list and how many `cid:` images its rendition inlines from the
//! stored blobs.
//!
//! `--backfill` runs the PERSO-165 inline backfill against a *copy* of the
//! account's store (take one with `sqlite3 <store> ".backup <copy>"`): the
//! same candidates, the same metadata listing and the same
//! [`mailypoppins::ingest::apply_inline_metadata`] the sync pass runs, with
//! every request measured, and the live store left alone. The blobs directory
//! is only read (the inline-only sweep reads HTML bodies from it).

use anyhow::{anyhow, bail, Context, Result};
use mailypoppins::config::{init_secrets_backend, load_global_config, GraphConfig};

const GRAPH_BASE: &str = "https://graph.microsoft.com/v1.0";

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let account_name = args
        .next()
        .ok_or_else(|| anyhow!("usage: graph_inline_probe <account> [folder] [messages]"))?;
    let folder = args.next().unwrap_or_else(|| "inbox".to_string());
    let backfill = folder == "--backfill";
    let rest: Vec<String> = args.collect();
    if folder == "--check" {
        let [store, blobs, row] = rest.as_slice() else {
            bail!("--check needs <store-copy> <blobs-dir> <row-id>");
        };
        return check_row(store, blobs, row.parse()?);
    }

    let global = load_global_config().context("loading config.toml")?;
    if let Err(e) = init_secrets_backend(&global) {
        if !matches!(e, mailypoppins::secrets::SecretsError::NotInitialized(_)) {
            bail!("initialising the secrets backend: {e}");
        }
    }
    let account = global
        .accounts
        .iter()
        .find(|a| a.name == account_name)
        .ok_or_else(|| anyhow!("no account named '{account_name}' in config.toml"))?
        .clone();
    let config = GraphConfig::load(&account)?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    if backfill {
        let [store, blobs, pairs @ ..] = rest.as_slice() else {
            bail!("--backfill needs <store-copy> <blobs-dir> <mailbox>=<folder>...");
        };
        return runtime.block_on(backfill_copy(&config, store, blobs, pairs));
    }
    let count: usize = rest.first().map(|s| s.parse()).transpose()?.unwrap_or(3);
    runtime.block_on(probe(&config, &folder, count))
}

async fn token(config: &GraphConfig) -> Result<String> {
    mailypoppins::oauth2::load_or_refresh_token(
        &config.account_name,
        &config.client_id,
        &config.tenant_id,
        mailypoppins::oauth2::GRAPH_SCOPES,
    )
    .await
}

/// GET `url`, answering the body's length and its JSON, or the refusal.
async fn get(http: &reqwest::Client, token: &str, url: &str) -> Result<(usize, serde_json::Value)> {
    let response = http.get(url).bearer_auth(token).send().await?;
    let status = response.status();
    let body = response.text().await?;
    if !status.is_success() {
        bail!(
            "HTTP {status}: {}",
            body.chars().take(300).collect::<String>()
        );
    }
    Ok((body.len(), serde_json::from_str(&body)?))
}

async fn probe(config: &GraphConfig, folder: &str, count: usize) -> Result<()> {
    let token = token(config).await?;
    let http = reqwest::Client::new();
    let list = format!(
        "{GRAPH_BASE}/me/mailFolders/{folder}/messages?\
         $filter=hasAttachments%20eq%20true&$select=id,subject&$top={count}"
    );
    let (_, listed) = get(&http, &token, &list).await?;
    for m in listed["value"].as_array().into_iter().flatten() {
        let id = m["id"].as_str().unwrap_or_default();
        println!("{}", m["subject"].as_str().unwrap_or_default());
        let base = format!("{GRAPH_BASE}/me/messages/{id}/attachments");
        let shapes = [
            ("full list", base.clone()),
            (
                "base $select",
                format!("{base}?$select=id,name,contentType,size,isInline"),
            ),
            (
                "derived property in $select",
                format!(
                    "{base}?$select=id,name,contentType,size,isInline,\
                     microsoft.graph.fileAttachment/contentId"
                ),
            ),
            (
                "type-cast segment",
                format!(
                    "{base}/microsoft.graph.fileAttachment?\
                     $select=id,name,contentType,size,isInline,contentId"
                ),
            ),
        ];
        for (label, url) in shapes {
            match get(&http, &token, &url).await {
                Ok((bytes, json)) => {
                    let rows = json["value"].as_array().cloned().unwrap_or_default();
                    let ids = rows.iter().filter(|a| a.get("contentId").is_some()).count();
                    let content = rows
                        .iter()
                        .filter(|a| a.get("contentBytes").is_some())
                        .count();
                    println!(
                        "  {label}: {} entries, {bytes} bytes, {ids} carry contentId, \
                         {content} carry contentBytes",
                        rows.len()
                    );
                    if label == "type-cast segment" {
                        for a in &rows {
                            println!(
                                "    {} inline={} cid={} type={}",
                                a["name"], a["isInline"], a["contentId"], a["contentType"]
                            );
                        }
                    }
                }
                Err(e) => println!("  {label}: refused, {e:#}"),
            }
        }
    }
    Ok(())
}

/// The inline backfill of the sync pass, against a store copy: the local
/// sweep and raw classification, then for each mailbox every metadata lookup
/// and every inline-only repair the passes would make, unbounded and
/// measured.
///
/// The repair downloads attachment content and writes blob files, so
/// `blobs_dir` must be a copy too (`cp -cR` clones it on APFS).
async fn backfill_copy(
    config: &GraphConfig,
    store_path: &str,
    blobs_dir: &str,
    pairs: &[String],
) -> Result<()> {
    use mailypoppins::ingest;
    let store = mailypoppins::store::Store::open(store_path)?;
    let blobs = mailypoppins::store::BlobStore::new(blobs_dir);
    let account = config.account_name.as_str();

    let started = std::time::Instant::now();
    let queued = ingest::mark_inline_only_rows(&store, &blobs, account)?;
    let mut from_raw = 0;
    loop {
        let n = ingest::classify_inline_from_raw(&store, &blobs, account, 200)?;
        if n == 0 {
            break;
        }
        from_raw += n;
    }
    println!(
        "local: {queued} inline-only row(s) queued for the repair, {from_raw} row(s) classified \
         from their raw, {:.2}s",
        started.elapsed().as_secs_f64()
    );

    let token = token(config).await?;
    let http = reqwest::Client::new();
    for pair in pairs {
        let (mailbox, folder) = pair
            .split_once('=')
            .ok_or_else(|| anyhow!("expected <mailbox>=<folder>, got {pair}"))?;

        // The folder's ids by Message-ID, as the sync pass's listing holds them.
        let mut ids = std::collections::HashMap::new();
        let mut url = format!(
            "{GRAPH_BASE}/me/mailFolders/{folder}/messages?$select=id,internetMessageId&$top=200"
        );
        loop {
            let (_, page) = get(&http, &token, &url).await?;
            for m in page["value"].as_array().into_iter().flatten() {
                if let (Some(mid), Some(id)) = (m["internetMessageId"].as_str(), m["id"].as_str()) {
                    ids.insert(mid.trim().to_string(), id.to_string());
                }
            }
            match page["@odata.nextLink"].as_str() {
                Some(next) => url = next.to_string(),
                None => break,
            }
        }

        // The metadata pass.
        let candidates = ingest::inline_lookup_candidates(&store, account, mailbox, usize::MAX)?;
        let started = std::time::Instant::now();
        let (mut bytes, mut gone) = (0usize, 0usize);
        for c in &candidates {
            let listing = match ids.get(&c.message_id) {
                None => {
                    gone += 1;
                    None
                }
                Some(id) => {
                    let url = format!(
                        "{GRAPH_BASE}/me/messages/{id}/attachments?\
                         $select=id,name,isInline,microsoft.graph.fileAttachment/contentId"
                    );
                    let (n, json) = get(&http, &token, &url).await?;
                    bytes += n;
                    Some(server_attachments(&json))
                }
            };
            ingest::apply_inline_metadata(&store, &blobs, c.row_id, listing.as_deref())?;
        }
        println!(
            "{mailbox} ({folder}): metadata for {} row(s), {gone} not in the folder, {bytes} bytes \
             ({} per row), {:.1}s",
            candidates.len(),
            bytes / candidates.len().max(1),
            started.elapsed().as_secs_f64()
        );

        // The attachment repair of the inline-only rows the sweep queued.
        let now = mailypoppins::outbox::unix_now();
        let owed = ingest::attachment_repair_candidates(&store, account, mailbox, now, usize::MAX)?;
        let started = std::time::Instant::now();
        let (mut bytes, mut repaired, mut empty, mut gone) = (0usize, 0usize, 0usize, 0usize);
        for c in &owed {
            let Some(id) = ids.get(&c.message_id) else {
                gone += 1;
                continue;
            };
            let url = format!("{GRAPH_BASE}/me/messages/{id}/attachments");
            let (n, json) = get(&http, &token, &url).await?;
            bytes += n;
            let attachments = file_attachments(&json);
            if attachments.is_empty() {
                ingest::settle_attachment_repair(&store, c.row_id)?;
                empty += 1;
            } else {
                ingest::attach_missing_attachments(&store, &blobs, c.row_id, None, &attachments)?;
                repaired += 1;
            }
        }
        println!(
            "{mailbox} ({folder}): repair of {} row(s), {repaired} repaired, {empty} with nothing \
             to store, {gone} not in the folder, {bytes} bytes, {:.1}s",
            owed.len(),
            started.elapsed().as_secs_f64()
        );
    }
    Ok(())
}

/// What the readers make of one row: its listed attachments and the `cid:`
/// images its rendition inlines from the stored blobs.
fn check_row(store_path: &str, blobs_dir: &str, row: i64) -> Result<()> {
    use mailypoppins::store::read;
    let store = mailypoppins::store::Store::open(store_path)?;
    let blobs = mailypoppins::store::BlobStore::new(blobs_dir);
    let listed: Vec<String> = read::attachments_for(&store, row)?
        .into_iter()
        .map(|a| a.name)
        .collect();
    println!("attachments: {listed:?}");
    let html = read::load_html(&store, &blobs, row).unwrap_or_default();
    match read::stored_inline_images(&store, &blobs, row)? {
        None => println!("inline images: not classified yet"),
        Some((images, _)) => {
            let page = mailypoppins::parse::embed_inline_images(&html, &images);
            println!(
                "inline images: {:?}; the rendition carries {} data: image(s) and {} cid: \
                 reference(s) left",
                images.iter().map(|i| &i.filename).collect::<Vec<_>>(),
                page.matches("data:image/").count(),
                page.to_ascii_lowercase().matches("cid:").count()
            );
        }
    }
    Ok(())
}

/// The entries of a metadata listing, as the sync pass reads them.
fn server_attachments(json: &serde_json::Value) -> Vec<mailypoppins::ingest::ServerAttachment> {
    json["value"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|a| mailypoppins::ingest::ServerAttachment {
            name: a["name"].as_str().unwrap_or("attachment").to_string(),
            content_id: a["contentId"].as_str().map(str::to_string),
            is_file: a["@odata.type"]
                .as_str()
                .is_none_or(|t| t.eq_ignore_ascii_case("#microsoft.graph.fileAttachment")),
        })
        .collect()
}

/// The file attachments of a full listing with their content, as ingest
/// keeps them (no invite lift: the inline-only rows carry none).
fn file_attachments(json: &serde_json::Value) -> Vec<mailypoppins::parse::AttachmentData> {
    use base64::Engine as _;
    json["value"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| {
            let content = base64::engine::general_purpose::STANDARD
                .decode(a["contentBytes"].as_str()?)
                .ok()
                .filter(|c| !c.is_empty())?;
            Some(mailypoppins::parse::AttachmentData {
                filename: mailypoppins::parse::sanitize_attachment_filename(
                    a["name"].as_str().unwrap_or("attachment"),
                ),
                content,
                content_id: a["contentId"].as_str().map(str::to_string),
            })
        })
        .collect()
}
