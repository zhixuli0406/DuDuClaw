//! Regression suite for the org / identity field guard.
//!
//! Split out of `org_field_guard.rs` on 2026-09-29 (audit O9). Every test
//! body is unchanged — they are the golden input/output pairs that pin the
//! data-driven rewrite of the comparators to byte-identical behavior. Shared
//! fixtures live here; the cases themselves are grouped into two submodules
//! so neither file exceeds the project's 800-line ceiling.

use super::*;
use std::path::PathBuf;

fn home() -> PathBuf {
    PathBuf::from("/Users/alice/.duduclaw")
}

/// An absolute path on the platform running the test: `/Users/alice/<rest>`
/// on Unix, `C:\Users\alice\<rest>` on Windows. For the cases that feed a
/// path into an `is_absolute()` check (the hook's Bash cwd), where the Unix
/// spelling `/Users/…` has no drive and is relative on Windows. `rest` uses
/// `/`; it is split into components so no separator is mixed in.
fn abs_user_path(rest: &str) -> PathBuf {
    let root = if cfg!(windows) { r"C:\Users\alice" } else { "/Users/alice" };
    rest.split('/').fold(PathBuf::from(root), |p, c| p.join(c))
}

fn agent_toml() -> PathBuf {
    home().join("agents/agnes/agent.toml")
}

const BASE: &str = r#"
[agent]
name = "agnes"
display_name = "Agnes"
role = "assistant"
status = "active"
trigger = "@agnes"
reports_to = "ceo"
icon = "🐾"
department = "engineering"

[model]
preferred = "sonnet"
"#;

fn mcp_json(id: &str, token: Option<&str>) -> String {
    let env = match token {
        Some(t) => format!(
            "{{\"DUDUCLAW_AGENT_ID\":\"{id}\",\"DUDUCLAW_AGENT_TOKEN\":\"{t}\"}}"
        ),
        None => format!("{{\"DUDUCLAW_AGENT_ID\":\"{id}\"}}"),
    };
    format!("{{\"mcpServers\":{{\"duduclaw\":{{\"command\":\"/usr/local/bin/duduclaw\",\"args\":[\"mcp-server\"],\"env\":{env}}}}}}}")
}

fn agent(id: &str) -> HookCaller {
    HookCaller::Agent(id.to_string())
}

/// A minimal `<home>/config.toml` carrying both protected sections; shared
/// by the section-diff cases and the caller-scope cases.
const CONFIG: &str = r#"
[general]
log_level = "info"

[delegation]
policy = "department"
allow = [["a", "b"]]
"#;

/// `agent.toml` / `config.toml` content rules: classification, the frozen
/// `[agent]` org fields, the frozen `[capabilities]` table, the frozen
/// `config.toml` sections, and the Bash speed bump over all of them.
mod toml_rules;
/// Identity / enforcement surfaces and caller-scoped directory isolation:
/// `.mcp.json`, hook settings, `identity.key`, `org.toml`, WP22 T2 and the
/// WP1.1 C3 SOUL.md self-write guard.
mod identity_scope;
/// Removed-name reservation: the `_trash` area is not AI-writable.
mod removed_area;
/// G1: `<home>` state, evidence and yardsticks are not agent-writable
/// (both lanes), and the untrusted caller is refused alike on both lanes.
mod home_state;
/// Own `agent.toml` sections that security gates read (agent callers only).
mod agent_security;
/// G1 round 2: the Bash `<home>` rule judges command positions.
mod bash_rules;
/// G1 round 2: the Write/Edit lane resolves symbolic links (Unix only).
mod symlinks;
/// G1 round 3: regressions of the positional Bash reading, allow-list freeze.
mod bash_round3;
/// G1 round 4: fd-duplication boundary, strict unknown commands, short read-only list.
mod bash_round4;
/// G1 round 5: braced home variables, `--exec=` actions, dangling-link message path.
mod bash_round5;
/// P2-B C-1: operator-only memory commands are refused for employees.
mod operator_memory;
/// `.mcp.json` and the CLI configuration directories are frozen for
/// employees.
mod mcp_json;

