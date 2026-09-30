//! Deciding whether a stored message passes a hook's `match` table (#0135).
//!
//! # The sender check is the one that matters
//!
//! A hook may run a command with the user's full rights, so `From:` alone
//! proves nothing: anyone can write any address there. What a message cannot
//! forge is the verdict the *receiving* server reached when it accepted the
//! message, which that server records in an `Authentication-Results` header
//! (RFC 8601) it prepends above everything the message arrived with. So the
//! check reads exactly one such header, the topmost, and only when it carries
//! the authserv-id the hook names (`mx.google.com` for Gmail):
//!
//! - A header the sender forged sits below the receiving server's own, so it is
//!   never the topmost; one that claims the trusted authserv-id is ignored for
//!   the same reason.
//! - A header an intermediate hop added (a forwarder's own verdict) sits below
//!   the receiving server's too; if it is the topmost, the receiving server
//!   stamped nothing, and the check fails rather than trust a hop it does not
//!   know.
//! - The verdict must be a DKIM pass whose signing domain (`header.d`, or the
//!   domain of `header.i`) *is* the `From:` domain, or an SPF pass whose
//!   `smtp.mailfrom` domain is. Exact equality, not DMARC's relaxed alignment:
//!   relaxed alignment would let any subdomain's mail server (a department's
//!   `xyz.tum.de`) vouch for an address at the parent (`tum.de`).
//! - `From:` must carry exactly one mailbox, and appear exactly once, so the
//!   address checked is unambiguously the one a mail client shows.
//!
//! What this cannot prove is the local part: a pass for `tum.de` means a
//! `tum.de` server sent it, and the check trusts that server not to let one of
//! its users send as another. That is the model DMARC itself rests on.
//!
//! This relies on the receiving server stamping every message it accepts. Gmail
//! does; a server that does not must not be named as `authserv_id`, because a
//! message that reached it would then carry only the headers its sender wrote.

use crate::config::HookMatch;

/// One criterion's outcome, worded for `mp hooks test` and the daemon log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Check {
    /// `authenticated_from`, `to`, `subject` or `headers.<name>`.
    pub criterion: String,
    pub passed: bool,
    pub detail: String,
}

/// Every criterion the hook sets, checked against one message.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Verdict {
    pub checks: Vec<Check>,
    /// The `From:` address once the sender check passed it, lowercased.
    pub authenticated_sender: Option<String>,
}

impl Verdict {
    /// True when every criterion passed. A verdict with no checks cannot
    /// happen for a validated hook, and does not match.
    pub fn matched(&self) -> bool {
        !self.checks.is_empty() && self.checks.iter().all(|check| check.passed)
    }

    /// The first criterion that failed, for a one-line log.
    pub fn first_failure(&self) -> Option<&Check> {
        self.checks.iter().find(|check| !check.passed)
    }

    fn push(&mut self, criterion: impl Into<String>, result: Result<String, String>) {
        let (passed, detail) = match result {
            Ok(detail) => (true, detail),
            Err(detail) => (false, detail),
        };
        self.checks.push(Check {
            criterion: criterion.into(),
            passed,
            detail,
        });
    }
}

