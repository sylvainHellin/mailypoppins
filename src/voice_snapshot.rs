//! Draft snapshots for the voice lane of the nightly reflection loop.
//!
//! A draft mp creates for an account `[voice_snapshots] accounts` lists
//! (default `["tum"]`, [`crate::config::VoiceSnapshotsConfig`]) is copied as
//! created to `<data_dir>/voice-snapshots/<account>/<draft-id>.json`, and the
//! send that delivers it adds the Message-ID, the time and the body that went
//! out. The pair is what the weekly voice lane reads: the draft as it was
//! written against the text that was actually sent.
//!
//! What a snapshot keeps is deliberately narrow: the subject, the two bodies
//! cut to the writer's own text, and of the recipients only the domain of the
//! first `to:`, never an address. A body is cut at the quote marker of a
//! reply or forward ([`own_text`]), since what mp writes below it (the
//! attribution line, the forwarded header block and the quoted original)
//! names the original's people and is their writing. Every write prunes the account's snapshots created more than
//! [`RETENTION_DAYS`] ago.
//!
//! Best effort throughout: a snapshot that cannot be written is a warning in
//! the log, and the draft or the send it rode on goes ahead untouched. The
//! send side needs no setting: it completes a snapshot that exists and writes
//! none that does not, so an account the setting stopped listing keeps no new
//! ones and its old ones age out.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use log::{debug, warn};
use serde::{Deserialize, Serialize};

use crate::config::VoiceSnapshotsConfig;
use crate::types::EmailDraft;

/// How long a snapshot is kept, counted from its `created` time.
pub const RETENTION_DAYS: i64 = 90;

/// One snapshot file, `<data_dir>/voice-snapshots/<account>/<draft-id>.json`.
///
/// The `sent_*` fields are absent until mp sends the draft.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VoiceSnapshot {
    /// The draft's frontmatter `id:`, which is also the file's stem.
    pub draft_id: String,
    pub account: String,
    /// When the draft was created, RFC 3339 in UTC.
    pub created: String,
    pub subject: String,
    /// The domain of the first `to:` recipient, lowercased; `None` when the
    /// draft was created with nobody on `to:`.
    pub to_domain: Option<String>,
    /// The draft's Markdown body as created, signature included, cut above
    /// the quote of a reply or forward ([`own_text`]).
    pub body: String,
    /// The `Message-ID` of the send that delivered the draft.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent_message_id: Option<String>,
    /// When that send went out, RFC 3339 in UTC.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent_at: Option<String>,
    /// The draft's Markdown body as it was sent, cut the same way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent_body: Option<String>,
}

/// The file the snapshot of `draft_id` lives in, or `None` for an id that is
/// not a plain file stem (empty, a leading `.`, or anything but ASCII letters,
/// digits, `-`, `_` and `.`), which could otherwise name a file outside the
/// account's directory.
pub fn snapshot_path(account: &str, draft_id: &str) -> Option<PathBuf> {
    let plain = !draft_id.is_empty()
        && !draft_id.starts_with('.')
        && draft_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    plain.then(|| crate::config::voice_snapshots_dir(account).join(format!("{draft_id}.json")))
}

/// Read one snapshot back; `None` when there is none or it does not parse.
pub fn load(account: &str, draft_id: &str) -> Option<VoiceSnapshot> {
    let text = fs::read_to_string(snapshot_path(account, draft_id)?).ok()?;
    serde_json::from_str(&text).ok()
}

/// Snapshot the draft just created at `path`, when `config` lists `account`.
///
/// Called by the daemon right after a draft-creating method wrote the file
/// and before its answer goes out, so the body is the one mp wrote and no
/// editor has touched it yet. A failure is logged and swallowed.
pub fn record_created(config: &VoiceSnapshotsConfig, account: &str, path: &Path) {
    if !config.covers(account) {
        return;
    }
    if let Err(e) = write_created(account, path, Utc::now()) {
        warn!(
            "[voice] could not snapshot the draft {}: {e:#}",
            path.display()
        );
    }
}

