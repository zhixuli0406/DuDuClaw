//! Origin is assigned at the real call boundary, never guessed from errors.
use super::*;
use crate::failover::{RuntimeDispatchFailure, RuntimePreflightKind, RuntimeStepOutcome};
use duduclaw_core::types::RuntimeType;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ReplyOrigin {
    Runtime(RuntimeType),
    AnthropicApi,
    Local,
    Moa,
    Unknown,
}
impl ReplyOrigin {
    pub(super) fn runtime_name(name: &str) -> Self {
        let id = if name == "openai_compat_sse" {
            "openai_compat"
        } else {
            name
        };
        RuntimeType::from_id(id)
            .map(Self::Runtime)
            .unwrap_or(Self::Unknown)
    }
    pub(super) fn is_claude_cli(self) -> bool {
        self == Self::Runtime(RuntimeType::Claude)
    }
    pub(super) fn telemetry_id(self) -> &'static str {
        match self {
            Self::Runtime(runtime) => runtime.as_str(),
            Self::AnthropicApi => "anthropic",
            Self::Local => "local",
            Self::Moa => "moa",
            Self::Unknown => "unknown",
        }
    }
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Runtime(RuntimeType::OpenAiCompat) => "所選 API 後端",
            Self::Runtime(runtime) => runtime.spec().display_name,
            Self::AnthropicApi => "Anthropic API",
            Self::Local => "本地模型",
            Self::Moa => "MoA API",
            Self::Unknown => "所選執行環境",
        }
    }
}
pub(super) struct ReplyAttempt {
    pub(super) text: String,
    pub(super) origin: ReplyOrigin,
}
impl ReplyAttempt {
    pub(super) fn new(text: String, origin: ReplyOrigin) -> Self {
        Self { text, origin }
    }
}
/// Reason code of an engine host-configuration refusal (see
/// [`ReplyFailure::reason_code`]); snake_case like the v1.83.0 codes.
pub(super) const ENGINE_CONFIG_REFUSED_CODE: &str = "engine_config_refused";

