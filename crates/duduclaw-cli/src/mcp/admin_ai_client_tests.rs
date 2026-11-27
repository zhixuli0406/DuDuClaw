use super::*;
use std::{cell::{Cell, RefCell}, sync::Arc};

pub(super) struct TestTransport {
    pub(super) credentials: Option<Credentials>,
    proof: Cell<Proof>,
    fail: Cell<bool>,
    calls: RefCell<Vec<Value>>,
}
impl TestTransport {
    fn new(proof: Proof) -> Self {
        Self { credentials: Some(who()), proof: Cell::new(proof), fail: Cell::new(false), calls: RefCell::new(Vec::new()) }
    }
    pub(super) fn fetch(&self, body: Value) -> Result<Vec<u8>, &'static str> {
        self.calls.borrow_mut().push(body.clone());
        if self.fail.get() { return Err(UNAVAILABLE); }
        let proof = self.proof.get();
        let value = match body["op"].as_str() {
            Some("proof") => json!({"ok":true,"read":proof.read,"write":proof.write}),
            Some("read") if proof.read => json!({"ok":true,"agents":[],"not_available":[]}),
            Some("propose") if proof.write => json!({
                "message":"已送出確認卡，請到儀表板收件匣確認；十分鐘內有效", "item_count":1}),
            _ => return Err(UNAVAILABLE),
        };
        Ok(serde_json::to_vec(&value).unwrap())
    }
}
fn who() -> Credentials {
    Credentials { agent: "avatar".into(), context: "a".repeat(64), key: "test-key".into(), token: "test-token".into() }
}
fn principal() -> crate::mcp_auth::Principal {
    let mut principal = crate::mcp::tools_list::test_principal(false);
    principal.client_id = "avatar".into();
    principal
}
fn home(enabled: bool) -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    crate::mcp::tests::create_test_agent(&home.path().join("agents"), "avatar", "");
    std::fs::write(home.path().join("config.toml"), format!("[admin_ai]\nenabled={enabled}\n")).unwrap();
    home
}
fn proposal() -> Value {
    json!({"items":[{"kind":"agent_field","agent_id":"other","field":"icon","value":"A"}],"reason":"Change icon"})
}
fn dispatcher(home: &Path) -> crate::mcp_dispatch::McpDispatcher {
    crate::mcp_dispatch::McpDispatcher::new(home.into(), reqwest::Client::new(),
        Arc::new(duduclaw_memory::SqliteMemoryEngine::new(&home.join("memory.db")).unwrap()), "avatar".into(),
        Arc::new(crate::odoo_pool::OdooConnectorPool::default()), crate::mcp_rate_limit::RateLimiter::new(),
        crate::mcp_memory_quota::DailyQuota::new())
}
async fn call(dispatcher: &crate::mcp_dispatch::McpDispatcher, principal: &crate::mcp_auth::Principal, name: &str) -> Value {
    dispatcher.dispatch_tool_call(principal, &crate::mcp_namespace::NamespaceContext {
        write_namespace: "internal/avatar".into(), read_namespaces: vec!["internal/avatar".into()],
    }, &json!({"name":name,"arguments":if name == "admin_settings_read" { json!({}) } else { proposal() }}), &json!(1)).await
}
fn succeeded(result: &Value) -> bool {
    result.get("error").is_none() && result.get("result").is_some() && result["result"]["isError"] != true
}
fn expire(home: &Path) {
    if let Some(entry) = PROOF_CACHE.get().unwrap().lock().unwrap().as_mut() {
        if entry.home == home { entry.started = Instant::now() - PROOF_TTL; }
    }
}

#[tokio::test]
async fn admin_settings_list_call_matrix() {
    for enabled in [false, true] {
        for expected in [Proof::default(), Proof { read: true, write: false }, Proof { read: true, write: true }] {
            let home = home(enabled);
            let principal = principal();
            let dispatcher = dispatcher(home.path());
            TEST_TRANSPORT.scope(TestTransport::new(expected), async {
                let listed = crate::mcp::tools_list::visible_tools_with(&principal, home.path(), "avatar", true).await;
                for name in TOOLS {
                    let allowed = enabled && expected.permits(name);
                    assert_eq!(listed.iter().any(|tool| tool.name == *name), allowed, "{enabled} {expected:?} {name}");
                    let result = call(&dispatcher, &principal, name).await;
                    assert_eq!(succeeded(&result), allowed, "{name}: {result}");
                    if !allowed { assert_eq!(result["result"]["isError"], true, "{result}"); }
                }
                TEST_TRANSPORT.with(|state| {
                    let calls = state.calls.borrow();
                    if !enabled { assert!(calls.is_empty(), "disabled must not call the route"); }
                    for body in calls.iter() {
                        assert_eq!(body["context"], "a".repeat(64));
                        let allowed: &[&str] = match body["op"].as_str().unwrap() {
                            "proof" => &["op", "context"],
                            "read" => &["op", "context", "agent_ids", "sections"],
                            "propose" => &["op", "context", "items", "reason"],
                            other => panic!("unexpected op: {other}"),
                        };
                        assert!(body.as_object().unwrap().keys().all(|key| allowed.contains(&key.as_str())));
                    }
                });
            }).await;
        }
    }
}

