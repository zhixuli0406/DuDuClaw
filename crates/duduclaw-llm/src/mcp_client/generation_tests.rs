use super::*;
use std::sync::Mutex as StdMutex;
use tokio::io::AsyncReadExt;

type Seen = Arc<StdMutex<Vec<(Value, HashMap<String, String>)>>>;

struct Mock {
    url: String,
    seen: Seen,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Mock { fn drop(&mut self) { self.task.abort(); } }

async fn mock(version: &'static str, discover_error: Option<i64>) -> Mock {
    mock_with_principals(version, discover_error, false).await
}

async fn mock_with_principals(version: &'static str, discover_error: Option<i64>, principals: bool) -> Mock {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let seen: Seen = Arc::new(StdMutex::new(Vec::new()));
    let recorded = seen.clone();
    let task = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let recorded = recorded.clone();
            tokio::spawn(async move {
                let mut bytes = Vec::new();
                let mut buf = [0; 4096];
                let end = loop {
                    let n = socket.read(&mut buf).await.unwrap();
                    if n == 0 { return; }
                    bytes.extend_from_slice(&buf[..n]);
                    if let Some(i) = bytes.windows(4).position(|w| w == b"\r\n\r\n") { break i + 4; }
                };
                let headers: HashMap<String, String> = String::from_utf8_lossy(&bytes[..end]).lines()
                    .filter_map(|line| line.split_once(':').map(|(k,v)| (k.to_ascii_lowercase(), v.trim().into())))
                    .collect();
                let length = headers["content-length"].parse::<usize>().unwrap();
                while bytes.len() < end + length {
                    let n = socket.read(&mut buf).await.unwrap();
                    if n == 0 { return; }
                    bytes.extend_from_slice(&buf[..n]);
                }
                let frame: Value = serde_json::from_slice(&bytes[end..end+length]).unwrap();
                let version = if principals {
                    if headers.get("authorization").map(String::as_str) == Some("Bearer legacy") { "2025-06-18" }
                    else { protocol::MODERN }
                } else { version };
                recorded.lock().unwrap().push((frame.clone(), headers));
                let method = frame["method"].as_str().unwrap();
                let mut status = "200 OK";
                let mut extra = "";
                let mut sse = false;
                let mut response = json!({"jsonrpc":"2.0","id":frame["id"]});
                if method == "server/discover" {
                    if let Some(code) = discover_error {
                        status = if code == -32601 { "404 Not Found" } else { "400 Bad Request" };
                        response["error"] = json!({"code":code,"message":"probe refused","data":{"supported":[version]}});
                    } else if version == protocol::MODERN {
                        // A modern peer's session header must be ignored.
                        extra = "mcp-session-id: must-not-echo\r\n";
                        response["result"] = json!({"resultType":"complete","supportedVersions":[version],"capabilities":{},"ttlMs":0,"cacheScope":"private"});
                    } else {
                        status = "405 Method Not Allowed";
                        response = Value::Null;
                    }
                } else if method == "initialize" {
                    extra = "mcp-session-id: legacy-session\r\n";
                    response["result"] = json!({"protocolVersion":version,"capabilities":{"tools":{}}});
                } else if method == "notifications/initialized" {
                    status = "202 Accepted";
                    response = Value::Null;
                } else if method == "tools/list" && recorded.lock().unwrap().iter().any(|(f, _)| f["params"]["arguments"]["refresh_downgrade"] == true) {
                    response["error"] = json!({"code":-32022,"message":"list downgrade","data":{"supported":["2025-06-18"]}});
                } else if method == "tools/list" {
                    response["result"] = json!({"tools":[
                        {"name":"echo","inputSchema":{"type":"object","properties":{"tenant":{"type":"string","x-mcp-header":"Tenant"}}}},
                        {"name":"bad","inputSchema":{"properties":{"a":{"type":"number","x-mcp-header":"Nope"}}}}
                    ],"ttlMs":0,"cacheScope":"private"});
                } else if method == "tools/call" {
                    let args = &frame["params"]["arguments"];
                    if args["refresh_downgrade"] == true {
                        response["error"] = json!({"code":-32020,"message":"refresh headers"});
                    } else if args["downgrade"] == true {
                        response["error"] = json!({"code":-32022,"message":"downgrade","data":{"supported":["2025-06-18"]}});
                    } else if args["unknown_type"] == true {
                        response["result"] = json!({"resultType":"future"});
                    } else if args["input"] == true && frame["params"].get("inputResponses").is_none() {
                        response["result"] = json!({"resultType":"input_required","requestState":"opaque\u{0000}狀態",
                            "inputRequests":{"login":{"method":"elicitation/create","params":{"mode":"form","message":"login"}}}});
                    } else if args["round"] == true && frame["params"].get("requestState").is_none() || args["loop"] == true {
                        response["result"] = json!({"resultType":"input_required","requestState":"state"});
                    } else {
                        sse = args["sse"] == true;
                        response["result"] = json!({"content":[{"type":"text","text":"ok"}]});
                    }
                }
                let body = if response.is_null() { String::new() }
                    else if sse { format!(": keepalive\r\n\r\ndata: {response}\r\n\r\n") }
                    else { response.to_string() };
                let content_type = if sse { "text/event-stream" } else { "application/json" };
                let response = format!("HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\n{extra}connection: close\r\n\r\n{body}", body.len());
                socket.write_all(response.as_bytes()).await.unwrap();
            });
        }
    });
    Mock { url, seen, task }
}

