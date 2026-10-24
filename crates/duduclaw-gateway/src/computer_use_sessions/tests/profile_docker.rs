//! Opt-in real orchestrator proof; no model, account or network is used.
#![cfg(unix)]

use super::*;

async fn command(args: &[&str]) -> Result<String, String> {
    let output = crate::computer_use_orchestrator::docker_output(
        args,
        Duration::from_secs(30),
        "profile acceptance",
    )
    .await
    .map_err(|e| format!("docker: {e:?}"))?;
    if !output.status.success() {
        return Err(format!(
            "docker command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().into())
}

async fn production_fingerprint() -> String {
    command(&[
        "inspect",
        "-f",
        "{{.Id}} {{.State.Running}} {{.State.StartedAt}}",
        "duduclaw-lwm-exp",
    ])
    .await
    .unwrap_or_else(|_| "absent".into())
}

async fn only_container(home: &std::path::Path) -> Result<String, String> {
    let ids = containers_of(home).await;
    if ids.lines().count() != 1 {
        return Err(format!("expected one owned container: {ids}"));
    }
    Ok(ids)
}

async fn cookie(container: &str, seed: bool) -> Result<(), String> {
    let code = r#"import sys,time
sys.dont_write_bytecode=True
sys.path.insert(0,'/usr/local/bin')
from duduclaw_cdp import list_pages,WebSocket
ws=WebSocket(list_pages(5)[0],5)
if sys.argv[1]=='seed':
    assert ws.call('Network.setCookie',{'name':'dudu_fixture','value':'synthetic-only','url':'https://example.com','expires':time.time()+86400})['success']
cookies=ws.call('Network.getCookies',{'urls':['https://example.com']})['cookies']
assert any(c['name']=='dudu_fixture' and c['value']=='synthetic-only' for c in cookies)
"#;
    command(&[
        "exec",
        container,
        "python3",
        "-c",
        code,
        if seed { "seed" } else { "verify" },
    ])
    .await?;
    Ok(())
}

#[tokio::test]
#[ignore = "needs Docker and DUDU_COMPUTER_USE_IMAGE with the profile helper"]
async fn computer_profile_real_gateway_roundtrip_and_reset() {
    let image = std::env::var("DUDU_COMPUTER_USE_IMAGE").expect("set DUDU_COMPUTER_USE_IMAGE");
    let tmp = home();
    let home = tmp.keep(); // Retain even failed runs for inspection.
    eprintln!("PROFILE_ARTIFACTS {}", home.display());
    write_config(
        &home,
        &format!("[computer_use]\nimage={image:?}\n[computer_use.profiles]\nenabled=true\n"),
    );
    write_agent(
        &home,
        "alice",
        "[capabilities]\ncomputer_use=true\n[capabilities.computer_use_config]\npersist_login=true\n",
    );
    std::fs::write(home.join(".keyfile"), [42u8; 32]).unwrap();
    duduclaw_core::platform::set_owner_only(&home.join(".keyfile")).unwrap();
    let before = production_fingerprint().await;
    let body = async {
        let manager = ComputerUseSessions::with_parts(
            home.clone(),
            backend::orchestrator_factory(),
            Duration::from_secs(600),
        );
        let first = manager
            .start("alice", StartRequest::default())
            .await
            .map_err(|e| format!("first start: {e:?}"))?;
        if first["login_state"] != "empty" {
            return Err(format!("first state: {}", first["login_state"]));
        }
        cookie(&only_container(&home).await?, true).await?;
        let stopped = manager
            .stop("alice", None)
            .await
            .map_err(|e| format!("stop: {e:?}"))?;
        if stopped["login_state"] != "saved" {
            return Err(format!("save state: {}", stopped["login_state"]));
        }
        let saved =
            crate::computer_profiles::state(&home, "alice")?.ok_or("missing saved metadata")?;
        let sealed = std::fs::read(
            home.join(crate::computer_profiles::ROOT)
                .join("alice")
                .join(format!("profile-{}.sealed", saved.generation)),
        )
        .map_err(|e| e.to_string())?;
        if sealed
            .windows(b"synthetic-only".len())
            .any(|s| s == b"synthetic-only")
        {
            return Err("plaintext cookie on host".into());
        }
        let second = manager
            .start("alice", StartRequest::default())
            .await
            .map_err(|e| format!("second start: {e:?}"))?;
        if second["login_state"] != "restored" {
            return Err(format!("restore state: {}", second["login_state"]));
        }
        cookie(&only_container(&home).await?, false).await?;
        manager
            .reset_login("alice", &duduclaw_auth::UserContext::admin_fallback())
            .await
            .map_err(|e| format!("reset: {e:?}"))?;
        if crate::computer_profiles::state(&home, "alice")?.is_some() {
            return Err("reset kept saved state".into());
        }
        if !manager.is_empty() {
            return Err("reset kept live session".into());
        }
        Ok::<_, String>(
            json!({"status":"PASS","saved_bytes":saved.sealed_bytes,"generation":saved.generation,"synthetic_cookie":true,"reset":true}),
        )
    };
    let result = tokio::time::timeout(Duration::from_secs(180), body).await;
    let evidence = match &result {
        Ok(Ok(value)) => value.clone(),
        Ok(Err(error)) => json!({"status":"FAIL","error":error}),
        Err(_) => json!({"status":"FAIL","error":"acceptance timeout"}),
    };
    std::fs::write(
        home.join("profile-evidence.json"),
        serde_json::to_vec_pretty(&evidence).unwrap(),
    )
    .unwrap();
    // Only this fresh home's labelled containers; never prune or remove volumes.
    for id in containers_of(&home).await.lines() {
        let _ = command(&["rm", "-f", id]).await;
    }
    assert_eq!(
        before,
        production_fingerprint().await,
        "production container changed"
    );
    assert!(
        containers_of(&home).await.is_empty(),
        "owned container left behind"
    );
    eprintln!("MEASURE {evidence}");
    result
        .expect("acceptance timeout")
        .expect("gateway profile acceptance");
}
