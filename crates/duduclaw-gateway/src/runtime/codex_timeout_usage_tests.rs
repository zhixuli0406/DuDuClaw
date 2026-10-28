use super::*;
use crate::cost_telemetry::RequestType;
use crate::runtime::{GoalRoundAttribution, RoleCostAttribution, RuntimeRegistry};
use crate::runtime_dispatch::{self, AgentPrompt};
use crate::runtime_config::RuntimeSettings;
use duduclaw_core::types::RuntimeType;
use std::path::{Path, PathBuf};
use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};

// Telemetry is process-global. Run only this exact fixture in a fresh process
// so parallel tests cannot choose its database or credentials.
fn isolated_home(name: &str) -> Option<PathBuf> {
    const CHILD_HOME: &str = "DUDU_TEST_TIMEOUT_LEDGER_HOME";
    if let Some(home) = std::env::var_os(CHILD_HOME) { return Some(home.into()); }
    let home = tempfile::tempdir().unwrap();
    let full_name = format!("{}::{name}", module_path!());
    let test_name = full_name.split_once("::").unwrap().1;
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .env_clear().env("HOME", home.path()).env(CHILD_HOME, home.path())
        .args(["--exact", test_name, "--nocapture"]).output().unwrap();
    assert!(String::from_utf8_lossy(&output.stdout).contains("running 1 test"));
    assert!(output.status.success(), "{} {}",
        String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    None
}

fn request<'a>(home: &'a Path, agent: &'a str, settings: &'a RuntimeSettings) -> AgentPrompt<'a> {
    AgentPrompt {
        agent_dir: None, home_dir: home, agent_id: agent, prompt: "fixture", system_prompt: "",
        model: "gpt-5.4", max_tokens: 16, provider_override: Some(RuntimeType::Codex),
        conversation_history: &[], request_type: RequestType::Dispatch,
        runtime_settings: Some(settings), effort: None, allow_cross_family_failover: true,
    }
}

type UsageRow = (String, String, i64, i64, i64, Option<String>, Option<String>, Option<i64>);

fn rows(home: &Path, agent: &str) -> Vec<UsageRow> {
    let conn = rusqlite::Connection::open(home.join("cost_telemetry.db")).unwrap();
    conn.prepare("SELECT request_type,model,input_tokens,output_tokens,cache_read_tokens,role,episode_id,round
        FROM token_usage WHERE agent_id=?1 ORDER BY id").unwrap()
        .query_map([agent], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?,
            r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?))).unwrap()
        .collect::<Result<_, _>>().unwrap()
}

struct MeasuredRuntime {
    partial: bool,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl AgentRuntime for MeasuredRuntime {
    fn name(&self) -> &str { if self.partial { "codex" } else { "claude" } }
    async fn is_available(&self) -> bool { panic!("fixture must never discover a CLI") }
    async fn execute(&self, _: &str, context: &RuntimeContext) -> Result<RuntimeResponse, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(RuntimeResponse {
            partial_timeout_secs: self.partial.then_some(37), content: "PASS".into(),
            input_tokens: if self.partial { 11 } else { 23 },
            output_tokens: if self.partial { 7 } else { 5 }, cache_read_tokens: 3,
            model_used: context.model.clone(), runtime_name: self.name().into(),
        })
    }
}

async fn measured_attempts(home: &Path, fallback: bool) {
    crate::cost_telemetry::init_telemetry(home).unwrap();
    let primary_calls = Arc::new(AtomicUsize::new(0));
    let fallback_calls = Arc::new(AtomicUsize::new(0));
    let mut runtimes: std::collections::HashMap<RuntimeType, Box<dyn AgentRuntime>> = std::collections::HashMap::new();
    runtimes.insert(RuntimeType::Codex, Box::new(MeasuredRuntime { partial: true, calls: primary_calls.clone() }));
    runtimes.insert(RuntimeType::Claude, Box::new(MeasuredRuntime { partial: false, calls: fallback_calls.clone() }));
    let _registry = runtime_dispatch::testing::RegistryGuard::install(home, RuntimeRegistry::with_runtimes(runtimes)).await;
    let settings = RuntimeSettings { provider: RuntimeType::Codex,
        fallback: fallback.then_some(RuntimeType::Claude), ..Default::default() };
    let result = crate::runtime::GOAL_ROUND_ATTRIBUTION.scope(
        GoalRoundAttribution { episode_id: "timeout-task".into(), round: Some(7) },
        crate::runtime::ROLE_COST_ATTRIBUTION.scope(
            RoleCostAttribution { role: "worker", episode_id: "timeout-task".into() },
            runtime_dispatch::run_agent_prompt_detailed(request(home, "worker", &settings)),
        ),
    ).await;
    assert_eq!(primary_calls.load(Ordering::SeqCst), 1);
    assert_eq!(fallback_calls.load(Ordering::SeqCst), usize::from(fallback));
    let usage = rows(home, "worker");
    assert_eq!(usage.len(), if fallback { 2 } else { 1 });
    assert_eq!(usage[0], ("dispatch".into(), "gpt-5.4".into(), 11, 7, 3,
        Some("worker".into()), Some("timeout-task".into()), Some(7)));
    if fallback {
        let response = result.unwrap();
        assert_eq!(response.runtime_name, "claude");
        assert_eq!(usage[1], ("dispatch".into(), response.model_used, 23, 5, 3,
            Some("worker".into()), Some("timeout-task".into()), Some(7)));
    } else {
        assert!(result.unwrap_err().legacy_message().contains("partial output discarded"));
    }
}

