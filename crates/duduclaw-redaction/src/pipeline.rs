//! High-level redact / restore API.
//!
//! `RedactionPipeline` ties together [`RuleEngine`], [`VaultStore`], and an
//! [`AuditSink`]. One pipeline instance is bound to a single
//! `(agent_id, session_id)` pair; create a fresh pipeline per conversation
//! via [`PipelineFactory`] so per-session salts isolate token spaces.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::audit::{AuditEvent, AuditSink};
use crate::config::{SourceMode, SourcePolicy};
use crate::engine::RuleEngine;
use crate::error::{RedactionError, Result};
use crate::rules::json_path::MAX_PATH_DEPTH;
use crate::rules::{RestoreScope, Rule};
use crate::source::{Caller, RestoreTarget, Source};
use crate::token::{self, TOKEN_PREFIX, TOKEN_SUFFIX, Token};
use crate::vault::VaultStore;

/// Largest string leaf we will try to parse as embedded JSON. Past this the
/// leaf still goes through the text pass, but the structured pass skips it —
/// a multi-megabyte parse per leaf is a denial-of-service lever, not a
/// feature.
pub const MAX_EMBEDDED_JSON_BYTES: usize = 4 * 1024 * 1024;

/// Result of a redact pass.
#[derive(Debug, Clone)]
pub struct RedactionOutput {
    /// Text with PII spans replaced by tokens.
    pub redacted_text: String,
    /// Distinct tokens inserted in this pass (in original-source order).
    pub tokens_written: Vec<Token>,
}

/// Which tool call a structured redaction pass belongs to.
///
/// `args` is the MCP call's `arguments` object — structured rules use it for
/// their `match_args` gate (e.g. "only when `model = res.partner`"), which is
/// how one generic tool like `odoo_search` can carry per-model field rules.
#[derive(Debug, Clone, Copy)]
pub struct ToolContext<'a> {
    pub tool_name: &'a str,
    pub args: Option<&'a Value>,
}

impl<'a> ToolContext<'a> {
    /// Context for a tool whose arguments are unavailable at this layer.
    pub fn new(tool_name: &'a str) -> Self {
        Self { tool_name, args: None }
    }
}

/// Per-conversation pipeline.
pub struct RedactionPipeline {
    engine: Arc<RuleEngine>,
    vault: Arc<VaultStore>,
    audit: Arc<dyn AuditSink>,

    agent_id: String,
    session_id: Option<String>,
    session_salt: [u8; 32],
    stable_salt: [u8; 32],

    source_policy: SourcePolicy,
    vault_ttl_hours: i64,
}

