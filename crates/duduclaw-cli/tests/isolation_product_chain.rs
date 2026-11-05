//! Real installed gateway -> account login -> WebChat -> Claude -> product task tool.
//! No test authority, mocked producer, direct handler call or fabricated token.
#![cfg(target_os = "linux")]

use std::{collections::BTreeMap, fs::{self, File, OpenOptions}, io::Read,
    net::{Ipv4Addr, SocketAddr, TcpStream}, os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Component, Path, PathBuf}, process::{Command, Stdio}, time::{Duration, Instant}};
use async_trait::async_trait;
use duduclaw_tui::{auth::{AuthClient, LoginMode, LoginPrompt, MfaMethod, PromptField},
    chat::Submission, client::{Client, ClientOptions, ConnectionState}, transport::GatewayUrl};
use rusqlite::{Connection, OpenFlags, params};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, Zeroizing};

const UNIT: &str = "duduclaw-gateway.service";
const HOME: &str = "/var/lib/duduclaw/.duduclaw";
const TOOL: &str = "mcp__duduclaw__tasks_create";
const DEADLINE: Duration = Duration::from_secs(240);

// Private operator-provisioned input. Never Debug/Serialize or print its fields.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Setup {
    port: u16,
    gateway_uid: u32,
    employee_uid: u32,
    agent: String,
    user_id: String,
    gateway_sha256: String,
    claude_sha256: String,
    bridge_sha256: String,
    email: String,
    password: String,
    mfa_recovery_code: Option<String>,
}
impl Drop for Setup {
    fn drop(&mut self) {
        self.email.zeroize(); self.password.zeroize();
        if let Some(code) = &mut self.mfa_recovery_code { code.zeroize(); }
    }
}
fn setup() -> Setup {
    assert_eq!(std::env::var("DUDUCLAW_ISO_DISPOSABLE").as_deref(), Ok("1"),
        "requires an explicitly disposable, idle, provisioned VM");
    assert_eq!(unsafe { libc::geteuid() }, 0, "root observer required");
    duduclaw_tui::auth::disable_core_dumps().expect("disable test core dumps before reading credentials");
    let path = PathBuf::from(std::env::var_os("ISO_PRODUCT_SETUP_FILE")
        .expect("set ISO_PRODUCT_SETUP_FILE to a private root-owned JSON file"));
    assert!(path.is_absolute());
    let mut ancestor = PathBuf::new();
    for part in path.components() {
        assert!(matches!(part, Component::RootDir | Component::Normal(_)));
        ancestor.push(part.as_os_str());
        let meta = fs::symlink_metadata(&ancestor).expect("setup ancestor exists");
        assert!(!meta.file_type().is_symlink() && meta.uid() == 0 && meta.mode() & 0o022 == 0,
            "setup path must have trusted non-writable ancestors");
    }
    let mut file = OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path).expect("open private setup");
    let meta = file.metadata().unwrap();
    assert!(meta.is_file() && meta.nlink() == 1 && meta.uid() == 0 && meta.mode() & 0o777 == 0o600
        && meta.len() <= 16384, "setup must be a bounded root-only regular file");
    let mut bytes = Zeroizing::new(Vec::new());
    file.by_ref().take(16385).read_to_end(&mut bytes).unwrap();
    assert!(bytes.len() <= 16384);
    let input: Setup = serde_json::from_slice(&bytes).unwrap_or_else(|_| panic!("invalid setup schema"));
    assert!(input.port >= 1024 && input.gateway_uid > 0 && input.employee_uid >= 70000
        && input.gateway_uid != input.employee_uid);
    assert!(duduclaw_core::is_valid_agent_id(&input.agent)
        && !duduclaw_core::is_reserved_agent_id(&input.agent));
    assert!(!input.user_id.is_empty() && !input.email.is_empty() && !input.password.is_empty());
    for value in [&input.gateway_sha256, &input.claude_sha256, &input.bridge_sha256] {
        assert!(value.len() == 64 && value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "expected signed release digests must be canonical SHA-256");
    }
    assert_ne!(input.claude_sha256, input.bridge_sha256);
    input
}

