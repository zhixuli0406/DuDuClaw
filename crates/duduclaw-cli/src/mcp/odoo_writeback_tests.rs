use super::*;
use serde_json::json;
use std::sync::Mutex;
use super::super::odoo_identity_client::tests::with_decision;

#[path = "odoo_preconnect_tests.rs"]
mod preconnect_tests;

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
                    _ if method == "search_read" && (args[3] == "crm.lead" || args[3] == "sale.order") && found => json!([{"id":9}]),
                    _ if method == "activity_schedule" => json!([42]),
                    _ if method == "message_post" => json!(43),
                    _ if method == "create" && args[3] == "ir.attachment" => json!(44),
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
            matches!(v["params"]["args"][4].as_str(), Some("activity_schedule" | "message_post" | "action_send_customer_email")))
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
    use super::super::approval::{gate_tool_approval_dispatch, with_odoo_approval_call};
    for (tool, args, follow) in [
        ("odoo_message_send", json!({"model":"crm.lead","res_id":9,"body":"test"}), false),
        ("odoo_message_send", json!({"model":"crm.lead","res_id":9,"body":"test"}), true),
        ("odoo_execute", json!({"model":"crm.lead","method":"action_send_customer_email","ids":"[9]"}), true),
    ] {
    for approved in [false, true] {
        with_odoo_approval_call(async {
        let stub = Stub::start(true).await;
        let home = home(&stub, "personal", follow, &["message:crm.lead"]);
        let path = home.path().join("agents/bot/agent.toml");
        let raw = std::fs::read_to_string(&path).unwrap();
        std::fs::write(path, format!("{raw}\n[capabilities]\naction_rules=[{{tool='{tool}',verdict='allow'}}]\n")).unwrap();
        let payload = json!({"name":tool,"arguments":args});
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
            gate_tool_approval_dispatch(home.path(), "bot", tool, payload)).await.unwrap();
        answer.await.unwrap();
        assert_eq!(outcome.is_ok(), approved);
        assert!(stub.writes().is_empty());
        if outcome.is_ok() {
            let result = call(home.path(), "personal", follow, tool, &args).await;
            assert_ne!(result["isError"], true, "{result}");
            if follow {
                let replay = call(home.path(), "personal", follow, tool, &args).await;
                assert_eq!(replay["isError"], true, "{replay}");
                assert!(replay.to_string().contains("核准"));
            }
            let id = seen.lock().unwrap().clone().unwrap();
            assert_eq!(broker.get(&id).await.unwrap().unwrap().status, ApprovalStatus::Invalidated);
        }
        assert_eq!(stub.writes().len(), usize::from(approved));
        }).await;
    }
    }
}

