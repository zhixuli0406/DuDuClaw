//! Draft member-facing dashboard RPC compatibility metadata (RM-213 part A).
//! The distributor's actual method inventory is still pending. This is not
//! a full authorization table; member_key_method narrows it for key authentication.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberContractKind {
    Read,
    Write,
}

#[derive(Debug)]
pub struct MemberContractMethod {
    pub name: &'static str,
    /// First contract version, not necessarily the method's introduction.
    pub since: &'static str,
    pub kind: MemberContractKind,
    pub summary_en: &'static str,
    /// Required top-level payload fields for the documented fixture shape.
    /// Empty for writes; nested fields and alternate shapes are documented separately.
    pub key_fields: &'static [&'static str],
}

pub const MEMBER_CONTRACT_METHODS: &[MemberContractMethod] = &[
    MemberContractMethod {
        name: "users.me", since: "1.85.0", kind: MemberContractKind::Read,
        summary_en: "Read the signed-in account and direct bindings",
        // Department grants are always empty for member-key authentication.
        key_fields: &["user", "bindings", "email_setup", "dept_grants"],
    },
    MemberContractMethod {
        name: "about.get", since: "1.85.0", kind: MemberContractKind::Read,
        summary_en: "Read product and vendor information",
        key_fields: &["product_name", "vendor", "version", "tier", "brand_revision"],
    },
    MemberContractMethod {
        name: "onboarding.get", since: "1.85.0", kind: MemberContractKind::Read,
        summary_en: "Read personal first-login progress",
        key_fields: &["enabled", "dismissed", "password_done", "steps", "examples"],
    },
    MemberContractMethod {
        name: "onboarding.update", since: "1.85.0", kind: MemberContractKind::Write,
        summary_en: "Update personal first-login progress",
        key_fields: &[],
    },
    MemberContractMethod {
        name: "users.set_primary_agent", since: "1.85.0", kind: MemberContractKind::Write,
        summary_en: "Choose a primary AI with an Operator or Owner binding; an assigned avatar locks this preference",
        key_fields: &[],
    },
    MemberContractMethod {
        name: "agents.list", since: "1.85.0", kind: MemberContractKind::Read,
        summary_en: "List visible AI employees",
        key_fields: &["agents"],
    },
    MemberContractMethod {
        name: "agents.callable", since: "1.85.0", kind: MemberContractKind::Read,
        summary_en: "List AI employees the caller can call",
        key_fields: &["agents", "default_agent", "unrestricted"],
    },
    MemberContractMethod {
        name: "agents.org_chart", since: "1.85.0", kind: MemberContractKind::Read,
        summary_en: "Read the visible AI organization chart",
        key_fields: &["agents"],
    },
    MemberContractMethod {
        name: "agents.update", since: "1.85.0", kind: MemberContractKind::Write,
        summary_en: "Update an AI employee with a direct Owner binding",
        key_fields: &[],
    },
    MemberContractMethod {
        name: "chat.sessions.list", since: "1.85.0", kind: MemberContractKind::Read,
        summary_en: "List conversations for a bound AI employee",
        key_fields: &["sessions"],
    },
    MemberContractMethod {
        name: "chat.sessions.history", since: "1.85.0", kind: MemberContractKind::Read,
        summary_en: "Read an authorized conversation",
        key_fields: &["session_id", "agent_id", "messages"],
    },
    MemberContractMethod {
        name: "chat.sessions.restore", since: "1.85.0", kind: MemberContractKind::Write,
        summary_en: "Restore an owned archived conversation",
        key_fields: &[],
    },
    MemberContractMethod {
        name: "knowledge.citations.for_session", since: "1.85.0", kind: MemberContractKind::Read,
        summary_en: "Read citations for an owned WebChat conversation",
        key_fields: &["citations"],
    },
    MemberContractMethod {
        name: "skills.recent", since: "1.85.0", kind: MemberContractKind::Read,
        summary_en: "Read personal recently used skills",
        key_fields: &["skills"],
    },
    MemberContractMethod {
        name: "decisions.list", since: "1.85.0", kind: MemberContractKind::Read,
        summary_en: "Read open decisions for a bound AI employee",
        key_fields: &["decisions"],
    },
    MemberContractMethod {
        name: "tasks.list", since: "1.85.0", kind: MemberContractKind::Read,
        summary_en: "List tasks for a bound AI employee",
        key_fields: &["tasks"],
    },
    MemberContractMethod {
        name: "tasks.create", since: "1.85.0", kind: MemberContractKind::Write,
        summary_en: "Create or schedule a task for a callable AI employee",
        key_fields: &[],
    },
    MemberContractMethod {
        name: "tasks.update", since: "1.85.0", kind: MemberContractKind::Write,
        summary_en: "Update an authorized task",
        key_fields: &[],
    },
    MemberContractMethod {
        name: "tasks.comment", since: "1.85.0", kind: MemberContractKind::Write,
        summary_en: "Comment on an authorized task",
        key_fields: &[],
    },
    MemberContractMethod {
        name: "tasks.comments", since: "1.85.0", kind: MemberContractKind::Read,
        summary_en: "Read comments on an authorized task",
        key_fields: &["comments"],
    },
    MemberContractMethod {
        name: "tasks.assign", since: "1.85.0", kind: MemberContractKind::Write,
        summary_en: "Assign an authorized task to a callable AI employee",
        key_fields: &[],
    },
    MemberContractMethod {
        name: "tasks.goal_decide", since: "1.85.0", kind: MemberContractKind::Write,
        summary_en: "Decide an authorized goal with Operator access",
        key_fields: &[],
    },
    MemberContractMethod {
        name: "tasks.goal_create", since: "1.85.0", kind: MemberContractKind::Write,
        summary_en: "Create or schedule a goal for a callable AI employee",
        key_fields: &[],
    },
    MemberContractMethod {
        name: "tasks.skill_assign", since: "1.85.0", kind: MemberContractKind::Write,
        summary_en: "Assign a skill using an authorized runner",
        key_fields: &[],
    },
    MemberContractMethod {
        name: "cron.list", since: "1.85.0", kind: MemberContractKind::Read,
        summary_en: "List visible routines",
        key_fields: &["tasks"],
    },
    MemberContractMethod {
        name: "cron.templates", since: "1.85.0", kind: MemberContractKind::Read,
        summary_en: "Read routine templates",
        key_fields: &["templates"],
    },
    MemberContractMethod {
        name: "cron.add", since: "1.85.0", kind: MemberContractKind::Write,
        summary_en: "Create a prompt-only routine for an Operator-bound AI employee",
        key_fields: &[],
    },
    MemberContractMethod {
        name: "cron.update", since: "1.85.0", kind: MemberContractKind::Write,
        summary_en: "Update a manageable routine",
        key_fields: &[],
    },
    MemberContractMethod {
        name: "cron.pause", since: "1.85.0", kind: MemberContractKind::Write,
        summary_en: "Pause a manageable routine",
        key_fields: &[],
    },
    MemberContractMethod {
        name: "cron.resume", since: "1.85.0", kind: MemberContractKind::Write,
        summary_en: "Resume an eligible manageable routine",
        key_fields: &[],
    },
    MemberContractMethod {
        name: "cron.remove", since: "1.85.0", kind: MemberContractKind::Write,
        summary_en: "Remove a manageable routine",
        key_fields: &[],
    },
    MemberContractMethod {
        name: "cron.run_now", since: "1.85.0", kind: MemberContractKind::Write,
        summary_en: "Run an eligible manageable routine now",
        key_fields: &[],
    },
    MemberContractMethod {
        name: "reminders.list", since: "1.85.0", kind: MemberContractKind::Read,
        summary_en: "List visible reminders",
        key_fields: &["reminders"],
    },
    MemberContractMethod {
        name: "reminders.cancel", since: "1.85.0", kind: MemberContractKind::Write,
        summary_en: "Cancel an owned or Operator-authorized reminder",
        key_fields: &[],
    },
    MemberContractMethod {
        name: "assign_schedules.list", since: "1.85.0", kind: MemberContractKind::Read,
        summary_en: "List personal scheduled assignments",
        key_fields: &["schedules", "max_active", "min_interval_minutes"],
    },
    MemberContractMethod {
        name: "assign_schedules.pause", since: "1.85.0", kind: MemberContractKind::Write,
        summary_en: "Pause a personal scheduled assignment",
        key_fields: &[],
    },
    MemberContractMethod {
        name: "assign_schedules.resume", since: "1.85.0", kind: MemberContractKind::Write,
        summary_en: "Resume a personal scheduled assignment",
        key_fields: &[],
    },
    MemberContractMethod {
        name: "assign_schedules.remove", since: "1.85.0", kind: MemberContractKind::Write,
        summary_en: "Remove a personal scheduled assignment",
        key_fields: &[],
    },
    MemberContractMethod {
        name: "skills.list", since: "1.85.0", kind: MemberContractKind::Read,
        summary_en: "List skills for a Viewer-bound AI employee",
        key_fields: &["agent_id", "skills", "scanned", "skill_state", "can_manage", "usage_error", "usage_truncated"],
    },
    MemberContractMethod {
        name: "skills.search", since: "1.85.0", kind: MemberContractKind::Read,
        summary_en: "Search local and configured market skills",
        key_fields: &["skills", "source", "total_indexed", "hub_errors"],
    },
    MemberContractMethod {
        name: "skills.install_request", since: "1.85.0", kind: MemberContractKind::Write,
        summary_en: "Request a skill installation through the approval chain",
        key_fields: &[],
    },
    MemberContractMethod {
        name: "skills.share", since: "1.85.0", kind: MemberContractKind::Write,
        summary_en: "Share a skill from an Owner-bound AI employee",
        key_fields: &[],
    },
    MemberContractMethod {
        name: "skills.shared", since: "1.85.0", kind: MemberContractKind::Read,
        summary_en: "List shared skills",
        key_fields: &["skills"],
    },
    MemberContractMethod {
        name: "skills.adopt", since: "1.85.0", kind: MemberContractKind::Write,
        summary_en: "Adopt a shared skill into an Owner-bound AI employee",
        key_fields: &[],
    },
    MemberContractMethod {
        name: "skills.leaderboard", since: "1.85.0", kind: MemberContractKind::Read,
        summary_en: "Read the visible skill ranking",
        key_fields: &["leaderboard", "metric", "note"],
    },
    MemberContractMethod {
        name: "memory.search", since: "1.85.0", kind: MemberContractKind::Read,
        summary_en: "Search memory for a Viewer-bound AI employee",
        key_fields: &["entries", "signals"],
    },
    MemberContractMethod {
        name: "memory.browse", since: "1.85.0", kind: MemberContractKind::Read,
        summary_en: "Browse memory for a Viewer-bound AI employee",
        key_fields: &["entries", "signals"],
    },
    MemberContractMethod {
        name: "memory.key_facts", since: "1.85.0", kind: MemberContractKind::Read,
        summary_en: "Read key facts for a Viewer-bound AI employee",
        key_fields: &["entries"],
    },
    MemberContractMethod {
        name: "memory.history", since: "1.85.0", kind: MemberContractKind::Read,
        summary_en: "Read the temporal history of a memory fact",
        key_fields: &["subject", "predicate", "chain", "current_id"],
    },
    MemberContractMethod {
        name: "wiki.pages", since: "1.85.0", kind: MemberContractKind::Read,
        summary_en: "List wiki pages for a Viewer-bound AI employee",
        key_fields: &["pages", "exists"],
    },
    MemberContractMethod {
        name: "wiki.read", since: "1.85.0", kind: MemberContractKind::Read,
        summary_en: "Read a wiki page for a Viewer-bound AI employee",
        key_fields: &["content", "path"],
    },
    MemberContractMethod {
        name: "wiki.search", since: "1.85.0", kind: MemberContractKind::Read,
        summary_en: "Search a wiki for a Viewer-bound AI employee",
        key_fields: &["hits"],
    },
    MemberContractMethod {
        name: "shared_wiki.pages", since: "1.85.0", kind: MemberContractKind::Read,
        summary_en: "List shared wiki pages visible to the caller",
        key_fields: &["pages", "exists"],
    },
    MemberContractMethod {
        name: "me.memory.list", since: "1.85.0", kind: MemberContractKind::Read,
        summary_en: "List memories about the signed-in person",
        key_fields: &["items", "next_cursor"],
    },
    MemberContractMethod {
        name: "me.memory.forget", since: "1.85.0", kind: MemberContractKind::Write,
        summary_en: "Forget a memory about the signed-in person",
        key_fields: &[],
    },
    MemberContractMethod {
        name: "dashboard.layout.get", since: "1.85.0", kind: MemberContractKind::Read,
        summary_en: "Read the personal dashboard layout",
        key_fields: &["layout"],
    },
    MemberContractMethod {
        name: "dashboard.layout.set", since: "1.85.0", kind: MemberContractKind::Write,
        summary_en: "Save the personal dashboard layout",
        key_fields: &[],
    },
    MemberContractMethod {
        name: "activity.list", since: "1.85.0", kind: MemberContractKind::Read,
        summary_en: "Read activity for a bound AI employee",
        key_fields: &["events", "total"],
    },
    MemberContractMethod {
        name: "users.change_password", since: "1.85.0", kind: MemberContractKind::Write,
        summary_en: "Change the signed-in account password",
        key_fields: &[],
    },
    MemberContractMethod {
        name: "users.change_email", since: "1.85.0", kind: MemberContractKind::Write,
        summary_en: "Change the signed-in account email",
        key_fields: &[],
    },
    MemberContractMethod {
        name: "me.status_tokens.list", since: "1.85.0", kind: MemberContractKind::Read,
        summary_en: "List personal external-status credentials",
        key_fields: &["enabled", "max_active", "endpoint_path", "tokens"],
    },
    MemberContractMethod {
        name: "me.status_tokens.create", since: "1.85.0", kind: MemberContractKind::Write,
        summary_en: "Create a personal status-only credential when enabled",
        key_fields: &[],
    },
    MemberContractMethod {
        name: "me.status_tokens.revoke", since: "1.85.0", kind: MemberContractKind::Write,
        summary_en: "Revoke a personal status-only credential",
        key_fields: &[],
    },
    MemberContractMethod {
        name: "me.google.write_confirmations", since: "1.86.0", kind: MemberContractKind::Read,
        summary_en: "Read only your pending Google Drive write confirmations", key_fields: &["requests"],
    },
    MemberContractMethod {
        name: "me.google.write_confirm", since: "1.86.0", kind: MemberContractKind::Write,
        summary_en: "Confirm or cancel your single Google Drive write", key_fields: &["decided"],
    },
    MemberContractMethod {
        name: "me.google.chat_confirmations", since: "1.85.0", kind: MemberContractKind::Read,
        summary_en: "Read only your pending Google Chat confirmations", key_fields: &["requests"],
    },
    MemberContractMethod {
        name: "me.google.chat_confirm", since: "1.85.0", kind: MemberContractKind::Write,
        summary_en: "Confirm or cancel your single Google Chat send", key_fields: &["decided"],
    },
    MemberContractMethod {
        name: "me.google.status", since: "1.85.0", kind: MemberContractKind::Read,
        summary_en: "Read personal Google connection and consent status (Enterprise)",
        key_fields: &["connection", "drive_write", "enabled_by_admin", "login_email", "client_configured", "chat_read", "chat_send", "integration_enabled", "requests", "delegated", "requires_login", "recent_usage"],
    },
    MemberContractMethod {
        name: "me.google.write_connect_start", since: "1.86.0", kind: MemberContractKind::Write,
        summary_en: "Start separate personal Google Drive write authorization (browser session only)",
        key_fields: &[],
    },
    MemberContractMethod {
        name: "me.google.write_disconnect", since: "1.86.0", kind: MemberContractKind::Write,
        summary_en: "Remove the local Google Drive write connection (browser session only)",
        key_fields: &["disconnected", "provider_note"],
    },
    MemberContractMethod {
        name: "me.google.connect_start", since: "1.85.0", kind: MemberContractKind::Write,
        summary_en: "Start personal Google authorization (Enterprise)",
        key_fields: &[],
    },
    MemberContractMethod {
        name: "me.google.connect_finish", since: "1.85.0", kind: MemberContractKind::Write,
        summary_en: "Finish personal Google authorization (Enterprise)",
        key_fields: &[],
    },
    MemberContractMethod {
        name: "me.google.connect_paste", since: "1.85.0", kind: MemberContractKind::Write,
        summary_en: "Finish Google authorization using a pasted callback (Enterprise)",
        key_fields: &[],
    },
    MemberContractMethod {
        name: "me.google.disconnect", since: "1.85.0", kind: MemberContractKind::Write,
        summary_en: "Disconnect the personal Google account (Enterprise)",
        key_fields: &[],
    },
    MemberContractMethod {
        name: "me.google.consents.set", since: "1.85.0", kind: MemberContractKind::Write,
        summary_en: "Set personal Google data-use consent for an AI employee (Enterprise)",
        key_fields: &[],
    },
];