impl RedactionPipeline {
    /// Construct a pipeline for a specific `(agent, session)` pair.
    ///
    /// `agent_key` should be the same 32-byte per-agent key used by
    /// the vault encryption layer — the pipeline derives both per-session
    /// and per-agent-stable salts from it.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        engine: Arc<RuleEngine>,
        vault: Arc<VaultStore>,
        audit: Arc<dyn AuditSink>,
        agent_id: impl Into<String>,
        session_id: Option<String>,
        agent_key: &[u8],
        source_policy: SourcePolicy,
        vault_ttl_hours: i64,
    ) -> Self {
        let session_label = session_id.clone().unwrap_or_else(|| "default".to_string());
        Self {
            engine,
            vault,
            audit,
            agent_id: agent_id.into(),
            session_id,
            session_salt: token::derive_session_salt(agent_key, &session_label),
            stable_salt: token::derive_stable_salt(agent_key),
            source_policy,
            vault_ttl_hours,
        }
    }

    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }

    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    /// Decide whether to redact text from `source`. Returns `SourceMode`
    /// in effect.
    fn setting_for(&self, source: &Source) -> &crate::config::SourceSetting {
        match source {
            Source::UserChannelInput { .. } => &self.source_policy.user_input,
            Source::ToolResult { .. } => &self.source_policy.tool_results,
            Source::SystemPrompt { .. } => &self.source_policy.system_prompt,
            Source::SubAgentReply { .. } => &self.source_policy.sub_agent,
            Source::CronContext => &self.source_policy.cron_context,
        }
    }

    /// Redact text. Returns the rewritten text and the list of new tokens
    /// inserted into the vault. **Fail-closed**: if any insert fails, the
    /// entire call returns `Err` and the caller MUST drop the LLM request.
    pub fn redact(&self, text: &str, source: &Source) -> Result<RedactionOutput> {
        let setting = self.setting_for(source);
        match setting.mode {
            SourceMode::Off | SourceMode::Inherit => {
                return Ok(RedactionOutput {
                    redacted_text: text.to_string(),
                    tokens_written: Vec::new(),
                });
            }
            SourceMode::On => {}
            SourceMode::Selective => {
                // Selective only takes effect for the system-prompt source,
                // where the engine restricts firing to `apply_to_system_prompt`
                // rules. For any other source, Selective must NOT force a full
                // redact — pass the text through unchanged.
                if !matches!(source, Source::SystemPrompt { .. }) {
                    return Ok(RedactionOutput {
                        redacted_text: text.to_string(),
                        tokens_written: Vec::new(),
                    });
                }
            }
        }

        // Spans already occupied by a `<REDACT:CAT:hash>` token. Since the
        // structured pass runs first, the text pass now routinely sees text
        // that already contains tokens — and a 32-hex hash can easily hold
        // eight consecutive digits, which a rule like `TW_BIZ_ID` (`\d{8}`)
        // would happily match, wrapping a token inside another token and
        // breaking restore. Any match overlapping a real token is dropped.
        let protected = token_spans(text);

        // Per-source field filter: the operator may narrow this source to
        // specific PII categories (only_categories / exclude_categories).
        let matches: Vec<_> = self
            .engine
            .apply(text, source)
            .into_iter()
            .filter(|m| setting.allows_category(m.rule.category()))
            .filter(|m| !overlaps_any(m.span.start, m.span.end, &protected))
            .collect();
        if matches.is_empty() {
            return Ok(RedactionOutput {
                redacted_text: text.to_string(),
                tokens_written: Vec::new(),
            });
        }

        let mut out = String::with_capacity(text.len());
        let mut cursor = 0;
        let mut tokens_written: Vec<Token> = Vec::with_capacity(matches.len());

        for m in matches {
            // Append text leading up to the match.
            out.push_str(&text[cursor..m.span.start]);

            let tok = self.mint_token(&m.span.original, m.rule.as_ref(), source, None)?;

            out.push_str(tok.as_str());
            cursor = m.span.end;
            tokens_written.push(tok);
        }

        out.push_str(&text[cursor..]);
        Ok(RedactionOutput {
            redacted_text: out,
            tokens_written,
        })
    }

    /// Mint one token for `original`, persist the mapping, and audit it.
    ///
    /// The single place vault + audit semantics live, shared by the text pass
    /// and the structured pass so the two can never drift apart (salt choice,
    /// session scoping, fail-closed insert, audit shape). `path` is the JSON
    /// pointer for a structured hit and `None` for a text hit.
    fn mint_token(
        &self,
        original: &str,
        rule: &dyn Rule,
        source: &Source,
        path: Option<String>,
    ) -> Result<Token> {
        // Per-session unless the rule is cross-session-stable.
        let salt = if rule.cross_session_stable() {
            &self.stable_salt[..]
        } else {
            &self.session_salt[..]
        };
        let hash = token::session_hash(salt, original.as_bytes());
        let tok = Token::new(rule.category(), &hash)?;

        let session_for_vault = if rule.cross_session_stable() {
            None
        } else {
            self.session_id.as_deref()
        };

        // Persist mapping (fail-closed: the caller must drop the payload).
        self.vault.insert_mapping(
            tok.as_str(),
            original,
            &self.agent_id,
            session_for_vault,
            rule.category(),
            rule.id(),
            rule.restore_scope(),
            rule.cross_session_stable(),
            self.vault_ttl_hours,
        )?;

        let (source_category, source_detail) = source_meta(source);
        self.audit.emit(AuditEvent::Redact {
            agent_id: self.agent_id.clone(),
            session_id: self.session_id.clone(),
            source_category: source_category.into(),
            source_detail,
            rule_id: rule.id().into(),
            category: rule.category().into(),
            token: tok.as_str().into(),
            path,
            // Read off the rule itself so the attribution cannot drift from
            // what actually produced the span (a `ner` rule always answers
            // `"ner"`; every deterministic matcher takes the trait default).
            engine: rule.engine_kind().to_string(),
            model_revision: rule.model_revision().map(str::to_string),
        });

        Ok(tok)
    }

    /// Redact a structured tool result in place.
    ///
    /// Three passes, in this order:
    ///
    /// 1. **Structured on the value** — field rules select nodes by JSON
    ///    position and the whole value becomes one token, no pattern needed.
    ///    A root-pointer hit is refused here: on the outer value the root is
    ///    the MCP envelope (`{"content":[{"type":"text",...}]}`), not a record,
    ///    so a `model.*` rule (whose `$` path legitimately means "the whole
    ///    record") would otherwise swallow the envelope itself — tokenising
    ///    `content[0].type` and the entire payload as one opaque blob.
    /// 2. **Structured inside JSON-in-text leaves** — the MCP tools wrap their
    ///    records as pretty-printed JSON inside `content[0].text`, so the
    ///    interesting structure is one level down inside a string. Leaves that
    ///    parse as a JSON object/array get the same treatment and are written
    ///    back in their original shape (pretty stays pretty, compact stays
    ///    compact). Here the root IS the record array / single record, so a
    ///    root-pointer hit is exactly what `model.*` asked for and is allowed.
    /// 3. **Text** — the existing pattern rules over every string leaf, which
    ///    still catches emails, ids and phone numbers in columns no field rule
    ///    named.
    ///
    /// Fail-closed: any vault write failure aborts the whole call with `Err`
    /// and the caller must withhold the value, not ship a half-redacted one.
    pub fn redact_value(&self, value: &mut Value, ctx: &ToolContext<'_>) -> Result<Vec<Token>> {
        let source = Source::ToolResult {
            tool_name: ctx.tool_name.to_string(),
        };
        let setting = self.setting_for(&source);
        match setting.mode {
            SourceMode::Off | SourceMode::Inherit => return Ok(Vec::new()),
            SourceMode::On => {}
            // Selective is a system-prompt-only concept; for a tool result it
            // means "leave alone" exactly as `redact` treats it.
            SourceMode::Selective => return Ok(Vec::new()),
        }

        let mut tokens: Vec<Token> = Vec::new();

        // ── 1. structured pass over the value itself ─────────────────────
        // `allow_root = false`: the root here is the tool envelope, never a record.
        self.structured_pass(value, ctx, &source, &mut tokens, false)?;

        // ── 2. structured pass inside JSON-bearing string leaves ─────────
        let mut embedded_err: Option<RedactionError> = None;
        walk_strings(value, &mut |s| {
            if embedded_err.is_some() {
                return;
            }
            match self.redact_embedded_json(s, ctx, &source, &mut tokens) {
                Ok(()) => {}
                Err(e) => embedded_err = Some(e),
            }
        });
        if let Some(e) = embedded_err {
            return Err(e);
        }

        // ── 3. text pass over every string leaf ──────────────────────────
        let mut text_err: Option<RedactionError> = None;
        walk_strings(value, &mut |s| {
            if text_err.is_some() || s.is_empty() {
                return;
            }
            match self.redact(s, &source) {
                Ok(out) => {
                    if !out.tokens_written.is_empty() {
                        *s = out.redacted_text;
                        tokens.extend(out.tokens_written);
                    }
                }
                Err(e) => text_err = Some(e),
            }
        });
        if let Some(e) = text_err {
            return Err(e);
        }

        Ok(tokens)
    }

    /// Run every applicable structured rule over `value`, tokenising the
    /// nodes they select.
    ///
    /// `allow_root` decides whether a hit on the root pointer (`""`, produced
    /// by the `$` path a `model.*` rule expands to) may be tokenised. It is
    /// true only when `value` really is the record payload — i.e. inside the
    /// embedded-JSON pass. On the outer value the root is the MCP envelope,
    /// and consuming it would mask the transport rather than the data.
    fn structured_pass(
        &self,
        value: &mut Value,
        ctx: &ToolContext<'_>,
        source: &Source,
        tokens: &mut Vec<Token>,
        allow_root: bool,
    ) -> Result<()> {
        let setting = self.setting_for(source);
        let hits = self.engine.apply_structured(value, ctx, setting);
        for hit in hits {
            let crate::engine::StructuredHit { pointer, rule } = hit;
            if pointer.is_empty() && !allow_root {
                // Root hit on the envelope — see `allow_root`. The same rule's
                // `$[*]` path still covers a bare record array, and the
                // embedded-JSON pass still covers the wrapped shape.
                continue;
            }
            // Re-enter by pointer: resolution was immutable, mutation is not.
            // A pointer can only go stale if a pass changed the document's
            // shape, and no pass ever inserts or removes nodes — scalars are
            // only ever replaced in place.
            let Some(node) = value.pointer_mut(&pointer) else {
                continue;
            };
            let mut mint = |original: &str, path: String| {
                self.mint_token(original, rule.as_ref(), source, Some(path))
            };
            tokenize_node(node, &pointer, rule.exclude_keys(), 0, &mut mint, tokens)?;
        }
        Ok(())
    }

    /// If `s` carries an embedded JSON object/array, run the structured pass
    /// inside it and write the result back in the same shape.
    fn redact_embedded_json(
        &self,
        s: &mut String,
        ctx: &ToolContext<'_>,
        source: &Source,
        tokens: &mut Vec<Token>,
    ) -> Result<()> {
        if s.len() > MAX_EMBEDDED_JSON_BYTES {
            return Ok(());
        }
        let trimmed = s.trim_start();
        if !(trimmed.starts_with('{') || trimmed.starts_with('[')) {
            return Ok(());
        }
        let Ok(mut parsed) = serde_json::from_str::<Value>(s) else {
            return Ok(());
        };

        let before = tokens.len();
        // `allow_root = true`: `parsed` is the record payload itself, so a
        // `model.*` rule's `$` path means what the operator wrote.
        self.structured_pass(&mut parsed, ctx, source, tokens, true)?;
        if tokens.len() == before {
            // Nothing changed — leave the operator's exact bytes alone.
            return Ok(());
        }

        // Preserve the incoming layout: these payloads are shown to the model
        // and a silent reflow is gratuitous churn (and cache-hostile).
        let reserialized = if s.contains('\n') {
            serde_json::to_string_pretty(&parsed)?
        } else {
            serde_json::to_string(&parsed)?
        };
        *s = reserialized;
        Ok(())
    }

    /// Restore tokens in `text`. Tokens whose scope is not satisfied stay
    /// in place. Expired tokens are replaced with a `[已過期 PII · DATE]`
    /// placeholder. The `AuditLog` target NEVER decrypts.
    pub fn restore(
        &self,
        text: &str,
        caller: &Caller,
        target: RestoreTarget,
    ) -> Result<String> {
        if matches!(target, RestoreTarget::AuditLog) {
            // Audit log path: never decrypt, just return as-is.
            return Ok(text.to_string());
        }

        let agent_id = self.agent_id.clone();
        let session_id = self.session_id.clone();
        let vault = self.vault.clone();
        let audit = self.audit.clone();
        let target_str = match &target {
            RestoreTarget::UserChannel => "user_channel".to_string(),
            RestoreTarget::SubAgent { agent_id } => format!("sub_agent:{agent_id}"),
            RestoreTarget::AuditLog => "audit_log".to_string(),
        };

        let mut result_error: Option<RedactionError> = None;

        let rewritten = rewrite_tokens(text, |tok| {
            // Stop processing further tokens if an unrecoverable error happened.
            if result_error.is_some() {
                return TokenAction::Keep;
            }
            match vault.lookup_mapping(tok.as_str(), &agent_id, session_id.as_deref()) {
                Err(e) => {
                    result_error = Some(e);
                    TokenAction::Keep
                }
                Ok(None) => {
                    audit.emit(AuditEvent::RestoreMiss {
                        agent_id: agent_id.clone(),
                        caller: caller_label(caller),
                        target: target_str.clone(),
                        token: tok.as_str().into(),
                    });
                    TokenAction::Keep
                }
                Ok(Some(entry)) => {
                    // Scope check.
                    if !entry.restore_scope.allows(caller) {
                        audit.emit(AuditEvent::RestoreDenied {
                            agent_id: agent_id.clone(),
                            caller: caller_label(caller),
                            target: target_str.clone(),
                            token: tok.as_str().into(),
                            required_scope: entry.restore_scope.wire(),
                        });
                        return TokenAction::Keep;
                    }
                    match entry.original {
                        Some(plain) => {
                            let _ = vault.record_reveal(
                                tok.as_str(),
                                &entry.agent_id,
                                entry.session_id.as_deref(),
                            );
                            audit.emit(AuditEvent::RestoreOk {
                                agent_id: agent_id.clone(),
                                caller: caller_label(caller),
                                target: target_str.clone(),
                                token: tok.as_str().into(),
                            });
                            TokenAction::Replace(plain)
                        }
                        None => {
                            // Expired token: no cleartext is revealed, only a
                            // placeholder is emitted. Emitting RestoreOk here
                            // would inflate the audit's PII-reveal count, so we
                            // emit a non-reveal event instead.
                            let placeholder = expired_placeholder(entry.expires_at);
                            audit.emit(AuditEvent::RestoreMiss {
                                agent_id: agent_id.clone(),
                                caller: caller_label(caller),
                                target: format!("{target_str}#expired"),
                                token: tok.as_str().into(),
                            });
                            TokenAction::Replace(placeholder)
                        }
                    }
                }
            }
        });

        if let Some(err) = result_error {
            return Err(err);
        }
        Ok(rewritten)
    }
}

