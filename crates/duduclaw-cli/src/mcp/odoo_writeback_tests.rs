use super::*;
use std::sync::Mutex;
use super::super::odoo_identity_client::tests::with_decision;

#[test]
fn text_is_escaped_without_mentions_or_truncation() {
    let body = "<b title=\"x\">中文 & '🙂'</b> @alice";
    for tool in ["odoo_note_post", "odoo_message_send"] {
        let call = parse(tool, &json!({"model":"crm.lead","res_id":1,"body":body}), true).unwrap();
        assert_eq!(call.kwargs["body"], "&lt;b title=&quot;x&quot;&gt;中文 &amp; &#39;🙂&#39;&lt;/b&gt; @alice");
        assert_eq!(call.kwargs["message_type"], "comment");
        let mut params = json!({"model":"crm.lead","res_id":1,"body":"🙂".repeat(10_000)});
        assert!(parse(tool, &params, false).is_ok());
        params["body"] = json!("中".repeat(10_001));
        assert!(parse(tool, &params, false).err().unwrap().contains("10,000"));
        params["body"] = json!("ok");
        params["partner_ids"] = json!([7]);
        assert!(parse(tool, &params, true).is_err());
    }
    for key in ["summary", "note"] {
        let mut params = json!({"model":"crm.lead","res_id":1,"summary":"test"});
        params[key] = json!("中".repeat(10_001));
        assert!(parse("odoo_activity_create", &params, true).is_err());
    }
}

#[test]
fn activity_arguments_are_closed_and_calendar_dates_are_real() {
    for tool in ["odoo_activity_create", "odoo_note_post", "odoo_message_send"] {
        assert_eq!(crate::mcp_auth::tool_requires_scope(tool), Some(crate::mcp_auth::Scope::OdooWrite));
    }
    let base = json!({"model":"crm.lead","res_id":1,"summary":"<review>","note":"a & b"});
    let call = parse("odoo_activity_create", &base, false).unwrap();
    assert_eq!(call.kwargs["summary"], "&lt;review&gt;");
    assert_eq!(call.kwargs["note"], "a &amp; b");
    assert_eq!(call.kwargs["act_type_xmlid"], "mail.mail_activity_data_todo");
    assert!(call.kwargs.get("user_id").is_none());
    for date in ["2026-02-29", "2026-2-03", "2026-13-01", "2026-11-01T00:00:00Z"] {
        let mut p = base.clone(); p["date_deadline"] = json!(date);
        assert!(parse("odoo_activity_create", &p, true).is_err(), "{date}");
    }
    let mut p = base.clone(); p["date_deadline"] = json!("2028-02-29");
    assert!(parse("odoo_activity_create", &p, true).is_ok());
    for id in [json!(0), json!(-1), json!(1.5), json!("7"), Value::Null] {
        p["user_id"] = id;
        assert!(parse("odoo_activity_create", &p, true).is_err());
    }
    p["user_id"] = json!(8);
    assert!(parse("odoo_activity_create", &p, false).is_err());
    assert_eq!(parse("odoo_activity_create", &p, true).unwrap().kwargs["user_id"], 8);
    for model in ["", "CRM.lead", "crm.lead;unlink", "../crm.lead"] {
        let mut p = base.clone(); p["model"] = json!(model);
        assert!(parse("odoo_activity_create", &p, true).is_err());
    }
    for id in [json!(0), json!(-1), json!(1.5), json!("7")] {
        let mut p = base.clone(); p["res_id"] = id;
        assert!(parse("odoo_activity_create", &p, true).is_err());
    }
}