#[tokio::test]
async fn three_generations_headers_sessions_and_independent_discovery() {
    for version in protocol::SUPPORTED {
        let server = mock(version, None).await;
        let mut client = McpClient::connect_http(&server.url, &[], Duration::from_secs(5)).await.unwrap();
        assert_eq!(client.protocol_version(), *version);
        let listed = client.list_tools().await.unwrap();
        assert_eq!(listed.len(), if *version == protocol::MODERN { 1 } else { 2 });
        assert_eq!(client.call_tool("echo", json!({"tenant":"台灣", "sse":true})).await.unwrap().content, "ok");
        let mut second = McpClient::connect_http(&server.url, &[], Duration::from_secs(5)).await.unwrap();
        second.list_tools().await.unwrap();
        let seen = server.seen.lock().unwrap();
        assert_eq!(seen.iter().filter(|(f,_)| f["method"] == "server/discover").count(), 2);
        for (frame, headers) in seen.iter() {
            let method = frame["method"].as_str().unwrap();
            if method == "server/discover" { continue; }
            assert_eq!(headers["mcp-protocol-version"], if method == "initialize" {
                frame["params"]["protocolVersion"].as_str().unwrap()
            } else { version });
            if *version == protocol::MODERN {
                assert!(!headers.contains_key("mcp-session-id"));
                assert_eq!(headers["mcp-method"], method);
                assert_eq!(frame["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"], *version);
                if method == "tools/call" {
                    assert_eq!(headers["mcp-name"], "echo");
                    assert_eq!(headers["mcp-param-tenant"], protocol::encode_header_value("台灣"));
                }
            } else if method != "initialize" { assert_eq!(headers["mcp-session-id"], "legacy-session"); }
        }
    }
}

#[tokio::test]
async fn version_mismatch_uses_best_supported_and_modern_errors_do_not_downgrade() {
    let server = mock("2025-06-18", Some(-32022)).await;
    let client = McpClient::connect_http(&server.url, &[], Duration::from_secs(5)).await.unwrap();
    assert_eq!(client.protocol_version(), "2025-06-18");
    for code in [-32020, -32021] {
        let server = mock(protocol::MODERN, Some(code)).await;
        assert!(matches!(McpClient::connect_http(&server.url, &[], Duration::from_secs(5)).await,
            Err(McpError::Rpc { code: c, .. }) if c == code));
        assert_eq!(server.seen.lock().unwrap().len(), 1);
    }
    let optional_discover = mock(protocol::MODERN, Some(-32601)).await;
    let mut client = McpClient::connect_http(&optional_discover.url, &[], Duration::from_secs(5)).await.unwrap();
    client.list_tools().await.unwrap();
    assert!(!optional_discover.seen.lock().unwrap().iter().any(|(f,_)| f["method"] == "initialize"));
    let server = mock("2099-01-01", Some(-32022)).await;
    assert!(McpClient::connect_http(&server.url, &[], Duration::from_secs(5)).await.is_err());
    assert_eq!(server.seen.lock().unwrap().len(), 1);
}

