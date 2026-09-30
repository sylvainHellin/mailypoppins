//! Tracing to stderr and to `<data>/logs/mp-desktop.log`, beside the daemon's
//! own log. `MP_DESKTOP_LOG` sets the level (`error` to `trace`, default
//! `info`); `mp-client`'s `log` records are bridged in by the subscriber.

use std::fs::File;
use std::sync::Arc;

use tracing::level_filters::LevelFilter;
use tracing_subscriber::fmt::writer::MakeWriterExt;

use crate::paths::Paths;

pub const LOG_ENV: &str = "MP_DESKTOP_LOG";

fn level() -> LevelFilter {
    std::env::var(LOG_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<LevelFilter>().ok())
        .unwrap_or(LevelFilter::INFO)
}

/// Install the global subscriber; a second call is a no-op.
pub fn init(paths: &Paths) {
    let file = mp_core::config::create_private_dir_all(&paths.logs_dir)
        .and_then(|_| {
            File::options()
                .create(true)
                .append(true)
                .open(&paths.desktop_log)
        })
        .map(Arc::new);
    let builder = tracing_subscriber::fmt()
        .with_max_level(level())
        .with_ansi(false)
        .with_target(false);
    let installed = match file {
        Ok(file) => builder.with_writer(std::io::stderr.and(file)).try_init(),
        Err(e) => {
            eprintln!(
                "mp-desktop: cannot write {}: {e}; logging to stderr only",
                paths.desktop_log.display()
            );
            builder.with_writer(std::io::stderr).try_init()
        }
    };
    if let Err(e) = installed {
        eprintln!("mp-desktop: tracing was already set up: {e}");
    }
}
