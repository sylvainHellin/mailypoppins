// OAuth2 Device Code Flow for Microsoft Entra ID (Microsoft 365)
//
// Implements the device authorization grant for acquiring the access tokens
// a Graph account (`auth_method = "graph"`) calls Microsoft Graph with.
//
// Token cache is stored encrypted at `<mailypoppins_data_dir>/tokens/<account>.enc`
// using the same machine-bound key as the secrets store (see `secrets.rs`).

use anyhow::{anyhow, Context, Result};
use log::{debug, info, warn};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::config::{AccountConfig, CalendarBackendKind};

// ---------------------------------------------------------------------------
// Microsoft Entra ID endpoints
// ---------------------------------------------------------------------------

fn device_code_url(tenant_id: &str) -> String {
    format!(
        "https://login.microsoftonline.com/{}/oauth2/v2.0/devicecode",
        tenant_id
    )
}

fn token_url(tenant_id: &str) -> String {
    format!(
        "https://login.microsoftonline.com/{}/oauth2/v2.0/token",
        tenant_id
    )
}

/// Application (client) ID of the shared mailypoppins app registration: a
/// multi-tenant public client in the `mailypoppins.onmicrosoft.com` tenant that
/// accepts any Entra tenant plus personal Microsoft accounts. The Graph setup
/// wizards (CLI and desktop) use it unless the user brings their own app; the
/// desktop wizard mirrors it as `MAILYPOPPINS_CLIENT_ID` in
/// `clients/desktop/src/app/wizard.ts`.
pub const MAILYPOPPINS_CLIENT_ID: &str = "439d7ebd-7898-4df2-b2d6-88bb61e2597b";

/// The tenant a Graph account signs in through when the user names none:
/// `common` lets a multi-tenant app take both work or school and personal
/// Microsoft accounts, at the authorize, device-code and token endpoints alike.
pub const DEFAULT_TENANT: &str = "common";

/// The client ID a setup wizard writes for `input`: the user's own app
/// registration when they typed one, else the shared mailypoppins app.
pub fn client_id_or_default(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        MAILYPOPPINS_CLIENT_ID.to_string()
    } else {
        trimmed.to_string()
    }
}

/// The tenant a setup wizard writes for `input`: the tenant ID or domain the
/// user typed, else [`DEFAULT_TENANT`].
pub fn tenant_or_default(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        DEFAULT_TENANT.to_string()
    } else {
        trimmed.to_string()
    }
}

/// The scope a Graph account's calendar needs (PERSO-200).
pub const CALENDAR_SCOPE: &str = "https://graph.microsoft.com/Calendars.ReadWrite";

/// Every scope a Graph account can sign in with: mail, and the calendar since
/// PERSO-200. What one account asks for is [`scopes_for`].
pub const GRAPH_SCOPES: &str = "https://graph.microsoft.com/Mail.Read \
                                https://graph.microsoft.com/Mail.ReadWrite \
                                https://graph.microsoft.com/Mail.Send \
                                https://graph.microsoft.com/Calendars.ReadWrite \
                                offline_access";

/// A scope without its resource prefix: `Calendars.ReadWrite` for
/// `https://graph.microsoft.com/Calendars.ReadWrite`, `offline_access` for
/// itself. Entra answers some scopes with the prefix and some without, so two
/// scopes are compared by this name.
pub fn scope_name(scope: &str) -> &str {
    scope.rsplit('/').next().unwrap_or(scope)
}

/// The one line every client shows for a Graph account whose sign-in lacks
/// the calendar scope: the `calendar:<account>` health check's detail, and
/// the line `mp config show` prints.
pub fn calendar_relogin_prompt(account: &str) -> String {
    format!("calendar needs a re-login (mp config oauth2-login -A {account})")
}

/// The scopes `account` signs in and refreshes with: [`GRAPH_SCOPES`] when its
/// calendar is Graph's, and without [`CALENDAR_SCOPE`] when it opted out
/// (`calendar_backend = "none"`, or another backend), so a sign-in never asks
/// for a permission the account does not use.
pub fn scopes_for(account: &AccountConfig) -> String {
    if account.calendar_backend() == CalendarBackendKind::Graph {
        GRAPH_SCOPES.to_string()
    } else {
        without_scope(GRAPH_SCOPES, CALENDAR_SCOPE)
    }
}

