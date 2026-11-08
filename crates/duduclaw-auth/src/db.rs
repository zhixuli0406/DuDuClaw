use std::path::Path;
use std::sync::Mutex;

use argon2::password_hash::SaltString;
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use tracing::warn;
use uuid::Uuid;

use crate::models::*;
use crate::org::OrgError;
use crate::user_update::{UserUpdate, UserWriteError};
use crate::user_row::{
    bump_acl_generation, has_column, load_user, user_from_row, USER_COLUMNS,
};

/// SQLite-backed user database with connection pool.
pub struct UserDb {
    pool: Vec<Mutex<Connection>>,
    pub(crate) remember_key: ring::hmac::Key,
}

const POOL_SIZE: usize = 2;

impl UserDb {
    /// Linearize a nonblocking delivery with every ACL writer, including other
    /// processes. A WAL read transaction alone would not exclude revocation.
    /// The callback must not await, block, or re-enter this database.
    pub fn with_acl_generation<T>(&self, expected: u64, deliver: impl FnOnce() -> T) -> Result<T, String> {
        let mut conn = self.pool.iter().find_map(|slot| slot.try_lock().ok())
            .ok_or_else(|| "ACL delivery connection busy".to_string())?;
        conn.busy_timeout(std::time::Duration::ZERO).map_err(|e| e.to_string())?;
        let result = (|| {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|e| e.to_string())?;
            if crate::user_row::read_acl_generation(&tx)? != expected {
                return Err("ACL changed before delivery".into());
            }
            // No database mutation: dropping the transaction releases the
            // writer reservation only after the synchronous delivery decision.
            Ok(deliver())
        })();
        let restored = conn.busy_timeout(std::time::Duration::from_secs(5));
        // The callback may already have sent bytes; never disguise its result
        // as a refusal because restoring the connection's timeout failed.
        if let Err(error) = restored { warn!(%error, "ACL connection timeout restore failed"); }
        result
    }

    /// Open (or create) the user database at the given path.
    pub fn new(db_path: &Path) -> Result<Self, String> {
        Self::new_with_mfa_default(db_path, false)
    }

    /// Seed a newly created MFA policy table atomically with the verified
    /// installation default. Existing policy values are never overwritten.
    pub fn new_with_mfa_default(db_path: &Path, enforced: bool) -> Result<Self, String> {
        let mut pool = Vec::with_capacity(POOL_SIZE);
        for _ in 0..POOL_SIZE {
            let conn = Connection::open(db_path)
                .map_err(|e| format!("failed to open user db: {e}"))?;
            conn.execute_batch("PRAGMA busy_timeout=5000; PRAGMA journal_mode=WAL;")
                .map_err(|e| format!("failed to set pragmas: {e}"))?;
            pool.push(Mutex::new(conn));
        }
        let remember_key = crate::remembered_sessions::load_key(db_path, &pool[0].lock().unwrap())?;
        let db = Self { pool, remember_key };
        db.init_tables(enforced)?;
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

    fn init_tables(&self, enforced: bool) -> Result<(), String> {
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

        // Old setup tokens have no purpose binding; invalidate them during upgrade.
        if !has_column(&conn, "setup_tokens", "recovery")? {
            conn.execute_batch("BEGIN IMMEDIATE;
                ALTER TABLE setup_tokens ADD COLUMN recovery INTEGER NOT NULL DEFAULT 0;
                ALTER TABLE setup_tokens ADD COLUMN attempts INTEGER NOT NULL DEFAULT 0;
                UPDATE setup_tokens SET consumed = 1;
                COMMIT;").map_err(|e| format!("setup token migration failed: {e}"))?;
        }
        conn.execute_batch("CREATE TABLE IF NOT EXISTS setup_claim_rate (
            id INTEGER PRIMARY KEY CHECK(id = 1), window_start INTEGER NOT NULL,
            attempts INTEGER NOT NULL
        );").map_err(|e| format!("setup rate table failed: {e}"))?;

        crate::google_identity::migrate(&conn)?;
        crate::mfa::migrate(&conn, enforced)?;
        crate::remembered_sessions::migrate(&conn)?;
        crate::user_invites::migrate(&conn)?;
        crate::status_tokens::migrate(&conn)?;

        // Idempotent migrations: ADD COLUMN errors if the column is already
        // present, so each one probes table_info first.
        if !has_column(&conn, "users", "must_change_password")? {
            conn.execute(
                "ALTER TABLE users ADD COLUMN must_change_password INTEGER NOT NULL DEFAULT 0",
                [],
            )
            .map_err(|e| format!("failed to migrate users table: {e}"))?;
        }

        // `department` column (install-approval routing).
        if !has_column(&conn, "users", "department")? {
            conn.execute("ALTER TABLE users ADD COLUMN department TEXT", [])
                .map_err(|e| format!("failed to migrate users.department: {e}"))?;
        }

        // `bootstrap_owner` marks the row `ensure_default_admin` created, so
        // the first-run claim and the Personal-edition local session can find
        // the owner after the claim replaced its placeholder email. Databases
        // from before this column existed only ever had the owner under the
        // placeholder address, so the backfill runs once, in the same step
        // that adds the column.
        if !has_column(&conn, "users", "bootstrap_owner")? {
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

        // `manager_user_id`: the person-to-person reporting line (`org.rs`).
        // Existing rows stay NULL (no manager); nothing is backfilled.
        if !has_column(&conn, "users", "manager_user_id")? {
            conn.execute("ALTER TABLE users ADD COLUMN manager_user_id TEXT", [])
                .map_err(|e| format!("failed to migrate users.manager_user_id: {e}"))?;
        }
        if !has_column(&conn, "users", "primary_agent_id")? {
            conn.execute("ALTER TABLE users ADD COLUMN primary_agent_id TEXT", [])
                .map_err(|e| format!("failed to migrate users.primary_agent_id: {e}"))?;
        }
        crate::onboarding::migrate(&conn)?;
        crate::invitation_content::migrate(&conn)?;
        // `auth_meta.acl_generation` invalidates every cached live ACL: each
        // authorization-relevant write bumps it in the same transaction.
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_users_manager ON users(manager_user_id);
             CREATE TABLE IF NOT EXISTS auth_meta (key TEXT PRIMARY KEY, value INTEGER NOT NULL);
             INSERT OR IGNORE INTO auth_meta (key, value) VALUES ('acl_generation', 1);",
        )
        .map_err(|e| format!("failed to migrate auth_meta: {e}"))?;
        migrate_isolation_personal_agents(&conn)?;
        crate::isolation::initialize_tenant(&conn)?;
        // Last: its triggers read users and every MFA table (F6 S1/S16).
        crate::admin_eligibility::migrate(&conn)?;
        Ok(())
    }

    /// Set a personal preference, checking the current binding in the same write.
    /// NULL clears the preference; only an Owner/Operator binding may be selected.
    pub fn set_primary_agent(&self, user_id: &str, agent_id: Option<&str>) -> Result<(), String> {
        let conn = self.conn();
        let changed = conn.execute(
            "UPDATE users SET primary_agent_id = ?1, updated_at = ?2
             WHERE id = ?3 AND status = 'active' AND
             (?1 IS NULL OR EXISTS(SELECT 1 FROM user_agent_bindings
                 WHERE user_id = ?3 AND agent_name = ?1
                   AND access_level IN ('owner', 'operator')))",
            params![agent_id, Utc::now().to_rfc3339(), user_id],
        ).map_err(|e| format!("set primary agent: {e}"))?;
        if changed == 1 { Ok(()) } else { Err("agent requires an Owner or Operator binding".into()) }
    }

    /// Resolve a stale preference against live bindings; clearing stays NULL.
    pub fn primary_agent(&self, user_id: &str) -> Result<Option<String>, String> {
        let conn = self.conn();
        conn.query_row(
            "SELECT CASE WHEN primary_agent_id IS NULL THEN NULL
             WHEN EXISTS(SELECT 1 FROM user_agent_bindings b
                 WHERE b.user_id = users.id AND b.agent_name = primary_agent_id
                   AND b.access_level IN ('owner', 'operator'))
             THEN primary_agent_id ELSE
                 (SELECT agent_name FROM user_agent_bindings b
                  WHERE b.user_id = users.id AND b.access_level = 'owner'
                  ORDER BY bound_at, agent_name LIMIT 1) END
             FROM users WHERE id = ?1 AND status = 'active'",
            [user_id], |row| row.get(0),
        ).optional().map(|value| value.flatten())
            .map_err(|e| format!("read primary agent: {e}"))
    }

    /// The explicitly selected primary employee, only while still bound.
    /// WebChat must not guess another owner when that preference becomes stale.
    pub fn bound_primary_agent(&self, user_id: &str) -> Result<Option<String>, String> {
        self.conn().query_row(
            "SELECT primary_agent_id FROM users WHERE id = ?1 AND status = 'active'
             AND EXISTS(SELECT 1 FROM user_agent_bindings b
                 WHERE b.user_id = users.id AND b.agent_name = primary_agent_id
                   AND b.access_level IN ('owner', 'operator'))",
            [user_id], |row| row.get(0),
        ).optional().map(|value| value.flatten())
            .map_err(|e| format!("read bound primary agent: {e}"))
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
            credential_epoch: 0,
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
            manager_user_id: None,
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

        // Fetch the row, then give the pooled connection back before the
        // (deliberately slow) argon2 check: the pool has two connections and
        // every RPC's live ACL read needs one.
        let row_result = {
            let conn = self.conn();
            conn.query_row(
                &format!("SELECT {USER_COLUMNS}, password_hash FROM users WHERE email = ?1"),
                params![email],
                |row| Ok((user_from_row(row)?, row.get::<_, String>(12)?)),
            )
        };

        // A missing row and a corrupt role/status take the same path, so
        // neither is distinguishable from a wrong password.
        let (user, stored_hash) = match row_result {
            Ok(r) => r,
            Err(_) => {
                // Timing-safe: perform dummy hash to equalize response time
                let _ = verify_password_hash(password, &DUMMY_HASH);
                return Err("invalid email or password".to_string());
            }
        };

        // Verify password (always runs, whether user exists or not handled above)
        verify_password_hash(password, &stored_hash)?;

        // Check status — use generic message to prevent status enumeration
        if user.status != UserStatus::Active {
            return Err("invalid email or password".to_string());
        }
        Ok(user)
    }

    /// Get a user by ID.
    pub fn get_user(&self, user_id: &str) -> Result<Option<User>, String> {
        let conn = self.conn();
        load_user(&conn, user_id)
    }

    /// Get a user by email address, without presenting a credential.
    ///
    /// Deliberately separate from [`Self::verify_password`]: callers must have
    /// established authority some other way. The only caller today is the
    /// Personal-edition local session endpoint, whose authority is the TCP peer
    /// address being loopback (`server.rs::handle_local_session`).
    pub fn get_user_by_email(&self, email: &str) -> Result<Option<User>, String> {
        let conn = self.conn();
        conn.query_row(
            &format!("SELECT {USER_COLUMNS} FROM users WHERE email = ?1"),
            params![email],
            user_from_row,
        )
        .optional()
        .map_err(|e| format!("query error: {e}"))
    }

    /// List all users.
    pub fn list_users(&self) -> Result<Vec<User>, String> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(&format!("SELECT {USER_COLUMNS} FROM users ORDER BY created_at"))
            .map_err(|e| format!("query error: {e}"))?;
        let rows = stmt
            .query_map([], user_from_row)
            .map_err(|e| format!("query error: {e}"))?;
        rows.collect::<Result<Vec<User>, _>>()
            .map_err(|e| format!("row error: {e}"))
    }

    /// Update a user's display_name, role and/or password, all or nothing
    /// (see [`Self::apply_user_update`], which also covers department and
    /// manager). A role or password change bumps the ACL generation.
    /// Demoting someone to Employee while they still have direct reports is
    /// refused with [`crate::org::MANAGER_HAS_REPORTS`] and changes nothing:
    /// an Employee cannot be a manager.
    pub fn update_user(
        &self,
        user_id: &str,
        display_name: Option<&str>,
        role: Option<UserRole>,
        password: Option<&str>,
    ) -> Result<(), String> {
        let update = UserUpdate {
            display_name,
            role,
            password,
            ..UserUpdate::default()
        };
        self.apply_user_update(user_id, &update).map_err(|e| match e {
            UserWriteError::Org(OrgError::HasReports) => crate::org::MANAGER_HAS_REPORTS.to_string(),
            UserWriteError::Org(OrgError::UserNotFound) => "user not found".to_string(),
            other => format!("update error: {other}"),
        })
    }

    /// Change a user's login email. `email` must already have passed
    /// [`validate_account_email`]. An address another account already uses
    /// returns [`EMAIL_IN_USE`] and changes nothing.
    pub fn change_email(&self, user_id: &str, email: &str) -> Result<(), String> {
        let now = Utc::now().to_rfc3339();
        let mut conn = self.conn();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("update error: {e}"))?;
        let affected = tx
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
        // The email is part of the live context.
        bump_acl_generation(&tx).map_err(|e| format!("update error: {e}"))?;
        tx.commit().map_err(|e| format!("update error: {e}"))
    }

    /// Set a user's status (active / suspended / offboarded).
    ///
    /// Reporting lines are kept as they are; `users.offboard` goes through
    /// [`Self::offboard_user`], which also moves the leaver's reports.
    pub fn set_user_status(&self, user_id: &str, status: UserStatus) -> Result<(), String> {
        self.set_user_status_as(None, user_id, status)
    }

    /// [`Self::set_user_status`] attributed to the acting Admin in the
    /// admin eligibility log (same transaction, F6 S1/S2).
    pub fn set_user_status_as(&self, actor: Option<&str>, user_id: &str, status: UserStatus) -> Result<(), String> {
        let now = Utc::now().to_rfc3339();
        let mut conn = self.conn();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("update error: {e}"))?;
        let since = crate::admin_eligibility::high_water(&tx)?;
        let affected = tx
            .execute(
                "UPDATE users SET status = ?1, updated_at = ?2 WHERE id = ?3",
                params![status.to_string(), now, user_id],
            )
            .map_err(|e| format!("update error: {e}"))?;

        if affected == 0 {
            return Err("user not found".to_string());
        }
        if let Some(actor) = actor { crate::admin_eligibility::stamp_actor(&tx, since, actor)?; }
        bump_acl_generation(&tx).map_err(|e| format!("update error: {e}"))?;
        tx.commit().map_err(|e| format!("update error: {e}"))
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
        let mut conn = self.conn();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("claim error: {e}"))?;
        let affected = tx
            .execute(
                "UPDATE users SET password_hash = ?1, updated_at = ?2, must_change_password = 0, \
                 email = COALESCE(?3, email) \
                 WHERE bootstrap_owner = 1 AND must_change_password = 1",
                params![hash, now, email],
            )
            .map_err(|e| format!("claim error: {e}"))?;
        if affected == 1 {
            // Clears the forced-change flag (and maybe the email) of a live
            // context.
            bump_acl_generation(&tx).map_err(|e| format!("claim error: {e}"))?;
        }
        tx.commit().map_err(|e| format!("claim error: {e}"))?;
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
        let mut conn = self.conn();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("bind error: {e}"))?;
        bind_agent_in_transaction(&tx, user_id, agent_name, access_level, &now)?;
        tx.commit().map_err(|e| format!("bind error: {e}"))
    }

    /// Unbind a user from an agent.
    pub fn unbind_agent(&self, user_id: &str, agent_name: &str) -> Result<(), String> {
        let mut conn = self.conn();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("unbind error: {e}"))?;
        tx.execute(
            "DELETE FROM user_agent_bindings WHERE user_id = ?1 AND agent_name = ?2",
            params![user_id, agent_name],
        )
        .map_err(|e| format!("unbind error: {e}"))?;
        bump_acl_generation(&tx).map_err(|e| format!("unbind error: {e}"))?;
        tx.commit().map_err(|e| format!("unbind error: {e}"))
    }

    /// Get all agents bound to a user.
    pub fn get_user_agents(&self, user_id: &str) -> Result<Vec<UserAgentBinding>, String> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT user_id, agent_name, access_level, bound_at, is_primary
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
                    row.get::<_, bool>(4)?,
                ))
            })
            .map_err(|e| format!("query error: {e}"))?;

        let mut bindings = Vec::new();
        for row in rows {
            let (uid, agent, level_str, bound, is_primary) = row.map_err(|e| format!("row error: {e}"))?;
            let access_level: AccessLevel = level_str.parse()
                .map_err(|e: String| format!("corrupt access_level in DB: {e}"))?;
            bindings.push(UserAgentBinding { user_id: uid, agent_name: agent, access_level, bound_at: bound, is_primary });
        }
        Ok(bindings)
    }

    /// Get all users bound to an agent.
    pub fn get_agent_users(&self, agent_name: &str) -> Result<Vec<UserAgentBinding>, String> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT user_id, agent_name, access_level, bound_at, is_primary
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
                    row.get::<_, bool>(4)?,
                ))
            })
            .map_err(|e| format!("query error: {e}"))?;

        let mut bindings = Vec::new();
        for row in rows {
            let (uid, agent, level_str, bound, is_primary) = row.map_err(|e| format!("row error: {e}"))?;
            let access_level: AccessLevel = level_str.parse()
                .map_err(|e: String| format!("corrupt access_level in DB: {e}"))?;
            bindings.push(UserAgentBinding { user_id: uid, agent_name: agent, access_level, bound_at: bound, is_primary });
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
    fn acl_delivery_generation_orders_cross_connection_writes() {
        let (db, tmp) = test_db();
        let other = Connection::open(tmp.path()).unwrap();
        other.busy_timeout(std::time::Duration::ZERO).unwrap();
        let generation = db.acl_generation().unwrap();
        let mut delivered = 0;
        db.with_acl_generation(generation, || {
            assert!(other.execute("UPDATE auth_meta SET value=value+1 WHERE key='acl_generation'", []).is_err());
            delivered += 1;
        }).unwrap();
        assert_eq!(delivered, 1);
        other.execute("UPDATE auth_meta SET value=value+1 WHERE key='acl_generation'", []).unwrap();
        assert!(db.with_acl_generation(generation, || delivered += 1).is_err());
        assert_eq!(delivered, 1, "stale generations must never enter the writer callback");
    }

    #[test]
    fn acl_delivery_generation_fails_closed_on_busy_and_unreadable_store() {
        let (db, tmp) = test_db();
        let other = Connection::open(tmp.path()).unwrap();
        let generation = db.acl_generation().unwrap();
        other.execute_batch("BEGIN IMMEDIATE").unwrap();
        assert!(db.with_acl_generation(generation, || panic!("busy must deny")).is_err());
        other.execute_batch("ROLLBACK; DELETE FROM auth_meta").unwrap();
        assert!(db.with_acl_generation(generation, || panic!("missing generation must deny")).is_err());
        other.execute_batch("DROP TABLE auth_meta").unwrap();
        assert!(db.with_acl_generation(generation, || panic!("unreadable generation must deny")).is_err());
    }

    #[test]
    fn primary_agent_checks_binding_falls_back_and_clears() {
        let (db, _tmp) = test_db();
        let user = db.create_user("primary@example.com", "Primary", "password1", UserRole::Employee).unwrap();
        assert!(db.set_primary_agent(&user.id, Some("unbound")).is_err());
        db.bind_agent(&user.id, "owner", AccessLevel::Owner).unwrap();
        db.bind_agent(&user.id, "chosen", AccessLevel::Operator).unwrap();
        db.set_primary_agent(&user.id, Some("chosen")).unwrap();
        assert_eq!(db.primary_agent(&user.id).unwrap().as_deref(), Some("chosen"));
        assert_eq!(db.bound_primary_agent(&user.id).unwrap().as_deref(), Some("chosen"));
        db.unbind_agent(&user.id, "chosen").unwrap();
        assert_eq!(db.primary_agent(&user.id).unwrap().as_deref(), Some("owner"));
        assert_eq!(db.bound_primary_agent(&user.id).unwrap(), None);
        db.set_primary_agent(&user.id, None).unwrap();
        assert_eq!(db.primary_agent(&user.id).unwrap(), None);
        db.set_primary_agent(&user.id, Some("owner")).unwrap();
        db.unbind_agent(&user.id, "owner").unwrap();
        assert_eq!(db.primary_agent(&user.id).unwrap(), None);
    }

    #[test]
    fn primary_agent_requires_live_operator_access_even_for_legacy_preferences() {
        let (db, _tmp) = test_db();
        let user = db.create_user("viewer@example.com", "Viewer", "password1", UserRole::Employee).unwrap();
        db.bind_agent(&user.id, "chosen", AccessLevel::Viewer).unwrap();
        assert!(db.set_primary_agent(&user.id, Some("chosen")).is_err());
        assert_eq!(db.primary_agent(&user.id).unwrap(), None);
        for access in [AccessLevel::Operator, AccessLevel::Owner] {
            db.bind_agent(&user.id, "chosen", access).unwrap();
            db.set_primary_agent(&user.id, Some("chosen")).unwrap();
            assert_eq!(db.bound_primary_agent(&user.id).unwrap().as_deref(), Some("chosen"));
            db.bind_agent(&user.id, "chosen", AccessLevel::Viewer).unwrap();
            assert_eq!(db.bound_primary_agent(&user.id).unwrap(), None);
            assert_eq!(db.primary_agent(&user.id).unwrap(), None);
        }
        // A corrupt access level is never a grant; clearing remains possible.
        db.conn().execute("UPDATE user_agent_bindings SET access_level='unknown' WHERE user_id=?1", [&user.id]).unwrap();
        assert!(db.set_primary_agent(&user.id, Some("chosen")).is_err());
        assert_eq!(db.bound_primary_agent(&user.id).unwrap(), None);
        assert_eq!(db.primary_agent(&user.id).unwrap(), None);
        db.set_primary_agent(&user.id, None).unwrap();
        db.bind_agent(&user.id, "chosen", AccessLevel::Operator).unwrap();
        db.set_primary_agent(&user.id, Some("chosen")).unwrap();
        db.set_user_status(&user.id, UserStatus::Suspended).unwrap();
        assert!(db.set_primary_agent(&user.id, Some("chosen")).is_err());
        assert_eq!(db.primary_agent(&user.id).unwrap(), None);
        assert_eq!(db.bound_primary_agent(&user.id).unwrap(), None);
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

    // ── Personal mirror ──────────────────────────────────────

    fn personal_tx<T>(db: &UserDb, write: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<T, PersonalAgentError>)
        -> Result<T, PersonalAgentError> {
        let mut conn = db.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).unwrap();
        let result = write(&tx);
        if result.is_ok() { tx.commit().unwrap(); }
        result
    }

    fn members(db: &UserDb, agent: &str) -> Vec<String> {
        let mut users: Vec<String> = db.get_agent_users(agent).unwrap().into_iter().map(|b| b.user_id).collect();
        users.sort();
        users
    }

    fn mirror_owner(db: &UserDb, agent: &str) -> Option<String> {
        personal_agent_owner(&db.conn(), agent).unwrap()
    }

    #[test]
    fn personal_owner_binding_is_idempotent_and_exclusive() {
        let (db, _tmp) = test_db();
        let alice = db.create_user("alice@example.com", "Alice", "password1", UserRole::Employee).unwrap();
        let bob = db.create_user("bob@example.com", "Bob", "password1", UserRole::Admin).unwrap();
        // No mirror (or another owner's) is never bound.
        assert_eq!(personal_tx(&db, |tx| bind_personal_owner_in_transaction(tx, "alice-personal", &alice.id)),
            Err(PersonalAgentError::OwnerConflict));
        assert!(personal_tx(&db, |tx| insert_personal_agent_in_transaction(tx, "alice-personal", &alice.id)).unwrap());
        assert_eq!(personal_tx(&db, |tx| bind_personal_owner_in_transaction(tx, "alice-personal", &bob.id)),
            Err(PersonalAgentError::OwnerConflict));
        db.bind_agent(&alice.id, "alice-personal", AccessLevel::Viewer).unwrap();
        // An existing lower binding is raised to Owner, then re-apply is a no-op.
        assert!(personal_tx(&db, |tx| bind_personal_owner_in_transaction(tx, "alice-personal", &alice.id)).unwrap());
        let generation = db.acl_generation().unwrap();
        assert!(!personal_tx(&db, |tx| bind_personal_owner_in_transaction(tx, "alice-personal", &alice.id)).unwrap());
        assert_eq!(db.acl_generation().unwrap(), generation);
        assert_eq!(members(&db, "alice-personal"), vec![alice.id.clone()]);
        assert_eq!(db.check_agent_access(&alice.id, "alice-personal").unwrap(), Some(AccessLevel::Owner));
        assert_eq!(personal_tx(&db, |tx| bind_personal_owner_in_transaction(tx, "dashboard", &alice.id)),
            Err(PersonalAgentError::InvalidAgent));
    }

    #[test]
    fn personal_agent_refuses_second_binding() {
        let (db, _tmp) = test_db();
        let alice = db.create_user("alice@example.com", "Alice", "password1", UserRole::Employee).unwrap();
        let bob = db.create_user("bob@example.com", "Bob", "password1", UserRole::Admin).unwrap();
        assert!(personal_tx(&db, |tx| insert_personal_agent_in_transaction(tx, "alice-personal", &alice.id)).unwrap());
        db.bind_agent(&alice.id, "alice-personal", AccessLevel::Owner).unwrap();
        let conflict = PersonalAgentError::BindingConflict.to_string();
        for access in [AccessLevel::Viewer, AccessLevel::Operator, AccessLevel::Owner] {
            let generation = db.acl_generation().unwrap();
            assert_eq!(db.bind_agent(&bob.id, "alice-personal", access).unwrap_err(), conflict);
            assert_eq!(db.acl_generation().unwrap(), generation, "a refused binding must not bump the ACL");
        }
        // Writers that bypass bind_agent_in_transaction are stopped by triggers.
        for statement in [
            "INSERT INTO user_agent_bindings(user_id,agent_name,access_level,bound_at) VALUES(?1,'alice-personal','owner','now')",
            "INSERT OR REPLACE INTO user_agent_bindings(user_id,agent_name,access_level,bound_at) VALUES(?1,'alice-personal','owner','now')",
        ] {
            assert!(db.conn().execute(statement, [&bob.id]).is_err(), "{statement}");
        }
        assert!(db.conn().execute("UPDATE user_agent_bindings SET user_id=?1 WHERE agent_name='alice-personal'", [&bob.id]).is_err());
        db.bind_agent(&bob.id, "team", AccessLevel::Owner).unwrap();
        assert!(db.conn().execute("UPDATE user_agent_bindings SET agent_name='alice-personal' WHERE agent_name='team'", []).is_err());
        assert_eq!(members(&db, "alice-personal"), vec![alice.id.clone()]);
        assert_eq!(db.check_agent_access(&bob.id, "alice-personal").unwrap(), None);
    }

    #[test]
    fn personal_agent_owner_rebind_is_idempotent() {
        let (db, _tmp) = test_db();
        let alice = db.create_user("alice@example.com", "Alice", "password1", UserRole::Employee).unwrap();
        let bob = db.create_user("bob@example.com", "Bob", "password1", UserRole::Employee).unwrap();
        let before = db.acl_generation().unwrap();
        assert!(personal_tx(&db, |tx| insert_personal_agent_in_transaction(tx, "alice-personal", &alice.id)).unwrap());
        let recorded = db.acl_generation().unwrap();
        assert!(recorded > before, "recording a Personal owner invalidates cached ACLs");
        assert!(!personal_tx(&db, |tx| insert_personal_agent_in_transaction(tx, "alice-personal", &alice.id)).unwrap());
        assert_eq!(db.acl_generation().unwrap(), recorded, "idempotent re-apply writes nothing");
        assert_eq!(personal_tx(&db, |tx| insert_personal_agent_in_transaction(tx, "alice-personal", &bob.id)),
            Err(PersonalAgentError::OwnerConflict));
        for access in [AccessLevel::Owner, AccessLevel::Owner, AccessLevel::Operator, AccessLevel::Viewer, AccessLevel::Owner] {
            db.bind_agent(&alice.id, "alice-personal", access).unwrap();
            assert_eq!(db.check_agent_access(&alice.id, "alice-personal").unwrap(), Some(access));
        }
        assert_eq!(members(&db, "alice-personal"), vec![alice.id.clone()]);
        assert_eq!(mirror_owner(&db, "alice-personal").as_deref(), Some(alice.id.as_str()));
        // The owner binding already existing is no obstacle to (re)recording.
        assert!(!personal_tx(&db, |tx| insert_personal_agent_in_transaction(tx, "alice-personal", &alice.id)).unwrap());
    }

    #[test]
    fn personal_mirror_insert_is_immutable() {
        let (db, _tmp) = test_db();
        let alice = db.create_user("alice@example.com", "Alice", "password1", UserRole::Employee).unwrap();
        let bob = db.create_user("bob@example.com", "Bob", "password1", UserRole::Employee).unwrap();
        personal_tx(&db, |tx| insert_personal_agent_in_transaction(tx, "alice-personal", &alice.id)).unwrap();
        db.bind_agent(&alice.id, "alice-personal", AccessLevel::Owner).unwrap();
        for statement in [
            "UPDATE isolation_personal_agents SET owner_user_id=?1",
            "UPDATE isolation_personal_agents SET created_at='later' WHERE ?1 IS NOT NULL",
            "INSERT OR REPLACE INTO isolation_personal_agents(agent_name,owner_user_id,created_at) VALUES('alice-personal',?1,'now')",
            "INSERT INTO isolation_personal_agents(agent_name,owner_user_id,created_at) VALUES('alice-personal',?1,'now')
             ON CONFLICT(agent_name) DO UPDATE SET owner_user_id=excluded.owner_user_id",
            "DELETE FROM isolation_personal_agents WHERE ?1 IS NOT NULL",
        ] {
            assert!(db.conn().execute(statement, [&bob.id]).is_err(), "{statement}");
            assert_eq!(mirror_owner(&db, "alice-personal").as_deref(), Some(alice.id.as_str()));
        }
        assert_eq!(personal_tx(&db, |tx| remove_personal_agent_in_transaction(tx, "alice-personal", &bob.id)),
            Err(PersonalAgentError::OwnerConflict));
        assert_eq!(mirror_owner(&db, "alice-personal").as_deref(), Some(alice.id.as_str()));

        // Tombstone: only the explicit lifecycle removal deletes the row, and
        // it takes the agent's bindings with it.
        let generation = db.acl_generation().unwrap();
        assert!(personal_tx(&db, |tx| remove_personal_agent_in_transaction(tx, "alice-personal", &alice.id)).unwrap());
        assert!(db.acl_generation().unwrap() > generation);
        assert_eq!(mirror_owner(&db, "alice-personal"), None);
        assert!(members(&db, "alice-personal").is_empty());
        let grants: i64 = db.conn().query_row("SELECT count(*) FROM isolation_personal_agent_removals", [], |r| r.get(0)).unwrap();
        assert_eq!(grants, 0, "removal grants never outlive their transaction");
        assert!(!personal_tx(&db, |tx| remove_personal_agent_in_transaction(tx, "alice-personal", &alice.id)).unwrap());
        // A recreated employee with the same label is a fresh record.
        assert!(personal_tx(&db, |tx| insert_personal_agent_in_transaction(tx, "alice-personal", &bob.id)).unwrap());
        assert_eq!(mirror_owner(&db, "alice-personal").as_deref(), Some(bob.id.as_str()));
    }

    #[test]
    fn personal_mirror_refuses_invalid_unknown_and_existing_members() {
        let (db, _tmp) = test_db();
        let alice = db.create_user("alice@example.com", "Alice", "password1", UserRole::Employee).unwrap();
        let bob = db.create_user("bob@example.com", "Bob", "password1", UserRole::Employee).unwrap();
        for agent in ["", "../x", "__system", duduclaw_core::UNTRUSTED_AGENT_ID] {
            assert_eq!(personal_tx(&db, |tx| insert_personal_agent_in_transaction(tx, agent, &alice.id)),
                Err(PersonalAgentError::InvalidAgent), "{agent}");
            assert_eq!(personal_tx(&db, |tx| remove_personal_agent_in_transaction(tx, agent, &alice.id)),
                Err(PersonalAgentError::InvalidAgent), "{agent}");
        }
        for owner in ["", "missing-user"] {
            assert_eq!(personal_tx(&db, |tx| insert_personal_agent_in_transaction(tx, "solo", owner)),
                Err(PersonalAgentError::UnknownOwner));
        }
        db.bind_agent(&alice.id, "team", AccessLevel::Owner).unwrap();
        db.bind_agent(&bob.id, "team", AccessLevel::Viewer).unwrap();
        assert_eq!(personal_tx(&db, |tx| insert_personal_agent_in_transaction(tx, "team", &alice.id)),
            Err(PersonalAgentError::BindingConflict));
        assert!(db.conn().execute("INSERT INTO isolation_personal_agents(agent_name,owner_user_id,created_at) VALUES('team',?1,'now')",
            [&alice.id]).is_err(), "the trigger refuses a mirror over existing members");
        assert_eq!(mirror_owner(&db, "team"), None);
        assert_eq!(mirror_owner(&db, "solo"), None);
        // An unreadable mirror refuses every binding instead of treating the agent as Shared.
        db.conn().execute_batch("DROP TABLE isolation_personal_agents").unwrap();
        assert!(db.bind_agent(&alice.id, "team", AccessLevel::Owner).is_err());
        assert!(personal_agent_owner(&db.conn(), "team").is_err());
    }

    #[test]
    fn shared_agent_binding_behaviour_is_unchanged() {
        let (db, _tmp) = test_db();
        let alice = db.create_user("alice@example.com", "Alice", "password1", UserRole::Employee).unwrap();
        let bob = db.create_user("bob@example.com", "Bob", "password1", UserRole::Employee).unwrap();
        let carol = db.create_user("carol@example.com", "Carol", "password1", UserRole::Employee).unwrap();
        let start = db.acl_generation().unwrap();
        db.bind_agent(&alice.id, "team", AccessLevel::Owner).unwrap();
        db.bind_agent(&bob.id, "team", AccessLevel::Owner).unwrap();
        db.bind_agent(&carol.id, "team", AccessLevel::Viewer).unwrap();
        db.bind_agent(&carol.id, "team", AccessLevel::Operator).unwrap();
        assert_eq!(db.acl_generation().unwrap(), start + 4);
        let mut expected = vec![alice.id.clone(), bob.id.clone(), carol.id.clone()];
        expected.sort();
        assert_eq!(members(&db, "team"), expected);
        assert_eq!(db.check_agent_access(&carol.id, "team").unwrap(), Some(AccessLevel::Operator));
        // The second Owner still promotes the single previous Owner to primary.
        let primary: Vec<String> = db.get_agent_users("team").unwrap().into_iter()
            .filter(|b| b.is_primary).map(|b| b.user_id).collect();
        assert_eq!(primary, vec![alice.id.clone()]);
        // Migration's Shared writer goes through the same function unchanged.
        db.conn().execute("UPDATE users SET last_login='2026-11-08T00:00:00Z' WHERE id=?1", [&carol.id]).unwrap();
        db.migration_bind_shared("abcd-1", &[7; 32], &carol.id, "other", AccessLevel::Operator).unwrap();
        assert_eq!(db.check_agent_access(&carol.id, "other").unwrap(), Some(AccessLevel::Operator));
        assert_eq!(mirror_owner(&db, "team"), None);
    }
}

