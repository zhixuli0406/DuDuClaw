//! Account writes that touch several fields at once — `users.create` and
//! `users.update` — committed as ONE `BEGIN IMMEDIATE` transaction, so a
//! refused manager or a refused demotion leaves every other field of the
//! same request untouched, and a created account never exists without the
//! manager it was created with.

use chrono::Utc;
use rusqlite::{params, Connection, TransactionBehavior};
use uuid::Uuid;

use crate::db::{hash_password, UserDb};
use crate::models::{User, UserRole};
use crate::org::OrgError;
use crate::org_write::{reports_count, validate_manager, write_manager};
use crate::user_row::{bump_acl_generation, load_user};

/// Why a combined account write was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserWriteError {
    /// A reporting-line rule (coded; see [`OrgError::code`]).
    Org(OrgError),
    /// Anything else: bad input, duplicate email, storage error.
    Other(String),
}

impl std::fmt::Display for UserWriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Org(e) => write!(f, "{e}"),
            Self::Other(e) => write!(f, "{e}"),
        }
    }
}

impl From<OrgError> for UserWriteError {
    fn from(e: OrgError) -> Self {
        Self::Org(e)
    }
}

fn other(e: impl std::fmt::Display) -> UserWriteError {
    UserWriteError::Other(e.to_string())
}

/// Fields of one `users.update`. `None` = leave as it is; for the two
/// clearable fields `Some(None)` = clear.
#[derive(Debug, Clone, Default)]
pub struct UserUpdate<'a> {
    pub display_name: Option<&'a str>,
    pub role: Option<UserRole>,
    pub password: Option<&'a str>,
    pub department: Option<Option<&'a str>>,
    pub manager: Option<Option<&'a str>>,
}

/// A new account (`users.create`).
#[derive(Debug, Clone)]
pub struct NewUser<'a> {
    pub email: &'a str,
    pub display_name: &'a str,
    pub password: &'a str,
    pub role: UserRole,
    pub department: Option<&'a str>,
    pub manager: Option<&'a str>,
}

fn clean_department(d: Option<&str>) -> Option<&str> {
    d.map(str::trim).filter(|d| !d.is_empty())
}

fn apply_update(
    tx: &Connection,
    user_id: &str,
    update: &UserUpdate<'_>,
    password_hash: Option<&str>,
) -> Result<(), UserWriteError> {
    let now = Utc::now().to_rfc3339();
    load_user(tx, user_id)
        .map_err(other)?
        .ok_or(UserWriteError::Org(OrgError::UserNotFound))?;
    // An Employee cannot be anyone's manager (Q2).
    if update.role == Some(UserRole::Employee) && reports_count(tx, user_id).map_err(other)? > 0 {
        return Err(OrgError::HasReports.into());
    }
    if let Some(Some(m)) = update.manager {
        validate_manager(tx, Some(user_id), m)?;
    }
    if let Some(name) = update.display_name {
        tx.execute(
            "UPDATE users SET display_name = ?1, updated_at = ?2 WHERE id = ?3",
            params![name, now, user_id],
        )
        .map_err(other)?;
    }
    if let Some(r) = update.role {
        tx.execute(
            "UPDATE users SET role = ?1, updated_at = ?2 WHERE id = ?3",
            params![r.to_string(), now, user_id],
        )
        .map_err(other)?;
    }
    if let Some(hash) = password_hash {
        // Changing the password clears the forced-change flag (C1 fix).
        tx.execute(
            "UPDATE users SET password_hash = ?1, updated_at = ?2, must_change_password = 0 WHERE id = ?3",
            params![hash, now, user_id],
        )
        .map_err(other)?;
    }
    if let Some(dept) = update.department {
        tx.execute(
            "UPDATE users SET department = ?1, updated_at = ?2 WHERE id = ?3",
            params![clean_department(dept), now, user_id],
        )
        .map_err(other)?;
    }
    if let Some(manager) = update.manager {
        write_manager(tx, user_id, manager)?;
    }
    // Role, the forced-change flag and the manager are live-context input.
    if update.role.is_some() || update.password.is_some() || update.manager.is_some() {
        bump_acl_generation(tx).map_err(other)?;
    }
    Ok(())
}

impl UserDb {
    /// Apply every field of `update` or none of them. Refusals: the user
    /// does not exist ([`OrgError::UserNotFound`]), a demotion to Employee
    /// while direct reports exist ([`OrgError::HasReports`]), and every
    /// [`Self::set_manager`] refusal for the manager field.
    pub fn apply_user_update(&self, user_id: &str, update: &UserUpdate<'_>) -> Result<(), UserWriteError> {
        // Hash before taking the write lock: argon2 is slow on purpose.
        let password_hash = update.password.map(hash_password).transpose().map_err(UserWriteError::Other)?;
        let mut conn = self.conn();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(other)?;
        apply_update(&tx, user_id, update, password_hash.as_deref())?;
        tx.commit().map_err(other)
    }

    /// Create an account with its department and manager in one
    /// transaction: a manager that fails the [`Self::set_manager`] checks
    /// means no account is created at all.
    pub fn create_user_with(&self, new: &NewUser<'_>) -> Result<User, UserWriteError> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        let password_hash = hash_password(new.password).map_err(UserWriteError::Other)?;
        let department = clean_department(new.department);
        let mut conn = self.conn();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(other)?;
        tx.execute(
            "INSERT INTO users (id, email, display_name, password_hash, role, status, created_at, updated_at, department)
             VALUES (?1, ?2, ?3, ?4, ?5, 'active', ?6, ?6, ?7)",
            params![id, new.email, new.display_name, password_hash, new.role.to_string(), now, department],
        )
        .map_err(|e| UserWriteError::Other(format!("failed to create user: {e}")))?;
        if let Some(m) = new.manager {
            validate_manager(&tx, Some(&id), m)?;
            write_manager(&tx, &id, Some(m))?;
            bump_acl_generation(&tx).map_err(other)?;
        }
        // Return the row as stored (timestamps included).
        let user = load_user(&tx, &id)
            .map_err(other)?
            .ok_or_else(|| other("created user vanished"))?;
        tx.commit().map_err(other)?;
        Ok(user)
    }
}
