use super::*;

impl DecisionStore {
    /// Register an immutable pilot policy before its first eligible UTC day.
    /// Overlapping windows for one source lineage are rejected.
    pub fn put_shadow_policy(
        &self,
        scope: &DecisionScope,
        id: &str,
        source_lineage: &str,
        queue_id: &str,
        effective_from_utc: &str,
        effective_until_utc: &str,
        issue_deadline_seconds: u32,
        min_training_days: usize,
        min_saturated_days: usize,
    ) -> Result<ShadowPilotPolicy, DecisionStoreError> {
        self.put_shadow_policy_at(
            scope,
            id,
            source_lineage,
            queue_id,
            effective_from_utc,
            effective_until_utc,
            issue_deadline_seconds,
            min_training_days,
            min_saturated_days,
            chrono::Utc::now().timestamp(),
        )
    }

    pub(super) fn put_shadow_policy_at(
        &self,
        scope: &DecisionScope,
        id: &str,
        source_lineage: &str,
        queue_id: &str,
        effective_from_utc: &str,
        effective_until_utc: &str,
        issue_deadline_seconds: u32,
        min_training_days: usize,
        min_saturated_days: usize,
        registered_at: i64,
    ) -> Result<ShadowPilotPolicy, DecisionStoreError> {
        if !scope.valid()
            || id.trim().is_empty()
            || source_lineage.trim().is_empty()
            || queue_id.is_empty()
            || queue_id.trim() != queue_id
            || queue_id.len() > 128
            || !(1..=3_600).contains(&issue_deadline_seconds)
            || !(7..=366).contains(&min_training_days)
            || min_saturated_days == 0
            || min_saturated_days > min_training_days
        {
            return Err(DecisionStoreError::Invalid);
        }
        // Persist the canonical spelling so later string equality (idempotent
        // re-registration) and SQLite day guards both see one form.
        let canonical_from = shadow_utc_day_key(effective_from_utc)?;
        let canonical_until = shadow_utc_day_key(effective_until_utc)?;
        let effective_from_utc = canonical_from.as_str();
        let effective_until_utc = canonical_until.as_str();
        let start = shadow_utc_midnight(effective_from_utc)?.timestamp();
        let end = shadow_utc_midnight(effective_until_utc)?.timestamp();
        if end <= start || end - start > 366 * 86_400 {
            return Err(DecisionStoreError::Invalid);
        }
        match self.get::<ShadowPilotPolicy>(scope, "shadow_policy", id) {
            Ok(existing) => {
                if existing.source_lineage != source_lineage
                    || existing.queue_id.as_deref() != Some(queue_id)
                    || existing.effective_from_utc != effective_from_utc
                    || existing.effective_until_utc != effective_until_utc
                    || existing.issue_deadline_seconds != issue_deadline_seconds
                    || existing.min_training_days != min_training_days
                    || existing.min_saturated_days != min_saturated_days
                {
                    return Err(DecisionStoreError::VersionConflict);
                }
                return self.load_shadow_policy(scope, id);
            }
            Err(DecisionStoreError::NotFound) => {}
            Err(error) => return Err(error),
        }
        if registered_at >= start {
            return Err(DecisionStoreError::Invalid);
        }
        let policy = ShadowPilotPolicy {
            id: id.into(),
            source_lineage: source_lineage.into(),
            queue_id: Some(queue_id.into()),
            effective_from_utc: effective_from_utc.into(),
            effective_until_utc: effective_until_utc.into(),
            issue_deadline_seconds,
            min_training_days,
            min_saturated_days,
            calibration_engine_sha256: calibration_engine_sha256(),
            registered_at,
        };
        let mut conn = self.open()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let existing: Option<(String, i64, i64)> = tx
            .query_row(
                "SELECT source_lineage,effective_from,effective_until
             FROM decision_shadow_policy_windows
             WHERE tenant_id=?1 AND acl=?2 AND policy_id=?3",
                params![scope.tenant_id, scope.acl, id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        if let Some((lineage, prior_start, prior_end)) = existing {
            if lineage != source_lineage || prior_start != start || prior_end != end {
                return Err(DecisionStoreError::VersionConflict);
            }
        } else {
            let overlap: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM decision_shadow_policy_windows
                 WHERE tenant_id=?1 AND acl=?2 AND source_lineage=?3
                 AND effective_from<?4 AND effective_until>?5)",
                params![scope.tenant_id, scope.acl, source_lineage, end, start],
                |row| row.get(0),
            )?;
            if overlap {
                return Err(DecisionStoreError::VersionConflict);
            }
            tx.execute(
                "INSERT INTO decision_shadow_policy_windows
                 (tenant_id,acl,source_lineage,effective_from,effective_until,policy_id)
                 VALUES (?1,?2,?3,?4,?5,?6)",
                params![scope.tenant_id, scope.acl, source_lineage, start, end, id],
            )?;
        }
        tx.commit()?;
        match self.put(scope, "shadow_policy", id, &policy, None) {
            Ok(_) => {}
            Err(DecisionStoreError::VersionConflict) => {
                let existing = self.load_shadow_policy(scope, id)?;
                if existing.source_lineage == source_lineage
                    && existing.queue_id.as_deref() == Some(queue_id)
                    && existing.effective_from_utc == effective_from_utc
                    && existing.effective_until_utc == effective_until_utc
                    && existing.issue_deadline_seconds == issue_deadline_seconds
                    && existing.min_training_days == min_training_days
                    && existing.min_saturated_days == min_saturated_days
                {
                    return Ok(existing);
                }
                return Err(DecisionStoreError::VersionConflict);
            }
            Err(error) => return Err(error),
        }
        self.load_shadow_policy(scope, id)
    }

