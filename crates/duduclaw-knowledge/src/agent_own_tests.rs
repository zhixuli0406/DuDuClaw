use super::*;
use crate::acl::{AgentCtx, AgentNarrowing, HumanCtx, Narrowing, effective_collections};
use crate::schema::open_in_memory;

fn cfg() -> KnowledgeConfig {
    KnowledgeConfig::default()
}

fn new_own<'a>(agent: &'a str, name: &'a str, readers: Vec<Grant>) -> NewOwn<'a> {
    NewOwn {
        agent_id: agent,
        display_name: name,
        sensitivity: None,
        share_chat_users: true,
        created_by: "u-owner",
        readers,
    }
}

fn agent(id: &str) -> AgentCtx {
    AgentCtx {
        agent_id: id.to_string(),
        department: None,
        narrowing: AgentNarrowing::All,
    }
}

fn readable(conn: &Connection, user: &str, agent_id: &str) -> Vec<String> {
    effective_collections(
        conn,
        &HumanCtx::dashboard(user, None, false),
        Some(&agent(agent_id)),
        &Narrowing::default(),
    )
    .unwrap()
    .ids()
    .to_vec()
}

fn grants(conn: &Connection, id: &str) -> Vec<(String, String, String)> {
    acl::get_acl(conn, id)
        .unwrap()
        .0
        .into_iter()
        .map(|e| {
            (
                e.kind.as_str().to_string(),
                e.id,
                e.role.as_str().to_string(),
            )
        })
        .collect()
}

#[test]
fn create_names_and_shares_by_employee_id() {
    let mut conn = open_in_memory().unwrap();
    let readers = vec![(PrincipalKind::User, "u-op".to_string(), Role::Viewer)];
    let got = create(
        &mut conn,
        &new_own("sales_bot", "業務小幫手", readers),
        &cfg(),
    )
    .unwrap();
    let Ensured::Created(c) = got else {
        panic!("expected a new collection");
    };
    assert_eq!(c.name, "業務小幫手 的資料");
    assert_eq!(c.slug, "agent-sales-bot");
    assert_eq!(c.sensitivity, "internal");
    let g = grants(&conn, &c.id);
    assert!(g.contains(&("user".into(), "u-owner".into(), "owner".into())));
    assert!(g.contains(&("agent".into(), "sales_bot".into(), "viewer".into())));
    assert!(g.contains(&("user".into(), "u-op".into(), "viewer".into())));
    assert_eq!(g.len(), 3);
    let row = get(&conn, "sales_bot").unwrap().unwrap();
    assert_eq!(row.collection_id, c.id);
    assert_eq!(
        for_collection(&conn, &c.id).unwrap().unwrap().agent_id,
        "sales_bot"
    );
    // The intersection: the employee answering u-op reads it, answering a
    // stranger it does not.
    assert_eq!(readable(&conn, "u-op", "sales_bot"), vec![c.id.clone()]);
    assert!(readable(&conn, "u-stranger", "sales_bot").is_empty());
    // Another employee answering u-op does not.
    assert!(readable(&conn, "u-op", "other_bot").is_empty());
}

#[test]
fn second_create_returns_the_same_collection_even_after_a_rename() {
    let mut conn = open_in_memory().unwrap();
    let first = create(&mut conn, &new_own("a1", "小明", vec![]), &cfg()).unwrap();
    let again = create(&mut conn, &new_own("a1", "改名後", vec![]), &cfg()).unwrap();
    assert!(matches!(again, Ensured::Existing(_)));
    assert_eq!(first.collection().id, again.collection().id);
    assert_eq!(again.collection().name, "小明 的資料");
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM collections", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn slug_collision_gets_a_suffix_and_sensitivity_is_kept() {
    let mut conn = open_in_memory().unwrap();
    store::create_collection(
        &mut conn,
        &NewCollection {
            name: "taken".into(),
            slug: Some("agent-a1".into()),
            ..Default::default()
        },
        "u",
        &cfg(),
    )
    .unwrap();
    let mut n = new_own("a1", "A1", vec![]);
    n.sensitivity = Some("personal");
    let c = create(&mut conn, &n, &cfg()).unwrap();
    let c = c.collection();
    assert!(c.slug.starts_with("agent-a1-"), "{}", c.slug);
    assert!(store::valid_slug(&c.slug));
    assert_eq!(c.sensitivity, "personal");
}

#[test]
fn bad_sensitivity_creates_nothing() {
    let mut conn = open_in_memory().unwrap();
    let mut n = new_own("a1", "A1", vec![]);
    n.sensitivity = Some("secret");
    assert!(create(&mut conn, &n, &cfg()).is_err());
    assert!(get(&conn, "a1").unwrap().is_none());
}