#[derive(Clone)]
pub(super) struct ReplyFailure {
    pub(super) origin: ReplyOrigin,
    summary: String,
    runtime: Option<RuntimeDispatchFailure>,
    secondary: Option<(ReplyOrigin, String)>,
}
impl std::ops::Deref for ReplyFailure {
    type Target = str;
    fn deref(&self) -> &str {
        &self.summary
    }
}
impl ReplyFailure {
    pub(super) fn simple(origin: ReplyOrigin, error: String) -> Self {
        Self {
            origin,
            summary: error,
            runtime: None,
            secondary: None,
        }
    }
    pub(super) fn runtime(error: RuntimeDispatchFailure) -> Self {
        Self {
            origin: error
                .requested
                .map(ReplyOrigin::Runtime)
                .unwrap_or(ReplyOrigin::Unknown),
            summary: error.legacy_message(),
            runtime: Some(error),
            secondary: None,
        }
    }
    pub(super) fn stop_fallbacks(&self) -> bool {
        if self.summary == crate::on_behalf_of::access::MAIN_TURN_REFUSAL { return true; }
        if crate::work_outcome::is_work_result(&self.summary) { return true; }
        if self.spawn_gate_message().is_some() { return true; }
        if self.runtime.as_ref().is_some_and(|failure| failure.preflight.is_some()) { return true; }
        crate::budget::budget_refusal_message(&self.summary).is_some()
            || matches!(self.summary.as_str(), "cancelled" | "canceled" | "turn cancelled" | "turn canceled")
    }
    pub(super) fn retryable_empty(&self) -> bool {
        if self.stop_fallbacks() { return false; }
        if let Some(runtime) = &self.runtime {
            let mut empty = false;
            for step in &runtime.steps {
                match &step.outcome {
                    RuntimeStepOutcome::Empty => empty = true,
                    RuntimeStepOutcome::ExecutionError(error)
                        if error.starts_with("Empty response") || error == crate::claude_runner::LOCAL_THINK_ONLY_ERR => empty = true,
                    RuntimeStepOutcome::NotRegistered | RuntimeStepOutcome::Cooldown
                    | RuntimeStepOutcome::NoCompatibleModel | RuntimeStepOutcome::CrossFamilyRefused => {}
                    _ => return false,
                }
            }
            return empty;
        }
        self.summary.starts_with("Empty response")
            || self.summary == crate::claude_runner::LOCAL_THINK_ONLY_ERR
    }
    pub(super) fn append_direct_api_failure(&mut self, error: String) {
        self.secondary = Some((ReplyOrigin::AnthropicApi, error));
    }
    pub(super) fn spawn_gate_message(&self) -> Option<&str> {
        if let Some(runtime) = &self.runtime {
            runtime.steps.iter().find_map(|step| match &step.outcome {
                RuntimeStepOutcome::ExecutionError(error) => {
                    duduclaw_agent::mcp_spawn_gate::spawn_gate_user_message(error)
                }
                _ => None,
            })
        } else {
            duduclaw_agent::mcp_spawn_gate::spawn_gate_user_message(&self.summary)
        }
    }
    /// The user-visible reason code. A refusal of an ENGINE's own host
    /// configuration (Codex external configuration layers, Grok global MCP
    /// sources) has its own code instead of `unknown` (v1.84.0).
    pub(super) fn reason_code(&self) -> &'static str {
        let gate_error = match &self.runtime {
            Some(runtime) => runtime.steps.iter().find_map(|step| match &step.outcome {
                RuntimeStepOutcome::ExecutionError(error)
                    if duduclaw_agent::mcp_spawn_gate::is_spawn_gate_error(error) => Some(error.as_str()),
                _ => None,
            }),
            None => duduclaw_agent::mcp_spawn_gate::is_spawn_gate_error(&self.summary).then_some(self.summary.as_str()),
        };
        if gate_error.is_some_and(duduclaw_agent::mcp_spawn_gate::is_engine_local_spawn_gate_error) {
            return ENGINE_CONFIG_REFUSED_CODE;
        }
        self.reason().as_str()
    }
    pub(super) fn claude_auth_error(&self) -> Option<&str> {
        if let Some(runtime) = &self.runtime {
            runtime.steps.iter().find_map(|step| match &step.outcome {
                RuntimeStepOutcome::ExecutionError(error)
                    if step.runtime == RuntimeType::Claude
                        && classify_cli_failure(error) == FailureReason::AuthFailed =>
                {
                    Some(error.as_str())
                }
                _ => None,
            })
        } else if self.origin.is_claude_cli()
            && classify_cli_failure(&self.summary) == FailureReason::AuthFailed
        {
            Some(&self.summary)
        } else {
            None
        }
    }
    pub(super) fn display_origin(&self) -> ReplyOrigin {
        if let Some((origin, _)) = &self.secondary {
            return *origin;
        }
        self.runtime
            .as_ref()
            .and_then(|failure| {
                failure
                    .steps
                    .iter()
                    .rev()
                    .find(|step| {
                        matches!(
                            step.outcome,
                            RuntimeStepOutcome::ExecutionError(_) | RuntimeStepOutcome::Empty
                        )
                    })
                    .map(|step| ReplyOrigin::Runtime(step.runtime))
            })
            .unwrap_or(self.origin)
    }
    pub(super) fn notice_reason(&self) -> TurnFailureReason {
        if self.spawn_gate_message().is_some() { return TurnFailureReason::SafetyGate; }
        if let Some(preflight) = self.runtime.as_ref().and_then(|failure| failure.preflight.as_ref()) {
            return match preflight.kind {
                RuntimePreflightKind::Config => TurnFailureReason::Unavailable,
                RuntimePreflightKind::Policy => TurnFailureReason::SafetyGate,
                // BudgetUnavailable must not claim that the quota was exhausted.
                RuntimePreflightKind::Budget => TurnFailureReason::Unknown,
            };
        }
        self.reason().into()
    }

    pub(super) fn reason(&self) -> FailureReason {
        if let Some((origin, error)) = &self.secondary {
            return classify_origin_reason(*origin, error);
        }
        let error = self
            .runtime
            .as_ref()
            .and_then(|failure| {
                failure
                    .steps
                    .iter()
                    .rev()
                    .find_map(|step| match &step.outcome {
                        RuntimeStepOutcome::ExecutionError(error) => Some(error.as_str()),
                        RuntimeStepOutcome::Empty => Some("Empty response"),
                        _ => None,
                    })
            })
            .unwrap_or(&self.summary);
        classify_origin_reason(self.display_origin(), error)
    }
    /// Metadata-only log/audit representation; raw provider bodies stay inside
    /// their individual typed errors and never reach a channel or log here.
    pub(super) fn audit(&self) -> serde_json::Value {
        let steps = self.runtime.as_ref().map(|failure| {
            let mut values = failure.audit_steps();
            for (value, step) in values
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .zip(&failure.steps)
            {
                value["reason"] = match &step.outcome {
                    RuntimeStepOutcome::ExecutionError(error) => serde_json::json!(format!(
                        "{:?}",
                        classify_origin_reason(ReplyOrigin::Runtime(step.runtime), error)
                    )),
                    RuntimeStepOutcome::Empty => serde_json::json!("EmptyResponse"),
                    _ => serde_json::Value::Null,
                };
            }
            values
        });
        serde_json::json!({
            "origin":self.origin.label(),"reason":format!("{:?}",self.reason()),
            "preflight":self.runtime.as_ref().and_then(|f|f.preflight.as_ref()).map(|p|format!("{:?}",p.kind)),
            "steps":steps,
            "fallback_skipped":self.runtime.as_ref().and_then(|f|f.fallback_skipped.as_ref())
                .map(|s|serde_json::json!({"runtime":s.runtime.as_str(),"reason":s.reason.token()})),
            "secondary":self.secondary.as_ref().map(|(origin,error)|serde_json::json!({"origin":origin.label(),"reason":format!("{:?}",classify_origin_reason(*origin,error))})),
        })
    }
    pub(super) fn message(&self, agent: &str, home: &Path) -> String {
        if let Some(runtime) = &self.runtime {
            if let Some(preflight) = &runtime.preflight {
                return match preflight.kind {
                    RuntimePreflightKind::Config => format!(
                        "{agent} 無法回應：模型與執行環境設定不一致或無效，請管理員儲存修正後重試。"
                    ),
                    RuntimePreflightKind::Budget => {
                        let unavailable = crate::budget::BudgetVerdict::Unavailable { reason: String::new() }.user_message();
                        if preflight.error == unavailable {
                            format!("{agent} 暫停回應：目前無法確認預算，請管理員檢查預算設定或花費紀錄後重試。")
                        } else {
                            format!("{agent} 暫停回應：已達預算限制，請管理員檢查預算設定。")
                        }
                    }
                    RuntimePreflightKind::Policy => {
                        format!("{agent} 無法回應：目前產品政策不允許此執行環境或模型。")
                    }
                };
            }
            let mut messages = Vec::new();
            for step in &runtime.steps {
                let origin = ReplyOrigin::Runtime(step.runtime);
                let body = match &step.outcome {
                    RuntimeStepOutcome::ExecutionError(error) => {
                        origin_message_with_applied_timeout(agent, origin, classify_origin_reason(origin, error), home, applied_claude_timeout(error))
                    }
                    RuntimeStepOutcome::Empty => {
                        origin_message(agent, origin, FailureReason::EmptyResponse, home)
                    }
                    RuntimeStepOutcome::NotRegistered => format!(
                        "{} 尚未註冊或不可用，請管理員檢查安裝與設定。",
                        origin.label()
                    ),
                    RuntimeStepOutcome::Cooldown => {
                        format!("{} 正在冷卻，這次沒有呼叫。", origin.label())
                    }
                    RuntimeStepOutcome::PolicyRefused(_) => {
                        format!("{} 未呼叫：產品政策拒絕。", origin.label())
                    }
                    RuntimeStepOutcome::NoCompatibleModel => {
                        format!("{} 未呼叫：沒有相容的備援模型。", origin.label())
                    }
                    RuntimeStepOutcome::CrossFamilyRefused => {
                        format!("{} 未呼叫：此工作禁止跨模型家族備援。", origin.label())
                    }
                };
                let heading = match step.role {
                    crate::failover::RuntimeStepRole::Primary => "原選執行環境",
                    crate::failover::RuntimeStepRole::Fallback => "明確備援",
                };
                messages.push(format!("{heading}：{body}"));
            }
            return messages.join("\n\n");
        }
        let primary = origin_message_with_applied_timeout(
            agent,
            self.origin,
            classify_origin_reason(self.origin, &self.summary),
            home,
            applied_claude_timeout(&self.summary),
        );
        if let Some((origin, error)) = &self.secondary {
            format!(
                "{primary}\n\nAPI 備援：{}",
                origin_message(agent, *origin, classify_origin_reason(*origin, error), home)
            )
        } else {
            primary
        }
    }
    /// RM-181: which engines were tried, in order, with each outcome in plain
    /// words, and — when a fallback engine is configured but the rule did
    /// not use it — that and why. No provider text, file names or accounts,
    /// so it is safe for operators and members (never for public visitors).
    /// `None` outside the multi-runtime dispatch path (nothing to list).
    pub(super) fn engine_summary(&self) -> Option<String> {
        let runtime = self.runtime.as_ref()?;
        let mut text = String::new();
        if !runtime.steps.is_empty() {
            let legs: Vec<String> = runtime.steps.iter().map(|step| {
                let role = match step.role {
                    crate::failover::RuntimeStepRole::Primary => "原選",
                    crate::failover::RuntimeStepRole::Fallback => "備援",
                };
                format!("{}（{role}）：{}", ReplyOrigin::Runtime(step.runtime).label(), plain_step_outcome(step))
            }).collect();
            text.push_str(&format!("已嘗試的引擎依序為 {}。", legs.join("；")));
        }
        if let Some(skip) = &runtime.fallback_skipped {
            text.push_str(&skip.sentence());
        }
        (!text.is_empty()).then_some(text)
    }
    /// [`Self::engine_summary`] for the audience of `session_id`: never for a
    /// public visitor (website live chat, widget visitor), whatever the
    /// failure. Every reply path that may append the summary goes through
    /// this one gate.
    pub(super) fn engine_summary_for_session(&self, session_id: &str) -> Option<String> {
        if super::assembly::public_session(session_id) {
            return None;
        }
        self.engine_summary()
    }
    pub(super) fn local_notice(&self) -> String {
        let origin = if let Some(runtime) = &self.runtime {
            if runtime.steps.len() == 2 {
                format!(
                    "原選 {} 未能回應，明確備援 {} 也未能接手",
                    ReplyOrigin::Runtime(runtime.steps[0].runtime).label(),
                    ReplyOrigin::Runtime(runtime.steps[1].runtime).label()
                )
            } else {
                format!("{} 本次未能回應", self.origin.label())
            }
        } else {
            format!("{} 本次未能回應", self.origin.label())
        };
        format!("⚠️ {origin}，改由已設定的本地模型代為回應。\n\n")
    }
}
/// One leg's outcome in plain words (no provider text).
fn plain_step_outcome(step: &crate::failover::RuntimeStep) -> &'static str {
    match &step.outcome {
        RuntimeStepOutcome::ExecutionError(error) => {
            if duduclaw_agent::mcp_spawn_gate::is_spawn_gate_error(error) {
                return "沒有通過啟動前檢查";
            }
            match classify_origin_reason(ReplyOrigin::Runtime(step.runtime), error) {
                FailureReason::AuthFailed => "登入或憑證失效",
                FailureReason::Timeout | FailureReason::TimeoutTotal => "達到執行時間上限",
                FailureReason::TimeoutNoProgress => "長時間沒有進展而停止",
                FailureReason::MaxTurns => "達到步驟上限",
                FailureReason::RateLimited => "使用量已達限制",
                FailureReason::Billing => "帳務或額度不足",
                FailureReason::EmptyResponse => "回傳空白內容",
                FailureReason::BinaryMissing | FailureReason::SpawnError => "無法啟動",
                _ => "執行失敗",
            }
        }
        RuntimeStepOutcome::Empty => "回傳空白內容",
        RuntimeStepOutcome::NotRegistered => "尚未安裝或無法使用，沒有呼叫",
        RuntimeStepOutcome::Cooldown => "正在冷卻，沒有呼叫",
        RuntimeStepOutcome::PolicyRefused(_) => "產品政策不允許，沒有呼叫",
        RuntimeStepOutcome::NoCompatibleModel => "沒有可用的相容模型，沒有呼叫",
        RuntimeStepOutcome::CrossFamilyRefused => "此工作不允許換到其他模型家族，沒有呼叫",
    }
}
fn classify_origin_reason(origin: ReplyOrigin, error: &str) -> FailureReason {
    if !origin.is_claude_cli() {
        let lower = error.to_ascii_lowercase();
        if lower.contains("401 unauthorized")
            || lower.starts_with("401 ")
            || lower.contains("invalid_api_key")
        {
            return FailureReason::AuthFailed;
        }
    }
    classify_cli_failure(error)
}
// The process timeout carries the captured run limit; do not re-read a
// setting that may have changed while that run was executing.
fn applied_claude_timeout(error: &str) -> Option<u64> {
    let (_, rest) = error.split_once("claude CLI hard timeout (")?;
    let (seconds, _) = rest.split_once("s,")?;
    seconds.parse::<u64>().ok().filter(|n| (30..=14_400).contains(n))
}

