import { useAuthStore } from '@/stores/auth-store';

export interface DecisionArtifact {
  kind: string;
  id: string;
  created_at_unix: number;
  status?: string;
  candidate_id?: string;
  target_snapshot_id?: string;
  scenario_id?: string;
  outcome_id?: string;
  ticket_backed: boolean;
  replay_hash?: string;
}

export interface DecisionOverview {
  status: 'exploratory' | string;
  generated_at_utc: string;
  tenant_id: string;
  acl: string;
  counts: Record<string, number>;
  invalidated_inputs: number;
  ticket_sources: {
    active: number;
    expired: number;
    revoked: number;
    next_expiry_utc: string | null;
  };
  artifacts: DecisionArtifact[];
  limitations: string[];
}

export interface DecisionCatalog {
  snapshots: Array<{ id: string; queue_id: string | null; data_cutoff_utc: string; horizon_days: number; source_kinds: string[] }>;
  models: Array<{ id: string }>;
  scenarios: Array<{ id: string; horizon_days: number }>;
  synthetic_pilots: Array<{ snapshot_id: string; model_version: string; baseline_scenario_id: string; alternative_scenario_id: string }>;
  uploaded_pilots: Array<{ snapshot_id: string; model_version: string; baseline_scenario_id: string; alternative_scenario_id: string;
    source_sha256?: string; sla_holdout_id?: string | null; sla_holdout_sha256?: string | null }>;
}

export interface SupportPilotExport {
  snapshot_id: string;
  baseline_scenario_id: string;
  window_start_utc: string;
  data_cutoff_utc: string;
  source_version_hashes: string[];
  seed: number;
  horizon_days: number;
  tickets: Array<{ queue_id?: string | null; ticket_id: string; created_at_utc: string; resolved_at_utc: string | null }>;
  staffing: Array<{ queue_id?: string | null; day_utc: string; agents: number; fixed_extra_capacity: number }>;
}

export interface QueueModelInput {
  version: string;
  service_capacity_per_agent_day: number;
  sla_days: number;
  staff_cost_cents_per_agent_day: number;
}

export interface StaffingScenarioInput {
  id: string;
  agents_by_day: number[];
  fixed_extra_capacity_by_day: number[];
}

export interface DecisionPilotUploadInput {
  tenant_id: string;
  acl: string;
  expected_queue_id: string;
  source_lineage: string;
  retention_until_utc: string;
  export: SupportPilotExport;
  model: QueueModelInput;
  alternative_scenario: StaffingScenarioInput;
}

export interface DecisionPilotUploadReceipt {
  status: 'exploratory_operator_upload';
  queue_id: string;
  source_artifact_id: string;
  source_sha256: string;
  snapshot_id: string;
  model_version: string;
  baseline_scenario_id: string;
  alternative_scenario_id: string;
  baseline_replay_hash: string;
  alternative_replay_hash: string;
  sla_holdout_id?: string | null;
  sla_holdout_sha256?: string | null;
  sla_holdout_unavailable_reason?: string | null;
  limitations: string[];
}

/** Scenario totals and their delta. Decimal strings, not JSON numbers: a
 *  backlog, resolution count, or staff-cost sum accumulates over a
 *  caller-chosen horizon and can pass Number.MAX_SAFE_INTEGER, where `Number`
 *  silently rounds. Parse and compare with BigInt; render with
 *  `formatDecimal`. The delta is signed. */
export interface DecisionKpis {
  final_backlog: string;
  total_resolved: string;
  resolved_within_sla: string;
  total_staff_cost_cents: string;
}

