use super::*;


pub(super) fn shadow_utc_midnight(
    value: &str,
) -> Result<chrono::DateTime<chrono::FixedOffset>, DecisionStoreError> {
    let time =
        chrono::DateTime::parse_from_rfc3339(value).map_err(|_| DecisionStoreError::Invalid)?;
    if time.offset().local_minus_utc() != 0
        || time.time() != chrono::NaiveTime::from_hms_opt(0, 0, 0).expect("midnight")
    {
        return Err(DecisionStoreError::Invalid);
    }
    Ok(time)
}

/// Canonical `YYYY-MM-DDTHH:MM:SSZ` spelling of a validated UTC midnight.
///
/// chrono's RFC3339 parser accepts `t`, `T` and a space as the date/time
/// separator and both `Z` and `+00:00` as the zero offset; SQLite's date
/// functions accept neither `t` nor `+00:00` in `strftime('%s',...)` and
/// return NULL instead. Every day string that reaches a SQLite key or day
/// guard is stored through this function so the Rust-side validator and the
/// SQL-side day arithmetic agree on exactly one spelling. Alias spellings are
/// still accepted as input; they are normalised, never stored verbatim.
pub(super) fn shadow_utc_day_key(value: &str) -> Result<String, DecisionStoreError> {
    Ok(shadow_utc_midnight(value)?.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}

/// Fail closed on any reservation row whose day SQLite cannot parse.
///
/// A row written before day strings were normalised is invisible to every
/// `strftime` day guard (`NULL = x` and `NULL >= x` are both false), which
/// would silently turn "a forecast already exists for this day" into "no
/// forecast exists". Unreadable rows are reported as corruption instead.
pub(super) fn assert_readable_shadow_days(
    conn: &Connection,
    scope: &DecisionScope,
    source_lineage: &str,
) -> Result<(), DecisionStoreError> {
    let unreadable: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM decision_shadow_targets
         WHERE tenant_id=?1 AND acl=?2 AND source_lineage=?3
         AND strftime('%s',target_day_utc) IS NULL)",
        params![scope.tenant_id, scope.acl, source_lineage],
        |row| row.get(0),
    )?;
    if unreadable {
        return Err(DecisionStoreError::Corrupt);
    }
    Ok(())
}

pub(super) fn first_full_utc_day_at_or_after(
    cutoff: chrono::DateTime<chrono::FixedOffset>,
) -> Option<chrono::DateTime<chrono::Utc>> {
    let midnight = chrono::NaiveTime::from_hms_opt(0, 0, 0)?;
    let start = cutoff.date_naive().and_time(midnight).and_utc();
    if cutoff.time() == midnight {
        Some(start)
    } else {
        start.checked_add_signed(chrono::Duration::days(1))
    }
}

pub(super) fn shadow_export_window(export: &ObservedOutcomeExport) -> Result<(i64, i64), DecisionStoreError> {
    if export.observed_days.is_empty()
        || export.observed_days.len() > 366
        || export
            .queue_id
            .as_deref()
            .is_some_and(|id| id.is_empty() || id.trim() != id || id.len() > 128)
    {
        return Err(DecisionStoreError::Invalid);
    }
    validate_observed_days(&export.observed_days)?;
    if !valid_observed_sla_labels(
        &export.observed_days,
        export.resolved_within_sla_by_day.as_deref(),
        export.sla_days,
    ) {
        return Err(DecisionStoreError::Invalid);
    }
    let start = shadow_utc_midnight(&export.window_start_utc)?;
    let through = shadow_utc_midnight(&export.observed_through_utc)?;
    let expected = start
        .checked_add_signed(chrono::Duration::days(export.observed_days.len() as i64))
        .ok_or(DecisionStoreError::Invalid)?;
    if expected != through {
        return Err(DecisionStoreError::Invalid);
    }
    Ok((start.timestamp(), through.timestamp()))
}

