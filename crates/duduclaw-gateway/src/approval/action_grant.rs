//! P7 (2026-10-08) — single-use, payload-bound approvals for employee tool
//! calls (`ActionGrant`).
//!
//! The operator command-line gate binds an approval to action + target +
//! request digest and consumes it once. This module applies the same idea to
//! one employee tool call that a person must approve:
//!
//! * the request stores a binding (`payload.action_grant`): employee, tool,
//!   effect class, a SHA-256 digest over `(agent, tool, canonical arguments)`
//!   and a short human-readable summary built from argument keys and safe
//!   scalar values (secret-looking keys and values masked with the same
//!   helpers `tool_calls.jsonl` uses; nested values shown only as counts);
//! * on approval the stored binding must match the call that is about to
//!   run (same employee, tool and digest), and the approval is consumed
//!   (`approved → invalidated`, reason `consumed:action_grant:<uuid>`) before
//!   the call proceeds. A second use, a changed row or a lost claim refuses.
//!
//! Which calls are bound ([`binding_applies`]): tools whose effect class is
//! `send`, `publish` or `purchase`, and every call that reached human approval because
//! an `[capabilities] action_rules` rule said `ask`. Everything else keeps
//! the previous behaviour (a fresh request per call, not consumed).

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use super::{ApprovalBroker, ApprovalId, ApprovalStatus};

/// Payload key the binding is stored under.
pub const ACTION_GRANT_KEY: &str = "action_grant";
/// Audit / invalidation reason prefix of a consumed grant.
pub const CONSUMED_PREFIX: &str = "consumed:action_grant:";

const MAX_SUMMARY_KEYS: usize = 8;
const MAX_VALUE_CHARS: usize = 60;
const MAX_KEY_CHARS: usize = 40;

/// The binding one approval covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionGrant {
    pub agent_id: String,
    pub tool: String,
    pub effect: duduclaw_core::ToolEffect,
    pub args_digest: String,
    /// `key = value` lines, already masked and capped.
    pub args_summary: Vec<String>,
}

/// Whether a call that needs a person's approval must carry a binding.
pub fn binding_applies(tool: &str, routed_by_action_rule_ask: bool) -> bool {
    use duduclaw_core::ToolEffect;
    routed_by_action_rule_ask
        || matches!(
            duduclaw_core::effect_of(tool),
            ToolEffect::Send | ToolEffect::Publish | ToolEffect::Purchase
        )
}

/// Egress requests bind even tools ordinarily classified as read or draft.
pub fn binding_applies_for_request(
    tool: &str,
    routed_by_action_rule_ask: bool,
    routed_by_egress_guard: bool,
) -> bool {
    routed_by_egress_guard || binding_applies(tool, routed_by_action_rule_ask)
}

/// Canonical JSON: object keys sorted at every depth, no whitespace.
pub fn canonical_json(v: &Value) -> String {
    fn canon(v: &Value) -> Value {
        match v {
            Value::Object(m) => {
                let mut keys: Vec<&String> = m.keys().collect();
                keys.sort();
                let mut out = Map::with_capacity(m.len());
                for k in keys {
                    out.insert(k.clone(), canon(&m[k]));
                }
                Value::Object(out)
            }
            Value::Array(a) => Value::Array(a.iter().map(canon).collect()),
            other => other.clone(),
        }
    }
    // `serde_json::Map` preserves insertion order only with the
    // `preserve_order` feature; without it the map is a BTreeMap and already
    // sorted. Either way the rebuilt value serializes in sorted order.
    canon(v).to_string()
}

/// Digest over `(agent, tool, canonical arguments)`.
pub fn args_digest(agent_id: &str, tool: &str, args: &Value) -> String {
    let body = serde_json::json!([agent_id, tool, canonical_json(args)]).to_string();
    hex::encode(Sha256::digest(body.as_bytes()))
}

