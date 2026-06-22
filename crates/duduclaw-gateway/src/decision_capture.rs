//! Decision Continuity capture & injection (RFC-24).
//!
//! When an agent sends a message offering an enumerated choice ("方案 A / B / C",
//! "Option 1 / 2", a lettered list under a "which one?" question), the content of
//! each option is fragile: it lives only in `session_messages`, which is window-
//! trimmed (last 20 turns) and destroyed by `compress()` (50k threshold → DELETE +
//! Haiku bullet summary). So when the user later replies "用方案 C" in a new turn /
//! session / process, the agent no longer holds what C was and either hallucinates
//! from unrelated history or has to ask again.
//!
//! This module captures such choices, the moment they are sent, into the Temporal
//! Memory **semantic** layer — an independent SQLite store that `compress()` never
//! touches — as `(decision:<id>, question|option:<key>|status)` triples. The
//! injection layer then surfaces still-open decisions into the system prompt so a
//! later "用方案 C" resolves from durable state, not conversation memory.
//!
//! Everything here is deterministic and zero-LLM: the detector is a conservative
//! line scanner (fail-safe — when unsure it captures nothing, RFC-24 §9), and the
//! store ride is `store_temporal` (RFC-24 §4.1). Wiring lives in `channel_reply`.

use duduclaw_core::text_utils::truncate_bytes;
use duduclaw_core::types::{MemoryEntry, MemoryLayer};
use duduclaw_memory::{SqliteMemoryEngine, TemporalMeta};
use sha2::{Digest, Sha256};

// ── P0.3 Triple encoding constants ──────────────────────────────────────────
//
// One decision is N+2 triples sharing `subject = "decision:<id>"`, distinguished
// by predicate. Centralised here so the writer (persist), reader (engine query),
// and resolver (MCP) never drift on a literal.

/// `predicate` for the decision's question text.
pub const PRED_QUESTION: &str = "question";
/// `predicate` for the decision's lifecycle status row.
pub const PRED_STATUS: &str = "status";
/// `object` of the status row while the decision awaits an answer.
pub const STATUS_OPEN: &str = "open";
/// `object` of the status row once a decision has expired unanswered (Phase 3 TTL).
pub const STATUS_EXPIRED: &str = "expired";

/// Build the shared `subject` for a decision id.
pub fn decision_subject(id: &str) -> String {
    format!("decision:{id}")
}

/// `predicate` for one option, keyed by its label (`A`, `1`, …).
pub fn pred_option(key: &str) -> String {
    format!("option:{key}")
}

/// `object` of the status row once resolved to a chosen option key.
pub fn status_resolved(key: &str) -> String {
    format!("resolved:{key}")
}

/// Per-option content byte budget (UTF-8-safe).
const MAX_OPTION_BYTES: usize = 2048;
/// Question byte budget (UTF-8-safe).
const MAX_QUESTION_BYTES: usize = 1024;
/// Max open decisions surfaced into the system prompt (RFC-24 §4.3).
pub const MAX_INJECTED_DECISIONS: usize = 5;

/// Choice-specific keywords. Deliberately narrow (RFC-24 §9 "寧漏勿錯"): weak words
/// like 建議/步驟 are excluded so a numbered *step* list ("1. 下載 2. 解壓") is not
/// mistaken for a decision. A labelled marker (方案/選項/Option) is itself a strong
/// signal and bypasses this gate; bare letter/digit lists must contain one of these.
const CHOICE_KEYWORDS: &[&str] = &[
    "方案",
    "選項",
    "哪一個",
    "哪個",
    "擇一",
    "二選一",
    "你想要哪",
    "選擇哪",
    "選哪",
    "option",
    "options",
    "choose",
    "which option",
    "prefer",
    "select one",
];

// ── P0.2 Draft + deterministic id ───────────────────────────────────────────

/// A detected choice before it is persisted (RFC-24 §5). Not stored directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionDraft {
    pub question: String,
    /// (key, content), in first-seen order.
    pub options: Vec<(String, String)>,
}

/// Deterministic 12-hex decision id from `agent_id` + the source message identity.
///
/// `Date.now()`/randomness are intentionally avoided so a re-capture of the same
/// outbound message yields the same id (idempotent supersession), and so the id is
/// reproducible across processes.
pub fn decision_id(agent_id: &str, source_message_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(agent_id.as_bytes());
    hasher.update(b":");
    hasher.update(source_message_id.as_bytes());
    let digest = hasher.finalize();
    let hex = digest
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    hex[..12].to_string()
}

// ── P1.1 Detector ───────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum MarkerKind {
    Fang,       // 方案 A
    Xuan,       // 選項 1
    OptionWord, // Option A
    Letter,     // A. / A) / A、
    Digit,      // 1. / 2) / 1️⃣
}

impl MarkerKind {
    /// Labelled markers are themselves a strong choice signal.
    fn is_labelled(self) -> bool {
        matches!(
            self,
            MarkerKind::Fang | MarkerKind::Xuan | MarkerKind::OptionWord
        )
    }
}