#[tokio::test]
async fn external_callers_never_list_admin_tools() {
    let home = home(true);
    let full = Proof { read: true, write: true };
    TEST_TRANSPORT.scope(TestTransport::new(full), async {
        let mut external = principal();
        external.is_external = true; // Even an external Admin key is refused.
        let listed = crate::mcp::tools_list::visible_tools_with(&external, home.path(), "avatar", true).await;
        let dispatcher = dispatcher(home.path());
        for name in TOOLS {
            assert!(!listed.iter().any(|tool| tool.name == *name));
            assert!(!crate::mcp_auth::external_tool_allowed(name, &external));
            assert!(!succeeded(&call(&dispatcher, &external, name).await));
            assert!(crate::mcp::remote_process_agent_tool_refused(name, &principal(), None, "avatar"));
        }
        // Also exclude the verified gateway-internal key on a remote transport.
        let internal = crate::mcp_auth::Principal {
            client_id: duduclaw_gateway::mcp_internal_key::INTERNAL_CLIENT_ID.into(), ..principal()
        };
        for name in TOOLS {
            assert!(crate::mcp::remote_process_agent_tool_refused(name, &internal,
                Some(&crate::mcp_auth::PrincipalKind::GatewayInternal), "avatar"));
            assert!(crate::mcp::RemoteAccess::GatewayInternal.refuses_process_agent_tool(name));
        }
        assert_eq!(available_for(home.path(), &internal, "avatar", true).await, Proof::default());
        let remote = dispatcher.for_remote_bearer();
        let response = crate::mcp::handle_tools_list_for_transport(&json!(1), &principal(), home.path(), "avatar", true).await;
        for name in TOOLS {
            assert!(!response["result"]["tools"].as_array().unwrap().iter().any(|tool| tool["name"] == *name));
            assert!(!succeeded(&call(&remote, &principal(), name).await));
        }
        TEST_TRANSPORT.with(|state| assert!(state.calls.borrow().is_empty()));
    }).await;
    // Standalone clients and engines without the opaque context get no proof,
    // even if their key carries Admin and an employee ID exists on disk.
    let mut no_context = TestTransport::new(full);
    no_context.credentials = None;
    TEST_TRANSPORT.scope(no_context, async {
        let listed = crate::mcp::tools_list::visible_tools_with(&principal(), home.path(), "avatar", false).await;
        let dispatcher = dispatcher(home.path());
        for name in TOOLS {
            assert!(!listed.iter().any(|tool| tool.name == *name));
            assert!(!succeeded(&call(&dispatcher, &principal(), name).await));
        }
        TEST_TRANSPORT.with(|state| assert!(state.calls.borrow().is_empty()));
    }).await;
}

#[test]
fn proof_cache_expires_at_five_seconds_and_binds_context_and_credentials() {
    let started = Instant::now();
    let entry = CachedProof { home: PathBuf::from("home"), who: who(), started, proof: Proof { read: true, write: true } };
    assert_eq!(entry.get(Path::new("home"), &who(), started + Duration::from_millis(4999)), Some(entry.proof));
    assert_eq!(entry.get(Path::new("home"), &who(), started + Duration::from_secs(5)), None);
    assert_eq!(entry.get(Path::new("other-home"), &who(), started), None);
    for index in 0..4 {
        let mut other = who();
        match index {
            0 => other.agent.push('x'), 1 => other.context = "b".repeat(64),
            2 => other.key.push('x'), _ => other.token.push('x'),
        }
        assert_eq!(entry.get(Path::new("home"), &other, started), None);
    }
}

#[tokio::test]
async fn attendance_end_changes_watcher_snapshot_and_emits_list_changed() {
    let home = home(true);
    TEST_TRANSPORT.scope(TestTransport::new(Proof { read: true, write: true }), async {
        let before = crate::mcp::visible_tool_snapshot(&principal(), home.path(), "avatar").await;
        for name in TOOLS { assert!(before.iter().any(|tool| tool.as_str() == *name)); }
        TEST_TRANSPORT.with(|state| state.proof.set(Proof::default()));
        expire(home.path());
        let after = crate::mcp::visible_tool_snapshot(&principal(), home.path(), "avatar").await;
        for name in TOOLS { assert!(!after.iter().any(|tool| tool.as_str() == *name)); }
        let mut previous = Some(before);
        let notifications = crate::mcp::server::tools_list_change_notifications(&mut previous, after, true, false, &[]);
        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0]["method"], "notifications/tools/list_changed");
    }).await;
}

