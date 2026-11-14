//! RM-152: Google Chat sender recognition.
use super::*;
use crate::trigger_route::access;
use duduclaw_auth::{AccessLevel, UserRole, UserStatus};
use duduclaw_knowledge::requester::RequesterIdentity;

// ── pure helpers ────────────────────────────────────────────────

#[test]
fn normalisation_has_one_canonical_form() {
    assert_eq!(normalize_user_id("users/123456").as_deref(), Some("users/123456"));
    assert_eq!(normalize_user_id("123456").as_deref(), Some("users/123456"));
    for bad in [
        "",
        "users/",
        "users/app",
        "users/abc123",
        "users/alice@example.com",
        "alice@example.com",
        "users/users/123",
        " users/123",
        "users/123 ",
        "USERS/123",
        "user/123",
        "spaces/123",
        "users/12-3",
        "users/١٢٣", // non-ASCII digits
        "unknown",
    ] {
        assert_eq!(normalize_user_id(bad), None, "{bad:?}");
    }
    let long = format!("users/{}", "9".repeat(MAX_ID_LEN + 1));
    assert_eq!(normalize_user_id(&long), None);
    let max = format!("users/{}", "9".repeat(MAX_ID_LEN));
    assert_eq!(normalize_user_id(&max), Some(max.clone()));
}

#[test]
fn sub_comparison_is_exact_on_the_whole_id() {
    assert!(google_sub_matches("123456", "users/123456"));
    assert!(google_sub_matches("123456", "123456"));
    // A longer id sharing a prefix, or a shorter one, never matches.
    assert!(!google_sub_matches("123456", "users/1234567"));
    assert!(!google_sub_matches("1234567", "users/123456"));
    assert!(!google_sub_matches("12345", "users/123456"));
    assert!(!google_sub_matches("23456", "users/123456"));
    // Non-numeric or empty values never match anything.
    assert!(!google_sub_matches("", "users/"));
    assert!(!google_sub_matches("abc", "users/abc"));
    assert!(!google_sub_matches("users/123456", "users/123456"));
    assert!(!google_sub_matches("123456", "users/123456 "));
}

#[test]
fn resolution_refuses_ambiguity_and_disagreement() {
    let s = |v: &str| v.to_string();
    assert_eq!(resolve(None, vec![]).unwrap(), None);
    assert_eq!(resolve(Some(s("a")), vec![]).unwrap(), Some(s("a")));
    assert_eq!(resolve(None, vec![s("a")]).unwrap(), Some(s("a")));
    assert_eq!(resolve(Some(s("a")), vec![s("a")]).unwrap(), Some(s("a")));
    assert!(resolve(Some(s("a")), vec![s("b")]).is_err());
    assert!(resolve(None, vec![s("a"), s("b")]).is_err());
}

// ── fixtures ────────────────────────────────────────────────────

struct Home {
    dir: tempfile::TempDir,
    db: duduclaw_auth::UserDb,
}

