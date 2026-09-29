use super::*;

impl DecisionStore {
    pub fn put_snapshot(
        &self,
        scope: &DecisionScope,
        input: &DecisionSnapshot,
    ) -> Result<String, DecisionStoreError> {
        if input.data_cutoff_utc.trim().is_empty()
            || input.source_version_hashes.is_empty()
            || input
                .queue_id
                .as_deref()
                .is_some_and(|id| id.is_empty() || id.trim() != id || id.len() > 128)
            || chrono::DateTime::parse_from_rfc3339(&input.data_cutoff_utc)
                .map(|time| time.offset().local_minus_utc() != 0)
                .unwrap_or(true)
        {
            return Err(DecisionStoreError::Invalid);
        }
        if input
            .source_version_hashes
            .iter()
            .any(|source| source.trim().is_empty())
        {
            return Err(DecisionStoreError::Invalid);
        }
        self.put(
            scope,
            "snapshot",
            &input.id,
            input,
            Some(&input.source_version_hashes),
        )
    }

    /// Bind a snapshot to an artifact whose exact content digest is listed in
    /// its source versions. Tenant and ACL must match in both stores.
    pub fn bind_causal_artifact(
        &self,
        scope: &DecisionScope,
        snapshot_id: &str,
        artifact_id: &str,
    ) -> Result<(), DecisionStoreError> {
        if !scope.valid() || snapshot_id.trim().is_empty() || artifact_id.trim().is_empty() {
            return Err(DecisionStoreError::Invalid);
        }
        let causal = self
            .causal_store
            .as_ref()
            .ok_or(DecisionStoreError::CausalStoreRequired)?;
        if !causal.path().exists() {
            return Err(DecisionStoreError::CausalStoreRequired);
        }
        let evidence_scope = EvidenceScope {
            tenant_id: scope.tenant_id.clone(),
            acl: scope.acl.clone(),
        };
        let content = causal.source_text(&evidence_scope, artifact_id)?;
        let source_version = format!("{:x}", Sha256::digest(content.as_bytes()));
        let snapshot: DecisionSnapshot = self.get(scope, "snapshot", snapshot_id)?;
        if !snapshot.source_version_hashes.contains(&source_version) {
            return Err(DecisionStoreError::Invalid);
        }
        let conn = self.open()?;
        conn.execute(
            "INSERT OR IGNORE INTO decision_causal_refs
             (tenant_id,acl,snapshot_id,artifact_id,source_version) VALUES (?1,?2,?3,?4,?5)",
            params![
                scope.tenant_id,
                scope.acl,
                snapshot_id,
                artifact_id,
                source_version
            ],
        )?;
        Ok(())
    }