/// Shared ordinary/migration writer; caller owns the immediate transaction.
///
/// Personal exclusivity (native Linux isolation): when `agent_name` has an
/// `isolation_personal_agents` mirror row, only the recorded owner may be
/// bound (at any access level), and only while no other user is bound. Any
/// other user is refused with [`PersonalAgentError::BindingConflict`] whatever
/// level is requested. Agents without a mirror row keep the Shared behaviour
/// unchanged. An unreadable mirror refuses every binding.
pub(crate) fn bind_agent_in_transaction(
    tx: &rusqlite::Transaction<'_>, user_id: &str, agent_name: &str,
    access_level: AccessLevel, now: &str,
) -> Result<(), PersonalAgentError> {
        if let Some(owner) = personal_agent_owner(tx, agent_name)? {
            if owner != user_id || other_members_exist(tx, agent_name, user_id)? {
                return Err(PersonalAgentError::BindingConflict);
            }
        }
        if access_level == AccessLevel::Owner {
            let owners: Vec<String> = {
                let mut statement = tx.prepare("SELECT user_id FROM user_agent_bindings WHERE agent_name = ?1 AND access_level = 'owner' AND user_id != ?2")
                    .map_err(PersonalAgentError::store)?;
                statement.query_map(params![agent_name, user_id], |row| row.get(0)).map_err(PersonalAgentError::store)?
                    .collect::<Result<Vec<_>, _>>().map_err(PersonalAgentError::store)?
            };
            if let [previous] = owners.as_slice() {
                let updated = tx.execute("UPDATE user_agent_bindings SET is_primary = 1 WHERE agent_name = ?1 AND user_id = ?2 AND NOT EXISTS(SELECT 1 FROM user_agent_bindings WHERE agent_name = ?1 AND is_primary = 1)", params![agent_name, previous]).map_err(PersonalAgentError::store)?;
                if updated > 0 {
                    tx.execute("INSERT INTO auth_audit_log(user_id, action, target, detail, timestamp) VALUES(NULL, 'agent.primary_owner_auto', ?1, ?2, ?3)",
                        params![agent_name, serde_json::json!({"primary_user_id": previous}).to_string(), now]).map_err(PersonalAgentError::store)?;
                }
            }
        }
        tx.execute(
            "INSERT INTO user_agent_bindings (user_id, agent_name, access_level, bound_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(user_id, agent_name) DO UPDATE SET access_level = excluded.access_level, bound_at = excluded.bound_at,
             is_primary = CASE WHEN excluded.access_level = 'owner' THEN user_agent_bindings.is_primary ELSE 0 END",
            params![user_id, agent_name, access_level.to_string(), now],
        )
        .map_err(|e| PersonalAgentError::Store(format!("bind error: {e}")))?;
        bump_acl_generation(&tx).map_err(|e| PersonalAgentError::Store(format!("bind error: {e}")))?;
    Ok(())
}

