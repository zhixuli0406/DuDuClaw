//! Dedicated Google keys must pass the real shared dispatcher, not a parallel
//! policy implementation. These tests stop at the named gate before any API.
use super::*;
use crate::mcp_auth::{authenticate_with_key_call, with_authenticated_call};
use serde_json::json;

const KEY: &str = "ddc_dev_0123456789abcdef0123456789abcdef";
fn fixture(capabilities: &str) -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join("agents/employee-a")).unwrap();
    std::fs::write(
        home.path().join("agents/employee-a/agent.toml"),
        format!("[agent]\nname='employee-a'\n{capabilities}\n"),
    )
    .unwrap();
    std::fs::write(home.path().join("config.toml"),format!(
        "[integrations]\ngoogle_workspace=true\n[mcp_keys.{KEY}]\nclient_id='employee-a'\nscopes=['google:read']\nis_external=true\ncreated_at='{}'\n",
        chrono::Utc::now().to_rfc3339(),
    )).unwrap();
    home
}
async fn dispatcher(home: &Path) -> McpDispatcher {
    McpDispatcher::new(
        home.to_path_buf(),
        reqwest::Client::new(),
        Arc::new(SqliteMemoryEngine::new(&home.join("memory.db")).unwrap()),
        "different-process-agent".into(),
        Arc::new(crate::odoo_pool::OdooConnectorPool::default()),
        RateLimiter::new(),
        DailyQuota::new(),
    )
}
fn namespace() -> NamespaceContext {
    NamespaceContext {
        write_namespace: "external/employee-a".into(),
        read_namespaces: vec!["external/employee-a".into()],
    }
}

#[tokio::test]
async fn dedicated_google_runs_denied_allowlist_action_policy_and_task_grant_gates() {
    for (capabilities, expected) in [
        (
            "[capabilities]\ndenied_tools=['gmail_search']",
            "denied_tools",
        ),
        (
            "[capabilities]\nallowed_tools=['gmail_read']",
            "allowed_tools",
        ),
        (
            "[[capabilities.action_rules]]\neffect='read'\nverdict='block'",
            "action_rules",
        ),
        (
            "[[capabilities.policy]]\ntool='gmail_search'\neffect='forbid'",
            "Denied by policy",
        ),
        ("[capabilities]\nscoped_tools=['gmail_search']", "授權"),
    ] {
        let home = fixture(capabilities);
        let parsed: PolicyOnlyConfig = toml::from_str(
            &std::fs::read_to_string(home.path().join("agents/employee-a/agent.toml")).unwrap(),
        )
        .unwrap();
        assert!(
            !parsed.capabilities.action_rules.malformed,
            "invalid action-rule fixture: {capabilities}"
        );
        let dispatcher = dispatcher(home.path()).await;
        let call = authenticate_with_key_call(KEY, home.path()).unwrap();
        let principal = call.principal().clone();
        let result = with_authenticated_call(
            call,
            dispatcher.dispatch_tool_call(
                &principal,
                &namespace(),
                &json!({"name":"gmail_search","arguments":{"query":"fixture"}}),
                &json!(1),
            ),
        )
        .await;
        assert_eq!(result["error"]["code"], -32003, "{capabilities}: {result}");
        assert!(
            result["error"]["message"]
                .as_str()
                .unwrap()
                .contains(expected),
            "{capabilities}: {result}"
        );
    }
}

#[tokio::test]
async fn dedicated_google_cannot_skip_actionguard_approval_when_broker_is_unavailable() {
    let home = fixture("[capabilities]\napproval_required_tools=['gmail_search']");
    std::fs::create_dir(home.path().join("approvals.db")).unwrap();
    let dispatcher = dispatcher(home.path()).await;
    let call = authenticate_with_key_call(KEY, home.path()).unwrap();
    let principal = call.principal().clone();
    let result = with_authenticated_call(
        call,
        dispatcher.dispatch_tool_call(
            &principal,
            &namespace(),
            &json!({"name":"gmail_search","arguments":{"query":"fixture"}}),
            &json!(2),
        ),
    )
    .await;
    assert_eq!(result["error"]["code"], -32003, "{result}");
    assert!(
        result["error"]["message"]
            .as_str()
            .unwrap()
            .contains("審批系統"),
        "{result}"
    );
}

