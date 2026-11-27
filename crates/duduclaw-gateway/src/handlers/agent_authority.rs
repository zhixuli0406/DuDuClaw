//! The admin-only `agent.toml` fields ("authority" keys).
//!
//! A non-admin Owner keeps every other field of their own employee; these
//! keys govern what the employee can do, reach or spend. The shared core
//! comparator permits proven budget/capability narrowing and reserves other
//! protected changes for administrators. Every write path that lets a non-admin touch `agent.toml`
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

use super::system_update_config_v168::ProtectedChange;

// One list and one base comparator, shared with the MCP caller.
#[allow(unused_imports)]
pub(crate) use duduclaw_core::agent_toml::authority::{
    AuthorityKind, AuthorityKey, AUTHORITY, admin_only_form_paths,
};

/// Before/after of every authority key whose change needs an admin.
pub(crate) fn authority_diff_with_timeout_policy(
    before: &toml::Table, after: &toml::Table, policy: crate::runtime_config::TimeoutPolicy,
) -> Vec<ProtectedChange> {
    // Keep the historical authority baseline (300 s when unset). The
    // runtime's actual Claude default is separate from permission to widen it.
    // A global default/ceiling may narrow this gate, never raise its baseline.
    let materialize = |table: &toml::Table| {
        let mut table = table.clone();
        let runtime = table.entry("runtime").or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if let Some(runtime) = runtime.as_table_mut() {
            let secs = crate::runtime_config::timeout_authorization_baseline(
                runtime.get("timeout_secs").and_then(toml::Value::as_integer), policy,
            );
            runtime.insert("timeout_secs".into(), toml::Value::Integer(secs as i64));
        }
        table
    };
    let mut changes = authority_diff(&materialize(before), &materialize(after));
    // Clearing adopts the actual runtime default of the AFTER provider.
    // That cannot grant an owner permission to increase the limit.
    let stored = |table: &toml::Table| table.get("runtime").and_then(|r| r.get("timeout_secs"))
        .and_then(toml::Value::as_integer);
    if stored(after).is_none() {
        if let Some(previous) = stored(before) {
            let provider = after.get("runtime").and_then(|r| r.get("provider"))
                .and_then(toml::Value::as_str);
            let default = crate::runtime_config::runtime_default_timeout(provider, policy);
            // A clear is judged only by the value that will actually apply,
            // not by the unset authorization baseline used for explicit writes.
            changes.retain(|c| c.key != "runtime.timeout_secs");
            if default > previous.max(30) as u64 {
                changes.push(ProtectedChange { key: "runtime.timeout_secs".into(),
                    before: json!(previous), after: json!(default) });
            }
        }
    }
    changes
}

pub(crate) fn authority_diff_with_runtime_policy(
    before: &toml::Table, after: &toml::Table, timeout: crate::runtime_config::TimeoutPolicy,
    turns: duduclaw_core::turn_limits::TurnPolicy,
) -> Vec<ProtectedChange> {
    let mut changes = authority_diff_with_timeout_policy(before, after, timeout);
    changes.retain(|c| c.key != "runtime.max_turns");
    let baseline = |table: &toml::Table| table.get("runtime").and_then(|r| r.get("max_turns"))
        .and_then(toml::Value::as_integer).filter(|n| *n > 0)
        .map(|n| n as u64).unwrap_or(turns.default.min(turns.limit));
    let (b, a) = (baseline(before), baseline(after));
    if a > b { changes.push(ProtectedChange { key: "runtime.max_turns".into(), before: json!(b), after: json!(a) }); }
    changes
}

