//! Operator-owned fixture resource scope, re-read at the native effect boundary.
use serde_json::Value;
use std::path::Path;

/// A staging home label alone never authorizes a task or scheduled-job mutation.
/// Native ownership, capability and approval checks still apply independently.
pub fn check_effect_scope(home: &Path, tool: &str, arguments: &Value) -> Result<(), String> {
    let raw = std::fs::read_to_string(home.join("config.toml"))
        .map_err(|_| "workflow staging config unavailable")?;
    let config: toml::Value =
        toml::from_str(&raw).map_err(|_| "workflow staging config invalid")?;
    let workflow = config
        .get("workflow")
        .ok_or("workflow staging not configured")?;
    if workflow
        .get("fixture_environment")
        .and_then(toml::Value::as_str)
        != Some("staging")
    {
        return Err("workflow fixture requires explicit staging home".into());
    }
    let (key, parameter) = match tool {
        "tasks_update" => ("fixture_task_ids", "task_id"),
        "update_cron_task" => ("fixture_cron_ids", "id"),
        _ => return Err("workflow fixture effect unsupported".into()),
    };
    let id = arguments
        .get(parameter)
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or("workflow fixture requires exact resource id")?;
    // Name-based cron selection can resolve to another row after a rename.
    if tool == "update_cron_task" && arguments.get("name").is_some() {
        return Err("workflow fixture cron name selection forbidden".into());
    }
    let ids = workflow
        .get(key)
        .and_then(toml::Value::as_array)
        .ok_or("workflow fixture resource allowlist missing")?;
    if ids
        .iter()
        .any(|entry| entry.as_str().is_none_or(str::is_empty))
    {
        return Err("workflow fixture resource allowlist invalid".into());
    }
    if !ids.iter().any(|entry| entry.as_str() == Some(id)) {
        return Err("workflow fixture resource outside staging scope".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn staging_label_does_not_allow_owned_production_resources() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join("config.toml"),
            "[workflow]\nfixture_environment='staging'\nfixture_task_ids=['test-task']\nfixture_cron_ids=['test-cron']\n"
        )
        .unwrap();
        assert!(
            check_effect_scope(home.path(), "tasks_update", &json!({"task_id":"test-task"}))
                .is_ok()
        );
        assert!(
            check_effect_scope(
                home.path(),
                "tasks_update",
                &json!({"task_id":"production-task"})
            )
            .is_err()
        );
        assert!(
            check_effect_scope(home.path(), "update_cron_task", &json!({"id":"test-cron"})).is_ok()
        );
        assert!(
            check_effect_scope(
                home.path(),
                "update_cron_task",
                &json!({"name":"test-cron"})
            )
            .is_err()
        );
        assert!(
            check_effect_scope(
                home.path(),
                "update_cron_task",
                &json!({"id":"test-cron","name":"other"})
            )
            .is_err()
        );
        assert!(
            check_effect_scope(home.path(), "tasks_delete", &json!({"task_id":"test-task"}))
                .is_err()
        );
        // Removing the operator's scope must affect the next boundary check.
        std::fs::write(
            home.path().join("config.toml"),
            "[workflow]\nfixture_environment='staging'\nfixture_task_ids=[]\n",
        )
        .unwrap();
        assert!(
            check_effect_scope(home.path(), "tasks_update", &json!({"task_id":"test-task"}))
                .is_err()
        );
    }

    #[test]
    fn missing_or_malformed_scope_fails_closed() {
        let home = tempfile::tempdir().unwrap();
        for config in [
            "",
            "[workflow]\nfixture_environment='staging'\n",
            "[workflow]\nfixture_environment='staging'\nfixture_task_ids=['test-task',7]\n",
            "[workflow]\nfixture_environment='production'\nfixture_task_ids=['test-task']\n",
        ] {
            std::fs::write(home.path().join("config.toml"), config).unwrap();
            assert!(
                check_effect_scope(home.path(), "tasks_update", &json!({"task_id":"test-task"}))
                    .is_err()
            );
        }
    }
}