fn key_shaped(k: &str) -> bool {
    !k.is_empty()
        && k.chars().count() <= MAX_KEY_CHARS
        && k.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn render_value(key: &str, v: &Value) -> String {
    // Key-based masking first (api_key, token, password, …).
    let probe = serde_json::json!({ key: v });
    let masked = duduclaw_security::audit::mask_sensitive_json(&probe);
    let v = masked.get(key).unwrap_or(&Value::Null);
    match v {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => {
            let flat: String = s
                .chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect();
            let masked = duduclaw_security::audit::mask_sensitive_text(&flat);
            let n = masked.chars().count();
            let cut = duduclaw_core::truncate_chars(&masked, MAX_VALUE_CHARS);
            if n > MAX_VALUE_CHARS {
                format!("\"{cut}…\" ({n} chars)")
            } else {
                format!("\"{cut}\"")
            }
        }
        Value::Array(a) => format!("[{} items]", a.len()),
        Value::Object(o) => format!("{{{} fields}}", o.len()),
    }
}

/// Human-readable argument summary (never raw secrets; nested values as
/// counts; at most [`MAX_SUMMARY_KEYS`] lines plus a "+N more" line).
pub fn summarize_args(args: &Value) -> Vec<String> {
    let Some(obj) = args.as_object() else {
        return if args.is_null() {
            Vec::new()
        } else {
            vec!["(non-object arguments)".into()]
        };
    };
    let mut keys: Vec<&String> = obj.keys().collect();
    keys.sort();
    let mut out = Vec::new();
    let mut hidden = 0usize;
    for k in keys {
        if !key_shaped(k) || out.len() >= MAX_SUMMARY_KEYS {
            hidden += 1;
            continue;
        }
        out.push(format!("{k} = {}", render_value(k, &obj[k])));
    }
    if hidden > 0 {
        out.push(format!("(+{hidden} more)"));
    }
    out
}

impl ActionGrant {
    /// Build the binding for one call. `payload` is the MCP `tools/call`
    /// params (`{name, arguments}`); bare arguments are accepted too.
    pub fn build(agent_id: &str, tool: &str, payload: &Value) -> Self {
        let args = payload.get("arguments").unwrap_or(&Value::Null);
        Self {
            agent_id: agent_id.to_string(),
            tool: tool.to_string(),
            effect: duduclaw_core::tool_effect::effect_of_call(tool, args),
            args_digest: args_digest(agent_id, tool, args),
            args_summary: summarize_args(args),
        }
    }

    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "agent_id": self.agent_id,
            "tool": self.tool,
            "effect": self.effect.as_str(),
            "args_digest": self.args_digest,
            "args_summary": self.args_summary,
            "single_use": true,
        })
    }

    /// Card text: what exactly this approval covers.
    pub fn card_text(&self) -> String {
        let mut s = format!(
            "此核准只涵蓋這一次呼叫（用過即失效）：\n工具：{}（{} 類動作）\n",
            self.tool,
            self.effect.as_str()
        );
        if self.args_summary.is_empty() {
            s.push_str("參數：（無）");
        } else {
            s.push_str("參數：\n");
            for line in &self.args_summary {
                s.push_str("  ");
                s.push_str(line);
                s.push('\n');
            }
        }
        s.push_str(&format!("\n參數指紋：{}", &self.args_digest[..16]));
        s
    }

    /// Whether a stored binding is exactly this one.
    pub fn matches_stored(&self, stored: &Value) -> bool {
        stored.get("agent_id").and_then(Value::as_str) == Some(self.agent_id.as_str())
            && stored.get("tool").and_then(Value::as_str) == Some(self.tool.as_str())
            && stored.get("args_digest").and_then(Value::as_str) == Some(self.args_digest.as_str())
    }
}

/// Why an approved request could not be used for this call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrantRefusal {
    /// The row is gone, unreadable, or not approved any more.
    NotApproved,
    /// The stored binding is missing or covers a different call.
    Mismatch,
    /// Someone else consumed it first (or it was used already).
    AlreadyUsed,
    Store(String),
}

