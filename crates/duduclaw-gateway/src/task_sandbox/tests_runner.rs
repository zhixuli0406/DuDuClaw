//! Runner tests with a scripted container client (no Docker needed), plus the
//! per-family `docker create` argv properties.
use std::os::unix::fs::PermissionsExt;

use duduclaw_agent::account_rotator::{Account, AccountRotator, RotationStrategy};
use serde_json::json;

use super::container::{ContainerPlan, DOCKER_PROGRAM, build_create};
use super::settings::SandboxSettings;
use super::*;

const IMAGE: &str = "fixture/task-sandbox:1";

struct Fixture {
    dir: tempfile::TempDir,
    home: PathBuf,
    agent: PathBuf,
    tools: PathBuf,
}

impl Fixture {
    fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.tools.join(name)).unwrap_or_default()
    }
    fn runs_left(&self) -> usize {
        std::fs::read_dir(self.home.join("sandbox/runs")).map(|d| d.count()).unwrap_or(0)
    }
}

/// A fake `docker`: answers `version` / `image inspect`, records `create`
/// argv and the secret values it received by environment, replays
/// `events.ndjson` on `start`, then runs `tail`.
fn fixture(events: &[Value], tail: &str, image_present: bool) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().canonicalize().unwrap().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let agent = home.join("agents/worker");
    std::fs::create_dir_all(&agent).unwrap();
    std::fs::write(agent.join("SOUL.md"), "soul").unwrap();
    let tools = dir.path().join("tools");
    std::fs::create_dir(&tools).unwrap();
    let stream: String = events.iter().map(|e| e.to_string() + "\n").collect();
    std::fs::write(tools.join("events.ndjson"), stream).unwrap();
    let inspect = if image_present { "printf 'sha256:%064d\\n' 7" } else { "echo 'Error: No such image' >&2; exit 1" };
    let client = tools.join("docker");
    std::fs::write(&client, format!(
        "#!/bin/sh\nd=\"${{0%/*}}\"\ncase \"$1\" in\n\
         version) echo 27.3.1;;\n\
         image) {inspect};;\n\
         create) for a in \"$@\"; do printf '%s\\n' \"$a\"; done > \"$d/create.args\"; \
printf '%s|' \"$ANTHROPIC_API_KEY\" \"$OPENAI_API_KEY\" \"$CODEX_API_KEY\" \"$XAI_API_KEY\" \"$GEMINI_API_KEY\" > \"$d/keys\"; printf '%064d\\n' 1;;\n\
         start) /bin/cat > \"$d/stdin\"; /bin/cat \"$d/events.ndjson\"; {tail};;\n\
         rm) printf 'rm %s\\n' \"$3\" >> \"$d/rm\";;\n\
         *) exit 3;;\nesac\n")).unwrap();
    std::fs::set_permissions(&client, std::fs::Permissions::from_mode(0o700)).unwrap();
    DOCKER_PROGRAM.with(|p| *p.borrow_mut() = Some(client));
    Fixture { dir, home, agent, tools }
}

async fn rotator(provider: &str, key: &str) -> AccountRotator {
    let rotator = AccountRotator::new(RotationStrategy::RoundRobin, 0);
    let mut account: Account = serde_json::from_value(json!({
        "id": "acct", "auth_method": "api_key", "provider": provider, "priority": 1, "monthly_budget_cents": 1000,
    })).unwrap();
    account.is_healthy = true;
    account.api_key = key.into();
    rotator.push_account_for_test(account).await;
    rotator
}

fn task(fx: &Fixture, runtime: RuntimeType) -> TaskSpec {
    TaskSpec {
        agent_id: "worker".into(),
        agent_dir: fx.agent.clone(),
        runtime,
        model: "model-x".into(),
        system_prompt: "# Soul\nbe precise".into(),
        prompt: "summarise the file".into(),
        network_access: true,
        timeout: Duration::from_secs(20),
        disallowed_tools: vec!["computer".into()],
        explicit_denied_tools: vec![],
        account_pool: vec![],
    }
}

fn settings(max_turns: u32) -> SandboxSettings {
    SandboxSettings { image: IMAGE.into(), max_turns, ..SandboxSettings::default() }
}

