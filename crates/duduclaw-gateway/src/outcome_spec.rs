//! WP2.4 — structured **outcome acceptance** for the goal loop.
//!
//! `/goal <描述> || <驗收標準> || outcome:<spec>` adds a third, machine-checkable
//! acceptance layer on top of the free-text acceptance criteria. The outcome
//! spec is validated **deterministically and at zero LLM cost** the moment an
//! agent reports a goal-mode task complete — *before* the MAV acceptance judge
//! ([`crate::dispatch_engine`]) is ever invoked. A deterministic failure sends
//! the task straight back to `revising` with a concrete defect list (which field
//! is missing, which file was not produced) and never burns a judge LLM call.
//! This is the guard against judge false-positives: a result that provably does
//! not meet the structural contract cannot be waved through by an over-eager
//! judge, and a result that passes the deterministic gate reaches the judge with
//! an explicit "deterministic 校驗已通過" note.
//!
//! Three spec shapes:
//! - `text` (default) — no structural contract; behaviour is byte-identical to
//!   pre-WP2.4 (nothing is persisted, nothing runs before the judge).
//! - `json:<schema>` — a **JSON Schema subset** (object / array / string /
//!   number / integer / boolean, with `properties` / `required` / `items`).
//!   The agent's final reply is scanned for a ```json fenced block (else the
//!   whole reply is parsed) and validated against the schema.
//! - `files:<glob,glob,…>` — assert that files matching each glob exist under
//!   the agent's working directory. Path traversal (`..`, absolute paths) is
//!   rejected fail-closed at parse time.
//!
//! ## Persistence (no schema change)
//!
//! The spec rides on the existing `TaskRow::tags` field (comma-separated), as a
//! single `outcome:<base64url>` tag — base64url (no padding) has no comma, so it
//! never collides with tag splitting. `text` specs persist nothing.

use std::path::{Component, Path, PathBuf};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde_json::Value;

/// Tag key under which the encoded spec is stored in `TaskRow::tags`.
const OUTCOME_TAG_PREFIX: &str = "outcome:";

/// A parsed, validated outcome acceptance spec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutcomeSpec {
    /// No structural contract (default). Nothing runs before the judge.
    Text,
    /// A JSON Schema subset the agent's final reply must satisfy.
    Json(Value),
    /// Globs (relative to the agent working dir) that must each match ≥1 file.
    Files(Vec<String>),
}

/// Result of a deterministic outcome check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutcomeCheck {
    /// True when the result satisfies the spec.
    pub passed: bool,
    /// Human-readable zh-TW defect lines (empty when `passed`). Fed verbatim
    /// into the retry feedback so the agent knows exactly what to fix.
    pub defects: Vec<String>,
}

impl OutcomeCheck {
    fn ok() -> Self {
        Self {
            passed: true,
            defects: Vec::new(),
        }
    }
    fn failed(defects: Vec<String>) -> Self {
        Self {
            passed: false,
            defects,
        }
    }
}

impl OutcomeSpec {
    /// Parse the raw `outcome:` payload (the text after `outcome:`) into a spec.
    ///
    /// Fail-closed: malformed JSON, an unknown type prefix, an empty file list,
    /// or a path-traversal glob is a hard `Err` (the `/goal` command is rejected
    /// with the message) — never a silent downgrade to `Text`. Never panics.
    pub fn parse(spec: &str) -> Result<OutcomeSpec, String> {
        let s = spec.trim();
        if s.is_empty() || s.eq_ignore_ascii_case("text") {
            return Ok(OutcomeSpec::Text);
        }
        if let Some(rest) = s.strip_prefix("json:") {
            let schema: Value = serde_json::from_str(rest.trim())
                .map_err(|e| format!("outcome json schema 解析失敗：{e}"))?;
            if !schema.is_object() {
                return Err("outcome json schema 必須是一個 JSON 物件（例如 \
                    `{\"type\":\"object\",\"required\":[\"total\"]}`）"
                    .to_string());
            }
            return Ok(OutcomeSpec::Json(schema));
        }
        if let Some(rest) = s.strip_prefix("files:") {
            let globs: Vec<String> = rest
                .split(',')
                .map(|g| g.trim().to_string())
                .filter(|g| !g.is_empty())
                .collect();
            if globs.is_empty() {
                return Err(
                    "outcome files 需要至少一個檔案樣式（例如 `files:report.docx`）".to_string(),
                );
            }
            for g in &globs {
                reject_traversal(g)?;
            }
            return Ok(OutcomeSpec::Files(globs));
        }
        Err(format!(
            "未知的 outcome 型別：`{s}`（支援 text / json:<schema> / files:<glob,…>）"
        ))
    }

