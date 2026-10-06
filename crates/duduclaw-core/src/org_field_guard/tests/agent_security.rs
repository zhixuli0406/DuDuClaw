use super::*;

// ── C: own `agent.toml` sections that security gates read ───────────────
//
// WP21 froze `[agent]` org fields and the whole `[capabilities]` table. The
// rest of the file was writable by its own agent through Write/Edit even
// though gates read it: `[permissions]` (MCP permission gate,
// `can_modify_own_soul`), `[container]` (task sandbox), `[budget]`,
// `[runtime] provider` (a non-Claude runtime never runs this hook), `[prompt]
// cli_bare_mode` (Claude `--bare` skips hooks), `[evolution]` (the AEE commit
// gate's noise band) and more. These rows apply to agent callers only.

fn agent_path() -> PathBuf {
    agent_toml()
}

fn with(extra: &str) -> String {
    format!("{BASE}\n{extra}\n")
}

fn as_agent(existing: &str, new: &str) -> GuardDecision {
    check_protected_toml_write_as(&agent_path(), &home(), &agent("agnes"), Some(existing), new)
}

/// `(section text before, section text after)` for every frozen section;
/// each pair changes exactly one security-relevant value.
const SECTION_CASES: &[(&str, &str, &str)] = &[
    (
        "permissions",
        "[permissions]\ncan_modify_own_soul = false",
        "[permissions]\ncan_modify_own_soul = true",
    ),
    (
        "container",
        "[container]\nsandbox_enabled = true",
        "[container]\nsandbox_enabled = false",
    ),
    (
        "budget",
        "[budget]\nmonthly_limit_cents = 1000\nhard_stop = true",
        "[budget]\nmonthly_limit_cents = 1000\nhard_stop = false",
    ),
    (
        "guardrails",
        "[guardrails]\nenabled = true\nblock_secrets = true",
        "[guardrails]\nenabled = true\nblock_secrets = false",
    ),
    (
        "runtime",
        "[runtime]\nprovider = \"claude\"",
        "[runtime]\nprovider = \"codex\"",
    ),
    (
        "fork",
        "[fork]\nenabled = false",
        "[fork]\nenabled = true",
    ),
    (
        "evolution",
        "[evolution]\ngvu_enabled = true\n[evolution.noise_band]\nmain = 0.02",
        "[evolution]\ngvu_enabled = true\n[evolution.noise_band]\nmain = 0.5",
    ),
    (
        "task_forward_model",
        "[task_forward_model]\nheld_out_gate_enabled = true",
        "[task_forward_model]\nheld_out_gate_enabled = false",
    ),
    (
        "channels",
        "[channels.telegram]\nbot_token_enc = \"a\"",
        "[channels.telegram]\nbot_token_enc = \"b\"",
    ),
    (
        "odoo",
        "[odoo]\nallowed_models = [\"crm.lead\"]",
        "[odoo]\nallowed_models = [\"*\"]",
    ),
    (
        "mcp",
        "[[mcp.external]]\nname = \"x\"\ndenied_tools = [\"rm\"]",
        "[[mcp.external]]\nname = \"x\"\ndenied_tools = []",
    ),
    (
        "redaction",
        "[redaction]\nenabled = true",
        "[redaction]\nenabled = false",
    ),
    (
        "ptc",
        "[ptc]\nenabled = false",
        "[ptc]\nenabled = true",
    ),
    (
        "night_engine",
        "[night_engine]\nmax_pass_cost_cents = 10",
        "[night_engine]\nmax_pass_cost_cents = 100000",
    ),
    (
        "team",
        "[team.roles.verifier]\nruntime = \"codex\"",
        "[team.roles.verifier]\nruntime = \"claude\"",
    ),
    (
        "os_watch",
        "[os_watch]\npaths = [\"~/Documents\"]",
        "[os_watch]\npaths = [\"~/\"]",
    ),
];