// ── Personal employee mirror (native Linux isolation) ───────────────────

/// Refusals of the users.db Personal-employee mirror and of binding writes
/// that would break Personal exclusivity. `Display` strings are stable, so a
/// caller holding only the `String` from [`UserDb::bind_agent`] can compare
/// against `PersonalAgentError::BindingConflict.to_string()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PersonalAgentError {
    /// The agent is a Personal employee and the user is not its owner, or
    /// another user is already bound to it.
    BindingConflict,
    /// A mirror row already exists for this agent with a different owner
    /// (rows are immutable), or the expected owner does not match on removal.
    OwnerConflict,
    /// The agent id is not a valid, non-reserved employee id.
    InvalidAgent,
    /// No users row has the given owner id.
    UnknownOwner,
    /// The store could not be read or written; always a refusal.
    Store(String),
}

impl PersonalAgentError {
    fn store(error: impl std::fmt::Display) -> Self { Self::Store(error.to_string()) }
}

impl std::fmt::Display for PersonalAgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BindingConflict => f.write_str("personal agent binding conflict"),
            Self::OwnerConflict => f.write_str("personal agent owner conflict"),
            Self::InvalidAgent => f.write_str("invalid personal agent"),
            Self::UnknownOwner => f.write_str("personal agent owner unknown"),
            Self::Store(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for PersonalAgentError {}

impl From<PersonalAgentError> for String {
    fn from(error: PersonalAgentError) -> Self { error.to_string() }
}

/// Idempotent schema for the Personal mirror. Besides the explicit checks in
/// the Rust writers, triggers enforce the invariants for every SQL writer in
/// this crate (member creation, offboarding transfer, raw upserts):
/// - mirror rows are never updated or replaced (`INSERT OR REPLACE` is caught
///   by the BEFORE INSERT trigger, before REPLACE deletes the old row);
/// - a mirror row is deleted only together with a same-transaction removal
///   grant, which only [`remove_personal_agent_in_transaction`] writes;
/// - no binding for a non-owner user can be inserted for a Personal agent, and
///   a mirror row cannot be created while another user is bound.
fn migrate_isolation_personal_agents(conn: &Connection) -> Result<(), String> {
    let tx = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(|e| format!("failed to migrate isolation_personal_agents: {e}"))?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS isolation_personal_agents (
            agent_name TEXT PRIMARY KEY NOT NULL CHECK (length(agent_name) > 0),
            owner_user_id TEXT NOT NULL CHECK (length(owner_user_id) > 0),
            created_at TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS isolation_personal_agent_removals (
            agent_name TEXT PRIMARY KEY NOT NULL,
            owner_user_id TEXT NOT NULL
         );
         CREATE TRIGGER IF NOT EXISTS isolation_personal_agents_no_replace
           BEFORE INSERT ON isolation_personal_agents
           WHEN EXISTS(SELECT 1 FROM isolation_personal_agents WHERE agent_name = NEW.agent_name)
           BEGIN SELECT RAISE(ABORT, 'personal agent mirror is immutable'); END;
         CREATE TRIGGER IF NOT EXISTS isolation_personal_agents_exclusive_insert
           BEFORE INSERT ON isolation_personal_agents
           WHEN EXISTS(SELECT 1 FROM user_agent_bindings
                       WHERE agent_name = NEW.agent_name AND user_id != NEW.owner_user_id)
           BEGIN SELECT RAISE(ABORT, 'personal agent binding conflict'); END;
         CREATE TRIGGER IF NOT EXISTS isolation_personal_agents_no_update
           BEFORE UPDATE ON isolation_personal_agents
           BEGIN SELECT RAISE(ABORT, 'personal agent mirror is immutable'); END;
         CREATE TRIGGER IF NOT EXISTS isolation_personal_agents_guarded_delete
           BEFORE DELETE ON isolation_personal_agents
           WHEN NOT EXISTS(SELECT 1 FROM isolation_personal_agent_removals
                           WHERE agent_name = OLD.agent_name AND owner_user_id = OLD.owner_user_id)
           BEGIN SELECT RAISE(ABORT, 'personal agent mirror removal requires the lifecycle path'); END;
         CREATE TRIGGER IF NOT EXISTS isolation_personal_binding_insert
           BEFORE INSERT ON user_agent_bindings
           WHEN EXISTS(SELECT 1 FROM isolation_personal_agents
                       WHERE agent_name = NEW.agent_name AND owner_user_id != NEW.user_id)
           BEGIN SELECT RAISE(ABORT, 'personal agent binding conflict'); END;
         CREATE TRIGGER IF NOT EXISTS isolation_personal_binding_update
           BEFORE UPDATE OF user_id, agent_name ON user_agent_bindings
           WHEN EXISTS(SELECT 1 FROM isolation_personal_agents
                       WHERE agent_name = NEW.agent_name AND owner_user_id != NEW.user_id)
           BEGIN SELECT RAISE(ABORT, 'personal agent binding conflict'); END;",
    )
    .map_err(|e| format!("failed to migrate isolation_personal_agents: {e}"))?;
    tx.commit().map_err(|e| format!("failed to migrate isolation_personal_agents: {e}"))
}

fn is_personal_agent_id(agent: &str) -> bool {
    duduclaw_core::is_valid_agent_id(agent) && !duduclaw_core::is_reserved_agent_id(agent)
        && agent != duduclaw_core::UNTRUSTED_AGENT_ID
}

fn other_members_exist(conn: &Connection, agent: &str, user_id: &str) -> Result<bool, PersonalAgentError> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM user_agent_bindings WHERE agent_name = ?1 AND user_id != ?2)",
        params![agent, user_id], |row| row.get(0),
    ).map_err(PersonalAgentError::store)
}