#[tokio::test]
async fn dedicated_google_listing_matches_scope_employee_denial_and_task_grants() {
    let home = fixture(
        "[capabilities]\nallowed_tools=['mcp__duduclaw__*']\ndenied_tools=['gmail_read']\nscoped_tools=['sheets_read']",
    );
    let call = authenticate_with_key_call(KEY, home.path()).unwrap();
    let principal = call.principal().clone();
    let names = with_authenticated_call(call, async {
        crate::mcp::visible_tool_names(&principal, home.path(), "different-process-agent").await
    })
    .await;
    assert!(names.contains(&"gmail_search"), "{names:?}");
    for tool in [
        "gmail_read",
        "gmail_create_draft",
        "sheets_read",
        "github_status",
        "os_open",
        "mail_read",
    ] {
        assert!(!names.contains(&tool), "{tool}: {names:?}");
    }
    let names =
        crate::mcp::visible_tool_names(&principal, home.path(), "different-process-agent").await;
    assert!(
        !names.contains(&"gmail_search"),
        "missing receipt: {names:?}"
    );
}

// The following SOURCE-ONLY host-pipeline tests use the real dispatcher,
// broker and encrypted redaction vault, but a no-socket metadata transport.
// They do NOT claim live-gateway edition validation; actual Router tests in
// gateway/account_selection_tests.rs cover that separate boundary. No CLI
// fixture may record an Enterprise edition to hide a process-local gap.
#[derive(Default)]
struct MetadataSpy {
    requests: Vec<Value>,
    stamp: Option<duduclaw_core::google_identity::GoogleSelectionStamp>,
    replace_account: bool,
    home_account: bool,
    execute_requests: Vec<Value>,
    _execute_guard: Option<crate::mcp::google_execute_test_transport::Guard>,
    on_bind: Option<Arc<dyn Fn() + Send + Sync>>,
}
fn metadata_fixture(
    home: &Path,
) -> (
    Arc<std::sync::Mutex<MetadataSpy>>,
    crate::mcp::google_selection_client::test_transport::Guard,
) {
    use duduclaw_core::google_identity::*;
    let config = home.join("config.toml");
    let mut raw = std::fs::read_to_string(&config).unwrap();
    raw.push_str(&format!("\n[mcp_keys.ddc_dev_11111111111111111111111111111111]\nclient_id='gateway-internal'\nscopes=['admin']\nis_external=false\ncreated_at='{}'\n",chrono::Utc::now().to_rfc3339()));
    std::fs::write(config, raw).unwrap();
    duduclaw_core::ensure_identity_key(home).unwrap();
    let spy = Arc::new(std::sync::Mutex::new(MetadataSpy::default()));
    let capture = spy.clone();
    let guard = crate::mcp::google_selection_client::test_transport::install(
        home,
        Arc::new(move |body| {
            let request: GoogleSelectionRequest = serde_json::from_value(body.clone())
                .map_err(|_| "invalid metadata fixture request")?;
            let mut state = capture.lock().unwrap();
            state.requests.push(body.clone());
            match request {
                GoogleSelectionRequest::SelectGoogleAccount { arguments, .. } => {
                    let mut stamp = GoogleSelectionStamp {
                        opaque_id: uuid::Uuid::new_v4().to_string(),
                        expires_at: duduclaw_gateway::google_personal::authority::now()
                            + GOOGLE_SELECTION_TTL_SECS,
                        original_arguments_digest: google_arguments_digest(&arguments),
                        final_arguments_digest: None,
                        approval: GoogleAccountApproval {
                            selected_identity: ApprovedGoogleIdentity::Personal,
                            selected_google_account: "alice-google@unit.test".into(),
                            selected_credential_owner: "unit-owner".into(),
                            selected_person_login: "login@unit.test".into(),
                            requester_kind: "unknown".into(),
                            configured_home_fallback: true,
                            home_fallback_authorized: false,
                            home_connection: None,
                            home_source: None,
                            account_binding_digest: "b".repeat(64),
                        },
                    };
                    if state.home_account {
                        stamp.approval.selected_identity =
                            ApprovedGoogleIdentity::HomeOAuthFallback;
                        stamp.approval.selected_google_account.clear();
                        stamp.approval.selected_credential_owner.clear();
                        stamp.approval.selected_person_login.clear();
                        stamp.approval.home_fallback_authorized = true;
                        stamp.approval.home_connection = Some(GoogleHomeConnectionApproval {
                            credential_id: uuid::Uuid::new_v4().to_string(),
                            logical_epoch: 1,
                            source_generation: uuid::Uuid::new_v4().to_string(),
                            original_selector: "owner".into(),
                            original_unavailable: "not_connected".into(),
                        });
                    }
                    state.stamp = Some(stamp.clone());
                    Ok(json!({"ok":true,"selection":stamp}))
                }
                GoogleSelectionRequest::BindGoogleArguments {
                    opaque_id,
                    original_arguments_digest,
                    account_binding_digest,
                    arguments,
                    ..
                } => {
                    let replace = state.replace_account;
                    let callback = state.on_bind.take();
                    let stamp = state.stamp.as_mut().ok_or("fixture has no selection")?;
                    if stamp.opaque_id != opaque_id
                        || stamp.original_arguments_digest != original_arguments_digest
                        || stamp.approval.account_binding_digest != account_binding_digest
                    {
                        return Err("fixture selection mismatch".into());
                    }
                    let digest = google_arguments_digest(&arguments);
                    if stamp
                        .final_arguments_digest
                        .as_ref()
                        .is_some_and(|d| *d != digest)
                    {
                        return Err("fixture final args changed".into());
                    }
                    stamp.final_arguments_digest = Some(digest);
                    let mut returned = stamp.clone();
                    if replace {
                        returned.approval.selected_google_account = "bob-google@unit.test".into();
                    }
                    drop(state);
                    if let Some(callback) = callback {
                        callback();
                    }
                    Ok(json!({"ok":true,"selection":returned}))
                }
            }
        }),
    );
    let weak = Arc::downgrade(&spy);
    let execute_guard = crate::mcp::google_execute_test_transport::install(
        home,
        Arc::new(move |body| {
            let data = weak.upgrade().ok_or("owned metadata fixture dropped")?;
            data.lock().unwrap().execute_requests.push(body.clone());
            Err("封閉送出介面仍在驗收中；這個owned fixture沒有socket。".into())
        }),
    );
    spy.lock().unwrap()._execute_guard = Some(execute_guard);
    (spy, guard)
}
const ALIAS: &str = "<REDACT:EMAIL:0123456789abcdef0123456789abcdef>";
fn alias_layer(home: &Path) -> Arc<crate::mcp_redaction::McpRedactionLayer> {
    let mut cfg = duduclaw_redaction::RedactionConfig::default();
    cfg.enabled = true;
    cfg.profiles = vec![];
    cfg.tool_egress.insert(
        "gmail_read".into(),
        duduclaw_redaction::ToolEgressRule {
            restore_args: duduclaw_redaction::RestoreArgsMode::Restore,
            audit_reveal: false,
        },
    );
    let manager = Arc::new(
        duduclaw_redaction::RedactionManager::open(
            cfg,
            duduclaw_redaction::ManagerPaths::under_home(home),
        )
        .unwrap(),
    );
    manager
        .vault()
        .insert_mapping(
            ALIAS,
            "unit-message-a",
            "external",
            Some("unit-session"),
            "EMAIL",
            "unit",
            &duduclaw_redaction::RestoreScope::AllScopes { scopes: vec![] },
            false,
            24,
        )
        .unwrap();
    Arc::new(crate::mcp_redaction::McpRedactionLayer {
        manager,
        agent_id: "ignored-process-agent".into(),
        session_id: "unit-session".into(),
    })
}
async fn pending_google_ask(
    home: &Path,
    layer: Arc<crate::mcp_redaction::McpRedactionLayer>,
) -> (
    duduclaw_gateway::approval::ApprovalBroker,
    duduclaw_gateway::approval::ApprovalRecord,
    tokio::task::JoinHandle<Value>,
) {
    let dispatcher = dispatcher(home).await.with_redaction(Some(layer));
    let call = authenticate_with_key_call(KEY, home).unwrap();
    let principal = call.principal().clone();
    let task = tokio::spawn(async move {
        with_authenticated_call(call,dispatcher.dispatch_tool_call(&principal,&namespace(),&json!({"name":"gmail_read","_meta":{"selection_stamp":"model-metadata-cannot-authorize"},"host_google_account":{"selected_google_account":"forged-bob@unit.test"},"arguments":{"message_id":ALIAS,"agent_id":"forged-agent","namespace":"forged-ns","selection_stamp":"model-cannot-authorize"}}),&json!(73))).await
    });
    let broker = duduclaw_gateway::approval::ApprovalBroker::open(home).unwrap();
    let mut record = None;
    for _ in 0..400 {
        if let Some(r) = broker.list_pending(None).await.unwrap().into_iter().next() {
            record = Some(r);
            break;
        }
        if task.is_finished() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    (
        broker,
        record.expect("account selection must precede the real Policy Ask"),
        task,
    )
}
fn ask_fixture() -> tempfile::TempDir {
    fixture(
        "[[capabilities.policy]]\ntool='gmail_read'\neffect='ask'\n[google]\nidentity='owner'\nrequesters='anyone'\nfallback='home'",
    )
}

#[tokio::test]
async fn real_host_policy_ask_precedes_trusted_alias_restore_and_binds_private_stamp() {
    let home = ask_fixture();
    let (spy, _transport) = metadata_fixture(home.path());
    let layer = alias_layer(home.path());
    let (broker, record, task) = pending_google_ask(home.path(), layer).await;
    assert!(record.summary.contains("alice-google@unit.test"));
    assert!(record.summary.contains("未核准改用公司帳號"));
    assert_eq!(
        record.payload["host_google_account"]["requester_kind"],
        "unknown"
    );
    assert_eq!(record.payload["arguments"]["message_id"], ALIAS);
    assert!(!record.payload.to_string().contains("unit-message-a"));
    {
        let data = spy.lock().unwrap();
        assert_eq!(data.requests.len(), 1, "no bind/restore before Ask");
        let stamp = data.stamp.as_ref().unwrap();
        assert!(!record.payload.to_string().contains(&stamp.opaque_id));
        assert!(data.requests[0]["arguments"].get("agent_id").is_none());
        assert!(data.requests[0]["arguments"].get("namespace").is_none());
    }
    broker
        .decide(&record.id, true, "unit-approver")
        .await
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(30), task)
        .await
        .unwrap()
        .unwrap();
    assert!(
        result.get("error").is_none(),
        "host gates should reach the explicitly closed effect handler"
    );
    assert_eq!(result["result"]["isError"], true);
    let data = spy.lock().unwrap();
    let binds: Vec<_> = data
        .requests
        .iter()
        .filter(|r| r["purpose"] == "bind_google_arguments")
        .collect();
    assert_eq!(binds.len(), 2);
    for bind in binds {
        assert_eq!(bind["arguments"]["message_id"], "unit-message-a");
        assert_ne!(bind["opaque_id"], "model-cannot-authorize");
        assert_eq!(bind["opaque_id"], data.stamp.as_ref().unwrap().opaque_id);
    }
}
#[tokio::test]
async fn real_host_rejects_mapping_replacement_during_policy_ask_before_any_final_bind() {
    let home = ask_fixture();
    let (spy, _transport) = metadata_fixture(home.path());
    let layer = alias_layer(home.path());
    let (broker, record, task) = pending_google_ask(home.path(), layer.clone()).await;
    layer
        .manager
        .vault()
        .insert_mapping(
            ALIAS,
            "unit-message-b",
            "external",
            Some("unit-session"),
            "EMAIL",
            "unit",
            &duduclaw_redaction::RestoreScope::AllScopes { scopes: vec![] },
            false,
            24,
        )
        .unwrap();
    broker
        .decide(&record.id, true, "unit-approver")
        .await
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(30), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result["error"]["code"], -32003);
    assert_eq!(spy.lock().unwrap().requests.len(), 1);
}
#[tokio::test]
async fn real_host_cannot_accept_another_account_in_the_post_restore_bind_reply() {
    let home = ask_fixture();
    let (spy, _transport) = metadata_fixture(home.path());
    let layer = alias_layer(home.path());
    let (broker, record, task) = pending_google_ask(home.path(), layer).await;
    spy.lock().unwrap().replace_account = true;
    broker
        .decide(&record.id, true, "unit-approver")
        .await
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(30), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result["error"]["code"], -32003);
    assert_eq!(
        spy.lock()
            .unwrap()
            .requests
            .iter()
            .filter(|r| r["purpose"] == "select_google_account")
            .count(),
        1,
        "a changed account must not trigger recapture"
    );
}
#[tokio::test]
async fn real_host_rechecks_original_mapping_after_final_bind_await() {
    let home = ask_fixture();
    let (spy, _transport) = metadata_fixture(home.path());
    let layer = alias_layer(home.path());
    let (broker, record, task) = pending_google_ask(home.path(), layer.clone()).await;
    spy.lock().unwrap().on_bind = Some(Arc::new(move || {
        layer
            .manager
            .vault()
            .insert_mapping(
                ALIAS,
                "unit-message-b",
                "external",
                Some("unit-session"),
                "EMAIL",
                "unit",
                &duduclaw_redaction::RestoreScope::AllScopes { scopes: vec![] },
                false,
                24,
            )
            .unwrap();
    }));
    broker
        .decide(&record.id, true, "unit-approver")
        .await
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(30), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result["error"]["code"], -32003);
}