/// `scopes` (space separated) without `scope`, compared by [`scope_name`].
fn without_scope(scopes: &str, scope: &str) -> String {
    scopes
        .split_whitespace()
        .filter(|s| !scope_name(s).eq_ignore_ascii_case(scope_name(scope)))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The `scope` field of a token response, one entry per granted scope.
fn granted_scopes(scope: &str) -> Vec<String> {
    scope.split_whitespace().map(str::to_string).collect()
}

// ---------------------------------------------------------------------------
// Token cache
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TokenCache {
    pub access_token: String,
    pub refresh_token: String,
    /// Unix timestamp when the access token expires.
    pub expires_at: u64,
    /// The scopes the last token response granted, as Entra spelled them.
    /// Empty for a cache written before PERSO-200, which recorded none and so
    /// counts as lacking every scope [`TokenCache::has_scope`] is asked about.
    #[serde(default)]
    pub scopes: Vec<String>,
}

impl TokenCache {
    /// Whether the token was granted `scope`, with or without its resource
    /// prefix on either side.
    pub fn has_scope(&self, scope: &str) -> bool {
        let wanted = scope_name(scope);
        self.scopes
            .iter()
            .any(|granted| scope_name(granted).eq_ignore_ascii_case(wanted))
    }

    pub fn is_expired(&self) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        // Consider expired 60 seconds early to avoid race conditions
        now >= self.expires_at.saturating_sub(60)
    }
}

/// Path to the encrypted token cache for `account_name`.
pub fn token_cache_path(account_name: &str) -> PathBuf {
    crate::config::tokens_dir().join(format!("{}.enc", account_name))
}

/// The subdirectory of the token directory a setup wizard's sign-in is staged
/// in. An account name never contains a path separator
/// (`validate_account_names`), so no configured account's cache can be a
/// staged one and no staged sign-in can overwrite a configured account's.
pub const STAGED_TOKEN_DIR: &str = "candidates";

/// The token-cache name a candidate account's sign-in is written under until
/// the configuration that names the account is committed.
///
/// A wizard signs a candidate in before `config.init` or `config.add_account`
/// writes it, and may be abandoned after the sign-in: a token cached under the
/// account's own name would by then have replaced the one of a configured
/// account of that name. Staged here, it replaces nothing until
/// [`promote_staged_token`] moves it, and a probe of the candidate reads it
/// rather than whatever a configured account of that name has cached.
pub fn staged_token_name(account_name: &str) -> String {
    format!("{STAGED_TOKEN_DIR}/{account_name}")
}

/// Whether a sign-in is staged for `account_name`.
pub fn staged_token_exists(account_name: &str) -> bool {
    token_cache_path(&staged_token_name(account_name)).is_file()
}

/// Move the staged sign-in of `account_name` over its token cache, once the
/// configuration that names the account is written. `Ok(false)` when nothing
/// is staged; a rename, so the cache is never half written.
pub fn promote_staged_token(account_name: &str) -> Result<bool> {
    let staged = token_cache_path(&staged_token_name(account_name));
    if !staged.is_file() {
        return Ok(false);
    }
    let path = token_cache_path(account_name);
    fs::rename(&staged, &path).with_context(|| {
        format!(
            "Failed to promote the staged token {} to {}",
            staged.display(),
            path.display()
        )
    })?;
    info!("OAuth2 token of the candidate '{account_name}' promoted");
    Ok(true)
}

/// How old a staged sign-in may grow before the next candidate sign-in or
/// daemon start drops it. A wizard that signed in a day ago and still has not written its
/// account was abandoned; one that comes back after that signs in again.
pub const STAGED_TOKEN_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// Drop the staged sign-ins older than [`STAGED_TOKEN_MAX_AGE`], which an
/// abandoned wizard left behind. The daemon runs it as it starts and as each
/// candidate sign-in starts, the first token-cache write of every OAuth2 or
/// Graph wizard in either client; a younger sign-in may belong to a wizard
/// still running and is kept. Answers how many it dropped.
pub fn sweep_staged_tokens() -> Result<usize> {
    sweep_staged_tokens_in(
        &crate::config::tokens_dir().join(STAGED_TOKEN_DIR),
        SystemTime::now(),
        STAGED_TOKEN_MAX_AGE,
    )
}

