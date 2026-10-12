use std::path::Path;
use std::sync::Mutex;

use argon2::password_hash::SaltString;
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use chrono::Utc;
use rusqlite::{params, Connection};
use tracing::warn;
use uuid::Uuid;

use crate::models::*;

/// SQLite-backed user database with connection pool.
pub struct UserDb {
    pool: Vec<Mutex<Connection>>,
}

const POOL_SIZE: usize = 2;

impl UserDb {
    /// Open (or create) the user database at the given path.
    pub fn new(db_path: &Path) -> Result<Self, String> {
        let mut pool = Vec::with_capacity(POOL_SIZE);
        for _ in 0..POOL_SIZE {
            let conn = Connection::open(db_path)
                .map_err(|e| format!("failed to open user db: {e}"))?;
            conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;")
                .map_err(|e| format!("failed to set pragmas: {e}"))?;
            pool.push(Mutex::new(conn));
        }
        let db = Self { pool };
        db.init_tables()?;
        Ok(db)
    }

    pub(crate) fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        for m in &self.pool {
            if let Ok(guard) = m.try_lock() {
                return guard;
            }
        }
        // Fallback: block on first. Recover from a poisoned lock instead of
        // panicking (L27 fix) — a single panicked guard must not turn into a
        // full auth DoS. The connection itself remains usable.
        self.pool[0].lock().unwrap_or_else(|e| e.into_inner())
    }

    fn init_tables(&self) -> Result<(), String> {
        let conn = self.conn();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS users (
                id TEXT PRIMARY KEY,
                email TEXT UNIQUE NOT NULL,
                display_name TEXT NOT NULL,
                password_hash TEXT NOT NULL,
                role TEXT NOT NULL DEFAULT 'employee',
                status TEXT NOT NULL DEFAULT 'active',
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                last_login TEXT,
                must_change_password INTEGER NOT NULL DEFAULT 0,
                bootstrap_owner INTEGER NOT NULL DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS user_agent_bindings (
                user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                agent_name TEXT NOT NULL,
                access_level TEXT NOT NULL DEFAULT 'owner',
                bound_at TEXT NOT NULL,
                PRIMARY KEY (user_id, agent_name)
            );

            CREATE TABLE IF NOT EXISTS auth_audit_log (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                user_id TEXT,
                action TEXT NOT NULL,
                target TEXT,
                detail TEXT,
                ip TEXT,
                timestamp TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_audit_timestamp ON auth_audit_log(timestamp);
            CREATE INDEX IF NOT EXISTS idx_audit_user ON auth_audit_log(user_id);
            CREATE INDEX IF NOT EXISTS idx_bindings_agent ON user_agent_bindings(agent_name);

            -- Passwordless channel OTP login (WP12). A verified 1:1 channel DM
            -- identity ties a person's messaging account to their user record;
            -- OTP challenges are short-lived, single-use, attempt-capped.
            CREATE TABLE IF NOT EXISTS channel_identities (
                user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                channel TEXT NOT NULL,
                channel_user_id TEXT NOT NULL,
                verified INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL,
                PRIMARY KEY (channel, channel_user_id)
            );
            CREATE INDEX IF NOT EXISTS idx_chid_user ON channel_identities(user_id);

            CREATE TABLE IF NOT EXISTS otp_challenges (
                id TEXT PRIMARY KEY,
                user_id TEXT NOT NULL,
                code_hash TEXT NOT NULL,
                channel TEXT NOT NULL,
                channel_user_id TEXT NOT NULL,
                created_at TEXT NOT NULL,
                expires_at TEXT NOT NULL,
                attempts INTEGER NOT NULL DEFAULT 0,
                consumed INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_otp_user ON otp_challenges(user_id);
            CREATE INDEX IF NOT EXISTS idx_otp_created ON otp_challenges(created_at);

            -- First-run / break-glass admin bootstrap tokens (host-access root).
            CREATE TABLE IF NOT EXISTS setup_tokens (
                id TEXT PRIMARY KEY,
                token_hash TEXT NOT NULL,
                created_at TEXT NOT NULL,
                expires_at TEXT NOT NULL,
                consumed INTEGER NOT NULL DEFAULT 0
            );",
        )
        .map_err(|e| format!("failed to create auth tables: {e}"))?;

        // Idempotent migration for databases created before
        // `must_change_password` existed. ADD COLUMN errors if the column is
        // already present, so probe table_info first.
        let has_col = conn
            .prepare("PRAGMA table_info(users)")
            .and_then(|mut stmt| {
                let cols = stmt
                    .query_map([], |row| row.get::<_, String>(1))?
                    .collect::<Result<Vec<String>, _>>()?;
                Ok(cols.iter().any(|c| c == "must_change_password"))
            })
            .map_err(|e| format!("failed to inspect users table: {e}"))?;
        if !has_col {
            conn.execute(
                "ALTER TABLE users ADD COLUMN must_change_password INTEGER NOT NULL DEFAULT 0",
                [],
            )
            .map_err(|e| format!("failed to migrate users table: {e}"))?;
        }

        // Idempotent migration: `department` column (install-approval routing).
        let has_dept = conn
            .prepare("PRAGMA table_info(users)")
            .and_then(|mut stmt| {
                let cols = stmt
                    .query_map([], |row| row.get::<_, String>(1))?
                    .collect::<Result<Vec<String>, _>>()?;
                Ok(cols.iter().any(|c| c == "department"))
            })
            .map_err(|e| format!("failed to inspect users table: {e}"))?;
        if !has_dept {
            conn.execute("ALTER TABLE users ADD COLUMN department TEXT", [])
                .map_err(|e| format!("failed to migrate users.department: {e}"))?;
        }

        // Idempotent migration: `bootstrap_owner` marks the row
        // `ensure_default_admin` created, so the first-run claim and the
        // Personal-edition local session can find the owner after the claim
        // replaced its placeholder email. Databases from before this column
        // existed only ever had the owner under the placeholder address, so
        // the backfill runs once, in the same step that adds the column.
        let has_owner = conn
            .prepare("PRAGMA table_info(users)")
            .and_then(|mut stmt| {
                let cols = stmt
                    .query_map([], |row| row.get::<_, String>(1))?
                    .collect::<Result<Vec<String>, _>>()?;
                Ok(cols.iter().any(|c| c == "bootstrap_owner"))
            })
            .map_err(|e| format!("failed to inspect users table: {e}"))?;
        if !has_owner {
            conn.execute(
                "ALTER TABLE users ADD COLUMN bootstrap_owner INTEGER NOT NULL DEFAULT 0",
                [],
            )
            .map_err(|e| format!("failed to migrate users.bootstrap_owner: {e}"))?;
            conn.execute(
                "UPDATE users SET bootstrap_owner = 1 WHERE email = ?1",
                params![BOOTSTRAP_PLACEHOLDER_EMAIL],
            )
            .map_err(|e| format!("failed to backfill users.bootstrap_owner: {e}"))?;
        }
        Ok(())
    }

    /// Set (or clear with `None`) a user's department. Empty string clears.
    pub fn set_department(&self, user_id: &str, department: Option<&str>) -> Result<(), String> {
        let now = Utc::now().to_rfc3339();
        let dept = department.map(|d| d.trim()).filter(|d| !d.is_empty());
        let conn = self.conn();
        conn.execute(
            "UPDATE users SET department = ?1, updated_at = ?2 WHERE id = ?3",
            params![dept, now, user_id],
        )
        .map_err(|e| format!("set department: {e}"))?;
        Ok(())
    }

    // ── User CRUD ────────────────────────────────────────────

    /// Create a new user with argon2id-hashed password.
    pub fn create_user(
        &self,
        email: &str,
        display_name: &str,
        password: &str,
        role: UserRole,
    ) -> Result<User, String> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        let password_hash = hash_password(password)?;

        let conn = self.conn();
        conn.execute(
            "INSERT INTO users (id, email, display_name, password_hash, role, status, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 'active', ?6, ?6)",
            params![id, email, display_name, password_hash, role.to_string(), now],
        )
        .map_err(|e| format!("failed to create user: {e}"))?;

        Ok(User {
            id,
            email: email.to_string(),
            display_name: display_name.to_string(),
            role,
            status: UserStatus::Active,
            created_at: now.clone(),
            updated_at: now,
            last_login: None,
            must_change_password: false,
            department: None,
        })
    }

    /// Verify email + password, return the user if valid.
    ///
    /// Timing-safe: always performs a hash verification even when the email
    /// does not exist, preventing account enumeration via response time.
    pub fn verify_password(&self, email: &str, password: &str) -> Result<User, String> {
        if password.len() > MAX_PASSWORD_LEN {
            return Err("invalid email or password".to_string());
        }

        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT id, email, display_name, password_hash, role, status, created_at, updated_at, last_login, must_change_password
                 FROM users WHERE email = ?1",
            )
            .map_err(|e| format!("query error: {e}"))?;

        let row_result = stmt.query_row(params![email], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, i64>(9)? != 0,
            ))
        });

        let row = match row_result {
            Ok(r) => r,
            Err(_) => {
                // Timing-safe: perform dummy hash to equalize response time
                let _ = verify_password_hash(password, &DUMMY_HASH);
                return Err("invalid email or password".to_string());
            }
        };

        let (id, email, display_name, stored_hash, role_str, status_str, created_at, updated_at, last_login, must_change_password) = row;

        // Verify password (always runs, whether user exists or not handled above)
        verify_password_hash(password, &stored_hash)?;

        let role: UserRole = role_str.parse()
            .map_err(|_| "invalid email or password".to_string())?;
        let status: UserStatus = status_str.parse()
            .map_err(|_| "invalid email or password".to_string())?;

        // Check status — use generic message to prevent status enumeration
        if status != UserStatus::Active {
            return Err("invalid email or password".to_string());
        }

        Ok(User {
            id,
            email,
            display_name,
            role,
            status,
            created_at,
            updated_at,
            last_login,
            must_change_password,
            department: None,
        })
    }

    /// Get a user by ID.
    pub fn get_user(&self, user_id: &str) -> Result<Option<User>, String> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT id, email, display_name, role, status, created_at, updated_at, last_login, must_change_password, department
                 FROM users WHERE id = ?1",
            )
            .map_err(|e| format!("query error: {e}"))?;

        let result = stmt
            .query_row(params![user_id], |row| {
                let role_str: String = row.get(3)?;
                let status_str: String = row.get(4)?;
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    role_str,
                    status_str,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, i64>(8)? != 0,
                    row.get::<_, Option<String>>(9)?,
                ))
            });

        match result {
            Ok((id, email, display_name, role_str, status_str, created_at, updated_at, last_login, must_change_password, department)) => {
                let role: UserRole = role_str.parse()
                    .map_err(|e: String| format!("corrupt role in DB: {e}"))?;
                let status: UserStatus = status_str.parse()
                    .map_err(|e: String| format!("corrupt status in DB: {e}"))?;
                Ok(Some(User { id, email, display_name, role, status, created_at, updated_at, last_login, must_change_password, department }))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("query error: {e}")),
        }
    }

    /// Get a user by email address, without presenting a credential.
    ///
    /// Deliberately separate from [`Self::verify_password`]: callers must have
    /// established authority some other way. The only caller today is the
    /// Personal-edition local session endpoint, whose authority is the TCP peer
    /// address being loopback (`server.rs::handle_local_session`).
    pub fn get_user_by_email(&self, email: &str) -> Result<Option<User>, String> {
        let id: String = {
            let conn = self.conn();
            match conn.query_row(
                "SELECT id FROM users WHERE email = ?1",
                params![email],
                |row| row.get::<_, String>(0),
            ) {
                Ok(id) => id,
                Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
                Err(e) => return Err(format!("query error: {e}")),
            }
            // guard dropped here, before `get_user` takes its own connection
        };
        self.get_user(&id)
    }

    /// List all users.
    pub fn list_users(&self) -> Result<Vec<User>, String> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT id, email, display_name, role, status, created_at, updated_at, last_login, must_change_password, department
                 FROM users ORDER BY created_at",
            )
            .map_err(|e| format!("query error: {e}"))?;

        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, i64>(8)? != 0,
                    row.get::<_, Option<String>>(9)?,
                ))
            })
            .map_err(|e| format!("query error: {e}"))?;

        let mut users = Vec::new();
        for row in rows {
            let (id, email, display_name, role_str, status_str, created_at, updated_at, last_login, must_change_password, department) =
                row.map_err(|e| format!("row error: {e}"))?;
            let role: UserRole = role_str.parse()
                .map_err(|e: String| format!("corrupt role in DB: {e}"))?;
            let status: UserStatus = status_str.parse()
                .map_err(|e: String| format!("corrupt status in DB: {e}"))?;
            users.push(User { id, email, display_name, role, status, created_at, updated_at, last_login, must_change_password, department });
        }
        Ok(users)
    }

    /// Update a user's display_name and/or role.
    pub fn update_user(
        &self,
        user_id: &str,
        display_name: Option<&str>,
        role: Option<UserRole>,
        password: Option<&str>,
    ) -> Result<(), String> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();

        if let Some(name) = display_name {
            conn.execute(
                "UPDATE users SET display_name = ?1, updated_at = ?2 WHERE id = ?3",
                params![name, now, user_id],
            )
            .map_err(|e| format!("update error: {e}"))?;
        }

        if let Some(r) = role {
            conn.execute(
                "UPDATE users SET role = ?1, updated_at = ?2 WHERE id = ?3",
                params![r.to_string(), now, user_id],
            )
            .map_err(|e| format!("update error: {e}"))?;
        }

        if let Some(pw) = password {
            let hash = hash_password(pw)?;
            // Changing the password clears the forced-change flag (C1 fix).
            conn.execute(
                "UPDATE users SET password_hash = ?1, updated_at = ?2, must_change_password = 0 WHERE id = ?3",
                params![hash, now, user_id],
            )
            .map_err(|e| format!("update error: {e}"))?;
        }

        Ok(())
    }

    /// Change a user's login email. `email` must already have passed
    /// [`validate_account_email`]. An address another account already uses
    /// returns [`EMAIL_IN_USE`] and changes nothing.
    pub fn change_email(&self, user_id: &str, email: &str) -> Result<(), String> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        let affected = conn
            .execute(
                "UPDATE users SET email = ?1, updated_at = ?2 WHERE id = ?3",
                params![email, now, user_id],
            )
            .map_err(|e| match e {
                rusqlite::Error::SqliteFailure(f, _)
                    if f.code == rusqlite::ErrorCode::ConstraintViolation =>
                {
                    EMAIL_IN_USE.to_string()
                }
                other => format!("update error: {other}"),
            })?;
        if affected == 0 {
            return Err("user not found".to_string());
        }
        Ok(())
    }

    /// Set a user's status (active / suspended / offboarded).
    pub fn set_user_status(&self, user_id: &str, status: UserStatus) -> Result<(), String> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        let affected = conn
            .execute(
                "UPDATE users SET status = ?1, updated_at = ?2 WHERE id = ?3",
                params![status.to_string(), now, user_id],
            )
            .map_err(|e| format!("update error: {e}"))?;

        if affected == 0 {
            return Err("user not found".to_string());
        }
        Ok(())
    }

    /// Update last_login timestamp.
    pub fn update_last_login(&self, user_id: &str) -> Result<(), String> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        conn.execute(
            "UPDATE users SET last_login = ?1 WHERE id = ?2",
            params![now, user_id],
        )
        .map_err(|e| format!("update error: {e}"))?;
        Ok(())
    }

    // ── First-run claim (onboarding chicken-and-egg fix) ─────
    //
    // A fresh install bootstraps an owner account under the placeholder email
    // `admin@local` with a random one-time password and
    // `must_change_password = 1`. That password is only visible on the server
    // console, which a Desktop-app / non-terminal operator never sees — so the
    // LoginPage offers a browser "claim" flow instead, where the operator sets
    // their own email and password. These helpers back it; the HTTP handler
    // additionally gates the flow to loopback callers. The owner row is found
    // by its `bootstrap_owner` flag, never by email, because the claim may
    // replace the placeholder address.

    /// True while the instance is unclaimed: the bootstrap owner still carries
    /// the forced-change flag (its bootstrap password has never been replaced).
    /// Once the password is set (via claim or a normal change), this is false
    /// and the claim endpoint becomes inert.
    pub fn is_unclaimed_default_admin(&self) -> bool {
        let conn = self.conn();
        conn.query_row(
            "SELECT must_change_password FROM users WHERE bootstrap_owner = 1",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map(|flag| flag == 1)
        .unwrap_or(false) // no such row / error → not claimable (fail-closed)
    }

    /// Claim the bootstrap owner keeping its current email. See
    /// [`Self::claim_default_admin_with_email`].
    pub fn claim_default_admin(&self, new_password: &str) -> Result<bool, String> {
        self.claim_default_admin_with_email(None, new_password)
    }

    /// Atomically set the bootstrap owner's password (and, when given, replace
    /// its placeholder email) and clear the forced-change flag, but ONLY while
    /// still unclaimed (`must_change_password = 1`). The single guarded
    /// `UPDATE` is the concurrency gate: a second racing claim affects 0 rows.
    /// Returns `Ok(true)` when this call performed the claim, `Ok(false)` when
    /// the instance was already claimed.
    ///
    /// `email` must already have passed [`validate_account_email`]; an address
    /// another account already uses fails the `email UNIQUE` constraint and
    /// returns `Err` without claiming.
    pub fn claim_default_admin_with_email(
        &self,
        email: Option<&str>,
        new_password: &str,
    ) -> Result<bool, String> {
        let hash = hash_password(new_password)?;
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        let affected = conn
            .execute(
                "UPDATE users SET password_hash = ?1, updated_at = ?2, must_change_password = 0, \
                 email = COALESCE(?3, email) \
                 WHERE bootstrap_owner = 1 AND must_change_password = 1",
                params![hash, now, email],
            )
            .map_err(|e| format!("claim error: {e}"))?;
        Ok(affected == 1)
    }

    /// Claim the bootstrap owner with a freshly generated random password.
    ///
    /// Used by the Personal-edition local session endpoint: `ensure_default_admin`
    /// leaves the owner flagged `must_change_password = 1`, and
    /// `authenticate_jwt` refuses every operation while that flag is set — so a
    /// token issued without clearing it would be dead on arrival. The generated
    /// password is intentionally discarded: "no password prompt" must not mean
    /// "empty password", and an operator who later needs one sets their own from
    /// the dashboard's account settings.
    ///
    /// Same single-shot atomicity as [`Self::claim_default_admin`] (it *is* that
    /// call) — a racing claim affects zero rows and gets `Ok(false)`.
    pub fn claim_default_admin_random(&self) -> Result<bool, String> {
        self.claim_default_admin(&generate_password(32))
    }

    /// The account `ensure_default_admin` bootstrapped, whatever email it
    /// carries now. `Ok(None)` when it was deleted (or predates the
    /// `bootstrap_owner` column and had already been renamed away from the
    /// placeholder address).
    ///
    /// Like [`Self::get_user_by_email`], presents no credential: the only
    /// caller is the Personal-edition local session endpoint, whose authority
    /// is the TCP peer address being loopback.
    pub fn get_bootstrap_owner(&self) -> Result<Option<User>, String> {
        let id: String = {
            let conn = self.conn();
            match conn.query_row(
                "SELECT id FROM users WHERE bootstrap_owner = 1",
                [],
                |row| row.get::<_, String>(0),
            ) {
                Ok(id) => id,
                Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
                Err(e) => return Err(format!("query error: {e}")),
            }
        };
        self.get_user(&id)
    }

    // ── Agent Bindings ───────────────────────────────────────

    /// Bind a user to an agent with a given access level.
    pub fn bind_agent(
        &self,
        user_id: &str,
        agent_name: &str,
        access_level: AccessLevel,
    ) -> Result<(), String> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        conn.execute(
            "INSERT OR REPLACE INTO user_agent_bindings (user_id, agent_name, access_level, bound_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![user_id, agent_name, access_level.to_string(), now],
        )
        .map_err(|e| format!("bind error: {e}"))?;
        Ok(())
    }

    /// Unbind a user from an agent.
    pub fn unbind_agent(&self, user_id: &str, agent_name: &str) -> Result<(), String> {
        let conn = self.conn();
        conn.execute(
            "DELETE FROM user_agent_bindings WHERE user_id = ?1 AND agent_name = ?2",
            params![user_id, agent_name],
        )
        .map_err(|e| format!("unbind error: {e}"))?;
        Ok(())
    }

    /// Get all agents bound to a user.
    pub fn get_user_agents(&self, user_id: &str) -> Result<Vec<UserAgentBinding>, String> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT user_id, agent_name, access_level, bound_at
                 FROM user_agent_bindings WHERE user_id = ?1",
            )
            .map_err(|e| format!("query error: {e}"))?;

        let rows = stmt
            .query_map(params![user_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .map_err(|e| format!("query error: {e}"))?;

        let mut bindings = Vec::new();
        for row in rows {
            let (uid, agent, level_str, bound) = row.map_err(|e| format!("row error: {e}"))?;
            let access_level: AccessLevel = level_str.parse()
                .map_err(|e: String| format!("corrupt access_level in DB: {e}"))?;
            bindings.push(UserAgentBinding { user_id: uid, agent_name: agent, access_level, bound_at: bound });
        }
        Ok(bindings)
    }

    /// Get all users bound to an agent.
    pub fn get_agent_users(&self, agent_name: &str) -> Result<Vec<UserAgentBinding>, String> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT user_id, agent_name, access_level, bound_at
                 FROM user_agent_bindings WHERE agent_name = ?1",
            )
            .map_err(|e| format!("query error: {e}"))?;

        let rows = stmt
            .query_map(params![agent_name], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .map_err(|e| format!("query error: {e}"))?;

        let mut bindings = Vec::new();
        for row in rows {
            let (uid, agent, level_str, bound) = row.map_err(|e| format!("row error: {e}"))?;
            let access_level: AccessLevel = level_str.parse()
                .map_err(|e: String| format!("corrupt access_level in DB: {e}"))?;
            bindings.push(UserAgentBinding { user_id: uid, agent_name: agent, access_level, bound_at: bound });
        }
        Ok(bindings)
    }

    /// Check a user's access level to an agent.
    pub fn check_agent_access(
        &self,
        user_id: &str,
        agent_name: &str,
    ) -> Result<Option<AccessLevel>, String> {
        let conn = self.conn();
        let result = conn.query_row(
            "SELECT access_level FROM user_agent_bindings
             WHERE user_id = ?1 AND agent_name = ?2",
            params![user_id, agent_name],
            |row| row.get::<_, String>(0),
        );

        match result {
            Ok(level_str) => {
                let level: AccessLevel = level_str.parse()
                    .map_err(|e: String| format!("corrupt access_level in DB: {e}"))?;
                Ok(Some(level))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("query error: {e}")),
        }
    }

    // ── Audit Log ────────────────────────────────────────────

    /// Record an action in the audit log.
    pub fn log_action(
        &self,
        user_id: Option<&str>,
        action: &str,
        target: Option<&str>,
        detail: Option<&str>,
        ip: Option<&str>,
    ) -> Result<(), String> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        conn.execute(
            "INSERT INTO auth_audit_log (user_id, action, target, detail, ip, timestamp)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![user_id, action, target, detail, ip, now],
        )
        .map_err(|e| format!("audit log error: {e}"))?;
        Ok(())
    }

    /// Query audit log entries.
    pub fn query_audit_log(
        &self,
        user_id: Option<&str>,
        action: Option<&str>,
        limit: u32,
    ) -> Result<Vec<AuditEntry>, String> {
        let conn = self.conn();
        let mut sql = "SELECT id, user_id, action, target, detail, ip, timestamp FROM auth_audit_log WHERE 1=1".to_string();
        let mut bound_params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if let Some(uid) = user_id {
            sql.push_str(&format!(" AND user_id = ?{}", bound_params.len() + 1));
            bound_params.push(Box::new(uid.to_string()));
        }
        if let Some(act) = action {
            sql.push_str(&format!(" AND action = ?{}", bound_params.len() + 1));
            bound_params.push(Box::new(act.to_string()));
        }

        sql.push_str(&format!(" ORDER BY timestamp DESC LIMIT ?{}", bound_params.len() + 1));
        bound_params.push(Box::new(limit));

        let mut stmt = conn.prepare(&sql).map_err(|e| format!("query error: {e}"))?;
        let params_ref: Vec<&dyn rusqlite::types::ToSql> = bound_params.iter().map(|p| p.as_ref()).collect();
        let rows = stmt
            .query_map(params_ref.as_slice(), |row| {
                Ok(AuditEntry {
                    id: row.get(0)?,
                    user_id: row.get(1)?,
                    action: row.get(2)?,
                    target: row.get(3)?,
                    detail: row.get(4)?,
                    ip: row.get(5)?,
                    timestamp: row.get(6)?,
                })
            })
            .map_err(|e| format!("query error: {e}"))?;

        let mut entries = Vec::new();
        for row in rows {
            entries.push(row.map_err(|e| format!("row error: {e}"))?);
        }
        Ok(entries)
    }

    // ── Bootstrap ────────────────────────────────────────────

    /// Ensure at least one admin user exists. Creates the bootstrap owner
    /// with the placeholder email [`BOOTSTRAP_PLACEHOLDER_EMAIL`] and a random
    /// 24-char password if the users table is empty; the first-run claim
    /// replaces both. Returns the generated password (if created) so the
    /// caller can display it once.
    pub fn ensure_default_admin(&self) -> Result<Option<String>, String> {
        // Race safety (L28): there is no explicit transaction here. The
        // `email UNIQUE` constraint plus `INSERT OR IGNORE` makes a concurrent
        // double-bootstrap idempotent — at most one placeholder-email row can
        // exist, and the post-insert COUNT verifies whether *this* call was
        // the one that created it before returning the generated password.
        let conn = self.conn();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM users", [], |row| row.get(0))
            .map_err(|e| format!("count error: {e}"))?;

        if count == 0 {
            // Generate a cryptographically-random initial password instead of a
            // well-known default (C1 fix). The account is also flagged
            // `must_change_password`, so the gateway forces a reset before any
            // operation is permitted even if this value is observed.
            let default_password = generate_password(24);
            let id = uuid::Uuid::new_v4().to_string();
            let now = chrono::Utc::now().to_rfc3339();
            let password_hash = hash_password(&default_password)?;

            conn.execute(
                "INSERT OR IGNORE INTO users (id, email, display_name, password_hash, role, status, created_at, updated_at, must_change_password, bootstrap_owner)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'active', ?6, ?6, 1, 1)",
                params![id, BOOTSTRAP_PLACEHOLDER_EMAIL, "Administrator", password_hash, "admin", now],
            )
            .map_err(|e| format!("failed to create default admin: {e}"))?;

            // Verify it was actually inserted (not ignored due to race)
            let inserted: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM users WHERE email = ?1",
                    params![BOOTSTRAP_PLACEHOLDER_EMAIL],
                    |row| row.get(0),
                )
                .map_err(|e| format!("failed to verify admin creation: {e}"))?;

            if inserted > 0 {
                warn!("╔════════════════════════════════════════════════════════╗");
                warn!("║  DEFAULT ADMIN CREATED — set a password on first login   ║");
                warn!("║  Open the dashboard from this machine to set the owner   ║");
                warn!("║  email and password. Until then the account is reachable ║");
                warn!("║  only as admin@local with a one-time random password.    ║");
                warn!("╚════════════════════════════════════════════════════════╝");
                return Ok(Some(default_password));
            }
        }
        Ok(None)
    }
}