export interface DecisionBrief {
  status: string;
  data_cutoff_utc?: string;
  source_version_hashes?: string[];
  horizon_days?: number;
  baseline: DecisionKpis;
  alternative: DecisionKpis;
  delta: DecisionKpis;
  replay: { baseline_replay_hash: string; alternative_replay_hash: string;
    baseline_command?: { program: string; args: string[] };
    alternative_command?: { program: string; args: string[] } };
  exploratory_empirical?: null | {
    run_id: string; run_sha256: string; fit_id: string; fit_sha256: string;
    source_origin?: 'synthetic_fixture' | 'operator_upload' | 'unclassified';
    report: { replay_hash: string; common_draws_sha256: string; runs: number;
      delta_backlog_p05: number; delta_backlog_p50: number; delta_backlog_p95: number;
      delta_sla_resolved_p05: number; delta_sla_resolved_p50: number; delta_sla_resolved_p95: number;
      baseline_joint_constraint_violation_bps: number;
      alternative_joint_constraint_violation_bps: number };
  };
  exploratory_event?: null | {
    source_sha256: string;
    window_start_utc: string;
    daily_engine_sha256: string;
    event_engine_sha256: string;
    config: { shift_start_seconds: number; shift_seconds: number };
    baseline_run_hash: string;
    baseline_run_sha256: string;
    alternative_run_hash: string;
    alternative_run_sha256: string;
    // Decimal strings for the same reason as DecisionKpis; null still means
    // "no resolved ticket", never zero.
    baseline: { final_backlog: string; total_resolved: string; resolved_within_sla: string;
      total_staff_cost_cents: string; resolved_wait_seconds_p50: string | null;
      resolved_wait_seconds_p95: string | null };
    alternative: { final_backlog: string; total_resolved: string; resolved_within_sla: string;
      total_staff_cost_cents: string; resolved_wait_seconds_p50: string | null;
      resolved_wait_seconds_p95: string | null };
    resolved_wait_seconds_p95_delta: string | null;
  };
  exploratory_sla_holdout?: null | {
    record_id: string;
    record_sha256: string;
    source_sha256: string;
    engine_sha256: string;
    // The same wire mirror POST /api/decision/engineering-validation returns
    // for this record: error sums are decimal strings (u128 on the server),
    // per-day counts stay JSON numbers.
    diagnostic: { training_days: number; holdout_days: number; observed_total_within_sla: number;
      predicted_total_within_sla: number; daily_abs_error_sum: string;
      no_change_abs_error_sum: string; seasonal_naive_abs_error_sum: string;
      training_mean_abs_error_sum: string;
      conditional_model_beats_all_baselines: boolean; limitations: string[] };
  };
  exploratory_forecast?: null | {
    record_id: string;
    record_sha256: string;
    source_sha256: string;
    calibration_engine_sha256: string;
    window_start_utc: string;
    calibration_points: number;
    forecast_points: number;
    // Decimal strings, the same wire type as the standalone forecast report
    // for this record. Compare and format with BigInt: these sums can pass
    // Number.MAX_SAFE_INTEGER, where a JSON number silently rounds.
    arrival_abs_error_sum: string;
    model_abs_error_sum: string;
    no_change_abs_error_sum: string;
    seasonal_naive_abs_error_sum: string;
    mean_change_abs_error_sum: string;
    model_beats_all_baselines: boolean;
    interval_evaluated_points: number | null;
    rolling_observed_coverage_basis_points: number | null;
    fixed_observed_coverage_basis_points: number | null;
  };
  exploratory_policy_screen?: null | {
    screen_hash: string; screen_record_sha256: string;
    report: { replay_hash: string; choice: string; failed_alternative_checks: string[] };
  };
  observational_effects?: Array<{
    estimate_id: string; model_id: string; data_artifact_id: string; data_sha256: string;
    data_ingested_at: number; method: string; identification_state: string; estimate: number;
    lower_bound: number; upper_bound: number; interval_kind: string; unit_count: number; strata_count: number;
    leave_one_stratum_out_sign_flip: boolean | null;
    leave_one_unit_out: { evaluated_units: number; skipped_due_positivity: number;
      max_abs_effect_shift: number | null; sign_flip: boolean | null };
    temporal_holdout: null | { cutoff: number; training_units: number; holdout_units: number;
      training_estimate: number; holdout_estimate: number; abs_gap: number; sign_flip: boolean };
    permutation_diagnostic: { method: string; runs: number; abs_at_least_observed: number;
      median_abs_placebo: number; max_abs_placebo: number };
    pre_treatment_placebo: null | { contrast: number; lower_bound: number; upper_bound: number; imbalance_flag: boolean };
    negative_control: null | { variable_id: string; review_id: string;
      contrast: number; lower_bound: number; upper_bound: number; imbalance_flag: boolean };
    treatment_variable_id: string; treatment_name: string; treatment_unit: string;
    outcome_variable_id: string; outcome_name: string; outcome_unit: string; population: string;
    window_start: number; window_end: number;
  }>;
  source_artifact_links: Array<{
    artifact_id: string;
    source_version_sha256: string;
    tenant_id?: string;
    acl?: string;
  }>;
  assumptions: string[];
  limitations: string[];
  uncertainty_interval: unknown | null;
}

export interface SimulationDay {
  day: number;
  arrivals: number;
  resolved: number;
  backlog_end: number;
  overdue_pending: number;
  resolved_within_sla: number;
  staff_cost_cents: number;
}

export interface SimulationResult {
  snapshot_id: string;
  scenario_id: string;
  model_version: string;
  replay_hash: string;
  days: SimulationDay[];
  total_arrivals: number;
  total_resolved: number;
  final_backlog: number;
  total_resolved_within_sla: number;
  total_staff_cost_cents: number;
}

export interface PolicySweepPoint {
  service_capacity_per_agent_day: number;
  baseline_final_backlog: number;
  alternative_final_backlog: number;
  baseline_staff_cost_cents: number;
  alternative_staff_cost_cents: number;
  baseline_feasible: boolean;
  alternative_feasible: boolean;
  choice: 'baseline' | 'alternative' | 'neither_feasible' | string;
}

export interface PolicySweepReport {
  status: string;
  replay_hash: string;
  points: PolicySweepPoint[];
  flips: Array<{ at_capacity_per_agent_day: number; from: string; to: string }>;
  decision_rule: string;
  limitations: string[];
}

export interface SensitivityReport {
  status: string;
  replay_hash: string;
  runs: number;
  delta_backlog_p05: number;
  delta_backlog_p50: number;
  delta_backlog_p95: number;
  alternative_lower_backlog_bps: number;
  baseline_backlog_violation_bps: number;
  alternative_backlog_violation_bps: number;
  baseline_cost_violation_bps: number;
  alternative_cost_violation_bps: number;
  worst_alternative_backlog: number;
  limitations: string[];
}