/// [`sweep_staged_tokens`] over `dir` at `now`: every `.enc` file last written
/// more than `max_age` before `now`. A missing directory holds nothing.
fn sweep_staged_tokens_in(
    dir: &std::path::Path,
    now: SystemTime,
    max_age: Duration,
) -> Result<usize> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e).with_context(|| format!("Failed to read {}", dir.display())),
    };
    let mut dropped = 0;
    for entry in entries {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("enc") || !path.is_file() {
            continue;
        }
        let written = fs::metadata(&path)?.modified()?;
        if now.duration_since(written).unwrap_or_default() <= max_age {
            continue;
        }
        fs::remove_file(&path)
            .with_context(|| format!("Failed to drop the staged token {}", path.display()))?;
        info!("Abandoned staged token {} dropped", path.display());
        dropped += 1;
    }
    Ok(dropped)
}

fn save_token_cache(account_name: &str, cache: &TokenCache) -> Result<()> {
    let path = token_cache_path(account_name);
    // The parent rather than the token directory: a staged sign-in lives one
    // level below it.
    let dir = path
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(crate::config::tokens_dir);
    crate::config::create_private_dir_all(&dir)
        .with_context(|| format!("Failed to create token cache dir: {}", dir.display()))?;
    let json = serde_json::to_vec(cache).context("Failed to serialize token cache")?;
    let blob = crate::secrets::encrypt_blob(&json).context("Failed to encrypt token cache")?;
    // Atomic write, created with 0600 from the start (no umask window).
    crate::secrets::write_secret_file_atomic(&path, &blob)
        .with_context(|| format!("Failed to write token cache: {}", path.display()))?;
    debug!("Token cache saved: {}", path.display());
    Ok(())
}

/// Load the cached token for `account_name`, or `None` if missing /
/// undecryptable / corrupt.
pub fn load_token_cache(account_name: &str) -> Option<TokenCache> {
    let path = token_cache_path(account_name);
    let bytes = fs::read(&path).ok()?;
    let plaintext = crate::secrets::decrypt_blob(&bytes).ok()?;
    serde_json::from_slice(&plaintext).ok()
}

// ---------------------------------------------------------------------------
// Device Code Flow response types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    #[serde(default = "default_interval")]
    interval: u64,
    #[serde(default = "default_expires_in")]
    expires_in: u64,
}

fn default_interval() -> u64 {
    5
}
fn default_expires_in() -> u64 {
    900
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: u64,
    /// The granted scopes, space separated.
    #[serde(default)]
    scope: Option<String>,
}

#[derive(Deserialize)]
struct TokenErrorResponse {
    error: String,
    error_description: Option<String>,
    /// Entra's numeric `AADSTS` codes (`65001` for missing consent).
    #[serde(default)]
    error_codes: Vec<u64>,
}

impl TokenErrorResponse {
    /// Whether this refusal is about the scopes asked for rather than the
    /// grant as a whole: `invalid_scope`, or an `invalid_grant` carrying
    /// AADSTS65001 (no consent) or a code of the AADSTS70000 family (a scope
    /// unauthorized or expired), read from `error_codes` or, failing that,
    /// from the `AADSTS<code>` the description opens with.
    fn is_scope_refusal(&self) -> bool {
        match self.error.as_str() {
            "invalid_scope" => true,
            "invalid_grant" => self
                .aadsts_codes()
                .any(|code| code == 65001 || (70000..70100).contains(&code)),
            _ => false,
        }
    }

    fn aadsts_codes(&self) -> impl Iterator<Item = u64> + '_ {
        let described = self
            .error_description
            .as_deref()
            .and_then(|text| text.trim_start().strip_prefix("AADSTS"))
            .and_then(|rest| {
                let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
                digits.parse().ok()
            });
        self.error_codes.iter().copied().chain(described)
    }
}

// ---------------------------------------------------------------------------
// Device Code Flow
// ---------------------------------------------------------------------------

/// Run the interactive device code flow. Prints the user code and verification URL,
/// then polls until the user completes sign-in or the flow expires.
pub async fn device_code_flow(
    client_id: &str,
    tenant_id: &str,
    account_name: &str,
    scopes: &str,
) -> Result<TokenCache> {
    device_code_flow_reporting(client_id, tenant_id, account_name, scopes, &|uri, code| {
        print!("{}", crate::oauth2::device_code_block(uri, code));
    })
    .await
}

