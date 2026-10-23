//! The server-drafts probe of PERSO-139 slice 0 (`docs/tickets/0143-server-drafts.md`).
//!
//! Answers what Outlook and Apple Mail do to a draft another client wrote, by
//! APPENDing one to an account's Drafts folder and dumping what the folder
//! holds after each edit made by hand in those clients.
//!
//! It links the library, loads the account from `config.toml` and opens its
//! session through [`open_imap_session`], the path the daemon takes. It opens
//! no store and takes no engine lock, so it runs beside a
//! live daemon.
//!
//! ```text
//! cargo run --example draft_probe -- append <account>
//! cargo run --example draft_probe -- dump <account> <id> --label "<step>"
//! cargo run --example draft_probe -- list <account> [--structure <uid>]
//! ```
//!
//! `list` is read-only: it EXAMINEs the Drafts folder and prints one line per
//! message, and with `--structure` the MIME tree of one message and the
//! reply or forward header block of its text, without the rest of the body.
//!
//! `append` records the folder's UIDNEXT and UIDVALIDITY before the APPEND in
//! `/var/tmp/mp-draft-probe-<account>-<id>.json`, and keeps the appended bytes
//! beside it as `.eml`. `dump` reads that state back to tell which messages
//! appeared since the APPEND, prints the findings and appends them to
//! `/var/tmp/mp-draft-probe-<account>-<id>.txt`, one block per run.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::io::Write as _;
use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};
use async_imap::types::{Fetch, Flag, NameAttribute};
use clap::{Parser, Subcommand};
use futures::TryStreamExt;
use lettre::message::header::{ContentType, Header, HeaderName, HeaderValue};
use lettre::message::{Attachment, Mailbox, MultiPart, SinglePart};
use lettre::Message;
use serde::{Deserialize, Serialize};

use mailypoppins::config::{
    init_secrets_backend, load_global_config, AccountConfig, EmailSettings, ImapConfig,
};
use mailypoppins::draft::{
    strip_quote_markers, strip_signature_sentinels, QUOTE_MARKER, SIG_END, SIG_START,
};
use mailypoppins::draft_mirror::carrier::{self, SOURCE_HEADER};
use mailypoppins::imap_client::{open_imap_session, ImapSession};
use mailypoppins::send::markdown_to_html;

/// The header that carries the draft's own `id:` in every server copy.
const MARKER_HEADER: &str = "X-Mailypoppins-Draft-Id";

/// How much of each text part a dump prints.
const TEXT_PREVIEW_BYTES: usize = 2048;

/// The flags the probe draft is appended with, as slice 1 will append them.
const APPEND_FLAGS: &str = "(\\Draft \\Seen)";

#[derive(Parser)]
#[command(about = "Probe what other mail clients do to a draft mp wrote (PERSO-139)")]
struct Args {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// APPEND one probe draft to the account's Drafts folder and print its id.
    Append {
        account: String,
        /// Server folder to append to; default: the `\Drafts` folder, then `Drafts`.
        #[arg(long)]
        folder: Option<String>,
        /// To address; default: the account's own address.
        #[arg(long)]
        to: Option<String>,
        /// Cc address; default: the account's own address.
        #[arg(long)]
        cc: Option<String>,
        /// Bcc address; default: the account's own address.
        #[arg(long)]
        bcc: Option<String>,
    },
    /// List every message of the account's Drafts folder, read-only (EXAMINE).
    List {
        account: String,
        /// Server folder to list; default: the `\Drafts` folder, then `Drafts`.
        #[arg(long)]
        folder: Option<String>,
        /// Print this UID's MIME tree and the header block of a quoted or
        /// forwarded message in its text, and none of the rest of its body.
        #[arg(long)]
        structure: Option<u32>,
    },
    /// Dump the copies of one probe draft and everything that appeared since its APPEND.
    Dump {
        account: String,
        id: String,
        /// Free text naming the step, written into the dump's heading.
        #[arg(long)]
        label: Option<String>,
    },
}

/// What `append` records for `dump` to read back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct ProbeState {
    account: String,
    id: String,
    folder: String,
    trash_folder: Option<String>,
    message_id: String,
    in_reply_to: Option<String>,
    uidvalidity: Option<u32>,
    /// UIDNEXT of the folder just before the APPEND: every UID at or above it
    /// appeared since.
    uidnext_before: Option<u32>,
    /// The appended copy's UID, recovered by a Message-ID search.
    uid: Option<u32>,
    appended_at: String,
}

fn state_path(account: &str, id: &str) -> PathBuf {
    PathBuf::from(format!("/var/tmp/mp-draft-probe-{account}-{id}.json"))
}

fn dump_path(account: &str, id: &str) -> PathBuf {
    PathBuf::from(format!("/var/tmp/mp-draft-probe-{account}-{id}.txt"))
}

fn eml_path(account: &str, id: &str) -> PathBuf {
    PathBuf::from(format!("/var/tmp/mp-draft-probe-{account}-{id}.eml"))
}

fn main() -> Result<()> {
    let args = Args::parse();
    let global = load_global_config().context("loading config.toml")?;
    if let Err(e) = init_secrets_backend(&global) {
        // An empty store is not fatal here either; the missing secret surfaces
        // from `ImapConfig::load` with a better message.
        if !matches!(e, mailypoppins::secrets::SecretsError::NotInitialized(_)) {
            bail!("initialising the secrets backend: {e}");
        }
    }
    let account_name = match &args.cmd {
        Cmd::Append { account, .. } | Cmd::Dump { account, .. } | Cmd::List { account, .. } => {
            account.clone()
        }
    };
    let account = global
        .accounts
        .iter()
        .find(|a| a.name == account_name)
        .ok_or_else(|| anyhow!("no account named '{account_name}' in config.toml"))?
        .clone();
    // Read before any login, so a mistyped id fails without touching the server.
    let state = match &args.cmd {
        Cmd::Dump { id, .. } => Some(read_state(&account.name, id)?),
        Cmd::Append { .. } | Cmd::List { .. } => None,
    };
    // Loaded outside the runtime: the secrets backend read blocks.
    let imap = ImapConfig::load(&account)?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let mut session = open_imap_session(&imap).await?;
        let result = match args.cmd {
            Cmd::Append {
                folder,
                to,
                cc,
                bcc,
                ..
            } => append(&mut session, &account, &global.email, folder, [to, cc, bcc]).await,
            Cmd::Dump { label, .. } => {
                let state = state.expect("read above for dump");
                dump(&mut session, &state, label).await
            }
            Cmd::List {
                folder, structure, ..
            } => list(&mut session, folder, structure).await,
        };
        session.logout().await.ok();
        result
    })
}

// ---------------------------------------------------------------------------
// append
// ---------------------------------------------------------------------------

