//! Upstream era negotiation and modern request exchanges. The egress gate owns
//! the caller generation across internal retries; only a terminal answer is
//! remapped and delivered to it.

use super::*;

const MAX_ROUNDS: usize = 8;

fn discover_frame() -> Value {
    let mut frame = json!({"jsonrpc":"2.0","id":format!("bridge-discover:{}", uuid::Uuid::new_v4()),
        "method":"server/discover","params":{}});
    protocol::inject_meta(&mut frame, protocol::MODERN, &json!({}));
    frame
}

fn facade_discovery(capabilities: Value) -> Value {
    json!({"resultType":"complete","supportedVersions":[protocol::MODERN,"2025-11-25","2025-06-18"],
        "capabilities":capabilities,"ttlMs":0,"cacheScope":"private",
        "_meta":{"io.modelcontextprotocol/serverInfo":{"name":"DuDuClaw remote bridge","version":env!("CARGO_PKG_VERSION")}}})
}

fn recognized_modern_error(status: u16, frame: &Value) -> bool {
    matches!(frame["error"]["code"].as_i64(), Some(-32022 | -32021 | -32020))
        || (status == 404 && frame["error"]["code"] == -32601)
}

fn legacy_fallback(status: u16, frame: &Value) -> bool {
    matches!(status, 400 | 404 | 405) && !recognized_modern_error(status, frame)
}

impl Bridge {
    pub(super) async fn negotiate(&self) -> Result<Value, String> {
        let _lock = self.negotiation.lock().await;
        if self.session.lock().await.requires_restart {
            return Err("MCP protocol changed; restart the conversation to reconnect".into());
        }
        if let Some(discovery) = &self.session.lock().await.discovery {
            return Ok(discovery.clone());
        }
        let result = self.negotiate_inner().await;
        if result.is_err() {
            let mut session = self.session.lock().await;
            session.protocol_version = None;
            session.session_id = None;
            session.legacy_endpoint = None;
            session.legacy_source = None;
            drop(session);
            *self.legacy_stream.lock().await = None;
        }
        result
    }

    async fn negotiate_inner(&self) -> Result<Value, String> {
        self.session.lock().await.protocol_version = Some(protocol::MODERN.into());
        let (status, reply, _) = self.round_trip(&discover_frame(), None).await?;
        let code = reply["error"]["code"].as_i64();
        let legacy = if code == Some(-32022) {
            let version = protocol::best_supported(&reply["error"]["data"]["supported"])
                .ok_or("remote MCP server has no mutually supported protocol version")?;
            if version == protocol::MODERN {
                return Err("remote MCP server refused its advertised protocol version".into());
            }
            Some(version)
        } else if legacy_fallback(status, &reply) {
            Some("2025-11-25")
        } else {
            if code.is_some() && !(status == 404 && code == Some(-32601)) {
                return Err(format!("remote MCP discovery failed (HTTP {status}, RPC {code:?})"));
            }
            if !(200..300).contains(&status) && !(status == 404 && code == Some(-32601)) {
                return Err(format!("remote MCP discovery failed (HTTP {status})"));
            }
            if code.is_none() {
                protocol::validate_result(&reply)?;
                if reply["result"]["resultType"] == "input_required" {
                    return Err("server/discover cannot require input".into());
                }
                let supported = &reply["result"]["supportedVersions"];
                if !supported.as_array().is_some_and(|v| v.iter().any(|v| v == protocol::MODERN)) {
                    return Err("remote MCP discovery omitted the requested version".into());
                }
            }
            None
        };
        let capabilities = if let Some(version) = legacy {
            self.initialize_legacy(version).await?
        } else {
            reply["result"]["capabilities"].clone()
        };
        let discovery = facade_discovery(if capabilities.is_object() { capabilities } else { json!({}) });
        self.session.lock().await.discovery = Some(discovery.clone());
        Ok(discovery)
    }

