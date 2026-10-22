//! The client side of `mp config …`: prompts and rendering.
//!
//! Every command here reads and writes through the daemon. `set-password`,
//! `oauth2-login` and `reset-secrets` answer from `config.set_password`,
//! `config.oauth2_login` and `config.reset_secrets` since P4-U14; `show`
//! renders `config.get` and `account.credentials`; and the two wizards
//! (`init`, `add-account`) prompt here and probe, sign in, write and store
//! passwords through the daemon since the daemon leftovers of 2026-10-22.
//! `path` is the one that never asks a daemon: it prints a computed path.

mod init;
pub mod password;
mod show;

pub use init::{
    cmd_config_add_account, cmd_config_init, graph_candidate, imap_candidate, ImapAnswers,
    Mailboxes, Probe, WizardDaemon,
};
pub use show::{cmd_config_path, cmd_config_show};

#[cfg(test)]
mod tests;
