//! The Graph threading probe of PERSO-147 (#0142, step 7).
//!
//! Answers, against a live Graph account, which Graph calls return
//! `internetMessageHeaders` and `conversationId`, and what the headers cost in
//! bytes: a folder listing page, a `/messages/delta` page and a single-message
//! GET, each with and without the headers in `$select`.
//!
//! It links the library, loads the account from `config.toml` and takes its
//! access token through the cached OAuth2 token, the path the daemon takes. It
//! only reads from the server, opens no store and takes no engine lock, so it
//! runs beside a live daemon.
//!
//! ```text
//! cargo run --example graph_thread_probe -- <account> [folder] [page-size]
//! cargo run --example graph_thread_probe -- <account> --rethread <store-copy> <mailbox>=<folder>...
//! ```
//!
//! `--rethread` runs the PERSO-147 re-thread against a *copy* of the
//! account's store (take one with `sqlite3 <store> ".backup <copy>"`): the
//! same candidates, the same folder listing and the same
//! [`mailypoppins::ingest::rethread_rows`] the sync pass runs, with the
//! listing's size measured, and the live store left alone.

use anyhow::{anyhow, bail, Context, Result};
use mailypoppins::config::{init_secrets_backend, load_global_config, GraphConfig};

const GRAPH_BASE: &str = "https://graph.microsoft.com/v1.0";
const MESSAGE_SELECT: &str = "id,internetMessageId,subject,from,toRecipients,ccRecipients,body,\
                              receivedDateTime,hasAttachments,isRead";

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let account_name = args
        .next()
        .ok_or_else(|| anyhow!("usage: graph_thread_probe <account> [folder] [page-size]"))?;
    let folder = args.next().unwrap_or_else(|| "inbox".to_string());
    let rethread = folder == "--rethread";
    let rest: Vec<String> = args.collect();

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
    if rethread {
        let (store, pairs) = rest
            .split_first()
            .ok_or_else(|| anyhow!("--rethread needs <store-copy> <mailbox>=<folder>..."))?;
        return runtime.block_on(rethread_copy(&config, store, pairs));
    }
    let page: usize = rest.first().map(|s| s.parse()).transpose()?.unwrap_or(50);
    runtime.block_on(probe(&config, &folder, page))
}

async fn token(config: &GraphConfig) -> Result<String> {
    mailypoppins::oauth2::load_or_refresh_token(
        &config.account_name,
        &config.client_id,
        &config.tenant_id,
        &config.scopes,
    )
    .await
}