/// Detect an enumerated choice in an outbound assistant message.
///
/// Returns `Some` only when ALL hold (RFC-24 §4.2, conservative):
/// 1. ≥2 markers of the **same** kind are found (outside code fences);
/// 2. each marker has non-empty content (inline or on following lines);
/// 3. for bare letter/digit lists, a [`CHOICE_KEYWORDS`] term is present
///    (labelled 方案/選項/Option markers satisfy this inherently).
pub fn detect_enumerated_options(text: &str) -> Option<DecisionDraft> {
    let lines: Vec<&str> = text.lines().collect();

    // Pass 1: collect marker lines, skipping fenced code blocks.
    let mut in_fence = false;
    let mut markers: Vec<(usize, MarkerKind, String, String)> = Vec::new();
    for (i, raw) in lines.iter().enumerate() {
        let line = raw.trim();
        if line.starts_with("```") || line.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        if let Some((kind, key, rest)) = parse_marker(line) {
            markers.push((i, kind, key, rest));
        }
    }
    if markers.len() < 2 {
        return None;
    }

    // Homogeneity: all markers must share the same kind (a real choice is
    // consistent; mixing "方案 A" with "1." signals a non-decision layout).
    let kind0 = markers[0].1;
    if !markers.iter().all(|m| m.1 == kind0) {
        return None;
    }

    // Pass 2: build options, content spanning to the next marker line.
    let marker_lines: Vec<usize> = markers.iter().map(|m| m.0).collect();
    let mut options: Vec<(String, String)> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for (mi, (li, _kind, key, inline_rest)) in markers.iter().enumerate() {
        if seen.iter().any(|k| k == key) {
            continue; // dedupe repeated keys, keep first
        }
        let end = marker_lines.get(mi + 1).copied().unwrap_or(lines.len());
        let mut parts: Vec<String> = Vec::new();
        let cleaned = strip_leading_sep(inline_rest);
        if !cleaned.is_empty() {
            parts.push(cleaned.to_string());
        }
        for raw in &lines[(li + 1)..end] {
            let t = raw.trim();
            if t.is_empty() || t.starts_with("```") || t.starts_with("~~~") {
                continue;
            }
            parts.push(t.to_string());
        }
        let content = parts.join(" ");
        let content = content.trim();
        if content.is_empty() {
            continue;
        }
        seen.push(key.clone());
        options.push((
            key.clone(),
            truncate_bytes(content, MAX_OPTION_BYTES).to_string(),
        ));
    }
    if options.len() < 2 {
        return None;
    }

    // Keyword gate for bare (non-labelled) lists.
    if !kind0.is_labelled() && !has_choice_keyword(text) {
        return None;
    }

    // Question = text before the first marker line.
    let q_end = markers[0].0;
    let question_raw = lines[..q_end]
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let question = truncate_bytes(question_raw.trim(), MAX_QUESTION_BYTES).to_string();
    let question = if question.is_empty() {
        "(未命名決策)".to_string()
    } else {
        question
    };

    Some(DecisionDraft { question, options })
}

fn has_choice_keyword(text: &str) -> bool {
    CHOICE_KEYWORDS
        .iter()
        .any(|k| duduclaw_core::match_utils::word_contains_ci(text, k))
}

/// Parse a single line into `(kind, key, inline_rest)` if it begins with a marker.
fn parse_marker(line: &str) -> Option<(MarkerKind, String, String)> {
    // Strip leading list decoration (-, *, •, ・) and whitespace.
    let s = line
        .trim_start_matches(['-', '*', '•', '・', ' ', '\t', '　'])
        .trim_start();

    if let Some(rest) = s.strip_prefix("方案") {
        if let Some((key, tail)) = take_key(rest.trim_start()) {
            return Some((MarkerKind::Fang, key, tail.to_string()));
        }
    }
    if let Some(rest) = s.strip_prefix("選項") {
        if let Some((key, tail)) = take_key(rest.trim_start()) {
            return Some((MarkerKind::Xuan, key, tail.to_string()));
        }
    }
    if let Some(rest) = strip_prefix_ci(s, "option") {
        if let Some((key, tail)) = take_key(rest.trim_start()) {
            return Some((MarkerKind::OptionWord, key, tail.to_string()));
        }
    }
    // Emoji keycap digit: <digit>[FE0F]20E3
    if let Some((key, tail)) = take_emoji_keycap(s) {
        return Some((MarkerKind::Digit, key, tail.to_string()));
    }
    // Bare letter/digit followed by a separator (. ) 、). Colon excluded to avoid
    // capturing times like "9:30".
    take_bare_marker(s)
}

/// Read a single alphanumeric key char, returning (uppercased key, remainder).
fn take_key(s: &str) -> Option<(String, &str)> {
    let c0 = s.chars().next()?;
    if !c0.is_ascii_alphanumeric() {
        return None;
    }
    Some((c0.to_ascii_uppercase().to_string(), &s[c0.len_utf8()..]))
}

/// Match a leading `<letter|digit><sep>` marker where sep ∈ { . ) 、 }.
fn take_bare_marker(s: &str) -> Option<(MarkerKind, String, String)> {
    let mut it = s.chars();
    let c0 = it.next()?;
    if !c0.is_ascii_alphanumeric() {
        return None;
    }
    let sep = it.next()?;
    if !matches!(sep, '.' | ')' | '、') {
        return None;
    }
    let kind = if c0.is_ascii_digit() {
        MarkerKind::Digit
    } else {
        MarkerKind::Letter
    };
    let key = c0.to_ascii_uppercase().to_string();
    let tail = &s[c0.len_utf8() + sep.len_utf8()..];
    Some((kind, key, tail.to_string()))
}

/// Match a leading emoji keycap digit (`1️⃣`), returning (digit, remainder).
fn take_emoji_keycap(s: &str) -> Option<(String, &str)> {
    let d = s.chars().next()?;
    if !d.is_ascii_digit() {
        return None;
    }
    let mut rest = &s[d.len_utf8()..];
    let mut next = rest.chars().next()?;
    if next == '\u{fe0f}' {
        rest = &rest[next.len_utf8()..];
        next = rest.chars().next()?;
    }
    if next == '\u{20e3}' {
        return Some((d.to_string(), &rest[next.len_utf8()..]));
    }
    None
}

/// Case-insensitive ASCII prefix strip. Boundary-safe: `split_at(prefix.len())`
/// would panic when byte `prefix.len()` lands mid-char (CJK/emoji input), so the
/// char-boundary check guards it (`prefix` is always ASCII here).
fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    if s.len() < prefix.len() || !s.is_char_boundary(prefix.len()) {
        return None;
    }
    let (head, tail) = s.split_at(prefix.len());
    if head.eq_ignore_ascii_case(prefix) {
        Some(tail)
    } else {
        None
    }
}

/// Strip leading separators/space from option content.
fn strip_leading_sep(s: &str) -> &str {
    s.trim_start_matches([
        '：', ':', '.', '、', ')', '-', '—', '·', ' ', '\t', '　',
    ])
    .trim()
}

