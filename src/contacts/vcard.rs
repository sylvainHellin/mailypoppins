//! Minimal `Contact` -> vCard 3.0 serializer (#0033).
//!
//! The contact index is derived from local mail, so a contact only ever holds
//! a display name and an email address (plus ranking metadata). We emit a
//! spec-conformant vCard 3.0 with just the fields we can populate: `FN`, `N`,
//! and `EMAIL`. vCard 3.0 (RFC 2426) is the interoperable baseline every
//! client — Apple Contacts, Google, Thunderbird, Outlook — imports cleanly.
//!
//! CRLF line endings and text escaping follow RFC 2426 §2.4 so the output is a
//! valid `.vcf` file that round-trips through mainstream address books.

use crate::contacts::types::Contact;

/// Escape a vCard text value per RFC 2426 §5: backslash, comma, semicolon, and
/// newlines are escaped. (We do not fold long lines: contact names/addresses
/// are short and unfolded output is universally accepted.)
fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            ',' => out.push_str("\\,"),
            ';' => out.push_str("\\;"),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            _ => out.push(ch),
        }
    }
    out
}

/// The formatted-name value: the display name if present, else the address.
fn formatted_name(contact: &Contact) -> &str {
    if contact.display_name.trim().is_empty() {
        &contact.address
    } else {
        &contact.display_name
    }
}

/// Split a display name into the vCard structured `N` value
/// (`Family;Given;Additional;Prefix;Suffix`). We only fill Family + Given from
/// a naive "Given Family" split; extra tokens fold into Given. When there is no
/// name we leave the structured value empty (a single trailing surname slot).
fn structured_name(contact: &Contact) -> String {
    let name = contact.display_name.trim();
    if name.is_empty() {
        // No name known: empty structured value (all five components blank).
        return ";;;;".to_string();
    }
    match name.rsplit_once(' ') {
        Some((given, family)) => {
            format!("{};{};;;", escape(family.trim()), escape(given.trim()))
        }
        // Single token: treat as the given name, empty family.
        None => format!(";{};;;", escape(name)),
    }
}

/// Serialize a single [`Contact`] to a vCard 3.0 document (`.vcf` contents).
///
/// Emits `FN`, `N`, and `EMAIL;TYPE=INTERNET`. Uses CRLF line endings per
/// RFC 2426. The result is a complete, standalone vCard.
pub fn contact_to_vcard(contact: &Contact) -> String {
    let mut out = String::new();
    out.push_str("BEGIN:VCARD\r\n");
    out.push_str("VERSION:3.0\r\n");
    out.push_str(&format!("FN:{}\r\n", escape(formatted_name(contact))));
    out.push_str(&format!("N:{}\r\n", structured_name(contact)));
    out.push_str(&format!(
        "EMAIL;TYPE=INTERNET:{}\r\n",
        escape(&contact.address)
    ));
    out.push_str("END:VCARD\r\n");
    out
}

/// A filesystem-safe base filename (without extension) for a contact's `.vcf`.
/// Derived from the display name (or the address local-part), lowercased, with
/// non-alphanumeric runs collapsed to single hyphens.
pub fn vcard_file_stem(contact: &Contact) -> String {
    let source = if contact.display_name.trim().is_empty() {
        contact
            .address
            .split('@')
            .next()
            .unwrap_or(&contact.address)
    } else {
        contact.display_name.trim()
    };
    let mut stem = String::new();
    let mut prev_dash = false;
    for ch in source.chars() {
        if ch.is_ascii_alphanumeric() {
            stem.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            stem.push('-');
            prev_dash = true;
        }
    }
    let stem = stem.trim_matches('-').to_string();
    if stem.is_empty() {
        "contact".to_string()
    } else {
        stem
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contacts::types::{Contact, ContactSource};

    fn mk(addr: &str, name: &str) -> Contact {
        Contact {
            address: addr.into(),
            display_name: name.into(),
            sent_to: 0,
            sent_cc: 0,
            received: 0,
            first_seen: "2026-01-01T00:00:00Z".into(),
            last_seen: "2026-01-01T00:00:00Z".into(),
            source: ContactSource::Local,
        }
    }

    #[test]
    fn full_name_contact_serializes_fn_n_email() {
        let vcf = contact_to_vcard(&mk("alice@example.com", "Alice Smith"));
        assert_eq!(
            vcf,
            "BEGIN:VCARD\r\n\
             VERSION:3.0\r\n\
             FN:Alice Smith\r\n\
             N:Smith;Alice;;;\r\n\
             EMAIL;TYPE=INTERNET:alice@example.com\r\n\
             END:VCARD\r\n"
        );
    }

    #[test]
    fn nameless_contact_falls_back_to_address_for_fn() {
        let vcf = contact_to_vcard(&mk("bob@example.com", ""));
        assert!(vcf.contains("FN:bob@example.com\r\n"));
        // Structured name is empty (all components blank).
        assert!(vcf.contains("N:;;;;\r\n"));
        assert!(vcf.contains("EMAIL;TYPE=INTERNET:bob@example.com\r\n"));
    }

    #[test]
    fn single_token_name_goes_to_given() {
        let vcf = contact_to_vcard(&mk("carol@example.com", "Carol"));
        assert!(vcf.contains("FN:Carol\r\n"));
        assert!(vcf.contains("N:;Carol;;;\r\n"));
    }

    #[test]
    fn multi_token_name_folds_extra_into_given() {
        let vcf = contact_to_vcard(&mk("d@example.com", "Jean Luc Picard"));
        // rsplit at last space: family = "Picard", given = "Jean Luc".
        assert!(vcf.contains("N:Picard;Jean Luc;;;\r\n"));
    }

    #[test]
    fn special_characters_are_escaped() {
        let vcf = contact_to_vcard(&mk("x@example.com", "Doe, John; Jr\\"));
        assert!(vcf.contains("FN:Doe\\, John\\; Jr\\\\\r\n"));
    }

    #[test]
    fn every_vcard_is_well_formed() {
        let vcf = contact_to_vcard(&mk("a@b.com", "A B"));
        assert!(vcf.starts_with("BEGIN:VCARD\r\n"));
        assert!(vcf.ends_with("END:VCARD\r\n"));
        assert!(vcf.contains("VERSION:3.0\r\n"));
        // Exactly one of each mandatory property.
        assert_eq!(vcf.matches("BEGIN:VCARD").count(), 1);
        assert_eq!(vcf.matches("END:VCARD").count(), 1);
        assert_eq!(vcf.matches("VERSION:3.0").count(), 1);
    }

    #[test]
    fn file_stem_slugifies() {
        assert_eq!(vcard_file_stem(&mk("a@b.com", "Alice Smith")), "alice-smith");
        assert_eq!(vcard_file_stem(&mk("bob.jones@x.com", "")), "bob-jones");
        assert_eq!(vcard_file_stem(&mk("@@@@@@", "")), "contact");
    }
}
