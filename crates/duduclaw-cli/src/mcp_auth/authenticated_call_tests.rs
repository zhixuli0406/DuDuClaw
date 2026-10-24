use super::*;
use duduclaw_core::google_identity::CredentialKind;
use sha2::Digest;

fn principal(id: &str) -> Principal {
    Principal {
        client_id: id.into(),
        scopes: [Scope::GoogleRead].into_iter().collect(),
        is_external: true,
        created_at: Utc::now(),
    }
}

#[test]
fn receipt_is_built_only_after_the_same_raw_key_authenticates() {
    let home = tempfile::tempdir().unwrap();
    let key = "ddc_dev_0123456789abcdef0123456789abcdef";
    std::fs::write(home.path().join("config.toml"), format!(
        "[mcp_keys.{key}]\nclient_id='employee-a'\nscopes=['google:read']\nis_external=true\ncreated_at='{}'\n",
        Utc::now().to_rfc3339(),
    )).unwrap();
    let call = authenticate_with_key_call(key, home.path()).unwrap();
    assert_eq!(call.principal().client_id, "employee-a");
    let reference = call.credential_ref().unwrap();
    assert_eq!(reference.kind, CredentialKind::Legacy);
    assert_eq!(
        reference.sha256,
        format!("{:x}", sha2::Sha256::digest(key.as_bytes()))
    );
    assert!(reference.is_well_formed());
    assert!(!format!("{call:?}").contains(key));
    assert!(!format!("{call:?}").contains(&reference.sha256));
    assert!(
        authenticate_with_key_call("ddc_dev_ffffffffffffffffffffffffffffffff", home.path())
            .is_err()
    );
    std::fs::write(home.path().join("config.toml"), "[mcp_keys]\n").unwrap();
    assert!(authenticate_with_key_call(key, home.path()).is_err());
}

#[tokio::test]
async fn task_local_receipts_do_not_cross_calls_or_outlive_the_scope() {
    assert!(current_authenticated_call().is_none());
    let a = principal("employee-a");
    let b = principal("employee-b");
    let call_a = AuthenticatedCall::verified(a.clone(), "one", CredentialKind::Legacy);
    let call_b = AuthenticatedCall::verified(b.clone(), "two", CredentialKind::Refresh);
    let check_a = with_authenticated_call(call_a, async {
        tokio::task::yield_now().await;
        assert_eq!(
            current_call_matches(&a).unwrap().principal().client_id,
            "employee-a"
        );
        assert!(current_call_matches(&b).is_none());
        let mut changed = a.clone();
        changed.scopes.insert(Scope::GoogleWrite);
        assert!(current_call_matches(&changed).is_none());
        changed = a.clone();
        changed.is_external = false;
        assert!(current_call_matches(&changed).is_none());
        changed = a.clone();
        changed.created_at += chrono::Duration::seconds(1);
        assert!(current_call_matches(&changed).is_none());
    });
    let check_b = with_authenticated_call(call_b, async {
        tokio::task::yield_now().await;
        let actual = current_call_matches(&b).unwrap();
        assert_eq!(
            actual.credential_ref().unwrap().kind,
            CredentialKind::Refresh
        );
        assert!(current_call_matches(&a).is_none());
    });
    tokio::join!(check_a, check_b);
    assert!(current_authenticated_call().is_none());
}

#[tokio::test]
async fn oauth_and_unauthenticated_principals_have_no_execution_receipt() {
    let principal = principal("employee-a");
    with_authenticated_call(
        AuthenticatedCall::without_reference(principal.clone()),
        async {
            assert!(
                current_call_matches(&principal)
                    .unwrap()
                    .credential_ref()
                    .is_none()
            );
        },
    )
    .await;
}

#[test]
fn authentication_scope_boxes_the_payload_before_constructing_its_future() {
    let call = AuthenticatedCall::without_reference(principal("employee-a"));
    let payload = [1_u8; 64 * 1024];
    let future = with_authenticated_call(call, async move {
        tokio::task::yield_now().await;
        std::hint::black_box(payload);
    });
    assert!(std::mem::size_of_val(&future) <= 512);
    assert!(current_authenticated_call().is_none());
}

#[tokio::test]
async fn dedicated_google_exception_requires_exact_tool_explicit_scope_and_same_call() {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join("agents/employee-a")).unwrap();
    std::fs::write(
        home.path().join("agents/employee-a/agent.toml"),
        "[agent]\nname='employee-a'\n",
    )
    .unwrap();
    let key = "ddc_dev_0123456789abcdef0123456789abcdef";
    let write = |scopes: &str| {
        std::fs::write(home.path().join("config.toml"), format!(
        "[mcp_keys.{key}]\nclient_id='employee-a'\nscopes=[{scopes}]\nis_external=true\ncreated_at='{}'\n",
        Utc::now().to_rfc3339(),
    )).unwrap()
    };
    write("'google:read'");
    let call = authenticate_with_key_call(key, home.path()).unwrap();
    let p = call.principal().clone();
    assert!(!dedicated_google_tool_allowed(
        home.path(),
        &p,
        "gmail_search"
    ));
    with_authenticated_call(call, async {
        assert!(dedicated_google_tool_allowed(
            home.path(),
            &p,
            "gmail_search"
        ));
        assert!(!dedicated_google_tool_allowed(
            home.path(),
            &p,
            "gmail_create_draft"
        ));
        assert!(!dedicated_google_tool_allowed(
            home.path(),
            &p,
            "gmail_search_extra"
        ));
        assert!(!dedicated_google_tool_allowed(home.path(), &p, "os_open"));
        assert!(!dedicated_google_tool_allowed(
            home.path(),
            &p,
            "github_status"
        ));
        let mut changed = p.clone();
        changed.scopes.insert(Scope::GoogleWrite);
        assert!(!dedicated_google_tool_allowed(
            home.path(),
            &changed,
            "gmail_create_draft"
        ));
        std::fs::write(home.path().join("agents/employee-a/agent.toml"), "[broken").unwrap();
        assert!(!dedicated_google_tool_allowed(
            home.path(),
            &p,
            "gmail_search"
        ));
    })
    .await;
    std::fs::write(
        home.path().join("agents/employee-a/agent.toml"),
        "[agent]\nname='employee-a'\n",
    )
    .unwrap();
    write("'admin'");
    let call = authenticate_with_key_call(key, home.path()).unwrap();
    let p = call.principal().clone();
    with_authenticated_call(call, async {
        assert!(!dedicated_google_tool_allowed(
            home.path(),
            &p,
            "gmail_search"
        ));
    })
    .await;
    write("'google:write'");
    let call = authenticate_with_key_call(key, home.path()).unwrap();
    let p = call.principal().clone();
    with_authenticated_call(call, async {
        assert!(dedicated_google_tool_allowed(
            home.path(),
            &p,
            "gmail_create_draft"
        ));
        assert!(!dedicated_google_tool_allowed(
            home.path(),
            &p,
            "gmail_search"
        ));
    })
    .await;
    with_authenticated_call(AuthenticatedCall::without_reference(p.clone()), async {
        assert!(!dedicated_google_tool_allowed(
            home.path(),
            &p,
            "gmail_create_draft"
        ));
    })
    .await;
}
