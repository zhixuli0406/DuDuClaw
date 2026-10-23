//! The admin-only `agent.toml` fields ("authority" keys).
//!
//! A non-admin Owner keeps every other field of their own employee; these
//! keys widen what the employee can do, reach or spend, so only an admin may
//! change them. Every write path that lets a non-admin touch `agent.toml`
//! (`agents.update`, and through it `os.settings.update`) runs
//! [`authority_diff`] over the before/after tables and refuses (and audits
//! `agent_authority_refused`) when a non-admin caller changed one.
//!
//! The same table, with the dashboard's form dirty-paths, is checked against
//! `web/src/pages/agent-form/__fixtures__/authority-fields.json` by a test on
//! each side, so the server and the edit page cannot drift (the edit page
//! drops admin-only dirty paths for non-admins and gets the list from
//! `agents.inspect.admin_only_fields`).

#[allow(unused_imports)]
use super::*;

use super::config_commit::toml_at_json;
use super::system_update_config_v168::ProtectedChange;

/// What counts as a change that needs an admin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AuthorityKind {
    /// Any change of the effective value.
    Change,
    /// Only loosening: raising a budget cap (0 = no cap) or turning
    /// `hard_stop` off. Tightening stays open to the Owner.
    Loosen,
    /// Becoming, or ceasing to be, `role = "main"`.
    RoleMain,
}

impl AuthorityKind {
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Change => "change",
            Self::Loosen => "loosen",
            Self::RoleMain => "role_main",
        }
    }
}

/// One admin-only field: its TOML path (a whole table when it names one),
/// the dashboard form dirty-paths that edit it (a trailing `.` is a prefix),
/// and what counts as a change.
#[derive(Debug, Clone, Copy)]
pub(crate) struct AuthorityKey {
    pub toml_key: &'static str,
    pub form_paths: &'static [&'static str],
    pub kind: AuthorityKind,
}

