//! WP-4H: **Delivery Gate** for the `📎DELIVER:` outbound path.
//!
//! Borrowed from the OfficeCLI harness research
//! (`research/harness-2026-08/officecli.md` §2.10 — "Delivery Gate: any
//! failure = REJECT, do NOT deliver"): put the cheap, deterministic checks
//! *before* a produced file ever reaches a channel, and separate them into
//! two tiers by how confident a zero-LLM check can be:
//!
//! - **Hard failures** (file objectively broken — empty, wrong magic bytes
//!   for its own claimed extension, or a zip container that won't open) ⇒
//!   the file is **never sent**. `office_docs::deliver_one` propagates the
//!   rejection through its existing degrade-to-text-note path, so the user
//!   always learns *why* nothing arrived instead of silently getting
//!   nothing.
//! - **Soft signals** (leftover template placeholders in a text-extractable
//!   format) ⇒ logged and reported, delivery proceeds unchanged. A
//!   zero-LLM keyword/pattern scan cannot tell "this TODO is a leftover
//!   placeholder" from "this document legitimately discusses a TODO list" —
//!   hard-blocking on that ambiguity would manufacture false-positive
//!   rejections of real work, which is worse than an occasional slip-through
//!   (see `docs/todo` conventions on graduated confidence). Advanced users
//!   can opt into hard-blocking via `delivery_gate_placeholder_block`.
//!
//! Everything here is pure byte-level inspection — zero LLM calls, zero
//! network calls. The only I/O is the best-effort audit/Activity Feed write
//! on a verdict, which never turns into a delivery failure of its own.
//!
//! Scope boundary: this module owns the **outbound** `📎DELIVER:` path only.
//! Inbound attachment parsing/resource limits (WP-4G, `document_limits.rs`)
//! are a separate concern and are not touched here.

use std::path::Path;

use serde::Deserialize;
use tracing::warn;

use crate::artifacts::derive_home_and_agent;

// ── `[office]` config ────────────────────────────────────────────────────

/// `config.toml [office]` — parsed in isolation from a generic `toml::Table`
/// (mirrors `GoalLoopConfig::from_home`), so an absent or malformed section —
/// or an error anywhere else in `config.toml` — can never break this one.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(default)]
pub struct OfficeGateConfig {
    /// Master switch for the whole delivery gate. `false` restores
    /// pre-WP-4H behavior byte-for-byte: every `📎DELIVER:` file is sent
    /// as-is, no checks, no audit rows.
    pub delivery_gate: bool,
    /// Promotes the placeholder-residue scan (normally warn-only) into a
    /// hard failure. Off by default — a false positive would silently block
    /// a legitimate delivery, which is a worse failure mode than an
    /// occasional undetected placeholder slipping through. Advanced users
    /// who would rather err toward blocking can opt in.
    pub delivery_gate_placeholder_block: bool,
}

impl Default for OfficeGateConfig {
    fn default() -> Self {
        Self {
            delivery_gate: true,
            delivery_gate_placeholder_block: false,
        }
    }
}

impl OfficeGateConfig {
    /// Load `[office]` from `<home>/config.toml`. Absent file, absent
    /// section, or a parse error anywhere ⇒ [`OfficeGateConfig::default`].
    pub fn from_home(home_dir: &Path) -> Self {
        let path = home_dir.join("config.toml");
        let Ok(content) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        let Ok(table) = content.parse::<toml::Table>() else {
            return Self::default();
        };
        match table.get("office") {
            Some(section) => section
                .clone()
                .try_into::<OfficeGateConfig>()
                .unwrap_or_default(),
            None => Self::default(),
        }
    }
}

// ── Magic-byte table (hard failures) ────────────────────────────────────

/// ZIP local-file-header signature — every OOXML package (docx/xlsx/pptx)
/// starts with this. Exact byte-prefix match (coding convention 2: magic
/// checks compare precise byte prefixes, never fuzzy substring matching).
const ZIP_MAGIC: &[u8] = b"PK\x03\x04";
const PDF_MAGIC: &[u8] = b"%PDF";
const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";
const JPEG_MAGIC: &[u8] = &[0xFF, 0xD8, 0xFF];