export interface DecisionEngineeringValidation {
  status: 'synthetic_only_exploratory' | string;
  generated_at_utc: string;
  snapshot_id: string;
  model_version: string;
  source_artifact_id: string;
  source_version_sha256: string;
  engineering_engine_sha256: string;
  simulation_engine_sha256: string;
  replay_hash: string;
  service_capacity_per_agent_day: number;
  initial_prefix_fit: null | { service_per_agent_day: number; observed_min: number; observed_max: number; saturated_days: number; training_days: number };
  initial_prefix_unavailable_reason: string | null;
  provided_model_matches_initial_prefix_fit: boolean | null;
  one_day_backtest: null | {
    evaluation_days: number;
    // Decimal strings: these sums can exceed Number.MAX_SAFE_INTEGER.
    arrival_abs_error_sum: string;
    model_abs_error_sum: string;
    no_change_abs_error_sum: string;
    seasonal_naive_abs_error_sum: string;
    mean_change_abs_error_sum: string;
    model_beats_all_baselines: boolean;
    points: Array<{ day_index: number; predicted_arrivals: number; actual_arrivals: number; predicted_backlog_end: number;
      actual_backlog_end: number; no_change_backlog_end: number; seasonal_naive_backlog_end: number;
      mean_change_backlog_end: number; fitted_capacity_per_agent: number }>;
  };
  one_day_backtest_unavailable_reason: string | null;
  fixed_interval_diagnostic: null | { method: string; target_coverage_basis_points: number; observed_coverage_basis_points: number;
    calibration_points: number; evaluated_points: number; recent_miss_count: number; recent_window_points: number;
    drift_signal: boolean; points: Array<{ day_index: number; predicted_backlog_end: number; actual_backlog_end: number;
      lower_bound: number; upper_bound: number; calibration_radius: number; covered: boolean }> };
  fixed_interval_unavailable_reason: string | null;
  ticket_sla_holdout: null | { method: string; source_sha256: string; model_version: string; training_days: number;
    holdout_days: number; sla_days: number; provided_model_capacity_per_agent_day: number; fitted_capacity_per_agent_day: number;
    provided_model_matches_fit: boolean; observed_total_within_sla: number; predicted_total_within_sla: number;
    daily_abs_error_sum: string; conditional_model_beats_all_baselines: boolean; limitations: string[] };
  ticket_sla_unavailable_reason: string | null;
  limitations: string[];
}

export interface DecisionShadowMonitor {
  status: 'descriptive_shadow_monitor';
  assessed_at_utc: string;
  policy: { id: string; queue_id: string | null; effective_from_utc: string; effective_until_utc: string };
  aggregate: ShadowMonitorAssessment;
  sla: ShadowMonitorAssessment;
  limitations: string[];
}

export interface ShadowMonitorAssessment {
  due_days: number;
  scored_days: number;
  corrected_days: number;
  complete: boolean;
  whole_window_skill: boolean | null;
  recent_skill: boolean | null;
  fixed_interval_available: boolean;
  drift_signal: boolean | null;
  status_counts: Record<string, number>;
}

export interface DecisionEmpiricalInput {
  tenant_id: string;
  acl: string;
  snapshot_id: string;
  model_version: string;
  baseline_scenario_id: string;
  alternative_scenario_id: string;
  training_days: number;
  min_saturated_days: number;
  runs: number;
  arrival_block_days: number;
  sampling_mode: 'independent' | 'paired_saturated_days';
  capacity_fallback_range?: { min: number; max: number };
  max_final_backlog: number;
  max_staff_cost_cents: number;
  min_sla_resolved?: number;
  screen?: {
    resource_plan: {
      available_agents_by_day: number[];
      max_added_agents_per_day: number;
      max_total_agent_days: number;
      max_staff_cost_cents: number;
      max_final_backlog: number;
      service_capacity_band: { min: number; max: number };
    };
    criteria: {
      max_joint_violation_bps: number;
      min_joint_recovery_bps: number;
      min_sla_improvement_bps: number;
    };
  };
}

export interface DecisionEmpiricalReport {
  status: 'synthetic_only_exploratory' | 'exploratory_operator_upload';
  fit: {
    id: string;
    training_days: number;
    min_saturated_days: number;
    capacity_identification: 'empirical_saturated_days' | 'unidentified';
    arrival_sample_count: number;
    capacity_sample_count: number;
    paired_saturated_day_count: number;
    unsaturated_lower_bound_day_count: number;
    partial_saturated_day_count: number;
    max_capacity_lower_bound: number;
  };
  run: {
    id: string;
    replay_hash: string;
    common_draws_sha256: string;
    runs: number;
    arrival_block_days: number;
    sampling_mode: 'independent' | 'paired_saturated_days';
    capacity_source: string;
    delta_backlog_p05: number;
    delta_backlog_p50: number;
    delta_backlog_p95: number;
    delta_sla_resolved_p05: number;
    delta_sla_resolved_p50: number;
    delta_sla_resolved_p95: number;
    baseline_joint_violation_bps: number;
    alternative_joint_violation_bps: number;
    alternative_joint_recovery_bps: number;
    alternative_sla_improvement_bps: number;
    worst_alternative_backlog: number;
    worst_alternative_sla_resolved: number;
  };
  screen: null | {
    choice: 'alternative_for_review' | 'baseline_only' | 'no_candidate';
    replay_hash: string;
    failed_alternative_checks: string[];
    baseline_deterministic_feasible: boolean;
    alternative_deterministic_feasible: boolean;
  };
  limitations: string[];
}

export interface DecisionEventComparison {
  status: 'synthetic_only_exploratory' | 'operator_supplied_exploratory';
  source_origin: 'synthetic' | 'uploaded';
  source_sha256: string;
  event_engine_sha256: string;
  baseline: DecisionEventScenario;
  alternative: DecisionEventScenario;
  limitations: string[];
}

export interface DecisionEventScenario {
  scenario_id: string;
  replay_hash: string;
  total_resolved_within_sla: number;
  wait_seconds_p50: number | null;
  wait_seconds_p95: number | null;
  final_backlog: number;
  total_staff_cost_cents: number;
}