fn systemctl(args: &[&str]) -> String {
    let output = Command::new("/usr/bin/timeout").args(["20s", "/usr/bin/systemctl"])
        .args(args).env_clear().env("PATH", "/usr/bin:/bin").env("LANG", "C")
        .stdin(Stdio::null()).output().expect("run bounded systemctl");
    assert!(output.status.success(), "systemctl prerequisite/lifecycle operation failed (output withheld)");
    assert!(output.stdout.len() <= 65536, "bounded systemctl response");
    String::from_utf8(output.stdout).expect("systemctl property encoding")
}
fn properties() -> BTreeMap<String, String> {
    systemctl(&["show", UNIT, "--property=ActiveState,SubState,MainPID,InvocationID,ControlGroup,User,Restart"])
        .lines().filter_map(|line| line.split_once('=').map(|(k, v)| (k.into(), v.into()))).collect()
}
fn hash(path: &Path) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let mut digest = Sha256::new();
    let mut bytes = [0; 65536];
    loop {
        let count = file.read(&mut bytes).ok()?;
        if count == 0 { break; }
        digest.update(&bytes[..count]);
    }
    Some(hex::encode(digest.finalize()))
}

struct ServiceGuard { invocation: Option<String>, previous_invocation: String }
impl ServiceGuard {
    fn start(input: &Setup) -> (Self, u32) {
        let before = properties();
        assert_eq!(before.get("ActiveState").map(String::as_str), Some("inactive"),
            "never take over an existing service");
        assert_eq!(before.get("MainPID").map(String::as_str), Some("0"));
        assert_eq!(before.get("Restart").map(String::as_str), Some("no"),
            "acceptance provisioning must disable automatic restart before this test");
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, input.port));
        assert!(TcpStream::connect_timeout(&addr, Duration::from_millis(100)).is_err(),
            "gateway port is already owned");
        // The root-owned installed unit supplies identity, protection and keys.
        // No test process environment or provider credential is handed to it.
        // Arm before the first lifecycle mutation, including start timeouts.
        // The disposable VM is exclusively assigned to this test: no other
        // operator may start/replace this fixed unit during the invocation.
        let mut guard = Self { invocation: None,
            previous_invocation: before.get("InvocationID").cloned().unwrap_or_default() };
        systemctl(&["start", UNIT]);
        let after = properties();
        let invocation = after.get("InvocationID").cloned().unwrap_or_default();
        assert!(invocation.len() == 32 && invocation.bytes().all(|b| b.is_ascii_hexdigit()),
            "new service invocation identity required");
        assert_ne!(invocation, guard.previous_invocation, "a fresh invocation is required");
        guard.invocation = Some(invocation);
        let pid: u32 = after.get("MainPID").expect("gateway pid").parse().unwrap();
        assert!(pid > 1 && after.get("ActiveState").map(String::as_str) == Some("active"));
        assert_eq!(hash(Path::new(&format!("/proc/{pid}/exe"))).as_deref(), Some(input.gateway_sha256.as_str()),
            "running gateway must be the independently verified installed release");
        assert_uid(pid, input.gateway_uid);
        (guard, pid)
    }
}
impl Drop for ServiceGuard {
    fn drop(&mut self) {
        // No force/restart or kill-by-UID. A replacement invocation belongs to
        // someone else; refuse cleanup rather than stop that process.
        let observed = std::panic::catch_unwind(properties);
        let Some(current) = observed.as_ref().ok().and_then(|p| p.get("InvocationID")) else {
            eprintln!("acceptance cleanup cannot read invocation; inspect/dispose the exclusive VM");
            return;
        };
        // If start/show panicked before capture, recover only a fresh, valid
        // invocation on this exclusively test-owned, previously inactive unit.
        let expected = match &self.invocation {
            Some(expected) => expected,
            None if current != &self.previous_invocation && current.len() == 32
                && current.bytes().all(|b| b.is_ascii_hexdigit()) => current,
            None => {
                eprintln!("acceptance cleanup found no identifiable new invocation; inspect disposable VM");
                return;
            }
        };
        let confirmed = std::panic::catch_unwind(properties);
        if current != expected
            || confirmed.as_ref().ok().and_then(|p| p.get("InvocationID")) != Some(expected) {
            eprintln!("acceptance cleanup refused: gateway invocation changed; inspect disposable VM");
            return;
        }
        if std::panic::catch_unwind(|| systemctl(&["stop", UNIT])).is_err() {
            eprintln!("acceptance gateway stop failed; inspect disposable VM");
        }
    }
}
fn assert_uid(pid: u32, expected: u32) {
    let text = fs::read_to_string(format!("/proc/{pid}/status")).expect("live proc status");
    let values: Vec<u32> = text.lines().find_map(|line| line.strip_prefix("Uid:")).unwrap()
        .split_whitespace().map(|value| value.parse().unwrap()).collect();
    assert_eq!(values, vec![expected; 4], "actual process UID mismatch");
}