async fn go(fx: &Fixture, runtime: RuntimeType, provider: &str, key: &str, max_turns: u32) -> Result<String, SandboxError> {
    let rotator = rotator(provider, key).await;
    let spec = task(fx, runtime);
    run(&fx.home, &settings(max_turns), &spec, Ok(&rotator)).await
}

fn claude_result(text: &str) -> Value {
    json!({"type":"result","result":text,"is_error":false,"total_cost_usd":0.001,
        "usage":{"input_tokens":10,"output_tokens":5}})
}

#[tokio::test]
async fn success_returns_the_reply_and_cleans_up() {
    let events = [json!({"type":"system","subtype":"init","mcp_servers":[]}),
        json!({"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read","id":"t1","input":{}}]}}),
        json!({"type":"assistant","message":{"content":[{"type":"text","text":"draft"}]}}),
        claude_result("final reply")];
    let fx = fixture(&events, "exit 0", true);
    let reply = go(&fx, RuntimeType::Claude, "anthropic", "sk-ant-fake-0123456789", 30).await;
    assert_eq!(reply.unwrap(), "final reply");
    let args = fx.read("create.args");
    assert!(args.lines().any(|l| l == "ANTHROPIC_API_KEY"), "secret passed by name: {args}");
    assert!(!args.contains("sk-ant-fake"), "secret value never in argv");
    assert!(fx.read("keys").contains("sk-ant-fake-0123456789"), "value reaches docker via its env");
    let stdin = fx.read("stdin");
    assert!(stdin.starts_with("<system_instructions>\n# Soul\nbe precise\n</system_instructions>\n\nsummarise the file"), "{stdin}");
    assert!(args.lines().any(|l| l == "--disallowedTools") && args.lines().any(|l| l == "computer"));
    assert!(fx.read("rm").starts_with("rm dudu-task-"), "container removed");
    assert_eq!(fx.runs_left(), 0, "per-task directory deleted");
    drop(fx.dir);
}

#[tokio::test]
async fn tool_violation_fails_without_retry_and_is_audited() {
    let events = [json!({"type":"assistant","message":{"content":[{"type":"tool_use","name":"WebFetch","id":"t1","input":{}}]}}),
        claude_result("should not be returned")];
    let fx = fixture(&events, "/bin/sleep 20", true);
    let started = std::time::Instant::now();
    let err = go(&fx, RuntimeType::Claude, "anthropic", "sk-ant-fake-0123456789", 30).await.unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(10), "the host stops the container");
    match err {
        SandboxError::Failed(f) => {
            assert_eq!(f.code, failure_code::TOOL_VIOLATION);
            assert!(f.message.contains("WebFetch") && f.message.contains("outside"), "{f:?}");
        }
        other => panic!("{other:?}"),
    }
    let audit = std::fs::read_to_string(fx.home.join("security_audit.jsonl")).unwrap_or_default();
    assert!(audit.contains("task_sandbox_tool_violation"), "{audit}");
    assert_eq!(fx.read("rm").lines().count(), 1, "one container, no retry");
    assert_eq!(fx.runs_left(), 0);
}

#[tokio::test]
async fn host_step_limit_returns_the_last_reply() {
    let events = [json!({"type":"item.completed","item":{"id":"m0","type":"agent_message","text":"partial answer"}}),
        json!({"type":"item.started","item":{"id":"c1","type":"command_execution"}}),
        json!({"type":"item.started","item":{"id":"c2","type":"command_execution"}})];
    let fx = fixture(&events, "/bin/sleep 20", true);
    let reply = go(&fx, RuntimeType::Codex, "openai", "sk-openai-fake-0123456789", 1).await.unwrap();
    assert_eq!(reply, "partial answer");
    assert_eq!(fx.runs_left(), 0);
}

#[tokio::test]
async fn step_limit_without_any_reply_is_an_error() {
    let events = [json!({"type":"item.started","item":{"id":"c1","type":"command_execution"}}),
        json!({"type":"item.started","item":{"id":"c2","type":"command_execution"}})];
    let fx = fixture(&events, "/bin/sleep 20", true);
    let err = go(&fx, RuntimeType::Codex, "openai", "sk-openai-fake-0123456789", 1).await.unwrap_err();
    assert!(matches!(&err, SandboxError::Failed(f) if f.code == failure_code::STEP_LIMIT && f.message.contains("step limit")), "{err:?}");
    assert_eq!(fx.runs_left(), 0);
}