export interface DecisionForecastValidation {
  status: 'synthetic_only_exploratory' | 'operator_supplied_exploratory';
  source_origin: 'synthetic' | 'uploaded';
  snapshot_id: string;
  model_version: string;
  baseline_scenario_id: string;
  alternative_scenario_id: string;
  source_sha256: string;
  min_training_days: number;
  min_saturated_days: number;
  calibration_points: number;
  record_id: string | null;
  record_sha256: string | null;
  forecast_evaluation_days: number | null;
  // Decimal strings: these sums can exceed Number.MAX_SAFE_INTEGER.
  arrival_abs_error_sum: string | null;
  model_abs_error_sum: string | null;
  no_change_abs_error_sum: string | null;
  seasonal_naive_abs_error_sum: string | null;
  mean_change_abs_error_sum: string | null;
  model_beats_all_baselines: boolean | null;
  rolling_interval_evaluated_points: number | null;
  rolling_observed_coverage_basis_points: number | null;
  fixed_interval_evaluated_points: number | null;
  fixed_observed_coverage_basis_points: number | null;
  unavailable_reason: string | null;
  interval_unavailable_reason: string | null;
  limitations: string[];
}

export interface DecisionCandidateModel {
  id: string;
  status: string;
  queue_id: string;
  parent_model_version: string;
  model: { version: string; service_capacity_per_agent_day: number; sla_days: number;
    staff_cost_cents_per_agent_day: number };
  observed_through_utc: string;
  screen_replay_hash: string;
  fit_id: string;
  outcome_id: string;
  source_version_hashes: string[];
  /// Digests the stored record carries; the list above is capped at 16.
  source_version_hashes_total: number;
  record_sha256: string;
  limitations: string[];
}

export interface DecisionOutcomeHoldout {
  training_days: number;
  holdout_days: number;
  candidate_abs_error_sum: string;
  parent_abs_error_sum: string;
  no_change_abs_error_sum: string;
  candidate_beats_parent: boolean;
  candidate_beats_no_change: boolean;
}

export interface DecisionOutcomeFit {
  id: string;
  outcome_id: string;
  replay_hash: string;
  observed_source_sha256: string;
  parent_model_version: string;
  parent_model_sha256: string;
  fit_engine_sha256: string;
  min_saturated_days: number;
  training_days: number;
  fit: { service_per_agent_day: number; observed_min: number; observed_max: number;
    saturated_days: number; training_days: number };
  holdout: DecisionOutcomeHoldout | null;
  record_sha256: string;
  /// Whether the installed simulator still carries the engine identity that
  /// produced the run at `replay_hash`. Stored runs are never recomputed on
  /// load, so `false` is the only signal that this fit scores an older engine.
  engine_matches_current: boolean;
  limitations: string[];
}

export interface DecisionOutcomeScreen {
  replay_hash: string;
  fit_id: string;
  fit_record_sha256: string;
  record_sha256: string;
  source_version_hashes: string[];
  /// Digests the stored record carries; the list above is capped at 16.
  source_version_hashes_total: number;
  /// Engine state of `report.replayed_run_hash`, recomputed on every read. It
  /// sits beside the report because the report is a frozen, hashed verdict.
  engine_matches_current: boolean;
  report: {
    status: string; replay_hash: string; review_engine_sha256: string;
    criteria: { min_saturated_days: number; min_holdout_days: number };
    fit_id: string; fit_record_sha256: string; outcome_id: string;
    replayed_run_hash: string; observed_source_sha256: string;
    parent_model_version: string; parent_model_sha256: string;
    fitted_capacity_per_agent_day: number; saturated_training_days: number;
    holdout: DecisionOutcomeHoldout | null; queue_id: string | null;
    local_run_precedes_window: boolean | null; eligible_for_human_review: boolean;
    failed_checks: string[]; limitations: string[];
  };
}

export interface DecisionOutcomeReview {
  link: { approval_id: string; agent_id: string; replay_hash: string;
    screen_record_sha256: string; fit_id: string; fit_record_sha256: string };
  status: 'pending' | 'approved' | 'denied' | 'expired';
  expires_at_utc: string;
  decided_by: string | null;
}

export interface DecisionCandidateRunKpis {
  model_version: string;
  replay_hash: string;
  // Decimal strings compared with BigInt: the panel re-derives the deltas
  // below from these, which a rounded JSON number would break.
  total_arrivals: string;
  total_resolved: string;
  final_backlog: string;
  total_resolved_within_sla: string;
  total_staff_cost_cents: string;
  /// Whether this stored simulation's engine digest is the installed one.
  engine_matches_current: boolean;
}

export interface DecisionCandidateRun {
  replay_hash: string;
  status: string;
  candidate_id: string;
  target_snapshot_id: string;
  scenario_id: string;
  record_sha256: string;
  candidate_record_sha256: string;
  target_snapshot_sha256: string;
  scenario_sha256: string;
  source_version_hashes: string[];
  /// Digests the stored record carries; the list above is capped at 16.
  source_version_hashes_total: number;
  parent: DecisionCandidateRunKpis;
  candidate: DecisionCandidateRunKpis;
  final_backlog_delta: string;
  resolved_within_sla_delta: string;
  limitations: string[];
}

export interface DecisionCandidateForecastErrors {
  days: number;
  arrivals_abs_error_sum: string;
  resolved_abs_error_sum: string;
  backlog_abs_error_sum: string;
  resolved_within_sla_abs_error_sum?: string | null;
}