/// Email the bootstrap owner carries until the first-run claim replaces it.
/// Callers that set no email at claim time (the OS shell OOBE, the installer's
/// `pending-account.json`, the Personal-edition implicit claim) keep it.
pub const BOOTSTRAP_PLACEHOLDER_EMAIL: &str = "admin@local";

/// Error returned when an email is already used by another account. Stable
/// string: the gateway passes it through and the dashboard keys on it.
pub const EMAIL_IN_USE: &str = "email is already in use";

/// Maximum length of an email address (RFC 5321 path limit).
const MAX_EMAIL_LEN: usize = 254;

/// Validate an account email entered by an operator and return it trimmed.
///
/// Deliberately a shape check, not RFC 5322 parsing: one `@` with a non-empty
/// local part and domain, no whitespace or control characters, at most 254
/// bytes. Case is preserved because login matches the stored email exactly.
pub fn validate_account_email(raw: &str) -> Result<&str, &'static str> {
    let email = raw.trim();
    if email.is_empty() {
        return Err("email is required");
    }
    if email.len() > MAX_EMAIL_LEN {
        return Err("email is too long");
    }
    if email.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("invalid email format");
    }
    match email.split_once('@') {
        Some((local, domain))
            if !local.is_empty() && !domain.is_empty() && !domain.contains('@') =>
        {
            Ok(email)
        }
        _ => Err("invalid email format"),
    }
}

