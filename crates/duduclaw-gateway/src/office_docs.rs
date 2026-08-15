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
        if let Err(e) = deliver_one(
            raw,
            agent_dir,
            home_dir,
            sender,
            crate::artifacts::ArtifactOrigin::Declared,
        )
        .await
        {
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
///
/// `origin` (I-2b) is how the deliverable was identified — the agent's own
/// `📎DELIVER:` declaration, or the undeclared sweep recovering it. It is
/// recorded alongside the archived copy so the dashboard can tell a hand-over
/// from a file a human sent in, instead of showing one flat directory.
/// The channel-agnostic first half of a `📎DELIVER:` hand-over: validate the
/// path, enforce the WP-4H delivery gate, and archive a copy into the agent's
/// `attachments/` (provenance ledger included) so the file is genuinely
/// downloadable from the dashboard Files page. Transports with an in-band
/// binary frame then push `data`; transports without one (WebChat) rely on
/// the archival side effect alone — which is exactly why `archived` is
/// reported instead of swallowed: a caller about to promise "可至檔案面板
/// 下載" must not make that claim when the copy failed.
pub struct StagedDeliverable {
    pub data: Vec<u8>,
    pub filename: String,
    pub mime: &'static str,
    /// True when a downloadable copy exists under `attachments/` (either the
    /// archive copy succeeded, or the file already lived there).
    pub archived: bool,
}

pub async fn stage_deliverable(
    raw: &str,
    agent_dir: &Path,
    home_dir: &Path,
    origin: crate::artifacts::ArtifactOrigin,
    channel: Option<&str>,
) -> Result<StagedDeliverable, String> {
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

    // WP-4H: zero-LLM delivery gate. Hard failures (empty / magic mismatch /
    // corrupt zip container) reject the whole delivery here — before
    // anything is archived or sent — and propagate through the caller's
    // existing `Err` degrade-to-text-note path. Soft signals (placeholder
    // residue) are logged and let `data` through unchanged.
    let office_gate_cfg = crate::artifact_gate::OfficeGateConfig::from_home(home_dir);
    crate::artifact_gate::run_delivery_gate(&path, &data, agent_dir, home_dir, &office_gate_cfg)
        .await?;

    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("document")
        .to_string();
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let mime = media::mime_from_extension(ext);

    // Archive a copy under the agent's attachments/ BEFORE sending, so the
    // deliverable stays browsable/downloadable in the dashboard Files page
    // even after channel delivery (and even when the send fails).
    // Files already inside attachments/ are listed as-is — no duplicate copy.
    // `path` is canonicalized by validate_deliver_path, so the guard base must
    // be canonicalized too (macOS: /var vs /private/var).
    let attach_base = std::fs::canonicalize(agent_dir.join("attachments"))
        .unwrap_or_else(|_| agent_dir.join("attachments"));
    let archived = if path.starts_with(&attach_base) {
        // A file already living in `attachments/` keeps whatever provenance it
        // was first recorded with (an inbound upload the agent is handing back
        // is still the human's file) — re-labelling it here would rewrite
        // history.
        true
    } else {
        // I-2b: archive WITHOUT the inbound-upload provenance the shared
        // helper stamps, then record this hand-over's real origin (declared /
        // swept) plus the channel it went out on and where it was produced.
        match media::save_attachment_in_base_untracked(agent_dir, &data, &filename).await {
            Ok(saved) => {
                crate::artifacts::record_saved(
                    agent_dir,
                    &saved,
                    &filename,
                    data.len() as u64,
                    &crate::artifacts::SaveContext::delivered(origin, channel, &path),
                );
                true
            }
            Err(e) => {
                warn!(path = %path.display(), error = %e, "📎DELIVER: archive copy failed — delivery continues");
                false
            }
        }
    };

    Ok(StagedDeliverable {
        data,
        filename,
        mime,
        archived,
    })
}

async fn deliver_one(
    raw: &str,
    agent_dir: &Path,
    home_dir: &Path,
    sender: &dyn ChannelSender,
    origin: crate::artifacts::ArtifactOrigin,
) -> Result<(), String> {
    let staged =
        stage_deliverable(raw, agent_dir, home_dir, origin, Some(sender.channel_type())).await?;
    sender
        .send_document(&staged.data, &staged.filename, staged.mime)
        .await
        .map_err(|e| e.to_string())
}

// ── WP1.3 hardening: undeclared-deliverable sweep ───────────────
//
// Live incident (2026-07-28): the agent produced a real .docx but wrote it to
// ~/Desktop and never emitted `📎DELIVER:` — the user got prose claiming the
// file exists, the gateway had nothing to send or archive. The always-on
// prompt rule (channel_reply) attacks the cause; this sweep is the
// deterministic net for the "wrote it in the workdir but forgot the marker"
// half: after a marker-less reply that *talks about* a produced document,
// recently-modified office files inside the agent directory are delivered and
// archived exactly as if they had been declared.

/// Extensions the sweep treats as user deliverables. Deliberately excludes
/// `.md`/`.txt`/`.json` — agents constantly write those as internal state.
const SWEEP_EXTS: &[&str] = &["docx", "xlsx", "pptx", "pdf", "csv", "odt", "ods", "odp"];
/// Max age of a file's mtime for the sweep to attribute it to the reply that
/// just finished (long CLI runs take minutes).
const SWEEP_WINDOW_SECS: u64 = 15 * 60;
/// Directory recursion cap — deliverables land at or near the workdir root.
const SWEEP_MAX_DEPTH: usize = 3;
/// At most this many files are auto-delivered per reply.
const SWEEP_MAX_FILES: usize = 3;

/// Heuristic gate: does the reply text talk about a produced document? Keeps
/// the sweep from firing on unrelated replies (and narrows the window in
/// which a concurrent conversation on the same agent could pick up the other
/// conversation's file). This is a delivery heuristic, not a security
/// decision — the fence stays `validate_deliver_path`.
pub fn reply_mentions_document(reply: &str) -> bool {
    let lower = reply.to_lowercase();
    const KEYWORDS: &[&str] = &[
        ".docx", ".xlsx", ".pptx", ".pdf", ".csv", "word", "excel", "powerpoint", "檔案", "文件",
        "簡報", "報表", "試算表",
    ];
    KEYWORDS.iter().any(|k| lower.contains(k))
}

/// Mirror of `media::save_attachment_in_base`'s filename sanitizer, used to
/// match a workdir file against its archived `<ts>_<safe_name>` copies.
fn sanitize_name(filename: &str) -> String {
    filename
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// True when `attachments/` already holds a copy of `filename` with `size`
/// bytes (archive names are `<ts>_<sanitized>`), meaning a previous turn
/// already delivered this exact file — the sweep must not re-send it.
fn already_archived(agent_dir: &Path, filename: &str, size: u64) -> bool {
    let safe = sanitize_name(filename);
    let Ok(entries) = std::fs::read_dir(agent_dir.join("attachments")) else {
        return false;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let suffix_match = name
            .split_once('_')
            .is_some_and(|(_, rest)| rest == safe);
        if suffix_match && entry.metadata().is_ok_and(|m| m.len() == size) {
            return true;
        }
    }
    false
}

/// Collect sweep candidates: office-typed files under `agent_dir` (depth ≤
/// [`SWEEP_MAX_DEPTH`]) modified within [`SWEEP_WINDOW_SECS`], excluding
/// `attachments/` (already archived) and hidden/internal directories. Newest
/// first, capped at [`SWEEP_MAX_FILES`].
fn sweep_candidates(agent_dir: &Path) -> Vec<PathBuf> {
    const SKIP_DIRS: &[&str] = &[
        "attachments",
        "sessions",
        "logs",
        "memory",
        "wiki",
        "node_modules",
        "target",
    ];
    let now = std::time::SystemTime::now();
    let mut found: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    let mut stack: Vec<(PathBuf, usize)> = vec![(agent_dir.to_path_buf(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if path.is_dir() {
                if depth + 1 <= SWEEP_MAX_DEPTH
                    && !name.starts_with('.')
                    && !SKIP_DIRS.contains(&name)
                {
                    stack.push((path, depth + 1));
                }
                continue;
            }
            let ext_ok = path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| SWEEP_EXTS.contains(&e.to_ascii_lowercase().as_str()));
            if !ext_ok {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            let Ok(modified) = meta.modified() else { continue };
            let recent = now
                .duration_since(modified)
                .map(|age| age.as_secs() <= SWEEP_WINDOW_SECS)
                .unwrap_or(true); // mtime in the future (clock skew) still counts
            if !recent || already_archived(agent_dir, name, meta.len()) {
                continue;
            }
            found.push((modified, path));
        }
    }
    found.sort_by(|a, b| b.0.cmp(&a.0));
    found.into_iter().take(SWEEP_MAX_FILES).map(|(_, p)| p).collect()
}

/// Deliver + archive recently-produced office files the agent forgot to
/// declare with `📎DELIVER:`. Returns how many files were sent. Failures are
/// logged and skipped — the sweep is a net, never a new failure mode for the
/// reply itself.
pub async fn sweep_undeclared_deliverables(
    agent_dir: &Path,
    home_dir: &Path,
    sender: &dyn ChannelSender,
) -> usize {
    let candidates = sweep_candidates(agent_dir);
    let mut sent = 0usize;
    for path in candidates {
        let raw = path.to_string_lossy();
        match deliver_one(
            &raw,
            agent_dir,
            home_dir,
            sender,
            crate::artifacts::ArtifactOrigin::Swept,
        )
        .await
        {
            Ok(()) => {
                tracing::info!(path = %path.display(), "deliver sweep: sent undeclared produced file");
                sent += 1;
            }
            Err(e) => {
                warn!(path = %path.display(), error = %e, "deliver sweep: send failed — skipped");
            }
        }
    }
    sent
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

    /// WP-4H: the delivery gate now rejects docx/xlsx/pptx content whose
    /// bytes aren't a real zip container, so fixtures for those extensions
    /// need to be an actual (minimal) well-formed zip — plain placeholder
    /// bytes like `b"docx"` no longer pass and would make these tests fail
    /// for the wrong reason (blocked by the gate, not exercising what the
    /// test is actually about).
    fn minimal_zip_bytes() -> Vec<u8> {
        use std::io::Write;
        let mut zw = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        zw.start_file("test.txt", opts).unwrap();
        zw.write_all(b"hello").unwrap();
        zw.finish().unwrap().into_inner()
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
        let data = minimal_zip_bytes();
        std::fs::write(&file, &data).unwrap();

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
        assert_eq!(sent[0].2, data.len());

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
        std::fs::write(&file, minimal_zip_bytes()).unwrap();

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

    /// I-2b: the archive copy must carry the hand-over's provenance, so the
    /// dashboard can tell a delivered file from a file the user sent in.
    #[tokio::test]
    async fn deliverable_archive_records_declared_provenance() {
        let home = std::env::temp_dir().join(format!("dd-prov-{}", uuid::Uuid::new_v4()));
        let agent_dir = home.join("agents").join("sales");
        std::fs::create_dir_all(&agent_dir).unwrap();
        let file = agent_dir.join("summary.xlsx");
        std::fs::write(&file, minimal_zip_bytes()).unwrap();

        let sender = RecordingSender {
            docs: Arc::new(Mutex::new(Vec::new())),
            texts: Arc::new(Mutex::new(Vec::new())),
            fail_docs: false,
        };
        let reply = format!("已完成彙總。\n📎DELIVER:{}", file.to_str().unwrap());
        let _ = process_deliverables(&reply, &agent_dir, &home, &sender).await;

        let idx = crate::artifacts::provenance_index(&home, Some("sales"));
        assert_eq!(idx.len(), 1, "exactly one provenance row: {idx:?}");
        let p = idx.values().next().unwrap();
        assert_eq!(p.origin, crate::artifacts::ArtifactOrigin::Declared);
        assert_eq!(p.display_name, "summary.xlsx");
        assert_eq!(p.channel.as_deref(), Some("recording"));

        let _ = std::fs::remove_dir_all(&home);
    }

    /// The sweep's recovery is recorded as such — 「掃描回收」 is a different
    /// fact from 「員工主動交付」 and the UI shows which one happened.
    #[tokio::test]
    async fn swept_deliverable_is_recorded_as_swept() {
        let home = std::env::temp_dir().join(format!("dd-prov-sweep-{}", uuid::Uuid::new_v4()));
        let agent_dir = home.join("agents").join("ops");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(agent_dir.join("總覽.docx"), minimal_zip_bytes()).unwrap();
        let sender = RecordingSender {
            docs: Arc::new(Mutex::new(Vec::new())),
            texts: Arc::new(Mutex::new(Vec::new())),
            fail_docs: false,
        };
        assert_eq!(
            sweep_undeclared_deliverables(&agent_dir, &home, &sender).await,
            1
        );
        let idx = crate::artifacts::provenance_index(&home, Some("ops"));
        assert_eq!(idx.len(), 1);
        assert_eq!(
            idx.values().next().unwrap().origin,
            crate::artifacts::ArtifactOrigin::Swept
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn process_deliverables_degrades_on_send_failure() {
        let home = std::env::temp_dir().join(format!("dd-proc-fail-{}", uuid::Uuid::new_v4()));
        let agent_dir = home.join("agents").join("a");
        std::fs::create_dir_all(&agent_dir).unwrap();
        let file = agent_dir.join("out.docx");
        // A valid zip so this test actually reaches `send_document` (and its
        // simulated failure) rather than being rejected earlier by the
        // WP-4H delivery gate — the assertions below are about the
        // send-failure degrade path, not the gate.
        std::fs::write(&file, minimal_zip_bytes()).unwrap();

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

    // ── undeclared-deliverable sweep ────────────────────────────────

    #[test]
    fn reply_mentions_document_gate() {
        assert!(reply_mentions_document("Word 檔已產出：report.docx"));
        assert!(reply_mentions_document("已完成簡報初稿"));
        assert!(reply_mentions_document("The Excel summary is ready"));
        assert!(!reply_mentions_document("今天天氣不錯，行程已確認"));
    }

    #[test]
    fn sweep_candidates_picks_recent_office_files_only() {
        let home = std::env::temp_dir().join(format!("dd-sweep-{}", uuid::Uuid::new_v4()));
        let agent_dir = home.join("agents").join("a");
        std::fs::create_dir_all(agent_dir.join("attachments")).unwrap();
        std::fs::create_dir_all(agent_dir.join("out")).unwrap();

        // Fresh deliverable in the workdir root + one in a subdir.
        std::fs::write(agent_dir.join("報告.docx"), b"d1").unwrap();
        std::fs::write(agent_dir.join("out").join("data.xlsx"), b"d2").unwrap();
        // Internal-state file types are never swept.
        std::fs::write(agent_dir.join("SOUL.md"), b"soul").unwrap();
        std::fs::write(agent_dir.join("state.json"), b"{}").unwrap();
        // Files already under attachments/ are archives, not candidates.
        std::fs::write(agent_dir.join("attachments").join("old.docx"), b"a").unwrap();

        let got = sweep_candidates(&agent_dir);
        let names: Vec<String> = got
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(got.len(), 2, "got: {names:?}");
        assert!(names.contains(&"報告.docx".to_string()));
        assert!(names.contains(&"data.xlsx".to_string()));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn sweep_skips_already_archived_copy() {
        let home = std::env::temp_dir().join(format!("dd-sweep-dup-{}", uuid::Uuid::new_v4()));
        let agent_dir = home.join("agents").join("a");
        std::fs::create_dir_all(agent_dir.join("attachments")).unwrap();
        // Workdir file whose archived copy (same sanitized name + size)
        // already exists — a previous turn delivered it.
        std::fs::write(agent_dir.join("report.docx"), b"same-bytes").unwrap();
        std::fs::write(
            agent_dir.join("attachments").join("1785000000000_report.docx"),
            b"same-bytes",
        )
        .unwrap();
        assert!(sweep_candidates(&agent_dir).is_empty());

        // Changed content (different size) → delivered again.
        std::fs::write(agent_dir.join("report.docx"), b"same-bytes-v2!").unwrap();
        assert_eq!(sweep_candidates(&agent_dir).len(), 1);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn sweep_delivers_and_archives_undeclared_file() {
        let home = std::env::temp_dir().join(format!("dd-sweep-send-{}", uuid::Uuid::new_v4()));
        let agent_dir = home.join("agents").join("a");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(agent_dir.join("總覽.docx"), minimal_zip_bytes()).unwrap();
        let docs = Arc::new(Mutex::new(Vec::new()));
        let sender = RecordingSender {
            docs: docs.clone(),
            texts: Arc::new(Mutex::new(Vec::new())),
            fail_docs: false,
        };
        let sent = sweep_undeclared_deliverables(&agent_dir, &home, &sender).await;
        assert_eq!(sent, 1);
        assert_eq!(docs.lock().unwrap().len(), 1);
        // Archived copy exists → an immediate second sweep is a no-op.
        let sent2 = sweep_undeclared_deliverables(&agent_dir, &home, &sender).await;
        assert_eq!(sent2, 0, "dedup against the archive must hold");
        let _ = std::fs::remove_dir_all(&home);
    }

    // ── WP-4H: delivery gate integration (end-to-end through deliver_one) ──

    /// A hard-failing file (here: 0 bytes) must never reach `send_document`
    /// or the attachments/ archive, and the reply text must still tell the
    /// human user why nothing arrived — the existing degrade-to-text-note
    /// path is how the gate's rejection reaches the user, since the agent's
    /// own turn has already ended by the time this runs.
    #[tokio::test]
    async fn process_deliverables_blocks_empty_file_end_to_end() {
        let home = std::env::temp_dir().join(format!("dd-gate-e2e-{}", uuid::Uuid::new_v4()));
        let agent_dir = home.join("agents").join("a");
        std::fs::create_dir_all(&agent_dir).unwrap();
        let file = agent_dir.join("empty.docx");
        std::fs::write(&file, b"").unwrap();

        let docs = Arc::new(Mutex::new(Vec::new()));
        let sender = RecordingSender {
            docs: docs.clone(),
            texts: Arc::new(Mutex::new(Vec::new())),
            fail_docs: false,
        };

        let reply = format!("已完成。\n📎DELIVER:{}", file.to_str().unwrap());
        let out = process_deliverables(&reply, &agent_dir, &home, &sender).await;

        // Nothing was sent through the channel.
        assert!(docs.lock().unwrap().is_empty());
        // Nothing was archived either — a rejected file isn't a delivery.
        let archived = std::fs::read_dir(agent_dir.join("attachments"))
            .map(|d| d.count())
            .unwrap_or(0);
        assert_eq!(archived, 0, "a hard-rejected file must not be archived");
        // The user still learns something was attempted and why it failed.
        assert!(out.starts_with("已完成。"));
        assert!(out.contains("檔案傳送失敗"));
        assert!(out.contains("空的"), "{out}");

        let _ = std::fs::remove_dir_all(&home);
    }

    /// `[office] delivery_gate = false` restores pre-WP-4H behavior: even an
    /// empty file is sent through untouched.
    #[tokio::test]
    async fn process_deliverables_gate_disabled_sends_even_broken_file() {
        let home = std::env::temp_dir().join(format!("dd-gate-off-e2e-{}", uuid::Uuid::new_v4()));
        let agent_dir = home.join("agents").join("a");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(&home.join("config.toml"), "[office]\ndelivery_gate = false\n").unwrap();
        let file = agent_dir.join("empty.docx");
        std::fs::write(&file, b"").unwrap();

        let docs = Arc::new(Mutex::new(Vec::new()));
        let sender = RecordingSender {
            docs: docs.clone(),
            texts: Arc::new(Mutex::new(Vec::new())),
            fail_docs: false,
        };

        let reply = format!("已完成。\n📎DELIVER:{}", file.to_str().unwrap());
        let out = process_deliverables(&reply, &agent_dir, &home, &sender).await;

        assert_eq!(docs.lock().unwrap().len(), 1, "gate off must let even a broken file through");
        assert_eq!(out, "已完成。");

        let _ = std::fs::remove_dir_all(&home);
    }
}