// ── P1.2 Persistence ────────────────────────────────────────────────────────

/// Persist a detected decision as N+2 temporal triples (RFC-24 §4.1).
///
/// Writes the question, each option, then the `status = open` row. Re-persisting
/// the same `(agent_id, decision_id)` is idempotent: each row's `(subject,
/// predicate)` supersedes its prior version via `store_temporal`.
pub async fn persist_decision(
    engine: &SqliteMemoryEngine,
    agent_id: &str,
    id: &str,
    draft: &DecisionDraft,
    ctx_meta: serde_json::Value,
) -> Result<(), String> {
    let subject = decision_subject(id);

    write_triple(
        engine,
        agent_id,
        &subject,
        PRED_QUESTION,
        &draft.question,
        &ctx_meta,
    )
    .await?;

    for (key, content) in &draft.options {
        write_triple(
            engine,
            agent_id,
            &subject,
            &pred_option(key),
            content,
            &ctx_meta,
        )
        .await?;
    }

    write_triple(
        engine,
        agent_id,
        &subject,
        PRED_STATUS,
        STATUS_OPEN,
        &ctx_meta,
    )
    .await?;
    Ok(())
}

async fn write_triple(
    engine: &SqliteMemoryEngine,
    agent_id: &str,
    subject: &str,
    predicate: &str,
    object: &str,
    ctx_meta: &serde_json::Value,
) -> Result<(), String> {
    let entry = MemoryEntry {
        id: uuid::Uuid::new_v4().to_string(),
        agent_id: agent_id.to_string(),
        content: object.to_string(),
        timestamp: chrono::Utc::now(),
        tags: vec!["decision".to_string(), subject.to_string()],
        embedding: None,
        layer: MemoryLayer::Semantic,
        importance: 7.0,
        access_count: 0,
        last_accessed: None,
        source_event: "decision_capture".to_string(),
    };
    let meta = TemporalMeta {
        subject: Some(subject.to_string()),
        predicate: Some(predicate.to_string()),
        object: Some(object.to_string()),
        valid_from: None,
        valid_until: None,
        confidence: Some(1.0),
        metadata: Some(ctx_meta.clone()),
    };
    engine
        .store_temporal(agent_id, entry, meta)
        .await
        .map(|_| ())
        .map_err(|e| format!("store decision triple ({predicate}): {e}"))
}

// ── P1.4 Injection ──────────────────────────────────────────────────────────

/// Render the agent's still-open decisions as a system-prompt section (RFC-24 §4.3).
///
/// Empty string when there are none (caller skips injection). Caps at
/// [`MAX_INJECTED_DECISIONS`]. Placed at the tail of the prompt (U-shaped
/// attention), not in the cached prefix.
pub async fn build_open_decisions_section(engine: &SqliteMemoryEngine, agent_id: &str) -> String {
    let decisions = match engine
        .list_open_decisions(agent_id, MAX_INJECTED_DECISIONS)
        .await
    {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(agent_id, error = %e, "list_open_decisions failed; skipping injection");
            return String::new();
        }
    };
    if decisions.is_empty() {
        return String::new();
    }

    let mut out = String::from(
        "## 待決事項 (Open Decisions)\n\
         你先前向使用者提出過以下選項，使用者可能會以「用方案 X / 選 X」回覆。\n\
         若使用者引用某個選項，直接依此內容執行，不要重新詢問、也不要從歷史臆測。\n\
         若手上沒有對應內容，先承認缺漏並查詢，切勿用模糊比對拼湊。\n",
    );
    for d in &decisions {
        out.push_str(&format!("\n[decision:{}] 題目：{}\n", d.id, d.question));
        for (key, content) in &d.options {
            out.push_str(&format!("  - {key}：{content}\n"));
        }
    }
    out
}

// ── P2.2 Auto-resolve reference detection ────────────────────────────────────

/// Detect that the user's message references one open decision's option (RFC-24
/// §4.4), e.g. "用方案 C" / "選 A" / "go with option B".
///
/// Returns `Some((decision_id, key))` only when **exactly one** open
/// `(decision, option)` is referenced. Ambiguity (two open decisions both having
/// the referenced key, or several keys mentioned) returns `None` so the agent
/// asks for clarification rather than guessing. Bare key matching is avoided —
/// the key must appear next to a choice anchor (方案/選項/選/用/option/…).
pub fn detect_decision_reference(
    user_text: &str,
    open: &[duduclaw_memory::DecisionView],
) -> Option<(String, String)> {
    let hay = user_text.to_lowercase();
    let mut hits: Vec<(String, String)> = Vec::new();
    for d in open {
        for (key, _) in &d.options {
            if key_referenced(&hay, key) && !hits.iter().any(|(i, k)| i == &d.id && k == key) {
                hits.push((d.id.clone(), key.clone()));
            }
        }
    }
    if hits.len() == 1 {
        hits.into_iter().next()
    } else {
        None // none, or ambiguous → let the agent clarify
    }
}

/// Whether the user message *looks like* it references a prior decision/option,
/// regardless of whether a matching open decision exists (RFC-24 §4.5).
///
/// Used to detect the Agnes failure shape — the user says "用方案 C" but no open
/// decision is on record — so the system can log a learning signal instead of
/// letting the agent guess from unrelated history. A choice anchor (方案/選項/
/// option) must be immediately followed (optionally across one space) by an
/// alphanumeric label, or one of the explicit "用方案"/"選方案" verbs is present.
pub fn mentions_decision_reference(user_text: &str) -> bool {
    let hay = user_text.to_lowercase();
    for verb in ["用方案", "選方案", "照方案", "用選項", "選選項", "用第", "選第"] {
        if hay.contains(verb) {
            return true;
        }
    }
    for anchor in ["方案", "選項", "option"] {
        let mut from = 0;
        while let Some(pos) = hay[from..].find(anchor) {
            let after = &hay[from + pos + anchor.len()..];
            if after
                .trim_start_matches([' ', '　'])
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphanumeric())
            {
                return true;
            }
            from += pos + anchor.len();
        }
    }
    false
}