/// Generate a cryptographically-random alphanumeric password of `len` chars
/// using the OS RNG. Uses an unbiased rejection-free mapping over a 62-char
/// alphabet by masking to the alphabet size via modulo on uniform bytes drawn
/// until enough are accepted.
pub(crate) fn generate_password(len: usize) -> String {
    use password_hash::rand_core::RngCore;
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut out = String::with_capacity(len);
    let mut rng = password_hash::rand_core::OsRng;
    // Rejection sampling to avoid modulo bias (256 % 62 != 0).
    let max_unbiased = (256u32 / ALPHABET.len() as u32) * ALPHABET.len() as u32;
    while out.len() < len {
        let mut buf = [0u8; 32];
        rng.fill_bytes(&mut buf);
        for &b in buf.iter() {
            if (b as u32) < max_unbiased {
                out.push(ALPHABET[(b as usize) % ALPHABET.len()] as char);
                if out.len() == len {
                    break;
                }
            }
        }
    }
    out
}

// ── Password helpers ─────────────────────────────────────────

/// Maximum password length to prevent Argon2 DoS (HIGH-4 fix).
const MAX_PASSWORD_LEN: usize = 1024;

pub(crate) fn hash_password(password: &str) -> Result<String, String> {
    if password.len() > MAX_PASSWORD_LEN {
        return Err("password too long".to_string());
    }
    let salt = SaltString::generate(&mut password_hash::rand_core::OsRng);
    let argon2 = Argon2::default();
    argon2
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| format!("password hash error: {e}"))
}