/// Check `criteria` against a parsed message.
pub fn evaluate(criteria: &HookMatch, parsed: &mailparse::ParsedMail) -> Verdict {
    let headers = &parsed.headers;
    let mut verdict = Verdict::default();

    if !criteria.authenticated_from.is_empty() {
        let trusted = criteria.authserv_id.as_deref().unwrap_or_default();
        let result = sender_check(headers, &criteria.authenticated_from, trusted);
        if let Ok((address, _)) = &result {
            verdict.authenticated_sender = Some(address.clone());
        }
        verdict.push(
            "authenticated_from",
            result.map(|(address, how)| format!("{address}, {how}")),
        );
    }

    if !criteria.to.is_empty() {
        let recipients = recipients(headers);
        let hit = recipients.iter().find(|address| {
            criteria
                .to
                .iter()
                .any(|want| want.eq_ignore_ascii_case(address))
        });
        verdict.push(
            "to",
            match hit {
                Some(address) => Ok(format!("addressed to {address}")),
                None if recipients.is_empty() => Err("no To, Cc or Delivered-To address".into()),
                None => Err(format!("addressed to {}", recipients.join(", "))),
            },
        );
    }

    if let Some(pattern) = &criteria.subject {
        let subject = header_values(headers, "Subject")
            .into_iter()
            .next()
            .unwrap_or_default();
        verdict.push(
            "subject",
            match regex::Regex::new(pattern) {
                Ok(re) if re.is_match(&subject) => Ok(format!("{subject:?}")),
                Ok(_) => Err(format!("{subject:?} does not match {pattern:?}")),
                Err(e) => Err(format!("{pattern:?} is not a regular expression: {e}")),
            },
        );
    }

    for (name, pattern) in &criteria.headers {
        let values = header_values(headers, name);
        verdict.push(
            format!("headers.{name}"),
            match regex::Regex::new(pattern) {
                Ok(re) => match values.iter().find(|value| re.is_match(value)) {
                    Some(value) => Ok(format!("{value:?}")),
                    None if values.is_empty() => Err(format!("no {name} header")),
                    None => Err(format!("no {name} value matches {pattern:?}")),
                },
                Err(e) => Err(format!("{pattern:?} is not a regular expression: {e}")),
            },
        );
    }

    verdict
}

/// The `From:` address, if it is one of `allowed` and the receiving server
/// authenticated its domain, with how it did; otherwise why not.
fn sender_check(
    headers: &[mailparse::MailHeader],
    allowed: &[String],
    trusted: &str,
) -> Result<(String, String), String> {
    let address = single_from(headers)?;
    if !allowed
        .iter()
        .any(|want| want.trim().eq_ignore_ascii_case(&address))
    {
        return Err(format!("{address} is not an allowed sender"));
    }
    let how = authenticated(headers, &address, trusted)?;
    Ok((address, how))
}

/// The one address `From:` carries, lowercased.
fn single_from(headers: &[mailparse::MailHeader]) -> Result<String, String> {
    let froms: Vec<&mailparse::MailHeader> = headers
        .iter()
        .filter(|header| header.get_key_ref().eq_ignore_ascii_case("From"))
        .collect();
    let [from] = froms.as_slice() else {
        return Err(format!("From appears {} times", froms.len()));
    };
    let list =
        mailparse::addrparse_header(from).map_err(|e| format!("From does not parse: {e}"))?;
    match list.as_slice() {
        [mailparse::MailAddr::Single(single)] => Ok(single.addr.trim().to_ascii_lowercase()),
        _ => Err("From does not carry exactly one address".to_string()),
    }
}

/// How the receiving server authenticated `address`'s domain, from the topmost
/// `Authentication-Results`, or why it did not.
fn authenticated(
    headers: &[mailparse::MailHeader],
    address: &str,
    trusted: &str,
) -> Result<String, String> {
    let Some(topmost) = headers.iter().find(|header| {
        header
            .get_key_ref()
            .eq_ignore_ascii_case("Authentication-Results")
    }) else {
        return Err("no Authentication-Results header".to_string());
    };
    let value = String::from_utf8_lossy(topmost.get_value_raw()).into_owned();
    let results = parse_authentication_results(&value);
    if !results.authserv_id.eq_ignore_ascii_case(trusted.trim()) {
        return Err(format!(
            "the topmost Authentication-Results is from {:?}, not {trusted:?}",
            results.authserv_id
        ));
    }
    let domain = domain_of(address);
    if domain.is_empty() {
        return Err(format!("{address} has no domain"));
    }
    for result in &results.results {
        if !result.result.eq_ignore_ascii_case("pass") {
            continue;
        }
        // The domain this pass vouches for: the DKIM signer (`header.d`, or
        // the domain of `header.i`), or the SPF envelope sender.
        let vouched = match result.method.as_str() {
            "dkim" => result
                .property("header.d")
                .or_else(|| result.property("header.i"))
                .map(domain_of),
            "spf" => result.property("smtp.mailfrom").map(domain_of),
            _ => None,
        };
        if vouched.as_deref() == Some(domain.as_str()) {
            return Ok(format!("{}=pass for {domain} by {trusted}", result.method));
        }
    }
    Err(format!(
        "{trusted} recorded no DKIM or SPF pass for {domain}"
    ))
}

