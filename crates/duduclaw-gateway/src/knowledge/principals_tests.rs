//! Grant rules: the picker lists exactly what `acl.set` lets the caller add.

use std::collections::HashMap;

use duduclaw_auth::{AccessLevel, UserRole};
use serde_json::json;

use super::*;
use crate::knowledge::rpc::handle;

fn ctx(id: &str, role: UserRole, agents: &[&str]) -> UserContext {
    UserContext {
        user_id: id.to_string(),
        email: format!("{id}@example.test"),
        role,
        agent_access: agents
            .iter()
            .map(|a| (a.to_string(), AccessLevel::Owner))
            .collect::<HashMap<_, _>>(),
        must_change_password: false,
        team_access: Default::default(),
        acl_generation: 0,
    }
}

struct Fx {
    home: tempfile::TempDir,
    dir: Directory,
    owner: UserContext,
    owner_user: ResolvedUser,
    colleague_id: String,
    collection: String,
}

fn resolved(c: &UserContext) -> ResolvedUser {
    ResolvedUser {
        user_id: c.user_id.clone(),
        department: None,
        is_admin: c.is_admin(),
    }
}

async fn fixture(personal: bool) -> Fx {
    let home = tempfile::tempdir().unwrap();
    let db = Arc::new(UserDb::new(&home.path().join("users.db")).unwrap());
    let owner_acc = db
        .create_user(
            "owner@example.test",
            "Owner",
            "pw-123456789",
            UserRole::Manager,
        )
        .unwrap();
    let colleague = db
        .create_user(
            "amy@example.test",
            "Amy Chen",
            "pw-123456789",
            UserRole::Employee,
        )
        .unwrap();
    db.set_department(&colleague.id, Some("sales")).unwrap();
    let gone = db
        .create_user(
            "gone@example.test",
            "Gone",
            "pw-123456789",
            UserRole::Employee,
        )
        .unwrap();
    db.set_user_status(&gone.id, UserStatus::Suspended).unwrap();

    let owner = ctx(&owner_acc.id, UserRole::Manager, &["helper"]);
    let created = handle(
        home.path(),
        "knowledge.collections.create",
        &json!({"name": "FAQ", "slug": "faq"}),
        &owner,
        None,
    )
    .await
    .unwrap()
    .unwrap();
    let collection = created["collection"]["id"].as_str().unwrap().to_string();
    let dir = Directory {
        personal,
        users: Some(db),
        agents: vec![
            AgentRef {
                id: "helper".into(),
                display_name: "小幫手".into(),
                department: None,
            },
            AgentRef {
                id: "finance-bot".into(),
                display_name: "財務".into(),
                department: Some("finance".into()),
            },
        ],
        departments: vec!["hr".into()],
    };
    Fx {
        owner_user: resolved(&owner),
        owner,
        home,
        dir,
        colleague_id: colleague.id,
        collection,
    }
}

fn user_count(db: &UserDb, n: usize) {
    for i in 0..n {
        db.create_user(
            &format!("p{i}@example.test"),
            &format!("Person {i}"),
            "pw-123456789",
            UserRole::Employee,
        )
        .unwrap();
    }
}

fn ids(v: &Value, key: &str) -> Vec<String> {
    v[key]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| {
            x.get("id")
                .and_then(Value::as_str)
                .or(x.as_str())
                .unwrap()
                .to_string()
        })
        .collect()
}

#[tokio::test]
async fn enterprise_search_lists_grantable_principals_without_email() {
    let f = fixture(false).await;
    let r = search(f.home.path(), &f.dir, &f.owner, &f.owner_user, &json!({})).unwrap();
    assert_eq!(r["edition"], "enterprise");
    assert!(
        ids(&r, "users").is_empty(),
        "no bulk listing of people without a search"
    );
    let one = search(
        f.home.path(),
        &f.dir,
        &f.owner,
        &f.owner_user,
        &json!({"query": "a"}),
    )
    .unwrap();
    assert!(
        ids(&one, "users").is_empty(),
        "a one-character search lists nobody"
    );
    let two = search(
        f.home.path(),
        &f.dir,
        &f.owner,
        &f.owner_user,
        &json!({"query": "ex"}),
    )
    .unwrap();
    // "ex" matches nobody's name or id except through e-mail, which is never searched.
    assert!(ids(&two, "users").is_empty());
    let gone = search(
        f.home.path(),
        &f.dir,
        &f.owner,
        &f.owner_user,
        &json!({"query": "gone"}),
    )
    .unwrap();
    assert!(
        ids(&gone, "users").is_empty(),
        "suspended accounts are not offered"
    );
    assert!(
        !r.to_string().contains("@example.test"),
        "no e-mail in the answer"
    );
    assert_eq!(
        ids(&r, "agents"),
        vec!["helper"],
        "only employees the caller manages"
    );
    let depts = ids(&r, "departments");
    assert!(
        depts.contains(&"hr".to_string())
            && depts.contains(&"sales".to_string())
            && depts.contains(&"finance".to_string())
    );
    assert!(ids(&r, "channels").contains(&"line".to_string()));
    assert!(!ids(&r, "channels").contains(&"webchat".to_string()));

    let filtered = search(
        f.home.path(),
        &f.dir,
        &f.owner,
        &f.owner_user,
        &json!({"query": "amy"}),
    )
    .unwrap();
    assert_eq!(ids(&filtered, "users"), vec![f.colleague_id.clone()]);
    assert!(ids(&filtered, "agents").is_empty());
}

