//! Claude Code session-transcript parsing (WP-9A): noise filtering + PII
//! redaction, kept apart from `claude_code.rs`'s orchestration.
//!
//! Signal-to-noise measured on-machine (`commercial/docs/DESIGN-runtime-state-sync-2026-08.md`
//! §1.6): human prompt text is ~1.1% of transcript bytes, assistant final-reply
//! text ~0.4% — combined 1.5%. The remaining 98.5% (hook `attachment` output,
//! `thinking` blocks, `tool_use`/`tool_result` payloads, `queue-operation`,
//! `file-history-snapshot`, ...) is discarded here, never written to
//! `sessions.db` / `memory.db`. [`classify_line`] is the single point that
//! decides signal vs. noise for one JSONL record — every other record `type`
//! (documented or not, current or future) falls through to `LineKind::Noise`
//! so an unrecognised type can never crash the import (design R1).

use std::io::BufRead as _;
use std::path::{Path, PathBuf};

use duduclaw_redaction::{Profile, RuleEngine, Source};

/// Hard per-line byte cap (design §3.5 R4). A line over this size is skipped
/// (counted, never parsed) rather than pulled fully into memory — guards
/// against the pathological outlier line in a 54 MB session file.
pub(super) const MAX_LINE_BYTES: usize = 4 * 1024 * 1024;

/// Per-session turn cap (design §3.5). Beyond this the session is truncated
/// and the caller reports PARTIAL — never silently dropped.
pub(super) const MAX_TURNS_PER_SESSION: usize = 2000;

// ---------------------------------------------------------------------------
// Pure line classifier
// ---------------------------------------------------------------------------

/// What one parsed JSONL record contributes to the imported transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum LineKind {
    /// A genuine human prompt (`type=user`, `origin.kind` absent-or-"human",
    /// not a meta/compact-summary synthetic record).
    Human(String),
    /// An assistant final-reply text block (`type=assistant`, `text` blocks
    /// only — `thinking`/`tool_use` are dropped).
    Assistant(String),
    /// A CLI-generated session title (`type=ai-title`).
    AiTitle(String),
    /// Everything else: hook attachments, tool results, queue operations,
    /// file-history snapshots, mode switches, synthetic user records
    /// (`origin.kind` = task-notification/peer, `isMeta`, `isCompactSummary`),
    /// and any unrecognised/future record `type`.
    Noise,
}

/// Classify one already-parsed JSONL record. Pure — no I/O, fully unit
/// testable against fixture `serde_json::Value`s mirroring real records
/// (see `commercial/docs/DESIGN-runtime-state-sync-2026-08.md` §1.2(a)).
pub(super) fn classify_line(v: &serde_json::Value) -> LineKind {
    match v.get("type").and_then(|t| t.as_str()) {
        Some("ai-title") => {
            let title = v
                .get("aiTitle")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .trim();
            if title.is_empty() {
                LineKind::Noise
            } else {
                LineKind::AiTitle(title.to_string())
            }
        }
        Some("user") => {
            // Synthetic user-role records are not genuine human input:
            // `origin.kind` ∈ {task-notification, peer} (real-data-verified,
            // §1.2(a)); `isMeta` / `isCompactSummary` mark CLI-synthesised
            // bookkeeping records.
            if let Some(kind) = v.pointer("/origin/kind").and_then(|k| k.as_str())
                && kind != "human"
            {
                return LineKind::Noise;
            }
            if v.get("isMeta").and_then(|b| b.as_bool()) == Some(true) {
                return LineKind::Noise;
            }
            if v.get("isCompactSummary").and_then(|b| b.as_bool()) == Some(true) {
                return LineKind::Noise;
            }
            let text = extract_user_text(v);
            if text.trim().is_empty() {
                LineKind::Noise
            } else {
                LineKind::Human(text)
            }
        }
        Some("assistant") => {
            let text = extract_assistant_text(v);
            if text.trim().is_empty() {
                LineKind::Noise
            } else {
                LineKind::Assistant(text)
            }
        }
        // `attachment` / `queue-operation` / `last-prompt` / `system` /
        // `mode` / `permission-mode` / `file-history-snapshot(-delta)` /
        // `pr-link` / `frame-link` / anything future/unrecognised.
        _ => LineKind::Noise,
    }
}