#[tokio::test]
async fn native_max_turns_returns_the_last_assistant_text() {
    let events = [json!({"type":"assistant","message":{"content":[{"type":"text","text":"so far"}]}}),
        json!({"type":"result","subtype":"error_max_turns","is_error":true,"usage":{"input_tokens":1,"output_tokens":1}})];
    let fx = fixture(&events, "exit 1", true);
    let reply = go(&fx, RuntimeType::Grok, "xai", "xai-fake-0123456789", 3).await.unwrap();
    assert_eq!(reply, "so far");
}

#[tokio::test]
async fn auth_failure_text_is_redacted() {
    let key = "sk-openai-fake-0123456789";
    let events = [json!({"type":"thread.started","thread_id":"t"}),
        json!({"type":"error","message":format!("401 Unauthorized: Incorrect API key provided: {key}")}),
        json!({"type":"turn.failed","error":{"message":format!("Incorrect API key provided: {key}")}})];
    let fx = fixture(&events, &format!("echo 'auth error for {key}' >&2; exit 1"), true);
    let err = go(&fx, RuntimeType::Codex, "openai", key, 30).await.unwrap_err();
    let SandboxError::Failed(failure) = err else { panic!("{err:?}") };
    assert_eq!(failure.code, failure_code::AUTH_FAILED);
    assert!(failure.message.contains("authentication failed"), "{failure:?}");
    // CLI output stays in the host-only detail, redacted.
    let detail = failure.detail.clone().unwrap_or_default();
    assert!(detail.contains("<redacted>"), "{failure:?}");
    assert!(!failure.message.contains("Incorrect API key"), "{failure:?}");
    assert!(!failure.host_text().contains(key), "{failure:?}");
    assert_eq!(fx.runs_left(), 0);
}

#[tokio::test]
async fn reply_text_is_redacted_too() {
    let key = "xai-fake-0123456789";
    let events = [json!({"type":"result","result":format!("my key is {key}"),"is_error":false,
        "usage":{"input_tokens":1,"output_tokens":1}})];
    let fx = fixture(&events, "exit 0", true);
    let reply = go(&fx, RuntimeType::Grok, "xai", key, 3).await.unwrap();
    assert_eq!(reply, "my key is <redacted>");
    assert!(fx.read("create.args").lines().any(|l| l == "XAI_API_KEY"));
}

#[tokio::test]
async fn missing_image_is_unavailable_and_creates_nothing() {
    let fx = fixture(&[], "exit 0", false);
    let err = go(&fx, RuntimeType::Claude, "anthropic", "sk-ant-fake-0123456789", 30).await.unwrap_err();
    assert_eq!(err, SandboxError::Unavailable(Unavailable::ImageMissing(IMAGE.into())));
    assert!(fx.read("create.args").is_empty());
    assert_eq!(fx.runs_left(), 0);
}

#[tokio::test]
async fn no_usable_account_is_unavailable() {
    let fx = fixture(&[], "exit 0", true);
    // An Anthropic account exists but the agent runs Gemini.
    let err = go(&fx, RuntimeType::Gemini, "anthropic", "sk-ant-fake-0123456789", 30).await.unwrap_err();
    let SandboxError::Unavailable(Unavailable::NoAccount { runtime, accepted }) = err else { panic!("{err:?}") };
    assert_eq!(runtime, "gemini");
    assert!(accepted.contains("Gemini API key"));
    assert!(fx.read("create.args").is_empty());
}

#[tokio::test]
async fn network_off_creates_nothing() {
    let fx = fixture(&[], "exit 0", true);
    let rotator = rotator("anthropic", "sk-ant-fake-0123456789").await;
    let mut spec = task(&fx, RuntimeType::Claude);
    spec.network_access = false;
    let err = run(&fx.home, &settings(30), &spec, Ok(&rotator)).await.unwrap_err();
    assert_eq!(err, SandboxError::Unavailable(Unavailable::NetworkDisabled));
    assert!(fx.read("create.args").is_empty());
    assert!(!fx.home.join("sandbox").exists());
}