/// The recorded Personal owner of `agent`, or `None` when the agent has no
/// mirror row (Shared or unknown). Evidence only: it never authenticates a
/// user, and use-time checks compare it with the confirmed employee lifecycle.
/// A missing or unreadable mirror table is an error, never `None`.
pub fn personal_agent_owner(conn: &Connection, agent: &str) -> Result<Option<String>, PersonalAgentError> {
    conn.query_row(
        "SELECT owner_user_id FROM isolation_personal_agents WHERE agent_name = ?1",
        [agent], |row| row.get::<_, String>(0),
    )
    .optional()
    .map_err(|e| PersonalAgentError::Store(format!("personal agent mirror unavailable: {e}")))
}

/// Record `agent` as a Personal employee owned by `owner_user_id`.
///
/// This is the API for the gateway employee lifecycle apply path (A2/A4). The
/// caller owns an IMMEDIATE transaction on users.db and commits it; nothing
/// here commits. Returns `Ok(true)` when the row was inserted (the ACL
/// generation is bumped in the same transaction) and `Ok(false)` when the
/// identical owner was already recorded (idempotent re-apply, no write).
///
/// Refuses with:
/// - `InvalidAgent` for an invalid, reserved or untrusted agent id;
/// - `OwnerConflict` when a row exists for a different owner (immutable);
/// - `UnknownOwner` when no users row has `owner_user_id`;
/// - `BindingConflict` when any other user is bound to the agent.
///
/// Owner status (active, logged in, no forced password change) is the
/// lifecycle's check, not this mirror's.
pub fn insert_personal_agent_in_transaction(
    tx: &rusqlite::Transaction<'_>, agent: &str, owner_user_id: &str,
) -> Result<bool, PersonalAgentError> {
    if !is_personal_agent_id(agent) { return Err(PersonalAgentError::InvalidAgent); }
    if owner_user_id.is_empty() { return Err(PersonalAgentError::UnknownOwner); }
    let existing = personal_agent_owner(tx, agent)?;
    if existing.as_deref().is_some_and(|owner| owner != owner_user_id) {
        return Err(PersonalAgentError::OwnerConflict);
    }
    let owner_exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM users WHERE id = ?1)", [owner_user_id], |row| row.get(0),
    ).map_err(PersonalAgentError::store)?;
    if !owner_exists { return Err(PersonalAgentError::UnknownOwner); }
    if other_members_exist(tx, agent, owner_user_id)? { return Err(PersonalAgentError::BindingConflict); }
    if existing.is_some() { return Ok(false); }
    let now = Utc::now().to_rfc3339();
    tx.execute(
        "INSERT INTO isolation_personal_agents (agent_name, owner_user_id, created_at) VALUES (?1, ?2, ?3)",
        params![agent, owner_user_id, now],
    ).map_err(PersonalAgentError::store)?;
    tx.execute(
        "INSERT INTO auth_audit_log(user_id, action, target, detail, timestamp)
         VALUES(NULL, 'isolation.personal_agent_recorded', ?1, ?2, ?3)",
        params![agent, serde_json::json!({"owner_user_id": owner_user_id}).to_string(), now],
    ).map_err(PersonalAgentError::store)?;
    bump_acl_generation(tx).map_err(PersonalAgentError::store)?;
    Ok(true)
}

