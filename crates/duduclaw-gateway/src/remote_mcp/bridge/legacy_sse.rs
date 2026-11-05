//! Deprecated HTTP+SSE is only entered after both modern discovery and legacy
//! Streamable HTTP initialization fail with a fallback-eligible status.

use super::*;

pub(super) struct LegacyStream {
    response: reqwest::Response,
    buffer: Vec<u8>,
}

fn endpoint_event(event: &[u8]) -> bool {
    String::from_utf8_lossy(event).lines().any(|line| {
        line.strip_prefix("event:").is_some_and(|value| value.trim() == "endpoint")
    })
}

fn endpoint_url(base: &Url, value: &str) -> Result<Url, String> {
    let target = base.join(value).map_err(|_| "legacy MCP server sent an invalid endpoint")?;
    super::super::url_policy::check_outbound(&target, OutboundPolicy::for_mcp_url(base))?;
    if target.origin() != base.origin() {
        return Err("legacy MCP endpoint must remain on the authenticated server origin".into());
    }
    Ok(target)
}

/// The saved SSE endpoint belongs to the exact MCP resource that opened it.
/// Reconnecting a named record must never route its new token to an old URL.
pub(super) fn checked_endpoint(endpoint: &Url, source: &Url, current: &Url) -> Result<Url, String> {
    if source != current || endpoint.origin() != current.origin() {
        return Err("remote MCP connection changed; restart the conversation before using the legacy stream".into());
    }
    Ok(endpoint.clone())
}

impl Bridge {
    pub(super) async fn open_legacy_sse(&self) -> Result<(), String> {
        let auth = self.auth(false).await.map_err(|e| e.to_string())?;
        let client = self.client_for(&auth.url).await?;
        let req = super::super::connect::apply_upstream_headers(
            client.get(auth.url.clone()).header("Accept", "text/event-stream"), &auth,
        );
        let mut response = req.send().await.map_err(|_| "cannot open legacy MCP event stream")?;
        if !response.status().is_success() { return Err("legacy MCP event stream refused".into()); }
        let mut buffer = Vec::new();
        let mut total = 0usize;
        while let Some(chunk) = response.chunk().await.map_err(|_| "legacy MCP event stream broke")? {
            total = total.saturating_add(chunk.len());
            if total > MAX_MESSAGE_BYTES { return Err("legacy MCP endpoint event too large".into()); }
            buffer.extend_from_slice(&chunk);
            while let Some((event, rest)) = split_event(&buffer) {
                buffer = rest;
                if endpoint_event(&event) {
                    let value = event_data(&event).ok_or("legacy MCP endpoint event has no data")?;
                    let endpoint = endpoint_url(&auth.url, &value)?;
                    {
                        let mut session = self.session.lock().await;
                        session.legacy_endpoint = Some(endpoint);
                        session.legacy_source = Some(auth.url.clone());
                    }
                    *self.legacy_stream.lock().await = Some(LegacyStream { response, buffer });
                    return Ok(());
                }
            }
        }
        Err("legacy MCP stream ended before its endpoint event".into())
    }

