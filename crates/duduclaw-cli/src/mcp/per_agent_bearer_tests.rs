//! v1.82 (RM-134 second and third review, High): on the HTTP MCP server only
//! the verified gateway-internal principal may call the tools that act for
//! the server's fixed `default_agent`; every other non-external principal is
//! refused them (default deny, decided from the authenticated principal's
//! kind), acts as its key's bound employee for the rest, and is refused
//! everything without a valid binding.
//!
//! Fixture: the HTTP server's process agent is `bob`; `alice`'s per-agent key
//! (internal, client id `alice`, employee scopes, never `admin`) calls it.

use super::*;
use crate::mcp_auth::{AuthenticatedCall, Principal, PrincipalKind, parse_scopes};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::sync::Arc;

const ALICE_SCOPES: &str =
    "memory:read,memory:write,wiki:read,wiki:write,mail:read,mail:send,messaging:send,files:read";

fn principal(client_id: &str, scopes: &str, is_external: bool) -> Principal {
    Principal {
        client_id: client_id.into(),
        scopes: parse_scopes(scopes).unwrap(),
        is_external,
        created_at: chrono::Utc::now(),
    }
}

/// A stored key bound to `agent`, issued now (after the fixture's employee
/// directories were created).
fn bound(agent: &str) -> PrincipalKind {
    PrincipalKind::Stored { agent_binding: Some(agent.into()), key_created_at: chrono::Utc::now() }
}

fn unbound() -> PrincipalKind {
    PrincipalKind::Stored { agent_binding: None, key_created_at: chrono::Utc::now() }
}

fn home() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    for agent in ["alice", "bob"] {
        let dir = tmp.path().join("agents").join(agent);
        std::fs::create_dir_all(dir.join("state")).unwrap();
        std::fs::write(
            dir.join("agent.toml"),
            format!("[agent]\nname = \"{agent} display\"\nstatus = \"active\"\n"),
        )
        .unwrap();
    }
    tmp
}

/// A dispatcher whose process agent is `default_agent`; `remote` marks it the
/// way the HTTP transport does (`mcp_http_server::build_router` / `run`).
fn dispatcher(home: &Path, default_agent: &str, remote: bool) -> crate::mcp_dispatch::McpDispatcher {
    let memory = Arc::new(duduclaw_memory::SqliteMemoryEngine::new(&home.join("memory.db")).unwrap());
    let odoo: crate::mcp_dispatch::OdooState = Arc::new(crate::odoo_pool::OdooConnectorPool::default());
    let d = crate::mcp_dispatch::McpDispatcher::new(
        home.to_path_buf(),
        reqwest::Client::new(),
        memory,
        default_agent.to_string(),
        odoo,
        crate::mcp_rate_limit::RateLimiter::new(),
        crate::mcp_memory_quota::DailyQuota::new(),
    );
    if remote { d.for_remote_bearer() } else { d }
}

/// One `tools/call` as the HTTP transport makes it: inside the authenticated
/// call (carrying the authority's `kind`), with the namespace resolved the
/// way `authenticate_bearer_call` resolves it.
async fn call(
    d: &crate::mcp_dispatch::McpDispatcher,
    p: &Principal,
    kind: PrincipalKind,
    tool: &str,
    args: Value,
) -> Value {
    let ns = crate::mcp_namespace::resolve_for_caller(
        p,
        crate::mcp_namespace::CallerIdentity {
            verified_agent: None,
            client_is_agent: crate::mcp_namespace::client_is_agent(&d.home_dir, &p.client_id),
        },
    )
    .unwrap();
    let authenticated = AuthenticatedCall::without_reference(p.clone()).with_kind(kind);
    let params = json!({"name": tool, "arguments": args});
    crate::mcp_auth::with_authenticated_call(authenticated, d.dispatch_tool_call(p, &ns, &params, &json!(1))).await
}

async fn listed(p: &Principal, kind: PrincipalKind, home: &Path, default_agent: &str, remote: bool) -> BTreeSet<String> {
    let authenticated = AuthenticatedCall::without_reference(p.clone()).with_kind(kind);
    let resp = crate::mcp_auth::with_authenticated_call(
        authenticated,
        handle_tools_list_for_transport(&json!(1), p, home, default_agent, remote),
    )
    .await;
    advertised_tool_names(&resp).into_iter().collect()
}