async fn append(
    session: &mut ImapSession,
    account: &AccountConfig,
    email: &EmailSettings,
    folder: Option<String>,
    [to, cc, bcc]: [Option<String>; 3],
) -> Result<()> {
    let listed = list_folders(session).await?;
    let folder = match folder {
        Some(f) => f,
        None => pick_folder(&listed, "Drafts", &["Drafts"]).ok_or_else(|| {
            anyhow!("no \\Drafts folder and no folder named Drafts; pass --folder")
        })?,
    };
    let trash_folder = pick_folder(&listed, "Trash", &["Deleted Items", "Trash"]);

    let own = address_of(&account.default_from)?;
    let inbox = account
        .mailboxes
        .inbox
        .as_ref()
        .map(|m| m.server.clone())
        .unwrap_or_else(|| "INBOX".to_string());
    let parent = newest_message(session, &inbox).await?;
    if parent.is_none() {
        eprintln!("warning: {inbox} is empty, the draft carries no In-Reply-To");
    }

    let id = mailypoppins::store::drafts::new_id();
    let spec = DraftSpec {
        message_id: format!("<mp-probe-{id}@{}>", domain_of(&own)),
        id: id.clone(),
        from: account.default_from.clone(),
        to: to.unwrap_or_else(|| format!("Probe To <{own}>")),
        cc: cc.unwrap_or_else(|| format!("Probe Cc <{own}>")),
        bcc: bcc.unwrap_or_else(|| format!("Probe Bcc <{own}>")),
        parent,
    };
    let raw = build_probe_message(&spec, email)?;

    let before = session
        .examine(&folder)
        .await
        .map_err(|e| anyhow!("EXAMINE {folder}: {e}"))?;
    session
        .append(&folder, Some(APPEND_FLAGS), None, &raw)
        .await
        .map_err(|e| anyhow!("APPEND to {folder}: {e}"))?;
    // async-imap 0.11 drops APPENDUID (src/imap_client/sent.rs), so the UID
    // comes back the way the outbox recovers it, after a fresh EXAMINE so the
    // new message is in the view the SEARCH runs over. A miss is only a
    // missing line in the state: `dump` finds the copy by its headers.
    let uid = match session.examine(&folder).await {
        Ok(_) => session
            .uid_search(format!("HEADER Message-ID \"{}\"", spec.message_id))
            .await
            .ok()
            .and_then(|uids| uids.into_iter().max()),
        Err(_) => None,
    };

    let state = ProbeState {
        account: account.name.clone(),
        id: id.clone(),
        folder,
        trash_folder,
        message_id: spec.message_id.clone(),
        in_reply_to: spec.parent.as_ref().map(|p| p.message_id.clone()),
        uidvalidity: before.uid_validity,
        uidnext_before: before.uid_next,
        uid,
        appended_at: chrono::Local::now().to_rfc3339(),
    };
    fs::write(
        state_path(&account.name, &id),
        serde_json::to_string_pretty(&state)?,
    )?;
    fs::write(eml_path(&account.name, &id), &raw)?;

    eprintln!(
        "appended to {} as UID {} (UIDVALIDITY {}, UIDNEXT before {}), state in {}",
        state.folder,
        opt(state.uid),
        opt(state.uidvalidity),
        opt(state.uidnext_before),
        state_path(&account.name, &id).display()
    );
    println!("{id}");
    Ok(())
}

/// The real message a probe draft replies to.
#[derive(Debug, Clone, PartialEq)]
struct Parent {
    message_id: String,
    references: Option<String>,
    subject: String,
    from: String,
    date: Option<String>,
}

/// Everything one probe draft is built from.
struct DraftSpec {
    id: String,
    message_id: String,
    from: String,
    to: String,
    cc: String,
    bcc: String,
    parent: Option<Parent>,
}

/// The newest message of `mailbox` by UID, read with `EXAMINE` so nothing is
/// marked seen.
async fn newest_message(session: &mut ImapSession, mailbox: &str) -> Result<Option<Parent>> {
    let mbox = session
        .examine(mailbox)
        .await
        .map_err(|e| anyhow!("EXAMINE {mailbox}: {e}"))?;
    if mbox.exists == 0 {
        return Ok(None);
    }
    let fetched: Vec<Fetch> = session
        .uid_fetch("*", "(UID BODY.PEEK[HEADER])")
        .await
        .map_err(|e| anyhow!("UID FETCH * in {mailbox}: {e}"))?
        .try_collect()
        .await
        .map_err(|e| anyhow!("UID FETCH * in {mailbox}: {e}"))?;
    let Some(header) = fetched.iter().find_map(|f| f.header()) else {
        return Ok(None);
    };
    Ok(parent_from_header(header))
}

fn parent_from_header(raw: &[u8]) -> Option<Parent> {
    let (headers, _) = mailparse::parse_headers(raw).ok()?;
    let get = |name: &str| {
        headers
            .iter()
            .find(|h| h.get_key().eq_ignore_ascii_case(name))
            .map(|h| h.get_value().trim().to_string())
            .filter(|v| !v.is_empty())
    };
    Some(Parent {
        message_id: get("Message-ID")?,
        references: get("References"),
        subject: get("Subject").unwrap_or_default(),
        from: get("From").unwrap_or_default(),
        date: get("Date"),
    })
}

/// The draft's Markdown, as mp writes a reply draft: text, a signature block
/// between the sentinels, the quote boundary and a Markdown quote below it.
fn probe_markdown(spec: &DraftSpec) -> String {
    let quote = match &spec.parent {
        Some(p) => format!(
            "> On {}, {} wrote:\n>\n> Probe quote of the original message; its body is not copied.\n",
            p.date.as_deref().unwrap_or("(unknown date)"),
            p.from
        ),
        None => "> Probe quote; the inbox was empty.\n".to_string(),
    };
    format!(
        "Hello,\n\n\
         This is mailypoppins draft probe {id}. Change a word in this sentence.\n\n\
         - a bullet with **bold** and _italic_\n\
         - a [link](https://mailypoppins.dev)\n\n\
         {SIG_START}\n\
         Best,\nSylvain (probe signature)\n\
         {SIG_END}\n\n\
         {QUOTE_MARKER}\n\n\
         {quote}",
        id = spec.id
    )
}

/// The companion `.html` quote a reply draft carries, shaped as the reply
/// builder writes it (`crates/mp-core/src/draft.rs`, the companion block).
fn probe_quote_html(spec: &DraftSpec) -> String {
    let (date, from) = match &spec.parent {
        Some(p) => (
            p.date.clone().unwrap_or_else(|| "(unknown date)".into()),
            p.from.clone(),
        ),
        None => ("(unknown date)".into(), "(empty inbox)".into()),
    };
    format!(
        "<p style=\"color:#666\">On {}, {} wrote:</p>\n\
         <div style=\"margin:0;padding:0 0 0 1em;border-left:2px solid #ccc\">\n\
         <p>Probe quote of the original message; its body is not copied.</p>\n\
         </div>",
        html_escape(&date),
        html_escape(&from)
    )
}