#[tokio::test]
async fn cli_failure_without_reply_is_a_readable_error() {
    let events = [json!({"type":"result","is_error":true,"result":"model not found: model-x"})];
    let fx = fixture(&events, "exit 1", true);
    let err = go(&fx, RuntimeType::Claude, "anthropic", "sk-ant-fake-0123456789", 30).await.unwrap_err();
    assert!(matches!(&err, SandboxError::Failed(f) if f.code == failure_code::NO_REPLY
        && !f.message.contains("model not found")
        && f.detail.as_deref().is_some_and(|d| d.contains("model not found"))), "{err:?}");
    assert_eq!(fx.runs_left(), 0);
}

// ── docker create argv properties, per family ───────────────────

fn soul_mount() -> Vec<super::container::AgentMount> {
    vec![super::container::AgentMount { source: PathBuf::from("/agents/a/SOUL.md"), name: "SOUL.md" }]
}

fn plan_args(family: RuntimeFamily, env: &BTreeMap<String, String>, ws: &Path) -> Vec<String> {
    let s = SandboxSettings::default();
    let argv = family.argv("model-x", 5, ws);
    let agent_files = soul_mount();
    let plan = ContainerPlan {
        image: &s.image, executable: s.executable(family), argv: &argv, env,
        agent_files: &agent_files, config_dir: Path::new("/runs/r/config"),
        memory_bytes: s.memory_bytes, pids: s.pids, cpu_millis: s.cpu_millis, tmp_bytes: s.tmp_bytes,
        workspace_bytes: s.workspace_bytes, home_label: "0123abcd",
        uid: 501, gid: 20, run_id: "abc", agent_id: "worker", timeout: Duration::from_secs(60),
    };
    let (command, name) = build_create(&plan).unwrap();
    assert_eq!(name, "dudu-task-abc");
    command.get_args().map(|a| a.to_string_lossy().into_owned()).collect()
}

fn pair(args: &[String], flag: &str, value: &str) -> bool {
    args.windows(2).any(|w| w[0] == flag && w[1] == value)
}

#[test]
fn every_family_gets_the_same_hardened_container() {
    let ws = Path::new(super::container::WORKSPACE);
    for family in settings::FAMILIES {
        let mut selected = std::collections::HashMap::new();
        for key in ["ANTHROPIC_API_KEY", "OPENAI_API_KEY", "GEMINI_API_KEY", "XAI_API_KEY"] {
            selected.insert(key.to_string(), format!("secret-{key}-value"));
        }
        let mut env = adapter::environment(family, &selected);
        if adapter::credential_destination(family).is_some() {
            env.insert(adapter::CREDENTIAL_DOC_ENV.into(), "{\"secret-doc\":\"secret-doc-token-value\"}".into());
            env.insert(adapter::CREDENTIAL_DEST_ENV.into(), "x/auth.json".into());
        }
        let args = plan_args(family, &env, ws);
        let name = family.name();
        assert_eq!(args[0], "create", "{name}");
        for flag in ["--read-only", "--interactive", "--rm"] {
            assert!(args.iter().any(|a| a == flag), "{name} {flag}");
        }
        assert!(pair(&args, "--user", "501:20"), "{name}");
        assert!(pair(&args, "--cap-drop", "ALL"), "{name}");
        assert!(pair(&args, "--security-opt", "no-new-privileges:true"), "{name}");
        assert!(pair(&args, "--network", "bridge"), "{name}");
        assert!(pair(&args, "--memory", &(4u64 << 30).to_string()) && pair(&args, "--pids-limit", "128") && pair(&args, "--cpus", "1.000"), "{name}");
        assert!(pair(&args, "--tmpfs", &format!("/tmp:rw,exec,nosuid,nodev,size={},mode=1777", 256u64 << 20)), "{name}");
        assert!(pair(&args, "--pull", "never"), "{name}");
        assert!(pair(&args, "--tmpfs", &format!("/workspace:rw,exec,nosuid,nodev,size={},uid=501,gid=20,mode=0700", 512u64 << 20)), "{name}");
        assert!(pair(&args, "--workdir", "/workspace"), "{name}");
        // The agent directory itself is never mounted: allowlisted entries only.
        assert!(pair(&args, "--mount", "type=bind,src=/agents/a/SOUL.md,dst=/agent/SOUL.md,readonly,bind-propagation=rprivate"), "{name}");
        assert!(!args.iter().any(|a| a.contains("dst=/agent,") || a.contains("src=/agents/a,")), "{name}: {args:?}");
        assert!(pair(&args, "--mount", "type=bind,src=/runs/r/config,dst=/dudu-runtime,readonly,bind-propagation=rprivate"), "{name}");
        assert_eq!(args.iter().filter(|a| *a == "--mount").count(), 2, "{name}: no workspace bind mount");
        assert!(pair(&args, "--label", "com.duduclaw.task-sandbox=1") && pair(&args, "--label", "com.duduclaw.task-sandbox.agent=worker"), "{name}");
        assert!(pair(&args, "--label", "com.duduclaw.task-sandbox.home=0123abcd"), "{name}");
        assert!(args.iter().any(|a| a.starts_with("com.duduclaw.task-sandbox.deadline=")), "{name}");
        assert!(pair(&args, "--entrypoint", "python3"), "{name}");
        let image_at = args.iter().position(|a| a == &SandboxSettings::default().image).unwrap();
        assert_eq!(&args[image_at + 1..image_at + 5], ["-I", "-S", "-B", "-c"], "{name}");
        assert_eq!(args[image_at + 7], SandboxSettings::default().executable(family).to_str().unwrap(), "{name}");
        // Secrets: names only, never values.
        assert!(!args.iter().any(|a| a.contains("secret-") && a.contains("value")), "{name}: {args:?}");
        for (key, _) in env.iter().filter(|(k, _)| adapter::is_secret_env(k)) {
            assert!(pair(&args, "--env", key), "{name} {key}");
        }
        assert!(!args.iter().any(|a| a == "--privileged" || a.starts_with("--network=none")), "{name}");
    }
}

