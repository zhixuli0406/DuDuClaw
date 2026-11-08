//! Fixed employee-side headless recording protocol. No host desktop or PATH.
use serde_json::{Value, json};

pub const RUNTIME: &str = "duduclaw-browser-record-v1";
pub const ARGUMENT: &str = "--browser-record-fd-v1";
pub const MAX_REQUEST: usize = 32 * 1024;
pub const MAX_RESULT: usize = 64 * 1024;
pub const MAX_ARTIFACT: u64 = 16 * 1024 * 1024;
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
pub fn valid_id(id: &str) -> bool { id.len() == 32 && id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) }

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
    for (artifact, name) in artifacts.iter().zip(["trace.zip", "actions.json", "network.json"]) {
        let fields = artifact.as_object().ok_or("invalid artifact")?;
        if fields.len() != 3 || fields.keys().any(|key| !matches!(key.as_str(), "name" | "bytes" | "sha256"))
            || artifact["name"] != name || artifact["bytes"].as_u64().is_none_or(|n| n == 0 || n > MAX_ARTIFACT)
            || !artifact["sha256"].as_str().is_some_and(crate::program_helper::valid_digest) { return Err("invalid artifact manifest".into()); }
    }
    Ok(value)
}

/// The gateway's placeholder id in an in-run request: the real id is spawnd's
/// call id, which the helper reads from its launcher-made scratch directory.
pub const PLACEHOLDER_ID: &str = "00000000000000000000000000000000";
/// Signed helper-table files, in fixed-argv digest order (HX3b: flat names
/// under `/helpers`).
pub const BROWSER_FILES: [&str; 3] = ["node", "playwright.cjs", "chromium"];
/// EX_CONFIG: the browser's nested sandbox could not be installed here.
pub const EXIT_SANDBOX_UNSUPPORTED: i32 = 78;

/// The in-run request with spawnd's call id. `scratch` must be the in-run
/// layout's `/helper-tmp/<call>`; the gateway never chooses the id.
pub fn in_run_request(request: Request, scratch: &std::path::Path) -> Result<Request, String> {
    if request.id != PLACEHOLDER_ID { return Err("in-run recording ids come from spawnd".into()); }
    let id = scratch.file_name().and_then(|name| name.to_str()).filter(|id| valid_id(id))
        .ok_or("invalid recording scratch")?;
    if id == PLACEHOLDER_ID { return Err("invalid recording scratch".into()); }
    let request = Request { id: id.to_owned(), ..request };
    request.validate()?;
    Ok(request)
}

/// `HTTPS_PROXY` must be the parent's forwarder: `http://127.0.0.1:<port>/`.
pub fn forwarder_port(proxy: &str) -> Option<u16> {
    let url = url::Url::parse(proxy).ok()?;
    if url.scheme() != "http" || url.host_str() != Some("127.0.0.1") || url.path() != "/" || url.query().is_some()
        || url.fragment().is_some() || !url.username().is_empty() || url.password().is_some() { return None; }
    url.port().filter(|port| *port != 0)
}