/// The `text/html` part: mp's own rendering ([`markdown_to_html`]), with the
/// signature sentinels and the quote marker kept as HTML comments.
///
/// The renderer strips all three, as a send must. Stand-in comments ride
/// through it instead (it removes only exact sentinel lines and splits on the
/// exact marker) and are swapped back afterwards.
fn probe_html(markdown: &str, quote_html: &str, email: &EmailSettings) -> String {
    const SIG_START_TMP: &str = "<!-- probe:sig-start -->";
    const SIG_END_TMP: &str = "<!-- probe:sig-end -->";
    const QUOTE_TMP: &str = "<!-- probe:quote -->";
    let staged: String = markdown
        .split('\n')
        .map(|line| match line.trim() {
            t if t == SIG_START => SIG_START_TMP.to_string(),
            t if t == SIG_END => SIG_END_TMP.to_string(),
            t if t == QUOTE_MARKER => format!("{QUOTE_TMP}\n\n{QUOTE_MARKER}"),
            _ => line.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n");
    markdown_to_html(&staged, email, None, Some(quote_html))
        .replace(SIG_START_TMP, SIG_START)
        .replace(SIG_END_TMP, SIG_END)
        .replace(QUOTE_TMP, QUOTE_MARKER)
}

#[derive(Clone)]
struct DraftIdHeader(String);

impl Header for DraftIdHeader {
    fn name() -> HeaderName {
        HeaderName::new_from_ascii_str(MARKER_HEADER)
    }

    fn parse(s: &str) -> std::result::Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Ok(Self(s.trim().to_string()))
    }

    fn display(&self) -> HeaderValue {
        HeaderValue::new(Self::name(), self.0.clone())
    }
}

/// The probe draft's bytes: `multipart/mixed` around a
/// `multipart/alternative` of `text/plain` and `text/html`, as
/// `build_draft_message` builds a send, plus one small PDF.
///
/// `build_draft_message` itself does not fit: it refuses a draft that is not
/// approved, mints its own Message-ID, drops Bcc and strips the comments the
/// probe has to carry. The parts are the same: the plain text goes through
/// the same strip, the HTML through the same renderer.
fn build_probe_message(spec: &DraftSpec, email: &EmailSettings) -> Result<Vec<u8>> {
    let markdown = probe_markdown(spec);
    let plain = strip_quote_markers(&strip_signature_sentinels(&markdown));
    let html = probe_html(&markdown, &probe_quote_html(spec), email);

    let subject = match &spec.parent {
        Some(p) if !p.subject.is_empty() => {
            format!("Re: {} (mp probe {})", strip_re(&p.subject), spec.id)
        }
        _ => format!("mp draft probe {}", spec.id),
    };
    let mut builder = Message::builder()
        .from(parse_mailbox(&spec.from, "from")?)
        .to(parse_mailbox(&spec.to, "to")?)
        .cc(parse_mailbox(&spec.cc, "cc")?)
        .bcc(parse_mailbox(&spec.bcc, "bcc")?)
        .subject(subject)
        .message_id(Some(spec.message_id.clone()))
        .header(DraftIdHeader(spec.id.clone()))
        .keep_bcc();
    if let Some(p) = &spec.parent {
        let references = match &p.references {
            Some(r) => format!("{r} {}", p.message_id),
            None => p.message_id.clone(),
        };
        builder = builder
            .in_reply_to(p.message_id.clone())
            .references(references);
    }

    let alternative = MultiPart::alternative()
        .singlepart(
            SinglePart::builder()
                .header(ContentType::TEXT_PLAIN)
                .body(plain),
        )
        .singlepart(
            SinglePart::builder()
                .header(ContentType::TEXT_HTML)
                .body(html),
        );
    let pdf = Attachment::new(format!("mp-probe-{}.pdf", spec.id))
        .body(probe_pdf(&spec.id), ContentType::parse("application/pdf")?);
    let message = builder
        .multipart(MultiPart::mixed().multipart(alternative).singlepart(pdf))
        .context("building the probe draft")?;
    Ok(message.formatted())
}

/// A one-page PDF saying which probe it belongs to, with a correct xref table.
///
/// The second line is the usual binary comment, which makes lettre encode the
/// part as base64 the way it encodes a real PDF, rather than as
/// quoted-printable.
fn probe_pdf(id: &str) -> Vec<u8> {
    let text = format!("(mailypoppins draft probe {id}) Tj");
    let stream = format!("BT /F1 18 Tf 72 720 Td {text} ET");
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
         /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>"
            .to_string(),
        format!(
            "<< /Length {} >>\nstream\n{stream}\nendstream",
            stream.len()
        ),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
    ];
    let mut out = b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offsets = Vec::new();
    for (n, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", n + 1).as_bytes());
    }
    let xref = out.len();
    let mut table = format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1);
    for off in offsets {
        let _ = writeln!(table, "{off:010} 00000 n ");
    }
    let _ = write!(
        table,
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
        objects.len() + 1
    );
    out.extend_from_slice(table.as_bytes());
    out
}

// ---------------------------------------------------------------------------
// dump
// ---------------------------------------------------------------------------

fn read_state(account: &str, id: &str) -> Result<ProbeState> {
    let path = state_path(account, id);
    serde_json::from_str(
        &fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?,
    )
    .with_context(|| format!("parsing {}", path.display()))
}

async fn dump(session: &mut ImapSession, state: &ProbeState, label: Option<String>) -> Result<()> {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "=== mp draft probe dump: account {} id {} at {}{} ===",
        state.account,
        state.id,
        chrono::Local::now().to_rfc3339(),
        label.map(|l| format!(" ({l})")).unwrap_or_default()
    );
    let _ = writeln!(
        out,
        "appended {} to {} as UID {} under UIDVALIDITY {}, UIDNEXT before {}, Message-ID {}, In-Reply-To {}",
        state.appended_at,
        state.folder,
        opt(state.uid),
        opt(state.uidvalidity),
        opt(state.uidnext_before),
        state.message_id,
        state.in_reply_to.as_deref().unwrap_or("(none)")
    );

    match session.capabilities().await {
        Ok(caps) => {
            // A set on the client side: sorted so two dumps diff cleanly.
            let mut tokens: Vec<String> = caps.iter().map(capability_token).collect();
            tokens.sort();
            let _ = writeln!(out, "CAPABILITY {}", tokens.join(" "));
            for cap in ["UIDPLUS", "CONDSTORE", "QRESYNC", "SPECIAL-USE", "MOVE"] {
                let _ = writeln!(out, "  {cap}: {}", yes_no(caps.has_str(cap)));
            }
        }
        Err(e) => {
            let _ = writeln!(out, "CAPABILITY failed: {e}");
        }
    }

    match list_folders(session).await {
        Ok(listed) => {
            let _ = writeln!(out, "LIST:");
            for (name, attrs) in &listed {
                let mark = if *name == state.folder || Some(name) == state.trash_folder.as_ref() {
                    "*"
                } else {
                    " "
                };
                let _ = writeln!(out, " {mark} {name} ({})", attrs.join(" "));
            }
        }
        Err(e) => {
            let _ = writeln!(out, "LIST failed: {e:#}");
        }
    }

    dump_folder(session, state, &state.folder, true, &mut out).await;
    if let Some(trash) = &state.trash_folder {
        dump_folder(session, state, trash, false, &mut out).await;
    }

    print!("{out}");
    let target = dump_path(&state.account, &state.id);
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&target)
        .with_context(|| format!("opening {}", target.display()))?;
    writeln!(file, "{out}")?;
    eprintln!("appended to {}", target.display());
    Ok(())
}

