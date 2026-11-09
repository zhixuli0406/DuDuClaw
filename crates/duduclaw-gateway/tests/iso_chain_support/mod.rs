//! Shared support for the `#[ignore]` root chain tests `iso_personal_chain`
//! (G2) and `iso_package_h_chain` (H15). They drive an INSTALLED protected
//! service on a disposable Linux host (`duduclaw service install
//! --protected-linux`): dashboard RPCs through
//! `scripts/live-test/linux-protected/connectors_rpc.py` (real account
//! sessions, TOTP step-up), employee tool calls through the operator's tool
//! driver (README "Tool driver contract"), and root-side observations of the
//! real spawnd (registry, `/proc`, units). Nothing here is a double.
//!
//! Environment (the H15 harness `package-h-acceptance.sh` sets all of it):
//! - `DUDUCLAW_ISO_DISPOSABLE=1` (else skip)
//! - `DUDUCLAW_ISO_DATA`        service home (default `/var/lib/duduclaw/.duduclaw`)
//! - `DUDUCLAW_ISO_STATE_DIR`   0700 dir with `admin.json` (claimed, TOTP-enrolled admin
//!                              session state written by `connectors_rpc.py`)
//! - `DUDUCLAW_ISO_RPC`         path of `connectors_rpc.py` (default: this checkout)
//! - `DUDUCLAW_ISO_TOOL_DRIVER` optional; checks that need a live employee call SKIP without it
//! - `DUDUCLAW_ISO_OUT`         optional evidence dir (RPC replies are written there)
//!
//! Result lines on stderr: `PASS|FAIL|SKIP <id> <detail>`; the test panics at
//! the end on any FAIL, never on a SKIP (a SKIP names what is missing).
#![allow(dead_code)]
use std::{fs, io::Read, os::unix::process::CommandExt, path::{Path, PathBuf}, process::{Command, Stdio},
    time::{Duration, Instant}};
use serde_json::{json, Value};

pub const REGISTRY: &str = "/var/lib/duduclaw-spawnd/registry.json";
pub const CONTROL_SOCKET: &str = "/run/duduclaw-spawnd/control.sock";
pub const ALLOCATION_SOCKET: &str = "/run/duduclaw-spawnd/allocation.sock";
const TOKEN_CHILD: &str = "DUDUCLAW_ISO_TOKEN_CHILD";

pub struct Env {
    pub data: PathBuf,
    pub rpc: PathBuf,
    pub state: PathBuf,
    pub driver: Option<PathBuf>,
    pub out: Option<PathBuf>,
    pub nonce: String,
    pub service: (u32, u32),
}

fn skip_all(test: &str, why: &str) -> Option<Env> {
    eprintln!("SKIP {test} {why}");
    None
}