/// Reject undeclared fields in each prior-day row. The general observation
/// schema is shared with historical imports, so this prospective boundary
/// applies stricter parsing without changing old calibration code hashes.
pub(super) fn parse_shadow_training_export(source: &str) -> Result<ObservedOutcomeExport, DecisionStoreError> {
    const DAY_FIELDS: [&str; 6] = [
        "arrivals",
        "backlog_start",
        "resolved",
        "backlog_end",
        "agents",
        "fixed_extra_capacity",
    ];
    let value: serde_json::Value = serde_json::from_str(source)?;
    let days = value
        .get("observed_days")
        .and_then(serde_json::Value::as_array)
        .ok_or(DecisionStoreError::Invalid)?;
    if days.iter().any(|day| {
        day.as_object().is_none_or(|fields| {
            fields.len() != DAY_FIELDS.len()
                || DAY_FIELDS.iter().any(|field| !fields.contains_key(*field))
        })
    }) {
        return Err(DecisionStoreError::Invalid);
    }
    serde_json::from_value(value).map_err(DecisionStoreError::from)
}

/// Validate prospective training bytes before registering a source artifact.
/// The store repeats this check after resolving the exact scoped artifact.
pub fn validate_shadow_training_source(
    source: &str,
    target_day_utc: &str,
    policy: &ShadowPilotPolicy,
    known: &KnownDayInputs,
) -> Result<(ObservedOutcomeExport, ProspectiveForecast), DecisionStoreError> {
    let target = shadow_utc_midnight(target_day_utc)?.timestamp();
    let export = parse_shadow_training_export(source)?;
    let (_, through) = shadow_export_window(&export)?;
    if through != target
        || export.observed_days.len() < policy.min_training_days
        || export.queue_id.is_none()
        || policy
            .queue_id
            .as_ref()
            .is_some_and(|expected| export.queue_id.as_ref() != Some(expected))
    {
        return Err(DecisionStoreError::Invalid);
    }
    let forecast = forecast_next_day(&export.observed_days, known, policy.min_saturated_days)?;
    Ok((export, forecast))
}

/// Preflight opening identities and ages against a frozen backlog forecast.
pub fn validate_shadow_sla_opening_source(
    source: &str,
    forecast: &StoredShadowForecast,
    training: &[ObservedSupportDay],
    model: &QueueModel,
) -> Result<
    (
        KnownSlaDayInputs,
        ProspectiveSlaForecast,
        ShadowSlaBaselines,
    ),
    DecisionStoreError,