#[tokio::test]
async fn message_approval_is_fixed_content_bound_and_single_use() {
    use super::super::approval::{consume_bound_approval, with_odoo_approval_call};
    use duduclaw_gateway::approval::{ApprovalBroker, ApprovalStore};
    use duduclaw_gateway::approval::action_grant::{ActionGrant, ACTION_GRANT_KEY, binding_applies};
    let stub = Stub::start(true).await;
    let home = home(&stub, "personal", true, &[]);
    let agent_dir = home.path().join("agents/bot");
    let path = agent_dir.join("agent.toml");
    let mut raw = std::fs::read_to_string(&path).unwrap();
    raw.push_str("\n[capabilities]\naction_rules=[{tool='odoo_message_send',verdict='allow'}]\n");
    std::fs::write(path, raw).unwrap();
    let args = json!({"model":"crm.lead","res_id":9,"body":"<hello> @alice"});
    let payload = json!({"name":"odoo_message_send","arguments":args});
    assert!(super::super::approval::static_gate_flags(&agent_dir, "odoo_message_send", &payload, crate::mcp::approval::OdooApprovalMode::Engine).0);
    assert!(binding_applies("odoo_message_send", false));
    assert_eq!(crate::mcp_auth::tool_requires_scope("odoo_message_send"), Some(crate::mcp_auth::Scope::OdooWrite));
    let broker = ApprovalBroker::new(Arc::new(ApprovalStore::open_in_memory().unwrap()));
    let grant = ActionGrant::build("bot", "odoo_message_send", &payload);
    let mut bound = payload.clone(); bound[ACTION_GRANT_KEY] = grant.to_json();
    let id = broker.request("bot", "mcp_call", "Send to followers", bound, 60).await.unwrap();
    // This handler-level test uses the same scope and consume/record step as
    // the real dispatcher; consuming a broker row alone is not execution proof.
    with_odoo_approval_call(async {
        if consume_bound_approval(&broker, &id, &grant).await.is_ok() {
            call(home.path(), "personal", true, "odoo_message_send", &args).await;
        }
        assert!(stub.writes().is_empty());
        broker.decide(&id, true, "dashboard:admin").await.unwrap();
        for (key, value) in [("body", json!("changed")), ("model", json!("sale.order")), ("res_id", json!(10))] {
            let mut changed = payload.clone(); changed["arguments"][key] = value;
            let changed_grant = ActionGrant::build("bot", "odoo_message_send", &changed);
            assert!(consume_bound_approval(&broker, &id, &changed_grant).await.is_err());
        }
        assert!(stub.writes().is_empty());
        consume_bound_approval(&broker, &id, &grant).await.unwrap();
        let result = call(home.path(), "personal", true, "odoo_message_send", &args).await;
        assert_ne!(result["isError"], true, "{result}");
        assert!(consume_bound_approval(&broker, &id, &grant).await.is_err());
    }).await;
    let writes = stub.writes();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0][6]["subtype_xmlid"], "mail.mt_comment");
    assert_eq!(writes[0][6]["message_type"], "comment");
    assert_eq!(writes[0][6]["body"], "&lt;hello&gt; @alice");
    assert!(writes[0][6].get("partner_ids").is_none());
}

#[tokio::test]
async fn follow_approval_is_single_use_across_dispatch_scopes() {
    use super::super::approval::{approve_odoo_call_for_test, with_odoo_approval_call};
    for (tool, args) in [
        ("odoo_message_send", json!({"model":"crm.lead","res_id":9,"body":"test"})),
        ("odoo_execute", json!({"model":"crm.lead","method":"action_send_customer_email","ids":"[9]"})),
    ] {
        let stub = Stub::start(true).await;
        let home = home(&stub, "personal", true, &[]);
        // Dispatch A consumes the approved row and executes exactly once.
        let first = with_odoo_approval_call(async {
            approve_odoo_call_for_test("bot", tool, &args).await;
            call(home.path(), "personal", true, tool, &args).await
        }).await;
        assert_ne!(first["isError"], true, "{first}");
        assert_eq!(stub.writes().len(), 1);
        // Dispatch B on the same task starts empty, even for identical args.
        let second = with_odoo_approval_call(
            call(home.path(), "personal", true, tool, &args)).await;
        assert_follow_approval_refused(&second);
        // Also discard proof when an approved dispatch exits before execution.
        with_odoo_approval_call(approve_odoo_call_for_test("bot", tool, &args)).await;
        let after_abandoned = with_odoo_approval_call(
            call(home.path(), "personal", true, tool, &args)).await;
        assert_follow_approval_refused(&after_abandoned);
        assert_eq!(stub.writes().len(), 1);
    }
}

#[tokio::test]
async fn follow_approval_for_other_arguments_is_refused() {
    use super::super::approval::{approve_odoo_call_for_test, with_odoo_approval_call};
    for (tool, args, key, changed) in [
        ("odoo_message_send", json!({"model":"crm.lead","res_id":9,"body":"test"}), "body", json!("changed")),
        ("odoo_execute", json!({"model":"crm.lead","method":"action_send_customer_email","ids":"[9]"}), "ids", json!("[10]")),
    ] {
        let stub = Stub::start(true).await;
        let home = home(&stub, "personal", true, &[]);
        with_odoo_approval_call(async {
            approve_odoo_call_for_test("bot", tool, &args).await;
            let mut other = args.clone();
            other[key] = changed;
            let result = call(home.path(), "personal", true, tool, &other).await;
            assert_follow_approval_refused(&result);
            // A mismatch burns the in-call proof too; it cannot be retried.
            let original = call(home.path(), "personal", true, tool, &args).await;
            assert_follow_approval_refused(&original);
        }).await;
        assert!(stub.requests.lock().unwrap().is_empty(), "refused before Odoo I/O");
    }
}

