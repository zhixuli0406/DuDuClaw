//! P10 integration tests. Run by the main session with the gateway test suite.
use std::{collections::BTreeMap, sync::Arc};

use duduclaw_auth::{AccessLevel, JwtConfig, UserContext, UserDb, UserRole};
use duduclaw_auth::signin_members::{SigninApproval, SigninOutcome, SigninPolicy, VerifiedGoogleIdentity};
use serde_json::{json, Value};

use super::{avatar::{SigninAvatarError as Error, SigninAvatarRequest}, config::{self, AvatarTemplate}};
use crate::{company_templates as company, handlers::MethodHandler, protocol::WsFrame};

struct Fixture {
    home: tempfile::TempDir,
    handler: MethodHandler,
    db: Arc<UserDb>,
    member: String,
}

impl Fixture {
    fn conn(&self) -> rusqlite::Connection {
        let conn = rusqlite::Connection::open(self.home.path().join("users.db")).unwrap();
        conn.busy_timeout(std::time::Duration::from_secs(5)).unwrap();
        conn
    }
}

async fn fixture() -> Fixture {
    let home = tempfile::tempdir().unwrap();
    let db = Arc::new(UserDb::new(&home.path().join("users.db")).unwrap());
    // Exercise the real provisioning API: no admin is present in this home.
    let member = match db.resolve_google_signin(&VerifiedGoogleIdentity {
        sub: "123456789".into(), email: "member@company.test".into(),
        hd: "company.test".into(), display_name: "新同仁".into(),
    }, &SigninPolicy {
        approval: SigninApproval::Direct, daily_cap: 20, settings_version: "fixture".into(),
        ack_by: "fixture-owner".into(), ack_at: "2026-11-27T00:00:00Z".into(),
    }, 1_800_000_000, &|_| Ok(None)).unwrap() {
        SigninOutcome::Ok { user_id, created: true, .. } => user_id,
        other => panic!("unexpected provisioning outcome: {other:?}"),
    };
    let handler = MethodHandler::new(home.path().into()).await;
    handler.set_user_db(db.clone(), Arc::new(JwtConfig::new(b"signin-avatar-test-secret-32-bytes!!"))).await;
    handler.set_test_edition(Some(duduclaw_core::EditionProfile::Enterprise)).await;
    handler.set_agent_cap_fixture(Some(0)).await;
    Fixture { home, handler, db, member }
}

fn request(f: &Fixture, name: &str, template: AvatarTemplate) -> SigninAvatarRequest {
    let mut table = toml::Table::new();
    let (key, revision) = match &template {
        AvatarTemplate::Blank => (String::new(), 0),
        AvatarTemplate::Company { key, revision } => (format!("company:{key}"), *revision as i64),
        AvatarTemplate::Invalid => ("builtin:evaluator".into(), 0),
    };
    table.insert("google_signin".into(), toml::toml! {
        enabled = true
        allowed_hd = ["company.test"]
        avatar_template = key
        avatar_template_revision = revision
        [enabled_ack]
        by = "fixture-owner"
        at = "2026-11-27T00:00:00Z"
    }.into());
    std::fs::write(f.home.path().join("config.toml"), toml::to_string(&table).unwrap()).unwrap();
    let settings = config::read_strict(f.home.path());
    assert!(settings.enabled);
    SigninAvatarRequest { member_id: f.member.clone(), agent_name: name.into(),
        display_name: "新同仁 的 AI 分身".into(), template, settings_revision: settings.revision }
}

fn template(f: &Fixture, premium: bool) -> company::CompanyTemplate {
    let mut table: toml::Table = include_str!("../../../../templates/evaluator/agent.toml").parse().unwrap();
    let agent = table.get_mut("agent").unwrap().as_table_mut().unwrap();
    agent.insert("role".into(), "main".into());
    agent.insert("reports_to".into(), "old-supervisor".into());
    agent.insert("department".into(), "old-department".into());
    // Company creation already projects identity to a specialist; get() also
    // rejects a stored noncanonical identity. Do not bypass that validation.
    company::create(f.home.path(), "Company assistant", "", &toml::to_string(&table).unwrap(),
        "Helpful assistant", "", BTreeMap::from([("writing".into(), "# Writing\nHelp write clearly.".into())]),
        premium, vec!["writing".into()]).unwrap()
}

fn choice(item: &company::CompanyTemplate) -> AvatarTemplate {
    AvatarTemplate::Company { key: item.id.clone(), revision: item.revision }
}

fn error(frame: WsFrame) -> Value {
    match frame {
        WsFrame::Response { ok: false, error: Some(error), .. } => error,
        other => panic!("expected refusal: {other:?}"),
    }
}

