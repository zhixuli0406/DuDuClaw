//! Legacy message links are routing data, never arbitrary filesystem grants.
//! Full history authorization and the durable artifact audience gate are both
//! required by consumers. Paths stay on the server, behind user-bound handles.
use std::{fs::File, path::{Component, Path}, sync::OnceLock};
use serde::{Deserialize, Serialize};
use crate::{fs_safe::SafeDir, history_privacy::handles};
pub(crate) mod limits;
pub(crate) mod response;

#[derive(Serialize)]
pub(crate) struct Attachment {
    pub id: String,
    pub name: String,
    pub size: u64,
    pub mime: &'static str,
    #[serde(skip)]
    pub filename: String,
}
#[derive(Serialize, Deserialize)]
pub(crate) struct Reference {
    pub session: String,
    pub message: i64,
    pub agent: String,
    pub path: String,
    pub name: String,
}

/// Parse only the formats written by media::format_attachment_ref and DELIVER.
/// Balanced parentheses preserve old filenames such as T240(0-1069-1).docx.
/// Even an invalid reference is removed from display text to avoid host paths.
pub(crate) fn parse(raw: &str) -> (String, Vec<(String, String)>) {
    static LINK: OnceLock<regex::Regex> = OnceLock::new();
    static MARKER: OnceLock<regex::Regex> = OnceLock::new();
    let marker = MARKER.get_or_init(|| regex::Regex::new(r"\[(?:📎|🖼️|🎵|🎬) ").expect("constant marker"));
    let link = LINK.get_or_init(|| regex::Regex::new(
        r"^\[(?:📎|🖼️|🎵|🎬) ([^\[\]{}\r\n]*?) \((?:file|image|audio|video)\)\]\("
    ).expect("constant attachment pattern"));
    let mut output = String::new();
    let mut refs = Vec::new();
    let mut unfinished = false;
    for (line_number, line) in raw.split('\n').enumerate() {
        if line_number > 0 { output.push('\n'); }
        // Legacy channel filenames could contain newlines. Once a host marker
        // is incomplete, hide its continuation rather than leak its path.
        if unfinished { continue; }
        if let Some(path) = line.trim().strip_prefix("📎DELIVER:")
            .or_else(|| line.trim().strip_prefix("DELIVER:")) {
            let path = path.trim();
            let name = Path::new(path).file_name().and_then(|s| s.to_str()).unwrap_or("file");
            if refs.len() < 32 { refs.push((name.to_owned(), path.to_owned())); }
            output.push_str("📎");
            continue;
        }
        let Some(start) = marker.find(line) else { output.push_str(line); continue; };
        let prefix = &line[..start.start()];
        output.push_str(prefix);
        output.push_str("📎");
        // A host reference occupies its whole line (apart from the fixed tool
        // hint). Malformed/embedded references are redacted through end of line,
        // never partially consumed: their tail may contain an absolute path.
        if !prefix.trim().is_empty() { unfinished = true; continue; }
        let Some(capture) = link.captures(&line[start.start()..]) else {
            unfinished = true;
            continue;
        };
        let tail = &line[start.start() + capture.get(0).expect("whole capture").end()..];
        let mut depth = 1usize;
        let end = tail.char_indices().find_map(|(i, c)| {
            if c == '(' { depth += 1; }
            if c == ')' { depth -= 1; }
            (depth == 0).then_some(i)
        });
        let Some(end) = end else { unfinished = true; continue; };
        let suffix = tail[end + 1..].trim();
        if !suffix.is_empty() && suffix != "請用 csv_read／xlsx_read／file_read 讀取" { unfinished = true; continue; }
        let target = tail[..end].trim();
        let target = target.strip_prefix('<').and_then(|p| p.strip_suffix('>')).unwrap_or(target);
        let target = target.strip_prefix("file://").unwrap_or(target);
        if refs.len() < 32 { refs.push((capture[1].to_owned(), target.to_owned())); }
        if !suffix.is_empty() { output.push(' '); output.push_str(suffix); }
    }
    (output, refs)
}

/// No lexical traversal, sibling prefix, foreign employee, or symlink is
/// accepted. Canonical comparison uses Path components. SafeDir pins every
/// untrusted component and the final file with openat/O_NOFOLLOW on Unix.
pub(crate) fn open(home: &Path, agent: &str, raw: &str) -> Result<(File, String), String> {
    let denied = || "attachment unavailable".to_owned();
    let dir = crate::files_api::attachments_dir(home, Some(agent)).ok_or_else(denied)?;
    let path = Path::new(raw);
    if !path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir | Component::CurDir)) {
        return Err(denied());
    }
    let name = path.file_name().and_then(|n| n.to_str()).ok_or_else(denied)?;
    if !crate::files_api::is_safe_filename(name) { return Err(denied()); }
    let root = home.canonicalize().map_err(|_| denied())?;
    let expected = root.join("agents").join(agent).join("attachments");
    // Accept the configured home spelling (e.g. macOS /var -> /private/var)
    // or its canonical spelling, but never normalize away untrusted parents.
    if path.parent() != Some(dir.as_path()) && path.parent() != Some(expected.as_path()) { return Err(denied()); }
    if dir.canonicalize().map_err(|_| denied())? != expected { return Err(denied()); }
    let canonical = path.canonicalize().map_err(|_| denied())?;
    if canonical.strip_prefix(&expected).ok() != Some(Path::new(name)) { return Err(denied()); }
    let mut directory = SafeDir::open_root(&root).map_err(|_| denied())?;
    for part in ["agents", agent, "attachments"] {
        directory = directory.child_dir(part, false).map_err(|_| denied())?.ok_or_else(denied)?;
    }
    let file = directory.open_read_checked(name, true).map_err(|_| denied())?.ok_or_else(denied)?;
    Ok((file, name.to_owned()))
}