/// `None` (after printing why) unless this is a prepared disposable root host
/// with the protected service and a reachable spawnd.
pub fn env_or_skip(test: &str) -> Option<Env> {
    if unsafe { libc::geteuid() } != 0 { return skip_all(test, "not root (run with sudo on the disposable host)"); }
    if std::env::var("DUDUCLAW_ISO_DISPOSABLE").as_deref() != Ok("1") {
        return skip_all(test, "DUDUCLAW_ISO_DISPOSABLE=1 not set (destructive: disposable hosts only)");
    }
    for socket in [CONTROL_SOCKET, ALLOCATION_SOCKET] {
        if !Path::new(socket).exists() && !unit_active("duduclaw-spawnd.socket") && !unit_active("duduclaw-spawnd.service") {
            return skip_all(test, &format!("spawnd missing: {socket} absent and duduclaw-spawnd not active"));
        }
    }
    if !Path::new(REGISTRY).is_file() { return skip_all(test, &format!("spawnd registry {REGISTRY} missing (not installed?)")); }
    let service = match duduclaw_spawnd::server::gateway_identity() {
        Ok(identity) => identity,
        Err(_) => return skip_all(test, "service account duduclaw missing (protected service not installed)"),
    };
    let data = std::env::var_os("DUDUCLAW_ISO_DATA").map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/var/lib/duduclaw/.duduclaw"));
    if !data.join("config.toml").is_file() { return skip_all(test, &format!("service home {} has no config.toml", data.display())); }
    let Some(state) = std::env::var_os("DUDUCLAW_ISO_STATE_DIR").map(PathBuf::from) else {
        return skip_all(test, "DUDUCLAW_ISO_STATE_DIR unset (needs admin.json from package-h-acceptance.sh / connectors_rpc.py)");
    };
    if !state.join("admin.json").is_file() { return skip_all(test, "no admin.json in DUDUCLAW_ISO_STATE_DIR"); }
    let rpc = std::env::var_os("DUDUCLAW_ISO_RPC").map(PathBuf::from).unwrap_or_else(||
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/live-test/linux-protected/connectors_rpc.py"));
    if !rpc.is_file() { return skip_all(test, &format!("connectors_rpc.py not found at {}", rpc.display())); }
    let driver = std::env::var_os("DUDUCLAW_ISO_TOOL_DRIVER").map(PathBuf::from).filter(|path| path.is_file());
    let out = std::env::var_os("DUDUCLAW_ISO_OUT").map(PathBuf::from).filter(|path| path.is_dir());
    let mut bytes = [0u8; 4];
    fs::File::open("/dev/urandom").and_then(|mut file| file.read_exact(&mut bytes)).expect("urandom");
    let nonce = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    Some(Env { data, rpc, state, driver, out, nonce, service })
}

pub fn unit_active(unit: &str) -> bool {
    Command::new("systemctl").args(["is-active", "--quiet", unit]).status().is_ok_and(|status| status.success())
}

// ── results ──────────────────────────────────────────────────────────────
#[derive(Default)]
pub struct Report { pub test: String, pass: usize, fail: Vec<String>, skip: usize }
impl Report {
    pub fn new(test: &str) -> Self { Self { test: test.into(), ..Default::default() } }
    pub fn pass(&mut self, id: &str, detail: impl AsRef<str>) { self.pass += 1; eprintln!("PASS {id} {}", detail.as_ref()); }
    pub fn fail(&mut self, id: &str, detail: impl AsRef<str>) {
        eprintln!("FAIL {id} {}", detail.as_ref());
        self.fail.push(format!("{id}: {}", detail.as_ref()));
    }
    pub fn skip(&mut self, id: &str, detail: impl AsRef<str>) { self.skip += 1; eprintln!("SKIP {id} {}", detail.as_ref()); }
    pub fn check(&mut self, id: &str, ok: bool, detail: impl AsRef<str>) {
        if ok { self.pass(id, detail) } else { self.fail(id, detail) }
    }
    pub fn finish(self) {
        eprintln!("RESULT {} PASS {} FAIL {} SKIP {}", self.test, self.pass, self.fail.len(), self.skip);
        assert!(self.fail.is_empty(), "{} failed checks:\n{}", self.test, self.fail.join("\n"));
    }
}

// ── dashboard RPC (connectors_rpc.py) ────────────────────────────────────
impl Env {
    fn python(&self, state: Option<&Path>, args: &[&str]) -> Value {
        let mut command = Command::new("python3");
        command.arg("-I").arg(&self.rpc).arg("--data-home").arg(&self.data);
        if let Some(state) = state { command.arg("--state").arg(state); }
        let output = command.args(args).env_clear().env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
            .env("HOME", &self.state).env("DUDUCLAW_HOME", &self.data).env("LC_ALL", "C")
            .stdin(Stdio::null()).output();
        match output {
            Ok(output) => serde_json::from_slice(&output.stdout).unwrap_or_else(|_| json!({"ok": false,
                "error": format!("unparseable connectors_rpc.py reply (exit {:?}): {}", output.status.code(),
                    String::from_utf8_lossy(&output.stderr).chars().take(300).collect::<String>())})),
            Err(error) => json!({"ok": false, "error": format!("python3 not runnable: {error}")}),
        }
    }
    pub fn state_file(&self, who: &str) -> PathBuf { self.state.join(format!("{who}.json")) }
    /// `call METHOD PARAMS [--stepup]` as `who` (state file `<who>.json`).
    pub fn call_as(&self, who: &str, method: &str, params: Value, stepup: bool) -> Value {
        let params = params.to_string();
        let mut args = vec!["call", method, params.as_str()];
        if stepup { args.push("--stepup"); }
        let reply = self.python(Some(&self.state_file(who)), &args);
        self.evidence(&format!("{who}-{method}"), &reply);
        reply
    }
    pub fn admin(&self, method: &str, params: Value) -> Value { self.call_as("admin", method, params, false) }
    /// Admin call with a fresh dashboard step-up; `"@csrf"` is filled in.
    pub fn admin_stepup(&self, method: &str, mut params: Value) -> Value {
        if let Some(object) = params.as_object_mut() { object.entry("csrf_token").or_insert(json!("@csrf")); }
        self.call_as("admin", method, params, true)
    }
    pub fn login_new(&self, who: &str, email: &str, password: &str) -> Value {
        self.python(Some(&self.state_file(who)), &["login", "--email", email, "--password", password])
    }
    fn evidence(&self, name: &str, value: &Value) {
        let Some(out) = &self.out else { return };
        let name: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect();
        let _ = fs::write(out.join(format!("{}-{name}.json", self.nonce)), value.to_string());
    }
}

pub fn ok(reply: &Value) -> bool { reply.get("ok").and_then(Value::as_bool) == Some(true) }
pub fn error_text(reply: &Value) -> String {
    reply.get("error").map(|error| error.as_str().map(str::to_owned).unwrap_or_else(|| error.to_string()))
        .unwrap_or_else(|| "(no error text)".into()).chars().take(200).collect()
}
/// An RPC the build does not implement (method unknown), as opposed to a refusal.
pub fn unimplemented(reply: &Value) -> bool {
    let text = error_text(reply).to_ascii_lowercase();
    !ok(reply) && (text.contains("unknown method") || text.contains("method not found") || text.contains("not implemented")
        || text.contains("-32601"))
}
/// First string (or number) found at any of the dotted `paths`.
pub fn field(value: &Value, paths: &[&str]) -> Option<String> {
    paths.iter().find_map(|path| {
        let mut node = value;
        for key in path.split('.') { node = node.get(key)?; }
        match node { Value::String(text) => Some(text.clone()), Value::Number(number) => Some(number.to_string()),
            Value::Bool(flag) => Some(flag.to_string()), _ => None }
    })
}
/// Every object below `value` (depth first).
pub fn objects(value: &Value) -> Vec<&serde_json::Map<String, Value>> {
    let mut found = Vec::new();
    let mut stack = vec![value];
    while let Some(node) = stack.pop() {
        match node {
            Value::Object(map) => { found.push(map); stack.extend(map.values()); }
            Value::Array(items) => stack.extend(items.iter()),
            _ => {}
        }
    }
    found
}
pub fn contains_text(value: &Value, needle: &str) -> bool { value.to_string().contains(needle) }

// ── employees ────────────────────────────────────────────────────────────
/// Nodes of `isolation.employee_list` for `label` (live and tombstoned).
pub fn employee_nodes(env: &Env, label: &str) -> Vec<Value> {
    let reply = env.admin("isolation.employee_list", json!({}));
    objects(&reply).into_iter().filter(|node| node.get("label").and_then(Value::as_str) == Some(label))
        .map(|node| Value::Object(node.clone())).collect()
}
pub fn is_live(node: &Value) -> bool {
    if let Some(flag) = node.get("disabled").and_then(Value::as_bool) { return !flag; }
    if let Some(flag) = node.get("tombstoned").and_then(Value::as_bool) { return !flag; }
    let state = field(node, &["state", "status", "lifecycle"]).unwrap_or_default().to_ascii_lowercase();
    !(state.contains("tombstone") || state.contains("disabled") || state.contains("quarantin") || state.contains("revoked"))
}
pub fn live_employee(env: &Env, label: &str) -> Option<Value> { employee_nodes(env, label).into_iter().find(is_live) }
pub fn employee_hex(node: &Value) -> Option<String> {
    field(node, &["employee_id", "employee", "id"]).filter(|hex| hex.len() == 32 && hex.chars().all(|c| c.is_ascii_hexdigit()))
}
pub fn incarnation(node: &Value) -> Option<u64> { field(node, &["incarnation"]).and_then(|value| value.parse().ok()) }

pub fn wait_live(env: &Env, label: &str, seconds: u64) -> Option<Value> {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    loop {
        if let Some(node) = live_employee(env, label) { return Some(node); }
        if Instant::now() > deadline { return None; }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// The open Employee proposal for `label` → approve (step-up) → applied.
pub fn approve_proposal(env: &Env, label: &str) -> Result<(), String> {
    let listing = env.admin("migration.confirmations.list", json!({}));
    let binding = objects(&listing).into_iter().find(|item| {
        let text = Value::Object((*item).clone()).to_string();
        text.contains(&format!("\"{label}\"")) && item.get("binding").is_some()
            && !text.contains("\"applied\"") && !text.contains("\"rejected\"")
    }).and_then(|item| item.get("binding").cloned()).ok_or_else(|| format!("no open proposal for {label}"))?;
    let reply = env.admin_stepup("migration.confirmations.approve", json!({"bindings": [binding]}));
    if ok(&reply) { Ok(()) } else { Err(format!("approve {label}: {}", error_text(&reply))) }
}

/// Staff request → admin proposal (step-up) → approval → root allocation.
pub fn create_personal(env: &Env, owner: &str, label: &str) -> Result<Value, String> {
    let reply = env.call_as(owner, "me.isolation.request_personal_employee",
        json!({"label": label, "display_name": label}), false);
    if !ok(&reply) { return Err(format!("request_personal_employee: {}", error_text(&reply))); }
    let request = field(&reply, &["payload.request_id", "payload.id", "payload.request.request_id", "payload.request.id"])
        .ok_or("no request id in the staff request reply")?;
    let reply = env.admin_stepup("isolation.employee_propose",
        json!({"label": label, "binding_kind": "personal", "request_id": request}));
    if !ok(&reply) { return Err(format!("employee_propose: {}", error_text(&reply))); }
    approve_proposal(env, label)?;
    wait_live(env, label, 120).ok_or_else(|| format!("{label} not confirmed within 120 s"))
}
pub fn create_shared(env: &Env, label: &str) -> Result<Value, String> {
    let reply = env.admin_stepup("isolation.employee_propose",
        json!({"label": label, "display_name": label, "binding_kind": "shared"}));
    if !ok(&reply) { return Err(format!("employee_propose shared: {}", error_text(&reply))); }
    approve_proposal(env, label)?;
    wait_live(env, label, 120).ok_or_else(|| format!("{label} not confirmed within 120 s"))
}
/// A fresh Active, logged-in staff account (state file `<who>.json`); returns its user id.
pub fn create_user(env: &Env, who: &str) -> Result<String, String> {
    let password = format!("Aa1-{}{}", env.nonce, who.len());
    let email = format!("{who}-{}@iso.test", env.nonce);
    let reply = env.admin("users.create", json!({"email": email, "display_name": who, "password": password, "role": "employee"}));
    if !ok(&reply) { return Err(format!("users.create {who}: {}", error_text(&reply))); }
    let reply = env.login_new(who, &email, &password);
    if !ok(&reply) { return Err(format!("login {who}: {}", error_text(&reply))); }
    field(&reply, &["payload.user_id", "payload.id"]).ok_or_else(|| format!("no user id for {who}"))
}

// ── root observations ────────────────────────────────────────────────────
pub fn parse_hex16(hex: &str) -> Option<[u8; 16]> {
    if hex.len() != 32 { return None; }
    let mut bytes = [0u8; 16];
    for (index, byte) in bytes.iter_mut().enumerate() { *byte = u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16).ok()?; }
    Some(bytes)
}
/// (uid, state) of the employee's spawnd account, any state.
pub fn registry_account(employee_hex: &str) -> Option<(u32, String)> {
    let id = duduclaw_isolation::ids::EmployeeId(parse_hex16(employee_hex)?);
    let registry: duduclaw_spawnd::registry::Registry = serde_json::from_slice(&fs::read(REGISTRY).ok()?).ok()?;
    registry.employees.iter().rev().find(|account| account.employee == id)
        .map(|account| (account.uid, format!("{:?}", account.state).to_ascii_lowercase()))
}
/// Every pid whose real/effective/saved/fs uid is `uid`.
pub fn uid_processes(uid: u32) -> Vec<u32> {
    let mut pids = Vec::new();
    let Ok(entries) = fs::read_dir("/proc") else { return pids };
    for entry in entries.flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|name| name.parse::<u32>().ok()) else { continue };
        let Ok(status) = fs::read_to_string(entry.path().join("status")) else { continue };
        let uids = status.lines().find_map(|line| line.strip_prefix("Uid:")).unwrap_or_default();
        if uids.split_whitespace().any(|value| value.parse::<u32>().ok() == Some(uid)) { pids.push(pid); }
    }
    pids
}
pub fn wait_no_processes(uid: u32, seconds: u64) -> Result<u128, Vec<u32>> {
    let start = Instant::now();
    loop {
        let pids = uid_processes(uid);
        if pids.is_empty() { return Ok(start.elapsed().as_millis()); }
        if start.elapsed() > Duration::from_secs(seconds) { return Err(pids); }
        std::thread::sleep(Duration::from_millis(100));
    }
}

