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

#[test]
fn google_chat_registration_matches_gmail_private_read_contract() {
    for tool in ["google_chat_spaces", "google_chat_messages"] {
        let def = duduclaw_core::mcp_tool_defs::tools().find(|def| def.name == tool).unwrap();
        let schema = duduclaw_core::mcp_tool_defs::build_tool_schema(def);
        assert_eq!(schema["inputSchema"]["properties"]["limit"]["type"], "integer");
        assert_eq!(schema["inputSchema"]["properties"]["limit"]["minimum"], 1);
        assert_eq!(schema["inputSchema"]["properties"]["limit"]["maximum"], if tool == "google_chat_spaces" { 50 } else { 100 });

        assert!(duduclaw_core::google_identity::NATIVE_GOOGLE_TOOLS.contains(&tool));
        assert_eq!(crate::mcp_auth::tool_requires_scope(tool), crate::mcp_auth::tool_requires_scope("gmail_read"));
        assert_eq!(duduclaw_core::effect_of_builtin(tool), duduclaw_core::effect_of_builtin("gmail_read"));
        assert_eq!(duduclaw_core::tool_effect::reads_private(tool), Some(true));
        assert_eq!(duduclaw_core::tool_effect::egress_capable(tool), duduclaw_core::tool_effect::egress_capable("gmail_read"));
    }
}

#[tokio::test]
async fn google_chat_listing_and_call_share_employee_denial() {
    for tool in ["google_chat_spaces", "google_chat_messages"] {
        let home = fixture(&format!("[capabilities]\ndenied_tools=['{tool}']"));
        let call = authenticate_with_key_call(KEY, home.path()).unwrap();
        let principal = call.principal().clone();
        let names = with_authenticated_call(call, async {
            crate::mcp::visible_tool_names(&principal, home.path(), "different-process-agent").await
        }).await;
        assert!(!names.contains(&tool));
        assert!(names.contains(&"gmail_read"));
        let dispatcher = dispatcher(home.path()).await;
        let call = authenticate_with_key_call(KEY, home.path()).unwrap();
        let result = with_authenticated_call(call, dispatcher.dispatch_tool_call(
            &principal, &namespace(), &json!({"name":tool,"arguments":{"space":"spaces/AAAA"}}), &json!(91),
        )).await;
        assert_eq!(result["error"]["code"], -32003, "{result}");
        assert!(result["error"]["message"].as_str().unwrap().contains("denied_tools"));
    }
}

#[tokio::test]
async fn google_chat_tools_list_uses_same_scope_and_identity_receipt_as_gmail() {
    let home = fixture("");
    duduclaw_gateway::google_workspace::set_chat_read_enabled(home.path(), true).unwrap();
    let call = authenticate_with_key_call(KEY, home.path()).unwrap();
    let principal = call.principal().clone();
    let names = with_authenticated_call(call, async {
        crate::mcp::visible_tool_names(&principal, home.path(), "different-process-agent").await
    }).await;
    for tool in ["gmail_read", "google_chat_spaces", "google_chat_messages"] {
        assert!(names.contains(&tool), "{tool}: {names:?}");
    }
    let names = crate::mcp::visible_tool_names(&principal, home.path(), "different-process-agent").await;
    for tool in ["gmail_read", "google_chat_spaces", "google_chat_messages"] {
        assert!(!names.contains(&tool), "missing authenticated receipt: {tool}");
    }
}

