//! Server-owned recipient identities and scheduled delivery admission.

use std::path::Path;
use duduclaw_auth::{AccessLevel, UserStatus};
use duduclaw_core::recipient_policy::{Decision, Mode, channel_recipient};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub mod origin;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecipientContext {
    pub source: Option<String>,
    /// Registered transport destination; this does not itself grant a source exemption.
    #[serde(default)]
    pub reply_channel: Option<String>,
    #[serde(default)]
    pub reply_sender: Option<String>,
    pub owners: Vec<String>,
    /// Opaque id of the gateway-held proof of the bound human behind
    /// `source` ([`origin::mint`]); the tuple itself stays in the gateway.
    /// A schedule may default to `source` only with this id; the gateway
    /// re-verifies its own record before every send. Minted only when the
    /// caller asks for a schedule context.
    #[serde(default)]
    pub origin_proof_id: Option<String>,
    /// The signed-in WebChat conversation this turn answers, already bound
    /// to the employee (session id + creation stamp, see
    /// [`crate::scheduled_webchat::bind_origin`]). Captured by the gateway at
    /// turn registration; never taken from the MCP process environment.
    #[serde(default)]
    pub webchat_origin: Option<String>,
    /// Opaque id of the gateway-held proof of `webchat_origin`
    /// ([`origin::mint_webchat`]), stored with a job that reports back there.
    #[serde(default)]
    pub webchat_origin_proof_id: Option<String>,
}

/// Only active users bound as owners contribute verified channel identities.
/// Proactive destinations are employee-editable and never establish ownership.
pub fn owners(home: &Path, agent: &str) -> Result<Vec<String>, String> {
    let Some(db) = crate::decision_notify::open_user_db(home)? else {
        return Ok(Vec::new());
    };
    let restricted = crate::machine_employee::requires_admin_for_id(home, agent);
    let mut recipients = Vec::new();
    for binding in db.get_agent_users(agent)? {
        if binding.access_level != AccessLevel::Owner { continue; }
        if !db.get_user(&binding.user_id)?.is_some_and(|user| user.status == UserStatus::Active
            && (!restricted || user.role == duduclaw_auth::UserRole::Admin)) {
            continue;
        }
        for identity in db.verified_dm_destinations_for_user(&binding.user_id)? {
            if let Some(recipient) = channel_recipient(&identity.channel, &identity.channel_user_id) {
                recipients.push(recipient);
            }
        }
    }
    recipients.sort();
    recipients.dedup();
    Ok(recipients)
}

/// The live turn context of `agent`'s turn `turn`. `mint_origin` (only the
/// schedule-creating tools ask for it) makes the gateway persist origin
/// proof records for this turn's source / WebChat conversation and return
/// their opaque ids; an ordinary recipient query never writes anything.
pub fn context(home: &Path, agent: &str, turn: &str, mint_origin: bool) -> Result<RecipientContext, String> {
    let source = crate::turn_context::source_conversation_for(agent, turn);
    let webchat_origin = crate::turn_context::webchat_origin_for(agent, turn)
        .and_then(|session| crate::scheduled_webchat::bind_origin(home, Some(&session), agent));
    let (origin_proof_id, webchat_origin_proof_id) = if mint_origin {
        (
            source.as_deref().and_then(|recipient| origin::mint(home, agent, turn, recipient)),
            webchat_origin.as_deref().and_then(|target| origin::mint_webchat(home, agent, turn, target)),
        )
    } else {
        (None, None)
    };
    Ok(RecipientContext {
        source,
        reply_channel: crate::turn_context::reply_channel_for(agent, turn),
        reply_sender: crate::turn_context::reply_sender_for(agent, turn),
        owners: owners(home, agent)?,
        origin_proof_id,
        webchat_origin,
        webchat_origin_proof_id,
    })
}