/// The domain of an address, a bare domain, or an `@domain` DKIM identity,
/// lowercased.
fn domain_of(value: &str) -> String {
    let value = value.trim().trim_matches(['<', '>']);
    value
        .rsplit_once('@')
        .map_or(value, |(_, domain)| domain)
        .trim_end_matches('.')
        .to_ascii_lowercase()
}

/// Every `To:`, `Cc:` and `Delivered-To:` address, lowercased, in header order.
fn recipients(headers: &[mailparse::MailHeader]) -> Vec<String> {
    let mut out = Vec::new();
    for header in headers {
        let key = header.get_key_ref();
        if !["To", "Cc", "Delivered-To"]
            .iter()
            .any(|name| key.eq_ignore_ascii_case(name))
        {
            continue;
        }
        let Ok(list) = mailparse::addrparse_header(header) else {
            continue;
        };
        for entry in list.iter() {
            match entry {
                mailparse::MailAddr::Single(single) => {
                    out.push(single.addr.trim().to_ascii_lowercase())
                }
                mailparse::MailAddr::Group(group) => out.extend(
                    group
                        .addrs
                        .iter()
                        .map(|single| single.addr.trim().to_ascii_lowercase()),
                ),
            }
        }
    }
    out.dedup();
    out
}

/// Every decoded value of the header `name`, in order.
fn header_values(headers: &[mailparse::MailHeader], name: &str) -> Vec<String> {
    headers
        .iter()
        .filter(|header| header.get_key_ref().eq_ignore_ascii_case(name))
        .map(|header| header.get_value())
        .collect()
}

// ---------------------------------------------------------------------------
// RFC 8601
// ---------------------------------------------------------------------------

/// One `Authentication-Results` header, parsed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AuthenticationResults {
    /// Lowercased; empty when the header opens with a result instead (which
    /// Exchange Online does), so it can never equal a trusted id.
    pub authserv_id: String,
    pub results: Vec<MethodResult>,
}

/// One `method=result` clause and its `ptype.property=value` pairs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MethodResult {
    /// Lowercased, any `/version` dropped.
    pub method: String,
    pub result: String,
    /// Keys lowercased, values unquoted.
    pub properties: Vec<(String, String)>,
}

impl MethodResult {
    fn property(&self, key: &str) -> Option<&str> {
        self.properties
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }
}

/// Parse one header value: comments stripped, clauses split on `;`, the
/// authserv-id first. Lenient on purpose, since what a parse error would
/// protect is a verdict, and a clause it cannot read simply never passes.
pub fn parse_authentication_results(value: &str) -> AuthenticationResults {
    let clean = strip_comments(value);
    let mut clauses = split_outside_quotes(&clean, ';').into_iter();
    let mut parsed = AuthenticationResults::default();
    let Some(first) = clauses.next() else {
        return parsed;
    };
    let head = normalise_equals(&first);
    let mut head_tokens = head.split_whitespace();
    match head_tokens.next() {
        Some(token) if !token.contains('=') => parsed.authserv_id = token.to_ascii_lowercase(),
        // No authserv-id: the first clause is already a result.
        Some(_) => parsed.results.extend(method_result(&head)),
        None => {}
    }
    for clause in clauses {
        parsed
            .results
            .extend(method_result(&normalise_equals(&clause)));
    }
    parsed
}

