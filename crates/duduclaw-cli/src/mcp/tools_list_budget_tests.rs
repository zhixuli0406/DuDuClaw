//! O7 — the `tools/list` fixed-cost budget, pinned.
//!
//! The MCP tool schemas this server advertises are a *fixed* prompt cost paid
//! on every single agent spawn: the CLI reads `tools/list` once per session and
//! the whole payload lands in the model's context before the first user token.
//! Two guards live here:
//!
//! * a **golden budget** on a scaffold agent (a freshly-created agent with no
//!   capability opt-ins) — the shape every new deployment starts from;
//! * a **per-tool description cap** (200 bytes) so a single verbose entry can
//!   not quietly re-inflate the payload. Long prose belongs in
//!   `docs/guides/mcp-tools.md`, not in the wire schema.
//!
//! The numbers are deliberately asserted as an *upper bound plus a floor*, not
//! an equality: a floor catches an accidental mass-deletion of tools, the cap
//! catches regression. Both move only with a deliberate CHANGELOG entry.

use super::*;
use serde_json::json;

/// The byte ceiling one `tools/list` response may cost a scaffold agent.
///
/// Measured 2026-09-29, same fixture either side of O7:
///
/// | | tools | bytes |
/// |---|---|---|
/// | before O7 | 215 | 120,092 |
/// | after O7  | 170 |  81,743 |
///
/// The 45 hidden tools are the ones a scaffold agent could never call
/// (`os_native` / `recording` / `system_operator` / `codrive` / `computer_use`
/// / `[fork] enabled`, all deny-by-default); the remaining ~7k is the
/// description budget below.
const SCAFFOLD_TOOLS_LIST_MAX_BYTES: usize = 85_000;
/// Floor — a mass deletion of tools must fail here, not silently ship.
const SCAFFOLD_TOOLS_LIST_MIN_BYTES: usize = 70_000;

/// Per-tool `description` byte cap (O7). Anything longer belongs in
/// `docs/guides/mcp-tools.md`.
pub(crate) const TOOL_DESCRIPTION_MAX_BYTES: usize = 200;

/// A freshly-scaffolded agent: identity only, zero capability opt-ins. This is
/// exactly what `duduclaw agent create` writes, and therefore the payload every
/// new deployment actually pays for.
fn scaffold_home() -> tempfile::TempDir {
    let home = tempfile::tempdir().expect("tempdir");
    let dir = home.path().join("agents").join("scaffold");
    std::fs::create_dir_all(&dir).expect("agent dir");
    std::fs::write(
        dir.join("agent.toml"),
        "[agent]\nid = \"scaffold\"\nname = \"Scaffold\"\n",
    )
    .expect("agent.toml");
    home
}

fn scaffold_principal() -> crate::mcp_auth::Principal {
    crate::mcp_auth::Principal {
        client_id: "scaffold".into(),
        scopes: std::collections::HashSet::new(),
        is_external: false,
        created_at: chrono::Utc::now(),
    }
}

async fn scaffold_tools_list() -> Value {
    let home = scaffold_home();
    handle_tools_list_for_agent(&json!(1), &scaffold_principal(), home.path(), "scaffold").await
}

#[tokio::test(flavor = "current_thread")]
async fn scaffold_agent_tools_list_stays_within_budget() {
    let resp = scaffold_tools_list().await;
    let tools = resp["result"]["tools"].as_array().expect("tools array");
    let bytes = serde_json::to_string(&resp["result"])
        .expect("serialize")
        .len();

    assert!(
        bytes <= SCAFFOLD_TOOLS_LIST_MAX_BYTES,
        "tools/list for a scaffold agent grew to {bytes} bytes ({} tools), over the \
         {SCAFFOLD_TOOLS_LIST_MAX_BYTES}-byte O7 budget. This is a FIXED per-spawn prompt \
         cost. Either trim descriptions (cap {TOOL_DESCRIPTION_MAX_BYTES} bytes, long prose \
         goes to docs/guides/mcp-tools.md) or gate the new tools behind a capability.",
        tools.len()
    );
    assert!(
        bytes >= SCAFFOLD_TOOLS_LIST_MIN_BYTES,
        "tools/list collapsed to {bytes} bytes ({} tools) — a capability filter probably \
         over-matched and is hiding tools the scaffold agent can actually call",
        tools.len()
    );
}