> {
    fn has_fields(value: &serde_json::Value, names: &[&str]) -> bool {
        value.as_object().is_some_and(|fields| {
            fields.len() == names.len() && names.iter().all(|name| fields.contains_key(*name))
        })
    }
    let opening: ShadowSlaOpeningExport = serde_json::from_str(source)?;
    let value: serde_json::Value = serde_json::from_str(source)?;
    if !has_fields(
        &value,
        &["inputs", "opening_tickets", "prior_resolved_tickets"],
    ) || !has_fields(
        &value["inputs"],
        &["target_day_utc", "queue_id", "opening_cohorts", "known"],
    ) || !has_fields(
        &value["inputs"]["known"],
        &[
            "opening_backlog",
            "planned_agents",
            "planned_fixed_extra_capacity",
        ],
    ) || value["inputs"]["opening_cohorts"]
        .as_array()
        .is_none_or(|cohorts| {
            cohorts
                .iter()
                .any(|cohort| !has_fields(cohort, &["age_days", "count"]))
        })
        || value["opening_tickets"].as_array().is_none_or(|tickets| {
            tickets
                .iter()
                .any(|ticket| !has_fields(ticket, &["ticket_id", "created_at_utc"]))
        })
    {
        return Err(DecisionStoreError::Invalid);
    }
    let inputs = opening.inputs;
    let target = shadow_utc_midnight(&inputs.target_day_utc)?.timestamp();
    if opening.opening_tickets.len() as u64 != inputs.known.opening_backlog {
        return Err(DecisionStoreError::Invalid);
    }
    let mut ids = std::collections::HashSet::new();
    let mut ages = std::collections::BTreeMap::<u32, u32>::new();
    for ticket in &opening.opening_tickets {
        if ticket.ticket_id.is_empty()
            || ticket.ticket_id.len() > 256
            || ticket.ticket_id.trim() != ticket.ticket_id
            || !ids.insert(&ticket.ticket_id)
        {
            return Err(DecisionStoreError::Invalid);
        }
        let created = chrono::DateTime::parse_from_rfc3339(&ticket.created_at_utc)
            .map_err(|_| DecisionStoreError::Invalid)?;
        if created.offset().local_minus_utc() != 0 || created.timestamp() >= target {
            return Err(DecisionStoreError::Invalid);
        }
        let age = (target - created.timestamp()).div_euclid(86_400)
            + i64::from((target - created.timestamp()).rem_euclid(86_400) != 0);
        let age = u32::try_from(age).map_err(|_| DecisionStoreError::Invalid)?;
        let count = ages.entry(age).or_default();
        *count = count.checked_add(1).ok_or(DecisionStoreError::Invalid)?;
    }
    let expected: std::collections::BTreeMap<u32, u32> = inputs
        .opening_cohorts
        .iter()
        .map(|cohort| (cohort.age_days, cohort.count))
        .collect();
    if ages != expected {
        return Err(DecisionStoreError::Invalid);
    }
    if forecast.queue_id.is_none()
        || inputs.queue_id != forecast.queue_id
        || inputs.target_day_utc != forecast.target_day_utc
        || inputs.known != forecast.known
        || training.len() != forecast.forecast.training_days
        || training.len() < 7
    {
        return Err(DecisionStoreError::Invalid);
    }
    let start = target - 7 * 86_400;
    let mut resolved_per_day = [0_u32; 7];
    let mut within_sla_per_day = [0_u32; 7];
    for ticket in &opening.prior_resolved_tickets {
        if ticket.queue_id != inputs.queue_id
            || ticket.ticket_id.is_empty()
            || ticket.ticket_id.len() > 256
            || ticket.ticket_id.trim() != ticket.ticket_id
            || !ids.insert(&ticket.ticket_id)
        {
            return Err(DecisionStoreError::Invalid);
        }
        let created = chrono::DateTime::parse_from_rfc3339(&ticket.created_at_utc)
            .map_err(|_| DecisionStoreError::Invalid)?;
        let resolved = chrono::DateTime::parse_from_rfc3339(
            ticket
                .resolved_at_utc
                .as_deref()
                .ok_or(DecisionStoreError::Invalid)?,
        )
        .map_err(|_| DecisionStoreError::Invalid)?;
        if created.offset().local_minus_utc() != 0
            || resolved.offset().local_minus_utc() != 0
            || created > resolved
            || resolved.timestamp() < start
            || resolved.timestamp() >= target
        {
            return Err(DecisionStoreError::Invalid);
        }
        let day = usize::try_from((resolved.timestamp() - start).div_euclid(86_400))
            .map_err(|_| DecisionStoreError::Invalid)?;
        resolved_per_day[day] = resolved_per_day[day]
            .checked_add(1)
            .ok_or(DecisionStoreError::Invalid)?;
        let age = resolved.timestamp().div_euclid(86_400) - created.timestamp().div_euclid(86_400);
        if age < i64::from(model.sla_days) {
            within_sla_per_day[day] = within_sla_per_day[day]
                .checked_add(1)
                .ok_or(DecisionStoreError::Invalid)?;
        }
    }
    for (i, observed) in training[training.len() - 7..].iter().enumerate() {
        if resolved_per_day[i] != observed.resolved {
            return Err(DecisionStoreError::Invalid);
        }
    }
    let total: u64 = within_sla_per_day
        .iter()
        .map(|count| u64::from(*count))
        .sum();
    let baselines = ShadowSlaBaselines {
        no_change: within_sla_per_day[6],
        seasonal_naive: within_sla_per_day[0],
        seven_day_mean: u32::try_from((total + 3) / 7).map_err(|_| DecisionStoreError::Invalid)?,
    };
    let prediction = forecast_next_day_sla(training, &inputs, model, forecast.min_saturated_days)?;
    if prediction.backlog_forecast != forecast.forecast {
        return Err(DecisionStoreError::Invalid);
    }
    Ok((inputs, prediction, baselines))
}

