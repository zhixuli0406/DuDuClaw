//! B8 — per-employee skill switches, removal and the permission-filtered
//! `skills.list`, end to end through `MethodHandler::handle` with a real
//! `users.db`. Explicit legacy personal copies keep removal/restore coverage
//! independent of the shared defaults used for new employees.
use super::*;

fn ok(frame: &WsFrame) -> Value {
    match frame {
        WsFrame::Response { ok: true, payload: Some(p), .. } => p.clone(),
        other => panic!("expected ok, got {other:?}"),
    }
}

fn err(frame: &WsFrame) -> String {
    match frame {
        WsFrame::Response { ok: false, error: Some(e), .. } => e.as_str().unwrap_or_default().to_string(),
        other => panic!("expected error, got {other:?}"),
    }
}

fn audit_rows(home: &Path, event: &str) -> Vec<Value> {
    std::fs::read_to_string(home.join("security_audit.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v["event_type"] == event)
        .collect()
}

fn ctx(id: &str, role: UserRole, access: &[(&str, AccessLevel)]) -> UserContext {
    UserContext {
        credential_epoch: 0,
        user_id: id.into(),
        email: format!("{id}@example.invalid"),
        role,
        agent_access: access.iter().map(|(a, l)| (a.to_string(), *l)).collect(),
        must_change_password: false,
        team_access: Default::default(),
        acl_generation: 0,
    }
}

struct Fx {
    _dir: tempfile::TempDir,
    home: PathBuf,
    h: MethodHandler,
    admin: UserContext,
    owner_a: UserContext,
    viewer_a: UserContext,
}

async fn fixture() -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().canonicalize().unwrap();
    // Simulate a legacy installation whose default migration already ran.
    std::fs::create_dir_all(home.join("migrations")).unwrap();
    for marker in ["wp19-builtin-skills-seed.done", "g1-connect-google-seed-v1.done"] {
        std::fs::write(home.join("migrations").join(marker), "done").unwrap();
    }
    let h = MethodHandler::new(home.clone()).await;
    let system = UserContext::admin_fallback();
    for (id, name) in [("a1", "Alice"), ("b1", "Bob")] {
        ok(&h.handle_agents_create_as(json!({ "name": id, "display_name": name }), Some(&system)).await);
        duduclaw_agent::builtin_skills::install_builtin_skills(&home.join("agents").join(id).join("SKILLS")).unwrap();
    }
    // A company-wide skill both employees load.
    std::fs::create_dir_all(home.join("skills")).unwrap();
    std::fs::write(home.join("skills/tone.md"), "---\nname: tone\ndescription: company voice\n---\nBe kind.\n").unwrap();
    h.registry.write().await.scan().await.unwrap();
    let db = duduclaw_auth::UserDb::new(&home.join("users.db")).unwrap();
    let add = |id: &str, role: UserRole, binds: &[(&str, AccessLevel)]| {
        let u = db.create_user(&format!("{id}@example.invalid"), id, "isolated-test-password", role).unwrap();
        rusqlite::Connection::open(home.join("users.db"))
            .unwrap()
            .execute("UPDATE users SET id=?1 WHERE id=?2", rusqlite::params![id, u.id])
            .unwrap();
        for (a, l) in binds {
            db.bind_agent(id, a, *l).unwrap();
        }
        ctx(id, role, binds)
    };
    let admin = add("admin-1", UserRole::Admin, &[]);
    let owner_a = add("owner-a", UserRole::Employee, &[("a1", AccessLevel::Owner)]);
    let viewer_a = add("viewer-a", UserRole::Employee, &[("a1", AccessLevel::Viewer)]);
    Fx { _dir: dir, home, h, admin, owner_a, viewer_a }
}

impl Fx {
    async fn call(&self, method: &str, params: Value, who: &UserContext) -> WsFrame {
        Box::pin(self.h.handle(method, params, who)).await
    }

    async fn loaded(&self, agent: &str) -> Vec<String> {
        let reg = self.h.registry.read().await;
        reg.get(agent).unwrap().skills.iter().map(|s| s.name.clone()).collect()
    }

    async fn listed(&self, agent: &str) -> Vec<Value> {
        let p = ok(&self.call("skills.list", json!({ "agent_id": agent }), &self.admin).await);
        p["skills"].as_array().unwrap().clone()
    }
}

fn item(agent: &str, skill: &str) -> Value {
    json!({ "agent_id": agent, "skill_name": skill })
}

fn entry<'a>(skills: &'a [Value], name: &str) -> &'a Value {
    skills.iter().find(|s| s["name"] == name).unwrap_or_else(|| panic!("{name} not listed"))
}

