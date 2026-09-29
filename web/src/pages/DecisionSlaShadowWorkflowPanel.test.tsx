import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { renderWithProviders } from '@/test/render';
import en from '@/i18n/en.json';
import { decisionApi, type DecisionShadowSlaAssessment, type DecisionShadowSlaForecast,
  type DecisionShadowSlaReview, type DecisionShadowSlaScore,
  type DecisionShadowSlaScreen } from '@/lib/decision-api';
import { DecisionSlaShadowWorkflowPanel } from './DecisionSlaShadowWorkflowPanel';

vi.mock('@/lib/decision-api', () => ({ decisionApi: {
  createShadowSlaForecast: vi.fn(), loadShadowSlaForecast: vi.fn(), createShadowSlaScore: vi.fn(),
  loadShadowSlaScore: vi.fn(), loadCurrentShadowSlaScore: vi.fn(), assessShadowSlaPolicy: vi.fn(),
  evaluateSlaShadowScreen: vi.fn(), saveSlaShadowScreen: vi.fn(), loadSlaShadowScreen: vi.fn(),
  requestSlaShadowReview: vi.fn(), slaShadowReviewStatus: vi.fn(),
} }));

const target = '2026-09-01T00:00:00+00:00';
const slaForecast: DecisionShadowSlaForecast = {
  id: 'sla-forecast-1', forecast_id: 'forecast-1', forecast_sha256: '1'.repeat(64),
  policy_id: 'sla-policy-1', model_version: 'queue-model-1', model_sha256: '2'.repeat(64),
  opening_artifact_id: 'opening-artifact-1', opening_sha256: '3'.repeat(64),
  source_lineage: 'synthetic-sla-lineage', queue_id: 'support-queue',
  target_day_utc: target, committed_at: Date.parse(target) / 1000 + 900,
  opening: { opening_backlog: '9007199254740993', opening_cohort_count: 4,
    prior_resolved_ticket_count: 112, planned_agents: 2, planned_fixed_extra_capacity: 0 },
  prediction: { predicted_resolved_within_sla: 16, predicted_arrivals: 20,
    predicted_backlog_end: '9007199254740994', fitted_capacity_per_agent: 8, training_days: 14 },
  baselines: { no_change: 16, seasonal_naive: 16, seven_day_mean: 16 },
  daily_engine_sha256: '4'.repeat(64), record_sha256: '5'.repeat(64),
  limitations: ['Ticket identities stay in the bound source artifact.'],
};
const slaScore: DecisionShadowSlaScore = {
  id: 'sla-score-1', sla_forecast_id: slaForecast.id, sla_forecast_sha256: slaForecast.record_sha256,
  aggregate_score_id: 'aggregate-score-1', aggregate_score_sha256: '6'.repeat(64),
  observation_artifact_id: 'observation-artifact-1', observation_sha256: '7'.repeat(64),
  scored_at: Date.parse(target) / 1000 + 90_000,
  predicted_resolved_within_sla: '16', observed_resolved_within_sla: '18',
  abs_error: '2', no_change_abs_error: '2', seasonal_naive_abs_error: '2',
  seven_day_mean_abs_error: '2', correction_revision: false,
  record_sha256: '8'.repeat(64), limitations: ['Exploratory evidence only.'],
};
const sums = { model: '14', no_change: '9007199254740993', seasonal_naive: '9007199254740994',
  seven_day_mean: '9007199254740995' };