/// Third-party effects have no recipient adapter. Never trust a guessed `to`
/// argument or a server's name as proof that its destination is allowed.
/// All side effects may egress; only trusted read-only tools are exempt.
pub(crate) fn unadapted_effect(
    home: &Path, agent: &str, tool: &str, effect: duduclaw_core::ToolEffect, audit: bool,
) -> Option<Value> {
    if !effect.is_side_effecting() { return None; }
    let dir = crate::ephemeral::resolve_agent_dir(home, agent)
        .unwrap_or_else(|| home.join("agents").join(agent));
    let policy = duduclaw_core::recipient_policy::load(home, Some(&dir));
    match policy.evaluate(&[String::new()], None, &[]) {
        Decision::Allow => None,
        Decision::WouldAsk => {
            if audit {
                duduclaw_security::audit::append_tool_call_with_extras(home, agent, tool,
                    "recipient_would_ask: third-party effect has no recipient adapter", true,
                    &[("recipient_guard", json!("would_ask"))]);
            }
            None
        }
        Decision::Ask => Some(json!({"reason":"unresolved_third_party_recipient"})),
    }
}

/// Share the Admin-only egress envelope and its complete argument binding.
pub(crate) fn merge_unadapted_effect(
    home: &Path, agent: &str, tool: &str, effect: duduclaw_core::ToolEffect, egress: &mut Option<Value>,
) {
    if let Some(reason) = unadapted_effect(home, agent, tool, effect, true) {
        let envelope = egress.get_or_insert_with(|| json!({"reason":"recipient_guard", "sources":[]}));
        envelope["recipient_guard"] = reason;
    }
}

pub(crate) fn confirm_unadapted_effect(
    home: &Path, agent: &str, tool: &str, effect: duduclaw_core::ToolEffect, approved: Option<&Value>,
) -> Result<(), String> {
    if unadapted_effect(home, agent, tool, effect, false).is_some()
        && approved.and_then(|value| value.get("recipient_guard")).is_none()
    {
        return Err("第三方工具的收件對象無法確認，請重試並取得管理員核准。".into());
    }
    Ok(())
}

/// Resolve legacy scheduled aliases against the same live registry as runtime dispatch.
pub(crate) async fn scheduled_agent(
    registry: &std::sync::Arc<tokio::sync::RwLock<duduclaw_agent::registry::AgentRegistry>>,
    agent: &str,
) -> Result<String, String> {
    if agent != "default" { return Ok(agent.to_owned()); }
    registry.read().await.main_agent().map(|agent| agent.config.agent.name.clone())
        .ok_or_else(|| "排程的預設員工不存在，無法確認收件人政策。".into())
}

fn scheduled_decision(home: &Path, agent: &str, targets: &[String], source: Option<&str>) -> Result<Decision, String> {
    let dir = crate::ephemeral::resolve_agent_dir(home, agent)
        .unwrap_or_else(|| home.join("agents").join(agent));
    let policy = duduclaw_core::recipient_policy::load(home, Some(&dir));
    if policy.mode == Mode::Off && policy.valid { return Ok(Decision::Allow); }
    let owners = match owners(home, agent) {
        Ok(owners) => owners,
        Err(_) if policy.mode == Mode::Shadow && policy.valid => return Ok(Decision::WouldAsk),
        Err(error) => return Err(error),
    };
    if !policy.valid { return Err("無法讀取收件人防護設定，已暫停排程通知。".into()); }
    Ok(policy.evaluate(targets, source, &owners))
}

/// Re-read policy at fire time. Legacy scheduled jobs have no source chat.
/// A fresh approval covers this exact occurrence, destination and message only.
pub(crate) async fn admit_scheduled(
    home: &Path, agent: &str, targets: &[String], args: Value,
) -> Result<(), String> {
    admit_scheduled_from_origin(home, agent, StoredOrigin::NONE, targets, args).await
        .map_err(String::from)
}

/// The turn origin stored with a scheduled job: the destination (normalized
/// channel recipient, or the stamped WebChat target) and the opaque id of
/// the gateway-held [`origin::OriginProof`]. Only the id is trusted, and only
/// through [`origin::verify`], which reads the gateway's own record.
#[derive(Debug, Clone, Copy)]
pub(crate) struct StoredOrigin<'a> {
    pub recipient: Option<&'a str>,
    pub proof: Option<&'a str>,
}

