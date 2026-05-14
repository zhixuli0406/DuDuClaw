//! High-level redact / restore API.
//!
//! `RedactionPipeline` ties together [`RuleEngine`], [`VaultStore`], and an
//! [`AuditSink`]. One pipeline instance is bound to a single
//! `(agent_id, session_id)` pair; create a fresh pipeline per conversation
//! via [`PipelineFactory`] so per-session salts isolate token spaces.

use std::sync::Arc;

use chrono::{DateTime, Utc};

use crate::audit::{AuditEvent, AuditSink};
use crate::config::{SourceMode, SourcePolicy};
use crate::engine::RuleEngine;
use crate::error::{RedactionError, Result};
use crate::rules::RestoreScope;
use crate::source::{Caller, RestoreTarget, Source};
use crate::token::{self, Token, TOKEN_PREFIX, TOKEN_SUFFIX};
use crate::vault::VaultStore;

/// Result of a redact pass.
#[derive(Debug, Clone)]
pub struct RedactionOutput {
    /// Text with PII spans replaced by tokens.
    pub redacted_text: String,
    /// Distinct tokens inserted in this pass (in original-source order).
    pub tokens_written: Vec<Token>,
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
    fn mode_for(&self, source: &Source) -> SourceMode {
        match source {
            Source::UserChannelInput { .. } => self.source_policy.user_input,
            Source::ToolResult { .. } => self.source_policy.tool_results,
            Source::SystemPrompt { .. } => self.source_policy.system_prompt,
            Source::SubAgentReply { .. } => self.source_policy.sub_agent,
            Source::CronContext => self.source_policy.cron_context,
        }
    }

    /// Redact text. Returns the rewritten text and the list of new tokens
    /// inserted into the vault. **Fail-closed**: if any insert fails, the
    /// entire call returns `Err` and the caller MUST drop the LLM request.
    pub fn redact(&self, text: &str, source: &Source) -> Result<RedactionOutput> {
        match self.mode_for(source) {
            SourceMode::Off | SourceMode::Inherit => {
                return Ok(RedactionOutput {
                    redacted_text: text.to_string(),
                    tokens_written: Vec::new(),
                });
            }
            SourceMode::On | SourceMode::Selective => {}
        }

        let matches = self.engine.apply(text, source);
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

            // Compute token (per-session unless rule is cross-session-stable).
            let salt = if m.rule.cross_session_stable() {
                &self.stable_salt[..]
            } else {
                &self.session_salt[..]
            };
            let hash = token::session_hash(salt, m.span.original.as_bytes());
            let tok = Token::new(m.rule.category(), &hash)?;

            // Persist mapping (fail-closed).
            let session_for_vault = if m.rule.cross_session_stable() {
                None
            } else {
                self.session_id.as_deref()
            };

            self.vault.insert_mapping(
                tok.as_str(),
                &m.span.original,
                &self.agent_id,
                session_for_vault,
                m.rule.category(),
                m.rule.id(),
                m.rule.restore_scope(),
                m.rule.cross_session_stable(),
                self.vault_ttl_hours,
            )?;

            // Audit.
            let (source_category, source_detail) = source_meta(source);
            self.audit.emit(AuditEvent::Redact {
                agent_id: self.agent_id.clone(),
                session_id: self.session_id.clone(),
                source_category: source_category.into(),
                source_detail,
                rule_id: m.rule.id().into(),
                category: m.rule.category().into(),
                token: tok.as_str().into(),
            });

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
                            let _ = vault.record_reveal(tok.as_str());
                            audit.emit(AuditEvent::RestoreOk {
                                agent_id: agent_id.clone(),
                                caller: caller_label(caller),
                                target: target_str.clone(),
                                token: tok.as_str().into(),
                            });
                            TokenAction::Replace(plain)
                        }
                        None => {
                            let placeholder = expired_placeholder(entry.expires_at);
                            audit.emit(AuditEvent::RestoreOk {
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

    fn email_rule(priority: i32) -> RuleSpec {
        RuleSpec {
            id: "email".into(),
            category: "EMAIL".into(),
            restore_scope: RestoreScope::Owner,
            priority,
            cross_session_stable: false,
            apply_to_system_prompt: false,
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
                "ping <REDACT:EMAIL:deadbeef> please",
                &Caller::owner("agnes"),
                RestoreTarget::UserChannel,
            )
            .unwrap();
        assert!(out.contains("<REDACT:EMAIL:deadbeef>"));
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
