use super::*;
use crate::knowledge::turn_requesters::CURRENT_REQUESTER;
use duduclaw_llm::{InterceptDecision, ToolInterceptor};

fn allowed(value: InterceptDecision) -> bool {
    matches!(value, InterceptDecision::Allow(_))
}

struct MountWire {
    endpoint: String,
    requests: std::sync::Arc<std::sync::Mutex<Vec<(String, String)>>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for MountWire {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn mount_wire() -> MountWire {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
    let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded = requests.clone();
    let task = tokio::spawn(async move {
        // The owner aborts this fixture on success, failure or cancellation.
        // Its independent cap cannot extend any production operation deadline.
        let _ = tokio::time::timeout(std::time::Duration::from_secs(60), async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                let mut chunk = [0;4096];
                let end = loop {
                    let n = stream.read(&mut chunk).await.unwrap();
                    assert_ne!(n,0); bytes.extend_from_slice(&chunk[..n]);
                    assert!(bytes.len() <= 64*1024);
                    if let Some(i) = bytes.windows(4).position(|w|w==b"\r\n\r\n") { break i+4; }
                };
                let headers = String::from_utf8_lossy(&bytes[..end]);
                let length = headers.lines().find_map(|line|line.split_once(':').filter(|(k,_)|k.eq_ignore_ascii_case("content-length")).map(|(_,v)|v.trim().parse::<usize>().unwrap())).unwrap_or(0);
                assert!(length<=16*1024);
                let authorization = headers.lines().find_map(|line|line.split_once(':').filter(|(k,_)|k.eq_ignore_ascii_case("authorization")).map(|(_,v)|v.trim().to_owned())).unwrap();
                while bytes.len()-end < length { let n=stream.read(&mut chunk).await.unwrap();assert_ne!(n,0);bytes.extend_from_slice(&chunk[..n]); }
                let frame:Value=serde_json::from_slice(&bytes[end..end+length]).unwrap();
                let method=frame["method"].as_str().unwrap();
                recorded.lock().unwrap().push((method.into(),authorization));
                let body=match method {
                    "initialize"=>json!({"jsonrpc":"2.0","id":frame["id"],"result":{}}),
                    "tools/list"=>json!({"jsonrpc":"2.0","id":frame["id"],"result":{"tools":[{"name":"lookup","description":"owned fixture","inputSchema":{"type":"object"}}]}}),
                    "tools/call"=>{assert_eq!(frame["params"]["name"],"lookup");json!({"jsonrpc":"2.0","id":frame["id"],"result":{"content":[{"type":"text","text":"owned-lookup"}]}})},
                    "notifications/initialized"=>Value::Null,
                    _=>panic!("unexpected owned MCP method"),
                };
                let payload=if body.is_null(){String::new()}else{body.to_string()};
                let status=if body.is_null(){"204 No Content"}else{"200 OK"};
                let header=format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",payload.len());
                stream.write_all(header.as_bytes()).await.unwrap();
                stream.write_all(payload.as_bytes()).await.unwrap();
            }
        }).await;
    });
    MountWire {
        endpoint,
        requests,
        task,
    }
}

#[tokio::test]
async fn reused_google_mount_checks_each_real_requester_and_preserves_task_local_system_identity() {
    let home = Home::new();
    home.ready("mount-bot", "mount-default@c.test", "");
    let (_, guard) = CURRENT_REQUESTER
        .scope(system(), async {
            mcp_mount::GoogleMountGuard::prepare(home.p(), &home.p().join("agents/mount-bot")).await
        })
        .await
        .unwrap();
    // A genuine system scope survives the blocking credential check. If the
    // guard read task locals on the blocking thread it would become Unknown.
    let value = CURRENT_REQUESTER
        .scope(
            system(),
            guard.before_call_async("workspace", "lookup", json!({})),
        )
        .await;
    assert!(allowed(value));
    assert!(
        !allowed(
            guard
                .before_call_async("workspace", "lookup", json!({}))
                .await
        ),
        "Unknown never becomes a system turn"
    );
    home.agent(
        "mount-bot",
        "[google]\nidentity='delegate:mount-default@c.test'\nrequesters='anyone'\n",
    );
    assert!(allowed(
        guard
            .before_call_async("workspace", "lookup", json!({}))
            .await
    ));
    for channel in ["widget", "odoolivechat"] {
        let visitor = RequesterIdentity::ChannelUser {
            channel: channel.into(),
            chat: "visitor".into(),
            sender: "v".into(),
            user_id: None,
        };
        let value = CURRENT_REQUESTER
            .scope(
                visitor,
                guard.before_call_async("workspace", "lookup", json!({})),
            )
            .await;
        assert!(!allowed(value), "public visitor {channel}");
    }
    home.agent("mount-bot", "[google]\nidentity='none'\n");
    assert!(!allowed(
        guard
            .before_call_async("workspace", "lookup", json!({}))
            .await
    ));
}