#[tokio::test]
async fn switching_off_narrows_one_employee_only() {
    let f = fixture().await;
    assert!(f.loaded("a1").await.contains(&"pdf".to_string()));
    let r = ok(&f
        .call("skills.set_enabled", json!({ "items": [item("a1", "pdf"), item("a1", "tone")], "enabled": false }), &f.owner_a)
        .await);
    assert!(r["results"].as_array().unwrap().iter().all(|x| x["ok"] == true), "{r}");
    // Not loaded for a1 (own layer and company-wide alike), still loaded for b1.
    let a1 = f.loaded("a1").await;
    assert!(!a1.contains(&"pdf".to_string()) && !a1.contains(&"tone".to_string()), "{a1:?}");
    let b1 = f.loaded("b1").await;
    assert!(b1.contains(&"pdf".to_string()) && b1.contains(&"tone".to_string()), "{b1:?}");
    // The dashboard still lists it, marked off, so it can be switched back on.
    let listed = f.listed("a1").await;
    assert_eq!(entry(&listed, "pdf")["enabled"], false);
    assert_eq!(entry(&f.listed("b1").await, "pdf")["enabled"], true);
    assert_eq!(audit_rows(&f.home, "skill_switched_off").len(), 1);

    ok(&f.call("skills.set_enabled", json!({ "items": [item("a1", "pdf")], "enabled": true }), &f.owner_a).await);
    assert!(f.loaded("a1").await.contains(&"pdf".to_string()));
    assert_eq!(audit_rows(&f.home, "skill_switched_on").len(), 1);
}

#[tokio::test]
async fn changes_need_owner_and_mixed_requests_are_refused_whole() {
    let f = fixture().await;
    let e = err(&f.call("skills.set_enabled", json!({ "items": [item("a1", "pdf")], "enabled": false }), &f.viewer_a).await);
    assert!(e.contains("permission"), "{e}");
    // owner_a owns a1 but not b1: nothing changes, not even for a1.
    let e = err(&f
        .call("skills.set_enabled", json!({ "items": [item("a1", "pdf"), item("b1", "pdf")], "enabled": false }), &f.owner_a)
        .await);
    assert!(e.contains("permission"), "{e}");
    assert!(f.loaded("a1").await.contains(&"pdf".to_string()));
    let e = err(&f.call("skills.remove", json!({ "items": [item("b1", "pdf")] }), &f.owner_a).await);
    assert!(e.contains("permission"), "{e}");
    // An admin may change several employees at once.
    ok(&f
        .call("skills.set_enabled", json!({ "items": [item("a1", "pdf"), item("b1", "pdf")], "enabled": false }), &f.admin)
        .await);
    assert!(!f.loaded("b1").await.contains(&"pdf".to_string()));
}