/// Dump one folder: the server's own SEARCH answers, then every message that
/// matches the draft by marker or Message-ID, plus, in the Drafts folder,
/// every message that appeared since the APPEND.
///
/// Matching is done client-side over every header in the folder, so a server
/// whose `SEARCH HEADER` ignores an X- header still yields the copies; what
/// the server's SEARCH answered is printed beside it as a finding of its own.
async fn dump_folder(
    session: &mut ImapSession,
    state: &ProbeState,
    folder: &str,
    with_since: bool,
    out: &mut String,
) {
    let _ = writeln!(out, "\n--- folder {folder} ---");
    let mbox = match session.examine(folder).await {
        Ok(m) => m,
        Err(e) => {
            let _ = writeln!(out, "EXAMINE failed: {e}");
            return;
        }
    };
    let _ = writeln!(
        out,
        "EXISTS {} UIDVALIDITY {} UIDNEXT {} HIGHESTMODSEQ {}",
        mbox.exists,
        opt(mbox.uid_validity),
        opt(mbox.uid_next),
        opt(mbox.highest_modseq)
    );
    let same_validity = mbox.uid_validity == state.uidvalidity;
    if with_since && !same_validity {
        let _ = writeln!(
            out,
            "UIDVALIDITY changed since the APPEND ({} then): the since-append match is off, only marker and Message-ID matches are listed",
            opt(state.uidvalidity)
        );
    }

    for (what, query) in [
        (
            "SEARCH HEADER Message-ID",
            format!("HEADER Message-ID \"{}\"", state.message_id),
        ),
        (
            "SEARCH HEADER marker",
            format!("HEADER {MARKER_HEADER} \"{}\"", state.id),
        ),
    ] {
        match session.uid_search(&query).await {
            Ok(uids) => {
                let uids: BTreeSet<u32> = uids.into_iter().collect();
                let _ = writeln!(out, "{what}: {uids:?}");
            }
            Err(e) => {
                let _ = writeln!(out, "{what} failed: {e}");
            }
        }
    }

    if mbox.exists == 0 {
        let _ = writeln!(out, "(empty)");
        return;
    }
    let headers: Vec<Fetch> = match fetch_all(session, "1:*", "(UID BODY.PEEK[HEADER])").await {
        Ok(f) => f,
        Err(e) => {
            let _ = writeln!(out, "header scan failed: {e:#}");
            return;
        }
    };
    let since = if with_since && same_validity {
        state.uidnext_before
    } else {
        None
    };
    let mut selected: BTreeMap<u32, Vec<&'static str>> = BTreeMap::new();
    for f in &headers {
        let (Some(uid), Some(header)) = (f.uid, f.header()) else {
            continue;
        };
        let reasons = match_reasons(header, uid, state, since);
        if !reasons.is_empty() {
            selected.insert(uid, reasons);
        }
    }
    if selected.is_empty() {
        let _ = writeln!(
            out,
            "no message matches the draft or appeared since the APPEND"
        );
        return;
    }

    let set = selected
        .keys()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let fetched = match fetch_all(
        session,
        &set,
        "(UID FLAGS INTERNALDATE BODYSTRUCTURE BODY.PEEK[])",
    )
    .await
    {
        Ok(f) => f,
        Err(e) => {
            let _ = writeln!(out, "UID FETCH {set} failed: {e:#}");
            return;
        }
    };
    for f in &fetched {
        let Some(uid) = f.uid else { continue };
        let probe = ProbeMessage {
            uid,
            uidvalidity: mbox.uid_validity,
            reasons: selected.get(&uid).cloned().unwrap_or_default(),
            flags: f.flags().map(|fl| flag_token(&fl)).collect(),
            internal_date: f.internal_date().map(|d| d.to_rfc3339()),
            bodystructure: f.bodystructure().map(|b| format!("{b:?}")),
            raw: f.body().map(<[u8]>::to_vec),
        };
        out.push_str(&format_message(&probe));
    }
}

async fn fetch_all(session: &mut ImapSession, set: &str, query: &str) -> Result<Vec<Fetch>> {
    session
        .uid_fetch(set, query)
        .await
        .map_err(|e| anyhow!("UID FETCH {set} {query}: {e}"))?
        .try_collect()
        .await
        .map_err(|e| anyhow!("UID FETCH {set} {query}: {e}"))
}

/// Why a message is part of the dump: its marker or Message-ID names the
/// draft, or it appeared at or after the folder's UIDNEXT of the APPEND.
fn match_reasons(
    header: &[u8],
    uid: u32,
    state: &ProbeState,
    since: Option<u32>,
) -> Vec<&'static str> {
    let mut reasons = Vec::new();
    if let Ok((headers, _)) = mailparse::parse_headers(header) {
        let has = |name: &str, want: &str| {
            headers
                .iter()
                .any(|h| h.get_key().eq_ignore_ascii_case(name) && h.get_value().trim() == want)
        };
        if has(MARKER_HEADER, &state.id) {
            reasons.push("marker");
        }
        if has("Message-ID", &state.message_id) {
            reasons.push("message-id");
        }
    }
    if since.is_some_and(|s| uid >= s) {
        reasons.push("since-append");
    }
    reasons
}

/// One fetched message, as the dump needs it.
struct ProbeMessage {
    uid: u32,
    uidvalidity: Option<u32>,
    reasons: Vec<&'static str>,
    flags: Vec<String>,
    internal_date: Option<String>,
    bodystructure: Option<String>,
    raw: Option<Vec<u8>>,
}

/// The dump of one message: its IMAP attributes, the headers the linking
/// rule depends on, its MIME tree and the head of each text part.
fn format_message(m: &ProbeMessage) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "\n## UID {} (UIDVALIDITY {}) matched by {}",
        m.uid,
        opt(m.uidvalidity),
        m.reasons.join(", ")
    );
    let _ = writeln!(out, "FLAGS ({})", m.flags.join(" "));
    let has_flag = |f: &str| m.flags.iter().any(|x| x.eq_ignore_ascii_case(f));
    let _ = writeln!(
        out,
        "  \\Draft: {}  \\Seen: {}  \\Deleted: {}",
        yes_no(has_flag("\\Draft")),
        yes_no(has_flag("\\Seen")),
        yes_no(has_flag("\\Deleted"))
    );
    let _ = writeln!(
        out,
        "INTERNALDATE {}",
        m.internal_date.as_deref().unwrap_or("(absent)")
    );
    let _ = writeln!(
        out,
        "BODYSTRUCTURE {}",
        m.bodystructure.as_deref().unwrap_or("(absent)")
    );
    let Some(raw) = &m.raw else {
        let _ = writeln!(out, "(no body returned)");
        return out;
    };
    let parsed = match mailparse::parse_mail(raw) {
        Ok(p) => p,
        Err(e) => {
            let _ = writeln!(out, "(body does not parse: {e})");
            return out;
        }
    };
    for name in [
        "Message-ID",
        MARKER_HEADER,
        "In-Reply-To",
        "References",
        "From",
        "To",
        "Cc",
        "Bcc",
        "Subject",
        "Date",
    ] {
        let values: Vec<String> = parsed
            .headers
            .iter()
            .filter(|h| h.get_key().eq_ignore_ascii_case(name))
            .map(|h| h.get_value().trim().to_string())
            .collect();
        let shown = if values.is_empty() {
            "(absent)".to_string()
        } else {
            values.join(" | ")
        };
        let _ = writeln!(out, "{name}: {shown}");
    }
    let _ = writeln!(out, "{}", source_line(&parsed));
    let _ = writeln!(out, "{}", carrier_line(raw));
    let _ = writeln!(out, "MIME:");
    mime_tree(&parsed, "1", 1, &mut out);
    text_parts(&parsed, "1", &mut out);
    out
}