#[tokio::test]
async fn google_mount_refuses_changed_domain_sealed_key_and_selector_without_using_home() {
    let home = Home::new();
    home.ready("mount-bot", "mount-live@c.test", "requesters='anyone'\n");
    let (token, guard) =
        mcp_mount::GoogleMountGuard::prepare(home.p(), &home.p().join("agents/mount-bot"))
            .await
            .unwrap();
    assert_eq!(token, "tok:mount-live@c.test");
    assert!(
        guard.fallback_notice().is_none(),
        "successful delegate never claims home fallback"
    );
    assert!(
        guard
            .account_notice()
            .unwrap()
            .contains("mount-live@c.test")
    );
    assert!(allowed(
        guard
            .before_call_async("workspace", "lookup", json!({}))
            .await
    ));
    home.domains(&["other.test"]);
    assert!(!allowed(
        guard
            .before_call_async("workspace", "lookup", json!({}))
            .await
    ));
    home.domains(&["c.test"]);
    assert!(allowed(
        guard
            .before_call_async("workspace", "lookup", json!({}))
            .await
    ));
    home.agent("mount-bot", "[google]\nidentity='home'\n");
    assert!(!allowed(
        guard
            .before_call_async("workspace", "lookup", json!({}))
            .await
    ));
    home.agent(
        "mount-bot",
        "[google]\nidentity='delegate:mount-live@c.test'\nrequesters='anyone'\n",
    );
    delegation_key::remove(home.p()).unwrap();
    assert!(!allowed(
        guard
            .before_call_async("workspace", "lookup", json!({}))
            .await
    ));
}

#[tokio::test]
async fn actual_delegate_failure_uses_home_and_keeps_a_per_call_fallback_notice() {
    let home = Home::new();
    home.ready(
        "mount-bot",
        "gone-mount@c.test",
        "requesters='anyone'\nfallback='home'\n",
    );
    crate::mcp_oauth::google_home_test_fixture::connect(
        home.p(), "home-mount-test-token", None,
    ).await;
    let (token, guard) =
        mcp_mount::GoogleMountGuard::prepare(home.p(), &home.p().join("agents/mount-bot"))
            .await
            .unwrap();
    assert_eq!(token, "home-mount-test-token");
    assert!(guard.fallback_notice().unwrap().contains("公司共用帳號"));
    assert!(allowed(
        guard
            .before_call_async("workspace", "lookup", json!({}))
            .await
    ));
    home.agent(
        "mount-bot",
        "[google]\nidentity='delegate:gone-mount@c.test'\nrequesters='anyone'\n",
    );
    assert!(!allowed(
        guard
            .before_call_async("workspace", "lookup", json!({}))
            .await
    ));
}

