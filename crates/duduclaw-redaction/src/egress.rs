//! Tool egress policy — whitelist-only restoration with default deny.
//!
//! When the LLM emits a tool call whose arguments contain `<REDACT:...>`
//! tokens, we MUST decide:
//!
//! 1. **Restore** — the tool needs real values to function (send_email,
//!    odoo.write). Replace tokens with real values, then execute.
//! 2. **Passthrough** — the tool doesn't need real values (e.g. an
//!    internal `log_event` tool that should never see plaintext PII).
//! 3. **Deny** — the tool is external or otherwise untrusted; refuse to
//!    execute. This is the default for any tool not present in the
//!    whitelist.
//!
//! Hallucinated tokens (well-formed but not in vault) also trigger Deny,
//! to prevent the LLM from constructing a poisoned tool call that smuggles
//! arbitrary `<REDACT:...>` text through an authorised channel.

use std::collections::HashMap;

use serde_json::Value;

use crate::audit::{AuditEvent, AuditSink};
use crate::config::{RestoreArgsMode, ToolEgressRule};
use crate::error::Result;
use crate::token::Token;
use crate::vault::VaultStore;

/// Outcome of an egress decision.
#[derive(Debug, Clone)]
pub enum EgressDecision {
    /// Tool may run. `args` has had its tokens replaced with real values.
    Allow {
        args: Value,
        tokens_restored: usize,
    },
    /// Tool may run with the original args (tokens left as placeholders).
    Passthrough(Value),
    /// Tool must not run.
    Deny {
        reason: String,
        tokens_seen: usize,
    },
}

/// Decides what to do with a tool call. Owns the rule table; the caller
/// owns the vault + audit sink.
pub struct EgressEvaluator {
    rules: HashMap<String, ToolEgressRule>,
}

impl EgressEvaluator {
    pub fn new(rules: HashMap<String, ToolEgressRule>) -> Self {
        Self { rules }
    }

    /// Locate the matching egress rule for a tool name. Exact match wins
    /// over wildcard; among wildcards the longest prefix wins.
    fn find_rule(&self, tool_name: &str) -> Option<&ToolEgressRule> {
        if let Some(r) = self.rules.get(tool_name) {
            return Some(r);
        }
        let mut best: Option<(&str, &ToolEgressRule)> = None;
        for (pattern, rule) in &self.rules {
            if let Some(prefix) = pattern.strip_suffix('*')
                && tool_name.starts_with(prefix)
            {
                match best {
                    Some((cur, _)) if cur.len() >= prefix.len() => {}
                    _ => best = Some((prefix, rule)),
                }
            }
        }
        best.map(|(_, r)| r)
    }

    /// Make a decision for `(tool_name, args)`. `agent_id` and
    /// `session_id` are used for vault lookup (must match the redaction
    /// scope rules).
    pub fn decide(
        &self,
        tool_name: &str,
        args: &Value,
        agent_id: &str,
        session_id: Option<&str>,
        vault: &VaultStore,
        audit: &dyn AuditSink,
    ) -> Result<EgressDecision> {
        let tokens = collect_tokens(args);

        // Tool has no tokens: trivially allow (don't audit — quiet path).
        if tokens.is_empty() {
            return Ok(EgressDecision::Passthrough(args.clone()));
        }

        let Some(rule) = self.find_rule(tool_name) else {
            audit.emit(AuditEvent::EgressDeny {
                agent_id: agent_id.into(),
                tool: tool_name.into(),
                reason: "tool not in egress whitelist".into(),
                tokens_seen: tokens.len(),
            });
            return Ok(EgressDecision::Deny {
                reason: format!("tool '{tool_name}' is not on the egress whitelist"),
                tokens_seen: tokens.len(),
            });
        };

        match rule.restore_args {
            RestoreArgsMode::Deny => {
                audit.emit(AuditEvent::EgressDeny {
                    agent_id: agent_id.into(),
                    tool: tool_name.into(),
                    reason: "tool egress rule is 'deny'".into(),
                    tokens_seen: tokens.len(),
                });
                Ok(EgressDecision::Deny {
                    reason: format!("tool '{tool_name}' egress is 'deny'"),
                    tokens_seen: tokens.len(),
                })
            }
            RestoreArgsMode::Passthrough => Ok(EgressDecision::Passthrough(args.clone())),
            RestoreArgsMode::Restore => {
                let mut restored = args.clone();
                let mut count: usize = 0;
                let mut hallucinated = false;
                substitute_tokens(&mut restored, &mut |tok| {
                    match vault.lookup_mapping(tok.as_str(), agent_id, session_id) {
                        Ok(Some(entry)) => match entry.original {
                            Some(plain) => {
                                count += 1;
                                Some(plain)
                            }
                            None => {
                                hallucinated = true;
                                None
                            }
                        },
                        _ => {
                            hallucinated = true;
                            None
                        }
                    }
                });
                if hallucinated {
                    audit.emit(AuditEvent::EgressDeny {
                        agent_id: agent_id.into(),
                        tool: tool_name.into(),
                        reason: "args contain hallucinated or expired tokens".into(),
                        tokens_seen: tokens.len(),
                    });
                    return Ok(EgressDecision::Deny {
                        reason: format!(
                            "tool '{tool_name}' refused: args contain hallucinated or expired tokens"
                        ),
                        tokens_seen: tokens.len(),
                    });
                }
                if rule.audit_reveal {
                    audit.emit(AuditEvent::EgressAllow {
                        agent_id: agent_id.into(),
                        tool: tool_name.into(),
                        tokens_restored: count,
                    });
                }
                Ok(EgressDecision::Allow {
                    args: restored,
                    tokens_restored: count,
                })
            }
        }
    }
}