fn assert_follow_approval_refused(result: &Value) {
    assert_eq!(result, &json!({
        "isError": true,
        "content": [{"type":"text", "text":"Error: 這次 Odoo 操作需要重新取得人工核准，尚未執行。"}]
    }));
}

fn attachment_requests(stub: &Stub) -> Vec<Value> {
    stub.requests.lock().unwrap().iter().filter(|v|
        v["params"]["args"][3] == "ir.attachment" && v["params"]["args"][4] == "create")
        .map(|v| v["params"]["args"].clone()).collect()
}

fn attachment_file(home: &Path) {
    std::fs::create_dir_all(home.join("agents/bot/attachments")).unwrap();
    std::fs::write(home.join("agents/bot/attachments/x.pdf"),
        super::super::odoo_attachment::tests::clean_pdf()).unwrap();
}

async fn attachment_call(home: &Path, kind: &str, follow: bool, params: &Value) -> Value {
    super::super::with_token_verified_caller("bot", call(home, kind, follow, "odoo_attachment_add", params)).await
}

#[tokio::test]
async fn attachment_upload_is_private_identity_bound_and_content_is_not_echoed() {
    use base64::Engine;
    assert_eq!(crate::mcp_auth::tool_requires_scope("odoo_attachment_add"), Some(crate::mcp_auth::Scope::OdooWrite));
    for (kind, follow, uid) in [("company", false, 7), ("agent", false, 17), ("personal", true, 37)] {
        let stub = Stub::start(true).await;
        let home = home(&stub, kind, follow, &["attach:crm.lead"]);
        attachment_file(home.path());
        let args = json!({"model":"crm.lead","res_id":9,"file":"x.pdf","name":"客戶文件.pdf"});
        let result = attachment_call(home.path(), kind, follow, &args).await;
        assert_ne!(result["isError"], true, "{result}");
        assert!(result.to_string().contains("44"));
        assert!(result.to_string().contains(match kind { "personal" => "alice", "company" => "公司帳號", _ => "AI 員工帳號" }));
        let writes = attachment_requests(&stub);
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0][1], uid);
        assert_eq!(writes[0][6], json!({}));
        let values = &writes[0][5][0];
        assert_eq!(values.as_object().unwrap().len(), 6);
        assert_eq!(values["public"], false);
        assert!(values.get("access_token").is_none());
        assert_eq!(values["name"], "客戶文件.pdf");
        assert_eq!(values["res_model"], "crm.lead");
        assert_eq!(values["res_id"], 9);
        assert_eq!(values["mimetype"], "application/pdf");
        let bytes = base64::engine::general_purpose::STANDARD.decode(values["datas"].as_str().unwrap()).unwrap();
        assert_eq!(bytes, std::fs::read(home.path().join("agents/bot/attachments/x.pdf")).unwrap());
        assert!(!result.to_string().contains("private attachment"));
        assert!(!result.to_string().contains(values["datas"].as_str().unwrap()));
    }
}

#[tokio::test]
async fn attachment_missing_target_and_unknown_content_never_create() {
    for found in [false, true] {
        let stub = Stub::start(found).await;
        let home = home(&stub, "personal", true, &[]);
        attachment_file(home.path());
        let mut args = json!({"model":"crm.lead","res_id":9,"file":"x.pdf"});
        if found { args["content"] = json!("JVBERi0="); }
        let result = attachment_call(home.path(), "personal", true, &args).await;
        assert_eq!(result["isError"], true, "{result}");
        assert!(attachment_requests(&stub).is_empty());
    }
}

