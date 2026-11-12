//! Named profiles (PERSO-176): `--profile <name>` and `MAILYPOPPINS_PROFILE`.
//!
//! A profile moves the two roots every other path derives from:
//! [`config_dir`](crate::config::config_dir) becomes
//! `<config_base>/profiles/<name>` and
//! [`mailypoppins_data_dir`](crate::config::mailypoppins_data_dir) becomes
//! `<data_base>/profiles/<name>`. The store, the drafts, the tokens, the
//! secrets file, the logs, the daemon's runtime directory and its socket all
//! follow, so a profile is a second, isolated daemon with its own accounts. No
//! profile, an empty variable or the reserved name [`DEFAULT_PROFILE`] are
//! exactly the paths there were before profiles existed.
//!
//! The resolvers read only the environment variable. An entry point (the `mp`
//! binary, the desktop app) turns its `--profile` flag into that variable with
//! [`apply`] while it still has one thread, which is what makes every process
//! it spawns (the on-demand daemon, `$EDITOR`, the desktop's sidecar) inherit
//! the profile without a line of its own.

use std::fmt;
use std::path::PathBuf;

/// The variable that names the active profile.
pub const PROFILE_ENV: &str = "MAILYPOPPINS_PROFILE";

/// The name that selects the default profile, so `--profile default`
/// overrides an exported [`PROFILE_ENV`].
pub const DEFAULT_PROFILE: &str = "default";

/// The longest profile name. The macOS socket path limit is 104 bytes, and
/// `/Users/<user>/Library/Application Support/mailypoppins/profiles/<name>/runtime/daemon.sock`
/// is 78 bytes plus the user name plus the profile name, so twelve characters
/// leave room for a thirteen-character user name.
pub const MAX_NAME_LEN: usize = 12;

/// The directory under each root that holds the profiles.
const PROFILES_DIR: &str = "profiles";

/// Why a launch's profile was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProfileError {
    /// A name outside the rule [`validate`] checks.
    InvalidName(String),
    /// A profile beside an explicit config or data directory variable.
    Conflict {
        /// How the profile was asked for: `--profile demo` or
        /// `MAILYPOPPINS_PROFILE=demo`.
        profile: String,
        /// The directory variable and its value, `MAILYPOPPINS_DATA_DIR=/x`.
        variable: String,
    },
    /// `--profile` as the last argument, with no name after it.
    MissingValue,
}

impl fmt::Display for ProfileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProfileError::InvalidName(name) => write!(
                f,
                "`{name}` is not a profile name: a profile name is 1 to {MAX_NAME_LEN} letters, \
                 digits, `-` or `_`"
            ),
            ProfileError::Conflict { profile, variable } => write!(
                f,
                "{profile} and {variable} both choose where mailypoppins keeps its files; \
                 use one of them"
            ),
            ProfileError::MissingValue => write!(f, "--profile needs a profile name"),
        }
    }
}

impl std::error::Error for ProfileError {}

/// Whether `name` is a profile name: 1 to [`MAX_NAME_LEN`] ASCII letters,
/// digits, `-` and `_`. ASCII only, because the limit stands for bytes in a
/// socket path and a multi-byte letter would spend more than one.
pub fn validate(name: &str) -> Result<(), ProfileError> {
    let valid = !name.is_empty()
        && name.len() <= MAX_NAME_LEN
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if valid {
        Ok(())
    } else {
        Err(ProfileError::InvalidName(name.to_string()))
    }
}

/// The profile a launch runs on, from its `--profile` flag and the
/// environment `env` reads: `None` is the default profile.
///
/// The flag wins over [`PROFILE_ENV`]. An empty variable is unset, as the
/// directory variables treat theirs. A profile other than the default beside a
/// non-empty `MAILYPOPPINS_CONFIG_DIR` or `MAILYPOPPINS_DATA_DIR` is refused,
/// naming both, since the two would disagree on where the files are.
pub fn resolve(
    flag: Option<&str>,
    env: impl Fn(&str) -> Option<String>,
) -> Result<Option<String>, ProfileError> {
    let (name, asked) = match flag {
        Some(name) => (name.to_string(), format!("--profile {name}")),
        None => match env(PROFILE_ENV).filter(|value| !value.is_empty()) {
            Some(name) => {
                let asked = format!("{PROFILE_ENV}={name}");
                (name, asked)
            }
            None => return Ok(None),
        },
    };
    if name == DEFAULT_PROFILE {
        return Ok(None);
    }
    validate(&name)?;
    for variable in [crate::config::CONFIG_DIR_ENV, crate::config::DATA_DIR_ENV] {
        if let Some(value) = env(variable).filter(|value| !value.is_empty()) {
            return Err(ProfileError::Conflict {
                profile: asked,
                variable: format!("{variable}={value}"),
            });
        }
    }
    Ok(Some(name))
}

