//! Fixed employee-side headless recording protocol. No host desktop or PATH.
use serde_json::{Value, json};

pub const RUNTIME: &str = "duduclaw-browser-record-v1";
pub const ARGUMENT: &str = "--browser-record-fd-v1";
pub const MAX_REQUEST: usize = 32 * 1024;
pub const MAX_RESULT: usize = 64 * 1024;
pub const MAX_ARTIFACT: u64 = duduclaw_isolation::recording_layout::MAX_ARTIFACT_BYTES;
pub const DRIVER: &str = include_str!("recording_driver.cjs");

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request { pub id: String, pub url: String, pub name: String, pub max_seconds: u64 }
impl Request {
    pub fn from_arguments(arguments: &Value, id: &str) -> Result<Self, String> {
        let fields = arguments.as_object().ok_or("invalid recording request")?;
        if fields.keys().any(|key| !matches!(key.as_str(), "url" | "name" | "headless" | "max_seconds" | "workspace"))
            || fields.get("headless").and_then(Value::as_bool) != Some(true)
            || fields.get("workspace").is_some_and(|v| v.as_str() != Some("employee-work")) {
            return Err("protected recording requires explicit headless mode".into());
        }
        let request = Self { id: id.into(), url: arguments["url"].as_str().ok_or("missing URL")?.into(),
            name: match fields.get("name") { None => "recording".into(), Some(v) => v.as_str().ok_or("invalid name")?.into() },
            max_seconds: match fields.get("max_seconds") { None => 30, Some(v) => v.as_u64().ok_or("invalid deadline")? } };
        request.validate()?;
        Ok(request)
    }
    fn validate(&self) -> Result<(), String> {
        if !valid_id(&self.id) || self.name.len() > 200 || self.name.chars().any(char::is_control)
            || !(1..=300).contains(&self.max_seconds) || self.url.len() > 8192 {
            return Err("invalid recording bounds".into());
        }
        let url = url::Url::parse(&self.url).map_err(|_| "invalid recording URL")?;
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none()
            || !url.username().is_empty() || url.password().is_some() || url.fragment().is_some()
            || self.url.chars().any(char::is_control) { return Err("invalid recording URL".into()); }
        // Reachability and private-address denial are enforced by C's bound
        // forwarder, including redirects/DNS. URL parsing is not that policy.
        Ok(())
    }
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        self.validate()?;
        serde_json::to_vec(&json!({"id":self.id,"url":self.url,"name":self.name,"max_seconds":self.max_seconds}))
            .map_err(|_| "recording encoding failed".into())
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_REQUEST { return Err("recording request too large".into()); }
        let value: Value = serde_json::from_slice(bytes).map_err(|_| "invalid recording JSON")?;
        let fields = value.as_object().ok_or("invalid recording request")?;
        if fields.len() != 4 || fields.keys().any(|key| !matches!(key.as_str(), "id" | "url" | "name" | "max_seconds")) {
            return Err("invalid recording fields".into());
        }
        let request = Self { id: value["id"].as_str().ok_or("invalid id")?.into(), url: value["url"].as_str().ok_or("invalid URL")?.into(),
            name: value["name"].as_str().ok_or("invalid name")?.into(), max_seconds: value["max_seconds"].as_u64().ok_or("invalid deadline")? };
        request.validate()?; Ok(request)
    }
}
pub fn valid_id(id: &str) -> bool { duduclaw_isolation::recording_layout::valid_id(id) }

