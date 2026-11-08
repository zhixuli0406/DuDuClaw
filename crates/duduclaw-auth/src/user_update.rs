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
        self.apply_user_update_as(None, user_id, update)
    }

    /// [`Self::apply_user_update`] attributed to the acting Admin in the
    /// admin eligibility log, in the same transaction (F6 S1/S2: a role
    /// change records who promoted or demoted the account).
    pub fn apply_user_update_as(&self, actor: Option<&str>, user_id: &str, update: &UserUpdate<'_>) -> Result<(), UserWriteError> {
        // Hash before taking the write lock: argon2 is slow on purpose.
        let password_hash = update.password.map(hash_password).transpose().map_err(UserWriteError::Other)?;
        let mut conn = self.conn();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(other)?;
        let since = crate::admin_eligibility::high_water(&tx).map_err(other)?;
        apply_update(&tx, user_id, update, password_hash.as_deref())?;
        if let Some(actor) = actor { crate::admin_eligibility::stamp_actor(&tx, since, actor).map_err(other)?; }
        tx.commit().map_err(other)
    }

    /// Self-service password change by the account holder in their own
    /// authenticated session (`users.change_password`, `ctx.user_id ==
    /// user_id`; the caller verified the current password). Same write as
    /// [`Self::update_user`] with only the password, plus the F6 Q3 stamp:
    /// the `credential_changes` row is marked owner-authenticated in the
    /// same transaction (F6d), so a later Personal restore accepts it.
    pub fn change_own_password(&self, user_id: &str, new_password: &str) -> Result<(), String> {
        let password_hash = hash_password(new_password)?;
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|e| format!("update error: {e}"))?;
        let since = crate::credential_changes::high_water(&tx)?;
        let update = UserUpdate { password: Some(new_password), ..UserUpdate::default() };
        apply_update(&tx, user_id, &update, Some(&password_hash)).map_err(|e| match e {
            UserWriteError::Org(OrgError::UserNotFound) => "user not found".to_string(),
            other => format!("update error: {other}"),
        })?;
        crate::credential_changes::stamp_owner(&tx, since, user_id)?;
        tx.commit().map_err(|e| format!("update error: {e}"))
    }

    /// Create an account with its department and manager in one
    /// transaction: a manager that fails the [`Self::set_manager`] checks
    /// means no account is created at all.
    pub fn create_user_with(&self, new: &NewUser<'_>) -> Result<User, UserWriteError> {
        self.create_user_with_credentials(None, new, false)
    }

    /// Create an invitation-only account. An empty stored hash is not a valid
    /// Argon2 hash, so no password (including an empty one) can authenticate.
    /// The supplied `NewUser.password` is deliberately ignored.
    pub fn create_invited_user_with(&self, new: &NewUser<'_>) -> Result<User, UserWriteError> {
        self.create_user_with_credentials(None, new, true)
    }

    /// [`Self::create_user_with`] / [`Self::create_invited_user_with`]
    /// attributed to the acting Admin in the admin eligibility log (an Admin
    /// creation records its creator, F6 S2).
    pub fn create_user_as(&self, actor: Option<&str>, new: &NewUser<'_>, invite: bool) -> Result<User, UserWriteError> {
        self.create_user_with_credentials(actor, new, invite)
    }

    fn create_user_with_credentials(&self, actor: Option<&str>, new: &NewUser<'_>, invite: bool) -> Result<User, UserWriteError> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        let password_hash = if invite {
            String::new()
        } else {
            hash_password(new.password).map_err(UserWriteError::Other)?
        };
        let department = clean_department(new.department);
        let mut conn = self.conn();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(other)?;
        let since = crate::admin_eligibility::high_water(&tx).map_err(other)?;
        tx.execute(
            "INSERT INTO users (id, email, display_name, password_hash, role, status, created_at, updated_at, department, must_change_password)
             VALUES (?1, ?2, ?3, ?4, ?5, 'active', ?6, ?6, ?7, ?8)",
            params![id, new.email, new.display_name, password_hash, new.role.to_string(), now, department, invite],
        )
        .map_err(|e| UserWriteError::Other(format!("failed to create user: {e}")))?;
        if let Some(m) = new.manager {
            validate_manager(&tx, Some(&id), m)?;
            write_manager(&tx, &id, Some(m))?;
            bump_acl_generation(&tx).map_err(other)?;
        }
        if let Some(actor) = actor { crate::admin_eligibility::stamp_actor(&tx, since, actor).map_err(other)?; }
        // Return the row as stored (timestamps included).
        let user = load_user(&tx, &id)
            .map_err(other)?
            .ok_or_else(|| other("created user vanished"))?;
        tx.commit().map_err(other)?;
        Ok(user)
    }
}
