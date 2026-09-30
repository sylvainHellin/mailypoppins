//! What the webview may navigate to, and the log of what it was refused.
//!
//! `on_navigation` sees the main frame and every subframe, the reader's
//! `mpmsg` iframe included, so the allowlist is the app's own origin, the
//! `mpmsg` scheme and the two `about:` documents a fresh frame starts on.
//! Everything else is refused and recorded; nothing here opens a browser.
//! Opening one is `open_external`'s, and only on an explicit frontend call.

use std::sync::Mutex;

use tauri::Url;

use crate::session::{InterceptSource, InterceptedUrl};

/// Entries the log keeps before dropping the oldest.
const LOG_CAP: usize = 1000;

/// Whether the webview may load `url`. `dev_origin` is the Vite dev server
/// (`build.devUrl`) in a debug build and `None` in a release one.
pub fn navigation_allowed(url: &Url, dev_origin: Option<&Url>) -> bool {
    match url.scheme() {
        // The bundled frontend on macOS and Linux.
        "tauri" => url.host_str() == Some("localhost"),
        // The reader: `mpmsg://localhost/<account>/<row_id>`.
        "mpmsg" => url.host_str() == Some("localhost"),
        // A frame before its first load.
        "about" => matches!(url.path(), "blank" | "srcdoc"),
        "http" | "https" => {
            // The Windows spellings of the two custom origins.
            let custom_origin = url.scheme() == "http"
                && matches!(
                    url.host_str(),
                    Some("tauri.localhost") | Some("mpmsg.localhost")
                );
            let dev = dev_origin.is_some_and(|dev| {
                dev.scheme() == url.scheme()
                    && dev.host_str() == url.host_str()
                    && dev.port_or_known_default() == url.port_or_known_default()
            });
            custom_origin || dev
        }
        _ => false,
    }
}

/// The refused URLs, oldest first, until `intercepted_urls` drains them.
#[derive(Default)]
pub struct InterceptLog(Mutex<Vec<InterceptedUrl>>);

impl InterceptLog {
    pub fn push(&self, entry: InterceptedUrl) {
        let mut log = match self.0.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        if log.len() >= LOG_CAP {
            log.remove(0);
        }
        log.push(entry);
    }

    /// Everything logged so far; the log is empty afterwards.
    pub fn drain(&self) -> Vec<InterceptedUrl> {
        match self.0.lock() {
            Ok(mut g) => std::mem::take(&mut *g),
            Err(poisoned) => std::mem::take(&mut *poisoned.into_inner()),
        }
    }
}

/// Record a refused URL; the caller also tells the frontend.
pub fn intercepted(log: &InterceptLog, url: &Url, source: InterceptSource) -> InterceptedUrl {
    let entry = InterceptedUrl::now(url.to_string(), source);
    tracing::info!("[nav] refused {source:?}: {url}");
    log.push(entry.clone());
    entry
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s).expect("a url")
    }

    #[test]
    fn the_app_and_the_reader_are_allowed() {
        for ok in [
            "tauri://localhost/",
            "tauri://localhost/index.html",
            "mpmsg://localhost/work/42",
            "http://mpmsg.localhost/work/42",
            "http://tauri.localhost/",
            "about:blank",
            "about:srcdoc",
        ] {
            assert!(navigation_allowed(&url(ok), None), "{ok}");
        }
    }

    #[test]
    fn everything_else_is_refused() {
        for bad in [
            "https://example.com/",
            "http://example.com/",
            "http://localhost:1420/",
            "http://localhost/",
            "mailto:someone@example.com",
            "file:///etc/hosts",
            "javascript:alert(1)",
            "data:text/html,hi",
            "tauri://evil.example/",
            "mpmsg://evil.example/work/1",
            "about:config",
            "https://mpmsg.localhost/work/1",
        ] {
            assert!(!navigation_allowed(&url(bad), None), "{bad}");
        }
    }

    #[test]
    fn the_dev_server_is_allowed_only_when_given() {
        let dev = url("http://localhost:1420");
        assert!(navigation_allowed(
            &url("http://localhost:1420/src/main.tsx"),
            Some(&dev)
        ));
        assert!(!navigation_allowed(
            &url("http://localhost:1421/"),
            Some(&dev)
        ));
        assert!(!navigation_allowed(
            &url("https://localhost:1420/"),
            Some(&dev)
        ));
        assert!(!navigation_allowed(&url("http://localhost:1420/"), None));
    }

    #[test]
    fn the_log_drains_and_stays_bounded() {
        let log = InterceptLog::default();
        for i in 0..(LOG_CAP + 5) {
            intercepted(
                &log,
                &url(&format!("https://e.example/{i}")),
                InterceptSource::Navigation,
            );
        }
        let drained = log.drain();
        assert_eq!(drained.len(), LOG_CAP);
        assert_eq!(drained[0].url, "https://e.example/5");
        assert!(log.drain().is_empty());
    }
}
