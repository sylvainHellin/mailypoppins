//! Attachments and the browser rendition (ATT-01 to ATT-05, #0131).
//!
//! A received message's part and its HTML rendition are files the daemon
//! writes (`message.materialise_attachment`, `message.materialise_html`)
//! under its runtime directory, with a lifetime; this layer opens that file
//! with the system opener, as the TUI's `to` and `tb` do, and leaves the
//! handle unreleased, because the viewer just launched holds the file. A save
//! copies each part into a directory the user names, with the `_1` rule for a
//! name already taken, then releases the handle.
//!
//! A draft's attachments are the paths its `attachments:` frontmatter lists.
//! The daemon serves neither `draft.attach` nor a way to remove an entry, so
//! the list is rewritten client-side, as the TUI's `ta` appends to it. An
//! entry is stored as typed (`~` kept), and resolves the way the send path
//! resolves it: `~` against the home directory, a relative entry against the
//! draft file's own directory (ATT-03).
//!
//! A server-only search hit has no row to materialise; its markup is in the
//! hit, and is written with the charset and CSP tags the TUI adds into this
//! app's cache directory before the browser opens it.

use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Manager, State};

use crate::commands::{call, draft_path_on, with_door, written, STUB_OPENER_ENV};
use crate::error::{Addressing, GuiError};
use crate::session::{Door, SessionHandle};

/// One materialisation: a store read and a file write in the daemon.
const HANDLE_BUDGET: Duration = Duration::from_secs(20);
const RELEASE_BUDGET: Duration = Duration::from_secs(5);

/// The directory the Save dialog offers first.
pub const DEFAULT_SAVE_DIR: &str = "~/Downloads";

/// The cache subdirectory a server-only hit's rendition is written under.
const RENDITIONS: &str = "renditions";

/// A rendition older than this is swept at the next write.
const RENDITION_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

// ---------------------------------------------------------------------------
// Result shapes
// ---------------------------------------------------------------------------

/// A file handed to the system opener.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
pub struct OpenedFile {
    pub name: String,
    pub path: String,
}

/// One part written into the save directory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
pub struct SavedFile {
    pub part: u32,
    /// The part's name as the message carries it.
    pub name: String,
    /// Where it landed, `_1` and all.
    pub path: String,
}

/// One part that was not saved, and why.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
pub struct SaveFailure {
    pub part: u32,
    pub error: GuiError,
}

/// What `attachment_save` came to, one part at a time.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
pub struct SavedAttachments {
    /// The directory, resolved to an absolute path.
    pub dir: String,
    pub saved: Vec<SavedFile>,
    pub failed: Vec<SaveFailure>,
}

/// One entry of a draft's `attachments:` list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
pub struct DraftAttachment {
    /// Zero-based, the list's order.
    pub index: u32,
    /// As the file spells it.
    pub entry: String,
    /// Where the send path finds it.
    pub path: String,
    /// Whether a file is there now; a send fails on a missing one.
    pub exists: bool,
}

/// A draft's attachments, from its file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export_to = "gui/"))]
pub struct DraftAttachments {
    pub account: String,
    pub id: String,
    /// The draft file.
    pub path: String,
    pub attachments: Vec<DraftAttachment>,
}

/// A materialiser's answer, the fields this layer reads.
#[derive(Deserialize)]
struct Handle {
    handle: String,
    path: String,
    name: String,
}

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

/// A file name that names one file in one directory: not empty, not `.` or
/// `..`, and without a path separator or a NUL.
pub fn safe_file_name(name: &str) -> Result<&str, GuiError> {
    let bad = name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\', '\0']);
    if bad {
        return Err(GuiError::protocol(format!(
            "refusing the file name `{name}`: it is not one file in one directory"
        )));
    }
    Ok(name)
}

/// `~` and `~/rest` against `home`; anything else as typed.
pub fn expand_home(input: &str, home: Option<&Path>) -> PathBuf {
    match (input, home) {
        ("~", Some(home)) => home.to_path_buf(),
        (_, Some(home)) if input.starts_with("~/") => home.join(&input[2..]),
        _ => PathBuf::from(input),
    }
}

/// The save directory the user typed, as an absolute path. A relative one
/// is refused: the app has no working directory a user could mean.
pub fn resolve_dir(input: &str, home: Option<&Path>) -> Result<PathBuf, GuiError> {
    let typed = input.trim();
    if typed.is_empty() {
        return Err(GuiError::protocol("Name the directory to save into"));
    }
    let dir = expand_home(typed, home);
    if !dir.is_absolute() {
        return Err(GuiError::protocol(format!(
            "`{typed}` is not an absolute path; start it with / or ~"
        )));
    }
    if dir.exists() && !dir.is_dir() {
        return Err(GuiError::protocol(format!("`{typed}` is not a directory")));
    }
    Ok(dir)
}

