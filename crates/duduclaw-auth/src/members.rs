//! One member row is one transaction: account, bindings, preference, invitation
//! and audit either all land or all roll back. Delivery happens after commit.
use std::collections::HashSet;

use chrono::Utc;
use rusqlite::{params, TransactionBehavior};
use serde::Deserialize;
use uuid::Uuid;

use crate::{AccessLevel, User, UserDb, UserRole};
use crate::user_invites::{issue_in_transaction, IssuedUserInvite};
use crate::user_row::{bump_acl_generation, load_user};

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemberBinding {
    pub agent_name: String,
    pub access_level: AccessLevel,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewMember {
    pub display_name: String,
    pub email: String,
    pub role: UserRole,
    #[serde(default)]
    pub department: Option<String>,
    pub primary_agent_id: String,
    pub bindings: Vec<MemberBinding>,
    pub invite: bool,
}

pub struct CreatedMember {
    pub user: User,
    pub invitation: Option<IssuedUserInvite>,
}

impl NewMember {
    pub fn validate(&self) -> Result<(), &'static str> {
        let name = self.display_name.trim();
        if name.is_empty() || name.chars().count() > 200 || name.chars().any(char::is_control) {
            return Err("member_invalid_name");
        }
        let email = self.email.trim();
        let parts: Vec<_> = email.split('@').collect();
        if email.len() > 254 || parts.len() != 2 || parts[0].is_empty() || parts[1].is_empty()
            || email.chars().any(|c| c.is_whitespace() || c.is_control() || matches!(c, ',' | ';' | '<' | '>' | '"'))
            || !parts[1].split('.').all(|label| !label.is_empty()) || parts[1].split('.').count() < 2
        { return Err("member_invalid_email"); }
        if let Some(department) = self.department.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
            if !duduclaw_core::is_valid_department(department) { return Err("member_invalid_department"); }
        }
        if self.bindings.is_empty() || self.bindings.len() > 64 { return Err("member_invalid_bindings"); }
        let mut names = HashSet::new();
        for binding in &self.bindings {
            if !duduclaw_core::is_valid_agent_id(&binding.agent_name)
                || binding.access_level == AccessLevel::Owner
                || !names.insert(binding.agent_name.as_str())
            { return Err("member_invalid_bindings"); }
        }
        if !self.bindings.iter().any(|binding| binding.agent_name == self.primary_agent_id
            && binding.access_level.at_least(AccessLevel::Operator)) {
            return Err("member_invalid_primary");
        }
        Ok(())
    }
}

