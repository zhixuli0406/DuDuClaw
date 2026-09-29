//! [`CcrStore`] construction, the SQLite connection / schema and the write
//! path (`put*`). Moved verbatim out of `ccr.rs`.

use super::*;

impl CcrStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            ttl_seconds: DEFAULT_TTL_SECONDS,
            max_entries: 1_000,
            last_expiry_sweep: Arc::new(std::sync::atomic::AtomicI64::new(0)),
        }
    }

    pub fn with_limits(mut self, ttl_seconds: i64, max_entries: usize) -> Self {
        self.ttl_seconds = ttl_seconds.max(1);
        self.max_entries = max_entries.max(1);
        self
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub(super) fn open(&self) -> Result<Connection, CcrError> {
        self.open_with_busy_timeout(std::time::Duration::from_secs(5))
    }

    pub(super) fn open_with_busy_timeout(
        &self,
        busy_timeout: std::time::Duration,
    ) -> Result<Connection, CcrError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(&self.path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600))?;
        }
        conn.busy_timeout(busy_timeout)?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "secure_delete", "ON")?;
        // Every read path opens a connection. A file already stamped with the
        // current schema must not pay for the DDL batch and the
        // `BEGIN IMMEDIATE` migration window, which take a write lock and so
        // serialise pure reads (`still_valid` alone opens 3-4 connections).
        let installed_schema_version: i64 =
            conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if installed_schema_version == SCHEMA_VERSION {
            self.sweep_expired_if_due(&conn)?;
            return Ok(conn);
        }
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS ccr_entries (
                id TEXT PRIMARY KEY,
                tenant_id TEXT NOT NULL,
                agent_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                source_acl TEXT NOT NULL,
                source_tool TEXT NOT NULL,
                source_call_id TEXT NOT NULL,
                content_sha256 TEXT NOT NULL,
                transform_version INTEGER NOT NULL DEFAULT 1,
                binding_required INTEGER NOT NULL DEFAULT 0 CHECK(binding_required IN (0,1)),
                original TEXT NOT NULL,
                content_bytes INTEGER NOT NULL,
                created_at INTEGER NOT NULL,
                expires_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_ccr_expiry ON ccr_entries(expires_at);
            -- `find` and every scope-wide revocation filter on the full scope
            -- tuple; without this index they degrade to a table scan over
            -- rows whose `original` can each be 2 MiB.
            CREATE INDEX IF NOT EXISTS idx_ccr_scope
                ON ccr_entries(tenant_id, agent_id, session_id, source_acl);",
        )?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS ccr_retrieval_audit (
                audit_id INTEGER PRIMARY KEY AUTOINCREMENT,
                requested_id_sha256 TEXT NOT NULL,
                tenant_id TEXT NOT NULL,
                agent_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                source_acl TEXT NOT NULL,
                status TEXT NOT NULL CHECK(status IN ('granted', 'refused')),
                returned_bytes INTEGER NOT NULL,
                attempted_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS ccr_revoked_sources (
                tenant_id TEXT NOT NULL,
                agent_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                source_acl TEXT NOT NULL,
                source_tool TEXT NOT NULL,
                source_call_id TEXT NOT NULL,
                revoked_at INTEGER NOT NULL,
                PRIMARY KEY (tenant_id, agent_id, session_id, source_acl, source_tool, source_call_id)
            );
            CREATE TABLE IF NOT EXISTS ccr_revoked_scopes (
                tenant_id TEXT NOT NULL,
                agent_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                source_acl TEXT NOT NULL,
                revoked_at INTEGER NOT NULL,
                PRIMARY KEY (tenant_id, agent_id, session_id, source_acl)
            );
            CREATE TABLE IF NOT EXISTS ccr_artifact_bindings (
                entry_id TEXT PRIMARY KEY REFERENCES ccr_entries(id) ON DELETE CASCADE,
                tenant_id TEXT NOT NULL,
                connector TEXT NOT NULL,
                artifact_id TEXT NOT NULL,
                version TEXT NOT NULL,
                acl_revision TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_ccr_artifact_binding
                ON ccr_artifact_bindings(tenant_id,connector,artifact_id,version);
            CREATE TABLE IF NOT EXISTS ccr_revoked_artifact_versions (
                tenant_id TEXT NOT NULL,
                connector TEXT NOT NULL,
                artifact_id TEXT NOT NULL,
                version TEXT NOT NULL,
                revoked_at INTEGER NOT NULL,
                PRIMARY KEY (tenant_id,connector,artifact_id,version)
            );",
        )?;
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let entry_columns = conn
            .prepare("PRAGMA table_info(ccr_entries)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<Vec<_>, _>>()?;
        if !entry_columns
            .iter()
            .any(|column| column == "transform_version")
        {
            conn.execute(
                "ALTER TABLE ccr_entries ADD COLUMN transform_version INTEGER NOT NULL DEFAULT 1",
                [],
            )?;
        }
        if !entry_columns
            .iter()
            .any(|column| column == "binding_required")
        {
            conn.execute(
                "ALTER TABLE ccr_entries ADD COLUMN binding_required INTEGER NOT NULL DEFAULT 0 CHECK(binding_required IN (0,1))",
                [],
            )?;
        }
        conn.execute(
            "UPDATE ccr_entries SET binding_required=1 WHERE binding_required=0
             AND EXISTS (SELECT 1 FROM ccr_artifact_bindings b WHERE b.entry_id=ccr_entries.id)",
            [],
        )?;
        conn.execute_batch(
            "CREATE TRIGGER IF NOT EXISTS ccr_binding_marks_entry
             AFTER INSERT ON ccr_artifact_bindings BEGIN
               UPDATE ccr_entries SET binding_required=1 WHERE id=NEW.entry_id;
             END;",
        )?;
        // `ccr_loop_telemetry` is created lazily by `record_loop_telemetry`,
        // so its `CREATE TABLE IF NOT EXISTS` cannot add a column to a table a
        // pre-`SCHEMA_VERSION = 2` binary already created. This is the only
        // place that upgrade can happen; absent table ⇒ nothing to migrate,
        // the lazy create already carries the current shape.
        let telemetry_exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master
             WHERE type='table' AND name='ccr_loop_telemetry')",
            [],
            |row| row.get(0),
        )?;
        if telemetry_exists {
            let telemetry_columns = conn
                .prepare("PRAGMA table_info(ccr_loop_telemetry)")?
                .query_map([], |row| row.get::<_, String>(1))?
                .collect::<Result<Vec<_>, _>>()?;
            if !telemetry_columns
                .iter()
                .any(|column| column == "ccr_find_rate_limited")
            {
                conn.execute(
                    "ALTER TABLE ccr_loop_telemetry
                     ADD COLUMN ccr_find_rate_limited INTEGER NOT NULL DEFAULT 0",
                    [],
                )?;
            }
        }
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        conn.execute_batch("COMMIT")?;
        self.sweep_expired_if_due(&conn)?;
        Ok(conn)
    }

    /// Delete expired originals at most once per
    /// [`EXPIRY_SWEEP_MIN_INTERVAL_SECONDS`] per store handle. Throttled, not
    /// removed: the sweep is what actually erases expired bytes from disk,
    /// but it is a write and every read used to pay for it.
    fn sweep_expired_if_due(&self, conn: &Connection) -> Result<(), CcrError> {
        use std::sync::atomic::Ordering;
        let now = unix_now();
        let last = self.last_expiry_sweep.load(Ordering::Relaxed);
        if last != 0 && now.saturating_sub(last) < EXPIRY_SWEEP_MIN_INTERVAL_SECONDS {
            return Ok(());
        }
        // Claim the slot before the DELETE so two threads racing on the same
        // handle issue one sweep, not two.
        if self
            .last_expiry_sweep
            .compare_exchange(last, now, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Ok(());
        }
        conn.execute("DELETE FROM ccr_entries WHERE expires_at <= ?1", [now])?;
        Ok(())
    }

    /// Commit the original before returning an ID that may be shown to a model.
    pub fn put(
        &self,
        scope: &CcrScope,
        source_tool: &str,
        source_call_id: &str,
        original: &str,
    ) -> Result<CcrEntry, CcrError> {
        self.put_with_artifact(scope, source_tool, source_call_id, original, None, None)
    }

    /// Bind an original to a connector-verified artifact version and ACL
    /// revision. Callers must authenticate and authorize the source first.
    pub fn put_bound(
        &self,
        scope: &CcrScope,
        source_tool: &str,
        source_call_id: &str,
        original: &str,
        artifact: &CcrSourceArtifact,
    ) -> Result<CcrEntry, CcrError> {
        self.put_with_artifact(
            scope,
            source_tool,
            source_call_id,
            original,
            Some(artifact),
            None,
        )
    }

    /// Connector-backed write whose CCR lifetime cannot exceed the upstream
    /// artifact retention deadline. The deadline must be a future Unix second;
    /// expiry is checked again on every search and retrieval.
    pub fn put_bound_until(
        &self,
        scope: &CcrScope,
        source_tool: &str,
        source_call_id: &str,
        original: &str,
        artifact: &CcrSourceArtifact,
        retention_at: i64,
    ) -> Result<CcrEntry, CcrError> {
        self.put_with_artifact(
            scope,
            source_tool,
            source_call_id,
            original,
            Some(artifact),
            Some(retention_at),
        )
    }

    fn put_with_artifact(
        &self,
        scope: &CcrScope,
        source_tool: &str,
        source_call_id: &str,
        original: &str,
        artifact: Option<&CcrSourceArtifact>,
        retention_at: Option<i64>,
    ) -> Result<CcrEntry, CcrError> {
        if !scope.valid() || source_tool.trim().is_empty() || source_call_id.trim().is_empty() {
            return Err(CcrError::InvalidScope);
        }
        if artifact.is_some_and(|artifact| !artifact.valid()) {
            return Err(CcrError::InvalidScope);
        }
        if original.len() > MAX_ORIGINAL_BYTES {
            return Err(CcrError::TooLarge);
        }
        let mut conn = self.open()?;
        let now = unix_now();
        if retention_at.is_some_and(|deadline| deadline <= now) {
            return Err(CcrError::Revoked);
        }
        let expires_at = now
            .saturating_add(self.ttl_seconds)
            .min(retention_at.unwrap_or(i64::MAX));
        let id = Uuid::new_v4().to_string();
        let digest = format!("{:x}", Sha256::digest(original.as_bytes()));
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if let Some(artifact) = artifact {
            let revoked: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM ccr_revoked_artifact_versions WHERE
                 tenant_id=?1 AND connector=?2 AND artifact_id=?3 AND version=?4)",
                params![
                    scope.tenant_id,
                    artifact.connector,
                    artifact.artifact_id,
                    artifact.version
                ],
                |row| row.get(0),
            )?;
            if revoked {
                return Err(CcrError::Revoked);
            }
        }
        let revoked: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM ccr_revoked_sources WHERE
             tenant_id=?1 AND agent_id=?2 AND session_id=?3 AND source_acl=?4
             AND source_tool=?5 AND source_call_id=?6)",
            params![
                scope.tenant_id,
                scope.agent_id,
                scope.session_id,
                scope.source_acl,
                source_tool,
                source_call_id
            ],
            |row| row.get(0),
        )?;
        if revoked {
            return Err(CcrError::Revoked);
        }
        let scope_revoked: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM ccr_revoked_scopes WHERE
             tenant_id=?1 AND agent_id=?2 AND session_id=?3 AND source_acl=?4)",
            params![
                scope.tenant_id,
                scope.agent_id,
                scope.session_id,
                scope.source_acl
            ],
            |row| row.get(0),
        )?;
        if scope_revoked {
            return Err(CcrError::Revoked);
        }
        tx.execute("DELETE FROM ccr_entries WHERE expires_at <= ?1", [now])?;
        // A call ID names one exact post-redaction result inside this scope.
        // Replayed identical calls reuse the committed handle; conflicting
        // content revokes every old handle before any new marker can appear.
        let existing = {
            let mut stmt = tx.prepare(
                "SELECT id, original, content_sha256, content_bytes, transform_version,
                        created_at, expires_at, binding_required
                 FROM ccr_entries WHERE tenant_id=?1 AND agent_id=?2 AND session_id=?3
                 AND source_acl=?4 AND source_tool=?5 AND source_call_id=?6
                 ORDER BY created_at DESC, rowid DESC",
            )?;
            stmt.query_map(
                params![
                    scope.tenant_id,
                    scope.agent_id,
                    scope.session_id,
                    scope.source_acl,
                    source_tool,
                    source_call_id
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, i64>(6)?,
                        row.get::<_, i64>(7)?,
                    ))
                },
            )?
            .collect::<Result<Vec<_>, _>>()?
        };
        let conflicting_binding = existing.iter().try_fold(
            false,
            |conflict, (id, _, _, _, _, _, _, binding_required)| {
                let saved: Option<(String, String, String, String)> = tx.query_row(
                "SELECT connector,artifact_id,version,acl_revision FROM ccr_artifact_bindings
                 WHERE entry_id=?1", [id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            ).optional()?;
                let matches = match (saved, artifact) {
                    (None, None) => *binding_required == 0,
                    (Some((connector, id, version, acl)), Some(expected)) => {
                        *binding_required == 1
                            && connector == expected.connector
                            && id == expected.artifact_id
                            && version == expected.version
                            && acl == expected.acl_revision
                    }
                    _ => false,
                };
                Ok::<bool, rusqlite::Error>(conflict || !matches)
            },
        )?;
        if conflicting_binding
            || existing
                .iter()
                .any(|(_, text, saved_digest, bytes, version, _, _, _)| {
                    *version != CCR_ENTRY_VERSION
                        || *bytes != text.len() as i64
                        || text != original
                        || saved_digest != &digest
                })
        {
            tx.execute(
                "INSERT OR IGNORE INTO ccr_revoked_sources
                 (tenant_id,agent_id,session_id,source_acl,source_tool,source_call_id,revoked_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![
                    scope.tenant_id,
                    scope.agent_id,
                    scope.session_id,
                    scope.source_acl,
                    source_tool,
                    source_call_id,
                    now
                ],
            )?;
            tx.execute(
                "DELETE FROM ccr_entries WHERE tenant_id=?1 AND agent_id=?2
                 AND session_id=?3 AND source_acl=?4 AND source_tool=?5 AND source_call_id=?6",
                params![
                    scope.tenant_id,
                    scope.agent_id,
                    scope.session_id,
                    scope.source_acl,
                    source_tool,
                    source_call_id
                ],
            )?;
            tx.commit()?;
            return Err(CcrError::Revoked);
        }
        if let Some((existing_id, _, _, bytes, version, created_at, existing_expiry, _)) =
            existing.into_iter().next()
        {
            if existing_expiry > expires_at {
                // A source shortened its retention while reusing the same
                // call ID. Never return a handle with the older, longer TTL.
                tx.execute(
                    "UPDATE ccr_entries SET expires_at=?1 WHERE id=?2",
                    params![expires_at, existing_id],
                )?;
            }
            tx.commit()?;
            return Ok(CcrEntry {
                id: existing_id,
                scope: scope.clone(),
                source_tool: source_tool.to_owned(),
                source_call_id: source_call_id.to_owned(),
                content_sha256: digest,
                transform_version: version,
                content_bytes: bytes as usize,
                created_at,
                expires_at: existing_expiry.min(expires_at),
            });
        }
        tx.execute(
            "INSERT INTO ccr_entries
             (id, tenant_id, agent_id, session_id, source_acl, source_tool, source_call_id,
              content_sha256, transform_version, binding_required, original, content_bytes, created_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                id,
                scope.tenant_id,
                scope.agent_id,
                scope.session_id,
                scope.source_acl,
                source_tool,
                source_call_id,
                digest,
                CCR_ENTRY_VERSION,
                i64::from(artifact.is_some()),
                original,
                original.len() as i64,
                now,
                expires_at
            ],
        )?;
        if let Some(artifact) = artifact {
            tx.execute(
                "INSERT INTO ccr_artifact_bindings
                 (entry_id,tenant_id,connector,artifact_id,version,acl_revision)
                 VALUES (?1,?2,?3,?4,?5,?6)",
                params![
                    id,
                    scope.tenant_id,
                    artifact.connector,
                    artifact.artifact_id,
                    artifact.version,
                    artifact.acl_revision
                ],
            )?;
        }
        tx.execute(
            "DELETE FROM ccr_entries WHERE id IN
             (SELECT id FROM ccr_entries ORDER BY created_at DESC, rowid DESC LIMIT -1 OFFSET ?1)",
            [self.max_entries as i64],
        )?;
        let mut total_bytes: i64 = tx.query_row(
            "SELECT COALESCE(SUM(content_bytes), 0) FROM ccr_entries",
            [],
            |row| row.get(0),
        )?;
        while total_bytes > MAX_STORE_BYTES as i64 {
            let (oldest_id, oldest_bytes): (String, i64) = tx.query_row(
                "SELECT id, content_bytes FROM ccr_entries ORDER BY created_at, rowid LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            tx.execute("DELETE FROM ccr_entries WHERE id = ?1", [&oldest_id])?;
            total_bytes -= oldest_bytes;
        }
        tx.commit()?;
        Ok(CcrEntry {
            id,
            scope: scope.clone(),
            source_tool: source_tool.to_owned(),
            source_call_id: source_call_id.to_owned(),
            content_sha256: digest,
            transform_version: CCR_ENTRY_VERSION,
            content_bytes: original.len(),
            created_at: now,
            expires_at,
        })
    }
}