// ── tool driver ──────────────────────────────────────────────────────────
impl Env {
    /// One JSON line from the operator's driver (`None` without a driver).
    pub fn driver(&self, args: &[&str]) -> Option<Value> {
        let driver = self.driver.as_ref()?;
        let output = Command::new(driver).args(args).env("DUDUCLAW_ISO_DATA", &self.data)
            .env("DUDUCLAW_ISO_RPC", &self.rpc).env("DUDUCLAW_ISO_STATE_DIR", &self.state)
            .stdin(Stdio::null()).output().ok()?;
        let line = String::from_utf8_lossy(&output.stdout).lines().last().unwrap_or_default().to_owned();
        let reply = serde_json::from_str(&line).unwrap_or_else(|_| json!({"outcome": "error", "raw": line}));
        self.evidence(&format!("driver-{}", args.first().copied().unwrap_or("call")), &reply);
        Some(reply)
    }
    pub fn tool(&self, label: &str, tool: &str, arguments: Value) -> Option<Value> {
        self.driver(&["call", label, tool, &arguments.to_string()])
    }
}
pub fn outcome(reply: &Value) -> String { field(reply, &["outcome"]).unwrap_or_else(|| "error".into()) }
pub fn code(reply: &Value) -> String { field(reply, &["code"]).unwrap_or_default() }

