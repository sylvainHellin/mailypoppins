//! The three IMAP string helpers the search grammar shares with the IMAP
//! client (#0126, P5-U10a).
//!
//! `search` parses one query language and renders it four ways, and two of
//! those renderings need a Message-ID in the exact form a server stores it and
//! a date in the `D-Mon-YYYY` form `SINCE` / `BEFORE` take. All three are pure
//! string functions with no session behind them; they lived in
//! `imap_client::search` only because that is where the first caller was, and
//! that module re-exports them from here so every existing path still
//! resolves.

/// Normalize a Message-ID for comparison: trim whitespace and strip one layer of
/// angle brackets, so `<a@b>` and `a@b` compare equal. Idempotent.
pub fn normalize_message_id(raw: &str) -> &str {
    let trimmed = raw.trim();
    trimmed
        .strip_prefix('<')
        .and_then(|s| s.strip_suffix('>'))
        .unwrap_or(trimmed)
}

/// Canonical wire form of a Message-ID, always angle-bracketed: `<a@b>`.
/// Servers store the header with the brackets, so queries must carry them.
pub fn bracketed_message_id(raw: &str) -> String {
    format!("<{}>", normalize_message_id(raw))
}

/// Parse a `YYYY-MM-DD` date into the IMAP `D-Mon-YYYY` form.
///
/// The shared date lowering: the IMAP renderer of [`crate::search`] and the
/// structured `imap_client::search::FetchCriteria` path both use it, which is
/// why it is here rather than in either of them.
pub fn parse_date_to_imap(date_str: &str) -> Option<String> {
    let parts: Vec<&str> = date_str.split('-').collect();
    if parts.len() != 3 {
        return None;
    }
    let year = parts[0];
    let month = match parts[1] {
        "01" => "Jan",
        "02" => "Feb",
        "03" => "Mar",
        "04" => "Apr",
        "05" => "May",
        "06" => "Jun",
        "07" => "Jul",
        "08" => "Aug",
        "09" => "Sep",
        "10" => "Oct",
        "11" => "Nov",
        "12" => "Dec",
        _ => return None,
    };
    let day: u32 = parts[2].parse().ok()?;
    Some(format!("{}-{}-{}", day, month, year))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_date_to_imap_valid() {
        assert_eq!(
            parse_date_to_imap("2024-12-25"),
            Some("25-Dec-2024".to_string())
        );
        assert_eq!(
            parse_date_to_imap("2026-01-01"),
            Some("1-Jan-2026".to_string())
        );
        assert_eq!(
            parse_date_to_imap("2023-06-15"),
            Some("15-Jun-2023".to_string())
        );
    }

    #[test]
    fn test_parse_date_to_imap_invalid_format() {
        assert_eq!(parse_date_to_imap("2024/12/25"), None);
        assert_eq!(parse_date_to_imap("Dec 25 2024"), None);
        assert_eq!(parse_date_to_imap("25-Dec-2024"), None);
        assert_eq!(parse_date_to_imap("2024-1-5"), None);
    }

    #[test]
    fn test_parse_date_to_imap_invalid_month() {
        assert_eq!(parse_date_to_imap("2024-13-01"), None);
        assert_eq!(parse_date_to_imap("2024-00-01"), None);
        assert_eq!(parse_date_to_imap("2024-99-01"), None);
    }

    #[test]
    fn test_parse_date_to_imap_invalid_day() {
        // parse_date_to_imap only validates that day is a valid u32, not range.
        // Invalid days will be rejected by the IMAP server at query time.
        assert_eq!(parse_date_to_imap("2024-12-00"), Some("0-Dec-2024".to_string()));
        assert_eq!(parse_date_to_imap("2024-12-32"), Some("32-Dec-2024".to_string()));
        assert_eq!(parse_date_to_imap("2024-12-ab"), None);
    }

    #[test]
    fn test_parse_date_to_imap_empty() {
        assert_eq!(parse_date_to_imap(""), None);
        assert_eq!(parse_date_to_imap("2024"), None);
        assert_eq!(parse_date_to_imap("2024-12"), None);
    }

    #[test]
    fn test_normalize_message_id_strips_one_bracket_layer() {
        assert_eq!(normalize_message_id("<abc@example.com>"), "abc@example.com");
        assert_eq!(normalize_message_id("abc@example.com"), "abc@example.com");
        assert_eq!(normalize_message_id("  <abc@example.com> "), "abc@example.com");
        // Half-bracketed input is left alone rather than silently mangled.
        assert_eq!(normalize_message_id("<abc@example.com"), "<abc@example.com");
    }

    #[test]
    fn test_bracketed_message_id_is_idempotent() {
        assert_eq!(bracketed_message_id("abc@example.com"), "<abc@example.com>");
        assert_eq!(
            bracketed_message_id("<abc@example.com>"),
            "<abc@example.com>"
        );
    }
}
