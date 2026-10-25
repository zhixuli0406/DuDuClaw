//! Opt-in, fresh-home dashboard fixture with a real gateway and container.
//! Host registrations below are synthetic; this does not validate a channel
//! adapter, model, provider account, or a physical user's acceptance.
#![cfg(unix)]

use super::*;

/// Refuse a direct invocation that could retain production event paths.
/// The launcher creates all paths before spawn; this code never mutates a
/// multithreaded process environment to try to repair an unsafe invocation.
fn isolated_home() -> Result<std::path::PathBuf, String> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let root = std::env::var_os("DUDU_TEAM_NATIVE_FIXTURE_ROOT")
        .ok_or("use the isolated subprocess launcher")?;
    let root = std::path::PathBuf::from(root);
    let temp = std::fs::canonicalize(std::env::temp_dir())
        .map_err(|_| "temporary directory unavailable")?;
    let canonical = std::fs::canonicalize(&root).map_err(|_| "fixture root unavailable")?;
    let meta = std::fs::symlink_metadata(&root).map_err(|_| "fixture root unavailable")?;
    let temp_meta = std::fs::metadata(&temp).map_err(|_| "temporary directory unavailable")?;
    if canonical != root
        || canonical.parent() != Some(temp.as_path())
        || !meta.is_dir()
        || meta.file_type().is_symlink()
        || meta.uid() != temp_meta.uid()
        || meta.permissions().mode() & 0o777 != 0o700
        || !canonical
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with("dudu-team-native-"))
    {
        return Err("fixture root is not a fresh private temporary directory".into());
    }
    for (name, child) in [
        ("HOME", "os-home"),
        ("DUDUCLAW_HOME", "data"),
        ("EVOLUTION_EVENTS_DIR", "evolution"),
    ] {
        let value = std::env::var_os(name).ok_or("fixture environment is incomplete")?;
        let path = std::path::PathBuf::from(value);
        let expected = canonical.join(child);
        let meta = std::fs::symlink_metadata(&path).map_err(|_| "fixture path unavailable")?;
        if path != expected
            || std::fs::canonicalize(&path).map_err(|_| "fixture path unavailable")? != expected
            || !meta.is_dir()
            || meta.file_type().is_symlink()
            || meta.uid() != temp_meta.uid()
            || meta.permissions().mode() & 0o777 != 0o700
        {
            return Err("fixture environment retains an external path".into());
        }
    }
    Ok(canonical.join("data"))
}

