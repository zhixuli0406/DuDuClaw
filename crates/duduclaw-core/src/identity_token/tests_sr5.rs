//! SR5 item 6: in protected mode a label without a confirmed row gets no
//! v1 token and no grace; an `eph-…` child is bound to its root's live tuple.
use super::*;
use rusqlite::{params, Connection};

const ROOT: &str = "alice-desk";
const ROOT_ID: [u8; 16] = [0xa1; 16];
const CHILD: &str = "eph-0123456789ab";

fn key() -> Vec<u8> { (0u8..32).collect() }

fn private(path: &Path, mode: u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }
    #[cfg(not(unix))]
    let _ = (path, mode);
}

/// A home with the employee index holding `ROOT` (live unless `disabled`)
/// and the identity key, and an ephemeral child `CHILD` of `ROOT`.
fn home(disabled: bool) -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join(APPLY_DB_DIR);
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join(APPLY_DB_FILE);
    let db = Connection::open(&path).unwrap();
    db.execute_batch("CREATE TABLE confirmed_employees (
        label TEXT PRIMARY KEY,operation TEXT NOT NULL UNIQUE,digest BLOB NOT NULL,
        receipt TEXT NOT NULL,tenant_id TEXT,incarnation INTEGER NOT NULL DEFAULT 0);").unwrap();
    let receipt = serde_json::json!({"label": ROOT, "employee": ROOT_ID, "uid": 70_001, "bytes": 1u64,
        "inodes": 1, "disabled": disabled}).to_string();
    db.execute("INSERT INTO confirmed_employees(label,operation,digest,receipt) VALUES(?1,'op',x'00',?2)",
        params![ROOT, receipt]).unwrap();
    drop(db);
    private(&path, 0o600);
    let child = home.path().join("agents").join(".ephemeral").join(CHILD);
    std::fs::create_dir_all(&child).unwrap();
    std::fs::write(child.join("agent.toml"), "[agent]\nname = \"eph\"\n").unwrap();
    std::fs::write(child.join("ephemeral.toml"), format!("parent = \"{ROOT}\"\ntier = \"standard\"\n\
        created_at = \"2026-11-09T00:00:00Z\"\nexpires_at = \"2026-11-10T00:00:00Z\"\n")).unwrap();
    {
        let org = crate::org_store::lock_org_mutations(home.path()).unwrap();
        crate::org_store::upsert(&org, CHILD, crate::org_store::OrgEntry::new(ROOT, "")).unwrap();
    }
    std::fs::write(home.path().join(IDENTITY_KEY_FILE), key()).unwrap();
    private(&home.path().join(IDENTITY_KEY_FILE), 0o600);
    home
}

fn root_tuple() -> AgentIncarnation { AgentIncarnation { employee: ROOT_ID, incarnation: 0 } }

fn grace(home: &Path) {
    std::fs::write(home.join("config.toml"), format!(
        "[delegation]\nrequire_identity_token = false\nidentity_token_grace_until = {:?}\n",
        (chrono::Utc::now() + chrono::Duration::days(1)).to_rfc3339())).unwrap();
}

#[test]
fn sr5_protected_mode_refuses_labels_without_a_confirmed_row() {
    let h = home(false);
    let k = key();
    // Unprotected: a label without a row keeps v1, exactly as before.
    assert_eq!(token_binding(h.path(), "legacy-bot", false), Ok(TokenBinding::V1));
    assert_eq!(current_agent_token_with(h.path(), &k, "legacy-bot", false), Some(mint_token(&k, "legacy-bot")));
    // Protected: no token, and a v1 token never verifies — not even with grace.
    grace(h.path());
    assert_eq!(token_binding(h.path(), "legacy-bot", true), Ok(TokenBinding::Refused));
    assert_eq!(current_agent_token_with(h.path(), &k, "legacy-bot", true), None);
    let v1 = mint_token(&k, "legacy-bot");
    assert_eq!(verify_claim_with(h.path(), "legacy-bot", &v1, false, true), IdentityVerdict::Rejected);
    assert_eq!(verify_claim_with(h.path(), "legacy-bot", "", false, true), IdentityVerdict::Rejected,
        "grace never applies in protected mode");
    assert_eq!(verify_claim_with(h.path(), "", "", false, true), IdentityVerdict::Rejected, "blank claim");
    // The same v1 token under grace outside protected mode still passes (unchanged).
    assert_eq!(verify_claim_with(h.path(), "legacy-bot", &v1, false, false), IdentityVerdict::Verified);
    assert_eq!(verify_claim_with(h.path(), "legacy-bot", "", false, false), IdentityVerdict::Unverified);
    // The live protected employee itself: v2 only.
    let v2 = mint_token_v2(&k, ROOT, &root_tuple());
    assert_eq!(verify_claim_with(h.path(), ROOT, &v2, false, true), IdentityVerdict::Verified);
    assert_eq!(verify_claim_with(h.path(), ROOT, &mint_token(&k, ROOT), false, true), IdentityVerdict::Rejected);
    // The gateway's signed cold-start probe keeps its v1 tag.
    assert_eq!(token_binding(h.path(), DOCTOR_PROBE, true), Ok(TokenBinding::V1));
}

#[test]
fn sr5_ephemeral_children_carry_v2_tokens_bound_to_their_root() {
    let h = home(false);
    let k = key();
    // Protected: the child is bound to the root's live tuple.
    assert_eq!(token_binding(h.path(), CHILD, true), Ok(TokenBinding::V2(root_tuple())));
    let token = current_agent_token_with(h.path(), &k, CHILD, true).unwrap();
    assert_eq!(token, mint_token_v2(&k, CHILD, &root_tuple()));
    assert!(token.starts_with(AGENT_TOKEN_V2_PREFIX));
    assert_eq!(verify_claim_with(h.path(), CHILD, &token, true, true), IdentityVerdict::Verified);
    // Its v1 tag, or the root's own v2 token presented for the child, is refused.
    assert_eq!(verify_claim_with(h.path(), CHILD, &mint_token(&k, CHILD), true, true), IdentityVerdict::Rejected);
    assert_eq!(verify_claim_with(h.path(), CHILD, &mint_token_v2(&k, ROOT, &root_tuple()), true, true),
        IdentityVerdict::Rejected);
    // A forged sidecar (org store disagrees) resolves to no root: refused.
    const FORGED: &str = "eph-ffffffffffff";
    let forged = h.path().join("agents").join(".ephemeral").join(FORGED);
    std::fs::create_dir_all(&forged).unwrap();
    std::fs::write(forged.join("agent.toml"), "[agent]\nname = \"eph\"\n").unwrap();
    std::fs::write(forged.join("ephemeral.toml"), format!("parent = \"{ROOT}\"\ntier = \"standard\"\n\
        created_at = \"2026-11-09T00:00:00Z\"\nexpires_at = \"2026-11-10T00:00:00Z\"\n")).unwrap();
    assert_eq!(token_binding(h.path(), FORGED, true), Ok(TokenBinding::Refused));
    // Unprotected installs: children keep their v1 tags.
    assert_eq!(token_binding(h.path(), CHILD, false), Ok(TokenBinding::V1));
    // A tombstoned root refuses its children too.
    let gone = home(true);
    assert_eq!(token_binding(gone.path(), CHILD, true), Ok(TokenBinding::Refused));
    assert_eq!(current_agent_token_with(gone.path(), &k, CHILD, true), None);
}