#[test]
fn root_uid_and_unsafe_mount_paths_are_refused() {
    let s = SandboxSettings::default();
    let argv: Vec<String> = vec![];
    let env = BTreeMap::new();
    let files = |src: &str, name: &'static str| vec![super::container::AgentMount { source: PathBuf::from(src), name }];
    let ok = files("/agents/a/SOUL.md", "SOUL.md");
    let base = |uid: u32, gid: u32, config: &'static str, agent_files: &'static [super::container::AgentMount]| ContainerPlan {
        image: "img:1", executable: Path::new("/usr/bin/claude"), argv: &argv, env: &env,
        agent_files, config_dir: Path::new(config),
        memory_bytes: s.memory_bytes, pids: s.pids, cpu_millis: s.cpu_millis, tmp_bytes: s.tmp_bytes,
        workspace_bytes: s.workspace_bytes, home_label: "h1",
        uid, gid, run_id: "abc", agent_id: "worker", timeout: Duration::from_secs(5),
    };
    let ok: &'static [_] = Box::leak(ok.into_boxed_slice());
    assert!(build_create(&base(0, 20, "/c", ok)).unwrap_err().contains("root"));
    assert!(build_create(&base(501, 0, "/c", ok)).unwrap_err().contains("root"), "gid 0 is refused too");
    assert!(build_create(&base(501, 20, "/c,x", ok)).is_err());
    assert!(build_create(&base(501, 20, "/c\nx", ok)).is_err());
    let comma: &'static [_] = Box::leak(files("/agents/a,b/SOUL.md", "SOUL.md").into_boxed_slice());
    assert!(build_create(&base(501, 20, "/c", comma)).is_err());
    let outside: &'static [_] = Box::leak(files("/agents/a/.mcp.json", ".mcp.json").into_boxed_slice());
    assert!(build_create(&base(501, 20, "/c", outside)).unwrap_err().contains("allowlist"));
    assert!(build_create(&base(501, 20, "/c", ok)).is_ok());
}

// ── review fixes (2026-10-01) ───────────────────────────────────