/// Where the send path finds a draft's entry: `~` against the home
/// directory, a relative entry against the draft's own directory.
pub fn resolve_entry(entry: &str, draft_dir: &Path, home: Option<&Path>) -> PathBuf {
    let expanded = expand_home(entry.trim(), home);
    if expanded.is_absolute() {
        expanded
    } else {
        draft_dir.join(expanded)
    }
}

/// Two paths name one file: compared canonically when both exist.
fn same_file(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

// ---------------------------------------------------------------------------
// The opener
// ---------------------------------------------------------------------------

/// Hand `path` to the system opener (`open` on macOS, `xdg-open` elsewhere,
/// `mp_core::parse::open_file_with_system`, the TUI's). A fixture door
/// journals it instead, and `MP_DESKTOP_STUB_OPENER=1` logs it.
fn open_file(door: &Door, path: &Path) -> Result<(), GuiError> {
    if let Door::Fixture(fixture) = door {
        tracing::info!("[open] fixture: would open {}", path.display());
        fixture.record_open(&path.display().to_string());
        return Ok(());
    }
    if std::env::var_os(STUB_OPENER_ENV).is_some_and(|v| !v.is_empty() && v != "0") {
        tracing::info!("[open] stubbed: {}", path.display());
        return Ok(());
    }
    tracing::info!("[open] {}", path.display());
    mp_core::parse::open_file_with_system(path)
        .map_err(|e| GuiError::internal(format!("could not open {}: {e:#}", path.display())))
}

fn opened(path: &Path) -> OpenedFile {
    OpenedFile {
        name: path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        path: path.display().to_string(),
    }
}

// ---------------------------------------------------------------------------
// Received messages
// ---------------------------------------------------------------------------

/// One materialisation, its file checked to be the one file it names.
fn materialise(door: &Door, method: &str, params: Value) -> Result<Handle, GuiError> {
    let answer = call(door, method, params, HANDLE_BUDGET, Addressing::Resource)?;
    let handle: Handle = crate::commands::decode(method, answer)?;
    safe_file_name(&handle.name)?;
    let file = Path::new(&handle.path);
    if !file.is_absolute() || file.file_name().is_none_or(|n| n != handle.name.as_str()) {
        return Err(GuiError::protocol(format!(
            "{method} answered a path `{}` that is not its file `{}`",
            handle.path, handle.name
        )));
    }
    Ok(handle)
}

/// Release a handle; a failure is a log line, since it expires anyway.
fn release(door: &Door, handle: &str) {
    if let Err(e) = call(
        door,
        "message.release_handle",
        json!({"handle": handle}),
        RELEASE_BUDGET,
        Addressing::Resource,
    ) {
        tracing::warn!("[attachments] releasing handle {handle}: {e}");
    }
}

/// Open part `part` of the stored message `row_id` (ATT-01, the TUI's `to`).
/// The handle stays live for its lifetime, since the viewer holds the file.
pub fn attachment_open_on(
    door: &Door,
    account: &str,
    row_id: i64,
    part: u32,
) -> Result<OpenedFile, GuiError> {
    let handle = materialise(
        door,
        "message.materialise_attachment",
        json!({"account": account, "row_id": row_id, "part": part}),
    )?;
    let path = PathBuf::from(&handle.path);
    open_file(door, &path)?;
    Ok(opened(&path))
}

/// Save `parts` of the stored message `row_id` into `dest_dir` (ATT-02, the
/// TUI's `ts`), in the order given. Two parts under one name, or a name the
/// directory already has, become `name_1.ext` and on; each handle is
/// released once its file is copied. A part that fails is in `failed` and
/// the rest go ahead.
pub fn attachment_save_on(
    door: &Door,
    account: &str,
    row_id: i64,
    parts: &[u32],
    dest_dir: &str,
    home: Option<&Path>,
) -> Result<SavedAttachments, GuiError> {
    if parts.is_empty() {
        return Err(GuiError::protocol("Pick at least one attachment to save"));
    }
    let dir = resolve_dir(dest_dir, home)?;
    let mut saved = Vec::new();
    let mut failed = Vec::new();
    for &part in parts {
        let handle = match materialise(
            door,
            "message.materialise_attachment",
            json!({"account": account, "row_id": row_id, "part": part}),
        ) {
            Ok(handle) => handle,
            Err(error) => {
                failed.push(SaveFailure { part, error });
                continue;
            }
        };
        let copied = mp_core::parse::save_attachment(Path::new(&handle.path), &dir);
        release(door, &handle.handle);
        match copied {
            Ok(path) => saved.push(SavedFile {
                part,
                name: handle.name,
                path: path.display().to_string(),
            }),
            Err(e) => failed.push(SaveFailure {
                part,
                error: GuiError::internal(format!(
                    "could not write {} into {}: {e:#}",
                    handle.name,
                    dir.display()
                )),
            }),
        }
    }
    Ok(SavedAttachments {
        dir: dir.display().to_string(),
        saved,
        failed,
    })
}

/// Open the browser rendition of the stored message `row_id` (ATT-05, the
/// TUI's `tb`): the daemon writes it with the charset, the CSP tag and the
/// `cid:` images inlined. `None` is a message with no HTML part (the
/// daemon's `-32602`), which the TUI reports as "No HTML version available".
pub fn html_open_on(
    door: &Door,
    account: &str,
    row_id: i64,
) -> Result<Option<OpenedFile>, GuiError> {
    let handle = match materialise(
        door,
        "message.materialise_html",
        json!({"account": account, "row_id": row_id}),
    ) {
        Ok(handle) => handle,
        Err(GuiError::NotFound {
            code: Some(-32602), ..
        }) => return Ok(None),
        Err(e) => return Err(e),
    };
    let path = PathBuf::from(&handle.path);
    open_file(door, &path)?;
    Ok(Some(opened(&path)))
}

/// Where a server-only hit's rendition goes: one directory per markup, so
/// the same hit opened twice rewrites one file.
pub fn rendition_path(cache_dir: &Path, html: &str) -> PathBuf {
    let mut hasher = DefaultHasher::new();
    html.hash(&mut hasher);
    cache_dir
        .join(RENDITIONS)
        .join(format!("hit-{:016x}", hasher.finish()))
        .join("message.html")
}

/// Remove the renditions older than [`RENDITION_MAX_AGE`].
fn sweep_renditions(cache_dir: &Path, now: SystemTime) {
    let Ok(entries) = fs::read_dir(cache_dir.join(RENDITIONS)) else {
        return;
    };
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .is_some_and(|age| age > RENDITION_MAX_AGE);
        if old {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

/// Open a server-only hit's markup in the browser (the TUI's search `b` on
/// an unresolved hit): the charset and CSP tags added, as the TUI's
/// `html_temp_file` does, written under `cache_dir`.
pub fn hit_html_open_on(door: &Door, cache_dir: &Path, html: &str) -> Result<OpenedFile, GuiError> {
    sweep_renditions(cache_dir, SystemTime::now());
    let path = rendition_path(cache_dir, html);
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)
            .map_err(|e| GuiError::internal(format!("could not create {}: {e}", dir.display())))?;
    }
    let page = mp_core::parse::inject_csp_meta(&mp_core::parse::ensure_utf8_charset(html));
    fs::write(&path, page)
        .map_err(|e| GuiError::internal(format!("could not write {}: {e}", path.display())))?;
    open_file(door, &path)?;
    Ok(opened(&path))
}

// ---------------------------------------------------------------------------
// A draft's attachments
// ---------------------------------------------------------------------------

/// The draft file of `id` and its parsed `attachments:` list.
fn draft_list(door: &Door, account: &str, id: &str) -> Result<(PathBuf, Vec<String>), GuiError> {
    let location = draft_path_on(door, account, id)?;
    let path = PathBuf::from(&location.path);
    let draft = mp_core::draft::parse_email_draft(&path)
        .map_err(|e| GuiError::protocol(format!("{} does not parse: {e:#}", location.path)))?;
    Ok((path, draft.frontmatter.attachments.unwrap_or_default()))
}

fn listing(
    account: &str,
    id: &str,
    path: &Path,
    entries: &[String],
    home: Option<&Path>,
) -> DraftAttachments {
    let dir = path.parent().unwrap_or(Path::new("/"));
    DraftAttachments {
        account: account.to_string(),
        id: id.to_string(),
        path: path.display().to_string(),
        attachments: entries
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let resolved = resolve_entry(entry, dir, home);
                DraftAttachment {
                    index: index as u32,
                    entry: entry.clone(),
                    exists: resolved.is_file(),
                    path: resolved.display().to_string(),
                }
            })
            .collect(),
    }
}