impl StoredOrigin<'static> {
    /// A job with no stored turn origin.
    pub(crate) const NONE: Self = StoredOrigin { recipient: None, proof: None };
}

/// A refused scheduled delivery: a closed turn-origin denial (recorded as
/// such, never retried elsewhere) or any other admission failure.
#[derive(Debug)]
pub(crate) enum ScheduledRefusal {
    Origin(origin::OriginDenial),
    Other(String),
}

impl std::fmt::Display for ScheduledRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Origin(denial) => write!(f, "{denial}"),
            Self::Other(message) => f.write_str(message),
        }
    }
}

impl From<ScheduledRefusal> for String {
    fn from(refusal: ScheduledRefusal) -> Self {
        refusal.to_string()
    }
}

/// The source exemption this delivery may use. Only a destination equal to
/// the stored origin can use it, and only after [`origin::verify`] re-proved
/// the creator against the gateway's record and live state. When a target
/// IS the stored origin, a missing (legacy row) or failing proof refuses the
/// delivery outright — whatever `recipient_guard` says, including `off`: no
/// approval card, no downgrade into ordinary policy, no other conversation.
/// A job whose targets do not include its stored origin is unaffected here
/// and follows ordinary policy.
fn origin_source(
    home: &Path, agent: &str, stored: StoredOrigin<'_>, targets: &[String],
) -> Result<Option<String>, origin::OriginDenial> {
    let Some(recipient) = stored.recipient.filter(|recipient| !recipient.is_empty()) else {
        return Ok(None);
    };
    if !targets.iter().any(|target| target == recipient) {
        return Ok(None);
    }
    origin::verify(home, agent, recipient, stored.proof).map(|_| Some(recipient.to_owned()))
}

/// Fire-time admission of a scheduled notification. The source exemption is
/// derived only from the stored, re-verified turn origin ([`origin_source`]);
/// explicit other targets remain subject to ordinary recipient policy.
pub(crate) async fn admit_scheduled_from_origin(
    home: &Path, agent: &str, stored: StoredOrigin<'_>, targets: &[String], args: Value,
) -> Result<(), ScheduledRefusal> {
    let source = origin_source(home, agent, stored, targets).map_err(ScheduledRefusal::Origin)?;
    admit_with_source(home, agent, source.as_deref(), targets, args).await
        .map_err(ScheduledRefusal::Other)
}

/// Policy + approval for one occurrence, with an already re-verified source.
async fn admit_with_source(
    home: &Path, agent: &str, source: Option<&str>, targets: &[String], args: Value,
) -> Result<(), String> {
    let decision = scheduled_decision(home, agent, targets, source)?;
    if decision == Decision::Allow { return Ok(()); }
    if decision == Decision::WouldAsk {
        duduclaw_security::audit::append_tool_call_with_extras(home, agent, "scheduled_notification",
            "scheduled recipient is neither source conversation nor allowed recipient", true,
            &[("recipient_guard", json!("would_ask"))]);
        return Ok(());
    }
    use crate::approval::{ApprovalBroker, ApprovalStatus, action_grant::verify_and_consume};
    let broker = ApprovalBroker::open(home)?;
    let (payload, grant) = scheduled_request(agent, targets, args);
    let id = broker.request(agent, "mcp_call", "排程通知的對象不是來源對話也不在允許清單；請確認完整收件人與內容。",
        payload, 300).await?;
    if broker.await_decision(&id, std::time::Duration::from_secs(2)).await? != ApprovalStatus::Approved {
        return Err(format!("排程通知未獲管理員核准（審批編號 {id}）。"));
    }
    // A malformed or unreadable policy cannot be overridden by an approval.
    scheduled_decision(home, agent, targets, source)?;
    verify_and_consume(&broker, &id, &grant).await
        .map_err(|_| "排程通知核准已失效或使用，已停止傳送。".into())
}