struct Stub {
    url: String,
    requests: Arc<Mutex<Vec<Value>>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Stub { fn drop(&mut self) { self.task.abort(); } }
impl Stub {
    async fn start(found: bool) -> Self {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = requests.clone();
        let task = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                let request: Value = loop {
                    let mut buf = [0u8; 4096];
                    let n = socket.read(&mut buf).await.unwrap();
                    assert!(n > 0);
                    bytes.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&bytes);
                    if let Some((head, body)) = text.split_once("\r\n\r\n") {
                        let len: usize = head.lines().find_map(|line| {
                            let (k,v) = line.split_once(':')?;
                            k.eq_ignore_ascii_case("content-length").then(|| v.trim().parse().unwrap())
                        }).unwrap();
                        if body.len() >= len { break serde_json::from_str(body).unwrap(); }
                    }
                };
                let args = &request["params"]["args"];
                let method = args[4].as_str().unwrap_or("");
                seen.lock().unwrap().push(request.clone());
                let result = match request["params"]["method"].as_str().unwrap_or("") {
                    "authenticate" => json!(if args[1] == "agent" { 17 } else if args[1] == "alice" { 37 } else { 7 }),
                    "version" => json!({"server_version":"18.0"}),
                    _ if method == "fields_get" => json!({"id":{"type":"integer"}}),
                    _ if method == "search_read" && args[3] == "crm.lead" && found => json!([{"id":9}]),
                    _ if method == "activity_schedule" => json!([42]),
                    _ if method == "message_post" => json!(43),
                    _ => json!([]),
                };
                let body = json!({"jsonrpc":"2.0","id":request["id"],"result":result}).to_string();
                let response = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });
        Self { url, requests, task }
    }
    fn writes(&self) -> Vec<Value> {
        self.requests.lock().unwrap().iter().filter(|v|
            matches!(v["params"]["args"][4].as_str(), Some("activity_schedule" | "message_post")))
            .map(|v| v["params"]["args"].clone()).collect()
    }
}

fn home(stub: &Stub, kind: &str, follow: bool, tokens: &[&str]) -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join("agents/bot")).unwrap();
    std::fs::write(home.path().join("config.toml"), format!(
        "[odoo]\nurl='{}'\ndb='test'\nusername='company'\napi_key_enc='literal-test-key'\nmember_credentials=true\n[odoo_access]\ncompany_key_agents=['bot']\nalways_refuse=[]\npermission_mode='{}'\npermission_mode_ack={{by='owner',at='2026-11-01T00:00:00Z'}}\n",
        stub.url, if follow { "follow_odoo_account" } else { "engine" })).unwrap();
    let credentials = if kind == "agent" { "username='agent'\napi_key_enc='agent-test-key'\n" } else { "" };
    std::fs::write(home.path().join("agents/bot/agent.toml"), format!(
        "[agent]\nname='bot'\nstatus='active'\n[odoo]\nallowed_models=['crm.lead']\nallowed_actions={}\n{credentials}", json!(tokens))).unwrap();
    if kind == "personal" { super::super::odoo_access_tests::personal_db(home.path(), "bot", true); }
    home
}

fn decision(kind: &str, follow: bool) -> Value {
    if kind == "personal" {
        json!({"ok":true,"identity":"personal","permission":if follow {"follow"} else {"engine"},
            "user_id":"alice","odoo_uid":37,"login":"alice","rev":1,"secret":"alice-test-key"})
    } else { json!({"ok":true,"identity":kind,"permission":"engine"}) }
}

async fn call(home: &Path, kind: &str, follow: bool, tool: &str, args: &Value) -> Value {
    let pool: OdooState = Arc::new(crate::odoo_pool::OdooConnectorPool::default());
    with_decision(home, decision(kind, follow),
        super::super::odoo::handle_odoo_tool(tool, args, home, &pool, "bot")).await
}

