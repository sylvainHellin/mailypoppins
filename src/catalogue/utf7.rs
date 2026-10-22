//! IMAP's modified UTF-7 (RFC 3501, section 5.1.3), decoded for display.
//!
//! A server spells `Gelöscht` as `Gel&APY-scht` on the wire, and that spelling
//! is what `SELECT` takes and what the store keys the folder by; only the label
//! a person reads is decoded. A malformed run is shown as the server sent it,
//! since a label is not the place to refuse a folder. Only IMAP names are
//! decoded: a Graph display name is plain text, where `Q&A-2024` means what it
//! says.

/// The display form of a modified UTF-7 mailbox name.
pub fn decode(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut rest = name;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('-') else {
            out.push_str(&rest[start..]);
            return out;
        };
        let run = &after[..end];
        if run.is_empty() {
            out.push('&');
        } else {
            match decode_run(run) {
                Some(text) => out.push_str(&text),
                None => out.push_str(&rest[start..start + end + 2]),
            }
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

/// One `&...-` run: modified base64 (`,` for `/`, no padding) of UTF-16BE.
///
/// `None` for a run that is not one: a character outside the alphabet, a run
/// too short to yield a byte (`Q&A-2024`), an odd byte count, or set bits left
/// over after the last whole byte, which a real encoder pads with zeros.
fn decode_run(run: &str) -> Option<String> {
    let mut bits: u32 = 0;
    let mut nbits = 0;
    let mut bytes = Vec::with_capacity(run.len());
    for c in run.chars() {
        let value = match c {
            'A'..='Z' => c as u32 - 'A' as u32,
            'a'..='z' => c as u32 - 'a' as u32 + 26,
            '0'..='9' => c as u32 - '0' as u32 + 52,
            '+' => 62,
            ',' => 63,
            _ => return None,
        };
        bits = (bits << 6) | value;
        nbits += 6;
        if nbits >= 8 {
            nbits -= 8;
            bytes.push((bits >> nbits) as u8);
            bits &= (1 << nbits) - 1;
        }
    }
    if bytes.is_empty() || bytes.len() % 2 != 0 || bits != 0 {
        return None;
    }
    let units: Vec<u16> = bytes
        .chunks(2)
        .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
        .collect();
    String::from_utf16(&units).ok()
}

#[cfg(test)]
mod tests {
    use super::decode;

    #[test]
    fn modified_utf7_names_decode_for_display() {
        assert_eq!(decode("Gel&APY-scht"), "Gelöscht");
        assert_eq!(decode("Entw&APw-rfe"), "Entwürfe");
        assert_eq!(decode("&ZeVnLIqe-"), "日本語");
        assert_eq!(decode("Tom &- Jerry"), "Tom & Jerry");
        assert_eq!(decode("INBOX/reimbursements"), "INBOX/reimbursements");
    }

    #[test]
    fn a_malformed_run_is_shown_as_sent() {
        assert_eq!(decode("bad&!!-name"), "bad&!!-name");
        assert_eq!(decode("open&APY"), "open&APY");
    }

    /// #0141 review: a run that yields no byte, or leaves set bits over, is
    /// not modified UTF-7, so the name reads as the server sent it.
    #[test]
    fn a_run_without_a_whole_character_is_shown_as_sent() {
        assert_eq!(decode("Q&A-2024"), "Q&A-2024");
        assert_eq!(decode("R&D-Team"), "R&D-Team");
        assert_eq!(decode("x&APZ-y"), "x&APZ-y", "leftover bits are not zero");
    }
}