    pub(super) async fn legacy_sse_round_trip(
        &self, frame: &Value, permit: Option<&crate::third_party_tools::DispatchPermit>,
    ) -> Result<(u16, Value, Option<String>), String> {
        // Serialize request writes and their reads. Notification/response writes
        // bypass this reader lock so a legacy server request cannot deadlock it.
        let mut stream = self.legacy_stream.lock().await;
        let stream = stream.as_mut().ok_or("legacy MCP stream is unavailable")?;
        let auth = self.auth(false).await.map_err(|e| e.to_string())?;
        let response = self.post(&auth, &frame.to_string(), permit).await?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            let body = super::super::http::read_capped(response, MAX_MESSAGE_BYTES).await?;
            return Ok((status, serde_json::from_slice(&body).unwrap_or(Value::Null), None));
        }
        let mut total = 0usize;
        loop {
            while let Some((event, rest)) = split_event(&stream.buffer) {
                stream.buffer = rest;
                let Some(data) = event_data(&event) else { continue };
                let value: Value = serde_json::from_str(&data).map_err(|_| "invalid legacy MCP event JSON")?;
                if value.get("id") == frame.get("id") && value.get("method").is_none() {
                    return Ok((200, value, None));
                }
                if value.get("method").is_some() { self.emit(value.to_string()); }
            }
            let chunk = stream.response.chunk().await.map_err(|_| "legacy MCP stream broke")?
                .ok_or("legacy MCP stream ended before its response")?;
            total = total.saturating_add(chunk.len());
            if total > MAX_MESSAGE_BYTES { return Err("legacy MCP response too large".into()); }
            stream.buffer.extend_from_slice(&chunk);
        }
    }

    pub(super) async fn legacy_sse_exchange(
        &self, frame: Value, list_id: Option<&Value>,
        permit: Option<&crate::third_party_tools::DispatchPermit>,
    ) -> Result<(), String> {
        if !matches!(classify(&frame), Kind::Request(_)) {
            let auth = self.auth(false).await.map_err(|e| e.to_string())?;
            let resp = self.post(&auth, &frame.to_string(), permit).await?;
            return if resp.status().is_success() { Ok(()) } else { Err("legacy MCP message refused".into()) };
        }
        let (status, reply, _) = self.legacy_sse_round_trip(&frame, permit).await?;
        if !(200..300).contains(&status) { return Err(format!("legacy MCP returned HTTP {status}")); }
        self.deliver(reply, false, list_id).await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Default)]
    struct SseMock {
        sender: Arc<std::sync::Mutex<Option<mpsc::UnboundedSender<String>>>>,
        methods: Arc<std::sync::Mutex<Vec<String>>>,
    }

    async fn mock_get(axum::extract::State(state): axum::extract::State<SseMock>) -> axum::response::Response {
        use axum::response::IntoResponse;
        let (tx, rx) = mpsc::unbounded_channel::<String>();
        *state.sender.lock().unwrap() = Some(tx.clone());
        tx.send("event: endpoint\ndata: /messages?session=one\n\n".into()).unwrap();
        let stream = futures_util::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|line| (Ok::<_, std::convert::Infallible>(line), rx))
        });
        ([("content-type", "text/event-stream")], axum::body::Body::from_stream(stream)).into_response()
    }

    async fn mock_message(
        axum::extract::State(state): axum::extract::State<SseMock>, headers: axum::http::HeaderMap, body: String,
    ) -> axum::http::StatusCode {
        assert_eq!(headers.get("authorization").unwrap(), "Bearer test-token");
        let frame: Value = serde_json::from_str(&body).unwrap();
        state.methods.lock().unwrap().push(frame["method"].as_str().unwrap().to_string());
        if frame["method"] == "initialize" {
            let reply = json!({"jsonrpc":"2.0","id":frame["id"],"result":{"protocolVersion":"2025-06-18","capabilities":{}}});
            state.sender.lock().unwrap().as_ref().unwrap().send(format!("data: {reply}\n\n")).unwrap();
        }
        axum::http::StatusCode::ACCEPTED
    }

    #[tokio::test]
    async fn legacy_sse_probe_round_trip() {
        let state = SseMock::default();
        let app = axum::Router::new().route("/mcp", axum::routing::get(mock_get))
            .route("/messages", axum::routing::post(mock_message)).with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = Url::parse(&format!("http://{}/mcp", listener.local_addr().unwrap())).unwrap();
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = reqwest::Client::builder().timeout(Duration::from_secs(5)).build().unwrap();
        probe_static_sse(&client, &url, Some("test-token"), &[], "2025-11-25").await.unwrap();
        assert_eq!(*state.methods.lock().unwrap(), vec!["initialize", "notifications/initialized"]);
        task.abort();
    }

    #[test]
    fn legacy_endpoint_stays_on_authenticated_origin() {
        let base = Url::parse("https://mcp.example.com/sse").unwrap();
        assert_eq!(endpoint_url(&base, "/messages?session=a").unwrap().path(), "/messages");
        for endpoint in ["https://evil.example/messages", "http://127.0.0.1/messages", "https://user@mcp.example.com/messages", "/messages#secret"] {
            assert!(endpoint_url(&base, endpoint).is_err());
        }
        let endpoint = endpoint_url(&base, "/messages?session=one").unwrap();
        assert!(checked_endpoint(&endpoint, &base, &base).is_ok());
        for changed in ["https://other.example.com/sse", "https://mcp.example.com/another-server"] {
            assert!(checked_endpoint(&endpoint, &base, &Url::parse(changed).unwrap()).is_err());
        }
        assert!(endpoint_event(b"event: endpoint\ndata: /messages"));
        assert!(!endpoint_event(b"event: other\ndata: /messages"));
    }
}