/// A recorded outbound source can name an archived copy. No filesystem
/// access to the source is allowed; missing or ambiguous legacy data denies.
pub(crate) fn archived_path_indexed(home: &Path, agent: &str, raw: &str, index: &crate::artifacts::HistoryIndex) -> Option<String> {
    let path = Path::new(raw);
    if !path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir | Component::CurDir)) { return None; }
    if open(home, agent, raw).is_ok() { return Some(raw.to_owned()); }
    let name = index.archive_for_source(path)?;
    let dir = crate::files_api::attachments_dir(home, Some(agent))?;
    dir.join(name).to_str().map(str::to_owned)
}

pub(crate) fn project_indexed(home: &Path, user: &str, session: &str, agent: &str, message: i64, raw: &str, index: &crate::artifacts::HistoryIndex, references_left: &mut usize, attachments_left: &mut usize) -> (String, Vec<Attachment>) {
    let (content, refs) = parse(raw);
    let mut attachments = Vec::new();
    for (name, path) in refs {
        if *references_left == 0 || *attachments_left == 0 { break; }
        *references_left -= 1;
        let Some(path) = archived_path_indexed(home, agent, &path, index) else { continue };
        let Ok((file, filename)) = open(home, agent, &path) else { continue };
        let Ok(metadata) = file.metadata() else { continue };
        if metadata.len() > limits::MAX_DOWNLOAD { continue; }
        let name = duduclaw_core::truncate_chars(&name.chars().filter(|c| !c.is_control()).collect::<String>(), 200);
        let reference = Reference { session: session.into(), message, agent: agent.into(), path, name: name.clone() };
        let Ok(raw) = serde_json::to_string(&reference) else { continue };
        let Ok(id) = handles::issue_attachment(home, user, &raw) else { continue };
        *attachments_left -= 1;
        attachments.push(Attachment { id, name, size: metadata.len(), mime: crate::files_api::content_type_for(&filename), filename });
    }
    (content, attachments)
}

#[cfg(test)]
mod tests;

/// Read only the latest bounded window, without loading the conversation first.
pub(crate) fn history_rows(home: &Path, session: &str, limit: usize) -> Result<Vec<crate::session::WebChatHistoryMessage>, String> {
    use rusqlite::params;
    let c = crate::history_privacy::read::open(home)?;
    let mut stmt = c.prepare("SELECT id,role,CASE WHEN length(CAST(content AS BLOB))<=?3 THEN content ELSE NULL END,tokens,timestamp,notice_kind FROM session_messages WHERE session_id=?1 AND undone_at IS NULL AND (hidden=0 OR notice_kind='turn_failure') ORDER BY id DESC LIMIT ?2").map_err(|_| "history unavailable")?;
    let rows = stmt.query_map(params![session, limit.min(2000) as i64, limits::MAX_MESSAGE as i64], |r| {
        Ok((r.get::<_,i64>(0)?, r.get::<_,String>(1)?, r.get::<_,Option<String>>(2)?, r.get::<_,u32>(3)?, r.get::<_,String>(4)?, r.get::<_,Option<String>>(5)?))
    }).map_err(|_| "history unavailable")?;
    let mut result = Vec::new();
    let mut bytes = 0;
    for row in rows {
        let (id, role, content, tokens, timestamp, notice_kind) = row.map_err(|_| "history unavailable")?;
        let content = content.ok_or("history message too large")?;
        bytes += content.len();
        if bytes > limits::MAX_HISTORY_BYTES { break; }
        result.push(crate::session::WebChatHistoryMessage { id, message: crate::session::SessionMessage { role, content, tokens, timestamp }, notice_kind });
    }
    result.reverse();
    Ok(result)
}

/// Validate and copy the already-open descriptor. The returned bytes are the
/// only bytes hashed and served, even if another process mutates the inode.
pub(crate) fn snapshot(file: File, max: u64) -> Result<(Vec<u8>, String), String> {
    let max = max.min(limits::MAX_DOWNLOAD);
    let metadata = file.metadata().map_err(|_| "attachment unavailable")?;
    if !metadata.is_file() || metadata.len() > max { return Err("attachment too large".into()); }
    read_snapshot(file, metadata.len(), max)
}

fn read_snapshot(reader: impl std::io::Read, expected: u64, max: u64) -> Result<(Vec<u8>, String), String> {
    use std::io::Read;
    use sha2::{Digest, Sha256};
    let mut bytes = Vec::new();
    reader.take(max + 1).read_to_end(&mut bytes).map_err(|_| "attachment unavailable")?;
    if bytes.len() as u64 > max || bytes.len() as u64 != expected { return Err("attachment changed".into()); }
    let hash = hex::encode(Sha256::digest(&bytes));
    Ok((bytes, hash))
}

#[cfg(test)]
fn archived_path(home: &Path, agent: &str, raw: &str) -> Option<String> {
    archived_path_indexed(home, agent, raw, &crate::artifacts::HistoryIndex::load(home, Some(agent)))
}
#[cfg(test)]
fn project(home: &Path, user: &str, session: &str, agent: &str, message: i64, raw: &str) -> (String, Vec<Attachment>) {
    let mut refs = limits::MAX_REFERENCES;
    let mut attachments = limits::MAX_ATTACHMENTS;
    project_indexed(home,user,session,agent,message,raw,&crate::artifacts::HistoryIndex::load(home,Some(agent)), &mut refs, &mut attachments)
}