/// Bind the recorded Personal owner of `agent` to it with Owner access, so
/// the employee has exactly one member (native Linux isolation v1.81, A4).
///
/// Gateway employee lifecycle apply path only, in the same caller-owned
/// IMMEDIATE users.db transaction as [`insert_personal_agent_in_transaction`];
/// nothing here commits. Returns `Ok(false)` without any write when the owner
/// is already bound with Owner access (idempotent re-apply), `Ok(true)` after
/// binding (the ACL generation is bumped by the binding writer).
///
/// Refuses with `OwnerConflict` when no mirror row names `owner_user_id`
/// (missing or another owner) and `BindingConflict` when any other user is
/// bound to the agent.
pub fn bind_personal_owner_in_transaction(
    tx: &rusqlite::Transaction<'_>, agent: &str, owner_user_id: &str,
) -> Result<bool, PersonalAgentError> {
    if !is_personal_agent_id(agent) { return Err(PersonalAgentError::InvalidAgent); }
    if personal_agent_owner(tx, agent)?.as_deref() != Some(owner_user_id) {
        return Err(PersonalAgentError::OwnerConflict);
    }
    if other_members_exist(tx, agent, owner_user_id)? { return Err(PersonalAgentError::BindingConflict); }
    let current: Option<String> = tx.query_row(
        "SELECT access_level FROM user_agent_bindings WHERE agent_name = ?1 AND user_id = ?2",
        params![agent, owner_user_id], |row| row.get(0),
    ).optional().map_err(PersonalAgentError::store)?;
    if current.as_deref().and_then(|level| level.parse::<AccessLevel>().ok()) == Some(AccessLevel::Owner) {
        return Ok(false);
    }
    bind_agent_in_transaction(tx, owner_user_id, agent, AccessLevel::Owner, &Utc::now().to_rfc3339())?;
    Ok(true)
}

