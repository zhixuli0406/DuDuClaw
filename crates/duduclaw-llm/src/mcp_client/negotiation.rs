use super::*;

impl McpClient {
    pub fn protocol_version(&self) -> &str { self.protocol_version }

    pub fn set_input_handler(&mut self, handler: Arc<dyn McpInputHandler>) {
        self.input_handler = Some(handler);
    }

    pub(super) fn client_capabilities(&self) -> Value {
        self.input_handler.as_ref().map(|h| h.capabilities()).unwrap_or_else(|| json!({}))
    }

    pub(super) async fn handshake(&mut self) -> Result<(), McpError> {
        // Negotiate each connection independently, including its auth principal.
        self.protocol_version = protocol::MODERN;
        if let McpTransport::Http { session_id, legacy, .. } = &mut self.transport {
            *session_id = None;
            *legacy = None;
        }
        let id = self.alloc_id();
        let frame = make_request(id, "server/discover", json!({}));
        let is_http = matches!(self.transport, McpTransport::Http { .. });
        let response = tokio::time::timeout(self.timeout.min(Duration::from_secs(2)), self.request(frame, id))
            .await.unwrap_or(Err(McpError::Timeout));
        match response {
            Ok(response) if response.pointer("/error/code").and_then(Value::as_i64) == Some(-32022) => {
                let selected = protocol::best_supported(&response["error"]["data"]["supported"])
                    .filter(|v| *v != protocol::MODERN)
                    .ok_or_else(|| McpError::Parse("no mutually supported MCP revision".into()))?;
                self.protocol_version = selected;
                self.legacy_handshake().await?;
            }
            Ok(response) if response.get("error").is_none() => {
                protocol::validate_result(&response).map_err(McpError::Parse)?;
                if response["result"].get("resultType").is_some_and(|v| v != "complete") {
                    return Err(McpError::Parse("discovery result must be complete".into()));
                }
                if !response["result"]["supportedVersions"].as_array()
                    .is_some_and(|versions| versions.iter().any(|v| v == protocol::MODERN))
                    || !response["result"]["capabilities"].is_object()
                {
                    return Err(McpError::Parse("invalid MCP discovery result".into()));
                }
                self.read_workflow_capability(&response);
            }
            Ok(response) => {
                let code = response.pointer("/error/code").and_then(Value::as_i64);
                if is_http && code == Some(-32601) && self.last_http_status == Some(404) {
                    // Discovery is optional; this is an explicitly modern error.
                    return Ok(());
                }
                if is_http && matches!(code, Some(-32020 | -32021)) {
                    return Err(rpc_error_of(&response).unwrap());
                }
                self.protocol_version = MCP_PROTOCOL_VERSION;
                self.legacy_handshake().await?;
            }
            Err(McpError::HttpStatus(400 | 404 | 405)) => {
                self.protocol_version = MCP_PROTOCOL_VERSION;
                self.legacy_handshake().await?;
            }
            Err(error) if !is_http && matches!(error, McpError::Timeout | McpError::Rpc { .. }) => {
                self.protocol_version = MCP_PROTOCOL_VERSION;
                self.legacy_handshake().await?;
            }
            Err(error) => return Err(error),
        }
        Ok(())
    }

    fn read_workflow_capability(&mut self, response: &Value) {
        self.workflow_version = response.pointer("/result/capabilities/experimental/duduclaw_workflow/version")
            .and_then(Value::as_u64).and_then(|v| u32::try_from(v).ok());
    }

    pub(super) async fn recover_version(&mut self, response: &Result<Value, McpError>) -> Result<bool, McpError> {
        if let Ok(response) = response {
            if response.pointer("/error/code").and_then(Value::as_i64) == Some(-32022) {
                self.protocol_version = protocol::best_supported(&response["error"]["data"]["supported"])
                    .filter(|v| *v != self.protocol_version)
                    .ok_or_else(|| McpError::Parse("no mutually supported MCP revision".into()))?;
                if self.protocol_version != protocol::MODERN { self.legacy_handshake().await?; }
                return Ok(true);
            }
        }
        if matches!(response, Err(McpError::HttpStatus(400 | 404 | 405))) {
            self.handshake().await?;
            return Ok(true);
        }
        Ok(false)
    }