impl Home {
    /// Google-account-derived recognition turned ON (the opt-in most of the
    /// tests below are about). [`Home::default_config`] is the shipped
    /// default (no `config.toml`, derivation off).
    fn new() -> Self {
        let h = Self::default_config();
        super::test_support::set_derivation(h.path(), true);
        h
    }
    fn default_config() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let db = duduclaw_auth::UserDb::new(&dir.path().join("users.db")).unwrap();
        Self { dir, db }
    }
    fn path(&self) -> &Path {
        self.dir.path()
    }
    fn user(&self, email: &str, role: UserRole) -> String {
        let id = self.db.create_user(email, email, "Passw0rd-123456", role).unwrap().id;
        self.db.bind_agent(&id, "finance", AccessLevel::Operator).unwrap();
        id
    }
    /// Linked Google accounts as `person_credentials` stores them (owner,
    /// sub, status). Only clear metadata; the sealed secret is a stub.
    fn link_google(&self, records: &[(&str, &str, &str)]) {
        super::test_support::link_google(self.path(), records);
    }
    fn corrupt_google_links(&self) {
        let file = crate::person_credentials::path(self.path());
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, b"{ not json").unwrap();
    }
    fn sender(&self, id: &str) -> Option<String> {
        access::verified_user_for_channel_sender(self.path(), CHANNEL, id)
    }
    fn requester(&self, id: &str) -> RequesterIdentity {
        crate::knowledge::requester_map::map_requester("googlechat:spaces/AAAA", id, None, |channel, sender| {
            access::verified_user_for_channel_sender(self.path(), channel, sender)
        })
    }
    fn gate(&self, requesters: crate::google_identity::Requesters, id: &str) -> bool {
        let people = crate::google_identity::requester_gate::UserDbPeople(std::sync::Arc::new(
            duduclaw_auth::UserDb::new(&self.path().join("users.db")).unwrap(),
        ));
        crate::google_identity::requester_gate::check(
            requesters,
            &self.requester(id),
            "finance",
            "shared@c.test",
            &people,
        )
        .is_ok()
    }
}

// ── recognition ─────────────────────────────────────────────────

#[tokio::test]
async fn own_google_account_sender_passes_bound_people() {
    let h = Home::new();
    let alice = h.user("alice@c.test", UserRole::Employee);
    h.link_google(&[(&alice, "111222333", "connected")]);
    assert_eq!(h.sender("users/111222333"), Some(alice.clone()));
    match h.requester("users/111222333") {
        RequesterIdentity::ChannelUser { user_id, channel, .. } => {
            assert_eq!(channel, "googlechat");
            assert_eq!(user_id, Some(alice));
        }
        other => panic!("expected a channel user, got {other:?}"),
    }
    assert!(h.gate(crate::google_identity::Requesters::BoundPeople, "users/111222333"));
}

#[tokio::test]
async fn admin_bound_sender_passes_bound_people_without_any_google_link() {
    let h = Home::new();
    let admin = h.user("admin@c.test", UserRole::Admin);
    let bob = h.user("bob@c.test", UserRole::Employee);
    h.db.bind_channel_identity_authorized(&bob, CHANNEL, "users/444555", true, Some(&admin))
        .unwrap();
    assert_eq!(h.sender("users/444555"), Some(bob));
    assert!(h.gate(crate::google_identity::Requesters::BoundPeople, "users/444555"));
}

#[tokio::test]
async fn unbound_and_near_miss_senders_are_denied() {
    let h = Home::new();
    let alice = h.user("alice@c.test", UserRole::Employee);
    h.link_google(&[(&alice, "111222333", "connected")]);
    for id in [
        "users/999",
        "users/1112223334", // longer id sharing the prefix
        "users/11122233",   // shorter id
        "users/111222333/", // trailing junk
        "users/person0@c.test", // the linked account's e-mail is never a key
        "unknown",
        "",
    ] {
        assert_eq!(h.sender(id), None, "{id:?}");
        assert!(!h.gate(crate::google_identity::Requesters::BoundPeople, id), "{id:?}");
    }
    // `anyone` behaves as before for an unbound sender.
    assert!(h.gate(crate::google_identity::Requesters::Anyone, "users/999"));
}

#[tokio::test]
async fn pending_admin_row_is_not_an_identity() {
    let h = Home::new();
    let bob = h.user("bob@c.test", UserRole::Employee);
    h.db.bind_channel_identity(&bob, CHANNEL, "users/777", false).unwrap();
    assert_eq!(h.sender("users/777"), None);
}