    /// Whether this spec runs a deterministic check before the judge. `Text`
    /// never does (nothing to check), so it is not persisted and adds no cost.
    pub fn is_deterministic(&self) -> bool {
        !matches!(self, OutcomeSpec::Text)
    }

    /// Canonical spec string (round-trips through [`OutcomeSpec::parse`]).
    fn to_spec_string(&self) -> String {
        match self {
            OutcomeSpec::Text => "text".to_string(),
            OutcomeSpec::Json(v) => format!("json:{v}"),
            OutcomeSpec::Files(globs) => format!("files:{}", globs.join(",")),
        }
    }

    /// The single `outcome:<base64url>` tag to store on `TaskRow::tags`. Returns
    /// `None` for `Text` (nothing to persist — behaviour unchanged).
    pub fn to_tag(&self) -> Option<String> {
        if !self.is_deterministic() {
            return None;
        }
        let encoded = URL_SAFE_NO_PAD.encode(self.to_spec_string().as_bytes());
        Some(format!("{OUTCOME_TAG_PREFIX}{encoded}"))
    }

    /// Recover a spec from a `TaskRow::tags` string. Returns `None` when no
    /// deterministic outcome tag is present, or (fail-safe) when the tag is
    /// corrupt/unparseable — in which case the caller falls through to the judge
    /// exactly as if no spec were set (the judge remains a backstop gate).
    pub fn from_tags(tags: &str) -> Option<OutcomeSpec> {
        for tag in tags.split(',') {
            let tag = tag.trim();
            if let Some(b64) = tag.strip_prefix(OUTCOME_TAG_PREFIX) {
                let bytes = URL_SAFE_NO_PAD.decode(b64.trim()).ok()?;
                let spec_str = String::from_utf8(bytes).ok()?;
                return OutcomeSpec::parse(&spec_str).ok();
            }
        }
        None
    }

    /// Deterministically validate an agent's final `reply` (and, for `files`,
    /// the filesystem under `work_dir`) against this spec. Never panics; a
    /// missing/unreadable work dir simply yields file-not-found defects.
    pub fn validate(&self, reply: &str, work_dir: &Path) -> OutcomeCheck {
        match self {
            OutcomeSpec::Text => OutcomeCheck::ok(),
            OutcomeSpec::Json(schema) => validate_json(schema, reply),
            OutcomeSpec::Files(globs) => validate_files(globs, work_dir),
        }
    }
}

// ── files:<glob,…> ──────────────────────────────────────────

/// Reject a glob that could escape the agent working dir (fail-closed): absolute
/// paths, a home shortcut, or any `..` component.
fn reject_traversal(glob: &str) -> Result<(), String> {
    let g = glob.trim();
    if g.is_empty() {
        return Err("outcome files 樣式不可為空".to_string());
    }
    if g.starts_with('/') || g.starts_with('\\') || g.starts_with('~') {
        return Err(format!("outcome files 樣式不可為絕對路徑或家目錄：`{g}`"));
    }
    // Windows drive prefix (C:\ …).
    let bytes = g.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        return Err(format!("outcome files 樣式不可為絕對路徑：`{g}`"));
    }
    for comp in g.split(['/', '\\']) {
        if comp == ".." {
            return Err(format!("outcome files 樣式不可包含上層目錄（..）：`{g}`"));
        }
    }
    Ok(())
}

/// Validate that every glob matches ≥1 existing file under `work_dir`.
fn validate_files(globs: &[String], work_dir: &Path) -> OutcomeCheck {
    let mut defects = Vec::new();
    for g in globs {
        // Defence in depth: re-reject traversal at check time (a corrupt tag
        // could in principle reach here without the parse-time guard).
        if reject_traversal(g).is_err() {
            defects.push(format!("拒絕不安全的檔案樣式：`{g}`"));
            continue;
        }
        if !glob_matches_any(work_dir, g) {
            defects.push(format!("找不到符合 `{g}` 的產出檔案"));
        }
    }
    if defects.is_empty() {
        OutcomeCheck::ok()
    } else {
        OutcomeCheck::failed(defects)
    }
}