pub(crate) fn authority_diff(before: &toml::Table, after: &toml::Table) -> Vec<ProtectedChange> {
    duduclaw_core::agent_toml::authority::authority_diff(before, after).into_iter()
        .map(|c| ProtectedChange { key: c.key, before: c.before, after: c.after }).collect()
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
            ("[runtime]\nutility_provider='openai_compat'\n", "runtime.utility_provider"),
            ("[prompt]\ncli_bare_mode=true\n", "prompt.cli_bare_mode"),
            ("[prompt]\nreply_language='en'\n", "prompt.reply_language"),
            ("[container]\nreadonly_project=false\n", "container.readonly_project"),
            ("[os_watch]\npaths=['/']\n", "os_watch"),
            ("[model]\naccount_pool=['acct']\n", "model.account_pool"),
            ("[permissions]\nallowed_channels=['line']\n", "permissions.allowed_channels"),
            ("[isolation]\npublic_web=true\n", "isolation.public_web"),
        ] {
            let after = t(&format!("[agent]\nname='a'\nrole='specialist'\n[budget]\nmonthly_limit_cents=1000\nhard_stop=true\n{frag}"));
            let diff = authority_diff(&before, &after);
            assert!(diff.iter().any(|c| c.key == k), "{k}: {diff:?}");
        }
        // Explicit defaults over absent keys are not changes.
        let same = t("[agent]\nname='a'\nrole='specialist'\n[budget]\nmonthly_limit_cents=1000\nhard_stop=true\n[runtime]\nprovider='claude'\n[prompt]\ncli_bare_mode=false\n[model]\naccount_pool=[]\n[os_watch]\n[isolation]\npublic_web=false\n");
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
    fn budget_unavailable_only_deny_to_allow_needs_admin() {
        let policy = |value: &str| t(&format!("[budget]\nwhen_unavailable={value}\n"));
        let deny = policy("'deny'");
        let allow = policy("'allow'");
        assert!(authority_diff(&allow, &deny).is_empty());
        assert_eq!(authority_diff(&deny, &allow).into_iter().map(|c| c.key).collect::<Vec<_>>(), vec!["budget.when_unavailable"]);
        assert_eq!(authority_diff(&deny, &t("")).len(), 1, "removing deny restores allow");
        assert_eq!(authority_diff(&policy("'typo'"), &allow).len(), 1, "unknown is effectively deny");
        assert!(authority_diff(&deny, &policy("42")).is_empty());
    }

    #[test]
    fn runtime_timeout_only_effective_increases_need_admin() {
        let value = |v: &str| t(&format!("[runtime]\ntimeout_secs={v}\n"));
        let unset = t("");
        for (before, after, widening) in [
            (unset.clone(), value("301"), true),
            (unset.clone(), value("300"), false),
            (unset.clone(), value("30"), false),
            (value("900"), value("600"), false),
            (value("900"), unset.clone(), false),
            (value("30"), unset.clone(), true),
            (value("4000"), value("3600"), false),
            (value("-1"), value("31"), true),
            (value("'bad'"), value("301"), true),
        ] {
            let changes = authority_diff(&before, &after);
            assert_eq!(!changes.is_empty(), widening, "{before:?} -> {after:?}");
            if widening { assert_eq!(changes[0].key, "runtime.timeout_secs"); }
        }
    }

    /// v1.81 (S2): `can_create_agents` is compared by its effective value, so
    /// an Owner cannot switch it on for a non-main employee by writing `true`
    /// over an absent key; on `main` the same write is no change.
    #[test]
    fn can_create_agents_effective_value_follows_role() {
        let keys = |b: &str, a: &str| authority_diff(&t(b), &t(a)).into_iter().map(|c| c.key).collect::<Vec<_>>();
        for role in ["specialist", "worker", "team-leader"] {
            let absent = format!("[agent]\nrole='{role}'\n");
            let on = format!("{absent}[permissions]\ncan_create_agents=true\n");
            let off = format!("{absent}[permissions]\ncan_create_agents=false\n");
            assert_eq!(keys(&absent, &on), vec!["permissions.can_create_agents"], "{role}");
            assert!(keys(&absent, &off).is_empty(), "{role}: explicit default");
            assert_eq!(keys(&on, &absent), vec!["permissions.can_create_agents"], "{role}: removing an explicit true");
        }
        // No role: fails closed like a non-main employee.
        assert_eq!(keys("", "[permissions]\ncan_create_agents=true\n"), vec!["permissions.can_create_agents"]);
        let main = "[agent]\nrole='main'\n";
        assert!(keys(main, &format!("{main}[permissions]\ncan_create_agents=true\n")).is_empty());
        assert_eq!(
            keys(main, &format!("{main}[permissions]\ncan_create_agents=false\n")),
            vec!["permissions.can_create_agents"]
        );
    }

    #[test]
    fn role_main_both_ways() {
        let s = t("[agent]\nrole='specialist'\n");
        let m = t("[agent]\nrole='main'\n");
        let w = t("[agent]\nrole='worker'\n");
        // v1.81 (S2): becoming / ceasing to be main also flips the effective
        // `can_create_agents` of an employee that never wrote the key.
        let keys = |b: &toml::Table, a: &toml::Table| authority_diff(b, a).into_iter().map(|c| c.key).collect::<Vec<_>>();
        assert_eq!(keys(&s, &m), vec!["agent.role", "permissions.can_create_agents"]);
        assert_eq!(keys(&m, &s), vec!["agent.role", "permissions.can_create_agents"]);
        assert!(authority_diff(&s, &w).is_empty());
        let pinned = |role: &str| t(&format!("[agent]\nrole='{role}'\n[permissions]\ncan_create_agents=false\n"));
        assert_eq!(keys(&pinned("specialist"), &pinned("main")), vec!["agent.role"]);
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

    #[test]
    fn clearing_timeout_checks_the_actual_after_provider_default() {
        let policy = crate::runtime_config::TimeoutPolicy::default();
        for (provider, previous, requires_admin) in [
            ("claude", 900, true), ("codex", 200, true), ("claude", 3600, false),
            ("Claude", 900, true), (" claude ", 900, true), ("removed-runtime", 900, true),
        ] {
            let before = t(&format!("[runtime]\nprovider='{provider}'\ntimeout_secs={previous}\n"));
            let after = t(&format!("[runtime]\nprovider='{provider}'\n"));
            assert_eq!(authority_diff_with_timeout_policy(&before, &after, policy)
                .iter().any(|c| c.key == "runtime.timeout_secs"), requires_admin);
        }
        let before = t("[runtime]\nprovider='codex'\ntimeout_secs=900\n");
        let after = t("[runtime]\nprovider='claude'\n");
        assert!(authority_diff_with_timeout_policy(&before, &after, policy)
            .iter().any(|c| c.key == "runtime.timeout_secs"));
    }

    #[test]
    fn timeout_tests_global_policy_does_not_replace_stored_authority() {
        let policy = crate::runtime_config::TimeoutPolicy { default_secs: Some(1200), max_secs: 7200 };
        let unset = t("[runtime]\nprovider='claude'\n");
        let raised = t("[runtime]\nprovider='claude'\ntimeout_secs=600\n");
        assert!(authority_diff_with_timeout_policy(&unset, &raised, policy).iter().any(|c| c.key == "runtime.timeout_secs"));
        let stored = t("[runtime]\ntimeout_secs=120\n");
        let raised = t("[runtime]\ntimeout_secs=121\n");
        assert!(authority_diff_with_timeout_policy(&stored, &raised, policy).iter().any(|c| c.key == "runtime.timeout_secs"));
        let policy = crate::runtime_config::TimeoutPolicy { default_secs: None, max_secs: 60 };
        // Comparing clamped values would hide this raise (both become 60).
        assert!(authority_diff_with_timeout_policy(&stored, &raised, policy).iter().any(|c| c.key == "runtime.timeout_secs"));
    }

    #[test]
    fn explicit_timeouts_use_the_authorization_baseline_not_the_clear_default() {
        let policy = crate::runtime_config::TimeoutPolicy::default();
        let unset = t("[runtime]\nprovider='claude'\n");
        for seconds in [30, 120, 300] {
            let explicit = t(&format!("[runtime]\nprovider='claude'\ntimeout_secs={seconds}\n"));
            assert!(authority_diff_with_timeout_policy(&unset, &explicit, policy).is_empty());
        }
        let before = t("[runtime]\nprovider='claude'\ntimeout_secs=600\n");
        let after = t("[runtime]\nprovider='claude'\ntimeout_secs=450\n");
        assert!(authority_diff_with_timeout_policy(&before, &after, policy).is_empty());
        let policy = crate::runtime_config::TimeoutPolicy { default_secs: Some(30), max_secs: 3600 };
        let before = t("[runtime]\nprovider='claude'\ntimeout_secs=60\n");
        assert!(authority_diff_with_timeout_policy(&before, &unset, policy).is_empty());
    }

    #[test]
    fn timeout_authority_uses_actual_defaults_and_global_ceiling() {
        let policy = crate::runtime_config::TimeoutPolicy { default_secs: Some(1200), max_secs: 7200 };
        let before = t("[runtime]\nprovider='claude'\ntimeout_secs=900\n");
        let unset = t("[runtime]\nprovider='claude'\n");
        assert!(authority_diff_with_timeout_policy(&before, &unset, policy).iter().any(|c| c.key == "runtime.timeout_secs"));
        assert!(authority_diff_with_timeout_policy(&unset, &before, policy).iter().any(|c| c.key == "runtime.timeout_secs"));
        let after = t("[runtime]\nprovider='claude'\ntimeout_secs=5000\n");
        let before = t("[runtime]\nprovider='claude'\ntimeout_secs=4000\n");
        assert!(authority_diff_with_timeout_policy(&before, &after, policy).iter().any(|c| c.key == "runtime.timeout_secs"));
    }

}