#[tokio::test]
async fn lookup_errors_fail_closed() {
    let h = Home::new();
    let admin = h.user("admin@c.test", UserRole::Admin);
    let bob = h.user("bob@c.test", UserRole::Employee);
    h.db.bind_channel_identity_authorized(&bob, CHANNEL, "users/444555", true, Some(&admin))
        .unwrap();
    h.db.bind_channel_identity_authorized(&bob, "line", "U-bob", true, Some(&admin))
        .unwrap();
    h.corrupt_google_links();
    // Google Chat cannot be resolved safely: nobody is recognised.
    assert_eq!(h.sender("users/444555"), None);
    assert!(!h.gate(crate::google_identity::Requesters::BoundPeople, "users/444555"));
    // Other channels do not read the Google links and are unchanged.
    assert_eq!(
        access::verified_user_for_channel_sender(h.path(), "line", "U-bob"),
        Some(bob)
    );
    // No identity store at all: unmapped.
    let empty = tempfile::tempdir().unwrap();
    assert_eq!(access::verified_user_for_channel_sender(empty.path(), CHANNEL, "users/1"), None);
}

#[tokio::test]
async fn two_sources_naming_different_people_recognise_nobody() {
    let h = Home::new();
    let alice = h.user("alice@c.test", UserRole::Employee);
    let bob = h.user("bob@c.test", UserRole::Employee);
    h.link_google(&[(&alice, "555", "connected")]);
    // Written below the admin endpoint (which refuses it) to model a race.
    h.db.bind_channel_identity(&bob, CHANNEL, "users/555", true).unwrap();
    assert_eq!(h.sender("users/555"), None);
    // Two accounts linked to the same Google account: ambiguous.
    let h = Home::new();
    let alice = h.user("alice@c.test", UserRole::Employee);
    let bob = h.user("bob@c.test", UserRole::Employee);
    h.link_google(&[(&alice, "666", "connected"), (&bob, "666", "connected")]);
    assert_eq!(h.sender("users/666"), None);
}

#[tokio::test]
async fn admin_bind_conflict_protects_the_google_owner() {
    let h = Home::new();
    let alice = h.user("alice@c.test", UserRole::Employee);
    let bob = h.user("bob@c.test", UserRole::Employee);
    h.link_google(&[(&alice, "888", "connected")]);
    assert!(admin_bind_conflict(h.path(), &h.db, "users/888", &bob).unwrap().is_some());
    assert!(admin_bind_conflict(h.path(), &h.db, "users/888", &alice).unwrap().is_none());
    assert!(admin_bind_conflict(h.path(), &h.db, "users/8888", &bob).unwrap().is_none());
    h.corrupt_google_links();
    assert!(admin_bind_conflict(h.path(), &h.db, "users/888", &bob).is_err());
}

// ── removal ─────────────────────────────────────────────────────

#[tokio::test]
async fn disconnecting_google_removes_recognition() {
    let h = Home::new();
    let alice = h.user("alice@c.test", UserRole::Employee);
    h.link_google(&[(&alice, "123123", "connected")]);
    assert_eq!(h.sender("users/123123"), Some(alice.clone()));
    // Through the trigger route too (it must not serve a cached mapping).
    assert_ne!(
        access::for_channel_sender(h.path(), CHANNEL, "users/123123"),
        access::SenderAccess::unmapped()
    );
    crate::person_credentials::disconnect(h.path(), &alice).unwrap();
    assert_eq!(h.sender("users/123123"), None);
    assert_eq!(
        access::for_channel_sender(h.path(), CHANNEL, "users/123123"),
        access::SenderAccess::unmapped()
    );
    assert!(!h.gate(crate::google_identity::Requesters::BoundPeople, "users/123123"));
}