#[tokio::test]
async fn activities_use_the_gateway_identity_and_engine_token() {
    for (kind, uid) in [("company",7), ("agent",17), ("personal",37)] {
        let stub = Stub::start(true).await;
        let home = home(&stub, kind, false, &["activity:crm.lead"]);
        let mut args = json!({"model":"crm.lead","res_id":9,"summary":"test","user_id":8});
        let denied = call(home.path(), kind, false, "odoo_activity_create", &args).await;
        assert_eq!(denied["isError"], true, "{denied}");
        assert!(stub.writes().is_empty());
        args.as_object_mut().unwrap().remove("user_id");
        let result = call(home.path(), kind, false, "odoo_activity_create", &args).await;
        assert_ne!(result["isError"], true, "{result}");
        let writes = stub.writes();
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0][1], uid);
        assert_eq!(writes[0][5], json!([[9]]));
        assert!(writes[0][6].get("user_id").is_none());
        assert_eq!(writes[0][6]["act_type_xmlid"], "mail.mail_activity_data_todo");
        assert!(result.to_string().contains(match kind { "personal" => "alice", "company" => "公司帳號", _ => "AI 員工帳號" }));
    }
    let stub = Stub::start(true).await;
    let no_token = home(&stub, "personal", false, &[]);
    assert_eq!(call(no_token.path(), "personal", false, "odoo_activity_create",
        &json!({"model":"crm.lead","res_id":9,"summary":"test"})).await["isError"], true);
    assert!(stub.writes().is_empty());
    let follow = home(&stub, "personal", true, &[]);
    let result = call(follow.path(), "personal", true, "odoo_activity_create",
        &json!({"model":"crm.lead","res_id":9,"summary":"test","user_id":8})).await;
    assert_ne!(result["isError"], true, "{result}");
    assert_eq!(stub.writes()[0][6]["user_id"], 8);
    assert_eq!(stub.writes()[0][1], 37);
}

#[tokio::test]
async fn note_kwargs_and_missing_record_refusal_reach_the_stub() {
    for found in [true, false] {
        let stub = Stub::start(found).await;
        let home = home(&stub, "personal", true, &[]);
        let args = json!({"model":"crm.lead","res_id":9,"body":"<b>中文</b> @alice"});
        let result = call(home.path(), "personal", true, "odoo_note_post", &args).await;
        assert_eq!(result["isError"] == true, !found, "{result}");
        let writes = stub.writes();
        assert_eq!(writes.len(), usize::from(found));
        if found {
            assert_eq!(writes[0][6]["subtype_xmlid"], "mail.mt_note");
            assert_eq!(writes[0][6]["partner_ids"], json!([]));
            assert_eq!(writes[0][6]["body"], "&lt;b&gt;中文&lt;/b&gt; @alice");
        }
        let oversized = json!({"model":"crm.lead","res_id":9,"body":"中".repeat(10_001)});
        assert_eq!(call(home.path(), "personal", true, "odoo_note_post", &oversized).await["isError"], true);
        assert_eq!(stub.writes().len(), usize::from(found));
    }
}

#[tokio::test]
async fn follow_message_admin_refusal_prevents_every_write() {
    let stub = Stub::start(true).await;
    let home = home(&stub, "personal", true, &[]);
    let config = home.path().join("config.toml");
    let raw = std::fs::read_to_string(&config).unwrap().replace("always_refuse=[]", "always_refuse=['message:*']");
    std::fs::write(config, raw).unwrap();
    let result = call(home.path(), "personal", true, "odoo_message_send",
        &json!({"model":"crm.lead","res_id":9,"body":"test"})).await;
    assert_eq!(result["isError"], true);
    assert!(result.to_string().contains("一律不允許"));
    assert!(stub.writes().is_empty());
}