/// No single tool may carry more than [`TOOL_DESCRIPTION_MAX_BYTES`] of prose.
#[test]
fn every_tool_description_is_within_the_byte_cap() {
    let mut over: Vec<(&str, usize)> = tools()
        .map(|t| (t.name, t.description.len()))
        .filter(|(_, n)| *n > TOOL_DESCRIPTION_MAX_BYTES)
        .collect();
    over.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    assert!(
        over.is_empty(),
        "these tool descriptions exceed the {TOOL_DESCRIPTION_MAX_BYTES}-byte O7 cap \
         (move the detail to docs/guides/mcp-tools.md): {over:?}"
    );
}

/// Parameter descriptions ride the same wire, so they carry the same cap.
///
/// `build_tool_schema` types every parameter as a bare JSON-Schema string, so
/// a parameter's *description* is the only place its real shape can be stated
/// — which is why exactly one entry is exempt below rather than being trimmed
/// into uselessness.
const PARAM_DESCRIPTION_MAX_BYTES: usize = 200;

/// The documented exemptions to [`PARAM_DESCRIPTION_MAX_BYTES`]: parameters
/// whose description IS the tool's schema, where trimming to the cap would
/// make the tool uncallable rather than merely terser.
///
/// `team_handoff.packet` is the whole TaskPacket contract, and a packet is
/// rejected whole (never truncated) — an agent that cannot see the shape
/// cannot produce a valid one. It is still bounded, and it is still far below
/// the 1,525 bytes it carried before O7.
const PARAM_CAP_EXEMPTIONS: &[(&str, &str, usize)] = &[("team_handoff", "packet", 1024)];

#[test]
fn every_param_description_is_within_the_byte_cap() {
    let cap_for = |tool: &str, param: &str| -> usize {
        PARAM_CAP_EXEMPTIONS
            .iter()
            .find(|(t, p, _)| *t == tool && *p == param)
            .map(|(_, _, cap)| *cap)
            .unwrap_or(PARAM_DESCRIPTION_MAX_BYTES)
    };
    let mut over: Vec<(&str, &str, usize)> = tools()
        .flat_map(|t| {
            t.params
                .iter()
                .map(move |p| (t.name, p.name, p.description.len()))
        })
        .filter(|(t, p, n)| *n > cap_for(t, p))
        .collect();
    over.sort_by_key(|(_, _, n)| std::cmp::Reverse(*n));
    assert!(
        over.is_empty(),
        "these parameter descriptions exceed the {PARAM_DESCRIPTION_MAX_BYTES}-byte O7 cap \
         (move the detail to docs/guides/mcp-tools.md, or add a justified row to \
         PARAM_CAP_EXEMPTIONS): {over:?}"
    );
}

/// An exemption must stay justified: if the parameter it names disappears or
/// shrinks under the ordinary cap, the row is dead weight and must go.
#[test]
fn param_cap_exemptions_are_all_still_needed() {
    for (tool, param, cap) in PARAM_CAP_EXEMPTIONS {
        let def = tools()
            .find(|t| t.name == *tool)
            .unwrap_or_else(|| panic!("exempted tool `{tool}` no longer exists"));
        let p = def
            .params
            .iter()
            .find(|p| p.name == *param)
            .unwrap_or_else(|| panic!("exempted param `{tool}.{param}` no longer exists"));
        assert!(
            p.description.len() > PARAM_DESCRIPTION_MAX_BYTES,
            "`{tool}.{param}` now fits the ordinary cap — drop its PARAM_CAP_EXEMPTIONS row"
        );
        assert!(
            p.description.len() <= *cap,
            "`{tool}.{param}` is {} bytes, over its own {cap}-byte exemption",
            p.description.len()
        );
    }
}

// ── O7 · capability pruning: discoverable ⇔ callable ─────────────────────

