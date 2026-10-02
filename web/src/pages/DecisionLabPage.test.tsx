import { beforeEach, describe, expect, it, vi } from 'vitest';
import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { IntlProvider } from 'react-intl';
import { MemoryRouter, Routes, Route, useNavigate } from 'react-router';
import { renderWithProviders } from '@/test/render';
import en from '@/i18n/en.json';
import { decisionApi, type DecisionOverview, type DecisionPilotUploadReceipt } from '@/lib/decision-api';
import { useSystemStore } from '@/stores/system-store';
import { DecisionLabPage } from './DecisionLabPage';
import { pilotReviewDeepLink } from '@/lib/decision-review-link';

vi.mock('@/lib/decision-api', () => ({ decisionApi: {
  overview: vi.fn(), scrubTicketSources: vi.fn(), catalog: vi.fn(), createSyntheticPilot: vi.fn(),
  importPilot: vi.fn(),
  stageSyntheticLifecycle: vi.fn(),
  shadowMonitor: vi.fn(),
  compare: vi.fn(), replay: vi.fn(), eventCompare: vi.fn(), forecastValidation: vi.fn(), policySweep: vi.fn(), sensitivity: vi.fn(),
  createCandidateModel: vi.fn(), loadCandidateModel: vi.fn(), compareCandidateModel: vi.fn(),
  loadCandidateRun: vi.fn(), scoreCandidateModel: vi.fn(), loadCandidateScore: vi.fn(),
  createOutcomeFit: vi.fn(), loadOutcomeFit: vi.fn(), saveOutcomeScreen: vi.fn(),
  loadOutcomeScreen: vi.fn(), requestOutcomeReview: vi.fn(), outcomeReviewStatus: vi.fn(),
  createShadowPolicy: vi.fn(), loadShadowPolicy: vi.fn(), createShadowForecast: vi.fn(), loadShadowForecast: vi.fn(),
  createShadowScore: vi.fn(), loadShadowScore: vi.fn(), assessShadowPolicy: vi.fn(),
  evaluateShadowScreen: vi.fn(), saveShadowScreen: vi.fn(), loadShadowScreen: vi.fn(),
  requestShadowReview: vi.fn(), shadowReviewStatus: vi.fn(),
  createShadowSlaForecast: vi.fn(), loadShadowSlaForecast: vi.fn(), createShadowSlaScore: vi.fn(),
  loadShadowSlaScore: vi.fn(), loadCurrentShadowSlaScore: vi.fn(), assessShadowSlaPolicy: vi.fn(),
  evaluateSlaShadowScreen: vi.fn(), saveSlaShadowScreen: vi.fn(), loadSlaShadowScreen: vi.fn(),
  requestSlaShadowReview: vi.fn(), slaShadowReviewStatus: vi.fn(),
  empiricalResampling: vi.fn(), engineeringValidation: vi.fn(), requestPilotReview: vi.fn(), pilotReviewStatus: vi.fn(),
} }));

const catalog = {
  snapshots: [{ id: 'snapshot-1', queue_id: 'queue-1', data_cutoff_utc: '2026-01-01T00:00:00Z', horizon_days: 2,
    source_kinds: ['synthetic_support_export'] }],
  models: [{ id: 'model-1' }],
  scenarios: [{ id: 'scenario-base', horizon_days: 2 }, { id: 'scenario-alt', horizon_days: 2 }],
  synthetic_pilots: [{ snapshot_id: 'snapshot-1', model_version: 'model-1', baseline_scenario_id: 'scenario-base', alternative_scenario_id: 'scenario-alt' }],
  uploaded_pilots: [],
};
const uploadedReceipt: DecisionPilotUploadReceipt = {
  status: 'exploratory_operator_upload', queue_id: 'operator-queue',
  source_artifact_id: 'operator-artifact', source_sha256: 'a'.repeat(64),
  snapshot_id: 'uploaded-snapshot', model_version: 'uploaded-model',
  baseline_scenario_id: 'uploaded-baseline', alternative_scenario_id: 'uploaded-alternative',
  baseline_replay_hash: 'b'.repeat(64), alternative_replay_hash: 'c'.repeat(64),
  sla_holdout_id: null, sla_holdout_sha256: null,
  sla_holdout_unavailable_reason: 'Capacity unidentified',
  limitations: ['Operator source identity is unverified.'],
};
const uploadedCatalog = {
  snapshots: [{ id: 'uploaded-snapshot', queue_id: 'operator-queue', data_cutoff_utc: '2026-01-03T00:00:00Z',
    horizon_days: 2, source_kinds: ['local_support_pilot_export'] }],
  models: [{ id: 'uploaded-model' }],
  scenarios: [{ id: 'uploaded-baseline', horizon_days: 2 }, { id: 'uploaded-alternative', horizon_days: 2 }],
  synthetic_pilots: [],
  uploaded_pilots: [{ snapshot_id: 'uploaded-snapshot', model_version: 'uploaded-model',
    baseline_scenario_id: 'uploaded-baseline', alternative_scenario_id: 'uploaded-alternative' }],
};

async function fillPilotUpload(user: ReturnType<typeof userEvent.setup>) {
  const exportJson = { snapshot_id: 'uploaded-snapshot', baseline_scenario_id: 'uploaded-baseline',
    window_start_utc: '2026-01-01T00:00:00Z', data_cutoff_utc: '2026-01-03T00:00:00Z',
    source_version_hashes: ['a'.repeat(64)], seed: 0, horizon_days: 2,
    tickets: [{ queue_id: 'operator-queue', ticket_id: 'PRIVATE-CUSTOMER-TICKET',
      created_at_utc: '2026-01-01T00:00:00Z', resolved_at_utc: null }],
    staffing: [{ queue_id: 'operator-queue', day_utc: '2026-01-01T00:00:00Z', agents: 1, fixed_extra_capacity: 0 },
      { queue_id: 'operator-queue', day_utc: '2026-01-02T00:00:00Z', agents: 1, fixed_extra_capacity: 0 }],
  };
  await user.upload(screen.getByLabelText('Ticket and staffing export JSON'),
    new File([JSON.stringify(exportJson)], 'private-customer-export.json', { type: 'application/json' }));
  await user.upload(screen.getByLabelText('Queue model JSON'),
    new File([JSON.stringify({ version: 'uploaded-model', service_capacity_per_agent_day: 8,
      sla_days: 2, staff_cost_cents_per_agent_day: 10_000 })], 'model.json', { type: 'application/json' }));
  await user.upload(screen.getByLabelText('Alternative staffing scenario JSON'),
    new File([JSON.stringify({ id: 'uploaded-alternative', agents_by_day: [2, 2],
      fixed_extra_capacity_by_day: [0, 0] })], 'alternative.json', { type: 'application/json' }));
  await user.type(screen.getByRole('textbox', { name: 'Expected queue ID' }), 'operator-queue');
  await user.type(screen.getByRole('textbox', { name: 'Source lineage' }), 'operator-export');
  await user.type(screen.getByRole('textbox', { name: 'Source retention until (UTC)' }), '2099-01-01T00:00:00Z');
}
const brief = {
  // Decimal strings on the wire: these totals accumulate over the horizon and
  // a JSON number would round past 2^53 in the browser.
  status: 'exploratory', baseline: { final_backlog: '12', total_resolved: '20', resolved_within_sla: '15', total_staff_cost_cents: '5000' },
  alternative: { final_backlog: '8', total_resolved: '24', resolved_within_sla: '19', total_staff_cost_cents: '7000' },
  delta: { final_backlog: '-4', total_resolved: '4', resolved_within_sla: '4', total_staff_cost_cents: '2000' },
  replay: { baseline_replay_hash: 'base-hash', alternative_replay_hash: 'alt-hash' },
  source_artifact_links: [{ artifact_id: 'source-1', source_version_sha256: 'source-sha' }],
  assumptions: ['Synthetic queue arrivals'], limitations: ['Not operational evidence'], uncertainty_interval: null,
};
const simDay = (day: number, backlog: number) => ({ day, arrivals: 10, resolved: 11, backlog_end: backlog,
  overdue_pending: 1, resolved_within_sla: 8, staff_cost_cents: 1000 });
const policyReport = {
  status: 'exploratory', replay_hash: 'policy-hash', decision_rule: 'Alternative if feasible with lower backlog.',
  flips: [{ at_capacity_per_agent_day: 5, from: 'neither_feasible', to: 'alternative' }],
  points: [{ service_capacity_per_agent_day: 5, baseline_final_backlog: 12, alternative_final_backlog: 8,
    baseline_staff_cost_cents: 5000, alternative_staff_cost_cents: 7000, baseline_feasible: false,
    alternative_feasible: true, choice: 'alternative' }], limitations: ['Synthetic assumptions only.'],
};
const sensitivityReport = {
  status: 'exploratory', replay_hash: 'sensitivity-hash', runs: 100,
  delta_backlog_p05: -8, delta_backlog_p50: -4, delta_backlog_p95: 2,
  alternative_lower_backlog_bps: 7000, baseline_backlog_violation_bps: 2000,
  alternative_backlog_violation_bps: 500, baseline_cost_violation_bps: 1000,
  alternative_cost_violation_bps: 3000, worst_alternative_backlog: 17, limitations: ['Bands are operator assumptions.'],
};
const engineeringReport = {
  status: 'synthetic_only_exploratory', generated_at_utc: '2026-09-27T00:00:00Z', snapshot_id: 'snapshot-1',
  model_version: 'model-1', source_artifact_id: 'synthetic-source-1', source_version_sha256: 'a'.repeat(64),
  engineering_engine_sha256: 'c'.repeat(64),
  simulation_engine_sha256: 'd'.repeat(64),
  replay_hash: 'b'.repeat(64),
  service_capacity_per_agent_day: 8,
  initial_prefix_fit: { service_per_agent_day: 8, observed_min: 8, observed_max: 8, saturated_days: 7, training_days: 7 },
  initial_prefix_unavailable_reason: null,
  provided_model_matches_initial_prefix_fit: true,
  one_day_backtest: { evaluation_days: 7, arrival_abs_error_sum: '3', model_abs_error_sum: '7', no_change_abs_error_sum: '14',
    seasonal_naive_abs_error_sum: '10', mean_change_abs_error_sum: '12', model_beats_all_baselines: true,
    points: [{ day_index: 14, predicted_arrivals: 20, actual_arrivals: 21, predicted_backlog_end: 40, actual_backlog_end: 41,
      no_change_backlog_end: 45, seasonal_naive_backlog_end: 42, mean_change_backlog_end: 44, fitted_capacity_per_agent: 8 }] },
  one_day_backtest_unavailable_reason: null,
  fixed_interval_diagnostic: { method: 'fixed_prefix_absolute_residual_rank90_v1', target_coverage_basis_points: 9000,
    observed_coverage_basis_points: 8000, calibration_points: 14, evaluated_points: 5, recent_miss_count: 3,
    recent_window_points: 5, drift_signal: true, points: [] }, fixed_interval_unavailable_reason: null,
  ticket_sla_holdout: { method: 'training_prefix_fifo_day_sla_holdout_v3', source_sha256: 'a'.repeat(64), model_version: 'model-1',
    training_days: 7, holdout_days: 23, sla_days: 2, provided_model_capacity_per_agent_day: 8,
    fitted_capacity_per_agent_day: 8, provided_model_matches_fit: true, observed_total_within_sla: 20,
    predicted_total_within_sla: 18, daily_abs_error_sum: '4', conditional_model_beats_all_baselines: true, limitations: [] },
  ticket_sla_unavailable_reason: null, limitations: ['Synthetic only.'],
};

const shadowAssessment = {
  due_days: 21, scored_days: 20, corrected_days: 1, complete: false,
  whole_window_skill: null, recent_skill: null, fixed_interval_available: false, drift_signal: null,
  status_counts: { missing_forecast: 0, unscored: 1, scored: 20 },
};
const shadowReport = {
  status: 'descriptive_shadow_monitor', assessed_at_utc: '2026-09-27T00:00:00Z',
  policy: { id: 'policy-a', queue_id: 'support-q', effective_from_utc: '2026-09-01T00:00:00Z',
    effective_until_utc: '2026-10-01T00:00:00Z' },
  aggregate: shadowAssessment,
  sla: { ...shadowAssessment, status_counts: { aggregate_unavailable: 1, scored: 20 } },
  limitations: ['Descriptive only'],
};
const empiricalReport = {
  status: 'synthetic_only_exploratory',
  fit: { id: 'fit-47', training_days: 14, min_saturated_days: 7,
    capacity_identification: 'empirical_saturated_days', arrival_sample_count: 14, capacity_sample_count: 14,
    paired_saturated_day_count: 14, unsaturated_lower_bound_day_count: 0,
    partial_saturated_day_count: 0, max_capacity_lower_bound: 0 },
  run: { id: 'run-47', replay_hash: 'run-hash', common_draws_sha256: 'draws-hash', runs: 100,
    arrival_block_days: 7, sampling_mode: 'independent', capacity_source: 'saturated_day_empirical',
    delta_backlog_p05: -20, delta_backlog_p50: -10, delta_backlog_p95: 5,
    delta_sla_resolved_p05: 1, delta_sla_resolved_p50: 3, delta_sla_resolved_p95: 8,
    baseline_joint_violation_bps: 5000, alternative_joint_violation_bps: 3000,
    alternative_joint_recovery_bps: 2000, alternative_sla_improvement_bps: 7500,
    worst_alternative_backlog: 90, worst_alternative_sla_resolved: 400 },
  screen: null, limitations: ['Synthetic resampling only.'],
};

const eventReport = {
  status: 'synthetic_only_exploratory', source_origin: 'synthetic',
  source_sha256: 'a'.repeat(64), event_engine_sha256: 'd'.repeat(64),
  baseline: { scenario_id: 'scenario-base', replay_hash: 'b'.repeat(64), total_resolved_within_sla: 10,
    wait_seconds_p50: 3600, wait_seconds_p95: 7200, final_backlog: 12, total_staff_cost_cents: 5000 },
  alternative: { scenario_id: 'scenario-alt', replay_hash: 'c'.repeat(64), total_resolved_within_sla: 14,
    wait_seconds_p50: 1800, wait_seconds_p95: 5400, final_backlog: 8, total_staff_cost_cents: 7000 },
  limitations: ['Synthetic event times and service rate.'],
} as const;

const forecastReport = {
  status: 'synthetic_only_exploratory', source_origin: 'synthetic',
  snapshot_id: 'snapshot-1', model_version: 'model-1',
  baseline_scenario_id: 'scenario-base', alternative_scenario_id: 'scenario-alt',
  source_sha256: 'a'.repeat(64), min_training_days: 14, min_saturated_days: 7, calibration_points: 14,
  record_id: 'forecast-record-1', record_sha256: 'b'.repeat(64),
  forecast_evaluation_days: 16, rolling_interval_evaluated_points: 2,
  arrival_abs_error_sum: '9', model_abs_error_sum: '11', no_change_abs_error_sum: '18',
  seasonal_naive_abs_error_sum: '15', mean_change_abs_error_sum: '14',
  model_beats_all_baselines: true,
  rolling_observed_coverage_basis_points: 5_000, fixed_interval_evaluated_points: 2,
  fixed_observed_coverage_basis_points: 10_000,
  unavailable_reason: null, interval_unavailable_reason: null,
  limitations: ['Historical observations only.'],
} as const;