#[tokio::test]
async fn list_is_filtered_to_visible_employees() {
    let f = fixture().await;
    let all = ok(&f.call("skills.list", json!({}), &f.viewer_a).await);
    let ids: Vec<&str> = all["agents"].as_array().unwrap().iter().map(|a| a["agent_id"].as_str().unwrap()).collect();
    assert_eq!(ids, vec!["a1"]);
    assert_eq!(all["agents"][0]["can_manage"], false);
    let tone = &all["global_skills"][0];
    assert_eq!(tone["name"], "tone");
    assert_eq!(tone["description"], "company voice");
    let holders: Vec<&str> = tone["holders"].as_array().unwrap().iter().map(|h| h["agent_id"].as_str().unwrap()).collect();
    assert_eq!(holders, vec!["a1"]);
    let e = err(&f.call("skills.list", json!({ "agent_id": "b1" }), &f.viewer_a).await);
    assert!(e.contains("permission"), "{e}");
    // Admin: both employees, in name order, each with the same-named seeded skills.
    let all = ok(&f.call("skills.list", json!({}), &f.admin).await);
    let ids: Vec<&str> = all["agents"].as_array().unwrap().iter().map(|a| a["agent_id"].as_str().unwrap()).collect();
    assert_eq!(ids, vec!["a1", "b1"]);
    assert_eq!(all["global_skills"][0]["holders"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn remove_archives_own_skills_and_restore_brings_them_back() {
    let f = fixture().await;
    let r = ok(&f.call("skills.remove", json!({ "items": [item("a1", "pdf"), item("a1", "tone")] }), &f.owner_a).await);
    let results = r["results"].as_array().unwrap();
    assert_eq!(results[0]["ok"], true, "{r}");
    // A company-wide skill is not this employee's own: refused, not removed.
    assert_eq!(results[1]["ok"], false, "{r}");
    assert!(f.home.join("skills/tone.md").exists());
    assert!(!f.home.join("agents/a1/SKILLS/pdf").exists());
    assert!(f.home.join("agents/b1/SKILLS/pdf").exists(), "b1's same-named skill is untouched");
    assert!(!f.loaded("a1").await.contains(&"pdf".to_string()));
    assert_eq!(audit_rows(&f.home, "skill_removed").len(), 1);

    let arch = ok(&f.call("skills.archived", json!({ "agent_id": "a1" }), &f.viewer_a).await);
    assert_eq!(arch["can_restore"], false);
    let id = arch["archived"][0]["archive_id"].as_str().unwrap().to_string();
    assert_eq!(arch["archived"][0]["skill_name"], "pdf");
    let e = err(&f.call("skills.restore", json!({ "agent_id": "a1", "archive_id": id }), &f.viewer_a).await);
    assert!(e.contains("permission"), "{e}");
    ok(&f.call("skills.restore", json!({ "agent_id": "a1", "archive_id": id }), &f.owner_a).await);
    assert!(f.home.join("agents/a1/SKILLS/pdf/SKILL.md").exists());
    assert!(f.loaded("a1").await.contains(&"pdf".to_string()));
    assert_eq!(audit_rows(&f.home, "skill_restored").len(), 1);
}

#[tokio::test]
async fn unreadable_switch_file_loads_nothing_and_is_reported() {
    let f = fixture().await;
    std::fs::create_dir_all(f.home.join("skill_state")).unwrap();
    std::fs::write(f.home.join("skill_state/a1.json"), "{ broken").unwrap();
    f.h.registry.write().await.scan().await.unwrap();
    assert!(f.loaded("a1").await.is_empty());
    let p = ok(&f.call("skills.list", json!({ "agent_id": "a1" }), &f.admin).await);
    assert_eq!(p["skill_state"]["ok"], false);
    assert!(p["skills"].as_array().unwrap().iter().all(|s| s["enabled"] == false));
    // Writes refuse rather than replace the switches the file held.
    let r = ok(&f.call("skills.set_enabled", json!({ "items": [item("a1", "pdf")], "enabled": true }), &f.admin).await);
    assert_eq!(r["results"][0]["ok"], false, "{r}");
    assert_eq!(std::fs::read_to_string(f.home.join("skill_state/a1.json")).unwrap(), "{ broken");
}

#[tokio::test]
async fn verdicts_are_keyed_by_layer_and_owner() {
    let f = fixture().await;
    f.h.record_skill_scan_verdict(
        "a1",
        "pdf",
        crate::skill_lifecycle::security_scanner::RiskLevel::Medium,
    );
    assert_eq!(entry(&f.listed("a1").await, "pdf")["security_status"], "warn");
    assert!(entry(&f.listed("b1").await, "pdf").get("security_status").is_none());
}

#[tokio::test]
async fn share_and_adopt_need_owner() {
    let f = fixture().await;
    let e = err(&f.call("skills.share", json!({ "agent_id": "a1", "skill_name": "docx" }), &f.viewer_a).await);
    assert!(e.contains("permission"), "{e}");
    let e = err(&f.call("skills.adopt", json!({ "skill_name": "docx", "target_agent_id": "b1" }), &f.owner_a).await);
    assert!(e.contains("permission"), "{e}");
}

#[tokio::test]
async fn search_and_leaderboard_only_show_visible_employees() {
    let f = fixture().await;
    for a in ["a1", "b1"] {
        std::fs::write(
            f.home.join(format!("agents/{a}/SKILLS/secret-{a}.md")),
            format!("---\nname: secret-{a}\nestimated_minutes_saved: 5\n---\nzebra-marker for {a}\n"),
        )
        .unwrap();
    }
    f.h.registry.write().await.scan().await.unwrap();
    let authors = |hits: Vec<Value>| -> Vec<String> {
        let mut v: Vec<String> = hits.iter().map(|h| h["author"].as_str().unwrap().to_string()).collect();
        v.sort();
        v
    };
    assert_eq!(authors(f.h.local_skill_hits(&f.viewer_a, "zebra-marker").await), vec!["a1"]);
    assert_eq!(authors(f.h.local_skill_hits(&f.admin, "zebra-marker").await), vec!["a1", "b1"]);

    let owners = |p: Value| -> Vec<String> {
        p["leaderboard"].as_array().unwrap().iter().filter_map(|r| r["owner"].as_str()).map(String::from).collect()
    };
    let lb = ok(&f.call("skills.leaderboard", json!({}), &f.viewer_a).await);
    assert_eq!(owners(lb), vec!["a1"]);
    let lb = ok(&f.call("skills.leaderboard", json!({}), &f.admin).await);
    assert_eq!(owners(lb), vec!["a1", "b1"]);
}

/// A switch that was written but could not be reloaded is reported as an
/// error, never as success.
#[cfg(unix)]
#[tokio::test]
async fn failed_reload_is_an_error_not_a_success() {
    use std::os::unix::fs::PermissionsExt;
    let f = fixture().await;
    let agents = f.home.join("agents");
    std::fs::set_permissions(&agents, std::fs::Permissions::from_mode(0o000)).unwrap();
    let frame = f
        .call("skills.set_enabled", json!({ "items": [item("a1", "pdf")], "enabled": false }), &f.admin)
        .await;
    std::fs::set_permissions(&agents, std::fs::Permissions::from_mode(0o755)).unwrap();
    let e = err(&frame);
    assert_eq!(e, crate::handlers::skills_manage_rpc::RELOAD_FAILED);
    // The switch itself was saved; a retry reloads it.
    assert!(duduclaw_agent::skill_state::read_disabled(&f.home, "a1").is_disabled("pdf"));
    ok(&f.call("skills.set_enabled", json!({ "items": [item("a1", "pdf")], "enabled": false }), &f.admin).await);
    assert!(!f.loaded("a1").await.contains(&"pdf".to_string()));
}

// B3: the aggregate contract separates global rows from per-employee rows.
#[tokio::test]
async fn skills_list_b3_composes_three_scopes_without_global_duplicates() {
    let f = fixture().await;
    let config = f.home.join("agents/a1/agent.toml");
    let mut agent: toml::Value = toml::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
    agent["agent"]["department"] = toml::Value::String("sales".into());
    std::fs::write(config, toml::to_string(&agent).unwrap()).unwrap();
    let dept = duduclaw_agent::skill_loader::department_skills_dir(&f.home, "sales");
    std::fs::create_dir_all(&dept).unwrap();
    std::fs::write(dept.join("sales-guide.md"), "# Sales guide\n").unwrap();
    // A personal copy overrides the department copy, which overrides global.
    std::fs::write(f.home.join("skills/override.md"), "# Global copy\n").unwrap();
    std::fs::write(dept.join("override.md"), "# Department copy\n").unwrap();
    std::fs::write(f.home.join("agents/a1/SKILLS/override.md"), "# Personal copy\n").unwrap();
    f.h.registry.write().await.scan().await.unwrap();

    let all = ok(&f.call("skills.list", json!({}), &f.admin).await);
    let globals = all["global_skills"].as_array().unwrap();
    assert_eq!(globals.len(), 2);
    assert!(globals.iter().all(|s| s["scope"] == "global"));
    assert_eq!(globals.iter().filter(|s| s["name"] == "tone").count(), 1);
    for a in all["agents"].as_array().unwrap() {
        assert!(a["skills"].as_array().unwrap().iter().all(|s| s["scope"] != "global"));
    }
    let one = ok(&f.call("skills.list", json!({"agent_id": "a1"}), &f.viewer_a).await);
    let skills = one["skills"].as_array().unwrap();
    assert_eq!(entry(skills, "tone")["scope"], "global");
    assert_eq!(entry(skills, "sales-guide")["scope"], "department");
    assert_eq!(entry(skills, "pdf")["scope"], "agent");
    assert_eq!(skills.iter().filter(|s| s["name"] == "override").count(), 1);
    assert_eq!(entry(skills, "override")["scope"], "agent");
    assert_eq!(entry(skills, "override")["content"], "# Personal copy\n");

    std::fs::remove_file(f.home.join("agents/a1/SKILLS/override.md")).unwrap();
    let one = ok(&f.call("skills.list", json!({"agent_id": "a1"}), &f.viewer_a).await);
    let skills = one["skills"].as_array().unwrap();
    assert_eq!(entry(skills, "override")["scope"], "department");
    assert_eq!(entry(skills, "override")["content"], "# Department copy\n");

    // Hidden employees must not leak via holders, paths, or personal rows.
    let visible = ok(&f.call("skills.list", json!({}), &f.viewer_a).await);
    assert_eq!(visible["agents"].as_array().unwrap().len(), 1);
    assert_eq!(visible["agents"][0]["agent_id"], "a1");
    for skill in visible["global_skills"].as_array().unwrap() {
        assert!(skill["holders"].as_array().unwrap().iter().all(|h| h["agent_id"] == "a1"));
    }
    for diag in visible["scanned"].as_array().unwrap() {
        assert!(!Path::new(diag["path"].as_str().unwrap()).starts_with(f.home.join("agents/b1")));
    }
}

#[tokio::test]
async fn skills_list_b3_rechecks_revoked_access_and_fails_closed() {
    let f = fixture().await;
    // Keep the stale socket context but revoke its live employee binding.
    rusqlite::Connection::open(f.home.join("users.db")).unwrap()
        .execute("DELETE FROM user_agent_bindings WHERE user_id = ?1", ["viewer-a"])
        .unwrap();
    let e = err(&f.call("skills.list", json!({"agent_id": "a1"}), &f.viewer_a).await);
    assert!(e.contains("permission"), "{e}");
    let all = ok(&f.call("skills.list", json!({}), &f.viewer_a).await);
    assert!(all["agents"].as_array().unwrap().is_empty());
    assert!(all["global_skills"].as_array().unwrap().iter()
        .all(|s| s["holders"].as_array().unwrap().is_empty()));
    std::fs::rename(f.home.join("users.db"), f.home.join("users.saved.db")).unwrap();
    std::fs::create_dir(f.home.join("users.db")).unwrap();
    let e = err(&f.call("skills.list", json!({}), &f.viewer_a).await);
    assert!(e.contains("permission"), "{e}");
}