#[tokio::test]
async fn personal_search_offers_only_employees_and_channels() {
    let f = fixture(true).await;
    let r = search(f.home.path(), &f.dir, &f.owner, &f.owner_user, &json!({})).unwrap();
    assert_eq!(r["kinds"], json!(["agent", "channel"]));
    assert!(ids(&r, "users").is_empty());
    assert!(ids(&r, "departments").is_empty());
    assert_eq!(ids(&r, "agents"), vec!["helper"]);
}

#[tokio::test]
async fn search_needs_an_owned_collection_or_admin() {
    let f = fixture(false).await;
    let stranger = ctx("stranger", UserRole::Employee, &[]);
    let e = search(
        f.home.path(),
        &f.dir,
        &stranger,
        &resolved(&stranger),
        &json!({}),
    )
    .unwrap_err();
    assert!(matches!(e, KbError::PermissionDenied));
    let e = search(
        f.home.path(),
        &f.dir,
        &stranger,
        &resolved(&stranger),
        &json!({"collection_id": f.collection}),
    )
    .unwrap_err();
    assert!(matches!(e, KbError::PermissionDenied));
    let admin = ctx("root", UserRole::Admin, &[]);
    let r = search(f.home.path(), &f.dir, &admin, &resolved(&admin), &json!({})).unwrap();
    assert_eq!(
        ids(&r, "agents").len(),
        2,
        "admin may grant to any employee"
    );
}

fn acl_params(f: &Fx, extra: Value) -> Value {
    json!({
        "collection_id": f.collection,
        "expected_revision": 0,
        "entries": [{"kind": "user", "id": f.owner.user_id, "role": "owner"}, extra],
    })
}

#[tokio::test]
async fn acl_set_check_matches_the_picker() {
    let f = fixture(false).await;
    let check = |extra: Value| {
        check_new_grants(
            f.home.path(),
            &f.dir,
            &f.owner,
            &f.owner_user,
            &acl_params(&f, extra),
        )
    };
    check(json!({"kind": "user", "id": f.colleague_id, "role": "viewer"})).unwrap();
    check(json!({"kind": "agent", "id": "helper", "role": "viewer"})).unwrap();
    check(json!({"kind": "department", "id": "sales", "role": "editor"})).unwrap();
    check(json!({"kind": "channel", "id": "line", "role": "viewer"})).unwrap();
    check(json!({"kind": "channel", "id": "line:U123", "role": "viewer"})).unwrap();

    assert!(check(json!({"kind": "agent", "id": "finance-bot", "role": "viewer"})).is_err());
    assert!(check(json!({"kind": "agent", "id": "nobody", "role": "viewer"})).is_err());
    assert!(check(json!({"kind": "user", "id": "made-up", "role": "viewer"})).is_err());
    assert!(check(json!({"kind": "department", "id": "nowhere", "role": "viewer"})).is_err());
    assert!(check(json!({"kind": "channel", "id": "webchat", "role": "viewer"})).is_err());
    assert!(check(json!({"kind": "channel", "id": "carrierpigeon", "role": "viewer"})).is_err());
}

#[tokio::test]
async fn existing_grants_are_not_rechecked_and_non_owners_get_denied() {
    let f = fixture(false).await;
    // An admin grants an employee the owner does not manage.
    let admin = ctx("root", UserRole::Admin, &[]);
    handle(
        f.home.path(),
        "knowledge.acl.set",
        &acl_params(
            &f,
            json!({"kind": "agent", "id": "finance-bot", "role": "viewer"}),
        ),
        &admin,
        None,
    )
    .await
    .unwrap()
    .unwrap();
    // The owner can still save the list that keeps it.
    let keep = json!({
        "collection_id": f.collection,
        "expected_revision": 1,
        "entries": [
            {"kind": "user", "id": f.owner.user_id, "role": "owner"},
            {"kind": "agent", "id": "finance-bot", "role": "viewer"},
        ],
    });
    check_new_grants(f.home.path(), &f.dir, &f.owner, &f.owner_user, &keep).unwrap();

    let stranger = ctx("stranger", UserRole::Employee, &["helper"]);
    let e = check_new_grants(
        f.home.path(),
        &f.dir,
        &stranger,
        &resolved(&stranger),
        &keep,
    )
    .unwrap_err();
    assert!(matches!(e, KbError::PermissionDenied));
}

