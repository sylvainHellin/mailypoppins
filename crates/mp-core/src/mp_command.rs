//! The `mp` commands a message tells the user to run (PERSO-199).
//!
//! Each profile has its own daemon, configuration and secrets (PERSO-176), so a
//! command printed on a profile carries its `--profile`: without it, the
//! command run as printed acts on the default profile. And it is pasted into a
//! shell as printed, so a word a POSIX shell would split or expand (an account
//! name with a space, say) is quoted.

use std::borrow::Cow;

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
}
