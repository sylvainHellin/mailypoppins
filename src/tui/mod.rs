pub mod app;
mod actions;
mod bg;
mod event;
mod helpers;
mod theme;
mod ui;

use std::io;
use std::sync::mpsc;

use anyhow::Result;
use ratatui::{backend::CrosstermBackend, Terminal};

use app::{App, BgResult};
use helpers::{
    graph_watcher_loop, init_terminal, install_panic_hook, restore_terminal, watcher_loop,
    WatchEvent,
};

/// Entry point for the TUI. Call this when `email` is invoked with no arguments.
pub fn run() -> Result<()> {
    install_panic_hook();
    let mut terminal = init_terminal()?;
    let result = run_loop(&mut terminal);
    restore_terminal()?;
    result
}

fn run_loop(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    let mut app = App::new();

    let size = terminal.size()?;
    app.terminal_width = size.width;
    app.terminal_height = size.height;

    // Spawn one watcher thread per account that has IMAP config
    let (watch_tx, watch_rx) = mpsc::channel::<WatchEvent>();
    for (i, acct) in app.accounts.iter_mut().enumerate() {
        if let Some(ref imap_cfg) = acct.imap_config {
            acct.watcher_active = true;
            let tx = watch_tx.clone();
            let imap_clone = imap_cfg.clone();
            let acct_idx = i;
            std::thread::spawn(move || {
                watcher_loop(tx, imap_clone, acct_idx);
            });
        } else if let Some(ref graph_cfg) = acct.graph_config {
            acct.watcher_active = true;
            let tx = watch_tx.clone();
            let graph_clone = graph_cfg.clone();
            let acct_idx = i;
            std::thread::spawn(move || {
                graph_watcher_loop(tx, graph_clone, acct_idx);
            });
        }
    }
    // Sync watcher_active for active account
    if let Some(acct) = app.accounts.first() {
        app.watcher_active = acct.watcher_active;
    }

    // Background task results channel
    let (bg_tx, bg_rx) = mpsc::channel::<BgResult>();

    // Kick off the per-account message-ID index scan on background
    // threads so the first frame paints without waiting on the
    // ~1.4 s walkdir over ~17 k frontmatter files (ticket #0003).
    // Each thread sends `BgResult::IndexReady` when done; the
    // existing `bg_count > 0` gate in `Action::Fetch` / `Action::Sync`
    // queues user-triggered sync operations until the index arrives.
    // The `IndexReady` handler in `bg.rs` also pushes a per-account
    // `Action::FetchAccount` to drive the startup auto-fetch (#0001),
    // which staggers naturally because each account fires as soon as
    // its own index lands.
    if !app.accounts.is_empty() {
        for (i, acct) in app.accounts.iter().enumerate() {
            let mailboxes = acct.mailboxes.clone();
            let account_name = acct.account_config.name.clone();
            let tx = bg_tx.clone();
            app.bg_count += 1;
            std::thread::spawn(move || {
                let index = app::build_message_id_index(&mailboxes, &account_name);
                let _ = tx.send(BgResult::IndexReady {
                    account_index: i,
                    index,
                });
            });
        }
        app.set_status_level(
            "Indexing...".to_string(),
            app::StatusLevel::Progress,
        );
    }

    while app.running {
        terminal.draw(|frame| ui::view(&mut app, frame))?;

        if let Some(msg) = event::poll_event()? {
            let mut current_msg = Some(msg);
            while let Some(m) = current_msg {
                current_msg = app.update(m);
            }
        } else {
            app.tick_status();
            if app.bg_count > 0 {
                app.bg_spin_tick = app.bg_spin_tick.wrapping_add(1);
            }
        }

        // Check background watcher
        match watch_rx.try_recv() {
            Ok(WatchEvent::Changed { account_index }) => {
                let mut current_msg = Some(app::Message::MailboxChanged { account_index });
                while let Some(m) = current_msg {
                    current_msg = app.update(m);
                }
            }
            Ok(WatchEvent::Reconnected { account_index }) => {
                let acct_name = app.accounts.get(account_index)
                    .map(|a| a.account_config.name.clone())
                    .unwrap_or_default();
                app.set_status(format!("Watch ({}): reconnected", acct_name));
                if let Some(acct) = app.accounts.get_mut(account_index) {
                    acct.watcher_active = true;
                }
                if account_index == app.active_account {
                    app.watcher_active = true;
                }
            }
            Ok(WatchEvent::Error { account_index, message }) => {
                let acct_name = app.accounts.get(account_index)
                    .map(|a| a.account_config.name.clone())
                    .unwrap_or_default();
                app.set_status(format!("Watch ({}): {}", acct_name, message));
                if let Some(acct) = app.accounts.get_mut(account_index) {
                    acct.watcher_active = false;
                }
                if account_index == app.active_account {
                    app.watcher_active = false;
                }
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                for acct in &mut app.accounts {
                    acct.watcher_active = false;
                }
                app.watcher_active = false;
            }
        }

        // Check background task results (drain all available)
        while let Ok(result) = bg_rx.try_recv() {
            bg::handle_bg_result(&mut app, result);
        }

        // Auto-execute queued action when all mutations complete
        if app.bg_mutations == 0 && app.pending_actions.is_empty() {
            if let Some(action) = app.queued_action.take() {
                app.pending_actions.push_back(action);
            }
        }

        // Process pending actions (drain queue so user actions are never lost)
        while let Some(action) = app.pending_actions.pop_front() {
            actions::handle_action(&mut app, terminal, action, &bg_tx)?;
        }
    }

    Ok(())
}