const slaAssessment: DecisionShadowSlaAssessment = {
  policy_id: slaForecast.policy_id, policy_sha256: '9'.repeat(64),
  source_lineage: slaForecast.source_lineage, queue_id: slaForecast.queue_id,
  assessed_at_utc: '2026-09-23T00:00:00+00:00', due_days: 21, scored_days: 21, corrected_days: 0,
  complete: true, error_sums: sums, model_abs_error_below_each_baseline: true,
  fixed_prefix_interval: { method: 'fixed_prefix_sla_absolute_residual_rank90_v1',
    target_coverage_basis_points: 9000, observed_coverage_basis_points: 10000,
    calibration_points: 14, evaluated_points: 7, calibration_radius: '3',
    recent_miss_count: 0, recent_window_points: 7, drift_signal: false, covered_points: 7 },
  recent_7_day_error_sums: sums, recent_7_day_model_abs_error_below_each_baseline: true,
  total_predicted_resolved_within_sla: '336', total_observed_resolved_within_sla: '350',
  day_status_counts: { scored: 21 }, limitations: ['Synthetic ticket-SLA evidence.'],
};
const savedScreen: DecisionShadowSlaScreen = {
  replay_hash: 'a'.repeat(64), policy_id: slaForecast.policy_id,
  policy_sha256: slaAssessment.policy_sha256, record_sha256: 'b'.repeat(64),
  source_version_hashes: ['3'.repeat(64), '7'.repeat(64)],
  report: { status: 'exploratory_sla_shadow_review_screen', screen_engine_sha256: 'c'.repeat(64),
    criteria: { min_complete_days: 21, min_fixed_coverage_bps: 8000 },
    assessment: slaAssessment,
    coverage_evidence: { covered_days: 7, evaluated_days: 7, minimum_covered_days: 6 },
    failed_checks: [], eligible_for_human_review: true, limitations: ['Inspection only.'] },
};
const previewScreen: DecisionShadowSlaScreen = {
  ...savedScreen, record_sha256: null, source_version_hashes: [],
};
const slaReview: DecisionShadowSlaReview = {
  link: { approval_id: 'sla-approval-1', agent_id: 'admin-1', replay_hash: savedScreen.replay_hash,
    screen_record_sha256: savedScreen.record_sha256!, policy_id: savedScreen.policy_id,
    policy_sha256: savedScreen.policy_sha256 },
  status: 'pending', expires_at_utc: '2026-09-23T01:00:00+00:00', decided_by: null,
};

const openingJson = JSON.stringify({
  inputs: { target_day_utc: target, queue_id: 'support-queue', opening_cohorts: [], known: {} },
  opening_tickets: [], prior_resolved_tickets: [],
});
const observationJson = JSON.stringify({
  queue_id: 'support-queue', target_day_utc: target,
  observed_through_utc: '2026-09-02T00:00:00Z', tickets: [],
});

function translate(id: string): string {
  return (en as Record<string, string>)[id] ?? id;
}

function renderPanel(scope = { tenant_id: 'tenant-a', acl: 'private' }) {
  return renderWithProviders(<DecisionSlaShadowWorkflowPanel
    key={`sla-shadow:${scope.tenant_id}:${scope.acl}`} scope={scope} blocked={false}
    onBusyChange={() => {}} locale="en" t={translate} />);
}

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(decisionApi.createShadowSlaForecast).mockResolvedValue({ sla_forecast: slaForecast });
  vi.mocked(decisionApi.loadShadowSlaForecast).mockResolvedValue({ sla_forecast: slaForecast });
  vi.mocked(decisionApi.createShadowSlaScore).mockResolvedValue({ sla_score: slaScore });
  vi.mocked(decisionApi.loadShadowSlaScore).mockResolvedValue({ sla_score: slaScore });
  vi.mocked(decisionApi.loadCurrentShadowSlaScore).mockResolvedValue({ sla_score: slaScore });
  vi.mocked(decisionApi.assessShadowSlaPolicy).mockResolvedValue({ assessment: slaAssessment });
  vi.mocked(decisionApi.evaluateSlaShadowScreen).mockResolvedValue({ screen: previewScreen });
  vi.mocked(decisionApi.saveSlaShadowScreen).mockResolvedValue({ screen: savedScreen });
  vi.mocked(decisionApi.loadSlaShadowScreen).mockResolvedValue({ screen: savedScreen });
  vi.mocked(decisionApi.requestSlaShadowReview).mockResolvedValue({ review: slaReview });
  vi.mocked(decisionApi.slaShadowReviewStatus).mockResolvedValue({ review: {
    ...slaReview, status: 'approved', decided_by: 'human-reviewer',
  } });
});

