use super::*;
use crate::session_origin::{OriginKind, SessionOrigin};
use duduclaw_auth::{AccessLevel, UserContext, UserRole};

fn employee(level: AccessLevel) -> UserContext {
    let mut ctx = UserContext::admin_fallback();
    ctx.user_id = "employee".into();
    ctx.role = UserRole::Employee;
    ctx.agent_access.insert("sales".into(), level);
    ctx
}
#[test]
fn bound_privacy_defaults_closed_and_explicit_legacy_toggle_is_live() {
    let home = tempfile::tempdir().unwrap();
    let p = home.path().join("config.toml");
    let viewer = employee(AccessLevel::Viewer);
    for raw in [
        None,
        Some(""),
        Some("[people]\nbound_history_privacy=true"),
        Some("bad = ["),
        Some("people=false"),
        Some("[people]\nbound_history_privacy='false'"),
    ] {
        if let Some(raw) = raw {
            std::fs::write(&p, raw).unwrap();
        }
        assert_eq!(mode(home.path(), &viewer, "sales").unwrap(), Mode::Filtered);
    }
    std::fs::write(&p, "[people]\nbound_history_privacy=false").unwrap();
    assert_eq!(mode(home.path(), &viewer, "sales").unwrap(), Mode::Full);
    std::fs::write(&p, "[people]\nbound_history_privacy=true").unwrap();
    assert_eq!(mode(home.path(), &viewer, "sales").unwrap(), Mode::Filtered);
}
#[test]
fn bound_privacy_preserves_owner_admin_and_never_turns_call_access_into_read_access() {
    let home = tempfile::tempdir().unwrap();
    assert_eq!(
        mode(home.path(), &employee(AccessLevel::Owner), "sales").unwrap(),
        Mode::Full
    );
    assert_eq!(
        mode(home.path(), &UserContext::admin_fallback(), "sales").unwrap(),
        Mode::Full
    );
    let mut ctx = employee(AccessLevel::Operator);
    ctx.agent_access.clear();
    ctx.role = UserRole::Manager;
    ctx.team_access.insert(
        "sales".into(),
        duduclaw_auth::TeamGrant {
            level: AccessLevel::Operator,
            via_user: "report".into(),
            depth: 1,
        },
    );
    assert!(ctx.can_call_agent("sales"));
    assert!(mode(home.path(), &ctx, "sales").is_err());
    std::fs::write(
        home.path().join("config.toml"),
        "[people]\nbound_history_privacy=false",
    )
    .unwrap();
    assert!(mode(home.path(), &ctx, "sales").is_err());
    std::fs::write(
        home.path().join("config.toml"),
        "[people]\nbound_history_privacy=true",
    )
    .unwrap();
    ctx = employee(AccessLevel::Viewer);
    ctx.must_change_password = true;
    assert!(mode(home.path(), &ctx, "sales").is_err());
    for level in [AccessLevel::Viewer, AccessLevel::Operator] {
        let ctx = employee(level);
        assert!(ctx.can_call_agent("sales"));
        assert_eq!(mode(home.path(), &ctx, "sales").unwrap(), Mode::Filtered);
    }
}
#[test]
fn bound_privacy_uses_host_origin_not_a_webchat_looking_id() {
    let own = SessionOrigin::webchat_user("employee");
    let other = SessionOrigin::webchat_user("other");
    assert_eq!(classify("webchat:any", Some(&own), "employee"), Class::Own);
    assert_eq!(
        classify("webchat:any", Some(&other), "employee"),
        Class::Hidden
    );
    assert_eq!(classify("webchat:any", None, "employee"), Class::Hidden);
    assert_eq!(
        classify(
            "webchat:any",
            Some(&SessionOrigin::of_kind(OriginKind::WebchatVisitor)),
            "employee"
        ),
        Class::External {
            channel: "widget".into()
        }
    );
    assert_eq!(
        classify(
            "whatsapp:+886912345678",
            Some(&SessionOrigin::of_kind(OriginKind::Channel)),
            "employee"
        ),
        Class::External {
            channel: "whatsapp".into()
        }
    );
    assert_eq!(
        classify(
            "webchat:any",
            Some(&SessionOrigin::of_kind(OriginKind::Internal)),
            "employee"
        ),
        Class::Hidden
    );
}
#[tokio::test]
async fn opaque_history_handle_is_stable_bound_to_home_user_and_not_an_access_grant() {
    let home = tempfile::tempdir().unwrap();
    let id = "whatsapp:+886912345678";
    let handle = handles::issue(home.path(), "employee", id).unwrap();
    assert!(handles::is_handle(&handle));
    assert!(!handle.contains("886912345678"));
    assert_eq!(handle, handles::issue(home.path(), "employee", id).unwrap());
    assert_ne!(handle, handles::issue(home.path(), "other", id).unwrap());
    assert_eq!(
        handles::resolve(home.path(), "employee", &handle)
            .await
            .unwrap(),
        id
    );
    assert!(
        handles::resolve(home.path(), "other", &handle)
            .await
            .is_err()
    );
    let other = tempfile::tempdir().unwrap();
    assert!(
        handles::resolve(other.path(), "employee", &handle)
            .await
            .is_err()
    );
    assert!(
        handles::resolve(home.path(), "employee", "view:v1:invalid")
            .await
            .is_err()
    );
    // Resolving a routing handle is never authorization; every API must then
    // use its live direct/oversight gate and stored origin classification.
    let mut revoked = employee(AccessLevel::Viewer);
    revoked.agent_access.clear();
    assert!(mode(home.path(), &revoked, "sales").is_err());
}