#[tokio::test]
async fn suspended_offboarded_or_deleted_accounts_are_not_recognised() {
    for status in [UserStatus::Suspended, UserStatus::Offboarded] {
        let h = Home::new();
        let admin = h.user("admin@c.test", UserRole::Admin);
        let alice = h.user("alice@c.test", UserRole::Employee);
        h.link_google(&[(&alice, "321", "connected")]);
        h.db.bind_channel_identity_authorized(&alice, CHANNEL, "users/654", true, Some(&admin))
            .unwrap();
        assert_eq!(h.sender("users/321"), Some(alice.clone()));
        h.db.set_user_status(&alice, status).unwrap();
        assert_eq!(h.sender("users/321"), None, "{status:?} via Google");
        assert_eq!(h.sender("users/654"), None, "{status:?} via admin binding");
    }
    // The linked record itself suspended (account suspension sync).
    let h = Home::new();
    let alice = h.user("alice@c.test", UserRole::Employee);
    h.link_google(&[(&alice, "321", "suspended")]);
    assert_eq!(h.sender("users/321"), None);
    // A record left behind by a deleted account is nobody's identity.
    let h = Home::new();
    let alice = h.user("alice@c.test", UserRole::Employee);
    h.link_google(&[(&alice, "321", "connected")]);
    rusqlite::Connection::open(h.path().join("users.db"))
        .unwrap()
        .execute("DELETE FROM users WHERE id = ?1", [&alice])
        .unwrap();
    assert_eq!(h.sender("users/321"), None);
}

#[tokio::test]
async fn admin_unbind_removes_recognition() {
    let h = Home::new();
    let admin = h.user("admin@c.test", UserRole::Admin);
    let bob = h.user("bob@c.test", UserRole::Employee);
    h.db.bind_channel_identity_authorized(&bob, CHANNEL, "users/1010", true, Some(&admin))
        .unwrap();
    assert_eq!(h.sender("users/1010"), Some(bob.clone()));
    assert!(h.db.unbind_channel_identity_authorized(&bob, CHANNEL, "users/1010", &admin).unwrap());
    assert_eq!(h.sender("users/1010"), None);
}

#[tokio::test]
async fn other_channels_are_answered_exactly_as_before() {
    let h = Home::new();
    let admin = h.user("admin@c.test", UserRole::Admin);
    let bob = h.user("bob@c.test", UserRole::Employee);
    // A Google link with a numeric sub must not leak into another channel.
    h.link_google(&[(&bob, "424242", "connected")]);
    assert_eq!(verified_user_id(h.path(), &h.db, "line", "424242").unwrap(), None);
    assert_eq!(verified_user_id(h.path(), &h.db, "line", "users/424242").unwrap(), None);
    // And no normalisation is applied to another channel's ids.
    h.db.bind_channel_identity_authorized(&bob, "telegram", "424242", true, Some(&admin))
        .unwrap();
    assert_eq!(verified_user_id(h.path(), &h.db, "telegram", "424242").unwrap(), Some(bob));
    assert_eq!(verified_user_id(h.path(), &h.db, "telegram", "users/424242").unwrap(), None);
}

// ── M4: Google-account-derived recognition is opt-in ───────────

#[tokio::test]
async fn derivation_is_off_by_default_and_only_a_literal_true_turns_it_on() {
    let h = Home::default_config();
    let alice = h.user("alice@c.test", UserRole::Employee);
    let bob = h.user("bob@c.test", UserRole::Employee);
    h.link_google(&[(&alice, "111222333", "connected")]);
    // Off (no config.toml): the sub match is not an identity anywhere.
    assert_eq!(derivation_enabled(h.path()), Ok(false));
    assert_eq!(h.sender("users/111222333"), None);
    assert!(!h.gate(crate::google_identity::Requesters::BoundPeople, "users/111222333"));
    assert_eq!(google_account_chat_user(h.path(), &alice).unwrap(), None);
    assert!(!any_derived_identity(h.path(), &[alice.as_str()]).unwrap());
    // …and it cannot block an administrator binding either.
    assert!(admin_bind_conflict(h.path(), &h.db, "users/111222333", &bob).unwrap().is_none());

    // Explicitly absent or explicitly false: off.
    for off in [
        "",
        "[other]\nx = 1\n",
        "[google]\nchat_read = true\n",
        "[google]\nchat_identity_from_account = false\n",
    ] {
        std::fs::write(h.path().join("config.toml"), off).unwrap();
        assert_eq!(derivation_enabled(h.path()), Ok(false), "{off:?}");
        assert_eq!(h.sender("users/111222333"), None, "{off:?}");
    }

    // Malformed settings are errors, never "off" (the fail-closed
    // consequences are covered by `config_errors_fail_closed_*` below).
    for bad in [
        "[google]\nchat_identity_from_account = 'true'\n",
        "[google]\nchat_identity_from_account = 1\n",
        "google = true\n",
        "[google]\nchat_identity_from_account = true\ninvalid = [\n",
    ] {
        std::fs::write(h.path().join("config.toml"), bad).unwrap();
        assert!(derivation_enabled(h.path()).is_err(), "{bad:?}");
        assert_eq!(h.sender("users/111222333"), None, "{bad:?}");
    }
    std::fs::remove_file(h.path().join("config.toml")).unwrap();
    std::fs::create_dir(h.path().join("config.toml")).unwrap(); // not a readable file
    assert!(derivation_enabled(h.path()).is_err());
    assert_eq!(h.sender("users/111222333"), None);
    std::fs::remove_dir(h.path().join("config.toml")).unwrap();

    // On: recognised.
    super::test_support::set_derivation(h.path(), true);
    assert_eq!(derivation_enabled(h.path()), Ok(true));
    assert_eq!(h.sender("users/111222333"), Some(alice.clone()));
    // Turning it off again takes effect on the next lookup.
    super::test_support::set_derivation(h.path(), false);
    assert_eq!(h.sender("users/111222333"), None);
}