/// Token-level rewrite. Used internally by [`RedactionPipeline::restore`].
enum TokenAction {
    /// Keep the raw `<REDACT:...>` text.
    Keep,
    /// Replace the token with this string.
    Replace(String),
}

fn rewrite_tokens(text: &str, mut decide: impl FnMut(&Token) -> TokenAction) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(TOKEN_PREFIX) {
        out.push_str(&rest[..start]);
        let from = &rest[start..];
        let Some(end_rel) = from.find(TOKEN_SUFFIX) else {
            out.push_str(from);
            return out;
        };
        let candidate = &from[..end_rel + TOKEN_SUFFIX.len()];
        match Token::parse(candidate) {
            Some(tok) => match decide(&tok) {
                TokenAction::Replace(plain) => out.push_str(&plain),
                TokenAction::Keep => out.push_str(candidate),
            },
            None => out.push_str(candidate),
        }
        rest = &from[end_rel + TOKEN_SUFFIX.len()..];
    }
    out.push_str(rest);
    out
}

/// Tokenise everything beneath a node a structured rule selected.
///
/// Scalars become one token each; containers recurse, re-applying
/// `exclude_keys` at every object level (not just the matched node's own).
/// A node that is already a token is left alone — that is what makes two
/// rules selecting the same node deterministic: the first one wins.
fn tokenize_node(
    node: &mut Value,
    pointer: &str,
    exclude_keys: &[String],
    depth: usize,
    mint: &mut dyn FnMut(&str, String) -> Result<Token>,
    tokens: &mut Vec<Token>,
) -> Result<()> {
    if depth >= MAX_PATH_DEPTH {
        return Ok(());
    }
    match node {
        // Nothing to hide, and a token would be a lie about the shape.
        Value::Null => Ok(()),
        Value::String(s) => {
            if s.is_empty() || Token::parse(s).is_some() {
                return Ok(());
            }
            let tok = mint(s, pointer.to_string())?;
            *s = tok.as_str().to_string();
            tokens.push(tok);
            Ok(())
        }
        Value::Bool(_) | Value::Number(_) => {
            // The model only ever sees text, so a numeric column becomes a
            // string token; restore puts the original digits back verbatim.
            let original = match node {
                Value::Bool(b) => b.to_string(),
                Value::Number(n) => n.to_string(),
                _ => unreachable!("guarded by the match arm"),
            };
            let tok = mint(&original, pointer.to_string())?;
            *node = Value::String(tok.as_str().to_string());
            tokens.push(tok);
            Ok(())
        }
        Value::Array(arr) => {
            for (idx, child) in arr.iter_mut().enumerate() {
                let child_ptr = crate::rules::json_path::push_token(pointer, &idx.to_string());
                tokenize_node(child, &child_ptr, exclude_keys, depth + 1, mint, tokens)?;
            }
            Ok(())
        }
        Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                if exclude_keys.iter().any(|e| e == key) {
                    continue;
                }
                let child_ptr = crate::rules::json_path::push_token(pointer, key);
                tokenize_node(child, &child_ptr, exclude_keys, depth + 1, mint, tokens)?;
            }
            Ok(())
        }
    }
}

/// Recursive in-place walk over every string leaf of a `serde_json::Value`.
///
/// Lives here rather than in the MCP layer so both the structured and the
/// text pass traverse identically — a second traversal implementation is how
/// one pass ends up missing a leaf the other covers.
pub(crate) fn walk_strings(v: &mut Value, f: &mut dyn FnMut(&mut String)) {
    match v {
        Value::String(s) => f(s),
        Value::Array(arr) => {
            for x in arr.iter_mut() {
                walk_strings(x, f);
            }
        }
        Value::Object(map) => {
            for x in map.values_mut() {
                walk_strings(x, f);
            }
        }
        _ => {}
    }
}

/// Byte ranges of well-formed `<REDACT:CAT:hash>` tokens already present in
/// `text`. Uses the same scan as [`rewrite_tokens`]; only spans that actually
/// parse as a token are protected, since a malformed look-alike is not
/// vault-backed and re-redacting it costs nothing.
fn token_spans(text: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut offset = 0usize;
    let mut rest = text;
    while let Some(start) = rest.find(TOKEN_PREFIX) {
        let from = &rest[start..];
        let Some(end_rel) = from.find(TOKEN_SUFFIX) else {
            break;
        };
        let len = end_rel + TOKEN_SUFFIX.len();
        if Token::parse(&from[..len]).is_some() {
            spans.push((offset + start, offset + start + len));
        }
        offset += start + len;
        rest = &from[len..];
    }
    spans
}

fn overlaps_any(start: usize, end: usize, spans: &[(usize, usize)]) -> bool {
    spans.iter().any(|(s, e)| start < *e && *s < end)
}

fn caller_label(c: &Caller) -> String {
    if c.is_owner {
        format!("owner:{}", c.agent_id)
    } else if c.scopes.is_empty() {
        format!("agent:{}", c.agent_id)
    } else {
        format!("agent:{}({})", c.agent_id, c.scopes.join(","))
    }
}

fn source_meta(source: &Source) -> (&'static str, Option<String>) {
    match source {
        Source::UserChannelInput { channel_id } => ("user_channel_input", Some(channel_id.clone())),
        Source::ToolResult { tool_name } => ("tool_result", Some(tool_name.clone())),
        Source::SystemPrompt { component } => ("system_prompt", Some(component.clone())),
        Source::SubAgentReply { agent_id } => ("sub_agent_reply", Some(agent_id.clone())),
        Source::CronContext => ("cron_context", None),
    }
}

fn expired_placeholder(expires_at: i64) -> String {
    let dt = DateTime::<Utc>::from_timestamp(expires_at, 0)
        .unwrap_or_else(Utc::now);
    format!("[已過期 PII · {}]", dt.format("%Y-%m-%d"))
}

