//! Noise-filter for the contact extractor.
//!
//! Hardcoded compile-time patterns reject common non-human senders
//! (no-reply, mailer-daemon, bulk/mailing-list domains). Not user-tunable
//! in v1 — tunable blocklist is a Tier B follow-up in BACKLOG.md.

use regex::Regex;
use std::sync::LazyLock;

/// Local-part prefixes that indicate a non-human sender.
///
/// Each regex anchors at the start of the local-part and terminates at a
/// word-boundary separator (`-`, `_`, `.`, `+`, `@`) so variants like
/// `no-reply-abc123@slack.com` and `noreply+token@host.com` are caught too.
static NOREPLY_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        // no-reply, noreply, no.reply, no_reply, donotreply, do-not-reply,
        // optionally followed by any local-part suffix (random tokens, etc.).
        Regex::new(r"(?i)^(no[-_.]?reply|donotreply|do[-_.]?not[-_.]?reply)([-_.+@]|$)").unwrap(),
        // mailer-daemon, postmaster, bounce-, bounces-
        Regex::new(r"(?i)^(mailer-daemon|postmaster|bounces?)([-_.+@]|$)").unwrap(),
        // notifications@, notification@, alerts@, alert@, news@, newsletter@
        Regex::new(r"(?i)^(notifications?|alerts?|news(letter)?|updates?)([-_.+@]|$)").unwrap(),
    ]
});

/// Domain prefixes/suffixes that indicate a bulk sender or mailing list.
static BULK_DOMAIN_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        Regex::new(r"(?i)@bounces?\.").unwrap(),
        Regex::new(r"(?i)@lists?\.").unwrap(),
        Regex::new(r"(?i)@em(ail)?\.").unwrap(),
        Regex::new(r"(?i)@mail(er|ing)?\.").unwrap(),
    ]
});

/// Returns `true` if `addr` (lowercased) should be kept.
pub fn is_usable_address(addr: &str) -> bool {
    if addr.len() < 3 || !addr.contains('@') {
        return false;
    }
    // Real email addresses never contain whitespace; any ws means the upstream
    // header-parser gave us garbage.
    if addr.chars().any(|c| c.is_whitespace()) {
        return false;
    }
    // Must start with an alphanumeric local-part character. Rejects malformed
    // fragments like ", user@host" or "<user@host".
    if !addr
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric())
    {
        return false;
    }
    // Must have a dot in the domain part (rejects `user@host` bare hostnames).
    let Some((_, domain)) = addr.split_once('@') else {
        return false;
    };
    if !domain.contains('.') {
        return false;
    }
    for pat in NOREPLY_PATTERNS.iter() {
        if pat.is_match(addr) {
            return false;
        }
    }
    for pat in BULK_DOMAIN_PATTERNS.iter() {
        if pat.is_match(addr) {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_noreply_variants() {
        assert!(!is_usable_address("no-reply@example.com"));
        assert!(!is_usable_address("noreply@example.com"));
        assert!(!is_usable_address("no.reply@example.com"));
        assert!(!is_usable_address("donotreply@example.com"));
        assert!(!is_usable_address("do-not-reply@example.com"));
        // Variants with suffixes (random tokens, campaign ids, etc.)
        assert!(!is_usable_address("no-reply-abc123@slack.com"));
        assert!(!is_usable_address("noreply+token@example.com"));
        assert!(!is_usable_address("no-reply.campaign@example.com"));
    }

    #[test]
    fn rejects_system_senders() {
        assert!(!is_usable_address("mailer-daemon@example.com"));
        assert!(!is_usable_address("postmaster@example.com"));
        assert!(!is_usable_address("bounces@example.com"));
    }

    #[test]
    fn rejects_bulk_domains() {
        assert!(!is_usable_address("user@lists.example.com"));
        assert!(!is_usable_address("user@bounces.example.com"));
        assert!(!is_usable_address("user@em.example.com"));
    }

    #[test]
    fn accepts_real_addresses() {
        assert!(is_usable_address("john.smith@example.com"));
        assert!(is_usable_address("müller@uni-münchen.de"));
    }

    #[test]
    fn rejects_malformed_fragments() {
        assert!(!is_usable_address(", user@example.com"));
        assert!(!is_usable_address("<user@example.com"));
        assert!(!is_usable_address("user @example.com"));
        assert!(!is_usable_address("user@host"));
        assert!(!is_usable_address(""));
        assert!(!is_usable_address("@"));
    }
}
