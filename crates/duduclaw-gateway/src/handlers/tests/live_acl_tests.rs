//! Live ACL at the dispatch chokepoint (`live_context.rs`): the context a
//! WebSocket built at handshake is never trusted for authority. An unbinding,
//! a demotion or a suspension takes effect on the caller's next RPC, a stale
//! JWT never widens access, and a database read error narrows. Plus the
//! `manager_user_id` surface of `users.create` / `users.update` /
//! `users.list` / `users.offboard`.
use super::*;
use std::collections::HashMap;

use crate::handlers::live_context::{LiveContextCache, resolve_live_context};

fn seed_agent(home: &std::path::Path, name: &str) {
    let dir = home.join("agents").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let toml = format!(
        r#"[agent]
name = "{name}"
display_name = "{name}"
role = "specialist"
status = "active"
trigger = ""
reports_to = ""
icon = "🤖"

[model]
preferred = "claude-sonnet-4-6"
fallback = "claude-haiku-4-5"
account_pool = ["main"]

[container]
timeout_ms = 1800000
max_concurrent = 1
readonly_project = true
additional_mounts = []

[heartbeat]
enabled = false
interval_seconds = 3600
max_concurrent_runs = 1
cron = ""

[budget]
monthly_limit_cents = 5000
warn_threshold_percent = 80
hard_stop = true

[permissions]
can_create_agents = false
can_send_cross_agent = true
can_modify_own_skills = true
can_modify_own_soul = false
can_schedule_tasks = false
allowed_channels = ["*"]

[evolution]
micro_reflection = false
meso_reflection = false
macro_reflection = false
skill_auto_activate = false
skill_security_scan = true
"#
    );
    std::fs::write(dir.join("agent.toml"), toml).unwrap();
}

struct Fixture {
    handler: MethodHandler,
    db: Arc<UserDb>,
    home: tempfile::TempDir,
}

async fn fixture() -> Fixture {
    let home = tempfile::tempdir().unwrap();
    for agent in ["sales", "hr", "finance"] {
        seed_agent(home.path(), agent);
    }
    let db = Arc::new(UserDb::new(&home.path().join("users.db")).unwrap());
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let jwt = Arc::new(JwtConfig::new(b"test-secret-at-least-32-bytes-long!!"));
    handler.set_user_db(db.clone(), jwt).await;
    handler
        .set_edition_override(Some(duduclaw_core::EditionProfile::Enterprise))
        .await;
    Fixture { handler, db, home }
}

fn person(db: &UserDb, name: &str, role: UserRole) -> String {
    db.create_user(&format!("{name}@test.invalid"), name, "isolated-test-password", role)
        .unwrap()
        .id
}

/// What `authenticate_jwt` builds at handshake: claims as they were then.
fn handshake_ctx(user_id: &str, role: UserRole, access: &[(&str, AccessLevel)]) -> UserContext {
    UserContext {
        credential_epoch: 0,
        user_id: user_id.to_string(),
        email: format!("{user_id}@test.invalid"),
        role,
        agent_access: access.iter().map(|(a, l)| (a.to_string(), *l)).collect(),
        must_change_password: false,
        team_access: HashMap::new(),
        acl_generation: 0,
    }
}

fn ok_payload(frame: &WsFrame) -> Value {
    match frame {
        WsFrame::Response { ok: true, payload: Some(p), .. } => p.clone(),
        other => panic!("expected ok response, got {other:?}"),
    }
}

fn error_of(frame: &WsFrame) -> Value {
    match frame {
        WsFrame::Response { ok: false, error: Some(e), .. } => e.clone(),
        other => panic!("expected error response, got {other:?}"),
    }
}