/// `(extension, expected magic prefix, human-readable label)`. Only
/// extensions with one unambiguous signature are listed — an extension not
/// in this table skips the magic check entirely ("only verify formats whose
/// claim is unambiguous; unknown extensions are skipped").
const MAGIC_TABLE: &[(&str, &[u8], &str)] = &[
    ("docx", ZIP_MAGIC, "Word 文件（docx）"),
    ("xlsx", ZIP_MAGIC, "Excel 試算表（xlsx）"),
    ("pptx", ZIP_MAGIC, "PowerPoint 簡報（pptx）"),
    ("pdf", PDF_MAGIC, "PDF 文件"),
    ("png", PNG_MAGIC, "PNG 圖片"),
    ("jpg", JPEG_MAGIC, "JPEG 圖片"),
    ("jpeg", JPEG_MAGIC, "JPEG 圖片"),
];

/// Extensions whose magic is [`ZIP_MAGIC`] but must ALSO open as a
/// well-formed zip container. A truncated OOXML file often still starts with
/// a valid local-file-header (passing the magic check alone) but is missing
/// or has a broken central directory — [`zip::ZipArchive::new`] catches
/// that. Legacy binary Office formats (`doc`/`xls`/`ppt`, OLE-CFB) are
/// deliberately NOT in this table or [`MAGIC_TABLE`] — they are a different
/// container format entirely and checking them against zip magic would be
/// wrong, not stricter.
const ZIP_CONTAINER_EXTS: &[&str] = &["docx", "xlsx", "pptx"];

// ── Placeholder residue (soft signal) ───────────────────────────────────

/// Text-extractable formats scanned for leftover placeholder markers.
/// Binary office formats are not scanned here — extracting their text would
/// require the same parsers WP-4G already bounds on the ingest side; that is
/// out of scope for this zero-cost byte-level gate.
const TEXT_SCAN_EXTS: &[&str] = &["md", "txt", "csv", "html", "htm"];

/// Literal placeholder markers, ASCII-case-insensitive whole-word/phrase
/// match via [`duduclaw_core::word_contains_ci`]. `{{...}}` is handled
/// separately by [`contains_double_brace_placeholder`] since it is a
/// wildcard shape, not a literal string.
const PLACEHOLDER_KEYWORDS: &[&str] = &["TODO", "FIXME", "lorem ipsum", "[待填]", "（請填寫）"];

/// `true` when `text` contains a `{{ ... }}` pair (any content, including
/// empty) — the canonical shape of an unresolved template variable.
fn contains_double_brace_placeholder(text: &str) -> bool {
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        if after.contains("}}") {
            return true;
        }
        rest = after;
    }
    false
}

/// Scan `text` for every placeholder pattern; returns the distinct pattern
/// labels that hit (empty when clean).
fn scan_placeholders(text: &str) -> Vec<String> {
    let mut hits = Vec::new();
    if contains_double_brace_placeholder(text) {
        hits.push("{{...}}".to_string());
    }
    for kw in PLACEHOLDER_KEYWORDS {
        if duduclaw_core::word_contains_ci(text, kw) {
            hits.push((*kw).to_string());
        }
    }
    hits
}

// ── Pure check — zero I/O, directly unit-testable ───────────────────────

/// Evaluate one deliverable's bytes against the gate.
///
/// `Ok(warnings)`: the file may be delivered. `warnings` is the (possibly
/// empty) placeholder-residue list — a non-empty result never blocks
/// delivery unless `cfg.delivery_gate_placeholder_block` is set, in which
/// case it is folded into `Err` instead.
///
/// `Err(reason)`: a hard failure. `reason` is already a complete, actionable
/// zh-TW message safe to show a channel user directly — it names only the
/// file's own basename/extension, never an internal filesystem path.
///
/// `path` is used only for its extension and basename (for the message); the
/// bytes to check are `data`, already read by the caller — this function
/// does no filesystem I/O of its own.
pub fn evaluate_bytes(path: &Path, data: &[u8], cfg: &OfficeGateConfig) -> Result<Vec<String>, String> {
    let display_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("(檔案)");

    if data.is_empty() {
        return Err(format!(
            "交付檔案「{display_name}」是空的（0 bytes），已攔截未送出。請確認產生檔案的步驟有正常寫入內容，重新產出後再交付。"
        ));
    }

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());

    if let Some(ext) = ext.as_deref() {
        if let Some(&(_, magic, label)) = MAGIC_TABLE.iter().find(|(e, _, _)| *e == ext)
            && !data.starts_with(magic)
        {
            return Err(format!(
                "交付檔案「{display_name}」副檔名宣稱是{label}，但檔案開頭內容的格式簽章不符，已攔截未送出。請確認產出的檔案真的是這個格式（不是純文字或其他格式誤存副檔名），重新產出後再交付。"
            ));
        }
        if ZIP_CONTAINER_EXTS.contains(&ext) {
            let cursor = std::io::Cursor::new(data);
            if zip::ZipArchive::new(cursor).is_err() {
                return Err(format!(
                    "交付檔案「{display_name}」的壓縮封裝已損毀或被截斷，無法開啟，已攔截未送出。這通常是寫入中斷或磁碟空間不足造成的，請重新產生這個檔案後再交付。"
                ));
            }
        }
    }

    let mut warnings = Vec::new();
    if let Some(ext) = ext.as_deref()
        && TEXT_SCAN_EXTS.contains(&ext)
    {
        let text = String::from_utf8_lossy(data);
        let hits = scan_placeholders(&text);
        if !hits.is_empty() {
            if cfg.delivery_gate_placeholder_block {
                return Err(format!(
                    "交付檔案「{display_name}」偵測到疑似未完成的佔位殘留（{}），已攔截未送出。請確認內容已填寫完整後再交付；若這是合法內容誤判，可在設定關閉 delivery_gate_placeholder_block 改回僅提醒不攔截。",
                    hits.join("、")
                ));
            }
            warnings = hits;
        }
    }
    Ok(warnings)
}