/// Check that approval `id` covers exactly `grant`, then consume it once.
pub async fn verify_and_consume(
    broker: &ApprovalBroker,
    id: &ApprovalId,
    grant: &ActionGrant,
) -> Result<(), GrantRefusal> {
    let rec = broker
        .get(id)
        .await
        .map_err(GrantRefusal::Store)?
        .ok_or(GrantRefusal::NotApproved)?;
    if rec.status != ApprovalStatus::Approved {
        return Err(GrantRefusal::NotApproved);
    }
    if rec.agent_id != grant.agent_id {
        return Err(GrantRefusal::Mismatch);
    }
    let stored = rec
        .payload
        .get(ACTION_GRANT_KEY)
        .ok_or(GrantRefusal::Mismatch)?;
    if !grant.matches_stored(stored) {
        return Err(GrantRefusal::Mismatch);
    }
    // Bound mail decisions retain the execution snapshot through the actual
    // single-use claim, including current requester authority after approval.
    if rec.payload["name"] == "mail_send" && rec.payload.get("egress_guard").is_some() {
        if let Some(binding) = rec.binding.as_ref().filter(|binding|
            binding.run_origin_kind == "ingress" && binding.resume_handler == "live_only_v1") {
            let home = broker.home_dir().ok_or_else(|| GrantRefusal::Store("missing policy home".into()))?;
            binding.validate(&rec.payload).map_err(GrantRefusal::Store)?;
            binding.validate_host_files().map_err(GrantRefusal::Store)?;
            broker.validate_task_snapshot(binding).await.map_err(GrantRefusal::Store)?;
            if super::policy_revision(&home, &rec.agent_id).map_err(GrantRefusal::Store)? != binding.policy_revision
                || super::mail_requester::environment_hash(&home, &rec.agent_id, &binding.run_id)
                    .map_err(GrantRefusal::Store)? != binding.environment_hash {
                return Err(GrantRefusal::Store("policy changed; request a new approval".into()));
            }
            if !rec.decided_by.as_deref().is_some_and(|who| who.strip_prefix("dashboard:").is_some())
                && !super::mail_requester::eligible(&home, &rec) {
                return Err(GrantRefusal::Store("mail requester authority changed".into()));
            }
        }
    }
    let reason = format!("{CONSUMED_PREFIX}{}", uuid::Uuid::new_v4());
    let mut conn = broker.store.conn.lock().await;
    let mail_binding = (rec.payload["name"] == "mail_send" && rec.payload.get("egress_guard").is_some())
        .then_some(rec.binding.as_ref()).flatten().filter(|binding|
            binding.run_origin_kind == "ingress" && binding.resume_handler == "live_only_v1");
    if let Some(binding) = mail_binding {
        broker.attach_tasks(&conn, binding).map_err(GrantRefusal::Store)?;
    }
    // Live-I/O authority is re-verified around the IMMEDIATE transaction.
    let tx = super::begin_live_write(&mut conn).map_err(GrantRefusal::Store)?;
    if let Some(binding) = mail_binding {
        ApprovalBroker::check_task(&tx, binding).map_err(GrantRefusal::Store)?;
    }
    if !rec.expires_at().is_some_and(|expiry| chrono::Utc::now() < expiry) {
        return Err(GrantRefusal::NotApproved);
    }
    // SR5 round 3 N1/N2: the card's employee incarnation, inside this
    // IMMEDIATE transaction (same rule as `consume_approved`). A card of
    // another incarnation, or a NULL card of a protected employee, is
    // invalidated and refused.
    let stored: Option<Option<i64>> = rusqlite::OptionalExtension::optional(tx.query_row(
        "SELECT employee_incarnation FROM approvals WHERE id=?1 AND status='approved'",
        rusqlite::params![id.as_str()], |row| row.get(0),
    )).map_err(|e| GrantRefusal::Store(e.to_string()))?;
    let Some(stored) = stored else { return Err(GrantRefusal::AlreadyUsed) };
    let home = broker.store.db_path.as_deref().and_then(std::path::Path::parent);
    if let Err((reason, refusal)) = super::store::card_consumable(home, &rec.agent_id, stored) {
        tx.execute(
            "UPDATE approvals SET status='invalidated', invalidated_reason=?1 WHERE id=?2 AND status='approved'",
            rusqlite::params![reason, id.as_str()],
        ).map_err(|e| GrantRefusal::Store(e.to_string()))?;
        super::commit_live_write(tx).map_err(GrantRefusal::Store)?;
        return Err(GrantRefusal::Store(refusal));
    }
    // Bind consumption to the row just verified, including its deadline.
    // Check the deadline in the UPDATE too, after any SQLite lock wait.
    let consumed = tx.execute(
        &format!("UPDATE approvals SET status='invalidated', invalidated_reason=?1
         WHERE id=?2 AND status='approved' AND agent_id=?3
           AND payload=?4 AND created_at=?5 AND ttl_seconds=?6
           AND binding_json IS ?7 AND {live_origin}
           AND (binding_json IS NULL OR julianday(json_extract(binding_json,'$.expires_at')) > julianday('now'))
           AND julianday(created_at) + ttl_seconds / 86400.0 > julianday('now')",
            live_origin = super::turn_origin::LIVE_ORIGIN_SQL),
        rusqlite::params![reason, id.as_str(), rec.agent_id, rec.payload.to_string(), rec.created_at, rec.ttl_seconds,
            rec.binding.as_ref().map(|binding| serde_json::to_string(binding).expect("serializable binding"))],
    ).map_err(|e| GrantRefusal::Store(e.to_string()))?;
    match consumed {
        1 => super::commit_live_write(tx).map_err(GrantRefusal::Store),
        _ => Err(GrantRefusal::AlreadyUsed),
    }
}