#[tokio::test]
async fn production_mount_loader_resolves_the_employee_and_skips_only_google_on_requester_refusal()
{
    let home = Home::new();
    home.ready("mount-bot", "mount-loader@c.test", "requesters='anyone'\n");
    let wire = mount_wire().await;
    let _owned = effect_authority::remote_testing::install(home.p(), &wire.endpoint);
    let external = format!(
        "\n[[mcp.external]]\nname='workspace'\nurl='{}'\nbearer_token='oauth://google'\n[[mcp.external]]\nname='other'\nurl='https://example.test/mcp'\nbearer_token='literal-other-token'\n",
        wire.endpoint
    );
    home.agent(
        "mount-bot",
        &format!(
            "[google]\nidentity='delegate:mount-loader@c.test'\nrequesters='anyone'\n{external}"
        ),
    );
    let dir = home.p().join("agents/mount-bot");
    let servers = crate::mcp_external::load_external_mcp_servers_resolved(&dir, home.p()).await;
    assert_eq!(servers.len(), 2);
    let google = servers.iter().find(|s| s.name == "workspace").unwrap();
    // Admission retains host authority, never a bearer in server config.
    assert!(google.bearer_token.is_none());
    assert!(google.google_guard.is_none());
    let mount = google.google_closed.clone().unwrap();
    let registry = duduclaw_llm::ToolRegistry::from_client_slots_named(
        vec![(google.name.clone(), mount.registry_slot())],
        vec![google.filter.clone()],
    )
    .await
    .unwrap();
    let outcome = duduclaw_llm::ToolExecutor::call(&registry, "lookup", json!({}))
        .await
        .unwrap();
    assert_eq!(outcome.content, "owned-lookup");
    assert!(!outcome.is_error);
    let calls = wire.requests.lock().unwrap();
    assert_eq!(calls.len(), 6);
    let expected_bearer = format!("Bearer {}", closed_fixture_token("mount-loader@c.test"));
    assert!(calls.iter().all(|(_, authorization)| authorization == &expected_bearer));
    assert_eq!(
        calls
            .iter()
            .filter(|(method, _)| method == "tools/call")
            .count(),
        1
    );
    drop(calls);
    assert!(
        servers
            .iter()
            .find(|s| s.name == "other")
            .unwrap()
            .google_guard
            .is_none()
    );
    let visitor = RequesterIdentity::ChannelUser {
        channel: "widget".into(),
        chat: "v".into(),
        sender: "v".into(),
        user_id: None,
    };
    let servers = CURRENT_REQUESTER
        .scope(
            visitor.clone(),
            crate::mcp_external::load_external_mcp_servers_resolved(&dir, home.p()),
        )
        .await;
    // Closed admission keeps the entry without acquiring authority. The
    // actual operation re-enters the live requester gate before any frame.
    assert_eq!(servers.len(), 2);
    let other = servers
        .iter()
        .find(|server| server.name == "other")
        .unwrap();
    assert!(other.google_closed.is_none());
    assert_eq!(other.bearer_token.as_deref(), Some("literal-other-token"));
    let before = wire.requests.lock().unwrap().len();
    let refused = CURRENT_REQUESTER
        .scope(
            visitor,
            duduclaw_llm::ToolExecutor::call(&registry, "lookup", json!({})),
        )
        .await
        .unwrap();
    assert!(refused.is_error);
    assert!(!refused.content.contains(&closed_fixture_token("mount-loader@c.test")));
    assert_eq!(wire.requests.lock().unwrap().len(), before);
}

#[tokio::test]
async fn person_mount_never_claims_a_stdio_child_used_an_ignored_http_bearer() {
    let home = Home::new();
    home.ready("mount-bot", "mount-stdio@c.test", "requesters='anyone'\n");
    home.agent("mount-bot","[google]\nidentity='delegate:mount-stdio@c.test'\nrequesters='anyone'\n[[mcp.external]]\nname='stdio-google'\ncommand='fixture-do-not-spawn'\nbearer_token='oauth://google'\n");
    let servers = crate::mcp_external::load_external_mcp_servers_resolved(
        &home.p().join("agents/mount-bot"),
        home.p(),
    )
    .await;
    assert!(servers.is_empty());
}


#[tokio::test]
async fn personal_selectors_refuse_external_mount_even_with_anyone_and_available_home_fallback() {
    let user = uuid::Uuid::new_v4().to_string();
    for selector in ["owner".to_owned(), format!("user:{user}")] {
        let home = Home::new();
        home.ready("mount-bot", "mount-personal-unsupported@c.test", "requesters='anyone'\nfallback='home'\n");
        home.agent("mount-bot", &format!("[google]\nidentity='{selector}'\nrequesters='anyone'\nfallback='home'\n"));
        crate::mcp_oauth::google_home_test_fixture::connect(
            home.p(), "home-personal-boundary-token", None,
        ).await;
        let result = CURRENT_REQUESTER.scope(system(), mcp_mount::GoogleMountGuard::prepare(home.p(), &home.p().join("agents/mount-bot"))).await;
        let error = match result { Err(error) => error, Ok(_) => panic!("Personal identity yielded an external mount bearer") };
        assert_eq!(error.code, "no_verified_caller");
        assert!(!error.fallback_eligible);
        assert!(!error.message_zh.contains("home-personal-boundary-token"));
        assert!(seen_for("mount-personal-unsupported@c.test").is_empty());
    }
}