export interface DecisionCandidateScore {
  replay_hash: string;
  status: string;
  comparison_run_hash: string;
  outcome_id: string;
  record_sha256: string;
  comparison_run_sha256: string;
  outcome_record_sha256: string;
  ticket_source_sha256: string | null;
  ticket_label_engine_sha256: string | null;
  ticket_source_retention_until_utc: string | null;
  candidate_created_at_unix: number;
  comparison_created_at_unix: number;
  observed_window_start_utc: string;
  source_version_hashes: string[];
  /// Digests the stored record carries; the list above is capped at 16.
  source_version_hashes_total: number;
  parent: DecisionCandidateForecastErrors;
  candidate: DecisionCandidateForecastErrors;
  candidate_backlog_error_below_parent: boolean;
  candidate_resolution_error_below_parent: boolean;
  candidate_sla_error_below_parent: boolean | null;
  limitations: string[];
}

export interface DecisionShadowPolicy {
  id: string; source_lineage: string; queue_id: string | null;
  effective_from_utc: string; effective_until_utc: string;
  issue_deadline_seconds: number; min_training_days: number; min_saturated_days: number;
  calibration_engine_sha256: string; registered_at: number; record_sha256: string;
  limitations: string[];
}

export interface DecisionShadowForecast {
  id: string; policy_id: string; policy_sha256: string;
  training_artifact_id: string; training_sha256: string;
  source_lineage: string; queue_id: string | null; training_window_start_utc: string;
  target_day_utc: string; committed_at: number; min_saturated_days: number;
  known: { opening_backlog: string; planned_agents: number; planned_fixed_extra_capacity: number };
  calibration_engine_sha256: string;
  forecast: { predicted_arrivals: number; predicted_backlog_end: string;
    no_change_backlog_end: string; seasonal_naive_backlog_end: string;
    mean_change_backlog_end: string; fitted_capacity_per_agent: number; training_days: number };
  record_sha256: string; limitations: string[];
}

export interface DecisionShadowScore {
  id: string; forecast_id: string; forecast_sha256: string;
  observation_artifact_id: string; observation_sha256: string; scored_at: number;
  arrivals_abs_error: string; backlog_abs_error: string; no_change_abs_error: string;
  seasonal_naive_abs_error: string; mean_change_abs_error: string;
  record_sha256: string; limitations: string[];
}

export interface DecisionShadowErrorSums {
  arrivals: string; backlog: string; no_change_backlog: string;
  seasonal_naive_backlog: string; mean_change_backlog: string;
}

export interface DecisionShadowAssessment {
  policy_id: string; policy_sha256: string; source_lineage: string; queue_id: string | null;
  assessed_at_utc: string; due_days: number; scored_days: number; corrected_days: number;
  complete: boolean; error_sums: DecisionShadowErrorSums | null;
  backlog_abs_error_below_each_baseline: boolean | null;
  fixed_prefix_interval: null | { method: string; target_coverage_basis_points: number;
    observed_coverage_basis_points: number; calibration_points: number; evaluated_points: number;
    recent_miss_count: number; recent_window_points: number; drift_signal: boolean;
    covered_points: number };
  recent_7_day_error_sums: DecisionShadowErrorSums | null;
  recent_7_day_backlog_abs_error_below_each_baseline: boolean | null;
  day_status_counts: Record<string, number>; limitations: string[];
}

export interface DecisionShadowScreen {
  replay_hash: string; policy_id: string; policy_sha256: string;
  record_sha256: string | null; source_version_hashes: string[];
  /// Digests the stored record carries; the list above is capped at 16.
  source_version_hashes_total: number;
  report: { status: string; screen_engine_sha256: string;
    criteria: { min_complete_days: number; min_fixed_coverage_bps: number };
    assessment: DecisionShadowAssessment;
    coverage_evidence: null | { covered_days: number; evaluated_days: number; minimum_covered_days: number };
    failed_checks: string[]; eligible_for_human_review: boolean; limitations: string[] };
}

export interface DecisionShadowReview {
  link: { approval_id: string; agent_id: string; replay_hash: string;
    screen_record_sha256: string; policy_id: string; policy_sha256: string };
  status: 'pending' | 'approved' | 'denied' | 'expired';
  expires_at_utc: string; decided_by: string | null;
}

export interface DecisionShadowSlaOpening {
  opening_backlog: string; opening_cohort_count: number; prior_resolved_ticket_count: number;
  planned_agents: number; planned_fixed_extra_capacity: number;
}

export interface DecisionShadowSlaForecast {
  id: string; forecast_id: string; forecast_sha256: string; policy_id: string;
  model_version: string; model_sha256: string;
  opening_artifact_id: string; opening_sha256: string; source_lineage: string;
  queue_id: string; target_day_utc: string; committed_at: number;
  opening: DecisionShadowSlaOpening;
  prediction: { predicted_resolved_within_sla: number; predicted_arrivals: number;
    predicted_backlog_end: string; fitted_capacity_per_agent: number; training_days: number };
  baselines: { no_change: number; seasonal_naive: number; seven_day_mean: number };
  daily_engine_sha256: string; record_sha256: string; limitations: string[];
}

export interface DecisionShadowSlaScore {
  id: string; sla_forecast_id: string; sla_forecast_sha256: string;
  aggregate_score_id: string; aggregate_score_sha256: string;
  observation_artifact_id: string; observation_sha256: string; scored_at: number;
  predicted_resolved_within_sla: string; observed_resolved_within_sla: string;
  abs_error: string; no_change_abs_error: string; seasonal_naive_abs_error: string;
  seven_day_mean_abs_error: string; correction_revision: boolean;
  record_sha256: string; limitations: string[];
}