/// The block the device-code instructions are printed as, kept here so the
/// in-process flow and `mp_client::format::oauth2_device_code_lines` cannot
/// drift apart.
pub fn device_code_block(verification_uri: &str, user_code: &str) -> String {
    format!(
        "\n  To sign in, open a browser and go to:\n\n    {verification_uri}\n\n  \
         Enter the code: {user_code}\n\n"
    )
}

/// The device-code flow with the user code and the verification URL handed to
/// `on_device_code` instead of printed (P4-U14).
///
/// A daemon has no terminal, so the two values a human has to read are reported
/// rather than rendered: `config.oauth2_login` turns them into one
/// `operation.progress`, and the client that asked prints the block and opens
/// the browser (`INT-04`).
pub async fn device_code_flow_reporting(
    client_id: &str,
    tenant_id: &str,
    account_name: &str,
    scopes: &str,
    on_device_code: &(dyn Fn(&str, &str) + Send + Sync),
) -> Result<TokenCache> {
    let client = reqwest::Client::new();

    // Step 1: Request device code
    let resp = client
        .post(device_code_url(tenant_id))
        .form(&[("client_id", client_id), ("scope", scopes)])
        .send()
        .await
        .context("Failed to request device code")?;

    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!("Device code request failed: {}", body));
    }

    let dc: DeviceCodeResponse = resp
        .json()
        .await
        .context("Failed to parse device code response")?;

    // Step 2: hand the instructions to whoever can show them.
    on_device_code(&dc.verification_uri, &dc.user_code);

    // Step 3: Poll for token
    let poll_interval = Duration::from_secs(dc.interval.max(5));
    let deadline = SystemTime::now() + Duration::from_secs(dc.expires_in);

    loop {
        tokio::time::sleep(poll_interval).await;

        if SystemTime::now() > deadline {
            return Err(anyhow!("Device code flow expired. Please try again."));
        }

        let resp = client
            .post(token_url(tenant_id))
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("client_id", client_id),
                ("device_code", &dc.device_code),
            ])
            .send()
            .await
            .context("Failed to poll for token")?;

        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();

        if status.is_success() {
            let token: TokenResponse =
                serde_json::from_str(&body).context("Failed to parse token response")?;

            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();

            let cache = TokenCache {
                access_token: token.access_token,
                refresh_token: token.refresh_token.unwrap_or_default(),
                expires_at: now + token.expires_in,
                scopes: token
                    .scope
                    .as_deref()
                    .map(granted_scopes)
                    .unwrap_or_default(),
            };

            save_token_cache(account_name, &cache)?;
            info!("OAuth2 token acquired for account '{}'", account_name);
            return Ok(cache);
        }

        // Check error type
        if let Ok(err) = serde_json::from_str::<TokenErrorResponse>(&body) {
            match err.error.as_str() {
                "authorization_pending" => {
                    // User hasn't completed sign-in yet, keep polling
                    continue;
                }
                "slow_down" => {
                    // Back off by adding 5 seconds
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                }
                "expired_token" => {
                    return Err(anyhow!("Device code expired. Please try again."));
                }
                "authorization_declined" => {
                    return Err(anyhow!("Authorization was declined by the user."));
                }
                other => {
                    let desc = err.error_description.unwrap_or_default();
                    return Err(anyhow!("OAuth2 error ({}): {}", other, desc));
                }
            }
        } else {
            return Err(anyhow!(
                "Unexpected token response (HTTP {}): {}",
                status,
                body
            ));
        }
    }
}

// ---------------------------------------------------------------------------
// Token refresh
// ---------------------------------------------------------------------------

/// A refresh the token endpoint answered with a non-2xx status.
#[derive(Debug, Clone)]
struct Refusal {
    status: u16,
    body: String,
}

impl Refusal {
    /// Whether Entra refused the scopes asked for (see
    /// [`TokenErrorResponse::is_scope_refusal`]), as opposed to a throttle, an
    /// outage or a dead refresh token, none of which a narrower request fixes.
    fn is_scope_refusal(&self) -> bool {
        serde_json::from_str::<TokenErrorResponse>(&self.body)
            .is_ok_and(|err| err.is_scope_refusal())
    }
}