    /// Review a prospective handoff while preserving both immutable policy
    /// records. The successor starts at a future UTC midnight inside the old
    /// window; existing forecasts before that cutoff keep their old policy.
    pub fn supersede_shadow_policy(
        &self,
        scope: &DecisionScope,
        old_id: &str,
        new_id: &str,
        cutoff_utc: &str,
        new_until_utc: &str,
        reviewer: &str,
        issue_deadline_seconds: u32,
        min_training_days: usize,
        min_saturated_days: usize,
    ) -> Result<ShadowPolicySupersession, DecisionStoreError> {
        self.supersede_shadow_policy_at(
            scope,
            old_id,
            new_id,
            cutoff_utc,
            new_until_utc,
            reviewer,
            issue_deadline_seconds,
            min_training_days,
            min_saturated_days,
            chrono::Utc::now().timestamp(),
        )
    }

    pub(super) fn supersede_shadow_policy_at(
        &self,
        scope: &DecisionScope,
        old_id: &str,
        new_id: &str,
        cutoff_utc: &str,
        new_until_utc: &str,
        reviewer: &str,
        issue_deadline_seconds: u32,
        min_training_days: usize,
        min_saturated_days: usize,
        reviewed_at: i64,
    ) -> Result<ShadowPolicySupersession, DecisionStoreError> {
        if !scope.valid()
            || old_id.trim().is_empty()
            || new_id.trim().is_empty()
            || old_id == new_id
            || reviewer.trim().is_empty()
            || !(1..=3_600).contains(&issue_deadline_seconds)
            || !(7..=366).contains(&min_training_days)
            || min_saturated_days == 0
            || min_saturated_days > min_training_days
        {
            return Err(DecisionStoreError::Invalid);
        }
        // The handoff cutoff becomes the successor's stored effective_from and
        // is compared as a string on every idempotent retry, so normalise it
        // before anything reads or writes it.
        let canonical_cutoff = shadow_utc_day_key(cutoff_utc)?;
        let canonical_until = shadow_utc_day_key(new_until_utc)?;
        let cutoff_utc = canonical_cutoff.as_str();
        let new_until_utc = canonical_until.as_str();
        let cutoff = shadow_utc_midnight(cutoff_utc)?.timestamp();
        let end = shadow_utc_midnight(new_until_utc)?.timestamp();
        let old = self.load_shadow_policy(scope, old_id)?;
        let old_start = shadow_utc_midnight(&old.effective_from_utc)?.timestamp();
        let old_end = shadow_utc_midnight(&old.effective_until_utc)?.timestamp();
        if cutoff <= old_start || cutoff >= old_end || end <= cutoff || end > old_end {
            return Err(DecisionStoreError::Invalid);
        }
        match self.load_shadow_policy_supersession(scope, old_id) {
            Ok(existing) => {
                let successor = self.load_shadow_policy(scope, new_id)?;
                if existing.new_policy_id == new_id
                    && existing.cutoff_utc == cutoff_utc
                    && existing.reviewer == reviewer
                    && successor.queue_id == old.queue_id
                    && successor.effective_until_utc == new_until_utc
                    && successor.issue_deadline_seconds == issue_deadline_seconds
                    && successor.min_training_days == min_training_days
                    && successor.min_saturated_days == min_saturated_days
                {
                    return Ok(existing);
                }
                return Err(DecisionStoreError::VersionConflict);
            }
            Err(DecisionStoreError::NotFound) => {}
            Err(error) => return Err(error),
        }
        if reviewed_at >= cutoff || reviewed_at < old.registered_at {
            return Err(DecisionStoreError::Invalid);
        }
        let conn = self.open()?;
        assert_readable_shadow_days(&conn, scope, &old.source_lineage)?;
        let has_future_reservation: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM decision_shadow_targets
             WHERE tenant_id=?1 AND acl=?2 AND source_lineage=?3
             AND strftime('%s',target_day_utc) IS NOT NULL
             AND CAST(strftime('%s',target_day_utc) AS INTEGER)>=?4)",
            params![scope.tenant_id, scope.acl, old.source_lineage, cutoff],
            |row| row.get(0),
        )?;
        drop(conn);
        if has_future_reservation {
            return Err(DecisionStoreError::VersionConflict);
        }
        let new_policy = match self.get::<ShadowPilotPolicy>(scope, "shadow_policy", new_id) {
            Ok(existing) => {
                if existing.source_lineage != old.source_lineage
                    || existing.queue_id != old.queue_id
                    || existing.effective_from_utc != cutoff_utc
                    || existing.effective_until_utc != new_until_utc
                    || existing.issue_deadline_seconds != issue_deadline_seconds
                    || existing.min_training_days != min_training_days
                    || existing.min_saturated_days != min_saturated_days
                    || existing.calibration_engine_sha256 != calibration_engine_sha256()
                    || existing.registered_at < old.registered_at
                    || existing.registered_at >= cutoff
                {
                    return Err(DecisionStoreError::VersionConflict);
                }
                existing
            }
            Err(DecisionStoreError::NotFound) => ShadowPilotPolicy {
                id: new_id.into(),
                source_lineage: old.source_lineage.clone(),
                queue_id: old.queue_id.clone(),
                effective_from_utc: cutoff_utc.into(),
                effective_until_utc: new_until_utc.into(),
                issue_deadline_seconds,
                min_training_days,
                min_saturated_days,
                calibration_engine_sha256: calibration_engine_sha256(),
                registered_at: reviewed_at,
            },
            Err(error) => return Err(error),
        };
        let old_digest = self
            .get_with_digest::<ShadowPilotPolicy>(scope, "shadow_policy", old_id)?
            .1;
        let new_digest = self.put(scope, "shadow_policy", new_id, &new_policy, None)?;
        let record = ShadowPolicySupersession {
            old_policy_id: old_id.into(),
            new_policy_id: new_id.into(),
            cutoff_utc: cutoff_utc.into(),
            reviewer: reviewer.into(),
            reviewed_at: new_policy.registered_at,
            old_policy_sha256: old_digest,
            new_policy_sha256: new_digest,
        };
        let payload = serde_json::to_string(&record)?;
        let digest = format!("{:x}", Sha256::digest(payload.as_bytes()));
        let mut conn = self.open()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let old_window_end: Option<i64> = tx
            .query_row(
                "SELECT effective_until FROM decision_shadow_policy_windows
             WHERE tenant_id=?1 AND acl=?2 AND policy_id=?3",
                params![scope.tenant_id, scope.acl, old_id],
                |row| row.get(0),
            )
            .optional()?;
        if old_window_end != Some(old_end) {
            return Err(DecisionStoreError::VersionConflict);
        }
        assert_readable_shadow_days(&tx, scope, &old.source_lineage)?;
        let reserved_future: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM decision_shadow_targets
             WHERE tenant_id=?1 AND acl=?2 AND source_lineage=?3
             AND strftime('%s',target_day_utc) IS NOT NULL
             AND CAST(strftime('%s',target_day_utc) AS INTEGER)>=?4)",
            params![scope.tenant_id, scope.acl, old.source_lineage, cutoff],
            |row| row.get(0),
        )?;
        if reserved_future {
            return Err(DecisionStoreError::VersionConflict);
        }
        tx.execute(
            "UPDATE decision_shadow_policy_windows SET effective_until=?4
             WHERE tenant_id=?1 AND acl=?2 AND policy_id=?3",
            params![scope.tenant_id, scope.acl, old_id, cutoff],
        )?;
        tx.execute(
            "INSERT INTO decision_shadow_policy_windows
             (tenant_id,acl,source_lineage,effective_from,effective_until,policy_id)
             VALUES (?1,?2,?3,?4,?5,?6)",
            params![
                scope.tenant_id,
                scope.acl,
                old.source_lineage,
                cutoff,
                end,
                new_id
            ],
        )?;
        tx.execute(
            "INSERT INTO decision_shadow_policy_supersessions
             (tenant_id,acl,old_policy_id,new_policy_id,cutoff,payload_sha256,payload_json)
             VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![
                scope.tenant_id,
                scope.acl,
                old_id,
                new_id,
                cutoff,
                digest,
                payload
            ],
        )?;
        tx.commit()?;
        self.load_shadow_policy_supersession(scope, old_id)
    }

    pub fn load_shadow_policy(
        &self,
        scope: &DecisionScope,
        id: &str,
    ) -> Result<ShadowPilotPolicy, DecisionStoreError> {
        let (policy, policy_sha256): (ShadowPilotPolicy, String) =
            self.get_with_digest(scope, "shadow_policy", id)?;
        let start = shadow_utc_midnight(&policy.effective_from_utc)?.timestamp();
        let end = shadow_utc_midnight(&policy.effective_until_utc)?.timestamp();
        let reserved: Option<(String, i64, i64)> = self
            .open()?
            .query_row(
                "SELECT source_lineage,effective_from,effective_until
             FROM decision_shadow_policy_windows
             WHERE tenant_id=?1 AND acl=?2 AND policy_id=?3",
                params![scope.tenant_id, scope.acl, id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let effective_end = reserved
            .as_ref()
            .map(|(_, _, effective_end)| *effective_end);
        let supersession_valid = if effective_end.is_some_and(|effective_end| effective_end != end)
        {
            let row: Option<(String, i64, String, String)> = self
                .open()?
                .query_row(
                    "SELECT new_policy_id,cutoff,payload_sha256,payload_json
                 FROM decision_shadow_policy_supersessions
                 WHERE tenant_id=?1 AND acl=?2 AND old_policy_id=?3",
                    params![scope.tenant_id, scope.acl, id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .optional()?;
            row.is_some_and(|(new_id, cutoff, digest, payload)| {
                cutoff == effective_end.unwrap_or_default()
                    && cutoff > start
                    && cutoff < end
                    && digest == format!("{:x}", Sha256::digest(payload.as_bytes()))
                    && serde_json::from_str::<ShadowPolicySupersession>(&payload).is_ok_and(
                        |handoff| {
                            handoff.old_policy_id == id
                                && handoff.new_policy_id == new_id
                                && shadow_utc_midnight(&handoff.cutoff_utc)
                                    .is_ok_and(|time| time.timestamp() == cutoff)
                                && handoff.old_policy_sha256 == policy_sha256
                        },
                    )
            })
        } else {
            let has_handoff: bool = self.open()?.query_row(
                "SELECT EXISTS(SELECT 1 FROM decision_shadow_policy_supersessions
                 WHERE tenant_id=?1 AND acl=?2 AND old_policy_id=?3)",
                params![scope.tenant_id, scope.acl, id],
                |row| row.get(0),
            )?;
            !has_handoff
        };
        if policy.id != id
            || policy.source_lineage.trim().is_empty()
            || policy
                .queue_id
                .as_deref()
                .is_some_and(|id| id.is_empty() || id.trim() != id || id.len() > 128)
            || reserved
                .as_ref()
                .is_none_or(|(lineage, reserved_start, reserved_end)| {
                    lineage != &policy.source_lineage
                        || *reserved_start != start
                        || *reserved_end <= start
                        || *reserved_end > end
                })
            || !supersession_valid
            || policy.registered_at >= start
            || end <= start
            || end - start > 366 * 86_400
            || !(1..=3_600).contains(&policy.issue_deadline_seconds)
            || !(7..=366).contains(&policy.min_training_days)
            || policy.min_saturated_days == 0
            || policy.min_saturated_days > policy.min_training_days
            || policy.calibration_engine_sha256.len() != 64
            || !policy
                .calibration_engine_sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(DecisionStoreError::Corrupt);
        }
        Ok(policy)
    }

    pub fn load_shadow_policy_supersession(
        &self,
        scope: &DecisionScope,
        old_id: &str,
    ) -> Result<ShadowPolicySupersession, DecisionStoreError> {
        if !scope.valid() || old_id.trim().is_empty() {
            return Err(DecisionStoreError::Invalid);
        }
        let (new_id, cutoff, digest, payload): (String, i64, String, String) = self
            .open()?
            .query_row(
                "SELECT new_policy_id,cutoff,payload_sha256,payload_json
             FROM decision_shadow_policy_supersessions
             WHERE tenant_id=?1 AND acl=?2 AND old_policy_id=?3",
                params![scope.tenant_id, scope.acl, old_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?
            .ok_or(DecisionStoreError::NotFound)?;
        if digest != format!("{:x}", Sha256::digest(payload.as_bytes())) {
            return Err(DecisionStoreError::Corrupt);
        }
        let record: ShadowPolicySupersession = serde_json::from_str(&payload)?;
        let old = self.load_shadow_policy(scope, old_id)?;
        let new = self.load_shadow_policy(scope, &record.new_policy_id)?;
        let old_sha = self
            .get_with_digest::<ShadowPilotPolicy>(scope, "shadow_policy", old_id)?
            .1;
        let new_sha = self
            .get_with_digest::<ShadowPilotPolicy>(scope, "shadow_policy", &record.new_policy_id)?
            .1;
        let old_active_end: Option<i64> = self
            .open()?
            .query_row(
                "SELECT effective_until FROM decision_shadow_policy_windows
             WHERE tenant_id=?1 AND acl=?2 AND policy_id=?3",
                params![scope.tenant_id, scope.acl, old_id],
                |row| row.get(0),
            )
            .optional()?;
        if record.old_policy_id != old_id
            || record.old_policy_sha256 != old_sha
            || record.new_policy_sha256 != new_sha
            || record.new_policy_id != new.id
            || record.new_policy_id != new_id
            || old_active_end != Some(cutoff)
            || record.cutoff_utc != new.effective_from_utc
            || shadow_utc_midnight(&record.cutoff_utc)?.timestamp() != cutoff
            || old.source_lineage != new.source_lineage
            || old.queue_id != new.queue_id
            || shadow_utc_midnight(&new.effective_until_utc)?.timestamp()
                > shadow_utc_midnight(&old.effective_until_utc)?.timestamp()
            || record.reviewer.trim().is_empty()
            || record.reviewed_at != new.registered_at
            || record.reviewed_at < old.registered_at
            || record.reviewed_at >= cutoff
        {
            return Err(DecisionStoreError::Corrupt);
        }
        Ok(record)
    }

    #[cfg(test)]
    pub(super) fn reserve_shadow_target(
        &self,
        scope: &DecisionScope,
        source_lineage: &str,
        target_day_utc: &str,
        forecast_id: &str,
        policy_id: &str,
    ) -> Result<(), DecisionStoreError> {
        let mut conn = self.open()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        Self::reserve_shadow_target_in_tx(
            &tx,
            scope,
            source_lineage,
            target_day_utc,
            forecast_id,
            policy_id,
        )?;
        tx.commit()?;
        Ok(())
    }

    pub(super) fn reserve_shadow_target_in_tx(
        tx: &rusqlite::Transaction<'_>,
        scope: &DecisionScope,
        source_lineage: &str,
        target_day_utc: &str,
        forecast_id: &str,
        policy_id: &str,
    ) -> Result<(), DecisionStoreError> {
        // The reservation row is the per-day uniqueness key; store the one
        // spelling SQLite's day arithmetic can read back.
        let canonical_day = shadow_utc_day_key(target_day_utc)?;
        let target_day_utc = canonical_day.as_str();
        let target = shadow_utc_midnight(target_day_utc)?.timestamp();
        let window: Option<(String, i64, i64)> = tx
            .query_row(
                "SELECT source_lineage,effective_from,effective_until
             FROM decision_shadow_policy_windows
             WHERE tenant_id=?1 AND acl=?2 AND policy_id=?3",
                params![scope.tenant_id, scope.acl, policy_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        if window.as_ref().is_none_or(|(lineage, start, end)| {
            lineage != source_lineage || target < *start || target >= *end
        }) {
            return Err(DecisionStoreError::VersionConflict);
        }
        assert_readable_shadow_days(&tx, scope, source_lineage)?;
        let existing = {
            let mut stmt = tx.prepare(
                "SELECT target_day_utc,forecast_id FROM decision_shadow_targets
                 WHERE tenant_id=?1 AND acl=?2 AND source_lineage=?3
                 AND strftime('%s',target_day_utc) IS NOT NULL
                 AND CAST(strftime('%s',target_day_utc) AS INTEGER)=?4",
            )?;
            stmt.query_map(
                params![scope.tenant_id, scope.acl, source_lineage, target],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )?
            .collect::<Result<Vec<_>, _>>()?
        };
        match existing.as_slice() {
            [] => {
                tx.execute(
                    "INSERT INTO decision_shadow_targets
                     (tenant_id,acl,source_lineage,target_day_utc,forecast_id)
                     VALUES (?1,?2,?3,?4,?5)",
                    params![
                        scope.tenant_id,
                        scope.acl,
                        source_lineage,
                        target_day_utc,
                        forecast_id
                    ],
                )?;
            }
            [(saved_day, saved_id)] if saved_day == target_day_utc && saved_id == forecast_id => {}
            [_] => return Err(DecisionStoreError::VersionConflict),
            _ => return Err(DecisionStoreError::Corrupt),
        }
        Ok(())
    }

    pub(super) fn reserve_shadow_score_in_tx(
        tx: &rusqlite::Transaction<'_>,
        scope: &DecisionScope,
        forecast_id: &str,
        score_id: &str,
    ) -> Result<(), DecisionStoreError> {
        tx.execute(
            "INSERT OR IGNORE INTO decision_shadow_scores
             (tenant_id,acl,forecast_id,score_id) VALUES (?1,?2,?3,?4)",
            params![scope.tenant_id, scope.acl, forecast_id, score_id],
        )?;
        let reserved: Option<String> = tx
            .query_row(
                "SELECT score_id FROM decision_shadow_scores
             WHERE tenant_id=?1 AND acl=?2 AND forecast_id=?3",
                params![scope.tenant_id, scope.acl, forecast_id],
                |row| row.get(0),
            )
            .optional()?;
        if reserved.as_deref() != Some(score_id) {
            return Err(DecisionStoreError::VersionConflict);
        }
        Ok(())
    }

    pub(super) fn reserve_shadow_sla_in_tx(
        tx: &rusqlite::Transaction<'_>,
        scope: &DecisionScope,
        forecast_id: &str,
        sla_id: &str,
    ) -> Result<(), DecisionStoreError> {
        tx.execute(
            "INSERT OR IGNORE INTO decision_shadow_sla_forecasts
             (tenant_id,acl,forecast_id,sla_id) VALUES (?1,?2,?3,?4)",
            params![scope.tenant_id, scope.acl, forecast_id, sla_id],
        )?;
        let reserved: Option<String> = tx
            .query_row(
                "SELECT sla_id FROM decision_shadow_sla_forecasts
             WHERE tenant_id=?1 AND acl=?2 AND forecast_id=?3",
                params![scope.tenant_id, scope.acl, forecast_id],
                |row| row.get(0),
            )
            .optional()?;
        if reserved.as_deref() != Some(sla_id) {
            return Err(DecisionStoreError::VersionConflict);
        }
        Ok(())
    }

}