/// Build a home whose caller agent carries exactly `capabilities_toml`.
fn home_with(agent: &str, body: &str) -> tempfile::TempDir {
    let home = tempfile::tempdir().expect("tempdir");
    let dir = home.path().join("agents").join(agent);
    std::fs::create_dir_all(&dir).expect("agent dir");
    std::fs::write(dir.join("agent.toml"), body).expect("agent.toml");
    home
}

fn principal_named(id: &str) -> crate::mcp_auth::Principal {
    crate::mcp_auth::Principal {
        client_id: id.into(),
        scopes: std::collections::HashSet::new(),
        is_external: false,
        created_at: chrono::Utc::now(),
    }
}

async fn names_for(home: &std::path::Path, agent: &str) -> Vec<&'static str> {
    visible_tool_names(&principal_named(agent), home, agent).await
}

/// Each deny-by-default master switch hides exactly its own family, and
/// turning it on reveals exactly that family — nothing else moves.
#[tokio::test(flavor = "current_thread")]
async fn each_master_switch_hides_exactly_its_own_tool_family() {
    let cases: [(&str, &[&str]); 6] = [
        ("os_native = true", crate::mcp_dispatch::OS_NATIVE_TOOLS),
        ("recording = true", crate::mcp_dispatch::RECORDING_TOOLS),
        (
            "system_operator = true",
            crate::mcp_dispatch::SYSTEM_OPERATOR_TOOLS,
        ),
        ("codrive = true", crate::mcp_dispatch::CODRIVE_TOOLS),
        (
            "computer_use = true",
            crate::mcp_dispatch::COMPUTER_USE_TOOLS,
        ),
        (
            "db_sources = [\"demo\"]",
            crate::mcp_dispatch::DB_SOURCE_TOOLS,
        ),
    ];

    let off = home_with("w", "[capabilities]\n");
    let off_names = names_for(off.path(), "w").await;

    for (line, family) in cases {
        for t in family {
            assert!(
                !off_names.contains(t),
                "{t} must be hidden while the capability is off"
            );
        }
        let on = home_with("w", &format!("[capabilities]\n{line}\n"));
        let on_names = names_for(on.path(), "w").await;
        for t in family {
            assert!(on_names.contains(t), "{t} must appear once `{line}`");
        }
        // Nothing but this family moved.
        let gained: Vec<_> = on_names
            .iter()
            .filter(|n| !off_names.contains(n))
            .copied()
            .collect();
        let mut expected = family.to_vec();
        expected.sort_unstable();
        let mut gained_sorted = gained.clone();
        gained_sorted.sort_unstable();
        assert_eq!(
            gained_sorted, expected,
            "`{line}` changed more than its own family"
        );
    }
}

/// P2-C: the workspace tools need `computer_use` AND the employee's own
/// `[capabilities.computer_use_config] workspace`; together they reveal
/// exactly those three. (Hiding is discovery only; the gateway re-checks.)
#[tokio::test(flavor = "current_thread")]
async fn workspace_tools_need_both_computer_use_and_the_workspace_switch() {
    let cu_only = home_with("w", "[capabilities]\ncomputer_use = true\n");
    let cu_only_names = names_for(cu_only.path(), "w").await;
    let ws_only = home_with("w", "[capabilities]\n[capabilities.computer_use_config]\nworkspace = true\n");
    let ws_only_names = names_for(ws_only.path(), "w").await;
    for t in crate::mcp_dispatch::COMPUTER_WORKSPACE_TOOLS {
        assert!(!cu_only_names.contains(t), "{t} hidden without the workspace switch");
        assert!(!ws_only_names.contains(t), "{t} hidden without computer_use");
    }
    let both = home_with(
        "w",
        "[capabilities]\ncomputer_use = true\n[capabilities.computer_use_config]\nworkspace = true\n",
    );
    let both_names = names_for(both.path(), "w").await;
    let mut gained: Vec<_> = both_names.iter().filter(|n| !cu_only_names.contains(n)).copied().collect();
    gained.sort_unstable();
    let mut expected = crate::mcp_dispatch::COMPUTER_WORKSPACE_TOOLS.to_vec();
    expected.sort_unstable();
    assert_eq!(gained, expected);
}