fn listed_agents(frame: &WsFrame) -> Vec<String> {
    let mut names: Vec<String> = ok_payload(frame)["agents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["name"].as_str().unwrap().to_string())
        .collect();
    names.sort();
    names
}

// ── live ACL ─────────────────────────────────────────────────────────

#[tokio::test]
async fn unbinding_reaches_an_open_connection_on_the_next_rpc() {
    let f = fixture().await;
    let emp = person(&f.db, "emp", UserRole::Employee);
    f.db.bind_agent(&emp, "sales", AccessLevel::Owner).unwrap();
    f.db.bind_agent(&emp, "hr", AccessLevel::Owner).unwrap();
    let ctx = handshake_ctx(&emp, UserRole::Employee, &[("sales", AccessLevel::Owner), ("hr", AccessLevel::Owner)]);

    let before = f.handler.handle("agents.list", json!({}), &ctx).await;
    assert_eq!(listed_agents(&before), vec!["hr", "sales"]);

    f.db.unbind_agent(&emp, "hr").unwrap();
    // Same connection, same handshake context, no re-login.
    let after = f.handler.handle("agents.list", json!({}), &ctx).await;
    assert_eq!(listed_agents(&after), vec!["sales"]);
    let denied = f
        .handler
        .handle("agents.avatar", json!({"agent_id": "hr"}), &ctx)
        .await;
    assert!(matches!(denied, WsFrame::Response { ok: false, .. }), "{denied:?}");
}

#[tokio::test]
async fn demotion_and_suspension_apply_without_relogin() {
    let f = fixture().await;
    let boss = person(&f.db, "boss", UserRole::Admin);
    let ctx = handshake_ctx(&boss, UserRole::Admin, &[]);
    ok_payload(&f.handler.handle("users.list", json!({}), &ctx).await);

    f.db.update_user(&boss, None, Some(UserRole::Employee), None).unwrap();
    let demoted = f.handler.handle("users.list", json!({}), &ctx).await;
    assert_eq!(error_of(&demoted), json!("permission denied"));

    f.db.set_user_status(&boss, duduclaw_auth::UserStatus::Suspended).unwrap();
    let suspended = f.handler.handle("agents.list", json!({}), &ctx).await;
    assert_eq!(error_of(&suspended), json!("account is suspended or offboarded"));
}

#[tokio::test]
async fn stale_claims_never_widen_and_unknown_users_are_refused() {
    let f = fixture().await;
    let emp = person(&f.db, "emp", UserRole::Employee);
    // The token claims Admin and a finance binding; the database says neither.
    let forged = handshake_ctx(&emp, UserRole::Admin, &[("finance", AccessLevel::Owner)]);
    assert_eq!(
        error_of(&f.handler.handle("users.list", json!({}), &forged).await),
        json!("permission denied")
    );
    assert!(listed_agents(&f.handler.handle("agents.list", json!({}), &forged).await).is_empty());

    let ghost = handshake_ctx("deleted-user", UserRole::Admin, &[]);
    assert_eq!(
        error_of(&f.handler.handle("agents.list", json!({}), &ghost).await),
        json!("account is suspended or offboarded")
    );
}

#[tokio::test]
async fn a_database_read_error_refuses_the_call_and_is_counted() {
    let f = fixture().await;
    let boss = person(&f.db, "boss", UserRole::Admin);
    f.db.bind_agent(&boss, "sales", AccessLevel::Owner).unwrap();
    let ctx = handshake_ctx(&boss, UserRole::Admin, &[("sales", AccessLevel::Owner)]);
    ok_payload(&f.handler.handle("users.list", json!({}), &ctx).await);
    let (failures_before, _) = crate::handlers::live_context::read_failure_stats();

    // Break the bindings table behind the gateway's back.
    let raw = rusqlite::Connection::open(f.home.path().join("users.db")).unwrap();
    raw.execute_batch("DROP TABLE user_agent_bindings").unwrap();
    raw.execute(
        "UPDATE auth_meta SET value = value + 1 WHERE key = 'acl_generation'",
        [],
    )
    .unwrap();
    drop(raw);

    // Nothing from the token is reused: not the role, not the bindings, and
    // not even the self-service allowlist (the store cannot prove the
    // forced-password-change flag is clear).
    for method in ["users.list", "agents.list", "users.me", "ping"] {
        assert_eq!(
            error_of(&f.handler.handle(method, json!({}), &ctx).await),
            json!("authentication service unavailable"),
            "{method}"
        );
    }
    let (failures_after, last) = crate::handlers::live_context::read_failure_stats();
    assert!(failures_after >= failures_before + 4, "{failures_before} → {failures_after}");
    assert!(last > 0);
}

#[tokio::test]
async fn the_forced_password_change_flag_is_live() {
    let f = fixture().await;
    let pw = f.db.ensure_default_admin().unwrap().expect("bootstrap admin");
    let owner = f.db.get_bootstrap_owner().unwrap().unwrap();
    let mut ctx = handshake_ctx(&owner.id, UserRole::Admin, &[]);
    ctx.must_change_password = true;
    let blocked = f.handler.handle("agents.list", json!({}), &ctx).await;
    assert_eq!(error_of(&blocked)["code"], MUST_CHANGE_PASSWORD_ERROR_CODE);

    ok_payload(
        &f.handler
            .handle(
                "users.change_password",
                json!({"current_password": pw, "new_password": "a-Str0ng-New-Passw0rd!"}),
                &ctx,
            )
            .await,
    );
    // The handshake snapshot still says "flagged"; the database does not.
    ok_payload(&f.handler.handle("agents.list", json!({}), &ctx).await);

    // The other direction: a context that thinks it is clear, for an account
    // the database still flags, stays restricted.
    let f2 = fixture().await;
    f2.db.ensure_default_admin().unwrap().expect("bootstrap admin");
    let owner2 = f2.db.get_bootstrap_owner().unwrap().unwrap();
    let clear = handshake_ctx(&owner2.id, UserRole::Admin, &[]);
    let still = f2.handler.handle("agents.list", json!({}), &clear).await;
    assert_eq!(error_of(&still)["code"], MUST_CHANGE_PASSWORD_ERROR_CODE);
}

#[tokio::test]
async fn system_and_unwired_handlers_keep_the_passed_context() {
    let f = fixture().await;
    person(&f.db, "someone", UserRole::Employee);
    let system = UserContext::admin_fallback();
    ok_payload(&f.handler.handle("users.list", json!({}), &system).await);

    // No user database wired in: nothing to refresh from (no JWT session can
    // exist without one).
    let home = tempfile::tempdir().unwrap();
    seed_agent(home.path(), "sales");
    let bare = MethodHandler::new(home.path().to_path_buf()).await;
    let emp = handshake_ctx("emp", UserRole::Employee, &[("sales", AccessLevel::Owner)]);
    assert_eq!(listed_agents(&bare.handle("agents.list", json!({}), &emp).await), vec!["sales"]);
}

#[test]
fn the_cache_is_keyed_by_generation() {
    let home = tempfile::tempdir().unwrap();
    let db = UserDb::new(&home.path().join("users.db")).unwrap();
    let emp = person(&db, "emp", UserRole::Employee);
    db.bind_agent(&emp, "sales", AccessLevel::Owner).unwrap();
    let cache = LiveContextCache::default();
    let jwt = handshake_ctx(&emp, UserRole::Employee, &[]);

    let first = resolve_live_context(&db, &cache, &jwt).unwrap();
    assert!(first.agent_access.contains_key("sales"));
    assert_eq!(cache.len(), 1);

    // A write that bypasses UserDb (and so the bump) is not seen: proves the
    // cached entry is what answered.
    let raw = rusqlite::Connection::open(home.path().join("users.db")).unwrap();
    raw.execute("DELETE FROM user_agent_bindings", []).unwrap();
    let cached = resolve_live_context(&db, &cache, &jwt).unwrap();
    assert!(cached.agent_access.contains_key("sales"));

    // Any UserDb write bumps the generation and the next call re-reads.
    db.bind_agent(&emp, "hr", AccessLevel::Viewer).unwrap();
    let fresh = resolve_live_context(&db, &cache, &jwt).unwrap();
    assert_eq!(fresh.agent_access.keys().collect::<Vec<_>>(), vec!["hr"]);
    assert_eq!(fresh.acl_generation, db.acl_generation().unwrap());

    // An unreadable generation bypasses the cache instead of trusting it,
    // and the read failure that follows refuses.
    raw.execute("DELETE FROM auth_meta", []).unwrap();
    assert_eq!(
        resolve_live_context(&db, &cache, &jwt).unwrap_err(),
        "authentication service unavailable"
    );
}

#[test]
fn team_grants_reach_the_live_context_but_not_agent_access() {
    let home = tempfile::tempdir().unwrap();
    let db = UserDb::new(&home.path().join("users.db")).unwrap();
    let mgr = person(&db, "mgr", UserRole::Manager);
    let emp = person(&db, "emp", UserRole::Employee);
    db.set_manager(&emp, Some(&mgr)).unwrap();
    db.bind_agent(&emp, "sales", AccessLevel::Owner).unwrap();
    let live = resolve_live_context(&db, &LiveContextCache::default(), &handshake_ctx(&mgr, UserRole::Manager, &[]))
        .unwrap();
    assert!(live.can_call_agent("sales") && live.can_assign_to("sales"));
    assert!(!live.has_agent_access("sales", AccessLevel::Viewer), "memory/settings RPCs stay direct-only");
    assert!(live.visible_agents().unwrap().is_empty(), "agents.list stays direct-only in this release");
}

// ── users.* manager_user_id ──────────────────────────────────────────

#[tokio::test]
async fn users_rpcs_set_clear_and_list_the_manager() {
    let f = fixture().await;
    let admin_id = person(&f.db, "admin", UserRole::Admin);
    let admin = handshake_ctx(&admin_id, UserRole::Admin, &[]);
    let mgr = person(&f.db, "mgr", UserRole::Manager);

    let created = ok_payload(
        &f.handler
            .handle(
                "users.create",
                json!({"email": "lead@test.invalid", "display_name": "lead", "password": "lead-password-1",
                       "role": "manager", "manager_user_id": mgr}),
                &admin,
            )
            .await,
    );
    let lead = created["user"]["id"].as_str().unwrap().to_string();
    assert_eq!(created["user"]["manager_user_id"], json!(mgr));

    let emp1 = person(&f.db, "emp1", UserRole::Employee);
    ok_payload(&f.handler.handle("users.update", json!({"user_id": emp1, "manager_user_id": lead}), &admin).await);

    let list = ok_payload(&f.handler.handle("users.list", json!({}), &admin).await);
    let manager_of = |id: &str| {
        list["users"].as_array().unwrap().iter().find(|u| u["id"] == id).unwrap()["manager_user_id"].clone()
    };
    assert_eq!(manager_of(&lead), json!(mgr));
    assert_eq!(manager_of(&emp1), json!(lead));
    assert_eq!(manager_of(&mgr), Value::Null);

    // Absent key leaves it; "" and null clear it.
    ok_payload(&f.handler.handle("users.update", json!({"user_id": emp1, "display_name": "Emp 1"}), &admin).await);
    assert_eq!(f.db.get_user(&emp1).unwrap().unwrap().manager_user_id, Some(lead.clone()));
    ok_payload(&f.handler.handle("users.update", json!({"user_id": emp1, "manager_user_id": ""}), &admin).await);
    assert_eq!(f.db.get_user(&emp1).unwrap().unwrap().manager_user_id, None);
    ok_payload(&f.handler.handle("users.update", json!({"user_id": emp1, "manager_user_id": lead}), &admin).await);
    ok_payload(&f.handler.handle("users.update", json!({"user_id": emp1, "manager_user_id": null}), &admin).await);
    assert_eq!(f.db.get_user(&emp1).unwrap().unwrap().manager_user_id, None);

    let audit = f.db.query_audit_log(Some(&admin_id), Some("user.set_manager"), 20).unwrap();
    assert!(audit.len() >= 4, "{audit:?}");
}

#[tokio::test]
async fn users_rpcs_return_coded_manager_errors() {
    let f = fixture().await;
    let admin = handshake_ctx(&person(&f.db, "admin", UserRole::Admin), UserRole::Admin, &[]);
    let mgr = person(&f.db, "mgr", UserRole::Manager);
    let lead = person(&f.db, "lead", UserRole::Manager);
    let emp1 = person(&f.db, "emp1", UserRole::Employee);
    f.db.set_manager(&lead, Some(&mgr)).unwrap();
    f.db.set_manager(&emp1, Some(&lead)).unwrap();

    let code = |frame: WsFrame| error_of(&frame)["code"].clone();
    // mgr under lead closes a loop.
    assert_eq!(
        code(f.handler.handle("users.update", json!({"user_id": mgr, "manager_user_id": lead}), &admin).await),
        json!("manager_cycle")
    );
    assert_eq!(
        code(f.handler.handle("users.update", json!({"user_id": mgr, "manager_user_id": mgr}), &admin).await),
        json!("manager_cycle")
    );
    // Below lead, but an Employee: the role check answers first.
    assert_eq!(
        code(f.handler.handle("users.update", json!({"user_id": lead, "manager_user_id": emp1}), &admin).await),
        json!("manager_role")
    );
    let emp2 = person(&f.db, "emp2", UserRole::Employee);
    assert_eq!(
        code(f.handler.handle("users.update", json!({"user_id": emp2, "manager_user_id": emp1}), &admin).await),
        json!("manager_role")
    );
    assert_eq!(
        code(
            f.handler
                .handle(
                    "users.create",
                    json!({"email": "x@test.invalid", "display_name": "x", "password": "x-password-1",
                           "manager_user_id": emp1}),
                    &admin,
                )
                .await
        ),
        json!("manager_role")
    );
    assert!(f.db.get_user_by_email("x@test.invalid").unwrap().is_none(), "no half-created account");

    // A refused manager in a request that also renames: nothing applies.
    let refused = f
        .handler
        .handle("users.update", json!({"user_id": mgr, "display_name": "renamed", "manager_user_id": lead}), &admin)
        .await;
    assert_eq!(error_of(&refused)["code"], json!("manager_cycle"));
    assert_eq!(f.db.get_user(&mgr).unwrap().unwrap().display_name, "mgr");

    // Demotion with a direct report: refused, and nothing else in the
    // request applies.
    let refused = f
        .handler
        .handle("users.update", json!({"user_id": lead, "role": "employee", "display_name": "renamed"}), &admin)
        .await;
    assert_eq!(error_of(&refused)["code"], json!("manager_has_reports"));
    let lead_row = f.db.get_user(&lead).unwrap().unwrap();
    assert_eq!((lead_row.role, lead_row.display_name.as_str()), (UserRole::Manager, "lead"));

    // Too deep: an eight-person chain cannot take a ninth below it.
    let mut above = mgr.clone();
    for i in 0..7 {
        let next = person(&f.db, &format!("c{i}"), UserRole::Manager);
        f.db.set_manager(&next, Some(&above)).unwrap();
        above = next;
    }
    assert_eq!(
        code(f.handler.handle("users.update", json!({"user_id": emp2, "manager_user_id": above}), &admin).await),
        json!("manager_too_deep")
    );
}

#[tokio::test]
async fn offboard_reparents_and_reports_it() {
    let f = fixture().await;
    let admin_id = person(&f.db, "admin", UserRole::Admin);
    let admin = handshake_ctx(&admin_id, UserRole::Admin, &[]);
    let mgr = person(&f.db, "mgr", UserRole::Manager);
    let lead = person(&f.db, "lead", UserRole::Manager);
    let emp1 = person(&f.db, "emp1", UserRole::Employee);
    f.db.set_manager(&lead, Some(&mgr)).unwrap();
    f.db.set_manager(&emp1, Some(&lead)).unwrap();
    f.db.bind_agent(&lead, "sales", AccessLevel::Owner).unwrap();

    let out = ok_payload(&f.handler.handle("users.offboard", json!({"user_id": lead}), &admin).await);
    assert_eq!(out["reparented"], json!([{"user_id": emp1, "manager_user_id": mgr}]));
    assert_eq!(out["transferred_agents"], json!([]));
    assert_eq!(f.db.get_user(&emp1).unwrap().unwrap().manager_user_id, Some(mgr.clone()));
    assert_eq!(f.db.query_audit_log(Some(&admin_id), Some("user.reparent"), 5).unwrap().len(), 1);

    // With transfer_to: an Employee target cannot take reports.
    let emp2 = person(&f.db, "emp2", UserRole::Employee);
    let refused = f
        .handler
        .handle("users.offboard", json!({"user_id": mgr, "transfer_to": emp2}), &admin)
        .await;
    assert_eq!(error_of(&refused)["code"], json!("manager_role"));
    assert_eq!(f.db.get_user(&mgr).unwrap().unwrap().status, duduclaw_auth::UserStatus::Active);

    let heir = person(&f.db, "heir", UserRole::Manager);
    let out = ok_payload(&f.handler.handle("users.offboard", json!({"user_id": mgr, "transfer_to": heir}), &admin).await);
    assert_eq!(out["reparented"], json!([{"user_id": emp1, "manager_user_id": heir}]));
}

#[tokio::test]
async fn manager_fields_stay_behind_the_edition_gate() {
    let f = fixture().await;
    f.handler
        .set_edition_override(Some(duduclaw_core::EditionProfile::Personal))
        .await;
    let admin = handshake_ctx(&person(&f.db, "admin", UserRole::Admin), UserRole::Admin, &[]);
    let emp = person(&f.db, "emp", UserRole::Employee);
    let frame = f
        .handler
        .handle("users.update", json!({"user_id": emp, "manager_user_id": "x"}), &admin)
        .await;
    assert_eq!(error_of(&frame)["code"], json!(ENTERPRISE_ONLY_ERROR_CODE));
    // agents.list is not edition-gated and still answers (live) in Personal.
    ok_payload(&f.handler.handle("agents.list", json!({}), &admin).await);
}

// ── latency (H1): run with `--ignored --nocapture` ──────────────────

fn percentile(sorted: &[std::time::Duration], p: f64) -> std::time::Duration {
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx]
}