// ── agent token probe (runs as the gateway uid) ──────────────────────────
/// Token operations must run as the service account: the F6m index reader
/// requires the store owner to be the effective uid. The parent re-executes
/// this test binary (`--exact <child_test>`) with dropped privileges. The
/// token itself only ever lives in `file` (0600, owned by the service uid);
/// it is never printed.
pub fn token_probe(env: &Env, child_test: &str, mode: &str, label: &str, file: &Path) -> Value {
    let (uid, gid) = env.service;
    let mut command = Command::new(std::env::current_exe().expect("test binary"));
    command.args(["--ignored", "--exact", child_test, "--nocapture", "--test-threads=1"]).env_clear()
        .env(TOKEN_CHILD, "1").env("ISO_TOKEN_HOME", &env.data).env("ISO_TOKEN_LABEL", label)
        .env("ISO_TOKEN_FILE", file).env("ISO_TOKEN_MODE", mode).env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null()).stderr(Stdio::inherit());
    // SAFETY: only async-signal-safe calls between fork and exec.
    unsafe { command.pre_exec(move || {
        if libc::setgroups(0, std::ptr::null()) != 0 || libc::setresgid(gid, gid, gid) != 0
            || libc::setresuid(uid, uid, uid) != 0 { return Err(std::io::Error::last_os_error()); }
        Ok(())
    }); }
    let Ok(output) = command.output() else { return json!({"error": "token child not runnable"}) };
    String::from_utf8_lossy(&output.stdout).lines().find_map(|line| line.strip_prefix("TOKEN_PROBE "))
        .and_then(|line| serde_json::from_str(line).ok())
        .unwrap_or_else(|| json!({"error": format!("token child exit {:?}", output.status.code())}))
}
/// A 0700 directory owned by the service account for token files.
pub fn token_dir(env: &Env) -> PathBuf {
    let dir = env.state.join(format!("tokens-{}", env.nonce));
    fs::create_dir_all(&dir).expect("token dir");
    let path = std::ffi::CString::new(dir.as_os_str().as_encoded_bytes()).unwrap();
    unsafe {
        libc::chown(path.as_ptr(), env.service.0, env.service.1);
        libc::chmod(path.as_ptr(), 0o700);
    }
    // The state dir itself must be traversable by the service account.
    let state = std::ffi::CString::new(env.state.as_os_str().as_encoded_bytes()).unwrap();
    unsafe { libc::chmod(state.as_ptr(), 0o711); }
    dir
}