#[test]
fn invalid_reader_rolls_the_collection_back() {
    let mut conn = open_in_memory().unwrap();
    // A channel may only be a viewer: set_acl refuses, nothing is left.
    let readers = vec![(PrincipalKind::Channel, "line".to_string(), Role::Owner)];
    assert!(create(&mut conn, &new_own("a1", "A1", readers), &cfg()).is_err());
    assert!(get(&conn, "a1").unwrap().is_none());
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM collections", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn archive_keeps_only_owners_and_restore_brings_the_same_grants_back() {
    let mut conn = open_in_memory().unwrap();
    let readers = vec![
        (PrincipalKind::User, "u-op".to_string(), Role::Viewer),
        (PrincipalKind::User, "u-co".to_string(), Role::Owner),
    ];
    let c = create(&mut conn, &new_own("a1", "A1", readers), &cfg())
        .unwrap()
        .collection()
        .clone();
    // A grant someone added by hand in the library survives the round trip.
    let (entries, rev) = acl::get_acl(&conn, &c.id).unwrap();
    let mut list: Vec<Grant> = entries
        .into_iter()
        .map(|e| (e.kind, e.id, e.role))
        .collect();
    list.push((PrincipalKind::Department, "sales".to_string(), Role::Viewer));
    acl::set_acl(&mut conn, &c.id, &list, rev, "u-owner").unwrap();

    let done = archive(&mut conn, "a1", "admin").unwrap().unwrap();
    assert_eq!(done.collection_id, c.id);
    assert_eq!(done.removed.len(), 3, "{:?}", done.removed);
    let g = grants(&conn, &c.id);
    assert_eq!(g.len(), 2);
    assert!(g.iter().all(|(k, _, r)| k == "user" && r == "owner"));
    assert!(readable(&conn, "u-owner", "a1").is_empty());
    assert!(readable(&conn, "u-op", "a1").is_empty());
    // Archived: a new upload is refused, a second archive is a no-op.
    assert!(create(&mut conn, &new_own("a1", "A1", vec![]), &cfg()).is_err());
    assert!(archive(&mut conn, "a1", "admin").unwrap().is_none());

    let added = restore(&mut conn, "a1", "admin").unwrap().unwrap();
    assert_eq!(added.len(), 3, "{added:?}");
    let g = grants(&conn, &c.id);
    assert!(
        g.contains(&("department".into(), "sales".into(), "viewer".into())),
        "{g:?}"
    );
    assert!(
        g.contains(&("agent".into(), "a1".into(), "viewer".into())),
        "{g:?}"
    );
    assert_eq!(readable(&conn, "u-op", "a1"), vec![c.id.clone()]);
    assert!(get(&conn, "a1").unwrap().unwrap().archived_at.is_none());
    assert!(restore(&mut conn, "a1", "admin").unwrap().is_none());
}

#[test]
fn a_grant_added_back_by_hand_while_archived_is_ignored() {
    let mut conn = open_in_memory().unwrap();
    let readers = vec![(PrincipalKind::User, "u-op".to_string(), Role::Viewer)];
    let c = create(&mut conn, &new_own("a1", "A1", readers), &cfg())
        .unwrap()
        .collection()
        .clone();
    archive(&mut conn, "a1", "admin").unwrap();
    let (entries, rev) = acl::get_acl(&conn, &c.id).unwrap();
    let mut list: Vec<Grant> = entries
        .into_iter()
        .map(|e| (e.kind, e.id, e.role))
        .collect();
    list.push((PrincipalKind::Agent, "a1".to_string(), Role::Viewer));
    list.push((PrincipalKind::User, "u-op".to_string(), Role::Viewer));
    acl::set_acl(&mut conn, &c.id, &list, rev, "u-owner").unwrap();
    assert!(
        readable(&conn, "u-op", "a1").is_empty(),
        "the employee side stays closed"
    );
    assert!(
        acl::agent_side_collections(&conn, &agent("a1"))
            .unwrap()
            .is_empty()
    );
    // Another employee granted the same collection is not affected.
    list.push((PrincipalKind::Agent, "a2".to_string(), Role::Viewer));
    let rev = acl::get_acl(&conn, &c.id).unwrap().1;
    acl::set_acl(&mut conn, &c.id, &list, rev, "u-owner").unwrap();
    assert_eq!(readable(&conn, "u-op", "a2"), vec![c.id.clone()]);
}

#[test]
fn deleting_the_collection_frees_the_employee_slot() {
    let mut conn = open_in_memory().unwrap();
    let c = create(&mut conn, &new_own("a1", "A1", vec![]), &cfg())
        .unwrap()
        .collection()
        .clone();
    store::delete_collection(&mut conn, &c.id, &c.name).unwrap();
    assert!(get(&conn, "a1").unwrap().is_none());
    let again = create(&mut conn, &new_own("a1", "A1", vec![]), &cfg()).unwrap();
    assert!(matches!(again, Ensured::Created(_)));
    assert_eq!(list(&conn).unwrap().len(), 1);
}

#[test]
fn own_name_falls_back_to_id_and_is_bounded() {
    assert_eq!(own_name("  ", "bot-1"), "bot-1 的資料");
    let long = "長".repeat(200);
    assert!(own_name(&long, "x").chars().count() <= 80);
    assert_eq!(base_slug("Sales_Bot.2"), "agent-sales-bot-2");
}
