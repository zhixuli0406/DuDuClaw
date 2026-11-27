//! Failure provenance between manager and dispatch. Raw error bodies are
//! owner-internal, not serialized to channel/audit output.
use crate::runtime::RuntimeType;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeStepRole {
    Primary,
    Fallback,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeStepOutcome {
    ExecutionError(String),
    Empty,
    NotRegistered,
    Cooldown,
    PolicyRefused(String),
    NoCompatibleModel,
    CrossFamilyRefused,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeStep {
    pub role: RuntimeStepRole,
    pub runtime: RuntimeType,
    pub model: Option<String>,
    pub outcome: RuntimeStepOutcome,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimePreflightKind {
    Config,
    Budget,
    Policy,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimePreflightFailure {
    pub kind: RuntimePreflightKind,
    pub error: String,
}
/// RM-181: a fallback engine was configured but deliberately not used,
/// and why (see [`super::fallback_rule`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FallbackSkip {
    pub runtime: RuntimeType,
    pub reason: super::fallback_rule::FallbackRefusal,
}
impl FallbackSkip {
    /// 「已設定備援引擎 X，但…。」 — plain words, no internal names.
    pub(crate) fn sentence(&self) -> String {
        let label = match self.runtime {
            RuntimeType::OpenAiCompat => "所選 API 後端",
            runtime => runtime.spec().display_name,
        };
        format!("已設定備援引擎 {label}，但{}。", self.reason.zh_tw())
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeDispatchFailure {
    pub requested: Option<RuntimeType>,
    pub requested_model: String,
    pub preflight: Option<RuntimePreflightFailure>,
    pub steps: Vec<RuntimeStep>,
    /// Set when a fallback engine is configured and the rule refused it.
    pub fallback_skipped: Option<FallbackSkip>,
}
impl RuntimeDispatchFailure {
    pub(crate) fn new(runtime: RuntimeType, model: &str) -> Self {
        Self {
            requested: Some(runtime),
            requested_model: model.into(),
            preflight: None,
            steps: vec![],
            fallback_skipped: None,
        }
    }
    /// Record that the configured `fallback` was not used for `reason`.
    pub(crate) fn skip_fallback(
        mut self,
        fallback: Option<&RuntimeType>,
        reason: super::fallback_rule::FallbackRefusal,
    ) -> Self {
        self.fallback_skipped = fallback.map(|runtime| FallbackSkip { runtime: *runtime, reason });
        self
    }
    pub(crate) fn preflight(
        runtime: Option<RuntimeType>,
        model: &str,
        kind: RuntimePreflightKind,
        error: String,
    ) -> Self {
        Self {
            requested: runtime,
            requested_model: model.into(),
            preflight: Some(RuntimePreflightFailure { kind, error }),
            steps: vec![],
            fallback_skipped: None,
        }
    }
    pub(crate) fn push(
        &mut self,
        role: RuntimeStepRole,
        runtime: RuntimeType,
        model: Option<&str>,
        outcome: RuntimeStepOutcome,
    ) {
        debug_assert!(self.steps.len() < 2);
        self.steps.push(RuntimeStep {
            role,
            runtime,
            model: model.map(str::to_owned),
            outcome,
        });
    }
    /// Preserve the authorization marker for String callers that must refuse
    /// every secondary route. Typed callers retain all primary/fallback data.
    pub(crate) fn legacy_message(&self) -> String {
        if let Some(preflight) = &self.preflight {
            if preflight.error == crate::on_behalf_of::access::MAIN_TURN_REFUSAL { return preflight.error.clone(); }
            if preflight.kind == RuntimePreflightKind::Budget {
                return format!("{}{}", crate::budget::BUDGET_REFUSAL_PREFIX, preflight.error);
            }
        }
        self.steps
            .iter()
            .find_map(|step| match &step.outcome {
                RuntimeStepOutcome::ExecutionError(error)
                    if error == crate::on_behalf_of::access::MAIN_TURN_REFUSAL
                        || crate::work_outcome::is_work_result(error)
                        || duduclaw_agent::mcp_spawn_gate::is_spawn_gate_error(error)
                        || crate::budget::budget_refusal_message(error).is_some()
                        || error.starts_with("AUTONOMY_PAUSED:") =>
                {
                    Some(error.clone())
                }
                _ => None,
            })
            .unwrap_or_else(|| self.to_string())
    }
    /// Bounded metadata only: never echo a provider response/token/request.
    pub(crate) fn audit_steps(&self) -> serde_json::Value {
        serde_json::Value::Array(self.steps.iter().take(2).map(|s|serde_json::json!({
            "role":match s.role {RuntimeStepRole::Primary=>"primary",RuntimeStepRole::Fallback=>"explicit_fallback"},
            "runtime":s.runtime.as_str(),"model":s.model.as_deref().map(|m|duduclaw_core::truncate_chars(m,160)),
            "outcome":s.outcome.token(),
        })).collect())
    }
}
impl RuntimeStepOutcome {
    pub(crate) fn token(&self) -> &'static str {
        match self {
            Self::ExecutionError(_) => "execution_error",
            Self::Empty => "empty_response",
            Self::NotRegistered => "not_registered",
            Self::Cooldown => "cooldown",
            Self::PolicyRefused(_) => "policy_refused",
            Self::NoCompatibleModel => "no_compatible_model",
            Self::CrossFamilyRefused => "cross_family_refused",
        }
    }
}
impl std::fmt::Display for RuntimeDispatchFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(preflight) = &self.preflight {
            return write!(f, "{}", preflight.error);
        }
        for (i, s) in self.steps.iter().enumerate() {
            if i > 0 {
                write!(f, "; ")?;
            }
            let role = match s.role {
                RuntimeStepRole::Primary => "primary",
                RuntimeStepRole::Fallback => "explicit fallback",
            };
            write!(
                f,
                "{role} runtime {} / {}: ",
                s.runtime.as_str(),
                s.model.as_deref().unwrap_or("model unresolved")
            )?;
            match &s.outcome {
                RuntimeStepOutcome::ExecutionError(error) => write!(f, "{error}")?,
                RuntimeStepOutcome::Empty if s.role == RuntimeStepRole::Primary => {
                    write!(f, "Empty response from primary ({:?})", s.runtime)?
                }
                RuntimeStepOutcome::Empty => {
                    write!(f, "Empty response from fallback ({:?})", s.runtime)?
                }
                RuntimeStepOutcome::NotRegistered => write!(f, "unavailable (not registered)")?,
                RuntimeStepOutcome::Cooldown => write!(f, "unavailable (cooldown)")?,
                RuntimeStepOutcome::PolicyRefused(error) => write!(f, "policy refused: {error}")?,
                RuntimeStepOutcome::NoCompatibleModel => {
                    write!(f, "no model configured for fallback runtime")?
                }
                RuntimeStepOutcome::CrossFamilyRefused => {
                    write!(f, "cross-family failover refused for this call")?
                }
            }
        }
        if self.steps.len() == 1 {
            write!(f, "; no fallback attempted")?;
        }
        if let Some(skip) = &self.fallback_skipped {
            write!(f, " (configured fallback {} not used: {})", skip.runtime.as_str(), skip.reason.token())?;
        }
        Ok(())
    }
}
impl std::error::Error for RuntimeDispatchFailure {}

#[cfg(test)]
mod budget_legacy_tests {
    use super::*;

    #[test]
    fn budget_marker_survives_execution_error_legacy_conversion() {
        let error = format!("{}US$402.00", crate::budget::BUDGET_REFUSAL_PREFIX);
        let mut failure = RuntimeDispatchFailure::new(RuntimeType::Claude, "claude-sonnet-4-6");
        failure.push(RuntimeStepRole::Primary, RuntimeType::Claude, Some("claude-sonnet-4-6"),
            RuntimeStepOutcome::ExecutionError(error.clone()));
        assert_eq!(failure.legacy_message(), error);
    }
}
