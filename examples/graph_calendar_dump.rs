//! Diagnostic probe: the raw JSON Graph returns for a calendar read.
//!
//! Lists `/me/calendars`, then for each calendar prints the first page of the
//! per-calendar `calendarView/delta` (the sync's first route) and of the plain
//! windowed `calendarView`, each truncated to its first three events, so the
//! two shapes can be compared. Requests carry the headers the calendar sync
//! sends: the bearer token and `Prefer: IdType="ImmutableId",
//! odata.maxpagesize=100`. The token comes from the cached OAuth2 token,
//! refreshed when expired, the path the daemon takes. It writes nothing and
//! opens no store, so it runs beside a live daemon.
//!
//! ```text
//! cargo run --example graph_calendar_dump -- [account]   # default: evoqs
//! ```

use anyhow::{anyhow, bail, Context, Result};
use mailypoppins::config::{init_secrets_backend, load_global_config, GraphConfig};

const GRAPH_BASE: &str = "https://graph.microsoft.com/v1.0";
const PREFER: &str = "IdType=\"ImmutableId\", odata.maxpagesize=100";
const RANGE: &str = "startDateTime=2026-10-25T23:00:00Z&endDateTime=2026-11-01T23:00:00Z";

/// Mirror of the crate-private `ID_ENCODE_SET` in `src/graph.rs`.
const ID_ENCODE_SET: &percent_encoding::AsciiSet = &percent_encoding::CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'&')
    .add(b'+')
    .add(b'/')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'`')
    .add(b'{')
    .add(b'}');

fn encode_id(id: &str) -> String {
    percent_encoding::utf8_percent_encode(id, ID_ENCODE_SET).to_string()
}

#[tokio::main]
async fn main() -> Result<()> {
    let account_name = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "evoqs".to_string());

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
    let token = mailypoppins::oauth2::load_or_refresh_token(
        &config.account_name,
        &config.client_id,
        &config.tenant_id,
        &config.scopes,
    )
    .await?;
    let http = reqwest::Client::new();

    let (status, body) = get(&http, &token, &format!("{GRAPH_BASE}/me/calendars")).await?;
    println!("GET /me/calendars -> HTTP {status}");
    let json: serde_json::Value = serde_json::from_str(&body)
        .with_context(|| format!("unreadable /me/calendars answer: {body}"))?;
    let calendars = json["value"]
        .as_array()
        .ok_or_else(|| anyhow!("no value array in /me/calendars: {body}"))?;
    for calendar in calendars {
        println!(
            "  {}  {}",
            calendar["id"].as_str().unwrap_or_default(),
            calendar["name"].as_str().unwrap_or_default()
        );
    }

    for calendar in calendars {
        let id = calendar["id"].as_str().unwrap_or_default();
        let name = calendar["name"].as_str().unwrap_or_default();
        let encoded = encode_id(id);
        for (label, url) in [
            (
                "calendarView/delta",
                format!("{GRAPH_BASE}/me/calendars/{encoded}/calendarView/delta?{RANGE}"),
            ),
            (
                "calendarView",
                format!("{GRAPH_BASE}/me/calendars/{encoded}/calendarView?{RANGE}&$top=3"),
            ),
        ] {
            println!("\n===== {name}: {label} =====");
            let (status, body) = get(&http, &token, &url).await?;
            println!("HTTP {status}");
            match serde_json::from_str::<serde_json::Value>(&body) {
                Ok(mut json) => {
                    if let Some(events) = json.get_mut("value").and_then(|v| v.as_array_mut()) {
                        let total = events.len();
                        events.truncate(3);
                        println!("(value: {total} events on this page, showing first 3)");
                    }
                    println!("{}", serde_json::to_string_pretty(&json)?);
                }
                Err(_) => println!("{body}"),
            }
        }
    }
    Ok(())
}

async fn get(http: &reqwest::Client, token: &str, url: &str) -> Result<(u16, String)> {
    let response = http
        .get(url)
        .bearer_auth(token)
        .header("Prefer", PREFER)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    let status = response.status().as_u16();
    let body = response.text().await?;
    Ok((status, body))
}
