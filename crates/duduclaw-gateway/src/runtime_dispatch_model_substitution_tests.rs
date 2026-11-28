//! v1.84.0 Step A: a bare Claude alias saved on a non-Claude engine that
//! declares model prefixes runs with that engine's catalog default instead of
//! being refused at preflight. Full `claude-*` names keep the refusal; Claude
//! itself and familyless runtimes are untouched.

use super::*;
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
struct Seen(Arc<Mutex<Vec<String>>>);

struct Recorder {
    name: &'static str,
    seen: Seen,
}

#[async_trait::async_trait]
impl crate::runtime::AgentRuntime for Recorder {
    fn name(&self) -> &str { self.name }
    async fn is_available(&self) -> bool { true }
    async fn execute(&self, _prompt: &str, context: &RuntimeContext) -> Result<RuntimeResponse, String> {
        self.seen.0.lock().unwrap().push(context.model.clone());
        Ok(RuntimeResponse {
            content: "ok".into(),
            model_used: context.model.clone(),
            runtime_name: self.name.into(),
            ..Default::default()
        })
    }
}

async fn dispatch(provider: RuntimeType, provider_id: &'static str, agent: &str, model: &str)
    -> (Result<RuntimeResponse, RuntimeDispatchFailure>, Vec<String>) {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join("agents").join(agent);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("agent.toml"), format!(
        "[agent]\nname = \"{agent}\"\nstatus = \"active\"\n[runtime]\nprovider = \"{provider_id}\"\n[model]\npreferred = \"{model}\"\n"
    )).unwrap();
    let seen = Seen::default();
    let mut runtimes: HashMap<RuntimeType, Box<dyn crate::runtime::AgentRuntime>> = HashMap::new();
    runtimes.insert(provider, Box::new(Recorder { name: provider_id, seen: seen.clone() }));
    let _registry = testing::RegistryGuard::install(home.path(), RuntimeRegistry::with_runtimes(runtimes)).await;
    let result = run_agent_prompt_detailed(AgentPrompt {
        agent_dir: Some(&dir),
        home_dir: home.path(),
        agent_id: agent,
        prompt: "hi",
        system_prompt: "",
        model,
        max_tokens: 8,
        provider_override: None,
        conversation_history: &[],
        request_type: RequestType::Chat,
        runtime_settings: None,
        effort: None,
        allow_cross_family_failover: false,
        purpose: SpawnPurpose::EmployeeTurn,
    })
    .await;
    let seen = seen.0.lock().unwrap().clone();
    (result, seen)
}

#[tokio::test]
async fn codex_with_a_bare_claude_alias_runs_on_the_codex_default() {
    let default = RuntimeType::Codex.spec().fallback_models.first().unwrap().0;
    let (result, seen) = dispatch(RuntimeType::Codex, "codex", "subst-codex-opus", "opus").await;
    assert!(result.is_ok(), "{:?}", result.err().map(|e| e.legacy_message()));
    assert_eq!(seen, vec![default.to_string()], "the engine runs its catalog default, never `-m opus`");
    let notice = crate::runtime_config::model_substitution_notice("subst-codex-opus").expect("surfaced once");
    assert_eq!(notice, "這位 AI 員工設定的模型（opus）不屬於它的執行引擎（ChatGPT），已改用該引擎的預設模型。請到員工設定重新選擇模型。");
    assert!(crate::runtime_config::model_substitution_notices().iter().any(|(a, _)| a == "subst-codex-opus"));
}

#[tokio::test]
async fn codex_with_a_full_claude_name_is_still_refused() {
    let (result, seen) = dispatch(RuntimeType::Codex, "codex", "subst-codex-full", "claude-opus-4-6").await;
    let error = result.err().expect("refused at preflight").legacy_message();
    assert_eq!(crate::entry_gate::background_refusal_code(&error), Some("model_provider_mismatch"), "{error}");
    assert!(seen.is_empty(), "nothing started");
    assert!(crate::runtime_config::model_substitution_notice("subst-codex-full").is_none());
}

#[tokio::test]
async fn claude_and_openai_compat_keep_the_alias_untouched() {
    for (provider, id, agent) in [
        (RuntimeType::Claude, "claude", "subst-claude-opus"),
        (RuntimeType::OpenAiCompat, "openai_compat", "subst-compat-opus"),
    ] {
        let (result, seen) = dispatch(provider, id, agent, "opus").await;
        assert!(result.is_ok(), "{id}: {:?}", result.err().map(|e| e.legacy_message()));
        assert_eq!(seen, vec!["opus".to_string()], "{id}");
        assert!(crate::runtime_config::model_substitution_notice(agent).is_none(), "{id}");
    }
}

#[test]
fn substitution_table() {
    use crate::runtime_config::bare_claude_alias_substitute as sub;
    let codex_default = RuntimeType::Codex.spec().fallback_models.first().map(|m| m.0);
    let grok_default = RuntimeType::Grok.spec().fallback_models.first().map(|m| m.0);
    assert_eq!(sub("opus", RuntimeType::Codex), codex_default);
    assert_eq!(sub(" Sonnet ", RuntimeType::Grok), grok_default);
    assert_eq!(sub("claude-opus-4-6", RuntimeType::Codex), None);
    assert_eq!(sub("opus-foo", RuntimeType::Codex), None);
    assert_eq!(sub("opus", RuntimeType::Claude), None);
    assert_eq!(sub("opus", RuntimeType::OpenAiCompat), None);
    // The pure preflight check itself is unchanged.
    assert!(crate::runtime_config::ensure_model_provider("opus", RuntimeType::Codex).is_err());
}