pub fn decode_result(bytes: &[u8]) -> Result<Value, String> {
    if bytes.len() > MAX_RESULT { return Err("recording result too large".into()); }
    let value: Value = serde_json::from_slice(bytes).map_err(|_| "invalid recording result")?;
    let fields = value.as_object().ok_or("invalid recording result")?;
    if fields.len() != 6 || fields.keys().any(|key| !matches!(key.as_str(), "id" | "status" | "reason" | "actions" | "artifacts" | "navigation"))
        || !value["id"].as_str().is_some_and(valid_id) || !matches!(value["status"].as_str(), Some("recorded" | "not_recorded"))
        || !matches!(value["reason"].as_str(), Some("stop" | "deadline"))
        || value["actions"].as_u64().is_none_or(|n| n > 1000)
        || !matches!(value["navigation"].as_str(), Some("not_attempted" | "completed" | "failed" | "interrupted")) { return Err("invalid recording result fields".into()); }
    let artifacts = value["artifacts"].as_array().ok_or("missing artifacts")?;
    if value["status"] == "not_recorded" {
        if !artifacts.is_empty() || value["reason"] != "stop" || value["actions"] != 0 || value["navigation"] != "not_attempted" { return Err("invalid early stop result".into()); }
        return Ok(value);
    }
    if artifacts.len() != 3 { return Err("invalid artifact count".into()); }
    for (artifact, name) in artifacts.iter().zip(duduclaw_isolation::recording_layout::ARTIFACTS) {
        let fields = artifact.as_object().ok_or("invalid artifact")?;
        if fields.len() != 3 || fields.keys().any(|key| !matches!(key.as_str(), "name" | "bytes" | "sha256"))
            || artifact["name"] != name || artifact["bytes"].as_u64().is_none_or(|n| n == 0 || n > MAX_ARTIFACT)
            || !artifact["sha256"].as_str().is_some_and(crate::program_helper::valid_digest) { return Err("invalid artifact manifest".into()); }
    }
    Ok(value)
}

#[cfg(target_os = "linux")]
pub fn main_entry() -> i32 {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    // The exact root-signed argv pins Node, the self-contained Playwright
    // bundle and Chromium. The driver is compiled into this signed helper.
    if arguments.len() != 4 || arguments[0] != ARGUMENT || arguments.iter().skip(1).any(|v| !crate::program_helper::valid_digest(v)) { return 64; }
    let args = arguments.iter().map(String::as_str).collect::<Vec<_>>();
    let Ok(bytes) = crate::program_helper::helper_input_args(&args, MAX_REQUEST) else { return 77; };
    let Ok(request) = Request::decode(&bytes) else { return 64; };
    for (path, digest) in ["/runtime/node", "/runtime/playwright.cjs", "/runtime/chromium"].into_iter().zip(arguments.iter().skip(1)) {
        if crate::program_helper::verify_runtime_file(path, digest).is_err() { return 77; }
    }
    let Ok(proxy) = std::env::var("HTTPS_PROXY") else { return 77; };
    let Ok(proxy_url) = url::Url::parse(&proxy) else { return 77; };
    if proxy_url.scheme() != "http" || proxy_url.host_str() != Some("127.0.0.1") || proxy_url.port().is_none()
        || proxy_url.path() != "/" || proxy_url.query().is_some() || proxy_url.fragment().is_some()
        || !proxy_url.username().is_empty() || proxy_url.password().is_some() { return 77; }
    let temporary = std::path::Path::new("/tmp/recording");
    if std::fs::create_dir(temporary).is_err() { return 74; }
    for (name, bytes) in [("driver.cjs", DRIVER.as_bytes()), ("request.json", bytes.as_slice())] {
        let Ok(mut file) = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(temporary.join(name)) else { return 74; };
        if file.write_all(bytes).is_err() { return 74; }
    }
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build() else { return 70; };
    let Ok(result) = runtime.block_on(run_driver(&request, &proxy)) else { return 74; };
    let (status, artifacts) = if result["started"] == false { ("not_recorded", vec![]) } else {
        let Ok(artifacts) = crate::recording_io::publish(std::path::Path::new("/work"), temporary, &request.id) else { return 74; };
        ("recorded", artifacts)
    };
    let result = json!({"id":request.id,"status":status,"reason":result["reason"],"actions":result["actions"],"artifacts":artifacts,"navigation":result["navigation"]});
    let Ok(bytes) = serde_json::to_vec(&result) else { return 70; };
    if decode_result(&bytes).is_err() || std::io::stdout().lock().write_all(&bytes).is_err() { return 74; }
    0
}
#[cfg(not(target_os = "linux"))]
pub fn main_entry() -> i32 { 64 }