/// Whether `pattern` (relative, `/`-separated, may contain `*`/`?` per segment)
/// matches at least one existing path under `base`.
fn glob_matches_any(base: &Path, pattern: &str) -> bool {
    let comps: Vec<&str> = pattern
        .split(['/', '\\'])
        .filter(|c| !c.is_empty() && *c != ".")
        .collect();
    if comps.is_empty() {
        return false;
    }
    let base_canon = base.canonicalize().ok();
    match_components(base, base_canon.as_deref(), &comps)
}

/// Recursively match path components from `dir`. `base_canon` is the canonical
/// root used to keep literal (symlink) descents from escaping the work dir.
fn match_components(dir: &Path, base_canon: Option<&Path>, comps: &[&str]) -> bool {
    let Some((first, rest)) = comps.split_first() else {
        return dir.exists();
    };
    if !has_glob_meta(first) {
        let next = dir.join(first);
        if !next.exists() {
            return false;
        }
        // Symlink-escape guard: a literal component must stay within base.
        if let Some(base) = base_canon {
            match next.canonicalize() {
                Ok(c) if c.starts_with(base) => {}
                Ok(_) => return false,
                Err(_) => return false,
            }
        }
        if rest.is_empty() {
            return true;
        }
        return next.is_dir() && match_components(&next, base_canon, rest);
    }
    // Glob segment: scan the directory's entries.
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !segment_matches(first, &name) {
            continue;
        }
        let p = dir.join(&*name);
        if rest.is_empty() {
            return true;
        }
        if p.is_dir() && match_components(&p, base_canon, rest) {
            return true;
        }
    }
    false
}

/// Whether a single path segment contains glob metacharacters.
fn has_glob_meta(seg: &str) -> bool {
    seg.contains('*') || seg.contains('?')
}

/// Match a single `*`/`?` glob segment against a file name (case-sensitive).
/// `*` matches any run of characters (within the segment), `?` exactly one.
fn segment_matches(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let n: Vec<char> = name.chars().collect();
    glob_seg(&p, &n)
}

