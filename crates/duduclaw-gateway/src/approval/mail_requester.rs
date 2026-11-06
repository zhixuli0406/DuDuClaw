//! Narrow, live account authority for the human requesting a single mail send.
use std::path::Path;
use duduclaw_auth::{AccessLevel, UserRole, UserStatus};
use super::{ApprovalRecord, DecisionContext, RequestKind, action_grant::ActionGrant};

/// Only adapters with an exact trusted transport currently attest an audience.
/// Additional adapters must wire their authenticated metadata before opting in.
pub(crate) fn adapter_audience(channel: &str, chat_type: Option<&str>) -> super::ConversationAudience {
    use super::ConversationAudience::{Private, MultiParty};
    match (channel, chat_type) {
        ("line", Some("user")) | ("telegram", Some("private"))
        | ("slack", Some("im")) | ("discord", Some("1")) => Private,
        _ => MultiParty,
    }
}

/// Owner-selected default-on. Only absence defaults; unreadable or malformed
/// configuration (including a non-boolean value) never grants chat authority.
pub(crate) fn enabled(home: &Path) -> bool {
    let raw = match std::fs::read_to_string(home.join("config.toml")) {
        Ok(raw) => raw,
        Err(e) => return e.kind() == std::io::ErrorKind::NotFound
            && std::fs::symlink_metadata(home.join("config.toml"))
                .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound),
    };
    let Ok(table) = raw.parse::<toml::Table>() else { return false; };
    let Some(security) = table.get("security") else { return true; };
    let Some(security) = security.as_table() else { return false; };
    match security.get("mail_requester_approval") {
        None => true,
        Some(value) => value.as_bool().unwrap_or(false),
    }
}

/// Mail-specific authority snapshot, in addition to the standard policy
/// revision. Avoid changing revisions of unrelated standing workflows.
pub(crate) fn environment_hash(home: &Path, agent: &str, turn: &str) -> Result<String, String> {
    let security = match std::fs::read_to_string(home.join("config.toml")) {
        Ok(raw) => {
            let table = raw.parse::<toml::Table>().map_err(|_| "mail policy malformed")?;
            serde_json::to_value(table.get("security")).map_err(|_| "mail policy malformed")?
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound
            && std::fs::symlink_metadata(home.join("config.toml"))
                .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound) => serde_json::Value::Null,
        Err(_) => return Err("mail policy unreadable".into()),
    };
    Ok(super::payload_hash(&serde_json::json!(["mail_send", agent, turn, security])))
}

/// Resolve the adapter-attested principal again, never a display name, model
/// claim, destination-only match, or an unverified channel link.
pub(crate) fn requester_user(home: &Path, context: &DecisionContext, agent: &str) -> Option<String> {
    context.validate().ok()?;
    if !matches!(context.channel.as_str(), "telegram" | "line" | "discord" | "slack"
        | "whatsapp" | "feishu" | "googlechat" | "teams" | "wecom" | "dingtalk" | "webchat") {
        return None;
    }
    let db = crate::decision_notify::open_user_db(home).ok()??;
    let uid = if context.channel == "webchat" {
        context.principal_id.clone()
    } else {
        db.find_verified_user_id_by_channel(&context.channel, &context.principal_id).ok()??
    };
    let user = db.get_user(&uid).ok()??;
    if user.status != UserStatus::Active
        || (crate::machine_employee::requires_admin_for_id(home, agent) && user.role != UserRole::Admin) {
        return None;
    }
    if user.role != UserRole::Admin
        && !db.check_agent_access(&uid, agent).ok()?.is_some_and(|level| level.at_least(AccessLevel::Operator)) {
        return None;
    }
    Some(uid)
}

/// One predicate for notification, channel decisions and dispatch revalidation.
/// Only the gateway's mail route creates this binding and stamps on_behalf_of
/// from the live turn registry. Legacy/unattributed requests remain Admin-only.
pub(crate) fn eligible(home: &Path, rec: &ApprovalRecord) -> bool {
    if !enabled(home) || rec.request_kind != RequestKind::Approval
        || !matches!(rec.action_kind.as_str(), "bound_action" | "mcp_call")
        || rec.payload["name"] != "mail_send" || !rec.payload["egress_guard"].is_object() {
        return false;
    }
    let Some(binding) = rec.binding.as_ref() else { return false; };
    if binding.conversation_audience != super::ConversationAudience::Private
        || binding.run_origin_kind != "ingress" || binding.resume_handler != "live_only_v1"
        || binding.actor_principal != rec.agent_id || binding.validate(&rec.payload).is_err()
        || environment_hash(home, &rec.agent_id, &binding.run_id).ok().as_deref() != Some(binding.environment_hash.as_str())
        || !ActionGrant::build(&rec.agent_id, "mail_send", &rec.payload)
            .matches_stored(&rec.payload["action_grant"]) {
        return false;
    }
    let Some(uid) = requester_user(home, &binding.decision_context, &rec.agent_id) else { return false; };
    duduclaw_core::HumanPrincipal::user(&uid)
        .is_some_and(|who| rec.on_behalf_of.as_deref() == Some(who.to_string().as_str()))
}

