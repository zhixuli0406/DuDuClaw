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

/// The gateway's placeholder id in an in-run request: the real id is spawnd's
/// call id, which the helper reads from its launcher-made scratch directory.
pub const PLACEHOLDER_ID: &str = "00000000000000000000000000000000";
/// Signed helper-table files, in fixed-argv digest order (HX3b: flat names
/// under `/helpers`).
pub const BROWSER_FILES: [&str; 3] = ["node", "playwright.cjs", "chromium"];
/// EX_CONFIG: the browser's nested sandbox could not be installed here.
pub const EXIT_SANDBOX_UNSUPPORTED: i32 = 78;

/// EX_TEMPFAIL (security fix round, finding 2): after the kill a browser
/// process was still alive or not every descendant was reaped (`waitpid(-1)`
/// never reached `ECHILD`); nothing is published, the gateway stores
/// `uncertain/browser_tree_survived`.
pub const EXIT_BROWSER_SURVIVED: i32 = 75;
/// The driver's only probe output; the helper answers spawnd with
/// `ipc::recording_probe_reply(<call>)`.
#[cfg(target_os = "linux")]
const DRIVER_PROBE_OK: &[u8] = br#"{"probe":"rendered"}"#;

/// spawnd's call id from the in-run scratch `/helper-tmp/<call>`.
pub fn scratch_id(scratch: &std::path::Path) -> Result<&str, String> {
    let id = scratch.file_name().and_then(|name| name.to_str()).filter(|id| valid_id(id))
        .ok_or("invalid recording scratch")?;
    if id == PLACEHOLDER_ID { return Err("invalid recording scratch".into()); }
    Ok(id)
}

/// The in-run request with spawnd's call id. `scratch` must be the in-run
/// layout's `/helper-tmp/<call>`; the gateway never chooses the id.
pub fn in_run_request(request: Request, scratch: &std::path::Path) -> Result<Request, String> {
    if request.id != PLACEHOLDER_ID { return Err("in-run recording ids come from spawnd".into()); }
    let id = scratch_id(scratch)?;
    let request = Request { id: id.to_owned(), ..request };
    request.validate()?;
    Ok(request)
}