fn origin_message(agent: &str, origin: ReplyOrigin, reason: FailureReason, home: &Path) -> String {
    origin_message_with_applied_timeout(agent, origin, reason, home, None)
}

fn origin_message_with_applied_timeout(
    agent: &str, origin: ReplyOrigin, reason: FailureReason, home: &Path, applied: Option<u64>,
) -> String {
    // RM-155: a timeout while macOS folder access is pending/denied is the
    // consent prompt nobody answered, not a slow model — say so.
    if reason == FailureReason::Timeout {
        if let Some(text) = crate::macos_folder_access::channel_timeout_explanation(home, agent, None) {
            return text;
        }
    }
    if origin.is_claude_cli() {
        return match applied {
            Some(seconds) => super::failure_message::format_fallback_message_with_timeout(agent, reason, home, seconds),
            None => format_fallback_message(agent, reason, home),
        };
    }
    let hint = match reason {
        FailureReason::AuthFailed => "認證失敗，請管理員檢查此後端的登入或 API 憑證設定。",
        FailureReason::BinaryMissing | FailureReason::SpawnError => {
            "無法啟動，請管理員檢查此後端的安裝與設定。"
        }
        FailureReason::RateLimited => "使用量已達限制，請稍後重試或檢查此後端配額。",
        FailureReason::Billing => "帳務或額度不足，請管理員檢查此後端帳務。",
        FailureReason::Timeout if matches!(origin, ReplyOrigin::Runtime(_)) => {
            "回應逾時，請管理員在員工設定調整時間上限。"
        }
        FailureReason::Timeout => "回應逾時，請稍後重試或將任務拆成較小的步驟。",
        FailureReason::EmptyResponse => "回傳空白內容，請稍後重試。",
        _ => "暫時無法回應，請管理員查看此後端的診斷紀錄。",
    };
    format!("{agent} 無法回應：{}{hint}", origin.label())
}
pub(super) fn direct_api_allowed(
    non_claude: Option<RuntimeType>,
    moa: bool,
    spawn_gate: bool,
) -> bool {
    non_claude.is_none() && !moa && !spawn_gate
}