fn collect_tokens(v: &Value) -> Vec<Token> {
    fn walk(v: &Value, out: &mut Vec<Token>) {
        match v {
            Value::String(s) => {
                for t in extract_tokens_from_str(s) {
                    out.push(t);
                }
            }
            Value::Array(arr) => arr.iter().for_each(|x| walk(x, out)),
            Value::Object(map) => map.values().for_each(|x| walk(x, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(v, &mut out);
    out
}

/// Visit every `<REDACT:...>` substring in a string and yield parsed
/// [`Token`]s. Unparseable matches are ignored.
fn extract_tokens_from_str(s: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(start) = rest.find(crate::token::TOKEN_PREFIX) {
        let from = &rest[start..];
        let Some(end_rel) = from.find(crate::token::TOKEN_SUFFIX) else {
            break;
        };
        let candidate = &from[..end_rel + crate::token::TOKEN_SUFFIX.len()];
        if let Some(tok) = Token::parse(candidate) {
            out.push(tok);
        }
        rest = &from[end_rel + crate::token::TOKEN_SUFFIX.len()..];
    }
    out
}

/// In-place substitution. `replace_fn` is called for every parseable
/// token; returning `Some(plain)` replaces the token with `plain`,
/// returning `None` leaves it in place.
fn substitute_tokens(v: &mut Value, replace_fn: &mut dyn FnMut(&Token) -> Option<String>) {
    match v {
        Value::String(s) => {
            let new_s = replace_in_str(s, replace_fn);
            if new_s != *s {
                *s = new_s;
            }
        }
        Value::Array(arr) => {
            for x in arr.iter_mut() {
                substitute_tokens(x, replace_fn);
            }
        }
        Value::Object(map) => {
            for x in map.values_mut() {
                substitute_tokens(x, replace_fn);
            }
        }
        _ => {}
    }
}

fn replace_in_str(s: &str, replace_fn: &mut dyn FnMut(&Token) -> Option<String>) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find(crate::token::TOKEN_PREFIX) {
        out.push_str(&rest[..start]);
        let from = &rest[start..];
        let Some(end_rel) = from.find(crate::token::TOKEN_SUFFIX) else {
            out.push_str(from);
            return out;
        };
        let candidate = &from[..end_rel + crate::token::TOKEN_SUFFIX.len()];
        match Token::parse(candidate) {
            Some(tok) => match replace_fn(&tok) {
                Some(plain) => out.push_str(&plain),
                None => out.push_str(candidate),
            },
            None => out.push_str(candidate),
        }
        rest = &from[end_rel + crate::token::TOKEN_SUFFIX.len()..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::NullAuditSink;
    use crate::config::{RestoreArgsMode, ToolEgressRule};
    use crate::rules::RestoreScope;
    use serde_json::json;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn rule_restore() -> ToolEgressRule {
        ToolEgressRule { restore_args: RestoreArgsMode::Restore, audit_reveal: false }
    }
    fn rule_deny() -> ToolEgressRule {
        ToolEgressRule { restore_args: RestoreArgsMode::Deny, audit_reveal: false }
    }

    fn fresh_vault() -> (VaultStore, TempDir) {
        let tmp = TempDir::new().unwrap();
        let kdir: PathBuf = tmp.path().to_path_buf();
        let store = VaultStore::in_memory(kdir).unwrap();
        (store, tmp)
    }

    #[test]
    fn no_tokens_passthrough_quietly() {
        let mut rules = HashMap::new();
        rules.insert("anything".into(), rule_restore());
        let ev = EgressEvaluator::new(rules);
        let (vault, _t) = fresh_vault();
        let dec = ev
            .decide(
                "anything",
                &json!({"to": "alice@acme.com"}),
                "agnes",
                Some("s1"),
                &vault,
                &NullAuditSink,
            )
            .unwrap();
        assert!(matches!(dec, EgressDecision::Passthrough(_)));
    }

    #[test]
    fn unknown_tool_with_tokens_denied() {
        let ev = EgressEvaluator::new(HashMap::new());
        let (vault, _t) = fresh_vault();
        vault
            .insert_mapping(
                "<REDACT:EMAIL:abcdef01>",
                "alice@acme.com",
                "agnes",
                Some("s1"),
                "EMAIL",
                "email",
                &RestoreScope::Owner,
                false,
                24,
            )
            .unwrap();
        let dec = ev
            .decide(
                "web_fetch",
                &json!({"url": "<REDACT:EMAIL:abcdef01>"}),
                "agnes",
                Some("s1"),
                &vault,
                &NullAuditSink,
            )
            .unwrap();
        match dec {
            EgressDecision::Deny { tokens_seen, .. } => assert_eq!(tokens_seen, 1),
            other => panic!("expected Deny, got {other:?}"),
        }
    }

    #[test]
    fn whitelisted_tool_restores_args() {
        let mut rules = HashMap::new();
        rules.insert("send_email".into(), rule_restore());
        let ev = EgressEvaluator::new(rules);
        let (vault, _t) = fresh_vault();
        vault
            .insert_mapping(
                "<REDACT:EMAIL:abcdef01>",
                "alice@acme.com",
                "agnes",
                Some("s1"),
                "EMAIL",
                "email",
                &RestoreScope::Owner,
                false,
                24,
            )
            .unwrap();
        let dec = ev
            .decide(
                "send_email",
                &json!({"to": "<REDACT:EMAIL:abcdef01>", "body": "hi"}),
                "agnes",
                Some("s1"),
                &vault,
                &NullAuditSink,
            )
            .unwrap();
        match dec {
            EgressDecision::Allow { args, tokens_restored } => {
                assert_eq!(tokens_restored, 1);
                assert_eq!(args["to"], Value::String("alice@acme.com".into()));
                assert_eq!(args["body"], Value::String("hi".into()));
            }
            other => panic!("expected Allow, got {other:?}"),
        }
    }

    #[test]
    fn wildcard_pattern_matches() {
        let mut rules = HashMap::new();
        rules.insert("odoo.*".into(), rule_restore());
        let ev = EgressEvaluator::new(rules);
        let (vault, _t) = fresh_vault();
        vault
            .insert_mapping(
                "<REDACT:E:abcdef01>",
                "alice",
                "a",
                Some("s"),
                "E",
                "r",
                &RestoreScope::Owner,
                false,
                24,
            )
            .unwrap();
        let dec = ev
            .decide(
                "odoo.search_partner",
                &json!({"q": "<REDACT:E:abcdef01>"}),
                "a",
                Some("s"),
                &vault,
                &NullAuditSink,
            )
            .unwrap();
        assert!(matches!(dec, EgressDecision::Allow { .. }));
    }

    #[test]
    fn hallucinated_token_denied() {
        let mut rules = HashMap::new();
        rules.insert("send_email".into(), rule_restore());
        let ev = EgressEvaluator::new(rules);
        let (vault, _t) = fresh_vault();
        // Note: no insert — token is hallucinated.
        let dec = ev
            .decide(
                "send_email",
                &json!({"to": "<REDACT:EMAIL:11111111>"}),
                "a",
                Some("s"),
                &vault,
                &NullAuditSink,
            )
            .unwrap();
        assert!(matches!(dec, EgressDecision::Deny { .. }));
    }

    #[test]
    fn explicit_deny_rule_blocks() {
        let mut rules = HashMap::new();
        rules.insert("web_fetch".into(), rule_deny());
        let ev = EgressEvaluator::new(rules);
        let (vault, _t) = fresh_vault();
        vault
            .insert_mapping(
                "<REDACT:E:abcdef01>",
                "x",
                "a",
                Some("s"),
                "E",
                "r",
                &RestoreScope::Owner,
                false,
                24,
            )
            .unwrap();
        let dec = ev
            .decide(
                "web_fetch",
                &json!({"url": "<REDACT:E:abcdef01>"}),
                "a",
                Some("s"),
                &vault,
                &NullAuditSink,
            )
            .unwrap();
        assert!(matches!(dec, EgressDecision::Deny { .. }));
    }

    #[test]
    fn nested_json_tokens_restored() {
        let mut rules = HashMap::new();
        rules.insert("send_email".into(), rule_restore());
        let ev = EgressEvaluator::new(rules);
        let (vault, _t) = fresh_vault();
        vault
            .insert_mapping(
                "<REDACT:E:abcdef01>",
                "alice@acme.com",
                "a",
                Some("s"),
                "E",
                "r",
                &RestoreScope::Owner,
                false,
                24,
            )
            .unwrap();
        let args = json!({
            "to": ["<REDACT:E:abcdef01>"],
            "options": {"reply_to": "<REDACT:E:abcdef01>"}
        });
        let dec = ev.decide("send_email", &args, "a", Some("s"), &vault, &NullAuditSink).unwrap();
        match dec {
            EgressDecision::Allow { args, tokens_restored } => {
                assert_eq!(tokens_restored, 2);
                assert_eq!(args["to"][0], Value::String("alice@acme.com".into()));
                assert_eq!(args["options"]["reply_to"], Value::String("alice@acme.com".into()));
            }
            other => panic!("expected Allow, got {other:?}"),
        }
    }

    #[test]
    fn extract_tokens_from_str_works() {
        let toks = extract_tokens_from_str("a <REDACT:E:abcdef01> b <REDACT:F:11223344> c");
        assert_eq!(toks.len(), 2);
        let toks2 = extract_tokens_from_str("no tokens here");
        assert!(toks2.is_empty());
        let toks3 = extract_tokens_from_str("<REDACT:bad:abc> ok");
        assert!(toks3.is_empty());
    }
}