/// The re-thread of the sync pass, against a store copy.
async fn rethread_copy(config: &GraphConfig, store_path: &str, pairs: &[String]) -> Result<()> {
    use mailypoppins::ingest::{rethread_candidates, rethread_rows, ThreadHints};
    let store = mailypoppins::store::Store::open(store_path)?;
    let token = token(config).await?;
    let http = reqwest::Client::new();
    let expand = "$expand=singleValueExtendedProperties($filter=id%20eq%20'String%200x1042'%20or%20id%20eq%20'String%200x1039')";
    for pair in pairs {
        let (mailbox, folder) = pair
            .split_once('=')
            .ok_or_else(|| anyhow!("expected <mailbox>=<folder>, got {pair}"))?;
        let candidates = rethread_candidates(&store, &config.account_name, mailbox)?;
        let started = std::time::Instant::now();
        let mut url = format!(
            "{GRAPH_BASE}/me/mailFolders/{folder}/messages?\
             $select=internetMessageId,conversationId&{expand}&$top=200"
        );
        let mut hints = std::collections::HashMap::new();
        let (mut pages, mut bytes) = (0usize, 0usize);
        loop {
            let response = http.get(&url).bearer_auth(&token).send().await?;
            let status = response.status();
            let body = response.text().await?;
            if !status.is_success() {
                bail!(
                    "HTTP {status}: {}",
                    body.chars().take(400).collect::<String>()
                );
            }
            pages += 1;
            bytes += body.len();
            let json: serde_json::Value = serde_json::from_str(&body)?;
            for m in json["value"].as_array().into_iter().flatten() {
                let Some(mid) = m["internetMessageId"].as_str().map(str::trim) else {
                    continue;
                };
                let property = |id: &str| {
                    m["singleValueExtendedProperties"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .find(|p| p["id"].as_str().is_some_and(|n| n.eq_ignore_ascii_case(id)))
                        .and_then(|p| p["value"].as_str())
                        .map(|v| v.trim().to_string())
                        .filter(|v| !v.is_empty())
                };
                hints.insert(
                    mid.to_string(),
                    ThreadHints {
                        in_reply_to: property("String 0x1042"),
                        references: property("String 0x1039"),
                        conversation_id: Some(
                            m["conversationId"].as_str().unwrap_or_default().to_string(),
                        ),
                    },
                );
            }
            match json["@odata.nextLink"].as_str() {
                Some(next) => url = next.to_string(),
                None => break,
            }
        }
        let listed = started.elapsed();
        let tally = rethread_rows(&store, &config.account_name, &candidates, &hints, true)?;
        println!(
            "{mailbox} ({folder}): {} candidates, listing {} messages in {pages} page(s), \
             {bytes} bytes ({} per message), {:.1}s; {tally:?}",
            candidates.len(),
            hints.len(),
            bytes / hints.len().max(1),
            listed.as_secs_f64(),
        );
    }
    Ok(())
}

async fn probe(config: &GraphConfig, folder: &str, page: usize) -> Result<()> {
    let token = token(config).await?;
    let http = reqwest::Client::new();
    let get = |url: String, prefer: Option<String>| {
        let mut request = http.get(url).bearer_auth(&token);
        if let Some(prefer) = prefer {
            request = request.header("Prefer", prefer);
        }
        async move {
            let response = request.send().await?;
            let status = response.status();
            let body = response.text().await?;
            if !status.is_success() {
                bail!(
                    "HTTP {status}: {}",
                    body.chars().take(400).collect::<String>()
                );
            }
            let json: serde_json::Value = serde_json::from_str(&body)?;
            Ok::<_, anyhow::Error>((body.len(), json))
        }
    };

    let ids_only = "id,internetMessageId,conversationId";
    let with_headers = "id,internetMessageId,conversationId,subject,internetMessageHeaders";

    // 1. A folder listing page, with and without the headers.
    let list = |select: &str| {
        format!("{GRAPH_BASE}/me/mailFolders/{folder}/messages?$select={select}&$top={page}")
    };
    let (bare_bytes, bare) = get(list(ids_only), None).await?;
    let (full_bytes, full) = get(list(with_headers), None).await?;
    let listed = full["value"].as_array().cloned().unwrap_or_default();
    report("listing", page, bare_bytes, full_bytes, &bare, &listed);

    // 2. A delta page, with the headers.
    let delta =
        format!("{GRAPH_BASE}/me/mailFolders/{folder}/messages/delta?$select={with_headers}");
    match get(delta, Some(format!("odata.maxpagesize={page}"))).await {
        Ok((bytes, json)) => {
            let rows = json["value"].as_array().cloned().unwrap_or_default();
            let carrying = rows
                .iter()
                .filter(|m| m.get("internetMessageHeaders").is_some())
                .count();
            let conversation = rows
                .iter()
                .filter(|m| m.get("conversationId").is_some())
                .count();
            println!(
                "delta page: {} messages, {bytes} bytes, {carrying} carry internetMessageHeaders, \
                 {conversation} carry conversationId",
                rows.len()
            );
        }
        Err(e) => println!("delta page with internetMessageHeaders refused: {e:#}"),
    }

    // 3. The two MAPI properties behind the headers, PidTagInReplyToId and
    //    PidTagInternetReferences, as expanded extended properties: what the
    //    threading needs without the rest of the header block.
    let expand = "$expand=singleValueExtendedProperties($filter=id%20eq%20'String%200x1042'%20or%20id%20eq%20'String%200x1039')";
    let mapi = format!("{GRAPH_BASE}/me/mailFolders/{folder}/messages?$select={ids_only},subject&$top={page}&{expand}");
    match get(mapi, None).await {
        Ok((bytes, json)) => {
            let rows = json["value"].as_array().cloned().unwrap_or_default();
            let carrying = rows
                .iter()
                .filter(|m| m.get("singleValueExtendedProperties").is_some())
                .count();
            println!(
                "listing with the two MAPI properties: {} messages, {bytes} bytes ({} per message), \
                 {carrying} carry singleValueExtendedProperties",
                rows.len(),
                bytes / rows.len().max(1)
            );
            for m in &rows {
                if m["subject"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("PERSO-144")
                {
                    println!("  {}", m["singleValueExtendedProperties"]);
                }
            }
        }
        Err(e) => println!("listing with the MAPI properties refused: {e:#}"),
    }
    let delta_mapi =
        format!("{GRAPH_BASE}/me/mailFolders/{folder}/messages/delta?$select={ids_only}&{expand}");
    match get(delta_mapi, Some(format!("odata.maxpagesize={page}"))).await {
        Ok((bytes, json)) => {
            let rows = json["value"].as_array().cloned().unwrap_or_default();
            let carrying = rows
                .iter()
                .filter(|m| m.get("singleValueExtendedProperties").is_some())
                .count();
            println!(
                "delta with the MAPI properties: {} messages, {bytes} bytes, {carrying} carry them",
                rows.len()
            );
        }
        Err(e) => println!("delta with the MAPI properties refused: {e:#}"),
    }

    // 4. A single GET of the first listed message, the sync download's select
    //    with and without the two new fields.
    if let Some(first) = listed.first().and_then(|m| m["id"].as_str()) {
        let one = |select: &str| format!("{GRAPH_BASE}/me/messages/{first}?$select={select}");
        let (plain, _) = get(one(MESSAGE_SELECT), None).await?;
        let (threaded, _) = get(
            one(&format!(
                "{MESSAGE_SELECT},conversationId,internetMessageHeaders"
            )),
            None,
        )
        .await?;
        println!(
            "single GET: {plain} bytes without, {threaded} bytes with the two fields (+{})",
            threaded.saturating_sub(plain)
        );
        let (mapi, _) = get(
            format!(
                "{}&{expand}",
                one(&format!("{MESSAGE_SELECT},conversationId"))
            ),
            None,
        )
        .await?;
        println!(
            "single GET with conversationId and the MAPI properties: {mapi} bytes (+{})",
            mapi.saturating_sub(plain)
        );
    }
    Ok(())
}

fn report(
    label: &str,
    page: usize,
    bare_bytes: usize,
    full_bytes: usize,
    bare: &serde_json::Value,
    listed: &[serde_json::Value],
) {
    let count = listed.len().max(1);
    let carrying = listed
        .iter()
        .filter(|m| m.get("internetMessageHeaders").is_some())
        .count();
    let header = |m: &serde_json::Value, name: &str| {
        m["internetMessageHeaders"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|h| {
                h["name"]
                    .as_str()
                    .is_some_and(|n| n.eq_ignore_ascii_case(name))
            })
    };
    let replies = listed.iter().filter(|m| header(m, "In-Reply-To")).count();
    let referencing = listed.iter().filter(|m| header(m, "References")).count();
    println!(
        "{label} page of {page}: {} messages; {bare_bytes} bytes without headers ({} per message), \
         {full_bytes} with ({} per message); {carrying} carry internetMessageHeaders, \
         {replies} In-Reply-To, {referencing} References",
        bare["value"].as_array().map_or(0, Vec::len),
        bare_bytes / count,
        full_bytes / count,
    );
    for m in listed {
        let subject = m["subject"].as_str().unwrap_or_default();
        if subject.contains("PERSO-144") {
            let pick = |name: &str| {
                m["internetMessageHeaders"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|h| {
                        h["name"]
                            .as_str()
                            .is_some_and(|n| n.eq_ignore_ascii_case(name))
                    })
                    .and_then(|h| h["value"].as_str())
                    .unwrap_or("-")
                    .to_string()
            };
            println!(
                "  {subject}\n    Message-ID {}\n    In-Reply-To {}\n    References {}\n    conversationId {}",
                m["internetMessageId"].as_str().unwrap_or("-"),
                pick("In-Reply-To"),
                pick("References"),
                m["conversationId"].as_str().unwrap_or("-"),
            );
        }
    }
}