pub(super) async fn record_reply_recovery(origin: ReplyOrigin, home: &Path, agent: &str) {
    if origin.is_claude_cli() {
        crate::auth_outage::record_recovery(
            home,
            if agent.trim().is_empty() {
                "system"
            } else {
                agent
            },
        )
        .await;
    }
}
/// Fail-closed spies at the actual legacy/direct call boundaries. The explicit
/// CLI mode permits an isolated fake CLI but still refuses Direct API access.
#[cfg(test)]
pub(super) mod testing {
    use super::*;
    use std::sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicUsize, Ordering},
    };
    #[derive(Default)]
    struct Counts {
        claude: AtomicUsize,
        direct: AtomicUsize,
        allow_claude: bool,
    }
    fn entries() -> &'static Mutex<std::collections::HashMap<std::path::PathBuf, Arc<Counts>>> {
        static MAP: OnceLock<Mutex<std::collections::HashMap<std::path::PathBuf, Arc<Counts>>>> =
            OnceLock::new();
        MAP.get_or_init(Default::default)
    }
    pub(crate) struct NoImplicitCloud {
        home: std::path::PathBuf,
        counts: Arc<Counts>,
    }
    impl NoImplicitCloud {
        pub(crate) fn at(home: &Path) -> Self {
            Self::register(home, false)
        }
        pub(crate) fn allowing_claude_at(home: &Path) -> Self {
            Self::register(home, true)
        }
        fn register(home: &Path, allow_claude: bool) -> Self {
            let home = home.to_owned();
            let counts = Arc::new(Counts { allow_claude, ..Default::default() });
            assert!(
                entries()
                    .lock()
                    .unwrap()
                    .insert(home.clone(), counts.clone())
                    .is_none()
            );
            Self { home, counts }
        }
        pub(crate) fn counts(&self) -> [usize; 2] {
            [
                self.counts.claude.load(Ordering::SeqCst),
                self.counts.direct.load(Ordering::SeqCst),
            ]
        }
    }
    impl Drop for NoImplicitCloud {
        fn drop(&mut self) {
            let mut map = entries().lock().unwrap();
            assert!(
                map.get(&self.home)
                    .is_some_and(|c| Arc::ptr_eq(c, &self.counts))
            );
            map.remove(&self.home);
        }
    }
    pub(crate) fn before_direct_api_credentials(home: &Path) {
        let counts = entries().lock().unwrap().get(home).cloned();
        // Strict spies still stop before credential access. The fake-CLI mode
        // allows checking for a key, but never the actual Direct API call.
        if counts.is_some_and(|counts| !counts.allow_claude) {
            boundary(home, true);
        }
    }
    pub(crate) fn boundary(home: &Path, direct: bool) {
        // Release the map lock before a refusal panics so Drop can unregister.
        let counts = entries().lock().unwrap().get(home).cloned();
        if let Some(counts) = counts {
            if direct {
                counts.direct.fetch_add(1, Ordering::SeqCst);
            } else {
                counts.claude.fetch_add(1, Ordering::SeqCst);
                if counts.allow_claude { return; }
            }
            panic!("owned test refused a real implicit cloud call before credential access");
        }
    }
}