#[cfg(target_os = "linux")]
pub fn main_entry() -> i32 {
    use std::{io::Write, os::unix::fs::{DirBuilderExt, OpenOptionsExt}};
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    // The exact root-signed argv pins Node, the self-contained Playwright
    // bundle and Chromium. The driver is compiled into this signed helper.
    if arguments.len() != 4 || arguments[0] != ARGUMENT || arguments.iter().skip(1).any(|v| !crate::program_helper::valid_digest(v)) { return 64; }
    let args = arguments.iter().map(String::as_str).collect::<Vec<_>>();
    // HX6: in-run only (D8 revision §4). The launcher made `/helper-tmp/<call>`
    // our cwd = HOME = TMPDIR; the legacy child-run layout (`/runtime`, `/tmp`)
    // is refused, so no recording scratch is ever reachable by the runtime.
    let Ok(layout) = crate::helper_layout::current() else { return 77; };
    if !layout.in_run() { return 77; }
    let Ok(bytes) = crate::program_helper::helper_input_args(&args, MAX_REQUEST) else { return 77; };
    let Ok(request) = Request::decode(&bytes) else { return 64; };
    let Ok(request) = in_run_request(request, &layout.scratch) else { return 64; };
    let mut files = Vec::with_capacity(BROWSER_FILES.len());
    for (name, digest) in BROWSER_FILES.into_iter().zip(arguments.iter().skip(1)) {
        let Ok(path) = layout.file(name) else { return 77; };
        if crate::program_helper::verify_runtime_file(&path, digest).is_err() { return 77; }
        files.push(path);
    }
    let Ok(proxy) = std::env::var("HTTPS_PROXY") else { return 77; };
    let Some(port) = forwarder_port(&proxy) else { return 77; };
    let staging = layout.staging("recording");
    if std::fs::DirBuilder::new().mode(0o700).create(&staging).is_err() { return 74; }
    let Ok(encoded) = request.encode() else { return 70; };
    for (name, bytes) in [("driver.cjs", DRIVER.as_bytes()), ("request.json", encoded.as_slice())] {
        let Ok(mut file) = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(staging.join(name)) else { return 74; };
        if file.write_all(bytes).is_err() { return 74; }
    }
    // [F10] Prepared before any fork; the browser never starts without it.
    let file_refs: Vec<&str> = files.iter().map(String::as_str).collect();
    let Ok(layer) = crate::recording_sandbox::BrowserLayer::prepare(&layout.scratch, &file_refs, port) else {
        return EXIT_SANDBOX_UNSUPPORTED;
    };
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build() else { return 70; };
    let browser = Browser { node: &files[0], staging: &staging, proxy: &proxy, layer: &layer };
    let Ok(result) = runtime.block_on(run_driver(&request, &browser)) else { return 74; };
    // `run_driver` returns only after the whole browser process group is gone,
    // so nothing the browser runs can race the publication into `/work`.
    let (status, artifacts) = if result["started"] == false { ("not_recorded", vec![]) } else {
        let Ok(artifacts) = crate::recording_io::publish(std::path::Path::new(crate::helper_layout::WORKSPACE), &staging, &request.id) else { return 74; };
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
struct Browser<'a> {
    node: &'a str,
    staging: &'a std::path::Path,
    proxy: &'a str,
    layer: &'a crate::recording_sandbox::BrowserLayer,
}

/// SIGKILL the browser's process group and wait (bounded) until no member is
/// left. `Err` keeps the artifacts unpublished.
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
async fn kill_group(pgid: i32) -> Result<(), ()> {
    if pgid <= 1 { return Err(()); }
    for _ in 0..100 {
        // SAFETY: signals only our own child's process group.
        unsafe { libc::kill(-pgid, libc::SIGKILL); }
        // SAFETY: existence probe of the same group.
        if unsafe { libc::kill(-pgid, 0) } != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    Err(())
}

#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
async fn run_driver(request: &Request, browser: &Browser<'_>) -> Result<Value, ()> {
    use std::{process::Stdio, time::Duration};
    use tokio::io::AsyncReadExt;
    let driver = browser.staging.join("driver.cjs");
    let mut command = tokio::process::Command::new(browser.node);
    command.arg("--no-addons").arg(&driver).env_clear()
        .env("HOME", browser.staging).env("TMPDIR", browser.staging).env("PATH", "/usr/bin:/bin")
        .env("DUDUCLAW_RECORD_PROXY", browser.proxy).env("DUDUCLAW_RECORD_DIR", browser.staging)
        .env("DUDUCLAW_RECORD_ROOT", crate::helper_layout::IN_RUN_ROOT).env("DUDUCLAW_RECORD_CONTAINED", "1")
        .current_dir(browser.staging)
        .stdin(Stdio::inherit()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true);
    let ruleset = browser.layer.raw();
    // SAFETY: runs in the forked child before exec; it issues setpgid and the
    // two Landlock syscalls only, allocating nothing.
    let in_child = move || -> std::io::Result<()> {
        // SAFETY: own process; a new process group so the whole browser tree can be killed.
        if unsafe { libc::setpgid(0, 0) } != 0 { return Err(std::io::Error::last_os_error()); }
        crate::recording_sandbox::enforce_raw(ruleset)
    };
    unsafe { command.pre_exec(in_child); }
    let mut child = command.spawn().map_err(|_| ())?;
    let pgid = child.id().and_then(|pid| i32::try_from(pid).ok()).ok_or(())?;
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
    let result = match result { Ok(result) => result, Err(_) => { let _ = child.kill().await; let _ = child.wait().await; Err(()) } };
    // Chromium's renderers/zygote outlive node unless the whole group dies.
    kill_group(pgid).await?;
    result
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
    /// HX6: the in-run id is spawnd's call id from `/helper-tmp/<call>`; the
    /// gateway's request carries only the placeholder; the proxy is exactly
    /// the parent's loopback forwarder.
    #[test]
    fn recording_helper_in_run_id_comes_from_scratch_and_proxy_is_the_forwarder() {
        let arguments = json!({"url":"https://example.com","headless":true});
        let placeholder = Request::from_arguments(&arguments, PLACEHOLDER_ID).unwrap();
        let scratch = std::path::Path::new("/helper-tmp/0123456789abcdef0123456789abcdef");
        assert_eq!(in_run_request(placeholder.clone(), scratch).unwrap().id, "0123456789abcdef0123456789abcdef");
        let chosen = Request::from_arguments(&arguments, &"ab".repeat(16)).unwrap();
        assert!(in_run_request(chosen, scratch).is_err(), "the gateway never picks the id");
        for bad in ["/helper-tmp/ABC", "/helper-tmp/00000000000000000000000000000000", "/helper-tmp"] {
            assert!(in_run_request(placeholder.clone(), std::path::Path::new(bad)).is_err(), "{bad}");
        }
        assert_eq!(forwarder_port("http://127.0.0.1:3128/"), Some(3128));
        for bad in ["http://127.0.0.1/", "http://127.0.0.1:0/", "https://127.0.0.1:3128/", "http://localhost:3128/",
            "http://10.0.0.1:3128/", "http://u:p@127.0.0.1:3128/", "http://127.0.0.1:3128/x"] {
            assert_eq!(forwarder_port(bad), None, "{bad}");
        }
        assert_eq!(BROWSER_FILES, ["node", "playwright.cjs", "chromium"]);
        // Chromium flags of the contained driver (pipe transport, no /dev/shm).
        assert!(DRIVER.contains("--disable-dev-shm-usage") && DRIVER.contains("--js-flags=--jitless"));
        assert!(!DRIVER.contains("--remote-debugging-port"));
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
