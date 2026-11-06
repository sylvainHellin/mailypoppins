//! What the blocked-senders list takes (PERSO-261): the entry the daemon's
//! `sender.block` writes, and the entries it refuses, read the same way by the
//! daemon, which refuses them, and by a client, which does not offer them.
//!
//! An entry is a full address or `@domain` ([`normalize`]). An entry that
//! names one of the user's own addresses, or the domain of one, is refused
//! too ([`own_mail`]): blocking `@company.com` from a message in Sent would
//! move every colleague's mail and the user's own to Junk.

use crate::config::AccountConfig;

/// An entry as the list holds it, or the sentence that refuses it: a full
/// address or `@domain`, trimmed and lowercased, one `@`, no space, no angle
/// bracket, and a domain with a dot in it.
pub fn normalize(raw: &str) -> Result<String, String> {
    let entry = raw.trim().to_lowercase();
    let refuse = |why: &str| {
        Err(format!(
            "'{}' is not a sender to block: {why}; give a full address (spam@example.com) or a domain (@example.com)",
            raw.trim()
        ))
    };
    if entry.is_empty() {
        return refuse("it is empty");
    }
    if entry
        .chars()
        .any(|c| c.is_whitespace() || matches!(c, '<' | '>' | ',' | ';' | '"'))
    {
        return refuse("it holds a space or a character no address has");
    }
    let Some((_, domain)) = entry.split_once('@') else {
        return refuse("it has no @");
    };
    if domain.contains('@') {
        return refuse("it has more than one @");
    }
    if domain.is_empty()
        || !domain.contains('.')
        || domain.starts_with('.')
        || domain.ends_with('.')
    {
        return refuse("its domain is not one");
    }
    Ok(entry)
}

/// The user's own addresses: each account's
/// [`own_address`](AccountConfig::own_address), as a sender is compared
/// ([`crate::parse::comparable_address`]); an account with no `default_from`
/// owns none.
pub fn own_addresses<'a>(accounts: impl IntoIterator<Item = &'a AccountConfig>) -> Vec<String> {
    let mut own: Vec<String> = accounts
        .into_iter()
        .map(|account| crate::parse::comparable_address(&account.own_address()))
        .filter(|address| address.contains('@'))
        .collect();
    own.sort();
    own.dedup();
    own
}

/// The own address of `own` that `entry`, already [`normalize`]d, would
/// block: the address itself, or one at the domain `entry` names; `None`
/// when it blocks none of them.
pub fn own_mail<'a>(entry: &str, own: &'a [String]) -> Option<&'a str> {
    own.iter()
        .map(String::as_str)
        .find(|address| match entry.strip_prefix('@') {
            Some(domain) => address
                .rsplit_once('@')
                .is_some_and(|(_, own_domain)| own_domain == domain),
            None => *address == entry,
        })
}

/// [`normalize`], and the sentence that refuses an entry blocking the user's
/// own mail ([`own_mail`]): `sender.block`'s whole check.
pub fn checked(raw: &str, own: &[String]) -> Result<String, String> {
    let entry = normalize(raw)?;
    match own_mail(&entry, own) {
        None => Ok(entry),
        Some(address) if entry.starts_with('@') => Err(format!(
            "'{entry}' is the domain of your own address {address}; blocking it would move your own mail to Junk"
        )),
        Some(_) => Err(format!(
            "'{entry}' is one of your own addresses; blocking it would move your own mail to Junk"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(default_from: &str) -> AccountConfig {
        AccountConfig {
            default_from: default_from.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn an_entry_is_an_address_or_a_domain_lowercased() {
        assert_eq!(normalize(" Spam@X.com ").unwrap(), "spam@x.com");
        assert_eq!(normalize("@X.com").unwrap(), "@x.com");
        for bad in [
            "",
            "x.com",
            "a@b@c.com",
            "a@",
            "@com",
            "<a@b.com>",
            "a b@c.com",
            "\"john doe\"@example.com",
        ] {
            assert!(normalize(bad).is_err(), "{bad:?} is refused");
        }
    }

    /// Each account's `default_from`, its address alone, lowercase; an
    /// account with none owns nothing.
    #[test]
    fn the_own_addresses_are_each_accounts_default_from() {
        let accounts = [
            account("Me <Me@Company.com>"),
            account("me@company.com"),
            account("private@mail.example"),
            account(""),
        ];
        assert_eq!(
            own_addresses(&accounts),
            ["me@company.com", "private@mail.example"]
        );
    }

    /// An own address and the domain of one are refused with a sentence that
    /// says why; a colleague's address at that domain, a subdomain and any
    /// other sender are taken.
    #[test]
    fn an_entry_blocking_the_users_own_mail_is_refused() {
        let own = vec!["me@company.com".to_string()];
        assert_eq!(
            checked("Me@Company.com", &own),
            Err("'me@company.com' is one of your own addresses; blocking it would move your own mail to Junk".to_string())
        );
        assert_eq!(
            checked("@COMPANY.com", &own),
            Err("'@company.com' is the domain of your own address me@company.com; blocking it would move your own mail to Junk".to_string())
        );
        for taken in ["colleague@company.com", "@sub.company.com", "me@other.com"] {
            assert_eq!(checked(taken, &own).as_deref(), Ok(taken), "{taken}");
        }
        assert!(checked("company.com", &own)
            .unwrap_err()
            .contains("give a full address"));
        assert_eq!(checked("@company.com", &[]).as_deref(), Ok("@company.com"));
    }
}