    async fn legacy_handshake(&mut self) -> Result<(), McpError> {
        if let McpTransport::Http { session_id, .. } = &mut self.transport { *session_id = None; }
        let id = self.alloc_id();
        let mut frame = build_initialize_request(id);
        frame["params"]["protocolVersion"] = json!(self.protocol_version);
        frame["params"]["capabilities"] = json!({});
        let response = if matches!(self.transport, McpTransport::Http { .. }) {
            match self.http_request(frame.clone(), id, true).await {
                Err(McpError::HttpStatus(400 | 404 | 405)) => {
                    tokio::time::timeout(self.timeout, self.open_legacy_http()).await.map_err(|_| McpError::Timeout)??;
                    let retry = self.alloc_id();
                    frame["id"] = json!(retry);
                    self.http_request(frame, retry, false).await?
                }
                result => result?,
            }
        } else { self.request(frame, id).await? };
        if let Some(error) = rpc_error_of(&response) { return Err(error); }
        let version = response.pointer("/result/protocolVersion").and_then(Value::as_str)
            .ok_or_else(|| McpError::Parse("missing negotiated MCP revision".into()))?;
        self.protocol_version = protocol::SUPPORTED.iter().copied()
            .find(|v| *v == version && *v != protocol::MODERN)
            .ok_or_else(|| McpError::Parse("unsupported negotiated MCP revision".into()))?;
        self.read_workflow_capability(&response);
        let note = build_initialized_notification();
        if matches!(self.transport, McpTransport::Http { .. }) {
            if let Err(error) = self.http_notify(&note).await {
                if self.is_scoped_http() { return Err(error); }
                warn!("MCP initialized notification not accepted");
            }
        } else { self.send_line(&note).await?; }
        Ok(())
    }

    pub(super) async fn call_rounds(&mut self, original: Value) -> Result<Value, McpError> {
        if self.protocol_version == protocol::MODERN && matches!(self.transport, McpTransport::Http { .. }) {
            let name = original["params"]["name"].as_str().unwrap_or("");
            if !self.tool_schemas.contains_key(name) {
                self.list_tools_inner(false).await?;
                if !self.tool_schemas.contains_key(name) {
                    return Err(McpError::Parse("tool is not available with valid header annotations".into()));
                }
            }
        }
        let mut frame = original.clone();
        let mut refreshed_headers = false;
        for _ in 0..8 {
            let id = self.alloc_id();
            frame["id"] = json!(id);
            let response = self.request(frame.clone(), id).await?;
            let code = response.pointer("/error/code").and_then(Value::as_i64);
            if code == Some(-32022) {
                // The caller's approval and tool definitions belong to this connection.
                self.requires_reconnect = true;
                self.tool_schemas.clear();
                return Err(McpError::Parse("MCP protocol changed; reconnect and rediscover tools before retrying".into()));
            }
            if code == Some(-32020) && self.protocol_version == protocol::MODERN && !refreshed_headers {
                refreshed_headers = true;
                self.list_tools_inner(false).await?;
                let name = original["params"]["name"].as_str().unwrap_or("");
                if !self.tool_schemas.contains_key(name) {
                    return Err(McpError::Parse("tool no longer available after schema refresh".into()));
                }
                continue;
            }
            protocol::validate_result(&response).map_err(McpError::Parse)?;
            if response.pointer("/result/resultType").and_then(Value::as_str) != Some("input_required") {
                return Ok(response);
            }
            if self.protocol_version != protocol::MODERN {
                return Err(McpError::Parse("MRTR requires modern MCP".into()));
            }
            let result = &response["result"];
            frame = original.clone();
            if let Some(state) = result.get("requestState") {
                frame["params"]["requestState"] = state.clone();
            }
            if let Some(inputs) = result.get("inputRequests") {
                let inputs = inputs.as_object().filter(|v| v.len() <= 32)
                    .ok_or_else(|| McpError::Parse("invalid MCP inputRequests".into()))?;
                if inputs.is_empty() && result.get("requestState").is_none() {
                    return Err(McpError::Parse("MCP input_required without state or inputs".into()));
                }
                let mut answers = serde_json::Map::new();
                for (key, input) in inputs {
                    let handler = self.input_handler.as_ref()
                        .ok_or_else(|| McpError::Parse("MCP requested an undeclared client capability".into()))?;
                    if !input_supported(input, &handler.capabilities()) {
                        return Err(McpError::Parse("MCP requested an undeclared client capability".into()));
                    }
                    answers.insert(key.clone(), handler.respond(input).await?);
                }
                frame["params"]["inputResponses"] = Value::Object(answers);
            } else if result.get("requestState").is_none() {
                return Err(McpError::Parse("MCP input_required without state or inputs".into()));
            }
        }
        Err(McpError::Parse("MCP input round limit exceeded".into()))
    }
}