/// Reviewed non-secret leaf paths accepted by key-authenticated agents.update.
/// Enumerated from agents_update and agents_update_v168. New fields, dotted
/// aliases, arbitrary prompt/advanced_kv entries, credential sections and
/// account references are closed until explicitly reviewed here.
pub(crate) const MEMBER_KEY_AGENT_UPDATE_FIELDS: &[&str] = &[
    "agent_id", "display_name", "role", "status", "trigger", "icon", "reports_to", "department",
    "preferred", "fallback", "api_mode", "local_model", "prefer_local", "use_router",
    "when_unavailable", "monthly_limit_cents", "warn_threshold_percent", "hard_stop",
    "heartbeat_enabled", "heartbeat_interval", "heartbeat_cron", "heartbeat_max_concurrent_runs",
    "heartbeat_cron_timezone", "can_create_agents", "can_send_cross_agent", "can_modify_own_skills",
    "can_modify_own_soul", "can_schedule_tasks", "timeout_ms", "sandbox_enabled", "network_access",
    "gvu_enabled", "skill_budget_unlimited", "max_active_skills", "max_silence_hours", "utility",
    "reply_language", "memory_language", "memory.language", "memory.decision_continuity",
    "memory.decision_ttl_days", "budget.daily_cap_cents", "model.effort", "fork.enabled",
    "night_engine.enabled", "team.enabled", "runtime.minimal_context",
    "guardrails.enabled", "guardrails.block_injection_echo", "guardrails.redact_pii",
    "proactive.enabled", "proactive.quiet_hours_start", "proactive.quiet_hours_end",
    "proactive.max_messages_per_hour", "proactive.notify_channel", "proactive.notify_chat_id",
    "proactive.notify_thread_id", "proactive.quiet_hours", "proactive.base_threshold",
    "proactive.max_per_hour", "proactive.timezone", "proactive.max_turns",
    "research.self_study", "research.self_study_hour", "prompt.reply_language",
];

