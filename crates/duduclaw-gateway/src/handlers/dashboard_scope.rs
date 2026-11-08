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
/// RM-027 org chart editing. Proposing and following proposals is Manager
/// work; applying directly (and any future `org.*` member) is Admin.
const MANAGER_METHODS: &[&str] = &["org.propose_change", "org.pending_changes", "org.placement"];

pub(super) fn minimum_role(method: &str) -> Option<UserRole> {
    if EMPLOYEE_METHODS.contains(&method) {
        return Some(UserRole::Employee);
    }
    if MANAGER_METHODS.contains(&method) {
        return Some(UserRole::Manager);
    }
    let namespace = method.split('.').next().unwrap_or("");
    if namespace == "org" {
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
            "assign_schedules.future", "agents.org_chart.future", "skills.recent.future"] {
            assert_eq!(minimum_role(method), Some(UserRole::Admin), "{method}");
        }
        for method in ["ping", "agents.list", "skills.list", "cronish.list", "organization.list"] {
            assert_eq!(minimum_role(method), None, "{method}");
        }
    }

    #[test]
    fn org_chart_editing_methods_have_explicit_floors() {
        assert_eq!(minimum_role("org.propose_change"), Some(UserRole::Manager));
        assert_eq!(minimum_role("org.pending_changes"), Some(UserRole::Manager));
        assert_eq!(minimum_role("org.placement"), Some(UserRole::Manager));
        assert_eq!(minimum_role("org.apply_change"), Some(UserRole::Admin));
        // Fail closed for any member nobody registered.
        assert_eq!(minimum_role("org.future"), Some(UserRole::Admin));
        assert_eq!(minimum_role("org"), Some(UserRole::Admin));
    }
}