struct Decline;
#[async_trait]
impl McpInputHandler for Decline {
    fn capabilities(&self) -> Value { json!({"elicitation":{"form":{}}}) }
    async fn respond(&self, _: &Value) -> Result<Value, McpError> { Ok(json!({"action":"decline"})) }
}

#[tokio::test]
async fn mrtr_round_trip_new_ids_exact_state_and_bounded_retries() {
    let server = mock(protocol::MODERN, None).await;
    let mut client = McpClient::connect_http(&server.url, &[], Duration::from_secs(5)).await.unwrap();
    assert!(client.call_tool("echo", json!({"input":true})).await.is_err());
    client.set_input_handler(Arc::new(Decline));
    assert_eq!(client.call_tool("echo", json!({"input":true})).await.unwrap().content, "ok");
    assert_eq!(client.call_tool("echo", json!({"round":true})).await.unwrap().content, "ok");
    assert!(client.call_tool("echo", json!({"loop":true})).await.is_err());
    assert!(client.call_tool("echo", json!({"unknown_type":true})).await.is_err());
    let seen = server.seen.lock().unwrap();
    let calls: Vec<_> = seen.iter().map(|(f,_)| f).filter(|f| f["method"] == "tools/call").collect();
    let ids: HashSet<_> = calls.iter().map(|f| f["id"].as_i64().unwrap()).collect();
    assert_eq!(ids.len(), calls.len());
    let retry = calls.iter().find(|f| f["params"].get("inputResponses").is_some()).unwrap();
    assert_eq!(retry["params"]["requestState"], "opaque\u{0000}狀態");
    assert_eq!(retry["params"]["inputResponses"]["login"]["action"], "decline");
    assert_eq!(retry["params"]["arguments"], json!({"input":true}));
    assert_eq!(calls.iter().filter(|f| f["params"]["arguments"]["loop"] == true).count(), 8);
}

#[cfg(unix)]
#[tokio::test]
async fn stdio_generations_fallback_errors_timeout_and_mrtr() {
    let script = r#"
import json, sys
version, mode = sys.argv[1:3]
seen_ids = set()
for line in sys.stdin:
    req = json.loads(line)
    if 'id' not in req: continue
    assert req['id'] not in seen_ids
    seen_ids.add(req['id'])
    method = req['method']
    if method == 'server/discover':
        if version == '2026-07-28':
            result = {'resultType':'complete','supportedVersions':[version],'capabilities':{}}
        elif mode == 'timeout': continue
        else:
            code = -32022 if mode == 'mismatch' else (-32602 if mode == 'params' else -32601)
            print(json.dumps({'jsonrpc':'2.0','id':req['id'],'error':{'code':code,'message':'legacy','data':{'supported':[version]}}}), flush=True)
            continue
    elif method == 'initialize':
        assert version != '2026-07-28'
        result = {'protocolVersion':version,'capabilities':{}}
    elif method == 'tools/list':
        result = {'tools':[{'name':'echo','inputSchema':{'type':'object'}}]}
    elif method == 'tools/call':
        params = req['params']
        if version == '2026-07-28':
            meta = params['_meta']
            assert meta['io.modelcontextprotocol/protocolVersion'] == version
            assert meta['io.modelcontextprotocol/clientCapabilities'] == {'elicitation':{'form':{}}}
            if params.get('arguments',{}).get('empty'):
                result = {'resultType':'input_required','inputRequests':{}}
            elif params.get('arguments',{}).get('round') and 'inputResponses' not in params:
                result = {'resultType':'input_required','requestState':'opaque state', 'inputRequests':{'form':{'method':'elicitation/create','params':{'mode':'form'}}}}
            else:
                if params.get('arguments',{}).get('round'):
                    assert params['requestState'] == 'opaque state'
                    assert params['inputResponses'] == {'form':{'action':'decline'}}
                result = {'resultType':'complete','content':[{'type':'text','text':'ok'}]}
        else: result = {'content':[{'type':'text','text':'ok'}]}
    print(json.dumps({'jsonrpc':'2.0','id':req['id'],'result':result}), flush=True)
"#;
    for (version, mode) in [
        (protocol::MODERN, "modern"), ("2025-11-25", "method"),
        ("2025-06-18", "params"), ("2025-06-18", "mismatch"),
        ("2025-06-18", "timeout"),
    ] {
        let mut client = McpClient::connect("/usr/bin/python3", &[
            "-u".into(), "-c".into(), script.into(), version.into(), mode.into(),
        ], &[], Duration::from_secs(5)).await.unwrap();
        assert_eq!(client.protocol_version(), version);
        client.set_input_handler(Arc::new(Decline));
        assert_eq!(client.list_tools().await.unwrap().len(), 1);
        assert_eq!(client.call_tool("echo", json!({"round":true})).await.unwrap().content, "ok");
        if version == protocol::MODERN {
            assert!(client.call_tool("echo", json!({"empty":true})).await.is_err());
        }
    }
}

