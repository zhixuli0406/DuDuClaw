//! Runs only inside an employee run. No persistent files or host credentials.
use std::{time::Duration};
use duduclaw_api_worker::{Request, Response, MAX_INPUT, MAX_RESPONSE, MAX_ROUNDS, ARGUMENTS};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
type Result<T> = std::result::Result<T, &'static str>;

struct Bridge {
    _child: tokio::process::Child,
    input: tokio::process::ChildStdin,
    output: BufReader<tokio::process::ChildStdout>,
    next: u64,
}
impl Bridge {
    async fn connect() -> Result<Self> {
        // fd 3 is deliberately inherited by the fixed, root-published shim.
        // No employee/environment-selected executable or arguments are used.
        let mut child = tokio::process::Command::new("/runtime/duduclaw-mcp-bridge")
            .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null()).env_clear().kill_on_drop(true)
            .spawn().map_err(|_| "MCP bridge unavailable")?;
        let input = child.stdin.take().ok_or("MCP input unavailable")?;
        let output = BufReader::new(child.stdout.take().ok_or("MCP output unavailable")?);
        let mut bridge = Self { _child: child, input, output, next: 1 };
        bridge.call("initialize", json!({"protocolVersion":"2025-06-18","capabilities":{},
            "clientInfo":{"name":"duduclaw-api-worker","version":env!("CARGO_PKG_VERSION")}})).await?;
        bridge.write(json!({"jsonrpc":"2.0","method":"notifications/initialized"})).await?;
        Ok(bridge)
    }
    async fn write(&mut self, value: Value) -> Result<()> {
        let mut bytes = serde_json::to_vec(&value).map_err(|_| "MCP request invalid")?;
        if bytes.len() > 256 * 1024 { return Err("MCP request too large"); }
        bytes.push(b'\n');
        self.input.write_all(&bytes).await.map_err(|_| "MCP write failed")
    }
    async fn frame(&mut self) -> Result<Value> {
        let mut bytes = Vec::new();
        loop {
            let chunk = self.output.fill_buf().await.map_err(|_| "MCP read failed")?;
            if chunk.is_empty() { return Err("MCP disconnected"); }
            let newline = chunk.iter().position(|b| *b == b'\n');
            let n = newline.unwrap_or(chunk.len());
            if n > MAX_RESPONSE.saturating_sub(bytes.len()) { return Err("MCP response too large"); }
            bytes.extend_from_slice(&chunk[..n]);
            self.output.consume(n + usize::from(newline.is_some()));
            if newline.is_some() { return serde_json::from_slice(&bytes).map_err(|_| "MCP response invalid"); }
        }
    }
    async fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next; self.next += 1;
        self.write(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})).await?;
        for _ in 0..8 {
            let response = self.frame().await?;
            if response.get("jsonrpc").and_then(Value::as_str) != Some("2.0") { return Err("MCP response invalid"); }
            if response.get("id").is_none() && response.get("method").and_then(Value::as_str).is_some() { continue; }
            if response.get("id").and_then(Value::as_u64) != Some(id) || response.get("error").is_some() {
                return Err("MCP request refused");
            }
            return response.get("result").cloned().ok_or("MCP result missing");
        }
        Err("MCP notification limit")
    }
}


pub async fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().map(String::as_str).ne(ARGUMENTS.iter().copied())
        || !(70000..=99999).contains(&rustix::process::geteuid().as_raw()) { return Err("worker launch refused"); }
    let bytes = duduclaw_api_worker::sealed::sealed_request()?;
    let mut stdin = Vec::new();
    tokio::time::timeout(Duration::from_secs(30), tokio::io::stdin().take(MAX_INPUT as u64 + 1).read_to_end(&mut stdin))
        .await.map_err(|_| "input timeout")?.map_err(|_| "input read failed")?;
    if bytes != stdin { return Err("input mismatch"); }
    let request: Request = serde_json::from_slice(&bytes).map_err(|_| "request invalid")?;
    request.validate()?;
    let mut config = Vec::new();
    tokio::fs::File::open("/config/mcp.json").await.map_err(|_| "MCP projection unavailable")?
        .take(16385).read_to_end(&mut config).await.map_err(|_| "MCP projection unavailable")?;
    if config.len() > 16384 { return Err("MCP projection invalid"); }
    duduclaw_api_worker::verify_config(&config)?;
    let response = tokio::time::timeout(Duration::from_secs(request.timeout_secs), execute(request))
        .await.map_err(|_| "API execution timeout")??;
    let mut output = serde_json::to_vec(&response).map_err(|_| "response invalid")?;
    if output.len() > MAX_RESPONSE { return Err("response too large"); }
    output.push(b'\n');
    tokio::io::stdout().write_all(&output).await.map_err(|_| "output failed")
}

