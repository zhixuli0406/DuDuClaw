use super::*;
use crate::dept_grants::action_allowed;
use std::collections::BTreeSet;

const PREFIXES: &[&str] = &[
    "cron.", "reminders.", "assign_schedules.", "heartbeat.", "autopilot.",
    "ticks.", "skills.", "cost.", "budget.", "billing.", "analytics.", "install_requests.",
];

// Include dispatch, forwarding handlers and method-name gates, not config-key
// parsers or push-event classifiers. Paths are relative to this source file.
const SOURCES: &[(&str, &str)] = &[
    ("mod.rs", include_str!("../handlers/mod.rs")),
    ("dispatch_knowledge.rs", include_str!("../handlers/dispatch_knowledge.rs")),
    ("dispatch_ops.rs", include_str!("../handlers/dispatch_ops.rs")),
    ("dispatch_org.rs", include_str!("../handlers/dispatch_org.rs")),
    ("dispatch_system.rs", include_str!("../handlers/dispatch_system.rs")),
    ("dispatch_core.rs", include_str!("../handlers/dispatch_core.rs")),
    ("dispatch_kb.rs", include_str!("../handlers/dispatch_kb.rs")),
    ("dispatch_kb_agent.rs", include_str!("../handlers/dispatch_kb_agent.rs")),
    ("cron_scoped_rpc.rs", include_str!("../handlers/cron_scoped_rpc.rs")),
    ("cron_rpc.rs", include_str!("../handlers/cron_rpc.rs")),
    ("autopilot_rpc.rs", include_str!("../handlers/autopilot_rpc.rs")),
    ("skills_manage_rpc.rs", include_str!("../handlers/skills_manage_rpc.rs")),
    ("skills_rpc.rs", include_str!("../handlers/skills_rpc.rs")),
    ("skills_list_rpc.rs", include_str!("../handlers/skills_list_rpc.rs")),
    ("skills_recent_rpc.rs", include_str!("../handlers/skills_recent_rpc.rs")),
    ("skills_install_target.rs", include_str!("../handlers/skills_install_target.rs")),
    ("skills_install_request.rs", include_str!("../handlers/skills_install_request.rs")),
    ("skills_share_rpc.rs", include_str!("../handlers/skills_share_rpc.rs")),
    ("skills_custom.rs", include_str!("../handlers/skills_custom.rs")),
    ("cost_rpc.rs", include_str!("../handlers/cost_rpc.rs")),
    ("member_contract.rs", include_str!("../handlers/member_contract.rs")),
    ("dashboard_scope.rs", include_str!("../handlers/dashboard_scope.rs")),
    ("personal_data_gate.rs", include_str!("../handlers/personal_data_gate.rs")),
    ("personal_delegation_gate.rs", include_str!("../handlers/personal_delegation_gate.rs")),
    ("personal_p1g_gate.rs", include_str!("../handlers/personal_p1g_gate.rs")),
    ("tools_catalog.rs", include_str!("../handlers/tools_catalog.rs")),
];

// Small lexer: comments cannot unbalance quoted literals; escaped quotes stay
// inside their literal. Method names use ordinary ASCII Rust string literals.
fn string_literals(source: &str) -> Vec<String> {
    let mut chars = source.chars().peekable();
    let mut literals = Vec::new();
    while let Some(ch) = chars.next() {
        if ch == '/' && chars.peek() == Some(&'/') {
            chars.next();
            for ch in chars.by_ref() {
                if ch == '\n' { break; }
            }
        } else if ch == '/' && chars.peek() == Some(&'*') {
            chars.next();
            let mut depth = 1;
            while let Some(ch) = chars.next() {
                if ch == '/' && chars.peek() == Some(&'*') {
                    chars.next();
                    depth += 1;
                } else if ch == '*' && chars.peek() == Some(&'/') {
                    chars.next();
                    depth -= 1;
                    if depth == 0 { break; }
                }
            }
        } else if ch == '"' {
            let mut literal = String::new();
            while let Some(ch) = chars.next() {
                if ch == '"' { break; }
                if ch == '\\' {
                    // Escapes cannot occur in a method identifier. Preserve
                    // the marker so they fail the identifier filter below.
                    literal.push(ch);
                    if let Some(escaped) = chars.next() { literal.push(escaped); }
                } else {
                    literal.push(ch);
                }
            }
            literals.push(literal);
        }
    }
    literals
}

fn is_method_name(name: &str) -> bool {
    // Exclude bare prefixes, wildcard patterns, paths and prose explicitly.
    PREFIXES.iter().any(|prefix| name.strip_prefix(*prefix).is_some_and(|tail| {
        !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    })) && name != "budget.daily_cap_cents" // Config field in member_contract.rs.
}

#[test]
fn routed_methods_have_explicit_coverage() {
    let mut found = BTreeSet::new();
    for (file, source) in SOURCES {
        // Embedded unit-test probes such as cron.future are not RPC routes.
        let production = source.split("#[cfg(test)]").next().unwrap();
        for literal in string_literals(production) {
            if is_method_name(&literal) { found.insert(literal); }
        }
        assert!(!production.is_empty(), "empty routing source: {file}");
    }
    // Keep the cron contract explicit even if a forwarding router switches
    // to strip_prefix("cron.") and matches short names such as "add" =>.
    // Today dispatch_knowledge and cron_scoped_rpc both spell them in full.
    for method in [
        "cron.list", "cron.add", "cron.update", "cron.pause", "cron.resume",
        "cron.remove", "cron.run_now", "cron.templates",
    ] {
        assert!(coverage_for(method).is_some(), "prefix-stripped cron route missing: {method}");
        found.insert(method.to_owned());
    }
    let missing: Vec<_> = found.iter().filter(|name| coverage_for(name).is_none()).collect();
    assert!(missing.is_empty(), "RPC methods missing METHOD_COVERAGE: {missing:?}");

    let mut unique = BTreeSet::new();
    for &(method, group, coverage) in METHOD_COVERAGE {
        assert!(unique.insert(method), "duplicate coverage row: {method}");
        if let Coverage::Scoped(action) = coverage {
            assert!(action_allowed(group, action), "invalid scoped action: {method}: {group:?}/{action:?}");
        }
        if let Coverage::NotDelegated(reason) = coverage {
            assert!(!reason.trim().is_empty(), "missing non-delegation reason: {method}");
        }
    }
}