/// The attachments the draft `id` lists, resolved.
pub fn draft_attachments_on(
    door: &Door,
    account: &str,
    id: &str,
    home: Option<&Path>,
) -> Result<DraftAttachments, GuiError> {
    let (path, entries) = draft_list(door, account, id)?;
    Ok(listing(account, id, &path, &entries, home))
}

/// Append `input` to the draft's `attachments:` list (ATT-03, the TUI's
/// `ta`), through `mp_core::draft::append_draft_attachment`: the body and
/// every other field stay byte for byte. The path must be absolute (or
/// `~`-relative) and name a file; one the list already names is refused.
/// The entry is stored as typed, so a `~` path stays portable.
pub fn draft_attach_on(
    door: &Door,
    account: &str,
    id: &str,
    input: &str,
    home: Option<&Path>,
) -> Result<DraftAttachments, GuiError> {
    let typed = input.trim();
    if typed.is_empty() {
        return Err(GuiError::protocol("Type the path of the file to attach"));
    }
    let file = expand_home(typed, home);
    if !file.is_absolute() {
        return Err(GuiError::protocol(format!(
            "`{typed}` is not an absolute path; start it with / or ~"
        )));
    }
    if file.is_dir() {
        return Err(GuiError::protocol(format!(
            "{typed} is a directory; attach a file"
        )));
    }
    if !file.is_file() {
        return Err(GuiError::not_found(format!("No such file: {typed}")));
    }
    let (path, entries) = draft_list(door, account, id)?;
    let dir = path.parent().unwrap_or(Path::new("/"));
    if let Some(dup) = entries
        .iter()
        .find(|e| same_file(&resolve_entry(e, dir, home), &file))
    {
        return Err(GuiError::protocol(format!("{dup} is already attached")));
    }
    mp_core::draft::append_draft_attachment(&path, typed).map_err(|e| {
        GuiError::protocol(format!("could not attach to {}: {e:#}", path.display()))
    })?;
    written(door, &path.display().to_string());
    draft_attachments_on(door, account, id, home)
}