/// Security fix round, finding 4: the sealed input is spawnd's fixed probe
/// request (never a recording request: `Request::decode` refuses it).
pub fn is_probe(input: &[u8]) -> bool { input == duduclaw_isolation::ipc::RECORDING_PROBE_REQUEST }

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
    // Finding 4: spawnd's per-host probe runs this same path (layout, signed
    // files, forwarder, nested sandbox) on a fixed local page and publishes nothing.
    let probe = is_probe(&bytes);
    let request = if probe { None } else {
        let Ok(request) = Request::decode(&bytes) else { return 64; };
        let Ok(request) = in_run_request(request, &layout.scratch) else { return 64; };
        Some(request)
    };
    let Ok(call) = scratch_id(&layout.scratch).map(str::to_owned) else { return 64; };
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
    let encoded = match &request { Some(request) => match request.encode() { Ok(bytes) => Some(bytes), Err(_) => return 70 }, None => None };
    let mut written: Vec<(&str, &[u8])> = vec![("driver.cjs", DRIVER.as_bytes())];
    if let Some(encoded) = &encoded { written.push(("request.json", encoded.as_slice())); }
    for (name, bytes) in written {
        let Ok(mut file) = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(staging.join(name)) else { return 74; };
        if file.write_all(bytes).is_err() { return 74; }
    }
    // [F10] Prepared before any fork; the browser never starts without them:
    // the nested Landlock, the nested seccomp filter (finding 1) and, so that
    // every orphaned browser process is re-parented to us and can be reaped
    // before publication, the child-subreaper attribute (finding 2).
    let file_refs: Vec<&str> = files.iter().map(String::as_str).collect();
    let Ok(layer) = crate::recording_sandbox::BrowserLayer::prepare(&layout.scratch, &file_refs, port) else {
        return EXIT_SANDBOX_UNSUPPORTED;
    };
    let Ok(filter) = crate::recording_seccomp::BrowserFilter::prepare() else { return EXIT_SANDBOX_UNSUPPORTED; };
    if become_subreaper().is_err() { return EXIT_SANDBOX_UNSUPPORTED; }
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build() else { return 70; };
    let Some(node) = files.first() else { return 77; };
    let browser = Browser { node, staging: &staging, proxy: &proxy, layer: &layer, filter: &filter, probe };
    let max_seconds = request.as_ref().map_or(PROBE_SECONDS, |request| request.max_seconds);
    let result = match runtime.block_on(run_driver(max_seconds, &browser)) {
        Ok(result) => result,
        Err(DriverError::Sandbox) => return EXIT_SANDBOX_UNSUPPORTED,
        Err(DriverError::Survived) => return EXIT_BROWSER_SURVIVED,
        Err(DriverError::Failed) => return 74,
    };
    let Some(request) = request else {
        // Probe: root reads exactly this line; nothing was published.
        let reply = duduclaw_isolation::ipc::recording_probe_reply(&call);
        return if std::io::stdout().lock().write_all(reply.as_bytes()).is_err() { 74 } else { 0 };
    };
    // `run_driver` returns only after every browser process is dead and
    // reaped (no group member left AND `waitpid(-1)` = ECHILD), so nothing
    // the browser runs can race the publication into `/work/recordings`.
    let (status, artifacts) = if result["started"] == false { ("not_recorded", vec![]) } else {
        let Ok(artifacts) = crate::recording_io::publish(std::path::Path::new(crate::helper_layout::RECORDINGS), &staging, &request.id) else { return 74; };
        ("recorded", artifacts)
    };
    let result = json!({"id":request.id,"status":status,"reason":result["reason"],"actions":result["actions"],"artifacts":artifacts,"navigation":result["navigation"]});
    let Ok(bytes) = serde_json::to_vec(&result) else { return 70; };
    if decode_result(&bytes).is_err() || std::io::stdout().lock().write_all(&bytes).is_err() { return 74; }
    0
}
#[cfg(not(target_os = "linux"))]
pub fn main_entry() -> i32 { 64 }

/// Probe deadline (seconds) inside the helper; root's watchdog bounds it too.
#[cfg(target_os = "linux")]
const PROBE_SECONDS: u64 = 60;

#[cfg(target_os = "linux")]
struct Browser<'a> {
    node: &'a str,
    staging: &'a std::path::Path,
    proxy: &'a str,
    layer: &'a crate::recording_sandbox::BrowserLayer,
    filter: &'a crate::recording_seccomp::BrowserFilter,
    probe: bool,
}

/// Why the browser run produced nothing publishable.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DriverError {
    /// The nested sandbox could not be entered (Landlock, seccomp, group).
    Sandbox,
    /// A browser process outlived the kill, or a descendant was not reaped.
    Survived,
    /// The driver failed or its output was invalid.
    Failed,
}

/// Finding 2: every orphaned descendant (Chromium's zygote, renderers,
/// crash handler) is re-parented to this helper, not to the run's init, so
/// `waitpid(-1)` can prove the whole browser tree is gone. Needs the signed
/// F_H `prctl` option `PR_SET_CHILD_SUBREAPER` (a self-only option).
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
fn become_subreaper() -> Result<(), ()> {
    // SAFETY: sets an attribute of this process only.
    if unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) } != 0 { return Err(()); }
    let mut value: libc::c_int = 0;
    // SAFETY: the kernel writes one int into `value`.
    if unsafe { libc::prctl(libc::PR_GET_CHILD_SUBREAPER, &mut value as *mut libc::c_int, 0, 0, 0) } != 0 || value != 1 {
        return Err(());
    }
    Ok(())
}

/// What `waitpid(-1, WNOHANG)` says about our remaining children.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Children { None, Alive, Unknown }

/// Reap every exited child (bounded); `None` only on `ECHILD`.
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
fn reap_children() -> Children {
    for _ in 0..4096 {
        // SAFETY: non-blocking reap of any child of this process.
        let pid = unsafe { libc::waitpid(-1, std::ptr::null_mut(), libc::WNOHANG) };
        if pid > 0 { continue; }
        if pid == 0 { return Children::Alive; }
        match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::ECHILD) => return Children::None,
            Some(libc::EINTR) => continue,
            _ => return Children::Unknown,
        }
    }
    Children::Unknown
}