export interface DecisionShadowSlaErrorSums {
  model: string; no_change: string; seasonal_naive: string; seven_day_mean: string;
}

export interface DecisionShadowSlaAssessment {
  policy_id: string; policy_sha256: string; source_lineage: string; queue_id: string | null;
  assessed_at_utc: string; due_days: number; scored_days: number; corrected_days: number;
  complete: boolean; error_sums: DecisionShadowSlaErrorSums | null;
  model_abs_error_below_each_baseline: boolean | null;
  fixed_prefix_interval: null | { method: string; target_coverage_basis_points: number;
    observed_coverage_basis_points: number; calibration_points: number; evaluated_points: number;
    calibration_radius: string; recent_miss_count: number; recent_window_points: number;
    drift_signal: boolean; covered_points: number };
  recent_7_day_error_sums: DecisionShadowSlaErrorSums | null;
  recent_7_day_model_abs_error_below_each_baseline: boolean | null;
  total_predicted_resolved_within_sla: string | null;
  total_observed_resolved_within_sla: string | null;
  day_status_counts: Record<string, number>; limitations: string[];
}

export interface DecisionShadowSlaScreen {
  replay_hash: string; policy_id: string; policy_sha256: string;
  record_sha256: string | null; source_version_hashes: string[];
  report: { status: string; screen_engine_sha256: string;
    criteria: { min_complete_days: number; min_fixed_coverage_bps: number };
    assessment: DecisionShadowSlaAssessment;
    coverage_evidence: null | { covered_days: number; evaluated_days: number; minimum_covered_days: number };
    failed_checks: string[]; eligible_for_human_review: boolean; limitations: string[] };
}

export interface DecisionShadowSlaReview {
  link: { approval_id: string; agent_id: string; replay_hash: string;
    screen_record_sha256: string; policy_id: string; policy_sha256: string };
  status: 'pending' | 'approved' | 'denied' | 'expired';
  expires_at_utc: string; decided_by: string | null;
}

export interface PilotReviewStatus {
  link: { approval_id: string; agent_id: string; replay_hash: string; snapshot_id: string; scenario_id: string };
  status: 'pending' | 'approved' | 'denied' | 'expired';
  expires_at_utc: string;
  decided_by: string | null;
}

export interface PilotReviewSelection {
  tenant_id: string;
  acl: string;
  snapshot_id: string;
  model_version: string;
  scenario_id: string;
  expected_hash: string;
}

export type SyntheticLifecycleKind = 'acl_lost' | 'deleted' | 'quarantined' | 'version_changed';
export type SyntheticLifecycleStageOutcome = 'staged' | 'already_staged' | 'already_completed';

async function request<T>(path: string, body?: object): Promise<T> {
  const jwt = useAuthStore.getState().jwt;
  const response = await fetch(path, {
    method: body ? 'POST' : 'GET',
    headers: {
      ...(jwt ? { Authorization: `Bearer ${jwt}` } : {}),
      ...(body ? { 'Content-Type': 'application/json' } : {}),
    },
    ...(body ? { body: JSON.stringify(body) } : {}),
  });
  if (!response.ok) {
    const payload: unknown = await response.json().catch(() => null);
    const detail = payload && typeof payload === 'object' && 'error' in payload && typeof payload.error === 'string'
      ? payload.error : `HTTP ${response.status}`;
    throw new Error(response.status === 409 ? `資料已變更，請重新載入：${detail}` : detail);
  }
  return response.json() as Promise<T>;
}

