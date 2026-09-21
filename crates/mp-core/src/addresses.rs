//! RFC 5322 address-string handling: the split, the quoting and the two
//! formatters, none of which needs a transport (#0126, P5-U10b).
//!
//! These lived in `send` because that is where the first caller was, exactly
//! as the three IMAP string helpers of [`crate::imap_query`] did. They are
//! pure string functions, and `draft`'s validation and its reply/forward
//! builders read them without wanting a single line of SMTP, so they moved
//! here rather than being duplicated. `send` re-exports all three, so
//! `crate::send::split_addresses` and every other old path resolves unchanged.

/// Normalize a single address so lettre's strict RFC 5322 `Mailbox` parser
/// accepts it.
///
/// Many real-world senders ship `Display Name <user@host>` headers where the
/// display name contains characters that are not RFC 5322 `atext` (e.g.
/// `[`, `]`, `:`, `;`, `(`, `)`, `,`). The fix is to wrap such display names
/// in a quoted-string. We only touch the display name; the address part is
/// left as-is.
///
/// Examples:
/// - `CCBE_Researchers [TUBVCMS] <r@x>` → `"CCBE_Researchers [TUBVCMS]" <r@x>`
/// - `"Doe, Jane" <j@x>` → unchanged (already quoted)
/// - `Alice <a@x>` → unchanged (atext-only display name)
/// - `bob@x.com` → unchanged (no display name)
pub fn normalize_address_for_smtp(addr: &str) -> String {
    let trimmed = addr.trim();
    let (open, close) = match (trimmed.rfind('<'), trimmed.rfind('>')) {
        (Some(o), Some(c)) if o < c => (o, c),
        _ => return trimmed.to_string(),
    };

    let name_part = trimmed[..open].trim();
    let email_part = trimmed[open + 1..close].trim();

    if name_part.is_empty() {
        return format!("<{}>", email_part);
    }

    // Already a single quoted-string spanning the whole display name -- keep.
    if name_part.len() >= 2 && name_part.starts_with('"') && name_part.ends_with('"') {
        return format!("{} <{}>", name_part, email_part);
    }

    format!("{} <{}>", quote_display_name(name_part), email_part)
}

/// Return an RFC 5322 `display-name` for `name`: the bare name if it is made
/// entirely of atext + FWS, otherwise wrapped in a quoted-string with `"` and
/// `\` escaped. Shared by [`normalize_address_for_smtp`] and
/// [`format_recipient`] so the quoting rule lives in exactly one place.
fn quote_display_name(name: &str) -> String {
    // RFC 5322 atext, plus FWS (space/tab) and `.` (allowed in dot-atom phrases).
    fn is_atext_or_fws(c: char) -> bool {
        c.is_ascii_alphanumeric()
            || matches!(
                c,
                '!' | '#'
                    | '$'
                    | '%'
                    | '&'
                    | '\''
                    | '*'
                    | '+'
                    | '-'
                    | '/'
                    | '='
                    | '?'
                    | '^'
                    | '_'
                    | '`'
                    | '{'
                    | '|'
                    | '}'
                    | '~'
                    | '.'
                    | ' '
                    | '\t'
            )
    }

    if name.chars().all(is_atext_or_fws) {
        return name.to_string();
    }

    // Quote it. Escape backslashes and double quotes per RFC 5322 quoted-string.
    let escaped = name.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{}\"", escaped)
}

/// Build a single RFC 5322 recipient string from a display name and address.
///
/// Emits `Name <addr>` with the display name wrapped in a quoted-string when it
/// contains characters outside atext + FWS (e.g. the `,` in a `"Last, First"`
/// contact name), so the result survives [`split_addresses`] as ONE recipient
/// and parses cleanly. When `display_name` is empty (after trimming) the bare
/// address is returned.
pub fn format_recipient(display_name: &str, address: &str) -> String {
    let name = display_name.trim();
    if name.is_empty() {
        return address.to_string();
    }
    format!("{} <{}>", quote_display_name(name), address)
}

/// Split a comma-separated address list respecting quoted display names.
/// e.g. `"Doe, Jane" <jane@x.com>, bob@x.com` → two entries, not three.
pub fn split_addresses(s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;

    for ch in s.chars() {
        match ch {
            '"' => {
                in_quotes = !in_quotes;
                current.push(ch);
            }
            ',' if !in_quotes => {
                let trimmed = current.trim().to_string();
                if !trimmed.is_empty() {
                    parts.push(trimmed);
                }
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    let trimmed = current.trim().to_string();
    if !trimmed.is_empty() {
        parts.push(trimmed);
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // split_addresses
    // -----------------------------------------------------------------------

    #[test]
    fn split_addresses_simple() {
        let r = split_addresses("alice@x.com, bob@x.com");
        assert_eq!(r, vec!["alice@x.com", "bob@x.com"]);
    }

    #[test]
    fn split_addresses_quoted_comma_in_display_name() {
        let r = split_addresses("\"Doe, Jane\" <jane@example.com>, bob@x.com");
        assert_eq!(r, vec!["\"Doe, Jane\" <jane@example.com>", "bob@x.com"]);
    }

    #[test]
    fn split_addresses_single_quoted_name() {
        let r = split_addresses("\"Doe, Jane\" <jane@x.com>");
        assert_eq!(r, vec!["\"Doe, Jane\" <jane@x.com>"]);
    }

    #[test]
    fn split_addresses_empty() {
        let r = split_addresses("");
        assert!(r.is_empty());
    }

    #[test]
    fn split_addresses_whitespace_only() {
        let r = split_addresses("   ");
        assert!(r.is_empty());
    }

    // -----------------------------------------------------------------------
    // normalize_address_for_smtp
    // -----------------------------------------------------------------------

    #[test]
    fn normalize_leaves_already_quoted_name_untouched() {
        let raw = "\"Doe, Jane\" <jane@x.com>";
        assert_eq!(normalize_address_for_smtp(raw), raw);
    }

    #[test]
    fn normalize_leaves_atext_only_name_untouched() {
        let raw = "Alice Smith <alice@x.com>";
        assert_eq!(normalize_address_for_smtp(raw), raw);
    }

    #[test]
    fn normalize_leaves_bare_address_untouched() {
        assert_eq!(normalize_address_for_smtp("bob@x.com"), "bob@x.com");
    }

    // -----------------------------------------------------------------------
    // format_recipient (Contacts view seed sites, #0033)
    // -----------------------------------------------------------------------

    #[test]
    fn format_recipient_plain_name_passthrough() {
        assert_eq!(
            format_recipient("Alice Smith", "alice@x.com"),
            "Alice Smith <alice@x.com>"
        );
    }

    #[test]
    fn format_recipient_quotes_comma_name() {
        assert_eq!(
            format_recipient("Doe, John", "john@x.com"),
            "\"Doe, John\" <john@x.com>"
        );
    }

    #[test]
    fn format_recipient_escapes_quote_and_backslash() {
        assert_eq!(
            format_recipient("Weird \\ \"name\"", "w@x.com"),
            "\"Weird \\\\ \\\"name\\\"\" <w@x.com>"
        );
    }

    #[test]
    fn format_recipient_empty_name_is_bare_address() {
        assert_eq!(format_recipient("", "bob@x.com"), "bob@x.com");
        // Whitespace-only display name is treated as empty.
        assert_eq!(format_recipient("   ", "bob@x.com"), "bob@x.com");
    }
}
