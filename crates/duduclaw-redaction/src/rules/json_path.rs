//! Structured-field rule — selects nodes in a tool result's JSON by
//! *position* rather than by content, and hands them to the pipeline for
//! whole-value tokenisation.
//!
//! This is the engine half of the 2026-09 "資料表欄位" design: a hand-rolled
//! subset of JSONPath (no new crate dependency) plus two gates that decide
//! whether the rule applies to a given tool call at all.
//!
//! ## Grammar
//!
//! ```text
//! path     := "$" segment*
//! segment  := "." key | "['" free_key "']" | "[*]" | "[" digits "]" | ".." key | ".*"
//! key      := [A-Za-z0-9_-]+
//! free_key := any Unicode character except `'`, CR and LF (at least one)
//! ```
//!
//! The bare `.key` form stays ASCII so a path reads unambiguously; the quoted
//! `['key']` form accepts anything a JSON object key can be (2026-09, for
//! local data files whose headers are CJK: `$.rows[*]['地址']`). A quoted key
//! is delimited by its own quotes, `]` included — `['a]b']` is one key.
//!
//! Anything else is a [`RedactionError::RuleCompile`] at load time — a
//! malformed path fails the whole `RedactionManager::open`, exactly like a
//! malformed regex. Silently skipping a field rule would leave an operator
//! believing a column is masked when it is not.
//!
//! ## Two-phase resolution
//!
//! Resolution first walks the value **immutably** and collects RFC-6901 JSON
//! pointers, then the caller re-enters each pointer with
//! `Value::pointer_mut`. Building the pointer list up front sidesteps the
//! borrow conflict of mutating while traversing, and gives the pipeline a
//! stable identifier it can put in the audit record.

use serde_json::Value;

use crate::error::{RedactionError, Result};
use crate::rules::{Match, RestoreScope, Rule, RuleKind, RuleSpec};

/// Maximum node depth visited during resolution (recursive descent and
/// object expansion included). Bounds `..key` / `.*` against adversarial or
/// pathological documents.
pub const MAX_PATH_DEPTH: usize = 64;

/// Maximum number of path expressions accepted in one rule.
pub const MAX_PATHS: usize = 64;

/// Maximum length of a single path expression.
pub const MAX_PATH_LEN: usize = 512;

/// One parsed path step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    /// `.key` or `['key']` — a named child of an object.
    Key(String),
    /// `[*]` — every element of an array.
    AllIndices,
    /// `[n]` — one element of an array by index.
    Index(usize),
    /// `..key` — every `key` child at any depth, current node included.
    Descend(String),
    /// `.*` — every child (object values / array elements).
    AllChildren,
}

/// A parsed path expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonPath {
    segments: Vec<Segment>,
}

impl JsonPath {
    /// Parse one path expression. Returns the raw reason on failure; callers
    /// wrap it into a [`RedactionError::RuleCompile`] with the rule id.
    pub fn parse(raw: &str) -> std::result::Result<Self, String> {
        if raw.len() > MAX_PATH_LEN {
            return Err(format!(
                "path too long ({} > {MAX_PATH_LEN}): {raw}",
                raw.len()
            ));
        }
        let bytes = raw.as_bytes();
        if bytes.first() != Some(&b'$') {
            return Err(format!("path must start with '$': {raw}"));
        }

        let mut segments = Vec::new();
        let mut i = 1usize;
        while i < bytes.len() {
            match bytes[i] {
                b'.' => {
                    // `..key` before `.key`; `.*` before `.key`.
                    if bytes.get(i + 1) == Some(&b'.') {
                        let (key, next) = take_key(bytes, i + 2, raw)?;
                        segments.push(Segment::Descend(key));
                        i = next;
                    } else if bytes.get(i + 1) == Some(&b'*') {
                        segments.push(Segment::AllChildren);
                        i += 2;
                    } else {
                        let (key, next) = take_key(bytes, i + 1, raw)?;
                        segments.push(Segment::Key(key));
                        i = next;
                    }
                }
                b'[' => {
                    let (seg, next) = take_bracket(bytes, i, raw)?;
                    segments.push(seg);
                    i = next;
                }
                other => {
                    return Err(format!(
                        "unexpected character '{}' at byte {i} in path: {raw}",
                        other as char
                    ));
                }
            }
        }

        Ok(JsonPath { segments })
    }

