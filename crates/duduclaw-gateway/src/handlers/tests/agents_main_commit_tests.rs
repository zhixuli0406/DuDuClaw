//! Main/default promotion after RPC admission must fence the locked write.
use super::agent_files_rpc_tests::{err, fixture, ok, Fx};
use super::*;

async fn prepare(f: &Fx) -> UserContext {
    ok(&f.call("agents.update", json!({
        "agent_id": "a1", "role": "specialist", "status": "paused",
    }), &f.admin).await);
    std::fs::write(f.home.join("config.toml"), "[general]\ndefault_agent = \"b1\"\n").unwrap();
    rusqlite::Connection::open(f.home.join("users.db")).unwrap()
        .execute("UPDATE users SET role='manager' WHERE id=?1", [&f.owner_a.user_id]).unwrap();
    UserDb::new(&f.home.join("users.db")).unwrap()
        .live_user_context(&f.owner_a.user_id).unwrap().unwrap()
}

fn params(method: &str) -> Value {
    if method == "agents.update" {
        // Two unrelated fields prove refusal covers the whole update.
        json!({"agent_id": "a1", "display_name": "Changed", "status": "active"})
    } else {
        json!({"agent_id": "a1"})
    }
}

async fn race(method: &str, promote_role: bool, stale_admin: bool) {
    let f = fixture().await;
    let manager = prepare(&f).await;
    let who = if stale_admin { &f.admin } else { &manager };
    let path = f.home.join("agents/a1/agent.toml");
    let lock = crate::agent_files::file_mutex(&f.home, "a1", &crate::agent_files::FileKey::AgentToml);
    let held = lock.lock().await;
    let reached = super::super::agents_ops::main_commit_test_seam::arm(&f.home);
    let mut pending = Box::pin(f.call(method, params(method), who));
    // Unlike yielding once, this proves the asynchronous early admission
    // finished before the promotion, while the existing writer mutex is held.
    tokio::select! {
        response = &mut pending => panic!("write did not wait for the lock: {response:?}"),
        result = tokio::time::timeout(std::time::Duration::from_secs(5), reached) => {
            result.expect("RPC did not reach the commit seam").expect("seam closed");
        },
    }
    if promote_role {
        let mut table: toml::Table = std::fs::read_to_string(&path).unwrap().parse().unwrap();
        table["agent"].as_table_mut().unwrap().insert("role".into(), "main".into());
        std::fs::write(&path, toml::to_string(&table).unwrap()).unwrap();
    } else {
        std::fs::write(f.home.join("config.toml"), "[general]\ndefault_agent = \"a1\"\n").unwrap();
    }
    if stale_admin {
        // Keep valid ownership and Manager role so only the fresh main gate
        // refuses: a stale cached Admin must not bypass it.
        let db = UserDb::new(&f.home.join("users.db")).unwrap();
        db.bind_agent(&who.user_id, "a1", AccessLevel::Owner).unwrap();
        rusqlite::Connection::open(f.home.join("users.db")).unwrap()
            .execute("UPDATE users SET role='manager' WHERE id=?1", [&who.user_id]).unwrap();
    }
    let promoted = std::fs::read(&path).unwrap();
    let soul = std::fs::read(f.home.join("agents/a1/SOUL.md")).ok();
    let identity = std::fs::read(f.home.join("agents/a1/IDENTITY.md")).ok();
    drop(held);
    let response = tokio::time::timeout(std::time::Duration::from_secs(5), pending)
        .await.expect("RPC did not finish after unlocking");
    assert!(err(&response).contains("主代理僅管理員可以使用。"), "{response:?}");
    assert_eq!(std::fs::read(&path).unwrap(), promoted, "whole update must be refused");
    assert_eq!(std::fs::read(f.home.join("agents/a1/SOUL.md")).ok(), soul);
    assert_eq!(std::fs::read(f.home.join("agents/a1/IDENTITY.md")).ok(), identity);
    // The Admin path still works after a promotion.
    ok(&f.call(method, params(method), &UserContext::admin_fallback()).await);
}

#[tokio::test]
async fn update_refuses_main_or_default_promoted_after_early_admission() {
    for role in [true, false] { race("agents.update", role, false).await; }
}

#[tokio::test]
async fn resume_refuses_main_or_default_promoted_after_early_admission() {
    for role in [true, false] { race("agents.resume", role, false).await; }
}

#[tokio::test]
async fn update_and_resume_recheck_live_admin_role_inside_commit_guard() {
    for method in ["agents.update", "agents.resume"] {
        for role in [true, false] { race(method, role, true).await; }
    }
}

#[tokio::test]
async fn ordinary_target_update_and_resume_remain_available() {
    let f = fixture().await;
    let manager = prepare(&f).await;
    for method in ["agents.update", "agents.resume"] {
        ok(&f.call(method, params(method), &manager).await);
    }
}
