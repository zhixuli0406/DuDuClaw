//! RM-134: `mcp_keys.*` over the hashed key store, end to end through
//! `MethodHandler::handle`.
use super::*;

use duduclaw_core::mcp_key_store as ks;

#[test]
fn mcp_key_days_left_rounds_up_and_never_goes_negative() {
    let now = Utc::now();
    let days_left = crate::identity_transition::days_left;

    assert_eq!(days_left(now + chrono::Duration::days(29) + chrono::Duration::seconds(1), now), 30);
    assert_eq!(days_left(now + chrono::Duration::days(30), now), 30);
    assert_eq!(days_left(now + chrono::Duration::seconds(1), now), 1);
    assert_eq!(days_left(now - chrono::Duration::seconds(1), now), 0);
}

const MUTATING: [&str; 8] = [
    "mcp_keys.create", "mcp_keys.rename", "mcp_keys.set_expiry", "mcp_keys.disable",
    "mcp_keys.enable", "mcp_keys.delete", "mcp_keys.revoke", "mcp_keys.dismiss_migration_hint",
];

fn admin() -> UserContext {
    let mut ctx = UserContext::admin_fallback();
    ctx.user_id = "admin-rm134".into();
    ctx
}

fn ok_payload(frame: WsFrame) -> Value {
    match frame {
        WsFrame::Response { ok: true, payload: Some(p), .. } => p,
        other => panic!("expected ok response, got {other:?}"),
    }
}

fn err(frame: &WsFrame) -> Value {
    match frame {
        WsFrame::Response { ok: false, error: Some(e), .. } => e.clone(),
        other => panic!("expected error response, got {other:?}"),
    }
}

fn err_code(frame: &WsFrame) -> String {
    err(frame).get("code").and_then(Value::as_str).unwrap_or("").to_string()
}

