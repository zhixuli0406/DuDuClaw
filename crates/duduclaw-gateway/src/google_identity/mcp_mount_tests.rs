use super::*;
use crate::knowledge::turn_requesters::CURRENT_REQUESTER;
use duduclaw_llm::{InterceptDecision, ToolInterceptor};

fn allowed(value: InterceptDecision) -> bool {
    matches!(value, InterceptDecision::Allow(_))
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
    crate::mcp_oauth::save_tokens(
        home.p(),
        &[crate::mcp_oauth::McpOAuthToken {
            provider_id: crate::google_workspace::GOOGLE_PROVIDER.into(),
            access_token: "home-mount-test-token".into(),
            refresh_token: None,
            expires_at: Some(chrono::Utc::now() + chrono::Duration::hours(1)),
            scopes: crate::google_workspace::REQUIRED_SCOPES
                .iter()
                .map(|s| s.to_string())
                .collect(),
        }],
    )
    .unwrap();
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
    let external = "\n[[mcp.external]]\nname='workspace'\nurl='https://gmailmcp.googleapis.com/mcp/v1'\nbearer_token='oauth://google'\n[[mcp.external]]\nname='other'\nurl='https://example.test/mcp'\nbearer_token='literal-other-token'\n";
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
    assert_eq!(
        google.bearer_token.as_deref(),
        Some("tok:mount-loader@c.test")
    );
    assert!(google.google_guard.is_some());
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
            visitor,
            crate::mcp_external::load_external_mcp_servers_resolved(&dir, home.p()),
        )
        .await;
    assert_eq!(servers.len(), 1);
    assert_eq!(servers[0].name, "other");
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
