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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeDispatchFailure {
    pub requested: Option<RuntimeType>,
    pub requested_model: String,
    pub preflight: Option<RuntimePreflightFailure>,
    pub steps: Vec<RuntimeStep>,
}
impl RuntimeDispatchFailure {
    pub(crate) fn new(runtime: RuntimeType, model: &str) -> Self {
        Self {
            requested: Some(runtime),
            requested_model: model.into(),
            preflight: None,
            steps: vec![],
        }
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
            if preflight.kind == RuntimePreflightKind::Budget {
                return format!("{}{}", crate::budget::BUDGET_REFUSAL_PREFIX, preflight.error);
            }
        }
        self.steps
            .iter()
            .find_map(|step| match &step.outcome {
                RuntimeStepOutcome::ExecutionError(error)
                    if duduclaw_agent::mcp_spawn_gate::is_spawn_gate_error(error)
                        || crate::budget::budget_refusal_message(error).is_some() =>
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