fn input_supported(input: &Value, capabilities: &Value) -> bool {
    match input["method"].as_str() {
        Some("roots/list") => capabilities["roots"].is_object(),
        Some("sampling/createMessage") => capabilities["sampling"].is_object(),
        Some("elicitation/create") => match input["params"]["mode"].as_str().unwrap_or("form") {
            "form" => capabilities["elicitation"].as_object()
                .is_some_and(|e| e.is_empty() || e.get("form").is_some_and(Value::is_object)),
            "url" => capabilities["elicitation"]["url"].is_object(),
            _ => false,
        },
        _ => false,
    }
}

pub(super) fn protocol_header(name: &str) -> bool {
    matches!(name.to_ascii_lowercase().as_str(), "mcp-protocol-version" | "mcp-method" | "mcp-name" | "mcp-session-id" | "last-event-id")
        || name.split_once('-').is_some_and(|(prefix, rest)| prefix.eq_ignore_ascii_case("mcp")
            && rest.split_once('-').is_some_and(|(part, _)| part.eq_ignore_ascii_case("param")))
}

pub(super) async fn read_http_body(mut response: reqwest::Response, id: i64, sse: bool) -> Result<String, McpError> {
    let mut buffer = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| McpError::Io("MCP response interrupted".into()))? {
        if buffer.len().saturating_add(chunk.len()) as u64 > MAX_HTTP_BODY_BYTES {
            return Err(McpError::Io("MCP HTTP response too large".into()));
        }
        buffer.extend_from_slice(&chunk);
        if sse {
            let text = match std::str::from_utf8(&buffer) {
                Ok(text) => text,
                Err(error) if error.error_len().is_none() => std::str::from_utf8(&buffer[..error.valid_up_to()])
                    .map_err(|_| McpError::Parse("invalid MCP SSE UTF-8".into()))?,
                Err(_) => return Err(McpError::Parse("invalid MCP SSE UTF-8".into())),
            }.replace("\r\n", "\n");
            for event in text.split("\n\n").take(text.split("\n\n").count().saturating_sub(1)) {
                let data = event.lines().filter_map(|line| line.strip_prefix("data:")
                    .map(|line| line.strip_prefix(' ').unwrap_or(line))).collect::<Vec<_>>().join("\n");
                if let Ok(frame) = serde_json::from_str::<Value>(&data) {
                    if frame.get("method").is_some() && frame.get("id").is_some() {
                        return Err(McpError::Parse("unsolicited MCP server request".into()));
                    }
                    if frame["id"].as_i64() == Some(id) { return Ok(format!("data: {frame}\n\n")); }
                }
            }
        }
    }
    String::from_utf8(buffer).map_err(|_| McpError::Parse("MCP response is not UTF-8".into()))
}