/// Whether the copy carries the draft file in `X-Mailypoppins-Draft-Source`
/// headers, and how much of it: the server may have dropped them.
fn source_line(parsed: &mailparse::ParsedMail<'_>) -> String {
    let values: Vec<String> = parsed
        .headers
        .iter()
        .filter(|h| h.get_key().eq_ignore_ascii_case(SOURCE_HEADER))
        .map(|h| h.get_value())
        .collect();
    if values.is_empty() {
        return format!("{SOURCE_HEADER}: (absent)");
    }
    let bytes: usize = values.iter().map(String::len).sum();
    let chunks: Vec<&str> = values
        .iter()
        .filter_map(|v| v.split_whitespace().next())
        .collect();
    format!(
        "{SOURCE_HEADER}: {} header(s), {bytes} bytes, chunks {}",
        values.len(),
        chunks.join(" ")
    )
}

/// What the drafts mirror reads off the copy (`draft_mirror::carrier::read`):
/// mp's own unedited copy with or without its source, or one edited
/// elsewhere.
fn carrier_line(raw: &[u8]) -> String {
    match carrier::read(raw) {
        Some(copy) => format!(
            "carrier: mp's copy of file {}, source {}",
            short_hash(&copy.file_hash),
            copy.source
                .map(|s| format!("{} bytes", s.len()))
                .unwrap_or_else(|| "(not carried)".to_string())
        ),
        None => "carrier: none (edited elsewhere, or not mp's)".to_string(),
    }
}

/// The `mp:copy` mark a `text/html` part ends with, if it is mp's.
fn copy_mark_line(body: &str) -> String {
    match mailypoppins::parse::draft_copy_mark(body) {
        Some(mark) => format!(
            "mp:copy mark: file={} src={}",
            short_hash(&mark.file_hash),
            mark.source_hash
                .as_deref()
                .map(short_hash)
                .unwrap_or("(none)")
        ),
        None if body.contains("mp:copy") => {
            "mp:copy mark: no (an mp:copy comment that is not the part's last)".to_string()
        }
        None => "mp:copy mark: no".to_string(),
    }
}

fn short_hash(hash: &str) -> &str {
    head(hash, 12)
}

/// The MIME tree, one part per line: path, type, charset, encoding,
/// disposition and filename, decoded size.
fn mime_tree(part: &mailparse::ParsedMail<'_>, path: &str, depth: usize, out: &mut String) {
    let encoding = header_of(part, "Content-Transfer-Encoding").unwrap_or_default();
    let disposition = part.get_content_disposition();
    let filename = disposition
        .params
        .get("filename")
        .cloned()
        .or_else(|| part.ctype.params.get("name").cloned());
    let size = part.get_body_raw().map(|b| b.len()).unwrap_or(0);
    let _ = writeln!(
        out,
        "{}{path} {} charset={} encoding={} disposition={:?}{} {} bytes",
        "  ".repeat(depth),
        part.ctype.mimetype,
        part.ctype.charset,
        if encoding.is_empty() { "-" } else { &encoding },
        disposition.disposition,
        filename
            .map(|f| format!(" filename={f}"))
            .unwrap_or_default(),
        size
    );
    for (n, sub) in part.subparts.iter().enumerate() {
        mime_tree(sub, &format!("{path}.{}", n + 1), depth + 1, out);
    }
}

/// The first [`TEXT_PREVIEW_BYTES`] of every inline `text/*` part, and which
/// of mp's HTML comments survived in each `text/html` part.
fn text_parts(part: &mailparse::ParsedMail<'_>, path: &str, out: &mut String) {
    if part.subparts.is_empty() {
        let attachment = matches!(
            part.get_content_disposition().disposition,
            mailparse::DispositionType::Attachment
        );
        if part.ctype.mimetype.starts_with("text/") && !attachment {
            let body = part.get_body().unwrap_or_default();
            if part.ctype.mimetype == "text/html" {
                let _ = writeln!(
                    out,
                    "{path} comments: mp:quote {}  mp:sig-start {}  mp:sig-end {}",
                    yes_no(body.contains(QUOTE_MARKER)),
                    yes_no(body.contains(SIG_START)),
                    yes_no(body.contains(SIG_END))
                );
                let _ = writeln!(out, "{path} {}", copy_mark_line(&body));
            }
            let _ = writeln!(
                out,
                "{path} {} (first {} of {} bytes):\n```\n{}\n```",
                part.ctype.mimetype,
                TEXT_PREVIEW_BYTES.min(body.len()),
                body.len(),
                head(&body, TEXT_PREVIEW_BYTES)
            );
        }
        return;
    }
    for (n, sub) in part.subparts.iter().enumerate() {
        text_parts(sub, &format!("{path}.{}", n + 1), out);
    }
}

// ---------------------------------------------------------------------------
// list
// ---------------------------------------------------------------------------

/// One line per message of the Drafts folder, over EXAMINE so nothing is
/// marked or moved; with `structure`, that message's shape.
async fn list(
    session: &mut ImapSession,
    folder: Option<String>,
    structure: Option<u32>,
) -> Result<()> {
    let listed = list_folders(session).await?;
    let folder = match folder {
        Some(f) => f,
        None => pick_folder(&listed, "Drafts", &["Drafts"])
            .ok_or_else(|| anyhow!("no \\Drafts folder and no folder named Drafts"))?,
    };
    let attrs = listed
        .iter()
        .find(|(name, _)| *name == folder)
        .map(|(_, attrs)| attrs.join(" "))
        .unwrap_or_default();
    let mbox = session
        .examine(&folder)
        .await
        .map_err(|e| anyhow!("EXAMINE {folder}: {e}"))?;
    println!(
        "folder {folder} ({attrs}): EXISTS {} UIDVALIDITY {} UIDNEXT {}",
        mbox.exists,
        opt(mbox.uid_validity),
        opt(mbox.uid_next)
    );
    if mbox.exists == 0 {
        return Ok(());
    }
    let set = structure.map_or_else(|| "1:*".to_string(), |uid| uid.to_string());
    let fetched = fetch_all(session, &set, "(UID FLAGS INTERNALDATE BODY.PEEK[])").await?;
    for f in &fetched {
        let Some(uid) = f.uid else { continue };
        let Some(raw) = f.body() else { continue };
        let flags: Vec<String> = f.flags().map(|fl| flag_token(&fl)).collect();
        let Ok(parsed) = mailparse::parse_mail(raw) else {
            println!("UID {uid} ({}) does not parse", flags.join(" "));
            continue;
        };
        let get = |name: &str| header_of(&parsed, name).unwrap_or_else(|| "(absent)".into());
        println!(
            "UID {uid} ({}) {} | Message-ID {} | Subject {} | {}: {} | {} | {} | MIME {}",
            flags.join(" "),
            f.internal_date()
                .map(|d| d.to_rfc3339())
                .unwrap_or_default(),
            get("Message-ID"),
            get("Subject"),
            MARKER_HEADER,
            get(MARKER_HEADER),
            source_line(&parsed),
            carrier_line(raw),
            mime_shape(&parsed)
        );
        if structure == Some(uid) {
            print!("{}", structure_of(&parsed));
        }
    }
    Ok(())
}