#[tokio::test]
async fn personal_refuses_new_user_and_department_grants() {
    let f = fixture(true).await;
    let e = check_new_grants(
        f.home.path(),
        &f.dir,
        &f.owner,
        &f.owner_user,
        &acl_params(
            &f,
            json!({"kind": "user", "id": f.colleague_id, "role": "viewer"}),
        ),
    )
    .unwrap_err();
    assert!(e.public_message().contains("個人版"), "{e}");
    // The creator's own owner row is already there and passes.
    check_new_grants(
        f.home.path(),
        &f.dir,
        &f.owner,
        &f.owner_user,
        &acl_params(
            &f,
            json!({"kind": "channel", "id": "telegram", "role": "viewer"}),
        ),
    )
    .unwrap();
}

#[test]
fn without_a_user_database_no_user_is_grantable() {
    let dir = Directory::default();
    let admin = ctx("root", UserRole::Admin, &[]);
    assert!(!dir.may_grant(&admin, PrincipalKind::User, "anyone"));
    assert!(dir.may_grant(&admin, PrincipalKind::Channel, "line"));
}

#[tokio::test]
async fn people_search_is_capped_and_resolve_names_only_listed_ids() {
    let f = fixture(false).await;
    user_count(f.dir.users.as_ref().unwrap(), 30);
    let r = search(
        f.home.path(),
        &f.dir,
        &f.owner,
        &f.owner_user,
        &json!({"query": "person"}),
    )
    .unwrap();
    assert_eq!(ids(&r, "users").len(), MAX_USER_RESULTS);

    // Resolve: only ids on this collection's list come back for an owner.
    let r = search(
        f.home.path(),
        &f.dir,
        &f.owner,
        &f.owner_user,
        &json!({"collection_id": f.collection, "resolve": [f.owner.user_id, f.colleague_id]}),
    )
    .unwrap();
    assert_eq!(ids(&r, "resolved_users"), vec![f.owner.user_id.clone()]);
    let admin = ctx("root", UserRole::Admin, &[]);
    let r = search(
        f.home.path(),
        &f.dir,
        &admin,
        &resolved(&admin),
        &json!({"resolve": [f.colleague_id]}),
    )
    .unwrap();
    assert_eq!(ids(&r, "resolved_users"), vec![f.colleague_id.clone()]);
}

#[tokio::test]
async fn viewer_or_operator_access_to_an_employee_is_not_enough() {
    let f = fixture(false).await;
    for level in [AccessLevel::Viewer, AccessLevel::Operator] {
        let mut c = f.owner.clone();
        c.agent_access = [("helper".to_string(), level)].into_iter().collect();
        let r = search(f.home.path(), &f.dir, &c, &f.owner_user, &json!({})).unwrap();
        assert!(ids(&r, "agents").is_empty(), "{level:?}");
        let e = check_new_grants(
            f.home.path(),
            &f.dir,
            &c,
            &f.owner_user,
            &acl_params(
                &f,
                json!({"kind": "agent", "id": "helper", "role": "viewer"}),
            ),
        )
        .unwrap_err();
        assert!(e.public_message().contains("helper"), "{level:?}: {e}");
    }
}

#[tokio::test]
async fn raising_a_role_is_checked_and_malformed_rows_are_refused() {
    let f = fixture(false).await;
    let admin = ctx("root", UserRole::Admin, &[]);
    // Admin adds an account that is later suspended, as a viewer.
    handle(
        f.home.path(),
        "knowledge.acl.set",
        &acl_params(
            &f,
            json!({"kind": "user", "id": f.colleague_id, "role": "viewer"}),
        ),
        &admin,
        None,
    )
    .await
    .unwrap()
    .unwrap();
    f.dir
        .users
        .as_ref()
        .unwrap()
        .set_user_status(&f.colleague_id, UserStatus::Suspended)
        .unwrap();
    let with = |role: &str| {
        json!({"collection_id": f.collection, "expected_revision": 1, "entries": [
            {"kind": "user", "id": f.owner.user_id, "role": "owner"},
            {"kind": "user", "id": f.colleague_id, "role": role},
        ]})
    };
    // Kept as it is, or lowered: allowed.
    check_new_grants(
        f.home.path(),
        &f.dir,
        &f.owner,
        &f.owner_user,
        &with("viewer"),
    )
    .unwrap();
    // Raised: checked again, and the suspended account no longer qualifies.
    for role in ["editor", "owner"] {
        assert!(
            check_new_grants(f.home.path(), &f.dir, &f.owner, &f.owner_user, &with(role)).is_err(),
            "{role}"
        );
    }
    for bad in [
        json!({"kind": "user", "id": f.owner.user_id}),
        json!({"kind": "robot", "id": "x", "role": "viewer"}),
        json!({"kind": "agent", "id": " ", "role": "viewer"}),
        json!({"kind": "agent", "id": "helper", "role": "boss"}),
    ] {
        let p = json!({"collection_id": f.collection, "expected_revision": 1, "entries": [
            {"kind": "user", "id": f.owner.user_id, "role": "owner"}, bad.clone()]});
        assert!(
            matches!(
                check_new_grants(f.home.path(), &f.dir, &f.owner, &f.owner_user, &p),
                Err(KbError::Invalid(_))
            ),
            "{bad}"
        );
    }
}
