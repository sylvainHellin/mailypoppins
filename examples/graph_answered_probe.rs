//! The Graph answered-state probe of PERSO-171.
//!
//! Answers, against a live Graph account, whether Exchange holds and accepts
//! the MAPI properties Outlook keeps the replied/forwarded state in:
//! `PidTagLastVerbExecuted` (Integer 0x1081), `PidTagLastVerbExecutionTime`
//! (SystemTime 0x1082) and `PidTagIconIndex` (Integer 0x1080).
//!
//! It links the library, loads the account from `config.toml` and takes its
//! access token through the cached OAuth2 token, the path the daemon takes.
//! Every message carrying the given Message-ID is read (all folders); with
//! `--patch <folder-name>` the copy in that folder is written to "replied"
//! (102, now, 261) by hand and read back, and with `--library-reply` every
//! copy is written through [`mailypoppins::graph::mark_last_verb_graph`], the
//! daemon's own path. `--listing <folder>` writes nothing: it measures what
//! the folder enumeration and the delta answer when asked for 0x1081. It
//! opens no store and takes no engine lock, so it runs beside a live daemon.
//!
//! ```text
//! cargo run --example graph_answered_probe -- <account> <message-id> [--patch <folder-name> | --library-reply]
//! cargo run --example graph_answered_probe -- <account> --listing [folder]
//! ```

use anyhow::{anyhow, bail, Context, Result};
use mailypoppins::config::{init_secrets_backend, load_global_config, GraphConfig};

const GRAPH_BASE: &str = "https://graph.microsoft.com/v1.0";
const EXPAND: &str = "$expand=singleValueExtendedProperties($filter=id%20eq%20'Integer%200x1081'%20or%20id%20eq%20'SystemTime%200x1082'%20or%20id%20eq%20'Integer%200x1080')";

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let usage = "usage: graph_answered_probe <account> <message-id> [--patch <folder-name>]";
    let account_name = args.next().ok_or_else(|| anyhow!(usage))?;
    let message_id = args.next().ok_or_else(|| anyhow!(usage))?;
    let listing = message_id == "--listing";
    let mut library = false;
    let patch_folder = match args.next().as_deref() {
        Some(folder) if listing => Some(folder.to_string()),
        Some("--patch") => Some(args.next().ok_or_else(|| anyhow!(usage))?),
        Some("--library-reply") => {
            library = true;
            None
        }
        Some(other) => bail!("unexpected argument {other}; {usage}"),
        None => None,
    };

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
    if listing {
        let folder = patch_folder.unwrap_or_else(|| "inbox".to_string());
        return runtime.block_on(listing_cost(&config, &folder));
    }
    if library {
        return runtime.block_on(async {
            mailypoppins::graph::mark_last_verb_graph(
                &config,
                &message_id,
                mailypoppins::graph::LastVerb::Reply,
            )
            .await?;
            println!("mark_last_verb_graph(Reply) -> Ok");
            probe(&config, &message_id, None).await
        });
    }
    runtime.block_on(probe(&config, &message_id, patch_folder.as_deref()))
}

/// Whether the sync's folder enumeration and its delta accept the 0x1081
/// expand, and what it costs on a 200-message page.
async fn listing_cost(config: &GraphConfig, folder: &str) -> Result<()> {
    let token = mailypoppins::oauth2::load_or_refresh_token(
        &config.account_name,
        &config.client_id,
        &config.tenant_id,
        &config.scopes,
    )
    .await?;
    let http = reqwest::Client::new();
    let verb = "$expand=singleValueExtendedProperties($filter=id%20eq%20'Integer%200x1081')";
    let base = format!(
        "{GRAPH_BASE}/me/mailFolders/{folder}/messages?\
         $select=id,internetMessageId,isRead,receivedDateTime&$orderby=receivedDateTime%20desc&$top=200"
    );
    let delta = format!(
        "{GRAPH_BASE}/me/mailFolders/{folder}/messages/delta?\
         $select=id,internetMessageId,isRead,receivedDateTime"
    );
    for (label, url) in [
        ("enumeration", base.clone()),
        ("enumeration + 0x1081", format!("{base}&{verb}")),
        (
            "enumeration + threading and 0x1081",
            format!(
                "{base}&$expand=singleValueExtendedProperties($filter=id%20eq%20'String%200x1042'%20or%20id%20eq%20'String%200x1039'%20or%20id%20eq%20'Integer%200x1081')"
            ),
        ),
        ("delta + 0x1081", format!("{delta}&{verb}")),
    ] {
        let response = http
            .get(url)
            .bearer_auth(&token)
            .header("Prefer", "odata.maxpagesize=200")
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;
        let json: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
        let rows = json["value"].as_array().cloned().unwrap_or_default();
        let mut verbs = std::collections::BTreeMap::<String, usize>::new();
        for m in &rows {
            for p in m["singleValueExtendedProperties"].as_array().into_iter().flatten() {
                *verbs
                    .entry(p["value"].as_str().unwrap_or("?").to_string())
                    .or_default() += 1;
            }
        }
        println!(
            "{label}: HTTP {status}, {} messages, {} bytes, verbs {verbs:?}{}",
            rows.len(),
            body.len(),
            if status.is_success() {
                String::new()
            } else {
                format!(": {}", body.chars().take(300).collect::<String>())
            }
        );
    }
    Ok(())
}