// ── Async wrapper: config gate + audit/Activity Feed side effects ──────

/// Run the delivery gate for one file about to be sent through
/// `📎DELIVER:` (either the agent's own declaration or the undeclared
/// sweep). Zero LLM cost.
///
/// - `cfg.delivery_gate == false` ⇒ byte-identical to pre-WP-4H behavior:
///   always `Ok(vec![])`, no audit/Activity Feed writes.
/// - Hard failure ⇒ `Err(actionable zh-TW message)`. The caller
///   (`office_docs::deliver_one`) is expected to propagate this through the
///   SAME degrade-to-text-note path that already handles read/archive/send
///   failures — the delivery-gate path in this codebase currently has no
///   way to hand feedback back into the agent's own turn (it already ended
///   by the time this runs), so the honest fallback is: never send the
///   file, and tell the human user why via the reply text, with the full
///   detail in the audit trail for the agent's own future reference.
/// - Soft signal ⇒ `Ok(warnings)`, delivery proceeds; one Activity Feed row
///   is appended purely for visibility.
pub async fn run_delivery_gate(
    path: &Path,
    data: &[u8],
    agent_dir: &Path,
    home_dir: &Path,
    cfg: &OfficeGateConfig,
) -> Result<Vec<String>, String> {
    if !cfg.delivery_gate {
        return Ok(Vec::new());
    }
    let (_, agent_id) = derive_home_and_agent(agent_dir);
    match evaluate_bytes(path, data, cfg) {
        Err(reason) => {
            record_gate_event(
                home_dir,
                &agent_id,
                path,
                "office.delivery_gate_blocked",
                &reason,
            )
            .await;
            Err(reason)
        }
        Ok(warnings) => {
            if !warnings.is_empty() {
                let filename = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("(檔案)");
                let summary = format!(
                    "交付檔案「{filename}」偵測到疑似佔位殘留（未攔截，僅提醒）：{}",
                    warnings.join("、")
                );
                record_gate_event(
                    home_dir,
                    &agent_id,
                    path,
                    "office.delivery_gate_warning",
                    &summary,
                )
                .await;
            }
            Ok(warnings)
        }
    }
}

