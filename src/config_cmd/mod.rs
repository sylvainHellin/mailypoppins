//! The `mp config …` handlers that still run in the client.
//!
//! `set-password`, `oauth2-login` and `reset-secrets` answer from
//! `config.set_password`, `config.oauth2_login` and `config.reset_secrets`
//! since P4-U14, so their direct handlers went with the rest of the direct
//! engine paths in P4-U15. What is left is the two wizards, which prompt and
//! then write `config.toml` themselves, and the two read-only commands.

mod helpers;
mod init;
pub mod password;
mod show;

pub use init::{cmd_config_add_account, cmd_config_init};
pub use show::{cmd_config_path, cmd_config_show};

#[cfg(test)]
mod tests;
