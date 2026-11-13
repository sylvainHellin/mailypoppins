//! The HTTP seam of the EWS backend: one SOAP envelope out, the answer body
//! back (PERSO-256).
//!
//! [`EwsFetch`] is what the transport implements: it posts an envelope to the
//! account's endpoint, logged in over NTLM (`ntlm.rs`), and answers the body.
//! The backend builds every envelope ([`envelope`]) and reads every answer,
//! so the transport knows nothing of SOAP; the tests drive the backend
//! through a scripted seam instead of a server.
//!
//! [`EwsSession`] is the backend's side of the seam for one sync pass: it
//! wraps each operation in the envelope and holds a refused login, so a
//! refusal stops the pass at once. TUM's directory locks an account after a
//! run of refused logins, and the backend never asks the server again after
//! one inside a pass.

use std::fmt;
use std::future::Future;
use std::sync::Mutex;

/// The schema version every request names: the server (Exchange 2019,
/// 15.2.2562) accepts `Exchange2010_SP2` to `Exchange2016` and refuses
/// `Exchange2019` with a SOAP fault, and `Exchange2013_SP1` is the one the
/// spike read every fixture with and the first that has `ProposedStart`.
pub const REQUEST_SERVER_VERSION: &str = "Exchange2013_SP1";

/// The SOAP envelope around one operation, `body` being its element in the
/// `m:` (messages) namespace with `t:` (types) children.
pub fn envelope(body: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
         <s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" \
         xmlns:t=\"http://schemas.microsoft.com/exchange/services/2006/types\" \
         xmlns:m=\"http://schemas.microsoft.com/exchange/services/2006/messages\">\
         <s:Header><t:RequestServerVersion Version=\"{REQUEST_SERVER_VERSION}\"/></s:Header>\
         <s:Body>{body}</s:Body></s:Envelope>"
    )
}

/// Why the seam brought back no answer to read.
#[derive(Debug)]
pub enum EwsFetchError {
    /// The server refused the login: an HTTP 401 after the third NTLM leg,
    /// carrying what the server said. Never retried inside a pass.
    LoginRefused(String),
    /// An HTTP status the backend cannot read a SOAP answer from (anything
    /// but 200, and 500, which carries a SOAP fault): a wrong endpoint (404),
    /// a server that sheds load (503), a proxy's error page.
    Status { status: u16, body: String },
    /// No answer at all: DNS, TCP, TLS, a timeout, a dropped connection.
    Transport(anyhow::Error),
}

impl fmt::Display for EwsFetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EwsFetchError::LoginRefused(answer) => {
                write!(f, "the Exchange server refused the EWS login")?;
                if !answer.trim().is_empty() {
                    write!(f, " ({})", answer.trim())?;
                }
                Ok(())
            }
            EwsFetchError::Status { status, .. } => {
                write!(f, "the Exchange server answered HTTP {status}")
            }
            EwsFetchError::Transport(err) => write!(f, "could not reach the EWS endpoint: {err:#}"),
        }
    }
}

impl std::error::Error for EwsFetchError {}

/// The transport: one logged-in POST of a SOAP envelope to the account's
/// endpoint.
pub trait EwsFetch: Send + Sync {
    /// Post `envelope` (the whole document, [`envelope`]) with
    /// `Content-Type: text/xml; charset=utf-8` and answer the response body
    /// of an HTTP 200, or of an HTTP 500, whose body is a SOAP fault the
    /// backend reads. Every other outcome is an [`EwsFetchError`].
    fn post(&self, envelope: &str) -> impl Future<Output = Result<String, EwsFetchError>> + Send;
}

/// The backend's side of the seam over one sync pass: envelopes out, and a
/// refused login held so no later call of the pass asks the server again.
pub struct EwsSession<F> {
    fetch: F,
    refused: Mutex<Option<String>>,
}

impl<F: EwsFetch> EwsSession<F> {
    pub fn new(fetch: F) -> Self {
        EwsSession {
            fetch,
            refused: Mutex::new(None),
        }
    }

    /// The seam itself, for a test that reads what was sent.
    #[cfg(test)]
    pub(super) fn fetch(&self) -> &F {
        &self.fetch
    }

    fn held(&self) -> std::sync::MutexGuard<'_, Option<String>> {
        self.refused
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Send the operation `body` in its envelope and answer the response
    /// body. After a refused login every call answers the same refusal
    /// without reaching the seam.
    pub async fn call(&self, body: &str) -> Result<String, EwsFetchError> {
        if let Some(answer) = self.held().clone() {
            return Err(EwsFetchError::LoginRefused(answer));
        }
        let answer = self.fetch.post(&envelope(body)).await;
        if let Err(EwsFetchError::LoginRefused(why)) = &answer {
            *self.held() = Some(why.clone());
        }
        answer
    }
}