/// Complete the snapshot of `draft`, which a send just delivered under
/// `message_id`, with the body it went out with.
///
/// Called before the send settles the draft, since settling may retire the
/// file. A draft without a snapshot is left alone, and a failure is logged and
/// swallowed: the message is out either way.
pub fn record_sent(account: &str, draft: &EmailDraft, message_id: &str) {
    if let Err(e) = write_sent(account, draft, message_id, Utc::now()) {
        warn!(
            "[voice] could not record the send of {} in its snapshot: {e:#}",
            draft.path.display()
        );
    }
}

fn write_created(account: &str, path: &Path, now: DateTime<Utc>) -> Result<()> {
    let draft = crate::draft::parse_email_draft(path)?;
    let id = draft_id(&draft).context("the draft has no id")?;
    let file = snapshot_path(account, id)
        .with_context(|| format!("the draft id {id:?} is not a file name"))?;
    let snapshot = VoiceSnapshot {
        draft_id: id.to_string(),
        account: account.to_string(),
        created: rfc3339(now),
        subject: draft.frontmatter.subject.clone(),
        to_domain: first_domain(draft.frontmatter.to.as_deref()),
        body: own_text(&draft.body_markdown),
        sent_message_id: None,
        sent_at: None,
        sent_body: None,
    };
    write(&file, &snapshot, now)
}

fn write_sent(
    account: &str,
    draft: &EmailDraft,
    message_id: &str,
    now: DateTime<Utc>,
) -> Result<()> {
    let Some(file) = draft_id(draft).and_then(|id| snapshot_path(account, id)) else {
        return Ok(());
    };
    let text = match fs::read_to_string(&file) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", file.display())),
    };
    let mut snapshot: VoiceSnapshot =
        serde_json::from_str(&text).with_context(|| format!("parsing {}", file.display()))?;
    snapshot.sent_message_id = Some(message_id.to_string());
    snapshot.sent_at = Some(rfc3339(now));
    snapshot.sent_body = Some(own_text(&draft.body_markdown));
    write(&file, &snapshot, now)
}

/// The writer's own text of a draft body: the part above the quote marker,
/// signature included ([`crate::draft::reply_area`]), with the blank lines
/// before the marker dropped; a body without a quote whole.
fn own_text(body: &str) -> String {
    crate::draft::reply_area(body).trim_end().to_string()
}

/// The draft's trimmed frontmatter `id:`, `None` when it has none.
fn draft_id(draft: &EmailDraft) -> Option<&str> {
    draft
        .frontmatter
        .id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
}

