//! Office-document plumbing (Phase 1, WP1.2 + WP1.3 shared core).
//!
//! Two runtime-agnostic, deterministic concerns live here:
//!
//! 1. **Attachment → skill routing** (WP1.2): map an inbound file's extension
//!    to one of the four bundled document skills (`docx` / `xlsx` / `pptx` /
//!    `pdf`) so the progressive-injection ranker can force the right skill into
//!    the active set when a matching file is attached. Pure lookup, zero cost
//!    when nothing matches.
//!
//! 2. **`📎DELIVER:` outbound protocol** (WP1.3): the agent appends a marker
//!    line `📎DELIVER:<absolute-path>` to its reply after producing a file. The
//!    gateway strips the marker, fail-closed-validates the path stays inside the
//!    agent's sandbox (agent dir or the shared `attachments/` fallback), then
//!    reads the bytes and hands them to the channel's `send_document`. Any
//!    failure degrades honestly to a text note that still tells the user where
//!    the file is — never a silent drop.

use std::path::{Path, PathBuf};

use tracing::warn;

use crate::channel_sender::ChannelSender;
use crate::media::{self, MAX_FILE_SIZE};

// ── WP1.2: attachment extension → skill routing ─────────────────

/// `(file_extension, skill_name)` — the deterministic routing table. Legacy
/// binary Office formats and CSV fold into the modern skill that handles them.
/// Order is irrelevant (first exact match wins); lookup is case-insensitive.
pub const ATTACHMENT_SKILL_ROUTES: &[(&str, &str)] = &[
    ("docx", "docx"),
    ("doc", "docx"),
    ("xlsx", "xlsx"),
    ("xls", "xlsx"),
    ("csv", "xlsx"),
    ("pptx", "pptx"),
    ("ppt", "pptx"),
    ("pdf", "pdf"),
];

/// Skill name for a file extension, or `None` when the extension is not a
/// routed document type. Leading dot tolerated, case-insensitive.
pub fn skill_for_extension(ext: &str) -> Option<&'static str> {
    let e = ext.trim_start_matches('.').to_ascii_lowercase();
    ATTACHMENT_SKILL_ROUTES
        .iter()
        .find(|(k, _)| *k == e)
        .map(|(_, v)| *v)
}

/// Scan a channel message (which may embed `[📎 name (file)](path)` attachment
/// refs) for routed document extensions and return the distinct skill names to
/// force active. Deterministic, zero cost when nothing matches — no doc file,
/// empty result, ranker unchanged.
pub fn skills_for_attachment_refs(message: &str) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    // Split on whitespace and the delimiters that bracket markdown link/path
    // tokens, so `report.xlsx)` and `/dir/1712_report.xlsx` both yield a bare
    // token whose trailing `.ext` we can read.
    for raw in message.split(|c: char| {
        c.is_whitespace()
            || matches!(
                c,
                '(' | ')' | '[' | ']' | '<' | '>' | '"' | '\'' | ',' | ';'
            )
    }) {
        let tok = raw.trim();
        if tok.is_empty() {
            continue;
        }
        if let Some(dot) = tok.rfind('.') {
            let ext = &tok[dot + 1..];
            if let Some(skill) = skill_for_extension(ext)
                && !out.contains(&skill)
            {
                out.push(skill);
            }
        }
    }
    out
}

// ── WP1.3: 📎DELIVER: outbound protocol ─────────────────────────

/// The marker an agent prepends (on its own line) to a delivered file's
/// absolute path.
pub const DELIVER_MARKER: &str = "📎DELIVER:";

/// Split a reply into (text with all marker lines removed, raw paths in order).
///
/// A marker line is one that — after trimming surrounding whitespace — starts
/// with [`DELIVER_MARKER`]. Everything after the marker is the raw path (still
/// untrusted DATA; validate before use). Non-marker lines are preserved
/// verbatim; when no marker is present the caller should keep the original
/// bytes (see [`process_deliverables`]) rather than the reassembled string.
pub fn parse_deliverables(reply: &str) -> (String, Vec<String>) {
    let mut kept: Vec<&str> = Vec::new();
    let mut paths: Vec<String> = Vec::new();
    for line in reply.lines() {
        if let Some(rest) = line.trim().strip_prefix(DELIVER_MARKER) {
            let p = rest.trim();
            if !p.is_empty() {
                paths.push(p.to_string());
            }
            // marker line is dropped from user-visible text
        } else {
            kept.push(line);
        }
    }
    (kept.join("\n").trim_end().to_string(), paths)
}