/// Resolve the launch's profile from `flag` and the process environment, and
/// make [`PROFILE_ENV`] say it: set to the name, or removed for the default
/// profile, so every child process inherits the choice.
///
/// Call it before the process starts a second thread (before the tokio
/// runtime, the logger, or a GUI toolkit): it writes the environment.
pub fn apply(flag: Option<&str>) -> Result<Option<String>, ProfileError> {
    let profile = resolve(flag, |name| std::env::var(name).ok())?;
    match &profile {
        Some(name) => std::env::set_var(PROFILE_ENV, name),
        None => std::env::remove_var(PROFILE_ENV),
    }
    Ok(profile)
}

/// The `--profile <name>` or `--profile=<name>` among a launch's arguments,
/// for a program that has no argument parser of its own (the desktop app).
/// The last one wins, as a repeated flag does with clap.
pub fn flag_in_args<I>(args: I) -> Result<Option<String>, ProfileError>
where
    I: IntoIterator<Item = String>,
{
    let mut args = args.into_iter();
    let mut found = None;
    while let Some(arg) = args.next() {
        if arg == "--profile" {
            found = Some(args.next().ok_or(ProfileError::MissingValue)?);
        } else if let Some(name) = arg.strip_prefix("--profile=") {
            found = Some(name.to_string());
        }
    }
    Ok(found)
}

/// The active profile, `None` for the default one.
///
/// # Panics
///
/// On a [`PROFILE_ENV`] that is not a profile name. Every entry point refuses
/// one through [`apply`] before anything resolves a path, so meeting one here
/// means a path was resolved before the entry point ran, and quietly falling
/// back to the default profile would be the mistake profiles exist to prevent.
pub fn active() -> Option<String> {
    let value = profile_env()?;
    if value.is_empty() || value == DEFAULT_PROFILE {
        return None;
    }
    if let Err(e) = validate(&value) {
        panic!("{PROFILE_ENV}: {e}");
    }
    Some(value)
}

/// `base` itself for the default profile, else `base/profiles/<name>`.
pub fn root_under(base: PathBuf) -> PathBuf {
    match active() {
        Some(name) => base.join(PROFILES_DIR).join(name),
        None => base,
    }
}

/// The desktop window's title: `mailypoppins`, or `mailypoppins (demo)` on a
/// profile, so a window on the demo profile cannot pass for the real one.
pub fn window_title(profile: Option<&str>) -> String {
    match profile {
        Some(name) => format!("mailypoppins ({name})"),
        None => "mailypoppins".to_string(),
    }
}