/// Whether `hay` (already lowercased) references option `key` in a choice context.
fn key_referenced(hay: &str, key: &str) -> bool {
    let k = key.to_lowercase();
    let mut patterns = vec![
        format!("方案{k}"),
        format!("方案 {k}"),
        format!("選項{k}"),
        format!("選項 {k}"),
        format!("選{k}"),
        format!("選 {k}"),
        format!("用{k}"),
        format!("用 {k}"),
        format!("選擇{k}"),
        format!("選擇 {k}"),
        format!("option{k}"),
        format!("option {k}"),
        format!("{k}方案"),
        format!("{k} 方案"),
    ];
    if k.chars().all(|c| c.is_ascii_digit()) {
        patterns.push(format!("第{k}個"));
        patterns.push(format!("第 {k} 個"));
        patterns.push(format!("第{k}"));
    }
    patterns.iter().any(|p| hay.contains(p.as_str()))
}

// ── P3.1 Confidence classification + LLM second-pass ─────────────────────────

/// Three-way classification of an outbound message (RFC-24 §P3.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetectionResult {
    /// The deterministic detector is confident — persist directly (zero cost).
    Confident(DecisionDraft),
    /// Looks like a choice but failed the strict rules (mixed/unknown markers,
    /// inline options, 甲乙丙 / ①②③ labels). Worth a background Haiku confirm.
    Suspected,
    /// Not a choice — do nothing.
    NoChoice,
}

/// Classify an outbound message for the capture pipeline (RFC-24 §P3.1).
///
/// `Confident` takes the zero-cost path. `Suspected` is reserved for the rare
/// shapes the deterministic scanner can't parse but that still carry a choice
/// keyword and ≥2 item-like lines — only those pay for an LLM second pass.
pub fn classify_outbound(text: &str) -> DetectionResult {
    if let Some(draft) = detect_enumerated_options(text) {
        return DetectionResult::Confident(draft);
    }
    if is_suspected_choice(text) {
        return DetectionResult::Suspected;
    }
    DetectionResult::NoChoice
}

/// Heuristic for the "suspected but unconfirmed" middle ground: a choice keyword
/// plus ≥2 item-like lines that the strict parser didn't turn into options.
fn is_suspected_choice(text: &str) -> bool {
    if !has_choice_keyword(text) {
        return false;
    }
    let itemish = text.lines().filter(|l| line_is_itemish(l.trim())).count();
    itemish >= 2
}

fn line_is_itemish(l: &str) -> bool {
    let Some(c) = l.chars().next() else {
        return false;
    };
    matches!(c, '-' | '*' | '•' | '·' | '・')
        || c.is_ascii_alphanumeric()
        || ('\u{2460}'..='\u{2473}').contains(&c) // ①..⑳ circled numbers
        || matches!(c, '甲' | '乙' | '丙' | '丁' | '戊')
        || l.starts_with("方案")
        || l.starts_with("選項")
}

/// Build the Haiku second-pass extraction prompt for a suspected choice.
pub fn build_extraction_prompt(text: &str) -> String {
    format!(
        "你是一個嚴格的抽取器。判斷以下助理訊息是否在向使用者提出「需要選擇的多個方案/選項」。\n\
         只輸出 JSON,不要任何其他文字、不要 markdown code fence。格式:\n\
         {{\"is_decision\": true|false, \"question\": \"簡短題目\", \"options\": [{{\"key\": \"A\", \"content\": \"選項內容\"}}]}}\n\
         規則:必須是「請使用者擇一」的選項清單才算 is_decision=true;一般步驟、說明、單一建議都不是。\n\
         至少要有 2 個選項才算。key 用 A/B/C 或 1/2/3。content 用原文精簡。\n\n\
         === 訊息 ===\n{text}\n=== 結束 ==="
    )
}

