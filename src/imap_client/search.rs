/// Structured search criteria for IMAP queries.
pub struct FetchCriteria {
    pub from: Option<String>,
    pub to: Option<String>,
    pub cc: Option<String>,
    pub subject: Option<String>,
    pub body: Option<String>,
    pub since: Option<String>,
    pub before: Option<String>,
    pub text: Option<String>,
    /// Routing directive: which mailbox to search. Not an IMAP search criterion.
    pub in_mailbox: Option<String>,
}

pub(crate) fn build_imap_search_query(criteria: &FetchCriteria) -> String {
    let mut parts: Vec<String> = Vec::new();

    if let Some(ref from) = criteria.from {
        parts.push(format!("FROM \"{}\"", from));
    }
    if let Some(ref to) = criteria.to {
        parts.push(format!("TO \"{}\"", to));
    }
    if let Some(ref cc) = criteria.cc {
        parts.push(format!("CC \"{}\"", cc));
    }
    if let Some(ref subject) = criteria.subject {
        parts.push(format!("SUBJECT \"{}\"", subject));
    }
    if let Some(ref body) = criteria.body {
        parts.push(format!("BODY \"{}\"", body));
    }
    if let Some(ref since) = criteria.since {
        if let Some(imap_date) = parse_date_to_imap(since) {
            parts.push(format!("SINCE {}", imap_date));
        }
    }
    if let Some(ref before) = criteria.before {
        if let Some(imap_date) = parse_date_to_imap(before) {
            parts.push(format!("BEFORE {}", imap_date));
        }
    }

    if let Some(ref text) = criteria.text {
        parts.push(format!("TEXT \"{}\"", text));
    }

    if parts.is_empty() {
        "ALL".to_string()
    } else {
        parts.join(" ")
    }
}

/// Parse a user search string into structured FetchCriteria.
///
/// Recognized prefixes: from:, to:, cc:, subject:, body:, since:, before:
/// Quoted values supported: from:"John Doe"
/// Bare text (no prefix) becomes a TEXT search.
pub fn parse_search_query(input: &str) -> FetchCriteria {
    let mut criteria = FetchCriteria {
        from: None,
        to: None,
        cc: None,
        subject: None,
        body: None,
        since: None,
        before: None,
        text: None,
        in_mailbox: None,
    };

    let mut remaining = Vec::new();
    let mut chars = input.chars().peekable();

    while chars.peek().is_some() {
        while chars.peek() == Some(&' ') {
            chars.next();
        }
        if chars.peek().is_none() {
            break;
        }

        let rest: String = chars.clone().collect();
        let lower_rest = rest.to_lowercase();

        let mut matched = false;
        for (prefix, setter) in [
            ("from:", 0u8),
            ("to:", 1),
            ("cc:", 2),
            ("subject:", 3),
            ("body:", 4),
            ("since:", 5),
            ("before:", 6),
            ("in:", 7),
        ] {
            if lower_rest.starts_with(prefix) {
                for _ in 0..prefix.len() {
                    chars.next();
                }
                let value = extract_search_value(&mut chars);
                match setter {
                    0 => criteria.from = Some(value),
                    1 => criteria.to = Some(value),
                    2 => criteria.cc = Some(value),
                    3 => criteria.subject = Some(value),
                    4 => criteria.body = Some(value),
                    5 => criteria.since = Some(value),
                    6 => criteria.before = Some(value),
                    7 => criteria.in_mailbox = Some(value),
                    _ => unreachable!(),
                }
                matched = true;
                break;
            }
        }

        if !matched {
            let mut word = String::new();
            while let Some(&c) = chars.peek() {
                if c == ' ' {
                    break;
                }
                word.push(c);
                chars.next();
            }
            if !word.is_empty() {
                remaining.push(word);
            }
        }
    }

    if !remaining.is_empty() {
        criteria.text = Some(remaining.join(" "));
    }

    criteria
}

fn extract_search_value(chars: &mut std::iter::Peekable<std::str::Chars>) -> String {
    while chars.peek() == Some(&' ') {
        chars.next();
    }

    if chars.peek() == Some(&'"') {
        chars.next();
        let mut value = String::new();
        for c in chars.by_ref() {
            if c == '"' {
                break;
            }
            value.push(c);
        }
        value
    } else {
        let mut value = String::new();
        while let Some(&c) = chars.peek() {
            if c == ' ' {
                break;
            }
            value.push(c);
            chars.next();
        }
        value
    }
}

pub(crate) fn parse_date_to_imap(date_str: &str) -> Option<String> {
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