#[tokio::test]
async fn rm159_write_listing_and_dispatch_match_explicit_employee_scopes() {
    let tools = ["docs_create", "docs_replace_text", "sheets_create", "sheets_update", "calendar_update_event", "gmail_send_draft", "calendar_delete_event"];
    for write in [false, true] {
        let home = fixture("[capabilities]\nallowed_tools=['mcp__duduclaw__*']");
        let path = home.path().join("config.toml");
        let config = std::fs::read_to_string(&path).unwrap()
            .replace("google_workspace=true", "google_workspace=true\ngoogle_credential_path='oauth'");
        std::fs::write(&path, config).unwrap();
        if write {
            let path = home.path().join("config.toml");
            let config = std::fs::read_to_string(&path).unwrap().replace("scopes=['google:read']", "scopes=['google:read','google:write']");
            std::fs::write(path, config).unwrap();
        }
        let call = authenticate_with_key_call(KEY, home.path()).unwrap();
        let principal = call.principal().clone();
        let names = with_authenticated_call(call, async {
            crate::mcp::visible_tool_names(&principal, home.path(), "different-process-agent").await
        }).await;
        for tool in tools {
            assert_eq!(names.contains(&tool), write, "{tool}: {names:?}");
            assert_eq!(crate::mcp_auth::tool_requires_scope(tool), Some(crate::mcp_auth::Scope::GoogleWrite));
            let effect = match tool {
                "calendar_update_event" | "gmail_send_draft" => duduclaw_core::ToolEffect::Send,
                "calendar_delete_event" => duduclaw_core::ToolEffect::Delete,
                _ => duduclaw_core::ToolEffect::Modify,
            };
            assert_eq!(duduclaw_core::effect_of(tool), effect);
            if !write {
                let dispatcher = dispatcher(home.path()).await;
                let call = authenticate_with_key_call(KEY, home.path()).unwrap();
                // Valid inputs reach the scope gate instead of failing argument validation first.
                let arguments = match tool {
                    "docs_create" | "sheets_create" => json!({"title":"test"}),
                    "docs_replace_text" => json!({"document_id":"doc","find":"old","replace":"new"}),
                    "sheets_update" => json!({"spreadsheet_id":"sheet","range":"A1","values":[["cell"]]}),
                    "calendar_update_event" => json!({"event_id":"event","summary":"test"}),
                    "gmail_send_draft" => json!({"draft_id":"draft"}),
                    "calendar_delete_event" => json!({"event_id":"event"}),
                    _ => unreachable!(),
                };
                let result = with_authenticated_call(call, dispatcher.dispatch_tool_call(
                    &principal, &namespace(), &json!({"name":tool,"arguments":arguments}), &json!(159),
                )).await;
                // RM-134 hides tools outside the authenticated employee scope.
                assert_eq!(result["error"]["code"], -32601, "{tool}: {result}");
            }
        }
    }
}

#[tokio::test]
async fn rm159_all_calendar_updates_use_the_existing_send_approval_policy() {
    let home = fixture("[capabilities]\nallowed_tools=['mcp__duduclaw__*']\n[[capabilities.action_rules]]\neffect='send'\nverdict='block'");
    let path = home.path().join("config.toml");
    let config = std::fs::read_to_string(&path).unwrap().replace("scopes=['google:read']", "scopes=['google:read','google:write']");
    std::fs::write(path, config).unwrap();
    let dispatcher = dispatcher(home.path()).await;
    for args in [json!({"event_id":"e","summary":"s"}),
        json!({"event_id":"e","summary":"s","notify_attendees":false}),
        json!({"event_id":"e","summary":"s","notify_attendees":true}),
        json!({"event_id":"e","attendees":["a@example.test"]})] {
        let call = authenticate_with_key_call(KEY, home.path()).unwrap();
        let principal = call.principal().clone();
        let result = with_authenticated_call(call, dispatcher.dispatch_tool_call(
            &principal, &namespace(), &json!({"name":"calendar_update_event","arguments":args}), &json!(159),
        )).await;
        assert_eq!(result["error"]["code"], -32003, "{result}");
        assert!(result["error"]["message"].as_str().unwrap().contains("action_rules"), "{result}");
    }
}