    async fn initialize_legacy(&self, version: &str) -> Result<Value, String> {
        self.session.lock().await.protocol_version = Some(version.into());
        let init = json!({"jsonrpc":"2.0","id":format!("bridge-initialize:{}",uuid::Uuid::new_v4()),
            "method":"initialize","params":{"protocolVersion":version,"capabilities":self.session.lock().await.downstream_capabilities.clone(),
            "clientInfo":{"name":"DuDuClaw remote bridge","version":env!("CARGO_PKG_VERSION")}}});
        let (mut status, mut reply, mut session_id) = self.round_trip(&init, None).await?;
        if matches!(status, 400 | 404 | 405) && !recognized_modern_error(status, &reply) {
            self.open_legacy_sse().await?;
            (status, reply, session_id) = self.round_trip(&init, None).await?;
        }
        if !(200..300).contains(&status) || reply.get("error").is_some() {
            return Err(format!("legacy MCP initialize failed (HTTP {status})"));
        }
        let selected = reply["result"]["protocolVersion"].as_str().ok_or("initialize omitted protocolVersion")?;
        if !["2025-11-25", "2025-06-18"].contains(&selected) {
            return Err("legacy MCP server selected an unsupported protocol version".into());
        }
        {
            let mut session = self.session.lock().await;
            session.protocol_version = Some(selected.to_string());
            session.session_id = session_id;
        }
        let auth = self.auth(false).await.map_err(|e| e.to_string())?;
        let notification = json!({"jsonrpc":"2.0","method":"notifications/initialized","params":{}});
        let resp = self.post(&auth, &notification.to_string(), None).await?;
        if !resp.status().is_success() { return Err("legacy MCP initialized notification failed".into()); }
        Ok(reply["result"]["capabilities"].clone())
    }