    /// Parsed segments (exposed for tests / diagnostics).
    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }

    /// Resolve this path against `root`, returning RFC-6901 JSON pointers
    /// for every matched node, in document order, without duplicates.
    pub fn resolve(&self, root: &Value) -> Vec<String> {
        // Frontier of (pointer, node, depth).
        let mut frontier: Vec<(String, &Value, usize)> = vec![(String::new(), root, 0)];

        for seg in &self.segments {
            let mut next: Vec<(String, &Value, usize)> = Vec::new();
            for (ptr, node, depth) in frontier.drain(..) {
                if depth >= MAX_PATH_DEPTH {
                    continue;
                }
                match seg {
                    Segment::Key(k) => {
                        if let Some(child) = node.as_object().and_then(|m| m.get(k)) {
                            next.push((push_token(&ptr, k), child, depth + 1));
                        }
                    }
                    Segment::AllIndices => {
                        if let Some(arr) = node.as_array() {
                            for (idx, child) in arr.iter().enumerate() {
                                next.push((push_token(&ptr, &idx.to_string()), child, depth + 1));
                            }
                        }
                    }
                    Segment::Index(n) => {
                        if let Some(child) = node.as_array().and_then(|a| a.get(*n)) {
                            next.push((push_token(&ptr, &n.to_string()), child, depth + 1));
                        }
                    }
                    Segment::AllChildren => match node {
                        Value::Object(map) => {
                            for (k, child) in map {
                                next.push((push_token(&ptr, k), child, depth + 1));
                            }
                        }
                        Value::Array(arr) => {
                            for (idx, child) in arr.iter().enumerate() {
                                next.push((push_token(&ptr, &idx.to_string()), child, depth + 1));
                            }
                        }
                        _ => {}
                    },
                    Segment::Descend(k) => {
                        collect_descend(&ptr, node, k, depth, &mut next);
                    }
                }
            }
            frontier = next;
            if frontier.is_empty() {
                return Vec::new();
            }
        }

        // Dedup while preserving document order — `..key` on nested shapes can
        // legitimately reach the same node twice.
        let mut seen = std::collections::HashSet::new();
        frontier
            .into_iter()
            .map(|(ptr, _, _)| ptr)
            .filter(|p| seen.insert(p.clone()))
            .collect()
    }
}

/// Walk every descendant of `node` (itself included) looking for objects that
/// carry `key`, appending each hit to `out`.
fn collect_descend<'a>(
    ptr: &str,
    node: &'a Value,
    key: &str,
    depth: usize,
    out: &mut Vec<(String, &'a Value, usize)>,
) {
    if depth >= MAX_PATH_DEPTH {
        return;
    }
    match node {
        Value::Object(map) => {
            if let Some(child) = map.get(key) {
                out.push((push_token(ptr, key), child, depth + 1));
            }
            for (k, child) in map {
                collect_descend(&push_token(ptr, k), child, key, depth + 1, out);
            }
        }
        Value::Array(arr) => {
            for (idx, child) in arr.iter().enumerate() {
                collect_descend(
                    &push_token(ptr, &idx.to_string()),
                    child,
                    key,
                    depth + 1,
                    out,
                );
            }
        }
        _ => {}
    }
}

/// Consume a `key` token starting at `from`. Returns the key and the index
/// just past it.
fn take_key(bytes: &[u8], from: usize, raw: &str) -> std::result::Result<(String, usize), String> {
    let mut end = from;
    while end < bytes.len() && is_key_byte(bytes[end]) {
        end += 1;
    }
    if end == from {
        return Err(format!("empty key at byte {from} in path: {raw}"));
    }
    // Key bytes are ASCII by construction, so this slice is char-safe.
    Ok((String::from_utf8_lossy(&bytes[from..end]).into_owned(), end))
}