/// Child half of [`token_probe`]; call from a `#[test] #[ignore]` whose name
/// does NOT start with `iso_root_` so the root filter never selects it alone.
pub fn token_child_main() {
    if std::env::var(TOKEN_CHILD).as_deref() != Ok("1") { return; }
    let get = |name: &str| std::env::var_os(name).map(PathBuf::from).expect("token child env");
    let (home, file) = (get("ISO_TOKEN_HOME"), get("ISO_TOKEN_FILE"));
    let label = std::env::var("ISO_TOKEN_LABEL").unwrap();
    let mode = std::env::var("ISO_TOKEN_MODE").unwrap();
    let reply = match duduclaw_core::load_identity_key(&home) {
        None => json!({"error": "identity key unreadable as the service account"}),
        Some(key) => {
            let current = duduclaw_core::current_agent_token(&home, &key, &label);
            match mode.as_str() {
                "capture" => match &current {
                    Some(token) => {
                        use std::os::unix::fs::OpenOptionsExt;
                        let written = fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600)
                            .open(&file).and_then(|mut out| std::io::Write::write_all(&mut out, token.as_bytes()));
                        json!({"captured": written.is_ok(), "v2": token.starts_with("v2.")})
                    }
                    None => json!({"captured": false, "current": "none (tombstoned or unreadable)"}),
                },
                _ => {
                    let saved = fs::read_to_string(&file).unwrap_or_default();
                    json!({"verifies": !saved.is_empty() && duduclaw_core::verify_agent_token(&home, &key, &label, &saved),
                        "current_differs": current.as_deref() != Some(saved.as_str()), "current_exists": current.is_some()})
                }
            }
        }
    };
    println!("TOKEN_PROBE {reply}");
}