pub(crate) fn member_key_agent_update_params(params: &serde_json::Value) -> Result<(), String> {
    fn visit(value: &serde_json::Value, prefix: &str) -> Result<(), String> {
        let object = value.as_object().ok_or("Member keys require an agents.update parameter object")?;
        for (field, value) in object {
            // Do not normalize aliases: only the reviewed spelling is accepted.
            let path = if prefix.is_empty() { field.clone() } else { format!("{prefix}.{field}") };
            let nested = match path.as_str() {
                "memory" | "budget" | "model" | "fork" | "night_engine" | "team" | "runtime"
                | "guardrails" | "proactive" | "research" | "prompt" => true,
                _ => false,
            };
            if field.chars().all(|c| c.is_ascii_lowercase() || c == '_') && nested {
                visit(value, &path)?;
            } else if field.chars().all(|c| c.is_ascii_lowercase() || c == '_')
                && MEMBER_KEY_AGENT_UPDATE_FIELDS.contains(&path.as_str())
                && !value.is_object() && !value.is_array() {
                continue;
            } else {
                return Err(format!("Member keys cannot update agents.update field '{path}'; use a browser session. No settings were written."));
            }
        }
        Ok(())
    }
    visit(params, "")
}

/// Reads that can expose the holder's conversation content or derived memory.
pub(crate) const MEMBER_KEY_CONVERSATION_METHODS: &[&str] = &[
    "chat.sessions.list", "chat.sessions.history", "chat.sessions.restore",
    "knowledge.citations.for_session", "memory.search", "memory.browse",
    "memory.key_facts", "memory.history", "me.memory.list",
];

