//! #210 A source-first: owned writer fixtures, all declarations UNRUN.
use super::*;
use duduclaw_core::types::RuntimeType;
fn ok(frame: &WsFrame) -> Value {
    match frame {
        WsFrame::Response {
            ok: true,
            payload: Some(v),
            ..
        } => v.clone(),
        other => panic!("{other:?}"),
    }
}
fn disk(home: &Path, id: &str) -> toml::Table {
    std::fs::read_to_string(home.join("agents").join(id).join("agent.toml"))
        .unwrap()
        .parse()
        .unwrap()
}
#[tokio::test(flavor = "current_thread")]
async fn create_gpt_without_runtime_uses_same_confident_alignment_for_installed_and_absent_cli() {
    for installed in [true, false] {
        let home = tempfile::tempdir().unwrap();
        let h = MethodHandler::new(home.path().to_owned()).await;
        let _availability =
            crate::runtime_config::testing::AvailabilityGuard::at("gpt-5.4", installed);
        let response = h
            .handle(
                "agents.create",
                json!({"name":"route","model_preferred":"gpt-5.4"}),
                &UserContext::admin_fallback(),
            )
            .await;
        let payload = ok(&response);
        let expected = if installed { "codex" } else { "openai_compat" };
        assert_eq!(
            disk(home.path(), "route")["runtime"]["provider"].as_str(),
            Some(expected)
        );
        assert_eq!(payload["runtime_provider_auto_aligned"], expected);
        assert!(h.registry.read().await.get("route").is_some());
    }
}
#[tokio::test(flavor = "current_thread")]
async fn create_api_custom_model_and_explicit_fallback_remain_unchanged() {
    let home = tempfile::tempdir().unwrap();
    let h = MethodHandler::new(home.path().to_owned()).await;
    ok(&h.handle("agents.create",json!({"name":"api","model_preferred":"gpt-5.4","runtime":{"provider":"openai_compat","fallback":"codex"}}),&UserContext::admin_fallback()).await);
    let t = disk(home.path(), "api");
    assert_eq!(t["runtime"]["provider"].as_str(), Some("openai_compat"));
    assert_eq!(t["runtime"]["fallback"].as_str(), Some("codex"));
    ok(&h
        .handle(
            "agents.create",
            json!({"name":"custom","model_preferred":"private-model"}),
            &UserContext::admin_fallback(),
        )
        .await);
    assert!(disk(home.path(), "custom").get("runtime").is_none());
}
#[tokio::test(flavor = "current_thread")]
async fn create_alignment_keeps_admin_and_product_policy_gates() {
    let home = tempfile::tempdir().unwrap();
    let h = MethodHandler::new(home.path().to_owned()).await;
    let mut employee = UserContext::admin_fallback();
    employee.role = UserRole::Employee;
    let refused = h
        .handle(
            "agents.create",
            json!({"name":"refused","model_preferred":"gpt-5.4"}),
            &employee,
        )
        .await;
    assert!(matches!(refused, WsFrame::Response { ok: false, .. }));
    assert!(!home.path().join("agents/refused/agent.toml").exists());
    std::fs::write(
        home.path().join("config.toml"),
        "[product]\nprofile='restricted'\n",
    )
    .unwrap();
    let _availability = crate::runtime_config::testing::AvailabilityGuard::at("gpt-5.4", false);
    let refused = h
        .handle(
            "agents.create",
            json!({"name":"policy","model_preferred":"gpt-5.4"}),
            &UserContext::admin_fallback(),
        )
        .await;
    assert!(matches!(refused, WsFrame::Response { ok: false, .. }));
    assert!(!home.path().join("agents/policy/agent.toml").exists());
}
#[test]
fn edited_template_document_uses_actual_final_render_and_keeps_comments_fallback_and_unknown_fields()
 {
    let home = tempfile::tempdir().unwrap();
    let _availability = crate::runtime_config::testing::AvailabilityGuard::at("gpt-5.4", true);
    let original = "# operator comment\n[agent]\nname='edited'\n[model]\npreferred='gpt-5.4'\n[runtime]\nprovider='claude'\nfallback='openai_compat'\n[custom]\nkeep='yes'\n";
    let (rendered, alignment) =
        super::super::templates_create::prepare_template_runtime(home.path(), original).unwrap();
    let table: toml::Table = rendered.parse().unwrap();
    assert!(alignment.aligned_provider().is_some());
    assert!(rendered.contains("# operator comment"));
    assert_eq!(table["runtime"]["provider"].as_str(), Some("codex"));
    assert_eq!(table["runtime"]["fallback"].as_str(), Some("openai_compat"));
    assert_eq!(table["custom"]["keep"].as_str(), Some("yes"));
}
#[test]
fn read_preflight_confident_mismatch_is_pure_and_unknown_or_api_compatible_passes() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("agent.toml");
    let original = "[model]\npreferred='gpt-5.4'\n[runtime]\nprovider='claude'\n";
    std::fs::write(&path, original).unwrap();
    let err =
        crate::runtime_config::ensure_model_provider("gpt-5.4", RuntimeType::Claude).unwrap_err();
    assert!(err.contains("runtime"));
    assert!(!err.contains("claude/login"));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    assert!(
        crate::runtime_config::ensure_model_provider("private-model", RuntimeType::Claude).is_ok()
    );
    assert!(
        crate::runtime_config::ensure_model_provider("gpt-5.4", RuntimeType::OpenAiCompat).is_ok()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn same_value_update_repairs_runtime_but_nonadmin_alignment_is_atomic_refusal() {
    let home = tempfile::tempdir().unwrap();
    let h = MethodHandler::new(home.path().to_owned()).await;
    let _availability = crate::runtime_config::testing::AvailabilityGuard::at("gpt-5.4", true);
    let admin = UserContext::admin_fallback();
    ok(&h
        .handle_agents_create_as(
            json!({"name":"update","model_preferred":"gpt-5.4"}),
            Some(&admin),
        )
        .await);
    let path = home.path().join("agents/update/agent.toml");
    let mut t = disk(home.path(), "update");
    t["runtime"]["provider"] = toml::Value::String("claude".into());
    std::fs::write(&path, toml::to_string(&t).unwrap()).unwrap();
    h.registry.write().await.scan().await.unwrap();
    let before = std::fs::read(&path).unwrap();
    let mut owner = admin.clone();
    owner.role = UserRole::Employee;
    let refused = h
        .handle_agents_update_as(
            json!({"agent_id":"update","preferred":"gpt-5.4"}),
            Some(&owner),
        )
        .await;
    assert!(matches!(refused, WsFrame::Response { ok: false, .. }));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let payload = ok(&h
        .handle_agents_update_as(
            json!({"agent_id":"update","preferred":"gpt-5.4"}),
            Some(&admin),
        )
        .await);
    assert_eq!(payload["runtime_provider_aligned"], "codex");
    assert_eq!(
        disk(home.path(), "update")["runtime"]["provider"].as_str(),
        Some("codex")
    );
}
#[test]
fn final_alignment_never_guesses_unknown_family_or_invalid_provider_and_keeps_displayname_skip() {
    use crate::runtime_config::{align_model_runtime, testing::AvailabilityGuard};
    let _available = AvailabilityGuard::at("gemini-2.5-pro", true);
    for provider in [toml::Value::String("typo".into()), toml::Value::Integer(42)] {
        let mut t = toml::toml! {[model] preferred="gpt-5.4"};
        t.insert(
            "runtime".into(),
            toml::Value::Table([(String::from("provider"), provider)].into_iter().collect()),
        );
        let before = t.clone();
        assert!(
            align_model_runtime(&mut t)
                .unwrap()
                .aligned_provider()
                .is_none()
        );
        assert_eq!(t, before);
    }
    let mut t = toml::toml! {[model] preferred="gemini-2.5-pro"};
    let before = t.clone();
    assert_eq!(
        align_model_runtime(&mut t).unwrap().skipped_reason(),
        Some("model_id_requires_display_name")
    );
    assert_eq!(t, before);
}

#[tokio::test(flavor = "current_thread")]
async fn actual_postlicense_template_writer_persists_aligned_runtime_and_retains_admin_gate() {
    let home = tempfile::tempdir().unwrap();
    let assets = tempfile::tempdir().unwrap();
    let h = MethodHandler::new(home.path().to_owned()).await;
    let _available = crate::runtime_config::testing::AvailabilityGuard::at("gpt-5.4", true);
    let assembled=crate::premium_templates::AssembledRole {
        role_id:"owned-role".into(),kind:crate::premium_templates::RoleKind::Worker,
        name:"template".into(),display_name:"Template".into(),trigger:"@Template".into(),reports_to:String::new(),summary:"Owned fixture".into(),
        soul_md:"# Owned fixture".into(),contract_toml:String::new(),
        agent_toml: r#"# retained
[agent]
name='template'
display_name='Template'
role='specialist'
status='active'
trigger='@Template'
reports_to=''
icon='🤖'
[model]
preferred='gpt-5.4'
fallback=''
account_pool=[]
[runtime]
provider='claude'
fallback='openai_compat'
[container]
timeout_ms=1800000
[heartbeat]
enabled=false
interval_seconds=3600
max_concurrent_runs=1
cron=''
[budget]
monthly_limit_cents=5000
warn_threshold_percent=80
hard_stop=true
[permissions]
can_create_agents=false
can_send_cross_agent=false
can_modify_own_skills=false
can_modify_own_soul=false
can_schedule_tasks=false
allowed_channels=['*']
[evolution]
enabled=false
"#.into(),extras_dir:None,
    };
    toml::from_str::<duduclaw_core::types::AgentConfig>(&assembled.agent_toml)
        .expect("post-license fixture must be a complete agent configuration");
    let params = json!({"name":"template"});
    let admin = UserContext::admin_fallback();
    let mut employee = admin.clone();
    employee.role = UserRole::Employee;
    assert!(matches!(
        h.create_assembled_template_agent(
            &params,
            &employee,
            assets.path(),
            "owned-role",
            &assembled
        )
        .await,
        WsFrame::Response { ok: false, .. }
    ));
    assert!(!home.path().join("agents/template/agent.toml").exists());
    let payload = ok(&h
        .create_assembled_template_agent(&params, &admin, assets.path(), "owned-role", &assembled)
        .await);
    assert_eq!(payload["runtime_provider_auto_aligned"], "codex");
    {
        let registry = h.registry.read().await;
        let loaded = registry
            .get("template")
            .expect("written template must hot-load");
        assert_eq!(loaded.config.model.preferred, "gpt-5.4");
        assert_eq!(loaded.config.runtime.provider.as_deref(), Some("codex"));
        assert_eq!(
            loaded.config.runtime.fallback.as_deref(),
            Some("openai_compat")
        );
    }
    assert_eq!(
        disk(home.path(), "template")["runtime"]["provider"].as_str(),
        Some("codex")
    );
    assert!(
        std::fs::read_to_string(home.path().join("agents/template/agent.toml"))
            .unwrap()
            .contains("# retained")
    );
}
