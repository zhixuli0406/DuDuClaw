//! Standalone profile (2026-10-07): `tools/list` for a caller that holds no
//! `admin` scope and is not an AI employee lists exactly the tools its scopes
//! reach, minus the tools that act for the process's own agent. Employee and
//! internal-key listings are unchanged.

use super::*;
use crate::mcp_auth::{Principal, Scope, parse_scopes, tool_requires_scope};
use std::collections::BTreeSet;

fn principal(client_id: &str, scopes: &str, is_external: bool) -> Principal {
    Principal {
        client_id: client_id.into(),
        scopes: parse_scopes(scopes).unwrap(),
        is_external,
        created_at: chrono::Utc::now(),
    }
}

const STANDALONE: &str = crate::mcp_init_cmd::STANDALONE_SCOPES;

async fn names(
    p: &Principal,
    home: &std::path::Path,
    agent: &str,
    employee: bool,
) -> BTreeSet<&'static str> {
    visible_tools_with(p, home, agent, employee)
        .await
        .into_iter()
        .map(|t| t.name)
        .collect()
}

/// A freshly-scaffolded employee, same fixture as the O7 budget test.
fn scaffold_home() -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join("agents").join("scaffold");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("agent.toml"),
        "[agent]\nid = \"scaffold\"\nname = \"Scaffold\"\n",
    )
    .unwrap();
    home
}