/// RM-124: admission for a scheduled result aimed at a WebChat conversation.
///
/// A WebChat target is an internal session of the employee's own
/// conversation with a signed-in dashboard account — the scheduled analogue
/// of "the source conversation", which the recipient policy always allows —
/// so it is NOT evaluated as an external recipient and never raises an
/// approval card. What replaces the policy is the fire-time ownership
/// re-check: a WebChat target is only ever a turn origin, so it must be the
/// job's stored origin and pass [`origin::verify`] against the gateway's
/// record (a row without a proof id is refused `origin_unproven`; v1.81
/// WebChat rows are backfilled at startup, `origin::startup_backfill`);
/// then the session must still exist, still belong to `agent` and to the
/// same dashboard account, and that account must still be Active and allowed
/// to talk to `agent`. Any miss refuses (fail closed); the caller drops the
/// message with an audit row.
pub(crate) async fn admit_scheduled_session(
    home: &Path, agent: &str, stored: StoredOrigin<'_>, session: &str,
) -> Result<(), ScheduledRefusal> {
    match stored.recipient.filter(|origin| !origin.is_empty()) {
        Some(origin) if origin == session => {}
        None => return Err(ScheduledRefusal::Origin(origin::OriginDenial::NoProof)),
        Some(_) => return Err(ScheduledRefusal::Origin(origin::OriginDenial::ProofInvalid)),
    }
    let (home, agent, session) = (home.to_path_buf(), agent.to_owned(), session.to_owned());
    let proof = stored.proof.map(str::to_owned);
    tokio::task::spawn_blocking(move || {
        origin::verify(&home, &agent, &session, proof.as_deref()).map_err(ScheduledRefusal::Origin)?;
        crate::scheduled_webchat::verify_delivery(&home, &agent, &session)
            .map(|_| ())
            .map_err(ScheduledRefusal::Other)
    })
    .await
    .unwrap_or_else(|_| Err(ScheduledRefusal::Other("WebChat 對話擁有者檢查失敗".into())))
}