/// One refresh request: the token response, or the refusal.
async fn request_refresh(
    client: &reqwest::Client,
    client_id: &str,
    tenant_id: &str,
    refresh_token_value: &str,
    scopes: String,
) -> Result<std::result::Result<TokenResponse, Refusal>> {
    let resp = client
        .post(token_url(tenant_id))
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", client_id),
            ("refresh_token", refresh_token_value),
            ("scope", scopes.as_str()),
        ])
        .send()
        .await
        .context("Failed to refresh OAuth2 token")?;

    let status = resp.status();
    if !status.is_success() {
        return Ok(Err(Refusal {
            status: status.as_u16(),
            body: resp.text().await.unwrap_or_default(),
        }));
    }
    let token: TokenResponse = resp
        .json()
        .await
        .context("Failed to parse refresh response")?;
    Ok(Ok(token))
}

/// Refresh `previous` with `scopes`, recording the scopes the answer grants.
///
/// A refresh token covers every permission the user already consented to, so
/// asking for the calendar scope picks it up where consent exists and is
/// refused where it does not (a token from before PERSO-200). That refusal
/// may not cost the mail: the request is repeated without the calendar scope,
/// and the cache then records a grant without it, which the daemon reports as
/// a re-login prompt.
async fn refresh_token(
    client_id: &str,
    tenant_id: &str,
    previous: &TokenCache,
    account_name: &str,
    scopes: &str,
) -> Result<TokenCache> {
    let client = reqwest::Client::new();
    let refresh_token_value = previous.refresh_token.as_str();

    let token = refresh_answer(account_name, scopes, |scopes| {
        request_refresh(&client, client_id, tenant_id, refresh_token_value, scopes)
    })
    .await?;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let cache = TokenCache {
        access_token: token.access_token,
        refresh_token: token
            .refresh_token
            .unwrap_or_else(|| refresh_token_value.to_string()),
        expires_at: now + token.expires_in,
        // An answer that names no scope changed none.
        scopes: token
            .scope
            .as_deref()
            .map(granted_scopes)
            .unwrap_or_else(|| previous.scopes.clone()),
    };

    save_token_cache(account_name, &cache)?;
    info!("OAuth2 token refreshed for account '{}'", account_name);
    Ok(cache)
}

/// The token a refresh with `scopes` ends in, `request` making each attempt.
///
/// Only a refusal of the scopes themselves ([`Refusal::is_scope_refusal`]) is
/// retried without the calendar scope. Anything else (a 429, a 5xx, a dead
/// refresh token) is the ordinary refresh error: retrying it mail-only would
/// record a grant without the calendar for a token that has it, and prompt a
/// re-login nothing needs.
async fn refresh_answer<F, Fut>(
    account_name: &str,
    scopes: &str,
    mut request: F,
) -> Result<TokenResponse>
where
    F: FnMut(String) -> Fut,
    Fut: std::future::Future<Output = Result<std::result::Result<TokenResponse, Refusal>>>,
{
    let mut answer = request(scopes.to_string()).await?;
    if let Err(refusal) = &answer {
        warn!(
            "OAuth2 refresh refused for '{}' (HTTP {}): {}",
            account_name, refusal.status, refusal.body
        );
        let mail_only = without_scope(scopes, CALENDAR_SCOPE);
        if refusal.is_scope_refusal() && mail_only != scopes {
            warn!(
                "OAuth2 refresh with the calendar scope refused for '{}', retrying without it",
                account_name
            );
            answer = request(mail_only).await?;
        }
    }
    answer.map_err(|refusal| {
        anyhow!(
            "Token refresh failed with HTTP {} (the refresh token may have expired -- run `mp config oauth2-login`): {}",
            refusal.status,
            refusal.body
        )
    })
}

// ---------------------------------------------------------------------------
// Public API: load or refresh
// ---------------------------------------------------------------------------