#[cfg(target_os = "linux")]
async fn run_driver(request: &Request, proxy: &str) -> Result<Value, ()> {
    use std::{process::Stdio, time::Duration};
    use tokio::io::AsyncReadExt;
    let mut command = tokio::process::Command::new("/runtime/node");
    command.args(["--no-addons", "/tmp/recording/driver.cjs"]).env_clear()
        .env("HOME", "/tmp/recording").env("TMPDIR", "/tmp").env("PATH", "/usr/bin:/bin")
        .env("DUDUCLAW_RECORD_PROXY", proxy).current_dir("/tmp/recording")
        .stdin(Stdio::inherit()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true);
    let mut child = command.spawn().map_err(|_| ())?;
    let output = child.stdout.take().ok_or(())?;
    let mut output = output.take(MAX_RESULT as u64 + 1);
    let mut bytes = vec![];
    let result = tokio::time::timeout(Duration::from_secs(request.max_seconds + 15), async {
        let (read, exit) = tokio::join!(output.read_to_end(&mut bytes), child.wait());
        if read.is_err() || !exit.map_err(|_| ())?.success() || bytes.len() > MAX_RESULT { return Err(()); }
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| ())?;
        let fields = value.as_object().ok_or(())?;
        if fields.len() != 4 || fields.keys().any(|key| !matches!(key.as_str(), "reason" | "actions" | "started" | "navigation"))
            || !matches!(value["navigation"].as_str(), Some("not_attempted" | "completed" | "failed" | "interrupted"))
            || value["started"].as_bool().is_none()
            || (value["started"] == false && (value["reason"] != "stop" || value["actions"] != 0 || value["navigation"] != "not_attempted"))
            || !matches!(value["reason"].as_str(), Some("stop" | "deadline"))
            || value["actions"].as_u64().is_none_or(|n| n > 1000) { return Err(()); }
        Ok(value)
    }).await;
    match result { Ok(result) => result, Err(_) => { let _ = child.kill().await; let _ = child.wait().await; Err(()) } }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recording_helper_request_requires_headless_and_rejects_authority() {
        let arguments = json!({"url":"https://example.com/demo","headless":true,"name":"範例"});
        let request = Request::from_arguments(&arguments, &"ab".repeat(16)).unwrap();
        assert_eq!(Request::decode(&request.encode().unwrap()).unwrap(), request);
        for field in ["id", "cwd", "root", "run", "env", "proxy", "browser", "argv"] {
            let mut value = arguments.clone(); value[field] = json!("forged");
            assert!(Request::from_arguments(&value, &request.id).is_err(), "{field}");
        }
        for url in ["file:///etc/passwd", "https://u:p@example.com", "data:text/plain,x", "https://example.com/#secret"] {
            let mut value = arguments.clone(); value["url"] = json!(url);
            assert!(Request::from_arguments(&value, &request.id).is_err());
        }
        for seconds in [0, 301, u64::MAX] {
            let mut value = arguments.clone(); value["max_seconds"] = json!(seconds);
            assert!(Request::from_arguments(&value, &request.id).is_err());
        }
        for headless in [Value::Null, json!(false), json!("true")] {
            let mut value = arguments.clone(); value["headless"] = headless;
            assert!(Request::from_arguments(&value, &request.id).is_err());
        }
        assert!(Request::from_arguments(&arguments, "../outside").is_err());
    }
    #[test]
    fn recording_helper_result_rejects_incomplete_or_forged_artifacts() {
        let artifacts = ["trace.zip", "actions.json", "network.json"].map(|name|
            json!({"name":name,"bytes":1,"sha256":"a".repeat(64)}));
        let good = json!({"id":"ab".repeat(16),"status":"recorded","reason":"stop","actions":0,"artifacts":artifacts,"navigation":"completed"});
        let encode = |v: &Value| serde_json::to_vec(v).unwrap();
        assert!(decode_result(&encode(&good)).is_ok());
        let early = json!({"id":"ab".repeat(16),"status":"not_recorded","reason":"stop","actions":0,"artifacts":[],"navigation":"not_attempted"});
        assert!(decode_result(&encode(&early)).is_ok());
        let mut forged_early = early; forged_early["reason"] = json!("deadline");
        assert!(decode_result(&encode(&forged_early)).is_err());
        for changed in [json!({"status":"recorded"}), {
            let mut v = good.clone(); v["artifacts"][0]["name"] = json!("../trace.zip"); v
        }, { let mut v = good.clone(); v["actions"] = json!(1001); v }, {
            let mut v = good.clone(); v["artifacts"][0]["bytes"] = json!(MAX_ARTIFACT + 1); v
        }] { assert!(decode_result(&encode(&changed)).is_err()); }
    }
}