    /// Read exactly the correlated response, without modern GET resumption.
    /// Non-success JSON-RPC bodies remain available for era/version decisions.
    async fn round_trip(
        &self, frame: &Value, permit: Option<&crate::third_party_tools::DispatchPermit>,
    ) -> Result<(u16, Value, Option<String>), String> {
        if self.session.lock().await.legacy_endpoint.is_some() {
            return self.legacy_sse_round_trip(frame, permit).await;
        }
        let mut auth = self.auth(false).await.map_err(|e| e.to_string())?;
        let body = frame.to_string();
        let mut resp = self.post(&auth, &body, permit).await?;
        if resp.status().as_u16() == 401 {
            auth = self.auth(true).await.map_err(|e| e.to_string())?;
            resp = self.post(&auth, &body, permit).await?;
        }
        let status = resp.status().as_u16();
        let session_id = resp.headers().get("mcp-session-id").and_then(|v| v.to_str().ok())
            .filter(|s| !s.is_empty() && s.len() <= 1024 && s.chars().all(|c| c.is_ascii_graphic()))
            .map(str::to_string);
        let sse = resp.headers().get("content-type").and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(';').next()).is_some_and(|v| v.trim().eq_ignore_ascii_case("text/event-stream"));
        if !sse {
            let bytes = super::super::http::read_capped(resp, MAX_MESSAGE_BYTES).await?;
            let reply = serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null);
            if !reply.is_null() && (reply.get("id") != frame.get("id") || reply.get("method").is_some()) {
                return Err("remote MCP returned an uncorrelated response".into());
            }
            return Ok((status, reply, session_id));
        }
        let modern = self.session.lock().await.protocol_version.as_deref() == Some(protocol::MODERN);
        let mut buf = Vec::new();
        let mut total = 0usize;
        while let Some(chunk) = resp.chunk().await.map_err(|_| "remote MCP event stream broke")? {
            total = total.saturating_add(chunk.len());
            if total > MAX_MESSAGE_BYTES { return Err("remote MCP response exceeded its byte limit".into()); }
            buf.extend_from_slice(&chunk);
            while let Some((event, rest)) = split_event(&buf) {
                buf = rest;
                let Some(data) = event_data(&event) else { continue };
                let reply: Value = serde_json::from_str(&data).map_err(|_| "invalid MCP SSE JSON")?;
                if reply.get("id").is_some() && reply.get("method").is_some() && modern {
                    return Err("modern MCP server sent an unsolicited request".into());
                }
                if reply.get("id") == frame.get("id") && reply.get("method").is_none() {
                    return Ok((status, reply, session_id));
                }
                if reply.get("id").is_none() && reply.get("method").is_some() {
                    self.emit(reply.to_string());
                }
            }
        }
        Err("remote MCP event stream ended before its answer; retry with a new request ID".into())
    }

    pub(super) async fn modern_subscription_once(&self) -> StreamEnd {
        let id = json!(format!("bridge-subscription:{}", uuid::Uuid::new_v4()));
        let mut frame = json!({"jsonrpc":"2.0","id":id,"method":"subscriptions/listen","params":{
            "notifications":{"toolsListChanged":true,"promptsListChanged":true,"resourcesListChanged":true}}});
        protocol::inject_meta(&mut frame, protocol::MODERN, &json!({}));
        let auth = match self.auth(false).await { Ok(auth) => auth, Err(_) => return StreamEnd::Stop("credential unavailable") };
        let mut response = match self.post(&auth, &frame.to_string(), None).await {
            Ok(response) => response, Err(_) => return StreamEnd::Retry,
        };
        if !response.status().is_success() { return StreamEnd::Stop("subscriptions unavailable"); }
        let mut acknowledged = false;
        let mut honored_notifications = std::collections::HashSet::new();
        let mut buffer = Vec::new();
        loop {
            let chunk = match response.chunk().await {
                Ok(Some(chunk)) => chunk,
                Ok(None) | Err(_) => return if acknowledged { StreamEnd::Progress } else { StreamEnd::Retry },
            };
            buffer.extend_from_slice(&chunk);
            if buffer.len() > MAX_MESSAGE_BYTES { return StreamEnd::Stop("subscription event too large"); }
            while let Some((event, rest)) = split_event(&buffer) {
                buffer = rest;
                let Some(data) = event_data(&event) else { continue };
                let Ok(value) = serde_json::from_str::<Value>(&data) else { return StreamEnd::Stop("invalid subscription event"); };
                if value.get("id") == Some(&id) && value.get("method").is_none() {
                    return if value["result"]["resultType"] == "complete"
                        && value["result"]["_meta"]["io.modelcontextprotocol/subscriptionId"] == id {
                        StreamEnd::Stop("subscription completed")
                    } else { StreamEnd::Stop("invalid subscription completion") };
                }
                if value.get("id").is_some() { return StreamEnd::Stop("server request on modern subscription"); }
                let params = &value["params"];
                if params["_meta"]["io.modelcontextprotocol/subscriptionId"] != id {
                    return StreamEnd::Stop("subscription ID mismatch");
                }
                let method = value["method"].as_str().unwrap_or("");
                if method == "notifications/subscriptions/acknowledged" {
                    if acknowledged { return StreamEnd::Stop("duplicate subscription acknowledgement"); }
                    let Some(honored) = params["notifications"].as_object() else { return StreamEnd::Stop("missing subscription acknowledgement"); };
                    if !honored.iter().all(|(name, value)| {
                        ["toolsListChanged", "promptsListChanged", "resourcesListChanged"].contains(&name.as_str()) && value.is_boolean()
                    }) { return StreamEnd::Stop("invalid acknowledged subscription subset"); }
                    for (name, value) in honored {
                        if value == true { honored_notifications.insert(name.clone()); }
                    }
                    acknowledged = true;
                    continue;
                }
                if !acknowledged { return StreamEnd::Stop("notification before subscription acknowledgement"); }
                let feature = match method {
                    "notifications/tools/list_changed" => "toolsListChanged",
                    "notifications/prompts/list_changed" => "promptsListChanged",
                    "notifications/resources/list_changed" => "resourcesListChanged",
                    _ => return StreamEnd::Stop("unrequested subscription notification"),
                };
                if !honored_notifications.contains(feature) { return StreamEnd::Stop("notification outside acknowledged subset"); }
                if method == "notifications/tools/list_changed" { self.schemas.lock().await.clear(); }
                self.emit(value.to_string());
            }
        }
    }

    pub(super) async fn remember_schemas(&self, result: &mut Value) {
        let Some(tools) = result.get_mut("tools").and_then(Value::as_array_mut) else { return };
        let mut schemas = self.schemas.lock().await;
        let mut annotations = self.effect_annotations.lock().await;
        schemas.clear();
        annotations.clear();
        tools.retain(|tool| {
            let Some(name) = tool["name"].as_str() else { return false };
            let schema = &tool["inputSchema"];
            if protocol::validate_header_schema(schema).is_err() {
                tracing::warn!(server = %self.server, "MCP tool has invalid header annotations; hiding it");
                return false;
            }
            schemas.insert(name.to_string(), schema.clone());
            annotations.insert(name.to_string(), tool["annotations"].clone());
            true
        });
    }

    pub(super) async fn ensure_schema(&self, name: &str) -> Result<(), String> {
        if !self.schemas.lock().await.contains_key(name) { self.refresh_schemas().await?; }
        if !self.schemas.lock().await.contains_key(name) { return Err("tool has no valid HTTP header schema".into()); }
        Ok(())
    }

    async fn refresh_schemas(&self) -> Result<(), String> {
        let mut request = json!({"jsonrpc":"2.0","id":format!("bridge-list:{}",uuid::Uuid::new_v4()),"method":"tools/list","params":{}});
        protocol::inject_meta(&mut request, protocol::MODERN, &json!({}));
        let (status, mut response, _) = self.round_trip(&request, None).await?;
        if response["error"]["code"] == -32022 {
            self.session.lock().await.requires_restart = true;
            self.schemas.lock().await.clear();
            self.effect_annotations.lock().await.clear();
            return Err("MCP protocol changed; restart the conversation to reconnect and reauthorize".into());
        }
        if !(200..300).contains(&status) || response.get("error").is_some() {
            return Err("cannot refresh MCP tool header annotations".into());
        }
        protocol::validate_result(&response)?;
        self.remember_schemas(&mut response["result"]).await;
        self.gate.filter_list_result(&mut response["result"]);
        Ok(())
    }

    pub(super) async fn modern_exchange(
        &self, original: Value, list_id: Option<&Value>,
        permit: Option<&crate::third_party_tools::DispatchPermit>,
    ) -> Result<(), String> {
        tokio::time::timeout(REQUEST_TIMEOUT, self.modern_exchange_inner(original, list_id, permit)).await
            .map_err(|_| "MCP operation deadline exceeded".to_string())?
    }

    async fn modern_exchange_inner(
        &self, original: Value, list_id: Option<&Value>,
        permit: Option<&crate::third_party_tools::DispatchPermit>,
    ) -> Result<(), String> {
        let Some(id) = original.get("id").cloned() else { return Ok(()) };
        let Some(method) = original["method"].as_str() else {
            return Err("modern HTTP cannot carry client JSON-RPC responses".into());
        };
        if ["ping", "logging/setLevel", "resources/subscribe", "resources/unsubscribe"].contains(&method) {
            return Err("method is not supported by the modern MCP server".into());
        }
        if method == "tools/call" {
            let name = original["params"]["name"].as_str().ok_or("tool call omitted its name")?;
            if !self.schemas.lock().await.contains_key(name) { return Err("tool has no valid HTTP header schema".into()); }
        }
        let mut request = original.clone();
        protocol::inject_meta(&mut request, protocol::MODERN, &json!({}));
        let mut refreshed = false;
        for _ in 0..MAX_ROUNDS {
            let (status, mut reply, _) = self.round_trip(&request, permit).await?;
            if reply["error"]["code"] == -32022 {
                // Never reuse a permit across protocol generations, even for reads.
                self.session.lock().await.requires_restart = true;
                self.schemas.lock().await.clear();
                self.effect_annotations.lock().await.clear();
                return Err("MCP protocol changed; restart the conversation to reconnect and reauthorize".into());
            }
            if reply["error"]["code"] == -32020 && method == "tools/call" && !refreshed {
                let name = original["params"]["name"].as_str().unwrap_or("");
                let previous = self.effect_annotations.lock().await.get(name).cloned();
                self.refresh_schemas().await?;
                if previous != self.effect_annotations.lock().await.get(name).cloned() {
                    return Err("tool annotations changed; retry as a new gated call".into());
                }
                if !self.schemas.lock().await.contains_key(name) { return Err("tool header schema became invalid".into()); }
                self.gate.check_call_with_args(name, &original["params"]["arguments"]).await?;
                request["id"] = json!(format!("bridge-retry:{}", uuid::Uuid::new_v4()));
                refreshed = true;
                continue;
            }
            if reply.is_null() { return Err(format!("remote MCP returned no JSON-RPC answer (HTTP {status})")); }
            protocol::validate_result(&reply)?;
            if reply["result"]["resultType"] == "input_required" {
                return Err("MCP bridge multi-round inputs are not supported".into());
            }
            reply["id"] = id;
            self.deliver(reply, false, list_id).await;
            return Ok(());
        }
        Err("MCP multi round-trip limit exceeded".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modern_bridge_fallback_distinguishes_protocol_errors() {
        for status in [400, 404, 405] {
            assert!(legacy_fallback(status, &Value::Null));
            assert!(legacy_fallback(status, &json!({"error":{"code":-32602}})));
            for code in [-32020, -32021, -32022] {
                assert!(!legacy_fallback(status, &json!({"error":{"code":code}})));
            }
        }
        assert!(!legacy_fallback(404, &json!({"error":{"code":-32601}})));
        assert!(!legacy_fallback(401, &Value::Null));
        assert!(!legacy_fallback(500, &Value::Null));
    }

    #[test]
    fn modern_bridge_discovery_has_required_metadata_and_no_session() {
        let frame = discover_frame();
        assert_eq!(frame["method"], "server/discover");
        assert_eq!(frame["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"], protocol::MODERN);
        assert_eq!(frame["params"]["_meta"]["io.modelcontextprotocol/clientCapabilities"], json!({}));
        let headers = protocol::request_headers(&frame, protocol::MODERN, None).unwrap();
        assert!(headers.iter().any(|(k, v)| k.eq_ignore_ascii_case("Mcp-Method") && v == "server/discover"));
        assert!(!headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("Mcp-Session-Id")));
    }

    #[derive(Clone)]
    struct MockState {
        version: &'static str,
        mismatch: bool,
        tool_reply: Arc<std::sync::Mutex<Value>>,
        seen: Arc<std::sync::Mutex<Vec<(Value, axum::http::HeaderMap)>>>,
    }

    async fn mock_post(
        axum::extract::State(state): axum::extract::State<MockState>,
        headers: axum::http::HeaderMap, body: String,
    ) -> axum::response::Response {
        use axum::response::IntoResponse;
        let frame: Value = serde_json::from_str(&body).unwrap();
        state.seen.lock().unwrap().push((frame.clone(), headers.clone()));
        let method = frame["method"].as_str().unwrap();
        if method == "server/discover" && state.version != protocol::MODERN {
            if state.mismatch {
                return (axum::http::StatusCode::BAD_REQUEST, axum::Json(json!({"jsonrpc":"2.0","id":frame["id"],
                    "error":{"code":-32022,"message":"unsupported","data":{"supported":[state.version]}}}))).into_response();
            }
            return axum::http::StatusCode::BAD_REQUEST.into_response();
        }
        if method == "notifications/initialized" { return axum::http::StatusCode::ACCEPTED.into_response(); }
        if method == "tools/list" && state.version == protocol::MODERN && state.mismatch {
            return axum::Json(json!({"jsonrpc":"2.0","id":frame["id"],
                "error":{"code":-32022,"message":"list downgrade","data":{"supported":["2025-06-18"]}}})).into_response();
        }
        if method != "initialize" && state.version != protocol::MODERN
            && headers.get("mcp-protocol-version").and_then(|v| v.to_str().ok()) == Some(protocol::MODERN) {
            return (axum::http::StatusCode::BAD_REQUEST, axum::Json(json!({"jsonrpc":"2.0","id":frame["id"],
                "error":{"code":-32022,"message":"unsupported","data":{"supported":[state.version]}}}))).into_response();
        }
        if state.version == protocol::MODERN {
            assert_eq!(headers.get("mcp-protocol-version").unwrap(), protocol::MODERN);
            assert_eq!(headers.get("mcp-method").unwrap(), method);
            assert!(headers.get("mcp-session-id").is_none());
            assert_eq!(frame["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"], protocol::MODERN);
        } else if method != "initialize" {
            assert_eq!(headers.get("mcp-session-id").unwrap(), "test-session");
            assert_eq!(headers.get("mcp-protocol-version").unwrap(), state.version);
        }
        if method == "tools/call" {
            let reply = state.tool_reply.lock().unwrap().clone();
            if !reply.is_null() {
                let mut response = reply;
                response["jsonrpc"] = json!("2.0");
                response["id"] = frame["id"].clone();
                return axum::Json(response).into_response();
            }
        }
        let result = match method {
            "server/discover" => json!({"resultType":"complete","supportedVersions":[protocol::MODERN],"capabilities":{"tools":{}},"ttlMs":0,"cacheScope":"private"}),
            "tools/list" => json!({"resultType":"complete","tools":[{"name":"echo","inputSchema":{"type":"object"},"annotations":{"readOnlyHint":true}}]}),
            "initialize" => json!({"protocolVersion":state.version,"capabilities":{"tools":{}},"serverInfo":{"name":"mock","version":"1"}}),
            "resources/read" if state.version == protocol::MODERN && frame["params"].get("requestState").is_none() => {
                assert_eq!(headers.get("mcp-name").unwrap().to_str().unwrap(), protocol::encode_header_value("file:///測試"));
                json!({"resultType":"input_required","requestState":"opaque 多位元組"})
            }
            "resources/read" => {
                if state.version == protocol::MODERN { assert_eq!(frame["params"]["requestState"], "opaque 多位元組"); }
                json!({"resultType":"complete","contents":[]})
            }
            _ => json!({"resultType":"complete","tools":[]}),
        };
        let mut response = axum::Json(json!({"jsonrpc":"2.0","id":frame["id"],"result":result})).into_response();
        response.headers_mut().insert("mcp-session-id", "test-session".parse().unwrap());
        response
    }

    async fn mock_bridge(version: &'static str, mismatch: bool) -> (Bridge, mpsc::UnboundedReceiver<String>, MockState, tempfile::TempDir, tokio::task::JoinHandle<()>) {
        let state = MockState { version, mismatch, tool_reply: Arc::new(std::sync::Mutex::new(Value::Null)), seen: Arc::new(std::sync::Mutex::new(Vec::new())) };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        let app = axum::Router::new().route("/mcp", axum::routing::post(mock_post)).with_state(state.clone());
        let server_task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let home = tempfile::tempdir().unwrap();
        let agent = home.path().join("agents/a1");
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::write(agent.join("agent.toml"), "").unwrap();
        let secrets = super::super::super::store::RemoteSecrets { url, bearer: None, oauth: None, headers: vec![] };
        let record = super::super::super::store::RemoteServerRecord {
            agent_id:"a1".into(), server:"mock".into(), auth:super::super::super::store::AuthKind::None,
            host:"127.0.0.1".into(), status:super::super::super::store::ConnStatus::Connected,
            created_at:"now".into(), updated_at:"now".into(), access_expires_at:None,
            has_refresh_token:false, server_stream:false, header_names:vec![],
            secret_enc:super::super::super::store::seal(home.path(), &secrets).unwrap(),
        };
        super::super::super::store::upsert(home.path(), record).unwrap();
        let (out, receiver) = mpsc::unbounded_channel();
        let bridge = Bridge {
            home:home.path().to_path_buf(), agent_id:"a1".into(), server:"mock".into(),
            session:Mutex::new(Session { session_id:None, protocol_version:None, discovery:None, requires_restart:false, downstream_capabilities:json!({}), legacy_endpoint:None, legacy_source:None }),
            clients:Mutex::new(ClientCache { client:None }), negotiation:Mutex::new(()),
            schemas:Mutex::new(std::collections::HashMap::new()), effect_annotations:Mutex::new(std::collections::HashMap::new()), legacy_stream:Mutex::new(None),
            last_auth:Mutex::new(None), out,
            gate:crate::third_party_tools::ThirdPartyGate::new(home.path(), "a1", "mock", "bridge"), server_stream:false,
        };
        (bridge, receiver, state, home, server_task)
    }

    #[tokio::test]
    async fn failed_bridge_connection_never_exposes_secret_url() {
        let (bridge, mut receiver, _, home, task) = mock_bridge(protocol::MODERN, false).await;
        let socket = tokio::net::TcpSocket::new_v4().unwrap();
        socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let url = format!("http://{}/api/SECRET_PATH?key=SECRET_QUERY", socket.local_addr().unwrap());
        let mut record = super::super::super::store::get(home.path(), "a1", "mock").unwrap().unwrap();
        let mut secrets = super::super::super::store::open(home.path(), &record).unwrap();
        secrets.url = url;
        record.secret_enc = super::super::super::store::seal(home.path(), &secrets).unwrap();
        super::super::super::store::upsert(home.path(), record).unwrap();
        bridge.forward(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}).to_string()).await;
        let reply = receiver.recv().await.unwrap();
        let frame: Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(frame["id"], 1);
        assert_eq!(frame["error"]["message"], format!("cannot reach {}", socket.local_addr().unwrap()));
        assert!(!reply.contains("SECRET"));
        assert!(!reply.contains("/api/"));
        task.abort();
    }

    #[tokio::test]
    async fn modern_bridge_generations_cache_headers_and_refuse_mrtr() {
        for (version, mismatch) in [(protocol::MODERN, false), ("2025-11-25", false), ("2025-06-18", false), ("2025-06-18", true)] {
            let (bridge, mut out, state, _home, server) = mock_bridge(version, mismatch).await;
            let discovery = bridge.negotiate().await.unwrap();
            assert_eq!(discovery["resultType"], "complete");
            assert_eq!(bridge.session.lock().await.protocol_version.as_deref(), Some(version));
            bridge.negotiate().await.unwrap();
            assert_eq!(state.seen.lock().unwrap().iter().filter(|(f, _)| f["method"] == "server/discover").count(), 1);
            let request = json!({"jsonrpc":"2.0","id":42,"method":"resources/read","params":{"uri":"file:///測試"}});
            if version == protocol::MODERN {
                assert!(bridge.modern_exchange(request, None, None).await.unwrap_err().contains("multi-round"));
                let calls: Vec<Value> = state.seen.lock().unwrap().iter().filter(|(f, _)| f["method"] == "resources/read").map(|(f, _)| f.clone()).collect();
                assert_eq!(calls.len(), 1);
                assert!(out.try_recv().is_err());
            } else {
                let (status, reply, _) = bridge.round_trip(&request, None).await.unwrap();
                assert_eq!(status, 200);
                assert_eq!(reply["id"], 42);
            }
            server.abort();
        }
    }


    #[test]
    fn modern_bridge_preserves_legacy_extension_metadata() {
        let mut frame = json!({"params":{"_meta":{"progressToken":"p","extension":{"value":1}}}});
        protocol::inject_meta(&mut frame, protocol::MODERN, &json!({}));
        strip_modern_metadata(&mut frame);
        assert_eq!(frame["params"]["_meta"], json!({"progressToken":"p","extension":{"value":1}}));
    }

    #[tokio::test]
    async fn modern_bridge_schema_refresh_mismatch_requires_restart() {
        let (bridge, _out, state, _home, server) = mock_bridge(protocol::MODERN, true).await;
        bridge.negotiate().await.unwrap();
        assert!(bridge.ensure_schema("echo").await.unwrap_err().contains("restart"));
        assert!(bridge.session.lock().await.requires_restart);
        let before = state.seen.lock().unwrap().len();
        assert!(bridge.ensure_schema("echo").await.is_err());
        assert_eq!(state.seen.lock().unwrap().len(), before);
        server.abort();
    }

    #[tokio::test]
    async fn modern_bridge_midcall_mismatch_never_replays_or_reuses_generation() {
        let (bridge, mut out, state, _home, server) = mock_bridge(protocol::MODERN, false).await;
        bridge.forward(json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}).to_string()).await;
        assert!(serde_json::from_str::<Value>(&out.recv().await.unwrap()).unwrap().get("result").is_some());
        *state.tool_reply.lock().unwrap() = json!({"error":{"code":-32022,"message":"downgrade","data":{"supported":["2025-06-18"]}}});
        let call = json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"echo","arguments":{}}});
        bridge.forward(call.to_string()).await;
        let response: Value = serde_json::from_str(&out.recv().await.unwrap()).unwrap();
        assert_eq!(response["id"], 2);
        assert!(response["error"]["message"].as_str().unwrap().contains("restart"));
        // Even a new downstream request cannot reuse stale schemas or switch eras.
        for method in ["tools/call", "tools/list", "server/discover"] {
            let mut retry = call.clone();
            retry["id"] = json!(3);
            retry["method"] = json!(method);
            bridge.forward(retry.to_string()).await;
            let response: Value = serde_json::from_str(&out.recv().await.unwrap()).unwrap();
            assert!(response["error"]["message"].as_str().unwrap().contains("restart"));
        }
        assert!(bridge.schemas.lock().await.is_empty());
        assert!(bridge.effect_annotations.lock().await.is_empty());
        let seen = state.seen.lock().unwrap();
        assert_eq!(seen.iter().filter(|(f, _)| f["method"] == "tools/call").count(), 1);
        assert!(!seen.iter().any(|(f, _)| f["method"] == "initialize"));
        server.abort();
    }

    #[tokio::test]
    async fn modern_bridge_refuses_all_inputs_and_forged_continuations() {
        let (bridge, mut out, state, _home, server) = mock_bridge(protocol::MODERN, false).await;
        let request = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"echo","arguments":{},
            "_meta":{"io.modelcontextprotocol/clientCapabilities":{"sampling":{},"roots":{},"elicitation":{"form":{}}}}}});
        // Declared and undeclared capabilities, unknown modes, oversized and malformed inputs.
        let oversized: serde_json::Map<String, Value> = (0..33).map(|i| (i.to_string(), json!({"method":"roots/list"}))).collect();
        for result in [
            json!({"inputRequests":{"a":{"method":"sampling/createMessage"}}}),
            json!({"inputRequests":{"a":{"method":"roots/list"}}}),
            json!({"inputRequests":{"a":{"method":"elicitation/create","params":{"mode":"url"}}}}),
            json!({"inputRequests":{"a":{"method":"elicitation/create","params":{"mode":"form"}}}}),
            json!({"inputRequests":{"a":{"method":"elicitation/create","params":{"mode":"unknown"}}}}),
            json!({"inputRequests":oversized}), json!({"inputRequests":[]}), json!({"inputRequests":{}}),
            json!({"requestState":"opaque sensitive state"}), json!({}),
        ] {
            let mut result = result;
            result["resultType"] = json!("input_required");
            *state.tool_reply.lock().unwrap() = json!({"result":result});
            bridge.forward(request.to_string()).await;
            let response: Value = serde_json::from_str(&out.recv().await.unwrap()).unwrap();
            assert_eq!(response["id"], 1);
            assert!(response["error"]["message"].as_str().unwrap().contains("multi-round"), "{response}");
            assert!(response.get("result").is_none());
        }
        let before = state.seen.lock().unwrap().len();
        for method in ["tools/call", "resources/read", "prompts/get"] {
            for field in ["requestState", "inputResponses"] {
                for value in [Value::Null, json!({"secret":"must not leave"})] {
                    let mut continuation = request.clone();
                    continuation["method"] = json!(method);
                    continuation["params"][field] = value;
                    bridge.forward(continuation.to_string()).await;
                    let response: Value = serde_json::from_str(&out.recv().await.unwrap()).unwrap();
                    assert!(response["error"]["message"].as_str().unwrap().contains("multi-round"));
                }
            }
        }
        let seen = state.seen.lock().unwrap();
        assert_eq!(seen.len(), before, "continuation data must never reach upstream");
        let calls: Vec<_> = seen.iter().filter(|(f, _)| f["method"] == "tools/call").collect();
        assert_eq!(calls.len(), 10, "no internal state-only retries");
        assert!(calls.iter().all(|(f, _)| f["params"]["_meta"]["io.modelcontextprotocol/clientCapabilities"] == json!({})));
        server.abort();
    }

    #[tokio::test]
    async fn modern_bridge_refuses_mrtr_from_legacy_upstream() {
        let (bridge, mut out, state, _home, server) = mock_bridge("2025-06-18", false).await;
        *state.tool_reply.lock().unwrap() = json!({"result":{"resultType":"input_required",
            "inputRequests":{"a":{"method":"sampling/createMessage"}}}});
        bridge.forward(json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}).to_string()).await;
        assert!(serde_json::from_str::<Value>(&out.recv().await.unwrap()).unwrap().get("result").is_some());
        bridge.forward(json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"echo","arguments":{}}}).to_string()).await;
        let response: Value = serde_json::from_str(&out.recv().await.unwrap()).unwrap();
        assert_eq!(response["id"], 2);
        assert!(response["error"]["message"].as_str().unwrap().contains("multi-round"));
        assert!(response.get("result").is_none());
        server.abort();
    }

    #[tokio::test]
    async fn modern_bridge_never_sends_reconnected_credentials_to_stale_sse_endpoint() {
        let (bridge, _out, state, _home, server) = mock_bridge("2025-06-18", false).await;
        let mut auth = bridge.auth(false).await.unwrap();
        {
            let mut session = bridge.session.lock().await;
            session.legacy_source = Some(auth.url.clone());
            session.legacy_endpoint = Some(auth.url.join("/messages?session=old").unwrap());
            session.protocol_version = Some("2025-06-18".into());
        }
        auth.authorization = Some("Bearer new-server-token".into());
        auth.url.set_path("/different-mcp-server");
        let request = json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}).to_string();
        assert!(bridge.post(&auth, &request, None).await.is_err());
        auth.url = Url::parse("https://other.example.com/mcp").unwrap();
        assert!(bridge.post(&auth, &request, None).await.is_err());
        assert!(state.seen.lock().unwrap().is_empty(), "neither credential was dispatched to the old server");
        server.abort();
    }

}
