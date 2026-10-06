use super::*;

async fn seed_read(home: &Path, session: &Session, args: &Value) -> (String, String) {
    seed_call(home, session, "web_fetch_cached", args, true, vec![]).await
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workflow_stdio_userinfo_rpc_denial_before_network() {
    let (home, key) = setup();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::<()>::new()));
    let observed = requests.clone();
    let proxy = tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            observed.lock().unwrap().push(());
            drop(socket);
        }
    });
    let url = format!("http://{address}");
    let env = vec![
        ("HTTP_PROXY".into(), url.clone()),
        ("http_proxy".into(), url),
        ("NO_PROXY".into(), String::new()),
        ("no_proxy".into(), String::new()),
    ];
    let mut alice = session_with_env(home.path(), "alice", &key, &env).await;
    // Userinfo must never reach the isolated proxy or successful source evidence.
    for url in [
        "http://fixture-user:fixture-password@example.com/p1-stdio/private",
        "http://fixture-user@example.com/p1-stdio/private",
        "http://:fixture-password@example.com/p1-stdio/private",
        "http://fixture-user:@example.com/p1-stdio/private",
        "http://%66ixture-user:%66ixture-password@example.com/p1-stdio/private",
        "http://:%66ixture-password@example.com/p1-stdio/private",
    ] {
        let args = json!({"url":url});
        let (run, step) = seed_read(home.path(), &alice, &args).await;
        let ctx = context(&alice, &run, &step, &args);
        let error = tokio::time::timeout(
            Duration::from_secs(5),
            alice
                .client
                .read_workflow_call(json!({"name":"web_fetch_cached","arguments":args}), ctx),
        )
        .await
        .expect("userinfo denial must arrive before the 30-second transport timeout")
        .unwrap_err();
        let McpError::Rpc { code, message } = error else {
            let class = match error {
                McpError::Timeout => "timeout",
                McpError::Closed => "closed",
                McpError::Spawn(_) => "spawn",
                McpError::Io(_) => "io",
                McpError::Parse(_) => "parse",
                McpError::Rpc { .. } => unreachable!(),
            };
            panic!("userinfo requires RPC denial; actual error class: {class}");
        };
        assert_eq!(code, -32003);
        assert_eq!(message, "workflow public URL rejected");
        let error = message;
        assert!(
            !error.contains("fixture-user")
                && !error.contains("fixture-password")
                && !error.contains(url)
        );
        assert_eq!(
            requests.lock().unwrap().len(),
            0,
            "userinfo refused before any HTTP request"
        );
        let stored = WorkflowStore::open(home.path())
            .unwrap()
            .get_step(&run, &step)
            .await
            .unwrap()
            .unwrap();
        assert!(
            stored.output.is_none() && stored.receipt.is_none(),
            "rejected credentials never become source evidence"
        );
    }
    proxy.abort();
    let _ = proxy.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workflow_stdio_three_exact_public_pages_fake_proxy_evidence_and_redirect_rejection() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (home, key) = setup();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let observed = requests.clone();
    let proxy = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let observed = observed.clone();
            tokio::spawn(async move {
                let mut bytes = Vec::new();
                let mut chunk = [0; 1024];
                while !bytes.windows(4).any(|w| w == b"\r\n\r\n") {
                    let n = socket.read(&mut chunk).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    bytes.extend_from_slice(&chunk[..n]);
                    assert!(bytes.len() < 8192);
                }
                let request = String::from_utf8(bytes).unwrap();
                let first = request.lines().next().unwrap().to_string();
                observed.lock().unwrap().push(first.clone());
                let (status, headers, body) = if first.contains("/redirect") {
                    (
                        "302 Found",
                        "Location: http://example.com/escape\r\n",
                        "redirect".to_string(),
                    )
                } else if first.contains("/login") {
                    (
                        "200 OK",
                        "",
                        "<form><input type=\"password\"></form>".to_string(),
                    )
                } else if first.contains("/empty") {
                    ("200 OK", "", String::new())
                } else {
                    (
                        "200 OK",
                        "",
                        format!("<html><body>fixture public DATA {first}</body></html>"),
                    )
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: text/html\r\nContent-Length: {}\r\n{headers}Connection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap();
                socket.shutdown().await.unwrap();
            });
        }
    });
    let proxy_url = format!("http://{address}");
    let env = vec![
        ("HTTP_PROXY".into(), proxy_url.clone()),
        ("http_proxy".into(), proxy_url),
        ("NO_PROXY".into(), String::new()),
        ("no_proxy".into(), String::new()),
    ];
    let mut alice = session_with_env(home.path(), "alice", &key, &env).await;
    for page in ["page-a", "page-b", "page-c"] {
        // Public target stays under the production URL/DNS/SSRF checks. The
        // isolated transport proxy is fake; this is not a live public-provider claim.
        let url = format!("http://example.com/p1-stdio/{page}");
        let args = json!({"url":url});
        let (run, step) = seed_read(home.path(), &alice, &args).await;
        let ctx = context(&alice, &run, &step, &args);
        let reply = alice
            .client
            .read_workflow_call(json!({"name":"web_fetch_cached","arguments":args}), ctx)
            .await
            .unwrap();
        assert_eq!(reply.result["url"], url);
        assert!(reply.result["body"].as_str().unwrap().contains(page));
        assert_eq!(reply.evidence["source_url"], url);
        assert_eq!(reply.evidence["redirects_followed"], 0);
        assert_eq!(reply.evidence["cached"], false);
        assert_eq!(reply.evidence["authenticated"], false);
        assert_eq!(reply.result_hash, payload_hash(&reply.result));
        assert!(
            reply.evidence["source_hash"]
                .as_str()
                .is_some_and(|h| h.len() == 64)
        );
    }
    for page in ["redirect", "login", "empty"] {
        let args = json!({"url":format!("http://example.com/p1-stdio/{page}")});
        let (run, step) = seed_read(home.path(), &alice, &args).await;
        let ctx = context(&alice, &run, &step, &args);
        assert!(
            alice
                .client
                .read_workflow_call(json!({"name":"web_fetch_cached","arguments":args}), ctx)
                .await
                .is_err()
        );
    }
    let args = json!({"url":"http://127.0.0.1/internal"});
    let (run, step) = seed_read(home.path(), &alice, &args).await;
    let ctx = context(&alice, &run, &step, &args);
    assert!(
        alice
            .client
            .read_workflow_call(json!({"name":"web_fetch_cached","arguments":args}), ctx)
            .await
            .is_err(),
        "fixture never weakens internal URL deny"
    );
    assert_eq!(requests.lock().unwrap().len(), 6);
    assert!(
        !requests
            .lock()
            .unwrap()
            .iter()
            .any(|r| r.contains("/escape"))
    );
    assert!(
        !home.path().join("web_cache").exists(),
        "workflow read has no cache effect"
    );
    proxy.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workflow_stdio_two_process_same_operation_cas_executes_original_handler_once() {
    let (home, key) = setup();
    let (mut first, mut second) = tokio::join!(
        session(home.path(), "alice", &key),
        session(home.path(), "alice", &key)
    );
    let id = task(&mut first, "before-race").await;
    let args = json!({"task_id":id,"title":"after-race"});
    let call = json!({"name":"tasks_update","arguments":args});
    let (run, step) = seed(home.path(), &first, "tasks_update", &args).await;
    let first_context = context(&first, &run, &step, &args);
    let second_context = context(&second, &run, &step, &args);
    let (first_prepare, second_prepare) = tokio::join!(
        first
            .client
            .prepare_workflow_call(call.clone(), first_context),
        second
            .client
            .prepare_workflow_call(call.clone(), second_context)
    );
    let first_prepare = first_prepare.unwrap();
    let second_prepare = second_prepare.unwrap();
    let first_ticket = authorized_ticket(home.path(), &first, &first_prepare).await;
    let mut second_ticket = first_ticket.clone();
    second_ticket.session_id = second.id.clone();
    second_ticket.prepare_digest = workflow_mcp::digest(&second_prepare).unwrap();
    second_ticket.mac = workflow_mcp::sign(
        &second.secret,
        "execute",
        &workflow_mcp::unsigned(&second_ticket).unwrap(),
    )
    .unwrap();
    let counter = rusqlite::Connection::open(home.path().join("tasks.db")).unwrap();
    counter
        .execute_batch("CREATE TABLE fixture_effect_counter(n INTEGER NOT NULL);INSERT INTO fixture_effect_counter
            VALUES(0);CREATE TRIGGER fixture_effect_count AFTER UPDATE OF title ON tasks
            BEGIN UPDATE fixture_effect_counter SET n=n+1;END;")
        .unwrap();
    let (first_result, second_result) = tokio::join!(
        first
            .client
            .execute_workflow_call(call.clone(), first_ticket.clone()),
        second.client.execute_workflow_call(call, second_ticket)
    );
    assert_eq!(
        usize::from(first_result.is_ok()) + usize::from(second_result.is_ok()),
        1,
        "one stdio child wins the before-effect CAS"
    );
    let effects: i64 = counter
        .query_row("SELECT n FROM fixture_effect_counter", [], |row| row.get(0))
        .unwrap();
    assert_eq!(effects, 1);
    let ledger = ApprovalBroker::open(home.path())
        .unwrap()
        .inspect_operation(&first_ticket.operation_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        ledger.state,
        duduclaw_gateway::approval::OperationState::Succeeded
    );
    assert!(ledger.receipt.is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workflow_stdio_source_audience_inherits_acl_and_explicit_user_and_revocation() {
    for explicit in [false, true] {
        let (home, key) = setup();
        let principal = fixture_principal(home.path());
        let mut alice = session(home.path(), "alice", &key).await;
        let id = task(&mut alice, "before-audience").await;
        let args = json!({"task_id":id,"title":"after-audience"});
        let audience = if explicit {
            vec![format!("user:{principal}")]
        } else {
            vec![]
        };
        let (run, step) =
            seed_call(home.path(), &alice, "tasks_update", &args, false, audience).await;
        let call = json!({"name":"tasks_update","arguments":args});
        let prepared = alice
            .client
            .prepare_workflow_call(call.clone(), context(&alice, &run, &step, &args))
            .await
            .unwrap();
        let ticket = authorized_ticket(home.path(), &alice, &prepared).await;
        let reply = alice
            .client
            .execute_workflow_call(call, ticket)
            .await
            .unwrap();
        assert_eq!(reply.state, "succeeded");
        assert_eq!(
            duduclaw_gateway::task_store::TaskStore::open(home.path())
                .unwrap()
                .get_task(&id)
                .await
                .unwrap()
                .unwrap()
                .title,
            "after-audience"
        );
    }
    {
        let (home, key) = setup();
        let mut alice = session(home.path(), "alice", &key).await;
        let id = task(&mut alice, "before-foreign-audience").await;
        let args = json!({"task_id":id,"title":"must-not-write"});
        let (run, step) = seed_call(
            home.path(),
            &alice,
            "tasks_update",
            &args,
            false,
            vec!["user:foreign-dashboard-user".into()],
        )
        .await;
        let call = json!({"name":"tasks_update","arguments":args});
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            alice
                .client
                .prepare_workflow_call(call, context(&alice, &run, &step, &args)),
        )
        .await
        .unwrap();
        assert!(matches!(result, Err(McpError::Rpc { code: -32003, .. })));
        assert_eq!(
            duduclaw_gateway::task_store::TaskStore::open(home.path())
                .unwrap()
                .get_task(&id)
                .await
                .unwrap()
                .unwrap()
                .title,
            "before-foreign-audience"
        );
    }
    let (home, key) = setup();
    let principal = fixture_principal(home.path());
    let mut alice = session(home.path(), "alice", &key).await;
    let id = task(&mut alice, "before-revocation").await;
    let args = json!({"task_id":id,"title":"must-not-write"});
    let (run, step) = seed(home.path(), &alice, "tasks_update", &args).await;
    let call = json!({"name":"tasks_update","arguments":args});
    let prepared = alice
        .client
        .prepare_workflow_call(call.clone(), context(&alice, &run, &step, &args))
        .await
        .unwrap();
    let ticket = authorized_ticket(home.path(), &alice, &prepared).await;
    duduclaw_auth::UserDb::new(&home.path().join("users.db"))
        .unwrap()
        .unbind_agent(&principal, "alice")
        .unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        alice.client.execute_workflow_call(call, ticket),
    )
    .await
    .unwrap();
    assert!(matches!(result, Err(McpError::Rpc { code: -32003, .. })));
    assert_eq!(
        duduclaw_gateway::task_store::TaskStore::open(home.path())
            .unwrap()
            .get_task(&id)
            .await
            .unwrap()
            .unwrap()
            .title,
        "before-revocation"
    );
}