#[tokio::test]
async fn attachment_dispatch_refuses_bad_paths_types_symlinks_and_configured_size() {
    let stub = Stub::start(true).await;
    let home = home(&stub, "personal", true, &[]);
    attachment_file(home.path());
    let root = home.path().join("agents/bot/attachments");
    for (file, bytes) in [("x.svg", b"<svg/>".as_slice()), ("x.html", b"<html/>".as_slice()),
        ("fake.pdf", b"\x89PNG\r\n\x1a\n".as_slice())] {
        std::fs::write(root.join(file), bytes).unwrap();
    }
    std::fs::create_dir_all(home.path().join("attachments")).unwrap();
    std::fs::create_dir_all(home.path().join("agents/other/attachments")).unwrap();
    std::fs::copy(root.join("x.pdf"), home.path().join("attachments/x.pdf")).unwrap();
    std::fs::copy(root.join("x.pdf"), home.path().join("agents/other/attachments/x.pdf")).unwrap();
    let config = home.path().join("config.toml");
    let raw = std::fs::read_to_string(&config).unwrap();
    std::fs::write(&config, format!("{raw}\nattachment_max_mb=1\n")).unwrap();
    std::fs::write(root.join("large.pdf"), vec![b'x'; 1024 * 1024 + 1]).unwrap();
    let mut paths = vec!["x.svg".to_owned(), "x.html".into(), "fake.pdf".into(), "large.pdf".into(),
        "../../other/attachments/x.pdf".into(), "../../../attachments/x.pdf".into(),
        home.path().join("attachments/x.pdf").to_string_lossy().into_owned(),
        root.join("x.pdf").to_string_lossy().into_owned()];
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(home.path().join("attachments/x.pdf"), root.join("link.pdf")).unwrap();
        std::os::unix::fs::symlink(home.path().join("agents/other/attachments"), root.join("link-dir")).unwrap();
        paths.extend(["link.pdf".into(), "link-dir/x.pdf".into()]);
    }
    for file in paths {
        let result = attachment_call(home.path(), "personal", true,
            &json!({"model":"crm.lead","res_id":9,"file":file})).await;
        assert_eq!(result["isError"], true, "{file}: {result}");
    }
    for value in ["0", "26", "'broken'"] {
        std::fs::write(&config, format!("{raw}\nattachment_max_mb={value}\n")).unwrap();
        assert_eq!(attachment_call(home.path(), "personal", true,
            &json!({"model":"crm.lead","res_id":9,"file":"x.pdf"})).await["isError"], true);
    }
    assert!(attachment_requests(&stub).is_empty());
}

#[tokio::test]
async fn attachment_listing_and_dispatch_share_identity_and_policy_gates() {
    use super::super::tools_list::{visible_tools_in_lane, test_principal};
    for (follow, token, refuse, readonly, verified, allowed) in [
        (false, false, false, false, true, false),
        (false, true, false, false, true, true),
        (true, false, false, false, true, true),
        (true, false, true, false, true, false),
        (true, false, false, true, true, false),
        (true, false, false, false, false, false),
    ] {
        let stub = Stub::start(true).await;
        let home = home(&stub, "personal", follow, if token { &["attach:crm.lead"] } else { &[] });
        attachment_file(home.path());
        if refuse {
            let config = home.path().join("config.toml");
            let raw = std::fs::read_to_string(&config).unwrap().replace("always_refuse=[]", "always_refuse=['attach:*']");
            std::fs::write(config, raw).unwrap();
        }
        if readonly {
            let config = home.path().join("agents/bot/agent.toml");
            let raw = std::fs::read_to_string(&config).unwrap();
            std::fs::write(config, format!("{raw}\npermission_override='read_only'\n")).unwrap();
        }
        let args = json!({"model":"crm.lead","res_id":9,"file":"x.pdf"});
        let mut principal = test_principal(false);
        // Listing resolves the principal's client_id, not the fallback agent.
        principal.client_id = "bot".into();
        let outcome = super::super::with_token_verified_caller(if verified { "bot" } else { "other" }, async {
            let tools = visible_tools_in_lane(&principal, home.path(), "bot", true, &duduclaw_core::ProcessLane::Normal).await;
            let listed = tools.iter().any(|tool| tool.name == "odoo_attachment_add");
            let result = call(home.path(), "personal", follow, "odoo_attachment_add", &args).await;
            (listed, result)
        }).await;
        assert_eq!(outcome.0, allowed, "follow={follow}, token={token}, refuse={refuse}, readonly={readonly}, verified={verified}");
        assert_eq!(outcome.1["isError"] != true, allowed, "{}", outcome.1);
        assert_eq!(attachment_requests(&stub).len(), usize::from(allowed));
        if !allowed {
            assert!(stub.requests.lock().unwrap().is_empty(), "refused attachment contacted Odoo");
        }
    }
}