#[cfg(test)]
mod turn_tests {
    use super::*;
    #[test]
    fn owners_can_only_narrow_steps_including_clear_and_stored_over_ceiling() {
        let table = |s: &str| s.parse::<toml::Table>().unwrap();
        let timeout = crate::runtime_config::TimeoutPolicy::default();
        let turns = duduclaw_core::turn_limits::TurnPolicy::from_table(&"[runtime]\ndefault_max_turns=70\nmax_turns_limit=90".parse().unwrap());
        for (before, after, admin) in [
            ("", "[runtime]\nmax_turns=71", true),
            ("", "[runtime]\nmax_turns=60", false),
            ("[runtime]\nmax_turns=60", "", true),
            ("[runtime]\nmax_turns=80", "", false),
            ("[runtime]\nmax_turns=100", "[runtime]\nmax_turns=110", true),
        ] {
            assert_eq!(authority_diff_with_runtime_policy(&table(before), &table(after), timeout, turns)
                .iter().any(|c| c.key == "runtime.max_turns"), admin);
        }
    }
}

/// Compare the limit that will actually apply, including a clear to global default.
pub(crate) fn idle_authority_diff(before: &toml::Table, after: &toml::Table, default: u64) -> Vec<ProtectedChange> {
    let read = |table: &toml::Table| table.get("runtime").and_then(|r| r.get("idle_timeout_secs"))
        .and_then(toml::Value::as_integer).and_then(|v| u64::try_from(v).ok()).unwrap_or(default);
    let (b, a) = (read(before), read(after));
    if a != b && (a == 0 || (b != 0 && a > b)) {
        vec![ProtectedChange { key: "runtime.idle_timeout_secs".into(), before: json!(b), after: json!(a) }]
    } else { vec![] }
}