async function pasteSource(user: ReturnType<typeof userEvent.setup>, stage: HTMLElement,
  kind: 'opening' | 'observation', json: string) {
  await user.click(within(stage).getByRole('radio', { name: 'Paste JSON' }));
  fireEvent.change(within(stage).getByRole('textbox', {
    name: kind === 'opening' ? 'Opening ticket source JSON' : 'Day-end ticket source JSON',
  }), { target: { value: json } });
  await user.type(within(stage).getByRole('textbox', { name: `${kind} Source retention until (UTC)` }),
    '2099-01-01T00:00:00Z');
}

describe('DecisionSlaShadowWorkflowPanel', () => {
  it('runs the ticket-SLA stages with exact decimal errors and a human inspection receipt', async () => {
    const user = userEvent.setup();
    renderPanel();
    const panel = screen.getByLabelText('Ticket-SLA shadow forecast workflow');
    const forecastStage = within(panel).getByLabelText('1. Commit ticket-SLA forecast');
    await user.type(within(forecastStage).getByRole('textbox', { name: 'SLA forecast ID' }), slaForecast.id);
    await user.type(within(forecastStage).getByRole('textbox', { name: 'Aggregate forecast ID' }), slaForecast.forecast_id);
    await user.type(within(forecastStage).getByRole('textbox', { name: 'Queue model version' }), slaForecast.model_version);
    await pasteSource(user, forecastStage, 'opening', openingJson);
    await user.click(within(forecastStage).getByRole('button', { name: 'Create' }));
    const loadedForecast = await within(forecastStage).findByLabelText('Loaded SLA forecast');
    expect(loadedForecast).toHaveTextContent('9,007,199,254,740,993');
    expect(loadedForecast).toHaveTextContent('112');
    expect(decisionApi.createShadowSlaForecast).toHaveBeenCalledWith({
      tenant_id: 'tenant-a', acl: 'private', sla_id: slaForecast.id,
      forecast_id: slaForecast.forecast_id, model_version: slaForecast.model_version,
      opening_source_json: openingJson, retention_until_utc: '2099-01-01T00:00:00Z',
    });
    // The submitted source is dropped; only its artifact ID remains on screen.
    expect(within(forecastStage).getByRole('textbox', { name: 'Opening ticket source record ID' }))
      .toHaveValue(slaForecast.opening_artifact_id);

    const scoreStage = within(panel).getByLabelText('2. Score ticket-SLA day');
    await user.type(within(scoreStage).getByRole('textbox', { name: 'SLA score ID' }), slaScore.id);
    await user.type(within(scoreStage).getByRole('textbox', { name: 'Aggregate score ID' }), slaScore.aggregate_score_id);
    await pasteSource(user, scoreStage, 'observation', observationJson);
    await user.click(within(scoreStage).getByRole('button', { name: 'Create' }));
    const loadedScore = await within(scoreStage).findByLabelText('Loaded SLA score');
    expect(loadedScore).toHaveTextContent('Initial revision');
    expect(loadedScore).toHaveTextContent('18');
    expect(decisionApi.createShadowSlaScore).toHaveBeenCalledWith(expect.objectContaining({
      score_id: slaScore.id, sla_forecast_id: slaForecast.id,
      aggregate_score_id: slaScore.aggregate_score_id, observation_source_json: observationJson,
    }));
    await user.click(within(scoreStage).getByRole('button', { name: 'Load current revision' }));
    expect(await within(scoreStage).findByLabelText('Loaded SLA score')).toHaveTextContent('Initial revision');

    const assessStage = within(panel).getByLabelText('3. Ticket-SLA assessment');
    await user.type(within(assessStage).getByRole('textbox', { name: 'SLA policy ID' }), slaForecast.policy_id);
    await user.click(within(assessStage).getByRole('button', { name: 'Assess exact SLA policy' }));
    expect(await within(assessStage).findByLabelText('Ticket-SLA assessment'))
      .toHaveTextContent('9,007,199,254,740,993');

    const screenStage = within(panel).getByLabelText('4. Evaluate or save ticket-SLA screen');
    await user.click(within(screenStage).getByRole('button', { name: 'Preview SLA screen' }));
    expect(await within(screenStage).findByLabelText('SLA screen result')).toHaveTextContent('Preview only');
    await user.click(within(screenStage).getByRole('button', { name: 'Save SLA screen' }));
    expect(await within(screenStage).findByLabelText('SLA screen result')).toHaveTextContent('Saved exact record');
    expect(within(screenStage).getByRole('textbox', { name: 'SLA screen verification code' }))
      .toHaveValue(savedScreen.replay_hash);

    const reviewStage = within(panel).getByLabelText('5. Human inspection receipt');
    await user.type(within(reviewStage).getByRole('textbox', { name: 'SLA inspection summary' }),
      'Check synthetic ticket-SLA evidence');
    await user.click(within(reviewStage).getByRole('button', { name: 'Request SLA inspection' }));
    expect(await within(reviewStage).findByLabelText('SLA inspection status')).toHaveTextContent('pending');
    await user.click(within(reviewStage).getByRole('button', { name: 'Check SLA inspection' }));
    expect(await within(reviewStage).findByLabelText('SLA inspection status')).toHaveTextContent('approved');
    expect(decisionApi.requestSlaShadowReview).toHaveBeenCalledWith({
      tenant_id: 'tenant-a', acl: 'private', replay_hash: savedScreen.replay_hash,
      summary: 'Check synthetic ticket-SLA evidence', ttl_seconds: 3600,
    });

    // No rendered record may carry ticket rows or identities. The static
    // shape hints name the export's JSON fields and are documentation only.
    const records = [
      within(forecastStage).getByLabelText('Loaded SLA forecast'),
      within(scoreStage).getByLabelText('Loaded SLA score'),
      within(assessStage).getByLabelText('Ticket-SLA assessment'),
      within(screenStage).getByLabelText('SLA screen result'),
    ];
    for (const record of records) {
      for (const forbidden of ['ticket_id', 'opening_tickets', 'prior_resolved_tickets']) {
        expect(record.textContent ?? '').not.toContain(forbidden);
      }
    }
    expect(panel.textContent ?? '').not.toContain('opening-1');
    expect(panel.textContent ?? '').not.toContain('arrival-');
  });

  it('rejects a foreign policy and refuses review for an ineligible screen', async () => {
    const user = userEvent.setup();
    vi.mocked(decisionApi.assessShadowSlaPolicy).mockResolvedValueOnce({ assessment: {
      ...slaAssessment, policy_id: 'other-policy',
    } });
    vi.mocked(decisionApi.saveSlaShadowScreen).mockResolvedValueOnce({ screen: {
      ...savedScreen, report: { ...savedScreen.report, eligible_for_human_review: false,
        failed_checks: ['recent_sla_skill_not_better_than_all_baselines'] },
    } });
    renderPanel();
    const panel = screen.getByLabelText('Ticket-SLA shadow forecast workflow');
    const assessStage = within(panel).getByLabelText('3. Ticket-SLA assessment');
    await user.type(within(assessStage).getByRole('textbox', { name: 'SLA policy ID' }), slaForecast.policy_id);
    await user.click(within(assessStage).getByRole('button', { name: 'Assess exact SLA policy' }));
    expect(await within(panel).findByRole('alert')).toHaveTextContent('does not match');
    expect(within(assessStage).queryByLabelText('Ticket-SLA assessment')).not.toBeInTheDocument();

    const screenStage = within(panel).getByLabelText('4. Evaluate or save ticket-SLA screen');
    fireEvent.change(within(screenStage).getByRole('spinbutton', { name: 'Minimum complete SLA days' }),
      { target: { value: '14' } });
    expect(within(screenStage).getByRole('button', { name: 'Save SLA screen' })).toBeDisabled();
    expect(decisionApi.saveSlaShadowScreen).not.toHaveBeenCalled();
    fireEvent.change(within(screenStage).getByRole('spinbutton', { name: 'Minimum complete SLA days' }),
      { target: { value: '21' } });
    await user.click(within(screenStage).getByRole('button', { name: 'Save SLA screen' }));
    expect(await within(screenStage).findByLabelText('SLA screen result'))
      .toHaveTextContent('recent_sla_skill_not_better_than_all_baselines');
    const reviewStage = within(panel).getByLabelText('5. Human inspection receipt');
    expect(within(reviewStage).getByRole('button', { name: 'Request SLA inspection' })).toBeDisabled();
    expect(decisionApi.requestSlaShadowReview).not.toHaveBeenCalled();
  });

  it('refuses an oversized ticket source in the browser before any request', async () => {
    const user = userEvent.setup();
    renderPanel();
    const panel = screen.getByLabelText('Ticket-SLA shadow forecast workflow');
    const forecastStage = within(panel).getByLabelText('1. Commit ticket-SLA forecast');
    await user.click(within(forecastStage).getByRole('radio', { name: 'Upload a file' }));
    const oversized = new File(['x'.repeat(2 * 1024 * 1024 + 1)], 'opening.json',
      { type: 'application/json' });
    await user.upload(within(forecastStage).getByLabelText('Opening ticket source file (JSON)'), oversized);
    expect(await within(panel).findByRole('alert')).toHaveTextContent('exceeds 2 MiB');
    expect(decisionApi.createShadowSlaForecast).not.toHaveBeenCalled();

    const notJson = new File(['not json at all'], 'opening.json', { type: 'application/json' });
    await user.upload(within(forecastStage).getByLabelText('Opening ticket source file (JSON)'), notJson);
    expect(await within(panel).findByRole('alert')).toHaveTextContent('UTF-8 encoded JSON');
    expect(decisionApi.createShadowSlaForecast).not.toHaveBeenCalled();
  });

  it('clears downstream ticket-SLA evidence when an upstream input or the scope changes', async () => {
    const user = userEvent.setup();
    const view = renderPanel();
    const panel = screen.getByLabelText('Ticket-SLA shadow forecast workflow');
    const screenStage = within(panel).getByLabelText('4. Evaluate or save ticket-SLA screen');
    await user.type(within(screenStage).getByRole('textbox', { name: 'SLA screen verification code' }), savedScreen.replay_hash);
    await user.click(within(screenStage).getByRole('button', { name: 'Load exact record' }));
    expect(await within(screenStage).findByLabelText('SLA screen result')).toHaveTextContent('Saved exact record');

    const forecastStage = within(panel).getByLabelText('1. Commit ticket-SLA forecast');
    await user.type(within(forecastStage).getByRole('textbox', { name: 'SLA forecast ID' }), 'x');
    await waitFor(() => expect(within(screenStage).queryByLabelText('SLA screen result')).not.toBeInTheDocument());
    expect(within(screenStage).getByRole('textbox', { name: 'SLA screen verification code' })).toHaveValue('');

    view.rerender(<DecisionSlaShadowWorkflowPanel key="sla-shadow:tenant-b:private"
      scope={{ tenant_id: 'tenant-b', acl: 'private' }} blocked={false}
      onBusyChange={() => {}} locale="en" t={translate} />);
    expect(screen.getByRole('textbox', { name: 'SLA forecast ID' })).toHaveValue('');
    expect(screen.getByRole('textbox', { name: 'SLA screen verification code' })).toHaveValue('');
  });

  it('refuses a review request whose receipt binds a different saved screen', async () => {
    const user = userEvent.setup();
    vi.mocked(decisionApi.requestSlaShadowReview).mockResolvedValueOnce({ review: {
      ...slaReview, link: { ...slaReview.link, replay_hash: 'f'.repeat(64) },
    } });
    renderPanel();
    const panel = screen.getByLabelText('Ticket-SLA shadow forecast workflow');
    const screenStage = within(panel).getByLabelText('4. Evaluate or save ticket-SLA screen');
    await user.type(within(screenStage).getByRole('textbox', { name: 'SLA screen verification code' }), savedScreen.replay_hash);
    await user.click(within(screenStage).getByRole('button', { name: 'Load exact record' }));
    expect(await within(screenStage).findByLabelText('SLA screen result')).toHaveTextContent('Saved exact record');
    const reviewStage = within(panel).getByLabelText('5. Human inspection receipt');
    await user.type(within(reviewStage).getByRole('textbox', { name: 'SLA inspection summary' }), 'Check');
    await user.click(within(reviewStage).getByRole('button', { name: 'Request SLA inspection' }));
    expect(await within(panel).findByRole('alert')).toHaveTextContent('does not match');
    expect(within(reviewStage).queryByLabelText('SLA inspection status')).not.toBeInTheDocument();
  });
});
