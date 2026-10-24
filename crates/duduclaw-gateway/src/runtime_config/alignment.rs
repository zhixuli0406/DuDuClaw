//! Canonical save-time model/runtime consistency; no read-time writes.
use super::*;
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ModelRuntimeAlignment {
    Unchanged,
    Aligned {
        from: RuntimeType,
        to: RuntimeType,
        model: String,
    },
    Skipped(&'static str),
}
impl ModelRuntimeAlignment {
    pub(crate) fn aligned_provider(&self) -> Option<RuntimeType> {
        match self {
            Self::Aligned { to, .. } => Some(to.clone()),
            _ => None,
        }
    }
    pub(crate) fn skipped_reason(&self) -> Option<&'static str> {
        match self {
            Self::Skipped(reason) => Some(reason),
            _ => None,
        }
    }
    pub(crate) fn description(&self) -> Option<String> {
        match self {
            Self::Aligned { from, to, model } => Some(format!(
                "runtime.provider auto-aligned \"{}\" → \"{}\" (model \"{model}\")",
                from.as_str(),
                to.as_str()
            )),
            _ => None,
        }
    }
    pub(crate) fn audit(&self, home: &Path, agent: &str, source: &str, user_id: &str) {
        if let Self::Aligned { from, to, model } = self {
            crate::security_autopilot::audit_and_emit(
                home,
                &duduclaw_security::audit::AuditEvent::new(
                    "agent_runtime_auto_aligned",
                    agent,
                    duduclaw_security::audit::Severity::Info,
                    serde_json::json!({"agent_id":agent,"source":source,"user_id":user_id,"from":from.as_str(),"to":to.as_str(),"model":duduclaw_core::truncate_chars(model,160)}),
                ),
            );
        }
    }
}
/// Existing update restrictions: display-name-only or deprecated inference
/// cannot silently replace a runtime. No substring family inference.
pub(crate) fn auto_align_target(
    inferred: Option<RuntimeType>,
    model: &str,
) -> (Option<RuntimeType>, Option<&'static str>) {
    match inferred {
        Some(RuntimeType::Antigravity) if model.to_ascii_lowercase().starts_with("gemini-") => {
            (None, Some("model_id_requires_display_name"))
        }
        Some(rt) if rt.is_deprecated() => (None, Some("deprecated_runtime")),
        other => (other, None),
    }
}
pub(crate) fn align_model_runtime(
    table: &mut toml::Table,
) -> Result<ModelRuntimeAlignment, String> {
    let Some(model) = table
        .get("model")
        .and_then(|m| m.get("preferred"))
        .and_then(toml::Value::as_str)
        .map(str::to_owned)
    else {
        return Ok(ModelRuntimeAlignment::Unchanged);
    };
    // An invalid stored provider is never replaced by a guessed default.
    let provider = match table.get("runtime") {
        None => RuntimeType::default(),
        Some(toml::Value::Table(runtime)) => match runtime.get("provider") {
            None => RuntimeType::default(),
            Some(toml::Value::String(value)) => match RuntimeType::parse(value) {
                Some(provider) => provider,
                None => return Ok(ModelRuntimeAlignment::Skipped("invalid_runtime_provider")),
            },
            Some(_) => return Ok(ModelRuntimeAlignment::Skipped("invalid_runtime_provider")),
        },
        Some(_) => return Err("Invalid [runtime] section".into()),
    };
    if model_matches_provider(&model, provider.clone()) {
        return Ok(ModelRuntimeAlignment::Unchanged);
    }
    let (inferred, skipped) = auto_align_target(infer_provider_for_model(&model), &model);
    if let Some(reason) = skipped {
        return Ok(ModelRuntimeAlignment::Skipped(reason));
    }
    let Some(to) = inferred else {
        return Ok(ModelRuntimeAlignment::Unchanged);
    };
    let runtime = table
        .entry("runtime")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
        .as_table_mut()
        .ok_or("Invalid [runtime] section")?;
    runtime.insert("provider".into(), toml::Value::String(to.as_str().into()));
    Ok(ModelRuntimeAlignment::Aligned {
        from: provider,
        to,
        model,
    })
}
/// Routing preflight is pure: known mismatches require an authorized save;
/// custom models/API-compatible shells retain their existing conservative rule.
pub(crate) fn ensure_model_provider(model: &str, provider: RuntimeType) -> Result<(), String> {
    if model_matches_provider(model, provider.clone()) {
        Ok(())
    } else {
        Err(format!(
            "Model/runtime configuration mismatch: model '{model}' cannot run on runtime '{}'. Ask an administrator to save the model/runtime settings before retrying.",
            provider.as_str()
        ))
    }
}
#[cfg(test)]
pub(crate) mod testing {
    use std::collections::HashMap;
    thread_local! { static AVAILABLE: std::cell::RefCell<HashMap<String,bool>>=std::cell::RefCell::new(HashMap::new()); }
    pub(crate) struct AvailabilityGuard {
        model: String,
    }
    impl AvailabilityGuard {
        pub(crate) fn at(model: &str, installed: bool) -> Self {
            AVAILABLE
                .with(|v| assert!(v.borrow_mut().insert(model.to_owned(), installed).is_none()));
            Self {
                model: model.to_owned(),
            }
        }
    }
    impl Drop for AvailabilityGuard {
        fn drop(&mut self) {
            AVAILABLE.with(|v| {
                v.borrow_mut().remove(&self.model);
            });
        }
    }
    pub(crate) fn availability(model: &str) -> Option<bool> {
        AVAILABLE.with(|v| v.borrow().get(model).copied())
    }
}