fn scheduled_request(agent: &str, targets: &[String], args: Value) -> (Value, crate::approval::action_grant::ActionGrant) {
    use crate::approval::action_grant::{ActionGrant, ACTION_GRANT_KEY};
    let mut payload = json!({"name": "scheduled_notification", "arguments": args});
    let grant = ActionGrant::build(agent, "scheduled_notification", &payload);
    payload[ACTION_GRANT_KEY] = grant.to_json();
    payload["recipient_guard"] = json!({"reason": "outside_allowed_recipients", "targets": targets});
    payload["egress_guard"] = json!({"reason": "recipient_guard", "sources": [],
        "recipient_guard": payload["recipient_guard"].clone()});
    (payload, grant)
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_auth::{UserDb, UserRole};

    // Not run: builds and tests are prohibited on the memory-constrained host.
    #[test]
    fn unadapted_effects_ask_independently_of_private_reads_and_allowed_recipients() {
        use duduclaw_core::ToolEffect;
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("agents/bot");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agent.toml"), "[capabilities]\nrecipients=['line:allowed']\n").unwrap();
        for mode in ["off", "shadow", "ask"] {
            std::fs::write(home.path().join("config.toml"), format!("[security]\nrecipient_guard='{mode}'\n")).unwrap();
            for effect in ToolEffect::ALL {
                let reason = unadapted_effect(home.path(), "bot", "crm.send", effect, false);
                assert_eq!(reason.is_some(), mode == "ask" && effect.is_side_effecting());
            }
        }
        let mut envelope = Some(json!({"reason":"egress_guard", "sources":["memory_read"]}));
        merge_unadapted_effect(home.path(), "bot", "crm.send", ToolEffect::Send, &mut envelope);
        assert_eq!(envelope.as_ref().unwrap()["sources"], json!(["memory_read"]));
        assert!(confirm_unadapted_effect(home.path(), "bot", "crm.send", ToolEffect::Send, envelope.as_ref()).is_ok());
        assert!(confirm_unadapted_effect(home.path(), "bot", "crm.send", ToolEffect::Send,
            Some(&json!({"reason":"egress_guard", "sources":["memory_read"]}))).is_err());
        std::fs::write(home.path().join("config.toml"), "[security]\nrecipient_guard='shadow'\n").unwrap();
        assert!(unadapted_effect(home.path(), "bot", "crm.send", ToolEffect::Publish, true).is_none());
        let audit = std::fs::read_to_string(home.path().join("tool_calls.jsonl")).unwrap();
        assert!(audit.contains("recipient_would_ask"));
        assert!(!home.path().join("approvals.db").exists());
        std::fs::write(home.path().join("config.toml"), "[security]\nrecipient_guard=42\n").unwrap();
        assert!(unadapted_effect(home.path(), "bot", "crm.send", ToolEffect::Send, false).is_some());
    }

    #[test]
    fn owners_require_owner_binding_and_verified_identity_and_ignore_proactive() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("agents/bot");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agent.toml"), "[proactive]\nnotify_channel='line'\nnotify_chat_id='editable'\n").unwrap();
        assert!(owners(home.path(), "bot").unwrap().is_empty());
        assert!(!home.path().join("users.db").exists());
        let db = UserDb::new(&home.path().join("users.db")).unwrap();
        let owner = db.create_user("owner@example.com", "Owner", "test-password", UserRole::Admin).unwrap();
        let operator = db.create_user("operator@example.com", "Operator", "test-password", UserRole::Admin).unwrap();
        db.bind_agent(&owner.id, "bot", AccessLevel::Owner).unwrap();
        db.bind_agent(&operator.id, "bot", AccessLevel::Operator).unwrap();
        db.bind_channel_identity(&owner.id, "line", "verified", true).unwrap();
        db.bind_channel_identity(&owner.id, "line", "unverified", false).unwrap();
        db.bind_channel_identity(&operator.id, "telegram", "123", true).unwrap();
        assert_eq!(owners(home.path(), "bot").unwrap(), ["line:verified"]);
        db.set_user_status(&owner.id, UserStatus::Suspended).unwrap();
        assert!(owners(home.path(), "bot").unwrap().is_empty());
    }

    #[test]
    fn scheduled_fire_rechecks_policy_with_an_empty_source_set() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("agents/bot");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(home.path().join("config.toml"), "[security]\nrecipient_guard='ask'\n").unwrap();
        std::fs::write(dir.join("agent.toml"), "[capabilities]\nrecipients=['telegram:123']\n").unwrap();
        let targets = vec!["telegram:123".into()];
        assert_eq!(scheduled_decision(home.path(), "bot", &targets, None).unwrap(), Decision::Allow);
        std::fs::write(dir.join("agent.toml"), "[capabilities]\nrecipients=[]\n").unwrap();
        assert_eq!(scheduled_decision(home.path(), "bot", &targets, None).unwrap(), Decision::Ask);
        std::fs::write(home.path().join("config.toml"), "[security]\nrecipient_guard='shadow'\n").unwrap();
        assert_eq!(scheduled_decision(home.path(), "bot", &targets, None).unwrap(), Decision::WouldAsk);
    }

    #[tokio::test]
    async fn shadow_scheduled_notification_never_blocks_and_records_would_ask() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join("agents/bot")).unwrap();
        std::fs::write(home.path().join("agents/bot/agent.toml"), "[capabilities]\n").unwrap();
        std::fs::write(home.path().join("config.toml"), "[security]\nrecipient_guard='shadow'\n").unwrap();
        admit_scheduled(home.path(), "bot", &["line:outside".into()], json!({"text":"hello"})).await.unwrap();
        let text = std::fs::read_to_string(home.path().join("tool_calls.jsonl")).unwrap();
        let row: Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
        assert_eq!(row["recipient_guard"], "would_ask");
    }
    #[tokio::test]
    async fn scheduled_approval_is_admin_only_and_binds_one_destination_and_body() {
        use crate::approval::{ApprovalBroker, action_grant::{ActionGrant, verify_and_consume}};
        let home = tempfile::tempdir().unwrap();
        let broker = ApprovalBroker::open(home.path()).unwrap();
        let args = json!({"task_id":"task-1", "channel":"line", "chat_id":"outside", "text":"private"});
        let (payload, grant) = scheduled_request("bot", &["line:outside".into()], args.clone());
        assert_eq!(payload["recipient_guard"]["reason"], "outside_allowed_recipients");
        let id = broker.request("bot", "mcp_call", "recipient approval", payload, 300).await.unwrap();
        assert!(broker.decide(&id, true, "channel:admin").await.is_err());
        let mut manager = duduclaw_auth::UserContext::admin_fallback();
        manager.role = UserRole::Manager;
        assert!(broker.decide_egress_dashboard(&id, &manager, true).await.is_err());
        broker.decide_egress_dashboard(&id, &duduclaw_auth::UserContext::admin_fallback(), true).await.unwrap();
        for (key, value) in [("chat_id", "other"), ("text", "changed"), ("task_id", "next-occurrence")] {
            let mut changed = args.clone();
            changed[key] = json!(value);
            let changed = ActionGrant::build("bot", "scheduled_notification", &json!({"arguments":changed}));
            assert!(verify_and_consume(&broker, &id, &changed).await.is_err());
        }
        verify_and_consume(&broker, &id, &grant).await.unwrap();
        assert!(verify_and_consume(&broker, &id, &grant).await.is_err());
    }

    #[tokio::test]
    async fn legacy_default_schedule_uses_the_real_main_employee_policy() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("agents/real-main");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(home.path().join("config.toml"), "[security]\nrecipient_guard='shadow'\n").unwrap();
        std::fs::write(dir.join("agent.toml"), r#"
[agent]
name = "real-main"
display_name = "Main"
role = "main"
status = "active"
trigger = "@main"
reports_to = ""
icon = "x"
[model]
preferred = "claude-sonnet-4-6"
fallback = "claude-haiku-4-5"
account_pool = ["main"]
[container]
timeout_ms = 60000
max_concurrent = 1
readonly_project = true
[heartbeat]
enabled = false
interval_seconds = 3600
max_concurrent_runs = 1
cron = ""
[budget]
monthly_limit_cents = 500
warn_threshold_percent = 80
hard_stop = false
[permissions]
can_create_agents = false
can_send_cross_agent = false
can_modify_own_skills = false
can_modify_own_soul = false
can_schedule_tasks = true
allowed_channels = []
[evolution]
skill_auto_activate = false
skill_security_scan = true
[capabilities]
recipient_guard = "ask"
"#).unwrap();
        let mut registry = duduclaw_agent::registry::AgentRegistry::new(home.path().join("agents"));
        registry.scan().await.unwrap();
        let registry = std::sync::Arc::new(tokio::sync::RwLock::new(registry));
        let agent = scheduled_agent(&registry, "default").await.unwrap();
        assert_eq!(agent, "real-main");
        assert_eq!(scheduled_decision(home.path(), &agent, &["line:outside".into()], None).unwrap(), Decision::Ask);
        let empty = std::sync::Arc::new(tokio::sync::RwLock::new(
            duduclaw_agent::registry::AgentRegistry::new(home.path().join("empty"))));
        assert!(scheduled_agent(&empty, "default").await.is_err());
    }

    /// RM-124: a WebChat session target is admitted by its gateway origin
    /// proof and ownership, not by the external-recipient policy — even
    /// `recipient_guard='ask'` with an empty allow-list files no approval
    /// card — and is refused without a proof, once the session owner loses
    /// access, the session moves to another employee, or it is deleted.
    #[tokio::test]
    async fn webchat_session_target_is_admitted_by_its_proof_and_ownership_not_policy() {
        use origin::OriginDenial;
        let f = crate::scheduled_webchat::tests::fixture("sales").await;
        let h = f.home.path();
        std::fs::create_dir_all(h.join("agents/sales")).unwrap();
        std::fs::write(h.join("config.toml"), "[security]\nrecipient_guard='ask'\n").unwrap();
        std::fs::write(h.join("agents/sales/agent.toml"), "[capabilities]\nrecipients=[]\n").unwrap();
        let stored = StoredOrigin { recipient: Some(&f.target), proof: Some(&f.proof) };
        admit_scheduled_session(h, "sales", stored, &f.target).await.unwrap();
        assert!(!h.join("approvals.db").exists(), "no approval card for the user's own conversation");
        let origin_denial = |result: Result<(), ScheduledRefusal>| match result {
            Err(ScheduledRefusal::Origin(denial)) => Some(denial),
            _ => None,
        };
        // A legacy WebChat row (no stored origin / proof) is refused, never downgraded.
        assert_eq!(origin_denial(admit_scheduled_session(h, "sales", StoredOrigin::NONE, &f.target).await),
            Some(OriginDenial::NoProof));
        let unproven = StoredOrigin { recipient: Some(&f.target), proof: None };
        assert_eq!(origin_denial(admit_scheduled_session(h, "sales", unproven, &f.target).await),
            Some(OriginDenial::NoProof));
        let elsewhere = StoredOrigin { recipient: Some("webchat:webchat:000000000000:x"), proof: Some(&f.proof) };
        assert_eq!(origin_denial(admit_scheduled_session(h, "sales", elsewhere, &f.target).await),
            Some(OriginDenial::ProofInvalid));
        assert!(admit_scheduled_session(h, "hr", stored, &f.target).await.is_err());
        let db = UserDb::new(&h.join("users.db")).unwrap();
        db.unbind_agent(&f.user, "sales").unwrap();
        assert!(admit_scheduled_session(h, "sales", stored, &f.target).await.is_err());
        db.bind_agent(&f.user, "sales", AccessLevel::Operator).unwrap();
        db.set_user_status(&f.user, UserStatus::Suspended).unwrap();
        assert!(admit_scheduled_session(h, "sales", stored, &f.target).await.is_err());
        // The external-recipient path still never accepts a webchat address.
        assert_eq!(duduclaw_core::recipient_policy::channel_recipient("webchat", &f.session), None);
    }

}

