//! Effect target pinning (F1b, A-H-1).
//!
//! An activated workflow may only change the exact records an Admin saw on
//! the activation card. Each effect tool names the one argument that selects
//! the record it changes; a tool missing from [`EFFECT_TARGETS`] cannot be a
//! workflow effect at all (fail closed). At activation the template's
//! `resource_scope` must pin that argument to a fixed id, and the step's input
//! must produce that same id statically — from a literal, possibly passed
//! through identity, artifact or approval steps — never from a read result,
//! the run input or a transform. `update_cron_task` additionally may not
//! select by `name` (a rename would redirect it), matching
//! `staging::check_effect_scope`.
use super::schema::{InputRef, ProcessTransform, StepAction, WorkflowDefinition};
use crate::approval::EffectTemplate;
use serde_json::{Value, json};

/// Effect tool → the argument that names the record it changes.
pub const EFFECT_TARGETS: &[(&str, &str)] =
    &[("tasks_update", "task_id"), ("update_cron_task", "id")];

/// The target argument of an effect tool, or `None` when the tool may not be
/// used as a workflow effect.
pub fn effect_target_param(tool: &str) -> Option<&'static str> {
    EFFECT_TARGETS
        .iter()
        .find(|(t, _)| *t == tool)
        .map(|(_, param)| *param)
}

/// The template's pinned target, checked on its own (no definition needed).
pub fn template_target(template: &EffectTemplate) -> Result<(&'static str, &str), String> {
    let param = effect_target_param(&template.tool)
        .ok_or("workflow effect tool has no fixed target and cannot be an effect")?;
    let id = template
        .resource_scope
        .get(param)
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .ok_or("workflow effect target id must be fixed in the template scope")?;
    if template.tool == "update_cron_task" && template.resource_scope.contains_key("name") {
        return Err("workflow cron effect may not select by name".into());
    }
    Ok((param, id))
}

/// Arguments that select a different record than the pinned one, checked at
/// the moment an effect is authorized.
pub fn check_effect_arguments(template: &EffectTemplate, arguments: &Value) -> Result<(), String> {
    let (param, id) = template_target(template)?;
    if arguments.get(param).and_then(Value::as_str) != Some(id) {
        return Err("effective resource outside accepted scope".into());
    }
    if template.tool == "update_cron_task" && arguments.get("name").is_some() {
        return Err("workflow cron effect may not select by name".into());
    }
    Ok(())
}

/// The value the step input yields at `pointer`, when it is fixed by the
/// definition itself. `None` means it depends on something only known at run
/// time (a read, the run input, a transform, a human answer).
fn static_value(
    definition: &WorkflowDefinition,
    input: &InputRef,
    pointer: &str,
    depth: usize,
) -> Option<Value> {
    if depth > 64 {
        return None;
    }
    match input {
        InputRef::Literal { value } => value.pointer(pointer).cloned(),
        InputRef::StepOutput {
            step_id,
            pointer: p,
        } => {
            let step = definition.steps.iter().find(|s| &s.step_id == step_id)?;
            let passes_through = matches!(
                &step.action,
                StepAction::Process {
                    transform: ProcessTransform::Identity
                } | StepAction::Artifact { .. }
                    | StepAction::Approval { .. }
            );
            if !passes_through {
                return None;
            }
            static_value(definition, &step.input, &format!("{p}{pointer}"), depth + 1)
        }
        InputRef::RunInput { .. } | InputRef::Array { .. } => None,
    }
}

/// Activation check: the template pins a target and the step can only ever
/// send that target.
pub fn check_pinned_target(
    definition: &WorkflowDefinition,
    template: &EffectTemplate,
) -> Result<(), String> {
    let (param, id) = template_target(template)?;
    let step = definition
        .steps
        .iter()
        .find(|s| s.step_id == template.step_id)
        .ok_or("grant step missing")?;
    match static_value(definition, &step.input, &format!("/{param}"), 0) {
        Some(Value::String(found)) if found == id => (),
        Some(_) => return Err("workflow effect target differs from the pinned id".into()),
        None => {
            return Err(
                "workflow effect target must be a fixed id, not a read result or run input".into(),
            );
        }
    }
    if template.tool == "update_cron_task" {
        match static_value(definition, &step.input, "", 0) {
            Some(Value::Object(args)) if !args.contains_key("name") => (),
            _ => return Err("workflow cron effect may not select by name".into()),
        }
    }
    Ok(())
}

/// What the activation card shows: each effect and the one record it may
/// change.
pub fn effect_targets<'a>(templates: impl IntoIterator<Item = &'a EffectTemplate>) -> Vec<Value> {
    templates
        .into_iter()
        .map(|t| match template_target(t) {
            Ok((param, id)) => json!({
                "step_id": t.step_id, "tool": t.tool, "param": param, "target": id
            }),
            Err(_) => json!({
                "step_id": t.step_id, "tool": t.tool, "param": null, "target": null
            }),
        })
        .collect()
}

/// Whether a step's input depends, directly or through earlier steps, on
/// the run input (R-M5). Only such an effect is held to the run input's age;
/// an effect built from literals or fresh reads is not blocked because a
/// person took longer to approve it than `input_max_age_seconds`.
pub fn step_uses_run_input(definition: &WorkflowDefinition, step_id: &str) -> bool {
    fn walk(definition: &WorkflowDefinition, input: &InputRef, depth: usize) -> bool {
        if depth > 64 {
            return true; // unknown ⇒ treat as dependent (keeps the check)
        }
        match input {
            InputRef::Literal { .. } => false,
            InputRef::RunInput { .. } => true,
            InputRef::Array { items } => items.iter().any(|i| walk(definition, i, depth + 1)),
            InputRef::StepOutput { step_id, .. } => definition
                .steps
                .iter()
                .find(|s| &s.step_id == step_id)
                .is_none_or(|s| walk(definition, &s.input, depth + 1)),
        }
    }
    definition
        .steps
        .iter()
        .find(|s| s.step_id == step_id)
        .is_none_or(|s| walk(definition, &s.input, 0))
}