async fn execute(request: Request) -> Result<Response> {
    // Parse only inside this employee's uid/namespace. Never read gateway
    // config, inherited API-key variables or a host-selected credential path.
    let mut bytes = Vec::new();
    tokio::fs::File::open("/home/employee/.duduclaw-api/providers.json").await
        .map_err(|_| "employee credentials unavailable")?.take(65537).read_to_end(&mut bytes).await
        .map_err(|_| "employee credentials unavailable")?;
    if bytes.len() > 65536 { return Err("employee credentials too large"); }
    let credentials: duduclaw_api_worker::Credentials = serde_json::from_slice(&bytes)
        .map_err(|_| "employee credentials invalid")?;
    let credential = credentials.select(request.provider.as_deref())?;
    // The run has only namespace loopback. reqwest uses B's fixed proxy env;
    // no direct gateway-uid HTTP path exists. Redirects never forward credentials.
    let client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10)).timeout(Duration::from_secs(request.timeout_secs))
        .build().map_err(|_| "HTTP transport unavailable")?;
    let mut bridge = Bridge::connect().await?;
    let tools = duduclaw_api_worker::provider_tools(&bridge.call("tools/list", json!({})).await?)?;
    let mut messages: Vec<Value> = request.messages.iter().map(|m| json!({"role":m.role,"content":m.content})).collect();
    let mut input_tokens = 0u64;
    let mut output_tokens = 0u64;
    for _ in 0..MAX_ROUNDS {
        let body = credential.protocol.body(&request.model, &messages, &tools, request.max_tokens)?;
        let encoded = serde_json::to_vec(&body).map_err(|_| "provider request invalid")?;
        if encoded.len() > 16 * 1024 * 1024 { return Err("conversation too large"); }
        let provider = async {
            let endpoint = credential.protocol.endpoint(&credential.base_url, &request.model)?;
            let http = client.post(endpoint).header("Content-Type", "application/json");
            let http = match credential.protocol {
                duduclaw_api_worker::vendor::Protocol::Anthropic => http.header("x-api-key", &credential.api_key).header("anthropic-version", "2023-06-01"),
                duduclaw_api_worker::vendor::Protocol::Gemini => http.header("x-goog-api-key", &credential.api_key),
                _ => http.bearer_auth(&credential.api_key),
            };
            let mut response = http.body(encoded).send().await.map_err(|_| "provider transport failed")?;
            if !response.status().is_success() { return Err("provider refused request"); }
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|_| "provider response failed")? {
                if chunk.len() > MAX_RESPONSE.saturating_sub(bytes.len()) { return Err("provider response too large"); }
                bytes.extend_from_slice(&chunk);
            }
            serde_json::from_slice::<Value>(&bytes).map_err(|_| "provider response invalid")
        };
        // D closes its endpoint on revocation/expiry. Do not continue a paid
        // provider request after that close, or accept an answer without a live
        // capability. Unexpected notifications conservatively abort this run.
        let response = tokio::select! {
            result = provider => result?,
            _ = bridge.frame() => return Err("MCP authority changed"),
        };
        let response = credential.protocol.normalize(response)?;
        let (input, output, step) = duduclaw_api_worker::provider_step(&response, &tools)?;
        input_tokens = input_tokens.saturating_add(input);
        output_tokens = output_tokens.saturating_add(output);
        match step {
            duduclaw_api_worker::Step::Answer(content) => {
                // Recheck authority even when the provider chose no tools.
                bridge.call("tools/list", json!({})).await?;
                return Ok(Response { content, input_tokens, output_tokens });
            }
            duduclaw_api_worker::Step::Tools { assistant, calls } => {
                messages.push(assistant);
                let mut retained = messages.iter().map(|m| m.to_string().len()).sum::<usize>();
                for call in calls {
                    let result = bridge.call("tools/call", json!({"name":call.name,"arguments":call.arguments})).await?;
                    let content = serde_json::to_string(&result).map_err(|_| "tool result invalid")?;
                    retained = retained.saturating_add(content.len());
                    if retained > 16 * 1024 * 1024 { return Err("conversation too large"); }
                    messages.push(json!({"role":"tool","tool_call_id":call.id,"content":content}));
                }
            }
        }
    }
    Err("tool round limit reached")
}