async fn probe(config: &GraphConfig, message_id: &str, patch_folder: Option<&str>) -> Result<()> {
    let token = mailypoppins::oauth2::load_or_refresh_token(
        &config.account_name,
        &config.client_id,
        &config.tenant_id,
        &config.scopes,
    )
    .await?;
    let http = reqwest::Client::new();

    let copies = read(&http, &token, message_id).await?;
    let Some(folder_name) = patch_folder else {
        return Ok(());
    };
    let mut target = None;
    for (id, folder) in &copies {
        let url = format!("{GRAPH_BASE}/me/mailFolders/{folder}?$select=displayName");
        let response = http.get(url).bearer_auth(&token).send().await?;
        let status = response.status();
        let json: serde_json::Value = response.json().await?;
        let name = json["displayName"].as_str().unwrap_or_default().to_string();
        println!("folder of {}: {name} (HTTP {status})", short(id));
        if name.eq_ignore_ascii_case(folder_name) {
            target = Some(id.clone());
        }
    }
    let target = target.ok_or_else(|| anyhow!("no copy in a folder named '{folder_name}'"))?;

    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let body = serde_json::json!({
        "singleValueExtendedProperties": [
            {"id": "Integer 0x1081", "value": "102"},
            {"id": "SystemTime 0x1082", "value": now},
            {"id": "Integer 0x1080", "value": "261"},
        ]
    });
    let response = http
        .patch(format!("{GRAPH_BASE}/me/messages/{target}"))
        .bearer_auth(&token)
        .json(&body)
        .send()
        .await?;
    let status = response.status();
    let text = response.text().await?;
    println!(
        "PATCH {} -> HTTP {status}: {}",
        short(&target),
        text.chars().take(600).collect::<String>()
    );
    println!("read back:");
    read(&http, &token, message_id).await?;
    Ok(())
}

/// Every copy of `message_id`, with its folder id, printing the three properties.
async fn read(
    http: &reqwest::Client,
    token: &str,
    message_id: &str,
) -> Result<Vec<(String, String)>> {
    let filter = format!("internetMessageId%20eq%20'{}'", urlencode(message_id));
    let url = format!(
        "{GRAPH_BASE}/me/messages?$filter={filter}&\
         $select=id,subject,parentFolderId,internetMessageId&{EXPAND}"
    );
    let response = http.get(url).bearer_auth(token).send().await?;
    let status = response.status();
    let body = response.text().await?;
    println!("GET copies of {message_id} -> HTTP {status}");
    if !status.is_success() {
        bail!("{}", body.chars().take(600).collect::<String>());
    }
    let json: serde_json::Value = serde_json::from_str(&body)?;
    let mut copies = Vec::new();
    for m in json["value"].as_array().into_iter().flatten() {
        let id = m["id"].as_str().unwrap_or_default().to_string();
        let folder = m["parentFolderId"].as_str().unwrap_or_default().to_string();
        println!(
            "  {} [{}] {}\n    properties: {}",
            short(&id),
            short(&folder),
            m["subject"].as_str().unwrap_or_default(),
            m.get("singleValueExtendedProperties")
                .map_or_else(|| "(none)".to_string(), ToString::to_string)
        );
        copies.push((id, folder));
    }
    Ok(copies)
}

fn short(id: &str) -> String {
    let n = id.len();
    format!("...{}", &id[n.saturating_sub(16)..])
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}