#[tokio::test]
async fn changed_connection_reprobes_legacy_and_invalid_header_tools_cannot_be_called() {
    let server = mock("2025-11-25", None).await;
    let mut client = McpClient::connect_http(&server.url, &[], Duration::from_secs(5)).await.unwrap();
    // Simulate the endpoint changing era after a connection was established.
    client.protocol_version = protocol::MODERN;
    // An explicit -32022 is exercised independently; unknown modern HTTP
    // responses are re-probed by recover_version without retaining a session.
    assert!(client.recover_version(&Err(McpError::HttpStatus(405))).await.unwrap());
    assert_eq!(client.protocol_version(), "2025-11-25");
    let modern = mock(protocol::MODERN, None).await;
    let mut client = McpClient::connect_http(&modern.url, &[], Duration::from_secs(5)).await.unwrap();
    assert!(client.call_tool("bad", json!({})).await.is_err());
    assert!(!modern.seen.lock().unwrap().iter().any(|(f,_)| f["method"] == "tools/call"));
}

#[tokio::test]
async fn http_protocol_negotiation_is_independent_across_principals() {
    for principals in [["legacy", "modern"], ["modern", "legacy"]] {
        let server = mock_with_principals(protocol::MODERN, None, true).await;
        for principal in principals {
            let headers = vec![("Authorization".into(), format!("Bearer {principal}"))];
            let client = McpClient::connect_http(&server.url, &headers, Duration::from_secs(5)).await.unwrap();
            assert_eq!(client.protocol_version(), if principal == "legacy" { "2025-06-18" } else { protocol::MODERN });
        }
        let seen = server.seen.lock().unwrap();
        for principal in principals {
            let requests: Vec<_> = seen.iter().filter(|(_, headers)| headers.get("authorization") == Some(&format!("Bearer {principal}"))).collect();
            assert_eq!(requests[0].0["method"], "server/discover");
            assert_eq!(requests.iter().filter(|(f, _)| f["method"] == "server/discover").count(), 1);
            assert_eq!(requests.iter().filter(|(f, _)| f["method"] == "initialize").count(), usize::from(principal == "legacy"));
        }
    }
}

#[tokio::test]
async fn tool_version_mismatch_never_replays_and_requires_new_connection() {
    let server = mock(protocol::MODERN, None).await;
    let mut client = McpClient::connect_http(&server.url, &[], Duration::from_secs(5)).await.unwrap();
    client.list_tools().await.unwrap();
    assert!(client.call_tool("echo", json!({"downgrade":true})).await.is_err());
    assert!(client.requires_reconnect);
    let before = server.seen.lock().unwrap().len();
    assert!(client.call_tool("echo", json!({})).await.is_err());
    assert!(client.list_tools().await.is_err());
    let seen = server.seen.lock().unwrap();
    assert_eq!(seen.len(), before, "invalidated client must not send more requests");
    assert_eq!(seen.iter().filter(|(f, _)| f["method"] == "tools/call").count(), 1);
    assert!(!seen.iter().any(|(f, _)| f["method"] == "initialize"));
}

