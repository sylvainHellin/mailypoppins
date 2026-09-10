//! Rendering for the draft slice: the bytes `mp list`, `mp validate` and the
//! bare-selector dry run print, built from what the daemon answered (P4-U6).
//!
//! The daemon decides *what* a draft command says about a draft and this module
//! decides how it reads, the same split [`crate::read_cmd`] makes for the read
//! slice. Every function here takes an `mp_protocol::draft` value and returns a
//! `String`, so the routed CLI and any later client render one payload through
//! one piece of code, and a colour or a dash moves in one place.
//!
//! Warnings go to stderr and listings to stdout, which is why the collision and
//! skip blocks are their own functions rather than a tail of the listing: a
//! broken file is a warning about the directory, not a failure of the command,
//! and the exit code stays 0.

use colored::*;

use mp_protocol::draft::{DraftCollision, DraftListing, DraftPreview, DraftSkip, DraftValidation};

/// The width of the rules `mp list` draws.
const RULE: usize = 72;

/// `mp list`, from the account's listing.
///
/// The rows arrive in the index's order (`mtime DESC, id ASC`) and are printed
/// without re-sorting. An empty listing says so and says whether anything was
/// skipped, because "no drafts" and "no *listable* drafts" are two different
/// states of the directory.
pub fn render_list(listing: &DraftListing) -> String {
    if listing.drafts.is_empty() {
        return match listing.skipped.is_empty() {
            true => format!("No drafts for {}\n", listing.account),
            false => format!("No listable drafts for {}\n", listing.account),
        };
    }

    let mut out = format!("\n{}\n{}\n", "Drafts:".bold(), "\u{2500}".repeat(RULE));
    let mut counts: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for row in &listing.drafts {
        *counts.entry(row.status.as_str()).or_default() += 1;
        let status = match row.status.as_str() {
            "draft" => "draft".yellow(),
            "approved" => "approved".green(),
            "sent" => "sent".dimmed(),
            other => other.normal(),
        };
        out.push_str(&format!(
            "[{}] {} \u{2192} {}\n",
            status,
            row.selector,
            row.to.as_deref().unwrap_or("(bcc only)")
        ));
        // A subject that is there and blank prints no line: the listing shows
        // what the file says, and a file with a blank `subject:` says nothing.
        if let Some(subject) = row
            .subject
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            out.push_str(&format!("      {}\n", subject.dimmed()));
        }
    }
    let summary = counts
        .iter()
        .map(|(status, n)| format!("{status}: {n}"))
        .collect::<Vec<_>>()
        .join(" | ");
    out.push_str(&format!(
        "{}\nTotal: {} | {}\n",
        "\u{2500}".repeat(RULE),
        listing.drafts.len(),
        summary
    ));
    out
}

/// The stderr block naming the files the scan could not parse (#0080).
///
/// A skipped file is a draft the index cannot see: no `id:`, no row, absent
/// from the listing. Naming it, with its one-line parse error, is what turns
/// "my draft disappeared" into a fixable line.
pub fn render_skipped(skipped: &[DraftSkip]) -> String {
    if skipped.is_empty() {
        return String::new();
    }
    let noun = if skipped.len() == 1 {
        "draft"
    } else {
        "drafts"
    };
    let mut out = format!(
        "\n{} {} {noun} skipped (frontmatter would not parse; fix the YAML to list them):\n",
        "\u{26a0}".yellow(),
        skipped.len()
    );
    for skip in skipped {
        out.push_str(&format!(
            "  {} - {}\n",
            skip.path.clone().yellow(),
            skip.error
        ));
    }
    out
}

/// The stderr block naming two files that claim one id.
///
/// The index cannot decide which the user meant, so it says so rather than
/// dropping the loser in silence.
pub fn render_collisions(collisions: &[DraftCollision]) -> String {
    collisions
        .iter()
        .map(|collision| {
            format!(
                "{} two drafts share the id {}: {} is indexed, {} is not addressable until its \
                 id: field is changed\n",
                "\u{26a0}".yellow(),
                collision.id,
                collision.kept,
                collision.shadowed
            )
        })
        .collect()
}