/// Reconcile every opening ID and all day-end flows with the aggregate score.
pub fn validate_shadow_sla_observation_source(
    source: &str,
    opening_source: &str,
    sla: &StoredShadowSlaForecast,
    aggregate: &StoredShadowScore,
    model: &QueueModel,
) -> Result<u64, DecisionStoreError> {
    let export: ShadowSlaObservationExport = serde_json::from_str(source)?;
    let opening: ShadowSlaOpeningExport = serde_json::from_str(opening_source)?;
    let target = shadow_utc_midnight(&sla.target_day_utc)?.timestamp();
    let end = target + 86_400;
    let through = chrono::DateTime::parse_from_rfc3339(&export.observed_through_utc)
        .map_err(|_| DecisionStoreError::Invalid)?;
    if export.queue_id != sla.queue_id
        || export.target_day_utc != sla.target_day_utc
        || through.offset().local_minus_utc() != 0
        || through.timestamp() != end
        || through.timestamp_subsec_nanos() != 0
        || opening.inputs != sla.inputs
    {
        return Err(DecisionStoreError::Invalid);
    }
    let opening_ids: std::collections::HashMap<&str, &str> = opening
        .opening_tickets
        .iter()
        .map(|ticket| (ticket.ticket_id.as_str(), ticket.created_at_utc.as_str()))
        .collect();
    let mut seen = std::collections::HashSet::new();
    let mut arrivals = 0_u64;
    let mut resolved = 0_u64;
    let mut within_sla = 0_u64;
    for ticket in &export.tickets {
        if ticket.queue_id.as_deref() != Some(sla.queue_id.as_str())
            || ticket.ticket_id.is_empty()
            || ticket.ticket_id.trim() != ticket.ticket_id
            || ticket.ticket_id.len() > 256
            || !seen.insert(ticket.ticket_id.as_str())
        {
            return Err(DecisionStoreError::Invalid);
        }
        let created = chrono::DateTime::parse_from_rfc3339(&ticket.created_at_utc)
            .map_err(|_| DecisionStoreError::Invalid)?;
        if created.offset().local_minus_utc() != 0 || created.timestamp() >= end {
            return Err(DecisionStoreError::Invalid);
        }
        if created.timestamp() < target {
            if opening_ids.get(ticket.ticket_id.as_str()).copied()
                != Some(ticket.created_at_utc.as_str())
            {
                return Err(DecisionStoreError::Invalid);
            }
        } else {
            if opening_ids.contains_key(ticket.ticket_id.as_str()) {
                return Err(DecisionStoreError::Invalid);
            }
            arrivals = arrivals.checked_add(1).ok_or(DecisionStoreError::Invalid)?;
        }
        if let Some(resolved_at) = &ticket.resolved_at_utc {
            let resolution = chrono::DateTime::parse_from_rfc3339(resolved_at)
                .map_err(|_| DecisionStoreError::Invalid)?;
            if resolution.offset().local_minus_utc() != 0
                || resolution.timestamp() < target
                || resolution.timestamp() >= end
                || resolution < created
            {
                return Err(DecisionStoreError::Invalid);
            }
            resolved = resolved.checked_add(1).ok_or(DecisionStoreError::Invalid)?;
            let age_days = (target - created.timestamp()).div_euclid(86_400)
                + i64::from((target - created.timestamp()).rem_euclid(86_400) != 0);
            if age_days < i64::from(model.sla_days) {
                within_sla = within_sla
                    .checked_add(1)
                    .ok_or(DecisionStoreError::Invalid)?;
            }
        }
    }
    let expected_len = usize::try_from(sla.inputs.known.opening_backlog)
        .map_err(|_| DecisionStoreError::Invalid)?;
    if seen.len()
        != expected_len + usize::try_from(arrivals).map_err(|_| DecisionStoreError::Invalid)?
        || opening_ids.keys().any(|id| !seen.contains(id))
        || arrivals != u64::from(aggregate.observed.arrivals)
        || resolved != u64::from(aggregate.observed.resolved)
        || sla
            .inputs
            .known
            .opening_backlog
            .checked_add(arrivals)
            .and_then(|stock| stock.checked_sub(resolved))
            != Some(aggregate.observed.backlog_end)
    {
        return Err(DecisionStoreError::Invalid);
    }
    Ok(within_sla)
}