/// Parse Haiku's JSON reply into a [`DecisionDraft`] (RFC-24 §P3.1).
///
/// Tolerates a leading/trailing code fence. Returns `None` unless the model said
/// `is_decision = true` with ≥2 non-empty options (fail-closed). Contents are
/// UTF-8-safe-truncated, mirroring the deterministic path.
pub fn parse_extracted_decision(reply: &str) -> Option<DecisionDraft> {
    let trimmed = reply.trim();
    // Strip an optional ```json … ``` fence.
    let body = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .map(|s| s.trim_end_matches("```").trim())
        .unwrap_or(trimmed);
    // Be lenient: grab the outermost {...} if there's surrounding chatter.
    let json_slice = match (body.find('{'), body.rfind('}')) {
        (Some(a), Some(b)) if b > a => &body[a..=b],
        _ => return None,
    };
    let v: serde_json::Value = serde_json::from_str(json_slice).ok()?;
    if v.get("is_decision").and_then(|b| b.as_bool()) != Some(true) {
        return None;
    }
    let question = v
        .get("question")
        .and_then(|q| q.as_str())
        .map(|q| truncate_bytes(q.trim(), MAX_QUESTION_BYTES).to_string())
        .filter(|q| !q.is_empty())
        .unwrap_or_else(|| "(未命名決策)".to_string());
    let mut options = Vec::new();
    let mut seen = Vec::new();
    for opt in v.get("options").and_then(|o| o.as_array())?.iter() {
        let key = opt.get("key").and_then(|k| k.as_str()).unwrap_or("").trim();
        let content = opt
            .get("content")
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .trim();
        if key.is_empty() || content.is_empty() || seen.iter().any(|k| k == key) {
            continue;
        }
        seen.push(key.to_string());
        options.push((
            key.to_uppercase(),
            truncate_bytes(content, MAX_OPTION_BYTES).to_string(),
        ));
    }
    if options.len() < 2 {
        return None;
    }
    Some(DecisionDraft { question, options })
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_core::traits::MemoryEngine;
    use tempfile::TempDir;

    // ── decision_id ──

    #[test]
    fn decision_id_is_deterministic_and_scoped() {
        let a = decision_id("agent-x", "msg-1");
        assert_eq!(a, decision_id("agent-x", "msg-1"), "same input → same id");
        assert_eq!(a.len(), 12);
        assert_ne!(a, decision_id("agent-y", "msg-1"), "agent changes id");
        assert_ne!(a, decision_id("agent-x", "msg-2"), "message changes id");
    }

    // ── detector positives ──

    #[test]
    fn detects_chinese_fang_markers() {
        let text = "我建議三個方案：\n方案 A：公有鏈 + L2\n方案 B：聯盟鏈 Hyperledger\n方案 C：私有 Ethereum PoA";
        let d = detect_enumerated_options(text).expect("should detect");
        assert_eq!(d.options.len(), 3);
        assert_eq!(d.options[0], ("A".into(), "公有鏈 + L2".into()));
        assert_eq!(d.options[2], ("C".into(), "私有 Ethereum PoA".into()));
    }

    #[test]
    fn detects_english_option_markers() {
        let text = "Which one do you want?\nOption A: public chain\nOption B: consortium chain";
        let d = detect_enumerated_options(text).expect("should detect");
        assert_eq!(d.options.len(), 2);
        assert_eq!(d.options[0].0, "A");
    }

    #[test]
    fn detects_bare_letter_list_with_choice_keyword() {
        let text = "你想要哪個方案？\nA. 公有鏈\nB. 聯盟鏈\nC. 私有鏈";
        let d = detect_enumerated_options(text).expect("should detect");
        assert_eq!(d.options.len(), 3);
    }

    #[test]
    fn detects_numbered_list_with_choice_keyword() {
        let text = "請選擇哪一個選項：\n1. 立即部署\n2. 先測試再部署";
        let d = detect_enumerated_options(text).expect("should detect");
        assert_eq!(d.options.len(), 2);
        assert_eq!(d.options[0].0, "1");
    }

    #[test]
    fn detects_emoji_keycap_markers() {
        let text = "你想選哪個？\n1️⃣ 方案一內容\n2️⃣ 方案二內容";
        let d = detect_enumerated_options(text).expect("should detect");
        assert_eq!(d.options.len(), 2);
        assert_eq!(d.options[0].0, "1");
    }

    #[test]
    fn multiline_option_content_is_joined() {
        let text = "選擇哪一個方案：\n方案 A：第一行\n第二行補充\n方案 B：另一個";
        let d = detect_enumerated_options(text).expect("should detect");
        assert_eq!(d.options[0].1, "第一行 第二行補充");
    }

    // ── detector negatives (fail-safe) ──

    #[test]
    fn rejects_numbered_steps_without_choice_keyword() {
        // A step list, not a decision — no choice keyword present.
        let text = "安裝步驟如下：\n1. 下載安裝包\n2. 解壓縮\n3. 執行安裝";
        assert!(detect_enumerated_options(text).is_none());
    }

    #[test]
    fn rejects_single_option() {
        let text = "你想要哪個方案？\n方案 A：唯一選擇";
        assert!(detect_enumerated_options(text).is_none());
    }

    #[test]
    fn rejects_plain_prose() {
        let text = "我覺得這個方案不錯，我們可以直接開始實作。";
        assert!(detect_enumerated_options(text).is_none());
    }

    #[test]
    fn rejects_mixed_marker_kinds() {
        let text = "選擇哪一個：\n方案 A：一\n2. 二";
        assert!(detect_enumerated_options(text).is_none());
    }

    #[test]
    fn ignores_markers_inside_code_fence() {
        let text = "選擇哪一個方案：\n```\n方案 A：程式碼內\n方案 B：也在碼內\n```\n方案 C：真選項一\n方案 D：真選項二";
        let d = detect_enumerated_options(text).expect("should detect outside fence");
        assert_eq!(d.options.len(), 2);
        assert_eq!(d.options[0].0, "C");
    }

    #[test]
    fn rejects_time_colon_lines() {
        // "9:30" must not parse as a digit marker (colon excluded for bare markers).
        let text = "會議安排如下：\n9:30 開場\n10:30 討論";
        assert!(detect_enumerated_options(text).is_none());
    }

    #[test]
    fn cjk_content_does_not_panic_on_truncation() {
        let long = "學".repeat(2000); // 6000 bytes > MAX_OPTION_BYTES
        let text = format!("選擇哪一個方案：\n方案 A：{long}\n方案 B：短");
        let d = detect_enumerated_options(&text).expect("should detect");
        assert!(d.options[0].1.len() <= MAX_OPTION_BYTES);
        // Still valid UTF-8 (guaranteed by &str), multiple of 3 bytes for 學.
        assert_eq!(d.options[0].1.chars().count() * 3, d.options[0].1.len());
    }

    // ── persist + query round-trip ──

    #[tokio::test]
    async fn persist_then_list_open_decisions() {
        let dir = TempDir::new().unwrap();
        let engine = SqliteMemoryEngine::new(&dir.path().join("memory.db")).unwrap();
        let draft = DecisionDraft {
            question: "區塊鏈整合方案".into(),
            options: vec![
                ("A".into(), "公有鏈".into()),
                ("B".into(), "聯盟鏈".into()),
                ("C".into(), "私有 Ethereum PoA".into()),
            ],
        };
        let id = decision_id("agent-a", "msg-1");
        persist_decision(
            &engine,
            "agent-a",
            &id,
            &draft,
            serde_json::json!({"channel":"discord"}),
        )
        .await
        .unwrap();

        let open = engine.list_open_decisions("agent-a", 5).await.unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].id, id);
        assert_eq!(open[0].question, "區塊鏈整合方案");
        assert_eq!(open[0].options.len(), 3);
        assert_eq!(open[0].options[2], ("C".into(), "私有 Ethereum PoA".into()));
    }

    #[tokio::test]
    async fn injection_section_contains_option_content() {
        let dir = TempDir::new().unwrap();
        let engine = SqliteMemoryEngine::new(&dir.path().join("memory.db")).unwrap();
        let draft = DecisionDraft {
            question: "整合方案".into(),
            options: vec![
                ("A".into(), "公有鏈".into()),
                ("C".into(), "私有 Ethereum PoA".into()),
            ],
        };
        let id = decision_id("agent-b", "m1");
        persist_decision(&engine, "agent-b", &id, &draft, serde_json::json!({}))
            .await
            .unwrap();

        let section = build_open_decisions_section(&engine, "agent-b").await;
        assert!(section.contains("待決事項"));
        assert!(
            section.contains("私有 Ethereum PoA"),
            "C content must be present"
        );
        assert!(section.contains(&format!("[decision:{id}]")));

        // No open decisions for an unrelated agent → empty.
        let empty = build_open_decisions_section(&engine, "agent-z").await;
        assert!(empty.is_empty());
    }

    #[tokio::test]
    async fn recapture_supersedes_not_duplicates() {
        let dir = TempDir::new().unwrap();
        let engine = SqliteMemoryEngine::new(&dir.path().join("memory.db")).unwrap();
        let draft = DecisionDraft {
            question: "Q".into(),
            options: vec![("A".into(), "one".into()), ("B".into(), "two".into())],
        };
        let id = decision_id("agent-c", "m1");
        persist_decision(&engine, "agent-c", &id, &draft, serde_json::json!({}))
            .await
            .unwrap();
        persist_decision(&engine, "agent-c", &id, &draft, serde_json::json!({}))
            .await
            .unwrap();

        let open = engine.list_open_decisions("agent-c", 5).await.unwrap();
        assert_eq!(open.len(), 1, "same decision id must not duplicate as open");
        assert_eq!(
            open[0].options.len(),
            2,
            "options superseded, not duplicated"
        );
    }

    // Ensures the semantic rows are actually full-text searchable (content == object).
    #[tokio::test]
    async fn decision_content_is_searchable() {
        let dir = TempDir::new().unwrap();
        let engine = SqliteMemoryEngine::new(&dir.path().join("memory.db")).unwrap();
        let draft = DecisionDraft {
            question: "Q".into(),
            options: vec![
                ("A".into(), "EthereumPoA".into()),
                ("B".into(), "Hyperledger".into()),
            ],
        };
        let id = decision_id("agent-d", "m1");
        persist_decision(&engine, "agent-d", &id, &draft, serde_json::json!({}))
            .await
            .unwrap();
        let hits = engine.search("agent-d", "Hyperledger", 10).await.unwrap();
        assert!(!hits.is_empty(), "option content should be FTS-searchable");
    }

    // ── P1.5 End-to-end regression: the Agnes incident ──
    //
    // The core RFC-24 claim: a decision survives `compress()`. Session turns
    // live in the SessionManager store, which `compress()` destroys (DELETE all
    // turns → one Haiku bullet). Decisions live in the independent memory store,
    // which `compress()` never touches. This test reproduces the full failure
    // path: capture → destroy the session → confirm the option content is still
    // recoverable and would be injected into the next turn's prompt.
    #[tokio::test]
    async fn decision_survives_session_compression() {
        use crate::session::SessionManager;

        let dir = TempDir::new().unwrap();
        let session_mgr = SessionManager::new(&dir.path().join("sessions.db")).unwrap();
        let engine = SqliteMemoryEngine::new(&dir.path().join("memory.db")).unwrap();
        let sid = "discord:thread-1";
        let agent = "agnes";

        // 1. Agnes offers A/B/C; the reply is saved as a session turn AND captured.
        let reply = "我建議三個方案：\n方案 A：公有鏈\n方案 B：聯盟鏈\n方案 C：私有 Ethereum PoA";
        session_mgr.get_or_create(sid, agent).await.unwrap();
        session_mgr
            .append_message(sid, "assistant", reply, 50)
            .await
            .unwrap();
        let draft = detect_enumerated_options(reply).expect("reply offers a choice");
        let id = decision_id(agent, &format!("{sid}|{reply}"));
        persist_decision(
            &engine,
            agent,
            &id,
            &draft,
            serde_json::json!({"channel":"discord"}),
        )
        .await
        .unwrap();

        // 2. The session is compressed (50k threshold simulated) → turns destroyed.
        session_mgr
            .compress(sid, "[summary] agnes 提了三個方案")
            .await
            .unwrap();
        let surviving_turns = session_mgr.get_messages(sid).await.unwrap();
        // Only the Haiku bullet remains; the literal "私有 Ethereum PoA" is gone
        // from the session store.
        assert!(
            surviving_turns
                .iter()
                .all(|m| !m.content.contains("私有 Ethereum PoA")),
            "compress() must strip the concrete option content from session turns"
        );

        // 3. The decision is still fully recoverable from the memory store …
        let open = engine.list_open_decisions(agent, 5).await.unwrap();
        assert_eq!(open.len(), 1, "decision survives compression");
        assert_eq!(open[0].options[2], ("C".into(), "私有 Ethereum PoA".into()));

        // 4. … and would be injected into the next (post-compression) turn.
        let section = build_open_decisions_section(&engine, agent).await;
        assert!(
            section.contains("私有 Ethereum PoA"),
            "next-turn prompt re-surfaces the option Agnes had lost"
        );
    }

    // Cross-process durability: a decision persisted by one engine instance is
    // recoverable after the engine is dropped and the SQLite file reopened
    // (simulates a gateway restart between capture and the user's reply).
    #[tokio::test]
    async fn decision_survives_engine_reopen() {
        let dir = TempDir::new().unwrap();
        let db = dir.path().join("memory.db");
        let draft = DecisionDraft {
            question: "Q".into(),
            options: vec![("A".into(), "alpha".into()), ("B".into(), "beta".into())],
        };
        let id = decision_id("agent-r", "m1");
        {
            let engine = SqliteMemoryEngine::new(&db).unwrap();
            persist_decision(&engine, "agent-r", &id, &draft, serde_json::json!({}))
                .await
                .unwrap();
        } // engine dropped → connection closed

        let reopened = SqliteMemoryEngine::new(&db).unwrap();
        let open = reopened.list_open_decisions("agent-r", 5).await.unwrap();
        assert_eq!(open.len(), 1, "decision survives a process restart");
        assert_eq!(open[0].options.len(), 2);
    }

    // ── P2.1 resolve ──

    #[tokio::test]
    async fn resolve_decision_full_flow() {
        use duduclaw_memory::DecisionResolveOutcome;
        let dir = TempDir::new().unwrap();
        let engine = SqliteMemoryEngine::new(&dir.path().join("memory.db")).unwrap();
        let draft = DecisionDraft {
            question: "整合方案".into(),
            options: vec![
                ("A".into(), "公有鏈".into()),
                ("C".into(), "私有 Ethereum PoA".into()),
            ],
        };
        let id = decision_id("agnes", "m1");
        persist_decision(&engine, "agnes", &id, &draft, serde_json::json!({}))
            .await
            .unwrap();

        // Resolve to C.
        let outcome = engine.resolve_decision("agnes", &id, "C").await.unwrap();
        match outcome {
            DecisionResolveOutcome::Resolved {
                chosen_key,
                chosen_content,
                ..
            } => {
                assert_eq!(chosen_key, "C");
                assert_eq!(chosen_content, "私有 Ethereum PoA");
            }
            other => panic!("expected Resolved, got {other:?}"),
        }

        // No longer open; status reflects the choice.
        assert!(engine.list_open_decisions("agnes", 5).await.unwrap().is_empty());
        assert_eq!(
            engine.decision_status("agnes", &id).await.unwrap().as_deref(),
            Some("resolved:C")
        );
        // The choice is recorded as a searchable long-lived fact.
        let hits = engine.search("agnes", "已解決的決策", 5).await.unwrap();
        assert!(!hits.is_empty(), "resolution recorded as a semantic fact");
    }

    #[tokio::test]
    async fn resolve_decision_fail_closed() {
        use duduclaw_memory::DecisionResolveOutcome;
        let dir = TempDir::new().unwrap();
        let engine = SqliteMemoryEngine::new(&dir.path().join("memory.db")).unwrap();
        let draft = DecisionDraft {
            question: "Q".into(),
            options: vec![("A".into(), "one".into()), ("B".into(), "two".into())],
        };
        let id = decision_id("agnes", "m1");
        persist_decision(&engine, "agnes", &id, &draft, serde_json::json!({}))
            .await
            .unwrap();

        // Unknown decision id → NotFound, nothing written.
        assert!(matches!(
            engine.resolve_decision("agnes", "deadbeef", "A").await.unwrap(),
            DecisionResolveOutcome::NotFound
        ));
        // Unknown option key → UnknownKey, nothing written.
        assert!(matches!(
            engine.resolve_decision("agnes", &id, "Z").await.unwrap(),
            DecisionResolveOutcome::UnknownKey { .. }
        ));
        // Other agent cannot resolve this decision (namespace isolation).
        assert!(matches!(
            engine.resolve_decision("intruder", &id, "A").await.unwrap(),
            DecisionResolveOutcome::NotFound
        ));
        // Still open after the failed attempts.
        assert_eq!(engine.list_open_decisions("agnes", 5).await.unwrap().len(), 1);

        // Resolve, then a second resolve is rejected as AlreadyResolved.
        engine.resolve_decision("agnes", &id, "A").await.unwrap();
        assert!(matches!(
            engine.resolve_decision("agnes", &id, "B").await.unwrap(),
            DecisionResolveOutcome::AlreadyResolved(_)
        ));
    }

    // ── P2.2 reference detection ──

    fn view(id: &str, keys: &[&str]) -> duduclaw_memory::DecisionView {
        duduclaw_memory::DecisionView {
            id: id.to_string(),
            question: "Q".into(),
            options: keys.iter().map(|k| (k.to_string(), format!("c{k}"))).collect(),
            created_at: None,
        }
    }

    #[test]
    fn reference_detects_chinese_fang() {
        let open = vec![view("d1", &["A", "B", "C"])];
        assert_eq!(
            detect_decision_reference("就用方案 C 吧", &open),
            Some(("d1".into(), "C".into()))
        );
        assert_eq!(
            detect_decision_reference("選A", &open),
            Some(("d1".into(), "A".into()))
        );
    }

    #[test]
    fn reference_detects_english_option() {
        let open = vec![view("d1", &["A", "B"])];
        assert_eq!(
            detect_decision_reference("let's go with option B", &open),
            Some(("d1".into(), "B".into()))
        );
    }

    #[test]
    fn reference_detects_numbered_choice() {
        let open = vec![view("d1", &["1", "2"])];
        assert_eq!(
            detect_decision_reference("我要第2個", &open),
            Some(("d1".into(), "2".into()))
        );
    }

    #[test]
    fn reference_none_when_no_choice_context() {
        let open = vec![view("d1", &["A", "B", "C"])];
        // Bare letters in prose must NOT match (no choice anchor).
        assert_eq!(detect_decision_reference("A car drove by, basically", &open), None);
        assert_eq!(detect_decision_reference("沒什麼想法", &open), None);
    }

    #[test]
    fn reference_none_when_ambiguous() {
        // Two open decisions both have key C → ambiguous → None.
        let open = vec![view("d1", &["A", "C"]), view("d2", &["C", "D"])];
        assert_eq!(detect_decision_reference("用方案 C", &open), None);
        // Two distinct keys referenced → ambiguous → None.
        let open2 = vec![view("d1", &["A", "B"])];
        assert_eq!(detect_decision_reference("方案 A 還是 方案 B?", &open2), None);
    }

    #[tokio::test]
    async fn ttl_expires_stale_open_decisions_only() {
        let dir = TempDir::new().unwrap();
        let engine = SqliteMemoryEngine::new(&dir.path().join("memory.db")).unwrap();

        // Build a 10-day-old open decision directly (backdated valid_from).
        let old = chrono::Utc::now() - chrono::Duration::days(10);
        let subject = decision_subject("old1");
        for (pred, obj) in [("question", "舊決策"), ("option:A", "一"), ("status", "open")] {
            let mut entry = MemoryEntry {
                id: uuid::Uuid::new_v4().to_string(),
                agent_id: "agnes".into(),
                content: obj.into(),
                timestamp: old,
                tags: vec!["decision".into()],
                embedding: None,
                layer: MemoryLayer::Semantic,
                importance: 7.0,
                access_count: 0,
                last_accessed: None,
                source_event: "test".into(),
            };
            entry.tags.push(subject.clone());
            engine
                .store_temporal(
                    "agnes",
                    entry,
                    TemporalMeta {
                        subject: Some(subject.clone()),
                        predicate: Some(pred.into()),
                        object: Some(obj.into()),
                        valid_from: Some(old),
                        valid_until: None,
                        confidence: Some(1.0),
                        metadata: None,
                    },
                )
                .await
                .unwrap();
        }
        // A fresh decision that must survive TTL.
        let fresh = DecisionDraft {
            question: "新決策".into(),
            options: vec![("A".into(), "x".into()), ("B".into(), "y".into())],
        };
        let fresh_id = decision_id("agnes", "fresh");
        persist_decision(&engine, "agnes", &fresh_id, &fresh, serde_json::json!({}))
            .await
            .unwrap();

        assert_eq!(engine.list_open_decisions("agnes", 10).await.unwrap().len(), 2);

        // TTL 7 days: the 10-day-old decision expires, the fresh one stays.
        let n = engine.expire_stale_decisions("agnes", 7).await.unwrap();
        assert!(n >= 1, "stale decision rows expired");
        let open = engine.list_open_decisions("agnes", 10).await.unwrap();
        assert_eq!(open.len(), 1, "only the fresh decision remains open");
        assert_eq!(open[0].id, fresh_id);

        // ttl_days <= 0 is a no-op.
        assert_eq!(engine.expire_stale_decisions("agnes", 0).await.unwrap(), 0);
    }

    // ── P3.1 confidence classification + Haiku parse ──

    #[test]
    fn classify_confident_for_clean_fang_list() {
        let text = "選擇哪一個方案：\n方案 A：一\n方案 B：二";
        assert!(matches!(
            classify_outbound(text),
            DetectionResult::Confident(_)
        ));
    }

    #[test]
    fn classify_suspected_for_unparseable_labels() {
        // 甲/乙 labels + choice keyword → strict fails, suspected fires.
        let text = "請問你想選哪個方案？\n甲、走公有鏈\n乙、走聯盟鏈";
        assert_eq!(classify_outbound(text), DetectionResult::Suspected);
        // Circled numbers similarly.
        let text2 = "你想選哪個方案？\n① 第一案\n② 第二案";
        assert_eq!(classify_outbound(text2), DetectionResult::Suspected);
    }

    #[test]
    fn classify_nochoice_for_prose() {
        assert_eq!(
            classify_outbound("這個方案不錯，我直接開始做。"),
            DetectionResult::NoChoice
        );
    }

    #[test]
    fn parse_extracted_decision_valid_json() {
        let reply = r#"{"is_decision": true, "question": "整合方案", "options": [{"key":"A","content":"公有鏈"},{"key":"B","content":"聯盟鏈"}]}"#;
        let d = parse_extracted_decision(reply).expect("valid");
        assert_eq!(d.options.len(), 2);
        assert_eq!(d.options[0], ("A".into(), "公有鏈".into()));
    }

    #[test]
    fn parse_extracted_decision_tolerates_fence_and_chatter() {
        let reply = "```json\n{\"is_decision\":true,\"question\":\"Q\",\"options\":[{\"key\":\"a\",\"content\":\"x\"},{\"key\":\"b\",\"content\":\"y\"}]}\n```";
        let d = parse_extracted_decision(reply).expect("valid");
        assert_eq!(d.options[0].0, "A", "key upcased");
    }

    #[test]
    fn parse_extracted_decision_rejects_non_decision_or_thin() {
        assert!(parse_extracted_decision(r#"{"is_decision": false}"#).is_none());
        assert!(
            parse_extracted_decision(
                r#"{"is_decision": true, "options": [{"key":"A","content":"only one"}]}"#
            )
            .is_none(),
            "needs >= 2 options"
        );
        assert!(parse_extracted_decision("not json at all").is_none());
    }

    #[tokio::test]
    async fn dismiss_removes_decision_and_reports_existence() {
        let dir = TempDir::new().unwrap();
        let engine = SqliteMemoryEngine::new(&dir.path().join("memory.db")).unwrap();
        let draft = DecisionDraft {
            question: "Q".into(),
            options: vec![("A".into(), "one".into()), ("B".into(), "two".into())],
        };
        let id = decision_id("agnes", "m1");
        persist_decision(&engine, "agnes", &id, &draft, serde_json::json!({}))
            .await
            .unwrap();

        assert!(engine.dismiss_decision("agnes", &id).await.unwrap(), "existed");
        assert!(engine.list_open_decisions("agnes", 5).await.unwrap().is_empty());
        // Dismissing again / unknown id → false (nothing closed).
        assert!(!engine.dismiss_decision("agnes", &id).await.unwrap());
        assert!(!engine.dismiss_decision("agnes", "nope").await.unwrap());
    }

    #[test]
    fn mentions_reference_detects_gap_shapes() {
        assert!(mentions_decision_reference("就用方案 C 吧"));
        assert!(mentions_decision_reference("用方案C"));
        assert!(mentions_decision_reference("go with option B"));
        assert!(mentions_decision_reference("我選第2個"));
        // Not a reference.
        assert!(!mentions_decision_reference("這個方案不錯，繼續做"));
        assert!(!mentions_decision_reference("今天天氣很好"));
    }
}