    pub(super) fn verify_causal_refs(
        &self,
        conn: &Connection,
        scope: &DecisionScope,
        snapshot_id: &str,
    ) -> Result<(), DecisionStoreError> {
        let refs = conn
            .prepare(
                "SELECT artifact_id,source_version FROM decision_causal_refs
             WHERE tenant_id=?1 AND acl=?2 AND snapshot_id=?3",
            )?
            .query_map(params![scope.tenant_id, scope.acl, snapshot_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        if refs.is_empty() {
            return Ok(());
        }
        let causal = self
            .causal_store
            .as_ref()
            .ok_or(DecisionStoreError::CausalStoreRequired)?;
        if !causal.path().exists() {
            return Err(DecisionStoreError::CausalStoreRequired);
        }
        let evidence_scope = EvidenceScope {
            tenant_id: scope.tenant_id.clone(),
            acl: scope.acl.clone(),
        };
        for (artifact_id, source_version) in refs {
            match causal.source_text(&evidence_scope, &artifact_id) {
                Ok(content)
                    if format!("{:x}", Sha256::digest(content.as_bytes())) == source_version => {}
                Err(CausalStoreError::NotFound)
                    if !causal.source_record_exists(&evidence_scope, &artifact_id)? =>
                {
                    return Err(DecisionStoreError::Causal(CausalStoreError::NotFound));
                }
                Ok(_) | Err(CausalStoreError::NotFound | CausalStoreError::InvalidInput) => {
                    self.revoke_source_version(scope, &source_version)?;
                    return Err(DecisionStoreError::Revoked);
                }
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    pub(crate) fn active_source_links(
        &self,
        scope: &DecisionScope,
        snapshot: &DecisionSnapshot,
    ) -> Result<Vec<DecisionSourceLink>, DecisionStoreError> {
        if !scope.valid() || snapshot.id.trim().is_empty() {
            return Err(DecisionStoreError::Invalid);
        }
        let conn = self.open()?;
        self.verify_causal_refs(&conn, scope, &snapshot.id)?;
        let refs = conn
            .prepare(
                "SELECT artifact_id,source_version FROM decision_causal_refs
                 WHERE tenant_id=?1 AND acl=?2 AND snapshot_id=?3
                 ORDER BY artifact_id,source_version",
            )?
            .query_map(params![scope.tenant_id, scope.acl, snapshot.id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut links = Vec::with_capacity(refs.len());
        for (artifact_id, source_version_sha256) in refs {
            if !snapshot
                .source_version_hashes
                .contains(&source_version_sha256)
            {
                return Err(DecisionStoreError::Corrupt);
            }
            links.push(DecisionSourceLink {
                tenant_id: scope.tenant_id.clone(),
                acl: scope.acl.clone(),
                artifact_id,
                source_version_sha256,
            });
        }
        Ok(links)
    }

    pub fn put_model(
        &self,
        scope: &DecisionScope,
        input: &QueueModel,
    ) -> Result<String, DecisionStoreError> {
        self.put(scope, "model", &input.version, input, None)
    }

    pub fn put_scenario(
        &self,
        scope: &DecisionScope,
        input: &StaffingScenario,
    ) -> Result<String, DecisionStoreError> {
        self.put(scope, "scenario", &input.id, input, None)
    }

    /// Propagate an upstream source revocation to every snapshot containing
    /// that version. Payloads are scrubbed while digests and reference IDs
    /// remain for audit; replay then fails closed.
    pub fn revoke_source_version(
        &self,
        scope: &DecisionScope,
        source_version: &str,
    ) -> Result<usize, DecisionStoreError> {
        if !scope.valid() || source_version.trim().is_empty() {
            return Err(DecisionStoreError::Invalid);
        }
        let mut conn = self.open()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let revoked_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|time| time.as_secs() as i64)
            .unwrap_or(0);
        let count = Self::revoke_source_version_in_tx(&tx, scope, source_version, revoked_at)?;
        tx.commit()?;
        Ok(count)
    }

    pub(super) fn revoke_source_version_in_tx(
        tx: &rusqlite::Transaction<'_>,
        scope: &DecisionScope,
        source_version: &str,
        revoked_at: i64,
    ) -> Result<usize, DecisionStoreError> {
        tx.execute(
            "INSERT OR IGNORE INTO revoked_decision_sources
             (tenant_id,acl,source_version,revoked_at) VALUES (?1,?2,?3,?4)",
            params![scope.tenant_id, scope.acl, source_version, revoked_at],
        )?;
        let count = tx.execute(
            "UPDATE decision_inputs SET payload_json='', invalidated_at=?1
             WHERE tenant_id=?2 AND acl=?3 AND kind='snapshot' AND invalidated_at IS NULL
             AND input_id IN (SELECT snapshot_id FROM decision_source_refs
                WHERE tenant_id=?2 AND acl=?3 AND source_version=?4)",
            params![revoked_at, scope.tenant_id, scope.acl, source_version],
        )?;
        tx.execute(
            "UPDATE decision_inputs SET payload_json='', invalidated_at=?1
             WHERE tenant_id=?2 AND acl=?3 AND invalidated_at IS NULL
             AND EXISTS (SELECT 1 FROM decision_derived_source_refs r
                WHERE r.tenant_id=?2 AND r.acl=?3 AND r.kind=decision_inputs.kind
                AND r.input_id=decision_inputs.input_id AND r.source_version=?4)",
            params![revoked_at, scope.tenant_id, scope.acl, source_version],
        )?;
        tx.execute(
            "UPDATE decision_ticket_source_blobs SET source_bytes=x'', invalidated_at=?1
             WHERE tenant_id=?2 AND acl=?3 AND source_sha256=?4 AND invalidated_at IS NULL",
            params![revoked_at, scope.tenant_id, scope.acl, source_version],
        )?;
        Ok(count)
    }

    /// Invalidate a scoped causal source and scrub every bound decision
    /// snapshot. Retry is safe if the causal transaction committed before the
    /// decision database could be updated.
    pub(crate) fn invalidate_causal_artifact(
        &self,
        scope: &DecisionScope,
        artifact_id: &str,
    ) -> Result<CausalInvalidationResult, DecisionStoreError> {
        if !scope.valid() || artifact_id.trim().is_empty() {
            return Err(DecisionStoreError::Invalid);
        }
        let causal = self
            .causal_store
            .as_ref()
            .ok_or(DecisionStoreError::CausalStoreRequired)?;
        if !self.path.is_file() || !causal.path().is_file() {
            return Err(DecisionStoreError::NotFound);
        }
        let evidence_scope = EvidenceScope {
            tenant_id: scope.tenant_id.clone(),
            acl: scope.acl.clone(),
        };
        if !causal.source_record_exists(&evidence_scope, artifact_id)? {
            return Err(DecisionStoreError::Causal(CausalStoreError::NotFound));
        }
        let conn = self.open()?;
        let source_versions = conn
            .prepare(
                "SELECT DISTINCT source_version FROM decision_causal_refs
                 WHERE tenant_id=?1 AND acl=?2 AND artifact_id=?3",
            )?
            .query_map(params![scope.tenant_id, scope.acl, artifact_id], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let demoted_claims = match causal.invalidate_artifact(&evidence_scope, artifact_id) {
            Ok(count) => count,
            Err(CausalStoreError::NotFound) => 0,
            Err(error) => return Err(error.into()),
        };
        let mut scrubbed_snapshots = 0;
        for source_version in source_versions {
            scrubbed_snapshots += self.revoke_source_version(scope, &source_version)?;
        }
        Ok(CausalInvalidationResult {
            demoted_claims,
            scrubbed_snapshots,
        })
    }

    /// Admin source removal across the causal, CCR, and decision databases.
    /// CCR is tombstoned first so its original cannot be read after the
    /// causal transaction commits. If a later store fails, retrying this
    /// operation completes the remaining steps using the retained causal
    /// record and immutable source version. Only trusted causal bindings are
    /// targeted; unbound generic tool results cannot be attributed here.
    pub fn remove_causal_artifact_with_dependents(
        &self,
        scope: &DecisionScope,
        artifact_id: &str,
        removal: CausalSourceRemoval,
        ccr_db: Option<&Path>,
    ) -> Result<CausalSourceRemovalResult, DecisionStoreError> {
        // The low-level causal invalidator is private to this crate; every
        // exposed removal must have a CCR tombstone destination before it
        // changes the source. An absent database is created by CcrStore.
        let ccr_db = ccr_db.ok_or(DecisionStoreError::Invalid)?;
        if !scope.valid() || artifact_id.trim().is_empty() {
            return Err(DecisionStoreError::Invalid);
        }
        let causal = self
            .causal_store
            .as_ref()
            .ok_or(DecisionStoreError::CausalStoreRequired)?;
        if !self.path.is_file() || !causal.path().is_file() {
            return Err(DecisionStoreError::NotFound);
        }
        let evidence_scope = EvidenceScope {
            tenant_id: scope.tenant_id.clone(),
            acl: scope.acl.clone(),
        };
        // This includes already invalidated or erased records, so retries
        // cannot lose the exact version needed to revoke CCR originals.
        // Fence new causal CCR deliveries before the cross-database tombstone.
        // A copied chunk with a live delivery lease keeps the later source
        // mutation pending; retry is safe and the fence stays durable.
        let source_version = causal.begin_ccr_revocation(&evidence_scope, artifact_id)?;
        // CCR refuses bindings whose artifact version exceeds 512 bytes, so
        // no trusted handle can exist for such a source.
        let scrubbed_ccr_originals = if source_version.len() > 512 {
            0
        } else {
            duduclaw_llm::CcrStore::new(ccr_db).revoke_artifact_version(
                &scope.tenant_id,
                "causal",
                artifact_id,
                &source_version,
            )?
        };
        let erased_demotions = if removal == CausalSourceRemoval::Erase {
            match causal.erase_artifact(&evidence_scope, artifact_id) {
                Ok(count) => count,
                Err(CausalStoreError::NotFound) => 0,
                Err(error) => return Err(error.into()),
            }
        } else {
            0
        };
        let invalidation = self.invalidate_causal_artifact(scope, artifact_id)?;
        Ok(CausalSourceRemovalResult {
            demoted_claims: erased_demotions + invalidation.demoted_claims,
            scrubbed_snapshots: invalidation.scrubbed_snapshots,
            scrubbed_ccr_originals: Some(scrubbed_ccr_originals),
        })
    }

    pub(super) fn shadow_artifact(
        &self,
        scope: &DecisionScope,
        artifact_id: &str,
        kind: &str,
        expected_sha256: Option<&str>,
    ) -> Result<(SourceArtifact, String), DecisionStoreError> {
        let causal = self
            .causal_store
            .as_ref()
            .ok_or(DecisionStoreError::CausalStoreRequired)?;
        if !causal.path().exists() {
            return Err(DecisionStoreError::CausalStoreRequired);
        }
        let evidence_scope = EvidenceScope {
            tenant_id: scope.tenant_id.clone(),
            acl: scope.acl.clone(),
        };
        let result = causal
            .read_artifact_metadata(&evidence_scope, artifact_id)
            .and_then(|metadata| {
                causal
                    .source_text(&evidence_scope, artifact_id)
                    .map(|content| (metadata, content))
            });
        let (metadata, content) = match result {
            Ok(value) => value,
            Err(CausalStoreError::NotFound | CausalStoreError::InvalidInput)
                if expected_sha256.is_some() =>
            {
                self.revoke_source_version(scope, expected_sha256.expect("checked"))?;
                return Err(DecisionStoreError::Revoked);
            }
            Err(error) => return Err(error.into()),
        };
        if metadata.kind != kind
            || expected_sha256.is_some_and(|digest| digest != metadata.content_sha256)
            || format!("{:x}", Sha256::digest(content.as_bytes())) != metadata.content_sha256
        {
            if let Some(digest) = expected_sha256 {
                self.revoke_source_version(scope, digest)?;
                return Err(DecisionStoreError::Revoked);
            }
            return Err(DecisionStoreError::Invalid);
        }
        Ok((metadata, content))
    }

}