// ── O9: the frozen-field table itself ───────────────────────────
//
// The cases in the two submodules above prove the *behavior* is unchanged.
// These prove the *table* still says what the module doc and the exported
// constants say — the failure mode a data-driven rewrite introduces is a
// silently shrinking table, which no behavioral case would notice because
// the dropped rule simply stops producing a verdict.

/// Every exported constant is actually wired into [`rules::FROZEN_FIELDS`].
///
/// Regression for the O9 data-ization: dropping a row here would make the
/// corresponding field silently writable while `AGENT_ORG_FIELDS` /
/// `CONFIG_PROTECTED_SECTIONS` still advertise it as frozen.
#[test]
fn frozen_table_covers_every_exported_constant() {
    use rules::{FrozenShape, FROZEN_FIELDS};

    let string_keys: Vec<&[&str]> = FROZEN_FIELDS
        .iter()
        .filter_map(|f| match f.shape {
            FrozenShape::StringKeys { keys, .. } => Some(keys),
            _ => None,
        })
        .collect();
    assert_eq!(
        string_keys,
        vec![AGENT_ORG_FIELDS],
        "the `[agent]` org-field row must carry exactly AGENT_ORG_FIELDS"
    );

    let table_keys: Vec<&str> = FROZEN_FIELDS
        .iter()
        .filter_map(|f| match f.shape {
            FrozenShape::TableKeys { section } => Some(section),
            _ => None,
        })
        .collect();
    assert_eq!(table_keys, vec![AGENT_CAPABILITY_SECTION]);

    // G1 round 3: one allow-list row carries the editable sections.
    let all_except: Vec<&[&str]> = FROZEN_FIELDS
        .iter()
        .filter_map(|f| match f.shape {
            FrozenShape::AllSectionsExcept { editable } => Some(editable),
            _ => None,
        })
        .collect();
    assert_eq!(all_except, vec![AGENT_EDITABLE_SECTIONS]);

    let value_keys: Vec<(&str, &[&str])> = FROZEN_FIELDS
        .iter()
        .filter_map(|f| match f.shape {
            FrozenShape::ValueKeys { section, keys } => Some((section, keys)),
            _ => None,
        })
        .collect();
    assert_eq!(value_keys, AGENT_SECURITY_KEYS.to_vec());

    let whole: Vec<&str> = FROZEN_FIELDS
        .iter()
        .filter_map(|f| match f.shape {
            FrozenShape::WholeSection { section } => Some(section),
            _ => None,
        })
        .collect();
    assert_eq!(whole, CONFIG_PROTECTED_SECTIONS.to_vec());
}

/// Both delegation-authority files still have at least one frozen entry, and
/// the org-field row is evaluated before the `[capabilities]` row.
///
/// The ordering is the contract `org_fields_win_when_both_moved` depends on;
/// asserting it on the table catches a reorder even if that behavioral case
/// were ever weakened.
#[test]
fn frozen_table_order_and_coverage_per_file_kind() {
    use rules::{frozen_for, FrozenVerdict};

    // G1 rows are agent-caller-only and come after the WP21 rows.
    let with_agent_rows: Vec<FrozenVerdict> =
        rules::frozen_for_caller(ProtectedTomlKind::AgentToml, true)
            .map(|f| f.verdict)
            .collect();
    assert_eq!(&with_agent_rows[..2], &[FrozenVerdict::OrgField, FrozenVerdict::ProtectedField]);
    assert!(with_agent_rows[2..]
        .iter()
        .all(|v| *v == FrozenVerdict::AgentSecuritySection));
    assert_eq!(
        with_agent_rows.len(),
        2 + 1 + AGENT_SECURITY_KEYS.len()
    );

    let agent: Vec<FrozenVerdict> = frozen_for(ProtectedTomlKind::AgentToml)
        .map(|f| f.verdict)
        .collect();
    assert_eq!(
        agent,
        vec![FrozenVerdict::OrgField, FrozenVerdict::ProtectedField],
        "org fields must be reported ahead of [capabilities]"
    );

    let config: Vec<FrozenVerdict> = frozen_for(ProtectedTomlKind::HomeConfigToml)
        .map(|f| f.verdict)
        .collect();
    assert_eq!(
        config,
        vec![
            FrozenVerdict::ProtectedSection,
            FrozenVerdict::ProtectedSection
        ],
        "both config.toml sections share one verdict so they report together"
    );
}
