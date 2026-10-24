use super::*;

fn candidate() -> Pending {
    Pending {
        home: PathBuf::from("/fixture"),
        user_id: "alice".into(),
        session_ref: uuid::Uuid::new_v4().to_string(),
        redirect_uri: "http://127.0.0.1:19111/api/me/google/callback".into(),
        dashboard_origin: "http://127.0.0.1:19111".into(),
        paste: false,
        code_verifier: "verifier-fixture".into(),
        client_id: "client-fixture".into(),
        client_secret: "secret-fixture".into(),
        epoch: 1,
        created: Instant::now(),
        code: None,
    }
}

#[test]
fn a_code_can_only_finish_for_the_same_person_and_login_once() {
    let pending = candidate();
    let session = pending.session_ref.clone();
    let state = insert(pending);
    receive_code(Path::new("/fixture"), &state, "code-fixture").unwrap();
    assert!(take(Path::new("/fixture"), &state, "bob", &session).is_err());
    assert!(
        take(
            Path::new("/fixture"),
            &state,
            "alice",
            &uuid::Uuid::new_v4().to_string()
        )
        .is_err()
    );
    assert!(take(Path::new("/different-home"), &state, "alice", &session).is_err());
    let result = take(Path::new("/fixture"), &state, "alice", &session).unwrap();
    assert_eq!(result.code.as_deref(), Some("code-fixture"));
    assert!(take(Path::new("/fixture"), &state, "alice", &session).is_err());
}

#[test]
fn callback_replay_expiry_and_disconnect_refuse() {
    let mut pending = candidate();
    let session = pending.session_ref.clone();
    pending.created = Instant::now() - TTL - Duration::from_secs(1);
    let state = insert(pending);
    assert!(receive_code(Path::new("/fixture"), &state, "code").is_err());
    assert!(take(Path::new("/fixture"), &state, "alice", &session).is_err());
    let state = insert(candidate());
    receive_code(Path::new("/fixture"), &state, "first").unwrap();
    assert!(receive_code(Path::new("/fixture"), &state, "replacement").is_err());
    cancel_user(Path::new("/fixture"), "alice");
    assert!(receive_code(Path::new("/fixture"), &state, "replacement").is_err());
}
#[test]
fn pasted_callback_requires_exact_redirect_and_same_session_before_recording_code() {
    let home = std::path::Path::new("/fixture/google-paste");
    let state = insert(Pending {
        home: home.into(),
        user_id: "alice".into(),
        session_ref: "session-a".into(),
        redirect_uri: "http://127.0.0.1:18789/api/me/google/callback".into(),
        dashboard_origin: "http://lan:18789".into(),
        paste: true,
        code_verifier: "verifier".into(),
        client_id: "client".into(),
        client_secret: "secret".into(),
        epoch: 1,
        created: Instant::now(),
        code: None,
    });
    let good = format!("http://127.0.0.1:18789/api/me/google/callback?state={state}&code=code-a");
    assert!(receive_pasted(home, "bob", "session-a", &good).is_err());
    assert!(receive_pasted(home, "alice", "session-b", &good).is_err());
    assert!(receive_pasted(home, "alice", "session-a", &good.replace(":18789", ":9999")).is_err());
    assert!(receive_pasted(home, "alice", "session-a", &format!("{good}&code=another")).is_err());
    assert_eq!(
        receive_pasted(home, "alice", "session-a", &good).unwrap(),
        state
    );
    assert_eq!(
        take(home, &state, "alice", "session-a")
            .unwrap()
            .code
            .as_deref(),
        Some("code-a")
    );
}