/// N cheap RPCs (`ping`, which still pays the live identity read) on a
/// two-worker runtime while two logins hash passwords in a loop. `inline`
/// reproduces the old login handler (argon2 on a runtime worker); the other
/// mode is the current one (blocking pool). Prints p50 / p95 / max.
async fn rpc_latency_under_logins(mode: &str, rpcs: usize) -> (std::time::Duration, std::time::Duration, std::time::Duration) {
    let f = fixture().await;
    let emp = person(&f.db, "emp", UserRole::Employee);
    f.db.bind_agent(&emp, "sales", AccessLevel::Owner).unwrap();
    let ctx = handshake_ctx(&emp, UserRole::Employee, &[]);
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut logins = Vec::new();
    if mode != "none" {
        for _ in 0..2 {
            let db = f.db.clone();
            let stop = stop.clone();
            let inline = mode == "inline";
            logins.push(tokio::spawn(async move {
                let mut n = 0u32;
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    if inline {
                        let _ = db.verify_password("emp@test.invalid", "isolated-test-password");
                    } else {
                        let db = db.clone();
                        let _ = tokio::task::spawn_blocking(move || {
                            db.verify_password("emp@test.invalid", "isolated-test-password")
                        })
                        .await;
                    }
                    n += 1;
                    tokio::task::yield_now().await;
                }
                n
            }));
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    // The RPC loop runs as a spawned task, i.e. on the same two runtime
    // workers the login tasks use, and paces itself so the run overlaps many
    // password hashes. Every 10th call follows a binding change, so it pays
    // the full live rebuild instead of a cache hit.
    let handler = Arc::new(f.handler);
    let rpc_task = {
        let handler = handler.clone();
        let db = f.db.clone();
        let emp = emp.clone();
        tokio::spawn(async move {
            let mut samples = Vec::with_capacity(rpcs);
            for i in 0..rpcs {
                if i % 10 == 0 {
                    let db = db.clone();
                    let emp = emp.clone();
                    tokio::task::spawn_blocking(move || db.bind_agent(&emp, "hr", AccessLevel::Viewer))
                        .await
                        .unwrap()
                        .unwrap();
                }
                let t = std::time::Instant::now();
                let frame = handler.handle("ping", json!({}), &ctx).await;
                samples.push(t.elapsed());
                ok_payload(&frame);
                tokio::time::sleep(std::time::Duration::from_millis(2)).await;
            }
            samples
        })
    };
    let mut samples = rpc_task.await.unwrap();
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let mut hashed = 0;
    for l in logins {
        hashed += l.await.unwrap();
    }
    samples.sort();
    let (p50, p95, max) = (percentile(&samples, 0.5), percentile(&samples, 0.95), *samples.last().unwrap());
    println!("mode={mode:<8} rpcs={rpcs} logins_hashed={hashed} p50={p50:?} p95={p95:?} max={max:?}");
    (p50, p95, max)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "latency measurement; run explicitly"]
async fn live_acl_latency_with_two_concurrent_logins() {
    let n = 300;
    rpc_latency_under_logins("none", n).await;
    let (_, p95, _) = rpc_latency_under_logins("blocking", n).await;
    rpc_latency_under_logins("inline", n).await;
    assert!(p95 < std::time::Duration::from_millis(250), "p95 {p95:?} with logins on the blocking pool");
}