#[cfg(test)]
mod idle_tests {
    use super::*;
    #[test]
    fn idle_raise_disable_and_clear_require_authority() {
        let table = |v: Option<u64>| v.map(|v| format!("[runtime]\nidle_timeout_secs={v}")).unwrap_or_default().parse::<toml::Table>().unwrap();
        for (before, after, admin) in [(None,Some(901),true), (None,Some(0),true),
            (None,Some(60),false), (Some(60),None,true), (Some(0),Some(900),false),
            (Some(1000),None,false), (Some(900),Some(900),false)] {
            assert_eq!(!idle_authority_diff(&table(before), &table(after), 900).is_empty(),admin);
        }
        assert!(!idle_authority_diff(&table(Some(60)), &table(None), 0).is_empty());
    }
}

#[cfg(test)]
mod az1_tests {
    use super::*;

    #[test]
    fn az1_capabilities_allow_revocation_but_refuse_grants() {
        let table = |s: &str| s.parse::<toml::Table>().unwrap();
        for (before, after) in [
            ("db_sources=['fixture_db','old']", "db_sources=['fixture_db']"),
            ("git_credentials=true", "git_credentials=false"),
            ("allowed_tools=['Read','Bash']", "allowed_tools=['Read']"),
            ("denied_tools=['Bash']", "denied_tools=['Bash','Write']"),
        ] {
            let b = table(&format!("[capabilities]\n{before}"));
            let a = table(&format!("[capabilities]\n{after}"));
            assert!(authority_diff(&b, &a).is_empty(), "{before} -> {after}");
            assert!(!authority_diff(&a, &b).is_empty(), "reverse widens");
        }
        let b = table("[capabilities]\nallowed_tools=['Read']");
        assert!(!authority_diff(&b, &table("[capabilities]\nallowed_tools=[]")).is_empty());
        assert!(!authority_diff(&table(""), &table("[capabilities]\nunknown_grant=true")).is_empty());
    }
}