impl UserDb {
    pub fn create_member_at_epoch(&self, member: &NewMember, actor: &str, epoch: u64) -> Result<CreatedMember, &'static str> {
        member.validate()?;
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| "member_storage_unavailable")?;
        let authorized: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM users WHERE id=?1 AND role='admin' AND status='active'
             AND must_change_password=0 AND credential_epoch=?2)",
            params![actor, epoch], |row| row.get(0),
        ).map_err(|_| "member_storage_unavailable")?;
        if !authorized { return Err("member_admin_required"); }
        let duplicate: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM users WHERE email=?1 COLLATE NOCASE)",
            [member.email.trim()], |row| row.get(0),
        ).map_err(|_| "member_storage_unavailable")?;
        if duplicate { return Err("member_email_exists"); }
        // A Personal employee (isolation mirror row) belongs to its owner only;
        // refuse explicitly instead of failing on the mirror trigger.
        for binding in &member.bindings {
            match crate::db::personal_agent_owner(&tx, &binding.agent_name) {
                Ok(None) => {}
                Ok(Some(_)) => return Err("member_agent_personal"),
                Err(_) => return Err("member_storage_unavailable"),
            }
        }
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        tx.execute(
            "INSERT INTO users(id,email,display_name,password_hash,role,status,created_at,updated_at,
             department,must_change_password,primary_agent_id)
             VALUES(?1,?2,?3,'',?4,'active',?5,?5,?6,1,?7)",
            params![id, member.email.trim(), member.display_name.trim(), member.role.to_string(), now,
                member.department.as_deref().map(str::trim).filter(|d| !d.is_empty()), member.primary_agent_id],
        ).map_err(|_| "member_storage_unavailable")?;
        for binding in &member.bindings {
            tx.execute(
                "INSERT INTO user_agent_bindings(user_id,agent_name,access_level,bound_at) VALUES(?1,?2,?3,?4)",
                params![id, binding.agent_name, binding.access_level.to_string(), now],
            ).map_err(|_| "member_storage_unavailable")?;
        }
        tx.execute("INSERT INTO user_onboarding(user_id) VALUES(?1)", [&id])
            .map_err(|_| "member_storage_unavailable")?;
        let invitation = if member.invite {
            Some(issue_in_transaction(&tx, &id, actor, Some(epoch)).map_err(|_| "member_invite_unavailable")?)
        } else { None };
        let detail = serde_json::json!({
            "primary_agent_id": member.primary_agent_id,
            "bindings": member.bindings.iter().map(|binding| serde_json::json!({
                "agent_name": binding.agent_name, "access_level": binding.access_level,
            })).collect::<Vec<_>>(),
            "invite": member.invite,
        }).to_string();
        tx.execute(
            "INSERT INTO auth_audit_log(user_id,action,target,detail,timestamp) VALUES(?1,'user.create_member',?2,?3,?4)",
            params![actor, id, detail, now],
        ).map_err(|_| "member_storage_unavailable")?;
        bump_acl_generation(&tx).map_err(|_| "member_storage_unavailable")?;
        let user = load_user(&tx, &id).map_err(|_| "member_storage_unavailable")?
            .ok_or("member_storage_unavailable")?;
        tx.commit().map_err(|_| "member_storage_unavailable")?;
        Ok(CreatedMember { user, invitation })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::UserStatus;

    fn fixture() -> (tempfile::TempDir, UserDb, User) {
        let dir = tempfile::tempdir().unwrap();
        let db = UserDb::new(&dir.path().join("users.db")).unwrap();
        let admin = db.create_user("admin@test.invalid", "Admin", "admin-password", UserRole::Admin).unwrap();
        (dir, db, admin)
    }
    fn member(invite: bool) -> NewMember {
        NewMember {
            email: "member@test.invalid".into(), display_name: "新同仁".into(), role: UserRole::Employee,
            department: Some("support".into()), primary_agent_id: "helper".into(), invite,
            bindings: vec![MemberBinding { agent_name: "helper".into(), access_level: AccessLevel::Operator },
                MemberBinding { agent_name: "reader".into(), access_level: AccessLevel::Viewer }],
        }
    }
    fn error(db: &UserDb, member: &NewMember, admin: &User) -> &'static str {
        db.create_member_at_epoch(member, &admin.id, admin.credential_epoch).err().unwrap()
    }
    #[test]
    fn member_creation_commits_credentials_bindings_primary_and_audit_together() {
        let (_dir, db, admin) = fixture();
        let created = db.create_member_at_epoch(&member(true), &admin.id, admin.credential_epoch).unwrap();
        let user = created.user;
        let invitation = created.invitation.unwrap();
        assert_eq!(db.primary_agent(&user.id).unwrap().as_deref(), Some("helper"));
        assert_eq!(db.get_user_agents(&user.id).unwrap().len(), 2);
        assert_eq!(user.department.as_deref(), Some("support"));
        assert!(user.must_change_password);
        assert!(db.verify_password(&user.email, "").is_err());
        assert_eq!(db.user_invite_email(&invitation.token).unwrap().as_deref(), Some("member@test.invalid"));
        assert_eq!(db.query_audit_log(None, Some("user.create_member"), 20).unwrap().len(), 1);
        assert!(db.consume_user_invite(&invitation.token, "new-member-password").unwrap().is_some());
        assert!(db.verify_password(&user.email, "new-member-password").is_ok());
    }
    #[test]
    fn member_without_email_delivery_still_has_no_password_and_no_invite() {
        let (_dir, db, admin) = fixture();
        let created = db.create_member_at_epoch(&member(false), &admin.id, admin.credential_epoch).unwrap();
        assert!(created.invitation.is_none());
        assert_eq!(db.user_invite_status(&created.user.id).unwrap(), "none");
        assert!(created.user.must_change_password);
    }
    #[test]
    fn member_validation_and_duplicate_email_leave_no_partial_rows() {
        let (_dir, db, admin) = fixture();
        let base = member(true);
        for malformed in [
            NewMember { email: "bad\r\n@test.invalid".into(), ..base.clone() },
            NewMember { primary_agent_id: "unbound".into(), ..base.clone() },
            NewMember { primary_agent_id: "reader".into(), ..base.clone() },
            NewMember { department: Some("../bad".into()), ..base.clone() },
            NewMember { bindings: vec![base.bindings[0].clone(), base.bindings[0].clone()], ..base.clone() },
            NewMember { bindings: vec![MemberBinding { agent_name: "helper".into(), access_level: AccessLevel::Owner }], ..base.clone() },
        ] {
            assert!(db.create_member_at_epoch(&malformed, &admin.id, 0).is_err());
            assert_eq!(db.list_users().unwrap().len(), 1);
        }
        let created = db.create_member_at_epoch(&base, &admin.id, 0).unwrap();
        assert_eq!(error(&db, &NewMember { email: "MEMBER@test.invalid".into(), ..base }, &admin), "member_email_exists");
        assert_eq!(db.list_users().unwrap().len(), 2);
        assert!(db.user_invite_email(&created.invitation.unwrap().token).unwrap().is_some());
    }
    #[test]
    fn member_transaction_rolls_back_on_binding_invite_and_audit_failures() {
        for target in ["user_agent_bindings", "user_invites", "auth_audit_log"] {
            let (_dir, db, admin) = fixture();
            db.conn().execute_batch(&format!("CREATE TRIGGER fail_member BEFORE INSERT ON {target} BEGIN SELECT RAISE(ABORT, 'test failure'); END;")).unwrap();
            assert!(db.create_member_at_epoch(&member(true), &admin.id, 0).is_err());
            let conn = db.conn();
            for table in ["user_agent_bindings", "user_invites", "user_onboarding"] {
                let count: usize = conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row.get(0)).unwrap();
                assert_eq!(count, 0, "{target} failure left {table} rows");
            }
            let count: usize = conn.query_row("SELECT COUNT(*) FROM users", [], |row| row.get(0)).unwrap();
            assert_eq!(count, 1);
        }
    }
    #[test]
    fn member_creation_rechecks_live_role_epoch_status_and_password_change() {
        for change in ["epoch", "role", "status", "password"] {
            let (_dir, db, admin) = fixture();
            match change {
                "epoch" => { db.conn().execute("UPDATE users SET credential_epoch=1 WHERE id=?1", [&admin.id]).unwrap(); }
                "role" => db.update_user(&admin.id, None, Some(UserRole::Employee), None).unwrap(),
                "status" => db.set_user_status(&admin.id, UserStatus::Suspended).unwrap(),
                "password" => { db.conn().execute("UPDATE users SET must_change_password=1 WHERE id=?1", [&admin.id]).unwrap(); }
                _ => unreachable!(),
            }
            assert_eq!(error(&db, &member(true), &admin), "member_admin_required");
            assert_eq!(db.list_users().unwrap().len(), 1);
        }
    }
}