/// `content` with item `index` of its top-level `attachments:` block list
/// removed, and the number of items the list had. The body and every other
/// line stay byte for byte; an emptied list keeps its bare key, as a new
/// draft's skeleton has it. A flow-style value or an item that spans lines
/// is refused, as `append_draft_attachment` refuses one.
pub fn remove_attachment_line(content: &str, index: usize) -> Result<(String, usize), String> {
    let newline = if content.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let after_open = content
        .strip_prefix("---\n")
        .or_else(|| content.strip_prefix("---\r\n"))
        .ok_or("No frontmatter found (file does not start with '---')")?;
    let mut lines: Vec<&str> = Vec::new();
    let mut body = None;
    let mut cursor = 0usize;
    while cursor < after_open.len() {
        let rest = &after_open[cursor..];
        let (line, advance) = match rest.find('\n') {
            Some(nl) => (&rest[..nl], nl + 1),
            None => (rest, rest.len()),
        };
        let line = line.trim_end_matches('\r');
        if line == "---" {
            body = Some(&after_open[cursor + advance..]);
            break;
        }
        lines.push(line);
        cursor += advance;
    }
    let body = body.ok_or("Malformed frontmatter: no closing '---' fence")?;
    let indented = |l: &str| l.starts_with(' ') || l.starts_with('\t');
    let key = lines
        .iter()
        .position(|l| !indented(l) && l.starts_with("attachments:"))
        .ok_or("the draft lists no attachments")?;
    let value = lines[key]["attachments:".len()..].trim();
    if !value.is_empty() && !value.starts_with('#') {
        return Err(format!(
            "attachments uses an inline value ({value}); edit the draft file to the block list form first"
        ));
    }
    let mut end = key + 1;
    while end < lines.len() && indented(lines[end]) {
        end += 1;
    }
    let block = key + 1..end;
    let items: Vec<usize> = block
        .clone()
        .filter(|&i| {
            let t = lines[i].trim_start();
            t == "-" || t.starts_with("- ")
        })
        .collect();
    if items.len() != block.len() {
        return Err(
            "an attachment entry spans several lines; edit the draft file to remove it".into(),
        );
    }
    let at = *items.get(index).ok_or_else(|| {
        format!(
            "the draft lists {} attachments, no number {}",
            items.len(),
            index + 1
        )
    })?;
    lines.remove(at);
    let mut out = String::with_capacity(content.len());
    out.push_str("---");
    out.push_str(newline);
    for line in lines {
        out.push_str(line);
        out.push_str(newline);
    }
    out.push_str("---");
    out.push_str(newline);
    out.push_str(body);
    Ok((out, items.len()))
}

