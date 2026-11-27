use super::*;
use crate::{otp::ChannelProofStatus, test_support::{test_db, TestDbPath}, UserRole};

const SESSION: &str = "authenticated-session-hash";
const PRINCIPAL: &str = "12344821";

#[test]
fn chat_link_dashboard_lists_are_owner_scoped() {
    let (db, _home, owner) = fixture();
    let other = admin(&db, "other@example.com");
    let waiting = begin(&db, &owner);
    let matched = begin(&db, &owner);
    receive(&db, &matched.code, PRINCIPAL, true);
    let foreign = begin(&db, &other);
    db.bind_channel_identity(&owner, "telegram", "pending-1", false).unwrap();
    db.bind_channel_identity(&owner, "googlechat", "users/1", true).unwrap();
    db.bind_channel_identity(&other, "telegram", "other-1", true).unwrap();
    let rows = db.channel_identities_for_user(&owner).unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|r| r.user_id == owner));
    assert!(rows.iter().any(|r| !r.verified));
    let pending = pending_ids(&db, &owner).unwrap();
    assert_eq!(pending.len(), 2);
    assert!(pending.contains(&waiting.handshake_id));
    assert!(pending.contains(&matched.handshake_id));
    assert!(!pending.contains(&foreign.handshake_id));
    cancel(&db, &owner, &waiting.handshake_id).unwrap();
    db.conn().execute("UPDATE chat_link_handshakes SET expires_at=0 WHERE id=?1",
        [&matched.handshake_id]).unwrap();
    assert!(pending_ids(&db, &owner).unwrap().is_empty());
}

fn admin(db: &UserDb, email: &str) -> String {
    db.create_user(email, "Admin", "pw-unused-123456", UserRole::Admin).unwrap().id
}

fn fixture() -> (UserDb, TestDbPath, String) {
    let (db, home) = test_db();
    let uid = admin(&db, "admin@example.com");
    (db, home, uid)
}

fn begin(db: &UserDb, uid: &str) -> StartedHandshake {
    start(db, uid, "telegram", SESSION).unwrap()
}

fn receive(db: &UserDb, code: &str, principal: &str, private: bool) -> MatchOutcome {
    match_code(db, "telegram", &parse_code(code).unwrap(), principal, "bot", principal, "王小明（@wang）", private).unwrap()
}

fn prove(db: &UserDb, uid: &str) -> ChatApprovalProof {
    let h = begin(db, uid);
    assert_eq!(receive(db, &h.code, PRINCIPAL, true), MatchOutcome::Matched);
    confirm(db, uid, &h.handshake_id, SESSION).unwrap()
}

#[test]
fn clear_approval_proof_keeps_binding_and_is_owner_scoped_and_idempotent() {
    let (db, _home, owner) = fixture();
    let original = prove(&db, &owner);
    let other = admin(&db, "other@example.com");
    let h = begin(&db, &other);
    assert_eq!(receive(&db, &h.code, "98765432", true), MatchOutcome::Matched);
    let other_proof = confirm(&db, &other, &h.handshake_id, SESSION).unwrap();
    let before = db.channel_identities_for_user(&owner).unwrap().pop().unwrap();
    let generation = db.acl_generation().unwrap();
    assert!(!db.clear_approval_proof(&other, "telegram", PRINCIPAL).unwrap());
    assert!(!db.clear_approval_proof(&owner, "googlechat", PRINCIPAL).unwrap());
    assert!(!db.clear_approval_proof(&owner, "telegram", "missing").unwrap());
    assert_eq!(db.approval_proof(&owner, "telegram").unwrap(), Some(original));
    assert_eq!(db.acl_generation().unwrap(), generation);
    assert!(db.clear_approval_proof(&owner, "telegram", PRINCIPAL).unwrap());
    assert_eq!(db.acl_generation().unwrap(), generation + 1);
    let rows = db.channel_identities_for_user(&owner).unwrap();
    assert_eq!(rows.len(), 1);
    assert!(rows[0].verified);
    assert_eq!(rows[0].user_id, before.user_id);
    assert_eq!(rows[0].channel_user_id, before.channel_user_id);
    assert_eq!(rows[0].created_at, before.created_at);
    assert_eq!(db.find_verified_user_id_by_channel("telegram", PRINCIPAL).unwrap(), Some(owner.clone()));
    let cleared: bool = db.conn().query_row(
        "SELECT proof_method IS NULL AND proof_user_id IS NULL AND proven_at IS NULL
            AND proof_account IS NULL AND proof_dm IS NULL AND verified_at IS NOT NULL
         FROM channel_identities WHERE user_id=?1 AND channel='telegram' AND channel_user_id=?2",
        params![owner, PRINCIPAL], |r| r.get(0),
    ).unwrap();
    assert!(cleared);
    assert!(db.approval_proof(&owner, "telegram").unwrap().is_none());
    assert_eq!(db.channel_identity_proof_status(&owner, "telegram", PRINCIPAL).unwrap(), Some(ChannelProofStatus::AdminPrefilled));
    assert_eq!(db.approval_proof(&other, "telegram").unwrap(), Some(other_proof));
    assert!(!db.clear_approval_proof(&owner, "telegram", PRINCIPAL).unwrap());
    assert_eq!(db.acl_generation().unwrap(), generation + 1);
}