/// A5: a forbidden Antigravity tool still pending when the host step limit
/// fires is a violation (audited), never `Ok(reply)`.
#[tokio::test]
async fn pending_forbidden_tool_at_the_step_limit_is_a_violation() {
    let step = |index: u64, state: &str, kind: &str, tool: Option<&str>| {
        let mut s = json!({"step_index":index,"state":state,"step_type":kind});
        if let Some(tool) = tool {
            s["tool_name"] = json!(tool);
        }
        if kind == "agent_response" {
            s["agent_response"] = json!({"text":"partial"});
        }
        json!({"event":"step_update","step_update":s})
    };
    let events = [step(1, "DONE", "agent_response", None), step(2, "ACTIVE", "tool", Some("search_web")),
        step(3, "DONE", "agent_response", None)];
    let fx = fixture(&events, "/bin/sleep 20", true);
    let err = go(&fx, RuntimeType::Antigravity, "gemini", concat!("AI", "zaFAKEdudutasksandbox00000000000000000"), 1).await.unwrap_err();
    assert!(matches!(&err, SandboxError::Failed(f) if f.message.contains("search_web") && f.message.contains("outside")), "{err:?}");
    let audit = std::fs::read_to_string(fx.home.join("security_audit.jsonl")).unwrap_or_default();
    assert!(audit.contains("task_sandbox_tool_violation"), "{audit}");
    assert_eq!(fx.runs_left(), 0);
}

/// A6: the auth-failure blame is decided from parsed events; a text or an
/// error message that merely contains `"tool_use"` does not count as work.
#[test]
fn tool_use_detection_parses_events() {
    let worked = json!({"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read"}]}}).to_string();
    assert!(used_a_tool(&worked));
    let quoted = json!({"type":"assistant","message":{"content":[{"type":"text","text":"\"tool_use\""}]}}).to_string();
    let error = json!({"type":"error","message":"401 near \"tool_use\""}).to_string();
    assert!(!used_a_tool(&format!("{quoted}\n{error}\nnot json \"tool_use\"")));
}

/// A1: only allowlisted, real, in-directory entries are mounted.
#[test]
fn agent_mounts_follow_the_allowlist() {
    let dir = tempfile::tempdir().unwrap();
    let agent = dir.path().canonicalize().unwrap().join("agent");
    std::fs::create_dir_all(agent.join("wiki")).unwrap();
    std::fs::create_dir_all(agent.join("state")).unwrap();
    std::fs::create_dir_all(agent.join(".claude")).unwrap();
    for name in ["SOUL.md", ".mcp.json", "agent.toml", "memory.db", ".env", "IDENTITY.md"] {
        std::fs::write(agent.join(name), "x").unwrap();
    }
    let outside = dir.path().join("secret.txt");
    std::fs::write(&outside, "secret").unwrap();
    std::os::unix::fs::symlink(&outside, agent.join("CLAUDE.md")).unwrap();
    std::os::unix::fs::symlink(agent.join(".mcp.json"), agent.join("AGENTS.md")).unwrap();
    std::fs::hard_link(agent.join(".mcp.json"), agent.join("GEMINI.md")).unwrap();
    std::os::unix::fs::symlink(dir.path(), agent.join("SKILLS")).unwrap();
    std::fs::create_dir(agent.join("CONTRACT.toml")).unwrap(); // wrong kind
    let mounts = super::container::agent_mounts(&agent).unwrap();
    let names: Vec<&str> = mounts.iter().map(|m| m.name).collect();
    assert_eq!(names, ["SOUL.md", "IDENTITY.md", "wiki"]);
    assert!(mounts.iter().all(|m| m.source.parent() == Some(agent.as_path())));
}

/// A2: the sweep removes only stopped or past-deadline containers and
/// refuses a listing it cannot verify.
#[test]
fn sweep_rules() {
    use super::sweep::{GRACE, Listed, parse_listing, removable};
    let id = "a".repeat(64);
    let parsed = parse_listing(&format!("{id}|running|abc123|100\n{id}|exited|abc124|\n")).unwrap();
    assert_eq!(parsed[0], Listed { id: id.clone(), state: "running".into(), run: "abc123".into(), deadline: Some(100) });
    assert_eq!(parsed[1].deadline, None);
    assert!(parse_listing("short|running|r|1").is_none());
    assert!(parse_listing(&format!("{id}|running|r|1|extra")).is_none());
    assert!(parse_listing(&format!("{id}|Running|r|1")).is_none());
    let grace = GRACE.as_secs();
    let running = |deadline| Listed { id: id.clone(), state: "running".into(), run: "r".into(), deadline };
    assert!(!removable(&running(Some(1000)), 1000 + grace), "within the grace period");
    assert!(removable(&running(Some(1000)), 1001 + grace), "past its deadline");
    assert!(!removable(&running(None), u64::MAX), "no deadline: never guessed");
    let created = Listed { state: "created".into(), ..running(Some(1000)) };
    assert!(!removable(&created, 1000));
    assert!(removable(&Listed { state: "exited".into(), ..running(None) }, 0));
    assert!(removable(&Listed { state: "dead".into(), ..running(None) }, 0));
}