/// Consume a bracketed segment (`[*]`, `[12]`, `['key']`) starting at the `[`.
fn take_bracket(
    bytes: &[u8],
    from: usize,
    raw: &str,
) -> std::result::Result<(Segment, usize), String> {
    // A quoted key is scanned by its OWN delimiters before anything else:
    // since 2026-09 it may hold any Unicode but `'` / CR / LF — `]` included —
    // so the "first `]` closes the bracket" scan below would truncate
    // `['a]b']`. Searching for the ASCII byte `'` is char-safe: a multi-byte
    // UTF-8 sequence never contains an ASCII byte, so both ends of the slice
    // land on char boundaries (coding convention 1).
    if bytes.get(from + 1) == Some(&b'\'') {
        let start = from + 2;
        let quote_end = bytes[start..]
            .iter()
            .position(|b| *b == b'\'')
            .map(|p| start + p)
            .ok_or_else(|| format!("unclosed quoted key at byte {from} in path: {raw}"))?;
        if bytes.get(quote_end + 1) != Some(&b']') {
            return Err(format!(
                "quoted key at byte {from} must be closed by \"']\" in path: {raw}"
            ));
        }
        let key = std::str::from_utf8(&bytes[start..quote_end]).map_err(|_| {
            format!("quoted key at byte {from} is not valid UTF-8 in path: {raw}")
        })?;
        if key.is_empty() {
            return Err(format!("empty quoted key at byte {from} in path: {raw}"));
        }
        if key.contains('\n') || key.contains('\r') {
            return Err(format!(
                "quoted key at byte {from} must not contain a line break in path: {raw}"
            ));
        }
        return Ok((Segment::Key(key.to_string()), quote_end + 2));
    }

    let close = bytes[from..]
        .iter()
        .position(|b| *b == b']')
        .map(|p| from + p)
        .ok_or_else(|| format!("unclosed '[' at byte {from} in path: {raw}"))?;
    let inner = &bytes[from + 1..close];

    if inner == b"*" {
        return Ok((Segment::AllIndices, close + 1));
    }
    if !inner.is_empty() && inner.iter().all(|b| b.is_ascii_digit()) {
        let n: usize = String::from_utf8_lossy(inner)
            .parse()
            .map_err(|_| format!("index out of range at byte {from} in path: {raw}"))?;
        return Ok((Segment::Index(n), close + 1));
    }
    Err(format!(
        "invalid bracket segment at byte {from} in path: {raw}"
    ))
}

fn is_key_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

/// Append one RFC-6901 reference token to a pointer, escaping `~` and `/`.
///
/// Shared with the pipeline's structured descent so a pointer built during
/// resolution and one built while tokenising are byte-identical.
pub(crate) fn push_token(ptr: &str, token: &str) -> String {
    let mut out = String::with_capacity(ptr.len() + token.len() + 1);
    out.push_str(ptr);
    out.push('/');
    for c in token.chars() {
        match c {
            '~' => out.push_str("~0"),
            '/' => out.push_str("~1"),
            _ => out.push(c),
        }
    }
    out
}

/// Does `tool_name` satisfy a `match_tool` pattern?
///
/// Exact equality, or a single trailing `*` acting as a prefix glob — the
/// same two forms `[redaction.tool_egress]` accepts (`egress.rs`
/// `find_rule`). Deliberately NOT an unanchored `contains` (coding
/// convention 2): `*` must be written explicitly to widen the match.
pub fn match_tool(pattern: &str, tool_name: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => tool_name.starts_with(prefix),
        None => pattern == tool_name,
    }
}

/// Compiled structured-field rule.
#[derive(Debug)]
pub struct JsonPathRule {
    spec: RuleSpec,
    paths: Vec<JsonPath>,
    match_tool: Option<String>,
    match_args: Vec<(String, String)>,
    match_result: Vec<(String, String)>,
    exclude_keys: Vec<String>,
}

