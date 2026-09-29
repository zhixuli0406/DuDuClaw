use super::*;

impl DecisionStore {
    pub(crate) fn put<T: Serialize>(
        &self,
        scope: &DecisionScope,
        kind: &str,
        id: &str,
        input: &T,
        source_refs: Option<&[String]>,
    ) -> Result<String, DecisionStoreError> {
        let mut conn = self.open()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let digest = Self::put_in_tx(&tx, scope, kind, id, input, source_refs)?;
        tx.commit()?;
        Ok(digest)
    }

    pub(super) fn put_in_tx<T: Serialize>(
        tx: &rusqlite::Transaction<'_>,
        scope: &DecisionScope,
        kind: &str,
        id: &str,
        input: &T,
        source_refs: Option<&[String]>,
    ) -> Result<String, DecisionStoreError> {
        if !scope.valid() || id.trim().is_empty() {
            return Err(DecisionStoreError::Invalid);
        }
        let payload = serde_json::to_string(input)?;
        if payload.len() > MAX_PAYLOAD_BYTES {
            return Err(DecisionStoreError::TooLarge);
        }
        let digest = format!("{:x}", Sha256::digest(payload.as_bytes()));
        if let Some(refs) = source_refs {
            for source in refs {
                let revoked: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM revoked_decision_sources
                     WHERE tenant_id=?1 AND acl=?2 AND source_version=?3)",
                    params![scope.tenant_id, scope.acl, source],
                    |row| row.get(0),
                )?;
                if revoked {
                    return Err(DecisionStoreError::Revoked);
                }
            }
        }
        let existing: Option<(String, String, Option<i64>)> = tx.query_row(
            "SELECT payload_sha256, payload_json, invalidated_at FROM decision_inputs WHERE tenant_id=?1 AND acl=?2 AND kind=?3 AND input_id=?4",
            params![scope.tenant_id, scope.acl, kind, id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).optional()?;
        if let Some((old_digest, old_payload, invalidated_at)) = existing {
            if invalidated_at.is_some() {
                return Err(DecisionStoreError::Revoked);
            }
            if format!("{:x}", Sha256::digest(old_payload.as_bytes())) != old_digest {
                return Err(DecisionStoreError::Corrupt);
            }
            if old_digest != digest {
                return Err(DecisionStoreError::VersionConflict);
            }
        } else {
            let created_at = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|time| time.as_secs() as i64)
                .unwrap_or(0);
            tx.execute("INSERT INTO decision_inputs
                (tenant_id, acl, kind, input_id, schema_version, payload_sha256, payload_json, created_at)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![scope.tenant_id, scope.acl, kind, id, STORE_SCHEMA_VERSION, digest, payload, created_at])?;
        }
        if let Some(refs) = source_refs {
            for source in refs {
                if kind != "snapshot" {
                    tx.execute(
                        "INSERT OR IGNORE INTO decision_derived_source_refs
                        (tenant_id,acl,kind,input_id,source_version) VALUES (?1,?2,?3,?4,?5)",
                        params![scope.tenant_id, scope.acl, kind, id, source],
                    )?;
                } else {
                    tx.execute(
                        "INSERT OR IGNORE INTO decision_source_refs
                        (tenant_id,acl,snapshot_id,source_version) VALUES (?1,?2,?3,?4)",
                        params![scope.tenant_id, scope.acl, id, source],
                    )?;
                }
            }
        }
        Ok(digest)
    }

    pub(crate) fn get<T: DeserializeOwned>(
        &self,
        scope: &DecisionScope,
        kind: &str,
        id: &str,
    ) -> Result<T, DecisionStoreError> {
        self.get_with_digest(scope, kind, id)
            .map(|(value, _)| value)
    }

    pub(crate) fn get_with_digest<T: DeserializeOwned>(
        &self,
        scope: &DecisionScope,
        kind: &str,
        id: &str,
    ) -> Result<(T, String), DecisionStoreError> {
        if !scope.valid() || id.trim().is_empty() {
            return Err(DecisionStoreError::Invalid);
        }
        let conn = self.open()?;
        if kind == "snapshot" && !self.allow_pending_operator_import {
            // A cross-database import can stop after any of its idempotent
            // writes. The snapshot becomes readable only after a complete,
            // digest-checked receipt is present in this exact scope.
            let intent: Option<(Option<i64>, String, String, String, String)> = conn.query_row(
                "SELECT completed_at,source_sha256,model_version,baseline_scenario_id,alternative_scenario_id
                 FROM decision_operator_pilot_imports WHERE tenant_id=?1 AND acl=?2 AND snapshot_id=?3",
                params![scope.tenant_id, scope.acl, id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            ).optional()?;
            if let Some((completed, source, model, baseline, alternative)) = intent {
                if completed.is_none() {
                    return Err(DecisionStoreError::NotFound);
                }
                let receipt: Option<(i64, String, String, Option<i64>)> = conn
                    .query_row(
                        "SELECT schema_version,payload_sha256,payload_json,invalidated_at
                     FROM decision_inputs WHERE tenant_id=?1 AND acl=?2
                       AND kind='uploaded_pilot_receipt' AND input_id=?3",
                        params![scope.tenant_id, scope.acl, id],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                    )
                    .optional()?;
                let Some((version, receipt_digest, payload, invalidated)) = receipt else {
                    return Err(DecisionStoreError::NotFound);
                };
                if version != STORE_SCHEMA_VERSION
                    || invalidated.is_some()
                    || format!("{:x}", Sha256::digest(payload.as_bytes())) != receipt_digest
                {
                    return Err(DecisionStoreError::NotFound);
                }
                let receipt: crate::decision_operator_import::OperatorPilotImportReceipt =
                    serde_json::from_str(&payload).map_err(|_| DecisionStoreError::NotFound)?;
                if receipt.snapshot_id != id
                    || receipt.source_sha256 != source
                    || receipt.model_version != model
                    || receipt.baseline_scenario_id != baseline
                    || receipt.alternative_scenario_id != alternative
                {
                    return Err(DecisionStoreError::NotFound);
                }
            }
        }
        let record: Option<(i64, String, String, Option<i64>)> = conn
            .query_row(
                "SELECT schema_version, payload_sha256, payload_json, invalidated_at FROM decision_inputs
             WHERE tenant_id=?1 AND acl=?2 AND kind=?3 AND input_id=?4",
                params![scope.tenant_id, scope.acl, kind, id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        let (version, digest, payload, invalidated_at) =
            record.ok_or(DecisionStoreError::NotFound)?;
        if invalidated_at.is_some() {
            return Err(DecisionStoreError::Revoked);
        }
        if kind == "snapshot" {
            self.verify_causal_refs(&conn, scope, id)?;
        }
        if version != STORE_SCHEMA_VERSION
            || format!("{:x}", Sha256::digest(payload.as_bytes())) != digest
        {
            return Err(DecisionStoreError::Corrupt);
        }
        Ok((serde_json::from_str(&payload)?, digest))
    }

    pub(super) fn put_ticket_source_blob_in_tx(
        tx: &rusqlite::Transaction<'_>,
        scope: &DecisionScope,
        digest: &str,
        source_bytes: &[u8],
        retention_until: i64,
    ) -> Result<(), DecisionStoreError> {
        if !scope.valid()
            || source_bytes.is_empty()
            || source_bytes.len() > MAX_PAYLOAD_BYTES
            || digest != format!("{:x}", Sha256::digest(source_bytes))
            || retention_until <= chrono::Utc::now().timestamp()
        {
            return Err(DecisionStoreError::Invalid);
        }
        let revoked: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM revoked_decision_sources
             WHERE tenant_id=?1 AND acl=?2 AND source_version=?3)",
            params![scope.tenant_id, scope.acl, digest],
            |row| row.get(0),
        )?;
        if revoked {
            return Err(DecisionStoreError::Revoked);
        }
        let existing: Option<(Vec<u8>, i64, Option<i64>)> = tx.query_row(
            "SELECT source_bytes,retention_until,invalidated_at FROM decision_ticket_source_blobs
             WHERE tenant_id=?1 AND acl=?2 AND source_sha256=?3",
            params![scope.tenant_id, scope.acl, digest],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).optional()?;
        match existing {
            Some((_, _, Some(_))) => return Err(DecisionStoreError::Revoked),
            Some((stored, old_retention, None))
                if stored != source_bytes || old_retention != retention_until =>
            {
                return Err(DecisionStoreError::VersionConflict);
            }
            Some(_) => {}
            None => {
                tx.execute(
                    "INSERT INTO decision_ticket_source_blobs
                     (tenant_id,acl,source_sha256,source_bytes,retention_until)
                     VALUES (?1,?2,?3,?4,?5)",
                    params![
                        scope.tenant_id,
                        scope.acl,
                        digest,
                        source_bytes,
                        retention_until
                    ],
                )?;
            }
        }
        Ok(())
    }

    pub(super) fn load_ticket_source_blob(
        &self,
        scope: &DecisionScope,
        digest: &str,
    ) -> Result<(Vec<u8>, i64), DecisionStoreError> {
        if !scope.valid()
            || digest.len() != 64
            || !digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(DecisionStoreError::Invalid);
        }
        let row: Option<(Vec<u8>, i64, Option<i64>)> = self.open()?.query_row(
            "SELECT source_bytes,retention_until,invalidated_at FROM decision_ticket_source_blobs
             WHERE tenant_id=?1 AND acl=?2 AND source_sha256=?3",
            params![scope.tenant_id, scope.acl, digest],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).optional()?;
        let (bytes, retention_until, invalidated_at) = row.ok_or(DecisionStoreError::Corrupt)?;
        if invalidated_at.is_some() {
            return Err(DecisionStoreError::Revoked);
        }
        if retention_until <= chrono::Utc::now().timestamp() {
            self.revoke_source_version(scope, digest)?;
            return Err(DecisionStoreError::Revoked);
        }
        if bytes.is_empty()
            || bytes.len() > MAX_PAYLOAD_BYTES
            || format!("{:x}", Sha256::digest(&bytes)) != digest
        {
            return Err(DecisionStoreError::Corrupt);
        }
        Ok((bytes, retention_until))
    }

}