struct Login<'a> { setup: &'a mut Setup }
#[async_trait]
impl LoginPrompt for Login<'_> {
    async fn read(&mut self, field: PromptField) -> duduclaw_tui::Result<Zeroizing<String>> {
        let value = match field {
            PromptField::Email => std::mem::take(&mut self.setup.email),
            PromptField::Password => std::mem::take(&mut self.setup.password),
            PromptField::RecoveryCode => self.setup.mfa_recovery_code.take().ok_or(duduclaw_tui::Error::MfaFailed)?,
            _ => return Err(duduclaw_tui::Error::MfaFailed),
        };
        if value.is_empty() { return Err(duduclaw_tui::Error::LoginRequired); }
        Ok(Zeroizing::new(value))
    }
    async fn mfa_method(&mut self) -> duduclaw_tui::Result<MfaMethod> { Ok(MfaMethod::RecoveryCode) }
}
fn readonly(name: &str) -> Connection {
    Connection::open_with_flags(Path::new(HOME).join(name), OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("existing product database must be readable")
}

#[derive(Default)]
struct ProcessEvidence { claude: bool, bridge: bool, run_unit: Option<String> }
impl ProcessEvidence {
    fn observe(&mut self, input: &Setup, gateway: u32) {
        if self.claude && self.bridge { return; }
        for entry in fs::read_dir("/proc").unwrap() {
            let Ok(entry) = entry else { continue; };
            let Some(pid) = entry.file_name().to_str().and_then(|name| name.parse::<u32>().ok()) else { continue; };
            let Ok(meta) = fs::metadata(entry.path()) else { continue; };
            if meta.uid() != input.employee_uid { continue; }
            let Some(digest) = hash(&entry.path().join("exe")) else { continue; };
            let runtime = digest == input.claude_sha256;
            let bridge = digest == input.bridge_sha256;
            if !runtime && !bridge { continue; }
            assert_uid(pid, input.employee_uid);
            for namespace in ["mnt", "pid", "net", "ipc", "uts"] {
                let Ok(actual) = fs::read_link(entry.path().join("ns").join(namespace)) else { return; };
                assert_ne!(actual, fs::read_link(format!("/proc/{gateway}/ns/{namespace}")).unwrap(),
                    "employee shares gateway namespace");
            }
            let Ok(cgroup) = fs::read_to_string(entry.path().join("cgroup")) else { continue; };
            let unit = cgroup.lines().filter_map(|line| line.strip_prefix("0::"))
                .flat_map(|path| path.split('/')).find(|unit| unit.strip_prefix("duduclaw-run-")
                    .and_then(|value| value.strip_suffix(".service"))
                    .is_some_and(|id| id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit())))
                .expect("employee must be in a spawnd run cgroup");
            if let Some(expected) = &self.run_unit {
                assert_eq!(unit, expected, "Claude and bridge must belong to the same single observed run");
            } else { self.run_unit = Some(unit.into()); }
            let Ok(environment) = fs::read(entry.path().join("environ")) else { continue; };
            let environment = Zeroizing::new(environment);
            for variable in environment.split(|byte| *byte == 0) {
                let key = variable.split(|byte| *byte == b'=').next().unwrap_or_default();
                assert!(![b"DUDUCLAW_AUTH_TOKEN".as_slice(), b"DUDUCLAW_AGENT_TOKEN", b"DUDUCLAW_ON_BEHALF_OF",
                    b"DUDUCLAW_PRINCIPAL_CONTEXT", b"DUDUCLAW_TURN_ID"].contains(&key),
                    "gateway bearer/authority environment leaked into employee execution");
            }
            if runtime {
                let Ok(fd) = fs::read_link(entry.path().join("fd/3")) else { continue; };
                assert!(fd.to_str().and_then(|s| s.strip_prefix("socket:[")).and_then(|s| s.strip_suffix(']'))
                    .is_some_and(|s| s.parse::<u64>().is_ok()), "real runtime MCP fd missing");
                self.claude = true;
            }
            if bridge { self.bridge = true; }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "root, disposable provisioned Linux VM, real signed product generation and isolated employee provider login required"]
async fn iso_root_e2e_claude_production_ingress_product_tool() {
    duduclaw_tui::without_tracing(async {
        let mut input = setup();
        let config: toml::Value = fs::read_to_string(Path::new(HOME).join("config.toml")).unwrap().parse().unwrap();
        assert_eq!(config.get("gateway").and_then(|g| g.get("bind")).and_then(toml::Value::as_str), Some("127.0.0.1"),
            "acceptance gateway must be configured for loopback only");
        let (_service, gateway_pid) = ServiceGuard::start(&input);
        let target = GatewayUrl::parse(&format!("http://127.0.0.1:{}", input.port)).unwrap();
        let http = reqwest::Client::builder().no_proxy().redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(2)).build().unwrap();
        let boot = Instant::now();
        loop {
            if http.get(format!("http://127.0.0.1:{}/health", input.port)).send().await
                .is_ok_and(|response| response.status().is_success()) { break; }
            assert!(boot.elapsed() < Duration::from_secs(30), "installed production gateway failed to boot");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let auth = AuthClient::new(target).unwrap();
        let session = auth.login(LoginMode::TeamOrEnterprise, &mut Login { setup: &mut input }).await
            .expect("real account login/MFA must succeed; no fabricated JWT or local admin fallback");
        assert_eq!(session.account.id, input.user_id, "login must preserve original user");
        let client = Client::start(auth, session, ClientOptions::default()).unwrap();
        let mut connection = client.connection();
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let ready = matches!(&*connection.borrow_and_update(), ConnectionState::Connected(_));
                if ready { break; }
                connection.changed().await.unwrap();
            }
        }).await.expect("authenticated product RPC connection");
        let readiness_deadline = Instant::now();
        loop {
            let status = client.call("system.status", json!({}), Duration::from_secs(10)).await.unwrap();
            assert_eq!(status["isolation_readiness"]["required"], true, "must be protected TEAM, never local fallback");
            if status["isolation_readiness"]["ready"] == true { break; }
            assert!(readiness_deadline.elapsed() < Duration::from_secs(20),
                "production composite readiness is incomplete; missing producers/evidence is a failing acceptance gate");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let chat = client.chat();
        let mut states = chat.state();
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                if states.borrow_and_update().connected { break; }
                states.changed().await.unwrap();
            }
        }).await.expect("authenticated real WebChat handshake");
        let nonce = format!("iso-product-{}", uuid::Uuid::new_v4().simple());
        let description = format!("product-handler-receipt-{nonce}");
        let prompt = format!("Acceptance task: call the real {TOOL} tool exactly once with kind=task, title={nonce}, description={description}, assigned_to={}. Do not set a schedule, goal, dependencies or dispatch options. Do not execute the task. Reply with the exact new task id returned by that tool. Do not use Bash or write files to create the task.", input.agent);
        let count: i64 = readonly("tasks.db").query_row("SELECT count(*) FROM tasks WHERE title=?1", [&nonce], |r| r.get(0)).unwrap();
        assert_eq!(count, 0, "fresh nonce must not already exist");
        let message_watermark: i64 = readonly("sessions.db").query_row(
            "SELECT coalesce(max(id),0) FROM session_messages", [], |r| r.get(0)).unwrap();
        let room = "isolation-product-acceptance";
        assert!(!fs::read_dir("/proc").unwrap().filter_map(Result::ok).any(|entry|
            entry.file_name().to_str().is_some_and(|name| name.parse::<u32>().is_ok())
                && fs::metadata(entry.path()).is_ok_and(|meta| meta.uid() == input.employee_uid)),
            "acceptance employee must have no preexisting process");
        chat.send(room, Submission { agent: input.agent.clone(), content: prompt.clone() }, vec![room.into()]).unwrap();
        let start = Instant::now();
        let mut observed = ProcessEvidence::default();
        loop {
            observed.observe(&input, gateway_pid);
            let finished = states.borrow().rooms.get(room).is_some_and(|r| r.msg_id.is_some() && !r.running);
            if finished { break; }
            assert!(start.elapsed() < DEADLINE, "real Claude/product tool turn exceeded acceptance deadline");
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let tasks = readonly("tasks.db");
        let rows: Vec<(String, String, String, String, String)> = tasks.prepare(
            "SELECT id,assigned_to,created_by,on_behalf_of,created_via FROM tasks WHERE title=?1 AND description=?2")
            .unwrap().query_map(params![nonce, description], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))
            .unwrap().map(Result::unwrap).collect();
        assert_eq!(rows.len(), 1, "exactly one real product task must land");
        let (task, assignee, creator, principal, via) = &rows[0];
        assert_eq!(assignee, &input.agent); assert_eq!(creator, &input.agent);
        assert_eq!(principal, &format!("user:{}", input.user_id)); assert_eq!(via, "agent");
        assert!(uuid::Uuid::parse_str(task).is_ok() && !prompt.contains(task), "product must mint the returned identifier");
        let sessions = readonly("sessions.db");
        let candidates: Vec<(String, String, String, String)> = sessions.prepare(
            "SELECT m.session_id,m.content,s.origin_kind,s.owner_user_id FROM session_messages m JOIN sessions s ON s.id=m.session_id WHERE m.role='user' AND m.id>?1 AND s.agent_id=?2 LIMIT 100")
            .unwrap().query_map(params![message_watermark, input.agent], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))
            .unwrap().map(Result::unwrap).collect();
        let matching: Vec<_> = candidates.iter().filter(|row|
            duduclaw_gateway::channel_reply::strip_sender_prefix(&row.1) == prompt).collect();
        assert_eq!(matching.len(), 1, "one exact original authenticated message must be stored");
        let (session_key, _, origin, owner) = matching[0];
        assert_eq!(origin, "webchat_user");
        assert_eq!(owner, &input.user_id);
        let responses: Vec<String> = sessions.prepare("SELECT content FROM session_messages WHERE session_id=?1 AND role='assistant'")
            .unwrap().query_map([session_key], |r| r.get(0)).unwrap().map(Result::unwrap).collect();
        assert!(responses.iter().any(|text| text.contains(task)), "actual Claude reply must include the product result");
        let steps = readonly("run_steps.db");
        let tool_count: i64 = steps.query_row(
            "SELECT count(*) FROM run_steps WHERE session_key=?1 AND agent_id=?2 AND kind='tool_step' AND label=?3",
            params![session_key, input.agent, TOOL], |r| r.get(0)).unwrap();
        assert!(tool_count > 0, "the original CLI stream must record the real product MCP tool");
        assert!(observed.claude && observed.bridge, "observe real signed Claude and shim in employee namespaces");
        chat.shutdown().await;
        client.shutdown().await;
        // Preserve the product row/session as evidence; VM disposal owns data cleanup.
        eprintln!("product ingress acceptance passed: one authenticated task, exact product result, live isolated Claude and shim");
    }).await;
}