/// `dkim=pass header.i=@example.com header.s=sel` as a [`MethodResult`].
fn method_result(clause: &str) -> Option<MethodResult> {
    let mut tokens = split_outside_quotes(clause, ' ')
        .into_iter()
        .map(|token| token.trim().to_string())
        .filter(|token| !token.is_empty());
    let first = tokens.next()?;
    let (method, result) = first.split_once('=')?;
    let method = method
        .split('/')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let properties = tokens
        .filter_map(|token| {
            let (key, value) = token.split_once('=')?;
            Some((key.to_ascii_lowercase(), unquote(value)))
        })
        .collect();
    Some(MethodResult {
        method,
        result: unquote(result).to_ascii_lowercase(),
        properties,
    })
}

/// Remove `(comments)`, nested ones included, outside quoted strings, and
/// unfold the header onto one line.
fn strip_comments(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut depth = 0usize;
    let mut quoted = false;
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                let next = chars.next();
                if depth == 0 {
                    out.push(c);
                    out.extend(next);
                }
            }
            '"' if depth == 0 => {
                quoted = !quoted;
                out.push(c);
            }
            '(' if !quoted => depth += 1,
            ')' if !quoted && depth > 0 => {
                depth -= 1;
                out.push(' ');
            }
            '\r' | '\n' | '\t' if depth == 0 => out.push(' '),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

/// Split on `separator` wherever it is not inside a quoted string.
fn split_outside_quotes(value: &str, separator: char) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut escaped = false;
    for c in value.chars() {
        if escaped {
            current.push(c);
            escaped = false;
            continue;
        }
        match c {
            '\\' => {
                escaped = true;
                current.push(c);
            }
            '"' => {
                quoted = !quoted;
                current.push(c);
            }
            _ if c == separator && !quoted => parts.push(std::mem::take(&mut current)),
            _ => current.push(c),
        }
    }
    parts.push(current);
    parts
}

/// Close up whitespace around `=`, which RFC 8601 allows, so a clause splits
/// into `key=value` tokens.
fn normalise_equals(clause: &str) -> String {
    let mut out = String::with_capacity(clause.len());
    let mut pending_space = false;
    for c in clause.chars() {
        if c.is_whitespace() {
            pending_space = true;
            continue;
        }
        if c == '=' {
            pending_space = false;
            out.push(c);
            continue;
        }
        if pending_space && !out.is_empty() && !out.ends_with('=') {
            out.push(' ');
        }
        pending_space = false;
        out.push(c);
    }
    out
}