/// Remove the Personal mirror row of a tombstoned employee.
///
/// **Only the gateway employee lifecycle apply path may call this**, in the
/// same users.db transaction in which it records the employee as tombstoned.
/// No RPC, migration, repair or cleanup path may call it: deleting the row
/// turns the agent name back into an unrestricted (Shared) binding target.
///
/// In the caller's IMMEDIATE transaction it deletes the mirror row and every
/// `user_agent_bindings` row of the agent (a Personal binding never outlives
/// its employee, and a recreated employee with the same label must not
/// inherit it), audits, and bumps the ACL generation. Returns `Ok(false)`
/// without any write when no row exists, `OwnerConflict` when the recorded
/// owner differs from `owner_user_id`.
pub fn remove_personal_agent_in_transaction(
    tx: &rusqlite::Transaction<'_>, agent: &str, owner_user_id: &str,
) -> Result<bool, PersonalAgentError> {
    if !is_personal_agent_id(agent) { return Err(PersonalAgentError::InvalidAgent); }
    let Some(owner) = personal_agent_owner(tx, agent)? else { return Ok(false) };
    if owner != owner_user_id { return Err(PersonalAgentError::OwnerConflict); }
    tx.execute(
        "INSERT INTO isolation_personal_agent_removals (agent_name, owner_user_id) VALUES (?1, ?2)",
        params![agent, owner_user_id],
    ).map_err(PersonalAgentError::store)?;
    let deleted = tx.execute(
        "DELETE FROM isolation_personal_agents WHERE agent_name = ?1 AND owner_user_id = ?2",
        params![agent, owner_user_id],
    ).map_err(PersonalAgentError::store)?;
    if deleted != 1 { return Err(PersonalAgentError::Store("personal agent mirror removal failed".into())); }
    tx.execute("DELETE FROM isolation_personal_agent_removals WHERE agent_name = ?1", [agent])
        .map_err(PersonalAgentError::store)?;
    tx.execute("DELETE FROM user_agent_bindings WHERE agent_name = ?1", [agent])
        .map_err(PersonalAgentError::store)?;
    tx.execute(
        "INSERT INTO auth_audit_log(user_id, action, target, detail, timestamp)
         VALUES(NULL, 'isolation.personal_agent_removed', ?1, ?2, ?3)",
        params![agent, serde_json::json!({"owner_user_id": owner_user_id}).to_string(), Utc::now().to_rfc3339()],
    ).map_err(PersonalAgentError::store)?;
    bump_acl_generation(tx).map_err(PersonalAgentError::store)?;
    Ok(true)
}

