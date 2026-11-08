//! Minimum roles for the employee dashboard surface, before extension dispatch.
//! Record-level ownership and Viewer/Operator checks still belong to handlers.
use duduclaw_auth::models::UserRole;

const EMPLOYEE_METHODS: &[&str] = &[
    "agents.org_chart",
    "skills.recent",
    "cron.list",
    "cron.templates",
    "cron.add",
    "cron.update",
    "cron.pause",
    "cron.resume",
    "cron.remove",
    "cron.run_now",
    "reminders.list",
    "reminders.cancel",
    "assign_schedules.list",
    "assign_schedules.pause",
    "assign_schedules.resume",
    "assign_schedules.remove",
];

/// `None` leaves unrelated legacy namespaces to their existing dispatch gates.
/// Unknown methods in the registered namespaces default to Admin, including
/// extensions. Nested chart/recent methods are reserved without changing the
/// rest of the pre-existing agents/skills APIs.
pub(super) fn minimum_role(method: &str) -> Option<UserRole> {
    if EMPLOYEE_METHODS.contains(&method) {
        return Some(UserRole::Employee);
    }
    let namespace = method.split('.').next().unwrap_or("");
    // Isolation connector lifecycle: Admin, plus step-up in its handler.
    if method.strip_prefix("isolation.").is_some_and(|rest| rest.starts_with("connector_")) {
        return Some(UserRole::Admin);
    }
    if matches!(namespace, "cron" | "reminders" | "assign_schedules")
        || method.starts_with("agents.org_chart.")
        || method.starts_with("skills.recent.")
    {
        Some(UserRole::Admin)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dashboard_scope_registers_employee_methods_explicitly() {
        for method in EMPLOYEE_METHODS {
            assert_eq!(minimum_role(method), Some(UserRole::Employee), "{method}");
        }
        for method in ["cron", "cron.future", "cron.add.future", "reminders.future",
            "assign_schedules.future", "agents.org_chart.future", "skills.recent.future",
            "isolation.connector_list", "isolation.connector_provision", "isolation.connector_future"] {
            assert_eq!(minimum_role(method), Some(UserRole::Admin), "{method}");
        }
        for method in ["ping", "agents.list", "skills.list", "cronish.list", "isolation.recording_status"] {
            assert_eq!(minimum_role(method), None, "{method}");
        }
    }
}