impl JsonPathRule {
    /// Compile `spec` into a runtime rule. Returns
    /// [`RedactionError::RuleCompile`] for a non-JsonPath kind, an empty /
    /// oversized path list, any malformed path expression, or a
    /// `match_result` key that is not a JSON pointer.
    pub fn compile(spec: RuleSpec) -> Result<Self> {
        let (raw_paths, match_tool_pat, match_args_map, match_result_map, exclude_keys) =
            match &spec.kind {
                RuleKind::JsonPath {
                    paths,
                    match_tool,
                    match_args,
                    match_result,
                    exclude_keys,
                } => (
                    paths.clone(),
                    match_tool.clone(),
                    match_args.clone(),
                    match_result.clone(),
                    exclude_keys.clone(),
                ),
                other => {
                    return Err(RedactionError::rule_compile(
                        &spec.id,
                        format!("expected JsonPath kind, got {other:?}"),
                    ));
                }
            };

        if raw_paths.is_empty() {
            return Err(RedactionError::rule_compile(
                &spec.id,
                "json_path rule needs at least one path",
            ));
        }
        if raw_paths.len() > MAX_PATHS {
            return Err(RedactionError::rule_compile(
                &spec.id,
                format!("too many paths ({} > {MAX_PATHS})", raw_paths.len()),
            ));
        }
        if let Some(pat) = &match_tool_pat
            && pat.trim().is_empty()
        {
            return Err(RedactionError::rule_compile(
                &spec.id,
                "match_tool must not be blank (omit it to match any tool)",
            ));
        }

        let mut paths = Vec::with_capacity(raw_paths.len());
        for raw in &raw_paths {
            let parsed =
                JsonPath::parse(raw).map_err(|e| RedactionError::rule_compile(&spec.id, e))?;
            paths.push(parsed);
        }

        // Deterministic ordering so two configs with the same pairs behave
        // identically regardless of HashMap iteration order.
        let mut match_args: Vec<(String, String)> = match_args_map.into_iter().collect();
        match_args.sort();

        let mut match_result: Vec<(String, String)> = match_result_map.into_iter().collect();
        match_result.sort();
        for (pointer, _) in &match_result {
            if !pointer.starts_with('/') {
                return Err(RedactionError::rule_compile(
                    &spec.id,
                    format!(
                        "match_result key '{pointer}' is not a JSON pointer — it must start \
                         with '/' (e.g. \"/table\")"
                    ),
                ));
            }
        }

        Ok(JsonPathRule {
            spec,
            paths,
            match_tool: match_tool_pat,
            match_args,
            match_result,
            exclude_keys,
        })
    }

    /// Does this rule apply to the tool named in `ctx`? `None` pattern ⇒ any.
    pub fn matches_tool(&self, tool_name: &str) -> bool {
        match &self.match_tool {
            None => true,
            Some(pat) => match_tool(pat, tool_name),
        }
    }

    /// Does this rule's `match_args` gate pass for the call's arguments?
    ///
    /// Every declared pair must be present in the top level of `args` and
    /// compare exactly equal after stringifying scalars. A missing
    /// `arguments` object, a non-object, a missing key, or a non-scalar value
    /// all fail the gate — the rule then simply does not fire, and the text
    /// pass (which is unconditional) remains in force.
    pub fn matches_args(&self, args: Option<&Value>) -> bool {
        if self.match_args.is_empty() {
            return true;
        }
        let Some(obj) = args.and_then(|v| v.as_object()) else {
            return false;
        };
        for (key, expected) in &self.match_args {
            let Some(actual) = obj.get(key) else {
                return false;
            };
            let rendered = match actual {
                Value::String(s) => s.clone(),
                Value::Number(n) => n.to_string(),
                Value::Bool(b) => b.to_string(),
                // null / array / object never equal a scalar literal.
                _ => return false,
            };
            if rendered != *expected {
                return false;
            }
        }
        true
    }

    /// Does this rule's `match_result` gate pass for `value`?
    ///
    /// Each declared pointer must resolve **inside `value` itself** to a
    /// scalar whose stringified form equals the expected literal — exact
    /// equality, never a substring (coding convention 2). Anything else
    /// (missing pointer, null, array, object, different value) fails the gate.
    ///
    /// The value the gate reads is deliberately the same one the rule's paths
    /// are resolved against: for a tool that names its table *in the result*
    /// (`csv_read` → `{"table": "customers.csv", "rows": […]}`) the table fact
    /// and the records live in the same document, and that document may be a
    /// JSON payload embedded in a text leaf. Evaluating the gate against the
    /// outer MCP envelope instead would never resolve and would make every
    /// such rule dead.
    pub fn matches_result(&self, value: &Value) -> bool {
        if self.match_result.is_empty() {
            return true;
        }
        for (pointer, expected) in &self.match_result {
            let Some(actual) = value.pointer(pointer) else {
                return false;
            };
            let rendered = match actual {
                Value::String(s) => s.clone(),
                Value::Number(n) => n.to_string(),
                Value::Bool(b) => b.to_string(),
                // null / array / object never equal a scalar literal.
                _ => return false,
            };
            if rendered != *expected {
                return false;
            }
        }
        true
    }