#[test]
fn every_security_section_change_is_refused_for_an_agent() {
    for (section, before, after) in SECTION_CASES {
        let d = as_agent(&with(before), &with(after));
        match &d {
            GuardDecision::BlockedAgentSecuritySection { changed, .. } => {
                assert!(
                    changed.iter().any(|c| c.starts_with(&format!("{section}."))
                        || c.starts_with(&format!("[{section}]"))),
                    "{section}: {changed:?}"
                );
            }
            other => panic!("{section}: expected BlockedAgentSecuritySection, got {other:?}"),
        }
        assert!(!d.is_allowed());
        assert!(d.block_message().unwrap().contains("agent.toml"));
    }
}

#[test]
fn adding_or_removing_a_security_section_is_refused() {
    // Absent → present (e.g. self-granting `[permissions]` from nothing)…
    let d = as_agent(BASE, &with("[permissions]\ncan_create_agents = true"));
    assert!(matches!(d, GuardDecision::BlockedAgentSecuritySection { .. }), "{d:?}");
    // …present → absent (deleting the sandbox switch)…
    let d = as_agent(&with("[container]\nsandbox_enabled = true"), BASE);
    assert!(matches!(d, GuardDecision::BlockedAgentSecuritySection { .. }), "{d:?}");
    // …and reshaping a table into a scalar.
    let d = as_agent(&with("[budget]\nhard_stop = true"), &with("budget = 1"));
    assert!(matches!(d, GuardDecision::BlockedAgentSecuritySection { .. }), "{d:?}");
}

#[test]
fn single_security_keys_are_frozen_and_their_siblings_are_not() {
    // `[prompt] cli_bare_mode` makes the Claude CLI skip hooks.
    let d = as_agent(
        &with("[prompt]\nmode = \"full\"\ncli_bare_mode = false"),
        &with("[prompt]\nmode = \"full\"\ncli_bare_mode = true"),
    );
    assert!(matches!(d, GuardDecision::BlockedAgentSecuritySection { .. }), "{d:?}");
    assert_eq!(
        as_agent(
            &with("[prompt]\nmode = \"full\""),
            &with("[prompt]\nmode = \"minimal\"")
        ),
        GuardDecision::AllowedAgentWrite
    );
    // `[model] account_pool` decides whose credentials the agent spends;
    // `[model] preferred` stays editable.
    let before = BASE.replace(
        "preferred = \"sonnet\"",
        "preferred = \"sonnet\"\naccount_pool = [\"a\"]",
    );
    let after = BASE.replace(
        "preferred = \"sonnet\"",
        "preferred = \"sonnet\"\naccount_pool = [\"a\", \"b\"]",
    );
    let d = as_agent(&before, &after);
    match &d {
        GuardDecision::BlockedAgentSecuritySection { changed, .. } => {
            assert!(changed.iter().any(|c| c.starts_with("model.account_pool")), "{changed:?}");
        }
        other => panic!("expected BlockedAgentSecuritySection, got {other:?}"),
    }
    assert_eq!(
        as_agent(&before, &before.replace("\"sonnet\"", "\"opus\"")),
        GuardDecision::AllowedAgentWrite
    );
    // `[agent] role` decides `shared_wiki_delete`'s main-agent rule.
    let d = as_agent(BASE, &BASE.replace("role = \"assistant\"", "role = \"main\""));
    assert!(matches!(d, GuardDecision::BlockedAgentSecuritySection { .. }), "{d:?}");
}

#[test]
fn unchanged_security_sections_and_ordinary_sections_pass() {
    let all: String = SECTION_CASES
        .iter()
        .map(|(_, before, _)| *before)
        .collect::<Vec<_>>()
        .join("\n");
    let existing = with(&all);
    // Same security sections, edits elsewhere.
    let edited = existing
        .replace("display_name = \"Agnes\"", "display_name = \"Agnes 2\"")
        .replace("preferred = \"sonnet\"", "preferred = \"opus\"")
        + "\n[heartbeat]\nenabled = true\n[proactive]\nenabled = true\n";
    assert_eq!(as_agent(&existing, &edited), GuardDecision::AllowedAgentWrite);
}