#[cfg(test)]
mod timeout_tests {
    use super::*;

    #[test]
    fn settings_hint_only_for_runtimes_with_configurable_timeout() {
        for origin in [ReplyOrigin::AnthropicApi, ReplyOrigin::Local, ReplyOrigin::Moa, ReplyOrigin::Unknown] {
            let message = origin_message("worker", origin, FailureReason::Timeout, Path::new("/nonexistent"));
            assert!(!message.contains("員工設定"), "{origin:?}");
        }
        for runtime in RuntimeType::ALL {
            let message = origin_message("worker", ReplyOrigin::Runtime(*runtime), FailureReason::Timeout, Path::new("/nonexistent"));
            if *runtime == RuntimeType::Claude {
                assert!(message.contains("30 分鐘安全上限"));
                assert!(!message.contains("員工設定"));
            } else {
                assert!(message.contains("員工設定"), "{runtime:?}");
            }
        }
    }

    #[test]
    fn configured_claude_timeout_and_work_deadline_use_their_respective_notices() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("agents/worker");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agent.toml"), "[runtime]\nprovider='claude'\ntimeout_secs=900\n").unwrap();
        let origin = ReplyOrigin::Runtime(RuntimeType::Claude);
        let configured = origin_message("worker", origin, FailureReason::Timeout, home.path());
        assert!(configured.contains("15 分鐘安全上限"), "{configured}");
        assert!(configured.contains("請洽管理員"));
        assert!(!configured.contains("員工設定"));
        // A classified work deadline names the captured limit, not the current setting.
        let failure = ReplyFailure::simple(origin, "claude CLI hard timeout (600s, no output)".into());
        let message = failure.message("worker", home.path());
        assert_eq!(failure.reason(), FailureReason::TimeoutTotal);
        assert!(message.contains("10 分鐘"), "{message}");
        assert!(!message.contains("15 分鐘"));
        assert!(message.contains("需要延長上限時，請洽管理員。"));
        let failure = ReplyFailure::simple(origin, "claude CLI hard timeout (95s, no output)".into());
        let message = failure.message("worker", home.path());
        assert_eq!(failure.reason(), FailureReason::TimeoutTotal);
        assert!(message.contains("95 秒"), "{message}");
    }

    /// RM-155: while macOS folder access is pending, a timeout names the
    /// consent prompt instead of the generic 30-minute text; other failures
    /// and other homes are untouched.
    #[test]
    fn timeout_names_the_pending_macos_prompt() {
        use crate::macos_folder_access::{AccessState, registry_for};
        let home = tempfile::tempdir().unwrap();
        registry_for(home.path()).set_states_for_test(std::collections::BTreeMap::from([(
            std::path::PathBuf::from("/Volumes/NAS"),
            AccessState::PendingPrompt,
        )]));
        let claude = ReplyOrigin::Runtime(RuntimeType::Claude);
        let message = origin_message("worker", claude, FailureReason::Timeout, home.path());
        assert!(message.contains("macOS") && message.contains("允許"), "{message}");
        assert!(!message.contains("30 分鐘安全上限"));
        let other = origin_message("worker", claude, FailureReason::EmptyResponse, home.path());
        assert!(!other.contains("macOS"), "{other}");
        let elsewhere = origin_message("worker", claude, FailureReason::Timeout, Path::new("/nonexistent"));
        assert!(elsewhere.contains("30 分鐘安全上限"));
    }
}