/// Load a cached token, refresh if expired, or return an error if no cache exists.
/// For interactive token acquisition, use `device_code_flow()` instead.
pub async fn load_or_refresh_token(
    account_name: &str,
    client_id: &str,
    tenant_id: &str,
    scopes: &str,
) -> Result<String> {
    let cache = load_token_cache(account_name).ok_or_else(|| {
        anyhow!(
            "No OAuth2 token cached for account '{}'. Run `mp config oauth2-login -A {}` first.",
            account_name,
            account_name
        )
    })?;

    if !cache.is_expired() {
        debug!("Using cached OAuth2 token for '{}'", account_name);
        return Ok(cache.access_token);
    }

    if cache.refresh_token.is_empty() {
        return Err(anyhow!(
            "OAuth2 token expired and no refresh token available. Run `mp config oauth2-login -A {}`.",
            account_name
        ));
    }

    warn!("OAuth2 token expired for '{}', refreshing...", account_name);
    let new_cache = refresh_token(client_id, tenant_id, &cache, account_name, scopes).await?;
    Ok(new_cache.access_token)
}

/// Blocking wrapper around `load_or_refresh_token` for use in config loading.
pub fn load_or_refresh_token_blocking(
    account_name: &str,
    client_id: &str,
    tenant_id: &str,
    scopes: &str,
) -> Result<String> {
    // Try to use existing tokio runtime first (e.g., when called from async context)
    let scopes = scopes.to_string();
    if let Ok(_handle) = tokio::runtime::Handle::try_current() {
        // We're inside an async runtime -- spawn a blocking task
        let scopes = scopes.clone();
        std::thread::scope(|s| {
            s.spawn(|| {
                let rt = tokio::runtime::Runtime::new()?;
                rt.block_on(load_or_refresh_token(
                    account_name,
                    client_id,
                    tenant_id,
                    &scopes,
                ))
            })
            .join()
            .map_err(|_| anyhow!("OAuth2 token refresh thread panicked"))?
        })
    } else {
        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(load_or_refresh_token(
            account_name,
            client_id,
            tenant_id,
            &scopes,
        ))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// A staged sign-in older than the limit goes, a younger one stays for
    /// the wizard that may still be running, and nothing else is touched.
    #[test]
    fn the_sweep_drops_only_staged_sign_ins_past_their_age() {
        let dir = tempfile::tempdir().unwrap();
        let now = SystemTime::now();
        let write = |name: &str, age: Duration| {
            let path = dir.path().join(name);
            fs::write(&path, b"x").unwrap();
            fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(now - age)
                .unwrap();
        };
        let day = STAGED_TOKEN_MAX_AGE;
        write("abandoned.enc", day + Duration::from_secs(60));
        write("running.enc", day - Duration::from_secs(60));
        write("notes.txt", day * 3);
        assert_eq!(sweep_staged_tokens_in(dir.path(), now, day).unwrap(), 1);
        let mut left: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, ["notes.txt", "running.enc"]);
        assert_eq!(
            sweep_staged_tokens_in(&dir.path().join("missing"), now, day).unwrap(),
            0
        );
    }

    #[test]
    fn test_token_cache_expired() {
        let cache = TokenCache {
            access_token: "token".to_string(),
            refresh_token: "refresh".to_string(),
            expires_at: 0, // long expired
            scopes: Vec::new(),
        };
        assert!(cache.is_expired());
    }

    #[test]
    fn test_token_cache_not_expired() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let cache = TokenCache {
            access_token: "token".to_string(),
            refresh_token: "refresh".to_string(),
            expires_at: now + 3600, // 1 hour from now
            scopes: Vec::new(),
        };
        assert!(!cache.is_expired());
    }

    #[test]
    fn test_token_cache_almost_expired() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        // Expires in 30 seconds -- should be considered expired (60s early margin)
        let cache = TokenCache {
            access_token: "token".to_string(),
            refresh_token: "refresh".to_string(),
            expires_at: now + 30,
            scopes: Vec::new(),
        };
        assert!(cache.is_expired());
    }

    #[test]
    fn test_token_cache_serde_roundtrip() {
        let cache = TokenCache {
            access_token: "access".to_string(),
            refresh_token: "refresh".to_string(),
            expires_at: 1700000000,
            scopes: Vec::new(),
        };
        let json = serde_json::to_string(&cache).unwrap();
        let back: TokenCache = serde_json::from_str(&json).unwrap();
        assert_eq!(back.access_token, "access");
        assert_eq!(back.refresh_token, "refresh");
        assert_eq!(back.expires_at, 1700000000);
    }

    /// A cache written before PERSO-200 has no `scopes` key: it still loads,
    /// and it counts as lacking the calendar scope.
    #[test]
    fn a_cache_without_recorded_scopes_loads_and_lacks_the_calendar() {
        let old = r#"{"access_token":"a","refresh_token":"r","expires_at":1}"#;
        let cache: TokenCache = serde_json::from_str(old).unwrap();
        assert!(cache.scopes.is_empty());
        assert!(!cache.has_scope(CALENDAR_SCOPE));
    }

    /// Entra answers with or without the resource prefix; either side may
    /// carry it, and the case does not matter.
    #[test]
    fn a_scope_matches_with_or_without_its_resource_prefix() {
        let token: TokenResponse = serde_json::from_str(
            r#"{"access_token":"a","expires_in":3600,
                "scope":"Mail.Read Mail.ReadWrite https://graph.microsoft.com/Calendars.ReadWrite offline_access"}"#,
        )
        .unwrap();
        let cache = TokenCache {
            access_token: token.access_token,
            refresh_token: String::new(),
            expires_at: 0,
            scopes: granted_scopes(token.scope.as_deref().unwrap()),
        };
        assert!(cache.has_scope(CALENDAR_SCOPE));
        assert!(cache.has_scope("calendars.readwrite"));
        assert!(cache.has_scope("https://graph.microsoft.com/Mail.Read"));
        assert!(cache.has_scope("offline_access"));
        assert!(!cache.has_scope("https://graph.microsoft.com/Mail.Send"));
    }

    /// The sign-in asks for the calendar, and the mail-only retry of a refused
    /// refresh keeps every other scope.
    #[test]
    fn the_mail_only_scopes_are_the_graph_scopes_minus_the_calendar() {
        assert!(GRAPH_SCOPES.split_whitespace().any(|s| s == CALENDAR_SCOPE));
        let mail_only = without_scope(GRAPH_SCOPES, "Calendars.ReadWrite");
        assert_eq!(
            mail_only,
            "https://graph.microsoft.com/Mail.Read https://graph.microsoft.com/Mail.ReadWrite \
             https://graph.microsoft.com/Mail.Send offline_access"
        );
        assert_eq!(without_scope(&mail_only, CALENDAR_SCOPE), mail_only);
        assert_eq!(scope_name(CALENDAR_SCOPE), "Calendars.ReadWrite");
    }

    /// A Graph account whose calendar is Graph's asks for the calendar; one
    /// that opted out (`calendar_backend = "none"`) signs in and refreshes
    /// with the mail scopes only.
    #[test]
    fn an_account_without_the_graph_calendar_asks_for_mail_scopes_only() {
        let config: crate::config::GlobalConfig = toml::from_str(
            "[[accounts]]\nname = \"evoqs\"\nauth_method = \"graph\"\n\n\
             [[accounts]]\nname = \"hines\"\nauth_method = \"graph\"\ncalendar_backend = \"none\"\n",
        )
        .unwrap();
        let with_calendar = scopes_for(&config.accounts[0]);
        assert_eq!(with_calendar, GRAPH_SCOPES);
        let opted_out = scopes_for(&config.accounts[1]);
        assert!(!opted_out
            .split_whitespace()
            .any(|s| scope_name(s) == "Calendars.ReadWrite"));
        assert_eq!(opted_out, without_scope(GRAPH_SCOPES, CALENDAR_SCOPE));
        assert!(opted_out.split_whitespace().any(|s| s == "offline_access"));
    }

    fn token(scope: &str) -> TokenResponse {
        TokenResponse {
            access_token: "at".into(),
            refresh_token: None,
            expires_in: 3600,
            scope: Some(scope.into()),
        }
    }

    /// Drive [`refresh_answer`] against canned answers, one per attempt, and
    /// return its outcome with the scopes each attempt asked for.
    fn run_refresh(
        answers: Vec<std::result::Result<TokenResponse, Refusal>>,
    ) -> (Result<TokenResponse>, Vec<String>) {
        let answers = std::cell::RefCell::new(answers.into_iter());
        let asked = std::cell::RefCell::new(Vec::new());
        let outcome = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(refresh_answer("evoqs", GRAPH_SCOPES, |scopes| {
                asked.borrow_mut().push(scopes);
                let answer = answers.borrow_mut().next().expect("an attempt too many");
                async move { Ok(answer) }
            }));
        (outcome, asked.into_inner())
    }

    /// A consent refusal of the calendar scope is retried mail-only, and the
    /// grant without the calendar is what the refresh ends in.
    #[test]
    fn a_refused_calendar_consent_retries_with_the_mail_scopes() {
        let mail_only = without_scope(GRAPH_SCOPES, CALENDAR_SCOPE);
        for body in [
            r#"{"error":"invalid_grant","error_description":"AADSTS65001: The user or administrator has not consented.","error_codes":[65001]}"#,
            r#"{"error":"invalid_grant","error_description":"AADSTS70000: The request was denied because one or more scopes requested are unauthorized or expired."}"#,
            r#"{"error":"invalid_scope","error_description":"AADSTS70011: The provided value for the input parameter 'scope' is not valid."}"#,
        ] {
            let (outcome, asked) = run_refresh(vec![
                Err(Refusal {
                    status: 400,
                    body: body.into(),
                }),
                Ok(token(&mail_only)),
            ]);
            let token = outcome.expect(body);
            assert_eq!(token.scope.as_deref(), Some(mail_only.as_str()));
            assert_eq!(asked, vec![GRAPH_SCOPES.to_string(), mail_only.clone()]);
        }
    }

    /// A throttle, an outage or a dead refresh token is the ordinary refresh
    /// error after one attempt: no mail-only retry, so no reduced grant is
    /// recorded.
    #[test]
    fn a_transient_or_unrelated_refusal_is_not_retried_without_the_calendar() {
        for (status, body) in [
            (429, r#"{"error":"temporarily_unavailable"}"#),
            (503, "Service Unavailable"),
            (
                400,
                r#"{"error":"invalid_grant","error_description":"AADSTS700082: The refresh token has expired due to inactivity.","error_codes":[700082]}"#,
            ),
        ] {
            let (outcome, asked) = run_refresh(vec![Err(Refusal {
                status,
                body: body.into(),
            })]);
            let error = format!("{:#}", outcome.err().expect(body));
            assert!(error.contains(&format!("HTTP {status}")), "{error}");
            assert!(error.contains(body), "{error}");
            assert_eq!(asked, vec![GRAPH_SCOPES.to_string()]);
        }
    }

    #[test]
    fn test_device_code_url() {
        let url = device_code_url("my-tenant-id");
        assert_eq!(
            url,
            "https://login.microsoftonline.com/my-tenant-id/oauth2/v2.0/devicecode"
        );
    }

    #[test]
    fn empty_client_id_is_the_mailypoppins_app() {
        assert_eq!(client_id_or_default(""), MAILYPOPPINS_CLIENT_ID);
        assert_eq!(client_id_or_default("  \n"), MAILYPOPPINS_CLIENT_ID);
        assert_eq!(
            MAILYPOPPINS_CLIENT_ID,
            "439d7ebd-7898-4df2-b2d6-88bb61e2597b"
        );
    }

    #[test]
    fn typed_client_id_is_kept() {
        assert_eq!(
            client_id_or_default(" 00000000-1111-2222-3333-444444444444 "),
            "00000000-1111-2222-3333-444444444444"
        );
    }

    #[test]
    fn empty_tenant_is_common_and_a_typed_one_is_kept() {
        assert_eq!(tenant_or_default(""), "common");
        assert_eq!(tenant_or_default(" "), DEFAULT_TENANT);
        assert_eq!(tenant_or_default("contoso.com"), "contoso.com");
        assert_eq!(
            tenant_or_default("365f441f-42ba-4b36-851f-8b124c7becbb"),
            "365f441f-42ba-4b36-851f-8b124c7becbb"
        );
    }

    #[test]
    fn common_tenant_reaches_the_common_endpoints() {
        assert_eq!(
            device_code_url(DEFAULT_TENANT),
            "https://login.microsoftonline.com/common/oauth2/v2.0/devicecode"
        );
        assert_eq!(
            token_url(DEFAULT_TENANT),
            "https://login.microsoftonline.com/common/oauth2/v2.0/token"
        );
    }

    #[test]
    fn test_token_url() {
        let url = token_url("my-tenant-id");
        assert_eq!(
            url,
            "https://login.microsoftonline.com/my-tenant-id/oauth2/v2.0/token"
        );
    }
}