const candidateModel = {
  id: 'candidate-capacity-1', status: 'simulation_only_model_candidate', queue_id: 'queue-1',
  parent_model_version: 'model-1', model: { version: 'candidate-capacity-1',
    service_capacity_per_agent_day: 9, sla_days: 2, staff_cost_cents_per_agent_day: 10_000 },
  observed_through_utc: '2026-01-15T00:00:00Z', screen_replay_hash: 'a'.repeat(64),
  fit_id: 'fit-1', outcome_id: 'training-outcome-1', source_version_hashes: ['b'.repeat(64)],
  source_version_hashes_total: 1,
  record_sha256: 'c'.repeat(64), limitations: ['Simulation only.'],
};
const outcomeHoldout = {
  training_days: 14, holdout_days: 7, candidate_abs_error_sum: '9007199254740993',
  parent_abs_error_sum: '9007199254740994', no_change_abs_error_sum: '9007199254740995',
  candidate_beats_parent: true, candidate_beats_no_change: true,
};
const outcomeFit = {
  id: 'fit-1', outcome_id: 'training-outcome-1', replay_hash: '1'.repeat(64),
  observed_source_sha256: '2'.repeat(64), parent_model_version: 'model-1',
  parent_model_sha256: '3'.repeat(64), fit_engine_sha256: '4'.repeat(64),
  min_saturated_days: 3, training_days: 14,
  fit: { service_per_agent_day: 9, observed_min: 8, observed_max: 10,
    saturated_days: 5, training_days: 14 }, holdout: outcomeHoldout,
  record_sha256: '5'.repeat(64), engine_matches_current: true, limitations: ['Local fit only.'],
};
const outcomeScreen = {
  replay_hash: 'a'.repeat(64), fit_id: outcomeFit.id,
  fit_record_sha256: outcomeFit.record_sha256, record_sha256: '6'.repeat(64),
  source_version_hashes: ['2'.repeat(64)], source_version_hashes_total: 1,
  engine_matches_current: true,
  report: { status: 'exploratory_outcome_model_review_screen', replay_hash: 'a'.repeat(64),
    review_engine_sha256: '7'.repeat(64), criteria: { min_saturated_days: 3, min_holdout_days: 3 },
    fit_id: outcomeFit.id, fit_record_sha256: outcomeFit.record_sha256,
    outcome_id: outcomeFit.outcome_id, replayed_run_hash: outcomeFit.replay_hash,
    observed_source_sha256: outcomeFit.observed_source_sha256,
    parent_model_version: outcomeFit.parent_model_version,
    parent_model_sha256: outcomeFit.parent_model_sha256,
    fitted_capacity_per_agent_day: 9, saturated_training_days: 5,
    holdout: outcomeHoldout, queue_id: 'queue-1', local_run_precedes_window: true,
    eligible_for_human_review: true, failed_checks: [], limitations: ['Inspection only.'] },
};
const outcomeReview = {
  link: { approval_id: 'outcome-approval-1', agent_id: 'admin-1',
    replay_hash: outcomeScreen.replay_hash, screen_record_sha256: outcomeScreen.record_sha256,
    fit_id: outcomeFit.id, fit_record_sha256: outcomeFit.record_sha256 },
  status: 'pending' as const, expires_at_utc: '2026-09-28T12:00:00Z', decided_by: null,
};
const candidateRun = {
  replay_hash: 'd'.repeat(64), status: 'exploratory_model_assumption_sensitivity',
  candidate_id: candidateModel.id, target_snapshot_id: 'later-snapshot-1', scenario_id: 'later-scenario-1',
  record_sha256: 'e'.repeat(64), candidate_record_sha256: candidateModel.record_sha256,
  target_snapshot_sha256: 'f'.repeat(64), scenario_sha256: '1'.repeat(64),
  source_version_hashes: ['b'.repeat(64)], source_version_hashes_total: 1,
  parent: { model_version: 'model-1', replay_hash: '2'.repeat(64), total_arrivals: '40',
    total_resolved: '30', final_backlog: '20', total_resolved_within_sla: '22',
    total_staff_cost_cents: '50000', engine_matches_current: true },
  candidate: { model_version: candidateModel.id, replay_hash: '3'.repeat(64), total_arrivals: '40',
    total_resolved: '35', final_backlog: '15', total_resolved_within_sla: '27',
    total_staff_cost_cents: '50000', engine_matches_current: true },
  final_backlog_delta: '-5', resolved_within_sla_delta: '5',
  limitations: ['Model assumption sensitivity only.'],
};
const candidateScore = {
  replay_hash: '4'.repeat(64), status: 'exploratory_locally_timed_forecast_diagnostic',
  comparison_run_hash: candidateRun.replay_hash, outcome_id: 'later-outcome-1',
  record_sha256: '5'.repeat(64), comparison_run_sha256: candidateRun.record_sha256,
  outcome_record_sha256: '6'.repeat(64), ticket_source_sha256: '7'.repeat(64),
  ticket_label_engine_sha256: '8'.repeat(64), ticket_source_retention_until_utc: '2026-12-01T00:00:00Z',
  candidate_created_at_unix: 1768608000, comparison_created_at_unix: 1768694400,
  observed_window_start_utc: '2026-01-19T00:00:00Z', source_version_hashes: ['b'.repeat(64)],
  source_version_hashes_total: 1,
  parent: { days: 7, arrivals_abs_error_sum: '9007199254740993', resolved_abs_error_sum: '12',
    backlog_abs_error_sum: '18', resolved_within_sla_abs_error_sum: '8' },
  candidate: { days: 7, arrivals_abs_error_sum: '9007199254740993', resolved_abs_error_sum: '10',
    backlog_abs_error_sum: '11', resolved_within_sla_abs_error_sum: '5' },
  candidate_backlog_error_below_parent: true, candidate_resolution_error_below_parent: true,
  candidate_sla_error_below_parent: true, limitations: ['Local timestamps are not upstream attestations.'],
};

const shadowPolicy = {
  id: 'shadow-policy-1', source_lineage: 'synthetic-shadow', queue_id: 'queue-1',
  effective_from_utc: '2026-09-29T00:00:00+00:00', effective_until_utc: '2026-10-21T00:00:00+00:00',
  issue_deadline_seconds: 3600, min_training_days: 14, min_saturated_days: 3,
  calibration_engine_sha256: '1'.repeat(64), registered_at: 1790550000,
  record_sha256: '2'.repeat(64), limitations: ['Synthetic only.'],
};
const shadowForecast = {
  id: 'shadow-forecast-1', policy_id: shadowPolicy.id, policy_sha256: shadowPolicy.record_sha256,
  training_artifact_id: 'training-artifact-1', training_sha256: '3'.repeat(64),
  source_lineage: shadowPolicy.source_lineage, queue_id: shadowPolicy.queue_id,
  training_window_start_utc: '2026-09-15T00:00:00+00:00', target_day_utc: '2026-09-29T00:00:00+00:00',
  committed_at: Date.parse('2026-09-29T00:00:00Z') / 1000 + 60, min_saturated_days: 3,
  known: { opening_backlog: '10', planned_agents: 2, planned_fixed_extra_capacity: 0 },
  calibration_engine_sha256: shadowPolicy.calibration_engine_sha256,
  forecast: { predicted_arrivals: 12, predicted_backlog_end: '9007199254740994',
    no_change_backlog_end: '9007199254740995', seasonal_naive_backlog_end: '9007199254740996',
    mean_change_backlog_end: '9007199254740997', fitted_capacity_per_agent: 8, training_days: 14 },
  record_sha256: '4'.repeat(64), limitations: ['Prospective local commitment only.'],
};
const shadowScore = {
  id: 'shadow-score-1', forecast_id: shadowForecast.id,
  forecast_sha256: shadowForecast.record_sha256,
  observation_artifact_id: 'observation-artifact-1', observation_sha256: '5'.repeat(64),
  scored_at: Date.parse('2026-09-30T01:00:00Z') / 1000,
  arrivals_abs_error: '9007199254740993', backlog_abs_error: '4', no_change_abs_error: '5',
  seasonal_naive_abs_error: '6', mean_change_abs_error: '7',
  record_sha256: '6'.repeat(64), limitations: ['Aggregate errors only.'],
};
const shadowSums = { arrivals: '9007199254740993', backlog: '4', no_change_backlog: '5',
  seasonal_naive_backlog: '6', mean_change_backlog: '7' };
const shadowAssessmentAggregate = {
  policy_id: shadowPolicy.id, policy_sha256: shadowPolicy.record_sha256,
  source_lineage: shadowPolicy.source_lineage, queue_id: shadowPolicy.queue_id,
  assessed_at_utc: '2026-10-21T00:00:00+00:00', due_days: 21, scored_days: 21,
  corrected_days: 0, complete: true, error_sums: shadowSums,
  backlog_abs_error_below_each_baseline: true,
  fixed_prefix_interval: { method: 'fixed_prefix_absolute_residual_rank90_v1',
    target_coverage_basis_points: 9000, observed_coverage_basis_points: 10000,
    calibration_points: 14, evaluated_points: 7, recent_miss_count: 0,
    recent_window_points: 7, drift_signal: false, covered_points: 7 },
  recent_7_day_error_sums: shadowSums,
  recent_7_day_backlog_abs_error_below_each_baseline: true,
  day_status_counts: { scored: 21 }, limitations: ['Synthetic aggregate evidence.'],
};
const shadowScreenSaved = {
  replay_hash: '7'.repeat(64), policy_id: shadowPolicy.id,
  policy_sha256: shadowPolicy.record_sha256, record_sha256: '8'.repeat(64),
  source_version_hashes: ['3'.repeat(64), '5'.repeat(64)], source_version_hashes_total: 2,
  report: { status: 'exploratory_shadow_policy_screen', screen_engine_sha256: '9'.repeat(64),
    criteria: { min_complete_days: 21, min_fixed_coverage_bps: 8000 },
    assessment: shadowAssessmentAggregate,
    coverage_evidence: { covered_days: 7, evaluated_days: 7, minimum_covered_days: 6 },
    failed_checks: [], eligible_for_human_review: true, limitations: ['Inspection only.'] },
};
const shadowScreenPreview = { ...shadowScreenSaved, record_sha256: null, source_version_hashes: [],
  source_version_hashes_total: 0 };
const shadowReview = {
  link: { approval_id: 'shadow-approval-1', agent_id: 'admin-1', replay_hash: shadowScreenSaved.replay_hash,
    screen_record_sha256: shadowScreenSaved.record_sha256, policy_id: shadowPolicy.id,
    policy_sha256: shadowPolicy.record_sha256 },
  status: 'pending' as const, expires_at_utc: '2026-10-21T01:00:00+00:00', decided_by: null,
};

const overview: DecisionOverview = {
  status: 'exploratory', generated_at_utc: '2026-09-27T00:00:00Z',
  tenant_id: 'tenant-a', acl: 'private', counts: { candidates: 2, outcomes: 1 },
  invalidated_inputs: 1,
  ticket_sources: { active: 3, expired: 1, revoked: 2, next_expiry_utc: '2026-10-01T00:00:00Z' },
  artifacts: [{ kind: 'scenario', id: 'scenario-1', created_at_unix: 1, status: 'draft',
    candidate_id: 'candidate-1', ticket_backed: true, replay_hash: 'hash-1' }],
  limitations: ['No promotion is authorized.'],
};

function ReviewNavigationHarness() {
  const navigate = useNavigate();
  return <><button type="button" onClick={() => navigate('/app/system/decision-lab')}>Leave review link</button><DecisionLabPage /></>;
}

async function pasteFixture(user: ReturnType<typeof userEvent.setup>, input: HTMLElement, value: string) {
  // Pasting IDs and timestamps still exercises focus, input and onChange, but
  // avoids hundreds of whole-workflow renders for fixture setup. A timeout
  // does not cancel an async test body, which can otherwise spill into the
  // next test's DOM and mocks under full-suite contention.
  await user.click(input);
  await user.paste(value);
}

beforeEach(() => {
  vi.resetAllMocks();
  vi.mocked(decisionApi.overview).mockResolvedValue(overview);
  vi.mocked(decisionApi.catalog).mockResolvedValue(catalog);
  vi.mocked(decisionApi.scrubTicketSources).mockResolvedValue({ scrubbed: 2, overview: { ...overview,
    ticket_sources: { ...overview.ticket_sources, active: 1, revoked: 4 } } });
  vi.mocked(decisionApi.createSyntheticPilot).mockResolvedValue({ snapshot_id: 'snapshot-1', model_version: 'model-1',
    baseline_scenario_id: 'scenario-base', alternative_scenario_id: 'scenario-alt', source_artifact_id: 'synthetic-source-1' });
  vi.mocked(decisionApi.stageSyntheticLifecycle).mockResolvedValue({ stage_outcome: 'staged' });
  vi.mocked(decisionApi.compare).mockResolvedValue({ brief } as never);
  vi.mocked(decisionApi.replay).mockImplementation(async (input) => ({ result: {
    snapshot_id: input.snapshot_id, model_version: input.model_version, scenario_id: input.scenario_id,
    replay_hash: input.expected_hash, days: [simDay(1, input.scenario_id === 'scenario-base' ? 12 : 8)],
    total_arrivals: 10, total_resolved: 11, final_backlog: input.scenario_id === 'scenario-base' ? 12 : 8,
    total_resolved_within_sla: 8, total_staff_cost_cents: 1000,
  } }));
  vi.mocked(decisionApi.policySweep).mockResolvedValue({ report: policyReport });
  vi.mocked(decisionApi.sensitivity).mockResolvedValue({ report: sensitivityReport });
  vi.mocked(decisionApi.engineeringValidation).mockResolvedValue({ report: engineeringReport });
  vi.mocked(decisionApi.empiricalResampling).mockResolvedValue(empiricalReport as never);
  vi.mocked(decisionApi.eventCompare).mockResolvedValue({ report: eventReport } as never);
  vi.mocked(decisionApi.forecastValidation).mockResolvedValue({ report: forecastReport } as never);
  vi.mocked(decisionApi.createCandidateModel).mockResolvedValue({ candidate: candidateModel });
  vi.mocked(decisionApi.loadCandidateModel).mockResolvedValue({ candidate: candidateModel });
  vi.mocked(decisionApi.compareCandidateModel).mockResolvedValue({ run: candidateRun });
  vi.mocked(decisionApi.loadCandidateRun).mockResolvedValue({ run: candidateRun });
  vi.mocked(decisionApi.scoreCandidateModel).mockResolvedValue({ score: candidateScore });
  vi.mocked(decisionApi.loadCandidateScore).mockResolvedValue({ score: candidateScore });
  vi.mocked(decisionApi.createOutcomeFit).mockResolvedValue({ fit: outcomeFit });
  vi.mocked(decisionApi.loadOutcomeFit).mockResolvedValue({ fit: outcomeFit });
  vi.mocked(decisionApi.saveOutcomeScreen).mockResolvedValue({ screen: outcomeScreen });
  vi.mocked(decisionApi.loadOutcomeScreen).mockResolvedValue({ screen: outcomeScreen });
  vi.mocked(decisionApi.requestOutcomeReview).mockResolvedValue({ review: outcomeReview });
  vi.mocked(decisionApi.outcomeReviewStatus).mockResolvedValue({ review: {
    ...outcomeReview, status: 'approved', decided_by: 'human-reviewer',
  } });
  vi.mocked(decisionApi.createShadowPolicy).mockResolvedValue({ policy: shadowPolicy });
  vi.mocked(decisionApi.loadShadowPolicy).mockResolvedValue({ policy: shadowPolicy });
  vi.mocked(decisionApi.createShadowForecast).mockResolvedValue({ forecast: shadowForecast });
  vi.mocked(decisionApi.loadShadowForecast).mockResolvedValue({ forecast: shadowForecast });
  vi.mocked(decisionApi.createShadowScore).mockResolvedValue({ score: shadowScore });
  vi.mocked(decisionApi.loadShadowScore).mockResolvedValue({ score: shadowScore });
  vi.mocked(decisionApi.assessShadowPolicy).mockResolvedValue({ assessment: shadowAssessmentAggregate });
  vi.mocked(decisionApi.evaluateShadowScreen).mockResolvedValue({ screen: shadowScreenPreview });
  vi.mocked(decisionApi.saveShadowScreen).mockResolvedValue({ screen: shadowScreenSaved });
  vi.mocked(decisionApi.loadShadowScreen).mockResolvedValue({ screen: shadowScreenSaved });
  vi.mocked(decisionApi.requestShadowReview).mockResolvedValue({ review: shadowReview });
  vi.mocked(decisionApi.shadowReviewStatus).mockResolvedValue({ review: {
    ...shadowReview, status: 'approved', decided_by: 'human-reviewer',
  } });
  vi.mocked(decisionApi.shadowMonitor).mockResolvedValue(shadowReport as never);
  vi.mocked(decisionApi.requestPilotReview).mockResolvedValue({ review: {
    link: { approval_id: 'approval-1', agent_id: 'admin-1', replay_hash: 'base-hash',
      snapshot_id: 'snapshot-1', scenario_id: 'scenario-base' },
    status: 'pending', expires_at_utc: '2026-09-27T12:00:00Z', decided_by: null,
  } });
  vi.mocked(decisionApi.pilotReviewStatus).mockResolvedValue({ review: {
    link: { approval_id: 'approval-1', agent_id: 'admin-1', replay_hash: 'base-hash',
      snapshot_id: 'snapshot-1', scenario_id: 'scenario-base' },
    status: 'approved', expires_at_utc: '2026-09-27T12:00:00Z', decided_by: 'human-reviewer',
  } });
  // No `system.status` reported ⇒ no `decision_enabled` field ⇒ the page renders
  // in full (fail-open, see `useDecisionEnabled`). Reset here so the one test
  // that switches the line off cannot leak into its neighbours.
  useSystemStore.setState({ status: null });
});