/// Render directly from final bound arguments. Recipients and subject are
/// never truncated by the generic summary cap; only the body is summarized.
pub(crate) fn card_body(rec: &ApprovalRecord) -> String {
    let args = &rec.payload["arguments"];
    let quoted = |key: &str| serde_json::to_string(&args[key]).unwrap_or_else(|_| "null".into());
    let mut body = format!("請確認這封郵件（僅限本次寄送）\n收件人：{}\n", quoted("to"));
    for (key, label) in [("cc", "副本"), ("bcc", "密件副本")] {
        if args.get(key).is_some() { body.push_str(&format!("{label}：{}\n", quoted(key))); }
    }
    body.push_str(&format!("主旨：{}\n內文摘要：{}\n", quoted("subject"),
        serde_json::to_string(&duduclaw_core::truncate_chars(args["body"].as_str().unwrap_or(""), 500)).unwrap()));
    let private = rec.payload["egress_guard"]["sources"].as_array().is_some_and(|s| !s.is_empty());
    body.push_str(if private { "本回合已讀取私人資料或無法確認資料來源。\n" }
        else { "本回合未記錄私人資料讀取。\n" });
    body.push_str(&format!("期限：{}\n", rec.deadline_rfc3339().unwrap_or_else(|| "已逾期".into())));
    body.push_str(&format!("僅限原交辦者的已驗證公司帳號在此對話決定，或由管理員至儀表板決定。\n逾期自動拒絕。\n確認 {}\n取消 {}",
        rec.id.as_str(), rec.id.as_str()));
    body
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::{ApprovalBroker, ApprovalId, ExecutionBinding, payload_hash, policy_revision,
        action_grant::verify_and_consume};
    use serde_json::json;

    struct Fixture {
        home: tempfile::TempDir,
        db: duduclaw_auth::UserDb,
        uid: String,
        broker: ApprovalBroker,
        context: DecisionContext,
    }
    impl Fixture {
        fn new() -> Self {
            let home = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(home.path().join("agents/alice")).unwrap();
            std::fs::write(home.path().join("agents/alice/agent.toml"), "[capabilities]\n").unwrap();
            let db = duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
            let user = db.create_user("requester@example.com", "Requester", "test-password", UserRole::Employee).unwrap();
            db.bind_channel_identity(&user.id, "line", "human", true).unwrap();
            db.bind_agent(&user.id, "alice", AccessLevel::Operator).unwrap();
            let broker = ApprovalBroker::open(home.path()).unwrap();
            Self { home, db, uid: user.id, broker, context: DecisionContext {
                channel: "line".into(), account_id: "bot".into(),
                conversation_id: "human".into(), principal_id: "human".into(),
            } }
        }
        async fn request(&self, tool: &str) -> (ApprovalId, ActionGrant) {
            let mut payload = json!({"name":tool, "arguments":{
                "to":"recipient@example.com", "subject":"A real subject", "body":"Mail body"},
                "egress_guard":{"sources":["memory_read"], "recipient_guard":{"reason":"outside_allowed_recipients"}}});
            let grant = ActionGrant::build("alice", tool, &payload);
            payload["action_grant"] = grant.to_json();
            let binding = ExecutionBinding {
                conversation_audience: super::super::ConversationAudience::Private,
                schema_version:1, run_id:"test-turn".into(), run_origin_kind:"ingress".into(),
                actor_principal:"alice".into(), decision_context:self.context.clone(),
                task_id:None, task_revision:None, task_snapshot_hash:None,
                payload_hash:payload_hash(&payload), policy_revision:policy_revision(self.home.path(), "alice").unwrap(),
                cwd:None, environment_hash:environment_hash(self.home.path(), "alice", "test-turn").unwrap_or_else(|_| "unreadable".into()), file_hashes:Default::default(),
                expires_at:(chrono::Utc::now()+chrono::Duration::seconds(300)).to_rfc3339(),
                resume_handler:"live_only_v1".into(), resume_version:1,
            };
            let principal = duduclaw_core::HumanPrincipal::user(&self.uid).unwrap().to_string();
            let id = crate::on_behalf_of::ON_BEHALF_OF.scope(Some(principal), self.broker.request_bound(
                RequestKind::Approval, "alice", "mail", payload, binding,
            )).await.unwrap();
            (id, grant)
        }
    }

    #[test]
    fn mail_requester_adapter_audience_requires_positive_private_metadata() {
        use super::super::ConversationAudience::{Private, MultiParty};
        for (channel, private, groups) in [
            ("line", "user", vec!["group", "room"]),
            ("telegram", "private", vec!["group", "supergroup", "channel"]),
            ("slack", "im", vec!["mpim", "group", "channel"]),
            ("discord", "1", vec!["0", "3", "11"]),
        ] {
            assert_eq!(adapter_audience(channel, Some(private)), Private);
            for kind in groups.into_iter().map(Some).chain([None, Some("unknown"), Some("")]) {
                assert_eq!(adapter_audience(channel, kind), MultiParty, "{channel}: {kind:?}");
            }
        }
        for channel in ["teams", "googlechat", "feishu", "wecom", "dingtalk", "whatsapp", "webchat"] {
            assert_eq!(adapter_audience(channel, Some("private")), MultiParty);
        }
    }

    #[tokio::test]
    async fn mail_requester_group_and_legacy_bindings_are_admin_only_without_content() {
        let f = Fixture::new();
        let admin = f.db.create_user("admin@example.com", "Admin", "test-password", UserRole::Admin).unwrap();
        let mut ctx = duduclaw_auth::UserContext::admin_fallback();
        ctx.user_id = admin.id;
        for (channel, kind) in [("line", Some("group")), ("telegram", Some("supergroup")), ("line", None)] {
            f.db.bind_channel_identity(&f.uid, channel, "human", true).unwrap();
            let (id, grant) = f.request("mail_send").await;
            let original = f.broker.get(&id).await.unwrap().unwrap();
            let mut binding = original.binding.clone().unwrap();
            binding.decision_context.channel = channel.into();
            binding.decision_context.conversation_id = "group".into();
            binding.conversation_audience = adapter_audience(channel, kind);
            let context = binding.decision_context.clone();
            // Old serialized bindings have no audience, and must stay denied.
            let mut serialized = serde_json::to_value(&binding).unwrap();
            serialized.as_object_mut().unwrap().remove("conversation_audience");
            let binding: ExecutionBinding = serde_json::from_value(serialized).unwrap();
            let principal = duduclaw_core::HumanPrincipal::user(&f.uid).unwrap().to_string();
            let id = crate::on_behalf_of::ON_BEHALF_OF.scope(Some(principal), f.broker.request_bound(
                RequestKind::Approval, "alice", "recipient@example.com A real subject Mail body", original.payload, binding,
            )).await.unwrap();
            let rec = f.broker.get(&id).await.unwrap().unwrap();
            assert!(!eligible(f.home.path(), &rec));
            assert!(crate::approval_notify::is_dashboard_only_record(Some(f.home.path()), &rec));
            for reminder in [false, true] {
                let notice = crate::approval_notify::dashboard_only_notice_body(&rec, reminder);
                for secret in ["recipient@example.com", "A real subject", "Mail body"] { assert!(!notice.contains(secret)); }
            }
            assert!(f.broker.decide_bound(&id, &context, true).await.is_err());
            let mut employee = ctx.clone(); employee.user_id = f.uid.clone(); employee.role = UserRole::Employee;
            assert!(f.broker.decide_bound_dashboard(&id, &employee, true).await.is_err());
            f.broker.decide_bound_dashboard(&id, &ctx, true).await.unwrap();
            verify_and_consume(&f.broker, &id, &grant).await.unwrap();
        }
    }

    #[test]
    fn mail_requester_config_defaults_only_when_absent() {
        let home = tempfile::tempdir().unwrap();
        assert!(enabled(home.path()));
        for (raw, expected) in [("",true), ("[security]\n",true),
            ("[security]\nmail_requester_approval=true",true),
            ("[security]\nmail_requester_approval=false",false),
            ("[security]\nmail_requester_approval='true'",false),
            ("[security",false), ("security=false",false)] {
            std::fs::write(home.path().join("config.toml"), raw).unwrap();
            assert_eq!(enabled(home.path()), expected, "{raw}");
        }
        std::fs::remove_file(home.path().join("config.toml")).unwrap();
        std::fs::create_dir(home.path().join("config.toml")).unwrap();
        assert!(!enabled(home.path()));
    }

    #[tokio::test]
    async fn mail_requester_chat_grant_dispatches_once_and_binds_recipients() {
        let f = Fixture::new();
        let (id, grant) = f.request("mail_send").await;
        let rec = f.broker.get(&id).await.unwrap().unwrap();
        assert!(eligible(f.home.path(), &rec));
        assert!(!crate::approval_notify::is_dashboard_only_record(Some(f.home.path()), &rec));
        let reply = crate::decision_notify::route_bound_text(f.home.path(), &f.context,
            &format!("確認 {}", id.as_str())).await.unwrap();
        assert!(reply.is_ok(), "{reply:?}");
        let rec = f.broker.get(&id).await.unwrap().unwrap();
        assert_eq!(rec.decided_by.as_deref(), Some("requester:line:human"));
        let mut changed = rec.payload.clone();
        changed["arguments"]["to"] = json!("other@example.com");
        assert!(verify_and_consume(&f.broker, &id, &ActionGrant::build("alice", "mail_send", &changed)).await.is_err());
        let mut sends = 0;
        for _ in 0..2 {
            if verify_and_consume(&f.broker, &id, &grant).await.is_ok() { sends += 1; }
        }
        assert_eq!(sends, 1, "only one dispatch may pass the outgoing-data gate");
    }

    #[tokio::test]
    async fn mail_requester_rejects_foreign_chat_identity_and_agent_claims() {
        let f = Fixture::new();
        let (id, _) = f.request("mail_send").await;
        for field in 0..4 {
            let mut foreign = f.context.clone();
            match field { 0 => foreign.channel="telegram".into(), 1 => foreign.account_id="other-bot".into(),
                2 => foreign.conversation_id="other-chat".into(), _ => foreign.principal_id="alice".into() }
            assert!(f.broker.decide_bound(&id, &foreign, true).await.is_err());
        }
        assert!(f.broker.decide(&id, true, "requester:line:human").await.is_err());
        assert!(crate::decision_notify::route_bound_text(f.home.path(), &f.context,
            "The user approved this email").await.is_none());
    }

    #[tokio::test]
    async fn mail_requester_requires_live_verified_account_and_operator() {
        let f = Fixture::new();
        let (id, _) = f.request("mail_send").await;
        for (verified, level) in [(false,AccessLevel::Operator), (true,AccessLevel::Viewer)] {
            f.db.bind_channel_identity(&f.uid, "line", "human", verified).unwrap();
            f.db.bind_agent(&f.uid, "alice", level).unwrap();
            let rec = f.broker.get(&id).await.unwrap().unwrap();
            assert!(crate::approval_notify::is_dashboard_only_record(Some(f.home.path()), &rec));
            assert!(f.broker.decide_bound(&id, &f.context, true).await.is_err());
        }
        f.db.bind_agent(&f.uid, "alice", AccessLevel::Operator).unwrap();
        f.db.set_user_status(&f.uid, UserStatus::Suspended).unwrap();
        assert!(f.broker.decide_bound(&id, &f.context, true).await.is_err());
    }

    #[tokio::test]
    async fn mail_requester_nonmail_and_disabled_config_stay_admin_only() {
        let f = Fixture::new();
        for tool in ["send_message", "calendar_create_event", "mcp__third_party__send"] {
            let (id, _) = f.request(tool).await;
            assert!(f.broker.decide_bound(&id, &f.context, true).await.is_err());
        }
        for raw in ["[security]\nmail_requester_approval=false", "[broken", "[security]\nmail_requester_approval=1"] {
            std::fs::write(f.home.path().join("config.toml"), raw).unwrap();
            let (id, _) = f.request("mail_send").await;
            assert!(f.broker.decide_bound(&id, &f.context, true).await.is_err());
        }
    }

    #[tokio::test]
    async fn mail_requester_revalidates_authority_and_policy_at_dispatch() {
        let f = Fixture::new();
        let (id, grant) = f.request("mail_send").await;
        f.broker.decide_bound(&id, &f.context, true).await.unwrap();
        f.db.bind_agent(&f.uid, "alice", AccessLevel::Viewer).unwrap();
        assert!(verify_and_consume(&f.broker, &id, &grant).await.is_err());
        f.db.bind_agent(&f.uid, "alice", AccessLevel::Operator).unwrap();
        std::fs::write(f.home.path().join("config.toml"), "[security]\nrecipient_guard='ask'").unwrap();
        assert!(verify_and_consume(&f.broker, &id, &grant).await.is_err());
    }

    #[tokio::test]
    async fn mail_requester_dashboard_admin_still_approves_and_expiry_denies() {
        let f = Fixture::new();
        let admin = f.db.create_user("admin@example.com", "Admin", "test-password", UserRole::Admin).unwrap();
        let mut ctx = duduclaw_auth::UserContext::admin_fallback();
        ctx.user_id = admin.id;
        let (id, grant) = f.request("mail_send").await;
        f.broker.decide_bound_dashboard(&id, &ctx, true).await.unwrap();
        assert!(f.broker.get(&id).await.unwrap().unwrap().decided_by.unwrap().starts_with("dashboard:"));
        verify_and_consume(&f.broker, &id, &grant).await.unwrap();
        let (id, grant) = f.request("mail_send").await;
        f.broker.decide_bound(&id, &f.context, true).await.unwrap();
        f.broker.store.conn.lock().await.execute("UPDATE approvals SET created_at='2000-01-01T00:00:00Z' WHERE id=?1",
            [id.as_str()]).unwrap();
        assert!(verify_and_consume(&f.broker, &id, &grant).await.is_err());
        let (id, _) = f.request("mail_send").await;
        f.broker.store.conn.lock().await.execute("UPDATE approvals SET created_at='2000-01-01T00:00:00Z' WHERE id=?1",
            [id.as_str()]).unwrap();
        assert!(f.broker.decide_bound(&id, &f.context, true).await.is_err());
    }

    #[tokio::test]
    async fn non_ingress_mail_keeps_its_existing_admin_binding() {
        let f = Fixture::new();
        let admin = f.db.create_user("admin@example.com", "Admin", "test-password", UserRole::Admin).unwrap();
        let mut ctx = duduclaw_auth::UserContext::admin_fallback();
        ctx.user_id = admin.id;
        let (id, grant) = f.request("mail_send").await;
        let rec = f.broker.get(&id).await.unwrap().unwrap();
        let mut binding = rec.binding.unwrap();
        binding.run_origin_kind = "computer_session".into();
        binding.environment_hash = "existing-host-environment".into();
        let id = f.broker.request_bound(RequestKind::Approval, "alice", "mail",
            rec.payload, binding).await.unwrap();
        assert!(f.broker.decide_bound(&id, &f.context, true).await.is_err());
        f.broker.decide_bound_dashboard(&id, &ctx, true).await.unwrap();
        verify_and_consume(&f.broker, &id, &grant).await.unwrap();
    }

    #[tokio::test]
    async fn mail_requester_card_keeps_full_recipient_and_subject_and_private_notice() {
        let f = Fixture::new();
        let (id, _) = f.request("mail_send").await;
        let mut rec = f.broker.get(&id).await.unwrap().unwrap();
        let address = format!("{}@example.com", "recipient".repeat(30));
        rec.payload["arguments"]["to"] = json!(address);
        rec.payload["arguments"]["subject"] = json!("subject".repeat(70));
        let body = card_body(&rec);
        assert!(body.contains(&address));
        assert!(body.contains(&"subject".repeat(70)));
        assert!(body.contains("私人資料"));
        assert!(body.contains(id.as_str()));
    }
}