#[cfg(test)]
mod action_grant_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn digest_is_canonical_and_payload_bound() {
        let a = json!({"to": "a@x", "amount": 3, "meta": {"b": 1, "a": 2}});
        let b = json!({"meta": {"a": 2, "b": 1}, "amount": 3, "to": "a@x"});
        assert_eq!(
            args_digest("e", "mail_send", &a),
            args_digest("e", "mail_send", &b)
        );
        assert_ne!(
            args_digest("e", "mail_send", &a),
            args_digest(
                "e",
                "mail_send",
                &json!({"to": "b@x", "amount": 3, "meta": {"a": 2, "b": 1}})
            )
        );
        assert_ne!(
            args_digest("e", "mail_send", &a),
            args_digest("f", "mail_send", &a)
        );
        assert_ne!(
            args_digest("e", "mail_send", &a),
            args_digest("e", "odoo_sale_confirm", &a)
        );
    }

    #[test]
    fn summary_masks_secrets_and_counts_nested_values() {
        let s = summarize_args(&json!({
            "to": "alice@example.com",
            "api_key": "sk-ant-abcdefghijklmnop",
            "note": "token sk-ant-zzzzzzzzzzzzzzzzzzzz inside",
            "lines": [1, 2, 3],
            "order": {"id": 7},
            "qty": 2,
            "bad key!": "x",
        }));
        let joined = s.join("\n");
        assert!(joined.contains("to = \"alice@example.com\""), "{joined}");
        assert!(joined.contains("api_key = \"***\""), "{joined}");
        assert!(!joined.contains("abcdefghijklmnop"), "{joined}");
        assert!(!joined.contains("zzzzzzzzzzzzzzzzzzzz"), "{joined}");
        assert!(joined.contains("lines = [3 items]"), "{joined}");
        assert!(joined.contains("order = {1 fields}"), "{joined}");
        assert!(joined.contains("qty = 2"), "{joined}");
        assert!(joined.contains("(+1 more)"), "{joined}");
    }

    #[test]
    fn sends_publications_purchases_rules_and_egress_requests_are_bound() {
        assert!(binding_applies("mail_send", false));
        assert!(binding_applies("wiki_share", false));
        assert!(binding_applies_for_request("web_extract", false, true));
        assert!(!binding_applies_for_request("web_extract", false, false));
        assert!(binding_applies("odoo_sale_confirm", false));
        assert!(!binding_applies("memory_search", false));
        assert!(binding_applies("memory_search", true));
    }

    #[tokio::test]
    async fn approved_but_expired_or_malformed_deadline_cannot_be_consumed() {
        let dir = tempfile::tempdir().unwrap();
        let broker = ApprovalBroker::open(dir.path()).unwrap();
        for created_at in ["2000-01-01T00:00:00Z", "not-a-date"] {
            let mut payload = json!({"arguments":{"url":"https://example.invalid/private"}});
            let grant = ActionGrant::build("alice", "web_extract", &payload);
            payload[ACTION_GRANT_KEY] = grant.to_json();
            let id = broker.request("alice", "mcp_call", "egress", payload, 60).await.unwrap();
            broker.decide(&id, true, "dashboard:test").await.unwrap();
            broker.store.conn.lock().await.execute(
                "UPDATE approvals SET created_at=?1 WHERE id=?2",
                rusqlite::params![created_at, id.as_str()],
            ).unwrap();
            assert_eq!(verify_and_consume(&broker, &id, &grant).await, Err(GrantRefusal::NotApproved));
        }
    }

    #[tokio::test]
    async fn an_approval_covers_exactly_one_matching_call() {
        let dir = tempfile::tempdir().unwrap();
        let broker = ApprovalBroker::open(dir.path()).unwrap();
        let payload = json!({"name": "mail_send", "arguments": {"to": "a@x", "body": "hi"}});
        let grant = ActionGrant::build("alice", "mail_send", &payload);
        let mut stored = payload.clone();
        stored[ACTION_GRANT_KEY] = grant.to_json();
        let id = broker
            .request("alice", "mcp_call", "s", stored, 60)
            .await
            .unwrap();
        // Pending ⇒ not usable.
        assert_eq!(
            verify_and_consume(&broker, &id, &grant).await,
            Err(GrantRefusal::NotApproved)
        );
        broker.decide(&id, true, "dashboard:test").await.unwrap();
        // A different payload does not match the stored binding.
        let other = ActionGrant::build(
            "alice",
            "mail_send",
            &json!({"arguments": {"to": "b@x", "body": "hi"}}),
        );
        assert_eq!(
            verify_and_consume(&broker, &id, &other).await,
            Err(GrantRefusal::Mismatch)
        );
        // The matching call consumes it; a second use is refused.
        assert_eq!(verify_and_consume(&broker, &id, &grant).await, Ok(()));
        assert_eq!(
            verify_and_consume(&broker, &id, &grant).await,
            Err(GrantRefusal::NotApproved)
        );
        let rec = broker.get(&id).await.unwrap().unwrap();
        assert_eq!(rec.status, ApprovalStatus::Invalidated);
    }
}