    /// Object keys never tokenised beneath a matched node.
    pub fn exclude_keys(&self) -> &[String] {
        &self.exclude_keys
    }

    /// Resolve every configured path against `root`, in rule order, deduped.
    pub fn resolve_pointers(&self, root: &Value) -> Vec<String> {
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for path in &self.paths {
            for ptr in path.resolve(root) {
                if seen.insert(ptr.clone()) {
                    out.push(ptr);
                }
            }
        }
        out
    }
}

impl Rule for JsonPathRule {
    fn id(&self) -> &str {
        &self.spec.id
    }

    fn category(&self) -> &str {
        &self.spec.category
    }

    fn restore_scope(&self) -> &RestoreScope {
        &self.spec.restore_scope
    }

    fn priority(&self) -> i32 {
        self.spec.priority
    }

    fn cross_session_stable(&self) -> bool {
        self.spec.cross_session_stable
    }

    fn apply_to_system_prompt(&self) -> bool {
        self.spec.apply_to_system_prompt
    }

    /// Structured rules never match free text — they only ever fire through
    /// the pipeline's structured pass. Returning matches here would let a
    /// field rule tokenise arbitrary prose that merely *looked* like the
    /// field's value.
    fn match_text(&self, _text: &str) -> Vec<Match> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn spec(paths: &[&str]) -> RuleSpec {
        RuleSpec {
            id: "field".into(),
            category: "DB_FIELD".into(),
            restore_scope: RestoreScope::Owner,
            priority: 50,
            cross_session_stable: false,
            apply_to_system_prompt: false,
            kind: RuleKind::JsonPath {
                paths: paths.iter().map(|s| s.to_string()).collect(),
                match_tool: None,
                match_args: HashMap::new(),
                match_result: Default::default(),
                exclude_keys: Vec::new(),
            },
        }
    }

    // ── grammar ──────────────────────────────────────────────────────────

    #[test]
    fn parses_every_segment_form() {
        let p = JsonPath::parse("$.a['b-c'][*][3]..deep.*").unwrap();
        assert_eq!(
            p.segments(),
            &[
                Segment::Key("a".into()),
                Segment::Key("b-c".into()),
                Segment::AllIndices,
                Segment::Index(3),
                Segment::Descend("deep".into()),
                Segment::AllChildren,
            ]
        );
    }

    #[test]
    fn bare_dollar_is_the_root() {
        let p = JsonPath::parse("$").unwrap();
        assert!(p.segments().is_empty());
        let v = serde_json::json!({"a": 1});
        assert_eq!(p.resolve(&v), vec!["".to_string()]);
    }

    #[test]
    fn rejects_malformed_paths() {
        for bad in [
            "name",        // no leading $
            "$.",          // empty key
            "$..",         // empty descend key
            "$[",          // unclosed bracket
            "$[abc]",      // non-numeric, non-quoted
            "$.a b",       // stray character (the BARE key form stays ASCII)
            "$.地址",      // CJK is only legal in the quoted form
            "$['unclosed", // quote never closed
            "$['x'",       // quote closed, bracket not
            "$['x'y]",     // `'` not followed by `]`
            "$['']",       // empty quoted key
            "$[]",         // empty bracket
            "$['a\nb']",   // line break inside a quoted key
            "$['a\rb']",
        ] {
            assert!(JsonPath::parse(bad).is_err(), "expected error for {bad:?}");
        }
    }

    #[test]
    fn quoted_key_accepts_any_unicode_but_quote_and_line_breaks() {
        // The 2026-09 relaxation: spreadsheet headers are not identifiers.
        for (raw, key) in [
            ("$['地址']", "地址"),
            ("$['unit price']", "unit price"),
            ("$['a]b']", "a]b"),
            ("$['e-mail (work)']", "e-mail (work)"),
            ("$['金額 $']", "金額 $"),
            ("$['a.b']", "a.b"),
        ] {
            let p = JsonPath::parse(raw).unwrap_or_else(|e| panic!("{raw}: {e}"));
            assert_eq!(p.segments(), &[Segment::Key(key.to_string())], "{raw}");
        }
    }