#[cfg(test)]
mod restricted_owner_tests {
    use super::*;
    use duduclaw_auth::{UserDb, UserRole};

    #[test]
    fn only_admin_owners_remain_trusted_while_the_preset_is_bound() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        std::fs::create_dir_all(h.join("agents/sales")).unwrap();
        std::fs::write(h.join("config.toml"), "[security]\nrecipient_guard='ask'\n").unwrap();
        let db = UserDb::new(&h.join("users.db")).unwrap();
        let admin = db.create_user("admin@test.invalid", "Admin", "fixture-password", UserRole::Admin).unwrap();
        let employee = db.create_user("owner@test.invalid", "Owner", "fixture-password", UserRole::Employee).unwrap();
        for (user, channel_id) in [(&admin, "admin"), (&employee, "old-owner")] {
            db.bind_agent(&user.id, "sales", AccessLevel::Owner).unwrap();
            db.bind_channel_identity(&user.id, "line", channel_id, true).unwrap();
        }
        let before = owners(h, "sales").unwrap();
        assert_eq!(before, ["line:admin", "line:old-owner"]);
        assert_eq!(scheduled_decision(h, "sales", &["line:old-owner".into()], None).unwrap(), Decision::Allow);
        duduclaw_core::preset::install_system_operator_preset(h, false).unwrap();
        duduclaw_core::preset::bind(h, "sales", &h.join("agents/sales"), "system-operator", "test", "test").unwrap();
        assert_eq!(owners(h, "sales").unwrap(), ["line:admin"]);
        assert_eq!(scheduled_decision(h, "sales", &["line:old-owner".into()], None).unwrap(), Decision::Ask);
        assert_eq!(scheduled_decision(h, "sales", &["line:admin".into()], None).unwrap(), Decision::Allow);
        duduclaw_core::preset::unbind(h, "sales", "test", "test").unwrap();
        assert_eq!(owners(h, "sales").unwrap(), before);
    }
}

