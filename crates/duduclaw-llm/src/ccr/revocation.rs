//! Source-call binding, revocation and the retrieval/loop audit writes —
//! everything that decides whether a saved original is still deliverable.
//! Moved verbatim out of `ccr.rs`.

use super::*;

impl CcrStore {
    /// Tombstone and remove every saved call whose trusted route is no longer
    /// allowed in this exact caller scope. One transaction excludes new puts.
    pub(super) fn revoke_unlisted_source_calls(
        &self,
        scope: &CcrScope,
        allowed: &HashSet<String>,
    ) -> Result<usize, CcrError> {
        if !scope.valid() {
            return Err(CcrError::InvalidScope);
        }
        let mut conn = self.open()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let removed_calls = {
            let mut stmt = tx.prepare(
                "SELECT DISTINCT source_tool,source_call_id FROM ccr_entries
                 WHERE tenant_id=?1 AND agent_id=?2 AND session_id=?3 AND source_acl=?4",
            )?;
            stmt.query_map(
                params![
                    scope.tenant_id,
                    scope.agent_id,
                    scope.session_id,
                    scope.source_acl
                ],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )?
            .collect::<Result<Vec<_>, _>>()?
        };
        let mut removed = 0;
        for (source_tool, source_call_id) in removed_calls {
            if allowed.contains(&source_tool) {
                continue;
            }
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
                    unix_now()
                ],
            )?;
            removed += tx.execute(
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
        }
        tx.commit()?;
        Ok(removed)
    }

    pub(super) fn source_tool_for_id(&self, scope: &CcrScope, id: &str) -> Result<Option<String>, CcrError> {
        if !scope.valid() || id.trim().is_empty() {
            return Err(CcrError::InvalidScope);
        }
        let conn = self.open()?;
        Ok(conn
            .query_row(
                "SELECT source_tool FROM ccr_entries WHERE id=?1 AND tenant_id=?2
                 AND agent_id=?3 AND session_id=?4 AND source_acl=?5",
                params![
                    id,
                    scope.tenant_id,
                    scope.agent_id,
                    scope.session_id,
                    scope.source_acl
                ],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub(super) fn source_call_id_for_id(
        &self,
        scope: &CcrScope,
        id: &str,
    ) -> Result<Option<String>, CcrError> {
        if !scope.valid() || id.trim().is_empty() {
            return Err(CcrError::InvalidScope);
        }
        let conn = self.open()?;
        Ok(conn
            .query_row(
                "SELECT source_call_id FROM ccr_entries WHERE id=?1 AND tenant_id=?2
             AND agent_id=?3 AND session_id=?4 AND source_acl=?5",
                params![
                    id,
                    scope.tenant_id,
                    scope.agent_id,
                    scope.session_id,
                    scope.source_acl
                ],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub(super) fn bound_source_for_id(
        &self,
        scope: &CcrScope,
        id: &str,
    ) -> Result<Option<CcrBoundSource>, CcrError> {
        if !scope.valid() || id.trim().is_empty() {
            return Err(CcrError::InvalidScope);
        }
        let conn = self.open()?;
        let row: Option<(i64, Option<String>, Option<String>, Option<String>, Option<String>, Option<String>, String)> = conn
            .query_row(
                "SELECT e.binding_required,b.tenant_id,b.connector,b.artifact_id,b.version,b.acl_revision,e.content_sha256
             FROM ccr_entries e LEFT JOIN ccr_artifact_bindings b ON b.entry_id=e.id
             WHERE e.id=?1 AND e.tenant_id=?2 AND e.agent_id=?3
               AND e.session_id=?4 AND e.source_acl=?5",
                params![
                    id,
                    scope.tenant_id,
                    scope.agent_id,
                    scope.session_id,
                    scope.source_acl
                ],
                |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?)),
            )
            .optional()?;
        match row {
            None | Some((0, None, None, None, None, None, _)) => Ok(None),
            Some((
                required,
                Some(tenant),
                Some(connector),
                Some(artifact_id),
                Some(version),
                Some(acl_revision),
                saved_sha256,
            )) if (required == 0 || required == 1) && tenant == scope.tenant_id => {
                Ok(Some(CcrBoundSource {
                    artifact: CcrSourceArtifact {
                        connector,
                        artifact_id,
                        version,
                        acl_revision,
                    },
                    saved_sha256,
                }))
            }
            _ => Err(CcrError::Revoked),
        }
    }

    /// Check both the caller-scope and exact tool-call tombstones even when
    /// the result is too small to enter the compression store.
    pub fn ensure_source_call_active(
        &self,
        scope: &CcrScope,
        source_tool: &str,
        source_call_id: &str,
    ) -> Result<(), CcrError> {
        if !scope.valid() || source_tool.trim().is_empty() || source_call_id.trim().is_empty() {
            return Err(CcrError::InvalidScope);
        }
        let conn = self.open()?;
        let revoked: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM ccr_revoked_scopes WHERE
                tenant_id=?1 AND agent_id=?2 AND session_id=?3 AND source_acl=?4)
              OR EXISTS(SELECT 1 FROM ccr_revoked_sources WHERE
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
        Ok(())
    }

    /// Verify a dispatched result against any still-retrievable original for
    /// the same exact call, including results too small to be compressed now.
    /// A changed or corrupt result tombstones the call and scrubs its handles.
    pub fn ensure_source_call_content(
        &self,
        scope: &CcrScope,
        source_tool: &str,
        source_call_id: &str,
        original: &str,
    ) -> Result<(), CcrError> {
        if !scope.valid() || source_tool.trim().is_empty() || source_call_id.trim().is_empty() {
            return Err(CcrError::InvalidScope);
        }
        let mut conn = self.open()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let revoked: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM ccr_revoked_scopes WHERE
                tenant_id=?1 AND agent_id=?2 AND session_id=?3 AND source_acl=?4)
              OR EXISTS(SELECT 1 FROM ccr_revoked_sources WHERE
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
        let now = unix_now();
        let inconsistent = {
            let mut stmt = tx.prepare(
                "SELECT original, content_sha256, content_bytes, transform_version
                 FROM ccr_entries WHERE tenant_id=?1 AND agent_id=?2 AND session_id=?3
                 AND source_acl=?4 AND source_tool=?5 AND source_call_id=?6
                 AND expires_at>?7",
            )?;
            let rows = stmt.query_map(
                params![
                    scope.tenant_id,
                    scope.agent_id,
                    scope.session_id,
                    scope.source_acl,
                    source_tool,
                    source_call_id,
                    now
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                },
            )?;
            let digest = format!("{:x}", Sha256::digest(original.as_bytes()));
            rows.collect::<Result<Vec<_>, _>>()?.iter().any(
                |(text, saved_digest, bytes, version)| {
                    *version != CCR_ENTRY_VERSION
                        || *bytes != text.len() as i64
                        || text != original
                        || saved_digest != &digest
                },
            )
        };
        if inconsistent {
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
        tx.commit()?;
        Ok(())
    }

    /// Record a refused retrieval that cannot reach `retrieve`, such as a
    /// missing ID. Only the ID digest and caller scope are retained.
    pub fn record_retrieval_refusal(
        &self,
        scope: &CcrScope,
        requested_id: &str,
    ) -> Result<(), CcrError> {
        if !scope.valid() {
            return Err(CcrError::InvalidScope);
        }
        let mut conn = self.open()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO ccr_retrieval_audit
             (requested_id_sha256, tenant_id, agent_id, session_id, source_acl,
              status, returned_bytes, attempted_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 'refused', 0, ?6)",
            params![
                format!("{:x}", Sha256::digest(requested_id.as_bytes())),
                scope.tenant_id,
                scope.agent_id,
                scope.session_id,
                scope.source_acl,
                unix_now()
            ],
        )?;
        tx.execute(
            "DELETE FROM ccr_retrieval_audit WHERE audit_id IN
             (SELECT audit_id FROM ccr_retrieval_audit ORDER BY audit_id DESC LIMIT -1 OFFSET 10000)",
            [],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Keep completed native-loop counters only. Telemetry has its own lazy
    /// schema so a telemetry-specific failure cannot break original storage
    /// or retrieval. No result text, query, handle, or caller ACL is written.
    pub fn record_loop_telemetry(
        &self,
        scope: &CcrScope,
        telemetry: &crate::tool_loop::ToolLoopTelemetry,
    ) -> Result<(), CcrError> {
        if !scope.valid()
            || telemetry.provider_rounds == 0
            || telemetry.usage_reported_rounds > telemetry.provider_rounds
            || telemetry
                .ccr_find_hits
                .checked_add(telemetry.ccr_find_misses)
                != Some(telemetry.ccr_find_attempts)
            || telemetry
                .ccr_retrieve_successes
                .checked_add(telemetry.ccr_retrieve_misses)
                != Some(telemetry.ccr_retrieve_attempts)
            || telemetry.ccr_delivered_bytes > telemetry.ccr_original_bytes
        {
            return Err(CcrError::InvalidTelemetry);
        }
        let number = |value: u64| i64::try_from(value).map_err(|_| CcrError::InvalidTelemetry);
        let elapsed =
            i64::try_from(telemetry.elapsed_millis).map_err(|_| CcrError::InvalidTelemetry)?;
        let mut conn = self.open_with_busy_timeout(std::time::Duration::from_millis(250))?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS ccr_loop_telemetry (
                event_id INTEGER PRIMARY KEY AUTOINCREMENT,
                tenant_id TEXT NOT NULL,
                observed_at INTEGER NOT NULL,
                provider_rounds INTEGER NOT NULL,
                usage_reported_rounds INTEGER NOT NULL,
                input_tokens INTEGER NOT NULL,
                output_tokens INTEGER NOT NULL,
                cache_read_tokens INTEGER NOT NULL,
                cache_write_tokens INTEGER NOT NULL,
                reasoning_tokens INTEGER NOT NULL,
                ccr_compressed_results INTEGER NOT NULL,
                ccr_original_bytes INTEGER NOT NULL,
                ccr_delivered_bytes INTEGER NOT NULL,
                ccr_find_attempts INTEGER NOT NULL,
                ccr_find_hits INTEGER NOT NULL,
                ccr_find_misses INTEGER NOT NULL,
                ccr_retrieve_attempts INTEGER NOT NULL,
                ccr_retrieve_successes INTEGER NOT NULL,
                ccr_retrieve_misses INTEGER NOT NULL,
                ccr_retrieved_bytes INTEGER NOT NULL,
                elapsed_millis INTEGER NOT NULL,
                -- Declared last so a freshly created table and one upgraded by
                -- the `ALTER TABLE` in `open_with_busy_timeout` (which can only
                -- append) have an identical column order.
                ccr_find_rate_limited INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_ccr_loop_telemetry_tenant
                ON ccr_loop_telemetry(tenant_id, event_id);",
        )?;
        tx.execute(
            "INSERT INTO ccr_loop_telemetry (
                tenant_id, observed_at, provider_rounds, usage_reported_rounds,
                input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
                reasoning_tokens, ccr_compressed_results, ccr_original_bytes,
                ccr_delivered_bytes, ccr_find_attempts, ccr_find_hits,
                ccr_find_misses, ccr_retrieve_attempts, ccr_retrieve_successes,
                ccr_retrieve_misses, ccr_retrieved_bytes, elapsed_millis,
                ccr_find_rate_limited
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21
             )",
            params![
                scope.tenant_id,
                unix_now(),
                number(telemetry.provider_rounds)?,
                number(telemetry.usage_reported_rounds)?,
                number(telemetry.provider_usage.input_tokens)?,
                number(telemetry.provider_usage.output_tokens)?,
                number(telemetry.provider_usage.cache_read_tokens)?,
                number(telemetry.provider_usage.cache_write_tokens)?,
                number(telemetry.provider_usage.reasoning_tokens)?,
                number(telemetry.ccr_compressed_results)?,
                number(telemetry.ccr_original_bytes)?,
                number(telemetry.ccr_delivered_bytes)?,
                number(telemetry.ccr_find_attempts)?,
                number(telemetry.ccr_find_hits)?,
                number(telemetry.ccr_find_misses)?,
                number(telemetry.ccr_retrieve_attempts)?,
                number(telemetry.ccr_retrieve_successes)?,
                number(telemetry.ccr_retrieve_misses)?,
                number(telemetry.ccr_retrieved_bytes)?,
                elapsed,
                number(telemetry.ccr_find_rate_limited)?,
            ],
        )?;
        tx.execute(
            "DELETE FROM ccr_loop_telemetry WHERE event_id IN
             (SELECT event_id FROM ccr_loop_telemetry ORDER BY event_id DESC LIMIT -1 OFFSET ?1)",
            [CCR_LOOP_TELEMETRY_MAX_ROWS],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Invalidate originals from one exact tool call in the caller's scope.
    /// A tombstone prevents the same source call from being stored again.
    pub fn revoke_source_call(
        &self,
        scope: &CcrScope,
        source_tool: &str,
        source_call_id: &str,
    ) -> Result<usize, CcrError> {
        if !scope.valid() || source_tool.trim().is_empty() || source_call_id.trim().is_empty() {
            return Err(CcrError::InvalidScope);
        }
        let mut conn = self.open()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT OR IGNORE INTO ccr_revoked_sources
             (tenant_id, agent_id, session_id, source_acl, source_tool, source_call_id, revoked_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                scope.tenant_id,
                scope.agent_id,
                scope.session_id,
                scope.source_acl,
                source_tool,
                source_call_id,
                unix_now()
            ],
        )?;
        let count = tx.execute(
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
        Ok(count)
    }

    /// Called by a trusted connector when an exact upstream version changes,
    /// loses access, or is deleted. Tombstoning and scrubbing are atomic, so
    /// a concurrent put cannot resurrect the old version.
    pub fn revoke_artifact_version(
        &self,
        tenant_id: &str,
        connector: &str,
        artifact_id: &str,
        version: &str,
    ) -> Result<usize, CcrError> {
        if tenant_id.trim().is_empty()
            || [connector, artifact_id, version]
                .iter()
                .any(|value| value.trim().is_empty() || value.len() > 512)
        {
            return Err(CcrError::InvalidScope);
        }
        let mut conn = self.open()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT OR IGNORE INTO ccr_revoked_artifact_versions
             (tenant_id,connector,artifact_id,version,revoked_at) VALUES (?1,?2,?3,?4,?5)",
            params![tenant_id, connector, artifact_id, version, unix_now()],
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO ccr_revoked_sources
             (tenant_id,agent_id,session_id,source_acl,source_tool,source_call_id,revoked_at)
             SELECT e.tenant_id,e.agent_id,e.session_id,e.source_acl,e.source_tool,
                    e.source_call_id,?5
             FROM ccr_entries e JOIN ccr_artifact_bindings b ON b.entry_id=e.id
             WHERE b.tenant_id=?1 AND b.connector=?2 AND b.artifact_id=?3 AND b.version=?4",
            params![tenant_id, connector, artifact_id, version, unix_now()],
        )?;
        let count = tx.execute(
            "DELETE FROM ccr_entries WHERE id IN
             (SELECT entry_id FROM ccr_artifact_bindings WHERE
              tenant_id=?1 AND connector=?2 AND artifact_id=?3 AND version=?4)",
            params![tenant_id, connector, artifact_id, version],
        )?;
        tx.commit()?;
        Ok(count)
    }

    /// Remove every original in one exact caller scope and prevent future
    /// inserts under that scope. SQLite serializes this with concurrent puts.
    pub fn revoke_scope(&self, scope: &CcrScope) -> Result<usize, CcrError> {
        if !scope.valid() {
            return Err(CcrError::InvalidScope);
        }
        let mut conn = self.open()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT OR IGNORE INTO ccr_revoked_scopes
             (tenant_id,agent_id,session_id,source_acl,revoked_at)
             VALUES (?1,?2,?3,?4,?5)",
            params![
                scope.tenant_id,
                scope.agent_id,
                scope.session_id,
                scope.source_acl,
                unix_now()
            ],
        )?;
        let count = tx.execute(
            "DELETE FROM ccr_entries WHERE tenant_id=?1 AND agent_id=?2
             AND session_id=?3 AND source_acl=?4",
            params![
                scope.tenant_id,
                scope.agent_id,
                scope.session_id,
                scope.source_acl
            ],
        )?;
        tx.commit()?;
        Ok(count)
    }
}