#[tokio::test]
async fn tool_header_refresh_cannot_downgrade_or_replay() {
    let server = mock(protocol::MODERN, None).await;
    let mut client = McpClient::connect_http(&server.url, &[], Duration::from_secs(5)).await.unwrap();
    client.list_tools().await.unwrap();
    assert!(client.call_tool("echo", json!({"refresh_downgrade":true})).await.is_err());
    assert!(client.requires_reconnect);
    let before = server.seen.lock().unwrap().len();
    assert!(client.list_tools().await.is_err());
    assert!(client.call_tool("echo", json!({})).await.is_err());
    let seen = server.seen.lock().unwrap();
    assert_eq!(seen.len(), before);
    assert_eq!(seen.iter().filter(|(f, _)| f["method"] == "tools/call").count(), 1);
    assert_eq!(seen.iter().filter(|(f, _)| f["method"] == "tools/list").count(), 2);
    assert!(!seen.iter().any(|(f, _)| f["method"] == "initialize"));
}

#[cfg(unix)]
#[tokio::test]
async fn oversized_stdio_without_newline_closes_client() {
    let mut command = Command::new("sh");
    command.args(["-c", "exec cat /dev/zero"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let mut child = ManagedChild::spawn(&mut command).unwrap();
    let stdin = child.child().stdin.take().unwrap();
    let stdout = BufReader::new(child.child().stdout.take().unwrap());
    let mut client = McpClient {
        transport: McpTransport::Stdio { child, stdin, stdout },
        next_id: AtomicI64::new(1), timeout: Duration::from_secs(10),
        label: "oversized-stdio-fixture".into(), workflow_version: None,
        builtin_tool_timeouts: false, protocol_version: protocol::MODERN,
        requires_reconnect: false, last_http_status: None,
        tool_schemas: HashMap::new(), input_handler: None,
    };
    // The producer never sends LF or EOF. Refuse before the request deadline.
    let error = tokio::time::timeout(Duration::from_secs(5), client.request(
        json!({"jsonrpc":"2.0","id":1,"method":"ping"}), 1,
    )).await.expect("oversized frame must not wait for LF or EOF").unwrap_err();
    assert!(matches!(error, McpError::Io(ref message) if message == "invalid or oversized MCP stdio frame"));
    assert!(client.requires_reconnect);
    let error = client.request(json!({"jsonrpc":"2.0","id":2,"method":"ping"}), 2).await.unwrap_err();
    assert!(matches!(error, McpError::Parse(ref message) if message.contains("reconnect")));
}

#[tokio::test]
async fn stdio_frames_accept_boundary_crlf_and_final_eof() {
    let mut bytes = vec![b' '; protocol::MAX_STDIO_FRAME_BYTES];
    bytes.extend_from_slice(b"\n{}\r\n{}");
    let mut reader = BufReader::with_capacity(1009, bytes.as_slice());
    assert_eq!(protocol::read_stdio_frame(&mut reader).await.unwrap().unwrap().len(), protocol::MAX_STDIO_FRAME_BYTES);
    assert_eq!(protocol::read_stdio_frame(&mut reader).await.unwrap().as_deref(), Some("{}\r"));
    assert_eq!(protocol::read_stdio_frame(&mut reader).await.unwrap().as_deref(), Some("{}"));
    assert!(protocol::read_stdio_frame(&mut reader).await.unwrap().is_none());
    let invalid = [0xff, b'\n'];
    assert!(protocol::read_stdio_frame(&mut invalid.as_slice()).await.is_err());
}

#[tokio::test]
async fn failed_http_connection_never_exposes_secret_url() {
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let url = format!("http://{}/api/SECRET_PATH?key=SECRET_QUERY", socket.local_addr().unwrap());
    let error = McpClient::connect_http(&url, &[], Duration::from_secs(2)).await.err().unwrap();
    assert_eq!(error.to_string(), McpError::Io("MCP HTTP request failed".into()).to_string());
    let error = McpClient::connect_http("http://remote.example/SECRET_PATH", &[], Duration::from_secs(2)).await.err().unwrap();
    assert!(!error.to_string().contains("SECRET_PATH"));
}