#[test]
fn unparseable_content_is_still_refused() {
    let d = as_agent(BASE, "[permissions\ncan_create_agents = true");
    assert!(matches!(d, GuardDecision::BlockedUnverifiable { .. }), "{d:?}");
    let d = as_agent("[[[broken", &with("[permissions]\ncan_create_agents = true"));
    assert!(matches!(d, GuardDecision::BlockedUnverifiable { .. }), "{d:?}");
}

#[test]
fn org_fields_and_capabilities_keep_their_own_verdicts_ahead_of_the_new_rows() {
    let before = with("[capabilities]\ncomputer_use = false\n[permissions]\ncan_create_agents = false");
    let after_org = before.replace("reports_to = \"ceo\"", "reports_to = \"cfo\"")
        .replace("can_create_agents = false", "can_create_agents = true");
    assert!(matches!(
        as_agent(&before, &after_org),
        GuardDecision::BlockedOrgFieldChange { .. }
    ));
    let after_caps = before
        .replace("computer_use = false", "computer_use = true")
        .replace("can_create_agents = false", "can_create_agents = true");
    assert!(matches!(
        as_agent(&before, &after_caps),
        GuardDecision::BlockedProtectedField { .. }
    ));
}

#[test]
fn operator_keeps_the_pre_existing_rules_only() {
    let before = with("[permissions]\ncan_create_agents = false");
    let after = with("[permissions]\ncan_create_agents = true");
    assert_eq!(
        check_protected_toml_write_as(
            &agent_path(),
            &home(),
            &HookCaller::Absent,
            Some(&before),
            &after
        ),
        GuardDecision::AllowedAgentWrite
    );
    // The caller-less entry point keeps its old contract too.
    assert_eq!(
        check_protected_toml_write(&agent_path(), &home(), Some(&before), &after),
        GuardDecision::AllowedAgentWrite
    );
    // …while the WP21 rows still apply to the operator, as before.
    let moved = before.replace("reports_to = \"ceo\"", "reports_to = \"cfo\"");
    assert!(matches!(
        check_protected_toml_write_as(
            &agent_path(),
            &home(),
            &HookCaller::Absent,
            Some(&before),
            &moved
        ),
        GuardDecision::BlockedOrgFieldChange { .. }
    ));
}

#[test]
fn config_toml_is_not_affected_by_the_agent_rows() {
    // The new rows are `AgentToml` only; `<home>/config.toml` is refused
    // outright for agents by `check_caller_scope` anyway.
    let p = home().join("config.toml");
    let before = "[budget]\nx = 1\n[delegation]\npolicy = \"department\"\n";
    let after = "[budget]\nx = 2\n[delegation]\npolicy = \"department\"\n";
    assert_eq!(
        check_protected_toml_write_as(&p, &home(), &agent("agnes"), Some(before), after),
        GuardDecision::AllowedAgentWrite
    );
}

// ── Drift guard: every known `agent.toml` section is classified ──────────

/// Field names of a `#[derive(Deserialize)]` struct, captured from the
/// `deserialize_struct` call serde makes (no instance needed).
fn struct_fields<T: serde::de::DeserializeOwned>() -> Vec<&'static str> {
    use serde::de::{self, Deserializer, Visitor};
    use std::cell::RefCell;

    #[derive(Debug)]
    struct Stop;
    impl std::fmt::Display for Stop {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("stop")
        }
    }
    impl std::error::Error for Stop {}
    impl de::Error for Stop {
        fn custom<M: std::fmt::Display>(_: M) -> Self {
            Stop
        }
    }

    struct Capture<'a>(&'a RefCell<Vec<&'static str>>);
    impl<'de> Deserializer<'de> for Capture<'_> {
        type Error = Stop;
        fn deserialize_any<V: Visitor<'de>>(self, _: V) -> Result<V::Value, Stop> {
            Err(Stop)
        }
        fn deserialize_struct<V: Visitor<'de>>(
            self,
            _: &'static str,
            fields: &'static [&'static str],
            _: V,
        ) -> Result<V::Value, Stop> {
            self.0.borrow_mut().extend_from_slice(fields);
            Err(Stop)
        }
        serde::forward_to_deserialize_any! {
            bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
            bytes byte_buf option unit unit_struct newtype_struct seq tuple
            tuple_struct map enum identifier ignored_any
        }
    }

    let out = RefCell::new(Vec::new());
    let _ = T::deserialize(Capture(&out));
    out.into_inner()
}

