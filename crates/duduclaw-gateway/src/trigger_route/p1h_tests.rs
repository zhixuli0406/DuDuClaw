//! v1.81 P1h (N1): calling (`@name`) and assigning (`/交辦`) another
//! person's Personal employee from a channel or WebChat. Real users.db
//! (hardened opener, private test home); the protected flag is given to
//! `AclRestriction::with_flag` — no test sets the process-wide flag or
//! installs the process-wide restriction (the `_under` predicates take it).
use std::sync::Arc;

use duduclaw_agent::registry::AgentRegistry;
use duduclaw_auth::GrantSource;
use duduclaw_auth::acl::PersonalRestriction;

use super::access::{Sender, SenderAccess};
use super::assign::{AssignRight, assign_right_under, assignment_mention_targets_under};
use crate::personal_visibility::AclRestriction;
use crate::personal_visibility::tests::{Home, PERSONAL, SHARED, home, private_store, record_personal};

/// alice's second Personal employee, opened by her with `public_trigger`.
const PUBLIC: &str = "alice-public";

async fn fixture() -> (Home, AgentRegistry) {
    let h = home();
    let agents = h.path().join("agents");
    super::tests::write_agent(&agents, PERSONAL, "Alice 私人助理", "@alice-personal", "specialist", "");
    super::tests::write_agent(&agents, SHARED, "團隊助理", "@team", "specialist", "");
    super::tests::write_agent(&agents, PUBLIC, "Alice 公開助理", "@alice-public", "specialist",
        "\n[channels]\npublic_trigger = true\n");
    record_personal(h.path(), PUBLIC, &h.alice);
    private_store(h.path());
    let mut reg = AgentRegistry::new(agents);
    reg.scan().await.unwrap();
    assert_eq!(reg.list().len(), 3);
    (h, reg)
}

fn sender(h: &Home, user: &str) -> SenderAccess {
    SenderAccess::from_effective(Arc::new(h.db.effective_access(user).unwrap().unwrap()))
}

#[tokio::test]
async fn p1h_channel_mention_of_another_owners_personal_employee_is_refused() {
    let (h, reg) = fixture().await;
    let on = AclRestriction::with_flag(h.path(), true);
    let r: Option<&dyn PersonalRestriction> = Some(&on);
    let agent = |id: &str| reg.get(id).unwrap();
    let (admin, alice, bob, unmapped) =
        (sender(&h, &h.admin), sender(&h, &h.alice), sender(&h, &h.bob), SenderAccess::unmapped());
    assert!(matches!(admin.sender, Sender::Admin { user_id: Some(_) }));

    // An Admin is refused another person's Personal employee — also when it
    // is the conversation's default employee; Shared employees are unchanged.
    assert_eq!(admin.grant_under(agent(PERSONAL), None, r), None);
    assert_eq!(admin.grant_under(agent(PERSONAL), Some(PERSONAL), r), None);
    assert!(!admin.may_call_under(agent(PERSONAL), None, r));
    assert_eq!(admin.grant_under(agent(SHARED), None, r), Some(GrantSource::Admin));
    // The owner calls it; the owner's manager and an unmapped sender do not.
    assert!(alice.may_call_under(agent(PERSONAL), None, r));
    assert_eq!(bob.grant_under(agent(PERSONAL), Some(PERSONAL), r), None);
    assert_eq!(unmapped.grant_under(agent(PERSONAL), Some(PERSONAL), r), None, "no person is no owner");
    // The owner-configured public trigger opens calling to everyone.
    for s in [&admin, &bob, &unmapped] {
        assert_eq!(s.grant_under(agent(PUBLIC), None, r), Some(GrantSource::Public));
    }
    assert!(alice.may_call_under(agent(PUBLIC), None, r));

    // A messaging-app sender mapped (verified) to the Admin account: same answer.
    h.db.bind_channel_identity(&h.admin, "line", "p1h-admin-line", true).unwrap();
    h.db.bind_channel_identity(&h.alice, "line", "p1h-alice-line", true).unwrap();
    private_store(h.path());
    let line_admin = super::access::lookup_channel_sender(h.path(), "line", "p1h-admin-line");
    assert!(matches!(line_admin.sender, Sender::Admin { .. }));
    assert_eq!(line_admin.grant_under(agent(PERSONAL), None, r), None);
    let line_alice = super::access::lookup_channel_sender(h.path(), "line", "p1h-alice-line");
    assert!(line_alice.may_call_under(agent(PERSONAL), None, r));

    // The synthetic "everyone" sender (is the bot addressed?) is not a person.
    let everyone = SenderAccess { sender: Sender::Admin { user_id: None }, webchat_scope: None };
    assert!(everyone.may_call_under(agent(PERSONAL), None, r));
    // Unprotected installs are unchanged; unreadable ownership refuses all.
    let off = AclRestriction::with_flag(h.path(), false);
    assert_eq!(admin.grant_under(agent(PERSONAL), None, Some(&off)), Some(GrantSource::Admin));
    let empty = crate::fs_safe::private_test_home().unwrap();
    let broken = AclRestriction::with_flag(empty.path(), true);
    assert_eq!(alice.grant_under(agent(PERSONAL), None, Some(&broken)), None);
}