/// Remove every member binding of a tombstoned **Shared** employee (native
/// Linux isolation v1.81, F6 §3.2 step 5).
///
/// **Only the gateway employee lifecycle tombstone (and its F6 recovery) may
/// call this**, in a caller-owned IMMEDIATE users.db transaction; nothing
/// here commits. Refuses `OwnerConflict` while a Personal mirror exists for
/// the agent (a Personal employee is removed through
/// [`remove_personal_agent_in_transaction`]). Deletes all
/// `user_agent_bindings` rows of the agent, audits the count, and bumps the
/// ACL generation even when no binding was left (a retry of a half-finished
/// tombstone still invalidates every cached live ACL). Returns the number of
/// bindings removed.
pub fn remove_shared_bindings_in_transaction(
    tx: &rusqlite::Transaction<'_>, agent: &str,
) -> Result<usize, PersonalAgentError> {
    if !is_personal_agent_id(agent) { return Err(PersonalAgentError::InvalidAgent); }
    if personal_agent_owner(tx, agent)?.is_some() { return Err(PersonalAgentError::OwnerConflict); }
    let removed = tx.execute("DELETE FROM user_agent_bindings WHERE agent_name = ?1", [agent])
        .map_err(PersonalAgentError::store)?;
    tx.execute(
        "INSERT INTO auth_audit_log(user_id, action, target, detail, timestamp)
         VALUES(NULL, 'isolation.shared_agent_bindings_removed', ?1, ?2, ?3)",
        params![agent, serde_json::json!({"removed": removed}).to_string(), Utc::now().to_rfc3339()],
    ).map_err(PersonalAgentError::store)?;
    bump_acl_generation(tx).map_err(PersonalAgentError::store)?;
    Ok(removed)
}
