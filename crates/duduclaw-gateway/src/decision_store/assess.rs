use super::*;

impl DecisionStore {
    /// Evaluate every completed UTC day in a policy's active window. Missing
    /// and revoked days remain in the denominator; aggregate errors are
    /// withheld until the whole due sequence is valid.
    pub fn assess_shadow_policy(
        &self,
        scope: &DecisionScope,
        policy_id: &str,
    ) -> Result<ShadowPolicyAssessment, DecisionStoreError> {
        self.assess_shadow_policy_at(scope, policy_id, chrono::Utc::now().timestamp())
    }

    pub(crate) fn assess_shadow_policy_at(
        &self,
        scope: &DecisionScope,
        policy_id: &str,
        assessed_at: i64,
    ) -> Result<ShadowPolicyAssessment, DecisionStoreError> {
        let policy = self.load_shadow_policy(scope, policy_id)?;
        let (_, policy_sha256): (ShadowPilotPolicy, String) =
            self.get_with_digest(scope, "shadow_policy", policy_id)?;
        let window: (i64, i64) = self.open()?.query_row(
            "SELECT effective_from,effective_until FROM decision_shadow_policy_windows
             WHERE tenant_id=?1 AND acl=?2 AND policy_id=?3",
            params![scope.tenant_id, scope.acl, policy_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let assessed = chrono::DateTime::<chrono::Utc>::from_timestamp(assessed_at, 0)
            .ok_or(DecisionStoreError::Invalid)?;
        let due_end = window.1.min(assessed_at.div_euclid(86_400) * 86_400);
        let mut days = Vec::new();
        let mut scored_days = 0;
        let mut corrected_days = 0;
        let mut sums = ShadowErrorSums {
            arrivals: 0,
            backlog: 0,
            no_change_backlog: 0,
            seasonal_naive_backlog: 0,
            mean_change_backlog: 0,
        };
        let mut forecast_points = Vec::new();
        // One connection for every day's reservation lookup: the window runs
        // up to 366 days and `open()` takes a write lock on each call.
        let reservation_conn = self.open()?;
        assert_readable_shadow_days(&reservation_conn, scope, &policy.source_lineage)?;
        let mut day = window.0;
        while day < due_end {
            let target_day_utc = chrono::DateTime::<chrono::Utc>::from_timestamp(day, 0)
                .ok_or(DecisionStoreError::Corrupt)?
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
            let mut item = ShadowDayAssessment {
                target_day_utc,
                status: ShadowDayStatus::MissingForecast,
                forecast_id: None,
                score_revision_id: None,
                score_revision_sha256: None,
                corrected: false,
            };
            let reservations = {
                let mut stmt = reservation_conn.prepare(
                    "SELECT forecast_id FROM decision_shadow_targets
                     WHERE tenant_id=?1 AND acl=?2 AND source_lineage=?3
                     AND strftime('%s',target_day_utc) IS NOT NULL
                     AND CAST(strftime('%s',target_day_utc) AS INTEGER)=?4",
                )?;
                stmt.query_map(
                    params![scope.tenant_id, scope.acl, policy.source_lineage, day],
                    |row| row.get::<_, String>(0),
                )?
                .collect::<Result<Vec<_>, _>>()?
            };
            match reservations.as_slice() {
                [] => {}
                [forecast_id] => {
                    item.forecast_id = Some(forecast_id.clone());
                    match self.load_shadow_forecast(scope, forecast_id) {
                        Ok(forecast)
                            if forecast.policy_id == policy_id
                                && forecast.committed_at <= assessed_at =>
                        {
                            match self.shadow_score_head(scope, forecast_id) {
                                Err(DecisionStoreError::NotFound) => {
                                    item.status = ShadowDayStatus::Unscored;
                                }
                                Err(_) => item.status = ShadowDayStatus::ScoreInvalid,
                                Ok((score_id, corrected)) => {
                                    item.score_revision_id = Some(score_id.clone());
                                    item.corrected = corrected;
                                    let score = if corrected {
                                        self.load_shadow_score_correction(scope, &score_id)
                                            .map(|record| record.corrected_score)
                                    } else {
                                        self.load_shadow_score(scope, &score_id)
                                    };
                                    match score {
                                        Err(DecisionStoreError::Revoked) => {
                                            item.status = ShadowDayStatus::ScoreRevoked;
                                        }
                                        Err(_) => item.status = ShadowDayStatus::ScoreInvalid,
                                        Ok(score) if score.scored_at > assessed_at => {
                                            item.status = ShadowDayStatus::ScoreInvalid;
                                        }
                                        Ok(score) => {
                                            let kind = if corrected {
                                                "shadow_score_correction"
                                            } else {
                                                "shadow_score"
                                            };
                                            match self.get_with_digest::<serde_json::Value>(
                                                scope, kind, &score_id,
                                            ) {
                                                Ok((_, digest)) => {
                                                    item.score_revision_sha256 = Some(digest);
                                                    item.status = ShadowDayStatus::Scored;
                                                    scored_days += 1;
                                                    if corrected {
                                                        corrected_days += 1;
                                                    }
                                                    sums.arrivals +=
                                                        score.arrivals_abs_error as u128;
                                                    sums.backlog += score.backlog_abs_error as u128;
                                                    sums.no_change_backlog +=
                                                        score.no_change_abs_error as u128;
                                                    sums.seasonal_naive_backlog +=
                                                        score.seasonal_naive_abs_error as u128;
                                                    sums.mean_change_backlog +=
                                                        score.mean_change_abs_error as u128;
                                                    forecast_points.push(ForecastPoint {
                                                        day_index: days.len(),
                                                        predicted_arrivals: forecast
                                                            .forecast
                                                            .predicted_arrivals,
                                                        actual_arrivals: score.observed.arrivals,
                                                        predicted_backlog_end: forecast
                                                            .forecast
                                                            .predicted_backlog_end,
                                                        actual_backlog_end: score
                                                            .observed
                                                            .backlog_end,
                                                        no_change_backlog_end: forecast
                                                            .forecast
                                                            .no_change_backlog_end,
                                                        seasonal_naive_backlog_end: forecast
                                                            .forecast
                                                            .seasonal_naive_backlog_end,
                                                        mean_change_backlog_end: forecast
                                                            .forecast
                                                            .mean_change_backlog_end,
                                                        fitted_capacity_per_agent: forecast
                                                            .forecast
                                                            .fitted_capacity_per_agent,
                                                    });
                                                }
                                                Err(DecisionStoreError::Revoked) => {
                                                    item.status = ShadowDayStatus::ScoreRevoked;
                                                }
                                                Err(_) => {
                                                    item.status = ShadowDayStatus::ScoreInvalid
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        Ok(_) => item.status = ShadowDayStatus::ForecastInvalid,
                        Err(DecisionStoreError::Revoked) => {
                            item.status = ShadowDayStatus::ForecastRevoked;
                        }
                        Err(_) => item.status = ShadowDayStatus::ForecastInvalid,
                    }
                }
                _ => item.status = ShadowDayStatus::ForecastInvalid,
            }
            days.push(item);
            day += 86_400;
        }
        drop(reservation_conn);
        let complete = !days.is_empty() && scored_days == days.len();
        let backlog_abs_error_below_each_baseline = complete.then_some(
            sums.backlog < sums.no_change_backlog
                && sums.backlog < sums.seasonal_naive_backlog
                && sums.backlog < sums.mean_change_backlog,
        );
        let recent_7_day_error_sums = if complete && forecast_points.len() >= 21 {
            let mut recent = ShadowErrorSums {
                arrivals: 0,
                backlog: 0,
                no_change_backlog: 0,
                seasonal_naive_backlog: 0,
                mean_change_backlog: 0,
            };
            for point in forecast_points.iter().rev().take(7) {
                recent.arrivals += point.predicted_arrivals.abs_diff(point.actual_arrivals) as u128;
                recent.backlog += point
                    .predicted_backlog_end
                    .abs_diff(point.actual_backlog_end) as u128;
                recent.no_change_backlog += point
                    .no_change_backlog_end
                    .abs_diff(point.actual_backlog_end)
                    as u128;
                recent.seasonal_naive_backlog += point
                    .seasonal_naive_backlog_end
                    .abs_diff(point.actual_backlog_end)
                    as u128;
                recent.mean_change_backlog += point
                    .mean_change_backlog_end
                    .abs_diff(point.actual_backlog_end)
                    as u128;
            }
            Some(recent)
        } else {
            None
        };
        let recent_7_day_backlog_abs_error_below_each_baseline =
            recent_7_day_error_sums.as_ref().map(|recent| {
                recent.backlog < recent.no_change_backlog
                    && recent.backlog < recent.seasonal_naive_backlog
                    && recent.backlog < recent.mean_change_backlog
            });
        let fixed_prefix_interval = if complete && forecast_points.len() >= 21 {
            let forecast = ForecastBacktestResult {
                evaluation_days: forecast_points.len(),
                arrival_abs_error_sum: sums.arrivals,
                model_abs_error_sum: sums.backlog,
                no_change_abs_error_sum: sums.no_change_backlog,
                seasonal_naive_abs_error_sum: sums.seasonal_naive_backlog,
                mean_change_abs_error_sum: sums.mean_change_backlog,
                model_beats_all_baselines: backlog_abs_error_below_each_baseline == Some(true),
                points: forecast_points,
            };
            Some(
                diagnose_fixed_forecast_intervals(&forecast, 14)
                    .map_err(|_| DecisionStoreError::Corrupt)?,
            )
        } else {
            None
        };
        Ok(ShadowPolicyAssessment {
            policy_id: policy_id.into(),
            policy_sha256,
            source_lineage: policy.source_lineage,
            queue_id: policy.queue_id,
            assessed_at_utc: assessed.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            due_days: days.len(),
            scored_days,
            corrected_days,
            complete,
            error_sums: complete.then_some(sums),
            backlog_abs_error_below_each_baseline,
            fixed_prefix_interval,
            recent_7_day_error_sums,
            recent_7_day_backlog_abs_error_below_each_baseline,
            days,
        })
    }

    pub fn assess_shadow_sla_policy(
        &self,
        scope: &DecisionScope,
        policy_id: &str,
    ) -> Result<ShadowSlaPolicyAssessment, DecisionStoreError> {
        self.assess_shadow_sla_policy_at(scope, policy_id, chrono::Utc::now().timestamp())
    }

    pub(crate) fn assess_shadow_sla_policy_at(
        &self,
        scope: &DecisionScope,
        policy_id: &str,
        assessed_at: i64,
    ) -> Result<ShadowSlaPolicyAssessment, DecisionStoreError> {
        let aggregate = self.assess_shadow_policy_at(scope, policy_id, assessed_at)?;
        let mut days = Vec::with_capacity(aggregate.due_days);
        let mut scored_days = 0_usize;
        let mut corrected_days = 0_usize;
        let mut total_abs_error = 0_u128;
        let mut no_change_total_abs_error = 0_u128;
        let mut seasonal_naive_total_abs_error = 0_u128;
        let mut seven_day_mean_total_abs_error = 0_u128;
        let mut total_predicted = 0_u128;
        let mut total_observed = 0_u128;
        let mut previous_observation: Option<(i64, ShadowSlaObservationExport)> = None;
        for aggregate_day in &aggregate.days {
            let mut item = ShadowSlaDayAssessment {
                target_day_utc: aggregate_day.target_day_utc.clone(),
                aggregate_status: aggregate_day.status,
                status: ShadowSlaDayStatus::AggregateUnavailable,
                aggregate_forecast_id: aggregate_day.forecast_id.clone(),
                sla_forecast_id: None,
                score_revision_id: None,
                score_revision_sha256: None,
                corrected: false,
                predicted_resolved_within_sla: None,
                observed_resolved_within_sla: None,
                abs_error: None,
                no_change_abs_error: None,
                seasonal_naive_abs_error: None,
                seven_day_mean_abs_error: None,
            };
            if aggregate_day.status != ShadowDayStatus::Scored {
                days.push(item);
                continue;
            }
            let forecast_id = aggregate_day
                .forecast_id
                .as_ref()
                .ok_or(DecisionStoreError::Corrupt)?;
            let sla_id: Option<String> = self
                .open()?
                .query_row(
                    "SELECT sla_id FROM decision_shadow_sla_forecasts
                 WHERE tenant_id=?1 AND acl=?2 AND forecast_id=?3",
                    params![scope.tenant_id, scope.acl, forecast_id],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(sla_id) = sla_id else {
                item.status = ShadowSlaDayStatus::MissingSlaForecast;
                days.push(item);
                continue;
            };
            item.sla_forecast_id = Some(sla_id.clone());
            let sla = match self.load_shadow_sla_forecast(scope, &sla_id) {
                Err(DecisionStoreError::Revoked) => {
                    item.status = ShadowSlaDayStatus::SlaForecastRevoked;
                    days.push(item);
                    continue;
                }
                Err(_) => {
                    item.status = ShadowSlaDayStatus::SlaForecastInvalid;
                    days.push(item);
                    continue;
                }
                Ok(sla)
                    if sla.forecast_id != *forecast_id
                        || sla.target_day_utc != aggregate_day.target_day_utc
                        || sla.committed_at > assessed_at =>
                {
                    item.status = ShadowSlaDayStatus::SlaForecastInvalid;
                    days.push(item);
                    continue;
                }
                Ok(sla) => sla,
            };
            let (score_id, corrected) = match self.shadow_sla_score_head(scope, &sla_id) {
                Ok(head) => head,
                Err(DecisionStoreError::NotFound) => {
                    item.status = ShadowSlaDayStatus::Unscored;
                    days.push(item);
                    continue;
                }
                Err(_) => {
                    item.status = ShadowSlaDayStatus::ScoreInvalid;
                    days.push(item);
                    continue;
                }
            };
            item.score_revision_id = Some(score_id.clone());
            item.corrected = corrected;
            let score = match self.load_current_shadow_sla_score(scope, &sla_id) {
                Ok(score) => score,
                Err(DecisionStoreError::Revoked) => {
                    item.status = ShadowSlaDayStatus::ScoreRevoked;
                    days.push(item);
                    continue;
                }
                Err(DecisionStoreError::VersionConflict) => {
                    item.status = ShadowSlaDayStatus::ScoreStale;
                    days.push(item);
                    continue;
                }
                Err(_) => {
                    item.status = ShadowSlaDayStatus::ScoreInvalid;
                    days.push(item);
                    continue;
                }
            };
            let kind = if corrected {
                "shadow_sla_score_correction"
            } else {
                "shadow_sla_score"
            };
            match self.get_with_digest::<serde_json::Value>(scope, kind, &score_id) {
                Ok((_, digest))
                    if score.id == score_id
                        && score.sla_forecast_id == sla_id
                        && score.aggregate_score_id
                            == aggregate_day.score_revision_id.as_deref().unwrap_or("")
                        && score.aggregate_score_sha256
                            == aggregate_day.score_revision_sha256.as_deref().unwrap_or("")
                        && score.scored_at <= assessed_at =>
                {
                    let sources = (|| -> Result<_, DecisionStoreError> {
                        let (_, opening_source) = self.shadow_artifact(
                            scope,
                            &sla.opening_artifact_id,
                            "shadow_sla_opening_export",
                            Some(&sla.opening_sha256),
                        )?;
                        let (_, observation_source) = self.shadow_artifact(
                            scope,
                            &score.observation_artifact_id,
                            "shadow_sla_observation_export",
                            Some(&score.observation_sha256),
                        )?;
                        Ok((
                            serde_json::from_str::<ShadowSlaOpeningExport>(&opening_source)?,
                            serde_json::from_str::<ShadowSlaObservationExport>(
                                &observation_source,
                            )?,
                        ))
                    })();
                    let (opening, observation) = match sources {
                        Ok(sources) => sources,
                        Err(DecisionStoreError::Revoked) => {
                            item.status = ShadowSlaDayStatus::ScoreRevoked;
                            days.push(item);
                            continue;
                        }
                        Err(_) => {
                            item.status = ShadowSlaDayStatus::ScoreInvalid;
                            days.push(item);
                            continue;
                        }
                    };
                    let target = shadow_utc_midnight(&item.target_day_utc)?.timestamp();
                    if previous_observation
                        .as_ref()
                        .is_some_and(|(previous_target, previous)| {
                            *previous_target == target - 86_400
                                && !matches!(
                                    shadow_sla_day_boundary_matches(previous, &opening),
                                    Ok(true)
                                )
                        })
                    {
                        item.status = ShadowSlaDayStatus::CrossDayIdentityMismatch;
                        previous_observation = None;
                        days.push(item);
                        continue;
                    }
                    previous_observation = Some((target, observation));
                    item.status = ShadowSlaDayStatus::Scored;
                    item.score_revision_sha256 = Some(digest);
                    item.predicted_resolved_within_sla = Some(score.predicted_resolved_within_sla);
                    item.observed_resolved_within_sla = Some(score.observed_resolved_within_sla);
                    item.abs_error = Some(score.abs_error);
                    item.no_change_abs_error = Some(score.no_change_abs_error);
                    item.seasonal_naive_abs_error = Some(score.seasonal_naive_abs_error);
                    item.seven_day_mean_abs_error = Some(score.seven_day_mean_abs_error);
                    scored_days += 1;
                    if corrected {
                        corrected_days += 1;
                    }
                    total_abs_error = total_abs_error
                        .checked_add(u128::from(score.abs_error))
                        .ok_or(DecisionStoreError::Corrupt)?;
                    no_change_total_abs_error = no_change_total_abs_error
                        .checked_add(u128::from(score.no_change_abs_error))
                        .ok_or(DecisionStoreError::Corrupt)?;
                    seasonal_naive_total_abs_error = seasonal_naive_total_abs_error
                        .checked_add(u128::from(score.seasonal_naive_abs_error))
                        .ok_or(DecisionStoreError::Corrupt)?;
                    seven_day_mean_total_abs_error = seven_day_mean_total_abs_error
                        .checked_add(u128::from(score.seven_day_mean_abs_error))
                        .ok_or(DecisionStoreError::Corrupt)?;
                    total_predicted = total_predicted
                        .checked_add(u128::from(score.predicted_resolved_within_sla))
                        .ok_or(DecisionStoreError::Corrupt)?;
                    total_observed = total_observed
                        .checked_add(u128::from(score.observed_resolved_within_sla))
                        .ok_or(DecisionStoreError::Corrupt)?;
                }
                Err(DecisionStoreError::Revoked) => {
                    item.status = ShadowSlaDayStatus::ScoreRevoked;
                }
                _ => item.status = ShadowSlaDayStatus::ScoreInvalid,
            }
            days.push(item);
        }
        let complete = !days.is_empty() && scored_days == days.len();
        let fixed_prefix_interval = if complete && days.len() >= 21 {
            Some(diagnose_fixed_sla_intervals(&days, 14)?)
        } else {
            None
        };
        let recent_7_day_error_sums = if complete && days.len() >= 21 {
            let mut recent = ShadowSlaErrorSums {
                model: 0,
                no_change: 0,
                seasonal_naive: 0,
                seven_day_mean: 0,
            };
            for day in days.iter().rev().take(7) {
                recent.model += u128::from(day.abs_error.ok_or(DecisionStoreError::Corrupt)?);
                recent.no_change +=
                    u128::from(day.no_change_abs_error.ok_or(DecisionStoreError::Corrupt)?);
                recent.seasonal_naive += u128::from(
                    day.seasonal_naive_abs_error
                        .ok_or(DecisionStoreError::Corrupt)?,
                );
                recent.seven_day_mean += u128::from(
                    day.seven_day_mean_abs_error
                        .ok_or(DecisionStoreError::Corrupt)?,
                );
            }
            Some(recent)
        } else {
            None
        };
        let recent_7_day_model_abs_error_below_each_baseline =
            recent_7_day_error_sums.as_ref().map(|recent| {
                recent.model < recent.no_change
                    && recent.model < recent.seasonal_naive
                    && recent.model < recent.seven_day_mean
            });
        Ok(ShadowSlaPolicyAssessment {
            policy_id: aggregate.policy_id,
            policy_sha256: aggregate.policy_sha256,
            source_lineage: aggregate.source_lineage,
            queue_id: aggregate.queue_id,
            assessed_at_utc: aggregate.assessed_at_utc,
            due_days: days.len(),
            scored_days,
            corrected_days,
            complete,
            total_abs_error: complete.then_some(total_abs_error),
            no_change_total_abs_error: complete.then_some(no_change_total_abs_error),
            seasonal_naive_total_abs_error: complete.then_some(seasonal_naive_total_abs_error),
            seven_day_mean_total_abs_error: complete.then_some(seven_day_mean_total_abs_error),
            model_abs_error_below_each_baseline: complete.then_some(
                total_abs_error < no_change_total_abs_error
                    && total_abs_error < seasonal_naive_total_abs_error
                    && total_abs_error < seven_day_mean_total_abs_error,
            ),
            fixed_prefix_interval,
            recent_7_day_error_sums,
            recent_7_day_model_abs_error_below_each_baseline,
            total_predicted_resolved_within_sla: complete.then_some(total_predicted),
            total_observed_resolved_within_sla: complete.then_some(total_observed),
            days,
        })
    }

}