#[tokio::test]
async fn off_means_only_administrator_bindings_are_recognised() {
    let h = Home::default_config();
    let admin = h.user("admin@c.test", UserRole::Admin);
    let alice = h.user("alice@c.test", UserRole::Employee);
    let bob = h.user("bob@c.test", UserRole::Employee);
    h.link_google(&[(&alice, "555", "connected")]);
    h.db.bind_channel_identity_authorized(&bob, CHANNEL, "users/555", true, Some(&admin))
        .unwrap();
    // The Google link is ignored: the administrator binding stands alone.
    assert_eq!(h.sender("users/555"), Some(bob));
    // With derivation on the two sources disagree and nobody is recognised.
    super::test_support::set_derivation(h.path(), true);
    assert_eq!(h.sender("users/555"), None);
}

#[tokio::test]
async fn the_setting_is_written_through_config_commit_and_preserves_the_file() {
    let h = Home::default_config();
    std::fs::write(h.path().join("config.toml"), "# keep\n[google]\nchat_read = true\n").unwrap();
    assert!(set_derivation_enabled(h.path(), true).unwrap());
    assert_eq!(derivation_enabled(h.path()), Ok(true));
    assert!(!set_derivation_enabled(h.path(), true).unwrap(), "unchanged value is not a change");
    let text = std::fs::read_to_string(h.path().join("config.toml")).unwrap();
    assert!(text.contains("# keep") && text.contains("chat_read = true"), "{text}");
    assert!(set_derivation_enabled(h.path(), false).unwrap());
    assert_eq!(derivation_enabled(h.path()), Ok(false));
    // A malformed file or value is refused, not overwritten.
    for bad in ["[google]\nchat_identity_from_account = 'yes'\n", "invalid = ["] {
        std::fs::write(h.path().join("config.toml"), bad).unwrap();
        assert!(set_derivation_enabled(h.path(), true).is_err(), "{bad:?}");
        assert_eq!(std::fs::read_to_string(h.path().join("config.toml")).unwrap(), bad);
    }
}

// ── H1: every channel-side identity read goes through the resolver ──