#[cfg(test)]
mod schedule_origin_tests {
    use super::*;
    use super::origin::tests::{RECIPIENT, fixture};

    #[tokio::test]
    async fn stored_origin_is_allowed_in_ask_mode_only_with_a_live_proof() {
        let f = fixture("sales").await;
        let h = f.home.path();
        std::fs::write(h.join("config.toml"), "[security]\nrecipient_guard='ask'\n").unwrap();
        let store = crate::cron_store::CronStore::open(h).unwrap();
        let mut row = crate::cron_store::CronTaskRow::new("id".into(), "Daily".into(), "sales".into(), "0 9 * * *".into(), "report".into());
        row.notify_origin = Some(RECIPIENT.into());
        row.notify_origin_proof = Some(f.proof.clone());
        store.insert(&row).await.unwrap();
        let row = store.get("id").await.unwrap().unwrap();
        let stored = StoredOrigin { recipient: row.notify_origin.as_deref(), proof: row.notify_origin_proof.as_deref() };
        let origin = [RECIPIENT.to_string()];
        let other = ["googlechat:spaces/OTHER".to_string()];
        assert_eq!(origin_source(h, "sales", stored, &origin).unwrap().as_deref(), Some(RECIPIENT));
        assert_eq!(origin_source(h, "sales", stored, &other).unwrap(), None, "another target never borrows it");
        assert_eq!(scheduled_decision(h, "sales", &other, Some(RECIPIENT)).unwrap(), Decision::Ask);
        admit_scheduled_from_origin(h, "sales", stored, &origin, json!({})).await.unwrap();
    }