    #[test]
    fn cjk_key_resolves_through_a_record_array() {
        let v = serde_json::json!({"rows": [{"地址": "臺北市中正區虛構路 100 號"}]});
        let p = JsonPath::parse("$.rows[*]['地址']").unwrap();
        assert_eq!(p.resolve(&v), vec!["/rows/0/地址"]);
    }

    #[test]
    fn quoted_key_pointer_escaping_is_unchanged() {
        // `/` and `~` still become `~1` / `~0` — the relaxed charset changed
        // what a key may CONTAIN, not how a pointer spells it.
        let v = serde_json::json!({"a/b~c": "x"});
        assert_eq!(
            JsonPath::parse("$['a/b~c']").unwrap().resolve(&v),
            vec!["/a~1b~0c"]
        );
    }

    #[test]
    fn rejects_oversized_path() {
        let long = format!("$.{}", "a".repeat(MAX_PATH_LEN));
        assert!(JsonPath::parse(&long).is_err());
    }

    // ── resolution ───────────────────────────────────────────────────────

    #[test]
    fn resolves_array_of_records() {
        let v = serde_json::json!([
            {"id": 1, "name": "Alice"},
            {"id": 2, "name": "Bob"},
        ]);
        let p = JsonPath::parse("$[*].name").unwrap();
        assert_eq!(p.resolve(&v), vec!["/0/name", "/1/name"]);
    }

    #[test]
    fn resolves_single_object() {
        let v = serde_json::json!({"id": 1, "name": "Alice"});
        let p = JsonPath::parse("$.name").unwrap();
        assert_eq!(p.resolve(&v), vec!["/name"]);
        // The array form simply misses — no panic, no hit.
        assert!(JsonPath::parse("$[*].name").unwrap().resolve(&v).is_empty());
    }

    #[test]
    fn resolves_index_and_quoted_key() {
        let v = serde_json::json!({"rows": [{"full-name": "Alice"}, {"full-name": "Bob"}]});
        assert_eq!(
            JsonPath::parse("$.rows[1]['full-name']")
                .unwrap()
                .resolve(&v),
            vec!["/rows/1/full-name"]
        );
        assert!(
            JsonPath::parse("$.rows[9]['full-name']")
                .unwrap()
                .resolve(&v)
                .is_empty()
        );
    }

    #[test]
    fn descend_finds_key_at_any_depth() {
        let v = serde_json::json!({
            "a": {"partner_name": "X"},
            "b": [{"c": {"partner_name": "Y"}}],
            "partner_name": "Z",
        });
        let mut got = JsonPath::parse("$..partner_name").unwrap().resolve(&v);
        got.sort();
        assert_eq!(
            got,
            vec!["/a/partner_name", "/b/0/c/partner_name", "/partner_name"]
        );
    }

    #[test]
    fn all_children_expands_objects_and_arrays() {
        let obj = serde_json::json!({"a": 1, "b": 2});
        let mut got = JsonPath::parse("$.*").unwrap().resolve(&obj);
        got.sort();
        assert_eq!(got, vec!["/a", "/b"]);

        let arr = serde_json::json!([10, 20]);
        assert_eq!(
            JsonPath::parse("$.*").unwrap().resolve(&arr),
            vec!["/0", "/1"]
        );
    }

    #[test]
    fn depth_cap_stops_runaway_descent() {
        // Build a chain deeper than the cap with the target key at the bottom.
        let mut v = serde_json::json!({"secret": "deep"});
        for _ in 0..(MAX_PATH_DEPTH + 10) {
            v = serde_json::json!({"n": v});
        }
        let hits = JsonPath::parse("$..secret").unwrap().resolve(&v);
        assert!(
            hits.is_empty(),
            "descent past the depth cap must not resolve, got {hits:?}"
        );
    }