#[test]
fn clear_approval_proof_rolls_back_when_acl_bump_fails() {
    let (db, _home, owner) = fixture();
    let proof = prove(&db, &owner);
    // Use an authorizer-independent trigger to fail the shared generation write.
    db.conn().execute_batch("CREATE TRIGGER fail_proof_acl BEFORE UPDATE ON auth_meta
        BEGIN SELECT RAISE(ABORT, 'fixture failure'); END;").unwrap();
    assert!(db.clear_approval_proof(&owner, "telegram", PRINCIPAL).is_err());
    assert_eq!(db.approval_proof(&owner, "telegram").unwrap(), Some(proof));
}

#[test]
fn proof_migration_idempotent_on_fresh_and_old_db() {
    let (db, home, uid) = fixture();
    db.bind_channel_identity(&uid, "telegram", PRINCIPAL, true).unwrap();
    drop(db);
    let db = UserDb::new(home.path()).unwrap();
    // Reconstruct the pre-RM-143 identity schema with a populated legacy row.
    for column in ["proof_method", "proof_user_id", "proven_at", "proof_account", "proof_dm"] {
        db.conn().execute(&format!("ALTER TABLE channel_identities DROP COLUMN {column}"), []).unwrap();
    }
    db.conn().execute_batch("DROP TABLE chat_link_handshakes; DROP TABLE chat_link_attempts;").unwrap();
    drop(db);
    for _ in 0..2 {
        let db = UserDb::new(home.path()).unwrap();
        assert_eq!(db.find_verified_user_id_by_channel("telegram", PRINCIPAL).unwrap(), Some(uid.clone()));
        assert!(db.approval_proof(&uid, "telegram").unwrap().is_none());
        for column in ["proof_method", "proof_user_id", "proven_at", "proof_account", "proof_dm"] {
            assert!(crate::user_row::has_column(&db.conn(), "channel_identities", column).unwrap());
        }
        migrate(&db.conn()).unwrap();
        migrate(&db.conn()).unwrap();
    }
}

#[test]
fn prefilled_binding_cannot_decide() {
    let (db, _home, uid) = fixture();
    db.bind_channel_identity_authorized(&uid, "telegram", PRINCIPAL, true, Some(&uid)).unwrap();
    assert!(db.approval_proof(&uid, "telegram").unwrap().is_none());
    assert!(db.approval_proofs(&uid).unwrap().is_empty());
    assert_eq!(db.channel_identity_proof_status(&uid, "telegram", PRINCIPAL).unwrap(), Some(ChannelProofStatus::AdminPrefilled));
    assert_eq!(db.find_verified_user_id_by_channel("telegram", PRINCIPAL).unwrap(), Some(uid));
}

#[test]
fn rebind_to_other_user_clears_proof() {
    let (db, _home, uid) = fixture();
    let other = admin(&db, "other@example.com");
    let proof = prove(&db, &uid);
    db.bind_channel_identity(&uid, "telegram", PRINCIPAL, true).unwrap();
    assert_eq!(db.approval_proof(&uid, "telegram").unwrap(), Some(proof));
    assert!(db.bind_channel_identity(&other, "telegram", PRINCIPAL, true).is_err());
    // Only an unverified row can be reassigned by the existing admin API.
    db.bind_channel_identity(&uid, "telegram", PRINCIPAL, false).unwrap();
    db.bind_channel_identity_authorized(&other, "telegram", PRINCIPAL, true, Some(&uid)).unwrap();
    let cleared: bool = db.conn().query_row("SELECT proof_method IS NULL AND proof_user_id IS NULL
        AND proven_at IS NULL AND proof_account IS NULL AND proof_dm IS NULL
        FROM channel_identities WHERE channel='telegram' AND channel_user_id=?1",
        [PRINCIPAL], |r| r.get(0)).unwrap();
    assert!(cleared);
    assert!(db.approval_proof(&other, "telegram").unwrap().is_none());
}

#[test]
fn downgrade_residue_is_not_proof() {
    let (db, _home, uid) = fixture();
    let other = admin(&db, "other@example.com");
    prove(&db, &uid);
    db.conn().execute("UPDATE channel_identities SET user_id=?1", [&other]).unwrap();
    assert!(db.approval_proof(&other, "telegram").unwrap().is_none());
    assert!(db.approval_proof(&uid, "telegram").unwrap().is_none());
    assert_eq!(db.channel_identity_proof_status(&other, "telegram", PRINCIPAL).unwrap(), Some(ChannelProofStatus::AdminPrefilled));
}

#[test]
fn proof_list_and_unbind_preserve_existing_uses() {
    let (db, _home, uid) = fixture();
    prove(&db, &uid);
    let h = start(&db, &uid, "googlechat", SESSION).unwrap();
    assert_eq!(match_code(&db, "googlechat", &parse_code(&h.code).unwrap(), "users/99", "chat-app", "spaces/dm", "Google user", true).unwrap(), MatchOutcome::Matched);
    let google = confirm(&db, &uid, &h.handshake_id, SESSION).unwrap();
    assert_eq!(google.proof_dm, "spaces/dm");
    assert_eq!(db.approval_proofs(&uid).unwrap().len(), 2);
    assert_eq!(db.channel_identity_proof_status(&uid, "telegram", PRINCIPAL).unwrap(), Some(ChannelProofStatus::SelfConfirmed));
    // Evidence queries intentionally leave the live-role decision to the gateway.
    db.conn().execute("UPDATE users SET role='manager' WHERE id=?1", [&uid]).unwrap();
    assert!(db.approval_proof(&uid, "telegram").unwrap().is_some());
    db.unbind_channel_identity("telegram", PRINCIPAL).unwrap();
    assert!(db.approval_proof(&uid, "telegram").unwrap().is_none());
    assert_eq!(db.channel_identity_proof_status(&uid, "telegram", PRINCIPAL).unwrap(), None);
}

#[test]
fn malformed_proof_and_read_errors_fail_closed() {
    let (db, _home, uid) = fixture();
    prove(&db, &uid);
    db.conn().execute("UPDATE channel_identities SET proof_method='unknown'", []).unwrap();
    assert!(db.approval_proofs(&uid).unwrap().is_empty());
    db.conn().execute("UPDATE channel_identities SET proof_method='chat_handshake',proof_dm=NULL", []).unwrap();
    assert!(db.approval_proofs(&uid).unwrap().is_empty());
    db.conn().execute("DROP TABLE channel_identities", []).unwrap();
    assert!(db.approval_proof(&uid, "telegram").is_err());
}

#[test]
fn code_parser_requires_whole_message_and_normalizes_aliases() {
    let code = parse_code(" \nDuDu-oIl0-abcz\t").unwrap();
    assert_eq!(code.0, "DUDU-0110-ABCZ");
    for invalid in ["DUDU-1234-5678 extra", "prefix DUDU-1234-5678", "DUDU-1234-5678\nDUDU-1234-5678",
        "DUDU-123-5678", "DUDU-12345-5678", "DUDU-1234-567U", "DUDU-１２３４-5678",
        "DUDU-1234-5678-", "DUDU-12 4-5678", "DU DU-1234-5678"] {
        assert!(parse_code(invalid).is_none(), "accepted invalid format");
    }
}

#[test]
fn matched_code_needs_dashboard_confirmation() {
    let (db, _home, uid) = fixture();
    let h = begin(&db, &uid);
    assert!((TTL_SECS - 2..=TTL_SECS).contains(&(h.expires_at - Utc::now().timestamp())));
    let generation = db.acl_generation().unwrap();
    assert_eq!(receive(&db, &h.code, PRINCIPAL, true), MatchOutcome::Matched);
    assert!(db.find_user_id_by_channel("telegram", PRINCIPAL).unwrap().is_none());
    assert!(db.approval_proof(&uid, "telegram").unwrap().is_none());
    let state = status(&db, &uid, &h.handshake_id).unwrap().unwrap();
    assert_eq!(state.status, HandshakeStatus::Matched);
    assert_eq!(state.masked_target.as_deref(), Some("••••4821"));
    assert_eq!(state.matched_display.as_deref(), Some("王小明（@wang）"));
    let proof = confirm(&db, &uid, &h.handshake_id, SESSION).unwrap();
    assert_eq!(proof.channel_user_id, PRINCIPAL);
    assert_eq!(db.approval_proof(&uid, "telegram").unwrap(), Some(proof));
    assert_eq!(db.acl_generation().unwrap(), generation + 1);
}

#[test]
fn handshake_single_use() {
    let (db, _home, uid) = fixture();
    let h = begin(&db, &uid);
    assert_eq!(receive(&db, &h.code, PRINCIPAL, true), MatchOutcome::Matched);
    assert_eq!(receive(&db, &h.code, "another-sender", true), MatchOutcome::InvalidOrExpired);
    confirm(&db, &uid, &h.handshake_id, SESSION).unwrap();
    let generation = db.acl_generation().unwrap();
    assert!(confirm(&db, &uid, &h.handshake_id, SESSION).is_err());
    assert_eq!(receive(&db, &h.code, PRINCIPAL, true), MatchOutcome::InvalidOrExpired);
    assert!(!reject(&db, &uid, &h.handshake_id).unwrap());
    assert_eq!(status(&db, &uid, &h.handshake_id).unwrap().unwrap().status, HandshakeStatus::Confirmed);
    assert_eq!(db.acl_generation().unwrap(), generation);
}

#[test]
fn handshake_expiry_in_waiting_and_matched_states() {
    let (db, _home, uid) = fixture();
    let h = begin(&db, &uid);
    db.conn().execute("UPDATE chat_link_handshakes SET expires_at=?1", [Utc::now().timestamp()]).unwrap();
    assert_eq!(receive(&db, &h.code, PRINCIPAL, true), MatchOutcome::InvalidOrExpired);
    assert_eq!(status(&db, &uid, &h.handshake_id).unwrap().unwrap().status, HandshakeStatus::Expired);
    let h = begin(&db, &uid);
    assert_eq!(receive(&db, &h.code, PRINCIPAL, true), MatchOutcome::Matched);
    db.conn().execute("UPDATE chat_link_handshakes SET expires_at=?1", [Utc::now().timestamp()]).unwrap();
    assert!(confirm(&db, &uid, &h.handshake_id, SESSION).is_err());
    let stored: String = db.conn().query_row("SELECT status FROM chat_link_handshakes WHERE id=?1", [&h.handshake_id], |r| r.get(0)).unwrap();
    assert_eq!(stored, "expired");
    assert!(db.approval_proofs(&uid).unwrap().is_empty());
}

#[test]
fn group_paste_voids_code() {
    let (db, _home, uid) = fixture();
    let h = begin(&db, &uid);
    assert_eq!(receive(&db, &h.code, PRINCIPAL, false), MatchOutcome::MustUsePrivateChat);
    assert_eq!(receive(&db, &h.code, PRINCIPAL, true), MatchOutcome::InvalidOrExpired);
    assert!(confirm(&db, &uid, &h.handshake_id, SESSION).is_err());
    assert_eq!(status(&db, &uid, &h.handshake_id).unwrap().unwrap().status, HandshakeStatus::Rejected);
}

#[test]
fn handshake_attempts_throttled_per_sender_and_globally() {
    let (db, _home, uid) = fixture();
    let h = begin(&db, &uid);
    let wrong = "DUDU-0000-0000";
    // Ensure the extremely unlikely generated all-zero code is not the test guess.
    let wrong = if h.code == wrong { "DUDU-1111-1111" } else { wrong };
    for _ in 0..5 { assert_eq!(receive(&db, wrong, PRINCIPAL, true), MatchOutcome::InvalidOrExpired); }
    // Even a correct code in a group is not looked up while silenced.
    assert_eq!(receive(&db, &h.code, PRINCIPAL, false), MatchOutcome::Throttled);
    assert_eq!(status(&db, &uid, &h.handshake_id).unwrap().unwrap().status, HandshakeStatus::Waiting);
    let sender_until: i64 = db.conn().query_row("SELECT MAX(blocked_until) FROM chat_link_attempts WHERE sender_key=?1",
        [sender_key(&db, "telegram", PRINCIPAL)], |r| r.get(0)).unwrap();
    assert!((3598..=3600).contains(&(sender_until - Utc::now().timestamp())));
    for i in 0..45 { assert_eq!(receive(&db, wrong, &format!("sender-{i}"), true), MatchOutcome::InvalidOrExpired); }
    assert_eq!(receive(&db, &h.code, "fresh-sender", true), MatchOutcome::Throttled);
    let global_until: i64 = db.conn().query_row("SELECT MAX(blocked_until) FROM chat_link_attempts WHERE sender_key=?1",
        [global_key(&db)], |r| r.get(0)).unwrap();
    assert!((598..=600).contains(&(global_until - Utc::now().timestamp())));
    // Simulate the global cooldown ending, while preserving the sender cooldown.
    db.conn().execute("UPDATE chat_link_attempts SET window_start=window_start-601,
        blocked_until=CASE WHEN sender_key=?1 THEN 0 ELSE blocked_until END", [global_key(&db)]).unwrap();
    assert_eq!(receive(&db, &h.code, PRINCIPAL, true), MatchOutcome::Throttled);
    assert_eq!(receive(&db, &h.code, "fresh-sender", true), MatchOutcome::Matched);
}

#[test]
fn throttle_uses_rolling_window_and_survives_reopen() {
    let (db, home, uid) = fixture();
    let h = begin(&db, &uid);
    let wrong = if h.code == "DUDU-0000-0000" { "DUDU-1111-1111" } else { "DUDU-0000-0000" };
    for _ in 0..4 { receive(&db, wrong, PRINCIPAL, true); }
    db.conn().execute("UPDATE chat_link_attempts SET window_start=window_start-590", []).unwrap();
    receive(&db, wrong, PRINCIPAL, true);
    drop(db);
    let db = UserDb::new(home.path()).unwrap();
    assert_eq!(receive(&db, &h.code, PRINCIPAL, true), MatchOutcome::Throttled);
    db.conn().execute("UPDATE chat_link_attempts SET window_start=window_start-3601,blocked_until=0", []).unwrap();
    assert_eq!(receive(&db, &h.code, PRINCIPAL, true), MatchOutcome::Matched);
}

#[test]
fn handshake_start_limits_waiting_and_rolling_hour() {
    let (db, _home, uid) = fixture();
    let first = begin(&db, &uid);
    let second = begin(&db, &uid);
    assert!(start(&db, &uid, "googlechat", SESSION).is_err());
    assert!(cancel(&db, &uid, &first.handshake_id).unwrap());
    assert!(cancel(&db, &uid, &second.handshake_id).unwrap());
    for _ in 0..4 {
        let h = begin(&db, &uid);
        cancel(&db, &uid, &h.handshake_id).unwrap();
    }
    assert!(start(&db, &uid, "telegram", SESSION).is_err());
    db.conn().execute("UPDATE chat_link_handshakes SET created_at=created_at-3601", []).unwrap();
    begin(&db, &uid);
}

#[test]
fn mfa_enforced_admin_without_factor_refused() {
    let (db, _home, uid) = fixture();
    db.conn().execute("UPDATE mfa_policy_state SET enforced=1 WHERE id=1", []).unwrap();
    assert!(start(&db, &uid, "telegram", SESSION).is_err());
}

#[test]
fn non_admin_inactive_and_unknown_channel_refused() {
    let (db, _home, uid) = fixture();
    for channel in ["line", "slack", "discord", "Telegram", "telegram.evil", ""] {
        assert!(start(&db, &uid, channel, SESSION).is_err());
    }
    assert!(start(&db, &uid, "telegram", "").is_err());
    assert!(start(&db, "missing", "telegram", SESSION).is_err());
    for role in ["manager", "employee"] {
        db.conn().execute("UPDATE users SET role=?1 WHERE id=?2", params![role, uid]).unwrap();
        assert!(start(&db, &uid, "telegram", SESSION).is_err());
    }
    db.conn().execute("UPDATE users SET role='admin',status='suspended' WHERE id=?1", [&uid]).unwrap();
    assert!(start(&db, &uid, "telegram", SESSION).is_err());
}

#[test]
fn confirm_with_other_session_refused() {
    let (db, _home, uid) = fixture();
    let h = begin(&db, &uid);
    receive(&db, &h.code, PRINCIPAL, true);
    assert!(confirm(&db, &uid, &h.handshake_id, "other-session").is_err());
    assert!(confirm(&db, &uid, &h.handshake_id, "").is_err());
    assert!(db.approval_proofs(&uid).unwrap().is_empty());
    confirm(&db, &uid, &h.handshake_id, SESSION).unwrap();
}

#[test]
fn confirm_by_other_user_refused() {
    let (db, _home, uid) = fixture();
    let other = admin(&db, "other@example.com");
    let h = begin(&db, &uid);
    receive(&db, &h.code, PRINCIPAL, true);
    assert!(confirm(&db, &other, &h.handshake_id, SESSION).is_err());
    assert!(status(&db, &other, &h.handshake_id).unwrap().is_none());
    assert!(!reject(&db, &other, &h.handshake_id).unwrap());
    confirm(&db, &uid, &h.handshake_id, SESSION).unwrap();
}

#[test]
fn confirm_rechecks_admin_and_mfa() {
    let (db, _home, uid) = fixture();
    let h = begin(&db, &uid);
    receive(&db, &h.code, PRINCIPAL, true);
    db.conn().execute("UPDATE users SET role='manager' WHERE id=?1", [&uid]).unwrap();
    assert!(confirm(&db, &uid, &h.handshake_id, SESSION).is_err());
    db.conn().execute("UPDATE users SET role='admin',status='suspended' WHERE id=?1", [&uid]).unwrap();
    assert!(confirm(&db, &uid, &h.handshake_id, SESSION).is_err());
    db.conn().execute("UPDATE users SET status='active' WHERE id=?1", [&uid]).unwrap();
    db.conn().execute("UPDATE mfa_policy_state SET enforced=1 WHERE id=1", []).unwrap();
    assert!(confirm(&db, &uid, &h.handshake_id, SESSION).is_err());
    assert!(db.approval_proofs(&uid).unwrap().is_empty());
}

#[test]
fn conflicting_owner_and_other_channel_are_generic_failures() {
    let (db, _home, uid) = fixture();
    let other = admin(&db, "other@example.com");
    db.bind_channel_identity(&other, "telegram", PRINCIPAL, true).unwrap();
    let h = begin(&db, &uid);
    assert_eq!(receive(&db, &h.code, PRINCIPAL, true), MatchOutcome::InvalidOrExpired);
    assert_eq!(match_code(&db, "googlechat", &parse_code(&h.code).unwrap(), "users/99", "app", "spaces/dm", "display", true).unwrap(), MatchOutcome::InvalidOrExpired);
    assert_eq!(status(&db, &uid, &h.handshake_id).unwrap().unwrap().status, HandshakeStatus::Waiting);
    db.unbind_channel_identity("telegram", PRINCIPAL).unwrap();
    assert_eq!(receive(&db, &h.code, PRINCIPAL, true), MatchOutcome::Matched);
    db.bind_channel_identity(&other, "telegram", PRINCIPAL, true).unwrap();
    assert!(confirm(&db, &uid, &h.handshake_id, SESSION).is_err());
    assert_eq!(db.find_verified_user_id_by_channel("telegram", PRINCIPAL).unwrap(), Some(other));
}

#[test]
fn rejected_match_and_cancel_are_terminal() {
    let (db, _home, uid) = fixture();
    let h = begin(&db, &uid);
    receive(&db, &h.code, PRINCIPAL, true);
    assert!(reject(&db, &uid, &h.handshake_id).unwrap());
    assert!(confirm(&db, &uid, &h.handshake_id, SESSION).is_err());
    assert_eq!(receive(&db, &h.code, PRINCIPAL, true), MatchOutcome::InvalidOrExpired);
    let h = begin(&db, &uid);
    assert!(cancel(&db, &uid, &h.handshake_id).unwrap());
    assert_eq!(receive(&db, &h.code, PRINCIPAL, true), MatchOutcome::InvalidOrExpired);
}

#[test]
fn raw_code_never_stored_and_digest_is_domain_separated() {
    let (db, _home, uid) = fixture();
    let h = begin(&db, &uid);
    let conn = db.conn();
    let mut stmt = conn.prepare("SELECT * FROM chat_link_handshakes WHERE id=?1").unwrap();
    let count = stmt.column_count();
    let mut rows = stmt.query([&h.handshake_id]).unwrap();
    let row = rows.next().unwrap().unwrap();
    for index in 0..count {
        match row.get_ref(index).unwrap() {
            rusqlite::types::ValueRef::Text(value) | rusqlite::types::ValueRef::Blob(value) => {
                assert!(!value.windows(h.code.len()).any(|window| window == h.code.as_bytes()));
            }
            _ => {}
        }
    }
    let digest: Vec<u8> = conn.query_row("SELECT code_digest FROM chat_link_handshakes WHERE id=?1", [&h.handshake_id], |r| r.get(0)).unwrap();
    assert_eq!(digest.len(), 32);
    assert!(hmac::verify(&db.remember_key, &code_message(&parse_code(&h.code).unwrap()), &digest).is_ok());
    assert!(hmac::verify(&db.remember_key, h.code.as_bytes(), &digest).is_err());
}

#[test]
fn confirm_failure_rolls_back_binding_acl_and_handshake() {
    let (db, _home, uid) = fixture();
    let h = begin(&db, &uid);
    receive(&db, &h.code, PRINCIPAL, true);
    let generation = db.acl_generation().unwrap();
    db.conn().execute_batch("CREATE TRIGGER fail_chat_confirm BEFORE UPDATE OF status ON chat_link_handshakes
        WHEN NEW.status='confirmed' BEGIN SELECT RAISE(ABORT,'injected failure'); END;").unwrap();
    assert!(confirm(&db, &uid, &h.handshake_id, SESSION).is_err());
    assert!(db.find_user_id_by_channel("telegram", PRINCIPAL).unwrap().is_none());
    assert_eq!(db.acl_generation().unwrap(), generation);
    assert_eq!(status(&db, &uid, &h.handshake_id).unwrap().unwrap().status, HandshakeStatus::Matched);
}

#[test]
fn purge_stale_retains_proof_and_expires_pending() {
    let (db, _home, uid) = fixture();
    let proof = prove(&db, &uid);
    db.conn().execute("UPDATE chat_link_handshakes SET created_at=created_at-86401", []).unwrap();
    let h = begin(&db, &uid);
    db.conn().execute("UPDATE chat_link_handshakes SET expires_at=?1 WHERE id=?2", params![Utc::now().timestamp(), h.handshake_id]).unwrap();
    let old = Utc::now().timestamp() - RETENTION_SECS - 1;
    db.conn().execute("INSERT INTO chat_link_attempts(sender_key,window_start,failures) VALUES(?1,?2,1)", params![global_key(&db), old]).unwrap();
    assert_eq!(purge_stale(&db).unwrap(), 2);
    assert_eq!(status(&db, &uid, &h.handshake_id).unwrap().unwrap().status, HandshakeStatus::Expired);
    assert_eq!(db.approval_proof(&uid, "telegram").unwrap(), Some(proof));
}

#[test]
fn missing_mfa_or_throttle_store_denies() {
    let (db, _home, uid) = fixture();
    let h = begin(&db, &uid);
    db.conn().execute("DELETE FROM mfa_policy_state", []).unwrap();
    assert!(start(&db, &uid, "telegram", SESSION).is_err());
    db.conn().execute("DROP TABLE chat_link_attempts", []).unwrap();
    assert!(match_code(&db, "telegram", &parse_code(&h.code).unwrap(), PRINCIPAL, "bot", PRINCIPAL, "display", true).is_err());
}