#[tokio::test]
async fn p1h_channel_assignment_of_another_owners_personal_employee_is_refused() {
    let (h, reg) = fixture().await;
    let on = AclRestriction::with_flag(h.path(), true);
    let r: Option<&dyn PersonalRestriction> = Some(&on);
    let (admin, alice, bob) = (sender(&h, &h.admin), sender(&h, &h.alice), sender(&h, &h.bob));
    let right = |s: &SenderAccess, id: &str, fallback: Option<&str>| assign_right_under(&reg, s, id, fallback, r);

    assert_eq!(right(&admin, PERSONAL, None), AssignRight::Denied);
    assert_eq!(right(&admin, PERSONAL, Some(PERSONAL)), AssignRight::Denied, "the default opens nothing");
    assert_eq!(right(&admin, SHARED, None), AssignRight::Allowed(GrantSource::Admin));
    assert!(matches!(right(&alice, PERSONAL, None), AssignRight::Allowed(_)), "the owner assigns");
    assert_eq!(right(&bob, PERSONAL, None), AssignRight::Denied);
    // Public trigger: everyone may talk to it, only the owner hands it work.
    assert_eq!(right(&admin, PUBLIC, None), AssignRight::CallOnly);
    assert_eq!(right(&bob, PUBLIC, None), AssignRight::CallOnly);
    assert!(matches!(right(&alice, PUBLIC, None), AssignRight::Allowed(_)));
    assert_eq!(right(&SenderAccess::unmapped(), PERSONAL, None), AssignRight::NeedsIdentity);
    // Unprotected: unchanged.
    let off = AclRestriction::with_flag(h.path(), false);
    assert_eq!(assign_right_under(&reg, &admin, PERSONAL, None, Some(&off)), AssignRight::Allowed(GrantSource::Admin));
}

#[tokio::test]
async fn p1h_webchat_admin_assignment_of_another_owners_personal_employee_is_refused() {
    let (h, reg) = fixture().await;
    let on = AclRestriction::with_flag(h.path(), true);
    let r: Option<&dyn PersonalRestriction> = Some(&on);
    let web_admin = sender(&h, &h.admin).for_webchat(Ok(String::new()));
    let web_alice = sender(&h, &h.alice).for_webchat(Ok(String::new()));
    let allowed = |s: &SenderAccess, text: &str| crate::webchat::access::assignment_text_allowed(&reg, s, text, r);

    assert!(!allowed(&web_admin, "@alice-personal 整理本週報表"), "Admin is refused");
    assert!(allowed(&web_admin, "@team 整理本週報表"));
    assert!(allowed(&web_admin, "@nobody-here 整理"), "Admins keep the legacy detail for unknown names");
    assert!(allowed(&web_alice, "@alice-personal 整理本週報表"), "the owner is allowed");
    let ids = |s: &SenderAccess| -> Vec<String> {
        assignment_mention_targets_under(&reg, s, "webchat", None, r).into_iter().map(|t| t.id).collect()
    };
    assert!(!ids(&web_admin).iter().any(|id| id == PERSONAL || id == PUBLIC), "{:?}", ids(&web_admin));
    assert!(ids(&web_alice).iter().any(|id| id == PERSONAL));

    // The Admin early return of `authorize_command` is gone.
    let source = include_str!("../webchat/access.rs");
    let start = source.find("async fn authorize_command(").unwrap();
    let end = start + source[start..].find("async fn target_error(").unwrap();
    assert!(!source[start..end].contains("UserRole::Admin { return Ok(()); }"));
    assert!(source[start..end].contains("assignment_text_allowed(&reg, &sender, text, duduclaw_auth::acl::personal_restriction())"));
}