#[cfg(test)]
mod engine_summary_tests {
    use super::*;
    use crate::failover::{FallbackRefusal, FallbackSkip, RuntimeStepRole, WorkLimitKind};

    #[test]
    fn fallback_configured_but_skipped_says_so_and_why() {
        let outcome = crate::work_outcome::WorkOutcome::TimeoutTotal { seconds: 1800, partial: String::new() }.encode();
        let mut failure = RuntimeDispatchFailure::new(RuntimeType::Codex, "gpt-5.4");
        failure.push(RuntimeStepRole::Primary, RuntimeType::Codex, Some("gpt-5.4"),
            RuntimeStepOutcome::ExecutionError(outcome));
        failure.fallback_skipped = Some(FallbackSkip { runtime: RuntimeType::Grok,
            reason: FallbackRefusal::WorkLimit(WorkLimitKind::TimeoutTotal) });
        let failure = ReplyFailure::runtime(failure);
        let summary = failure.engine_summary().expect("summary");
        let codex = ReplyOrigin::Runtime(RuntimeType::Codex).label();
        let grok = ReplyOrigin::Runtime(RuntimeType::Grok).label();
        assert!(summary.starts_with(&format!("已嘗試的引擎依序為 {codex}（原選）：達到執行時間上限。")), "{summary}");
        assert!(summary.ends_with(&format!("已設定備援引擎 {grok}，但這次的失敗屬於任務本身的時間上限，不會換引擎重跑。")), "{summary}");
        assert_eq!(failure.audit()["fallback_skipped"]["reason"], "work_timeout_total");
        // A fallback that was never configured is never mentioned.
        let mut plain = RuntimeDispatchFailure::new(RuntimeType::Codex, "gpt-5.4");
        plain.push(RuntimeStepRole::Primary, RuntimeType::Codex, Some("gpt-5.4"),
            RuntimeStepOutcome::ExecutionError("500 secret provider body".into()));
        let summary = ReplyFailure::runtime(plain).engine_summary().unwrap();
        assert!(!summary.contains("備援") && !summary.contains("secret"), "{summary}");
    }