/// Fail-closed validation of a delivered path.
///
/// The path MUST be absolute, exist as a regular file, and — after
/// canonicalisation (which resolves `..` and symlinks) — live under either the
/// agent's own directory or the shared `{home}/attachments` fallback. A
/// traversal like `.../agents/me/../victim/secret` canonicalises out of the
/// agent root and is rejected. Returns the canonical path on success.
pub fn validate_deliver_path(
    raw: &str,
    agent_dir: &Path,
    home_dir: &Path,
) -> Result<PathBuf, String> {
    let candidate = Path::new(raw);
    if !candidate.is_absolute() {
        return Err(format!("path is not absolute: {raw}"));
    }
    let canon =
        std::fs::canonicalize(candidate).map_err(|e| format!("path not accessible: {e}"))?;
    if !canon.is_file() {
        return Err("path is not a regular file".to_string());
    }

    // Canonicalise the allowed roots too, so macOS `/var`→`/private/var` style
    // symlinks don't produce spurious mismatches. A root that doesn't exist
    // yet simply can't match (fail-closed).
    let agent_root = std::fs::canonicalize(agent_dir).ok();
    let attach_root = std::fs::canonicalize(home_dir.join("attachments")).ok();

    let under_agent = agent_root.as_ref().is_some_and(|r| canon.starts_with(r));
    let under_attach = attach_root.as_ref().is_some_and(|r| canon.starts_with(r));

    if under_agent || under_attach {
        Ok(canon)
    } else {
        Err(format!(
            "path escapes the agent sandbox: {}",
            canon.display()
        ))
    }
}

/// Post-process an agent reply: deliver any `📎DELIVER:` files through the
/// channel and return the user-visible text.
///
/// - No marker present → the original reply is returned byte-for-byte (keeps
///   prompt-cache/formatting stable for the overwhelming common case).
/// - Each valid file is sent via [`ChannelSender::send_document`]; on any
///   validation/read/send failure a zh-TW note naming the path is appended so
///   the user is never left without the deliverable's location.
pub async fn process_deliverables(
    reply: &str,
    agent_dir: &Path,
    home_dir: &Path,
    sender: &dyn ChannelSender,
) -> String {
    let (cleaned, paths) = parse_deliverables(reply);
    if paths.is_empty() {
        return reply.to_string();
    }

    let mut notes: Vec<String> = Vec::new();
    for raw in &paths {
        if let Err(e) = deliver_one(raw, agent_dir, home_dir, sender).await {
            warn!(path = %raw, error = %e, "📎DELIVER: delivery failed — degrading to text");
            notes.push(format!("⚠️ 檔案傳送失敗，已生成於 {raw}（{e}）"));
        }
    }

    match (cleaned.is_empty(), notes.is_empty()) {
        (_, true) => cleaned,
        (true, false) => notes.join("\n"),
        (false, false) => format!("{cleaned}\n\n{}", notes.join("\n")),
    }
}

