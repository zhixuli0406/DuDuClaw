use super::*;

impl DecisionStore {
    /// Scrub expired ticket source bytes and invalidate every derived record.
    pub fn scrub_expired_ticket_sources(
        &self,
        scope: &DecisionScope,
    ) -> Result<usize, DecisionStoreError> {
        if !scope.valid() {
            return Err(DecisionStoreError::Invalid);
        }
        let now = chrono::Utc::now().timestamp();
        let mut conn = self.open()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut statement = tx.prepare(
            "SELECT source_sha256 FROM decision_ticket_source_blobs
             WHERE tenant_id=?1 AND acl=?2 AND invalidated_at IS NULL
             AND retention_until<=?3 ORDER BY source_sha256",
        )?;
        let digests = statement
            .query_map(params![scope.tenant_id, scope.acl, now], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        let revoked_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|time| time.as_secs() as i64)
            .unwrap_or(0);
        for digest in &digests {
            Self::revoke_source_version_in_tx(&tx, scope, digest, revoked_at)?;
        }
        tx.commit()?;
        Ok(digests.len())
    }

    /// Scopes that still hold un-revoked ticket source bytes. Only the
    /// retention sweeper needs to enumerate scopes; every other read path is
    /// given its exact scope by the caller.
    pub fn ticket_source_scopes(&self) -> Result<Vec<DecisionScope>, DecisionStoreError> {
        let conn = self.open()?;
        let mut statement = conn.prepare(
            "SELECT DISTINCT tenant_id,acl FROM decision_ticket_source_blobs
             WHERE invalidated_at IS NULL ORDER BY tenant_id,acl",
        )?;
        let scopes = statement
            .query_map([], |row| {
                Ok(DecisionScope {
                    tenant_id: row.get(0)?,
                    acl: row.get(1)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(scopes.into_iter().filter(DecisionScope::valid).collect())
    }

    /// Enforce every scope's ticket retention deadline in one pass. A scope
    /// that fails is reported rather than silently skipped, and never stops
    /// the remaining scopes: the deadline is a promise about all of them.
    pub fn sweep_expired_ticket_sources(&self) -> TicketRetentionSweep {
        let scopes = match self.ticket_source_scopes() {
            Ok(scopes) => scopes,
            Err(error) => {
                return TicketRetentionSweep {
                    failures: vec![format!("scope enumeration failed: {error}")],
                    ..TicketRetentionSweep::default()
                };
            }
        };
        let mut report = TicketRetentionSweep {
            scopes: scopes.len(),
            ..TicketRetentionSweep::default()
        };
        for scope in scopes {
            match self.scrub_expired_ticket_sources(&scope) {
                Ok(scrubbed) => report.scrubbed += scrubbed,
                Err(error) => report.failures.push(format!(
                    "{}/{}: {error}",
                    duduclaw_core::truncate_chars(&scope.tenant_id, 64),
                    duduclaw_core::truncate_chars(&scope.acl, 64)
                )),
            }
        }
        report
    }

}