/// A2: a home where the sandbox never ran is a no-op that never calls Docker.
#[tokio::test]
async fn sweep_without_a_sandbox_directory_does_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let report = super::sweep::sweep_once(dir.path()).await;
    assert_eq!(report, super::sweep::SweepReport::default());
    assert!(!dir.path().join("security_audit.jsonl").exists());
}

/// A2: with a scripted client, an old orphan directory is removed, a young
/// one and one still used by a live container are kept, and a past-deadline
/// container is removed.
#[tokio::test]
async fn sweep_removes_orphans_and_keeps_live_runs() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().canonicalize().unwrap().join("home");
    let runs = home.join("sandbox/runs");
    let live = "1".repeat(32);
    let orphan = "2".repeat(32);
    let young = "3".repeat(32);
    for run in [&live, &orphan, &young] {
        std::fs::create_dir_all(runs.join(run).join("config")).unwrap();
    }
    std::fs::create_dir_all(runs.join("not-a-run")).unwrap();
    let old = std::time::SystemTime::now() - Duration::from_secs(3600);
    for run in [&live, &orphan] {
        let file = std::fs::File::open(runs.join(run)).unwrap();
        file.set_modified(old).unwrap();
    }
    let tools = dir.path().join("tools");
    std::fs::create_dir(&tools).unwrap();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let stale = "b".repeat(64);
    let running = "c".repeat(64);
    let client = tools.join("docker");
    std::fs::write(&client, format!(
        "#!/bin/sh\nd=\"${{0%/*}}\"\ncase \"$1\" in\n\
         ps) case \"$*\" in *id=*) [ -f \"$d/removed\" ] || echo {stale};; \
*) printf '%s|running|{orphan}|1\\n%s|running|{live}|{future}\\n' {stale} {running};; esac;;\n\
         rm) echo \"$3\" > \"$d/removed\";;\n\
         *) exit 3;;\nesac\n", future = now + 3600)).unwrap();
    std::fs::set_permissions(&client, std::fs::Permissions::from_mode(0o700)).unwrap();
    DOCKER_PROGRAM.with(|p| *p.borrow_mut() = Some(client));
    let report = super::sweep::sweep_once(&home).await;
    assert_eq!(report.containers_removed, 1, "{report:?}");
    assert_eq!(std::fs::read_to_string(tools.join("removed")).unwrap().trim(), stale);
    assert!(!runs.join(&orphan).exists(), "orphan directory removed");
    assert!(runs.join(&live).exists(), "a live container's directory is kept");
    assert!(runs.join(&young).exists(), "a young directory is kept");
    assert!(runs.join("not-a-run").exists());
    assert!(report.failures.is_empty(), "{report:?}");
}

/// A2: an unreachable Docker never deletes directories and is audited.
#[tokio::test]
async fn sweep_with_docker_unreachable_keeps_directories_and_audits() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().canonicalize().unwrap().join("home");
    let run = home.join("sandbox/runs").join("4".repeat(32));
    std::fs::create_dir_all(&run).unwrap();
    std::fs::File::open(&run).unwrap().set_modified(std::time::SystemTime::now() - Duration::from_secs(3600)).unwrap();
    let tools = dir.path().join("tools");
    std::fs::create_dir(&tools).unwrap();
    let client = tools.join("docker");
    std::fs::write(&client, "#!/bin/sh\necho 'Cannot connect to the Docker daemon' >&2\nexit 1\n").unwrap();
    std::fs::set_permissions(&client, std::fs::Permissions::from_mode(0o700)).unwrap();
    DOCKER_PROGRAM.with(|p| *p.borrow_mut() = Some(client));
    let report = super::sweep::sweep_once(&home).await;
    assert!(run.exists());
    assert_eq!(report.failures, vec![(super::sweep::SweepFailure::DockerUnreachable, 1)]);
    let audit = std::fs::read_to_string(home.join("security_audit.jsonl")).unwrap();
    assert!(audit.contains("task_sandbox_cleanup_failed") && audit.contains("docker_unreachable"), "{audit}");
}