describe('DecisionLabPage', () => {
  // X1 落日條款 (2026-09-29 audit): with `config.toml [decision] enabled =
  // false` every `/api/decision/*` route answers 404, so the page states who
  // turned it off rather than rendering controls that can only fail. The route
  // stays reachable on purpose — a bookmark lands on this, not on a redirect
  // that reads as a bug.
  it('states the operator switched the line off, and fires no request', () => {
    useSystemStore.setState({ status: { decision_enabled: false } as never });
    renderWithProviders(<DecisionLabPage />);
    expect(screen.getByTestId('decision-lab-disabled')).toBeInTheDocument();
    expect(screen.getByText(en['decisionLab.disabledTitle'])).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Load overview' })).not.toBeInTheDocument();
    expect(decisionApi.overview).not.toHaveBeenCalled();
  });

  it('renders in full when the switch is on', async () => {
    useSystemStore.setState({ status: { decision_enabled: true } as never });
    renderWithProviders(<DecisionLabPage />);
    expect(screen.queryByTestId('decision-lab-disabled')).not.toBeInTheDocument();
    expect(await screen.findByRole('button', { name: 'Load overview' })).toBeInTheDocument();
  });

  it('runs the scoped aggregate shadow stages with exact decimal errors and a human inspection receipt', async () => {
    const user = userEvent.setup();
    renderWithProviders(<DecisionLabPage />);
    await pasteFixture(user, screen.getByRole('textbox', { name: 'Organization ID' }), 'tenant-a');
    await pasteFixture(user, screen.getByRole('textbox', { name: 'Access scope' }), 'private');
    await user.click(screen.getByRole('button', { name: 'Load overview' }));
    const panel = await screen.findByLabelText('Aggregate shadow forecast workflow');
    const policyStage = within(panel).getByLabelText('1. Register or load policy');
    await pasteFixture(user, within(policyStage).getByRole('textbox', { name: 'Policy ID' }), shadowPolicy.id);
    await pasteFixture(user, within(policyStage).getByRole('textbox', { name: 'Shadow source lineage' }), shadowPolicy.source_lineage);
    await pasteFixture(user, within(policyStage).getByRole('textbox', { name: 'Queue ID' }), shadowPolicy.queue_id);
    await pasteFixture(user, within(policyStage).getByRole('textbox', { name: 'Effective from (UTC)' }), shadowPolicy.effective_from_utc);
    await pasteFixture(user, within(policyStage).getByRole('textbox', { name: 'Effective until (UTC)' }), shadowPolicy.effective_until_utc);
    await user.click(within(policyStage).getByRole('button', { name: 'Create' }));
    expect(await within(policyStage).findByLabelText('Loaded shadow policy')).toHaveTextContent(shadowPolicy.id);
    expect(decisionApi.createShadowPolicy).toHaveBeenCalledWith(expect.objectContaining({
      tenant_id: 'tenant-a', acl: 'private', policy_id: shadowPolicy.id,
      source_lineage: shadowPolicy.source_lineage, queue_id: shadowPolicy.queue_id,
    }));

    const forecastStage = within(panel).getByLabelText('2. Commit prospective forecast');
    await pasteFixture(user, within(forecastStage).getByRole('textbox', { name: 'Forecast ID' }), shadowForecast.id);
    await pasteFixture(user, within(forecastStage).getByRole('textbox', { name: 'Target day (UTC midnight)' }), shadowForecast.target_day_utc);
    await user.clear(within(forecastStage).getByRole('textbox', { name: 'Known opening backlog' }));
    await pasteFixture(user, within(forecastStage).getByRole('textbox', { name: 'Known opening backlog' }), '10');
    await user.clear(within(forecastStage).getByRole('textbox', { name: 'Planned agents' }));
    await pasteFixture(user, within(forecastStage).getByRole('textbox', { name: 'Planned agents' }), '2');
    fireEvent.change(within(forecastStage).getByRole('textbox', { name: 'Prior-only training source JSON (or source record ID)' }),
      { target: { value: JSON.stringify({ queue_id: 'queue-1', observed_days: [{ arrivals: 10, backlog_end: 12 }] }) } });
    await pasteFixture(user, within(forecastStage).getByRole('textbox', { name: 'training Source retention until (UTC)' }),
      '2099-01-01T00:00:00Z');
    await user.click(within(forecastStage).getByRole('button', { name: 'Create' }));
    const forecastLoaded = await within(forecastStage).findByLabelText('Loaded forecast');
    expect(forecastLoaded).toHaveTextContent('9007199254740994');
    expect(decisionApi.createShadowForecast).toHaveBeenCalledWith(expect.objectContaining({
      tenant_id: 'tenant-a', acl: 'private', forecast_id: shadowForecast.id, policy_id: shadowPolicy.id,
      known: { opening_backlog: 10, planned_agents: 2, planned_fixed_extra_capacity: 0 },
    }));
    expect(within(forecastStage).getByRole('textbox', { name: 'Prior-only training source JSON (or source record ID)' }))
      .toHaveValue('');

    const scoreStage = within(panel).getByLabelText('3. Score observed day');
    await pasteFixture(user, within(scoreStage).getByRole('textbox', { name: 'Score ID' }), shadowScore.id);
    fireEvent.change(within(scoreStage).getByRole('textbox', { name: 'Target-day observation source JSON (or source record ID)' }),
      { target: { value: JSON.stringify({ queue_id: 'queue-1', observed_days: [{ arrivals: 11, backlog_end: 13 }] }) } });
    await pasteFixture(user, within(scoreStage).getByRole('textbox', { name: 'observation Source retention until (UTC)' }),
      '2099-01-01T00:00:00Z');
    await user.click(within(scoreStage).getByRole('button', { name: 'Create' }));
    expect(await within(scoreStage).findByLabelText('Loaded score')).toHaveTextContent('9,007,199,254,740,993');
    expect(within(scoreStage).getByRole('textbox', { name: 'Target-day observation source JSON (or source record ID)' }))
      .toHaveValue('');
    expect(decisionApi.createShadowScore).toHaveBeenCalledWith(expect.objectContaining({
      tenant_id: 'tenant-a', acl: 'private', score_id: shadowScore.id, forecast_id: shadowForecast.id,
    }));

    const assessStage = within(panel).getByLabelText('4. Aggregate assessment');
    await user.click(within(assessStage).getByRole('button', { name: 'Assess exact policy' }));
    expect(await within(assessStage).findByLabelText('Aggregate assessment'))
      .toHaveTextContent('9,007,199,254,740,993');
    const screenStage = within(panel).getByLabelText('5. Evaluate or save aggregate screen');
    await user.click(within(screenStage).getByRole('button', { name: 'Preview screen' }));
    expect(await within(screenStage).findByLabelText('Screen result')).toHaveTextContent('Preview only');
    await user.click(within(screenStage).getByRole('button', { name: 'Save screen' }));
    expect(await within(screenStage).findByLabelText('Screen result')).toHaveTextContent('Saved exact record');
    expect(within(screenStage).getByRole('textbox', { name: 'Screen verification code' }))
      .toHaveValue(shadowScreenSaved.replay_hash);

    const reviewStage = within(panel).getByLabelText('6. Human inspection receipt');
    await pasteFixture(user, within(reviewStage).getByRole('textbox', { name: 'Inspection summary' }), 'Check synthetic aggregate evidence');
    await user.click(within(reviewStage).getByRole('button', { name: 'Request inspection' }));
    expect(await within(reviewStage).findByLabelText('Inspection status')).toHaveTextContent('pending');
    await user.click(within(reviewStage).getByRole('button', { name: 'Check shadow inspection' }));
    expect(await within(reviewStage).findByLabelText('Inspection status')).toHaveTextContent('approved');
    expect(decisionApi.requestShadowReview).toHaveBeenCalledWith({ tenant_id: 'tenant-a', acl: 'private',
      replay_hash: shadowScreenSaved.replay_hash, summary: 'Check synthetic aggregate evidence', ttl_seconds: 3600 });
  });

  it('rejects mismatched shadow lineage and prevents review for an ineligible screen', async () => {
    const user = userEvent.setup();
    vi.mocked(decisionApi.loadShadowForecast).mockResolvedValueOnce({ forecast: {
      ...shadowForecast, policy_sha256: 'f'.repeat(64),
    } });
    vi.mocked(decisionApi.saveShadowScreen).mockResolvedValueOnce({ screen: {
      ...shadowScreenSaved, report: { ...shadowScreenSaved.report,
        coverage_evidence: { covered_days: 5, evaluated_days: 7, minimum_covered_days: 6 } },
    } }).mockResolvedValueOnce({ screen: {
      ...shadowScreenSaved, report: { ...shadowScreenSaved.report,
        eligible_for_human_review: false, failed_checks: ['coverage below threshold'] },
    } });
    renderWithProviders(<DecisionLabPage />);
    await user.type(screen.getByRole('textbox', { name: 'Organization ID' }), 'tenant-a');
    await user.type(screen.getByRole('textbox', { name: 'Access scope' }), 'private');
    await user.click(screen.getByRole('button', { name: 'Load overview' }));
    const panel = await screen.findByLabelText('Aggregate shadow forecast workflow');
    const forecastStage = within(panel).getByLabelText('2. Commit prospective forecast');
    await user.type(within(forecastStage).getByRole('textbox', { name: 'Forecast ID' }), shadowForecast.id);
    await user.click(within(forecastStage).getByRole('button', { name: 'Load exact record' }));
    expect(await within(panel).findByRole('alert')).toHaveTextContent('does not match');
    expect(within(forecastStage).queryByLabelText('Loaded forecast')).not.toBeInTheDocument();
    const policyStage = within(panel).getByLabelText('1. Register or load policy');
    await user.type(within(policyStage).getByRole('textbox', { name: 'Policy ID' }), shadowPolicy.id);
    await user.click(within(policyStage).getByRole('button', { name: 'Load exact record' }));
    expect(await within(policyStage).findByLabelText('Loaded shadow policy')).toBeInTheDocument();
    await user.click(within(forecastStage).getByRole('button', { name: 'Load exact record' }));
    expect(await within(forecastStage).findByLabelText('Loaded forecast')).toHaveTextContent(shadowForecast.id);
    const screenStage = within(panel).getByLabelText('5. Evaluate or save aggregate screen');
    fireEvent.change(within(screenStage).getByRole('spinbutton', { name: 'Minimum complete days' }),
      { target: { value: '14' } });
    expect(within(screenStage).getByRole('button', { name: 'Save screen' })).toBeDisabled();
    expect(decisionApi.saveShadowScreen).not.toHaveBeenCalled();
    fireEvent.change(within(screenStage).getByRole('spinbutton', { name: 'Minimum complete days' }),
      { target: { value: '21' } });
    await user.click(within(screenStage).getByRole('button', { name: 'Save screen' }));
    expect(await within(panel).findByRole('alert')).toHaveTextContent('does not match');
    expect(within(screenStage).queryByLabelText('Screen result')).not.toBeInTheDocument();
    await user.click(within(screenStage).getByRole('button', { name: 'Save screen' }));
    expect(await within(screenStage).findByLabelText('Screen result')).toHaveTextContent('coverage below threshold');
    const reviewStage = within(panel).getByLabelText('6. Human inspection receipt');
    expect(within(reviewStage).getByRole('button', { name: 'Request inspection' })).toBeDisabled();
    expect(decisionApi.requestShadowReview).not.toHaveBeenCalled();
  });

  it('drops the frozen assessment when the screen hash is edited', async () => {
    const user = userEvent.setup();
    renderWithProviders(<DecisionLabPage />);
    await user.type(screen.getByRole('textbox', { name: 'Organization ID' }), 'tenant-a');
    await user.type(screen.getByRole('textbox', { name: 'Access scope' }), 'private');
    await user.click(screen.getByRole('button', { name: 'Load overview' }));
    const panel = await screen.findByLabelText('Aggregate shadow forecast workflow');
    const screenStage = within(panel).getByLabelText('5. Evaluate or save aggregate screen');
    await user.type(within(screenStage).getByRole('textbox', { name: 'Screen verification code' }),
      shadowScreenSaved.replay_hash);
    await user.click(within(screenStage).getByRole('button', { name: 'Load exact record' }));
    expect(await within(screenStage).findByLabelText('Screen result')).toBeInTheDocument();
    const assessStage = within(panel).getByLabelText('4. Aggregate assessment');
    expect(within(assessStage).getByLabelText('Aggregate assessment')).toBeInTheDocument();
    // Editing the hash replaces the screen; the assessment it froze must not
    // stay on screen looking like a live assessment of the current policy.
    await user.type(within(screenStage).getByRole('textbox', { name: 'Screen verification code' }), 'x');
    expect(within(screenStage).queryByLabelText('Screen result')).not.toBeInTheDocument();
    expect(within(assessStage).queryByLabelText('Aggregate assessment')).not.toBeInTheDocument();
  });

  it('clears saved shadow evidence on revocation and overview scope revision', async () => {
    const user = userEvent.setup();
    vi.mocked(decisionApi.overview).mockResolvedValueOnce(overview).mockResolvedValueOnce({
      ...overview, invalidated_inputs: overview.invalidated_inputs + 1,
    });
    renderWithProviders(<DecisionLabPage />);
    await user.type(screen.getByRole('textbox', { name: 'Organization ID' }), 'tenant-a');
    await user.type(screen.getByRole('textbox', { name: 'Access scope' }), 'private');
    await user.click(screen.getByRole('button', { name: 'Load overview' }));
    const panel = await screen.findByLabelText('Aggregate shadow forecast workflow');
    const screenStage = within(panel).getByLabelText('5. Evaluate or save aggregate screen');
    await user.type(within(screenStage).getByRole('textbox', { name: 'Screen verification code' }), shadowScreenSaved.replay_hash);
    await user.click(within(screenStage).getByRole('button', { name: 'Load exact record' }));
    expect(await within(screenStage).findByLabelText('Screen result')).toHaveTextContent('Saved exact record');
    vi.mocked(decisionApi.loadShadowScreen).mockRejectedValueOnce(new Error('source revoked'));
    await user.click(within(screenStage).getByRole('button', { name: 'Load exact record' }));
    expect(await within(panel).findByRole('alert')).toHaveTextContent('source revoked');
    expect(within(screenStage).queryByLabelText('Screen result')).not.toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Load overview' }));
    await waitFor(() => expect(screen.getByLabelText('Aggregate shadow forecast workflow'))
      .toContainElement(screen.getByRole('textbox', { name: 'Screen verification code' })));
    expect(screen.getByRole('textbox', { name: 'Screen verification code' })).toHaveValue('');
    expect(screen.queryByText('source revoked')).not.toBeInTheDocument();
  });

  it('uploads an explicit operator pilot pair and selects the verified catalog pair for comparison', async () => {
    const user = userEvent.setup();
    vi.mocked(decisionApi.catalog).mockResolvedValueOnce(catalog).mockResolvedValueOnce(uploadedCatalog);
    vi.mocked(decisionApi.importPilot).mockResolvedValue(uploadedReceipt);
    renderWithProviders(<DecisionLabPage />);
    await user.type(screen.getByRole('textbox', { name: 'Organization ID' }), 'tenant-a');
    await user.type(screen.getByRole('textbox', { name: 'Access scope' }), 'private');
    await user.click(screen.getByRole('button', { name: 'Load overview' }));
    await screen.findByRole('button', { name: 'Create pilot' });
    await fillPilotUpload(user);
    expect(screen.queryByText('private-customer-export.json')).not.toBeInTheDocument();
    expect(screen.queryByText('PRIVATE-CUSTOMER-TICKET')).not.toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Import pilot pair' }));
    await waitFor(() => expect(decisionApi.importPilot).toHaveBeenCalledWith(expect.objectContaining({
      tenant_id: 'tenant-a', acl: 'private', expected_queue_id: 'operator-queue', source_lineage: 'operator-export',
      retention_until_utc: '2099-01-01T00:00:00Z',
      export: expect.objectContaining({ snapshot_id: 'uploaded-snapshot',
        tickets: [expect.objectContaining({ ticket_id: 'PRIVATE-CUSTOMER-TICKET' })] }),
      model: expect.objectContaining({ version: 'uploaded-model' }),
      alternative_scenario: expect.objectContaining({ id: 'uploaded-alternative' }),
    })));
    expect(await screen.findByText('Pilot imported and selected for comparison. Review source and verification codes below.')).toBeInTheDocument();
    expect(screen.getByText(/SLA holdout unavailable: Capacity unidentified/)).toBeInTheDocument();
    expect(screen.queryByText('PRIVATE-CUSTOMER-TICKET')).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Run engineering validation' })).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Run comparison' }));
    await waitFor(() => expect(decisionApi.compare).toHaveBeenCalledWith({
      tenant_id: 'tenant-a', acl: 'private', snapshot_id: 'uploaded-snapshot', model_version: 'uploaded-model',
      baseline_scenario_id: 'uploaded-baseline', alternative_scenario_id: 'uploaded-alternative',
    }));
  });

  it('locks the upload and scope while persistence is in flight', async () => {
    const user = userEvent.setup();
    let finish!: (receipt: DecisionPilotUploadReceipt) => void;
    vi.mocked(decisionApi.catalog).mockResolvedValueOnce(catalog).mockResolvedValueOnce(uploadedCatalog);
    vi.mocked(decisionApi.importPilot).mockImplementation(() => new Promise((resolve) => { finish = resolve; }));
    renderWithProviders(<DecisionLabPage />);
    await user.type(screen.getByRole('textbox', { name: 'Organization ID' }), 'tenant-a');
    await user.type(screen.getByRole('textbox', { name: 'Access scope' }), 'private');
    await user.click(screen.getByRole('button', { name: 'Load overview' }));
    await screen.findByRole('button', { name: 'Create pilot' });
    await fillPilotUpload(user);
    await user.click(screen.getByRole('button', { name: 'Import pilot pair' }));
    await waitFor(() => expect(decisionApi.importPilot).toHaveBeenCalledTimes(1));
    expect(screen.getByRole('textbox', { name: 'Access scope' })).toBeDisabled();
    expect(screen.getByRole('textbox', { name: 'Expected queue ID' })).toBeDisabled();
    expect(screen.getByLabelText('Queue model JSON')).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Load overview' })).toBeDisabled();
    await act(async () => { finish(uploadedReceipt); });
    expect(await screen.findByText('Pilot imported and selected for comparison. Review source and verification codes below.')).toBeInTheDocument();
    expect(screen.getByRole('textbox', { name: 'Access scope' })).not.toBeDisabled();
  });

  it('suppresses private server errors and unlocks files after a failed upload', async () => {
    const user = userEvent.setup();
    vi.mocked(decisionApi.importPilot).mockRejectedValueOnce(new Error('PRIVATE-CUSTOMER-TICKET was invalid'));
    renderWithProviders(<DecisionLabPage />);
    await user.type(screen.getByRole('textbox', { name: 'Organization ID' }), 'tenant-a');
    await user.type(screen.getByRole('textbox', { name: 'Access scope' }), 'private');
    await user.click(screen.getByRole('button', { name: 'Load overview' }));
    await screen.findByRole('button', { name: 'Create pilot' });
    await fillPilotUpload(user);
    await user.click(screen.getByRole('button', { name: 'Import pilot pair' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('The pilot import failed.');
    expect(screen.queryByText(/PRIVATE-CUSTOMER-TICKET/)).not.toBeInTheDocument();
    expect(screen.getByLabelText('Queue model JSON')).not.toBeDisabled();
    await user.upload(screen.getByLabelText('Queue model JSON'),
      new File([JSON.stringify({ version: 'uploaded-model', service_capacity_per_agent_day: 9,
        sla_days: 2, staff_cost_cents_per_agent_day: 10_000 })], 'different-model.json', { type: 'application/json' }));
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
  });

  it('loads a scoped exploratory overview, artifacts, and limitations', async () => {
    const user = userEvent.setup();
    renderWithProviders(<DecisionLabPage />);
    await user.type(screen.getByRole('textbox', { name: 'Organization ID' }), 'tenant-a');
    await user.type(screen.getByRole('textbox', { name: 'Access scope' }), 'private');
    await user.click(screen.getByRole('button', { name: 'Load overview' }));
    await waitFor(() => expect(decisionApi.overview).toHaveBeenCalledWith({ tenant_id: 'tenant-a', acl: 'private' }));
    expect(await screen.findByText('scenario-1')).toBeInTheDocument();
    expect(screen.getByText('No promotion is authorized.')).toBeInTheDocument();
    expect(screen.getByText(/does not authorize operational actions/i)).toBeInTheDocument();
    expect(screen.getByText('1')).toBeInTheDocument();
  });

  it('requires explicit confirmation before scrubbing and refreshes the overview', async () => {
    const user = userEvent.setup();
    renderWithProviders(<DecisionLabPage />);
    await user.type(screen.getByRole('textbox', { name: 'Organization ID' }), 'tenant-a');
    await user.type(screen.getByRole('textbox', { name: 'Access scope' }), 'private');
    await user.click(screen.getByRole('button', { name: 'Load overview' }));
    const scrub = await screen.findByRole('button', { name: 'Scrub expired sources' });
    expect(scrub).toBeDisabled();
    await user.click(screen.getByRole('checkbox'));
    await user.click(scrub);
    await waitFor(() => expect(decisionApi.scrubTicketSources).toHaveBeenCalledWith({ tenant_id: 'tenant-a', acl: 'private' }));
    expect(await screen.findByText('Scrubbed 2 expired ticket sources.')).toBeInTheDocument();
  });

  it('invalidates the loaded scope before a different scope can be scrubbed', async () => {
    const user = userEvent.setup();
    renderWithProviders(<DecisionLabPage />);
    await user.type(screen.getByRole('textbox', { name: 'Organization ID' }), 'tenant-a');
    await user.type(screen.getByRole('textbox', { name: 'Access scope' }), 'private');
    await user.click(screen.getByRole('button', { name: 'Load overview' }));
    await screen.findByRole('button', { name: 'Scrub expired sources' });
    expect(screen.getByText('Loaded scope: tenant-a / private')).toBeInTheDocument();
    await user.type(screen.getByRole('textbox', { name: 'Organization ID' }), 'b');
    expect(screen.queryByRole('button', { name: 'Scrub expired sources' })).not.toBeInTheDocument();
    expect(decisionApi.scrubTicketSources).not.toHaveBeenCalled();
  });

  it('loads one exact shadow policy and clears its counts when scope changes', async () => {
    const user = userEvent.setup();
    renderWithProviders(<DecisionLabPage />);
    await user.type(screen.getByRole('textbox', { name: 'Organization ID' }), 'tenant-a');
    await user.type(screen.getByRole('textbox', { name: 'Access scope' }), 'private');
    await user.click(screen.getByRole('button', { name: 'Load overview' }));
    await user.type(await screen.findByRole('textbox', { name: 'Shadow policy ID' }), 'policy-a');
    await user.click(screen.getByRole('button', { name: 'Load shadow status' }));
    await waitFor(() => expect(decisionApi.shadowMonitor).toHaveBeenCalledWith(
      { tenant_id: 'tenant-a', acl: 'private' }, 'policy-a',
    ));
    expect(screen.getByText(/support-q/)).toBeInTheDocument();
    expect(screen.getAllByText(/Scored 20 \/ 21 due days/)).toHaveLength(2);
    expect(screen.getAllByText('Evidence incomplete')).toHaveLength(2);
    await user.type(screen.getByRole('textbox', { name: 'Access scope' }), '-new');
    expect(screen.queryByText(/Scored 20 \/ 21 due days/)).not.toBeInTheDocument();
  });

  it('refuses to display a shadow response for a different policy', async () => {
    vi.mocked(decisionApi.shadowMonitor).mockResolvedValueOnce({
      ...shadowReport, policy: { ...shadowReport.policy, id: 'another-policy' },
    } as never);
    const user = userEvent.setup();
    renderWithProviders(<DecisionLabPage />);
    await user.type(screen.getByRole('textbox', { name: 'Organization ID' }), 'tenant-a');
    await user.type(screen.getByRole('textbox', { name: 'Access scope' }), 'private');
    await user.click(screen.getByRole('button', { name: 'Load overview' }));
    await user.type(await screen.findByRole('textbox', { name: 'Shadow policy ID' }), 'policy-a');
    await user.click(screen.getByRole('button', { name: 'Load shadow status' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('The returned shadow policy differs');
    expect(screen.queryByText(/support-q/)).not.toBeInTheDocument();
  });

  it('wires a scoped candidate through frozen comparison and later score without losing exact large errors', async () => {
    const user = userEvent.setup();
    renderWithProviders(<DecisionLabPage />);
    await user.type(screen.getByRole('textbox', { name: 'Organization ID' }), 'tenant-a');
    await user.type(screen.getByRole('textbox', { name: 'Access scope' }), 'private');
    await user.click(screen.getByRole('button', { name: 'Load overview' }));
    const panel = await screen.findByLabelText('Outcome model candidate review');
    await user.type(within(panel).getByRole('textbox', { name: 'Candidate ID' }), 'candidate-capacity-1');
    await user.type(within(panel).getByRole('textbox', { name: 'Saved review screen verification code' }), 'a'.repeat(64));
    await user.click(within(panel).getByRole('button', { name: 'Save candidate' }));
    await waitFor(() => expect(decisionApi.createCandidateModel).toHaveBeenCalledWith({
      tenant_id: 'tenant-a', acl: 'private', candidate_id: 'candidate-capacity-1',
      screen_replay_hash: 'a'.repeat(64),
    }));
    expect(await within(panel).findByLabelText('Loaded candidate')).toHaveTextContent('Simulation only.');
    await user.type(within(panel).getByRole('textbox', { name: 'Later snapshot ID' }), 'later-snapshot-1');
    await user.type(within(panel).getByRole('textbox', { name: 'Staffing scenario ID' }), 'later-scenario-1');
    await user.click(within(panel).getByRole('button', { name: 'Save comparison' }));
    await waitFor(() => expect(decisionApi.compareCandidateModel).toHaveBeenCalledWith({
      tenant_id: 'tenant-a', acl: 'private', candidate_id: 'candidate-capacity-1',
      target_snapshot_id: 'later-snapshot-1', scenario_id: 'later-scenario-1',
    }));
    expect(await within(panel).findByLabelText('Loaded candidate comparison')).toHaveTextContent('Model assumption sensitivity only.');
    await user.type(within(panel).getByRole('textbox', { name: 'Later outcome ID' }), 'later-outcome-1');
    await user.click(within(panel).getByRole('button', { name: 'Save later score' }));
    await waitFor(() => expect(decisionApi.scoreCandidateModel).toHaveBeenCalledWith({
      tenant_id: 'tenant-a', acl: 'private', comparison_run_hash: candidateRun.replay_hash,
      outcome_id: 'later-outcome-1',
    }));
    const score = await within(panel).findByLabelText('Loaded candidate score');
    expect(within(score).getAllByText('9,007,199,254,740,993')).toHaveLength(2);
    expect(within(score).getByText('Ticket-label source checked locally')).toBeInTheDocument();
    expect(JSON.stringify(vi.mocked(decisionApi.scoreCandidateModel).mock.lastCall?.[0])).not.toContain('ticket_id');
    await user.type(within(panel).getByRole('textbox', { name: 'Candidate ID' }), '-changed');
    expect(within(panel).queryByLabelText('Loaded candidate comparison')).not.toBeInTheDocument();
    expect(within(panel).queryByLabelText('Loaded candidate score')).not.toBeInTheDocument();
  });

  it('fits an outcome, saves an eligible screen, requests inspection, and passes its exact hash to candidate creation', async () => {
    vi.mocked(decisionApi.overview).mockResolvedValueOnce(overview).mockResolvedValueOnce({
      ...overview, invalidated_inputs: overview.invalidated_inputs + 1,
    });
    const user = userEvent.setup();
    renderWithProviders(<DecisionLabPage />);
    await user.type(screen.getByRole('textbox', { name: 'Organization ID' }), 'tenant-a');
    await user.type(screen.getByRole('textbox', { name: 'Access scope' }), 'private');
    await user.click(screen.getByRole('button', { name: 'Load overview' }));
    const panel = await screen.findByLabelText('Outcome capacity fit and human inspection');
    await user.type(within(panel).getByRole('textbox', { name: 'Fit ID' }), 'fit-1');
    await user.type(within(panel).getByRole('textbox', { name: 'Saved outcome ID' }), 'training-outcome-1');
    await user.click(within(panel).getByRole('button', { name: 'Save fit' }));
    await waitFor(() => expect(decisionApi.createOutcomeFit).toHaveBeenCalledWith({
      tenant_id: 'tenant-a', acl: 'private', fit_id: 'fit-1', outcome_id: 'training-outcome-1',
      min_saturated_days: 3, training_days: 14,
    }));
    const fit = await within(panel).findByLabelText('Loaded capacity fit');
    expect(within(fit).getByText(/9,007,199,254,740,993/)).toBeInTheDocument();
    await user.click(within(panel).getByRole('button', { name: 'Save screen' }));
    await waitFor(() => expect(decisionApi.saveOutcomeScreen).toHaveBeenCalledWith({
      tenant_id: 'tenant-a', acl: 'private', fit_id: 'fit-1',
      min_saturated_days: 3, min_holdout_days: 3,
    }));
    const saved = await within(panel).findByLabelText('Loaded model review screen');
    expect(within(saved).getByText('Eligible for human inspection')).toBeInTheDocument();
    await user.click(within(saved).getByRole('button', { name: 'Use for candidate' }));
    const candidatePanel = screen.getByLabelText('Outcome model candidate review');
    expect(within(candidatePanel).getByRole('textbox', { name: 'Saved review screen verification code' }))
      .toHaveValue(outcomeScreen.replay_hash);
    await user.type(within(panel).getByRole('textbox', { name: 'Inspection summary' }), 'Inspect capacity fit');
    await user.click(within(panel).getByRole('button', { name: 'Request inspection' }));
    await waitFor(() => expect(decisionApi.requestOutcomeReview).toHaveBeenCalledWith({
      tenant_id: 'tenant-a', acl: 'private', replay_hash: outcomeScreen.replay_hash,
      summary: 'Inspect capacity fit', ttl_seconds: 3600,
    }));
    expect(await within(panel).findByLabelText('Inspection receipt status')).toHaveTextContent('pending');
    await user.click(within(panel).getByRole('button', { name: 'Check status' }));
    expect(await within(panel).findByLabelText('Inspection receipt status')).toHaveTextContent('approved');
    expect(JSON.stringify(vi.mocked(decisionApi.createOutcomeFit).mock.lastCall?.[0])).not.toContain('ticket_id');
    await user.click(screen.getByRole('button', { name: 'Load overview' }));
    await waitFor(() => expect(screen.getByLabelText('Outcome model candidate review'))
      .toContainElement(screen.getByRole('textbox', { name: 'Saved review screen verification code' })));
    expect(screen.getByRole('textbox', { name: 'Saved review screen verification code' })).toHaveValue('');
    expect(screen.queryByLabelText('Inspection receipt status')).not.toBeInTheDocument();
  });

  // Regression: the fit and its review screen showed a stored prediction with
  // no indication of which simulator produced it.
  it('flags an outcome fit and screen whose scored run predates the installed engine', async () => {
    vi.mocked(decisionApi.loadOutcomeFit).mockResolvedValueOnce({ fit: {
      ...outcomeFit, engine_matches_current: false,
      limitations: [...outcomeFit.limitations, 'The scored run was produced by an earlier simulator version.'],
    } });
    vi.mocked(decisionApi.saveOutcomeScreen).mockResolvedValueOnce({ screen: {
      ...outcomeScreen, engine_matches_current: false,
    } });
    const user = userEvent.setup();
    renderWithProviders(<DecisionLabPage />);
    await user.type(screen.getByRole('textbox', { name: 'Organization ID' }), 'tenant-a');
    await user.type(screen.getByRole('textbox', { name: 'Access scope' }), 'private');
    await user.click(screen.getByRole('button', { name: 'Load overview' }));
    const panel = await screen.findByLabelText('Outcome capacity fit and human inspection');
    await user.type(within(panel).getByRole('textbox', { name: 'Fit ID' }), 'fit-1');
    await user.click(within(panel).getByRole('button', { name: 'Load fit' }));
    const fit = await within(panel).findByLabelText('Loaded capacity fit');
    expect(within(fit).getByText('Computed by an earlier engine')).toBeInTheDocument();
    await user.click(within(panel).getByRole('button', { name: 'Save screen' }));
    const saved = await within(panel).findByLabelText('Loaded model review screen');
    expect(within(saved).getByText('Computed by an earlier engine')).toBeInTheDocument();
  });

  it('rejects a screen linked to a different fit and does not prefill the candidate', async () => {
    vi.mocked(decisionApi.loadOutcomeScreen).mockResolvedValueOnce({ screen: {
      ...outcomeScreen, fit_record_sha256: '8'.repeat(64),
    } });
    const user = userEvent.setup();
    renderWithProviders(<DecisionLabPage />);
    await user.type(screen.getByRole('textbox', { name: 'Organization ID' }), 'tenant-a');
    await user.type(screen.getByRole('textbox', { name: 'Access scope' }), 'private');
    await user.click(screen.getByRole('button', { name: 'Load overview' }));
    const panel = await screen.findByLabelText('Outcome capacity fit and human inspection');
    await user.type(within(panel).getByRole('textbox', { name: 'Saved screen verification code' }), outcomeScreen.replay_hash);
    await user.click(within(panel).getByRole('button', { name: 'Load screen' }));
    expect(await within(panel).findByRole('alert')).toHaveTextContent('does not match its requested saved record');
    expect(within(panel).queryByLabelText('Loaded model review screen')).not.toBeInTheDocument();
    expect(within(screen.getByLabelText('Outcome model candidate review'))
      .getByRole('textbox', { name: 'Saved review screen verification code' })).toHaveValue('');
  });

  it('keeps failed screens ineligible and clears fit state when the overview changes', async () => {
    vi.mocked(decisionApi.saveOutcomeScreen).mockResolvedValueOnce({ screen: {
      ...outcomeScreen, report: { ...outcomeScreen.report,
        eligible_for_human_review: false, local_run_precedes_window: false,
        failed_checks: ['run_not_committed_before_observation_window'] },
    } });
    vi.mocked(decisionApi.overview).mockResolvedValueOnce(overview).mockResolvedValueOnce({
      ...overview, invalidated_inputs: overview.invalidated_inputs + 1,
    });
    const user = userEvent.setup();
    renderWithProviders(<DecisionLabPage />);
    await user.type(screen.getByRole('textbox', { name: 'Organization ID' }), 'tenant-a');
    await user.type(screen.getByRole('textbox', { name: 'Access scope' }), 'private');
    await user.click(screen.getByRole('button', { name: 'Load overview' }));
    const panel = await screen.findByLabelText('Outcome capacity fit and human inspection');
    await user.type(within(panel).getByRole('textbox', { name: 'Fit ID' }), 'fit-1');
    await user.click(within(panel).getByRole('button', { name: 'Load fit' }));
    expect(await within(panel).findByLabelText('Loaded capacity fit')).toBeInTheDocument();
    await user.click(within(panel).getByRole('button', { name: 'Save screen' }));
    const saved = await within(panel).findByLabelText('Loaded model review screen');
    expect(within(saved).getByText('Screen checks failed')).toBeInTheDocument();
    expect(within(panel).getByRole('button', { name: 'Request inspection' })).toBeDisabled();
    expect(within(saved).queryByRole('button', { name: 'Use for candidate' })).not.toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Load overview' }));
    await waitFor(() => expect(screen.queryByLabelText('Loaded model review screen')).not.toBeInTheDocument());
  });

  it('rejects a mismatched candidate response before the comparison can be saved', async () => {
    vi.mocked(decisionApi.createCandidateModel).mockResolvedValueOnce({ candidate: {
      ...candidateModel, id: 'different-candidate', model: { ...candidateModel.model, version: 'different-candidate' },
    } });
    const user = userEvent.setup();
    renderWithProviders(<DecisionLabPage />);
    await user.type(screen.getByRole('textbox', { name: 'Organization ID' }), 'tenant-a');
    await user.type(screen.getByRole('textbox', { name: 'Access scope' }), 'private');
    await user.click(screen.getByRole('button', { name: 'Load overview' }));
    const panel = await screen.findByLabelText('Outcome model candidate review');
    await user.type(within(panel).getByRole('textbox', { name: 'Candidate ID' }), 'candidate-capacity-1');
    await user.type(within(panel).getByRole('textbox', { name: 'Saved review screen verification code' }), 'a'.repeat(64));
    await user.click(within(panel).getByRole('button', { name: 'Save candidate' }));
    expect(await within(panel).findByRole('alert')).toHaveTextContent('does not match the requested saved record');
    expect(within(panel).queryByLabelText('Loaded candidate')).not.toBeInTheDocument();
    expect(within(panel).getByRole('button', { name: 'Save comparison' })).toBeDisabled();
  });

  it('reloads verified candidate, comparison, and score IDs and clears them after an overview change', async () => {
    const user = userEvent.setup();
    vi.mocked(decisionApi.overview).mockResolvedValueOnce(overview).mockResolvedValueOnce({
      ...overview, invalidated_inputs: overview.invalidated_inputs + 1,
    });
    renderWithProviders(<DecisionLabPage />);
    await user.type(screen.getByRole('textbox', { name: 'Organization ID' }), 'tenant-a');
    await user.type(screen.getByRole('textbox', { name: 'Access scope' }), 'private');
    await user.click(screen.getByRole('button', { name: 'Load overview' }));
    const panel = await screen.findByLabelText('Outcome model candidate review');
    await user.type(within(panel).getByRole('textbox', { name: 'Candidate ID' }), 'candidate-capacity-1');
    await user.click(within(panel).getByRole('button', { name: 'Load candidate' }));
    expect(await within(panel).findByLabelText('Loaded candidate')).toBeInTheDocument();
    await user.type(within(panel).getByRole('textbox', { name: 'Saved comparison verification code' }), candidateRun.replay_hash);
    await user.click(within(panel).getByRole('button', { name: 'Load comparison' }));
    expect(await within(panel).findByLabelText('Loaded candidate comparison')).toBeInTheDocument();
    await user.type(within(panel).getByRole('textbox', { name: 'Saved score verification code' }), candidateScore.replay_hash);
    await user.click(within(panel).getByRole('button', { name: 'Load score' }));
    expect(await within(panel).findByLabelText('Loaded candidate score')).toBeInTheDocument();
    expect(decisionApi.loadCandidateRun).toHaveBeenCalledWith({ tenant_id: 'tenant-a', acl: 'private',
      replay_hash: candidateRun.replay_hash });
    expect(decisionApi.loadCandidateScore).toHaveBeenCalledWith({ tenant_id: 'tenant-a', acl: 'private',
      replay_hash: candidateScore.replay_hash });
    await user.click(screen.getByRole('button', { name: 'Load overview' }));
    await waitFor(() => expect(screen.queryByLabelText('Loaded candidate score')).not.toBeInTheDocument());
  });

  it('marks a truncated source-version list instead of showing it as complete', async () => {
    vi.mocked(decisionApi.loadCandidateRun).mockResolvedValueOnce({ run: {
      ...candidateRun, source_version_hashes_total: 42,
    } });
    const user = userEvent.setup();
    renderWithProviders(<DecisionLabPage />);
    await user.type(screen.getByRole('textbox', { name: 'Organization ID' }), 'tenant-a');
    await user.type(screen.getByRole('textbox', { name: 'Access scope' }), 'private');
    await user.click(screen.getByRole('button', { name: 'Load overview' }));
    const panel = await screen.findByLabelText('Outcome model candidate review');
    await user.type(within(panel).getByRole('textbox', { name: 'Candidate ID' }), 'candidate-capacity-1');
    await user.click(within(panel).getByRole('button', { name: 'Load candidate' }));
    expect(await within(panel).findByLabelText('Loaded candidate')).toBeInTheDocument();
    await user.type(within(panel).getByRole('textbox', { name: 'Saved comparison verification code' }), candidateRun.replay_hash);
    await user.click(within(panel).getByRole('button', { name: 'Load comparison' }));
    const loaded = await within(panel).findByLabelText('Loaded candidate comparison');
    expect(loaded).toHaveTextContent('(1 / 42)');
  });

  // Regression: a stored run recorded by an earlier simulator rendered exactly
  // like one the installed engine still reproduces. Loading never recomputes
  // such a run, so the badge is the only thing that can say so.
  it('flags a candidate comparison whose stored simulation predates the installed engine', async () => {
    vi.mocked(decisionApi.loadCandidateRun).mockResolvedValueOnce({ run: {
      ...candidateRun, parent: { ...candidateRun.parent, engine_matches_current: false },
    } });
    const user = userEvent.setup();
    renderWithProviders(<DecisionLabPage />);
    await user.type(screen.getByRole('textbox', { name: 'Organization ID' }), 'tenant-a');
    await user.type(screen.getByRole('textbox', { name: 'Access scope' }), 'private');
    await user.click(screen.getByRole('button', { name: 'Load overview' }));
    const panel = await screen.findByLabelText('Outcome model candidate review');
    await user.type(within(panel).getByRole('textbox', { name: 'Saved comparison verification code' }), candidateRun.replay_hash);
    await user.click(within(panel).getByRole('button', { name: 'Load comparison' }));
    const loaded = await within(panel).findByLabelText('Loaded candidate comparison');
    expect(within(loaded).getByText('Computed by an earlier engine')).toBeInTheDocument();
  });

  it('keeps a currently reproducible candidate comparison unbadged', async () => {
    const user = userEvent.setup();
    renderWithProviders(<DecisionLabPage />);
    await user.type(screen.getByRole('textbox', { name: 'Organization ID' }), 'tenant-a');
    await user.type(screen.getByRole('textbox', { name: 'Access scope' }), 'private');
    await user.click(screen.getByRole('button', { name: 'Load overview' }));
    const panel = await screen.findByLabelText('Outcome model candidate review');
    await user.type(within(panel).getByRole('textbox', { name: 'Saved comparison verification code' }), candidateRun.replay_hash);
    await user.click(within(panel).getByRole('button', { name: 'Load comparison' }));
    const loaded = await within(panel).findByLabelText('Loaded candidate comparison');
    expect(within(loaded).queryByText('Computed by an earlier engine')).not.toBeInTheDocument();
  });

  it('refuses to show a saved score whose strict-win flag contradicts its exact error sums', async () => {
    vi.mocked(decisionApi.loadCandidateScore).mockResolvedValueOnce({ score: {
      ...candidateScore, candidate_backlog_error_below_parent: false,
    } });
    const user = userEvent.setup();
    renderWithProviders(<DecisionLabPage />);
    await user.type(screen.getByRole('textbox', { name: 'Organization ID' }), 'tenant-a');
    await user.type(screen.getByRole('textbox', { name: 'Access scope' }), 'private');
    await user.click(screen.getByRole('button', { name: 'Load overview' }));
    const panel = await screen.findByLabelText('Outcome model candidate review');
    await user.type(within(panel).getByRole('textbox', { name: 'Saved score verification code' }), candidateScore.replay_hash);
    await user.click(within(panel).getByRole('button', { name: 'Load score' }));
    expect(await within(panel).findByRole('alert')).toHaveTextContent('does not match the requested saved record');
    expect(within(panel).queryByLabelText('Loaded candidate score')).not.toBeInTheDocument();
  });

  it('does not attribute another candidate score to the currently loaded candidate', async () => {
    const otherRunHash = '9'.repeat(64);
    const otherRunSha = '8'.repeat(64);
    const otherScoreHash = '7'.repeat(64);
    vi.mocked(decisionApi.loadCandidateScore).mockResolvedValueOnce({ score: {
      ...candidateScore, replay_hash: otherScoreHash, comparison_run_hash: otherRunHash,
      comparison_run_sha256: otherRunSha,
    } });
    vi.mocked(decisionApi.loadCandidateRun).mockResolvedValueOnce({ run: {
      ...candidateRun, replay_hash: otherRunHash, record_sha256: otherRunSha,
      candidate_id: 'candidate-capacity-other', candidate_record_sha256: '1'.repeat(64),
      candidate: { ...candidateRun.candidate, model_version: 'candidate-capacity-other' },
    } });
    const user = userEvent.setup();
    renderWithProviders(<DecisionLabPage />);
    await user.type(screen.getByRole('textbox', { name: 'Organization ID' }), 'tenant-a');
    await user.type(screen.getByRole('textbox', { name: 'Access scope' }), 'private');
    await user.click(screen.getByRole('button', { name: 'Load overview' }));
    const panel = await screen.findByLabelText('Outcome model candidate review');
    await user.type(within(panel).getByRole('textbox', { name: 'Candidate ID' }), 'candidate-capacity-1');
    await user.click(within(panel).getByRole('button', { name: 'Load candidate' }));
    expect(await within(panel).findByLabelText('Loaded candidate')).toBeInTheDocument();
    await user.type(within(panel).getByRole('textbox', { name: 'Saved score verification code' }), otherScoreHash);
    await user.click(within(panel).getByRole('button', { name: 'Load score' }));
    expect(await within(panel).findByRole('alert')).toHaveTextContent('does not match the requested saved record');
    expect(within(panel).queryByLabelText('Loaded candidate score')).not.toBeInTheDocument();
  });

  it('creates a synthetic pilot, compares scenarios, and verifies both replay trajectories', async () => {
    const user = userEvent.setup();
    renderWithProviders(<DecisionLabPage />);
    await user.type(screen.getByRole('textbox', { name: 'Organization ID' }), 'tenant-a');
    await user.type(screen.getByRole('textbox', { name: 'Access scope' }), 'private');
    await user.click(screen.getByRole('button', { name: 'Load overview' }));
    await user.click(await screen.findByRole('button', { name: 'Create pilot' }));
    await waitFor(() => expect(decisionApi.createSyntheticPilot).toHaveBeenCalledWith(
      { tenant_id: 'tenant-a', acl: 'private' }, { seed: 23, days: 30 },
    ));
    expect(await screen.findByText(/synthetic-source-1/)).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Run comparison' }));
    await waitFor(() => expect(decisionApi.compare).toHaveBeenCalledWith({
      tenant_id: 'tenant-a', acl: 'private', snapshot_id: 'snapshot-1', model_version: 'model-1',
      baseline_scenario_id: 'scenario-base', alternative_scenario_id: 'scenario-alt',
    }));
    expect(await screen.findByRole('heading', { name: 'Scenario comparison' })).toBeInTheDocument();
    expect(screen.getByRole('spinbutton', { name: 'Maximum total added agent-days' })).toHaveValue(90);
    expect(screen.getByText(/-4/)).toBeInTheDocument();
    expect(screen.getByText('source-sha')).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Verify both replays' }));
    await waitFor(() => expect(decisionApi.replay).toHaveBeenCalledTimes(2));
    expect(await screen.findByText('Both simulations reproduced the expected verification codes.')).toBeInTheDocument();
    expect(screen.getByRole('heading', { name: 'Daily simulation trajectories' })).toBeInTheDocument();
    expect(screen.getAllByText('12').length).toBeGreaterThan(0);
    expect(screen.getAllByText('8').length).toBeGreaterThan(0);
  });

  // Regression: the brief's KPI totals and their delta crossed as JSON
  // numbers, so a sum past Number.MAX_SAFE_INTEGER was already rounded by the
  // time it reached this table. They are decimal strings now and must be
  // formatted through BigInt, exactly, with the delta signed.
  it('renders scenario totals past 2^53 exactly', async () => {
    const user = userEvent.setup();
    vi.mocked(decisionApi.compare).mockResolvedValue({ brief: { ...brief,
      baseline: { ...brief.baseline, total_resolved: '9007199254740993' },
      alternative: { ...brief.alternative, total_resolved: '9007199254740994' },
      delta: { ...brief.delta, total_resolved: '1', final_backlog: '-9007199254740993' },
    } } as never);
    renderWithProviders(<DecisionLabPage />);
    await user.type(screen.getByRole('textbox', { name: 'Organization ID' }), 'tenant-a');
    await user.type(screen.getByRole('textbox', { name: 'Access scope' }), 'private');
    await user.click(screen.getByRole('button', { name: 'Load overview' }));
    await user.click(await screen.findByRole('button', { name: 'Run comparison' }));
    expect(await screen.findByText('9,007,199,254,740,993')).toBeInTheDocument();
    expect(screen.getByText('9,007,199,254,740,994')).toBeInTheDocument();
    expect(screen.getByText('-9,007,199,254,740,993')).toBeInTheDocument();
    expect(screen.getByText('+1')).toBeInTheDocument();
  });

  it('replays the selected synthetic ticket-event pair and removes the result after selection changes', async () => {
    const user = userEvent.setup();
    renderWithProviders(<DecisionLabPage />);
    await user.type(screen.getByRole('textbox', { name: 'Organization ID' }), 'tenant-a');
    await user.type(screen.getByRole('textbox', { name: 'Access scope' }), 'private');
    await user.click(screen.getByRole('button', { name: 'Load overview' }));
    await user.click(await screen.findByRole('button', { name: 'Run comparison' }));
    await user.click(await screen.findByRole('button', { name: 'Run ticket-event comparison' }));
    await waitFor(() => expect(decisionApi.eventCompare).toHaveBeenCalledWith({
      tenant_id: 'tenant-a', acl: 'private', snapshot_id: 'snapshot-1', model_version: 'model-1',
      baseline_scenario_id: 'scenario-base', alternative_scenario_id: 'scenario-alt',
    }));
    const panel = screen.getByLabelText('Ticket-event FIFO comparison');
    expect(within(panel).getByText('Synthetic event times and service rate.')).toBeInTheDocument();
    expect(within(panel).getByRole('row', { name: /Resolved-ticket wait p95/ })).toHaveTextContent('120');
    expect(within(panel).getByRole('row', { name: /Resolved-ticket wait p95/ })).toHaveTextContent('90');
    await user.selectOptions(screen.getByRole('combobox', { name: 'Alternative scenario' }), 'scenario-base');
    expect(screen.queryByText('Synthetic event times and service rate.')).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Run ticket-event comparison' })).not.toBeInTheDocument();
  });

  it('labels uploaded event evidence separately and refuses a mismatched scenario response', async () => {
    const user = userEvent.setup();
    vi.mocked(decisionApi.catalog).mockResolvedValue(uploadedCatalog);
    vi.mocked(decisionApi.eventCompare).mockResolvedValueOnce({ report: {
      ...eventReport, status: 'operator_supplied_exploratory', source_origin: 'uploaded',
      baseline: { ...eventReport.baseline, scenario_id: 'uploaded-baseline' },
      alternative: { ...eventReport.alternative, scenario_id: 'uploaded-alternative' },
      limitations: ['Operator source identity is unverified.'],
    } } as never).mockResolvedValueOnce({ report: {
      ...eventReport, status: 'operator_supplied_exploratory', source_origin: 'uploaded',
      baseline: { ...eventReport.baseline, scenario_id: 'wrong-baseline' },
      alternative: { ...eventReport.alternative, scenario_id: 'uploaded-alternative' },
    } } as never);
    renderWithProviders(<DecisionLabPage />);
    await user.type(screen.getByRole('textbox', { name: 'Organization ID' }), 'tenant-a');
    await user.type(screen.getByRole('textbox', { name: 'Access scope' }), 'private');
    await user.click(screen.getByRole('button', { name: 'Load overview' }));
    await user.click(await screen.findByRole('button', { name: 'Run comparison' }));
    await user.click(await screen.findByRole('button', { name: 'Run ticket-event comparison' }));
    const panel = screen.getByLabelText('Ticket-event FIFO comparison');
    expect(await within(panel).findByText('Operator source identity is unverified.')).toBeInTheDocument();
    expect(within(panel).getByText('Operator-provided local source')).toBeInTheDocument();
    expect(decisionApi.eventCompare).toHaveBeenCalledWith({
      tenant_id: 'tenant-a', acl: 'private', snapshot_id: 'uploaded-snapshot', model_version: 'uploaded-model',
      baseline_scenario_id: 'uploaded-baseline', alternative_scenario_id: 'uploaded-alternative',
    });
    await user.click(within(panel).getByRole('button', { name: 'Run ticket-event comparison' }));
    expect(await within(panel).findByRole('alert')).toHaveTextContent('does not match the selected pilot pair');
    expect(within(panel).queryByText('Operator source identity is unverified.')).not.toBeInTheDocument();
  });

  it('attaches the selected event pair to a source-verified empirical brief without sending ticket rows', async () => {
    const user = userEvent.setup();
    vi.mocked(decisionApi.catalog).mockResolvedValue({ ...catalog,
      snapshots: [{ ...catalog.snapshots[0], horizon_days: 35 }],
      scenarios: catalog.scenarios.map((scenario) => ({ ...scenario, horizon_days: 35 })),
    });
    renderWithProviders(<DecisionLabPage />);
    await user.type(screen.getByRole('textbox', { name: 'Organization ID' }), 'tenant-a');
    await user.type(screen.getByRole('textbox', { name: 'Access scope' }), 'private');
    await user.click(screen.getByRole('button', { name: 'Load overview' }));
    await user.click(await screen.findByRole('button', { name: 'Run comparison' }));
    await user.click(await screen.findByRole('button', { name: 'Run ticket-event comparison' }));
    await user.click(await screen.findByRole('button', { name: 'Run empirical resampling' }));
    const include = await screen.findByRole('checkbox', { name: 'Attach this ticket-event pair to the empirical brief' });
    await user.click(include);
    const eventEvidence = { source_sha256: eventReport.source_sha256, window_start_utc: '2026-01-01T00:00:00Z',
      daily_engine_sha256: 'e'.repeat(64), event_engine_sha256: eventReport.event_engine_sha256,
      config: { shift_start_seconds: 32_400, shift_seconds: 28_800 },
      baseline_run_hash: eventReport.baseline.replay_hash, baseline_run_sha256: 'f'.repeat(64),
      alternative_run_hash: eventReport.alternative.replay_hash, alternative_run_sha256: '1'.repeat(64),
      baseline: { final_backlog: '12', total_resolved: '20', resolved_within_sla: '10',
        total_staff_cost_cents: '5000', resolved_wait_seconds_p50: '3600', resolved_wait_seconds_p95: '7200' },
      alternative: { final_backlog: '8', total_resolved: '24', resolved_within_sla: '14',
        total_staff_cost_cents: '7000', resolved_wait_seconds_p50: '1800', resolved_wait_seconds_p95: '5400' },
      resolved_wait_seconds_p95_delta: '-1800' };
    vi.mocked(decisionApi.compare).mockResolvedValueOnce({ brief: { ...brief,
      exploratory_empirical: { run_id: 'run-47', run_sha256: 'stored-run-sha', fit_id: 'fit-47',
        fit_sha256: 'stored-fit-sha', source_origin: 'synthetic_fixture', report: {
          replay_hash: 'run-hash', common_draws_sha256: 'draws-hash', runs: 100,
          delta_backlog_p05: -20, delta_backlog_p50: -10, delta_backlog_p95: 5,
          delta_sla_resolved_p05: 1, delta_sla_resolved_p50: 3, delta_sla_resolved_p95: 8,
          baseline_joint_constraint_violation_bps: 5_000,
          alternative_joint_constraint_violation_bps: 3_000 } },
      exploratory_event: eventEvidence,
    } } as never);
    await user.click(screen.getByRole('button', { name: 'Compose verified brief' }));
    await waitFor(() => expect(decisionApi.compare).toHaveBeenLastCalledWith({
      tenant_id: 'tenant-a', acl: 'private', snapshot_id: 'snapshot-1', model_version: 'model-1',
      baseline_scenario_id: 'scenario-base', alternative_scenario_id: 'scenario-alt',
      empirical_run_id: 'run-47', baseline_event_replay_hash: eventReport.baseline.replay_hash,
      alternative_event_replay_hash: eventReport.alternative.replay_hash,
    }));
    const evidence = await screen.findByLabelText('Linked ticket-event replay evidence');
    expect(within(evidence).getByText(/09:00 UTC/)).toBeInTheDocument();
    expect(within(evidence).getByText(/60 \/ 30/)).toBeInTheDocument();
    expect(within(evidence).getByText(/120 \/ 90/)).toBeInTheDocument();
    expect(JSON.stringify(vi.mocked(decisionApi.compare).mock.lastCall?.[0])).not.toContain('ticket_id');
    await user.click(screen.getByRole('button', { name: 'Run ticket-event comparison' }));
    await waitFor(() => expect(screen.getByRole('checkbox', {
      name: 'Attach this ticket-event pair to the empirical brief',
    })).not.toBeChecked());
    expect(screen.queryByLabelText('Linked ticket-event replay evidence')).not.toBeInTheDocument();
  });

  it('attaches stored historical forecast validation alone and clears it on rerun', async () => {
    const user = userEvent.setup();
    vi.mocked(decisionApi.catalog).mockResolvedValue({ ...catalog,
      snapshots: [{ ...catalog.snapshots[0], horizon_days: 35 }],
      scenarios: catalog.scenarios.map((scenario) => ({ ...scenario, horizon_days: 35 })),
    });
    renderWithProviders(<DecisionLabPage />);
    await user.type(screen.getByRole('textbox', { name: 'Organization ID' }), 'tenant-a');
    await user.type(screen.getByRole('textbox', { name: 'Access scope' }), 'private');
    await user.click(screen.getByRole('button', { name: 'Load overview' }));
    await user.click(await screen.findByRole('button', { name: 'Run comparison' }));
    await user.click(await screen.findByRole('button', { name: 'Validate historical forecasts' }));
    await waitFor(() => expect(decisionApi.forecastValidation).toHaveBeenCalledWith({
      tenant_id: 'tenant-a', acl: 'private', snapshot_id: 'snapshot-1', model_version: 'model-1',
      baseline_scenario_id: 'scenario-base', alternative_scenario_id: 'scenario-alt',
    }));
    const panel = screen.getByLabelText('Historical one-step forecast validation');
    expect(within(panel).getByText('Historical observations only.')).toBeInTheDocument();
    expect(within(panel).getByText('50%')).toBeInTheDocument();
    expect(within(panel).getByText(/Arrival absolute error sum: 9/)).toBeInTheDocument();
    expect(within(panel).getByText(/Strictly lower backlog error than all three baselines: Yes/)).toBeInTheDocument();
    await user.click(await screen.findByRole('checkbox', { name: 'Attach this stored historical forecast validation' }));
    vi.mocked(decisionApi.compare).mockResolvedValueOnce({ brief: { ...brief,
      exploratory_forecast: { record_id: forecastReport.record_id, record_sha256: forecastReport.record_sha256,
        source_sha256: forecastReport.source_sha256, calibration_engine_sha256: 'c'.repeat(64),
        window_start_utc: '2026-01-01T00:00:00Z', calibration_points: 14, forecast_points: 16,
        // Past 2^53: `Number` rounds this to …992, `BigInt` does not.
        arrival_abs_error_sum: '9', model_abs_error_sum: '9007199254740993',
        no_change_abs_error_sum: '18',
        seasonal_naive_abs_error_sum: '15', mean_change_abs_error_sum: '14',
        model_beats_all_baselines: true,
        interval_evaluated_points: 2, rolling_observed_coverage_basis_points: 5_000,
        fixed_observed_coverage_basis_points: 10_000 },
    } } as never);
    await user.click(screen.getByRole('button', { name: 'Compose verified brief' }));
    await waitFor(() => expect(decisionApi.compare).toHaveBeenLastCalledWith({
      tenant_id: 'tenant-a', acl: 'private', snapshot_id: 'snapshot-1', model_version: 'model-1',
      baseline_scenario_id: 'scenario-base', alternative_scenario_id: 'scenario-alt',
      forecast_validation_id: 'forecast-record-1',
    }));
    const evidence = await screen.findByLabelText('Linked historical one-step forecast validation');
    // Regression: the brief shipped these sums as JSON numbers, so a value
    // past 2^53 rounded before it ever reached the table.
    expect(within(evidence).getByText('9,007,199,254,740,993')).toBeInTheDocument();
    expect(within(evidence).getByText(/50.00%/)).toBeInTheDocument();
    expect(within(evidence).getByText(/forecast-record-1/)).toBeInTheDocument();
    expect(within(evidence).getByText(/Strictly lower backlog error than all three baselines: Yes/)).toBeInTheDocument();
    expect(within(screen.getByLabelText('Verified evidence brief')).getByText('Synthetic')).toBeInTheDocument();
    expect(JSON.stringify(vi.mocked(decisionApi.compare).mock.lastCall?.[0])).not.toContain('ticket_id');
    await user.click(within(panel).getByRole('button', { name: 'Validate historical forecasts' }));
    await waitFor(() => expect(screen.getByRole('checkbox', {
      name: 'Attach this stored historical forecast validation',
    })).not.toBeChecked());
    expect(screen.queryByLabelText('Linked historical one-step forecast validation')).not.toBeInTheDocument();
  });

  it('shows an unavailable uploaded validation without allowing evidence attachment', async () => {
    const user = userEvent.setup();
    vi.mocked(decisionApi.catalog).mockResolvedValue(uploadedCatalog);
    vi.mocked(decisionApi.forecastValidation).mockResolvedValueOnce({ report: {
      ...forecastReport, status: 'operator_supplied_exploratory', source_origin: 'uploaded',
      snapshot_id: 'uploaded-snapshot', model_version: 'uploaded-model',
      baseline_scenario_id: 'uploaded-baseline', alternative_scenario_id: 'uploaded-alternative',
      record_id: null, record_sha256: null, forecast_evaluation_days: null,
      arrival_abs_error_sum: null, model_abs_error_sum: null, no_change_abs_error_sum: null,
      seasonal_naive_abs_error_sum: null, mean_change_abs_error_sum: null,
      model_beats_all_baselines: null,
      rolling_interval_evaluated_points: null, rolling_observed_coverage_basis_points: null,
      fixed_interval_evaluated_points: null, fixed_observed_coverage_basis_points: null,
      unavailable_reason: 'Fewer than 15 observed days.',
    } } as never);
    renderWithProviders(<DecisionLabPage />);
    await user.type(screen.getByRole('textbox', { name: 'Organization ID' }), 'tenant-a');
    await user.type(screen.getByRole('textbox', { name: 'Access scope' }), 'private');
    await user.click(screen.getByRole('button', { name: 'Load overview' }));
    await user.click(await screen.findByRole('button', { name: 'Run comparison' }));
    await user.click(await screen.findByRole('button', { name: 'Validate historical forecasts' }));
    const panel = screen.getByLabelText('Historical one-step forecast validation');
    expect(await within(panel).findByText(/Fewer than 15 observed days/)).toBeInTheDocument();
    expect(within(panel).getByText('Operator-provided local source')).toBeInTheDocument();
    expect(screen.queryByRole('checkbox', { name: 'Attach this stored historical forecast validation' })).not.toBeInTheDocument();
    expect(decisionApi.compare).toHaveBeenCalledTimes(1);
  });

  it('attaches an uploaded pilot stored SLA holdout without requiring empirical draws or ticket rows', async () => {
    const user = userEvent.setup();
    const sourceSha = 'a'.repeat(64);
    const holdoutSha = 'b'.repeat(64);
    vi.mocked(decisionApi.catalog).mockResolvedValue({ ...uploadedCatalog,
      snapshots: [{ ...uploadedCatalog.snapshots[0], horizon_days: 21 }],
      scenarios: uploadedCatalog.scenarios.map((scenario) => ({ ...scenario, horizon_days: 21 })),
      uploaded_pilots: [{ ...uploadedCatalog.uploaded_pilots[0], source_sha256: sourceSha,
        sla_holdout_id: 'uploaded-sla-holdout-1', sla_holdout_sha256: holdoutSha }],
    });
    renderWithProviders(<DecisionLabPage />);
    await user.type(screen.getByRole('textbox', { name: 'Organization ID' }), 'tenant-a');
    await user.type(screen.getByRole('textbox', { name: 'Access scope' }), 'private');
    await user.click(screen.getByRole('button', { name: 'Load overview' }));
    await user.click(await screen.findByRole('button', { name: 'Run comparison' }));
    const include = await screen.findByRole('checkbox', { name: "Attach this uploaded pilot's stored SLA holdout" });
    await user.click(include);
    vi.mocked(decisionApi.compare).mockResolvedValueOnce({ brief: { ...brief,
      exploratory_sla_holdout: { record_id: 'uploaded-sla-holdout-1', record_sha256: holdoutSha,
        source_sha256: sourceSha, engine_sha256: 'c'.repeat(64), diagnostic: {
          // The brief now embeds the same wire mirror the engineering
          // validation endpoint returns: u128 error sums cross as decimal
          // strings, bounded per-day counts stay JSON numbers.
          training_days: 14, holdout_days: 7, observed_total_within_sla: 20,
          predicted_total_within_sla: 18, daily_abs_error_sum: '9007199254740993',
          no_change_abs_error_sum: '12', seasonal_naive_abs_error_sum: '9',
          training_mean_abs_error_sum: '11',
          conditional_model_beats_all_baselines: false,
          limitations: ['Observed later demand was supplied to the check.'],
        } },
    } } as never);
    await user.click(screen.getByRole('button', { name: 'Compose verified brief' }));
    await waitFor(() => expect(decisionApi.compare).toHaveBeenLastCalledWith({
      tenant_id: 'tenant-a', acl: 'private', snapshot_id: 'uploaded-snapshot', model_version: 'uploaded-model',
      baseline_scenario_id: 'uploaded-baseline', alternative_scenario_id: 'uploaded-alternative',
      sla_holdout_id: 'uploaded-sla-holdout-1',
    }));
    const evidence = await screen.findByLabelText('Linked historical SLA holdout');
    expect(within(evidence).getByText('Observed later demand was supplied to the check.')).toBeInTheDocument();
    expect(within(evidence).getByText(/20 \/ 18/)).toBeInTheDocument();
    // The holdout error sum is a u128 on the server: it must render exactly
    // rather than through a rounding `Number`.
    expect(within(evidence).getByText('9,007,199,254,740,993')).toBeInTheDocument();
    expect(JSON.stringify(vi.mocked(decisionApi.compare).mock.lastCall?.[0])).not.toContain('ticket_id');
  });

  it('runs source-bound empirical resampling from the selected pilot and clears stale results on input changes', async () => {
    const user = userEvent.setup();
    vi.mocked(decisionApi.catalog).mockResolvedValue({
      ...catalog,
      snapshots: [{ ...catalog.snapshots[0], horizon_days: 35 }],
      scenarios: catalog.scenarios.map((scenario) => ({ ...scenario, horizon_days: 35 })),
    });
    renderWithProviders(<DecisionLabPage />);
    await user.type(screen.getByRole('textbox', { name: 'Organization ID' }), 'tenant-a');
    await user.type(screen.getByRole('textbox', { name: 'Access scope' }), 'private');
    await user.click(screen.getByRole('button', { name: 'Load overview' }));
    await user.click(await screen.findByRole('button', { name: 'Run comparison' }));
    await user.click(await screen.findByRole('button', { name: 'Run empirical resampling' }));
    await waitFor(() => expect(decisionApi.empiricalResampling).toHaveBeenCalledWith(expect.objectContaining({
      tenant_id: 'tenant-a', acl: 'private', snapshot_id: 'snapshot-1', model_version: 'model-1',
      baseline_scenario_id: 'scenario-base', alternative_scenario_id: 'scenario-alt',
      training_days: 14, min_saturated_days: 7, runs: 100, arrival_block_days: 7,
      sampling_mode: 'independent', max_final_backlog: 105, max_staff_cost_cents: 1_050_000,
      min_sla_resolved: 525,
    })));
    expect(await screen.findByText(/run-47/)).toBeInTheDocument();
    expect(screen.getByText(/draws-hash/)).toBeInTheDocument();
    expect(screen.getByText(/Observed saturated days/)).toBeInTheDocument();
    vi.mocked(decisionApi.compare).mockResolvedValueOnce({ brief: { ...brief,
      source_version_hashes: ['source-sha'],
      exploratory_empirical: { run_id: 'run-47', run_sha256: 'stored-run-sha',
        fit_id: 'fit-47', fit_sha256: 'stored-fit-sha', report: {
          replay_hash: 'run-hash', common_draws_sha256: 'draws-hash', runs: 100,
          delta_backlog_p05: -20, delta_backlog_p50: -10, delta_backlog_p95: 5,
          delta_sla_resolved_p05: 1, delta_sla_resolved_p50: 3, delta_sla_resolved_p95: 8,
          baseline_joint_constraint_violation_bps: 5000,
          alternative_joint_constraint_violation_bps: 3000 } },
      exploratory_policy_screen: null } } as never);
    await user.click(screen.getByRole('button', { name: 'Compose verified brief' }));
    await waitFor(() => expect(decisionApi.compare).toHaveBeenLastCalledWith(expect.objectContaining({
      snapshot_id: 'snapshot-1', model_version: 'model-1', empirical_run_id: 'run-47',
    })));
    expect(await screen.findByText(/stored-run-sha/)).toBeInTheDocument();
    await user.clear(screen.getByRole('spinbutton', { name: 'Training-prefix days' }));
    await user.type(screen.getByRole('spinbutton', { name: 'Training-prefix days' }), '12');
    expect(screen.queryByText(/run-47/)).not.toBeInTheDocument();
    expect(screen.queryByText(/stored-run-sha/)).not.toBeInTheDocument();
    await user.click(screen.getByRole('checkbox', { name: 'Apply a human-review resource and joint-risk screen' }));
    vi.mocked(decisionApi.empiricalResampling).mockResolvedValueOnce({ ...empiricalReport,
      screen: { choice: 'baseline_only', replay_hash: 'screen-hash',
        failed_alternative_checks: ['alternative_joint_risk_worse_than_baseline'],
        baseline_deterministic_feasible: true, alternative_deterministic_feasible: false } } as never);
    await user.click(screen.getByRole('button', { name: 'Run empirical resampling' }));
    await waitFor(() => expect(decisionApi.empiricalResampling).toHaveBeenLastCalledWith(expect.objectContaining({
      training_days: 12, screen: expect.objectContaining({
        resource_plan: expect.objectContaining({ available_agents_by_day: Array(35).fill(4),
          max_staff_cost_cents: 1_050_000, max_final_backlog: 105 }),
        criteria: { max_joint_violation_bps: 5000, min_joint_recovery_bps: 1000,
          min_sla_improvement_bps: 1000 },
      }),
    })));
    expect(await screen.findByText(/Worse joint risk than baseline/)).toBeInTheDocument();
    expect(screen.getByRole('textbox', { name: 'Stored policy screen verification code (optional)' })).toHaveValue('screen-hash');
  });

  it('links a reviewed observational effect without changing the scenario comparison', async () => {
    const user = userEvent.setup();
    renderWithProviders(<DecisionLabPage />);
    await user.type(screen.getByRole('textbox', { name: 'Organization ID' }), 'tenant-a');
    await user.type(screen.getByRole('textbox', { name: 'Access scope' }), 'private');
    await user.click(screen.getByRole('button', { name: 'Load overview' }));
    await user.click(await screen.findByRole('button', { name: 'Run comparison' }));
    const effect = { estimate_id: 'effect-reviewed-1', model_id: 'causal-model-1',
      data_artifact_id: 'cohort-1', data_sha256: 'cohort-sha',
      data_ingested_at: Date.parse('2026-02-01T00:00:00Z') / 1000,
      method: 'stratified_backdoor', identification_state: 'identified',
      estimate: 2.5, lower_bound: 1.2, upper_bound: 3.8, interval_kind: 'approximate',
      unit_count: 80, strata_count: 2, leave_one_stratum_out_sign_flip: false,
      leave_one_unit_out: { evaluated_units: 79, skipped_due_positivity: 1,
        max_abs_effect_shift: 0.3, sign_flip: false },
      temporal_holdout: null,
      permutation_diagnostic: { method: 'within_stratum_shuffle', runs: 100,
        abs_at_least_observed: 3, median_abs_placebo: 0.2, max_abs_placebo: 1.1 },
      pre_treatment_placebo: null,
      negative_control: { variable_id: 'nc-1', review_id: 'review-1', contrast: 2,
        lower_bound: 1, upper_bound: 3, imbalance_flag: true },
      window_start: Date.parse('2025-12-01T00:00:00Z') / 1000,
      window_end: Date.parse('2025-12-20T00:00:00Z') / 1000,
      treatment_variable_id: 'staffing', treatment_unit: 'agents',
      outcome_variable_id: 'sla_resolved', outcome_unit: 'tickets',
      treatment_name: 'Extra staffing', outcome_name: 'SLA resolutions', population: 'Pilot queue' };
    vi.mocked(decisionApi.compare).mockResolvedValueOnce({ brief: {
      ...brief, data_cutoff_utc: '2026-01-01T00:00:00Z', observational_effects: [effect],
    } } as never);
    await user.type(screen.getByRole('textbox', { name: 'Reviewed effect IDs (comma separated, optional)' }), effect.estimate_id);
    await user.click(screen.getByRole('button', { name: 'Compose verified brief' }));
    await waitFor(() => expect(decisionApi.compare).toHaveBeenLastCalledWith(expect.objectContaining({
      tenant_id: 'tenant-a', acl: 'private', effect_ids: ['effect-reviewed-1'],
    })));
    expect(await screen.findByText(/Extra staffing → SLA resolutions/)).toBeInTheDocument();
    expect(screen.getByText(/do not establish a staffing intervention effect/)).toBeInTheDocument();
    expect(screen.getByText('Refutation warning')).toBeInTheDocument();
    expect(screen.getByText('Ingested after snapshot cutoff')).toBeInTheDocument();
    expect(screen.getByText(/Negative control imbalance: Flagged/)).toBeInTheDocument();
    expect(screen.getByText(/Negative control nc-1, review review-1: contrast 2/)).toBeInTheDocument();
    expect(screen.getByText(/treatment staffing \(agents\).*outcome sla_resolved \(tickets\)/)).toBeInTheDocument();
    expect(screen.getByText(/cohort-1 · cohort-sha/)).toBeInTheDocument();
    expect(screen.getByRole('heading', { name: 'Scenario comparison' })).toBeInTheDocument();
  });

  it('stages only a selected canonical synthetic source event and clears stale simulation results', async () => {
    const user = userEvent.setup();
    const exactCatalog = {
      snapshots: [{ id: 'synthetic-support-47-35', queue_id: 'synthetic-support-queue',
        data_cutoff_utc: '2026-01-01T00:00:00Z', horizon_days: 35, source_kinds: ['synthetic_support_export'] }],
      models: [{ id: 'dashboard-synthetic-capacity-v1-47-35' }],
      scenarios: [
        { id: 'dashboard-synthetic-baseline-two-agents-47-35', horizon_days: 35 },
        { id: 'dashboard-synthetic-three-agents-47-35', horizon_days: 35 },
      ],
      synthetic_pilots: [{ snapshot_id: 'synthetic-support-47-35',
        model_version: 'dashboard-synthetic-capacity-v1-47-35',
        baseline_scenario_id: 'dashboard-synthetic-baseline-two-agents-47-35',
        alternative_scenario_id: 'dashboard-synthetic-three-agents-47-35' }],
      uploaded_pilots: [],
    };
    vi.mocked(decisionApi.catalog).mockResolvedValueOnce(exactCatalog).mockResolvedValueOnce({
      snapshots: [], models: [], scenarios: [], synthetic_pilots: [], uploaded_pilots: [],
    });
    renderWithProviders(<DecisionLabPage />);
    await user.type(screen.getByRole('textbox', { name: 'Organization ID' }), 'tenant-a');
    await user.type(screen.getByRole('textbox', { name: 'Access scope' }), 'private');
    await user.click(screen.getByRole('button', { name: 'Load overview' }));
    await screen.findByRole('button', { name: 'Stage source event' });
    await user.click(screen.getByRole('button', { name: 'Run comparison' }));
    expect(await screen.findByRole('heading', { name: 'Scenario comparison' })).toBeInTheDocument();
    await user.selectOptions(screen.getByRole('combobox', { name: 'Source change' }), 'acl_lost');
    await user.click(screen.getByRole('button', { name: 'Stage source event' }));
    await waitFor(() => expect(decisionApi.stageSyntheticLifecycle).toHaveBeenCalledWith(
      { tenant_id: 'tenant-a', acl: 'private' }, { seed: 47, days: 35, kind: 'acl_lost' },
    ));
    expect(await screen.findByText(/Synthetic source event queued/)).toBeInTheDocument();
    expect(screen.queryByRole('heading', { name: 'Scenario comparison' })).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Stage source event' })).toBeDisabled();
  });

  it('files one exact verified synthetic run for human inspection and checks status without self-approval', async () => {
    const user = userEvent.setup();
    renderWithProviders(<DecisionLabPage />);
    await user.type(screen.getByRole('textbox', { name: 'Organization ID' }), 'tenant-a');
    await user.type(screen.getByRole('textbox', { name: 'Access scope' }), 'private');
    await user.click(screen.getByRole('button', { name: 'Load overview' }));
    await user.click(await screen.findByRole('button', { name: 'Run comparison' }));
    expect(screen.getByRole('button', { name: 'Request human inspection' })).toBeDisabled();
    await user.click(screen.getByRole('button', { name: 'Verify both replays' }));
    await screen.findByText('Both simulations reproduced the expected verification codes.');
    await user.type(screen.getByRole('textbox', { name: 'Reason for inspection' }), 'Inspect backlog assumptions');
    await user.click(screen.getByRole('button', { name: 'Request human inspection' }));
    await waitFor(() => expect(decisionApi.requestPilotReview).toHaveBeenCalledWith({
      tenant_id: 'tenant-a', acl: 'private', snapshot_id: 'snapshot-1', model_version: 'model-1',
      scenario_id: 'scenario-base', expected_hash: 'base-hash',
      summary: 'Inspect backlog assumptions', ttl_seconds: 3600,
    }));
    expect(await screen.findByText('Pending human decision')).toBeInTheDocument();
    expect(screen.getByText('Pending human inspection request filed. Approval has not been granted.')).toBeInTheDocument();
    expect(screen.getByRole('textbox', { name: 'Approval ID' })).toHaveValue('approval-1');
    await user.click(screen.getByRole('button', { name: 'Check inspection status' }));
    await waitFor(() => expect(decisionApi.pilotReviewStatus).toHaveBeenCalledWith({
      tenant_id: 'tenant-a', acl: 'private', snapshot_id: 'snapshot-1', model_version: 'model-1',
      scenario_id: 'scenario-base', expected_hash: 'base-hash', approval_id: 'approval-1',
    }));
    expect(await screen.findByText('Approved inspection receipt')).toBeInTheDocument();
    await user.selectOptions(screen.getByRole('combobox', { name: 'Run to inspect' }), 'alternative');
    expect(screen.queryByText('Approved inspection receipt')).not.toBeInTheDocument();
    expect(screen.getByRole('textbox', { name: 'Approval ID' })).toHaveValue('');
  });

  it('offers source-bound diagnostics and inspection for a verified uploaded pilot', async () => {
    const user = userEvent.setup();
    vi.mocked(decisionApi.catalog).mockResolvedValue(uploadedCatalog);
    vi.mocked(decisionApi.engineeringValidation).mockResolvedValue({ report: {
      ...engineeringReport, status: 'exploratory_operator_upload',
      snapshot_id: 'uploaded-snapshot', model_version: 'uploaded-model',
      initial_prefix_fit: null, initial_prefix_unavailable_reason: 'No saturated training days.',
      provided_model_matches_initial_prefix_fit: null,
      one_day_backtest: null, one_day_backtest_unavailable_reason: 'Capacity is not identified.',
      fixed_interval_diagnostic: null, fixed_interval_unavailable_reason: 'Backtest unavailable.',
    } });
    renderWithProviders(<DecisionLabPage />);
    await user.type(screen.getByRole('textbox', { name: 'Organization ID' }), 'tenant-a');
    await user.type(screen.getByRole('textbox', { name: 'Access scope' }), 'private');
    await user.click(screen.getByRole('button', { name: 'Load overview' }));
    await user.click(await screen.findByRole('button', { name: 'Run comparison' }));
    await user.click(screen.getByRole('button', { name: 'Verify both replays' }));
    expect(await screen.findByRole('heading', { name: 'Human inspection of one uploaded pilot run' })).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Run engineering validation' }));
    await waitFor(() => expect(decisionApi.engineeringValidation).toHaveBeenCalledWith({
      tenant_id: 'tenant-a', acl: 'private', snapshot_id: 'uploaded-snapshot', model_version: 'uploaded-model',
    }));
    expect(await screen.findByText('exploratory_operator_upload')).toBeInTheDocument();
    expect(screen.getByText(/No saturated training days/)).toBeInTheDocument();
    expect(screen.getByText(/Capacity is not identified/)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Run empirical resampling' })).toBeInTheDocument();
    await user.type(screen.getByRole('textbox', { name: 'Reason for inspection' }), 'Inspect uploaded backlog assumptions');
    await user.click(screen.getByRole('button', { name: 'Request human inspection' }));
    await waitFor(() => expect(decisionApi.requestPilotReview).toHaveBeenCalledWith({
      tenant_id: 'tenant-a', acl: 'private', snapshot_id: 'uploaded-snapshot', model_version: 'uploaded-model',
      scenario_id: 'uploaded-baseline', expected_hash: 'base-hash',
      summary: 'Inspect uploaded backlog assumptions', ttl_seconds: 3600,
    }));
  });

  it('sends empirical resampling for the uploaded pair selected from the catalog', async () => {
    const user = userEvent.setup();
    vi.mocked(decisionApi.empiricalResampling).mockResolvedValue({ ...empiricalReport,
      status: 'exploratory_operator_upload', limitations: ['Operator history only.'] } as never);
    vi.mocked(decisionApi.catalog).mockResolvedValue({
      ...uploadedCatalog,
      snapshots: [{ ...uploadedCatalog.snapshots[0], horizon_days: 10 }],
      scenarios: uploadedCatalog.scenarios.map((scenario) => ({ ...scenario, horizon_days: 10 })),
    });
    renderWithProviders(<DecisionLabPage />);
    await user.type(screen.getByRole('textbox', { name: 'Organization ID' }), 'tenant-a');
    await user.type(screen.getByRole('textbox', { name: 'Access scope' }), 'private');
    await user.click(screen.getByRole('button', { name: 'Load overview' }));
    await user.click(await screen.findByRole('button', { name: 'Run comparison' }));
    await user.click(screen.getByRole('button', { name: 'Run empirical resampling' }));
    await waitFor(() => expect(decisionApi.empiricalResampling).toHaveBeenCalledWith(expect.objectContaining({
      tenant_id: 'tenant-a', acl: 'private', snapshot_id: 'uploaded-snapshot', model_version: 'uploaded-model',
      baseline_scenario_id: 'uploaded-baseline', alternative_scenario_id: 'uploaded-alternative',
    })));
    const direct = await screen.findByLabelText('Training-prefix empirical resampling');
    expect(within(direct).getByText('Operator-provided local source')).toBeInTheDocument();
    expect(within(direct).queryByText('Synthetic')).not.toBeInTheDocument();
    vi.mocked(decisionApi.compare).mockResolvedValueOnce({ brief: { ...brief,
      exploratory_empirical: { source_origin: 'operator_upload', run_id: 'run-47',
        run_sha256: 'stored-run-sha', fit_id: 'fit-47', fit_sha256: 'stored-fit-sha',
        report: empiricalReport.run },
    } } as never);
    await user.click(screen.getByRole('button', { name: 'Compose verified brief' }));
    const composed = await screen.findByLabelText('Verified evidence brief');
    expect(within(composed).getByText('Operator-provided local source')).toBeInTheDocument();
    expect(within(composed).queryByText('Synthetic')).not.toBeInTheDocument();
  });

  it('opens an Inbox review link only after exact catalog, compare, and server replay checks', async () => {
    const approvalId = '123e4567-e89b-42d3-a456-426614174000';
    const baselineHash = 'a'.repeat(64);
    const alternativeHash = 'b'.repeat(64);
    const snapshot_id = 'synthetic-support-47-35';
    const model_version = 'dashboard-synthetic-capacity-v1-47-35';
    const baseline_scenario_id = 'dashboard-synthetic-baseline-two-agents-47-35';
    const alternative_scenario_id = 'dashboard-synthetic-three-agents-47-35';
    vi.mocked(decisionApi.catalog).mockResolvedValue({
      snapshots: [{ id: snapshot_id, queue_id: 'synthetic-support-queue', data_cutoff_utc: '2026-01-01T00:00:00Z',
        horizon_days: 35, source_kinds: ['synthetic_support_export'] }],
      models: [{ id: model_version }],
      scenarios: [{ id: baseline_scenario_id, horizon_days: 35 }, { id: alternative_scenario_id, horizon_days: 35 }],
      synthetic_pilots: [{ snapshot_id, model_version, baseline_scenario_id, alternative_scenario_id }],
      uploaded_pilots: [],
    });
    vi.mocked(decisionApi.compare).mockResolvedValue({ brief: {
      ...brief, replay: { baseline_replay_hash: baselineHash, alternative_replay_hash: alternativeHash },
    } } as never);
    vi.mocked(decisionApi.pilotReviewStatus).mockResolvedValue({ review: {
      link: { approval_id: approvalId, agent_id: 'admin-1', replay_hash: alternativeHash,
        snapshot_id, scenario_id: alternative_scenario_id },
      status: 'pending', expires_at_utc: '2026-09-27T12:00:00Z', decided_by: null,
    } });
    const path = pilotReviewDeepLink({ tenant_id: 'tenant-a', acl: 'private', snapshot_id,
      scenario_id: alternative_scenario_id, replay_hash: alternativeHash }, approvalId)!;
    render(<IntlProvider messages={en} locale="en" defaultLocale="en"><MemoryRouter initialEntries={[path]}>
      <Routes><Route path="/app/system/decision-lab" element={<DecisionLabPage />} /></Routes>
    </MemoryRouter></IntlProvider>);
    expect(await screen.findByRole('heading', { name: 'Daily simulation trajectories' })).toBeInTheDocument();
    expect(screen.getByRole('combobox', { name: 'Run to inspect' })).toHaveValue('alternative');
    expect(screen.getByText('Pending human decision')).toBeInTheDocument();
    expect(decisionApi.compare).toHaveBeenCalledWith({ tenant_id: 'tenant-a', acl: 'private',
      snapshot_id, model_version, baseline_scenario_id, alternative_scenario_id });
    expect(decisionApi.replay).toHaveBeenCalledWith({ tenant_id: 'tenant-a', acl: 'private',
      snapshot_id, model_version, scenario_id: alternative_scenario_id, expected_hash: alternativeHash });
    expect(decisionApi.pilotReviewStatus).toHaveBeenCalledWith({ tenant_id: 'tenant-a', acl: 'private',
      snapshot_id, model_version, scenario_id: alternative_scenario_id,
      expected_hash: alternativeHash, approval_id: approvalId });
    expect(decisionApi.requestPilotReview).not.toHaveBeenCalled();
  });

  it('opens an uploaded pilot inspection link only with its active paired catalog receipt', async () => {
    const approvalId = '123e4567-e89b-42d3-a456-426614174001';
    vi.mocked(decisionApi.catalog).mockResolvedValue(uploadedCatalog);
    vi.mocked(decisionApi.compare).mockResolvedValue({ brief: { ...brief, replay: {
      baseline_replay_hash: uploadedReceipt.baseline_replay_hash,
      alternative_replay_hash: uploadedReceipt.alternative_replay_hash,
    } } } as never);
    vi.mocked(decisionApi.pilotReviewStatus).mockResolvedValue({ review: {
      link: { approval_id: approvalId, agent_id: 'admin-1', replay_hash: uploadedReceipt.alternative_replay_hash,
        snapshot_id: uploadedReceipt.snapshot_id, scenario_id: uploadedReceipt.alternative_scenario_id },
      status: 'pending', expires_at_utc: '2026-09-27T12:00:00Z', decided_by: null,
    } });
    const path = pilotReviewDeepLink({ tenant_id: 'tenant-a', acl: 'private',
      snapshot_id: uploadedReceipt.snapshot_id, scenario_id: uploadedReceipt.alternative_scenario_id,
      model_version: uploadedReceipt.model_version,
      replay_hash: uploadedReceipt.alternative_replay_hash }, approvalId)!;
    render(<IntlProvider messages={en} locale="en" defaultLocale="en"><MemoryRouter initialEntries={[path]}>
      <Routes><Route path="/app/system/decision-lab" element={<DecisionLabPage />} /></Routes>
    </MemoryRouter></IntlProvider>);
    expect(await screen.findByRole('heading', { name: 'Human inspection of one uploaded pilot run' })).toBeInTheDocument();
    expect(screen.getByRole('combobox', { name: 'Run to inspect' })).toHaveValue('alternative');
    expect(decisionApi.compare).toHaveBeenCalledWith({ tenant_id: 'tenant-a', acl: 'private',
      snapshot_id: uploadedReceipt.snapshot_id, model_version: uploadedReceipt.model_version,
      baseline_scenario_id: uploadedReceipt.baseline_scenario_id,
      alternative_scenario_id: uploadedReceipt.alternative_scenario_id });
    expect(decisionApi.pilotReviewStatus).toHaveBeenCalledWith({ tenant_id: 'tenant-a', acl: 'private',
      snapshot_id: uploadedReceipt.snapshot_id, model_version: uploadedReceipt.model_version,
      scenario_id: uploadedReceipt.alternative_scenario_id,
      expected_hash: uploadedReceipt.alternative_replay_hash, approval_id: approvalId });
  });

  it('withholds a linked trajectory when the current comparison hash differs', async () => {
    const approvalId = '123e4567-e89b-42d3-a456-426614174000';
    const snapshot_id = 'synthetic-support-47-35';
    const model_version = 'dashboard-synthetic-capacity-v1-47-35';
    const baseline_scenario_id = 'dashboard-synthetic-baseline-two-agents-47-35';
    const alternative_scenario_id = 'dashboard-synthetic-three-agents-47-35';
    vi.mocked(decisionApi.catalog).mockResolvedValue({
      snapshots: [{ id: snapshot_id, queue_id: 'queue', data_cutoff_utc: '2026-01-01T00:00:00Z', horizon_days: 35,
        source_kinds: ['synthetic_support_export'] }],
      models: [{ id: model_version }],
      scenarios: [{ id: baseline_scenario_id, horizon_days: 35 }, { id: alternative_scenario_id, horizon_days: 35 }],
      synthetic_pilots: [{ snapshot_id, model_version, baseline_scenario_id, alternative_scenario_id }],
      uploaded_pilots: [],
    });
    const path = pilotReviewDeepLink({ tenant_id: 'tenant-a', acl: 'private', snapshot_id,
      scenario_id: baseline_scenario_id, replay_hash: 'a'.repeat(64) }, approvalId)!;
    render(<IntlProvider messages={en} locale="en" defaultLocale="en"><MemoryRouter initialEntries={[path]}>
      <Routes><Route path="/app/system/decision-lab" element={<DecisionLabPage />} /></Routes>
    </MemoryRouter></IntlProvider>);
    expect(await screen.findByRole('alert')).toHaveTextContent('This review link no longer matches an active pilot run or its replay digest.');
    expect(screen.queryByRole('heading', { name: 'Daily simulation trajectories' })).not.toBeInTheDocument();
    expect(decisionApi.replay).not.toHaveBeenCalled();
    expect(decisionApi.pilotReviewStatus).not.toHaveBeenCalled();
  });

  it('cancels an in-flight review link load when the URL query is removed', async () => {
    const user = userEvent.setup();
    const approvalId = '123e4567-e89b-42d3-a456-426614174000';
    const path = pilotReviewDeepLink({ tenant_id: 'tenant-a', acl: 'private',
      snapshot_id: 'synthetic-support-47-35',
      scenario_id: 'dashboard-synthetic-baseline-two-agents-47-35',
      replay_hash: 'a'.repeat(64) }, approvalId)!;
    let releaseOverview!: (value: DecisionOverview) => void;
    vi.mocked(decisionApi.overview).mockReturnValue(new Promise((resolve) => { releaseOverview = resolve; }));
    render(<IntlProvider messages={en} locale="en" defaultLocale="en"><MemoryRouter initialEntries={[path]}>
      <Routes><Route path="/app/system/decision-lab" element={<ReviewNavigationHarness />} /></Routes>
    </MemoryRouter></IntlProvider>);
    expect(screen.getByRole('textbox', { name: 'Organization ID' })).toBeDisabled();
    await user.click(screen.getByRole('button', { name: 'Leave review link' }));
    await waitFor(() => expect(screen.getByRole('textbox', { name: 'Organization ID' })).toBeEnabled());
    await act(async () => { releaseOverview(overview); });
    expect(decisionApi.overview).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole('heading', { name: 'Daily simulation trajectories' })).not.toBeInTheDocument();
    expect(screen.queryByText('Loaded scope: tenant-a / private')).not.toBeInTheDocument();
    expect(screen.queryByRole('textbox', { name: 'Approval ID' })).not.toBeInTheDocument();
  });

  it('recomputes active synthetic engineering diagnostics and displays baselines and interval drift', async () => {
    const user = userEvent.setup();
    renderWithProviders(<DecisionLabPage />);
    await user.type(screen.getByRole('textbox', { name: 'Organization ID' }), 'tenant-a');
    await user.type(screen.getByRole('textbox', { name: 'Access scope' }), 'private');
    await user.click(screen.getByRole('button', { name: 'Load overview' }));
    await user.click(await screen.findByRole('button', { name: 'Run engineering validation' }));
    await waitFor(() => expect(decisionApi.engineeringValidation).toHaveBeenCalledWith({
      tenant_id: 'tenant-a', acl: 'private', snapshot_id: 'snapshot-1', model_version: 'model-1',
    }));
    expect(await screen.findByText('synthetic_only_exploratory')).toBeInTheDocument();
    expect(screen.getAllByText(/Verification code:/).length).toBeGreaterThan(0);
    expect(screen.getAllByText(/Rolling prior-only capacity-fit diagnostic/).length).toBeGreaterThan(0);
    expect(screen.getByText('Rolling-fit diagnostic beats all baselines')).toBeInTheDocument();
    expect(screen.getAllByText('Rolling prior-only capacity-fit diagnostic').length).toBeGreaterThan(0);
    expect(screen.getByText(/Queue simulator content fingerprint/)).toBeInTheDocument();
    expect(screen.getAllByText('No-change baseline').length).toBeGreaterThan(0);
    expect(screen.getAllByText('Seasonal naive baseline').length).toBeGreaterThan(0);
    expect(screen.getAllByText('Mean-change baseline').length).toBeGreaterThan(0);
    expect(screen.getByText(/Observed coverage: 80%/)).toBeInTheDocument();
    expect(screen.getByText(/Recent drift signal: Yes/)).toBeInTheDocument();
    expect(screen.getByText('synthetic-source-1')).toBeInTheDocument();
  });

  it('reloads an explicit pilot pair when same-horizon scenarios sort as two baselines', async () => {
    const user = userEvent.setup();
    vi.mocked(decisionApi.catalog).mockResolvedValue({
      snapshots: [
        { id: 'synthetic-support-11-30', queue_id: 'queue', data_cutoff_utc: '2026-01-31T00:00:00Z', horizon_days: 30, source_kinds: ['synthetic_support_export'] },
        { id: 'synthetic-support-22-30', queue_id: 'queue', data_cutoff_utc: '2026-01-31T00:00:00Z', horizon_days: 30, source_kinds: ['synthetic_support_export'] },
      ],
      models: [{ id: 'dashboard-synthetic-capacity-v1-11-30' }, { id: 'dashboard-synthetic-capacity-v1-22-30' }],
      scenarios: [
        { id: 'dashboard-synthetic-baseline-two-agents-11-30', horizon_days: 30 },
        { id: 'dashboard-synthetic-baseline-two-agents-22-30', horizon_days: 30 },
        { id: 'dashboard-synthetic-three-agents-22-30', horizon_days: 30 },
        { id: 'dashboard-synthetic-three-agents-11-30', horizon_days: 30 },
      ],
      synthetic_pilots: [
        { snapshot_id: 'synthetic-support-11-30', model_version: 'dashboard-synthetic-capacity-v1-11-30', baseline_scenario_id: 'dashboard-synthetic-baseline-two-agents-11-30', alternative_scenario_id: 'dashboard-synthetic-three-agents-11-30' },
        { snapshot_id: 'synthetic-support-22-30', model_version: 'dashboard-synthetic-capacity-v1-22-30', baseline_scenario_id: 'dashboard-synthetic-baseline-two-agents-22-30', alternative_scenario_id: 'dashboard-synthetic-three-agents-22-30' },
      ],
      uploaded_pilots: [],
    });
    renderWithProviders(<DecisionLabPage />);
    await user.type(screen.getByRole('textbox', { name: 'Organization ID' }), 'tenant-a');
    await user.type(screen.getByRole('textbox', { name: 'Access scope' }), 'private');
    await user.click(screen.getByRole('button', { name: 'Load overview' }));
    await user.click(await screen.findByRole('button', { name: 'Run comparison' }));
    await waitFor(() => expect(decisionApi.compare).toHaveBeenCalledWith({
      tenant_id: 'tenant-a', acl: 'private', snapshot_id: 'synthetic-support-11-30',
      model_version: 'dashboard-synthetic-capacity-v1-11-30',
      baseline_scenario_id: 'dashboard-synthetic-baseline-two-agents-11-30',
      alternative_scenario_id: 'dashboard-synthetic-three-agents-11-30',
    }));
  });

  it('leaves scenario selectors blank when the catalog has no verified pair', async () => {
    const user = userEvent.setup();
    vi.mocked(decisionApi.catalog).mockResolvedValue({
      ...catalog,
      synthetic_pilots: [],
      scenarios: [{ id: 'unpaired-scenario-a', horizon_days: 2 }, { id: 'unpaired-scenario-b', horizon_days: 2 }],
    });
    renderWithProviders(<DecisionLabPage />);
    await user.type(screen.getByRole('textbox', { name: 'Organization ID' }), 'tenant-a');
    await user.type(screen.getByRole('textbox', { name: 'Access scope' }), 'private');
    await user.click(screen.getByRole('button', { name: 'Load overview' }));
    expect(await screen.findByRole('combobox', { name: 'Baseline scenario' })).toHaveValue('');
    expect(screen.getByRole('combobox', { name: 'Alternative scenario' })).toHaveValue('');
    expect(screen.getByRole('button', { name: 'Run comparison' })).toBeDisabled();
    await user.selectOptions(screen.getByRole('combobox', { name: 'Baseline scenario' }), 'unpaired-scenario-a');
    await user.selectOptions(screen.getByRole('combobox', { name: 'Alternative scenario' }), 'unpaired-scenario-b');
    await user.click(screen.getByRole('button', { name: 'Run comparison' }));
    expect(await screen.findByRole('heading', { name: 'Scenario comparison' })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Run empirical resampling' })).not.toBeInTheDocument();
  });

  it('clears catalog selections and simulation results when the scope changes', async () => {
    const user = userEvent.setup();
    renderWithProviders(<DecisionLabPage />);
    const tenant = screen.getByRole('textbox', { name: 'Organization ID' });
    await user.type(tenant, 'tenant-a');
    await user.type(screen.getByRole('textbox', { name: 'Access scope' }), 'private');
    await user.click(screen.getByRole('button', { name: 'Load overview' }));
    await user.click(await screen.findByRole('button', { name: 'Run comparison' }));
    expect(await screen.findByRole('heading', { name: 'Scenario comparison' })).toBeInTheDocument();
    await user.clear(tenant);
    await user.type(tenant, 'tenant-b');
    expect(screen.queryByRole('heading', { name: 'Scenario comparison' })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Run comparison' })).not.toBeInTheDocument();
    expect(screen.queryByText('Loaded scope: tenant-a / private')).not.toBeInTheDocument();
  });

  it('submits explicit sweep and paired sensitivity assumptions and renders their diagnostics', async () => {
    const user = userEvent.setup();
    renderWithProviders(<DecisionLabPage />);
    await user.type(screen.getByRole('textbox', { name: 'Organization ID' }), 'tenant-a');
    await user.type(screen.getByRole('textbox', { name: 'Access scope' }), 'private');
    await user.click(screen.getByRole('button', { name: 'Load overview' }));
    await user.click(await screen.findByRole('button', { name: 'Run comparison' }));

    expect(screen.getAllByText('Synthetic').length).toBeGreaterThan(0);
    expect(screen.getAllByText(/not calibrated or fitted values/i)).toHaveLength(2);
    await user.click(screen.getByRole('button', { name: 'Run policy sweep' }));
    await waitFor(() => expect(decisionApi.policySweep).toHaveBeenCalledWith({
      tenant_id: 'tenant-a', acl: 'private', snapshot_id: 'snapshot-1', model_version: 'model-1',
      baseline_scenario_id: 'scenario-base', alternative_scenario_id: 'scenario-alt',
      resource_plan: {
        available_agents_by_day: [4, 4], max_added_agents_per_day: 1, max_total_agent_days: 270,
        max_staff_cost_cents: 2700000, max_final_backlog: 100,
        service_capacity_band: { min: 2, max: 12 },
      },
    }));
    expect(await screen.findByText(/Choice changes at capacity 5 per agent\/day/)).toBeInTheDocument();
    expect(screen.getByText('Alternative feasible')).toBeInTheDocument();
    expect(screen.getByText('Alternative if feasible with lower backlog.')).toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: 'Run paired sensitivity' }));
    await waitFor(() => expect(decisionApi.sensitivity).toHaveBeenCalledWith({
      tenant_id: 'tenant-a', acl: 'private', snapshot_id: 'snapshot-1', model_version: 'model-1',
      baseline_scenario_id: 'scenario-base', alternative_scenario_id: 'scenario-alt',
      runs: 100, arrival_delta: 2, capacity_min: 2, capacity_max: 12,
      max_final_backlog: 100, max_staff_cost_cents: 2700000,
    }));
    expect(await screen.findByText(/-8/)).toBeInTheDocument();
    expect(screen.getByText('70.00%')).toBeInTheDocument();
    expect(screen.getByText('Worst sampled alternative backlog')).toBeInTheDocument();
    expect(screen.getByText('17')).toBeInTheDocument();
    expect(screen.getByText('Bands are operator assumptions.')).toBeInTheDocument();
  });

  it('clears sweep and sensitivity reports when the comparison selection changes', async () => {
    const user = userEvent.setup();
    renderWithProviders(<DecisionLabPage />);
    await user.type(screen.getByRole('textbox', { name: 'Organization ID' }), 'tenant-a');
    await user.type(screen.getByRole('textbox', { name: 'Access scope' }), 'private');
    await user.click(screen.getByRole('button', { name: 'Load overview' }));
    await user.click(await screen.findByRole('button', { name: 'Run comparison' }));
    await user.click(screen.getByRole('button', { name: 'Run policy sweep' }));
    expect(await screen.findByText('policy-hash')).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Run paired sensitivity' }));
    expect(await screen.findByText('sensitivity-hash')).toBeInTheDocument();
    await user.selectOptions(screen.getByRole('combobox', { name: 'Baseline scenario' }), 'scenario-alt');
    expect(screen.queryByText('policy-hash')).not.toBeInTheDocument();
    expect(screen.queryByText('sensitivity-hash')).not.toBeInTheDocument();
    expect(screen.queryByRole('heading', { name: 'Scenario comparison' })).not.toBeInTheDocument();
  });

  it('clears an exploratory result as soon as its operator assumptions change', async () => {
    const user = userEvent.setup();
    renderWithProviders(<DecisionLabPage />);
    await user.type(screen.getByRole('textbox', { name: 'Organization ID' }), 'tenant-a');
    await user.type(screen.getByRole('textbox', { name: 'Access scope' }), 'private');
    await user.click(screen.getByRole('button', { name: 'Load overview' }));
    await user.click(await screen.findByRole('button', { name: 'Run comparison' }));
    await user.click(screen.getByRole('button', { name: 'Run policy sweep' }));
    expect(await screen.findByText('policy-hash')).toBeInTheDocument();
    await user.clear(screen.getByRole('spinbutton', { name: 'Maximum agents added per day' }));
    expect(screen.queryByText('policy-hash')).not.toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Run paired sensitivity' }));
    expect(await screen.findByText('sensitivity-hash')).toBeInTheDocument();
    await user.clear(screen.getByRole('spinbutton', { name: 'Daily arrival perturbation (± tickets)' }));
    expect(screen.queryByText('sensitivity-hash')).not.toBeInTheDocument();
  });

  it('links to the existing causal review surface', async () => {
    const user = userEvent.setup();
    render(<IntlProvider messages={en} locale="en" defaultLocale="en"><MemoryRouter initialEntries={['/app/system/decision-lab']}>
      <Routes>
        <Route path="/app/system/decision-lab" element={<DecisionLabPage />} />
        <Route path="/app/system/causal" element={<div>causal-review-destination</div>} />
      </Routes>
    </MemoryRouter></IntlProvider>);
    await user.type(screen.getByRole('textbox', { name: 'Organization ID' }), 'tenant-a');
    await user.type(screen.getByRole('textbox', { name: 'Access scope' }), 'private');
    await user.click(screen.getByRole('button', { name: 'Load overview' }));
    await user.click(await screen.findByRole('button', { name: 'Open causal review' }));
    expect(await screen.findByText('causal-review-destination')).toBeInTheDocument();
  });
});