/// Admin binding `users/777` → Admin A, while user B's own Google `sub` is
/// `777`. The resolver recognises nobody; install sign-off must not fall
/// back to the administrator binding and approve with A's role.
#[tokio::test]
async fn conflicting_sources_cannot_sign_off_an_install() {
    let h = Home::new();
    let admin_a = h.user("a@c.test", UserRole::Admin);
    let b = h.user("b@c.test", UserRole::Employee);
    h.link_google(&[(&b, "777", "connected")]);
    // Written below the admin endpoint (which refuses it) to model a race.
    h.db.bind_channel_identity(&admin_a, CHANNEL, "users/777", true).unwrap();
    assert_eq!(h.sender("users/777"), None);

    let store = crate::install_requests::InstallRequestStore::open(h.path()).unwrap();
    let id = store
        .create(
            "skill", "t", "d", "mgr", "mgr@c.test", "manager", None, "Low",
            &serde_json::json!([]), &serde_json::json!({}), 3600,
        )
        .await
        .unwrap();
    let refusal = crate::decision_notify::refusal_text(crate::decision_notify::PressAuth::DenyUnknown, "核准");
    let out = crate::install_notify::apply_decision(h.path(), CHANNEL, "users/777", &id, true).await;
    assert_eq!(out, Err(refusal.clone()));
    // Also through the unified press router.
    let wire = crate::decision_action::encode(
        crate::decision_action::DecisionSource::Install,
        crate::decision_action::DecisionAct::Approve,
        &id,
    );
    let routed = crate::decision_notify::route_press(h.path(), CHANNEL, "users/777", &wire).await;
    assert_eq!(routed, Some(Err(refusal)));
    let req = store.get(&id).await.unwrap().unwrap();
    assert_eq!(req.status, crate::install_requests::RequestStatus::Pending);
    assert!(req.admin_by.is_none());
    // The decider name and takeover display do not attribute it to A either.
    assert_eq!(crate::decision_card::resolve_decider_name(h.path(), CHANNEL, "users/777"), None);
    assert_eq!(crate::takeover::manager_display_name(h.path(), CHANNEL, "users/777"), None);
}

#[tokio::test]
async fn conflicting_sources_cannot_give_digest_feedback() {
    let h = Home::new();
    let admin_a = h.user("a@c.test", UserRole::Admin);
    let b = h.user("b@c.test", UserRole::Employee);
    h.link_google(&[(&b, "777", "connected")]);
    h.db.bind_channel_identity(&admin_a, CHANNEL, "users/777", true).unwrap();
    let unknown =
        crate::decision_notify::refusal_text(crate::decision_notify::PressAuth::DenyUnknown, "給回饋");
    let press = || {
        crate::digest::apply_channel_feedback(
            h.path(),
            CHANNEL,
            "users/777",
            "20261008.1",
            crate::decision_action::DecisionAct::Up,
        )
    };
    // Refused at the identity step (before any digest is read).
    assert_eq!(press().await, Err(unknown.clone()));
    assert!(!h.path().join("feedback.jsonl").exists());
    // Control: with derivation off the administrator binding alone names A,
    // so the press gets past identity (and is refused later, for want of a
    // saved digest) — proving the refusal above came from the resolver.
    super::test_support::set_derivation(h.path(), false);
    let later = press().await;
    assert!(later.is_err());
    assert_ne!(later, Err(unknown));
}

// ── H2: "is there a recognisable approver" ──────────────────────