/// `message.content` for a `user` record is a plain string OR an array of
/// blocks (real-data-verified block `type`s: `text` / `tool_result` /
/// `image`). Only `text` blocks are genuine human-authored prompt text —
/// `tool_result` is the CLI feeding a tool's output back to itself, not
/// something the human typed.
fn extract_user_text(v: &serde_json::Value) -> String {
    match v.pointer("/message/content") {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// `message.content` for an `assistant` record is an array of blocks
/// (real-data-verified: `text` / `thinking` / `tool_use`). Only `text` is
/// the final reply surfaced to the user — `thinking` is private
/// chain-of-thought and `tool_use` is a function call, neither is "the
/// assistant's reply".
fn extract_assistant_text(v: &serde_json::Value) -> String {
    match v.pointer("/message/content") {
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("\n\n"),
        Some(serde_json::Value::String(s)) => s.clone(),
        _ => String::new(),
    }
}

// ---------------------------------------------------------------------------
// Streaming file extraction (I/O)
// ---------------------------------------------------------------------------

/// The signal extracted from one session `.jsonl` file after noise filtering.
#[derive(Debug, Clone, Default)]
pub(super) struct SessionExtract {
    /// First non-empty `cwd` seen in the file (real project path — the
    /// escaped directory slug is NOT reversible, so this is the only way
    /// back to it; design R3).
    pub cwd: Option<String>,
    pub ai_title: Option<String>,
    /// Ordered `(role, text)` turns, `role` ∈ {"user", "assistant"}.
    pub turns: Vec<(&'static str, String)>,
    /// Last `timestamp` seen (ISO8601) — used as the L3 summary's `valid_from`
    /// approximation (the session's own historical time, not import time).
    pub last_timestamp: Option<String>,
    pub noise_lines: u64,
    pub unparsed_lines: u64,
    pub oversized_lines: u64,
    /// `true` when [`MAX_TURNS_PER_SESSION`] was hit — the caller must report
    /// PARTIAL, not silently drop the remainder.
    pub truncated: bool,
}

/// Stream a session `.jsonl` file line by line (never `read_to_string` — a
/// single session can be tens of MB; design R4) and classify every record.
pub(super) fn extract_session(path: &Path) -> std::io::Result<SessionExtract> {
    let file = std::fs::File::open(path)?;
    let reader = std::io::BufReader::new(file);
    let mut out = SessionExtract::default();

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => {
                out.unparsed_lines += 1;
                continue;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        if line.len() > MAX_LINE_BYTES {
            out.oversized_lines += 1;
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
            out.unparsed_lines += 1;
            continue;
        };

        if out.cwd.is_none()
            && let Some(cwd) = v.get("cwd").and_then(|c| c.as_str())
            && !cwd.trim().is_empty()
        {
            out.cwd = Some(cwd.to_string());
        }
        if let Some(ts) = v.get("timestamp").and_then(|t| t.as_str()) {
            out.last_timestamp = Some(ts.to_string());
        }

        match classify_line(&v) {
            LineKind::AiTitle(t) => out.ai_title = Some(t),
            LineKind::Human(t) => push_turn(&mut out, "user", t),
            LineKind::Assistant(t) => push_turn(&mut out, "assistant", t),
            LineKind::Noise => out.noise_lines += 1,
        }
    }

    Ok(out)
}

fn push_turn(out: &mut SessionExtract, role: &'static str, text: String) {
    if out.turns.len() >= MAX_TURNS_PER_SESSION {
        out.truncated = true;
        return;
    }
    out.turns.push((role, text));
}

// ---------------------------------------------------------------------------
// Real-path recovery (design R3)
// ---------------------------------------------------------------------------

/// 8-hex-char SHA-256 prefix, used to build stable subject/session keys from
/// a real filesystem path instead of Claude Code's lossy escaped directory
/// name (design R3 — CJK project names collapse to `-----`, colliding).
pub(super) fn sha8(input: &str) -> String {
    let digest = <sha2::Sha256 as sha2::Digest>::digest(input.as_bytes());
    format!("{digest:x}")[..8].to_string()
}

/// Best-effort repo-root resolution: walk up from `start` looking for a
/// `.git` entry (directory for a normal clone, file for a linked worktree)
/// and return that directory; fall back to `start` unchanged if none is
/// found (e.g. the source path no longer exists locally, or was never a git
/// repo).
///
/// Known limitation (documented, not silently overclaimed): for a *linked*
/// git worktree, `.git` is a file pointing at
/// `<main-repo>/.git/worktrees/<name>`, and this walk stops at the
/// worktree's own root rather than chasing that pointer back to the main
/// repo. Official Claude Code docs say the `memory/` directory is keyed by
/// git-repo-root, so two worktrees of the same repo could in principle
/// disagree with this function's answer — full `gitdir:` resolution is left
/// as a documented follow-up, not attempted here (design R3 note).
pub(super) fn resolve_subject_root(start: &Path) -> PathBuf {
    let mut cur = start;
    loop {
        if cur.join(".git").exists() {
            return cur.to_path_buf();
        }
        match cur.parent() {
            Some(p) => cur = p,
            None => return start.to_path_buf(),
        }
    }
}

// ---------------------------------------------------------------------------
// PII redaction (design §3.4 — transcripts only, never memory shards)
// ---------------------------------------------------------------------------

/// Build the PII detection engine from the bundled `general` redaction
/// profile (email / phone / IPv4 / JWT / cloud API key shapes — the same
/// rule set `duduclaw-redaction` uses everywhere else in the project).
///
/// Deliberately does **not** go through `RedactionPipeline` /
/// `VaultStore`: import is one-way (source is read-only, never restored to
/// its original value), so there is nothing to reverse — masking
/// permanently with the detection engine alone avoids standing up a
/// per-session salt whose TTL/vault semantics the design doc explicitly
/// flagged as unresolved for this use case (§8 pending-decision 2).
/// Returns `None` when `enabled` is false or the profile fails to load
/// (fail-open to "no redaction" only in the disabled/build-error case —
/// never silently on a per-text basis).
pub(super) fn build_redaction_engine(enabled: bool) -> Option<RuleEngine> {
    if !enabled {
        return None;
    }
    let profile: Profile = duduclaw_redaction::profiles::load_builtin("general")
        .ok()
        .flatten()?;
    RuleEngine::from_specs(profile.into_specs()).ok()
}

/// Redact PII spans in `text` using the shared rule engine (the same
/// detection rules `duduclaw-redaction` applies to any other `ToolResult`
/// source). A hit is replaced with `[REDACTED:<CATEGORY>]`; `engine = None`
/// (redaction disabled) returns `text` unchanged.
pub(super) fn pii_redact(engine: Option<&RuleEngine>, text: &str) -> String {
    let Some(engine) = engine else {
        return text.to_string();
    };
    let source = Source::ToolResult {
        tool_name: "claude-code-transcript".to_string(),
    };
    let matches = engine.apply(text, &source);
    if matches.is_empty() {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0usize;
    for m in matches {
        out.push_str(&text[cursor..m.span.start]);
        out.push_str(&format!("[REDACTED:{}]", m.rule.category()));
        cursor = m.span.end;
    }
    out.push_str(&text[cursor..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── classify_line: noise filtering (core acceptance test) ──────────

    #[test]
    fn human_prompt_string_content_classified() {
        let v = json!({"type": "user", "message": {"content": "幫我修這個 bug"}});
        assert_eq!(classify_line(&v), LineKind::Human("幫我修這個 bug".to_string()));
    }

    #[test]
    fn human_prompt_array_content_keeps_only_text_blocks() {
        let v = json!({
            "type": "user",
            "message": {"content": [
                {"type": "text", "text": "看看這個結果"},
                {"type": "tool_result", "tool_use_id": "x", "content": "raw tool output noise"}
            ]}
        });
        assert_eq!(classify_line(&v), LineKind::Human("看看這個結果".to_string()));
    }

    #[test]
    fn user_task_notification_origin_is_noise() {
        let v = json!({
            "type": "user",
            "origin": {"kind": "task-notification"},
            "message": {"content": "synthetic notification text"}
        });
        assert_eq!(classify_line(&v), LineKind::Noise);
    }

    #[test]
    fn user_peer_origin_is_noise() {
        let v = json!({
            "type": "user",
            "origin": {"kind": "peer"},
            "message": {"content": "peer relay text"}
        });
        assert_eq!(classify_line(&v), LineKind::Noise);
    }

    #[test]
    fn user_meta_and_compact_summary_are_noise() {
        let meta = json!({"type": "user", "isMeta": true, "message": {"content": "meta text"}});
        assert_eq!(classify_line(&meta), LineKind::Noise);
        let compact = json!({"type": "user", "isCompactSummary": true, "message": {"content": "compact"}});
        assert_eq!(classify_line(&compact), LineKind::Noise);
    }

    #[test]
    fn assistant_text_block_kept_thinking_and_tool_use_dropped() {
        let v = json!({
            "type": "assistant",
            "message": {"content": [
                {"type": "thinking", "thinking": "let me reason about this...", "signature": "x"},
                {"type": "tool_use", "id": "1", "name": "Bash", "input": {"command": "ls"}},
                {"type": "text", "text": "已經修好了。"}
            ]}
        });
        assert_eq!(classify_line(&v), LineKind::Assistant("已經修好了。".to_string()));
    }

    #[test]
    fn assistant_multiple_text_blocks_joined() {
        let v = json!({
            "type": "assistant",
            "message": {"content": [
                {"type": "text", "text": "第一段"},
                {"type": "text", "text": "第二段"}
            ]}
        });
        assert_eq!(classify_line(&v), LineKind::Assistant("第一段\n\n第二段".to_string()));
    }

    #[test]
    fn assistant_pure_tool_use_is_noise() {
        let v = json!({
            "type": "assistant",
            "message": {"content": [
                {"type": "tool_use", "id": "1", "name": "Read", "input": {}}
            ]}
        });
        assert_eq!(classify_line(&v), LineKind::Noise);
    }

    #[test]
    fn ai_title_extracted() {
        let v = json!({"type": "ai-title", "aiTitle": "修復逐字稿匯入", "sessionId": "s1"});
        assert_eq!(classify_line(&v), LineKind::AiTitle("修復逐字稿匯入".to_string()));
    }

    #[test]
    fn hook_and_bookkeeping_types_are_noise() {
        for t in [
            "attachment",
            "queue-operation",
            "last-prompt",
            "system",
            "mode",
            "permission-mode",
            "file-history-snapshot",
            "file-history-snapshot-delta",
            "pr-link",
            "frame-link",
        ] {
            let v = json!({"type": t, "content": "irrelevant noise payload"});
            assert_eq!(classify_line(&v), LineKind::Noise, "type={t} must be noise");
        }
    }

    #[test]
    fn unknown_future_type_is_noise_not_a_panic() {
        // R1: a type introduced by a future Claude Code release must degrade
        // to Noise, never crash the parser.
        let v = json!({"type": "some-brand-new-2027-type", "payload": {"nested": true}});
        assert_eq!(classify_line(&v), LineKind::Noise);
    }

    // ── extract_session: streaming I/O + caps ───────────────────────────

    #[test]
    fn extract_session_streams_and_filters_real_shape_fixture() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("session.jsonl");
        let lines = vec![
            json!({"type": "user", "cwd": "/Users/x/proj", "timestamp": "2026-08-01T00:00:00Z", "message": {"content": "第一句人類提問"}}).to_string(),
            json!({"type": "attachment", "attachment": {"type": "hook_success"}}).to_string(),
            json!({"type": "assistant", "timestamp": "2026-08-01T00:00:05Z", "message": {"content": [{"type": "thinking", "thinking": "..."}, {"type": "text", "text": "第一句回覆"}]}}).to_string(),
            json!({"type": "queue-operation", "operation": "noop"}).to_string(),
            json!({"type": "ai-title", "aiTitle": "測試會話"}).to_string(),
            "not even json".to_string(),
        ];
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();

        let extract = extract_session(&path).unwrap();
        assert_eq!(extract.cwd.as_deref(), Some("/Users/x/proj"));
        assert_eq!(extract.ai_title.as_deref(), Some("測試會話"));
        assert_eq!(
            extract.turns,
            vec![
                ("user", "第一句人類提問".to_string()),
                ("assistant", "第一句回覆".to_string()),
            ]
        );
        // attachment + queue-operation are noise; ai-title is captured
        // separately (not counted as noise); the unparsed line is tracked
        // in its own counter.
        assert_eq!(extract.noise_lines, 2);
        assert_eq!(extract.unparsed_lines, 1);
        assert_eq!(extract.last_timestamp.as_deref(), Some("2026-08-01T00:00:05Z"));
        assert!(!extract.truncated);
    }

    #[test]
    fn extract_session_caps_turns_and_marks_truncated() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("big.jsonl");
        let mut buf = String::new();
        for i in 0..(MAX_TURNS_PER_SESSION + 5) {
            buf.push_str(&json!({"type": "user", "message": {"content": format!("prompt {i}")}}).to_string());
            buf.push('\n');
        }
        std::fs::write(&path, buf).unwrap();
        let extract = extract_session(&path).unwrap();
        assert_eq!(extract.turns.len(), MAX_TURNS_PER_SESSION);
        assert!(extract.truncated);
    }

    #[test]
    fn extract_session_skips_oversized_line_without_crashing() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("huge.jsonl");
        let huge_text = "x".repeat(MAX_LINE_BYTES + 10);
        let huge_line = json!({"type": "user", "message": {"content": huge_text}}).to_string();
        let normal_line = json!({"type": "user", "message": {"content": "normal prompt"}}).to_string();
        std::fs::write(&path, format!("{huge_line}\n{normal_line}\n")).unwrap();
        let extract = extract_session(&path).unwrap();
        assert_eq!(extract.oversized_lines, 1);
        assert_eq!(extract.turns, vec![("user", "normal prompt".to_string())]);
    }

    // ── sha8 / resolve_subject_root ──────────────────────────────────────

    #[test]
    fn sha8_is_deterministic_and_8_hex_chars() {
        let a = sha8("/Users/lizhixu/Project/DuDuClaw");
        let b = sha8("/Users/lizhixu/Project/DuDuClaw");
        assert_eq!(a, b);
        assert_eq!(a.len(), 8);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        // Different input → (overwhelmingly likely) different output.
        assert_ne!(a, sha8("/Users/lizhixu/Project/Other"));
    }

    #[test]
    fn resolve_subject_root_finds_git_dir_walking_up() {
        let tmp = tempfile::tempdir().unwrap();
        let repo_root = tmp.path().join("repo");
        let nested = repo_root.join("a").join("b").join("c");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir_all(repo_root.join(".git")).unwrap();

        let found = resolve_subject_root(&nested);
        assert_eq!(found, repo_root);
    }

    #[test]
    fn resolve_subject_root_falls_back_when_no_git_found() {
        let tmp = tempfile::tempdir().unwrap();
        let leaf = tmp.path().join("no").join("git").join("here");
        std::fs::create_dir_all(&leaf).unwrap();
        // tmp.path() itself has no .git ancestor (assuming test runs outside
        // a repo checkout at the tempdir root, which tempdir guarantees).
        let found = resolve_subject_root(&leaf);
        assert_eq!(found, leaf);
    }

    // ── redaction ─────────────────────────────────────────────────────

    #[test]
    fn pii_redact_masks_email_leaves_other_text_untouched() {
        let engine = build_redaction_engine(true).expect("general profile must load");
        let text = "請聯絡 alice@example.com 確認時間，其他不變。";
        let redacted = pii_redact(Some(&engine), text);
        assert!(!redacted.contains("alice@example.com"));
        assert!(redacted.contains("[REDACTED:EMAIL]"));
        assert!(redacted.contains("其他不變"));
    }

    #[test]
    fn pii_redact_disabled_returns_text_unchanged() {
        let text = "alice@example.com 不會被遮蔽";
        let redacted = pii_redact(None, text);
        assert_eq!(redacted, text);
    }

    #[test]
    fn build_redaction_engine_disabled_is_none() {
        assert!(build_redaction_engine(false).is_none());
    }

    #[test]
    fn pii_redact_clean_text_untouched_and_unallocated_path() {
        let engine = build_redaction_engine(true).unwrap();
        let text = "沒有任何敏感資訊的一般文字。";
        assert_eq!(pii_redact(Some(&engine), text), text);
    }
}