/// Remove entry `index` from the draft's `attachments:` list; the file it
/// named is not touched. The line count is checked against the parsed list
/// first, so a list YAML reads differently is never rewritten.
pub fn draft_attachment_remove_on(
    door: &Door,
    account: &str,
    id: &str,
    index: u32,
    home: Option<&Path>,
) -> Result<DraftAttachments, GuiError> {
    let (path, entries) = draft_list(door, account, id)?;
    let index = index as usize;
    if index >= entries.len() {
        return Err(GuiError::not_found(format!(
            "the draft lists {} attachments, no number {}",
            entries.len(),
            index + 1
        )));
    }
    let content = fs::read_to_string(&path)
        .map_err(|e| GuiError::internal(format!("could not read {}: {e}", path.display())))?;
    let (rewritten, count) = remove_attachment_line(&content, index).map_err(GuiError::protocol)?;
    if count != entries.len() {
        return Err(GuiError::protocol(format!(
            "the attachments list of {} does not read one entry per line; edit the draft file",
            path.display()
        )));
    }
    mp_core::draft::write_atomic(&path, rewritten.as_bytes())
        .map_err(|e| GuiError::internal(format!("could not write {}: {e:#}", path.display())))?;
    written(door, &path.display().to_string());
    draft_attachments_on(door, account, id, home)
}

/// Open entry `index` of the draft's list with the system opener (ATT-04):
/// the very file a send would attach.
pub fn draft_attachment_open_on(
    door: &Door,
    account: &str,
    id: &str,
    index: u32,
    home: Option<&Path>,
) -> Result<OpenedFile, GuiError> {
    let (path, entries) = draft_list(door, account, id)?;
    let entry = entries.get(index as usize).ok_or_else(|| {
        GuiError::not_found(format!(
            "the draft lists {} attachments, no number {}",
            entries.len(),
            index + 1
        ))
    })?;
    let file = resolve_entry(entry, path.parent().unwrap_or(Path::new("/")), home);
    if !file.is_file() {
        return Err(GuiError::not_found(format!(
            "{entry} is missing: no file at {}",
            file.display()
        )));
    }
    open_file(door, &file)?;
    Ok(opened(&file))
}

// ---------------------------------------------------------------------------
// The commands
// ---------------------------------------------------------------------------

#[tauri::command(rename_all = "snake_case")]
pub async fn attachment_open(
    session: State<'_, SessionHandle>,
    account: String,
    row_id: i64,
    part: u32,
) -> Result<OpenedFile, GuiError> {
    with_door(&session, move |_, door| {
        attachment_open_on(door, &account, row_id, part)
    })
    .await
}

/// `dest_dir` absolute or `~`-relative; the dialog offers [`DEFAULT_SAVE_DIR`].
#[tauri::command(rename_all = "snake_case")]
pub async fn attachment_save(
    session: State<'_, SessionHandle>,
    account: String,
    row_id: i64,
    parts: Vec<u32>,
    dest_dir: String,
) -> Result<SavedAttachments, GuiError> {
    with_door(&session, move |_, door| {
        attachment_save_on(
            door,
            &account,
            row_id,
            &parts,
            &dest_dir,
            home_dir().as_deref(),
        )
    })
    .await
}

/// `null` when the message has no HTML part.
#[tauri::command(rename_all = "snake_case")]
pub async fn html_open(
    session: State<'_, SessionHandle>,
    account: String,
    row_id: i64,
) -> Result<Option<OpenedFile>, GuiError> {
    with_door(&session, move |_, door| {
        html_open_on(door, &account, row_id)
    })
    .await
}

/// The app's cache directory, or the fixture's own under its run directory.
fn cache_dir(app: &AppHandle, door: &Door) -> Result<PathBuf, GuiError> {
    match door {
        Door::Fixture(fixture) => Ok(fixture.root().join("cache")),
        Door::Daemon(_) => app
            .path()
            .app_cache_dir()
            .map_err(|e| GuiError::internal(format!("no app cache directory: {e}"))),
    }
}