fn candidate(
    reg: &AgentRegistry,
    s: &SenderAccess,
    id: &str,
    r: Option<&dyn PersonalRestriction>,
) -> bool {
    crate::webchat::access::primary_candidate_under(s, reg.get(id).unwrap(), r)
}

#[tokio::test]
async fn p1h_primary_candidate_refuses_another_owners_personal_employee() {
    let (h, reg) = fixture().await;
    let on = AclRestriction::with_flag(h.path(), true);
    let r: Option<&dyn PersonalRestriction> = Some(&on);
    let (admin, bob) = (sender(&h, &h.admin), sender(&h, &h.bob));

    // alice-public has public_trigger on: still not bob's (alice's manager)
    // primary employee.
    assert!(!candidate(&reg, &bob, PUBLIC, r), "public_trigger opens calling, not routing");
    assert!(!candidate(&reg, &bob, PERSONAL, r));
    assert!(!candidate(&reg, &admin, PERSONAL, r));
    assert!(!candidate(&reg, &admin, PUBLIC, r));
    assert!(!candidate(&reg, &SenderAccess::unmapped(), PUBLIC, r));
    assert!(candidate(&reg, &admin, SHARED, r), "Shared employees are unchanged");
}

#[tokio::test]
async fn p1h_primary_candidate_allows_own_personal_employee() {
    let (h, reg) = fixture().await;
    let on = AclRestriction::with_flag(h.path(), true);
    let r: Option<&dyn PersonalRestriction> = Some(&on);
    let alice = sender(&h, &h.alice);
    assert!(candidate(&reg, &alice, PERSONAL, r));
    assert!(candidate(&reg, &alice, PUBLIC, r));
    assert!(candidate(&reg, &alice, SHARED, r));
}

#[tokio::test]
async fn p1h_primary_candidate_refuses_when_ownership_unreadable() {
    let (h, reg) = fixture().await;
    let empty = crate::fs_safe::private_test_home().unwrap();
    let broken = AclRestriction::with_flag(empty.path(), true);
    let r: Option<&dyn PersonalRestriction> = Some(&broken);
    let alice = sender(&h, &h.alice);
    assert!(!candidate(&reg, &alice, PERSONAL, r), "the owner is refused too");
    assert!(!candidate(&reg, &alice, PUBLIC, r));
}

#[tokio::test]
async fn p1h_primary_candidate_unrestricted_matches_previous_rules() {
    let (h, reg) = fixture().await;
    let (admin, alice, bob) = (sender(&h, &h.admin), sender(&h, &h.alice), sender(&h, &h.bob));
    // No restriction installed: the pre-P1 rules for a Shared employee.
    assert!(candidate(&reg, &admin, SHARED, None));
    assert!(candidate(&reg, &alice, SHARED, None));
    assert!(candidate(&reg, &bob, SHARED, None), "team-derived Operator may assign");
    assert!(!candidate(&reg, &SenderAccess::unmapped(), SHARED, None));
    // An unprotected install (flag off) is the same.
    let off = AclRestriction::with_flag(h.path(), false);
    assert!(candidate(&reg, &admin, SHARED, Some(&off)));
    assert!(candidate(&reg, &admin, PERSONAL, Some(&off)));
}

/// N1 trace: a default-routed channel turn (no `@`) on a Personal employee
/// is authorized by the isolated runtime only for its owner. The channel
/// ingress identity is the verified mapped account (`ingress::reply_scope`;
/// an unmapped sender captures nothing), and `record_authorization` →
/// `issuance_facts` → `personal_facts` requires that principal to be the
/// lifecycle owner, the mirror owner and the employee's only member.
#[test]
fn p1h_runtime_refuses_a_non_owner_foreground_turn() {
    let authority = include_str!("../isolation_mcp/authority.rs");
    let at = authority.find("pub(crate) fn personal_facts(").unwrap();
    let body: String = authority[at..].chars().take(700).collect();
    assert!(body.contains("user_id.as_str() != owner") && body.contains("current.personal_owner.as_deref() != Some(owner)"));
    let boot = include_str!("../isolation_mcp/boot.rs");
    assert!(boot.contains("personal_facts(agent, owner.as_str(), principal, current)"));
    let ingress = include_str!("../isolation_mcp/ingress.rs");
    assert!(ingress.contains("RequesterIdentity::ChannelUser { channel, sender, user_id: Some(user), .. }"));
    assert!(ingress.contains("_ => return Err(VerificationError::Invalid)"));
}