#[tokio::test]
async fn cached_proof_never_bypasses_gateway_refusal() {
    let home = home(true);
    TEST_TRANSPORT.scope(TestTransport::new(Proof { read: true, write: true }), async {
        assert!(proof(home.path(), "avatar").await.write);
        TEST_TRANSPORT.with(|state| state.fail.set(true));
        let result = handle("admin_settings_propose", &proposal(), home.path(), "avatar").await;
        assert_eq!(result, crate::mcp::tool_error(UNAVAILABLE));
        expire(home.path());
        assert_eq!(proof(home.path(), "avatar").await, Proof::default());
    }).await;
}

#[test]
fn proof_decoding_fails_closed() {
    for value in [json!({"ok":false,"read":true,"write":true}), json!({"read":true,"write":true}),
        json!({"ok":true,"read":false,"write":true}), json!({"ok":true,"read":"true","write":false}),
        json!({"ok":true,"read":true,"write":true,"principal":"admin"})] {
        assert!(decode_proof(&serde_json::to_vec(&value).unwrap()).is_err());
    }
    assert!(decode_proof(b"not json").is_err());
}

#[test]
fn propose_result_has_no_id_or_code() {
    let message = "已送出確認卡，請到儀表板收件匣確認；十分鐘內有效";
    let expected = json!({"message":message,"item_count":1});
    assert_eq!(decode_proposal(&serde_json::to_vec(&expected).unwrap()).unwrap(), expected);
    for field in ["id", "approval_id", "code", "check_code", "summary"] {
        let mut value = expected.clone();
        value[field] = json!("secret");
        assert!(decode_proposal(&serde_json::to_vec(&value).unwrap()).is_err());
    }
    assert!(decode_proposal(br#"{"message":"card ID 1234","item_count":1}"#).is_err());
    assert_eq!(decode_proposal("{\"message\":\"沒有需要變更的內容\",\"item_count\":0}".as_bytes()).unwrap()["item_count"], 0);
}

#[tokio::test]
async fn arguments_are_closed_and_bounded_before_route_call() {
    let home = home(true);
    TEST_TRANSPORT.scope(TestTransport::new(Proof { read: true, write: true }), async {
        for field in ["context", "user", "run", "turn", "principal"] {
            let mut read = json!({});
            read[field] = json!("forged");
            assert_eq!(handle("admin_settings_read", &read, home.path(), "avatar").await["isError"], true);
            let mut propose = proposal();
            propose[field] = json!("forged");
            assert_eq!(handle("admin_settings_propose", &propose, home.path(), "avatar").await["isError"], true);
        }
        for args in [json!({"agent_ids":vec!["a";201]}), json!({"sections":["secrets"]})] {
            assert_eq!(handle("admin_settings_read", &args, home.path(), "avatar").await["isError"], true);
        }
        let mut extra_item_field = proposal();
        extra_item_field["items"][0]["principal"] = json!("forged");
        assert_eq!(handle("admin_settings_propose", &extra_item_field, home.path(), "avatar").await["isError"], true);
        let item = proposal()["items"][0].clone();
        for args in [json!({"items":[],"reason":""}), json!({"items":vec![item.clone();301],"reason":""}),
            json!({"items":[item],"reason":"字".repeat(201)})] {
            assert_eq!(handle("admin_settings_propose", &args, home.path(), "avatar").await["isError"], true);
        }
        TEST_TRANSPORT.with(|state| assert!(state.calls.borrow().is_empty()));
    }).await;
}

#[test]
fn admin_tools_scope_effect_schema_and_description_budget() {
    use duduclaw_core::ToolEffect;
    for (name, effect) in [("admin_settings_read", ToolEffect::Read), ("admin_settings_propose", ToolEffect::Admin)] {
        assert_eq!(crate::mcp_auth::tool_requires_scope(name), Some(crate::mcp_auth::Scope::Admin));
        assert_eq!(duduclaw_core::tool_effect::effect_of_builtin(name), Some(effect));
        assert!(duduclaw_mcp_boundary::tool_policy::policy(name).is_none());
        let definition = crate::mcp::tools().find(|tool| tool.name == name).unwrap();
        assert!(definition.description.len() <= 200);
        assert!(definition.params.iter().all(|param| param.description.len() <= 200));
        let schema = crate::mcp::build_tool_schema(definition);
        assert_eq!(schema["inputSchema"]["additionalProperties"], false);
        let props = &schema["inputSchema"]["properties"];
        if name == "admin_settings_read" {
            assert_eq!(props["agent_ids"]["maxItems"], 200);
            assert_eq!(props["sections"]["items"]["enum"].as_array().unwrap().len(), 6);
        } else {
            assert_eq!(props["items"]["maxItems"], 300);
            assert_eq!(props["reason"]["maxLength"], 200);
            let variants = props["items"]["items"]["oneOf"].as_array().unwrap();
            assert_eq!(variants.len(), 7);
            assert!(variants.iter().all(|item| item["additionalProperties"] == false));
            assert_eq!(variants[0]["properties"]["field"]["enum"].as_array().unwrap().len(),
                duduclaw_core::admin_ai_settings::AdminAiField::ALL.len());
        }
    }
}