fn unquote(value: &str) -> String {
    let value = value.trim();
    value
        .strip_prefix('"')
        .and_then(|inner| inner.strip_suffix('"'))
        .unwrap_or(value)
        .replace("\\\"", "\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::HookMatch;

    const GMAIL: &str = "mx.google.com";

    fn criteria(allowed: &[&str]) -> HookMatch {
        HookMatch {
            authenticated_from: allowed.iter().map(|s| s.to_string()).collect(),
            authserv_id: Some(GMAIL.to_string()),
            ..HookMatch::default()
        }
    }

    fn verdict(criteria: &HookMatch, raw: &str) -> Verdict {
        let raw = raw.replace('\n', "\r\n");
        let parsed = mailparse::parse_mail(raw.as_bytes()).expect("parses");
        evaluate(criteria, &parsed)
    }

    /// Gmail's own stamp for a Proton-sent message: DKIM with `header.i`,
    /// SPF with the envelope sender, and DMARC, folded over four lines.
    const GMAIL_STAMP: &str = "Authentication-Results: mx.google.com;\n       dkim=pass header.i=@hellin.me header.s=protonmail3 header.b=abc123;\n       spf=pass (google.com: domain of sylvain@hellin.me designates 185.70.43.17 as permitted sender) smtp.mailfrom=sylvain@hellin.me;\n       dmarc=pass (p=QUARANTINE sp=QUARANTINE dis=NONE) header.from=hellin.me\n";

    fn message(stamp: &str, from: &str, extra: &str) -> String {
        format!(
            "Delivered-To: assistant@gmail.com\nReceived: by 2002:a05::1 with SMTP id x;\n{stamp}{extra}From: {from}\nTo: assistant@gmail.com\nSubject: Do the thing\nMessage-ID: <m1@hellin.me>\n\nbody\n"
        )
    }

    #[test]
    fn a_genuine_gmail_stamp_authenticates_the_allowed_sender() {
        let v = verdict(
            &criteria(&["sylvain@hellin.me"]),
            &message(GMAIL_STAMP, "Sylvain Hellin <Sylvain@Hellin.me>", ""),
        );
        assert!(v.matched(), "{v:?}");
        assert_eq!(v.authenticated_sender.as_deref(), Some("sylvain@hellin.me"));
        assert!(v.checks[0].detail.contains("dkim=pass"), "{v:?}");
    }

    #[test]
    fn a_spoofed_from_that_failed_authentication_does_not_match() {
        let stamp = "Authentication-Results: mx.google.com;\n       dkim=none;\n       spf=softfail (google.com: domain of transitioning x@evil.example does not designate 1.2.3.4) smtp.mailfrom=x@evil.example;\n       dmarc=fail (p=QUARANTINE) header.from=hellin.me\n";
        let v = verdict(
            &criteria(&["sylvain@hellin.me"]),
            &message(stamp, "sylvain@hellin.me", ""),
        );
        assert!(!v.matched());
        assert!(v.checks[0].detail.contains("no DKIM or SPF pass"), "{v:?}");
    }

    /// The sender wrote a perfect-looking stamp of their own; the receiving
    /// server's real one sits above it and says fail.
    #[test]
    fn a_forged_stamp_below_the_real_one_is_never_read() {
        let real = "Authentication-Results: mx.google.com;\n       spf=fail smtp.mailfrom=x@evil.example;\n       dkim=none\n";
        let forged = "Authentication-Results: mx.google.com; dkim=pass header.i=@hellin.me; spf=pass smtp.mailfrom=sylvain@hellin.me\n";
        let v = verdict(
            &criteria(&["sylvain@hellin.me"]),
            &message(real, "sylvain@hellin.me", forged),
        );
        assert!(!v.matched(), "{v:?}");
    }

    /// No stamp at all fails. (A forged stamp would be the topmost here, which
    /// is why `authserv_id` may only name a server that stamps every message.)
    #[test]
    fn a_missing_stamp_does_not_match() {
        let v = verdict(
            &criteria(&["sylvain@hellin.me"]),
            &message("", "sylvain@hellin.me", ""),
        );
        assert!(!v.matched());
        assert_eq!(v.checks[0].detail, "no Authentication-Results header");
    }

    /// A stamp an intermediate hop added, topmost because the receiving server
    /// added none, is from a server the hook does not trust.
    #[test]
    fn a_stamp_from_another_hop_does_not_match() {
        let hop = "Authentication-Results: relay.example.net; dkim=pass header.d=hellin.me; spf=pass smtp.mailfrom=sylvain@hellin.me\n";
        let v = verdict(
            &criteria(&["sylvain@hellin.me"]),
            &message(hop, "sylvain@hellin.me", ""),
        );
        assert!(!v.matched());
        assert!(v.checks[0].detail.contains("relay.example.net"), "{v:?}");
    }

    /// Exchange Online writes no authserv-id; its header can never be trusted.
    #[test]
    fn a_stamp_without_an_authserv_id_does_not_match() {
        let exo = "Authentication-Results: spf=pass (sender IP is 1.2.3.4) smtp.mailfrom=hellin.me; dkim=pass (signature was verified) header.d=hellin.me;dmarc=pass action=none header.from=hellin.me\n";
        let v = verdict(
            &criteria(&["sylvain@hellin.me"]),
            &message(exo, "sylvain@hellin.me", ""),
        );
        assert!(!v.matched());
    }

    /// The pass is real but for another domain than the one in From.
    #[test]
    fn a_pass_for_another_domain_does_not_vouch_for_from() {
        let stamp = "Authentication-Results: mx.google.com; dkim=pass header.i=@evil.example; spf=pass smtp.mailfrom=bounce@evil.example\n";
        let v = verdict(
            &criteria(&["sylvain@hellin.me"]),
            &message(stamp, "sylvain@hellin.me", ""),
        );
        assert!(!v.matched());
    }

    /// Relaxed alignment is not enough: a subdomain's signature does not vouch
    /// for the parent domain's address.
    #[test]
    fn a_subdomain_signature_does_not_vouch_for_the_parent() {
        let stamp = "Authentication-Results: mx.google.com; dkim=pass header.d=xyz.tum.de; dmarc=pass header.from=tum.de\n";
        let v = verdict(
            &criteria(&["sylvain.hellin@tum.de"]),
            &message(stamp, "sylvain.hellin@tum.de", ""),
        );
        assert!(!v.matched(), "{v:?}");
    }

    #[test]
    fn an_spf_pass_for_the_from_domain_is_enough() {
        let stamp = "Authentication-Results: mx.google.com; spf=pass (google.com: ...) smtp.mailfrom=sylvain.hellin@tum.de; dkim=none\n";
        let v = verdict(
            &criteria(&["sylvain.hellin@tum.de"]),
            &message(stamp, "\"Hellin, Sylvain\" <sylvain.hellin@tum.de>", ""),
        );
        assert!(v.matched(), "{v:?}");
    }

    #[test]
    fn an_authenticated_sender_not_on_the_list_does_not_match() {
        let v = verdict(
            &criteria(&["sylvain@hellin.me"]),
            &message(GMAIL_STAMP, "other@hellin.me", ""),
        );
        assert!(!v.matched());
        assert!(v.checks[0].detail.contains("not an allowed sender"));
    }

    #[test]
    fn two_from_headers_or_two_addresses_do_not_match() {
        let twice = message(
            GMAIL_STAMP,
            "sylvain@hellin.me",
            "From: other@evil.example\n",
        );
        assert!(!verdict(&criteria(&["sylvain@hellin.me"]), &twice).matched());
        let two = message(GMAIL_STAMP, "sylvain@hellin.me, other@evil.example", "");
        assert!(!verdict(&criteria(&["sylvain@hellin.me"]), &two).matched());
    }

    #[test]
    fn recipient_subject_and_header_criteria_all_have_to_pass() {
        let mut c = criteria(&["sylvain@hellin.me"]);
        c.to = vec!["assistant+pi@gmail.com".to_string()];
        c.subject = Some("^Do ".to_string());
        c.headers.insert("X-Task".to_string(), "^yes$".to_string());
        let raw = |to: &str, task: &str| {
            message(
                GMAIL_STAMP,
                "sylvain@hellin.me",
                &format!("X-Task: {task}\n"),
            )
            .replace(
                "Delivered-To: assistant@gmail.com",
                &format!("Delivered-To: {to}"),
            )
        };
        assert!(verdict(&c, &raw("assistant+pi@gmail.com", "yes")).matched());
        let wrong_tag = verdict(&c, &raw("assistant@gmail.com", "yes"));
        assert_eq!(wrong_tag.first_failure().unwrap().criterion, "to");
        let wrong_header = verdict(&c, &raw("assistant+pi@gmail.com", "no"));
        assert_eq!(
            wrong_header.first_failure().unwrap().criterion,
            "headers.X-Task"
        );
    }

    #[test]
    fn the_parser_reads_versions_quotes_comments_and_spaced_equals() {
        let parsed = parse_authentication_results(
            "MX.Google.com 1; dkim/1 = pass (good (nested) sig) header.d=\"hellin.me\"; spf=pass smtp.mailfrom=a@b.c",
        );
        assert_eq!(parsed.authserv_id, "mx.google.com");
        assert_eq!(parsed.results[0].method, "dkim");
        assert_eq!(parsed.results[0].result, "pass");
        assert_eq!(parsed.results[0].property("header.d"), Some("hellin.me"));
        assert_eq!(parsed.results[1].property("smtp.mailfrom"), Some("a@b.c"));
    }
}