/// HTTP reads of conversation content. Knowledge originals are collection documents;
/// uploads, voice transforms, users.me and task creation do not read stored conversations.
pub(crate) const MEMBER_KEY_CONVERSATION_HTTP_OPERATIONS: &[&str] = &[
    "http.chat.attachment",
];

/// Contract membership is necessary, never sufficient: existing ACL and
/// private-data gates still apply. Credential/step-up namespaces stay closed.
pub(crate) fn member_key_method(method: &str) -> Option<MemberContractKind> {
    let excluded = matches!(method, "users.change_password" | "users.change_email"
        | "me.google.write_connect_start" | "me.google.write_disconnect"
        | "me.google.disconnect" | "me.google.consents.set" | "me.google.chat_confirm"
        | "google.signin.get" | "google.signin.set"
        | "users.signin_requests.list" | "users.signin_requests.approve" | "users.signin_requests.reject"
        | "users.google_signin.attest" | "users.google_signin.unlink"
        | "users.signin_onboarding.retry_avatar"
        | "me.google.write_confirmations" | "me.google.write_confirm")
        || matches!(method.split('.').next(), Some("mfa" | "sessions" | "config" | "member_keys"))
        || method.split('.').take(2).eq(["me", "google_signin"])
        || method.split('.').take(2).eq(["me", "status_tokens"])
        || method.split('.').take(2).eq(["me", "member_keys"])
        || method.split('.').take(2).eq(["me", "chat_link"])
        || method.split('.').take(2).eq(["me", "odoo"])
        || method.strip_prefix("me.google.").is_some_and(|op| op.starts_with("connect_"));
    if excluded { return None; }
    MEMBER_CONTRACT_METHODS.iter().find(|entry| entry.name == method).map(|entry| entry.kind)
}

