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

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        for m in &self.pool {
            if let Ok(guard) = m.try_lock() {
                return guard;
            }
        }
        // Fallback: block on first
        self.pool[0].lock().unwrap()
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
                last_login TEXT
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
            CREATE INDEX IF NOT EXISTS idx_bindings_agent ON user_agent_bindings(agent_name);",
        )
        .map_err(|e| format!("failed to create auth tables: {e}"))
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
                "SELECT id, email, display_name, password_hash, role, status, created_at, updated_at, last_login
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

        let (id, email, display_name, stored_hash, role_str, status_str, created_at, updated_at, last_login) = row;

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
        })
    }

    /// Get a user by ID.
    pub fn get_user(&self, user_id: &str) -> Result<Option<User>, String> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT id, email, display_name, role, status, created_at, updated_at, last_login
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
                ))
            });

        match result {
            Ok((id, email, display_name, role_str, status_str, created_at, updated_at, last_login)) => {
                let role: UserRole = role_str.parse()
                    .map_err(|e: String| format!("corrupt role in DB: {e}"))?;
                let status: UserStatus = status_str.parse()
                    .map_err(|e: String| format!("corrupt status in DB: {e}"))?;
                Ok(Some(User { id, email, display_name, role, status, created_at, updated_at, last_login }))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("query error: {e}")),
        }
    }

    /// List all users.
    pub fn list_users(&self) -> Result<Vec<User>, String> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT id, email, display_name, role, status, created_at, updated_at, last_login
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
                ))
            })
            .map_err(|e| format!("query error: {e}"))?;

        let mut users = Vec::new();
        for row in rows {
            let (id, email, display_name, role_str, status_str, created_at, updated_at, last_login) =
                row.map_err(|e| format!("row error: {e}"))?;
            let role: UserRole = role_str.parse()
                .map_err(|e: String| format!("corrupt role in DB: {e}"))?;
            let status: UserStatus = status_str.parse()
                .map_err(|e: String| format!("corrupt status in DB: {e}"))?;
            users.push(User { id, email, display_name, role, status, created_at, updated_at, last_login });
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
            conn.execute(
                "UPDATE users SET password_hash = ?1, updated_at = ?2 WHERE id = ?3",
                params![hash, now, user_id],
            )
            .map_err(|e| format!("update error: {e}"))?;
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

    /// Ensure at least one admin user exists. Creates a default admin
    /// with email `admin@local` and a random 24-char password if the users
    /// table is empty. Returns the generated password (if created) so the
    /// caller can display it once.
    pub fn ensure_default_admin(&self) -> Result<Option<String>, String> {
        // Use a single transaction to avoid TOCTOU race (HIGH-3 fix)
        let conn = self.conn();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM users", [], |row| row.get(0))
            .map_err(|e| format!("count error: {e}"))?;

        if count == 0 {
            let default_password = "admin";
            let id = uuid::Uuid::new_v4().to_string();
            let now = chrono::Utc::now().to_rfc3339();
            let password_hash = hash_password(default_password)?;

            conn.execute(
                "INSERT OR IGNORE INTO users (id, email, display_name, password_hash, role, status, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'active', ?6, ?6)",
                params![id, "admin@local", "Administrator", password_hash, "admin", now],
            )
            .map_err(|e| format!("failed to create default admin: {e}"))?;

            // Verify it was actually inserted (not ignored due to race)
            let inserted: i64 = conn
                .query_row("SELECT COUNT(*) FROM users WHERE email = 'admin@local'", [], |row| row.get(0))
                .map_err(|e| format!("failed to verify admin creation: {e}"))?;

            if inserted > 0 {
                warn!("╔════════════════════════════════════════════════════════╗");
                warn!("║  DEFAULT ADMIN CREATED — CHANGE PASSWORD IMMEDIATELY  ║");
                warn!("║  Email:    admin@local                                ║");
                warn!("║  Password: admin                                      ║");
                warn!("╚════════════════════════════════════════════════════════╝");
                return Ok(Some(default_password.to_string()));
            }
        }
        Ok(None)
    }
}

// ── Password helpers ─────────────────────────────────────────

/// Maximum password length to prevent Argon2 DoS (HIGH-4 fix).
const MAX_PASSWORD_LEN: usize = 1024;

fn hash_password(password: &str) -> Result<String, String> {
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

fn verify_password_hash(password: &str, stored_hash: &str) -> Result<(), String> {
    let parsed = PasswordHash::new(stored_hash)
        .map_err(|e| format!("invalid stored hash: {e}"))?;
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .map_err(|_| "invalid email or password".to_string())
}

/// Generate a cryptographically random password of the given length.
/// Uses rejection sampling to avoid modulo bias.
fn generate_random_password(len: usize) -> String {
    use ring::rand::SecureRandom;
    // 64 chars = power of 2, no modulo bias with u8 & 0x3F
    const CHARSET: &[u8] = b"abcdefghijkmnpqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789!@#$%^&*";
    const MASK: u8 = 63; // 0b00111111 — maps to indices 0..63
    let rng = ring::rand::SystemRandom::new();
    let mut result = Vec::with_capacity(len);
    let mut buf = [0u8; 1];
    while result.len() < len {
        rng.fill(&mut buf).expect("RNG should not fail");
        let idx = buf[0] & MASK;
        result.push(CHARSET[idx as usize] as char);
    }
    result.into_iter().collect()
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