/// Compare the exact ticket state at a scored day boundary. The next opening
/// source also carries the previous day's resolved rows for its frozen baselines.
pub(super) fn shadow_sla_day_boundary_matches(
    previous: &ShadowSlaObservationExport,
    opening: &ShadowSlaOpeningExport,
) -> Result<bool, DecisionStoreError> {
    let target = shadow_utc_midnight(&opening.inputs.target_day_utc)?.timestamp();
    if shadow_utc_midnight(&previous.target_day_utc)?.timestamp() != target - 86_400
        || previous.queue_id != opening.inputs.queue_id.as_deref().unwrap_or("")
    {
        return Ok(false);
    }
    let prior_open: std::collections::BTreeMap<_, _> = previous
        .tickets
        .iter()
        .filter(|ticket| ticket.resolved_at_utc.is_none())
        .map(|ticket| (&ticket.ticket_id, &ticket.created_at_utc))
        .collect();
    let next_open: std::collections::BTreeMap<_, _> = opening
        .opening_tickets
        .iter()
        .map(|ticket| (&ticket.ticket_id, &ticket.created_at_utc))
        .collect();
    let prior_resolved: std::collections::BTreeMap<_, _> = previous
        .tickets
        .iter()
        .filter(|ticket| ticket.resolved_at_utc.is_some())
        .map(|ticket| (&ticket.ticket_id, ticket))
        .collect();
    let mut next_prior_resolved = std::collections::BTreeMap::new();
    for ticket in &opening.prior_resolved_tickets {
        let resolved = chrono::DateTime::parse_from_rfc3339(
            ticket
                .resolved_at_utc
                .as_deref()
                .ok_or(DecisionStoreError::Invalid)?,
        )
        .map_err(|_| DecisionStoreError::Invalid)?
        .timestamp();
        if resolved >= target - 86_400 && resolved < target {
            next_prior_resolved.insert(&ticket.ticket_id, ticket);
        }
    }
    Ok(prior_open == next_open && prior_resolved == next_prior_resolved)
}

/// Validate an observed day against its frozen forecast before source ingest.
pub fn validate_shadow_observation_source(
    source: &str,
    forecast: &StoredShadowForecast,
) -> Result<ObservedSupportDay, DecisionStoreError> {
    let target = shadow_utc_midnight(&forecast.target_day_utc)?.timestamp();
    let export = parse_shadow_training_export(source)?;
    let (start, through) = shadow_export_window(&export)?;
    if start != target
        || through != target + 86_400
        || export.observed_days.len() != 1
        || export.queue_id != forecast.queue_id
    {
        return Err(DecisionStoreError::Invalid);
    }
    let observed = export
        .observed_days
        .into_iter()
        .next()
        .ok_or(DecisionStoreError::Invalid)?;
    if observed.backlog_start != forecast.known.opening_backlog
        || observed.agents != forecast.known.planned_agents
        || observed.fixed_extra_capacity != forecast.known.planned_fixed_extra_capacity
    {
        return Err(DecisionStoreError::Invalid);
    }
    Ok(observed)
}

pub(super) fn shadow_score(
    id: &str,
    forecast: &StoredShadowForecast,
    forecast_sha256: &str,
    observation_artifact_id: &str,
    observation_sha256: &str,
    scored_at: i64,
    observed: ObservedSupportDay,
) -> StoredShadowScore {
    StoredShadowScore {
        id: id.into(),
        forecast_id: forecast.id.clone(),
        forecast_sha256: forecast_sha256.into(),
        observation_artifact_id: observation_artifact_id.into(),
        observation_sha256: observation_sha256.into(),
        scored_at,
        arrivals_abs_error: forecast
            .forecast
            .predicted_arrivals
            .abs_diff(observed.arrivals) as u64,
        backlog_abs_error: forecast
            .forecast
            .predicted_backlog_end
            .abs_diff(observed.backlog_end),
        no_change_abs_error: forecast
            .forecast
            .no_change_backlog_end
            .abs_diff(observed.backlog_end),
        seasonal_naive_abs_error: forecast
            .forecast
            .seasonal_naive_backlog_end
            .abs_diff(observed.backlog_end),
        mean_change_abs_error: forecast
            .forecast
            .mean_change_backlog_end
            .abs_diff(observed.backlog_end),
        observed,
    }
}