/// Validate, read, and send one delivered file. Errors are the honest-degrade
/// signal for [`process_deliverables`].
async fn deliver_one(
    raw: &str,
    agent_dir: &Path,
    home_dir: &Path,
    sender: &dyn ChannelSender,
) -> Result<(), String> {
    let path = validate_deliver_path(raw, agent_dir, home_dir)?;

    let meta = tokio::fs::metadata(&path)
        .await
        .map_err(|e| format!("stat failed: {e}"))?;
    if meta.len() > MAX_FILE_SIZE {
        return Err(format!(
            "file too large: {} bytes (max {MAX_FILE_SIZE})",
            meta.len()
        ));
    }

    let data = tokio::fs::read(&path)
        .await
        .map_err(|e| format!("read failed: {e}"))?;

    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("document");
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let mime = media::mime_from_extension(ext);

    // Archive a copy under the agent's attachments/ BEFORE sending, so the
    // deliverable stays browsable/downloadable in the dashboard Files page
    // even after channel delivery (and even when the send below fails).
    // Files already inside attachments/ are listed as-is — no duplicate copy.
    // `path` is canonicalized by validate_deliver_path, so the guard base must
    // be canonicalized too (macOS: /var vs /private/var).
    let attach_base = std::fs::canonicalize(agent_dir.join("attachments"))
        .unwrap_or_else(|_| agent_dir.join("attachments"));
    if !path.starts_with(&attach_base) {
        if let Err(e) = media::save_attachment_in_base(agent_dir, &data, filename).await {
            warn!(path = %path.display(), error = %e, "📎DELIVER: archive copy failed — delivery continues");
        }
    }

    sender
        .send_document(&data, filename, mime)
        .await
        .map_err(|e| e.to_string())
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel_sender::{ChannelSendError, ChannelSender};
    use async_trait::async_trait;
    use std::sync::{Arc, Mutex};

    /// Recording sender: captures every `send_document` call for assertions.
    struct RecordingSender {
        docs: Arc<Mutex<Vec<(String, String, usize)>>>,
        texts: Arc<Mutex<Vec<String>>>,
        fail_docs: bool,
    }

    #[async_trait]
    impl ChannelSender for RecordingSender {
        async fn send_text(&self, text: &str) -> Result<(), ChannelSendError> {
            self.texts.lock().unwrap().push(text.to_string());
            Ok(())
        }
        async fn send_photo(&self, _png: &[u8], _cap: &str) -> Result<(), ChannelSendError> {
            Ok(())
        }
        async fn send_document(
            &self,
            data: &[u8],
            filename: &str,
            mime: &str,
        ) -> Result<(), ChannelSendError> {
            if self.fail_docs {
                return Err(ChannelSendError("simulated failure".into()));
            }
            self.docs
                .lock()
                .unwrap()
                .push((filename.to_string(), mime.to_string(), data.len()));
            Ok(())
        }
        async fn request_confirmation(
            &self,
            _p: &str,
            _s: Option<&[u8]>,
            _t: u64,
        ) -> Result<bool, ChannelSendError> {
            Ok(false)
        }
        fn channel_type(&self) -> &'static str {
            "recording"
        }
    }

    #[test]
    fn skill_for_extension_maps_office_family() {
        assert_eq!(skill_for_extension("docx"), Some("docx"));
        assert_eq!(skill_for_extension(".DOCX"), Some("docx"));
        assert_eq!(skill_for_extension("doc"), Some("docx"));
        assert_eq!(skill_for_extension("xlsx"), Some("xlsx"));
        assert_eq!(skill_for_extension("csv"), Some("xlsx"));
        assert_eq!(skill_for_extension("xls"), Some("xlsx"));
        assert_eq!(skill_for_extension("pptx"), Some("pptx"));
        assert_eq!(skill_for_extension("pdf"), Some("pdf"));
        assert_eq!(skill_for_extension("png"), None);
        assert_eq!(skill_for_extension("txt"), None);
    }

    #[test]
    fn skills_for_attachment_refs_extracts_from_markdown() {
        let msg = "請幫我彙總\n\n[📎 report.xlsx (file)](/home/u/.duduclaw/agents/a/attachments/1712_report.xlsx)";
        assert_eq!(skills_for_attachment_refs(msg), vec!["xlsx"]);

        // Multiple distinct types, deduped, order of first appearance.
        let msg2 = "[📎 a.pdf (file)](/x/a.pdf)\n[📎 b.docx (file)](/x/b.docx)\n[📎 c.pdf (file)](/x/c.pdf)";
        assert_eq!(skills_for_attachment_refs(msg2), vec!["pdf", "docx"]);

        // No document attachment → empty (zero cost, ranker untouched).
        assert!(skills_for_attachment_refs("just a plain question").is_empty());
        assert!(skills_for_attachment_refs("[📎 cat.png (image)](/x/cat.png)").is_empty());
    }

    #[test]
    fn parse_deliverables_strips_marker_lines() {
        let reply = "報告完成，請查收。\n📎DELIVER:/home/u/.duduclaw/agents/a/out.docx\n謝謝";
        let (cleaned, paths) = parse_deliverables(reply);
        assert_eq!(cleaned, "報告完成，請查收。\n謝謝");
        assert_eq!(paths, vec!["/home/u/.duduclaw/agents/a/out.docx"]);

        // Leading whitespace before the marker is tolerated.
        let (_, p2) = parse_deliverables("done\n   📎DELIVER:  /x/y.pdf  ");
        assert_eq!(p2, vec!["/x/y.pdf"]);

        // No marker → no paths (caller keeps original bytes).
        let (c3, p3) = parse_deliverables("just text");
        assert_eq!(c3, "just text");
        assert!(p3.is_empty());
    }

    #[tokio::test]
    async fn validate_deliver_path_accepts_agent_and_rejects_traversal() {
        let home = std::env::temp_dir().join(format!("dd-deliver-{}", uuid::Uuid::new_v4()));
        let agent_dir = home.join("agents").join("sales");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::create_dir_all(home.join("attachments")).unwrap();

        // A legit file inside the agent dir validates.
        let good = agent_dir.join("report.docx");
        std::fs::write(&good, b"docx").unwrap();
        let ok = validate_deliver_path(good.to_str().unwrap(), &agent_dir, &home).unwrap();
        assert!(ok.ends_with("report.docx"));

        // A file under the shared attachments fallback validates.
        let attach = home.join("attachments").join("in.xlsx");
        std::fs::write(&attach, b"xlsx").unwrap();
        assert!(validate_deliver_path(attach.to_str().unwrap(), &agent_dir, &home).is_ok());

        // Traversal escaping the agent dir into a sibling agent is rejected.
        let victim_dir = home.join("agents").join("victim");
        std::fs::create_dir_all(&victim_dir).unwrap();
        let secret = victim_dir.join("secret.txt");
        std::fs::write(&secret, b"top secret").unwrap();
        let evil = format!("{}/../victim/secret.txt", agent_dir.to_str().unwrap());
        let err = validate_deliver_path(&evil, &agent_dir, &home).unwrap_err();
        assert!(err.contains("escapes"), "{err}");

        // Relative path rejected.
        assert!(validate_deliver_path("out.docx", &agent_dir, &home).is_err());
        // Non-existent absolute path rejected (fail-closed).
        assert!(validate_deliver_path("/nope/does/not/exist.docx", &agent_dir, &home).is_err());

        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn process_deliverables_sends_and_strips_on_success() {
        let home = std::env::temp_dir().join(format!("dd-proc-{}", uuid::Uuid::new_v4()));
        let agent_dir = home.join("agents").join("a");
        std::fs::create_dir_all(&agent_dir).unwrap();
        let file = agent_dir.join("summary.xlsx");
        std::fs::write(&file, b"real xlsx bytes").unwrap();

        let docs = Arc::new(Mutex::new(Vec::new()));
        let sender = RecordingSender {
            docs: docs.clone(),
            texts: Arc::new(Mutex::new(Vec::new())),
            fail_docs: false,
        };

        let reply = format!("已完成彙總。\n📎DELIVER:{}", file.to_str().unwrap());
        let out = process_deliverables(&reply, &agent_dir, &home, &sender).await;

        assert_eq!(out, "已完成彙總。");
        let sent = docs.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0, "summary.xlsx");
        assert_eq!(
            sent[0].1,
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        );
        assert_eq!(sent[0].2, "real xlsx bytes".len());

        // The deliverable is archived under the agent's attachments/ so the
        // dashboard Files page can list it after delivery.
        let archived: Vec<_> = std::fs::read_dir(agent_dir.join("attachments"))
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(archived.len(), 1, "expected one archived copy: {archived:?}");
        assert!(archived[0].ends_with("summary.xlsx"), "{archived:?}");

        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn deliver_from_attachments_does_not_duplicate_archive() {
        let home = std::env::temp_dir().join(format!("dd-proc-dup-{}", uuid::Uuid::new_v4()));
        let agent_dir = home.join("agents").join("a");
        let attach_dir = agent_dir.join("attachments");
        std::fs::create_dir_all(&attach_dir).unwrap();
        let file = attach_dir.join("already.docx");
        std::fs::write(&file, b"docx").unwrap();

        let docs = Arc::new(Mutex::new(Vec::new()));
        let sender = RecordingSender {
            docs: docs.clone(),
            texts: Arc::new(Mutex::new(Vec::new())),
            fail_docs: false,
        };

        let reply = format!("done\n📎DELIVER:{}", file.to_str().unwrap());
        let _ = process_deliverables(&reply, &agent_dir, &home, &sender).await;

        assert_eq!(docs.lock().unwrap().len(), 1);
        // Still exactly one file — no timestamped duplicate alongside it.
        let count = std::fs::read_dir(&attach_dir).unwrap().count();
        assert_eq!(count, 1);

        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn process_deliverables_degrades_on_send_failure() {
        let home = std::env::temp_dir().join(format!("dd-proc-fail-{}", uuid::Uuid::new_v4()));
        let agent_dir = home.join("agents").join("a");
        std::fs::create_dir_all(&agent_dir).unwrap();
        let file = agent_dir.join("out.docx");
        std::fs::write(&file, b"docx").unwrap();

        let sender = RecordingSender {
            docs: Arc::new(Mutex::new(Vec::new())),
            texts: Arc::new(Mutex::new(Vec::new())),
            fail_docs: true,
        };

        let reply = format!("完成。\n📎DELIVER:{}", file.to_str().unwrap());
        let out = process_deliverables(&reply, &agent_dir, &home, &sender).await;
        // Text kept + honest note naming the path.
        assert!(out.starts_with("完成。"));
        assert!(out.contains("檔案傳送失敗"));
        assert!(out.contains(file.to_str().unwrap()));

        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn process_deliverables_no_marker_is_byte_identical() {
        let home = std::env::temp_dir().join(format!("dd-proc-nm-{}", uuid::Uuid::new_v4()));
        let agent_dir = home.join("agents").join("a");
        std::fs::create_dir_all(&agent_dir).unwrap();
        let sender = RecordingSender {
            docs: Arc::new(Mutex::new(Vec::new())),
            texts: Arc::new(Mutex::new(Vec::new())),
            fail_docs: false,
        };
        let reply = "一般回覆，沒有附件。\n第二行。";
        let out = process_deliverables(reply, &agent_dir, &home, &sender).await;
        assert_eq!(out, reply);
        let _ = std::fs::remove_dir_all(&home);
    }
}