    // (f) A legacy row (origin but no proof) aimed at that origin is refused
    // `origin_unproven` in every mode — `off` included — with no approval
    // card; a target that is not the stored origin still follows ordinary
    // policy (allowed by `off`, asked by `ask`).
    #[tokio::test]
    async fn legacy_origin_without_proof_is_denied_even_when_the_guard_is_off() {
        let f = fixture("sales").await;
        let h = f.home.path();
        let legacy = StoredOrigin { recipient: Some(RECIPIENT), proof: None };
        let at_origin = [RECIPIENT.to_string()];
        let other = ["googlechat:spaces/OTHER".to_string()];
        for mode in ["off", "shadow", "ask"] {
            std::fs::write(h.join("config.toml"), format!("[security]\nrecipient_guard='{mode}'\n")).unwrap();
            assert_eq!(origin_source(h, "sales", legacy, &at_origin).unwrap_err(), origin::OriginDenial::NoProof, "{mode}");
            let refusal = admit_scheduled_from_origin(h, "sales", legacy, &at_origin, json!({})).await.unwrap_err();
            assert!(matches!(refusal, ScheduledRefusal::Origin(origin::OriginDenial::NoProof)), "{mode}: {refusal}");
            assert!(refusal.to_string().starts_with("origin_unproven"));
            assert_eq!(origin_source(h, "sales", legacy, &other).unwrap(), None, "{mode}: not the origin");
        }
        assert!(!h.join("approvals.db").exists());
        std::fs::write(h.join("config.toml"), "[security]\nrecipient_guard='off'\n").unwrap();
        admit_scheduled_from_origin(h, "sales", legacy, &other, json!({})).await.unwrap();
        std::fs::write(h.join("config.toml"), "[security]\nrecipient_guard='ask'\n").unwrap();
        assert_eq!(scheduled_decision(h, "sales", &other, None).unwrap(), Decision::Ask);
    }

    // (b) A failing proof refuses outright with its closed reason — even when
    // policy is off — and files no approval card.
    #[tokio::test]
    async fn revoked_creator_refuses_the_origin_delivery_without_fallback() {
        let f = fixture("sales").await;
        let h = f.home.path();
        std::fs::write(h.join("config.toml"), "[security]\nrecipient_guard='off'\n").unwrap();
        duduclaw_auth::UserDb::new(&h.join("users.db")).unwrap()
            .set_user_status(&f.user, UserStatus::Suspended).unwrap();
        let stored = StoredOrigin { recipient: Some(RECIPIENT), proof: Some(&f.proof) };
        let refusal = admit_scheduled_from_origin(h, "sales", stored, &[RECIPIENT.to_string()], json!({}))
            .await.unwrap_err();
        assert!(matches!(refusal, ScheduledRefusal::Origin(origin::OriginDenial::AccountInactive)), "{refusal}");
        assert!(!h.join("approvals.db").exists());
    }
}