/// `mp validate`, from the account's reports.
///
/// The exit code is the caller's: this returns the lines and
/// [`invalid_count`] the number that decides it.
pub fn render_validation(validation: &DraftValidation) -> String {
    if validation.reports.is_empty() {
        return format!("No drafts to validate for {}\n", validation.account);
    }
    let mut out = String::new();
    for report in &validation.reports {
        match &report.error {
            None => {
                out.push_str(&format!("{} {}", "\u{2713}".green(), report.selector));
                if !report.warnings.is_empty() {
                    out.push_str(&format!(" ({})", report.warnings.join(", ").yellow()));
                }
                out.push('\n');
            }
            Some(error) => out.push_str(&format!(
                "{} {} - {}\n",
                "\u{2717}".red(),
                report.selector,
                error
            )),
        }
    }
    out.push_str(&format!(
        "\nValidation complete: {} valid, {} invalid\n",
        (validation.reports.len() - invalid_count(validation))
            .to_string()
            .green(),
        invalid_count(validation).to_string().red()
    ));
    out
}

/// How many reports failed, which is what makes `mp validate` exit 1.
pub fn invalid_count(validation: &DraftValidation) -> usize {
    validation
        .reports
        .iter()
        .filter(|report| !report.valid)
        .count()
}

/// The bare-selector dry run, from the preview the daemon rendered.
///
/// Field for field [`crate::draft::preview_draft`], including its two cut-offs:
/// the body arrives already cut at 500 characters and `body_truncated` already
/// decided on 500 bytes, so nothing here re-derives either.
pub fn render_preview(preview: &DraftPreview) -> String {
    let mut out = render_send_preview(preview);
    out.push_str(&format!(
        "\n{}\n\n",
        "[DRY RUN] Would send email (use 'send' subcommand to actually send)"
            .yellow()
            .bold()
    ));
    out
}

/// The same block without the dry-run trailer, which is the preview `mp send`
/// prints before it asks (`preview_draft` with `is_dry_run = false`).
pub fn render_send_preview(preview: &DraftPreview) -> String {
    let mut out = format!(
        "\n{}\n{}: {}\n{}: {}\n",
        "=== Email Draft Preview ===".bold().cyan(),
        "From".bold(),
        preview.from,
        "To".bold(),
        preview.to.as_deref().unwrap_or("(bcc only)")
    );
    if let Some(cc) = &preview.cc {
        out.push_str(&format!("{}: {}\n", "Cc".bold(), cc));
    }
    if let Some(bcc) = &preview.bcc {
        out.push_str(&format!("{}: {}\n", "Bcc".bold(), bcc));
    }
    out.push_str(&format!("{}: {}\n", "Subject".bold(), preview.subject));

    out.push_str(&format!(
        "\n{}\n\n{}\n",
        "--- Body Preview (first 500 chars) ---".dimmed(),
        preview.body
    ));
    if preview.body_truncated {
        out.push_str(&format!("{}\n", "...".dimmed()));
    }

    out.push_str(&format!("\n{}\n", "--- Settings ---".dimmed()));
    out.push_str(&format!(
        "  Font: {} ({})\n",
        preview.font_family, preview.font_size
    ));
    match &preview.signature {
        Some(signature) => out.push_str(&format!(
            "  Signature: {} ...\n",
            signature
                .chars()
                .take(50)
                .collect::<String>()
                .replace('\n', " ")
        )),
        None => out.push_str(&format!("  Signature: {}\n", "none".dimmed())),
    }

    out.push_str(&format!("\n{}\n", "--- Status ---".dimmed()));
    match &preview.error {
        None => {
            out.push_str(&format!("{} Valid YAML frontmatter\n", "\u{2713}".green()));
            out.push_str(&format!(
                "{} Status: {}\n",
                "\u{2713}".green(),
                preview.status.yellow()
            ));
            out.push_str(&format!(
                "{} All required fields present\n",
                "\u{2713}".green()
            ));
            for warning in &preview.warnings {
                out.push_str(&format!("{} {}\n", "\u{26a0}".yellow(), warning));
            }
        }
        Some(error) => out.push_str(&format!(
            "{} Validation error: {}\n",
            "\u{2717}".red(),
            error
        )),
    }
    out
}
