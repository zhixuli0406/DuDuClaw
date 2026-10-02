//! Real-Docker checks (`#[ignore]`), run serially against an image that has
//! the CLIs. Fake keys only: each CLI must reach its provider and come back
//! with an authentication error (zero inference).
//!
//! Environment:
//! - `DUDU_TASK_SANDBOX_IMAGE` (required): the image to run.
//! - `DUDU_TASK_SANDBOX_EXECUTABLES` (optional): comma-separated
//!   `runtime=/absolute/path` overrides applied on top of the product defaults
//!   for `[container.sandbox.executables]`. Unset means no path is overridden.
//!
//! Published image (product defaults are correct for it):
//! `DUDU_TASK_SANDBOX_IMAGE=ghcr.io/zhixuli0406/duduclaw:v<version> cargo test -p duduclaw-gateway --lib --no-default-features -- --ignored --test-threads=1 task_sandbox::tests_docker`
//!
//! Local validation image (CLIs live under /usr/local/bin):
//! `DUDU_TASK_SANDBOX_IMAGE=sha256:be45dfabcca9ecb7c783fd06b08a98bc33ab37255684cf14bed46327f8d98776 DUDU_TASK_SANDBOX_EXECUTABLES=codex=/usr/local/bin/codex cargo test -p duduclaw-gateway --lib --no-default-features -- --ignored --test-threads=1 task_sandbox::tests_docker`
use duduclaw_agent::account_rotator::{Account, AccountRotator, RotationStrategy};
use serde_json::json;

use super::settings::SandboxSettings;
use super::*;

fn image() -> String {
    std::env::var("DUDU_TASK_SANDBOX_IMAGE").expect("set DUDU_TASK_SANDBOX_IMAGE to a local image id")
}

/// Product defaults for every executable path, plus the optional overrides
/// from `DUDU_TASK_SANDBOX_EXECUTABLES` (`codex=/usr/local/bin/codex,...`).
fn settings() -> SandboxSettings {
    let mut s = SandboxSettings { image: image(), max_turns: 3, ..SandboxSettings::default() };
    let Ok(raw) = std::env::var("DUDU_TASK_SANDBOX_EXECUTABLES") else { return s };
    for pair in raw.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let (name, path) = pair.split_once('=').unwrap_or_else(|| {
            panic!("DUDU_TASK_SANDBOX_EXECUTABLES entry `{pair}` is not `runtime=/absolute/path`")
        });
        let (name, path) = (name.trim(), path.trim());
        let family = super::settings::FAMILIES.iter().find(|f| f.name() == name).unwrap_or_else(|| {
            let known: Vec<_> = super::settings::FAMILIES.iter().map(|f| f.name()).collect();
            panic!("DUDU_TASK_SANDBOX_EXECUTABLES: unknown runtime `{name}` (known: {known:?})")
        });
        assert!(
            Path::new(path).is_absolute(),
            "DUDU_TASK_SANDBOX_EXECUTABLES: path for `{name}` must be absolute, got `{path}`"
        );
        s.executables.insert(family.name(), PathBuf::from(path));
    }
    s
}