#[tokio::test]
async fn dispatch_approval_gate_denies_or_sends_once_despite_allow_rule() {
    use duduclaw_gateway::approval::{ApprovalBroker, ApprovalStatus};
    use super::super::approval::gate_tool_approval_dispatch;
    for approved in [false, true] {
        let stub = Stub::start(true).await;
        let home = home(&stub, "personal", true, &[]);
        let path = home.path().join("agents/bot/agent.toml");
        let raw = std::fs::read_to_string(&path).unwrap();
        std::fs::write(path, format!("{raw}\n[capabilities]\naction_rules=[{{tool='odoo_message_send',verdict='allow'}}]\n")).unwrap();
        let args = json!({"model":"crm.lead","res_id":9,"body":"test"});
        let payload = json!({"name":"odoo_message_send","arguments":args});
        let broker = ApprovalBroker::open(home.path()).unwrap();
        let observer = broker.clone();
        let seen = Arc::new(Mutex::new(None));
        let captured = seen.clone();
        let answer = tokio::spawn(async move {
            for _ in 0..200 {
                let pending = observer.list_pending(Some("bot")).await.unwrap();
                if let Some(card) = pending.first() {
                    assert_eq!(card.payload["action_grant"]["single_use"], true);
                    *captured.lock().unwrap() = Some(card.id.clone());
                    observer.decide(&card.id, approved, "dashboard:admin").await.unwrap();
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            panic!("fixed approval did not produce a card");
        });
        let outcome = tokio::time::timeout(std::time::Duration::from_secs(5),
            gate_tool_approval_dispatch(home.path(), "bot", "odoo_message_send", payload)).await.unwrap();
        answer.await.unwrap();
        assert_eq!(outcome.is_ok(), approved);
        assert!(stub.writes().is_empty());
        if outcome.is_ok() {
            let result = call(home.path(), "personal", true, "odoo_message_send", &args).await;
            assert_ne!(result["isError"], true, "{result}");
            let id = seen.lock().unwrap().clone().unwrap();
            assert_eq!(broker.get(&id).await.unwrap().unwrap().status, ApprovalStatus::Invalidated);
        }
        assert_eq!(stub.writes().len(), usize::from(approved));
    }
}

#[tokio::test]
async fn message_approval_is_fixed_content_bound_and_single_use() {
    use duduclaw_gateway::approval::{ApprovalBroker, ApprovalStore};
    use duduclaw_gateway::approval::action_grant::{ActionGrant, ACTION_GRANT_KEY, binding_applies, verify_and_consume};
    let stub = Stub::start(true).await;
    let home = home(&stub, "personal", true, &[]);
    let agent_dir = home.path().join("agents/bot");
    let path = agent_dir.join("agent.toml");
    let mut raw = std::fs::read_to_string(&path).unwrap();
    raw.push_str("\n[capabilities]\naction_rules=[{tool='odoo_message_send',verdict='allow'}]\n");
    std::fs::write(path, raw).unwrap();
    let args = json!({"model":"crm.lead","res_id":9,"body":"<hello> @alice"});
    let payload = json!({"name":"odoo_message_send","arguments":args});
    assert!(super::super::approval::static_gate_flags(&agent_dir, "odoo_message_send", &payload).0);
    assert!(binding_applies("odoo_message_send", false));
    assert_eq!(crate::mcp_auth::tool_requires_scope("odoo_message_send"), Some(crate::mcp_auth::Scope::OdooWrite));
    let broker = ApprovalBroker::new(Arc::new(ApprovalStore::open_in_memory().unwrap()));
    let grant = ActionGrant::build("bot", "odoo_message_send", &payload);
    let mut bound = payload.clone(); bound[ACTION_GRANT_KEY] = grant.to_json();
    let id = broker.request("bot", "mcp_call", "Send to followers", bound, 60).await.unwrap();
    // The same gate as run_approval_bound: only consume success permits I/O.
    if verify_and_consume(&broker, &id, &grant).await.is_ok() {
        call(home.path(), "personal", true, "odoo_message_send", &args).await;
    }
    assert!(stub.writes().is_empty());
    broker.decide(&id, true, "dashboard:admin").await.unwrap();
    for (key, value) in [("body", json!("changed")), ("model", json!("sale.order")), ("res_id", json!(10))] {
        let mut changed = payload.clone(); changed["arguments"][key] = value;
        let changed_grant = ActionGrant::build("bot", "odoo_message_send", &changed);
        assert!(verify_and_consume(&broker, &id, &changed_grant).await.is_err());
    }
    assert!(stub.writes().is_empty());
    verify_and_consume(&broker, &id, &grant).await.unwrap();
    let result = call(home.path(), "personal", true, "odoo_message_send", &args).await;
    assert_ne!(result["isError"], true, "{result}");
    assert!(verify_and_consume(&broker, &id, &grant).await.is_err());
    let writes = stub.writes();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0][6]["subtype_xmlid"], "mail.mt_comment");
    assert_eq!(writes[0][6]["message_type"], "comment");
    assert_eq!(writes[0][6]["body"], "&lt;hello&gt; @alice");
    assert!(writes[0][6].get("partner_ids").is_none());
}