impl super::ApprovalBroker {
    /// Ask the live gateway to bind native mail approval to its attested human.
    /// In-memory/legacy callers retain dashboard-only approval.
    pub async fn request_mail_requester(
        &self, agent_id: &str, summary: &str, payload: &serde_json::Value,
        ttl_seconds: i64, simulation: Option<&serde_json::Value>,
    ) -> Result<Option<super::ApprovalId>, String> {
        let Some(home) = self.home_dir() else { return Ok(None); };
        crate::egress_client::request_mail_approval(
            &home, agent_id, summary, payload, ttl_seconds, simulation,
        ).await
    }
}

impl super::ApprovalBroker {
    /// Persist the existing simulation as display data in the same insert as
    /// the host-bound mail approval. It never supplies decision authority.
    pub(crate) async fn request_bound_mail(
        &self, agent_id: &str, summary: &str, payload: serde_json::Value,
        binding: super::ExecutionBinding, simulation: Option<serde_json::Value>,
    ) -> Result<super::ApprovalId, String> {
        let mut record = self.bound_record(super::ApprovalId::new(), RequestKind::Approval,
            agent_id, summary, payload, binding).await?;
        record.simulation = simulation.map(|value| super::SimulationNarrative::from_json(&value))
            .filter(|narrative| !narrative.is_empty()).map(|narrative| narrative.to_json());
        self.store.insert(&record).await?;
        Ok(record.id)
    }
}