/// Pre-computed dummy hash for timing-safe verification when user not found.
/// Panics on init failure — OsRng unavailable means the system cannot run securely.
static DUMMY_HASH: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    hash_password("timing-equalization-dummy")
        .expect("DUMMY_HASH init failed: OsRng unavailable — cannot start securely")
});

pub(crate) fn verify_password_hash(password: &str, stored_hash: &str) -> Result<(), String> {
    let parsed = PasswordHash::new(stored_hash)
        .map_err(|e| format!("invalid stored hash: {e}"))?;
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .map_err(|_| "invalid email or password".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    fn test_db() -> (UserDb, NamedTempFile) {
        let tmp = NamedTempFile::new().unwrap();
        let db = UserDb::new(tmp.path()).unwrap();
        (db, tmp)
    }

    #[test]
    fn create_and_verify_user() {
        let (db, _tmp) = test_db();
        let user = db
            .create_user("test@example.com", "Test User", "password123", UserRole::Employee)
            .unwrap();
        assert_eq!(user.email, "test@example.com");
        assert_eq!(user.role, UserRole::Employee);

        let verified = db.verify_password("test@example.com", "password123").unwrap();
        assert_eq!(verified.id, user.id);
    }

    #[test]
    fn wrong_password_fails() {
        let (db, _tmp) = test_db();
        db.create_user("test@example.com", "Test", "correct-pw", UserRole::Employee)
            .unwrap();
        assert!(db.verify_password("test@example.com", "wrong-pwd").is_err());
    }

    #[test]
    fn agent_binding_roundtrip() {
        let (db, _tmp) = test_db();
        let user = db
            .create_user("test@example.com", "Test", "password1", UserRole::Employee)
            .unwrap();

        db.bind_agent(&user.id, "my-agent", AccessLevel::Owner).unwrap();

        let bindings = db.get_user_agents(&user.id).unwrap();
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].agent_name, "my-agent");
        assert_eq!(bindings[0].access_level, AccessLevel::Owner);

        let access = db.check_agent_access(&user.id, "my-agent").unwrap();
        assert_eq!(access, Some(AccessLevel::Owner));

        let no_access = db.check_agent_access(&user.id, "other-agent").unwrap();
        assert_eq!(no_access, None);
    }

    #[test]
    fn ensure_default_admin_creates_once() {
        let (db, _tmp) = test_db();
        db.ensure_default_admin().unwrap();
        let users = db.list_users().unwrap();
        assert_eq!(users.len(), 1);
        assert_eq!(users[0].role, UserRole::Admin);

        // Calling again should not create a second admin
        db.ensure_default_admin().unwrap();
        let users = db.list_users().unwrap();
        assert_eq!(users.len(), 1);
    }

    #[test]
    fn default_admin_password_is_random_and_forces_change() {
        let (db, _tmp) = test_db();
        let pw = db.ensure_default_admin().unwrap().expect("admin created");

        // C1: never the well-known default, and a strong random length.
        assert_ne!(pw, "admin");
        assert_eq!(pw.len(), 24);
        assert!(pw.chars().all(|c| c.is_ascii_alphanumeric()));

        // The well-known password must not work.
        assert!(db.verify_password("admin@local", "admin").is_err());

        // The generated password works and the account is forced to change it.
        let user = db.verify_password("admin@local", &pw).unwrap();
        assert!(user.must_change_password);

        // Changing the password clears the flag.
        db.update_user(&user.id, None, None, Some("a-new-strong-password"))
            .unwrap();
        let user2 = db
            .verify_password("admin@local", "a-new-strong-password")
            .unwrap();
        assert!(!user2.must_change_password);
    }

    /// WP-F1: the two helpers the Personal-edition local session depends on.
    /// The implicit claim exists solely to clear `must_change_password` — a
    /// token issued while that flag is set is refused on its very next use, so
    /// "it returned a token" would be a false success without this.
    #[test]
    fn random_claim_unblocks_the_bootstrap_admin_and_lookup_by_email_works() {
        let (db, _tmp) = test_db();
        db.ensure_default_admin().unwrap().expect("admin created");
        assert!(db.is_unclaimed_default_admin());

        assert!(db.claim_default_admin_random().unwrap(), "first claim wins");
        assert!(!db.is_unclaimed_default_admin());
        // Single-shot: a second attempt changes nothing.
        assert!(!db.claim_default_admin_random().unwrap());

        let admin = db
            .get_user_by_email("admin@local")
            .unwrap()
            .expect("admin found by email");
        assert_eq!(admin.role, UserRole::Admin);
        assert!(
            !admin.must_change_password,
            "the claim must clear the flag, otherwise every issued token is dead on arrival",
        );
        assert!(db.get_user_by_email("nobody@example.com").unwrap().is_none());
    }

    /// The first-run claim replaces the placeholder email, and the owner stays
    /// findable (claim state, local session lookup) by its flag afterwards.
    #[test]
    fn claim_with_email_replaces_placeholder_and_owner_stays_findable() {
        let (db, _tmp) = test_db();
        db.ensure_default_admin().unwrap().expect("admin created");

        assert!(db
            .claim_default_admin_with_email(Some("owner@example.com"), "a-strong-password")
            .unwrap());
        assert!(!db.is_unclaimed_default_admin());
        assert!(db.get_user_by_email(BOOTSTRAP_PLACEHOLDER_EMAIL).unwrap().is_none());

        let user = db
            .verify_password("owner@example.com", "a-strong-password")
            .expect("log in with the claimed email");
        assert!(!user.must_change_password);
        assert_eq!(user.role, UserRole::Admin);

        let owner = db.get_bootstrap_owner().unwrap().expect("owner by flag");
        assert_eq!(owner.id, user.id);
        assert_eq!(owner.email, "owner@example.com");

        // Single-shot: a later claim cannot move the email again.
        assert!(!db
            .claim_default_admin_with_email(Some("attacker@example.com"), "another-password")
            .unwrap());
        assert!(db.get_user_by_email("attacker@example.com").unwrap().is_none());
    }

    #[test]
    fn claim_without_email_keeps_placeholder() {
        let (db, _tmp) = test_db();
        db.ensure_default_admin().unwrap().expect("admin created");
        assert!(db.claim_default_admin("a-strong-password").unwrap());
        assert!(db
            .verify_password(BOOTSTRAP_PLACEHOLDER_EMAIL, "a-strong-password")
            .is_ok());
    }

    /// An email another account already uses must not claim (the UNIQUE
    /// constraint fails the whole UPDATE), so the instance stays claimable.
    #[test]
    fn claim_with_taken_email_errors_and_stays_unclaimed() {
        let (db, _tmp) = test_db();
        db.ensure_default_admin().unwrap().expect("admin created");
        db.create_user("taken@example.com", "Someone", "password-1", UserRole::Employee)
            .unwrap();
        assert!(db
            .claim_default_admin_with_email(Some("taken@example.com"), "a-strong-password")
            .is_err());
        assert!(db.is_unclaimed_default_admin());
    }

    /// Databases created before `bootstrap_owner` existed: the migration
    /// marks the placeholder-email row as the owner, exactly once.
    #[test]
    fn migration_backfills_bootstrap_owner_on_legacy_db() {
        let tmp = NamedTempFile::new().unwrap();
        {
            let conn = rusqlite::Connection::open(tmp.path()).unwrap();
            conn.execute_batch(
                "CREATE TABLE users (
                    id TEXT PRIMARY KEY,
                    email TEXT UNIQUE NOT NULL,
                    display_name TEXT NOT NULL,
                    password_hash TEXT NOT NULL,
                    role TEXT NOT NULL DEFAULT 'employee',
                    status TEXT NOT NULL DEFAULT 'active',
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    last_login TEXT,
                    must_change_password INTEGER NOT NULL DEFAULT 0
                );",
            )
            .unwrap();
            let hash = hash_password("legacy-bootstrap").unwrap();
            conn.execute(
                "INSERT INTO users VALUES ('legacy-id', 'admin@local', 'Administrator', ?1, 'admin', 'active', 'x', 'x', NULL, 1)",
                params![hash],
            )
            .unwrap();
        }
        let db = UserDb::new(tmp.path()).unwrap();
        assert!(db.is_unclaimed_default_admin());
        assert_eq!(db.get_bootstrap_owner().unwrap().unwrap().id, "legacy-id");
        assert!(db
            .claim_default_admin_with_email(Some("owner@example.com"), "a-strong-password")
            .unwrap());
        // Re-opening must not re-run the backfill against the renamed row.
        drop(db);
        let db = UserDb::new(tmp.path()).unwrap();
        assert_eq!(db.get_bootstrap_owner().unwrap().unwrap().email, "owner@example.com");
    }

    #[test]
    fn change_email_updates_login_and_refuses_taken_address() {
        let (db, _tmp) = test_db();
        let a = db
            .create_user("a@example.com", "A", "password-a", UserRole::Admin)
            .unwrap();
        db.create_user("b@example.com", "B", "password-b", UserRole::Employee)
            .unwrap();

        db.change_email(&a.id, "a-new@example.com").unwrap();
        assert!(db.verify_password("a-new@example.com", "password-a").is_ok());
        assert!(db.verify_password("a@example.com", "password-a").is_err());

        assert_eq!(
            db.change_email(&a.id, "b@example.com").unwrap_err(),
            EMAIL_IN_USE
        );
        assert_eq!(db.get_user(&a.id).unwrap().unwrap().email, "a-new@example.com");
        assert!(db.change_email("no-such-id", "c@example.com").is_err());
    }

    #[test]
    fn validate_account_email_shape() {
        assert_eq!(validate_account_email("  owner@example.com "), Ok("owner@example.com"));
        assert_eq!(validate_account_email("Owner@Example.COM"), Ok("Owner@Example.COM"));
        assert!(validate_account_email("").is_err());
        assert!(validate_account_email("   ").is_err());
        assert!(validate_account_email("no-at-sign").is_err());
        assert!(validate_account_email("@example.com").is_err());
        assert!(validate_account_email("owner@").is_err());
        assert!(validate_account_email("a@b@c").is_err());
        assert!(validate_account_email("own er@example.com").is_err());
        assert!(validate_account_email("owner@exa\u{0}mple.com").is_err());
        let long = format!("{}@example.com", "a".repeat(250));
        assert!(validate_account_email(&long).is_err());
    }

    #[test]
    fn generate_password_is_unbiased_length() {
        let a = generate_password(24);
        let b = generate_password(24);
        assert_eq!(a.len(), 24);
        assert_eq!(b.len(), 24);
        assert_ne!(a, b, "two generated passwords must differ");
    }

    #[test]
    fn suspended_user_cannot_login() {
        let (db, _tmp) = test_db();
        let user = db
            .create_user("test@example.com", "Test", "password123", UserRole::Employee)
            .unwrap();
        db.set_user_status(&user.id, UserStatus::Suspended).unwrap();
        let result = db.verify_password("test@example.com", "password123");
        // Generic error message — doesn't reveal account status (security fix)
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "invalid email or password");
    }

    #[test]
    fn offboarded_user_cannot_login() {
        let (db, _tmp) = test_db();
        let user = db
            .create_user("test@example.com", "Test", "password123", UserRole::Employee)
            .unwrap();
        db.set_user_status(&user.id, UserStatus::Offboarded).unwrap();
        let result = db.verify_password("test@example.com", "password123");
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "invalid email or password");
    }

    #[test]
    fn nonexistent_email_timing_safe() {
        let (db, _tmp) = test_db();
        // Should not panic or behave differently for nonexistent email
        let result = db.verify_password("nobody@example.com", "password123");
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "invalid email or password");
    }

    #[test]
    fn password_too_long_rejected() {
        let (db, _tmp) = test_db();
        let long_pw = "a".repeat(2000);
        let result = db.verify_password("anyone@test.com", &long_pw);
        assert!(result.is_err());
    }

    #[test]
    fn audit_log_roundtrip() {
        let (db, _tmp) = test_db();
        db.log_action(Some("user-1"), "login", None, None, Some("127.0.0.1"))
            .unwrap();
        db.log_action(Some("user-1"), "agent.create", Some("my-agent"), None, None)
            .unwrap();

        let entries = db.query_audit_log(Some("user-1"), None, 10).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].action, "agent.create"); // DESC order
    }
}