#[tokio::test]
async fn opaque_history_handles_separate_run_routes_from_session_routes() {
    let h = tempfile::tempdir().unwrap();
    let raw = "whatsapp:+886912345678#123";
    let s = handles::issue(h.path(), "me", raw).unwrap();
    let r = handles::issue_run(h.path(), "me", raw).unwrap();
    assert_ne!(s, r);
    assert!(handles::resolve(h.path(), "me", &r).await.is_err());
    assert!(handles::resolve_run(h.path(), "me", &s).await.is_err());
    assert_eq!(handles::resolve_run(h.path(), "me", &r).await.unwrap(), raw);
    let reference = handles::reference(h.path(), "me", raw).unwrap();
    assert!(!handles::is_handle(&reference));
    assert!(!reference.contains("886912345678"));
}

#[tokio::test]
async fn opaque_history_cursor_hides_raw_ids_and_is_a_separate_ephemeral_route() {
    let h = tempfile::tempdir().unwrap();
    let raw = "[timestamp,whatsapp:+886912345678]";
    let c = handles::issue_cursor(h.path(), "me", raw).unwrap();
    assert!(!c.contains("886912345678"));
    assert_eq!(
        handles::resolve_cursor(h.path(), "me", &c).await.unwrap(),
        raw
    );
    assert!(handles::resolve(h.path(), "me", &c).await.is_err());
    assert!(
        handles::resolve_cursor(h.path(), "other", &c)
            .await
            .is_err()
    );
}

#[test]
fn bound_own_projection_preserves_whole_text_and_attachment_lines_without_ner() {
    let home=tempfile::tempdir().unwrap();
    let original=format!("Own phone 0912-345-678 📎DELIVER:/own/customer-name.txt {}","字".repeat(40_000));
    let rows=vec![crate::session::SessionMessage{role:"assistant".into(),content:original.clone(),timestamp:"2026-10-24T00:00:00Z".into(),tokens:1}];
    let transcript=projection::project_sync(home.path(),rows,false,"sales").unwrap();
    assert_eq!(plain_parts(&transcript.messages[0].parts),original);
    assert!(!transcript.ner);
}
