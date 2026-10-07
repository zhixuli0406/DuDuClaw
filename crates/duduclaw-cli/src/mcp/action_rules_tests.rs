//! 2026-10: tool effect classes, `[capabilities] action_rules` and the
//! read-only explore lane — the parts that live on the MCP server side.
//! Dispatch-gate refusals are tested next to the other gates in
//! `mcp_dispatch.rs`.

use super::*;
use duduclaw_core::{ProcessLane, ToolEffect};
use std::collections::BTreeSet;

/// Every advertised `ToolDef` must have an explicit entry in the effect
/// table: an unclassified tool would silently fall to `admin` (the fail-closed
/// default), which hides it from the explore lane and puts it under any
/// `admin` rule without anyone having decided that.
#[test]
fn every_advertised_tool_has_an_explicit_effect_class() {
    let missing: Vec<&str> = tools()
        .map(|t| t.name)
        .filter(|name| duduclaw_core::effect_of_builtin(name).is_none())
        .collect();
    assert!(
        missing.is_empty(),
        "tools without an effect class — add them to \
         duduclaw-core/src/tool_effect.rs::effect_of_builtin: {missing:?}"
    );
}

/// The effect table must not carry names that are no longer tools (a stale
/// row would hide a typo in a real entry). Read the classified set back from
/// the catalog, which lists exactly the advertised MCP tools.
#[test]
fn catalog_effect_matches_the_table() {
    for entry in duduclaw_core::builtin_tool_catalog() {
        if entry.kind == "mcp" {
            assert_eq!(
                Some(entry.effect),
                duduclaw_core::effect_of_builtin(entry.name),
                "{}",
                entry.name
            );
        }
    }
}

fn home_with_rules(rules: &str) -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join("agents").join("test");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("agent.toml"),
        format!("[agent]\nid = \"test\"\nname = \"Test\"\n\n[capabilities]\naction_rules = {rules}\n"),
    )
    .unwrap();
    home
}

async fn listed(home: &std::path::Path, lane: &ProcessLane) -> BTreeSet<&'static str> {
    visible_tools_in_lane(&test_principal(false), home, "", false, lane)
        .await
        .into_iter()
        .map(|t| t.name)
        .collect()
}

#[tokio::test(flavor = "current_thread")]
async fn blocked_tools_are_hidden_and_asked_tools_stay_listed() {
    let plain = home_with_rules("[]");
    let before = listed(plain.path(), &ProcessLane::Normal).await;
    assert!(before.contains("send_message") && before.contains("wiki_write"));

    let home = home_with_rules(
        "[{ effect = \"send\", verdict = \"block\" }, { tool = \"send_message\", verdict = \"ask\" }, \
         { effect = \"modify\", verdict = \"ask\" }]",
    );
    let after = listed(home.path(), &ProcessLane::Normal).await;
    // Effect block hides every send tool …
    assert!(!after.contains("mail_send"));
    assert!(!after.contains("send_to_agent"));
    // … except the one a tool rule lifts to `ask`.
    assert!(after.contains("send_message"));
    // `ask` is callable after an approval, so it stays discoverable.
    assert!(after.contains("wiki_write"));
    assert!(after.contains("memory_search"));
    let hidden: BTreeSet<_> = before.difference(&after).copied().collect();
    assert!(hidden.iter().all(|n| duduclaw_core::effect_of(n) == ToolEffect::Send), "{hidden:?}");
}

#[tokio::test(flavor = "current_thread")]
async fn a_malformed_rule_list_hides_nothing_but_asks_for_side_effects() {
    let home = home_with_rules("\"block everything\"");
    let names = listed(home.path(), &ProcessLane::Normal).await;
    assert!(names.contains("send_message"));
    let dir = home.path().join("agents").join("test");
    let (always, _) = static_gate_flags(&dir, "send_message", &serde_json::json!({ "arguments": {} }));
    assert!(always, "malformed rules must ask before a side effect");
    let (always, _) = static_gate_flags(&dir, "memory_search", &serde_json::json!({ "arguments": {} }));
    assert!(!always, "reads stay free");
}

#[test]
fn ask_rules_fold_into_the_static_always_flag_and_allow_never_removes_a_list() {
    let home = home_with_rules(
        "[{ effect = \"send\", verdict = \"ask\" }, { tool = \"wiki_write\", verdict = \"allow\" }]",
    );
    let dir = home.path().join("agents").join("test");
    let payload = serde_json::json!({ "arguments": {} });
    assert!(static_gate_flags(&dir, "mail_send", &payload).0);
    assert!(!static_gate_flags(&dir, "memory_store", &payload).0);

    // `allow` for a tool that `approval_required_tools` names: still asks.
    std::fs::write(
        dir.join("agent.toml"),
        "[capabilities]\napproval_required_tools = [\"wiki_write\"]\n\
         action_rules = [{ tool = \"wiki_write\", verdict = \"allow\" }]\n",
    )
    .unwrap();
    assert!(static_gate_flags(&dir, "wiki_write", &payload).0);
}

#[tokio::test(flavor = "current_thread")]
async fn explore_lane_lists_only_read_and_draft_tools() {
    let home = tmp_home_with_all_capabilities();
    let normal = listed(home.path(), &ProcessLane::Normal).await;
    let explore = listed(home.path(), &ProcessLane::Explore).await;
    assert!(!explore.is_empty());
    assert!(explore.contains("memory_search"));
    assert!(explore.contains("gmail_create_draft") || !normal.contains("gmail_create_draft"));
    for name in &explore {
        assert!(!duduclaw_core::effect_of(name).is_side_effecting(), "{name}");
    }
    for name in normal.difference(&explore) {
        assert!(duduclaw_core::effect_of(name).is_side_effecting(), "{name}");
    }
    assert!(listed(home.path(), &ProcessLane::Invalid).await.is_empty());
}

#[test]
fn lane_refusal_names_the_tool_and_the_variable() {
    assert!(lane_refusal(&ProcessLane::Normal, "agent_remove").is_none());
    assert!(lane_refusal(&ProcessLane::Explore, "wiki_read").is_none());
    let msg = lane_refusal(&ProcessLane::Explore, "send_message").unwrap();
    assert!(msg.contains("send_message") && msg.contains("explore"), "{msg}");
    let msg = lane_refusal(&ProcessLane::Invalid, "wiki_read").unwrap();
    assert!(msg.contains(duduclaw_core::ENV_LANE), "{msg}");
}