async fn docker(args: &[&str]) -> Result<String, String> {
    let output = crate::computer_use_orchestrator::docker_output(
        args,
        Duration::from_secs(30),
        "team native fixture",
    )
    .await
    .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

async fn owned(home: &std::path::Path) -> Result<Vec<String>, String> {
    let label = crate::computer_use_orchestrator::computer_use_home_label(home);
    let filter = format!(
        "label={}={label}",
        crate::computer_use_orchestrator::HOME_LABEL
    );
    Ok(docker(&["ps", "-aq", "--filter", &filter])
        .await?
        .lines()
        .map(str::to_owned)
        .collect())
}

async fn lwm() -> Result<String, String> {
    let id = docker(&["ps", "-aq", "--filter", "name=^/duduclaw-lwm-exp$"]).await?;
    if id.is_empty() {
        return Ok("absent".into());
    }
    docker(&[
        "inspect",
        "-f",
        "{{.Id}} {{.State.Running}} {{.State.StartedAt}}",
        &id,
    ])
    .await
}

// Construct and box the large server future outside the native test's
// async state machine; do not raise the test worker's stack limit.
fn spawn_gateway(
    config: crate::server::GatewayConfig,
) -> tokio::task::JoinHandle<duduclaw_core::error::Result<()>> {
    tokio::spawn(Box::pin(crate::server::start_gateway(config)))
}

#[tokio::test]
#[ignore = "needs Docker, DUDU_COMPUTER_USE_IMAGE and an explicit native dashboard driver"]
async fn computer_team_native_dashboard_fixture() {
    let home = isolated_home().expect("run scripts/live-test/computer-team-dashboard.mjs");
    let image = std::env::var("DUDU_COMPUTER_USE_IMAGE").expect("set DUDU_COMPUTER_USE_IMAGE");
    eprintln!("TEAM_NATIVE_ARTIFACTS {}", home.display());
    write_config(
        &home,
        &format!(
            "[people]\nmanager_team_history=true\n[computer_use]\nimage={image:?}\n[dispatch]\nenabled=false\n[proactive]\nenabled=false\n"
        ),
    );
    let agent = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../scripts/live-test/fixtures/computer-team-agent.toml"
    ));
    let _: duduclaw_core::AgentConfig =
        toml::from_str(agent).expect("valid registry fixture config");
    write_agent(&home, "alice", agent);
    duduclaw_core::ensure_identity_key(&home).unwrap();
    let key = [
        uuid::Uuid::new_v4().as_bytes().as_slice(),
        uuid::Uuid::new_v4().as_bytes().as_slice(),
    ]
    .concat();
    std::fs::write(home.join(".keyfile"), key).unwrap();
    duduclaw_core::platform::set_owner_only(&home.join(".keyfile")).unwrap();
    let db = duduclaw_auth::UserDb::new(&home.join("users.db")).unwrap();
    let manager = db
        .create_user(
            "manager@team.test",
            "Fixture Manager",
            "isolated-password",
            duduclaw_auth::UserRole::Manager,
        )
        .unwrap()
        .id;
    let report = db
        .create_user(
            "report@team.test",
            "Fixture Report",
            "isolated-password",
            duduclaw_auth::UserRole::Employee,
        )
        .unwrap()
        .id;
    db.create_user(
        "admin@team.test",
        "Fixture Admin",
        "isolated-password",
        duduclaw_auth::UserRole::Admin,
    )
    .unwrap();
    db.create_user("peer@team.test", "Unbound peer", "isolated-password", duduclaw_auth::UserRole::Employee).unwrap();
    db.set_manager(&report, Some(&manager)).unwrap();
    db.bind_agent(&report, "alice", duduclaw_auth::AccessLevel::Operator)
        .unwrap();
    let port = {
        let lease = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        lease.local_addr().unwrap().port()
    };
    let before = lwm().await.expect("production fingerprint before starting");
    let config = crate::server::GatewayConfig {
        bind: "127.0.0.1".into(),
        port,
        auth_token: Some(uuid::Uuid::new_v4().to_string()),
        home_dir: home.clone(),
        allowed_origins: vec![],
        extension: Arc::new(crate::extension::NullExtension),
        edition: Some(duduclaw_core::EditionProfile::Enterprise),
    };
    let server = spawn_gateway(config);
    let mut running = None;
    let body = async {
        let http = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(2))
            .build()
            .map_err(|e| e.to_string())?;
        let mut ready = false;
        for _ in 0..120 {
            if server.is_finished() {
                return Err("fixture gateway exited before readiness".into());
            }
            if http
                .get(format!("http://127.0.0.1:{port}/healthz"))
                .send()
                .await
                .is_ok()
            {
                ready = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        if !ready {
            return Err("fixture gateway readiness timeout".into());
        }
        let sessions = super::super::workspace_admin::admin_sessions(&home);
        running = Some(sessions.clone());
        crate::team_oversight::observe_window(&home, &db, "fixture")?;
        let turn = format!("native-team-{}", uuid::Uuid::new_v4());
        let _requester = crate::knowledge::turn_requesters::register(
            "alice",
            &turn,
            duduclaw_knowledge::requester::RequesterIdentity::DashboardUser {
                user_id: report.clone(),
            },
        )
        .ok_or("fixture requester registration failed")?;
        let conversation = "webchat:team-native-fixture";
        let first = super::super::attended::verified_webchat_message(
            conversation,
            Some(&report),
            duduclaw_memory::feedback::CURRENT_SESSION_ID.scope(Some(conversation.into()),
                duduclaw_memory::feedback::CURRENT_TURN_ID.scope(Some(turn.clone()), async {
                if !super::super::attended::bind_final_route(
                    conversation,
                    conversation,
                    "alice",
                    &turn,
                ) {
                    return Err("fixture trusted route stamp failed".into());
                }
                let _live = super::super::turns::register("alice", &turn, conversation)
                    .ok_or("fixture attended registration failed")?;
                let runtime = crate::on_behalf_of::context::register(&home, "alice")
                    .ok_or("fixture runtime registration failed")?;
                let caller = crate::on_behalf_of::context::attended_call(&home, "alice", runtime.reference())
                    .ok_or("fixture attended caller proof missing")?;
                super::super::attended_call::request_scope(Some(caller),
                    sessions.start(
                        "alice",
                        StartRequest {
                            turn_id: Some(turn.clone()),
                            ..Default::default()
                        },
                    ))
                    .await
                    .map_err(|e| format!("{e:?}"))
            })),
        )
        .await?;
        let ids = owned(&home).await?;
        if ids.len() != 1 {
            return Err("expected exactly one owned container".into());
        }
        let page = r#"import sys,time
sys.dont_write_bytecode=True
sys.path.insert(0,'/usr/local/bin')
from duduclaw_cdp import list_pages,WebSocket
ws=WebSocket(list_pages(5)[0],5)
ws.call('Page.navigate',{'url':'data:text/html,<title>Team fixture</title><body style="margin:0;background:%2327483a;color:white;font:28px sans-serif;padding:48px">TEAM NATIVE FIXTURE<input type="password" value="synthetic-only"></body>'})
time.sleep(0.3)
"#;
        docker(&["exec", &ids[0], "python3", "-c", page]).await?;
        let now = chrono::Utc::now().timestamp();
        let id = first["session_id"].as_str().ok_or("fixture missing session id")?;
        let remaining = sessions.live_status("alice", &manager)["seconds_left"].as_u64().unwrap_or(0);
        let watch_token = crate::computer_watch_links::issue_at(&home, "alice", id, now, remaining)?;
        let expired_token = crate::computer_watch_links::issue_at(&home, "alice", id, now - 5, 1)?;
        let other_session_token = crate::computer_watch_links::issue_at(&home, "alice", "cu-00000000000000000000000000000000", now, remaining)?;
        let manifest = json!({"status":"READY","port":port,"session_id":first["session_id"],"manager":manager,"report":report,
            "manager_email":"manager@team.test","admin_email":"admin@team.test","password":"isolated-password",
            "peer_email":"peer@team.test", "watch_token":watch_token,"expired_token":expired_token,"other_session_token":other_session_token,
            "gateway_internal_test_key":INTERNAL_KEY,
            "boundary":"synthetic host requester, real gateway/container, no channel/model/provider acceptance"});
        let path = home.join("team-ui-ready.json");
        std::fs::write(
            &path,
            serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        duduclaw_core::platform::set_owner_only(&path).map_err(|e| e.to_string())?;
        eprintln!(
            "TEAM_NATIVE_READY http://127.0.0.1:{port} {}",
            home.display()
        );
        let finish = home.join("team-ui-finish.json");
        for _ in 0..720 {
            if server.is_finished() {
                return Err("fixture gateway exited during dashboard test".into());
            }
            match std::fs::read(&finish) {
                Ok(bytes) => {
                    let evidence: Value =
                        serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
                    if evidence["status"] != "PASS" {
                        return Err("native driver did not pass".into());
                    }
                    return Ok::<_, String>(evidence);
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.to_string()),
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        Err("native dashboard driver timed out".into())
    };
    let result = tokio::time::timeout(Duration::from_secs(480), body).await;
    // Cleanup runs before any result write, even on a driver error/timeout.
    server.abort();
    let _ = server.await;
    if let Some(sessions) = running {
        sessions.stop_all_for_emergency();
    }
    let cleanup = async {
        let expected = crate::computer_use_orchestrator::computer_use_home_label(&home);
        // Let the normal owned shutdown settle before the labelled fallback.
        for _ in 0..10 {
            if owned(&home).await?.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        let mut errors = Vec::new();
        for id in owned(&home).await? {
            let label = docker(&[
                "inspect",
                "-f",
                "{{index .Config.Labels \"com.duduclaw.computer-use.home\"}}",
                &id,
            ])
            .await;
            if label.as_deref() != Ok(expected.as_str()) {
                errors.push(format!("owned container label check failed for {id}"));
                continue;
            }
            if let Err(error) = docker(&["rm", "-f", &id]).await {
                errors.push(error);
            }
        }
        if !owned(&home).await?.is_empty() {
            errors.push("owned containers remain".into());
        }
        if lwm().await? != before {
            errors.push("production container changed".into());
        }
        if errors.is_empty() {
            Ok::<_, String>(())
        } else {
            Err(errors.join("; "))
        }
    }
    .await;
    let evidence = json!({"status":if matches!(&result,Ok(Ok(_))) && cleanup.is_ok() {"PASS"} else {"FAIL"},
        "driver":format!("{result:?}"),"cleanup":format!("{cleanup:?}")});
    std::fs::write(
        home.join("team-native-result.json"),
        serde_json::to_vec_pretty(&evidence).unwrap(),
    )
    .unwrap();
    assert!(cleanup.is_ok(), "cleanup: {cleanup:?}");
    result
        .expect("native fixture timeout")
        .expect("native fixture driver");
}
