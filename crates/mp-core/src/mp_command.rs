//! The `mp` commands a message tells the user to run (PERSO-199).
//!
//! Each profile has its own daemon, configuration and secrets (PERSO-176), so a
//! command printed on a profile carries its `--profile`: without it, the
//! command run as printed acts on the default profile. And it is pasted into a
//! shell as printed, so a word a POSIX shell would split or expand (an account
//! name with a space, say) is quoted.

use std::borrow::Cow;

use mp_protocol::account::RejectedLogin;

/// `mp` followed by `args`, with `--profile <name>` on `profile`, each word
/// [`quote`]d.
pub fn on(profile: Option<&str>, args: &[&str]) -> String {
    let mut words = vec!["mp"];
    if let Some(name) = profile {
        words.extend(["--profile", name]);
    }
    words.extend_from_slice(args);
    words.into_iter().map(quote).collect::<Vec<_>>().join(" ")
}

/// `mp` followed by `args`, on the profile this process runs on.
pub fn here(args: &[&str]) -> String {
    on(crate::profile::active().as_deref(), args)
}

/// What every surface says about an account whose `login` the server refused
/// (PERSO-296), naming the login and the command that fixes it: the daemon's
/// `account:<name>` health check detail, which `mp daemon health` prints and
/// the desktop app's health line shows, the error of a change or a send held
/// behind the refusal, and the TUI's sidebar. Lower-case and without a full
/// stop, since each caller puts it after the account's name or inside a
/// longer line.
///
/// `shared_password` is whether the account's IMAP and SMTP logins use one
/// password (`account.credentials`' `shared_password`, PERSO-297): then the
/// command is the kind-less one, which sets that password for both, since a
/// password changed on the server changed for both logins; else it names the
/// refused login's kind, leaving the other password alone.
///
/// Here rather than in `mp_client::format` beside the other shared
/// sentences, because its command is [`here`]'s, on this process's profile
/// and quoted, and `mp-client` links `mp-protocol` alone.
pub fn login_rejected_detail(account: &str, login: RejectedLogin, shared_password: bool) -> String {
    let set_password = |kind: &str| {
        let mut args = vec!["config", "set-password", "-A", account];
        if !shared_password {
            args.push(kind);
        }
        here(&args)
    };
    match login {
        RejectedLogin::Imap => format!(
            "the server rejected the IMAP password; set it with {}",
            set_password("imap")
        ),
        RejectedLogin::Smtp => format!(
            "the server rejected the SMTP password; set it with {}",
            set_password("smtp")
        ),
        RejectedLogin::Graph => format!(
            "the sign-in expired or was revoked; sign in again with {}",
            here(&["config", "oauth2-login", "-A", account])
        ),
    }
}

/// `word` as a POSIX shell reads it back: as is when it is made of letters,
/// digits and `_.@-` alone, else in single quotes, each `'` in it closed,
/// escaped and reopened.
pub fn quote(word: &str) -> Cow<'_, str> {
    let safe = |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '@' | '-');
    if !word.is_empty() && word.chars().all(safe) {
        Cow::Borrowed(word)
    } else {
        Cow::Owned(format!("'{}'", word.replace('\'', r"'\''")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_on_a_profile_carries_the_profile() {
        assert_eq!(on(None, &["daemon", "restart"]), "mp daemon restart");
        assert_eq!(
            on(Some("demo"), &["daemon", "restart"]),
            "mp --profile demo daemon restart"
        );
        let _demo = crate::config::test_env::ProfileOverride::set(Some("demo"));
        assert_eq!(here(&["sync"]), "mp --profile demo sync");
    }

    /// An account name may hold spaces or quotes, and the command is pasted
    /// into a shell as printed, so a word outside the safe set is quoted.
    #[test]
    fn a_word_a_shell_would_split_or_expand_is_quoted() {
        assert_eq!(
            on(None, &["sync", "-A", "TUM Mail"]),
            "mp sync -A 'TUM Mail'"
        );
        assert_eq!(
            on(None, &["sync", "-A", "Bob's $HOME"]),
            r"mp sync -A 'Bob'\''s $HOME'"
        );
        assert_eq!(on(None, &["sync", "-A", ""]), "mp sync -A ''");
        assert_eq!(
            on(None, &["sync", "-A", "me@tum.de_2-x"]),
            "mp sync -A me@tum.de_2-x"
        );
        assert_eq!(quote("work"), "work");
        assert_eq!(quote("a;b"), "'a;b'");
    }

    /// The sentence every surface shows for a login the server refused
    /// (PERSO-296) names the login and the command that fixes it, with the
    /// account in it so it can be pasted as it is.
    #[test]
    fn a_rejected_login_names_the_login_and_the_command_that_fixes_it() {
        assert_eq!(
            login_rejected_detail("tum", RejectedLogin::Imap, false),
            "the server rejected the IMAP password; set it with mp config set-password -A tum imap"
        );
        assert_eq!(
            login_rejected_detail("tum", RejectedLogin::Smtp, false),
            "the server rejected the SMTP password; set it with mp config set-password -A tum smtp"
        );
        assert_eq!(
            login_rejected_detail("hines", RejectedLogin::Graph, false),
            "the sign-in expired or was revoked; sign in again with mp config oauth2-login -A hines"
        );
    }

    /// An account whose IMAP and SMTP logins use one password (PERSO-297) is
    /// told the kind-less command, which sets that password for both; a
    /// Graph account has no password, so the flag changes nothing for it.
    #[test]
    fn a_rejected_shared_password_names_the_command_that_sets_both() {
        assert_eq!(
            login_rejected_detail("tum", RejectedLogin::Imap, true),
            "the server rejected the IMAP password; set it with mp config set-password -A tum"
        );
        assert_eq!(
            login_rejected_detail("tum", RejectedLogin::Smtp, true),
            "the server rejected the SMTP password; set it with mp config set-password -A tum"
        );
        assert_eq!(
            login_rejected_detail("hines", RejectedLogin::Graph, true),
            login_rejected_detail("hines", RejectedLogin::Graph, false)
        );
        let _demo = crate::config::test_env::ProfileOverride::set(Some("demo"));
        assert_eq!(
            login_rejected_detail("TUM Mail", RejectedLogin::Imap, true),
            "the server rejected the IMAP password; set it with \
             mp --profile demo config set-password -A 'TUM Mail'"
        );
    }

    /// Its command pastes as one argument for an account name a shell would
    /// split, and runs on the profile whose daemon refused the login.
    #[test]
    fn a_rejected_login_command_is_quoted_and_on_the_profile() {
        let _demo = crate::config::test_env::ProfileOverride::set(Some("demo"));
        assert_eq!(
            login_rejected_detail("TUM Mail", RejectedLogin::Imap, false),
            "the server rejected the IMAP password; set it with \
             mp --profile demo config set-password -A 'TUM Mail' imap"
        );
        assert_eq!(
            login_rejected_detail("TUM Mail", RejectedLogin::Smtp, false),
            "the server rejected the SMTP password; set it with \
             mp --profile demo config set-password -A 'TUM Mail' smtp"
        );
        assert_eq!(
            login_rejected_detail("Bob's", RejectedLogin::Graph, false),
            "the sign-in expired or was revoked; sign in again with \
             mp --profile demo config oauth2-login -A 'Bob'\\''s'"
        );
    }
}