fn audit_classes(home: &Path, tool: &str) -> Vec<String> {
    std::fs::read_to_string(home.join("tool_calls.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|r| r["tool"] == tool || r["tool_name"] == tool)
        .filter_map(|r| r["error_class"].as_str().map(str::to_string))
        .collect()
}

fn refused_by_union_gate(resp: &Value) -> bool {
    resp["error"]["code"] == -32003
        && resp["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("not available to a per-agent key over HTTP")
}

fn refused_unbound(resp: &Value) -> bool {
    resp["error"]["code"] == -32003 && resp["error"]["message"] == REMOTE_UNBOUND_MESSAGE
}

fn result_text(resp: &Value) -> String {
    resp["result"]["content"][0]["text"].as_str().unwrap_or_default().to_string()
}

#[tokio::test(flavor = "current_thread")]
async fn bound_key_over_http_is_refused_process_agent_tools() {
    let tmp = home();
    let d = dispatcher(tmp.path(), "bob", true);
    let alice = principal("alice", ALICE_SCOPES, false);
    for (tool, args) in [
        ("working_state_get", json!({})),
        ("working_state_set", json!({"key": "k", "value": "v", "reason": "r"})),
        ("mail_list", json!({})),
        ("mail_read", json!({"id": "x"})),
        ("canvas_push", json!({"html": "<p>x</p>"})),
        ("wiki_read", json!({"page_path": "a.md"})),
        ("wiki_write", json!({"page_path": "a.md", "content": "x"})),
        ("send_message", json!({"channel": "telegram", "chat_id": "1", "text": "hi"})),
    ] {
        let resp = call(&d, &alice, bound("alice"), tool, args).await;
        assert!(refused_by_union_gate(&resp), "{tool}: {resp}");
        assert_eq!(audit_classes(tmp.path(), tool), vec!["per_agent_key_process_tool"], "{tool}");
    }
    // Neither employee's state was touched.
    assert!(!tmp.path().join("agents/bob/state/working_state.json").exists());
    assert!(!tmp.path().join("agents/alice/state/working_state.json").exists());
}

#[tokio::test(flavor = "current_thread")]
async fn bound_key_over_http_does_not_list_process_agent_tools() {
    let tmp = home();
    let alice = principal("alice", ALICE_SCOPES, false);
    let remote = listed(&alice, bound("alice"), tmp.path(), "bob", true).await;
    for hidden in ["working_state_get", "working_state_set", "mail_list", "canvas_push", "wiki_read"] {
        assert!(!remote.contains(hidden), "{hidden} must be hidden: {remote:?}");
    }
    for name in &remote {
        assert!(!acts_for_process_agent(name), "{name} acts for the process agent");
    }
    // A tool that derives its target from the principal stays listed.
    assert!(remote.contains("memory_store") && remote.contains("memory_search"), "{remote:?}");
    assert!(remote.contains("file_read"), "{remote:?}");

    // The same key on a stdio server whose process agent IS alice: unchanged.
    let stdio = listed(&alice, bound("alice"), tmp.path(), "alice", false).await;
    assert!(stdio.contains("working_state_get"), "{stdio:?}");
}

/// Third review: a bound key acts as ITS employee — never the HTTP server's
/// `default_agent` — for the tools that remain callable.
#[tokio::test(flavor = "current_thread")]
async fn bound_key_over_http_acts_as_its_own_employee() {
    let tmp = home();
    let d = dispatcher(tmp.path(), "bob", true);
    let alice = principal("alice", ALICE_SCOPES, false);

    let resp = call(&d, &alice, bound("alice"), "memory_store", json!({"content": "alice keeps her own note"})).await;
    assert!(resp.get("error").is_none(), "{resp}");
    assert_ne!(resp["result"]["isError"], true, "{resp}");

    // file_read: alice's own directory is readable, bob's is not.
    std::fs::write(tmp.path().join("agents/alice/notes.txt"), "alice notes").unwrap();
    std::fs::write(tmp.path().join("agents/bob/notes.txt"), "bob notes").unwrap();
    let own = tmp.path().join("agents/alice/notes.txt").canonicalize().unwrap();
    let other = tmp.path().join("agents/bob/notes.txt").canonicalize().unwrap();
    let resp = call(&d, &alice, bound("alice"), "file_read", json!({"path": own.to_string_lossy()})).await;
    assert!(resp.get("error").is_none() && resp["result"]["isError"] != true, "{resp}");
    assert!(result_text(&resp).contains("alice notes"), "{resp}");
    let resp = call(&d, &alice, bound("alice"), "file_read", json!({"path": other.to_string_lossy()})).await;
    assert!(resp.get("error").is_some() || resp["result"]["isError"] == true, "{resp}");
    assert!(!result_text(&resp).contains("bob notes"), "{resp}");

    // Fourth review (Medium 2): `tasks_create` needs `admin`, which a key
    // bound to an employee never exercises — even a principal that somehow
    // carries it is refused by the scope gate (admin never stands in), and
    // nothing is created for either employee.
    let admin_alice = principal("alice", &format!("{ALICE_SCOPES},admin"), false);
    let resp = call(&d, &admin_alice, bound("alice"), "tasks_create", json!({"title": "alice's task"})).await;
    assert_eq!(resp["error"]["code"], -32003, "{resp}");
    assert!(audit_classes(tmp.path(), "tasks_create").iter().any(|c| c == "insufficient_scope"));
}

/// Fourth review (Medium 2): on a key bound to an employee, `admin` never
/// stands in for the individual scopes — a scope the key lacks is refused
/// whatever else it carries; the scopes it holds keep working.
#[tokio::test(flavor = "current_thread")]
async fn admin_never_stands_in_for_scopes_on_a_bound_key() {
    let tmp = home();
    let d = dispatcher(tmp.path(), "bob", true);
    let admin_alice = principal("alice", "memory:read,admin", false);
    let resp = call(&d, &admin_alice, bound("alice"), "memory_store", json!({"content": "x"})).await;
    assert_eq!(resp["error"]["code"], -32003, "memory:write is missing; admin does not stand in: {resp}");
    assert!(audit_classes(tmp.path(), "memory_store").iter().any(|c| c == "insufficient_scope"));
    let resp = call(&d, &admin_alice, bound("alice"), "memory_search", json!({"query": "x"})).await;
    assert!(resp.get("error").is_none(), "held scope still works: {resp}");
    // The gate itself.
    use crate::mcp_auth::{Scope, scope_gate_admits};
    assert!(!scope_gate_admits(&admin_alice, &Scope::MemoryWrite, true));
    assert!(!scope_gate_admits(&admin_alice, &Scope::Admin, true), "admin is not a per-employee scope");
    assert!(scope_gate_admits(&admin_alice, &Scope::MemoryRead, true));
    assert!(scope_gate_admits(&admin_alice, &Scope::MemoryWrite, false), "unchanged for every other principal");
}

/// Fourth review (Medium 2): a stored row bound to an employee that somehow
/// carries `admin` (or another non-employee scope) authenticates with its
/// per-employee scopes only.
#[tokio::test(flavor = "current_thread")]
async fn a_bound_rows_admin_scope_never_takes_effect() {
    use duduclaw_auth::mcp_keys::{Expiry, KeyBinding, McpKeyStore, NewMcpKey};
    let tmp = home();
    let store = McpKeyStore::open(tmp.path()).unwrap();
    let created = store
        .create(
            NewMcpKey {
                name: "alice key".into(),
                client_id: "alice".into(),
                binding: KeyBinding::Employee,
                env: "prod".into(),
                scopes: vec!["memory:read".into()],
                expiry: Expiry::Days(30),
                created_by: Some("admin".into()),
            },
            chrono::Utc::now(),
        )
        .unwrap();
    drop(store);
    // A tampered / legacy row: admin and an internal-only scope added.
    rusqlite::Connection::open(duduclaw_core::mcp_key_store::store_path(tmp.path()))
        .unwrap()
        .execute(
            "UPDATE mcp_api_keys SET scopes = '[\"memory:read\",\"admin\",\"team:handoff\"]' WHERE id = ?1",
            [&created.key.id],
        )
        .unwrap();
    let authenticated = crate::mcp_auth::authenticate_with_key_call(&created.secret, tmp.path()).unwrap();
    let scopes = &authenticated.principal().scopes;
    assert!(!scopes.contains(&crate::mcp_auth::Scope::Admin), "{scopes:?}");
    assert!(!scopes.contains(&crate::mcp_auth::Scope::TeamHandoff), "{scopes:?}");
    assert!(scopes.contains(&crate::mcp_auth::Scope::MemoryRead));
    let d = dispatcher(tmp.path(), "bob", true);
    let p = authenticated.principal().clone();
    let resp = call(&d, &p, authenticated.kind().clone(), "tasks_create", json!({"title": "x"})).await;
    assert_eq!(resp["error"]["code"], -32003, "{resp}");
}

/// Fourth review (Medium 1): a key whose employee was deleted is revoked for
/// good (tombstone); a key older than a re-created employee's directory is
/// refused even before any sweep (where the platform reports a birth time);
/// the new employee's own key works.
#[tokio::test(flavor = "current_thread")]
async fn old_keys_never_act_for_a_recreated_employee() {
    use duduclaw_auth::mcp_keys::{EMPLOYEE_GONE_ACTOR, Expiry, KeyBinding, McpKeyStore, NewMcpKey};
    let tmp = home();
    let req = |name: &str| NewMcpKey {
        name: name.into(),
        client_id: "alice".into(),
        binding: KeyBinding::Employee,
        env: "prod".into(),
        scopes: vec!["memory:read".into(), "memory:write".into()],
        expiry: Expiry::Never,
        created_by: Some("admin".into()),
    };
    let store = McpKeyStore::open(tmp.path()).unwrap();
    let issued = chrono::Utc::now() - chrono::Duration::days(60);
    let tombstoned = store.create(req("alice old 1"), issued).unwrap();
    let predating = store.create(req("alice old 2"), issued).unwrap();
    // The dashboard deleted alice (her keys revoked) … except one key the
    // revocation did not reach (e.g. deleted outside the gateway and
    // re-created before the next sweep).
    store.revoke_employee_keys("alice", EMPLOYEE_GONE_ACTOR, chrono::Utc::now()).unwrap();
    drop(store);
    let conn = rusqlite::Connection::open(duduclaw_core::mcp_key_store::store_path(tmp.path())).unwrap();
    conn.execute(
        "UPDATE mcp_api_keys SET deleted_at = NULL, deleted_by = NULL WHERE id = ?1",
        [&predating.key.id],
    )
    .unwrap();
    drop(conn);
    // A different alice is created later.
    std::fs::remove_dir_all(tmp.path().join("agents/alice")).unwrap();
    std::fs::create_dir_all(tmp.path().join("agents/alice")).unwrap();
    std::fs::write(tmp.path().join("agents/alice/agent.toml"), "[agent]\nname = \"New Alice\"\n").unwrap();

    // The tombstone never authenticates.
    assert!(crate::mcp_auth::authenticate_with_key_call(&tombstoned.secret, tmp.path()).is_err());
    // The surviving old key predates the new directory.
    let authenticated = crate::mcp_auth::authenticate_with_key_call(&predating.secret, tmp.path()).unwrap();
    let p = authenticated.principal().clone();
    let kind = authenticated.kind().clone();
    let d = dispatcher(tmp.path(), "bob", true);
    let resp = call(&d, &p, kind.clone(), "memory_store", json!({"content": "as the new alice"})).await;
    if duduclaw_core::mcp_key_store::employee_dir_birth(tmp.path(), "alice").is_some() {
        assert!(refused_unbound(&resp), "{resp}");
        assert!(listed(&p, kind, tmp.path(), "bob", true).await.is_empty());
        let raw = std::fs::read_to_string(tmp.path().join("tool_calls.jsonl")).unwrap();
        assert!(raw.contains("employee_recreated"), "{raw}");
    }
    // The new alice's own key works.
    let fresh = McpKeyStore::open(tmp.path()).unwrap().create(req("alice new"), chrono::Utc::now()).unwrap();
    let authenticated = crate::mcp_auth::authenticate_with_key_call(&fresh.secret, tmp.path()).unwrap();
    let p = authenticated.principal().clone();
    let resp = call(&d, &p, authenticated.kind().clone(), "memory_store", json!({"content": "new alice"})).await;
    assert!(resp.get("error").is_none(), "{resp}");
}

/// Third review: a key whose employee was removed (directory gone) or
/// soft-deleted is refused EVERYTHING over HTTP, audited with its own reason,
/// and is listed nothing.
#[tokio::test(flavor = "current_thread")]
async fn stale_key_whose_employee_is_gone_or_deleted_is_refused_everything() {
    let tmp = home();
    let d = dispatcher(tmp.path(), "bob", true);
    let alice = principal("alice", ALICE_SCOPES, false);
    std::fs::remove_dir_all(tmp.path().join("agents/alice")).unwrap();
    for (tool, args) in [
        ("memory_store", json!({"content": "x"})),
        ("memory_search", json!({"query": "x"})),
        ("wiki_read", json!({"page_path": "a.md"})),
        ("working_state_get", json!({})),
    ] {
        let resp = call(&d, &alice, bound("alice"), tool, args).await;
        assert!(refused_unbound(&resp), "{tool}: {resp}");
        assert_eq!(audit_classes(tmp.path(), tool), vec!["remote_key_unbound"], "{tool}");
    }
    assert!(listed(&alice, bound("alice"), tmp.path(), "bob", true).await.is_empty());
    let raw = std::fs::read_to_string(tmp.path().join("tool_calls.jsonl")).unwrap();
    assert!(raw.contains("employee_missing"), "distinct internal reason: {raw}");

    // Soft-deleted employee.
    let tmp = home();
    let d = dispatcher(tmp.path(), "bob", true);
    std::fs::write(tmp.path().join("agents/alice/agent.toml"), "[agent]\nname = \"alice\"\nstatus = \"deleted\"\n").unwrap();
    let resp = call(&d, &alice, bound("alice"), "memory_store", json!({"content": "x"})).await;
    assert!(refused_unbound(&resp), "{resp}");
    assert!(listed(&alice, bound("alice"), tmp.path(), "bob", true).await.is_empty());
}

/// Third review: a key without a valid binding — created under a display name
/// / alias, a migrated legacy entry whose client id was not a directory id,
/// an unknown internal client id, a binding that is not the key's own client
/// id, or a call with no authority decision at all — is refused everything
/// over HTTP and is listed nothing. Never the server's `default_agent`.
#[tokio::test(flavor = "current_thread")]
async fn keys_without_a_valid_binding_are_refused_everything() {
    let tmp = home();
    let d = dispatcher(tmp.path(), "bob", true);
    let cases: Vec<(Principal, PrincipalKind)> = vec![
        // (A client id with a space never reaches dispatch: the HTTP
        // transport's namespace resolution refuses it first.)
        (principal("AliceDisplay", ALICE_SCOPES, false), unbound()),
        (principal("Alice", ALICE_SCOPES, false), unbound()),
        (principal("ops-bot", ALICE_SCOPES, false), unbound()),
        (principal("alice", ALICE_SCOPES, false), unbound()),
        (principal("alice", ALICE_SCOPES, false), bound("bob")),
        (principal("alice", ALICE_SCOPES, false), PrincipalKind::Other),
        (principal("operator", "admin", false), PrincipalKind::Other),
    ];
    for (p, kind) in cases {
        for (tool, args) in [
            ("memory_store", json!({"content": "x"})),
            ("file_read", json!({"path": "/etc/hosts"})),
            ("working_state_get", json!({})),
        ] {
            let resp = call(&d, &p, kind.clone(), tool, args).await;
            assert!(refused_unbound(&resp), "{} {kind:?} {tool}: {resp}", p.client_id);
        }
        assert!(listed(&p, kind.clone(), tmp.path(), "bob", true).await.is_empty(), "{} {kind:?}", p.client_id);
    }
    assert!(!tmp.path().join("agents/bob/state/working_state.json").exists());
}

/// End to end through the real authenticator: a per-agent key created by the
/// employee's id is bound and works as that employee; a legacy entry migrated
/// under a display name has no binding and is refused over HTTP.
#[tokio::test(flavor = "current_thread")]
async fn stored_keys_end_to_end_bound_works_and_migrated_alias_is_refused() {
    use duduclaw_auth::mcp_keys::{Expiry, KeyBinding, LegacyKey, McpKeyStore, NewMcpKey};
    let tmp = home();
    let now = chrono::Utc::now();
    let store = McpKeyStore::open(tmp.path()).unwrap();
    let created = store
        .create(
            NewMcpKey {
                name: "alice key".into(),
                client_id: "alice".into(),
                binding: KeyBinding::Employee,
                env: "prod".into(),
                scopes: vec!["memory:read".into(), "memory:write".into()],
                expiry: Expiry::Days(30),
                created_by: Some("admin".into()),
            },
            now,
        )
        .unwrap();
    assert_eq!(created.key.agent_binding.as_deref(), Some("alice"));
    let legacy_raw = format!("ddc_prod_{}", "a1".repeat(16));
    store
        .import_legacy(
            &LegacyKey {
                raw: &legacy_raw,
                client_id: "AliceChen",
                is_external: false,
                scopes: vec!["memory:read".into(), "memory:write".into()],
                created_at: now,
                label: None,
                legacy_entry_hash: "f".repeat(64),
                agent_binding: None,
            },
            now,
        )
        .unwrap();
    drop(store);

    let d = dispatcher(tmp.path(), "bob", true);
    for (raw, works) in [(created.secret.clone(), true), (legacy_raw.clone(), false)] {
        let authenticated = crate::mcp_auth::authenticate_with_key_call(&raw, tmp.path()).unwrap();
        let p = authenticated.principal().clone();
        let kind = authenticated.kind().clone();
        let resp = call(&d, &p, kind.clone(), "memory_store", json!({"content": "via a stored key"})).await;
        if works {
            assert!(resp.get("error").is_none(), "{resp}");
            let listing = listed(&p, kind, tmp.path(), "bob", true).await;
            assert!(listing.contains("memory_store") && !listing.contains("working_state_get"), "{listing:?}");
        } else {
            assert!(refused_unbound(&resp), "{resp}");
            assert!(listed(&p, kind, tmp.path(), "bob", true).await.is_empty());
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn gateway_internal_and_stdio_employee_paths_are_unaffected() {
    let tmp = home();
    let internal = principal(duduclaw_gateway::mcp_internal_key::INTERNAL_CLIENT_ID, "admin", false);
    // The verified gateway-internal principal is unaffected on either transport.
    for remote in [true, false] {
        let d = dispatcher(tmp.path(), "bob", remote);
        let resp = call(&d, &internal, PrincipalKind::GatewayInternal, "working_state_get", json!({})).await;
        assert!(!refused_by_union_gate(&resp) && !refused_unbound(&resp), "remote={remote}: {resp}");
        let names = listed(&internal, PrincipalKind::GatewayInternal, tmp.path(), "bob", remote).await;
        assert!(names.contains("working_state_get"), "remote={remote}: {names:?}");
    }
    // A config entry merely claiming the client id (no recorded fingerprint)
    // is not the verified principal: refused over HTTP.
    let d = dispatcher(tmp.path(), "bob", true);
    let resp = call(&d, &internal, PrincipalKind::Other, "working_state_get", json!({})).await;
    assert!(refused_unbound(&resp), "{resp}");
    // A stdio process registered for alice with alice's own key: unchanged,
    // whatever the authority recorded.
    let d = dispatcher(tmp.path(), "alice", false);
    let alice = principal("alice", ALICE_SCOPES, false);
    for kind in [bound("alice"), unbound(), PrincipalKind::Other] {
        let resp = call(&d, &alice, kind.clone(), "working_state_get", json!({})).await;
        assert!(!refused_by_union_gate(&resp) && !refused_unbound(&resp), "{kind:?}: {resp}");
    }
    assert!(!audit_classes(tmp.path(), "working_state_get").iter().any(|c| c == "per_agent_key_process_tool"));
}

/// The predicate the dispatch gate and `tools/list` share. Default deny: it
/// refuses the union to every non-external principal except the verified
/// gateway-internal one — including a client id that names no employee
/// directory (the case a v1.82 review-round test used to pin as allowed).
#[test]
fn predicate_refuses_the_union_to_every_non_gateway_internal_principal() {
    let alice = principal("alice", ALICE_SCOPES, false);
    let alias = principal("Alice Chen", ALICE_SCOPES, false);
    let internal = principal(duduclaw_gateway::mcp_internal_key::INTERNAL_CLIENT_ID, "memory:read", false);
    let external = principal("alice", "memory:read", true);
    let gw = PrincipalKind::GatewayInternal;
    for name in PROCESS_AGENT_TOOLS
        .iter()
        .chain(DEFAULT_AGENT_HANDLER_TOOLS)
        .chain(COMPUTER_USE_TOOLS)
        .chain(COMPUTER_WORKSPACE_TOOLS)
        .chain(FORK_TOOLS)
        .chain(super::KNOWLEDGE_TOOLS)
        .chain(["odoo_search", "odoo_create"].iter())
    {
        assert!(remote_process_agent_tool_refused(name, &alice, Some(&bound("alice")), "bob"), "{name}");
        assert!(!remote_process_agent_tool_refused(name, &internal, Some(&gw), "bob"), "internal: {name}");
        assert!(!remote_process_agent_tool_refused(name, &external, None, "bob"), "external: {name}");
        // Formerly pinned as allowed ("not an employee"): now refused.
        assert!(remote_process_agent_tool_refused(name, &alias, Some(&unbound()), "bob"), "alias: {name}");
        assert!(remote_process_agent_tool_refused(name, &alice, None, "bob"), "no authority decision: {name}");
        // The gateway-internal client id without the recorded kind, or in a
        // process without a verified employee identity, is not verified.
        assert!(remote_process_agent_tool_refused(name, &internal, Some(&PrincipalKind::Other), "bob"), "{name}");
        assert!(remote_process_agent_tool_refused(name, &internal, Some(&gw), ""), "{name}");
        assert!(
            remote_process_agent_tool_refused(name, &internal, Some(&gw), duduclaw_core::UNTRUSTED_AGENT_ID),
            "{name}"
        );
    }
    for name in ["memory_store", "memory_search", "tasks_create", "create_task", "file_read"] {
        assert!(!remote_process_agent_tool_refused(name, &alice, Some(&bound("alice")), "bob"), "{name}");
    }
}

/// Third review: inside a remote scope, identity resolution never falls back
/// to `default_agent`.
#[tokio::test(flavor = "current_thread")]
async fn remote_identity_resolution_never_falls_back_to_default_agent() {
    let tmp = home();
    let path = tmp.path().to_path_buf();
    with_remote_binding(RemoteBinding::Unbound, async {
        assert_eq!(try_acting_agent_id("alice", "bob"), Err(RemoteBindingMissing));
        assert_eq!(acting_agent_id("alice", "bob"), duduclaw_core::UNTRUSTED_AGENT_ID);
        assert_eq!(acting_agent_id("", "bob"), duduclaw_core::UNTRUSTED_AGENT_ID);
        assert!(try_record_actor_for(&path, "alice", "bob").is_err());
        assert_eq!(record_actor_for(&path, "alice", "bob").agent(), Some(duduclaw_core::UNTRUSTED_AGENT_ID));
    })
    .await;
    with_remote_binding(RemoteBinding::Bound("alice".into()), async {
        assert_eq!(try_acting_agent_id("alice", "bob"), Ok("alice"));
        assert_eq!(try_acting_agent_id("Alice", "bob"), Err(RemoteBindingMissing));
        assert_eq!(try_record_actor_for(&path, "alice", "bob").unwrap().agent(), Some("alice"));
        assert!(try_record_actor_for(&path, "gateway-internal", "bob").is_err());
    })
    .await;
    assert_eq!(RecordActor::resolve_remote("alice", Some("alice")).unwrap().agent(), Some("alice"));
    assert!(RecordActor::resolve_remote("alice", None).is_err());
    assert!(RecordActor::resolve_remote("alice", Some("bob")).is_err());
    // Outside a remote scope (stdio) nothing changes.
    assert_eq!(acting_agent_id("", "bob"), "bob");
    assert_eq!(acting_agent_id(duduclaw_gateway::mcp_internal_key::INTERNAL_CLIENT_ID, "bob"), "bob");
    assert_eq!(acting_agent_id("alice", "bob"), "alice");
}