fn audits(home: &Path, event: &str) -> Vec<Value> {
    std::fs::read_to_string(home.join("security_audit.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .filter(|v| v["event_type"] == event)
        .collect()
}

async fn create_with(handler: &MethodHandler, params: Value) -> WsFrame {
    handler.handle("mcp_keys.create", params, &admin()).await
}

async fn create(handler: &MethodHandler, name: &str, expiry: Value) -> WsFrame {
    create_with(
        handler,
        json!({ "name": name, "client_id": "zapier", "env": "prod", "scopes": ["memory:read"], "expiry": expiry }),
    )
    .await
}

async fn fixture() -> (tempfile::TempDir, MethodHandler) {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"), "[general]\n").unwrap();
    let handler = MethodHandler::new(home.path().into()).await;
    (home, handler)
}

fn employee(home: &Path, id: &str, name: &str) {
    std::fs::create_dir_all(home.join("agents").join(id)).unwrap();
    std::fs::write(home.join("agents").join(id).join("agent.toml"), format!("[agent]\nname = \"{name}\"\n")).unwrap();
}

#[tokio::test]
async fn mcp_keys_rpc_is_admin_only_and_refusals_are_audited() {
    let (home, handler) = fixture().await;
    let mut caller = admin();
    for role in [UserRole::Employee, UserRole::Manager] {
        caller.role = role;
        for method in std::iter::once("mcp_keys.list").chain(MUTATING) {
            let frame = handler.handle(method, json!({ "id": "mk_abc", "name": "x" }), &caller).await;
            assert!(matches!(frame, WsFrame::Response { ok: false, .. }), "{method} as {role:?}");
        }
    }
    let refused = audits(home.path(), "mcp_key_refused");
    assert_eq!(refused.len(), MUTATING.len() * 2, "every refused mutation is on record");
    assert!(refused.iter().all(|e| e["details"]["actor"] == "admin-rm134" && e["details"]["outcome"] == "denied"));
    assert!(!ks::store_path(home.path()).exists(), "a refused call never touches the store");
}

#[tokio::test]
async fn create_returns_the_key_once_and_stores_only_its_digest() {
    let (home, handler) = fixture().await;
    let payload = ok_payload(create(&handler, "Zapier", json!({ "kind": "days", "days": 90 })).await);
    let secret = payload["key"].as_str().unwrap().to_string();
    assert!(ks::is_key_format(&secret));
    let info = &payload["key_info"];
    assert_eq!(info["name"], "Zapier");
    assert_eq!(info["status"], "active");
    assert_eq!(info["created_by"], "admin-rm134");
    assert_eq!(info["days_left"], 90);
    assert_eq!(info["is_external"], true, "every dashboard key is external");

    let listed = ok_payload(handler.handle("mcp_keys.list", json!({}), &admin()).await);
    assert_eq!(listed["keys"].as_array().unwrap().len(), 1);
    assert_eq!(listed["migration_hint"], false);
    assert_eq!(listed["legacy_pending"], 0);
    assert_eq!(listed["ignored_legacy_entries"], json!([]));
    assert_eq!(listed["effective_scopes"], json!(["memory:read", "memory:write", "wiki:read", "wiki:write", "messaging:send"]));
    assert_eq!(listed["keys"][0]["ineffective_scopes"], json!([]));
    assert!(listed["keys"][0]["last_used_at"].is_null());
    let digest = ks::digest(&secret);
    let rendered = listed.to_string() + &info.to_string();
    assert!(!rendered.contains(&secret) && !rendered.contains(&digest) && !rendered.contains("key_hash"));
    // Neither config.toml, the store file nor the audit log hold the key.
    assert!(!std::fs::read_to_string(home.path().join("config.toml")).unwrap().contains(&secret));
    let store_bytes = std::fs::read(ks::store_path(home.path())).unwrap();
    assert!(!store_bytes.windows(secret.len()).any(|w| w == secret.as_bytes()));
    let audit_raw = std::fs::read_to_string(home.path().join("security_audit.jsonl")).unwrap();
    assert!(!audit_raw.contains(&secret) && !audit_raw.contains(&digest));
    let created = audits(home.path(), "mcp_key_created");
    assert_eq!(created.len(), 1);
    assert_eq!(created[0]["details"]["actor"], "admin-rm134");
    assert_eq!(created[0]["details"]["no_expiry"], false);
    assert_eq!(created[0]["severity"], "info");

    // H4: last use comes from the usage file, merged when the list is built.
    let id = info["id"].as_str().unwrap();
    ks::record_usage(home.path(), id, Utc::now()).unwrap();
    let listed = ok_payload(handler.handle("mcp_keys.list", json!({}), &admin()).await);
    assert!(listed["keys"][0]["last_used_at"].as_str().is_some());
}

#[tokio::test]
async fn no_expiry_needs_confirmation_and_is_recorded_with_who_and_when() {
    let (home, handler) = fixture().await;
    let refused = create(&handler, "forever", json!({ "kind": "never" })).await;
    assert_eq!(err_code(&refused), "mcp_key_no_expiry_unconfirmed");
    let refused = create(&handler, "forever", json!({ "kind": "never", "confirm": "yes" })).await;
    assert_eq!(err_code(&refused), "mcp_key_no_expiry_unconfirmed");
    for bad in [json!(null), json!({ "kind": "days", "days": 45 }), json!({ "kind": "date", "at": "2020-01-01T00:00:00Z" }),
        json!({ "kind": "date", "at": "next week" }), json!({ "kind": "forever" })] {
        let frame = create(&handler, "bad", bad.clone()).await;
        assert_eq!(err_code(&frame), "mcp_key_invalid_expiry", "{bad}");
    }
    assert!(audits(home.path(), "mcp_key_created").is_empty());
    let payload = ok_payload(create(&handler, "forever", json!({ "kind": "never", "confirm": true })).await);
    assert!(payload["key_info"]["expires_at"].is_null());
    let created = audits(home.path(), "mcp_key_created");
    assert_eq!(created[0]["severity"], "warning");
    assert_eq!(created[0]["details"]["no_expiry"], true);
    assert_eq!(created[0]["details"]["no_expiry_confirmed_by"], "admin-rm134");
    assert!(created[0]["details"]["no_expiry_confirmed_at"].as_str().is_some());
    // A custom date is accepted.
    let at = ks::timestamp(Utc::now() + chrono::Duration::days(200));
    let payload = ok_payload(create(&handler, "custom", json!({ "kind": "date", "at": at })).await);
    assert_eq!(payload["key_info"]["expires_at"], at);
}

#[tokio::test]
async fn management_acts_by_id_and_every_change_is_audited() {
    let (home, handler) = fixture().await;
    let payload = ok_payload(create(&handler, "ops", json!({ "kind": "days", "days": 30 })).await);
    let secret = payload["key"].as_str().unwrap().to_string();
    let id = payload["key_info"]["id"].as_str().unwrap().to_string();
    let call = |method: &'static str, params: Value| {
        let handler = &handler;
        async move { handler.handle(method, params, &admin()).await }
    };

    let renamed = ok_payload(call("mcp_keys.rename", json!({ "id": id, "name": "ops bot" })).await);
    assert_eq!(renamed["key_info"]["name"], "ops bot");
    let changed = ok_payload(call("mcp_keys.set_expiry", json!({ "id": id, "expiry": { "kind": "days", "days": 365 } })).await);
    assert_eq!(changed["key_info"]["days_left"], 365);
    let disabled = ok_payload(call("mcp_keys.disable", json!({ "id": id })).await);
    assert_eq!(disabled["key_info"]["status"], "disabled");
    assert_eq!(disabled["key_info"]["disabled_by"], "admin-rm134");
    assert_eq!(ks::lookup(home.path(), &secret).unwrap().unwrap().state(Utc::now()), ks::KeyState::Disabled);
    let enabled = ok_payload(call("mcp_keys.enable", json!({ "id": id })).await);
    assert_eq!(enabled["key_info"]["status"], "active");
    let changed = ok_payload(call("mcp_keys.set_expiry", json!({ "id": id, "expiry": { "kind": "never", "confirm": true } })).await);
    assert!(changed["key_info"]["expires_at"].is_null());
    // The plaintext is no way to address a key any more.
    assert_eq!(err_code(&call("mcp_keys.revoke", json!({ "key": secret })).await), "mcp_key_id_required");
    assert_eq!(err_code(&call("mcp_keys.disable", json!({ "id": secret })).await), "mcp_key_not_found");
    let deleted = ok_payload(call("mcp_keys.delete", json!({ "id": id })).await);
    assert_eq!(deleted["deleted"], id.as_str());
    assert_eq!(err_code(&call("mcp_keys.delete", json!({ "id": id })).await), "mcp_key_not_found");
    assert!(ok_payload(call("mcp_keys.list", json!({})).await)["keys"].as_array().unwrap().is_empty());
    assert_eq!(ks::lookup(home.path(), &secret).unwrap().unwrap().state(Utc::now()), ks::KeyState::Deleted);

    for (event, count) in [("mcp_key_renamed", 1), ("mcp_key_expiry_changed", 2), ("mcp_key_disabled", 1),
        ("mcp_key_enabled", 1), ("mcp_key_deleted", 1)] {
        let records = audits(home.path(), event);
        assert_eq!(records.len(), count, "{event}");
        assert!(records.iter().all(|e| e["details"]["actor"] == "admin-rm134" && e["details"]["key_id"] == id.as_str()), "{event}");
    }
    let renamed = &audits(home.path(), "mcp_key_renamed")[0];
    assert_eq!((renamed["details"]["from"].as_str(), renamed["details"]["to"].as_str()), (Some("ops"), Some("ops bot")));
    let never = &audits(home.path(), "mcp_key_expiry_changed")[1];
    assert_eq!(never["details"]["no_expiry"], true);
    assert_eq!(never["details"]["no_expiry_confirmed_by"], "admin-rm134");
    let raw = std::fs::read_to_string(home.path().join("security_audit.jsonl")).unwrap();
    assert!(!raw.contains(&secret) && !raw.contains(&ks::digest(&secret)));
}

#[tokio::test]
async fn names_are_unique_and_validated() {
    let (_home, handler) = fixture().await;
    ok_payload(create(&handler, "Zapier", json!({ "kind": "days", "days": 30 })).await);
    assert_eq!(err_code(&create(&handler, "zapier", json!({ "kind": "days", "days": 30 })).await), "mcp_key_name_taken");
    assert_eq!(err_code(&create(&handler, "", json!({ "kind": "days", "days": 30 })).await), "mcp_key_invalid_name");
    let long = "名".repeat(61);
    assert_eq!(err_code(&create(&handler, &long, json!({ "kind": "days", "days": 30 })).await), "mcp_key_invalid_name");
    let frame = create_with(&handler, json!({ "name": "eph", "client_id": "eph-1",
        "scopes": ["memory:read"], "expiry": { "kind": "days", "days": 30 } })).await;
    assert_eq!(err_code(&frame), "mcp_key_invalid_client_id");
    let frame = create_with(&handler, json!({ "name": "scopes", "client_id": "c", "scopes": ["root"],
        "expiry": { "kind": "days", "days": 30 } })).await;
    assert_eq!(err_code(&frame), "mcp_key_invalid_scopes");
}

/// H3: the dashboard issues external keys only, never under an AI
/// employee's id or a reserved system name.
#[tokio::test]
async fn create_forces_external_and_refuses_employee_ids_and_reserved_names() {
    let (home, handler) = fixture().await;
    employee(home.path(), "sales", "Sales Bot");
    let expiry = json!({ "kind": "days", "days": 30 });
    for flag in [json!(false), json!("true"), json!(null)] {
        let frame = create_with(&handler, json!({ "name": "internal", "client_id": "ci", "is_external": flag,
            "scopes": ["memory:read"], "expiry": expiry })).await;
        assert_eq!(err_code(&frame), "mcp_key_internal_not_allowed", "{flag}");
    }
    for client in ["sales", "SALES", "Sales Bot", "gateway-internal", "dashboard", "cron", "default", "Heartbeat",
        "workflow", "system", "standalone-x", "__x"] {
        let frame = create_with(&handler, json!({ "name": format!("n-{client}"), "client_id": client,
            "scopes": ["memory:read"], "expiry": expiry })).await;
        assert_eq!(err_code(&frame), "mcp_key_invalid_client_id", "{client}");
    }
    let ok = ok_payload(create_with(&handler, json!({ "name": "explicit", "client_id": "crm", "is_external": true,
        "scopes": ["memory:read"], "expiry": expiry })).await);
    assert_eq!(ok["key_info"]["is_external"], true);
    let stored = ks::lookup(home.path(), ok["key"].as_str().unwrap()).unwrap().unwrap();
    assert!(stored.is_external);
}

/// RM-157: only the externally grantable scopes, named in the error.
#[tokio::test]
async fn create_refuses_scopes_outside_the_effective_list() {
    let (_home, handler) = fixture().await;
    for scope in ["admin", "odoo:read", "google:read", "messaging:read", "team:handoff"] {
        let frame = create_with(&handler, json!({ "name": format!("s-{scope}"), "client_id": "c",
            "scopes": ["memory:read", scope], "expiry": { "kind": "days", "days": 30 } })).await;
        let error = err(&frame);
        assert_eq!(error["code"], "mcp_key_scope_not_grantable", "{scope}");
        assert_eq!(error["scope"], scope);
        assert_eq!(error["allowed_scopes"].as_array().unwrap().len(), 5);
    }
}

/// H3: the documented per-agent keys (RFC-21, the v1.76 dedicated Google
/// key) stay available — only through the explicit `agent_id` path (which the
/// Personal gate covers: `personal_data_gate::write_scoped`), only for an
/// existing employee, and never with `admin`.
#[tokio::test]
async fn per_agent_keys_need_agent_id_an_existing_employee_and_no_admin() {
    let (home, handler) = fixture().await;
    employee(home.path(), "sales", "Sales Bot");
    let expiry = json!({ "kind": "days", "days": 30 });
    let ok = ok_payload(create_with(&handler, json!({ "name": "sales odoo", "agent_id": "sales",
        "scopes": ["odoo:read", "google:read"], "expiry": expiry })).await);
    assert_eq!(ok["key_info"]["client_id"], "sales");
    assert_eq!(ok["key_info"]["is_external"], false, "a per-agent key acts as the employee");
    assert_eq!(ok["key_info"]["employee_key"], true);
    assert_eq!(ok["key_info"]["ineffective_scopes"], json!([]));
    for scope in ["admin", "team:handoff"] {
        let frame = create_with(&handler, json!({ "name": format!("x-{scope}"), "agent_id": "sales",
            "scopes": ["memory:read", scope], "expiry": expiry })).await;
        assert_eq!(err_code(&frame), "mcp_key_scope_not_grantable", "{scope}");
    }
    let frame = create_with(&handler, json!({ "name": "flag", "agent_id": "sales", "is_external": true,
        "scopes": ["memory:read"], "expiry": expiry })).await;
    assert_eq!(err_code(&frame), "mcp_key_internal_not_allowed");
    assert_eq!(crate::handlers::personal_data_gate::write_scoped("mcp_keys.create"),
        Some(("agent_id", crate::personal_visibility::DataCategory::Identity)));
    for (agent, client) in [("ghost", None), ("sales", Some("other"))] {
        let mut params = json!({ "name": "bad", "agent_id": agent, "scopes": ["google:read"], "expiry": expiry });
        if let Some(client) = client {
            params["client_id"] = json!(client);
        }
        assert_eq!(err_code(&create_with(&handler, params).await), "mcp_key_invalid_client_id", "{agent}");
    }
    let created = audits(home.path(), "mcp_key_created");
    assert_eq!(created.len(), 1);
    assert_eq!(created[0]["details"]["employee_key"], true);
}

/// Third review (High): a per-agent key is created by the employee's exact
/// directory id and bound to it; the `[agent] name` / display name or another
/// case of the id is refused with its own message. A key whose employee is
/// removed or deleted is listed as "employee no longer exists".
#[tokio::test]
async fn per_agent_keys_are_created_by_directory_id_and_flag_a_missing_employee() {
    let (home, handler) = fixture().await;
    employee(home.path(), "sales", "Sales Bot");
    employee(home.path(), "ops", "Ops Desk");
    let expiry = json!({ "kind": "days", "days": 30 });
    for alias in ["Sales Bot", "SALES", "Sales"] {
        let frame = create_with(&handler, json!({ "name": format!("alias {alias}"), "agent_id": alias,
            "scopes": ["memory:read"], "expiry": expiry })).await;
        assert_eq!(err_code(&frame), "mcp_key_employee_id_required", "{alias}");
        assert!(err(&frame)["message"].as_str().unwrap().contains("employee's id"), "{alias}");
    }
    let sales = ok_payload(create_with(&handler, json!({ "name": "sales key", "agent_id": "sales",
        "scopes": ["memory:read"], "expiry": expiry })).await);
    let ops = ok_payload(create_with(&handler, json!({ "name": "ops key", "agent_id": "ops",
        "scopes": ["memory:read"], "expiry": expiry })).await);
    assert_eq!(sales["key_info"]["agent_binding"], "sales");
    assert_eq!(sales["key_info"]["binding_status"], "live");
    assert_eq!(sales["key_info"]["employee_missing"], false);
    let stored = ks::lookup(home.path(), sales["key"].as_str().unwrap()).unwrap().unwrap();
    assert_eq!(stored.agent_binding.as_deref(), Some("sales"));
    // An integration key has no binding.
    let zapier = ok_payload(create(&handler, "Zapier", expiry.clone()).await);
    assert!(zapier["key_info"]["agent_binding"].is_null() && zapier["key_info"]["binding_status"].is_null());

    // Employee removed (directory gone) and employee soft-deleted.
    std::fs::remove_dir_all(home.path().join("agents/sales")).unwrap();
    std::fs::write(home.path().join("agents/ops/agent.toml"), "[agent]\nname = \"Ops Desk\"\nstatus = \"deleted\"\n").unwrap();
    let listed = ok_payload(handler.handle("mcp_keys.list", json!({}), &admin()).await);
    let row = |name: &str| listed["keys"].as_array().unwrap().iter().find(|k| k["name"] == name).unwrap().clone();
    assert_eq!(row("sales key")["employee_missing"], true);
    assert_eq!(row("sales key")["binding_status"], "employee_missing");
    assert_eq!(row("ops key")["employee_missing"], true);
    assert_eq!(row("ops key")["binding_status"], "employee_deleted");
    assert_eq!(row("Zapier")["employee_missing"], false);
    assert_eq!(ops["key_info"]["agent_binding"], "ops");
}

/// A complete, non-main `agent.toml` the dashboard's off-board RPCs can
/// parse and rewrite.
fn offboardable_employee(home: &Path, id: &str) {
    let dir = home.join("agents").join(id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("agent.toml"),
        format!(
            "[agent]\nname = \"{id}\"\ndisplay_name = \"{id}\"\nrole = \"specialist\"\nstatus = \"active\"\n\
             trigger = \"@{id}\"\nreports_to = \"root-lead\"\nicon = \"🤖\"\ndepartment = \"銷售部\"\n\n\
             [model]\npreferred = \"claude-sonnet-4-6\"\nfallback = \"claude-haiku-4-5\"\naccount_pool = [\"main\"]\n\n\
             [container]\ntimeout_ms = 1800000\nmax_concurrent = 1\nreadonly_project = true\nadditional_mounts = []\n\n\
             [heartbeat]\nenabled = false\ninterval_seconds = 3600\nmax_concurrent_runs = 1\ncron = \"\"\n\n\
             [budget]\nmonthly_limit_cents = 5000\nwarn_threshold_percent = 80\nhard_stop = true\n\n\
             [permissions]\ncan_create_agents = false\ncan_send_cross_agent = true\ncan_modify_own_skills = true\n\
             can_modify_own_soul = false\ncan_schedule_tasks = false\nallowed_channels = [\"*\"]\n\n\
             [evolution]\nmicro_reflection = false\nmeso_reflection = false\nmacro_reflection = false\n\
             skill_auto_activate = false\nskill_security_scan = true\n"
        ),
    )
    .unwrap();
}

/// Fourth review (Medium 1): deleting an employee on the dashboard
/// permanently revokes (tombstones, audits) every key bound to it in the same
/// operation. Unarchiving a deleted employee is refused, a later
/// employee with the same id never gets them, and a new key for it works.
#[tokio::test]
async fn deleting_an_employee_on_the_dashboard_permanently_revokes_its_keys() {
    let (home, _) = fixture().await;
    offboardable_employee(home.path(), "alice");
    offboardable_employee(home.path(), "bob");
    let handler = MethodHandler::new(home.path().into()).await;
    let expiry = json!({ "kind": "never", "confirm": true });
    let mut alice_secrets = Vec::new();
    for name in ["alice key 1", "alice key 2"] {
        let created = ok_payload(create_with(&handler, json!({ "name": name, "agent_id": "alice",
            "scopes": ["memory:read"], "expiry": expiry })).await);
        alice_secrets.push(created["key"].as_str().unwrap().to_string());
    }
    let bob = ok_payload(create_with(&handler, json!({ "name": "bob key", "agent_id": "bob",
        "scopes": ["memory:read"], "expiry": expiry })).await)["key"].as_str().unwrap().to_string();
    let zapier = ok_payload(create(&handler, "Zapier", json!({ "kind": "days", "days": 30 })).await)["key"]
        .as_str().unwrap().to_string();

    let frame = handler.handle_agents_remove(json!({ "agent_id": "alice" })).await;
    let payload = ok_payload(frame);
    assert_eq!(payload["status"], "deleted");
    assert_eq!(payload["mcp_keys_revoked"], 2);
    let state = |secret: &str| ks::lookup(home.path(), secret).unwrap().unwrap().state(Utc::now());
    for secret in &alice_secrets {
        assert_eq!(state(secret), ks::KeyState::Deleted, "tombstoned");
    }
    assert_eq!(state(&bob), ks::KeyState::Active);
    assert_eq!(state(&zapier), ks::KeyState::Active);
    let revoked = audits(home.path(), "mcp_key_deleted");
    assert_eq!(revoked.len(), 2);
    assert!(revoked.iter().all(|e| e["details"]["agent_binding"] == "alice"
        && e["details"]["reason"] == "employee_deleted"
        && e["details"]["source"] == "dashboard:agents.remove"
        && e["details"]["permanent"] == true));
    let raw = std::fs::read_to_string(home.path().join("security_audit.jsonl")).unwrap();
    for secret in &alice_secrets {
        assert!(!raw.contains(secret.as_str()) && !raw.contains(&ks::digest(secret)));
    }
    // Hidden from the keys page.
    let listed = ok_payload(handler.handle("mcp_keys.list", json!({}), &admin()).await);
    assert!(!listed["keys"].as_array().unwrap().iter().any(|k| k["agent_binding"] == "alice"));

    // The dashboard cannot restore a soft-deleted employee; keys stay deleted.
    let refused = handler.handle_agents_unarchive(json!({ "agent_id": "alice" })).await;
    assert_eq!(err_code(&refused), "agent_deleted");
    for secret in &alice_secrets {
        assert_eq!(state(secret), ks::KeyState::Deleted);
    }
    // An operator restores active employee data on disk; the tombstones remain.
    offboardable_employee(home.path(), "alice");
    assert_eq!(ks::binding_state(home.path(), "alice"), ks::BindingState::Live);
    for secret in &alice_secrets {
        assert_eq!(state(secret), ks::KeyState::Deleted);
    }
    // A different employee re-created under the same id: the old keys stay
    // tombstoned; its own new key works.
    std::fs::remove_dir_all(home.path().join("agents/alice")).unwrap();
    offboardable_employee(home.path(), "alice");
    for secret in &alice_secrets {
        assert_eq!(state(secret), ks::KeyState::Deleted);
    }
    let fresh = ok_payload(create_with(&handler, json!({ "name": "new alice key", "agent_id": "alice",
        "scopes": ["memory:read"], "expiry": expiry })).await);
    let fresh_secret = fresh["key"].as_str().unwrap();
    assert_eq!(state(fresh_secret), ks::KeyState::Active);
    assert_eq!(fresh["key_info"]["binding_status"], "live");
    let row = ks::lookup(home.path(), fresh_secret).unwrap().unwrap();
    assert_eq!(ks::key_binding_state(home.path(), "alice", row.created_at), ks::BindingState::Live);
}

/// Fourth review (Medium 2): an employee's migrated legacy entry carrying a
/// scope a per-employee key may not (here `admin`) is listed unbound and
/// flagged "needs re-issue"; a bound one is not.
#[tokio::test]
async fn list_flags_migrated_employee_keys_whose_scopes_need_a_reissue() {
    let (home, handler) = fixture().await;
    employee(home.path(), "sales", "Sales Bot");
    let (admin_key, fine_key) = ("ddc_prod_eeeeeeeeeeeeeeeeeeeeeeeeeeeeee01", "ddc_prod_eeeeeeeeeeeeeeeeeeeeeeeeeeeeee02");
    std::fs::write(home.path().join("config.toml"), format!(
        "[mcp_keys.{admin_key}]\nclient_id='sales'\nscopes=['admin']\nis_external=false\ncreated_at='{now}'\nlabel='sales admin'\n\n\
         [mcp_keys.{fine_key}]\nclient_id='sales'\nscopes=['memory:read']\nis_external=false\ncreated_at='{now}'\nlabel='sales fine'\n",
        now = Utc::now().to_rfc3339())).unwrap();
    crate::mcp_keys_lifecycle::migrate_config_keys(home.path()).unwrap();
    let listed = ok_payload(handler.handle("mcp_keys.list", json!({}), &admin()).await);
    let row = |name: &str| listed["keys"].as_array().unwrap().iter().find(|k| k["name"] == name).unwrap().clone();
    assert_eq!(row("sales admin")["needs_reissue"], "scopes_not_allowed_for_employee_key");
    assert!(row("sales admin")["agent_binding"].is_null());
    assert_eq!(row("sales admin")["scopes"], json!(["admin"]), "nothing dropped or downgraded");
    assert!(row("sales fine")["needs_reissue"].is_null());
    assert_eq!(row("sales fine")["agent_binding"], "sales");
    assert!(!listed.to_string().contains(admin_key) && !listed.to_string().contains(&ks::digest(admin_key)));
}

/// The audit log is a directory: no write can succeed.
fn break_audit(home: &Path) {
    let path = home.join("security_audit.jsonl");
    let _ = std::fs::remove_file(&path);
    std::fs::create_dir(&path).unwrap();
}

#[tokio::test]
async fn a_change_that_cannot_be_audited_is_not_kept_unless_it_tightens() {
    let (home, handler) = fixture().await;
    let payload = ok_payload(create(&handler, "kept", json!({ "kind": "days", "days": 30 })).await);
    let id = payload["key_info"]["id"].as_str().unwrap().to_string();
    let secret = payload["key"].as_str().unwrap().to_string();
    ok_payload(handler.handle("mcp_keys.disable", json!({ "id": id }), &admin()).await);
    break_audit(home.path());

    // Issue: never handed out.
    let frame = create(&handler, "unaudited", json!({ "kind": "days", "days": 30 })).await;
    assert_eq!(err_code(&frame), "mcp_key_audit_unavailable");
    // Loosening changes are put back.
    for (method, params) in [
        ("mcp_keys.enable", json!({ "id": id })),
        ("mcp_keys.rename", json!({ "id": id, "name": "renamed" })),
        ("mcp_keys.set_expiry", json!({ "id": id, "expiry": { "kind": "never", "confirm": true } })),
    ] {
        assert_eq!(err_code(&handler.handle(method, params, &admin()).await), "mcp_key_audit_unavailable", "{method}");
    }
    let key = ks::lookup(home.path(), &secret).unwrap().unwrap();
    assert_eq!(key.state(Utc::now()), ks::KeyState::Disabled);
    assert_eq!(key.name, "kept");
    assert!(key.expires_at.is_some());
    let names: Vec<String> = duduclaw_auth::mcp_keys::McpKeyStore::open(home.path()).unwrap()
        .list().unwrap().into_iter().map(|k| k.name).collect();
    assert_eq!(names, vec!["kept".to_string()], "the unaudited key was discarded");
    // The hint dismissal is audited too, and put back when it cannot be.
    assert_eq!(err_code(&handler.handle("mcp_keys.dismiss_migration_hint", json!({}), &admin()).await),
        "mcp_key_audit_unavailable");
    // Delete is tightening: applied even though the record failed.
    assert_eq!(err_code(&handler.handle("mcp_keys.delete", json!({ "id": id }), &admin()).await), "mcp_key_audit_unavailable");
    assert_eq!(ks::lookup(home.path(), &secret).unwrap().unwrap().state(Utc::now()), ks::KeyState::Deleted);
}

#[tokio::test]
async fn list_shows_pending_ignored_and_ineffective_scopes_and_the_hint_until_dismissed() {
    let (home, handler) = fixture().await;
    let key = "ddc_prod_cccccccccccccccccccccccccccccccc";
    std::fs::write(home.path().join("config.toml"), format!(
        "[mcp_keys.{key}]\nclient_id='claude'\nscopes=['memory:read', 'odoo:read']\nis_external=true\ncreated_at='{}'\n",
        Utc::now().to_rfc3339())).unwrap();
    // Before the gateway's startup migration the entry is waiting to move.
    let listed = ok_payload(handler.handle("mcp_keys.list", json!({}), &admin()).await);
    assert_eq!(listed["legacy_pending"], 1);
    crate::mcp_keys_lifecycle::migrate_config_keys(home.path()).unwrap();
    let listed = ok_payload(handler.handle("mcp_keys.list", json!({}), &admin()).await);
    assert_eq!(listed["legacy_pending"], 0);
    assert_eq!(listed["migration_hint"], true);
    let row = &listed["keys"][0];
    assert_eq!(row["migrated"], true);
    assert_eq!(row["expiry_reviewed"], false);
    assert!(row["created_by"].is_null());
    assert_eq!(row["ineffective_scopes"], json!(["odoo:read"]), "stored but never effective for an external key");
    assert!(!listed.to_string().contains(key));
    // An entry added after the marker is reported (by hint), never imported.
    let late = "ddc_prod_dddddddddddddddddddddddddddddddd";
    let mut text = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
    text.push_str(&format!("\n[mcp_keys.{late}]\nclient_id='late'\nscopes=['admin']\nis_external=false\ncreated_at='{}'\n",
        Utc::now().to_rfc3339()));
    std::fs::write(home.path().join("config.toml"), text).unwrap();
    let listed = ok_payload(handler.handle("mcp_keys.list", json!({}), &admin()).await);
    assert_eq!(listed["ignored_legacy_entries"], json!([ks::key_hint(late)]));
    assert!(!listed.to_string().contains(&ks::digest(late)), "hints only, never fingerprints");
    ok_payload(handler.handle("mcp_keys.dismiss_migration_hint", json!({}), &admin()).await);
    let listed = ok_payload(handler.handle("mcp_keys.list", json!({}), &admin()).await);
    assert_eq!(listed["migration_hint"], false);
    assert_eq!(audits(home.path(), "mcp_key_migration_hint_dismissed").len(), 1);
}

#[tokio::test]
async fn an_unreadable_store_fails_every_call_visibly() {
    let (home, handler) = fixture().await;
    std::fs::write(ks::store_path(home.path()), b"corrupt store bytes, not sqlite").unwrap();
    assert_eq!(err_code(&handler.handle("mcp_keys.list", json!({}), &admin()).await), "mcp_key_store_unavailable");
    assert_eq!(err_code(&create(&handler, "x", json!({ "kind": "days", "days": 30 })).await), "mcp_key_store_unavailable");
}