    #[test]
    fn pointer_tokens_are_rfc6901_escaped() {
        let v = serde_json::json!({"a/b": {"c~d": "x"}});
        assert_eq!(
            JsonPath::parse("$.*.*").unwrap().resolve(&v),
            vec!["/a~1b/c~0d"]
        );
    }

    // ── compile + gates ──────────────────────────────────────────────────

    #[test]
    fn compile_rejects_empty_and_bad_paths() {
        assert!(JsonPathRule::compile(spec(&[])).is_err());
        assert!(JsonPathRule::compile(spec(&["not-a-path"])).is_err());
        assert!(JsonPathRule::compile(spec(&["$.ok", "$["])).is_err());
    }

    #[test]
    fn compile_rejects_wrong_kind() {
        let mut s = spec(&["$.name"]);
        s.kind = RuleKind::Regex {
            pattern: "x".into(),
        };
        assert!(JsonPathRule::compile(s).is_err());
    }

    #[test]
    fn match_text_is_always_empty() {
        let rule = JsonPathRule::compile(spec(&["$.name"])).unwrap();
        assert!(rule.match_text("Alice lives at name: Alice").is_empty());
    }

    #[test]
    fn tool_gate_exact_and_glob() {
        assert!(match_tool("odoo_search", "odoo_search"));
        assert!(!match_tool("odoo_search", "odoo_search_x"));
        assert!(match_tool("odoo_*", "odoo_search"));
        assert!(!match_tool("odoo_*", "crm_search"));
        // Not an unanchored contains.
        assert!(!match_tool("search", "odoo_search"));
    }

    #[test]
    fn tool_gate_none_matches_anything() {
        let rule = JsonPathRule::compile(spec(&["$.name"])).unwrap();
        assert!(rule.matches_tool("anything_at_all"));
    }

    fn spec_with_args(pairs: &[(&str, &str)]) -> RuleSpec {
        let mut s = spec(&["$.name"]);
        s.kind = RuleKind::JsonPath {
            paths: vec!["$.name".into()],
            match_tool: Some("odoo_search".into()),
            match_args: pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            match_result: Default::default(),
            exclude_keys: vec![],
        };
        s
    }

    fn spec_with_result(pairs: &[(&str, &str)]) -> RuleSpec {
        let mut s = spec(&["$.rows[*].name"]);
        s.kind = RuleKind::JsonPath {
            paths: vec!["$.rows[*].name".into()],
            match_tool: Some("csv_read".into()),
            match_args: HashMap::new(),
            match_result: pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            exclude_keys: vec![],
        };
        s
    }

    #[test]
    fn result_gate_requires_exact_equality() {
        let rule = JsonPathRule::compile(spec_with_result(&[("/table", "customers.csv")])).unwrap();
        assert!(rule.matches_result(&serde_json::json!({"table": "customers.csv", "rows": []})));
        // A different file of the SAME tool must not fire.
        assert!(!rule.matches_result(&serde_json::json!({"table": "orders.csv", "rows": []})));
        // Substring must NOT satisfy the gate (coding convention 2).
        assert!(!rule.matches_result(&serde_json::json!({"table": "x/customers.csv"})));
        assert!(!rule.matches_result(&serde_json::json!({"table": "customers.csv.bak"})));
        // Missing pointer, or a non-scalar at it.
        assert!(!rule.matches_result(&serde_json::json!({"rows": []})));
        assert!(!rule.matches_result(&serde_json::json!({"table": null})));
        assert!(!rule.matches_result(&serde_json::json!({"table": ["customers.csv"]})));
        // The MCP envelope does not carry `/table` — the gate is meant to be
        // evaluated on the payload, not on the transport.
        assert!(!rule.matches_result(&serde_json::json!({
            "content": [{"type": "text", "text": "{\"table\":\"customers.csv\"}"}]
        })));
    }

    #[test]
    fn result_gate_stringifies_scalars_and_walks_nested_pointers() {
        let rule =
            JsonPathRule::compile(spec_with_result(&[("/meta/sheet", "1"), ("/ok", "true")]))
                .unwrap();
        assert!(rule.matches_result(&serde_json::json!({"meta": {"sheet": 1}, "ok": true})));
        assert!(rule.matches_result(&serde_json::json!({"meta": {"sheet": "1"}, "ok": "true"})));
        assert!(!rule.matches_result(&serde_json::json!({"meta": {"sheet": 2}, "ok": true})));
    }