const fn key(toml_key: &'static str, form_paths: &'static [&'static str], kind: AuthorityKind) -> AuthorityKey {
    AuthorityKey { toml_key, form_paths, kind }
}

use AuthorityKind::{Change, Loosen, RoleMain};

/// Every admin-only field. Keep in sync with the fixture (a test enforces it).
pub(crate) const AUTHORITY: &[AuthorityKey] = &[
    key("agent.reports_to", &["form.reports_to"], Change),
    key("agent.department", &["form.department"], Change),
    key("agent.name", &[], Change),
    // `main` decides shared-wiki deletion and default dispatch.
    key("agent.role", &["form.role"], RoleMain),
    key("capabilities", &["caps."], Change),
    key("container.sandbox_enabled", &["form.sandbox_enabled"], Change),
    key("container.network_access", &["form.network_access"], Change),
    // Not written by the form any more; listed so it cannot come back unguarded.
    key("container.readonly_project", &[], Change),
    key("permissions.can_modify_own_soul", &["form.can_modify_own_soul"], Change),
    // v1.68: enforced at the MCP gate, so they are authority too.
    key("permissions.can_create_agents", &["form.can_create_agents"], Change),
    key("permissions.can_send_cross_agent", &["form.can_send_cross_agent"], Change),
    key("permissions.can_schedule_tasks", &["form.can_schedule_tasks"], Change),
    key("permissions.can_modify_own_skills", &["form.can_modify_own_skills"], Change),
    // Which channels the employee may answer on.
    key("permissions.allowed_channels", &[], Change),
    key("guardrails.enabled", &["v.guardrails_enabled"], Change),
    key("guardrails.block_secrets", &["v.guardrails_block_secrets"], Change),
    // v1.76: opens the employee to `@` calls from unmapped channel senders.
    key("channels.public_trigger", &["v.public_trigger"], Change),
    // A per-employee bot token connects a new outside channel
    // (`channels.add` is admin-only).
    key("channels.discord", &[], Change),
    key("channels.telegram", &[], Change),
    key("channels.slack", &[], Change),
    // B11: an employee's own Google Chat app (an organisation cloud
    // credential and a new inbound endpoint).
    key("channels.googlechat", &[], Change),
    // ERP models, actions, companies and credentials
    // (`odoo.agent_config_set` is admin-only).
    key("odoo", &["odoo."], Change),
    // The runtime decides the sandbox and the tool semantics.
    key("runtime.provider", &["runtime.provider"], Change),
    key("runtime.fallback", &["runtime.fallback"], Change),
    // `--bare` skips the hooks, the employee file guard included.
    key("prompt.cli_bare_mode", &[], Change),
    // Widens what the employee watches on the machine.
    key("os_watch", &["osWatch."], Change),
    // Which accounts' quota the employee spends.
    key("model.account_pool", &["adv.account_pool"], Change),
    key("budget.monthly_limit_cents", &["form.monthly_limit_cents"], Loosen),
    key("budget.daily_cap_cents", &["v.daily_cap_cents"], Loosen),
    key("budget.hard_stop", &["form.hard_stop"], Loosen),
];

/// The effective value of an authority key: an absent key reads as the value
/// the loader would use, so writing an explicit default over an absent key
/// is not a change.
fn effective(table: &toml::Table, key: &str) -> Value {
    let raw = toml_at_json(table, key);
    let or = |v: Value| if raw.is_null() { v } else { raw.clone() };
    match key {
        "agent.reports_to" | "agent.department" | "agent.name" | "agent.role" | "runtime.fallback" => {
            or(Value::String(String::new()))
        }
        "runtime.provider" => or(Value::String(duduclaw_core::types::RuntimeType::default().as_str().into())),
        "container.sandbox_enabled"
        | "container.network_access"
        | "permissions.can_modify_own_soul"
        | "guardrails.enabled"
        | "channels.public_trigger"
        | "prompt.cli_bare_mode"
        | "budget.hard_stop" => or(Value::Bool(false)),
        // Absent ⇒ allowed (the gate refuses only an explicit `false`);
        // `block_secrets` defaults on.
        "permissions.can_create_agents"
        | "permissions.can_send_cross_agent"
        | "permissions.can_schedule_tasks"
        | "permissions.can_modify_own_skills"
        | "guardrails.block_secrets" => or(Value::Bool(true)),
        "model.account_pool" | "permissions.allowed_channels" => or(json!([])),
        "budget.monthly_limit_cents" | "budget.daily_cap_cents" => or(json!(0)),
        "capabilities" => {
            let defaults = serde_json::to_value(duduclaw_core::types::CapabilitiesConfig::default())
                .unwrap_or_else(|_| json!({}));
            let mut obj = raw.as_object().cloned().unwrap_or_default();
            obj.retain(|k, v| {
                let is_default = defaults.get(k) == Some(v);
                let is_empty = v.as_array().is_some_and(|a| a.is_empty())
                    || v.as_object().is_some_and(|o| o.is_empty());
                !(is_default || (is_empty && defaults.get(k).is_none()))
            });
            Value::Object(obj)
        }
        // Whole tables: an empty table is the same as none.
        _ => match raw {
            Value::Object(o) if o.is_empty() => Value::Null,
            v => v,
        },
    }
}

/// A budget cap where 0 means "no cap": is `after` looser than `before`?
fn cap_loosened(before: &Value, after: &Value) -> bool {
    let b = before.as_u64().unwrap_or(0);
    let a = after.as_u64().unwrap_or(0);
    b > 0 && (a == 0 || a > b)
}

fn needs_admin(k: &AuthorityKey, before: &Value, after: &Value) -> bool {
    if before == after {
        return false;
    }
    match k.kind {
        Change => true,
        RoleMain => (before.as_str() == Some("main")) != (after.as_str() == Some("main")),
        Loosen if k.toml_key == "budget.hard_stop" => {
            before.as_bool() == Some(true) && after.as_bool() != Some(true)
        }
        Loosen => cap_loosened(before, after),
    }
}

/// Before/after of every authority key whose change needs an admin.
pub(crate) fn authority_diff(before: &toml::Table, after: &toml::Table) -> Vec<ProtectedChange> {
    AUTHORITY
        .iter()
        .filter_map(|k| {
            let (b, a) = (effective(before, k.toml_key), effective(after, k.toml_key));
            needs_admin(k, &b, &a).then(|| ProtectedChange { key: k.toml_key.to_string(), before: b, after: a })
        })
        .collect()
}

/// Form dirty-paths a non-admin may never send (`Change` keys only: a
/// loosening or `role = "main"` depends on the value, so the server judges
/// those). For `agents.inspect.admin_only_fields`.
pub(crate) fn admin_only_form_paths() -> Vec<&'static str> {
    AUTHORITY
        .iter()
        .filter(|k| k.kind == Change)
        .flat_map(|k| k.form_paths.iter().copied())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../../../../web/src/pages/agent-form/__fixtures__/authority-fields.json");

    fn t(s: &str) -> toml::Table {
        s.parse().unwrap()
    }

    #[test]
    fn table_matches_the_dashboard_fixture() {
        let fx: Value = serde_json::from_str(FIXTURE).unwrap();
        let rows = fx["fields"].as_array().unwrap();
        assert_eq!(rows.len(), AUTHORITY.len(), "fixture and AUTHORITY differ in length");
        for (row, k) in rows.iter().zip(AUTHORITY) {
            assert_eq!(row["key"], k.toml_key);
            assert_eq!(row["kind"], k.kind.as_str(), "{}", k.toml_key);
            let paths: Vec<&str> = row["form_paths"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
            assert_eq!(paths, k.form_paths, "{}", k.toml_key);
        }
        let kv: Vec<&str> = fx["kv_refused_sections"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
        let mut server: Vec<&str> = super::super::agents_update_v168::KV_DEAD_SECTIONS.to_vec();
        server.extend(super::super::agents_update_v168::KV_DENIED_SECTIONS);
        let (mut a, mut b) = (kv.clone(), server.clone());
        a.sort();
        b.sort();
        assert_eq!(a, b, "kv_refused_sections must equal dead + denied sections");
    }

    #[test]
    fn new_keys_need_an_admin_and_resending_is_not_a_change() {
        let before = t("[agent]\nname='a'\nrole='specialist'\n[budget]\nmonthly_limit_cents=1000\nhard_stop=true\n");
        for (frag, k) in [
            ("[channels.discord]\nbot_token_enc='x'\n", "channels.discord"),
            ("[odoo]\nallowed_models=['res.partner']\n", "odoo"),
            ("[runtime]\nprovider='codex'\n", "runtime.provider"),
            ("[runtime]\nfallback='claude'\n", "runtime.fallback"),
            ("[prompt]\ncli_bare_mode=true\n", "prompt.cli_bare_mode"),
            ("[container]\nreadonly_project=false\n", "container.readonly_project"),
            ("[os_watch]\npaths=['/']\n", "os_watch"),
            ("[model]\naccount_pool=['acct']\n", "model.account_pool"),
            ("[permissions]\nallowed_channels=['line']\n", "permissions.allowed_channels"),
        ] {
            let after = t(&format!("[agent]\nname='a'\nrole='specialist'\n[budget]\nmonthly_limit_cents=1000\nhard_stop=true\n{frag}"));
            let diff = authority_diff(&before, &after);
            assert!(diff.iter().any(|c| c.key == k), "{k}: {diff:?}");
        }
        // Explicit defaults over absent keys are not changes.
        let same = t("[agent]\nname='a'\nrole='specialist'\n[budget]\nmonthly_limit_cents=1000\nhard_stop=true\n[runtime]\nprovider='claude'\n[prompt]\ncli_bare_mode=false\n[model]\naccount_pool=[]\n[os_watch]\n");
        assert!(authority_diff(&before, &same).is_empty(), "{:?}", authority_diff(&before, &same));
    }

    #[test]
    fn budget_only_loosening_needs_an_admin() {
        let base = |m: u64, d: u64, hs: bool| {
            t(&format!("[budget]\nmonthly_limit_cents={m}\ndaily_cap_cents={d}\nhard_stop={hs}\n"))
        };
        let before = base(1000, 100, true);
        assert!(authority_diff(&before, &base(500, 50, true)).is_empty(), "lowering is allowed");
        assert!(authority_diff(&base(0, 0, false), &base(500, 50, true)).is_empty(), "adding a cap is allowed");
        let keys = |a: &toml::Table| authority_diff(&before, a).into_iter().map(|c| c.key).collect::<Vec<_>>();
        assert_eq!(keys(&base(2000, 100, true)), vec!["budget.monthly_limit_cents"]);
        assert_eq!(keys(&base(1000, 0, true)), vec!["budget.daily_cap_cents"], "0 removes the cap");
        assert_eq!(keys(&base(1000, 100, false)), vec!["budget.hard_stop"]);
    }

    #[test]
    fn role_main_both_ways() {
        let s = t("[agent]\nrole='specialist'\n");
        let m = t("[agent]\nrole='main'\n");
        let w = t("[agent]\nrole='worker'\n");
        assert_eq!(authority_diff(&s, &m).len(), 1);
        assert_eq!(authority_diff(&m, &s).len(), 1);
        assert!(authority_diff(&s, &w).is_empty());
    }

    #[test]
    fn admin_only_paths_are_the_change_kind_paths() {
        let p = admin_only_form_paths();
        for path in ["form.can_create_agents", "v.guardrails_enabled", "odoo.", "osWatch.", "adv.account_pool", "caps."] {
            assert!(p.contains(&path), "{path}");
        }
        for path in ["form.monthly_limit_cents", "form.hard_stop", "form.role", "v.daily_cap_cents"] {
            assert!(!p.contains(&path), "{path} is judged by value on the server");
        }
    }
}
