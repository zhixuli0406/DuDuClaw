//! Opt-in separate-process acceptance probe. The authority is a disposable
//! real route + verified test-issuer license, not a production entitlement.
use super::*;
use serde_json::{Value, json};
use std::future::IntoFuture;
use std::path::Path;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

async fn mcp_call(
    home: &Path,
    port: u16,
    principal: Option<&str>,
    context: Option<&str>,
    tool: &str,
    args: Value,
) -> Value {
    let binary = std::env::var("DUDUCLAW_NATIVE_MCP_BINARY").expect("binary required");
    let token =
        duduclaw_core::mint_identity_token(&duduclaw_core::load_identity_key(home).unwrap(), "bot");
    let mut command = tokio::process::Command::new(binary);
    command
        .arg("mcp-server")
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home.join("os-home"))
        .env("DUDUCLAW_HOME", home)
        .env("DUDUCLAW_PORT", port.to_string())
        .env(duduclaw_core::ENV_AGENT_ID, "bot")
        .env(duduclaw_core::ENV_AGENT_TOKEN, token)
        .env(
            duduclaw_core::ENV_MCP_API_KEY,
            "ddc_prod_0123456789abcdef0123456789abcdef",
        )
        // An MCP-provided edition must never promote an unlicensed authority.
        .env("DUDUCLAW_EDITION", "enterprise")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    if let Some(p) = principal {
        command.env(ENV_ON_BEHALF_OF, p);
    }
    if let Some(r) = context {
        command.env(super::context::ENV_CONTEXT, r);
    }
    let mut child = command.spawn().expect("start native MCP");
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = tokio::io::BufReader::new(child.stdout.take().unwrap()).lines();
    let requests = [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"obo-native","version":"1"}}}),
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":tool,"arguments":args}}),
    ];
    for req in requests {
        stdin
            .write_all(format!("{req}\n").as_bytes())
            .await
            .unwrap();
    }
    stdin.flush().await.unwrap();
    let result = tokio::time::timeout(std::time::Duration::from_secs(60), async {
        loop {
            let line = stdout
                .next_line()
                .await
                .unwrap()
                .expect("native MCP exited before response");
            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if v["id"] == 2 {
                assert!(v.get("error").is_none(), "{v}");
                break v["result"].clone();
            }
        }
    })
    .await
    .expect("native MCP timed out");
    child.kill().await.unwrap();
    child.wait().await.unwrap();
    result
}
fn signed_license(
    key: &ed25519_dalek::SigningKey,
    tier: duduclaw_license::LicenseTier,
) -> duduclaw_license::License {
    use ed25519_dalek::Signer;
    let mut license = duduclaw_license::License::new(
        "sub_obo_native_fixture",
        "fixture",
        tier,
        duduclaw_license::generate_fingerprint(),
        chrono::Duration::days(1),
        "obo-test",
    );
    license.nfr = true;
    license.signature = key
        .sign(&license.canonical_payload().unwrap())
        .to_bytes()
        .to_vec();
    license
}
fn is_ok(result: &Value) -> bool {
    result.get("isError").and_then(Value::as_bool) != Some(true)
}

