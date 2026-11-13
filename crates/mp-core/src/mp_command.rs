//! The `mp` commands a message tells the user to run (PERSO-199).
//!
//! Each profile has its own daemon, configuration and secrets (PERSO-176), so a
//! command printed on a profile carries its `--profile`: without it, the
//! command run as printed acts on the default profile.

/// `mp` followed by `args`, with `--profile <name>` on `profile`.
pub fn on(profile: Option<&str>, args: &[&str]) -> String {
    let mut words = vec!["mp"];
    if let Some(name) = profile {
        words.extend(["--profile", name]);
    }
    words.extend_from_slice(args);
    words.join(" ")
}

/// `mp` followed by `args`, on the profile this process runs on.
pub fn here(args: &[&str]) -> String {
    on(crate::profile::active().as_deref(), args)
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
}