    #[test]
    fn empty_result_gate_always_passes() {
        let rule = JsonPathRule::compile(spec(&["$.name"])).unwrap();
        assert!(rule.matches_result(&serde_json::json!({})));
        assert!(rule.matches_result(&serde_json::json!(null)));
    }

    #[test]
    fn compile_rejects_a_match_result_key_that_is_not_a_pointer() {
        let err = JsonPathRule::compile(spec_with_result(&[("table", "customers.csv")]))
            .unwrap_err();
        assert!(err.to_string().contains("JSON pointer"), "{err}");
        // The root pointer is refused too: it would ask the whole document to
        // equal a string.
        assert!(JsonPathRule::compile(spec_with_result(&[("", "x")])).is_err());
    }

    #[test]
    fn arg_gate_requires_exact_equality() {
        let rule = JsonPathRule::compile(spec_with_args(&[("model", "res.partner")])).unwrap();
        assert!(rule.matches_args(Some(&serde_json::json!({"model": "res.partner"}))));
        assert!(!rule.matches_args(Some(&serde_json::json!({"model": "res.partners"}))));
        // Substring must NOT satisfy the gate (coding convention 2).
        assert!(!rule.matches_args(Some(&serde_json::json!({"model": "xres.partnerx"}))));
        assert!(!rule.matches_args(Some(&serde_json::json!({"other": "res.partner"}))));
        assert!(!rule.matches_args(Some(&serde_json::json!("res.partner"))));
        assert!(!rule.matches_args(None));
    }

    #[test]
    fn arg_gate_stringifies_scalars() {
        let rule =
            JsonPathRule::compile(spec_with_args(&[("limit", "20"), ("all", "true")])).unwrap();
        assert!(rule.matches_args(Some(&serde_json::json!({"limit": 20, "all": true}))));
        assert!(rule.matches_args(Some(&serde_json::json!({"limit": "20", "all": "true"}))));
        assert!(!rule.matches_args(Some(&serde_json::json!({"limit": 21, "all": true}))));
        // A non-scalar can never equal the literal.
        assert!(!rule.matches_args(Some(&serde_json::json!({"limit": [20], "all": true}))));
    }

    #[test]
    fn empty_arg_gate_always_passes() {
        let rule = JsonPathRule::compile(spec(&["$.name"])).unwrap();
        assert!(rule.matches_args(None));
        assert!(rule.matches_args(Some(&serde_json::json!({}))));
    }

    #[test]
    fn match_result_is_wire_compatible_and_round_trips() {
        // An existing config that never heard of `match_result` must keep
        // parsing, and a new one must survive the toml round trip.
        let old: RuleSpec = toml::from_str(
            "type = \"json_path\"\ncategory = \"DB_FIELD\"\npaths = [\"$.name\"]\n",
        )
        .expect("pre-2026-09 json_path rule must still parse");
        match &old.kind {
            RuleKind::JsonPath { match_result, .. } => assert!(match_result.is_empty()),
            other => panic!("expected JsonPath, got {other:?}"),
        }

        let new: RuleSpec = toml::from_str(
            "type = \"json_path\"\ncategory = \"DB_FIELD\"\npaths = [\"$.rows[*].name\"]\n\
             match_tool = \"csv_read\"\nmatch_result = { \"/table\" = \"customers.csv\" }\n",
        )
        .unwrap();
        match &new.kind {
            RuleKind::JsonPath { match_result, .. } => {
                assert_eq!(match_result["/table"], "customers.csv");
            }
            other => panic!("expected JsonPath, got {other:?}"),
        }
        let back: RuleSpec = toml::from_str(&toml::to_string(&new).unwrap()).unwrap();
        assert_eq!(back.kind, new.kind);
    }

    #[test]
    fn resolve_pointers_dedups_across_paths() {
        let rule = JsonPathRule::compile(spec(&["$[*].name", "$..name"])).unwrap();
        let v = serde_json::json!([{"name": "A"}, {"name": "B"}]);
        assert_eq!(rule.resolve_pointers(&v), vec!["/0/name", "/1/name"]);
    }
}