/// `[fork] enabled` lives outside `[capabilities]`; same rule applies.
#[tokio::test(flavor = "current_thread")]
async fn fork_tools_follow_the_per_agent_fork_toggle() {
    let off = home_with("w", "[capabilities]\n");
    let off_names = names_for(off.path(), "w").await;
    for t in crate::mcp_dispatch::FORK_TOOLS {
        assert!(!off_names.contains(t), "{t} hidden while [fork] is off");
    }
    let on = home_with("w", "[fork]\nenabled = true\n");
    let on_names = names_for(on.path(), "w").await;
    for t in crate::mcp_dispatch::FORK_TOOLS {
        assert!(on_names.contains(t), "{t} appears once [fork] enabled");
    }
}

/// PORTICO: a tool named in `scoped_tools` is hidden until a grant is active,
/// and appears the moment one is minted — which is exactly what makes the
/// `notifications/tools/list_changed` companion necessary.
#[tokio::test(flavor = "current_thread")]
async fn scoped_tools_appear_only_while_a_grant_is_active() {
    let home = home_with("w", "[capabilities]\nscoped_tools = [\"memory_search\"]\n");

    let before = names_for(home.path(), "w").await;
    assert!(
        !before.contains(&"memory_search"),
        "a scoped tool with no grant must not be advertised"
    );
    assert!(
        before.contains(&"memory_store"),
        "unscoped tools are unaffected"
    );

    let store =
        duduclaw_gateway::capability_grants::CapabilityGrantStore::open(home.path()).unwrap();
    store
        .grant("w", None, "memory_search", "test-operator", 3600)
        .await
        .expect("grant");

    let after = names_for(home.path(), "w").await;
    assert!(
        after.contains(&"memory_search"),
        "the scoped tool must appear once the grant is active"
    );
}

/// The hidden set is a discovery filter, never an authorization one: the
/// dispatch gate still refuses the call, with its own message.
#[tokio::test(flavor = "current_thread")]
async fn a_hidden_tool_is_still_refused_when_called_directly() {
    let home = home_with("w", "[capabilities]\n");
    assert!(!names_for(home.path(), "w").await.contains(&"os_notify"));

    let dispatcher = crate::mcp_dispatch::McpDispatcher::new(
        home.path().to_path_buf(),
        reqwest::Client::new(),
        std::sync::Arc::new(
            duduclaw_memory::SqliteMemoryEngine::new(&home.path().join("memory.db")).unwrap(),
        ),
        "w".to_string(),
        std::sync::Arc::new(crate::odoo_pool::OdooConnectorPool::default()),
        crate::mcp_rate_limit::RateLimiter::new(),
        crate::mcp_memory_quota::DailyQuota::new(),
    );
    let mut principal = principal_named("w");
    principal.scopes.insert(crate::mcp_auth::Scope::Admin);
    let ns = crate::mcp_namespace::resolve(&principal).unwrap();
    let resp = dispatcher
        .dispatch_tool_call(
            &principal,
            &ns,
            &json!({"name": "os_notify", "arguments": {"title": "t", "body": "b"}}),
            &json!(1),
        )
        .await;
    let text = resp.to_string();
    assert!(
        text.contains("os_native"),
        "hiding must not have replaced the gate's own refusal: {text}"
    );
}

/// The `initialize` handshake must declare `listChanged`, or a client has no
/// reason to re-read `tools/list` and hiding becomes permanent.
#[test]
fn initialize_declares_the_tools_list_changed_capability() {
    let resp = handle_initialize(&json!(1), &json!({}));
    assert_eq!(
        resp["result"]["capabilities"]["tools"]["listChanged"],
        serde_json::Value::Bool(true)
    );
}

/// The notification is a JSON-RPC notification: a method, and no `id`.
#[test]
fn list_changed_notification_is_well_formed() {
    let n = tools_list_changed_notification();
    assert_eq!(n["jsonrpc"], "2.0");
    assert_eq!(n["method"], "notifications/tools/list_changed");
    assert!(
        n.get("id").is_none(),
        "a notification must not carry an id: {n}"
    );
}