/// Backtracking `*`/`?` matcher over char slices.
fn glob_seg(p: &[char], n: &[char]) -> bool {
    // Iterative two-pointer with star backtracking (O(len) typical).
    let (mut pi, mut ni) = (0usize, 0usize);
    let (mut star, mut mark): (Option<usize>, usize) = (None, 0);
    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ni;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ni = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

// ── json:<schema> ───────────────────────────────────────────

/// Validate the agent reply against a JSON Schema subset.
fn validate_json(schema: &Value, reply: &str) -> OutcomeCheck {
    let Some(instance) = extract_json(reply) else {
        return OutcomeCheck::failed(vec![
            "最終回覆中找不到可解析的 JSON（請以 ```json 區塊或純 JSON 回覆）".to_string(),
        ]);
    };
    let mut defects = Vec::new();
    validate_schema_node(schema, &instance, "", &mut defects);
    if defects.is_empty() {
        OutcomeCheck::ok()
    } else {
        OutcomeCheck::failed(defects)
    }
}

/// Extract a JSON value from an agent reply: a ```json fenced block first, then
/// any ``` fenced block, then the whole trimmed reply, then a first-brace /
/// first-bracket to last-brace / last-bracket slice.
fn extract_json(reply: &str) -> Option<Value> {
    // 1) ```json … ``` fenced block.
    if let Some(body) = fenced_block(reply, "json") {
        if let Ok(v) = serde_json::from_str::<Value>(body.trim()) {
            return Some(v);
        }
    }
    // 2) any ``` … ``` block.
    if let Some(body) = fenced_block(reply, "") {
        if let Ok(v) = serde_json::from_str::<Value>(body.trim()) {
            return Some(v);
        }
    }
    // 3) whole reply.
    if let Ok(v) = serde_json::from_str::<Value>(reply.trim()) {
        return Some(v);
    }
    // 4) widest brace/bracket slice.
    if let Some(v) = widest_slice(reply, '{', '}') {
        return Some(v);
    }
    widest_slice(reply, '[', ']')
}

/// Return the body of the first ```<lang> fenced code block. `lang` empty ⇒ the
/// first fence of any language.
fn fenced_block(text: &str, lang: &str) -> Option<String> {
    let needle = format!("```{lang}");
    let start = text.find(&needle)?;
    // Body begins after the fence line's newline.
    let after = &text[start + needle.len()..];
    let body_start = after.find('\n')? + 1;
    let body = &after[body_start..];
    let end = body.find("```")?;
    Some(body[..end].to_string())
}

/// Parse the widest `open..=close` slice as JSON (ASCII delimiters ⇒ always on a
/// char boundary).
fn widest_slice(text: &str, open: char, close: char) -> Option<Value> {
    let start = text.find(open)?;
    let end = text.rfind(close)?;
    if end < start {
        return None;
    }
    serde_json::from_str::<Value>(&text[start..=end]).ok()
}

/// Recursively validate one schema node against an instance node, collecting
/// zh-TW defect lines. Fail-closed on unsupported constructs.
fn validate_schema_node(schema: &Value, instance: &Value, path: &str, defects: &mut Vec<String>) {
    let label = if path.is_empty() { "根" } else { path };
    // A node with no explicit `type` but with object keywords is an object.
    let ty = schema.get("type").and_then(|t| t.as_str());
    let effective_ty = ty.or_else(|| {
        (schema.get("properties").is_some() || schema.get("required").is_some()).then_some("object")
    });
    match effective_ty {
        Some("object") => {
            let Some(obj) = instance.as_object() else {
                defects.push(format!("`{label}` 型別不符（應為 object）"));
                return;
            };
            if let Some(req) = schema.get("required").and_then(|r| r.as_array()) {
                for r in req {
                    if let Some(k) = r.as_str() {
                        if !obj.contains_key(k) {
                            defects.push(format!("缺少必要欄位 `{}`", child_path(path, k)));
                        }
                    }
                }
            }
            if let Some(props) = schema.get("properties").and_then(|p| p.as_object()) {
                for (k, subschema) in props {
                    if let Some(v) = obj.get(k) {
                        validate_schema_node(subschema, v, &child_path(path, k), defects);
                    }
                }
            }
        }
        Some("array") => {
            let Some(arr) = instance.as_array() else {
                defects.push(format!("`{label}` 型別不符（應為 array）"));
                return;
            };
            if let Some(items) = schema.get("items") {
                for (i, el) in arr.iter().enumerate() {
                    validate_schema_node(items, el, &format!("{label}[{i}]"), defects);
                }
            }
        }
        Some("string") => {
            if !instance.is_string() {
                defects.push(format!("欄位 `{label}` 型別不符（應為 string）"));
            }
        }
        Some("number") => {
            if !instance.is_number() {
                defects.push(format!("欄位 `{label}` 型別不符（應為 number）"));
            }
        }
        Some("integer") => {
            if !(instance.is_i64() || instance.is_u64()) {
                defects.push(format!("欄位 `{label}` 型別不符（應為 integer）"));
            }
        }
        Some("boolean") => {
            if !instance.is_boolean() {
                defects.push(format!("欄位 `{label}` 型別不符（應為 boolean）"));
            }
        }
        Some(other) => {
            defects.push(format!("`{label}` 使用不支援的 schema type：`{other}`"));
        }
        // No type and no object keywords ⇒ no constraint on this node.
        None => {}
    }
}

/// Compose a dotted field path for defect messages.
fn child_path(parent: &str, key: &str) -> String {
    if parent.is_empty() {
        key.to_string()
    } else {
        format!("{parent}.{key}")
    }
}

/// Resolve the agent working directory used for `files:` assertions:
/// `<home>/agents/<agent_id>`. Returned even if absent (the file checks then
/// simply report not-found), and normalized so callers can log it.
pub fn agent_work_dir(home_dir: &Path, agent_id: &str) -> PathBuf {
    // Guard against an `agent_id` that tries path tricks (defence in depth —
    // ids are UUID-ish, but never trust the boundary).
    let safe = agent_id
        .split(['/', '\\'])
        .filter(|c| !c.is_empty() && *c != "." && *c != "..")
        .collect::<Vec<_>>()
        .join("_");
    let mut p = home_dir.join("agents").join(safe);
    // Collapse any residual `.` components without resolving symlinks.
    if p.components().any(|c| matches!(c, Component::ParentDir)) {
        p = home_dir.join("agents");
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── spec parsing (three shapes + malformed, fail-closed, no panic) ──

    #[test]
    fn parse_text_default_and_explicit() {
        assert_eq!(OutcomeSpec::parse("").unwrap(), OutcomeSpec::Text);
        assert_eq!(OutcomeSpec::parse("  ").unwrap(), OutcomeSpec::Text);
        assert_eq!(OutcomeSpec::parse("text").unwrap(), OutcomeSpec::Text);
        assert_eq!(OutcomeSpec::parse("TEXT").unwrap(), OutcomeSpec::Text);
        assert!(!OutcomeSpec::Text.is_deterministic());
    }

    #[test]
    fn parse_json_ok_and_malformed() {
        let spec = OutcomeSpec::parse(r#"json:{"type":"object","required":["total"]}"#).unwrap();
        assert!(matches!(spec, OutcomeSpec::Json(_)));
        assert!(spec.is_deterministic());
        // Malformed JSON ⇒ Err (fail-closed, not a downgrade to Text).
        assert!(OutcomeSpec::parse("json:{not valid").is_err());
        // Valid JSON but not an object ⇒ Err.
        assert!(OutcomeSpec::parse("json:[1,2,3]").is_err());
        assert!(OutcomeSpec::parse("json:\"hi\"").is_err());
    }

    #[test]
    fn parse_files_ok_and_traversal_rejected() {
        let spec = OutcomeSpec::parse("files:report.docx, out/*.pdf").unwrap();
        assert_eq!(
            spec,
            OutcomeSpec::Files(vec!["report.docx".into(), "out/*.pdf".into()])
        );
        // Empty list ⇒ Err.
        assert!(OutcomeSpec::parse("files:   ").is_err());
        assert!(OutcomeSpec::parse("files:,,").is_err());
        // Path traversal / absolute ⇒ Err (fail-closed).
        assert!(OutcomeSpec::parse("files:../../etc/passwd").is_err());
        assert!(OutcomeSpec::parse("files:/etc/passwd").is_err());
        assert!(OutcomeSpec::parse("files:~/secrets").is_err());
        assert!(OutcomeSpec::parse("files:a/../../b").is_err());
        assert!(OutcomeSpec::parse(r"files:C:\Windows\x").is_err());
        // A good glob alongside a bad one still fails the whole spec.
        assert!(OutcomeSpec::parse("files:ok.txt,../bad").is_err());
    }

    #[test]
    fn parse_unknown_type_rejected() {
        assert!(OutcomeSpec::parse("yaml:foo").is_err());
        assert!(OutcomeSpec::parse("random garbage").is_err());
    }

    // ── tag round-trip ──

    #[test]
    fn tag_round_trips_and_text_is_not_persisted() {
        // Text ⇒ no tag.
        assert_eq!(OutcomeSpec::Text.to_tag(), None);

        let specs = vec![
            OutcomeSpec::parse(
                r#"json:{"type":"object","required":["a"],"properties":{"a":{"type":"number"}}}"#,
            )
            .unwrap(),
            OutcomeSpec::parse("files:report.docx,out/*.pdf").unwrap(),
        ];
        for spec in specs {
            let tag = spec.to_tag().unwrap();
            assert!(tag.starts_with("outcome:"));
            assert!(!tag.contains(','), "base64url tag must not contain a comma");
            // Recover from a realistic tags string.
            let tags = format!("goal:telegram,{tag},grant:send_email");
            assert_eq!(OutcomeSpec::from_tags(&tags), Some(spec));
        }
    }

    #[test]
    fn from_tags_absent_or_corrupt_is_none() {
        assert_eq!(OutcomeSpec::from_tags("goal:telegram,grant:x"), None);
        assert_eq!(OutcomeSpec::from_tags(""), None);
        // Corrupt base64 ⇒ None (fail-safe: fall through to the judge).
        assert_eq!(OutcomeSpec::from_tags("outcome:@@@not-base64@@@"), None);
    }

    // ── json validation (pass / missing / type mismatch) ──

    fn schema() -> Value {
        json!({
            "type": "object",
            "required": ["revenue", "month"],
            "properties": {
                "revenue": {"type": "number"},
                "month": {"type": "string"},
                "items": {"type": "array", "items": {"type": "string"}}
            }
        })
    }

    #[test]
    fn json_validate_passes_on_fenced_block() {
        let spec = OutcomeSpec::Json(schema());
        let reply = "這是我的月報結果：\n```json\n{\"revenue\": 12345, \"month\": \"2026-07\"}\n```\n完成。";
        let check = spec.validate(reply, Path::new("."));
        assert!(check.passed, "defects: {:?}", check.defects);
    }

    #[test]
    fn json_validate_passes_on_bare_json() {
        let spec = OutcomeSpec::Json(schema());
        let reply = r#"{"revenue": 100.5, "month": "July", "items": ["a", "b"]}"#;
        assert!(spec.validate(reply, Path::new(".")).passed);
    }

    #[test]
    fn json_validate_missing_required_field() {
        let spec = OutcomeSpec::Json(schema());
        let reply = r#"{"revenue": 100}"#;
        let check = spec.validate(reply, Path::new("."));
        assert!(!check.passed);
        assert!(
            check.defects.iter().any(|d| d.contains("month")),
            "expected a missing-`month` defect, got {:?}",
            check.defects
        );
    }

    #[test]
    fn json_validate_type_mismatch() {
        let spec = OutcomeSpec::Json(schema());
        let reply = r#"{"revenue": "not a number", "month": "2026-07"}"#;
        let check = spec.validate(reply, Path::new("."));
        assert!(!check.passed);
        assert!(check
            .defects
            .iter()
            .any(|d| d.contains("revenue") && d.contains("number")));
    }

    #[test]
    fn json_validate_nested_array_items() {
        let spec = OutcomeSpec::Json(schema());
        // items[1] is a number, not a string ⇒ defect.
        let reply = r#"{"revenue": 1, "month": "x", "items": ["ok", 5]}"#;
        let check = spec.validate(reply, Path::new("."));
        assert!(!check.passed);
        assert!(check.defects.iter().any(|d| d.contains("items[1]")));
    }

    #[test]
    fn json_validate_no_json_in_reply() {
        let spec = OutcomeSpec::Json(schema());
        let check = spec.validate("我做完了，沒有附任何結構化資料。", Path::new("."));
        assert!(!check.passed);
        assert!(check.defects.iter().any(|d| d.contains("JSON")));
    }

    // ── files assertion (exists / missing / traversal) ──

    #[test]
    fn files_validate_exists_and_missing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("report.docx"), b"x").unwrap();
        std::fs::create_dir(dir.path().join("out")).unwrap();
        std::fs::write(dir.path().join("out").join("q3.pdf"), b"x").unwrap();

        let spec = OutcomeSpec::parse("files:report.docx,out/*.pdf").unwrap();
        assert!(spec.validate("done", dir.path()).passed);

        // A missing glob fails with a concrete defect.
        let miss = OutcomeSpec::parse("files:missing.xlsx").unwrap();
        let check = miss.validate("done", dir.path());
        assert!(!check.passed);
        assert!(check.defects.iter().any(|d| d.contains("missing.xlsx")));
    }

    #[test]
    fn files_validate_glob_question_mark() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a1.txt"), b"x").unwrap();
        let spec = OutcomeSpec::parse("files:a?.txt").unwrap();
        assert!(spec.validate("done", dir.path()).passed);
        let spec2 = OutcomeSpec::parse("files:a??.txt").unwrap();
        assert!(!spec2.validate("done", dir.path()).passed);
    }

    #[test]
    fn files_validate_rejects_traversal_at_check_time() {
        // Even if a traversal glob somehow reaches validate (corrupt tag), it is
        // re-rejected rather than walked.
        let dir = tempfile::tempdir().unwrap();
        let spec = OutcomeSpec::Files(vec!["../escape.txt".into()]);
        let check = spec.validate("done", dir.path());
        assert!(!check.passed);
        assert!(check.defects.iter().any(|d| d.contains("不安全")));
    }

    #[test]
    fn segment_matcher_basics() {
        assert!(segment_matches("*.pdf", "report.pdf"));
        assert!(segment_matches("report.*", "report.pdf"));
        assert!(segment_matches("a?c", "abc"));
        assert!(segment_matches("*", "anything"));
        assert!(!segment_matches("*.pdf", "report.docx"));
        assert!(!segment_matches("a?c", "ac"));
        assert!(segment_matches("q*_*.txt", "q3_final.txt"));
    }
}