#[tokio::test]
async fn recognisable_approver_predicate_counts_identities_not_destinations() {
    use crate::decision_notify::{PressAuth, approver_links, authorize_press, has_recognisable_approver_identity as recognisable};
    // No users.db at all: solo deployment.
    let empty = tempfile::tempdir().unwrap();
    assert!(!recognisable(empty.path()));

    // An Admin recognised ONLY through the Google-derived identity, setting on.
    let h = Home::new();
    let admin = h.user("admin@c.test", UserRole::Admin);
    h.link_google(&[(&admin, "4242", "connected")]);
    assert!(approver_links(h.path()).is_empty(), "no notification destination");
    assert!(recognisable(h.path()));
    // An unrecognised sender who is the exact delivery destination is refused.
    assert_eq!(authorize_press(None, recognisable(h.path()), true), PressAuth::DenyUnknown);
    // Setting off: the derived identity is ignored.
    super::test_support::set_derivation(h.path(), false);
    assert!(!recognisable(h.path()));
    assert_eq!(authorize_press(None, recognisable(h.path()), true), PressAuth::Allow);
    super::test_support::set_derivation(h.path(), true);
    // Duplicate owners (the admin would not be recognised) still count.
    let emp = h.user("emp@c.test", UserRole::Employee);
    h.link_google(&[(&admin, "4242", "connected"), (&emp, "4242", "connected")]);
    assert_eq!(h.sender("users/4242"), None);
    assert!(recognisable(h.path()));
    // A store read error is fail closed.
    h.corrupt_google_links();
    assert!(recognisable(h.path()));
    // Only an employee is derived: no approver identity.
    h.link_google(&[(&emp, "4242", "connected")]);
    assert!(!recognisable(h.path()));
    // A suspended linked record is not usable.
    h.link_google(&[(&admin, "4242", "suspended")]);
    assert!(!recognisable(h.path()));

    // An administrator-bound Google Chat identity counts (recognition), even
    // though it is never a notification destination.
    let h = Home::default_config();
    let admin = h.user("admin@c.test", UserRole::Admin);
    let mgr = h.user("mgr@c.test", UserRole::Manager);
    assert!(!recognisable(h.path()));
    h.db.bind_channel_identity_authorized(&mgr, CHANNEL, "users/9001", true, Some(&admin)).unwrap();
    assert!(recognisable(h.path()));
    assert!(approver_links(h.path()).is_empty());
    // …but not once that manager is suspended.
    h.db.set_user_status(&mgr, UserStatus::Suspended).unwrap();
    assert!(!recognisable(h.path()));
    // A telegram row of an approver is both an identity and a destination.
    h.db.bind_channel_identity_authorized(&admin, "telegram", "tg-1", true, Some(&admin)).unwrap();
    assert!(recognisable(h.path()));
    assert_eq!(approver_links(h.path()), vec![("telegram".to_string(), "tg-1".to_string())]);
    // A corrupt users.db is fail closed.
    let bad = tempfile::tempdir().unwrap();
    std::fs::write(bad.path().join("users.db"), b"not a database").unwrap();
    assert!(recognisable(bad.path()));
}

// ── M1 (round 2): a config read failure is not "derivation off" ─