/// The value of [`PROFILE_ENV`], through the test seam.
fn profile_env() -> Option<String> {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(value) = crate::config::test_env::profile() {
        return value;
    }
    std::env::var(PROFILE_ENV).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |name| map.get(name).cloned()
    }

    #[test]
    fn a_profile_name_takes_letters_digits_dash_underscore_up_to_twelve() {
        for good in ["demo", "work-2", "a_b", "A", "abcdefghijkl"] {
            assert_eq!(validate(good), Ok(()), "{good}");
        }
        for bad in ["", "abcdefghijklm", "a/b", "..", "a b", "dé", "a.b"] {
            assert_eq!(
                validate(bad),
                Err(ProfileError::InvalidName(bad.to_string())),
                "{bad}"
            );
        }
        assert_eq!(
            validate("a/b").unwrap_err().to_string(),
            "`a/b` is not a profile name: a profile name is 1 to 12 letters, digits, `-` or `_`"
        );
    }

    #[test]
    fn the_flag_wins_over_the_environment() {
        let env = env_of(&[(PROFILE_ENV, "work")]);
        assert_eq!(resolve(Some("demo"), &env), Ok(Some("demo".to_string())));
        assert_eq!(resolve(None, &env), Ok(Some("work".to_string())));
        assert_eq!(
            resolve(None, env_of(&[(PROFILE_ENV, "a/b")])),
            Err(ProfileError::InvalidName("a/b".to_string()))
        );
    }

    #[test]
    fn default_and_empty_mean_no_profile() {
        assert_eq!(resolve(None, env_of(&[])), Ok(None));
        assert_eq!(resolve(None, env_of(&[(PROFILE_ENV, "")])), Ok(None));
        assert_eq!(resolve(None, env_of(&[(PROFILE_ENV, "default")])), Ok(None));
        // `--profile default` overrides an exported profile, and keeps the
        // explicit directory variables working as they did.
        let env = env_of(&[
            (PROFILE_ENV, "demo"),
            ("MAILYPOPPINS_DATA_DIR", "/d"),
            ("MAILYPOPPINS_CONFIG_DIR", "/c"),
        ]);
        assert_eq!(resolve(Some("default"), &env), Ok(None));
    }

    #[test]
    fn a_profile_beside_an_explicit_dir_variable_is_refused_naming_both() {
        let refused = resolve(Some("demo"), env_of(&[("MAILYPOPPINS_DATA_DIR", "/d")]));
        assert_eq!(
            refused.unwrap_err().to_string(),
            "--profile demo and MAILYPOPPINS_DATA_DIR=/d both choose where mailypoppins keeps \
             its files; use one of them"
        );
        let refused = resolve(
            None,
            env_of(&[(PROFILE_ENV, "demo"), ("MAILYPOPPINS_CONFIG_DIR", "/c")]),
        );
        assert_eq!(
            refused,
            Err(ProfileError::Conflict {
                profile: "MAILYPOPPINS_PROFILE=demo".to_string(),
                variable: "MAILYPOPPINS_CONFIG_DIR=/c".to_string(),
            })
        );
        // An empty directory variable is unset, so it does not conflict.
        assert_eq!(
            resolve(Some("demo"), env_of(&[("MAILYPOPPINS_DATA_DIR", "")])),
            Ok(Some("demo".to_string()))
        );
    }

    #[test]
    fn the_launch_arguments_carry_the_profile_flag() {
        let args = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(flag_in_args(args(&["mp-desktop"])), Ok(None));
        assert_eq!(
            flag_in_args(args(&["mp-desktop", "--profile", "demo"])),
            Ok(Some("demo".to_string()))
        );
        assert_eq!(
            flag_in_args(args(&["mp-desktop", "--profile=demo", "--fixture"])),
            Ok(Some("demo".to_string()))
        );
        assert_eq!(
            flag_in_args(args(&["x", "--profile", "a", "--profile=b"])),
            Ok(Some("b".to_string()))
        );
        assert_eq!(
            flag_in_args(args(&["mp-desktop", "--profile"])),
            Err(ProfileError::MissingValue)
        );
    }

    #[test]
    fn the_window_title_names_a_profile() {
        assert_eq!(window_title(None), "mailypoppins");
        assert_eq!(window_title(Some("demo")), "mailypoppins (demo)");
    }

    #[test]
    fn the_active_profile_reads_the_variable() {
        use crate::config::test_env::ProfileOverride;
        let _none = ProfileOverride::set(None);
        assert_eq!(active(), None);
        let _default = ProfileOverride::set(Some("default"));
        assert_eq!(active(), None);
        let _demo = ProfileOverride::set(Some("demo"));
        assert_eq!(active(), Some("demo".to_string()));
        assert_eq!(
            root_under(PathBuf::from("/base")),
            PathBuf::from("/base/profiles/demo")
        );
    }

    #[test]
    #[should_panic(expected = "MAILYPOPPINS_PROFILE: `../x` is not a profile name")]
    fn an_invalid_variable_reaching_a_resolver_panics() {
        let _bad = crate::config::test_env::ProfileOverride::set(Some("../x"));
        active();
    }
}