/// The tags of an HTML body from `<body` on, each text run replaced by its
/// length in brackets, so the layout prints and the words do not.
fn tag_skeleton(html: &str) -> String {
    let start = html.find("<body").unwrap_or(0);
    let mut out = String::new();
    let mut rest = &html[start..];
    while let Some(open) = rest.find('<') {
        let text = rest[..open].trim();
        if !text.is_empty() && text != "&nbsp;" {
            let _ = write!(out, "[{}]", text.len());
        } else if text == "&nbsp;" {
            out.push_str("&nbsp;");
        }
        let Some(close) = rest[open..].find('>') else {
            break;
        };
        let tag = &rest[open..open + close + 1];
        if tag.starts_with("<p") || tag.starts_with("<div") || tag.starts_with("<hr") {
            out.push('\n');
        }
        out.push_str(tag);
        rest = &rest[open + close + 1..];
    }
    out
}

/// A MIME tree on one line: `multipart/mixed[text/html, message/rfc822]`.
fn mime_shape(part: &mailparse::ParsedMail<'_>) -> String {
    if part.subparts.is_empty() {
        return part.ctype.mimetype.clone();
    }
    format!(
        "{}[{}]",
        part.ctype.mimetype,
        part.subparts
            .iter()
            .map(mime_shape)
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// Whether a text line is one of a quoted message's header lines, in the
/// English or German Outlook and Apple Mail spell them.
fn header_block_line(line: &str) -> bool {
    const NAMES: [&str; 14] = [
        "From:",
        "Sent:",
        "To:",
        "Cc:",
        "Subject:",
        "Date:",
        "Von:",
        "Gesendet:",
        "An:",
        "Betreff:",
        "Datum:",
        "Begin forwarded message:",
        "Anfang der weitergeleiteten",
        "Original Message",
    ];
    let t = line.trim_start_matches(['>', ' ', '*']).trim();
    NAMES
        .iter()
        .any(|n| t.starts_with(n) || t.contains("-----Original"))
}

/// A separator line: underscores, dashes, or the probe's `<hr>` stand-in.
fn separator_line(line: &str) -> bool {
    let t = line.trim();
    t.len() >= 5 && t.chars().all(|c| matches!(c, '_' | '-' | '─' | '='))
}

/// The shape of one message: its MIME tree, its header names, and for each
/// text part the quote wrappers its HTML carries and the header block of the
/// message it quotes or forwards. Lines of the body that are neither are
/// counted, not printed.
fn structure_of(parsed: &mailparse::ParsedMail<'_>) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "  MIME:");
    mime_tree(parsed, "1", 2, &mut out);
    let names: Vec<String> = parsed.headers.iter().map(|h| h.get_key()).collect();
    let _ = writeln!(out, "  header names: {}", names.join(", "));
    for name in ["In-Reply-To", "References", "Thread-Topic", "Thread-Index"] {
        let _ = writeln!(
            out,
            "  {name}: {}",
            if header_of(parsed, name).is_some() {
                "present"
            } else {
                "absent"
            }
        );
    }
    structure_parts(parsed, "1", &mut out);
    out
}

fn structure_parts(part: &mailparse::ParsedMail<'_>, path: &str, out: &mut String) {
    if part.ctype.mimetype == "message/rfc822" {
        let inner = part.get_body_raw().unwrap_or_default();
        if let Ok(inner) = mailparse::parse_mail(&inner) {
            let names: Vec<String> = inner.headers.iter().map(|h| h.get_key()).collect();
            let _ = writeln!(
                out,
                "  {path} message/rfc822: {} header(s) ({}), MIME {}",
                names.len(),
                names.join(", "),
                mime_shape(&inner)
            );
        }
        return;
    }
    if part.subparts.is_empty() {
        let attachment = matches!(
            part.get_content_disposition().disposition,
            mailparse::DispositionType::Attachment
        );
        if !part.ctype.mimetype.starts_with("text/") || attachment {
            return;
        }
        let body = part.get_body().unwrap_or_default();
        let text = if part.ctype.mimetype == "text/html" {
            let wrappers: Vec<&str> = [
                "divRplyFwdMsg",
                "appendonsend",
                "<hr",
                "border-top:solid",
                "type=\"cite\"",
                "<blockquote",
                "OutlookMessageHeader",
                "x_divRplyFwdMsg",
                "mail-editor-reference-message-container",
                "solid none none",
            ]
            .into_iter()
            .filter(|w| body.contains(w))
            .collect();
            let _ = writeln!(
                out,
                "  {path} text/html, {} bytes, wrappers: {}, reply boundary at {}",
                body.len(),
                if wrappers.is_empty() {
                    "none".to_string()
                } else {
                    wrappers.join(" ")
                },
                opt(mailypoppins::parse::draft_reply_boundary(&body))
            );
            let _ = writeln!(out, "  {path} tag skeleton of <body>, text as [bytes]:");
            let _ = writeln!(out, "{}", tag_skeleton(&body));
            mailypoppins::parse::html_to_plain(&body)
        } else {
            let _ = writeln!(
                out,
                "  {path} {}, {} bytes",
                part.ctype.mimetype,
                body.len()
            );
            body
        };
        let mut skipped = 0usize;
        for (n, line) in text.lines().enumerate() {
            if header_block_line(line) || separator_line(line) {
                if skipped > 0 {
                    let _ = writeln!(out, "    ({skipped} other non-empty line(s))");
                    skipped = 0;
                }
                let _ = writeln!(out, "    line {}: {}", n + 1, line.trim_end());
            } else if !line.trim().is_empty() {
                skipped += 1;
            }
        }
        if skipped > 0 {
            let _ = writeln!(out, "    ({skipped} other non-empty line(s))");
        }
        return;
    }
    for (n, sub) in part.subparts.iter().enumerate() {
        structure_parts(sub, &format!("{path}.{}", n + 1), out);
    }
}

// ---------------------------------------------------------------------------
// small helpers
// ---------------------------------------------------------------------------

/// Every folder `LIST "" "*"` returns, with its attributes as `\Name` tokens.
async fn list_folders(session: &mut ImapSession) -> Result<Vec<(String, Vec<String>)>> {
    let names: Vec<_> = session
        .list(Some(""), Some("*"))
        .await
        .map_err(|e| anyhow!("LIST: {e}"))?
        .try_collect()
        .await
        .map_err(|e| anyhow!("LIST: {e}"))?;
    Ok(names
        .iter()
        .map(|n| {
            (
                n.name().to_string(),
                n.attributes().iter().map(attribute_token).collect(),
            )
        })
        .collect())
}

/// The folder carrying the SPECIAL-USE attribute `\<special>`, else the first
/// whose name is one of `names`.
fn pick_folder(listed: &[(String, Vec<String>)], special: &str, names: &[&str]) -> Option<String> {
    let token = format!("\\{special}");
    listed
        .iter()
        .find(|(_, attrs)| attrs.iter().any(|a| a.eq_ignore_ascii_case(&token)))
        .or_else(|| {
            names.iter().find_map(|want| {
                listed
                    .iter()
                    .find(|(name, _)| name.eq_ignore_ascii_case(want))
            })
        })
        .map(|(name, _)| name.clone())
}

fn attribute_token(attribute: &NameAttribute<'_>) -> String {
    match attribute {
        NameAttribute::NoInferiors => "\\Noinferiors".to_string(),
        NameAttribute::NoSelect => "\\Noselect".to_string(),
        NameAttribute::Extension(name) => format!("\\{}", name.trim_start_matches('\\')),
        other => format!("\\{other:?}"),
    }
}

fn capability_token(cap: &async_imap::types::Capability) -> String {
    use async_imap::types::Capability;
    match cap {
        Capability::Imap4rev1 => "IMAP4rev1".to_string(),
        Capability::Auth(a) => format!("AUTH={a}"),
        Capability::Atom(a) => a.to_string(),
    }
}

fn flag_token(flag: &Flag<'_>) -> String {
    match flag {
        Flag::Seen => "\\Seen".to_string(),
        Flag::Answered => "\\Answered".to_string(),
        Flag::Flagged => "\\Flagged".to_string(),
        Flag::Deleted => "\\Deleted".to_string(),
        Flag::Draft => "\\Draft".to_string(),
        Flag::Recent => "\\Recent".to_string(),
        Flag::MayCreate => "\\*".to_string(),
        Flag::Custom(c) => c.to_string(),
    }
}

fn header_of(part: &mailparse::ParsedMail<'_>, name: &str) -> Option<String> {
    part.headers
        .iter()
        .find(|h| h.get_key().eq_ignore_ascii_case(name))
        .map(|h| h.get_value().trim().to_string())
}

fn parse_mailbox(s: &str, what: &str) -> Result<Mailbox> {
    s.trim()
        .parse()
        .with_context(|| format!("invalid {what} address '{s}'"))
}

/// The bare address of an `Name <addr>` or `addr` string.
fn address_of(s: &str) -> Result<String> {
    Ok(parse_mailbox(s, "default_from")?.email.to_string())
}

fn domain_of(address: &str) -> &str {
    address
        .rsplit_once('@')
        .map(|(_, d)| d)
        .unwrap_or("localhost")
}

fn strip_re(subject: &str) -> &str {
    let t = subject.trim_start();
    if t.get(..3).is_some_and(|p| p.eq_ignore_ascii_case("re:")) {
        t[3..].trim_start()
    } else {
        t
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The first `max` bytes of `s`, cut back to a char boundary.
fn head(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

fn opt<T: std::fmt::Display>(v: Option<T>) -> String {
    v.map(|v| v.to_string()).unwrap_or_else(|| "(none)".into())
}

fn yes_no(b: bool) -> &'static str {
    if b {
        "yes"
    } else {
        "no"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parent() -> Parent {
        Parent {
            message_id: "<parent@example.org>".into(),
            references: Some("<root@example.org>".into()),
            subject: "Re: Site visit".into(),
            from: "Jane Doe <jane@example.org>".into(),
            date: Some("Mon, 12 Oct 2026 09:00:00 +0200".into()),
        }
    }

    fn spec() -> DraftSpec {
        DraftSpec {
            id: "a0123456789abcde".into(),
            message_id: "<mp-probe-a0123456789abcde@example.org>".into(),
            from: "Sylvain <me@example.org>".into(),
            to: "Probe To <me@example.org>".into(),
            cc: "Probe Cc <me@example.org>".into(),
            bcc: "Probe Bcc <me@example.org>".into(),
            parent: Some(parent()),
        }
    }

    fn state() -> ProbeState {
        ProbeState {
            account: "tum".into(),
            id: "a0123456789abcde".into(),
            folder: "Drafts".into(),
            trash_folder: Some("Deleted Items".into()),
            message_id: "<mp-probe-a0123456789abcde@example.org>".into(),
            in_reply_to: Some("<parent@example.org>".into()),
            uidvalidity: Some(7),
            uidnext_before: Some(40),
            uid: Some(40),
            appended_at: "2026-10-13T10:00:00+02:00".into(),
        }
    }

    fn header(raw: &[u8], name: &str) -> Option<String> {
        let parsed = mailparse::parse_mail(raw).unwrap();
        header_of(&parsed, name)
    }

    #[test]
    fn the_draft_carries_every_header_the_ticket_names() {
        let raw = build_probe_message(&spec(), &EmailSettings::default()).unwrap();
        assert_eq!(
            header(&raw, "Message-ID").as_deref(),
            Some("<mp-probe-a0123456789abcde@example.org>")
        );
        assert_eq!(
            header(&raw, MARKER_HEADER).as_deref(),
            Some("a0123456789abcde")
        );
        assert_eq!(
            header(&raw, "In-Reply-To").as_deref(),
            Some("<parent@example.org>")
        );
        assert_eq!(
            header(&raw, "References").as_deref(),
            Some("<root@example.org> <parent@example.org>")
        );
        assert!(header(&raw, "To").unwrap().contains("Probe To"));
        assert!(header(&raw, "Cc").unwrap().contains("Probe Cc"));
        assert!(header(&raw, "Bcc").unwrap().contains("Probe Bcc"));
        assert_eq!(
            header(&raw, "Subject").as_deref(),
            Some("Re: Site visit (mp probe a0123456789abcde)")
        );
    }

    #[test]
    fn the_draft_is_mixed_around_an_alternative_and_a_pdf() {
        let raw = build_probe_message(&spec(), &EmailSettings::default()).unwrap();
        let parsed = mailparse::parse_mail(&raw).unwrap();
        assert_eq!(parsed.ctype.mimetype, "multipart/mixed");
        assert_eq!(parsed.subparts.len(), 2);
        let alt = &parsed.subparts[0];
        assert_eq!(alt.ctype.mimetype, "multipart/alternative");
        let kinds: Vec<&str> = alt
            .subparts
            .iter()
            .map(|p| p.ctype.mimetype.as_str())
            .collect();
        assert_eq!(kinds, ["text/plain", "text/html"]);
        let pdf = &parsed.subparts[1];
        assert_eq!(pdf.ctype.mimetype, "application/pdf");
        assert_eq!(
            header_of(pdf, "Content-Transfer-Encoding").as_deref(),
            Some("base64")
        );
        assert!(pdf.get_body_raw().unwrap().starts_with(b"%PDF-1.4"));

        let plain = alt.subparts[0].get_body().unwrap();
        assert!(plain.contains("draft probe a0123456789abcde"));
        assert!(!plain.contains("mp:quote") && !plain.contains("mp:sig"));

        let html = alt.subparts[1].get_body().unwrap();
        assert!(html.contains(QUOTE_MARKER), "{html}");
        assert!(html.contains(SIG_START) && html.contains(SIG_END), "{html}");
        assert!(!html.contains("probe:"), "a stand-in leaked: {html}");
        // The marker sits between the reply and the companion quote.
        let marker = html.find(QUOTE_MARKER).unwrap();
        assert!(html.find("Change a word").unwrap() < marker);
        assert!(html.find("Probe quote of the original").unwrap() > marker);
        assert!(html.find(SIG_START).unwrap() < html.find("Sylvain (probe").unwrap());
        assert!(html.find("Sylvain (probe").unwrap() < html.find(SIG_END).unwrap());
    }

    #[test]
    fn a_draft_without_a_parent_is_not_a_reply() {
        let mut s = spec();
        s.parent = None;
        let raw = build_probe_message(&s, &EmailSettings::default()).unwrap();
        assert_eq!(header(&raw, "In-Reply-To"), None);
        assert_eq!(
            header(&raw, "Subject").as_deref(),
            Some("mp draft probe a0123456789abcde")
        );
    }

    #[test]
    fn the_pdf_xref_points_at_its_objects() {
        let pdf = probe_pdf("a0123456789abcde");
        let text = String::from_utf8_lossy(&pdf[15..]);
        let xref_at: usize = text
            .rsplit("startxref\n")
            .next()
            .unwrap()
            .lines()
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert!(pdf[xref_at..].starts_with(b"xref\n0 6\n"));
        let table = String::from_utf8_lossy(&pdf[xref_at..]);
        for (n, line) in table.lines().skip(3).take(5).enumerate() {
            let off: usize = line[..10].parse().unwrap();
            assert!(pdf[off..].starts_with(format!("{} 0 obj", n + 1).as_bytes()));
        }
    }

    #[test]
    fn the_parent_is_read_off_a_header_block() {
        let raw = b"From: Jane <jane@example.org>\r\nSubject: Hi\r\nMessage-ID: <p@x>\r\nDate: Tue, 13 Oct 2026 08:00:00 +0200\r\n\r\n";
        let p = parent_from_header(raw).unwrap();
        assert_eq!(p.message_id, "<p@x>");
        assert_eq!(p.references, None);
        assert_eq!(p.subject, "Hi");
        assert!(parent_from_header(b"Subject: no id\r\n\r\n").is_none());
    }

    #[test]
    fn matching_names_marker_message_id_and_arrival() {
        let st = state();
        let ours = format!(
            "Message-ID: {}\r\n{MARKER_HEADER}: {}\r\n\r\n",
            st.message_id, st.id
        );
        assert_eq!(
            match_reasons(ours.as_bytes(), 40, &st, Some(40)),
            ["marker", "message-id", "since-append"]
        );
        // An edit that kept the marker under a new Message-ID.
        let edited = format!("Message-ID: <new@x>\r\n{MARKER_HEADER}: {}\r\n\r\n", st.id);
        assert_eq!(match_reasons(edited.as_bytes(), 41, &st, None), ["marker"]);
        // A foreign draft that arrived after the APPEND.
        let foreign = b"Message-ID: <outlook@x>\r\n\r\n";
        assert_eq!(match_reasons(foreign, 45, &st, Some(40)), ["since-append"]);
        assert!(match_reasons(foreign, 12, &st, Some(40)).is_empty());
    }

    #[test]
    fn the_folder_is_picked_by_special_use_then_by_name() {
        let listed = vec![
            ("INBOX".to_string(), vec![]),
            ("Entwürfe".to_string(), vec!["\\Drafts".to_string()]),
            ("Drafts".to_string(), vec![]),
            ("Deleted Items".to_string(), vec![]),
        ];
        assert_eq!(
            pick_folder(&listed, "Drafts", &["Drafts"]).as_deref(),
            Some("Entwürfe")
        );
        assert_eq!(
            pick_folder(&listed, "Trash", &["Deleted Items", "Trash"]).as_deref(),
            Some("Deleted Items")
        );
        assert_eq!(pick_folder(&listed, "Junk", &["Spam"]), None);
    }

    #[test]
    fn the_state_round_trips() {
        let st = state();
        let back: ProbeState =
            serde_json::from_str(&serde_json::to_string_pretty(&st).unwrap()).unwrap();
        assert_eq!(back, st);
    }

    /// A canned FETCH: the attributes a server returns for an Outlook-edited
    /// copy, which dropped the alternative and the marker.
    #[test]
    fn the_dump_of_one_message_names_every_item() {
        let raw = b"Message-ID: <outlook-rewrite@x>\r\n\
In-Reply-To: <parent@example.org>\r\n\
To: Probe To <me@example.org>, New <new@example.org>\r\n\
Subject: Re: Site visit\r\n\
Content-Type: multipart/mixed; boundary=\"b1\"\r\n\
\r\n\
--b1\r\n\
Content-Type: text/html; charset=utf-8\r\n\
Content-Transfer-Encoding: 7bit\r\n\
\r\n\
<p>Hello edited</p><!-- mp:quote --><div>quote</div>\r\n\
--b1\r\n\
Content-Type: application/pdf; name=\"mp-probe.pdf\"\r\n\
Content-Disposition: attachment; filename=\"mp-probe.pdf\"\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
JVBERi0xLjQK\r\n\
--b1--\r\n";
        let m = ProbeMessage {
            uid: 41,
            uidvalidity: Some(7),
            reasons: vec!["since-append"],
            flags: vec!["\\Draft".into(), "\\Seen".into()],
            internal_date: Some("2026-10-13T10:05:00+02:00".into()),
            bodystructure: Some("Multipart { .. }".into()),
            raw: Some(raw.to_vec()),
        };
        let out = format_message(&m);
        for want in [
            "## UID 41 (UIDVALIDITY 7) matched by since-append",
            "FLAGS (\\Draft \\Seen)",
            "\\Draft: yes  \\Seen: yes  \\Deleted: no",
            "INTERNALDATE 2026-10-13T10:05:00+02:00",
            "BODYSTRUCTURE Multipart { .. }",
            "Message-ID: <outlook-rewrite@x>",
            "X-Mailypoppins-Draft-Id: (absent)",
            "In-Reply-To: <parent@example.org>",
            "References: (absent)",
            "Bcc: (absent)",
            "To: Probe To <me@example.org>, New <new@example.org>",
            "  1 multipart/mixed",
            "    1.1 text/html charset=utf-8 encoding=7bit",
            "    1.2 application/pdf",
            "filename=mp-probe.pdf 9 bytes",
            "1.1 comments: mp:quote yes  mp:sig-start no  mp:sig-end no",
            "1.1 mp:copy mark: no",
            "X-Mailypoppins-Draft-Source: (absent)",
            "carrier: none (edited elsewhere, or not mp's)",
            "1.1 text/html (first",
            "<p>Hello edited</p>",
        ] {
            assert!(out.contains(want), "missing {want:?} in:\n{out}");
        }
        // The attachment is not previewed as text.
        assert!(!out.contains("JVBERi0"));
    }

    /// A copy the drafts mirror wrote: the dump names its source headers,
    /// the mark its HTML part ends with, and the source the carrier reads.
    #[test]
    fn the_dump_of_an_mp_copy_names_its_source_and_mark() {
        let mid = "<mp-copy@example.org>";
        let text = format!(
            "---\nid: \"d1\"\nto: robin@example.com\nbcc: chef@example.com\n\
             subject: Angebot\nstatus: draft\nserver_message_id: \"{mid}\"\n---\n\nHallo\n"
        );
        let draft =
            mailypoppins::draft::parse_email_draft_content(std::path::Path::new("copy.md"), &text)
                .unwrap();
        let raw = mailypoppins::send::build_server_draft(
            &draft,
            "me@example.org",
            &EmailSettings::default(),
            mid,
            text.as_bytes(),
        )
        .unwrap();
        let out = format_message(&ProbeMessage {
            uid: 42,
            uidvalidity: Some(7),
            reasons: vec!["message-id"],
            flags: vec![],
            internal_date: None,
            bodystructure: None,
            raw: Some(raw),
        });
        for want in [
            "X-Mailypoppins-Draft-Source: 1 header(s), ",
            "chunks 1/1",
            "mp:copy mark: file=",
            "carrier: mp's copy of file ",
            &format!("source {} bytes", text.len()),
        ] {
            assert!(out.contains(want), "missing {want:?} in:\n{out}");
        }
    }

    #[test]
    fn a_long_text_part_is_cut_at_a_char_boundary() {
        let s = "é".repeat(TEXT_PREVIEW_BYTES);
        let cut = head(&s, TEXT_PREVIEW_BYTES + 1);
        assert_eq!(cut.len(), TEXT_PREVIEW_BYTES);
        assert_eq!(head("short", 10), "short");
    }
}