    #[test]
    fn both_legs_failed_lists_each_engine_in_order_in_plain_words() {
        let mut failure = RuntimeDispatchFailure::new(RuntimeType::Codex, "gpt-5.4");
        failure.push(RuntimeStepRole::Primary, RuntimeType::Codex, Some("gpt-5.4"),
            RuntimeStepOutcome::ExecutionError("401 invalid bearer token; secret body".into()));
        failure.push(RuntimeStepRole::Fallback, RuntimeType::Grok, Some("grok-build-0.1"), RuntimeStepOutcome::Empty);
        let failure = ReplyFailure::runtime(failure);
        let summary = failure.engine_summary().unwrap();
        let codex = ReplyOrigin::Runtime(RuntimeType::Codex).label();
        let grok = ReplyOrigin::Runtime(RuntimeType::Grok).label();
        assert_eq!(summary, format!("已嘗試的引擎依序為 {codex}（原選）：登入或憑證失效；{grok}（備援）：回傳空白內容。"));
        assert!(!summary.contains("secret") && !summary.contains("401"));
        // The operator text still carries each leg's own advice.
        let message = failure.message("Worker", Path::new("/nonexistent"));
        assert!(message.contains("原選執行環境") && message.contains("明確備援"), "{message}");
        // A Claude-path failure has no engine list to add.
        assert!(ReplyFailure::simple(ReplyOrigin::Runtime(RuntimeType::Claude), "boom".into()).engine_summary().is_none());
    }

    /// Visitor sessions never receive the engine summary, for any failure
    /// shape (both legs failed, a skipped fallback, a start-up refusal, a
    /// prompt too large); members and operators do.
    #[test]
    fn visitor_sessions_never_receive_the_engine_summary() {
        let gate = duduclaw_agent::mcp_spawn_gate::engine_local_spawn_gate_error("Codex 外部設定 /etc/codex/config.toml：x");
        let too_large = crate::runtime::prompt_budget::prompt_too_large_error("grok");
        let failures = || {
            let mut both = RuntimeDispatchFailure::new(RuntimeType::Codex, "gpt-5.4");
            both.push(RuntimeStepRole::Primary, RuntimeType::Codex, Some("gpt-5.4"),
                RuntimeStepOutcome::ExecutionError("500 internal server error".into()));
            both.push(RuntimeStepRole::Fallback, RuntimeType::Grok, Some("grok-build-0.1"), RuntimeStepOutcome::Empty);
            let mut skipped = RuntimeDispatchFailure::new(RuntimeType::Grok, "grok-4");
            skipped.push(RuntimeStepRole::Primary, RuntimeType::Grok, Some("grok-4"),
                RuntimeStepOutcome::ExecutionError(too_large.clone()));
            skipped.fallback_skipped = Some(FallbackSkip { runtime: RuntimeType::Qwen, reason: FallbackRefusal::PromptTooLarge });
            let mut refused = RuntimeDispatchFailure::new(RuntimeType::Codex, "gpt-5.4");
            refused.push(RuntimeStepRole::Primary, RuntimeType::Codex, Some("gpt-5.4"), RuntimeStepOutcome::ExecutionError(gate.clone()));
            [both, skipped, refused].map(ReplyFailure::runtime)
        };
        for session in ["widget-visitor:abc", "odoolivechat:site:visitor"] {
            assert!(super::super::assembly::public_session(session), "{session} is a visitor session");
            for failure in failures() {
                assert!(failure.engine_summary().is_some(), "fixture has a summary");
                assert_eq!(failure.engine_summary_for_session(session), None, "{session}");
            }
        }
        for session in ["discord:member", "webchat:owner", "line:owner"] {
            for failure in failures() {
                assert_eq!(failure.engine_summary_for_session(session), failure.engine_summary(), "{session}");
            }
        }
    }

    #[test]
    fn a_spawn_gate_leg_is_named_without_its_text() {
        let gate = duduclaw_agent::mcp_spawn_gate::engine_local_spawn_gate_error("Codex 外部設定 /etc/codex/config.toml：x");
        let mut failure = RuntimeDispatchFailure::new(RuntimeType::Codex, "gpt-5.4");
        failure.push(RuntimeStepRole::Primary, RuntimeType::Codex, Some("gpt-5.4"), RuntimeStepOutcome::ExecutionError(gate));
        failure.push(RuntimeStepRole::Fallback, RuntimeType::Claude, Some("claude-opus-4-6"),
            RuntimeStepOutcome::ExecutionError("503 service unavailable".into()));
        let summary = ReplyFailure::runtime(failure).engine_summary().unwrap();
        assert!(summary.contains("沒有通過啟動前檢查"), "{summary}");
        assert!(!summary.contains("config.toml") && !summary.contains("[engine_local_config]"), "{summary}");
    }