#[tauri::command(rename_all = "snake_case")]
pub async fn hit_html_open(
    app: AppHandle,
    session: State<'_, SessionHandle>,
    html: String,
) -> Result<OpenedFile, GuiError> {
    with_door(&session, move |_, door| {
        let cache = cache_dir(&app, door)?;
        hit_html_open_on(door, &cache, &html)
    })
    .await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn draft_attachments(
    session: State<'_, SessionHandle>,
    account: String,
    id: String,
) -> Result<DraftAttachments, GuiError> {
    with_door(&session, move |_, door| {
        draft_attachments_on(door, &account, &id, home_dir().as_deref())
    })
    .await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn draft_attach(
    session: State<'_, SessionHandle>,
    account: String,
    id: String,
    path: String,
) -> Result<DraftAttachments, GuiError> {
    with_door(&session, move |_, door| {
        draft_attach_on(door, &account, &id, &path, home_dir().as_deref())
    })
    .await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn draft_attachment_remove(
    session: State<'_, SessionHandle>,
    account: String,
    id: String,
    index: u32,
) -> Result<DraftAttachments, GuiError> {
    with_door(&session, move |_, door| {
        draft_attachment_remove_on(door, &account, &id, index, home_dir().as_deref())
    })
    .await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn draft_attachment_open(
    session: State<'_, SessionHandle>,
    account: String,
    id: String,
    index: u32,
) -> Result<OpenedFile, GuiError> {
    with_door(&session, move |_, door| {
        draft_attachment_open_on(door, &account, &id, index, home_dir().as_deref())
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::Fixture;
    use std::sync::Arc;

    /// A fixture door and the fixture behind it, its events dropped.
    fn fixture_door() -> (Door, Arc<Fixture>) {
        let (tx, rx) = std::sync::mpsc::channel();
        std::mem::forget(rx);
        let fixture = Arc::new(Fixture::load(tx).expect("fixture"));
        (Door::Fixture(Arc::clone(&fixture)), fixture)
    }

    /// A fresh directory for one test.
    fn scratch(purpose: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mp-desktop-att-{purpose}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn calls_of(fixture: &Fixture, method: &str) -> Vec<Value> {
        fixture
            .calls()
            .into_iter()
            .filter(|(m, _)| m == method)
            .map(|(_, p)| p)
            .collect()
    }

    #[test]
    fn a_file_name_with_a_separator_or_a_dot_name_is_refused() {
        for bad in ["", ".", "..", "a/b.pdf", "..\\x.pdf", "/etc/passwd", "a\0b"] {
            assert!(safe_file_name(bad).is_err(), "{bad:?} passed");
        }
        for good in ["ledger-q3.pdf", ".hidden", "a b (1).pdf", "Ümlaut.txt"] {
            assert_eq!(safe_file_name(good).unwrap(), good);
        }
    }

    #[test]
    fn the_save_directory_expands_home_and_refuses_a_relative_path_or_a_file() {
        let home = scratch("home");
        let file = home.join("plain.txt");
        fs::write(&file, "x").unwrap();
        assert_eq!(
            resolve_dir(DEFAULT_SAVE_DIR, Some(&home)).unwrap(),
            home.join("Downloads")
        );
        assert_eq!(resolve_dir(" ~ ", Some(&home)).unwrap(), home);
        assert_eq!(
            resolve_dir("/var/x", Some(&home)).unwrap(),
            PathBuf::from("/var/x")
        );
        assert!(resolve_dir("Downloads", Some(&home))
            .unwrap_err()
            .message()
            .contains("not an absolute path"));
        assert!(resolve_dir("~/Downloads", None).is_err());
        assert!(resolve_dir("  ", Some(&home)).is_err());
        assert!(resolve_dir("~/plain.txt", Some(&home))
            .unwrap_err()
            .message()
            .contains("not a directory"));
    }

    #[test]
    fn a_draft_entry_resolves_as_the_send_path_resolves_it() {
        let home = Path::new("/home/me");
        let dir = Path::new("/data/drafts/work");
        assert_eq!(
            resolve_entry("~/a.pdf", dir, Some(home)),
            home.join("a.pdf")
        );
        assert_eq!(
            resolve_entry("/abs/b.pdf", dir, Some(home)),
            PathBuf::from("/abs/b.pdf")
        );
        assert_eq!(
            resolve_entry("rel/c.pdf", dir, Some(home)),
            dir.join("rel/c.pdf")
        );
    }

    #[test]
    fn a_hit_rendition_lands_in_the_cache_directory_one_directory_per_markup() {
        let cache = Path::new("/cache/dev.mailypoppins.desktop");
        let a = rendition_path(cache, "<p>a</p>");
        assert!(a.starts_with(cache.join(RENDITIONS)));
        assert_eq!(a.file_name().unwrap(), "message.html");
        assert_eq!(a, rendition_path(cache, "<p>a</p>"));
        assert_ne!(a, rendition_path(cache, "<p>b</p>"));
    }

    #[test]
    fn a_sweep_removes_renditions_older_than_a_day_only() {
        let cache = scratch("sweep");
        let path = rendition_path(&cache, "<p>old</p>");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "x").unwrap();
        sweep_renditions(&cache, SystemTime::now());
        assert!(path.exists());
        sweep_renditions(&cache, SystemTime::now() + RENDITION_MAX_AGE * 2);
        assert!(!path.exists());
    }

    const DRAFT: &str = "---\nid: x\nto: a@example.com\nattachments:\n  - \"/one.pdf\"\n  - \"~/two.pdf\"\n  - \"three.pdf\"\nstatus: draft\n---\n\nBody line\n---\nnot a fence of the frontmatter\n";

    #[test]
    fn removing_an_entry_keeps_the_order_the_other_lines_and_the_body() {
        let (out, count) = remove_attachment_line(DRAFT, 1).unwrap();
        assert_eq!(count, 3);
        assert_eq!(
            out,
            "---\nid: x\nto: a@example.com\nattachments:\n  - \"/one.pdf\"\n  - \"three.pdf\"\nstatus: draft\n---\n\nBody line\n---\nnot a fence of the frontmatter\n"
        );
        // The last one out leaves the bare key a new draft's skeleton has.
        let (one, _) = remove_attachment_line(&out, 0).unwrap();
        let (none, count) = remove_attachment_line(&one, 0).unwrap();
        assert_eq!(count, 1);
        assert!(none.contains("\nattachments:\nstatus: draft\n"));
        assert!(remove_attachment_line(&none, 0)
            .unwrap_err()
            .contains("no number 1"));
    }

    #[test]
    fn removing_keeps_crlf_and_refuses_what_it_cannot_rewrite_line_by_line() {
        let crlf = DRAFT.replace('\n', "\r\n");
        let (out, _) = remove_attachment_line(&crlf, 0).unwrap();
        assert!(out.starts_with("---\r\nid: x\r\n"));
        assert!(!out.contains("/one.pdf"));
        assert!(out.ends_with("Body line\r\n---\r\nnot a fence of the frontmatter\r\n"));
        let inline = "---\nattachments: [\"/a\"]\n---\nb\n";
        assert!(remove_attachment_line(inline, 0)
            .unwrap_err()
            .contains("inline value"));
        let folded = "---\nattachments:\n  - \"/a\n    b\"\n---\nb\n";
        assert!(remove_attachment_line(folded, 0)
            .unwrap_err()
            .contains("spans several lines"));
        assert!(remove_attachment_line("no frontmatter", 0).is_err());
        assert!(remove_attachment_line("---\nid: x\n---\n", 0)
            .unwrap_err()
            .contains("lists no attachments"));
    }

    #[test]
    fn open_materialises_the_part_and_hands_the_daemons_file_to_the_opener() {
        let (door, fixture) = fixture_door();
        let file = attachment_open_on(&door, "work", 1001, 0).unwrap();
        assert_eq!(file.name, "ledger-q3.pdf");
        assert!(Path::new(&file.path).is_file());
        assert!(Path::new(&file.path).starts_with(fixture.root().join("handles")));
        assert_eq!(fixture.opened(), vec![file.path.clone()]);
        // The viewer holds the file: no release.
        assert!(calls_of(&fixture, "message.release_handle").is_empty());
        let missing = attachment_open_on(&door, "work", 1001, 3).unwrap_err();
        assert!(matches!(missing, GuiError::NotFound { .. }), "{missing:?}");
        assert_eq!(fixture.opened().len(), 1);
    }

    #[test]
    fn save_applies_the_underscore_rule_and_releases_each_handle() {
        let (door, fixture) = fixture_door();
        let home = scratch("save");
        let first = attachment_save_on(&door, "work", 1001, &[0], "~/out", Some(&home)).unwrap();
        assert_eq!(first.dir, home.join("out").display().to_string());
        assert_eq!(first.saved.len(), 1);
        assert_eq!(
            first.saved[0].path,
            home.join("out/ledger-q3.pdf").display().to_string()
        );
        let again = attachment_save_on(&door, "work", 1001, &[0, 4], "~/out", Some(&home)).unwrap();
        assert_eq!(
            again.saved[0].path,
            home.join("out/ledger-q3_1.pdf").display().to_string()
        );
        assert_eq!(again.failed.len(), 1);
        assert_eq!(again.failed[0].part, 4);
        assert_eq!(calls_of(&fixture, "message.release_handle").len(), 2);
        assert!(fixture.opened().is_empty());
        assert!(attachment_save_on(&door, "work", 1001, &[], "~/out", Some(&home)).is_err());
        assert!(attachment_save_on(&door, "work", 1001, &[0], "out", Some(&home)).is_err());
    }

    #[test]
    fn html_opens_the_daemons_rendition_and_a_message_without_markup_is_none() {
        let (door, fixture) = fixture_door();
        let file = html_open_on(&door, "work", 1001)
            .unwrap()
            .expect("1001 has HTML");
        assert_eq!(file.name, "message.html");
        let page = fs::read_to_string(&file.path).unwrap();
        assert!(page.contains("Content-Security-Policy"), "{page}");
        assert_eq!(fixture.opened(), vec![file.path]);
        assert_eq!(html_open_on(&door, "work", 1002).unwrap(), None);
        assert_eq!(fixture.opened().len(), 1);
    }

    #[test]
    fn a_hit_rendition_carries_the_charset_and_the_csp_and_opens() {
        let (door, fixture) = fixture_door();
        let cache = scratch("cache");
        let file = hit_html_open_on(&door, &cache, "<html><body>Grüße</body></html>").unwrap();
        assert!(Path::new(&file.path).starts_with(cache.join(RENDITIONS)));
        let page = fs::read_to_string(&file.path).unwrap();
        assert!(page.contains("Content-Security-Policy"));
        assert!(page.to_lowercase().contains("charset=\"utf-8\""), "{page}");
        assert!(page.contains("Grüße"));
        assert_eq!(fixture.opened(), vec![file.path]);
    }

    #[test]
    fn attach_appends_in_order_refuses_duplicates_and_missing_files_and_remove_rewrites() {
        let (door, fixture) = fixture_door();
        let home = scratch("attach");
        let (a, b) = (home.join("a.pdf"), home.join("b.pdf"));
        fs::write(&a, "a").unwrap();
        fs::write(&b, "b").unwrap();
        let draft = draft_path_on(&door, "work", "angebot-antwort")
            .unwrap()
            .path;
        let body = |p: &str| {
            mp_core::draft::parse_email_draft(Path::new(p))
                .unwrap()
                .body_markdown
        };
        let before = body(&draft);

        let one = draft_attach_on(
            &door,
            "work",
            "angebot-antwort",
            &a.display().to_string(),
            Some(&home),
        )
        .unwrap();
        assert_eq!(one.attachments.len(), 1);
        let two =
            draft_attach_on(&door, "work", "angebot-antwort", " ~/b.pdf ", Some(&home)).unwrap();
        let entries: Vec<&str> = two.attachments.iter().map(|x| x.entry.as_str()).collect();
        assert_eq!(entries, [a.display().to_string().as_str(), "~/b.pdf"]);
        assert_eq!(two.attachments[1].path, b.display().to_string());
        assert!(two.attachments.iter().all(|x| x.exists));
        assert_eq!(body(&draft), before);

        let dup =
            draft_attach_on(&door, "work", "angebot-antwort", "~/a.pdf", Some(&home)).unwrap_err();
        assert!(dup.message().contains("already attached"), "{dup:?}");
        let gone = draft_attach_on(&door, "work", "angebot-antwort", "~/nope.pdf", Some(&home))
            .unwrap_err();
        assert!(matches!(gone, GuiError::NotFound { .. }));
        assert!(gone.message().contains("No such file: ~/nope.pdf"));
        assert!(draft_attach_on(&door, "work", "angebot-antwort", "a.pdf", Some(&home)).is_err());
        assert!(
            draft_attach_on(&door, "work", "angebot-antwort", "~", Some(&home))
                .unwrap_err()
                .message()
                .contains("is a directory")
        );

        let opened =
            draft_attachment_open_on(&door, "work", "angebot-antwort", 1, Some(&home)).unwrap();
        assert_eq!(fixture.opened(), vec![b.display().to_string()]);
        assert_eq!(opened.name, "b.pdf");

        let left =
            draft_attachment_remove_on(&door, "work", "angebot-antwort", 0, Some(&home)).unwrap();
        let entries: Vec<&str> = left.attachments.iter().map(|x| x.entry.as_str()).collect();
        assert_eq!(entries, ["~/b.pdf"]);
        assert!(a.exists(), "a remove leaves the file alone");
        assert_eq!(body(&draft), before);
        assert!(
            draft_attachment_remove_on(&door, "work", "angebot-antwort", 5, Some(&home)).is_err()
        );
        assert!(
            draft_attachment_open_on(&door, "work", "angebot-antwort", 5, Some(&home)).is_err()
        );

        fs::remove_file(&b).unwrap();
        let listed = draft_attachments_on(&door, "work", "angebot-antwort", Some(&home)).unwrap();
        assert!(!listed.attachments[0].exists);
        let missing =
            draft_attachment_open_on(&door, "work", "angebot-antwort", 0, Some(&home)).unwrap_err();
        assert!(missing.message().contains("is missing"));
    }
}