/// Best-effort audit log + Activity Feed append for a gate verdict.
/// Every failure (lock error, missing task DB, serialization) degrades to
/// "the event isn't recorded" — recording a verdict must never itself turn
/// into a delivery failure.
async fn record_gate_event(
    home_dir: &Path,
    agent_id: &str,
    path: &Path,
    event_type: &str,
    detail: &str,
) {
    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("(unknown)");
    let severity = if event_type == "office.delivery_gate_blocked" {
        duduclaw_security::audit::Severity::Warning
    } else {
        duduclaw_security::audit::Severity::Info
    };
    duduclaw_security::audit::append_audit_event(
        home_dir,
        &duduclaw_security::audit::AuditEvent::new(
            event_type,
            agent_id,
            severity,
            serde_json::json!({
                "filename": filename,
                "detail": duduclaw_core::truncate_chars(detail, 400),
            }),
        ),
    );

    match crate::task_store::TaskStore::open(home_dir) {
        Ok(store) => {
            let row = crate::task_store::ActivityRow {
                id: uuid::Uuid::new_v4().to_string(),
                event_type: event_type.to_string(),
                agent_id: agent_id.to_string(),
                task_id: None,
                summary: duduclaw_core::truncate_chars(detail, 400),
                timestamp: chrono::Utc::now().to_rfc3339(),
                metadata: Some(serde_json::json!({ "filename": filename }).to_string()),
            };
            if let Err(e) = store.append_activity(&row).await {
                warn!(error = %e, "artifact_gate: activity append failed (non-fatal)");
            }
        }
        Err(e) => {
            warn!(error = %e, agent = agent_id, filename = %filename, "artifact_gate: TaskStore open failed — audit-only");
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal well-formed zip archive whose single entry holds
    /// `content` — valid stand-in for docx/xlsx/pptx test fixtures.
    /// Deliberately does not attempt real OOXML internals
    /// (`[Content_Types].xml` etc.) — the gate only checks zip-container
    /// health, not OOXML schema validity.
    fn minimal_zip_bytes_with_content(content: &[u8]) -> Vec<u8> {
        use std::io::Write;
        let mut zw = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let opts =
            zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zw.start_file("test.txt", opts).unwrap();
        zw.write_all(content).unwrap();
        zw.finish().unwrap().into_inner()
    }

    fn minimal_zip_bytes() -> Vec<u8> {
        minimal_zip_bytes_with_content(b"hello")
    }

    // ① 零位元組拒絕
    #[test]
    fn zero_bytes_rejected() {
        let err = evaluate_bytes(Path::new("out.docx"), &[], &OfficeGateConfig::default())
            .expect_err("empty file must be a hard failure");
        assert!(err.contains("空的"), "{err}");
        assert!(err.contains("out.docx"), "{err}");
    }

    // ② magic 不符拒絕（副檔名 .docx 內容是純文字）
    #[test]
    fn magic_mismatch_rejected_for_docx_with_plain_text() {
        let data = b"hello world, this is definitely not a docx file";
        let err = evaluate_bytes(Path::new("report.docx"), data, &OfficeGateConfig::default())
            .expect_err("plain text under a .docx extension must be a hard failure");
        assert!(err.contains("簽章不符"), "{err}");
    }

    /// A zip that opens fine but claims to be a docx while actually being a
    /// jpeg (magic mismatch even though it IS a real file of *some* format)
    /// — same code path, different concrete bytes.
    #[test]
    fn magic_mismatch_rejected_for_png_with_jpeg_bytes() {
        let data = [0xFFu8, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
        let err = evaluate_bytes(Path::new("photo.png"), &data, &OfficeGateConfig::default())
            .expect_err("jpeg bytes under a .png extension must be a hard failure");
        assert!(err.contains("簽章不符"), "{err}");
    }

    /// docx/xlsx/pptx whose magic matches (`PK\x03\x04`) but whose zip
    /// container is truncated must still be rejected — the magic-byte check
    /// alone cannot catch this.
    #[test]
    fn truncated_zip_container_rejected_despite_correct_magic() {
        let full = minimal_zip_bytes();
        assert!(full.len() > 16, "fixture too small to truncate meaningfully");
        let truncated = &full[..12]; // keeps the PK\x03\x04 header, drops the rest
        assert!(truncated.starts_with(ZIP_MAGIC));
        let err = evaluate_bytes(Path::new("out.xlsx"), truncated, &OfficeGateConfig::default())
            .expect_err("truncated zip container must be a hard failure");
        assert!(err.contains("損毀") || err.contains("截斷"), "{err}");
    }

    // ③ 合法小 docx 通過
    #[test]
    fn legit_minimal_docx_passes() {
        let data = minimal_zip_bytes();
        let warnings = evaluate_bytes(Path::new("report.docx"), &data, &OfficeGateConfig::default())
            .expect("a well-formed zip under .docx must pass");
        assert!(warnings.is_empty());
    }

    // ④ 佔位殘留 warn 不擋
    #[test]
    fn placeholder_residue_warns_but_does_not_block() {
        let data = b"# Report\n\nCustomer: {{customer_name}}\n\nTODO: fill in totals\n";
        let warnings = evaluate_bytes(Path::new("draft.md"), data, &OfficeGateConfig::default())
            .expect("placeholder residue must be warn-only by default, not a hard failure");
        assert!(warnings.contains(&"{{...}}".to_string()), "{warnings:?}");
        assert!(warnings.contains(&"TODO".to_string()), "{warnings:?}");
    }

    /// Legitimate content that happens to contain the word "TODO" as prose
    /// (not a placeholder) still only warns — hard-blocking here would be a
    /// false-positive delivery failure for real work.
    #[test]
    fn placeholder_scan_is_advisory_for_real_content_too() {
        let data = "本週的 TODO 清單已經全部完成。".as_bytes();
        let warnings = evaluate_bytes(Path::new("summary.txt"), data, &OfficeGateConfig::default())
            .expect("must never hard-fail by default");
        assert!(!warnings.is_empty());
    }

    /// Binary formats (docx) are never placeholder-scanned even when an
    /// inner zip entry's content literally contains "TODO" — the scan is
    /// restricted to [`TEXT_SCAN_EXTS`], docx is a container format whose
    /// text lives inside compressed XML this zero-LLM gate never unzips.
    #[test]
    fn placeholder_scan_skips_non_text_extensions() {
        assert!(!TEXT_SCAN_EXTS.contains(&"docx"));
        let data = minimal_zip_bytes_with_content(b"TODO: this text is inside a docx zip entry");
        let warnings = evaluate_bytes(Path::new("report.docx"), &data, &OfficeGateConfig::default())
            .expect("a well-formed zip under .docx must still pass");
        assert!(
            warnings.is_empty(),
            "docx must never be placeholder-scanned: {warnings:?}"
        );
    }

    // ⑤ delivery_gate=false 全放行
    #[tokio::test]
    async fn delivery_gate_disabled_allows_everything() {
        let home = std::env::temp_dir().join(format!("dd-gate-off-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();
        let agent_dir = home.join("agents").join("a");
        std::fs::create_dir_all(&agent_dir).unwrap();
        let cfg = OfficeGateConfig {
            delivery_gate: false,
            delivery_gate_placeholder_block: false,
        };
        // Would be a hard failure (empty) if the gate were on.
        let result =
            run_delivery_gate(Path::new("out.docx"), &[], &agent_dir, &home, &cfg).await;
        assert!(result.is_ok(), "{result:?}");
        let _ = std::fs::remove_dir_all(&home);
    }

    // ⑥ placeholder_block=true 時佔位變硬失敗
    #[test]
    fn placeholder_block_upgrades_to_hard_failure() {
        let cfg = OfficeGateConfig {
            delivery_gate: true,
            delivery_gate_placeholder_block: true,
        };
        let data = b"Dear {{customer_name}},\n\nTODO: finish this letter.\n";
        let err = evaluate_bytes(Path::new("letter.md"), data, &cfg)
            .expect_err("placeholder residue must be a hard failure when the flag is set");
        assert!(err.contains("佔位"), "{err}");
    }

    #[test]
    fn unknown_extension_skips_magic_and_placeholder_checks() {
        // `.bin` is in neither MAGIC_TABLE nor TEXT_SCAN_EXTS — any non-empty
        // content passes untouched, warnings empty.
        let data = b"\x00\x01\x02arbitrary binary junk with the word TODO in it";
        let warnings = evaluate_bytes(Path::new("payload.bin"), data, &OfficeGateConfig::default())
            .expect("unknown extension must not be magic/placeholder checked");
        assert!(warnings.is_empty());
    }

    #[test]
    fn legacy_binary_office_extensions_are_not_zip_checked() {
        // .doc/.xls/.ppt are OLE-CFB, not zip — must not be forced through
        // the zip-magic/container check (that would be a false rejection of
        // a genuinely valid legacy file).
        let ole_magic = [0xD0u8, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1, 0, 0];
        let warnings = evaluate_bytes(Path::new("legacy.doc"), &ole_magic, &OfficeGateConfig::default())
            .expect("legacy OLE doc must not be rejected by the zip-shaped check");
        assert!(warnings.is_empty());
    }

    // ── config parsing ──────────────────────────────────────────────

    #[test]
    fn office_gate_config_defaults_when_absent() {
        let home = std::env::temp_dir().join(format!("dd-gate-cfg-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();
        let cfg = OfficeGateConfig::from_home(&home);
        assert!(cfg.delivery_gate);
        assert!(!cfg.delivery_gate_placeholder_block);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn office_gate_config_parses_section() {
        let home = std::env::temp_dir().join(format!("dd-gate-cfg2-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join("config.toml"),
            "[office]\ndelivery_gate = false\ndelivery_gate_placeholder_block = true\n",
        )
        .unwrap();
        let cfg = OfficeGateConfig::from_home(&home);
        assert!(!cfg.delivery_gate);
        assert!(cfg.delivery_gate_placeholder_block);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn office_gate_config_malformed_toml_degrades_to_default() {
        let home = std::env::temp_dir().join(format!("dd-gate-cfg3-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join("config.toml"), "not = [valid toml").unwrap();
        let cfg = OfficeGateConfig::from_home(&home);
        assert!(cfg.delivery_gate);
        let _ = std::fs::remove_dir_all(&home);
    }
}