#[cfg(test)]
mod member_key_tests {
    use super::*;
    #[test]
    fn chat_link_methods_are_never_in_member_key_contract() {
        for method in ["me.chat_link.start", "me.chat_link.status", "me.chat_link.confirm",
            "me.chat_link.cancel", "me.chat_link.reject", "me.chat_link.list", "me.chat_link.remove",
            "me.chat_link.future"] {
            assert_eq!(member_key_method(method), None, "{method}");
            assert!(!MEMBER_CONTRACT_METHODS.iter().any(|entry| entry.name == method));
        }
    }
    #[test]
    fn keys_use_contract_intersected_with_credential_exclusions() {
        for method in ["users.change_password", "users.change_email", "me.status_tokens.list",
            "me.member_keys.create", "me.google.connect_start", "me.google.disconnect",
            "me.google.write_connect_start", "me.google.write_disconnect",
            "me.google.consents.set", "sessions.list", "mfa.status", "config.get",
            "system.update_config", "agents.delete", "unknown.method",
            "me.odoo.status", "me.odoo.connect", "me.odoo.test", "me.odoo.disconnect",
            "me.odoo.future", "me.member_keys.narrow", "member_keys.admin_narrow"] {
            assert_eq!(member_key_method(method), None, "{method}");
        }
        assert_eq!(member_key_method("tasks.create"), Some(MemberContractKind::Write));
        assert_eq!(member_key_method("users.me"), Some(MemberContractKind::Read));
    }
    #[test]
    fn key_agent_update_allows_only_reviewed_leaf_paths() {
        assert!(member_key_agent_update_params(&serde_json::json!({
            "agent_id":"a", "display_name":"Example", "heartbeat_enabled":false,
            "memory":{"language":"zh-TW", "decision_ttl_days":7}, "model":{"effort":"high"}
        })).is_ok());
        for params in [serde_json::json!([]), serde_json::json!({"display_name":{"token":"x"}}),
            serde_json::json!({"proactive":[{"secret":"x"}]}), serde_json::json!({"memory.language":"en"}),
            serde_json::json!({"runtime":{"future_key":"x"}}), serde_json::json!({"odoo":{}})] {
            assert!(member_key_agent_update_params(&params).is_err(), "{params}");
        }
    }

}