#[tokio::test]
#[ignore = "requires rebuilt native MCP binary and an empty disposable DUDUCLAW_HOME"]
async fn native_mcp_team_manager_uses_host_authority() {
    use duduclaw_auth::{AccessLevel, UserDb, UserRole};
    use duduclaw_license::{LicenseTier, PublicKeyRegistry};
    let home =
        std::path::PathBuf::from(std::env::var("DUDUCLAW_HOME").expect("explicit fixture home"));
    let expected =
        std::env::var("DUDUCLAW_NATIVE_FIXTURE_HOME").expect("explicit fixture home required");
    assert_eq!(home, std::path::PathBuf::from(expected));
    let canonical_home = home.canonicalize().unwrap();
    assert!(
        canonical_home.starts_with(std::env::temp_dir().canonicalize().unwrap())
            || Path::new("/tmp")
                .canonicalize()
                .is_ok_and(|base| canonical_home.starts_with(base)),
        "fixture must be under the system temporary directory or /tmp"
    );
    assert_eq!(
        std::fs::read_dir(&home).unwrap().count(),
        0,
        "fixture must be empty"
    );
    std::fs::create_dir(home.join("os-home")).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    duduclaw_core::ensure_identity_key(&home).unwrap();
    let key = ed25519_dalek::SigningKey::generate(&mut rand::rngs::OsRng);
    let registry =
        PublicKeyRegistry::new().with_key("obo-test", key.verifying_key().to_bytes().to_vec());
    duduclaw_license::storage::save_to(
        &signed_license(&key, LicenseTier::SelfHostPro),
        &home.join("license.json"),
    )
    .unwrap();
    let runtime = crate::license_runtime::LicenseRuntime::bootstrap(home.clone(), registry).await;
    assert_eq!(runtime.current_tier().await, LicenseTier::SelfHostPro);
    std::fs::write(home.join("config.toml"), format!("[gateway]\nbind='127.0.0.1'\nport={port}\n[delegation]\npolicy='open'\nhuman_principal='enforce'\nrequire_identity_token=true\n[mcp_keys.\"{}\"]\nclient_id='gateway-internal'\nis_external=false\ncreated_at='{}'\nscopes=['admin']\n", "ddc_prod_0123456789abcdef0123456789abcdef", chrono::Utc::now().to_rfc3339())).unwrap();
    for name in ["bot", "sales", "hr"] {
        let dir = home.join("agents").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agent.toml"), format!("[agent]\nname='{name}'\nrole='worker'\nstatus='active'\n[model]\npreferred='claude-haiku-4-5'\n[runtime]\nprovider='claude'\n")).unwrap();
    }
    let db = UserDb::new(&home.join("users.db")).unwrap();
    let mgr = db
        .create_user(
            "mgr@fixture.local",
            "mgr",
            "TeamPass-12345",
            UserRole::Manager,
        )
        .unwrap();
    let emp = db
        .create_user(
            "emp@fixture.local",
            "emp",
            "TeamPass-12345",
            UserRole::Employee,
        )
        .unwrap();
    let viewer = db
        .create_user(
            "viewer@fixture.local",
            "viewer",
            "TeamPass-12345",
            UserRole::Employee,
        )
        .unwrap();
    db.set_manager(&emp.id, Some(&mgr.id)).unwrap();
    db.bind_agent(&emp.id, "sales", AccessLevel::Owner).unwrap();
    db.bind_agent(&emp.id, "hr", AccessLevel::Viewer).unwrap();
    db.bind_agent(&viewer.id, "sales", AccessLevel::Viewer)
        .unwrap();
    // The real gateway creates this schema at boot; this route-only fixture
    // must initialize the same store before native MCP can queue messages.
    let _queue = crate::message_queue::MessageQueue::open(&home).unwrap();
    let state = Arc::new(route::RouteState::new(home.clone(), Some(runtime.clone())));
    let server = tokio::spawn(
        axum::serve(
            listener,
            route::router(state).into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .into_future(),
    );
    let principal = format!("user:{}", mgr.id);
    scope_first(Some(principal.clone()), async {
        let guard = context::register(&home, "bot").unwrap();
        let r = Some(guard.reference());
        // A signed Enterprise license alone is insufficient before the host
        // has resolved its edition, or when the operator selected Personal.
        assert!(!is_ok(&mcp_call(&home, port, Some(&principal), r, "tasks_create", json!({"title":"native-host-unrecorded","assigned_to":"sales"})).await));
        crate::team_edition::record(&home, duduclaw_core::EditionProfile::Personal);
        assert!(!is_ok(&mcp_call(&home, port, Some(&principal), r, "tasks_create", json!({"title":"native-host-personal","assigned_to":"sales"})).await));
        crate::team_edition::record(&home, duduclaw_core::EditionProfile::Enterprise);

        assert!(is_ok(&mcp_call(&home, port, Some(&principal), r, "tasks_create", json!({"title":"native-team-manager","assigned_to":"sales","on_behalf_of":"user:forged"})).await));
        assert!(is_ok(&mcp_call(&home, port, Some(&principal), r, "send_to_agent", json!({"agent_id":"hr","message":"native-team-viewer-call"})).await));
        assert!(!is_ok(&mcp_call(&home, port, Some(&principal), r, "tasks_create", json!({"title":"native-team-viewer-denied","assigned_to":"hr"})).await));
        assert!(!is_ok(&mcp_call(&home, port, Some(&principal), None, "tasks_create", json!({"title":"native-no-context","assigned_to":"sales"})).await));
        assert!(!is_ok(&mcp_call(&home, port, Some("user:fake"), r, "tasks_create", json!({"title":"native-mismatched-stamp","assigned_to":"sales"})).await));
        db.unbind_agent(&emp.id, "sales").unwrap();
        assert!(!is_ok(&mcp_call(&home, port, Some(&principal), r, "tasks_create", json!({"title":"native-binding-removed","assigned_to":"sales"})).await));
        db.bind_agent(&emp.id, "sales", AccessLevel::Owner).unwrap();
        runtime.install_and_reload(signed_license(&key, LicenseTier::PersonalProSelfHost)).await.unwrap();
        assert!(!is_ok(&mcp_call(&home, port, Some(&principal), r, "tasks_create", json!({"title":"native-personal-denied","assigned_to":"sales"})).await));
        runtime.install_and_reload(signed_license(&key, LicenseTier::SelfHostPro)).await.unwrap();
        db.set_user_status(&mgr.id, duduclaw_auth::UserStatus::Suspended).unwrap();
        assert!(!is_ok(&mcp_call(&home, port, Some(&principal), r, "tasks_create", json!({"title":"native-inactive-denied","assigned_to":"sales"})).await));
        guard.reference().to_owned()
    }).await;
    let viewer_principal = format!("user:{}", viewer.id);
    scope_first(Some(viewer_principal.clone()), async {
        let guard = context::register(&home, "bot").unwrap();
        assert!(is_ok(
            &mcp_call(
                &home,
                port,
                Some(&viewer_principal),
                Some(guard.reference()),
                "send_to_agent",
                json!({"agent_id":"sales","message":"native-direct-viewer-call"})
            )
            .await
        ));
        assert!(!is_ok(
            &mcp_call(
                &home,
                port,
                Some(&viewer_principal),
                Some(guard.reference()),
                "tasks_create",
                json!({"title":"native-direct-viewer-denied","assigned_to":"sales"})
            )
            .await
        ));
    })
    .await;
    let background = context::register(&home, "bot").unwrap();
    assert!(is_ok(
        &mcp_call(
            &home,
            port,
            None,
            Some(background.reference()),
            "tasks_create",
            json!({"title":"native-background","assigned_to":"bot"})
        )
        .await
    ));
    assert!(!is_ok(
        &mcp_call(
            &home,
            port,
            None,
            None,
            "tasks_create",
            json!({"title":"native-unproven-none","assigned_to":"bot"})
        )
        .await
    ));
    let foreign = context::register(&home, "other").unwrap();
    assert!(!is_ok(
        &mcp_call(
            &home,
            port,
            None,
            Some(foreign.reference()),
            "tasks_create",
            json!({"title":"native-foreign-context","assigned_to":"bot"})
        )
        .await
    ));
    let expired = background.reference().to_owned();
    drop(background);
    assert!(!is_ok(
        &mcp_call(
            &home,
            port,
            None,
            Some(&expired),
            "tasks_create",
            json!({"title":"native-dropped-context","assigned_to":"bot"})
        )
        .await
    ));
    let conn = rusqlite::Connection::open(home.join("tasks.db")).unwrap();
    let mut stmt = conn
        .prepare("SELECT title,on_behalf_of FROM tasks ORDER BY title")
        .unwrap();
    let tasks: Vec<(String, Option<String>)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        tasks,
        vec![
            ("native-background".into(), None),
            ("native-team-manager".into(), Some(principal))
        ]
    );
    server.abort();
    let _ = server.await;
    println!(
        "PASS native separate MCP: signed test-license Manager team assign; team/direct Viewer call-only; live revoke; Personal downgrade; inactive user; host None; missing/foreign/dropped/mismatched context fail closed; no model or dispatcher run"
    );
}