/// Silence unused-import warning when restore branches don't use the scope.
#[allow(dead_code)]
fn _scope_pin(_s: &RestoreScope) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::NullAuditSink;
    use crate::config::SourcePolicy;
    use crate::rules::{RestoreScope, RuleKind, RuleSpec};
    use std::path::PathBuf;
    use tempfile::TempDir;

    use std::sync::Mutex;

    /// Audit sink that records every event for assertion in tests.
    #[derive(Default)]
    struct RecordingAuditSink {
        events: Mutex<Vec<AuditEvent>>,
    }

    impl AuditSink for RecordingAuditSink {
        fn emit(&self, event: AuditEvent) {
            self.events.lock().unwrap().push(event);
        }
    }

    fn email_rule(priority: i32) -> RuleSpec {
        RuleSpec {
            id: "email".into(),
            category: "EMAIL".into(),
            restore_scope: RestoreScope::Owner,
            priority,
            cross_session_stable: false,
            apply_to_system_prompt: false,
            enabled: true,
            kind: RuleKind::Regex {
                pattern: r"[\w.+-]+@[\w-]+\.[\w.-]+".into(),
            },
        }
    }

    fn codename_rule() -> RuleSpec {
        RuleSpec {
            id: "codename".into(),
            category: "CODENAME".into(),
            restore_scope: RestoreScope::Owner,
            priority: 80,
            cross_session_stable: true,
            apply_to_system_prompt: false,
            enabled: true,
            kind: RuleKind::Regex { pattern: r"Project Falcon".into() },
        }
    }

    fn build_pipeline(rules: Vec<RuleSpec>, session: Option<&str>) -> (RedactionPipeline, TempDir) {
        let tmp = TempDir::new().unwrap();
        let key_dir: PathBuf = tmp.path().to_path_buf();
        let vault = Arc::new(VaultStore::in_memory(key_dir.clone()).unwrap());
        let engine = Arc::new(RuleEngine::from_specs(rules).unwrap());
        let audit: Arc<dyn AuditSink> = Arc::new(NullAuditSink);

        // Use deterministic key bytes for tests.
        let agent_key = [7u8; 32];
        let pipeline = RedactionPipeline::new(
            engine,
            vault,
            audit,
            "agnes",
            session.map(|s| s.to_string()),
            &agent_key,
            SourcePolicy::default(),
            24,
        );
        (pipeline, tmp)
    }

    #[test]
    fn tool_result_round_trip() {
        let (p, _t) = build_pipeline(vec![email_rule(50)], Some("s1"));
        let out = p
            .redact(
                "contact alice@acme.com",
                &Source::ToolResult { tool_name: "odoo.search".into() },
            )
            .unwrap();
        assert_ne!(out.redacted_text, "contact alice@acme.com");
        assert_eq!(out.tokens_written.len(), 1);

        let restored = p
            .restore(&out.redacted_text, &Caller::owner("agnes"), RestoreTarget::UserChannel)
            .unwrap();
        assert_eq!(restored, "contact alice@acme.com");
    }

    #[test]
    fn user_input_passes_through_by_default() {
        let (p, _t) = build_pipeline(vec![email_rule(50)], Some("s1"));
        let out = p
            .redact(
                "send mail to alice@acme.com",
                &Source::UserChannelInput { channel_id: "line".into() },
            )
            .unwrap();
        assert_eq!(out.redacted_text, "send mail to alice@acme.com");
        assert!(out.tokens_written.is_empty());
    }

    #[test]
    fn same_value_same_token_within_session() {
        let (p, _t) = build_pipeline(vec![email_rule(50)], Some("s1"));
        let a = p
            .redact("alice@acme.com", &Source::ToolResult { tool_name: "x".into() })
            .unwrap();
        let b = p
            .redact("alice@acme.com", &Source::ToolResult { tool_name: "y".into() })
            .unwrap();
        assert_eq!(a.tokens_written[0], b.tokens_written[0]);
    }

    #[test]
    fn different_sessions_different_tokens() {
        let (p1, _t1) = build_pipeline(vec![email_rule(50)], Some("sess-A"));
        let (p2, _t2) = build_pipeline(vec![email_rule(50)], Some("sess-B"));
        let a = p1
            .redact("alice@acme.com", &Source::ToolResult { tool_name: "x".into() })
            .unwrap();
        let b = p2
            .redact("alice@acme.com", &Source::ToolResult { tool_name: "x".into() })
            .unwrap();
        assert_ne!(a.tokens_written[0], b.tokens_written[0]);
    }

    #[test]
    fn cross_session_stable_rule_same_token_across_sessions() {
        // Use a SHARED vault + engine so the two pipelines share state.
        let tmp = TempDir::new().unwrap();
        let vault = Arc::new(VaultStore::in_memory(tmp.path().to_path_buf()).unwrap());
        let engine = Arc::new(RuleEngine::from_specs(vec![codename_rule()]).unwrap());
        let audit: Arc<dyn AuditSink> = Arc::new(NullAuditSink);
        let agent_key = [9u8; 32];
        let p1 = RedactionPipeline::new(
            engine.clone(),
            vault.clone(),
            audit.clone(),
            "agnes",
            Some("sess-A".into()),
            &agent_key,
            SourcePolicy::default(),
            24,
        );
        let p2 = RedactionPipeline::new(
            engine,
            vault.clone(),
            audit,
            "agnes",
            Some("sess-B".into()),
            &agent_key,
            SourcePolicy::default(),
            24,
        );
        let a = p1
            .redact("Project Falcon", &Source::ToolResult { tool_name: "x".into() })
            .unwrap();
        let b = p2
            .redact("Project Falcon", &Source::ToolResult { tool_name: "x".into() })
            .unwrap();
        assert_eq!(a.tokens_written[0], b.tokens_written[0]);

        // p2 can also restore p1's token (cross_session stable in vault).
        let restored = p2
            .restore(&a.redacted_text, &Caller::owner("agnes"), RestoreTarget::UserChannel)
            .unwrap();
        assert_eq!(restored, "Project Falcon");
    }

    #[test]
    fn hallucinated_token_stays_in_place() {
        let (p, _t) = build_pipeline(vec![email_rule(50)], Some("s1"));
        let out = p
            .restore(
                "ping <REDACT:EMAIL:deadbeefdeadbeefdeadbeefdeadbeef> please",
                &Caller::owner("agnes"),
                RestoreTarget::UserChannel,
            )
            .unwrap();
        assert!(out.contains("<REDACT:EMAIL:deadbeefdeadbeefdeadbeefdeadbeef>"));
    }

    #[test]
    fn audit_log_target_does_not_decrypt() {
        let (p, _t) = build_pipeline(vec![email_rule(50)], Some("s1"));
        let red = p
            .redact("alice@acme.com", &Source::ToolResult { tool_name: "x".into() })
            .unwrap();
        let out = p
            .restore(&red.redacted_text, &Caller::owner("agnes"), RestoreTarget::AuditLog)
            .unwrap();
        assert_eq!(out, red.redacted_text);
        assert!(!out.contains("alice@acme.com"));
    }

    #[test]
    fn per_source_category_filter_narrows_redaction() {
        use crate::config::SourceSetting;

        // Two rules: EMAIL + a keyword rule categorised TW_MOBILE.
        let mobile_rule = RuleSpec {
            id: "mobile".into(),
            category: "TW_MOBILE".into(),
            restore_scope: RestoreScope::Owner,
            priority: 50,
            cross_session_stable: false,
            apply_to_system_prompt: false,
            enabled: true,
            kind: RuleKind::Regex { pattern: r"09\d{8}".into() },
        };
        let text = "mail alice@acme.com phone 0912345678";
        let src = Source::ToolResult { tool_name: "odoo.search".into() };

        // only_categories = TW_MOBILE ⇒ email survives, phone tokenised.
        let policy = SourcePolicy {
            tool_results: SourceSetting {
                mode: SourceMode::On,
                only_categories: vec!["TW_MOBILE".into()],
                exclude_categories: vec![],
            },
            ..SourcePolicy::default()
        };
        let (p, _t) = build_pipeline_with_policy(
            vec![email_rule(50), mobile_rule.clone()],
            Some("s1"),
            policy,
        );
        let out = p.redact(text, &src).unwrap();
        assert!(out.redacted_text.contains("alice@acme.com"), "{}", out.redacted_text);
        assert!(!out.redacted_text.contains("0912345678"), "{}", out.redacted_text);
        assert_eq!(out.tokens_written.len(), 1);

        // exclude_categories = TW_MOBILE ⇒ phone survives, email tokenised.
        let policy = SourcePolicy {
            tool_results: SourceSetting {
                mode: SourceMode::On,
                only_categories: vec![],
                exclude_categories: vec!["TW_MOBILE".into()],
            },
            ..SourcePolicy::default()
        };
        let (p, _t) =
            build_pipeline_with_policy(vec![email_rule(50), mobile_rule], Some("s1"), policy);
        let out = p.redact(text, &src).unwrap();
        assert!(!out.redacted_text.contains("alice@acme.com"), "{}", out.redacted_text);
        assert!(out.redacted_text.contains("0912345678"), "{}", out.redacted_text);
    }

    fn build_pipeline_with_policy(
        rules: Vec<RuleSpec>,
        session: Option<&str>,
        policy: SourcePolicy,
    ) -> (RedactionPipeline, TempDir) {
        let tmp = TempDir::new().unwrap();
        let key_dir: PathBuf = tmp.path().to_path_buf();
        let vault = Arc::new(VaultStore::in_memory(key_dir.clone()).unwrap());
        let engine = Arc::new(RuleEngine::from_specs(rules).unwrap());
        let audit: Arc<dyn AuditSink> = Arc::new(NullAuditSink);
        let agent_key = [7u8; 32];
        let pipeline = RedactionPipeline::new(
            engine,
            vault,
            audit,
            "agnes",
            session.map(|s| s.to_string()),
            &agent_key,
            policy,
            24,
        );
        (pipeline, tmp)
    }

    #[test]
    fn selective_does_not_redact_non_system_prompt_source() {
        use crate::config::SourceMode;
        // tool_results = Selective: must NOT force a full redact for a
        // tool-result source (Selective only applies to system-prompt).
        let policy = SourcePolicy {
            tool_results: SourceMode::Selective.into(),
            ..SourcePolicy::default()
        };
        let (p, _t) = build_pipeline_with_policy(vec![email_rule(50)], Some("s1"), policy);
        let out = p
            .redact(
                "contact alice@acme.com",
                &Source::ToolResult { tool_name: "x".into() },
            )
            .unwrap();
        assert_eq!(out.redacted_text, "contact alice@acme.com");
        assert!(out.tokens_written.is_empty());
    }

    #[test]
    fn selective_redacts_opted_in_rule_on_system_prompt() {
        use crate::config::SourceMode;
        // system_prompt = Selective + an opted-in rule must still redact.
        let mut rule = email_rule(50);
        rule.apply_to_system_prompt = true;
        let policy = SourcePolicy {
            system_prompt: SourceMode::Selective.into(),
            ..SourcePolicy::default()
        };
        let (p, _t) = build_pipeline_with_policy(vec![rule], Some("s1"), policy);
        let out = p
            .redact(
                "soul says alice@acme.com",
                &Source::SystemPrompt { component: "soul".into() },
            )
            .unwrap();
        assert_ne!(out.redacted_text, "soul says alice@acme.com");
        assert_eq!(out.tokens_written.len(), 1);
    }

    #[test]
    fn expired_token_does_not_emit_restore_ok() {
        // Build the pieces directly so we can hold the vault + recording sink.
        let tmp = TempDir::new().unwrap();
        let vault = Arc::new(VaultStore::in_memory(tmp.path().to_path_buf()).unwrap());
        let engine = Arc::new(RuleEngine::from_specs(vec![email_rule(50)]).unwrap());
        let sink = Arc::new(RecordingAuditSink::default());
        let audit: Arc<dyn AuditSink> = sink.clone();
        let agent_key = [7u8; 32];
        let p = RedactionPipeline::new(
            engine,
            vault.clone(),
            audit,
            "agnes",
            Some("s1".into()),
            &agent_key,
            SourcePolicy::default(),
            24,
        );

        // Insert a mapping that is already expired (negative TTL ⇒ expires_at <= now).
        let hash = token::session_hash(&[0u8; 32], b"alice@acme.com");
        let tok = Token::new("EMAIL", &hash).unwrap();
        vault
            .insert_mapping(
                tok.as_str(),
                "alice@acme.com",
                "agnes",
                Some("s1"),
                "EMAIL",
                "email",
                &RestoreScope::Owner,
                false,
                -1,
            )
            .unwrap();

        let text = format!("contact {} please", tok.as_str());
        let restored = p
            .restore(&text, &Caller::owner("agnes"), RestoreTarget::UserChannel)
            .unwrap();

        // Placeholder is rendered, cleartext is NOT revealed.
        assert!(restored.contains("已過期 PII"));
        assert!(!restored.contains("alice@acme.com"));

        // No RestoreOk event for an expired token — it must not inflate the
        // PII-reveal count. A RestoreMiss is emitted instead.
        let events = sink.events.lock().unwrap();
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AuditEvent::RestoreOk { .. })),
            "expired restore must not emit RestoreOk"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AuditEvent::RestoreMiss { .. })),
            "expired restore should emit a non-reveal event"
        );
    }

    // ── structured field rules (`redact_value`) ──────────────────────────

    /// A `db_field`-style rule bound to `odoo_search` for one model.
    fn partner_name_rule() -> RuleSpec {
        RuleSpec {
            id: "customer_master".into(),
            category: "DB_FIELD".into(),
            restore_scope: RestoreScope::Owner,
            priority: 70,
            cross_session_stable: false,
            apply_to_system_prompt: false,
            enabled: true,
            kind: RuleKind::JsonPath {
                paths: vec!["$[*].name".into(), "$.name".into()],
                match_tool: Some("odoo_search".into()),
                match_args: [("model".to_string(), "res.partner".to_string())]
                    .into_iter()
                    .collect(),
                match_result: Default::default(),
                exclude_keys: vec![],
            },
        }
    }

    fn json_path_rule(id: &str, paths: &[&str], tool: Option<&str>, exclude: &[&str]) -> RuleSpec {
        RuleSpec {
            id: id.into(),
            category: "DB_FIELD".into(),
            restore_scope: RestoreScope::Owner,
            priority: 70,
            cross_session_stable: false,
            apply_to_system_prompt: false,
            enabled: true,
            kind: RuleKind::JsonPath {
                paths: paths.iter().map(|s| s.to_string()).collect(),
                match_tool: tool.map(|s| s.to_string()),
                match_args: std::collections::HashMap::new(),
                match_result: Default::default(),
                exclude_keys: exclude.iter().map(|s| s.to_string()).collect(),
            },
        }
    }

    fn partner_args() -> serde_json::Value {
        serde_json::json!({"model": "res.partner", "limit": "20"})
    }

    /// Three fictional partner rows in the shape `odoo_search` returns.
    fn partner_rows() -> serde_json::Value {
        serde_json::json!([
            {"id": 41, "name": "王小明", "email": "ming@example.test", "credit_limit": 50000},
            {"id": 42, "name": "陳美玲", "email": "meiling@example.test", "credit_limit": 0},
        ])
    }

    #[test]
    fn redact_value_tokenises_a_native_record_array() {
        let (p, _t) = build_pipeline(vec![partner_name_rule()], Some("s1"));
        let mut value = partner_rows();
        let args = partner_args();
        let ctx = ToolContext { tool_name: "odoo_search", args: Some(&args) };

        let tokens = p.redact_value(&mut value, &ctx).unwrap();
        assert_eq!(tokens.len(), 2, "one token per row name");

        // Names are whole-value tokens; ids survive untouched.
        assert!(value[0]["name"].as_str().unwrap().starts_with("<REDACT:DB_FIELD:"));
        assert!(value[1]["name"].as_str().unwrap().starts_with("<REDACT:DB_FIELD:"));
        assert_eq!(value[0]["id"], serde_json::json!(41));
        let rendered = serde_json::to_string(&value).unwrap();
        assert!(!rendered.contains("王小明"), "{rendered}");
        assert!(!rendered.contains("陳美玲"), "{rendered}");

        // Owner restore returns the originals.
        let restored = p
            .restore(&rendered, &Caller::owner("agnes"), RestoreTarget::UserChannel)
            .unwrap();
        assert!(restored.contains("王小明"), "{restored}");
        assert!(restored.contains("陳美玲"), "{restored}");
    }

    #[test]
    fn redact_value_reaches_into_json_in_text_leaves() {
        let (p, _t) = build_pipeline(vec![partner_name_rule()], Some("s1"));
        let args = partner_args();
        let ctx = ToolContext { tool_name: "odoo_search", args: Some(&args) };

        // Pretty-printed payload (what the Odoo tools actually emit) stays pretty.
        let pretty = serde_json::to_string_pretty(&partner_rows()).unwrap();
        let mut wrapped = serde_json::json!({"content": [{"type": "text", "text": pretty}]});
        let tokens = p.redact_value(&mut wrapped, &ctx).unwrap();
        assert_eq!(tokens.len(), 2);
        let out = wrapped["content"][0]["text"].as_str().unwrap();
        assert!(out.contains('\n'), "pretty input must stay pretty: {out}");
        assert!(!out.contains("王小明"), "{out}");
        assert!(out.contains("<REDACT:DB_FIELD:"), "{out}");

        // Compact payload stays compact.
        let compact = serde_json::to_string(&partner_rows()).unwrap();
        let mut wrapped = serde_json::json!({"content": [{"type": "text", "text": compact}]});
        let tokens = p.redact_value(&mut wrapped, &ctx).unwrap();
        assert_eq!(tokens.len(), 2);
        let out = wrapped["content"][0]["text"].as_str().unwrap();
        assert!(!out.contains('\n'), "compact input must stay compact: {out}");
        assert!(!out.contains("王小明"), "{out}");
    }

    #[test]
    fn redact_value_tool_glob_hits_and_misses() {
        let (p, _t) = build_pipeline(
            vec![json_path_rule("f", &["$[*].name"], Some("odoo_*"), &[])],
            Some("s1"),
        );

        let mut hit = partner_rows();
        let tokens = p
            .redact_value(&mut hit, &ToolContext::new("odoo_partner_search"))
            .unwrap();
        assert_eq!(tokens.len(), 2);

        let mut miss = partner_rows();
        let tokens = p
            .redact_value(&mut miss, &ToolContext::new("memory_search"))
            .unwrap();
        assert!(tokens.is_empty());
        assert_eq!(miss[0]["name"], serde_json::json!("王小明"));
    }

    #[test]
    fn redact_value_arg_gate_blocks_a_different_model() {
        let (p, _t) = build_pipeline(vec![partner_name_rule()], Some("s1"));

        let wrong = serde_json::json!({"model": "crm.lead"});
        let mut value = partner_rows();
        let tokens = p
            .redact_value(
                &mut value,
                &ToolContext { tool_name: "odoo_search", args: Some(&wrong) },
            )
            .unwrap();
        assert!(tokens.is_empty(), "wrong model must not fire the field rule");
        assert_eq!(value[0]["name"], serde_json::json!("王小明"));

        // Args missing altogether: the gate is unsatisfiable, so the rule
        // stays out (the unconditional text pass is the remaining defence).
        let mut value = partner_rows();
        let tokens = p
            .redact_value(&mut value, &ToolContext::new("odoo_search"))
            .unwrap();
        assert!(tokens.is_empty());
    }

    #[test]
    fn redact_value_tokenises_numbers_as_whole_values() {
        let (p, _t) = build_pipeline(
            vec![json_path_rule("credit", &["$[*].credit_limit"], None, &[])],
            Some("s1"),
        );
        let mut value = partner_rows();
        let tokens = p
            .redact_value(&mut value, &ToolContext::new("odoo_search"))
            .unwrap();
        assert_eq!(tokens.len(), 2);
        // The number became a string token; restore gives the digits back.
        let tok = value[0]["credit_limit"].as_str().unwrap().to_string();
        assert!(tok.starts_with("<REDACT:DB_FIELD:"));
        let restored = p
            .restore(&tok, &Caller::owner("agnes"), RestoreTarget::UserChannel)
            .unwrap();
        assert_eq!(restored, "50000");
    }

    #[test]
    fn redact_value_null_and_empty_string_are_skipped() {
        let (p, _t) = build_pipeline(
            vec![json_path_rule("f", &["$[*].note"], None, &[])],
            Some("s1"),
        );
        let mut value = serde_json::json!([{"note": null}, {"note": ""}]);
        let tokens = p
            .redact_value(&mut value, &ToolContext::new("odoo_search"))
            .unwrap();
        assert!(tokens.is_empty());
        assert_eq!(value, serde_json::json!([{"note": null}, {"note": ""}]));
    }

    #[test]
    fn redact_value_wildcard_node_preserves_excluded_keys() {
        let (p, _t) = build_pipeline(
            vec![json_path_rule("whole", &["$[*]"], None, &["id"])],
            Some("s1"),
        );
        let mut value = partner_rows();
        let tokens = p
            .redact_value(&mut value, &ToolContext::new("odoo_search"))
            .unwrap();
        // name + email + credit_limit per row; id excluded.
        assert_eq!(tokens.len(), 6);
        assert_eq!(value[0]["id"], serde_json::json!(41));
        assert_eq!(value[1]["id"], serde_json::json!(42));
        for row in value.as_array().unwrap() {
            for key in ["name", "email", "credit_limit"] {
                assert!(
                    row[key].as_str().unwrap().starts_with("<REDACT:"),
                    "{key} should be tokenised: {row}"
                );
            }
        }
    }

    /// A `db_field` wildcard rule: `res.partner.*` over the whole record.
    ///
    /// Handed to the engine unexpanded, exactly as a config file would — the
    /// engine expands it internally (after id dedup), so the three bound tools
    /// keep the operator's single rule id without colliding.
    fn partner_wildcard_rules() -> Vec<RuleSpec> {
        vec![RuleSpec {
            id: "customer_all".into(),
            category: "DB_FIELD".into(),
            restore_scope: RestoreScope::Owner,
            priority: 70,
            cross_session_stable: false,
            apply_to_system_prompt: false,
            enabled: true,
            kind: RuleKind::DbField {
                source: Some("odoo".into()),
                connector: None,
                fields: vec!["res.partner.*".into()],
            },
        }]
    }

    fn wildcard_records() -> serde_json::Value {
        serde_json::json!([
            {"id": 1001, "name": "王大福", "email": "dafu@example.invalid"},
            {"id": 1002, "name": "陳美玲", "email": "meiling@example.invalid"},
        ])
    }

    #[test]
    fn wildcard_rule_never_tokenises_the_mcp_envelope() {
        // Regression: `model.*` expands to paths ["$[*]", "$"]. On the OUTER
        // value `$` resolves to the envelope root, which used to swallow the
        // whole payload as one token and mask `content[0].type` ("text") too.
        let (p, _t) = build_pipeline(partner_wildcard_rules(), Some("s1"));
        let args = partner_args();
        let ctx = ToolContext { tool_name: "odoo_search", args: Some(&args) };

        let records = wildcard_records();
        let pretty = serde_json::to_string_pretty(&records).unwrap();
        let mut value = serde_json::json!({
            "content": [{"type": "text", "text": pretty}]
        });

        let tokens = p.redact_value(&mut value, &ctx).unwrap();

        // The envelope survives verbatim.
        assert_eq!(
            value["content"][0]["type"],
            serde_json::json!("text"),
            "the envelope's `type` must not be tokenised"
        );
        assert!(
            value["content"][0]["text"].is_string(),
            "the envelope shape must be intact"
        );
        let text = value["content"][0]["text"].as_str().unwrap();
        assert!(
            Token::parse(text.trim()).is_none(),
            "the whole payload must not collapse into one token: {text}"
        );

        // The embedded payload is still real JSON, with the records masked.
        let inner: serde_json::Value = serde_json::from_str(text).unwrap();
        let rows = inner.as_array().expect("embedded payload stays an array");
        assert_eq!(rows.len(), 2);
        for (idx, row) in rows.iter().enumerate() {
            for key in ["name", "email"] {
                assert!(
                    row[key].as_str().is_some_and(|v| v.starts_with("<REDACT:")),
                    "row {idx} key {key} should be a token: {row}"
                );
            }
        }
        // `id` is the documented wildcard exclusion — verbatim, still numeric.
        assert_eq!(rows[0]["id"], serde_json::json!(1001));
        assert_eq!(rows[1]["id"], serde_json::json!(1002));

        // One token per non-id scalar leaf: 2 rows x {name, email}.
        assert_eq!(tokens.len(), 4, "tokens: {tokens:?}");

        // No token anywhere outside the embedded JSON.
        let mut outside: Vec<String> = Vec::new();
        walk_strings(&mut value, &mut |leaf| {
            if serde_json::from_str::<serde_json::Value>(leaf)
                .is_ok_and(|v| v.is_object() || v.is_array())
            {
                return; // the embedded payload itself
            }
            if leaf.contains(TOKEN_PREFIX) {
                outside.push(leaf.clone());
            }
        });
        assert!(outside.is_empty(), "tokens leaked into the envelope: {outside:?}");
    }

    #[test]
    fn wildcard_rule_still_covers_a_bare_record_array() {
        // The root skip must not disable array-of-records handling: on a raw
        // value (no envelope) the `$[*]` path still selects each record.
        let (p, _t) = build_pipeline(partner_wildcard_rules(), Some("s1"));
        let args = partner_args();
        let ctx = ToolContext { tool_name: "odoo_search", args: Some(&args) };

        let mut value = wildcard_records();
        let tokens = p.redact_value(&mut value, &ctx).unwrap();

        assert_eq!(tokens.len(), 4, "tokens: {tokens:?}");
        for row in value.as_array().unwrap() {
            for key in ["name", "email"] {
                assert!(
                    row[key].as_str().is_some_and(|v| v.starts_with("<REDACT:")),
                    "{key} should be tokenised: {row}"
                );
            }
        }
        assert_eq!(value[0]["id"], serde_json::json!(1001));
        assert_eq!(value[1]["id"], serde_json::json!(1002));
    }

    #[test]
    fn redact_value_honours_category_filters() {
        use crate::config::SourceSetting;

        // exclude_categories = DB_FIELD ⇒ the field rule is inert.
        let policy = SourcePolicy {
            tool_results: SourceSetting {
                mode: SourceMode::On,
                only_categories: vec![],
                exclude_categories: vec!["DB_FIELD".into()],
            },
            ..SourcePolicy::default()
        };
        let (p, _t) =
            build_pipeline_with_policy(vec![partner_name_rule(), email_rule(50)], Some("s1"), policy);
        let args = partner_args();
        let ctx = ToolContext { tool_name: "odoo_search", args: Some(&args) };
        let mut value = partner_rows();
        let tokens = p.redact_value(&mut value, &ctx).unwrap();
        assert_eq!(value[0]["name"], serde_json::json!("王小明"));
        // The email regex still fires (different category).
        assert!(!tokens.is_empty());
        assert!(value[0]["email"].as_str().unwrap().starts_with("<REDACT:EMAIL:"));

        // only_categories = DB_FIELD ⇒ the reverse.
        let policy = SourcePolicy {
            tool_results: SourceSetting {
                mode: SourceMode::On,
                only_categories: vec!["DB_FIELD".into()],
                exclude_categories: vec![],
            },
            ..SourcePolicy::default()
        };
        let (p, _t) =
            build_pipeline_with_policy(vec![partner_name_rule(), email_rule(50)], Some("s1"), policy);
        let mut value = partner_rows();
        let _ = p.redact_value(&mut value, &ctx).unwrap();
        assert!(value[0]["name"].as_str().unwrap().starts_with("<REDACT:DB_FIELD:"));
        assert_eq!(value[0]["email"], serde_json::json!("ming@example.test"));
    }

    #[test]
    fn redact_value_off_mode_leaves_the_value_untouched() {
        let policy = SourcePolicy {
            tool_results: SourceMode::Off.into(),
            ..SourcePolicy::default()
        };
        let (p, _t) =
            build_pipeline_with_policy(vec![partner_name_rule(), email_rule(50)], Some("s1"), policy);
        let args = partner_args();
        let before = partner_rows();
        let mut value = before.clone();
        let tokens = p
            .redact_value(
                &mut value,
                &ToolContext { tool_name: "odoo_search", args: Some(&args) },
            )
            .unwrap();
        assert!(tokens.is_empty());
        assert_eq!(value, before);
    }

    /// A `duduclaw_files`-style rule: same tool for every file, bound to one
    /// table by what the RESULT says, with a CJK header in the quoted form.
    fn csv_name_rule(table: &str, column_path: &str) -> RuleSpec {
        RuleSpec {
            id: "file_columns".into(),
            category: "DB_FIELD".into(),
            restore_scope: RestoreScope::Owner,
            priority: 70,
            cross_session_stable: false,
            apply_to_system_prompt: false,
            enabled: true,
            kind: RuleKind::JsonPath {
                paths: vec![column_path.to_string()],
                match_tool: Some("csv_read".into()),
                match_args: std::collections::HashMap::new(),
                match_result: [("/table".to_string(), table.to_string())]
                    .into_iter()
                    .collect(),
                exclude_keys: vec![],
            },
        }
    }

    /// `csv_read`'s shape, pretty-printed into the MCP envelope.
    fn csv_envelope(table: &str, rows: serde_json::Value) -> serde_json::Value {
        let payload = serde_json::json!({
            "path": format!("/data/{table}"),
            "table": table,
            "columns": ["id", "name", "email"],
            "rows": rows,
            "row_count": 1,
            "truncated": false,
        });
        serde_json::json!({
            "content": [{
                "type": "text",
                "text": serde_json::to_string_pretty(&payload).unwrap(),
            }]
        })
    }

    #[test]
    fn result_gate_fires_on_the_embedded_payload_that_names_the_table() {
        let (p, _t) = build_pipeline(
            vec![csv_name_rule("customers.csv", "$.rows[*].name"), email_rule(50)],
            Some("s1"),
        );
        let mut value = csv_envelope(
            "customers.csv",
            serde_json::json!([{"id": 1, "name": "王大福", "email": "dafu@example.invalid"}]),
        );
        let tokens = p
            .redact_value(&mut value, &ToolContext::new("csv_read"))
            .unwrap();
        assert_eq!(tokens.len(), 2, "1 name (field rule) + 1 email (regex)");

        let text = value["content"][0]["text"].as_str().unwrap();
        let inner: serde_json::Value = serde_json::from_str(text).unwrap();
        assert!(
            inner["rows"][0]["name"]
                .as_str()
                .unwrap()
                .starts_with("<REDACT:DB_FIELD:")
        );
        assert!(
            inner["rows"][0]["email"]
                .as_str()
                .unwrap()
                .starts_with("<REDACT:EMAIL:")
        );
        // The gate's own field, the envelope and the id all survive.
        assert_eq!(inner["table"], serde_json::json!("customers.csv"));
        assert_eq!(inner["rows"][0]["id"], serde_json::json!(1));
        assert_eq!(value["content"][0]["type"], serde_json::json!("text"));
    }

    #[test]
    fn result_gate_blocks_a_different_file_from_the_same_tool() {
        // Same tool name, same shape, different `/table` → the field rule must
        // not fire; the unconditional text pass still catches the email.
        let (p, _t) = build_pipeline(
            vec![csv_name_rule("customers.csv", "$.rows[*].name"), email_rule(50)],
            Some("s1"),
        );
        let mut value = csv_envelope(
            "suppliers.csv",
            serde_json::json!([{"id": 9, "name": "王大福", "email": "dafu@example.invalid"}]),
        );
        let tokens = p
            .redact_value(&mut value, &ToolContext::new("csv_read"))
            .unwrap();
        assert_eq!(tokens.len(), 1, "only the email: {tokens:?}");

        let text = value["content"][0]["text"].as_str().unwrap();
        let inner: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(inner["rows"][0]["name"], serde_json::json!("王大福"));
        assert!(
            inner["rows"][0]["email"]
                .as_str()
                .unwrap()
                .starts_with("<REDACT:EMAIL:")
        );
    }

    #[test]
    fn result_gate_masks_a_cjk_header_through_the_quoted_path() {
        let (p, _t) = build_pipeline(
            vec![csv_name_rule("客戶清單.xlsx", "$.rows[*]['地址']")],
            Some("s1"),
        );
        let mut value = csv_envelope(
            "客戶清單.xlsx",
            serde_json::json!([{"id": 3, "地址": "臺北市中正區虛構路 100 號"}]),
        );
        let tokens = p
            .redact_value(&mut value, &ToolContext::new("csv_read"))
            .unwrap();
        assert_eq!(tokens.len(), 1, "{tokens:?}");

        let text = value["content"][0]["text"].as_str().unwrap();
        assert!(!text.contains("臺北市中正區虛構路 100 號"), "{text}");
        let inner: serde_json::Value = serde_json::from_str(text).unwrap();
        assert!(
            inner["rows"][0]["地址"]
                .as_str()
                .unwrap()
                .starts_with("<REDACT:DB_FIELD:")
        );
        assert_eq!(inner["rows"][0]["id"], serde_json::json!(3));
    }

    #[test]
    fn redact_value_text_pass_still_covers_unnamed_columns() {
        // Only `name` has a field rule; the email column is caught by regex.
        let (p, _t) = build_pipeline(vec![partner_name_rule(), email_rule(50)], Some("s1"));
        let args = partner_args();
        let mut value = partner_rows();
        let tokens = p
            .redact_value(
                &mut value,
                &ToolContext { tool_name: "odoo_search", args: Some(&args) },
            )
            .unwrap();
        assert_eq!(tokens.len(), 4, "2 names + 2 emails");
        assert!(value[0]["name"].as_str().unwrap().starts_with("<REDACT:DB_FIELD:"));
        assert!(value[0]["email"].as_str().unwrap().starts_with("<REDACT:EMAIL:"));
    }

    #[test]
    fn existing_tokens_are_never_redacted_twice() {
        // A hash can hold eight consecutive digits, which an 8-digit business
        // id rule would otherwise match *inside* the token.
        let biz_id = RuleSpec {
            id: "tw_biz".into(),
            category: "TW_BIZ_ID".into(),
            restore_scope: RestoreScope::Owner,
            priority: 50,
            cross_session_stable: false,
            apply_to_system_prompt: false,
            enabled: true,
            kind: RuleKind::Regex { pattern: r"\d{8}".into() },
        };
        let (p, _t) = build_pipeline(vec![biz_id], Some("s1"));
        let src = Source::ToolResult { tool_name: "odoo_search".into() };

        let text = "客戶 <REDACT:DB_FIELD:a1234567891bcdefa1234567891bcdef> 已建檔";
        let out = p.redact(text, &src).unwrap();
        assert!(
            out.tokens_written.is_empty(),
            "a token must not be re-tokenised: {}",
            out.redacted_text
        );
        assert_eq!(out.redacted_text, text);

        // Control: the same rule still fires on a real 8-digit run.
        let out = p.redact("統編 12345678 已登記", &src).unwrap();
        assert_eq!(out.tokens_written.len(), 1);

        // And a malformed look-alike is not protected (not vault-backed).
        let out = p.redact("<REDACT:X:12345678>", &src).unwrap();
        assert_eq!(out.tokens_written.len(), 1);
    }

    #[test]
    fn structured_hit_is_audited_with_its_json_pointer() {
        let tmp = TempDir::new().unwrap();
        let vault = Arc::new(VaultStore::in_memory(tmp.path().to_path_buf()).unwrap());
        let engine = Arc::new(RuleEngine::from_specs(vec![partner_name_rule()]).unwrap());
        let sink = Arc::new(RecordingAuditSink::default());
        let audit: Arc<dyn AuditSink> = sink.clone();
        let p = RedactionPipeline::new(
            engine,
            vault,
            audit,
            "agnes",
            Some("s1".into()),
            &[7u8; 32],
            SourcePolicy::default(),
            24,
        );

        let args = partner_args();
        let mut value = partner_rows();
        p.redact_value(
            &mut value,
            &ToolContext { tool_name: "odoo_search", args: Some(&args) },
        )
        .unwrap();

        let events = sink.events.lock().unwrap();
        let paths: Vec<Option<String>> = events
            .iter()
            .filter_map(|e| match e {
                AuditEvent::Redact { path, rule_id, .. } if rule_id == "customer_master" => {
                    Some(path.clone())
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            paths,
            vec![Some("/0/name".to_string()), Some("/1/name".to_string())]
        );
    }

    #[test]
    fn redact_value_is_err_when_the_vault_cannot_write() {
        // An unsafe agent id makes per-agent key loading (and therefore every
        // vault insert) fail — the fail-closed path the MCP layer relies on.
        let tmp = TempDir::new().unwrap();
        let vault = Arc::new(VaultStore::in_memory(tmp.path().to_path_buf()).unwrap());
        let engine = Arc::new(RuleEngine::from_specs(vec![partner_name_rule()]).unwrap());
        let audit: Arc<dyn AuditSink> = Arc::new(NullAuditSink);
        let p = RedactionPipeline::new(
            engine,
            vault,
            audit,
            "../escape",
            Some("s1".into()),
            &[7u8; 32],
            SourcePolicy::default(),
            24,
        );

        let args = partner_args();
        let mut value = partner_rows();
        let res = p.redact_value(
            &mut value,
            &ToolContext { tool_name: "odoo_search", args: Some(&args) },
        );
        assert!(res.is_err(), "a vault write failure must fail the whole call");
    }

    #[test]
    fn token_spans_finds_only_well_formed_tokens() {
        let good = "<REDACT:EMAIL:a1234567891bcdefa1234567891bcdef>";
        assert_eq!(token_spans(good), vec![(0, good.len())]);
        assert!(token_spans("no tokens here").is_empty());
        assert!(token_spans("<REDACT:EMAIL:short>").is_empty());
        assert!(token_spans("<REDACT:EMAIL:unterminated").is_empty());
        let two = format!("a {good} b {good}");
        assert_eq!(token_spans(&two).len(), 2);
    }

    #[test]
    fn walk_strings_visits_every_leaf() {
        let mut v = serde_json::json!({"a": "hello", "b": ["world", {"c": "deep"}], "n": 1});
        let mut seen: Vec<String> = Vec::new();
        walk_strings(&mut v, &mut |s| seen.push(s.clone()));
        seen.sort();
        assert_eq!(seen, vec!["deep", "hello", "world"]);
    }

    #[test]
    fn restore_denied_for_caller_without_scope() {
        let mut rule = email_rule(50);
        rule.restore_scope = RestoreScope::AnyScope { scope: "CustomerRead".into() };

        let (p, _t) = build_pipeline(vec![rule], Some("s1"));
        let red = p
            .redact("alice@acme.com", &Source::ToolResult { tool_name: "x".into() })
            .unwrap();
        let outsider = Caller::agent("other", vec!["NoSuchScope".into()]);
        let out = p
            .restore(&red.redacted_text, &outsider, RestoreTarget::SubAgent { agent_id: "x".into() })
            .unwrap();
        assert!(!out.contains("alice@acme.com"));
        assert!(out.contains("<REDACT:"));
    }
}