/// A per-task cleanup failure (the container could not be removed and is
/// still listed) fails the task AND is audited under the task's agent, with
/// a reason code and no secret.
#[tokio::test]
async fn task_cleanup_failure_is_audited() {
    let fx = fixture(&[claude_result("final reply")], "exit 0", true);
    let client = fx.tools.join("docker");
    let script = std::fs::read_to_string(&client).unwrap();
    let script = script.replacen("rm) printf", "ps) printf '%064d\\n' 1;;\n         rm) exit 1; printf", 1);
    assert!(script.contains("rm) exit 1"), "fixture rewritten");
    std::fs::write(&client, script).unwrap();
    let err = go(&fx, RuntimeType::Claude, "anthropic", "sk-ant-fake-0123456789", 30).await.unwrap_err();
    match err {
        SandboxError::Failed(f) => assert!(f.code == failure_code::CLEANUP_FAILED && f.message.contains("could not be removed"), "{f:?}"),
        other => panic!("{other:?}"),
    }
    let audit = std::fs::read_to_string(fx.home.join("security_audit.jsonl")).unwrap_or_default();
    let line = audit.lines().find(|l| l.contains("task_sandbox_cleanup_failed")).expect(&audit);
    assert!(line.contains("\"agent_id\":\"worker\"") && line.contains("container_remove_failed"), "{line}");
    assert!(!audit.contains("sk-ant-fake"), "no secret in the audit: {audit}");
}

/// More leftovers than one batch: the sweep removes a bounded batch and
/// defers the rest to the next sweep instead of refusing the listing; a
/// deferred container's directory is kept with it.
#[tokio::test]
async fn sweep_drains_a_large_backlog_in_bounded_batches() {
    use super::sweep::MAX_REMOVALS_PER_SWEEP;
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().canonicalize().unwrap().join("home");
    let runs = home.join("sandbox/runs");
    std::fs::create_dir_all(&runs).unwrap();
    let total = MAX_REMOVALS_PER_SWEEP + 20;
    let last_run = format!("{:032x}", total - 1);
    std::fs::create_dir_all(runs.join(&last_run)).unwrap();
    std::fs::File::open(runs.join(&last_run)).unwrap()
        .set_modified(std::time::SystemTime::now() - Duration::from_secs(3600)).unwrap();
    let tools = dir.path().join("tools");
    std::fs::create_dir(&tools).unwrap();
    let listing: String = (0..total).map(|i| format!("{:064x}|exited|{:032x}|\n", i, i)).collect();
    std::fs::write(tools.join("listing"), listing).unwrap();
    let client = tools.join("docker");
    std::fs::write(&client, "#!/bin/sh\nd=\"${0%/*}\"\ncase \"$1\" in\n\
         ps) case \"$*\" in *id=*) ;; *) cat \"$d/listing\";; esac;;\n\
         rm) echo \"$3\" >> \"$d/removed\";;\n\
         *) exit 3;;\nesac\n").unwrap();
    std::fs::set_permissions(&client, std::fs::Permissions::from_mode(0o700)).unwrap();
    DOCKER_PROGRAM.with(|p| *p.borrow_mut() = Some(client));
    let report = super::sweep::sweep_once(&home).await;
    assert_eq!(report.containers_removed, MAX_REMOVALS_PER_SWEEP, "{report:?}");
    assert_eq!(report.containers_deferred, 20, "{report:?}");
    assert!(report.failures.is_empty(), "no list_invalid: {report:?}");
    assert_eq!(std::fs::read_to_string(tools.join("removed")).unwrap().lines().count(), MAX_REMOVALS_PER_SWEEP);
    assert!(runs.join(&last_run).exists(), "a deferred container's directory is kept");
}