fn assert_absent(f: &Fixture, name: &str) {
    assert!(!f.home.path().join("agents").join(name).exists());
    assert_eq!(f.db.avatar_agent(&f.member).unwrap(), None);
    assert!(f.db.get_agent_users(name).unwrap().is_empty());
    assert!(duduclaw_core::org_store::load(f.home.path()).get(name).is_none());
    let pending: String = f.conn().query_row("SELECT avatar FROM user_onboarding WHERE user_id=?1",
        [&f.member], |r| r.get(0)).unwrap();
    assert_eq!(pending, "pending");
}

fn assert_created(f: &Fixture, name: &str, actor: &str) {
    let table: toml::Table = std::fs::read_to_string(f.home.path().join("agents").join(name).join("agent.toml"))
        .unwrap().parse().unwrap();
    assert_eq!(table["agent"]["role"].as_str(), Some("specialist"));
    assert_eq!(table["agent"]["reports_to"].as_str(), Some(""));
    assert_eq!(table["agent"]["department"].as_str(), Some(""));
    assert_eq!(f.db.avatar_agent(&f.member).unwrap().as_deref(), Some(name));
    assert_eq!(f.db.check_agent_access(&f.member, name).unwrap(), Some(AccessLevel::Owner));
    let conn = f.conn();
    let audit: (String, String, String) = conn.query_row(
        "SELECT user_id,target,detail FROM auth_audit_log WHERE action='user.create_avatar'",
        [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
    assert_eq!(audit.0, actor);
    assert_eq!(audit.1, f.member);
    if actor == "system:google_signin" {
        let detail: Value = serde_json::from_str(&audit.2).unwrap();
        assert_eq!(detail["agent"], name);
        assert_eq!(detail["revision"], config::read_strict(f.home.path()).revision);
    }
    let owner: String = conn.query_row("SELECT owner_user_id FROM isolation_personal_agents WHERE agent_name=?1",
        [name], |r| r.get(0)).unwrap();
    assert_eq!(owner, f.member);
}

#[tokio::test]
async fn rpc_json_cannot_select_signin_actor() {
    let f = fixture().await;
    let caller = f.db.live_user_context(&f.member).unwrap().unwrap();
    let baseline = error(f.handler.handle_agents_create_as(json!({
        "name":"rpc-avatar", "avatar_for_user":f.member,
    }), Some(&caller)).await);
    let spoofed = error(f.handler.handle_agents_create_as(json!({
        "name":"rpc-avatar", "avatar_for_user":f.member,
        "avatar_actor":"signin", "signin_policy":true, "settings_revision":"x",
    }), Some(&caller)).await);
    assert_eq!(baseline, spoofed);
    assert_eq!(spoofed, json!("只有管理員可以為成員建立 AI 分身。"));
    assert_absent(&f, "rpc-avatar");
}

#[tokio::test]
async fn signin_avatar_blank_created() {
    let f = fixture().await;
    let mut req = request(&f, "signin-blank", AvatarTemplate::Blank);
    req.display_name = format!("\0\n{}\t", "同仁🦀".repeat(100));
    // Compile-time Send check for the future used by P11's background job.
    fn require_send<T: Send>(value: T) -> T { value }
    require_send(f.handler.create_signin_avatar(req)).await.unwrap();
    assert_created(&f, "signin-blank", "system:google_signin");
    let config: toml::Table = std::fs::read_to_string(f.home.path().join("agents/signin-blank/agent.toml"))
        .unwrap().parse().unwrap();
    let display = config["agent"]["display_name"].as_str().unwrap();
    assert_eq!(display.chars().count(), 200);
    assert!(!display.chars().any(char::is_control));
}

#[tokio::test]
async fn signin_avatar_requires_google_member() {
    let f = fixture().await;
    f.conn().execute("DELETE FROM google_signin_links WHERE user_id=?1", [&f.member]).unwrap();
    let result = f.handler.create_signin_avatar(request(&f, "no-link", AvatarTemplate::Blank)).await;
    assert_eq!(result, Err(Error::NotEligible));
    assert_absent(&f, "no-link");
}

#[tokio::test]
async fn signin_avatar_policy_changed_rolls_back() {
    for from_template in [false, true] {
        let f = fixture().await;
        let selected = if from_template { choice(&template(&f, false)) } else { AvatarTemplate::Blank };
        let req = request(&f, "policy-changed", selected);
        // Change after capturing the job request. The check occurs only at
        // publish, after staging and org preparation, inside users.db.
        let path = f.home.path().join("config.toml");
        let changed = std::fs::read_to_string(&path).unwrap().replace("enabled = true", "enabled = false");
        std::fs::write(path, changed).unwrap();
        assert_eq!(f.handler.create_signin_avatar(req).await, Err(Error::PolicyChanged));
        assert_absent(&f, "policy-changed");
        assert!(!duduclaw_agent::skill_state::read_disabled(f.home.path(), "policy-changed").is_disabled("writing"));
    }
}

#[tokio::test]
async fn template_revision_mismatch_waits() {
    let f = fixture().await;
    let item = template(&f, false);
    let req = request(&f, "stale-template", AvatarTemplate::Company { key: item.id, revision: item.revision + 1 });
    assert_eq!(f.handler.create_signin_avatar(req).await, Err(Error::TemplateChanged));
    assert_absent(&f, "stale-template");
    assert!(!f.home.path().join("skill_state/stale-template.json").exists());
}

#[tokio::test]
async fn template_rewritten_to_specialist() {
    let f = fixture().await;
    let item = template(&f, false);
    f.handler.create_signin_avatar(request(&f, "from-template", choice(&item))).await.unwrap();
    assert_created(&f, "from-template", "system:google_signin");
    let dir = f.home.path().join("agents/from-template");
    let table: toml::Table = std::fs::read_to_string(dir.join("agent.toml")).unwrap().parse().unwrap();
    assert_eq!(table["agent"]["display_name"].as_str(), Some("新同仁 的 AI 分身"));
    assert_eq!(std::fs::read_to_string(dir.join("SOUL.md")).unwrap(), item.soul_md);
    assert!(dir.join("SKILLS/writing/SKILL.md").is_file());
    assert!(duduclaw_agent::skill_state::read_disabled(f.home.path(), "from-template").is_disabled("writing"));
}

#[tokio::test]
async fn team_or_premium_template_refused() {
    let f = fixture().await;
    let premium = template(&f, true);
    let member = template(&f, false);
    let team = company::create_team(f.home.path(), "Team", "", vec![member]).unwrap();
    for selected in [choice(&premium), choice(&team), AvatarTemplate::Invalid,
        AvatarTemplate::Company { key: "builtin:evaluator".into(), revision: 1 },
        AvatarTemplate::Company { key: "premium:restaurant:chef".into(), revision: 1 }] {
        assert_eq!(f.handler.create_signin_avatar(request(&f, "invalid-template", selected)).await,
            Err(Error::TemplateInvalid));
        assert_absent(&f, "invalid-template");
    }
}

// isolation_active() is a process-wide durable latch (and also reads host
// installation state). Never toggle it in a parallel gateway test process.
#[tokio::test]
#[ignore = "run alone on an isolated protected-mode gateway host; no process-local isolation fixture exists"]
async fn protected_mode_refused() {
    assert!(crate::runtime_readiness::isolation_active());
    let f = fixture().await;
    assert_eq!(f.handler.create_signin_avatar(request(&f, "protected-avatar", AvatarTemplate::Blank)).await,
        Err(Error::ProtectedMode));
    assert_absent(&f, "protected-avatar");
}

#[tokio::test]
async fn existing_directory_name_taken() {
    for from_template in [false, true] {
        let f = fixture().await;
        let selected = if from_template { choice(&template(&f, false)) } else { AvatarTemplate::Blank };
        let dir = f.home.path().join("agents/leftover");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("keep"), "earlier attempt").unwrap();
        assert_eq!(f.handler.create_signin_avatar(request(&f, "leftover", selected)).await, Err(Error::NameTaken));
        assert_eq!(std::fs::read_to_string(dir.join("keep")).unwrap(), "earlier attempt");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        assert_eq!(f.db.avatar_agent(&f.member).unwrap(), None);
    }
}

#[tokio::test]
async fn admin_avatar_path_unchanged() {
    let f = fixture().await;
    let admin = f.db.create_user("admin@company.test", "Admin", "password123", UserRole::Admin).unwrap();
    let caller: UserContext = f.db.live_user_context(&admin.id).unwrap().unwrap();
    // Admin avatars do not require Google link or onboarding policy.
    f.conn().execute("DELETE FROM google_signin_links WHERE user_id=?1", [&f.member]).unwrap();
    let response = f.handler.create_agent_files(json!({"name":"admin-avatar", "avatar_for_user":f.member}), Some(&caller)).await;
    assert!(matches!(response, WsFrame::Response { ok: true, .. }), "{response:?}");
    assert_created(&f, "admin-avatar", &admin.id);
    assert_eq!(error(f.handler.create_agent_files(json!({"name":"bad-role", "avatar_for_user":f.member, "role":"main"}),
        Some(&caller)).await), json!("AI 分身必須使用 specialist 角色。"));
}

#[tokio::test]
async fn signin_avatar_failure_after_publish_restores_directory_and_org() {
    for from_template in [false, true] {
        let f = fixture().await;
        let selected = if from_template { choice(&template(&f, false)) } else { AvatarTemplate::Blank };
        // Force failure AFTER publication, owner binding and avatar assignment.
        // The users transaction and the outer filesystem compensation must
        // both roll back; no std Mutex guard crosses the await below.
        f.conn().execute_batch("CREATE TRIGGER reject_avatar_audit BEFORE INSERT ON auth_audit_log
            WHEN NEW.action='user.create_avatar' BEGIN SELECT RAISE(ABORT,'fixture publish failure'); END;").unwrap();
        assert_eq!(f.handler.create_signin_avatar(request(&f, "rollback-avatar", selected)).await, Err(Error::Failed));
        assert_absent(&f, "rollback-avatar");
        assert!(!duduclaw_agent::skill_state::read_disabled(f.home.path(), "rollback-avatar").is_disabled("writing"));
    }
}

#[tokio::test]
async fn signin_avatar_seat_recount_rolls_back_both_writers() {
    for from_template in [false, true] {
        let f = fixture().await;
        // Existing employees already exceed the new cap. Even a Personal
        // avatar must run the auth transaction's final seat recount.
        for name in ["ordinary-one", "ordinary-two"] {
            assert!(matches!(f.handler.create_agent_files(json!({"name":name}), None).await,
                WsFrame::Response { ok: true, .. }));
        }
        f.handler.set_agent_cap_fixture(Some(1)).await;
        let selected = if from_template { choice(&template(&f, false)) } else { AvatarTemplate::Blank };
        assert_eq!(f.handler.create_signin_avatar(request(&f, "over-cap", selected)).await, Err(Error::SeatsFull));
        assert_absent(&f, "over-cap");
    }
}

#[tokio::test]
async fn signin_avatar_policy_failure_restores_prior_org_entry() {
    let f = fixture().await;
    let prior = duduclaw_core::OrgEntry::new("", "prior-department");
    {
        let org = duduclaw_core::org_store::lock_org_mutations(f.home.path()).unwrap();
        duduclaw_core::org_store::upsert(&org, "prior-org", prior.clone()).unwrap();
    }
    let mut req = request(&f, "prior-org", choice(&template(&f, false)));
    req.settings_revision = "stale-job-revision".into();
    assert_eq!(f.handler.create_signin_avatar(req).await, Err(Error::PolicyChanged));
    assert_eq!(duduclaw_core::org_store::load(f.home.path()).get("prior-org"), Some(&prior));
    assert!(!f.home.path().join("agents/prior-org").exists());
    assert_eq!(f.db.avatar_agent(&f.member).unwrap(), None);
}

#[tokio::test]
async fn signin_avatar_does_not_remove_a_competing_directory() {
    let f = fixture().await;
    let target = f.home.path().join("agents/competing-avatar");
    // Deterministically place a foreign directory after staging but before
    // rename, inside the synchronous transaction callback.
    let policy_check = || {
        std::fs::create_dir(&target).map_err(|e| e.to_string())?;
        std::fs::write(target.join("keep"), "another creator").map_err(|e| e.to_string())
    };
    let params = json!({"name":"competing-avatar", "role":"specialist"});
    let response = f.handler.create_agent_files_inner(&params, None,
        super::avatar::AvatarCreation::SigninPolicy {
            member: &f.member, revision: "fixture", policy_check: &policy_check,
        }).await;
    assert!(matches!(response, WsFrame::Response { ok: false, .. }));
    assert_eq!(std::fs::read_to_string(target.join("keep")).unwrap(), "another creator");
    assert_eq!(f.db.avatar_agent(&f.member).unwrap(), None);
    assert!(f.db.get_agent_users("competing-avatar").unwrap().is_empty());
}

#[tokio::test]
async fn tampered_company_template_is_not_silently_normalized() {
    let f = fixture().await;
    let mut item = template(&f, false);
    let selected = choice(&item);
    let mut config: toml::Table = item.agent_toml.parse().unwrap();
    config.get_mut("agent").unwrap().as_table_mut().unwrap().insert("role".into(), "main".into());
    item.agent_toml = toml::to_string(&config).unwrap();
    std::fs::write(f.home.path().join("company_templates").join(format!("{}.json", item.id)),
        serde_json::to_vec(&item).unwrap()).unwrap();
    assert_eq!(f.handler.create_signin_avatar(request(&f, "tampered-template", selected)).await, Err(Error::TemplateInvalid));
    assert_absent(&f, "tampered-template");
}