#[tokio::test]
async fn real_home_logical_ask_restore_and_private_execute_stamp_remain_one_host_pipeline() {
    let home = ask_fixture();
    let (spy, _transport) = metadata_fixture(home.path());
    spy.lock().unwrap().home_account = true;
    let layer = alias_layer(home.path());
    let (broker, record, task) = pending_google_ask(home.path(), layer).await;
    assert!(record.summary.contains("公司共用"));
    assert!(record.summary.contains("主體未驗證"));
    assert!(!record.summary.contains("alice-google@unit.test"));
    assert_eq!(
        record.payload["host_google_account"]["selected_identity"],
        "home_oauth_fallback"
    );
    assert_eq!(spy.lock().unwrap().requests.len(), 1);
    broker
        .decide(&record.id, true, "unit-approver")
        .await
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(30), task)
        .await
        .unwrap()
        .unwrap();
    assert!(result.get("error").is_none());
    assert_eq!(result["result"]["isError"], true);
    let data = spy.lock().unwrap();
    assert_eq!(data.execute_requests.len(), 1);
    let body = &data.execute_requests[0];
    assert_eq!(
        body["selection_stamp"],
        data.stamp.as_ref().unwrap().opaque_id
    );
    assert_eq!(body["arguments"]["message_id"], "unit-message-a");
    assert_ne!(body["selection_stamp"], "model-cannot-authorize");
    assert!(body.get("host_google_account").is_none());
}