fn rfc3339(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// The lowercased domain of the first address on a `to:` line.
fn first_domain(to: Option<&str>) -> Option<String> {
    let first = crate::parse::mailbox_addresses(to?).into_iter().next()?;
    let (_, domain) = first.rsplit_once('@')?;
    let domain = domain.trim().trim_end_matches('>').to_ascii_lowercase();
    (!domain.is_empty()).then_some(domain)
}

/// Replace `file` with `snapshot` through a temporary file beside it, then
/// prune the directory. A prune that fails is logged: the snapshot is written.
fn write(file: &Path, snapshot: &VoiceSnapshot, now: DateTime<Utc>) -> Result<()> {
    let dir = file.parent().context("a snapshot file has a directory")?;
    crate::config::create_private_dir_all(dir)
        .with_context(|| format!("creating {}", dir.display()))?;
    let name = file
        .file_name()
        .context("a snapshot file has a name")?
        .to_string_lossy();
    let staging = dir.join(format!(".{name}.tmp"));
    fs::write(&staging, serde_json::to_vec_pretty(snapshot)?)
        .with_context(|| format!("writing {}", staging.display()))?;
    fs::rename(&staging, file).with_context(|| format!("writing {}", file.display()))?;
    match prune(dir, now) {
        Ok(0) => {}
        Ok(n) => debug!("[voice] pruned {n} snapshot(s) from {}", dir.display()),
        Err(e) => warn!("[voice] could not prune {}: {e:#}", dir.display()),
    }
    Ok(())
}

/// The one field a prune reads.
#[derive(Deserialize)]
struct Created {
    created: String,
}

/// Delete the snapshots in `dir` created more than [`RETENTION_DAYS`] before
/// `now`, and say how many went. A file whose `created` does not parse is
/// aged by its modification time instead.
fn prune(dir: &Path, now: DateTime<Utc>) -> Result<usize> {
    let cutoff = now - Duration::days(RETENTION_DAYS);
    let mut pruned = 0;
    for entry in fs::read_dir(dir).with_context(|| format!("listing {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let created = fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<Created>(&text).ok())
            .and_then(|c| DateTime::parse_from_rfc3339(&c.created).ok())
            .map(|at| at.with_timezone(&Utc))
            .or_else(|| {
                let modified = entry.metadata().and_then(|m| m.modified()).ok()?;
                Some(DateTime::<Utc>::from(modified))
            });
        if created.is_some_and(|at| at < cutoff) {
            match fs::remove_file(&path) {
                Ok(()) => pruned += 1,
                Err(e) => warn!("[voice] could not prune {}: {e}", path.display()),
            }
        }
    }
    Ok(pruned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::test_env::TestDataDir;
    use chrono::{DateTime, Duration, Utc};

    const ACCOUNT: &str = "tum";

    fn tum_only() -> VoiceSnapshotsConfig {
        VoiceSnapshotsConfig::default()
    }

    /// A draft as `draft.create` writes it, with recipients and a subject.
    fn write_draft(id: &str, to: &str, body: &str) -> PathBuf {
        let dir = crate::config::drafts_dir(ACCOUNT);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{id}.md"));
        std::fs::write(
            &path,
            crate::draft::new_draft_with_body(
                "me@tum.de",
                "Thu, 12 Nov 2026 08:00:00 +0000",
                id,
                body,
                Some("Best,\nSylvain"),
            ),
        )
        .unwrap();
        crate::draft::rewrite_draft_recipients(
            &path,
            &crate::draft::DraftRecipientEdit {
                to: to.to_string(),
                cc: "Carol <carol@other.org>".to_string(),
                bcc: String::new(),
                subject: "Kickoff next week".to_string(),
            },
        )
        .unwrap();
        path
    }

    fn snapshot_dir() -> PathBuf {
        crate::config::voice_snapshots_dir(ACCOUNT)
    }

    fn raw_snapshot(id: &str) -> String {
        std::fs::read_to_string(snapshot_dir().join(format!("{id}.json")))
            .unwrap_or_else(|e| panic!("the snapshot of {id} is on disk: {e}"))
    }

    /// A draft of a listed account is snapshotted as created: its id, the
    /// account, an RFC 3339 creation time, the subject, the first
    /// recipient's domain and the body, and never a recipient address.
    #[test]
    fn a_created_draft_of_a_listed_account_is_snapshotted() {
        let _data = TestDataDir::new();
        let path = write_draft("d1", "\"Doe, Jane\" <Jane.Doe@Example.ORG>", "Hello Jane,");
        let draft = crate::draft::parse_email_draft(&path).unwrap();

        let before = Utc::now() - Duration::seconds(1);
        record_created(&tum_only(), ACCOUNT, &path);

        let snapshot = load(ACCOUNT, "d1").expect("the snapshot was written");
        assert_eq!(snapshot.draft_id, "d1");
        assert_eq!(snapshot.account, ACCOUNT);
        let created = DateTime::parse_from_rfc3339(&snapshot.created)
            .unwrap_or_else(|e| panic!("created is RFC 3339, got {:?}: {e}", snapshot.created));
        assert!(created >= before && created <= Utc::now() + Duration::seconds(1));
        assert_eq!(snapshot.subject, "Kickoff next week");
        assert_eq!(snapshot.to_domain.as_deref(), Some("example.org"));
        assert_eq!(snapshot.body, draft.body_markdown);
        assert!(snapshot.body.contains("Hello Jane,"));
        assert_eq!(snapshot.sent_message_id, None);

        let raw = raw_snapshot("d1");
        for address in ["Jane.Doe@", "jane.doe@", "carol@", "me@tum.de"] {
            assert!(
                !raw.contains(address),
                "the snapshot names {address}:\n{raw}"
            );
        }
    }

    /// The message the reply and forward rows answer: a named sender, a
    /// named Cc and a body that is someone else's writing.
    fn source() -> crate::draft::SourceMessage {
        crate::draft::SourceMessage {
            from: "\"Doe, Jane\" <jane.doe@example.org>".to_string(),
            to: "Me <me@tum.de>".to_string(),
            cc: Some("Carol <carol@other.org>".to_string()),
            subject: "Budget".to_string(),
            message_id: Some("<budget-1@example.org>".to_string()),
            date: Some("Thu, 2 Jul 2026 13:57:30 +0200".to_string()),
            body: "Jane's own words about the budget.\nWrite to jane.doe@example.org.\n"
                .to_string(),
            ..Default::default()
        }
    }

    /// A reply or inline forward as mp writes it, with `text` typed above
    /// the signature and the frontmatter `id:` the daemon's file carries.
    fn write_quoting_draft(id: &str, forward: bool, text: &str) -> PathBuf {
        let dir = crate::config::drafts_dir(ACCOUNT);
        let signature = Some("Best,\nSylvain");
        let path = if forward {
            crate::draft::create_forward_draft_from(&source(), "me@tum.de", Some(&dir), signature)
        } else {
            crate::draft::create_reply_draft_from(
                &source(),
                false,
                "me@tum.de",
                Some(&dir),
                signature,
            )
        }
        .unwrap();
        let file = std::fs::read_to_string(&path).unwrap();
        let (fm, body) = file[4..].split_once("\n---\n").unwrap();
        std::fs::write(&path, format!("---\nid: {id}\n{fm}\n---\n\n{text}\n{body}")).unwrap();
        path
    }

    /// The raw snapshot holds no address and none of the quoted mail: the
    /// attribution, the forward header block and the original stay out.
    fn assert_only_own_text(id: &str, kind: &str) {
        let raw = raw_snapshot(id);
        let snapshot: VoiceSnapshot = serde_json::from_str(&raw).unwrap();
        let without_mid = match snapshot.sent_message_id.as_deref() {
            Some(mid) => raw.replace(mid, ""),
            None => raw.clone(),
        };
        assert!(
            !without_mid.contains('@'),
            "{kind}: the snapshot names an address:\n{raw}"
        );
        for quoted in [
            "Jane's own words",
            "Doe, Jane",
            "wrote:",
            "From:",
            "Subject:",
            crate::draft::QUOTE_MARKER,
        ] {
            assert!(
                !raw.contains(quoted),
                "{kind}: the snapshot holds {quoted:?}:\n{raw}"
            );
        }
    }

    /// A reply and an inline forward are snapshotted as the text above the
    /// quote, signature included, as created and as sent.
    #[test]
    fn a_quoting_draft_keeps_only_the_text_above_the_quote() {
        let _data = TestDataDir::new();
        for (id, forward) in [("reply1", false), ("fwd1", true)] {
            let kind = if forward { "forward" } else { "reply" };
            let path = write_quoting_draft(id, forward, "Thanks Jane, agreed.");
            record_created(&tum_only(), ACCOUNT, &path);
            let created = load(ACCOUNT, id).expect("the snapshot was written");
            assert!(
                created.body.starts_with("Thanks Jane, agreed."),
                "{kind}: {:?}",
                created.body
            );
            assert!(
                created.body.ends_with(crate::draft::SIG_END),
                "{kind}: the signature stays, the quote goes: {:?}",
                created.body
            );
            assert_only_own_text(id, kind);

            let text = std::fs::read_to_string(&path).unwrap();
            std::fs::write(&path, text.replace("agreed.", "agreed, see you.")).unwrap();
            let draft = crate::draft::parse_email_draft(&path).unwrap();
            record_sent(ACCOUNT, &draft, "<sent-q@tum.de>");
            let sent = load(ACCOUNT, id).unwrap();
            let sent_body = sent.sent_body.expect("the send was recorded");
            assert!(sent_body.starts_with("Thanks Jane, agreed, see you."));
            assert!(sent_body.ends_with(crate::draft::SIG_END), "{sent_body:?}");
            assert_only_own_text(id, kind);
        }
    }

    /// A draft with nobody on `to:` yet has no domain to record.
    #[test]
    fn a_draft_with_no_recipient_records_no_domain() {
        let _data = TestDataDir::new();
        let path = write_draft("d2", "", "");
        record_created(&tum_only(), ACCOUNT, &path);
        assert_eq!(load(ACCOUNT, "d2").expect("written").to_domain, None);
    }

    /// An account the setting does not list gets no snapshot at all.
    #[test]
    fn an_unlisted_account_is_not_snapshotted() {
        let _data = TestDataDir::new();
        let path = write_draft("d3", "a@example.org", "Hi");
        let other = VoiceSnapshotsConfig {
            accounts: vec!["proton".to_string()],
        };
        record_created(&other, ACCOUNT, &path);
        assert!(load(ACCOUNT, "d3").is_none());
        assert!(
            !snapshot_dir().exists(),
            "nothing was created for an unlisted account"
        );
    }

    /// The send adds the Message-ID, the time and the body as it went out,
    /// and leaves the body as created alone.
    #[test]
    fn a_sent_draft_completes_its_snapshot() {
        let _data = TestDataDir::new();
        let path = write_draft("d4", "a@example.org", "First version.");
        record_created(&tum_only(), ACCOUNT, &path);
        let created = load(ACCOUNT, "d4").unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, text.replace("First version.", "Edited by hand.")).unwrap();
        let draft = crate::draft::parse_email_draft(&path).unwrap();
        let before = Utc::now() - Duration::seconds(1);
        record_sent(ACCOUNT, &draft, "<sent-1@tum.de>");

        let sent = load(ACCOUNT, "d4").expect("still there");
        assert_eq!(sent.body, created.body, "the body as created is kept");
        assert_eq!(sent.created, created.created);
        assert_eq!(sent.sent_message_id.as_deref(), Some("<sent-1@tum.de>"));
        let sent_at = DateTime::parse_from_rfc3339(sent.sent_at.as_deref().unwrap()).unwrap();
        assert!(sent_at >= before);
        assert_eq!(
            sent.sent_body.as_deref(),
            Some(draft.body_markdown.as_str())
        );
        assert!(sent.sent_body.unwrap().contains("Edited by hand."));
    }

    /// A draft that was never snapshotted (another account, or one written
    /// before the setting listed its account) gets no snapshot at send.
    #[test]
    fn a_draft_without_a_snapshot_gets_none_at_send() {
        let _data = TestDataDir::new();
        let path = write_draft("d5", "a@example.org", "Hi");
        let draft = crate::draft::parse_email_draft(&path).unwrap();
        record_sent(ACCOUNT, &draft, "<sent-5@tum.de>");
        assert!(load(ACCOUNT, "d5").is_none());
    }

    fn plant(id: &str, created: DateTime<Utc>) -> PathBuf {
        let dir = snapshot_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{id}.json"));
        let snapshot = VoiceSnapshot {
            draft_id: id.to_string(),
            account: ACCOUNT.to_string(),
            created: created.to_rfc3339(),
            subject: String::new(),
            to_domain: None,
            body: String::new(),
            sent_message_id: None,
            sent_at: None,
            sent_body: None,
        };
        std::fs::write(&path, serde_json::to_string(&snapshot).unwrap()).unwrap();
        path
    }

    /// Every write prunes the account's snapshots created more than 90 days
    /// ago, and keeps the younger ones.
    #[test]
    fn a_snapshot_write_prunes_what_is_older_than_ninety_days() {
        let _data = TestDataDir::new();
        let old = plant("old", Utc::now() - Duration::days(RETENTION_DAYS + 1));
        let young = plant("young", Utc::now() - Duration::days(RETENTION_DAYS - 1));

        let path = write_draft("d6", "a@example.org", "Hi");
        record_created(&tum_only(), ACCOUNT, &path);

        assert!(!old.exists(), "a snapshot of 91 days is pruned");
        assert!(young.exists(), "a snapshot of 89 days is kept");
        assert!(load(ACCOUNT, "d6").is_some());

        let older = plant("older", Utc::now() - Duration::days(RETENTION_DAYS + 5));
        let draft = crate::draft::parse_email_draft(&path).unwrap();
        record_sent(ACCOUNT, &draft, "<sent-6@tum.de>");
        assert!(!older.exists(), "the send's write prunes too");
    }

    /// A draft id that would leave the snapshot directory names no file.
    #[test]
    fn a_draft_id_with_a_path_in_it_names_no_snapshot() {
        let _data = TestDataDir::new();
        assert!(snapshot_path(ACCOUNT, "d7").is_some());
        for id in ["", "../escape", "a/b", "..", "a\\b"] {
            assert_eq!(snapshot_path(ACCOUNT, id), None, "{id:?}");
        }
    }

    /// A snapshot that cannot be written is a warning, never a failure: the
    /// directory blocked by a file leaves the draft and the call unharmed.
    #[test]
    fn a_snapshot_that_cannot_be_written_is_shrugged_off() {
        let _data = TestDataDir::new();
        let root = crate::config::mailypoppins_data_dir().join("voice-snapshots");
        std::fs::create_dir_all(root.parent().unwrap()).unwrap();
        std::fs::write(&root, "not a directory").unwrap();
        let path = write_draft("d8", "a@example.org", "Hi");
        let draft = crate::draft::parse_email_draft(&path).unwrap();

        record_created(&tum_only(), ACCOUNT, &path);
        record_sent(ACCOUNT, &draft, "<sent-8@tum.de>");

        assert!(path.exists(), "the draft is untouched");
        assert!(load(ACCOUNT, "d8").is_none());
    }

    /// A send a resume completed records the snapshot too, before the draft
    /// file is retired.
    #[test]
    fn a_send_a_resume_completed_completes_the_snapshot() {
        use crate::outbox::{self, Envelope, SubmitOutcome};
        use crate::store::{BlobStore, Store};

        let _data = TestDataDir::new();
        let path = write_draft("d9", "a@example.org", "Queued once.");
        record_created(&tum_only(), ACCOUNT, &path);

        let account_dir = crate::config::account_dir(ACCOUNT);
        std::fs::create_dir_all(&account_dir).unwrap();
        let store = Store::open(crate::config::store_path(ACCOUNT)).unwrap();
        let blobs = BlobStore::new(crate::config::blobs_dir(ACCOUNT));
        let mid = "<resumed-9@tum.de>";
        let raw = format!("Message-ID: {mid}\r\nSubject: x\r\n\r\nbody\r\n");
        let row = outbox::enqueue(
            &store,
            &blobs,
            ACCOUNT,
            None,
            mid,
            raw.as_bytes(),
            &Envelope {
                from: "me@tum.de".to_string(),
                recipients: vec![("a@example.org".to_string(), crate::send::RecipientRole::To)],
                draft_key: Some("id:d9".to_string()),
                ..Envelope::default()
            },
        )
        .unwrap();
        outbox::record_submission(&store, &blobs, row, &SubmitOutcome::Accepted).unwrap();
        store
            .conn()
            .execute("UPDATE outbox SET state = 'done' WHERE id = ?1", [row])
            .unwrap();

        crate::send::retire_draft_of_row(&crate::config::drafts_dir(ACCOUNT), &store, row);

        assert!(!path.exists(), "the resume retired the draft file");
        let snapshot = load(ACCOUNT, "d9").expect("still there");
        assert_eq!(snapshot.sent_message_id.as_deref(), Some(mid));
        assert!(snapshot.sent_body.unwrap().contains("Queued once."));
        assert!(snapshot.sent_at.is_some());
    }
}