fn docker(args: &[&str]) -> String {
    let out = std::process::Command::new("docker").args(args).output().expect("docker CLI");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn leftovers(agent: &str) -> String {
    docker(&["ps", "-a", "-q", "--filter", &format!("label=com.duduclaw.task-sandbox.agent={agent}")])
}

struct Home {
    _dir: tempfile::TempDir,
    home: PathBuf,
    agent_dir: PathBuf,
}

fn home(agent: &str) -> Home {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().canonicalize().unwrap().join("home");
    let agent_dir = home.join("agents").join(agent);
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(agent_dir.join("SOUL.md"), "soul").unwrap();
    Home { _dir: dir, home, agent_dir }
}

async fn fake_key_run(runtime: RuntimeType, provider: &str, key: &str, model: &str) {
    let agent = format!("rt-{}-{}", runtime.as_str(), &uuid::Uuid::new_v4().simple().to_string()[..8]);
    let h = home(&agent);
    let rotator = AccountRotator::new(RotationStrategy::RoundRobin, 0);
    let mut account: Account = serde_json::from_value(json!({
        "id": "fake", "auth_method": "api_key", "provider": provider, "priority": 1, "monthly_budget_cents": 1000,
    })).unwrap();
    account.is_healthy = true;
    account.api_key = key.into();
    rotator.push_account_for_test(account).await;
    let spec = TaskSpec {
        agent_id: agent.clone(), agent_dir: h.agent_dir.clone(), runtime, model: model.into(),
        system_prompt: "# Soul\nsoul".into(), prompt: "Reply with the word ok.".into(), network_access: true,
        timeout: Duration::from_secs(120), disallowed_tools: vec![], explicit_denied_tools: vec![], account_pool: vec![],
    };
    let result = run(&h.home, &settings(), &spec, Ok(&rotator)).await;
    eprintln!("{runtime:?} -> {result:?}");
    let SandboxError::Failed(failure) = result.expect_err("a fake key never yields a reply") else {
        panic!("sandbox was unavailable instead of reaching the provider");
    };
    assert!(!failure.host_text().contains(key), "secret leaked: {failure:?}");
    assert!(failure.message.contains("authentication failed"), "{failure:?}");
    assert!(leftovers(&agent).trim().is_empty(), "leftover container for {agent}");
    let runs = std::fs::read_dir(h.home.join("sandbox/runs")).map(|d| d.count()).unwrap_or(0);
    assert_eq!(runs, 0, "per-task directory left behind");
}

#[tokio::test]
#[ignore = "requires Docker and DUDU_TASK_SANDBOX_IMAGE"]
async fn real_codex_fake_key_is_an_authentication_error() {
    fake_key_run(RuntimeType::Codex, "openai", "sk-proj-FAKEdudutasksandbox0000000000", "gpt-5.1-codex").await;
}

#[tokio::test]
#[ignore = "requires Docker and DUDU_TASK_SANDBOX_IMAGE"]
async fn real_grok_fake_key_is_an_authentication_error() {
    fake_key_run(RuntimeType::Grok, "xai", "xai-FAKEdudutasksandbox000000000000000", "grok-code-fast-1").await;
}

#[tokio::test]
#[ignore = "requires Docker and DUDU_TASK_SANDBOX_IMAGE"]
async fn real_antigravity_fake_key_is_an_authentication_error() {
    fake_key_run(RuntimeType::Antigravity, "gemini", concat!("AI", "zaFAKEdudutasksandbox00000000000000000"), "Gemini 3.8 Flash (Low)").await;
}

#[tokio::test]
#[ignore = "requires Docker and DUDU_TASK_SANDBOX_IMAGE"]
async fn real_agent_dir_is_read_only_and_nothing_is_left() {
    let agent = format!("rt-probe-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
    let h = home(&agent);
    // A planted MCP config with fabricated credentials must stay invisible.
    std::fs::write(h.agent_dir.join(".mcp.json"), r#"{"env":{"DUDUCLAW_MCP_API_KEY":"fake-probe-key"}}"#).unwrap();
    std::fs::write(h.agent_dir.join("agent.toml"), "[agent]\nname = \"probe\"\n").unwrap();
    std::fs::create_dir_all(h.agent_dir.join("wiki")).unwrap();
    std::fs::write(h.agent_dir.join("wiki/page.md"), "wiki page").unwrap();
    let s = SandboxSettings { workspace_bytes: 1024 * 1024, ..settings() };
    let mut dir = super::container::TaskDir::create(&h.home).unwrap();
    super::container::write_config(&dir.config, 3, &[]).unwrap();
    let agent_dir = h.agent_dir.canonicalize().unwrap();
    let agent_files = super::container::agent_mounts(&agent_dir).unwrap();
    let script = "if touch /agent/probe 2>/dev/null; then echo agent-writable; else echo agent-readonly; fi; \
        if touch /agent/SOUL.md 2>/dev/null; then echo soul-writable; fi; \
        if [ -e /agent/.mcp.json ]; then echo mcp-visible; else echo mcp-hidden; fi; \
        if [ -e /agent/agent.toml ]; then echo toml-visible; fi; \
        grep -rqs fake-probe-key /agent && echo key-found; \
        cat /agent/SOUL.md; echo; cat /agent/wiki/page.md; echo; \
        if touch \"$PWD/probe\" 2>/dev/null; then echo ws-writable; fi; echo \"pwd=$PWD\"; \
        if dd if=/dev/zero of=\"$PWD/big\" bs=1024 count=2048 2>/dev/null; then echo ws-unbounded; else echo ws-capped; fi; \
        if touch /usr/probe 2>/dev/null; then echo root-writable; else echo root-readonly; fi; id -u";
    let argv = vec!["-c".to_string(), script.to_string()];
    let env = BTreeMap::new();
    let home_label = super::container::home_label(&h.home.canonicalize().unwrap());
    let plan = super::container::ContainerPlan {
        image: &s.image, executable: Path::new("/bin/sh"), argv: &argv, env: &env, agent_files: &agent_files,
        config_dir: &dir.config, memory_bytes: s.memory_bytes, pids: s.pids,
        cpu_millis: s.cpu_millis, tmp_bytes: s.tmp_bytes, workspace_bytes: s.workspace_bytes, home_label: &home_label,
        uid: unsafe { libc::geteuid() }, gid: unsafe { libc::getegid() },
        run_id: &dir.run_id, agent_id: &agent, timeout: Duration::from_secs(60),
    };
    let (create, name) = super::container::build_create(&plan).unwrap();
    let out = super::container::launch(create, name, b"", Duration::from_secs(60), |_| true).await.unwrap();
    eprintln!("probe stdout: {} stderr: {}", out.stdout, out.stderr);
    assert!(out.status.success(), "{}", out.stderr);
    assert!(out.stdout.contains("agent-readonly") && !out.stdout.contains("agent-writable"), "{}", out.stdout);
    assert!(!out.stdout.contains("soul-writable"), "{}", out.stdout);
    assert!(out.stdout.contains("mcp-hidden") && !out.stdout.contains("mcp-visible"), "{}", out.stdout);
    assert!(!out.stdout.contains("toml-visible") && !out.stdout.contains("key-found"), "{}", out.stdout);
    assert!(out.stdout.contains("soul") && out.stdout.contains("wiki page"), "{}", out.stdout);
    assert!(out.stdout.contains("ws-writable") && out.stdout.contains("pwd=/workspace"), "{}", out.stdout);
    assert!(out.stdout.contains("ws-capped") && !out.stdout.contains("ws-unbounded"), "{}", out.stdout);
    assert!(out.stdout.contains("root-readonly"), "{}", out.stdout);
    assert!(out.stdout.contains(&unsafe { libc::geteuid() }.to_string()), "{}", out.stdout);
    assert!(!h.agent_dir.join("probe").exists());
    assert!(leftovers(&agent).trim().is_empty(), "leftover container");
    dir.remove().unwrap();
    assert_eq!(std::fs::read_dir(h.home.join("sandbox/runs")).unwrap().count(), 0);
}

/// A2 against real Docker: a container of this home past its deadline is
/// swept at boot; one within its deadline is kept.
#[tokio::test]
#[ignore = "requires Docker and DUDU_TASK_SANDBOX_IMAGE"]
async fn real_boot_sweep_removes_only_past_deadline_containers() {
    let agent = format!("rt-sweep-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
    let h = home(&agent);
    std::fs::create_dir_all(h.home.join("sandbox/runs")).unwrap();
    let label = super::container::home_label(&h.home.canonicalize().unwrap());
    let create = |run: &str, deadline: u64| {
        let out = std::process::Command::new("docker").args([
            "create", "--pull", "never",
            "--label", "com.duduclaw.task-sandbox=1",
            "--label", &format!("com.duduclaw.task-sandbox.run={run}"),
            "--label", &format!("com.duduclaw.task-sandbox.agent={agent}"),
            "--label", &format!("com.duduclaw.task-sandbox.home={label}"),
            "--label", &format!("com.duduclaw.task-sandbox.deadline={deadline}"),
            "--entrypoint", "/bin/sh", &image(), "-c", "sleep 120",
        ]).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let id = String::from_utf8_lossy(&out.stdout).trim().to_string();
        assert!(std::process::Command::new("docker").args(["start", &id]).status().unwrap().success());
        id
    };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let stale = create("stalerun", 1);
    let live = create("liverun", now + 3600);
    let report = super::sweep::sweep_once(&h.home).await;
    let present = |id: &str| !docker(&["ps", "-a", "-q", "--no-trunc", "--filter", &format!("id={id}")]).trim().is_empty();
    let stale_left = present(&stale);
    let live_left = present(&live);
    docker(&["rm", "--force", &live]);
    docker(&["rm", "--force", &stale]);
    assert_eq!(report.containers_removed, 1, "{report:?}");
    assert!(!stale_left, "past-deadline container swept");
    assert!(live_left, "a container within its deadline is kept");
}