export const decisionApi = {
  overview: (scope: { tenant_id: string; acl: string }, limit = 50) => {
    const query = new URLSearchParams({ ...scope, limit: String(limit) });
    return request<DecisionOverview>(`/api/decision/overview?${query}`);
  },
  scrubTicketSources: (scope: { tenant_id: string; acl: string }) =>
    request<{ scrubbed: number; overview: DecisionOverview }>(
      '/api/decision/ticket-sources/scrub', scope,
    ),
  catalog: (scope: { tenant_id: string; acl: string }) => {
    const query = new URLSearchParams(scope);
    return request<DecisionCatalog>(`/api/decision/catalog?${query}`);
  },
  shadowMonitor: (scope: { tenant_id: string; acl: string }, policyId: string) => {
    const query = new URLSearchParams({ ...scope, policy_id: policyId });
    return request<DecisionShadowMonitor>(`/api/decision/shadow-monitor?${query}`);
  },
  createSyntheticPilot: (scope: { tenant_id: string; acl: string }, input: { seed: number; days: number }) =>
    request<{ snapshot_id: string; model_version: string; baseline_scenario_id: string;
      alternative_scenario_id: string; source_artifact_id: string }>(
      '/api/decision/synthetic-pilot', { ...scope, ...input },
    ),
  importPilot: (input: DecisionPilotUploadInput) =>
    request<DecisionPilotUploadReceipt>('/api/decision/import-pilot', input),
  stageSyntheticLifecycle: (scope: { tenant_id: string; acl: string }, input: {
    seed: number; days: number; kind: SyntheticLifecycleKind,
  }) => request<{ stage_outcome: SyntheticLifecycleStageOutcome }>(
    '/api/decision/synthetic-pilot/lifecycle', { ...scope, ...input },
  ),
  compare: (input: { tenant_id: string; acl: string; snapshot_id: string; model_version: string;
    baseline_scenario_id: string; alternative_scenario_id: string;
    empirical_run_id?: string; policy_screen_hash?: string; effect_ids?: string[];
    baseline_event_replay_hash?: string; alternative_event_replay_hash?: string;
    sla_holdout_id?: string; forecast_validation_id?: string }) =>
    request<{ brief: DecisionBrief }>('/api/decision/compare', input),
  replay: (input: { tenant_id: string; acl: string; snapshot_id: string; model_version: string;
    scenario_id: string; expected_hash: string }) =>
    request<{ result: SimulationResult }>('/api/decision/replay', input),
  eventCompare: (input: { tenant_id: string; acl: string; snapshot_id: string; model_version: string;
    baseline_scenario_id: string; alternative_scenario_id: string }) =>
    request<{ report: DecisionEventComparison }>('/api/decision/event-compare', input),
  forecastValidation: (input: { tenant_id: string; acl: string; snapshot_id: string; model_version: string;
    baseline_scenario_id: string; alternative_scenario_id: string }) =>
    request<{ report: DecisionForecastValidation }>('/api/decision/forecast-validation', input),
  createOutcomeFit: (input: { tenant_id: string; acl: string; fit_id: string; outcome_id: string;
    min_saturated_days: number; training_days: number }) =>
    request<{ fit: DecisionOutcomeFit }>('/api/decision/outcome-fit/create', input),
  loadOutcomeFit: (input: { tenant_id: string; acl: string; fit_id: string }) =>
    request<{ fit: DecisionOutcomeFit }>('/api/decision/outcome-fit/load', input),
  saveOutcomeScreen: (input: { tenant_id: string; acl: string; fit_id: string;
    min_saturated_days: number; min_holdout_days: number }) =>
    request<{ screen: DecisionOutcomeScreen }>('/api/decision/outcome-screen/save', input),
  loadOutcomeScreen: (input: { tenant_id: string; acl: string; replay_hash: string }) =>
    request<{ screen: DecisionOutcomeScreen }>('/api/decision/outcome-screen/load', input),
  requestOutcomeReview: (input: { tenant_id: string; acl: string; replay_hash: string;
    summary: string; ttl_seconds: number }) =>
    request<{ review: DecisionOutcomeReview }>('/api/decision/outcome-screen/review/request', input),
  outcomeReviewStatus: (input: { tenant_id: string; acl: string; replay_hash: string; approval_id: string }) =>
    request<{ review: DecisionOutcomeReview }>('/api/decision/outcome-screen/review/status', input),
  createShadowPolicy: (input: { tenant_id: string; acl: string; policy_id: string; source_lineage: string;
    queue_id: string; effective_from_utc: string; effective_until_utc: string;
    issue_deadline_seconds: number; min_training_days: number; min_saturated_days: number }) =>
    request<{ policy: DecisionShadowPolicy }>('/api/decision/shadow-policy/create', input),
  loadShadowPolicy: (input: { tenant_id: string; acl: string; policy_id: string }) =>
    request<{ policy: DecisionShadowPolicy }>('/api/decision/shadow-policy/load', input),
  createShadowForecast: (input: { tenant_id: string; acl: string; forecast_id: string; policy_id: string;
    target_day_utc: string; known: { opening_backlog: number; planned_agents: number;
      planned_fixed_extra_capacity: number };
    training_artifact_id?: string; training_source_json?: string; retention_until_utc?: string }) =>
    request<{ forecast: DecisionShadowForecast }>('/api/decision/shadow-forecast/create', input),
  loadShadowForecast: (input: { tenant_id: string; acl: string; forecast_id: string }) =>
    request<{ forecast: DecisionShadowForecast }>('/api/decision/shadow-forecast/load', input),
  createShadowScore: (input: { tenant_id: string; acl: string; score_id: string; forecast_id: string;
    observation_artifact_id?: string; observation_source_json?: string; retention_until_utc?: string }) =>
    request<{ score: DecisionShadowScore }>('/api/decision/shadow-score/create', input),
  loadShadowScore: (input: { tenant_id: string; acl: string; score_id: string }) =>
    request<{ score: DecisionShadowScore }>('/api/decision/shadow-score/load', input),
  assessShadowPolicy: (input: { tenant_id: string; acl: string; policy_id: string }) =>
    request<{ assessment: DecisionShadowAssessment }>('/api/decision/shadow-policy/assess', input),
  evaluateShadowScreen: (input: { tenant_id: string; acl: string; policy_id: string;
    min_complete_days: number; min_fixed_coverage_bps: number }) =>
    request<{ screen: DecisionShadowScreen }>('/api/decision/shadow-screen/evaluate', input),
  saveShadowScreen: (input: { tenant_id: string; acl: string; policy_id: string;
    min_complete_days: number; min_fixed_coverage_bps: number }) =>
    request<{ screen: DecisionShadowScreen }>('/api/decision/shadow-screen/save', input),
  loadShadowScreen: (input: { tenant_id: string; acl: string; replay_hash: string }) =>
    request<{ screen: DecisionShadowScreen }>('/api/decision/shadow-screen/load', input),
  requestShadowReview: (input: { tenant_id: string; acl: string; replay_hash: string;
    summary: string; ttl_seconds: number }) =>
    request<{ review: DecisionShadowReview }>('/api/decision/shadow-screen/review/request', input),
  shadowReviewStatus: (input: { tenant_id: string; acl: string; replay_hash: string; approval_id: string }) =>
    request<{ review: DecisionShadowReview }>('/api/decision/shadow-screen/review/status', input),
  createShadowSlaForecast: (input: { tenant_id: string; acl: string; sla_id: string; forecast_id: string;
    model_version: string; opening_artifact_id?: string; opening_source_json?: string;
    retention_until_utc?: string }) =>
    request<{ sla_forecast: DecisionShadowSlaForecast }>('/api/decision/shadow-sla-forecast/create', input),
  loadShadowSlaForecast: (input: { tenant_id: string; acl: string; sla_id: string }) =>
    request<{ sla_forecast: DecisionShadowSlaForecast }>('/api/decision/shadow-sla-forecast/load', input),
  createShadowSlaScore: (input: { tenant_id: string; acl: string; score_id: string; sla_forecast_id: string;
    aggregate_score_id: string; observation_artifact_id?: string; observation_source_json?: string;
    retention_until_utc?: string }) =>
    request<{ sla_score: DecisionShadowSlaScore }>('/api/decision/shadow-sla-score/create', input),
  loadShadowSlaScore: (input: { tenant_id: string; acl: string; score_id: string }) =>
    request<{ sla_score: DecisionShadowSlaScore }>('/api/decision/shadow-sla-score/load', input),
  loadCurrentShadowSlaScore: (input: { tenant_id: string; acl: string; sla_forecast_id: string }) =>
    request<{ sla_score: DecisionShadowSlaScore }>('/api/decision/shadow-sla-score/load-current', input),
  assessShadowSlaPolicy: (input: { tenant_id: string; acl: string; policy_id: string }) =>
    request<{ assessment: DecisionShadowSlaAssessment }>('/api/decision/shadow-sla-policy/assess', input),
  evaluateSlaShadowScreen: (input: { tenant_id: string; acl: string; policy_id: string;
    min_complete_days: number; min_fixed_coverage_bps: number }) =>
    request<{ screen: DecisionShadowSlaScreen }>('/api/decision/shadow-sla-screen/evaluate', input),
  saveSlaShadowScreen: (input: { tenant_id: string; acl: string; policy_id: string;
    min_complete_days: number; min_fixed_coverage_bps: number }) =>
    request<{ screen: DecisionShadowSlaScreen }>('/api/decision/shadow-sla-screen/save', input),
  loadSlaShadowScreen: (input: { tenant_id: string; acl: string; replay_hash: string }) =>
    request<{ screen: DecisionShadowSlaScreen }>('/api/decision/shadow-sla-screen/load', input),
  requestSlaShadowReview: (input: { tenant_id: string; acl: string; replay_hash: string;
    summary: string; ttl_seconds: number }) =>
    request<{ review: DecisionShadowSlaReview }>('/api/decision/shadow-sla-screen/review/request', input),
  slaShadowReviewStatus: (input: { tenant_id: string; acl: string; replay_hash: string; approval_id: string }) =>
    request<{ review: DecisionShadowSlaReview }>('/api/decision/shadow-sla-screen/review/status', input),
  createCandidateModel: (input: { tenant_id: string; acl: string; candidate_id: string; screen_replay_hash: string }) =>
    request<{ candidate: DecisionCandidateModel }>('/api/decision/model-candidate/create', input),
  loadCandidateModel: (input: { tenant_id: string; acl: string; candidate_id: string }) =>
    request<{ candidate: DecisionCandidateModel }>('/api/decision/model-candidate/load', input),
  compareCandidateModel: (input: { tenant_id: string; acl: string; candidate_id: string;
    target_snapshot_id: string; scenario_id: string }) =>
    request<{ run: DecisionCandidateRun }>('/api/decision/model-candidate/compare', input),
  loadCandidateRun: (input: { tenant_id: string; acl: string; replay_hash: string }) =>
    request<{ run: DecisionCandidateRun }>('/api/decision/model-candidate/load-run', input),
  scoreCandidateModel: (input: { tenant_id: string; acl: string; comparison_run_hash: string; outcome_id: string }) =>
    request<{ score: DecisionCandidateScore }>('/api/decision/model-candidate/score', input),
  loadCandidateScore: (input: { tenant_id: string; acl: string; replay_hash: string }) =>
    request<{ score: DecisionCandidateScore }>('/api/decision/model-candidate/load-score', input),
  policySweep: (input: { tenant_id: string; acl: string; snapshot_id: string; model_version: string;
    baseline_scenario_id: string; alternative_scenario_id: string; resource_plan: {
      available_agents_by_day: number[]; max_added_agents_per_day: number; max_total_agent_days: number;
      max_staff_cost_cents: number; max_final_backlog: number; service_capacity_band: { min: number; max: number };
    } }) => request<{ report: PolicySweepReport }>('/api/decision/policy-sweep', input),
  sensitivity: (input: { tenant_id: string; acl: string; snapshot_id: string; model_version: string;
    baseline_scenario_id: string; alternative_scenario_id: string; runs: number; arrival_delta: number;
    capacity_min: number; capacity_max: number; max_final_backlog: number; max_staff_cost_cents: number }) =>
    request<{ report: SensitivityReport }>('/api/decision/sensitivity', input),
  engineeringValidation: (input: { tenant_id: string; acl: string; snapshot_id: string; model_version: string }) =>
    request<{ report: DecisionEngineeringValidation }>('/api/decision/engineering-validation', input),
  empiricalResampling: (input: DecisionEmpiricalInput) =>
    request<DecisionEmpiricalReport>('/api/decision/empirical-resampling', input),
  requestPilotReview: (input: PilotReviewSelection & { summary: string; ttl_seconds: number }) =>
    request<{ review: PilotReviewStatus }>('/api/decision/pilot-review/request', input),
  pilotReviewStatus: (input: PilotReviewSelection & { approval_id: string }) =>
    request<{ review: PilotReviewStatus }>('/api/decision/pilot-review/status', input),
};