#[tokio::test]
async fn follow_execute_approval_binds_model_method_args_and_is_single_use() {
    use duduclaw_gateway::approval::{ApprovalBroker, ApprovalStore};
    use duduclaw_gateway::approval::action_grant::{ActionGrant, ACTION_GRANT_KEY, verify_and_consume};
    let stub = Stub::start(true).await;
    let home = home(&stub, "personal", true, &[]);
    let args = json!({"model":"crm.lead","method":"action_send_customer_email","ids":"[9]"});
    let payload = json!({"name":"odoo_execute","arguments":args});
    assert!(super::super::approval::static_gate_flags(&home.path().join("agents/bot"), "odoo_execute", &payload, super::super::approval::OdooApprovalMode::load(home.path(), "bot")).0);
    let broker = ApprovalBroker::new(Arc::new(ApprovalStore::open_in_memory().unwrap()));
    let grant = ActionGrant::build("bot", "odoo_execute", &payload);
    let mut bound = payload.clone(); bound[ACTION_GRANT_KEY] = grant.to_json();
    let id = broker.request("bot", "mcp_call", "Odoo customer email", bound, 60).await.unwrap();
    assert!(verify_and_consume(&broker, &id, &grant).await.is_err());
    assert!(stub.writes().is_empty());
    broker.decide(&id, true, "dashboard:admin").await.unwrap();
    for (key, value) in [("model", "sale.order"), ("method", "other_method"), ("ids", "[10]"), ("args", "[[10]]"), ("kwargs", "{\"context\":{\"lang\":\"en_US\"}}")] {
        let mut changed = payload.clone(); changed["arguments"][key] = json!(value);
        let different = ActionGrant::build("bot", "odoo_execute", &changed);
        assert!(verify_and_consume(&broker, &id, &different).await.is_err());
    }
    assert!(stub.writes().is_empty());
    verify_and_consume(&broker, &id, &grant).await.unwrap();
    let result = call(home.path(), "personal", true, "odoo_execute", &args).await;
    assert_eq!(result["isError"], true, "{result}");
    assert!(verify_and_consume(&broker, &id, &grant).await.is_err());
    assert!(stub.writes().is_empty());
}

#[tokio::test]
async fn execute_and_buttons_force_approval_only_when_follow_is_possible() {
    use super::super::approval::{odoo_requires_approval, static_gate_flags, OdooApprovalMode};
    let stub = Stub::start(true).await;
    for (follow, override_, expected) in [
        (false, "inherit", false), (true, "inherit", true),
        (true, "engine", false), (true, "read_only", false),
    ] {
        let home = home(&stub, "personal", follow, &[]);
        let path = home.path().join("agents/bot/agent.toml");
        let raw = std::fs::read_to_string(&path).unwrap();
        std::fs::write(path, format!("{raw}permission_override='{override_}'\n[capabilities]\naction_rules=[{{tool='odoo_execute',verdict='allow'}},{{tool='odoo_sale_confirm',verdict='allow'}},{{tool='odoo_message_send',verdict='allow'}}]\n")).unwrap();
        let mode = OdooApprovalMode::load(home.path(), "bot");
        for method in ["action_send_customer_email", "button_custom", "toggle_custom", "unknown", "create", "write", "unlink"] {
            // Arguments cannot opt into or out of the trusted mode.
            let payload = json!({"arguments":{"method":method,"permission_mode":"engine","identity":"company"}});
            assert_eq!(odoo_requires_approval("odoo_execute", &payload, mode), expected);
            assert_eq!(static_gate_flags(&home.path().join("agents/bot"), "odoo_execute", &payload, mode).0, expected);
        }
        for tool in ["odoo_execute", "odoo_sale_confirm"] {
            let payload = json!({"arguments":{}});
            assert_eq!(odoo_requires_approval(tool, &payload, mode), expected);
            assert_eq!(static_gate_flags(&home.path().join("agents/bot"), tool, &payload, mode).0, expected);
        }
        for method in ["read", "search_read", "fields_get", "read_group"] {
            assert!(!odoo_requires_approval("odoo_execute", &json!({"arguments":{"method":method}}), mode));
        }
        assert!(odoo_requires_approval("odoo_message_send", &json!({"arguments":{}}), mode));
    }
}