#[tokio::test]
async fn drive_writes_are_refused_by_dispatch_and_hidden_from_listing() {
    let home = fixture("[capabilities]\nallowed_tools=['mcp__duduclaw__*']");
    let config = home.path().join("config.toml");
    let raw = std::fs::read_to_string(&config).unwrap().replace(
        "scopes=['google:read']", "scopes=['google:read','google:write']");
    std::fs::write(config, raw).unwrap();
    let dispatcher = dispatcher(home.path()).await;
    for tool in duduclaw_core::google_identity::DRIVE_WRITE_TOOLS {
        let call = authenticate_with_key_call(KEY, home.path()).unwrap();
        let principal = call.principal().clone();
        let result = with_authenticated_call(call, dispatcher.dispatch_tool_call(
            &principal, &namespace(), &json!({"name":tool,"arguments":{}}), &json!(1),
        )).await;
        assert_eq!(result["error"]["code"], -32003, "{tool}: {result}");
        assert_eq!(result["error"]["message"], "此連線不支援雲端硬碟寫入。", "{tool}");
    }
    for external in [false, true] {
        let mut principal = crate::mcp::test_principal(external);
        principal.scopes = vec![crate::mcp_auth::Scope::Admin, crate::mcp_auth::Scope::GoogleRead,
            crate::mcp_auth::Scope::GoogleWrite].into_iter().collect();
        let response = crate::mcp::handle_tools_list_for_agent(
            &json!(2), &principal, home.path(), "employee-a",
        ).await;
        let listed = crate::mcp::advertised_tool_names(&response);
        for tool in duduclaw_core::google_identity::DRIVE_WRITE_TOOLS {
            assert!(!listed.iter().any(|name| name.as_str() == *tool), "{tool}");
        }
    }
}

fn drive_write_fixture(create: bool, move_: bool, backend: &str, capabilities: &str) -> tempfile::TempDir {
    use duduclaw_gateway::google_workspace::drive_write_policy as policy;
    let home = fixture(capabilities);
    let mut config: toml::Value = std::fs::read_to_string(home.path().join("config.toml"))
        .unwrap().parse().unwrap();
    let integrations = config.as_table_mut().expect("fixture config must be a table")
        .entry("integrations").or_insert_with(|| toml::Value::Table(Default::default()))
        .as_table_mut().expect("fixture integrations must be a table");
    integrations.insert("google_credential_path".into(), toml::Value::String(backend.into()));
    if backend == "apps_script" {
        integrations.insert("google_apps_script".into(), toml::Value::Table(Default::default()));
    }
    let mut settings = policy::DriveWritePolicy::all_off();
    settings.create = create;
    settings.move_ = move_;
    let ack = policy::sign(&settings, "admin", "admin@example.test", chrono::Utc::now());
    config.as_table_mut().unwrap().insert("google".into(), toml::Value::Table(
        [("drive_write".into(), policy::render_section(&settings, &ack))].into_iter().collect(),
    ));
    std::fs::write(home.path().join("config.toml"), toml::to_string(&config).unwrap()).unwrap();
    home
}
fn drive_principal() -> Principal {
    let mut p = crate::mcp::test_principal(false);
    p.client_id = "employee-a".into();
    p.scopes = [crate::mcp_auth::Scope::Admin, crate::mcp_auth::Scope::GoogleRead,
        crate::mcp_auth::Scope::GoogleWrite].into_iter().collect();
    p
}