/// The connection UI probes before a connected record exists, so it uses the
/// already pinned client and static headers instead of the bridge's store.
pub(crate) async fn probe_static_sse(
    client: &reqwest::Client, url: &Url, bearer: Option<&str>, headers: &[(String, String)],
    version: &str,
) -> Result<(), String> {
    let decorate = |mut req: reqwest::RequestBuilder| {
        for (name, value) in headers { req = req.header(name, value); }
        if let Some(bearer) = bearer { req = req.bearer_auth(bearer); }
        req
    };
    let response = decorate(client.get(url.clone()).header("Accept", "text/event-stream"))
        .send().await.map_err(|_| "legacy MCP probe could not open an event stream")?;
    if !response.status().is_success() { return Err("legacy MCP probe stream refused".into()); }
    let mut stream = LegacyStream { response, buffer:Vec::new() };
    let mut total = 0usize;
    let endpoint = loop {
        let mut target = None;
        while let Some((event, rest)) = split_event(&stream.buffer) {
            stream.buffer = rest;
            if endpoint_event(&event) {
                target = Some(endpoint_url(url, &event_data(&event).ok_or("missing legacy endpoint data")?)?);
                break;
            }
        }
        if let Some(target) = target { break target; }
        let chunk = stream.response.chunk().await.map_err(|_| "legacy MCP probe stream failed")?
            .ok_or("legacy MCP probe ended before its endpoint")?;
        total = total.saturating_add(chunk.len());
        if total > MAX_MESSAGE_BYTES { return Err("legacy MCP probe too large".into()); }
        stream.buffer.extend_from_slice(&chunk);
    };
    let id = json!(format!("bridge-probe:{}", uuid::Uuid::new_v4()));
    let frame = json!({"jsonrpc":"2.0","id":id,"method":"initialize","params":{
        "protocolVersion":version,"capabilities":{},"clientInfo":{"name":"DuDuClaw","version":env!("CARGO_PKG_VERSION")}}});
    let response = decorate(client.post(endpoint.clone()).header("MCP-Protocol-Version", version).json(&frame))
        .send().await.map_err(|_| "legacy MCP initialize probe failed")?;
    if !response.status().is_success() { return Err("legacy MCP initialize probe refused".into()); }
    loop {
        while let Some((event, rest)) = split_event(&stream.buffer) {
            stream.buffer = rest;
            let Some(data) = event_data(&event) else { continue };
            let reply: Value = serde_json::from_str(&data).map_err(|_| "invalid legacy MCP probe JSON")?;
            if reply.get("id") == Some(&id) && reply.get("method").is_none() {
                let selected = reply["result"]["protocolVersion"].as_str().ok_or("legacy MCP probe did not initialize")?;
                if !["2025-11-25", "2025-06-18"].contains(&selected) { return Err("unsupported legacy MCP version".into()); }
                let initialized = json!({"jsonrpc":"2.0","method":"notifications/initialized","params":{}});
                let resp = decorate(client.post(endpoint).header("MCP-Protocol-Version", selected).json(&initialized))
                    .send().await.map_err(|_| "legacy MCP initialized probe failed")?;
                return if resp.status().is_success() { Ok(()) } else { Err("legacy MCP initialized probe refused".into()) };
            }
        }
        let chunk = stream.response.chunk().await.map_err(|_| "legacy MCP probe stream failed")?
            .ok_or("legacy MCP probe ended before its response")?;
        total = total.saturating_add(chunk.len());
        if total > MAX_MESSAGE_BYTES { return Err("legacy MCP probe too large".into()); }
        stream.buffer.extend_from_slice(&chunk);
    }
}
