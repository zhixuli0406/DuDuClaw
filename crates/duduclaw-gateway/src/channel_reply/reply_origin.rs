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
            summary: error.to_string(),
            runtime: Some(error),
            secondary: None,
        }
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
                        format!("{agent} 暫停回應：已達預算限制，請管理員檢查預算設定。")
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
                        origin_message(agent, origin, classify_origin_reason(origin, error), home)
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
        let primary = origin_message(
            agent,
            self.origin,
            classify_origin_reason(self.origin, &self.summary),
            home,
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
fn origin_message(agent: &str, origin: ReplyOrigin, reason: FailureReason, home: &Path) -> String {
    if origin.is_claude_cli() {
        return format_fallback_message(agent, reason, home);
    }
    let hint = match reason {
        FailureReason::AuthFailed => "認證失敗，請管理員檢查此後端的登入或 API 憑證設定。",
        FailureReason::BinaryMissing | FailureReason::SpawnError => {
            "無法啟動，請管理員檢查此後端的安裝與設定。"
        }
        FailureReason::RateLimited => "使用量已達限制，請稍後重試或檢查此後端配額。",
        FailureReason::Billing => "帳務或額度不足，請管理員檢查此後端帳務。",
        FailureReason::Timeout => "回應逾時，請稍後重試。",
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
/// Fail-closed spies at the actual legacy/direct call boundaries. They never
/// return mock answers or silently permit a real subprocess/provider call.
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
            let home = home.to_owned();
            let counts = Arc::new(Counts::default());
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
    pub(crate) fn boundary(home: &Path, direct: bool) {
        if let Some(counts) = entries().lock().unwrap().get(home).cloned() {
            if direct {
                counts.direct.fetch_add(1, Ordering::SeqCst);
            } else {
                counts.claude.fetch_add(1, Ordering::SeqCst);
            }
            panic!("owned test refused a real implicit cloud call before credential access");
        }
    }
}