/// Sections read straight out of `agent.toml` by readers that use neither
/// `AgentConfig` nor `AgentTomlSections`.
const OTHER_READER_SECTIONS: &[&str] = &[
    "research",           // self_study.rs
    "redaction",          // redaction_integration.rs
    "odoo",               // RFC-21 per-agent credentials
    "task_forward_model", // prediction layer
    "preset",             // preset.rs mirror (informational)
    "planner",            // mcp_planner.rs
];

#[test]
fn every_known_agent_toml_section_is_frozen_or_listed_editable() {
    let mut known: Vec<&str> = struct_fields::<crate::types::AgentConfig>();
    known.extend(struct_fields::<crate::agent_toml::AgentTomlSections>());
    known.extend_from_slice(OTHER_READER_SECTIONS);
    known.sort_unstable();
    known.dedup();
    assert!(known.len() > 20, "field capture broke: {known:?}");

    let key_frozen: Vec<&str> = AGENT_SECURITY_KEYS.iter().map(|(s, _)| *s).collect();
    let unclassified: Vec<&str> = known
        .iter()
        .copied()
        .filter(|s| {
            *s != AGENT_CAPABILITY_SECTION
                && !AGENT_SECURITY_SECTIONS.contains(s)
                && !key_frozen.contains(s)
                && !AGENT_EDITABLE_SECTIONS.contains(s)
        })
        .collect();
    assert!(
        unclassified.is_empty(),
        "agent.toml sections neither frozen nor listed editable: {unclassified:?} — \
         add each to AGENT_SECURITY_SECTIONS (a security gate reads it) or \
         AGENT_EDITABLE_SECTIONS (none does) in org_field_guard/rules.rs"
    );
    // The two lists must not overlap.
    for s in AGENT_EDITABLE_SECTIONS {
        assert!(!AGENT_SECURITY_SECTIONS.contains(s), "{s} is in both lists");
    }
    // Round 3: the editable list has no typos (every entry is a real section)…
    for s in AGENT_EDITABLE_SECTIONS {
        assert!(known.contains(s), "editable entry {s} is not a known agent.toml section");
    }
    // …and the allow-list behaves: every known non-editable section is
    // frozen, every editable one accepts an ordinary key change.
    for s in &known {
        let before = format!("[{s}]\nzz_probe = 1\n");
        let after = format!("[{s}]\nzz_probe = 2\n");
        let d = as_agent(&before, &after);
        if AGENT_EDITABLE_SECTIONS.contains(s) {
            assert_eq!(d, GuardDecision::AllowedAgentWrite, "{s} should stay editable");
        } else {
            assert!(!d.is_allowed(), "{s} should be frozen: {d:?}");
        }
    }
}

/// P2-C §8.2: an employee cannot switch its own computer-use workspace on
/// (the `[capabilities.computer_use_config]` sub-table is part of the frozen
/// `[capabilities]` table).
#[test]
fn an_employee_cannot_turn_its_own_workspace_switch_on() {
    let before = with("[capabilities]\ncomputer_use = true\n[capabilities.computer_use_config]\nworkspace = false");
    let after = before.replace("workspace = false", "workspace = true");
    assert!(matches!(
        as_agent(&before, &after),
        GuardDecision::BlockedProtectedField { .. }
    ));
    let added = with("[capabilities]\ncomputer_use = true\n[capabilities.computer_use_config]\nworkspace = true");
    let without = with("[capabilities]\ncomputer_use = true");
    assert!(matches!(
        as_agent(&without, &added),
        GuardDecision::BlockedProtectedField { .. }
    ));
}
