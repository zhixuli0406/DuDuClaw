use super::*;

impl DecisionStore {
    /// Return a metadata-only view of one exact decision scope. Every active
    /// payload is digest checked before selected identifiers are projected;
    /// raw imports, observations, and ticket bytes never leave the store.
    pub fn dashboard_overview(
        &self,
        scope: &DecisionScope,
        limit: usize,
    ) -> Result<DecisionDashboardOverview, DecisionStoreError> {
        if !scope.valid() || !(1..=200).contains(&limit) {
            return Err(DecisionStoreError::Invalid);
        }
        let mut conn = self.open()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Deferred)?;
        let mut counts = BTreeMap::new();
        let mut count_statement = tx.prepare(
            "SELECT kind,COUNT(*) FROM decision_inputs
             WHERE tenant_id=?1 AND acl=?2 AND invalidated_at IS NULL
             AND NOT EXISTS (
               SELECT 1 FROM decision_operator_pilot_imports p
               WHERE p.tenant_id=decision_inputs.tenant_id AND p.acl=decision_inputs.acl
                 AND decision_inputs.input_id=p.snapshot_id
                 AND decision_inputs.kind IN ('snapshot','uploaded_pilot_receipt')
                 AND (p.completed_at IS NULL OR NOT EXISTS (
                   SELECT 1 FROM decision_inputs r
                   WHERE r.tenant_id=p.tenant_id AND r.acl=p.acl
                     AND r.kind='uploaded_pilot_receipt' AND r.input_id=p.snapshot_id
                     AND r.invalidated_at IS NULL
                 ))
             )
             GROUP BY kind ORDER BY kind",
        )?;
        for row in count_statement.query_map(params![scope.tenant_id, scope.acl], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?))
        })? {
            let (kind, count) = row?;
            counts.insert(kind, count);
        }
        drop(count_statement);
        let invalidated_inputs = tx.query_row(
            "SELECT COUNT(*) FROM decision_inputs
             WHERE tenant_id=?1 AND acl=?2 AND invalidated_at IS NOT NULL",
            params![scope.tenant_id, scope.acl],
            |row| row.get::<_, u64>(0),
        )?;
        let now = chrono::Utc::now();
        let (active, expired, revoked, next_expiry): (u64, u64, u64, Option<i64>) = tx.query_row(
            "SELECT
                   COALESCE(SUM(CASE WHEN invalidated_at IS NULL AND retention_until>?3
                                     THEN 1 ELSE 0 END),0),
                   COALESCE(SUM(CASE WHEN invalidated_at IS NULL AND retention_until<=?3
                                     THEN 1 ELSE 0 END),0),
                   COALESCE(SUM(CASE WHEN invalidated_at IS NOT NULL THEN 1 ELSE 0 END),0),
                   MIN(CASE WHEN invalidated_at IS NULL AND retention_until>?3
                            THEN retention_until END)
                 FROM decision_ticket_source_blobs WHERE tenant_id=?1 AND acl=?2",
            params![scope.tenant_id, scope.acl, now.timestamp()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
        let next_expiry_utc = next_expiry
            .and_then(|timestamp| chrono::DateTime::from_timestamp(timestamp, 0))
            .map(|time| time.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));

        let mut statement = tx.prepare(
            "SELECT kind,input_id,created_at,payload_sha256,payload_json
             FROM decision_inputs
             WHERE tenant_id=?1 AND acl=?2 AND invalidated_at IS NULL
             AND NOT EXISTS (
               SELECT 1 FROM decision_operator_pilot_imports p
               WHERE p.tenant_id=decision_inputs.tenant_id AND p.acl=decision_inputs.acl
                 AND decision_inputs.input_id=p.snapshot_id
                 AND decision_inputs.kind IN ('snapshot','uploaded_pilot_receipt')
                 AND (p.completed_at IS NULL OR NOT EXISTS (
                   SELECT 1 FROM decision_inputs r
                   WHERE r.tenant_id=p.tenant_id AND r.acl=p.acl
                     AND r.kind='uploaded_pilot_receipt' AND r.input_id=p.snapshot_id
                     AND r.invalidated_at IS NULL
                 ))
             )
             ORDER BY created_at DESC,kind,input_id LIMIT ?3",
        )?;
        let rows = statement
            .query_map(params![scope.tenant_id, scope.acl, limit as i64], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        let mut artifacts = Vec::with_capacity(rows.len());
        for (kind, id, created_at_unix, digest, payload) in rows {
            if format!("{:x}", Sha256::digest(payload.as_bytes())) != digest {
                return Err(DecisionStoreError::Corrupt);
            }
            let value: serde_json::Value =
                serde_json::from_str(&payload).map_err(|_| DecisionStoreError::Corrupt)?;
            let string = |field: &str| {
                value
                    .get(field)
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            };
            artifacts.push(DecisionDashboardArtifact {
                kind,
                id,
                created_at_unix,
                status: string("status").or_else(|| string("review_state")),
                candidate_id: string("candidate_id"),
                target_snapshot_id: string("target_snapshot_id").or_else(|| string("snapshot_id")),
                scenario_id: string("scenario_id"),
                outcome_id: string("outcome_id"),
                ticket_backed: value
                    .get("ticket_source_sha256")
                    .is_some_and(|source| source.as_str().is_some_and(|source| !source.is_empty())),
                replay_hash: string("replay_hash"),
            });
        }
        tx.commit()?;
        Ok(DecisionDashboardOverview {
            status: "exploratory".into(),
            generated_at_utc: now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            tenant_id: scope.tenant_id.clone(),
            acl: scope.acl.clone(),
            counts,
            invalidated_inputs,
            ticket_sources: DecisionDashboardTicketSources {
                active, expired, revoked, next_expiry_utc,
            },
            artifacts,
            limitations: vec![
                "Local artifact timestamps and digests do not attest an upstream producer".into(),
                "Synthetic validation does not establish real queue calibration or intervention effects".into(),
                "This inventory does not promote a model or authorize a staffing action".into(),
            ],
        })
    }

}