#[test]
fn scope_listing_applies_only_to_scoped_non_employee_callers() {
    let internal = duduclaw_gateway::mcp_internal_key::INTERNAL_CLIENT_ID;
    // Scoped, not an employee: applies (internal or external key).
    assert!(scope_listing_applies(
        &principal("standalone-x", STANDALONE, false),
        false,
        false
    ));
    assert!(scope_listing_applies(
        &principal("standalone-x", STANDALONE, true),
        false,
        false
    ));
    assert!(scope_listing_applies(
        &principal("oauth_c", "", true),
        false,
        false
    ));
    // Admin holders see the full listing.
    assert!(!scope_listing_applies(
        &principal("standalone-x", "admin", false),
        false,
        false
    ));
    assert!(!scope_listing_applies(
        &principal("ext", "admin,memory:read", true),
        false,
        false
    ));
    // The gateway-internal key, a per-agent key, an employee process.
    assert!(!scope_listing_applies(
        &principal(internal, "memory:read", false),
        false,
        false
    ));
    assert!(!scope_listing_applies(
        &principal("scaffold", "", false),
        true,
        false
    ));
    assert!(!scope_listing_applies(
        &principal("standalone-x", STANDALONE, false),
        false,
        true
    ));
    // An external key is never an employee, whatever the environment says.
    assert!(scope_listing_applies(
        &principal("ext", STANDALONE, true),
        false,
        true
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn scoped_non_employee_sees_exactly_its_callable_tools() {
    let home = tempfile::tempdir().unwrap();
    let p = principal("standalone-claude-code", STANDALONE, false);
    let got = names(&p, home.path(), "dudu", false).await;

    let want: BTreeSet<&'static str> = tools()
        .map(|t| t.name)
        .filter(|n| tool_requires_scope(n).is_some_and(|s| p.scopes.contains(&s)))
        .filter(|n| !PROCESS_AGENT_TOOLS.contains(n))
        .collect();
    assert_eq!(got, want);
    for n in &got {
        let s = tool_requires_scope(n).unwrap();
        assert!(
            matches!(
                s,
                Scope::MemoryRead | Scope::MemoryWrite | Scope::WikiRead | Scope::WikiWrite
            ),
            "{n} needs {s:?}"
        );
    }
    for hidden in PROCESS_AGENT_TOOLS {
        assert!(!got.contains(hidden), "{hidden} must be hidden");
    }
    for hidden in [
        "tasks_list",
        "tasks_create",
        "web_fetch_cached",
        "send_message",
    ] {
        assert!(
            !got.contains(hidden),
            "{hidden} needs a scope the caller lacks"
        );
    }
    assert!(got.contains("memory_store") && got.contains("wiki_read"));

    // A single scope narrows the listing to that scope's tools.
    let read_only = principal("standalone-x", "memory:read", false);
    let got = names(&read_only, home.path(), "dudu", false).await;
    assert!(got.contains("memory_search"));
    assert!(!got.contains("memory_store") && !got.contains("wiki_read"));
}

#[tokio::test(flavor = "current_thread")]
async fn init_token_listing_matches_the_internal_scoped_listing() {
    // `duduclaw mcp init` issues an external key; its listing is the same
    // set as an internal key with the same scopes.
    let home = tempfile::tempdir().unwrap();
    let ext = names(
        &principal("standalone-claude-code", STANDALONE, true),
        home.path(),
        "dudu",
        false,
    )
    .await;
    let int = names(
        &principal("standalone-claude-code", STANDALONE, false),
        home.path(),
        "dudu",
        false,
    )
    .await;
    assert_eq!(ext, int);
    assert!(ext.len() >= 20, "{ext:?}");
}

#[tokio::test(flavor = "current_thread")]
async fn employee_and_internal_listings_are_unchanged() {
    let home = scaffold_home();
    // Per-agent key whose client id is the employee (the O7 budget fixture):
    // no scopes, but an employee, so the scope listing does not apply.
    let per_agent = names(
        &principal("scaffold", "", false),
        home.path(),
        "scaffold",
        false,
    )
    .await;
    // Pinned at 1.70.1 (CLAUDE.md: "a scaffold employee's tools/list is 167
    // tools"). Moves only with a deliberate CHANGELOG entry.
    assert_eq!(per_agent.len(), 167, "{per_agent:?}");

    // The gateway-internal key acting for the same employee.
    let internal = principal(
        duduclaw_gateway::mcp_internal_key::INTERNAL_CLIENT_ID,
        "admin",
        false,
    );
    assert_eq!(
        names(&internal, home.path(), "scaffold", false).await,
        per_agent
    );

    // An employee process holding some other scoped key keeps the full list
    // its capabilities allow.
    let other = principal("standalone-x", STANDALONE, false);
    let employee_view = names(&other, home.path(), "scaffold", true).await;
    let operator_view = names(
        &principal("standalone-x", "admin", false),
        home.path(),
        "scaffold",
        false,
    )
    .await;
    assert_eq!(employee_view, operator_view);
}

// ── Dispatch gate: discoverable ⇔ callable (review 2026-10-07) ─────────────

mod dispatch_gate {
    use super::principal;
    use crate::mcp_auth::Principal;
    use serde_json::{Value, json};
    use std::sync::Arc;

    const STANDALONE: &str = crate::mcp_init_cmd::STANDALONE_SCOPES;

    /// A real dispatcher whose process agent is `dudu`, an employee of the
    /// `sales` department with a working-state entry, on a temp home.
    fn setup() -> (tempfile::TempDir, crate::mcp_dispatch::McpDispatcher) {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_path_buf();
        let dudu = home.join("agents").join("dudu");
        std::fs::create_dir_all(dudu.join("state")).unwrap();
        std::fs::write(
            dudu.join("agent.toml"),
            "[agent]\nname = \"dudu\"\ndepartment = \"sales\"\n",
        )
        .unwrap();
        let scaffold = home.join("agents").join("scaffold");
        std::fs::create_dir_all(&scaffold).unwrap();
        std::fs::write(
            scaffold.join("agent.toml"),
            "[agent]\nname = \"scaffold\"\n",
        )
        .unwrap();
        let shared = home.join("shared").join("wiki");
        std::fs::create_dir_all(shared.join("departments").join("sales")).unwrap();
        std::fs::write(
            shared.join("public.md"),
            "---\ntitle: Public\n---\nopen page\n",
        )
        .unwrap();
        std::fs::write(
            shared.join("departments").join("sales").join("plan.md"),
            "---\ntitle: Sales plan\n---\nsales only\n",
        )
        .unwrap();
        let memory =
            Arc::new(duduclaw_memory::SqliteMemoryEngine::new(&home.join("memory.db")).unwrap());
        let odoo: crate::mcp_dispatch::OdooState =
            Arc::new(crate::odoo_pool::OdooConnectorPool::default());
        let d = crate::mcp_dispatch::McpDispatcher::new(
            home,
            reqwest::Client::new(),
            memory,
            "dudu".to_string(),
            odoo,
            crate::mcp_rate_limit::RateLimiter::new(),
            crate::mcp_memory_quota::DailyQuota::new(),
        );
        (tmp, d)
    }

    async fn call(
        d: &crate::mcp_dispatch::McpDispatcher,
        p: &Principal,
        tool: &str,
        args: Value,
    ) -> Value {
        let ns = crate::mcp_namespace::resolve(p).unwrap();
        d.dispatch_tool_call(p, &ns, &json!({"name": tool, "arguments": args}), &json!(1))
            .await
    }

    fn audit_classes(home: &std::path::Path, tool: &str) -> Vec<String> {
        std::fs::read_to_string(home.join("tool_calls.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|r| r["tool"] == tool || r["tool_name"] == tool)
            .filter_map(|r| r["error_class"].as_str().map(str::to_string))
            .collect()
    }

    fn text(resp: &Value) -> String {
        resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }

    /// Every process-agent tool an external key's scopes would otherwise let
    /// through is refused with -32003 and an audit row, and the employee's
    /// state is untouched.
    #[tokio::test(flavor = "current_thread")]
    async fn external_key_cannot_call_process_agent_tools() {
        let (tmp, d) = setup();
        let ext = principal("standalone-claude-code", STANDALONE, true);
        for (tool, args) in [
            ("working_state_get", json!({})),
            (
                "working_state_set",
                json!({"key": "k", "value": "v", "reason": "r"}),
            ),
            (
                "memory_search_by_layer",
                json!({"query": "x", "layer": "semantic"}),
            ),
            ("memory_successful_conversations", json!({})),
            ("memory_episodic_pressure", json!({})),
            ("memory_consolidation_status", json!({})),
            ("wiki_namespace_status", json!({})),
            ("canvas_push", json!({"html": "<p>x</p>"})),
            ("shared_wiki_delete", json!({"page_path": "public.md"})),
        ] {
            let resp = call(&d, &ext, tool, args).await;
            assert_eq!(resp["error"]["code"], -32003, "{tool}: {resp}");
            let msg = resp["error"]["message"].as_str().unwrap_or_default();
            assert!(msg.contains("acts for the AI employee"), "{tool}: {msg}");
            assert_eq!(
                audit_classes(tmp.path(), tool),
                vec!["process_agent_tool"],
                "{tool}"
            );
        }
        assert!(tmp.path().join("shared/wiki/public.md").is_file());
        assert!(
            !tmp.path()
                .join("agents/dudu/state/working_state.json")
                .exists()
        );

        // An external key holding `admin` is still not an employee.
        let ext_admin = principal("ext-admin", "admin,memory:read", true);
        let resp = call(&d, &ext_admin, "working_state_get", json!({})).await;
        assert_eq!(resp["error"]["code"], -32003, "{resp}");
        assert!(
            resp["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("acts for the AI employee"),
            "{resp}"
        );
    }

    /// Employee callers are unaffected: the gateway-internal key acting for
    /// `dudu`, and a per-agent key whose client id is an employee.
    #[tokio::test(flavor = "current_thread")]
    async fn employee_callers_still_reach_process_agent_tools() {
        let (tmp, d) = setup();
        let internal = principal(
            duduclaw_gateway::mcp_internal_key::INTERNAL_CLIENT_ID,
            "admin",
            false,
        );
        let per_agent = principal("scaffold", "memory:read", false);
        for p in [&internal, &per_agent] {
            let resp = call(&d, p, "working_state_get", json!({})).await;
            let msg = resp["error"]["message"].as_str().unwrap_or_default();
            assert!(
                !msg.contains("acts for the AI employee"),
                "{}: {resp}",
                p.client_id
            );
        }
        assert!(
            !audit_classes(tmp.path(), "working_state_get")
                .iter()
                .any(|c| c == "process_agent_tool")
        );
    }

    /// The predicate itself for internal keys, with the employee-process fact
    /// passed in (the dispatch gate reads it from the environment).
    #[test]
    fn internal_key_refusal_follows_scope_listing() {
        use super::super::process_agent_tool_refused as refused;
        let scoped = principal("standalone-x", STANDALONE, false);
        assert!(refused("working_state_set", &scoped, false, false));
        assert!(
            !refused("working_state_set", &scoped, false, true),
            "employee process"
        );
        assert!(
            !refused("working_state_set", &scoped, true, false),
            "per-agent key"
        );
        assert!(
            !refused("memory_store", &scoped, false, false),
            "not a process-agent tool"
        );
        let admin = principal("standalone-x", "admin", false);
        assert!(!refused("working_state_set", &admin, false, false));
        let internal = principal(
            duduclaw_gateway::mcp_internal_key::INTERNAL_CLIENT_ID,
            "memory:read",
            false,
        );
        assert!(!refused("working_state_set", &internal, false, false));
    }

    /// An external key cannot write the shared wiki as the process agent,
    /// and reads it with no department.
    #[tokio::test(flavor = "current_thread")]
    async fn external_key_shared_wiki_is_read_only_and_department_free() {
        let (tmp, d) = setup();
        let ext = principal("standalone-claude-code", STANDALONE, true);
        let page = "---\ntitle: X\ncreated: 2026-10-07\nupdated: 2026-10-07\ntags: [a]\n\
                    layer: context\ntrust: 0.5\n---\nbody\n";

        let resp = call(
            &d,
            &ext,
            "wiki_write",
            json!({"scope": "shared", "page_path": "x.md", "content": page}),
        )
        .await;
        assert_eq!(resp["error"]["code"], -32003, "{resp}");
        assert_eq!(
            audit_classes(tmp.path(), "wiki_write"),
            vec!["external_shared_wiki_write"]
        );
        assert!(!tmp.path().join("shared/wiki/x.md").exists());

        // Its own wiki still works.
        let resp = call(
            &d,
            &ext,
            "wiki_write",
            json!({"page_path": "notes/a.md", "content": "# A\n\nok"}),
        )
        .await;
        assert!(
            resp.get("error").is_none() && resp["result"]["isError"] != json!(true),
            "{resp}"
        );

        // Reads: the public page, never the process agent's department page.
        let ls = text(&call(&d, &ext, "wiki_ls", json!({"scope": "shared"})).await);
        assert!(ls.contains("public.md"), "{ls}");
        assert!(!ls.contains("departments/sales"), "{ls}");
        let read = call(
            &d,
            &ext,
            "wiki_read",
            json!({"scope": "shared", "page_path": "departments/sales/plan.md"}),
        )
        .await;
        assert!(!text(&read).contains("sales only"), "{read}");
        let stats = text(&call(&d, &ext, "wiki_stats", json!({"scope": "shared"})).await);
        assert!(!stats.contains("departments/sales"), "{stats}");
        let search = text(
            &call(
                &d,
                &ext,
                "wiki_search",
                json!({"scope": "shared", "query": "sales"}),
            )
            .await,
        );
        assert!(!search.contains("departments/sales"), "{search}");

        // The employee itself (internal key acting for dudu) does see it.
        let internal = principal(
            duduclaw_gateway::mcp_internal_key::INTERNAL_CLIENT_ID,
            "admin",
            false,
        );
        let ls = text(&call(&d, &internal, "wiki_ls", json!({"scope": "shared"})).await);
        assert!(ls.contains("departments/sales/plan.md"), "{ls}");
    }
}