#[test]
fn partial_only_books_usage_once_before_error() {
    let Some(home) = isolated_home("partial_only_books_usage_once_before_error") else { return; };
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
        .block_on(measured_attempts(&home, false));
}

#[test]
fn partial_primary_and_successful_fallback_book_each_leg_once() {
    let Some(home) = isolated_home("partial_primary_and_successful_fallback_book_each_leg_once") else { return; };
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
        .block_on(measured_attempts(&home, true));
}

#[test]
fn codex_collected_timeout_books_discarded_and_displayed_usage_once() {
    let Some(home) = isolated_home("codex_collected_timeout_books_discarded_and_displayed_usage_once") else { return; };
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        crate::cost_telemetry::init_telemetry(&home).unwrap();
        for (agent, reply, completed, measured) in [
            ("discarded-text", false, true, true), ("discarded-no-text", false, false, true),
            ("reply-no-text", true, false, true), ("reply-text", true, true, true),
            ("unmeasured", false, false, false),
        ] {
            let call_home = home.join(agent);
            std::fs::create_dir(&call_home).unwrap();
            let mut runtimes: std::collections::HashMap<RuntimeType, Box<dyn AgentRuntime>> = std::collections::HashMap::new();
            runtimes.insert(RuntimeType::Codex, Box::new(CodexRuntime { codex_path: "/never-spawn-fixture".into() }));
            let _registry = runtime_dispatch::testing::RegistryGuard::install(&call_home, RuntimeRegistry::with_runtimes(runtimes)).await;
            let probe: Box<dyn Fn(&tokio::process::Command) -> Result<CodexOutput, String> + Send + Sync> = Box::new(move |_| {
                let mut output = CodexOutput::default();
                if completed {
                    output.stdout_bytes(b"{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"PASS\"}}\n");
                }
                if measured {
                    output.stdout_bytes(b"{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":31,\"output_tokens\":37}}\n");
                }
                Ok(output)
            });
            // The utility profile avoids all employee MCP/config preparation.
            // CHANNEL_REPLY_PARTIAL still loses to UTILITY_CALL, so only the
            // display case uses the normal producer with an agent-less context.
            let settings = RuntimeSettings::default();
            let mut req = request(&call_home, agent, &settings);
            req.request_type = if reply { RequestType::Chat } else { RequestType::Evolution };
            let call = super::tests::COLLECT_PROBE.scope(probe,
                runtime_dispatch::run_agent_prompt_detailed(req));
            let result = if reply {
                crate::runtime::ROLE_COST_ATTRIBUTION.scope(
                    RoleCostAttribution { role: "worker", episode_id: "collector-task".into() },
                    runtime_dispatch::CHANNEL_REPLY_PARTIAL.scope((), call),
                ).await
            } else { runtime_dispatch::UTILITY_CALL.scope((), call).await };
            if reply && completed {
                let response = result.unwrap();
                assert_eq!(response.content, "PASS");
                assert_eq!(response.partial_timeout_secs, Some(300));
            } else {
                let error = result.unwrap_err().legacy_message();
                assert!(error.contains(if reply { "no completed output" } else { "partial output discarded" }), "{error}");
            }
            let usage = rows(&home, agent);
            assert_eq!(usage.len(), usize::from(measured), "{agent}: {usage:?}");
            if measured {
                assert_eq!((&*usage[0].0, &*usage[0].1, usage[0].2, usage[0].3),
                    (if reply { "chat" } else { "evolution" }, "gpt-5.4", 31, 37));
            }
        }
    });
}