#[tokio::test]
async fn drive_write_listing_admission_and_snapshot_follow_policy() {
    use duduclaw_core::{ProcessLane, google_identity::DRIVE_WRITE_TOOLS};
    for (create, move_, backend, capabilities) in [
        (false, false, "oauth", "[capabilities]\nallowed_tools=['mcp__duduclaw__*']"),
        (true, false, "oauth", "[capabilities]\nallowed_tools=['mcp__duduclaw__*']"),
        (true, true, "oauth", "[capabilities]\nallowed_tools=['mcp__duduclaw__*']"),
        (true, false, "apps_script", "[capabilities]\nallowed_tools=['mcp__duduclaw__*']"),
        (true, false, "oauth", "[capabilities]\ndenied_tools=['drive_*']"),
        (true, false, "oauth", "[capabilities]\nallowed_tools=['drive_read']"),
        (true, false, "oauth", "[[capabilities.action_rules]]\neffect='publish'\nverdict='block'"),
    ] {
        let home = drive_write_fixture(create, move_, backend, capabilities);
        let principal = drive_principal();
        let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let captured = requests.clone();
        let _transport = crate::mcp::google_execute_test_transport::install(home.path(), Arc::new(move |_| {
            captured.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err("gateway unavailable".into())
        }));
        let dispatcher = dispatcher(home.path()).await;
        for lane in [ProcessLane::Normal, ProcessLane::Explore] {
            let listed = crate::mcp::visible_tools_in_lane(&principal, home.path(), "employee-a", true, &lane).await;
            for tool in DRIVE_WRITE_TOOLS {
                let refusal = crate::mcp::drive_write_refusal(home.path(), "employee-a", tool, &principal, &lane);
                assert_eq!(listed.iter().any(|t| t.name == *tool), refusal.is_none(), "{backend} {tool}: {refusal:?}");
                if lane == ProcessLane::Normal && refusal.is_some() {
                    let result = dispatcher.dispatch_tool_call(&principal, &namespace(),
                        &json!({"name":tool,"arguments":{}}), &json!(131)).await;
                    assert_eq!(result["error"]["message"], refusal.unwrap(), "{result}");
                }
                if move_ && *tool == "drive_move_file" && lane == ProcessLane::Normal {
                    assert_eq!(crate::mcp::drive_write_refusal(home.path(), "employee-a", tool, &principal, &lane).as_deref(), Some("這項操作需要先限定可使用的資料夾，請管理員設定。"));
                }
            }
        }
        assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 0);
    }
    let home = drive_write_fixture(false, false, "oauth", "[capabilities]\nallowed_tools=['mcp__duduclaw__*']");
    let principal = drive_principal();
    let before = crate::mcp::visible_tool_snapshot(&principal, home.path(), "employee-a").await;
    let enabled = drive_write_fixture(true, false, "oauth", "[capabilities]\nallowed_tools=['mcp__duduclaw__*']");
    std::fs::copy(enabled.path().join("config.toml"), home.path().join("config.toml")).unwrap();
    let after = crate::mcp::visible_tool_snapshot(&principal, home.path(), "employee-a").await;
    assert_ne!(before, after, "the existing listChanged watcher must see the change");
    for tool in ["drive_create_file", "drive_copy_file"] {
        assert!(!before.iter().any(|n| n == tool));
        assert!(after.iter().any(|n| n == tool));
    }
}

#[tokio::test]
async fn enabled_drive_dispatch_prepares_at_gateway_and_refuses_gateway_failure() {
    let home = drive_write_fixture(true, false, "oauth", "[capabilities]\nallowed_tools=['mcp__duduclaw__*']");
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured = requests.clone();
    let _transport = crate::mcp::google_execute_test_transport::install(home.path(), Arc::new(move |wire| {
        captured.lock().unwrap().push(wire.clone());
        Err("gateway down; no fallback".into())
    }));
    let principal = drive_principal();
    let result = dispatcher(home.path()).await.dispatch_tool_call(&principal, &namespace(),
        &json!({"name":"drive_create_file","arguments":{"name":"test.txt","text":"test"}}), &json!(131)).await;
    assert_eq!(result["error"]["message"], "gateway down; no fallback", "{result}");
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["route"], "/api/internal/google-execute");
    assert_eq!(requests[0]["body"]["kind"], "employee_prepare");
}

#[tokio::test]
async fn drive_write_approval_failure_after_prepare_never_executes() {
    let home = drive_write_fixture(true, false, "oauth",
        "[capabilities]\nallowed_tools=['mcp__duduclaw__*']\napproval_required_tools=['drive_create_file']");
    // An unavailable approval store must fail closed after preparation.
    std::fs::create_dir(home.path().join("approvals.db")).unwrap();
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured = requests.clone();
    let _transport = crate::mcp::google_execute_test_transport::install(home.path(), Arc::new(move |wire| {
        captured.lock().unwrap().push(wire.clone());
        assert_eq!(wire["body"]["kind"], "employee_prepare", "execute must not run before CLI approval");
        Ok(json!({"ok":true,"result":{"approval_binding_digest":"a".repeat(64)}}))
    }));
    let principal = drive_principal();
    let result = dispatcher(home.path()).await.dispatch_tool_call(&principal, &namespace(),
        &json!({"name":"drive_create_file","arguments":{"name":"test.txt","text":"test"}}), &json!(131)).await;
    assert_eq!(result["error"]["code"], -32003, "{result}");
    assert_eq!(requests.lock().unwrap().len(), 1);
}