#[tokio::test]
async fn engine_execute_and_sale_confirm_run_without_forced_approval() {
    use super::super::approval::{gate_tool_approval_dispatch, with_odoo_approval_call};
    let stub = Stub::start(true).await;
    let home = home(&stub, "personal", false, &[]);
    let path = home.path().join("agents/bot/agent.toml");
    let raw = std::fs::read_to_string(&path).unwrap()
        .replace("allowed_models=['crm.lead']", "allowed_models=['crm.lead','sale.order']");
    std::fs::write(path, raw).unwrap();
    for (tool, args) in [
        ("odoo_execute", json!({"model":"crm.lead","method":"write","ids":"[9]"})),
        ("odoo_execute", json!({"model":"crm.lead","method":"button_custom","ids":"[9]"})),
        ("odoo_execute", json!({"model":"crm.lead","method":"action_custom","ids":"[9]"})),
        ("odoo_sale_confirm", json!({"order_id":9})),
    ] {
        with_odoo_approval_call(async {
            let payload = json!({"name":tool,"arguments":args});
            tokio::time::timeout(std::time::Duration::from_secs(1),
                gate_tool_approval_dispatch(home.path(), "bot", tool, payload)).await.unwrap().unwrap();
            let result = call(home.path(), "personal", false, tool, &args).await;
            assert_ne!(result["isError"], true, "{tool}: {result}");
        }).await;
    }
    assert!(!home.path().join("approvals.db").exists());
}

#[tokio::test]
async fn follow_approval_uses_stored_personal_candidates_and_valid_ack() {
    use super::super::approval::{odoo_requires_approval, OdooApprovalMode};
    let stub = Stub::start(true).await;
    let payload = json!({"arguments":{"model":"crm.lead","method":"write"}});
    for kind in ["agent", "company"] {
        let home = home(&stub, kind, true, &[]);
        super::super::odoo_access_tests::personal_db(home.path(), "someone_else", true);
        // An unrelated stored credential does not make a company/agent call personal.
        assert!(!odoo_requires_approval("odoo_execute", &payload, OdooApprovalMode::load(home.path(), "bot")));
        let path = home.path().join("agents/bot/agent.toml");
        let raw = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, format!("{raw}act_as_requester=true\n")).unwrap();
        // A verified requester can select a personal credential before either fallback.
        assert!(odoo_requires_approval("odoo_execute", &payload, OdooApprovalMode::load(home.path(), "bot")));
        let db = rusqlite::Connection::open(home.path().join("users.db")).unwrap();
        db.execute("UPDATE member_odoo_credentials SET credential_epoch=0", []).unwrap();
        assert!(!odoo_requires_approval("odoo_execute", &payload, OdooApprovalMode::load(home.path(), "bot")));
    }
    for ack in ["", "permission_mode_ack={by='',at='invalid'}"] {
        let home = home(&stub, "personal", true, &[]);
        let path = home.path().join("config.toml");
        let raw = std::fs::read_to_string(&path).unwrap()
            .replace("permission_mode_ack={by='owner',at='2026-11-01T00:00:00Z'}", ack);
        std::fs::write(path, raw).unwrap();
        assert!(!odoo_requires_approval("odoo_execute", &payload, OdooApprovalMode::load(home.path(), "bot")));
    }
}

#[tokio::test]
async fn engine_retains_employee_approval_lists_and_action_rules() {
    use super::super::approval::{static_gate_flags, OdooApprovalMode};
    let stub = Stub::start(true).await;
    let home = home(&stub, "personal", false, &[]);
    let dir = home.path().join("agents/bot");
    let raw = std::fs::read_to_string(dir.join("agent.toml")).unwrap();
    for tool in ["odoo_execute", "odoo_sale_confirm"] {
        let payload = json!({"arguments":{"model":"crm.lead","method":"button_custom"}});
        for (rule, expected) in [
            (format!("action_rules=[{{tool='{tool}',verdict='allow'}}]"), false),
            (format!("approval_required_tools=['{tool}']\naction_rules=[{{tool='{tool}',verdict='allow'}}]"), true),
            (format!("action_rules=[{{tool='{tool}',verdict='ask'}}]"), true),
            (format!("irreversible_tools=['{tool}']"), true),
        ] {
            std::fs::write(dir.join("agent.toml"), format!("{raw}\n[capabilities]\n{rule}\n")).unwrap();
            let mode = OdooApprovalMode::load(home.path(), "bot");
            assert_eq!(static_gate_flags(&dir, tool, &payload, mode).0, expected);
        }
    }
}