    /// v1.84.0: an engine host-configuration refusal has its own reason code
    /// (it used to show `unknown`); the employee `.mcp.json` gate and every
    /// other failure keep their v1.83.0 code.
    #[test]
    fn engine_host_config_refusal_has_its_own_reason_code() {
        let gate = duduclaw_agent::mcp_spawn_gate::engine_local_spawn_gate_error(
            "Codex 外部設定 /home/u/.codex/config.toml：定義了 mcp_servers；拒絕啟動。");
        let mut failure = RuntimeDispatchFailure::new(RuntimeType::Codex, "gpt-5.4");
        failure.push(RuntimeStepRole::Primary, RuntimeType::Codex, Some("gpt-5.4"), RuntimeStepOutcome::ExecutionError(gate.clone()));
        assert_eq!(ReplyFailure::runtime(failure).reason_code(), ENGINE_CONFIG_REFUSED_CODE);
        assert_eq!(ReplyFailure::simple(ReplyOrigin::Runtime(RuntimeType::Codex), gate).reason_code(), ENGINE_CONFIG_REFUSED_CODE);
        let employee = duduclaw_agent::mcp_spawn_gate::spawn_gate_error("worker .mcp.json refused");
        let employee = ReplyFailure::simple(ReplyOrigin::Runtime(RuntimeType::Codex), employee);
        assert_eq!(employee.reason_code(), employee.reason().as_str());
        let plain = ReplyFailure::simple(ReplyOrigin::Runtime(RuntimeType::Codex), "Codex CLI exited with 1".into());
        assert_eq!(plain.reason_code(), plain.reason().as_str());
    }

    /// v1.84.0: the stored notice is the sentence shown live (sanitized the
    /// same way), with the same reason code; visitors keep the generic line.
    #[test]
    fn stored_refusal_notice_is_the_sentence_shown_live() {
        use super::super::failure_notice::{FAILURE_DETAILS, FAILURE_SHOWN_TEXT, notice_suffix};
        let shown = crate::dispatcher::sanitize_for_channel(
            "Codex 外部設定 /Users/sam/.codex/work.config.toml：定義了 mcp_servers；拒絕啟動。");
        assert!(shown.contains("：定義了 mcp_servers；拒絕啟動"), "{shown}");
        let render = |visitor: bool| FAILURE_SHOWN_TEXT.sync_scope(shown.clone(), || {
            FAILURE_DETAILS.sync_scope((ENGINE_CONFIG_REFUSED_CODE, None), || {
                TurnFailureReason::SafetyGate.render(Some("zh-TW"), "12345678abcdef", visitor)
            })
        });
        assert_eq!(render(false), format!("{shown}\n{}",
            notice_suffix(Some("zh-TW"), Some(ENGINE_CONFIG_REFUSED_CODE), "12345678abcdef")));
        let visitor = render(true);
        assert!(!visitor.contains("mcp_servers") && !visitor.contains(ENGINE_CONFIG_REFUSED_CODE), "{visitor}");
        // Without the scope the generic line is unchanged.
        assert!(TurnFailureReason::SafetyGate.render(Some("zh-TW"), "1", false).starts_with("本次未收到回覆，因為安全檢查已停止這次請求。"));
    }
}

#[cfg(test)]
mod budget_message_tests {
    use super::*;

    #[test]
    fn budget_unavailable_never_claims_cap_reached_or_echoes_untrusted_error() {
        let unavailable = crate::budget::BudgetVerdict::Unavailable { reason: "internal secret".into() };
        let failure = ReplyFailure::runtime(RuntimeDispatchFailure::preflight(Some(RuntimeType::Claude),
            "claude-sonnet-4-6", RuntimePreflightKind::Budget, unavailable.user_message()));
        let message = failure.message("worker", Path::new("/nonexistent"));
        assert!(message.contains("無法確認預算"));
        assert!(!message.contains("已達") && !message.contains("secret"));
        let failure = ReplyFailure::runtime(RuntimeDispatchFailure::preflight(Some(RuntimeType::Claude),
            "claude-sonnet-4-6", RuntimePreflightKind::Budget, "provider secret".into()));
        assert!(!failure.message("worker", Path::new("/nonexistent")).contains("secret"));
    }
}