/// SIGKILL the browser's process group, reap, and wait (bounded) until no
/// group member is left AND this subreaper has no child at all. The nested
/// filter refuses `setsid`/`setpgid`, so no browser process can leave the
/// group; a process that survives anyway (or an unreapable child) makes this
/// `Err`, which keeps the artifacts unpublished.
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
async fn kill_and_reap(pgid: i32) -> Result<(), ()> {
    if pgid <= 1 { return Err(()); }
    for _ in 0..100 {
        // SAFETY: signals only our own child's process group.
        unsafe { libc::kill(-pgid, libc::SIGKILL); }
        // Zombies still count as group members until they are reaped.
        let children = reap_children();
        if children == Children::Unknown { return Err(()); }
        // SAFETY: existence probe of the same group.
        let group_gone = unsafe { libc::kill(-pgid, 0) } != 0
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
        if group_gone && children == Children::None { return Ok(()); }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    Err(())
}

/// The driver's result line: the probe's fixed reply, or the recording summary.
#[cfg(target_os = "linux")]
fn driver_result(bytes: &[u8], probe: bool) -> Result<Value, ()> {
    if probe { return if bytes == DRIVER_PROBE_OK { Ok(json!({"probe":"rendered"})) } else { Err(()) }; }
    let value: Value = serde_json::from_slice(bytes).map_err(|_| ())?;
    let fields = value.as_object().ok_or(())?;
    if fields.len() != 4 || fields.keys().any(|key| !matches!(key.as_str(), "reason" | "actions" | "started" | "navigation"))
        || !matches!(value["navigation"].as_str(), Some("not_attempted" | "completed" | "failed" | "interrupted"))
        || value["started"].as_bool().is_none()
        || (value["started"] == false && (value["reason"] != "stop" || value["actions"] != 0 || value["navigation"] != "not_attempted"))
        || !matches!(value["reason"].as_str(), Some("stop" | "deadline"))
        || value["actions"].as_u64().is_none_or(|n| n > 1000) { return Err(()); }
    Ok(value)
}

#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
async fn run_driver(max_seconds: u64, browser: &Browser<'_>) -> Result<Value, DriverError> {
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
    if browser.probe { command.env("DUDUCLAW_RECORD_PROBE", "1"); }
    let ruleset = browser.layer.raw();
    let filter = browser.filter.raw();
    // Runs in the forked child before exec: setpgid, the two Landlock calls
    // and one seccomp call, allocating nothing (an OS-error `io::Error` is a
    // plain code). Every failure reports ENOTSUP so the parent can tell a
    // sandbox failure (exit 78) from an exec failure.
    let in_child = move || -> std::io::Result<()> {
        let sandbox = || std::io::Error::from_raw_os_error(libc::ENOTSUP);
        // SAFETY: own process; a new process group so the whole browser tree can be killed.
        if unsafe { libc::setpgid(0, 0) } != 0 { return Err(sandbox()); }
        crate::recording_sandbox::enforce_raw(ruleset).map_err(|_| sandbox())?;
        // Last: from here on `setsid`/`setpgid` are EPERM for the browser tree.
        crate::recording_seccomp::install_raw(filter).map_err(|_| sandbox())
    };
    // SAFETY: the closure is async-signal-safe (see above); `filter` and
    // `ruleset` stay valid until `spawn` returned (`browser` outlives it).
    unsafe { command.pre_exec(in_child); }
    let mut child = command.spawn().map_err(|error|
        if error.raw_os_error() == Some(libc::ENOTSUP) { DriverError::Sandbox } else { DriverError::Failed })?;
    let pgid = child.id().and_then(|pid| i32::try_from(pid).ok()).ok_or(DriverError::Failed)?;
    let mut bytes = vec![];
    let limit = u64::try_from(MAX_RESULT).map_err(|_| DriverError::Failed)?.saturating_add(1);
    let result = match child.stdout.take() {
        None => Err(()),
        Some(output) => {
            let mut output = output.take(limit);
            let deadline = Duration::from_secs(max_seconds.saturating_add(15));
            match tokio::time::timeout(deadline, async {
                let (read, exit) = tokio::join!(output.read_to_end(&mut bytes), child.wait());
                if read.is_err() || !exit.map_err(|_| ())?.success() || bytes.len() > MAX_RESULT { return Err(()); }
                driver_result(&bytes, browser.probe)
            }).await {
                Ok(result) => result,
                Err(_) => Err(()),
            }
        }
    };
    if result.is_err() { let _ = child.kill().await; let _ = child.wait().await; }
    // Chromium's renderers/zygote outlive node unless the whole group dies;
    // finding 2: and none may be left unreaped (subreaper + ECHILD).
    kill_and_reap(pgid).await.map_err(|()| DriverError::Survived)?;
    result.map_err(|()| DriverError::Failed)
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
    /// Finding 4: spawnd's probe input is never a recording request, and the
    /// probe reply is never a recording result the gateway would store.
    #[test]
    fn hx6fix_probe_input_and_reply_never_cross_the_recording_protocol() {
        let probe = duduclaw_isolation::ipc::RECORDING_PROBE_REQUEST;
        assert!(is_probe(probe));
        assert!(Request::decode(probe).is_err());
        let request = Request::from_arguments(&json!({"url":"https://example.com","headless":true}), PLACEHOLDER_ID).unwrap();
        assert!(!is_probe(&request.encode().unwrap()));
        let id = "0123456789abcdef0123456789abcdef";
        let reply = duduclaw_isolation::ipc::recording_probe_reply(id);
        assert!(decode_result(reply.as_bytes()).is_err());
        assert_eq!(scratch_id(std::path::Path::new(&format!("/helper-tmp/{id}"))), Ok(id));
        assert!(scratch_id(std::path::Path::new(&format!("/helper-tmp/{PLACEHOLDER_ID}"))).is_err());
        assert_eq!(EXIT_BROWSER_SURVIVED, 75);
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn hx6fix_probe_driver_output_is_exact() {
        assert_eq!(driver_result(br#"{"probe":"rendered"}"#, true), Ok(json!({"probe":"rendered"})));
        let bad_outputs: [&[u8]; 4] = [br#"{"probe":"rendered","x":1}"#, br#"{"probe":"failed"}"#, b"",
            br#"{"reason":"stop","actions":0,"started":false,"navigation":"not_attempted"}"#];
        for bad in bad_outputs {
            assert!(driver_result(bad, true).is_err());
        }
        assert!(driver_result(br#"{"probe":"rendered"}"#, false).is_err());
        assert!(driver_result(br#"{"reason":"stop","actions":0,"started":false,"navigation":"not_attempted"}"#, false).is_ok());
    }
    /// Finding 2: as child subreaper, an orphaned grandchild becomes our child,
    /// and `reap_children` reports `None` (ECHILD) only once it is gone.
    /// Runs in a forked child: the subreaper attribute is process-wide.
    #[cfg(target_os = "linux")]
    #[test]
    #[allow(unsafe_code)]
    fn hx6fix_subreaper_reaps_orphans_before_echild() {
        // SAFETY: the child only issues syscalls and `_exit`s.
        let child = unsafe { libc::fork() };
        assert!(child >= 0);
        if child == 0 {
            let ok = (|| {
                become_subreaper().ok()?;
                // SAFETY: as above; the middle process exits at once, the grandchild after 300 ms.
                let middle = unsafe { libc::fork() };
                if middle == 0 {
                    if unsafe { libc::fork() } == 0 { std::thread::sleep(std::time::Duration::from_millis(300)); }
                    unsafe { libc::_exit(0) }
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
                // The orphaned grandchild is re-parented to us: still a child.
                if reap_children() != Children::Alive { return None; }
                std::thread::sleep(std::time::Duration::from_millis(500));
                (reap_children() == Children::None).then_some(())
            })().is_some();
            unsafe { libc::_exit(if ok { 0 } else { 1 }) }
        }
        let mut status = 0;
        // SAFETY: waits for the forked child above.
        assert_eq!(unsafe { libc::waitpid(child, &mut status, 0) }, child);
        assert!(libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0, "{status}");
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
