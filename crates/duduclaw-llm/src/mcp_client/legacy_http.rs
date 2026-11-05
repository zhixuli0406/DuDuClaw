//! Deprecated HTTP+SSE fallback. The event endpoint is confined to the exact
//! configured origin before any bearer is sent there.
use super::*;
use tokio::sync::mpsc;

pub(super) struct LegacyHttp {
    pub endpoint: String,
    receiver: mpsc::Receiver<Value>,
    reader: tokio::task::JoinHandle<()>,
}
impl Drop for LegacyHttp { fn drop(&mut self) { self.reader.abort(); } }
impl LegacyHttp {
    pub async fn response(&mut self, id: i64) -> Result<Value, McpError> {
        while let Some(frame) = self.receiver.recv().await {
            if frame.get("method").is_some() && frame.get("id").is_some() {
                return Err(McpError::Parse("unsolicited MCP server request".into()));
            }
            if frame["id"].as_i64() == Some(id) { return Ok(frame); }
        }
        Err(McpError::Closed)
    }
}

fn pop_event(buffer: &mut Vec<u8>) -> Result<Option<String>, McpError> {
    let Some((index, length)) = buffer.windows(4).position(|w| w == b"\r\n\r\n").map(|i| (i, 4))
        .into_iter().chain(buffer.windows(2).position(|w| w == b"\n\n").map(|i| (i, 2)))
        .min_by_key(|(index, _)| *index) else { return Ok(None) };
    let event = std::str::from_utf8(&buffer[..index])
        .map_err(|_| McpError::Parse("invalid legacy SSE UTF-8".into()))?.to_string();
    buffer.drain(..index+length);
    Ok(Some(event))
}
fn event_data(event: &str) -> String {
    event.lines().filter_map(|line| line.strip_prefix("data:")
        .map(|line| line.strip_prefix(' ').unwrap_or(line))).collect::<Vec<_>>().join("\n")
}

impl McpClient {
    pub(super) async fn open_legacy_http(&mut self) -> Result<(), McpError> {
        self.validate_scoped_authority().await?;
        let McpTransport::Http { http, url, headers, scoped, legacy, .. } = &mut self.transport else {
            return Err(McpError::Parse("not an HTTP connection".into()));
        };
        if legacy.is_some() { return Err(McpError::Parse("legacy SSE fallback already attempted".into())); }
        let mut request = http.get(url.as_str()).header("Accept", "text/event-stream");
        for (name, value) in headers.iter() {
            if !protocol_header(name) { request = request.header(name, value); }
        }
        if let Some(authority) = scoped.as_ref() {
            request = request.header(reqwest::header::AUTHORIZATION, bearer_header(&authority.bearer)?);
        }
        let mut response = request.send().await.map_err(|_| McpError::Io("legacy SSE connection failed".into()))?;
        if !response.status().is_success() { return Err(McpError::HttpStatus(response.status().as_u16())); }
        if response.headers().get("content-type").and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(';').next()) != Some("text/event-stream") {
            return Err(McpError::Parse("legacy endpoint is not SSE".into()));
        }
        let mut buffer = Vec::new();
        let endpoint = loop {
            if let Some(event) = pop_event(&mut buffer)? {
                if event.lines().any(|line| line.strip_prefix("event:").is_some_and(|v| v.trim() == "endpoint")) {
                    break legacy_endpoint(url, &event_data(&event))?;
                }
                continue;
            }
            let chunk = response.chunk().await.map_err(|_| McpError::Io("legacy endpoint stream interrupted".into()))?
                .ok_or(McpError::Closed)?;
            if buffer.len() + chunk.len() > MAX_HTTP_BODY_BYTES as usize { return Err(McpError::Parse("legacy SSE event too large".into())); }
            buffer.extend_from_slice(&chunk);
        };
        let (sender, receiver) = mpsc::channel(32);
        let reader = tokio::spawn(async move {
            loop {
                loop {
                    let event = match pop_event(&mut buffer) {
                        Ok(Some(event)) => event,
                        Ok(None) => break,
                        Err(_) => return,
                    };
                    if let Ok(frame) = serde_json::from_str(&event_data(&event)) {
                        if sender.send(frame).await.is_err() { return; }
                    }
                }
                let Ok(Some(chunk)) = response.chunk().await else { return; };
                if buffer.len() + chunk.len() > MAX_HTTP_BODY_BYTES as usize { return; }
                buffer.extend_from_slice(&chunk);
            }
        });
        *legacy = Some(LegacyHttp { endpoint, receiver, reader });
        self.validate_scoped_authority().await
    }
}

pub(super) fn bearer_header(bearer: &SecretBearer) -> Result<reqwest::header::HeaderValue, McpError> {
    let token = bearer.as_bytes();
    if token.is_empty() || token.iter().any(|b| !b.is_ascii() || b.is_ascii_control() || b.is_ascii_whitespace()) {
        return Err(McpError::Io("invalid scoped MCP bearer".into()));
    }
    let mut bytes = b"Bearer ".to_vec();
    bytes.extend_from_slice(token);
    let value = reqwest::header::HeaderValue::from_bytes(&bytes);
    bytes.fill(0);
    let mut value = value.map_err(|_| McpError::Io("invalid scoped MCP bearer".into()))?;
    value.set_sensitive(true);
    Ok(value)
}

fn legacy_endpoint(url: &str, endpoint: &str) -> Result<String, McpError> {
    let base = reqwest::Url::parse(url).map_err(|_| McpError::Parse("invalid MCP URL".into()))?;
    let target = base.join(endpoint).map_err(|_| McpError::Parse("invalid legacy endpoint".into()))?;
    if target.origin() != base.origin() || !target.username().is_empty()
        || target.password().is_some() || target.fragment().is_some() {
        return Err(McpError::Parse("legacy endpoint changed credential origin".into()));
    }
    Ok(target.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_endpoint_never_changes_credential_origin() {
        let base = "https://example.com/mcp";
        assert_eq!(legacy_endpoint(base, "/messages?session=1").unwrap(), "https://example.com/messages?session=1");
        for endpoint in ["https://evil.example/messages", "http://example.com/messages",
            "https://example.com:444/messages", "//user:pass@example.com/messages", "/messages#fragment"] {
            assert!(legacy_endpoint(base, endpoint).is_err(), "{endpoint}");
        }
    }

    #[test]
    fn legacy_sse_preserves_fragmented_unicode_and_rejects_invalid_utf8() {
        let mut buffer = "data: 台灣".as_bytes().to_vec();
        assert!(pop_event(&mut buffer).unwrap().is_none());
        buffer.extend_from_slice(b"\n\ndata: second\r\n\r\n");
        assert_eq!(pop_event(&mut buffer).unwrap().as_deref(), Some("data: 台灣"));
        assert_eq!(pop_event(&mut buffer).unwrap().as_deref(), Some("data: second"));
        assert!(pop_event(&mut vec![0xff, b'\n', b'\n']).is_err());
    }
}