/// An Admin recognised ONLY through the Google-derived identity, and an
/// employee recognised only through an administrator binding. With the
/// setting unreadable, malformed or of the wrong type, the resolver must
/// deny every Google Chat sender and the "recognisable approver" predicate
/// must stay true, so an unmapped presser at the exact delivery destination
/// is still refused.
#[tokio::test]
async fn config_errors_fail_closed_in_the_resolver_and_the_approver_predicate() {
    use crate::decision_notify::{PressAuth, authorize_press, has_recognisable_approver_identity as recognisable, mapped_role};
    let h = Home::new();
    let admin = h.user("admin@c.test", UserRole::Admin);
    let bob = h.user("bob@c.test", UserRole::Employee);
    h.link_google(&[(&admin, "4242", "connected")]);
    h.db.bind_channel_identity_authorized(&bob, CHANNEL, "users/5151", true, Some(&admin))
        .unwrap();
    // Control: setting on, both recognised, approver recognisable.
    assert_eq!(h.sender("users/4242"), Some(admin.clone()));
    assert_eq!(h.sender("users/5151"), Some(bob.clone()));
    assert!(recognisable(h.path()));

    let config = h.path().join("config.toml");
    let cases: [(&str, Option<&str>); 3] = [
        ("unreadable config (a directory)", None),
        ("malformed TOML", Some("[google]\nchat_identity_from_account = true\ninvalid = [\n")),
        ("wrong type", Some("[google]\nchat_identity_from_account = \"yes\"\n")),
    ];
    for (label, content) in cases {
        if config.is_dir() {
            std::fs::remove_dir(&config).unwrap();
        } else if config.exists() {
            std::fs::remove_file(&config).unwrap();
        }
        match content {
            Some(text) => std::fs::write(&config, text).unwrap(),
            None => std::fs::create_dir(&config).unwrap(),
        }
        assert!(derivation_enabled(h.path()).is_err(), "{label}");
        // (a) The resolver denies: no Google Chat sender is recognised, not
        // even through an administrator binding.
        assert!(verified_user_id(h.path(), &h.db, CHANNEL, "users/4242").is_err(), "{label}");
        assert!(verified_user_id(h.path(), &h.db, CHANNEL, "users/5151").is_err(), "{label}");
        assert_eq!(h.sender("users/4242"), None, "{label}");
        assert_eq!(h.sender("users/5151"), None, "{label}");
        assert_eq!(mapped_role(h.path(), CHANNEL, "users/4242"), None, "{label}");
        assert!(!h.gate(crate::google_identity::Requesters::BoundPeople, "users/5151"), "{label}");
        assert!(admin_bind_conflict(h.path(), &h.db, "users/4242", &bob).is_err(), "{label}");
        assert!(any_derived_identity(h.path(), &[admin.as_str()]).is_err(), "{label}");
        // (b) The predicate stays true: the destination fallback stays off.
        assert!(recognisable(h.path()), "{label}");
        assert_eq!(authorize_press(None, recognisable(h.path()), true), PressAuth::DenyUnknown, "{label}");
    }
    std::fs::remove_file(&config).unwrap();

    // Explicitly absent key (file present, `[google]` present): off, as
    // before — the derived-only Admin is not counted, the administrator
    // binding alone stands.
    std::fs::write(&config, "[google]\nchat_read = true\n").unwrap();
    assert_eq!(derivation_enabled(h.path()), Ok(false));
    assert!(!recognisable(h.path()));
    assert_eq!(authorize_press(None, recognisable(h.path()), true), PressAuth::Allow);
    assert_eq!(h.sender("users/4242"), None);
    assert_eq!(h.sender("users/5151"), Some(bob.clone()));
    // Absent file: same.
    std::fs::remove_file(&config).unwrap();
    assert_eq!(derivation_enabled(h.path()), Ok(false));
    assert!(!recognisable(h.path()));
    assert_eq!(h.sender("users/5151"), Some(bob));
}

// ── L6: an identity is not a DM destination ─────────────────────

#[tokio::test]
async fn a_googlechat_identity_is_never_an_outbound_destination() {
    let h = Home::default_config();
    let admin = h.user("admin@c.test", UserRole::Admin);
    h.db.bind_channel_identity_authorized(&admin, CHANNEL, "users/31337", true, Some(&admin)).unwrap();
    assert!(crate::decision_notify::approver_links(h.path()).is_empty());
    assert!(crate::macos_folder_access::admin_links(h.path()).is_empty());
    assert!(crate::notify_push::admin_targets(h.path()).unwrap().is_empty());
    assert!(crate::workflow::workflow_notify::admin_links(h.path()).is_empty());
    // The identity list still shows it.
    assert_eq!(h.db.verified_channels_for_user(&admin).unwrap().len(), 1);
    // A real DM channel next to it is still a destination.
    h.db.bind_channel_identity_authorized(&admin, "line", "U-admin", true, Some(&admin)).unwrap();
    let expected = vec![("line".to_string(), "U-admin".to_string())];
    assert_eq!(crate::decision_notify::approver_links(h.path()), expected);
    assert_eq!(crate::notify_push::admin_targets(h.path()).unwrap(), expected);
}

#[tokio::test]
async fn the_dashboard_list_shows_the_google_derived_identity() {
    let h = Home::new();
    let alice = h.user("alice@c.test", UserRole::Employee);
    assert_eq!(google_account_chat_user(h.path(), &alice).unwrap(), None);
    h.link_google(&[(&alice, "13579", "needs_reconnect")]);
    let (chat, _) = google_account_chat_user(h.path(), &alice).unwrap().unwrap();
    assert_eq!(chat, "users/13579");
    h.link_google(&[(&alice, "13579", "suspended")]);
    assert_eq!(google_account_chat_user(h.path(), &alice).unwrap(), None);
}