#[tokio::test]
async fn unreadable_odoo_policy_or_identity_never_skips_forced_approval() {
    use super::super::approval::{odoo_requires_approval, OdooApprovalMode};
    let stub = Stub::start(true).await;
    let home = home(&stub, "personal", true, &[]);
    let payload = json!({"arguments":{"model":"crm.lead","method":"write"}});
    std::fs::remove_file(home.path().join("users.db")).unwrap();
    assert!(odoo_requires_approval("odoo_execute", &payload, OdooApprovalMode::load(home.path(), "bot")));
    std::fs::write(home.path().join("config.toml"), "[odoo_access]\npermission_mode=42").unwrap();
    assert!(odoo_requires_approval("odoo_execute", &payload, OdooApprovalMode::load(home.path(), "bot")));
    assert!(super::super::odoo_access::ToolPolicy::load(home.path(), "bot").is_err());
}

#[tokio::test]
async fn gateway_follow_without_forced_approval_refuses_before_odoo_io() {
    let stub = Stub::start(true).await;
    let home = home(&stub, "personal", false, &["execute", "action", "write"]);
    // Local Engine snapshot, followed by a gateway Follow decision: no proof
    // can be manufactured by arguments when settings change between gates.
    let args = json!({"model":"crm.lead","method":"action_send_customer_email","ids":"[9]",
        "approved":true,"permission_mode":"engine","action_grant":{"approved":true}});
    let result = call(home.path(), "personal", true, "odoo_execute", &args).await;
    assert_eq!(result["isError"], true, "{result}");
    assert!(result.to_string().contains("核准"), "{result}");
    assert!(stub.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn generic_writes_to_mail_and_model_families_never_reach_odoo() {
    for follow in [false, true] {
        let stub = Stub::start(true).await;
        let home = home(&stub, "personal", follow, &[]);
        for model in ["mail.template", "mail.message", "ir.model.data", "ir.model.constraint"] {
            for (tool, args) in [
                ("odoo_write", json!({"model":model,"ids":[1],"values":{"name":"x"}})),
                ("odoo_create", json!({"model":model,"values":{"name":"x"}})),
                ("odoo_execute", json!({"model":model,"method":"action_send_customer_email","ids":"[1]"})),
            ] {
                let result = call(home.path(), "personal", follow, tool, &args).await;
                assert_eq!(result["isError"], true, "{follow} {tool} {model}: {result}");
            }
        }
        assert!(stub.requests.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn attachment_images_upload_reencoded_bytes_and_webp_uses_png_name_and_mime() {
    use base64::Engine;
    use image::ImageFormat;
    use super::super::odoo_attachment::tests::image_bytes;
    let stub = Stub::start(true).await;
    let home = home(&stub, "company", false, &["attach:crm.lead"]);
    attachment_file(home.path());
    for (file, format, name, expected_name, mime) in [
        ("x.png", ImageFormat::Png, "圖片.PNG", "圖片.PNG", "image/png"),
        ("x.jpg", ImageFormat::Jpeg, "相片.jpeg", "相片.jpeg", "image/jpeg"),
        ("x.webp", ImageFormat::WebP, "圖片.WEBP", "圖片.png", "image/png"),
    ] {
        let original = image_bytes(format, 2, 2);
        let mut input = original.clone();
        if format != ImageFormat::WebP { input.extend_from_slice(b"private-trailing-payload"); }
        std::fs::write(home.path().join("agents/bot/attachments").join(file), &input).unwrap();
        let result = attachment_call(home.path(), "company", false,
            &json!({"model":"crm.lead","res_id":9,"file":file,"name":name})).await;
        assert_ne!(result["isError"], true, "{result}");
        let writes = attachment_requests(&stub);
        let values = &writes.last().unwrap()[5][0];
        assert_eq!(values["name"], expected_name);
        assert_eq!(values["mimetype"], mime);
        let uploaded = base64::engine::general_purpose::STANDARD.decode(values["datas"].as_str().unwrap()).unwrap();
        let decoded = image::load_from_memory(&original).unwrap();
        let mut expected = Vec::new();
        if format == ImageFormat::Jpeg {
            decoded.write_with_encoder(image::codecs::jpeg::JpegEncoder::new_with_quality(&mut expected, 90)).unwrap();
        } else {
            decoded.write_with_encoder(image::codecs::png::PngEncoder::new(&mut expected)).unwrap();
        }
        assert_eq!(uploaded, expected);
        assert_ne!(uploaded, input);
    }
    assert_eq!(attachment_requests(&stub).len(), 3);
    let before = attachment_requests(&stub).len();
    for (file, bytes) in [
        ("bad.jpg", b"\xff\xd8\xff\xda\x00\x02data\xff\xd9".to_vec()),
        ("bad.pdf", b"%PDF-1.7\n<< /Open#41ction null >>\n%%EOF".to_vec()),
        ("wide.png", image_bytes(ImageFormat::Png, 12_001, 1)),
    ] {
        std::fs::write(home.path().join("agents/bot/attachments").join(file), bytes).unwrap();
        let result = attachment_call(home.path(), "company", false,
            &json!({"model":"crm.lead","res_id":9,"file":file})).await;
        assert_eq!(result["isError"], true, "{result}");
    }
    assert_eq!(attachment_requests(&stub).len(), before);
}

#[tokio::test]
async fn attachment_pdf_scan_refuses_active_incremental_updates_before_upload() {
    use super::super::odoo_attachment::tests::{clean_pdf, raw_object_pdf};
    let stub = Stub::start(true).await;
    let home = home(&stub, "company", false, &["attach:crm.lead"]);
    attachment_file(home.path());
    let clean = clean_pdf();
    for bytes in [
        raw_object_pdf(b"<< /Java#53cript null >>"),
        raw_object_pdf(b"<< /Encrypt 8 0 R >>"),
        raw_object_pdf(b"<< /Bad#zz null >>"),
        [clean.as_slice(), b"\n99 0 obj\n<< /OpenAction null >>\nendobj\n%%EOF"].concat(),
        b"%PDF-1.7\n<<>>".to_vec(),
    ] {
        std::fs::write(home.path().join("agents/bot/attachments/x.pdf"), bytes).unwrap();
        let result = attachment_call(home.path(), "company", false,
            &json!({"model":"crm.lead","res_id":9,"file":"x.pdf"})).await;
        assert_eq!(result["isError"], true, "{result}");
    }
    assert!(attachment_requests(&stub).is_empty());
}

#[tokio::test]
async fn attachment_clean_pdf_uploads_original_bytes_to_sales_order() {
    use base64::Engine;
    let stub = Stub::start(true).await;
    let home = home(&stub, "company", false, &["attach:sale.order"]);
    let agent = home.path().join("agents/bot/agent.toml");
    let config = std::fs::read_to_string(&agent).unwrap();
    std::fs::write(&agent, config.replace("allowed_models=['crm.lead']", "allowed_models=['sale.order']")).unwrap();
    attachment_file(home.path());
    let original = std::fs::read(home.path().join("agents/bot/attachments/x.pdf")).unwrap();
    let result = attachment_call(home.path(), "company", false,
        &json!({"model":"sale.order","res_id":9,"file":"x.pdf","name":"訂單.pdf"})).await;
    assert_ne!(result["isError"], true, "{result}");
    let writes = attachment_requests(&stub);
    assert_eq!(writes.len(), 1);
    let values = &writes[0][5][0];
    assert_eq!(values["res_model"], "sale.order");
    assert_eq!(values["mimetype"], "application/pdf");
    let uploaded = base64::engine::general_purpose::STANDARD.decode(values["datas"].as_str().unwrap()).unwrap();
    assert_eq!(uploaded, original);
}
