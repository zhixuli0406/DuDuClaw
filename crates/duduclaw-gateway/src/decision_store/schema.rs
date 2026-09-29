use super::*;

impl DecisionStore {
    pub(crate) fn open(&self) -> Result<Connection, DecisionStoreError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(&self.path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600))?;
        }
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "secure_delete", "ON")?;
        // Every read path opens a connection; an already-current file must not
        // pay for the DDL batch and the `BEGIN IMMEDIATE` migration window,
        // which serialise pure reads against any concurrent writer.
        let installed_schema_version: i64 =
            conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if installed_schema_version == SCHEMA_VERSION {
            return Ok(conn);
        }
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS decision_inputs (
            tenant_id TEXT NOT NULL, acl TEXT NOT NULL, kind TEXT NOT NULL,
            input_id TEXT NOT NULL, schema_version INTEGER NOT NULL,
            payload_sha256 TEXT NOT NULL, payload_json TEXT NOT NULL,
            created_at INTEGER NOT NULL, invalidated_at INTEGER,
            PRIMARY KEY (tenant_id, acl, kind, input_id)
        );
        CREATE TABLE IF NOT EXISTS decision_source_refs (
            tenant_id TEXT NOT NULL, acl TEXT NOT NULL,
            snapshot_id TEXT NOT NULL, source_version TEXT NOT NULL,
            PRIMARY KEY (tenant_id, acl, snapshot_id, source_version)
        );
        CREATE INDEX IF NOT EXISTS decision_snapshot_source_version_idx
            ON decision_source_refs (tenant_id, acl, source_version, snapshot_id);
        CREATE TABLE IF NOT EXISTS revoked_decision_sources (
            tenant_id TEXT NOT NULL, acl TEXT NOT NULL,
            source_version TEXT NOT NULL, revoked_at INTEGER NOT NULL,
            PRIMARY KEY (tenant_id, acl, source_version)
        );
        CREATE TABLE IF NOT EXISTS decision_causal_refs (
            tenant_id TEXT NOT NULL, acl TEXT NOT NULL,
            snapshot_id TEXT NOT NULL, artifact_id TEXT NOT NULL,
            source_version TEXT NOT NULL,
            PRIMARY KEY (tenant_id, acl, snapshot_id, artifact_id)
        );
        CREATE TABLE IF NOT EXISTS decision_operator_pilot_imports (
            tenant_id TEXT NOT NULL, acl TEXT NOT NULL,
            snapshot_id TEXT NOT NULL, request_sha256 TEXT NOT NULL,
            source_sha256 TEXT NOT NULL, model_version TEXT NOT NULL,
            baseline_scenario_id TEXT NOT NULL, alternative_scenario_id TEXT NOT NULL,
            completed_at INTEGER,
            PRIMARY KEY (tenant_id, acl, snapshot_id)
        );
        CREATE TABLE IF NOT EXISTS decision_derived_source_refs (
            tenant_id TEXT NOT NULL, acl TEXT NOT NULL, kind TEXT NOT NULL,
            input_id TEXT NOT NULL, source_version TEXT NOT NULL,
            PRIMARY KEY (tenant_id, acl, kind, input_id, source_version)
        );
        CREATE TABLE IF NOT EXISTS decision_ticket_source_blobs (
            tenant_id TEXT NOT NULL, acl TEXT NOT NULL,
            source_sha256 TEXT NOT NULL, source_bytes BLOB NOT NULL,
            retention_until INTEGER NOT NULL,
            invalidated_at INTEGER,
            PRIMARY KEY (tenant_id, acl, source_sha256)
        );
        CREATE TABLE IF NOT EXISTS decision_shadow_targets (
            tenant_id TEXT NOT NULL, acl TEXT NOT NULL,
            source_lineage TEXT NOT NULL, target_day_utc TEXT NOT NULL,
            forecast_id TEXT NOT NULL,
            PRIMARY KEY (tenant_id, acl, source_lineage, target_day_utc)
        );
        CREATE TABLE IF NOT EXISTS decision_shadow_policy_windows (
            tenant_id TEXT NOT NULL, acl TEXT NOT NULL,
            source_lineage TEXT NOT NULL, effective_from INTEGER NOT NULL,
            effective_until INTEGER NOT NULL, policy_id TEXT NOT NULL,
            PRIMARY KEY (tenant_id, acl, source_lineage, effective_from),
            UNIQUE (tenant_id, acl, policy_id)
        );
        CREATE TABLE IF NOT EXISTS decision_shadow_policy_supersessions (
            tenant_id TEXT NOT NULL, acl TEXT NOT NULL,
            old_policy_id TEXT NOT NULL, new_policy_id TEXT NOT NULL,
            cutoff INTEGER NOT NULL, payload_sha256 TEXT NOT NULL,
            payload_json TEXT NOT NULL,
            PRIMARY KEY (tenant_id, acl, old_policy_id),
            UNIQUE (tenant_id, acl, new_policy_id)
        );
        CREATE TABLE IF NOT EXISTS decision_shadow_scores (
            tenant_id TEXT NOT NULL, acl TEXT NOT NULL,
            forecast_id TEXT NOT NULL, score_id TEXT NOT NULL,
            PRIMARY KEY (tenant_id, acl, forecast_id)
        );
        CREATE TABLE IF NOT EXISTS decision_shadow_sla_forecasts (
            tenant_id TEXT NOT NULL, acl TEXT NOT NULL,
            forecast_id TEXT NOT NULL, sla_id TEXT NOT NULL,
            PRIMARY KEY (tenant_id, acl, forecast_id),
            UNIQUE (tenant_id, acl, sla_id)
        );
        CREATE TABLE IF NOT EXISTS decision_shadow_sla_scores (
            tenant_id TEXT NOT NULL, acl TEXT NOT NULL,
            sla_forecast_id TEXT NOT NULL, score_id TEXT NOT NULL,
            PRIMARY KEY (tenant_id, acl, sla_forecast_id),
            UNIQUE (tenant_id, acl, score_id)
        );
        CREATE TABLE IF NOT EXISTS decision_shadow_sla_score_correction_links (
            tenant_id TEXT NOT NULL, acl TEXT NOT NULL,
            sla_forecast_id TEXT NOT NULL, correction_id TEXT NOT NULL,
            previous_revision_id TEXT NOT NULL, previous_revision_sha256 TEXT NOT NULL,
            PRIMARY KEY (tenant_id, acl, correction_id)
        );
        CREATE TABLE IF NOT EXISTS decision_shadow_sla_score_heads (
            tenant_id TEXT NOT NULL, acl TEXT NOT NULL,
            sla_forecast_id TEXT NOT NULL, correction_id TEXT NOT NULL,
            PRIMARY KEY (tenant_id, acl, sla_forecast_id)
        );
        CREATE TABLE IF NOT EXISTS decision_shadow_sla_score_revision_audit (
            tenant_id TEXT NOT NULL, acl TEXT NOT NULL,
            kind TEXT NOT NULL, revision_id TEXT NOT NULL,
            sla_forecast_id TEXT NOT NULL, recorded_at INTEGER NOT NULL,
            payload_sha256 TEXT NOT NULL,
            PRIMARY KEY (tenant_id, acl, kind, revision_id)
        );
        CREATE TABLE IF NOT EXISTS decision_shadow_score_correction_links (
            tenant_id TEXT NOT NULL, acl TEXT NOT NULL,
            forecast_id TEXT NOT NULL, correction_id TEXT NOT NULL,
            previous_revision_id TEXT NOT NULL, previous_revision_sha256 TEXT NOT NULL,
            PRIMARY KEY (tenant_id, acl, correction_id)
        );
        CREATE TABLE IF NOT EXISTS decision_shadow_score_heads (
            tenant_id TEXT NOT NULL, acl TEXT NOT NULL,
            forecast_id TEXT NOT NULL, correction_id TEXT NOT NULL,
            PRIMARY KEY (tenant_id, acl, forecast_id)
        );
        CREATE TABLE IF NOT EXISTS decision_shadow_score_revision_audit (
            tenant_id TEXT NOT NULL, acl TEXT NOT NULL,
            kind TEXT NOT NULL, revision_id TEXT NOT NULL,
            forecast_id TEXT NOT NULL, recorded_at INTEGER NOT NULL,
            payload_sha256 TEXT NOT NULL,
            PRIMARY KEY (tenant_id, acl, kind, revision_id)
        );
        CREATE UNIQUE INDEX IF NOT EXISTS decision_shadow_forecast_id_idx
            ON decision_shadow_targets (tenant_id, acl, forecast_id);
        CREATE UNIQUE INDEX IF NOT EXISTS decision_shadow_score_id_idx
            ON decision_shadow_scores (tenant_id, acl, score_id);
        CREATE INDEX IF NOT EXISTS decision_derived_source_version_idx
            ON decision_derived_source_refs (tenant_id, acl, kind, source_version, input_id);",
        )?;
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let has_invalidated = conn
            .prepare("PRAGMA table_info(decision_inputs)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<Vec<_>, _>>()?
            .iter()
            .any(|column| column == "invalidated_at");
        if !has_invalidated {
            conn.execute(
                "ALTER TABLE decision_inputs ADD COLUMN invalidated_at INTEGER",
                [],
            )?;
        }
        let ticket_blob_columns = conn
            .prepare("PRAGMA table_info(decision_ticket_source_blobs)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<Vec<_>, _>>()?;
        if !ticket_blob_columns
            .iter()
            .any(|column| column == "retention_until")
        {
            // Blobs written by the short-lived pre-retention implementation
            // expire immediately rather than becoming unbounded legacy data.
            conn.execute(
                "ALTER TABLE decision_ticket_source_blobs
                 ADD COLUMN retention_until INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
        let ref_schema_version: i64 =
            conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if ref_schema_version < 2 {
            // The original source-ref table did not include input kind. Rebuild
            // from active, digest-checked inputs so equal IDs cannot cross-link
            // a snapshot and a parameter fit. Revoked rows stay tombstoned in
            // decision_inputs and revoked_decision_sources.
            let mut statement = conn.prepare(
                "SELECT tenant_id,acl,kind,input_id,payload_sha256,payload_json
                 FROM decision_inputs WHERE kind IN ('snapshot','parameter_fit')
                 AND invalidated_at IS NULL",
            )?;
            let rows = statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            drop(statement);
            let mut refs = Vec::new();
            for (tenant, acl, kind, id, digest, payload) in rows {
                if format!("{:x}", Sha256::digest(payload.as_bytes())) != digest {
                    return Err(DecisionStoreError::Corrupt);
                }
                let (record_id, sources) = if kind == "snapshot" {
                    let snapshot: DecisionSnapshot =
                        serde_json::from_str(&payload).map_err(|_| DecisionStoreError::Corrupt)?;
                    (snapshot.id, snapshot.source_version_hashes)
                } else {
                    let fit: StoredEmpiricalParameterFit =
                        serde_json::from_str(&payload).map_err(|_| DecisionStoreError::Corrupt)?;
                    (fit.id, fit.source_version_hashes)
                };
                if record_id != id || sources.is_empty() {
                    return Err(DecisionStoreError::Corrupt);
                }
                for source in sources {
                    if source.trim().is_empty() {
                        return Err(DecisionStoreError::Corrupt);
                    }
                    refs.push((
                        tenant.clone(),
                        acl.clone(),
                        kind.clone(),
                        id.clone(),
                        source,
                    ));
                }
            }
            conn.execute("DELETE FROM decision_source_refs", [])?;
            conn.execute(
                "DELETE FROM decision_derived_source_refs WHERE kind='parameter_fit'",
                [],
            )?;
            for (tenant, acl, kind, id, source) in refs {
                if kind == "snapshot" {
                    conn.execute(
                        "INSERT OR IGNORE INTO decision_source_refs
                         (tenant_id,acl,snapshot_id,source_version) VALUES (?1,?2,?3,?4)",
                        params![tenant, acl, id, source],
                    )?;
                } else {
                    conn.execute(
                        "INSERT OR IGNORE INTO decision_derived_source_refs
                         (tenant_id,acl,kind,input_id,source_version) VALUES (?1,?2,?3,?4,?5)",
                        params![tenant, acl, kind, id, source],
                    )?;
                }
            }
            conn.pragma_update(None, "user_version", 2)?;
        }
        if ref_schema_version < 3 {
            // Early ticket-backed candidate scores carried the ticket digest in
            // their payload but omitted it from the revocation index. Backfill
            // that exact dependency before any subsequent read or cleanup.
            let mut statement = conn.prepare(
                "SELECT tenant_id,acl,input_id,payload_sha256,payload_json
                 FROM decision_inputs
                 WHERE kind='outcome_model_candidate_score' AND invalidated_at IS NULL",
            )?;
            let rows = statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            drop(statement);
            for (tenant, acl, id, digest, payload) in rows {
                if format!("{:x}", Sha256::digest(payload.as_bytes())) != digest {
                    return Err(DecisionStoreError::Corrupt);
                }
                let value: serde_json::Value =
                    serde_json::from_str(&payload).map_err(|_| DecisionStoreError::Corrupt)?;
                if let Some(source) = value.get("ticket_source_sha256") {
                    let source = source.as_str().ok_or(DecisionStoreError::Corrupt)?;
                    if source.len() != 64 || !source.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                        return Err(DecisionStoreError::Corrupt);
                    }
                    conn.execute(
                        "INSERT OR IGNORE INTO decision_derived_source_refs
                         (tenant_id,acl,kind,input_id,source_version)
                         VALUES (?1,?2,'outcome_model_candidate_score',?3,?4)",
                        params![tenant, acl, id, source],
                    )?;
                }
            }
            let invalidated_at = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|time| time.as_secs() as i64)
                .unwrap_or(0);
            conn.execute(
                "UPDATE decision_inputs SET payload_json='', invalidated_at=?1
                 WHERE kind='outcome_model_candidate_score' AND invalidated_at IS NULL
                 AND EXISTS (SELECT 1 FROM decision_derived_source_refs r
                    JOIN revoked_decision_sources revoked
                      ON revoked.tenant_id=r.tenant_id AND revoked.acl=r.acl
                     AND revoked.source_version=r.source_version
                    WHERE r.tenant_id=decision_inputs.tenant_id
                      AND r.acl=decision_inputs.acl
                      AND r.kind=decision_inputs.kind
                      AND r.input_id=decision_inputs.input_id)",
                params![invalidated_at],
            )?;
            conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        conn.execute_batch("COMMIT")?;
        Ok(conn)
    }

}
