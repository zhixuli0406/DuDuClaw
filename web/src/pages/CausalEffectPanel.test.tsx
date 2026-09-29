import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { renderWithProviders } from '@/test/render';
import en from '@/i18n/en.json';
import { causalApi, type ModelReview } from '@/lib/causal-api';
import { CausalEffectPanel } from './CausalEffectPanel';

vi.mock('@/lib/causal-api', () => ({ causalApi: {
  assumptions: vi.fn(), reviewAssumption: vi.fn(),
  negativeControlReview: vi.fn(), addNegativeControlProtocol: vi.fn(),
  reviewNegativeControl: vi.fn(), estimateEffect: vi.fn(), effect: vi.fn(),
} }));

const scope = { tenant_id: 'synthetic', acl: 'private' };
const model: ModelReview = {
  model: {
    id: 'model-1', name: 'support', version: 'v1', review_state: 'approved',
    treatment_variable_id: 'staff', outcome_variable_id: 'wait',
  },
  effective_state: 'approved', active_opposition_count: 0, active_opposition_digest: 'x',
  edges: [], variables: [
    { id: 'staff', name: 'staffing', version: 'v1', kind: 'binary' },
    { id: 'wait', name: 'wait', version: 'v1', kind: 'continuous' },
    { id: 'control', name: 'unrelated', version: 'v1', kind: 'count' },
  ],
};

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(causalApi.assumptions).mockResolvedValue({
    reviews: [], readiness: { state: 'unknown', reasons: ['data_availability:missing'] },
  });
  vi.mocked(causalApi.reviewAssumption).mockResolvedValue({
    review_id: 'assumption-1', readiness: { state: 'unknown' },
  });
  vi.mocked(causalApi.negativeControlReview)
    .mockResolvedValueOnce({ review: null, readiness: { state: 'unknown' } })
    .mockResolvedValue({ review: {
      id: 'review-1', model_id: 'model-1', variable_id: 'control',
      protocol_artifact_id: 'protocol-1', verdict: 'pass',
      rationale: 'Synthetic exclusion reviewed in detail', reviewer: 'admin', reviewed_at: 1,
    }, readiness: { state: 'reviewed' } });
  vi.mocked(causalApi.addNegativeControlProtocol).mockResolvedValue({ artifact: { id: 'protocol-1', version: 'v1' } });
  vi.mocked(causalApi.reviewNegativeControl).mockResolvedValue({ review_id: 'review-1' });
  vi.mocked(causalApi.estimateEffect).mockResolvedValue({
    dataset_artifact_id: 'dataset-1', result: { state: 'unknown', reasons: ['insufficient observations'] },
  });
});

describe('CausalEffectPanel', () => {
  it('submits source-backed review and shows estimator unknown reasons', async () => {
    const user = userEvent.setup();
    renderWithProviders(<CausalEffectPanel scope={scope} model={model} />);
    await waitFor(() => expect(causalApi.assumptions).toHaveBeenCalledWith(scope, 'model-1'));
    await waitFor(() => expect(screen.getByRole('button', { name: en['causalCuration.effect.submitAssumption'] })).toBeEnabled());
    await user.type(screen.getByRole('textbox', { name: en['causalCuration.effect.assumptionRationaleAria'] }), 'Synthetic data availability is complete.');
    await user.selectOptions(screen.getByRole('combobox', { name: en['causalCuration.effect.assumptionVerdictAria'] }), 'pass');
    await user.click(screen.getByRole('button', { name: en['causalCuration.effect.submitAssumption'] }));
    await waitFor(() => expect(causalApi.reviewAssumption).toHaveBeenCalledWith(scope,
      expect.objectContaining({ model_id: 'model-1', kind: 'data_availability',
        verdict: 'pass', expected_review_id: null })));
    await waitFor(() => expect(causalApi.negativeControlReview).toHaveBeenCalledWith(scope, 'model-1', 'control'));
    await user.type(screen.getByRole('textbox', { name: en['causalCuration.effect.protocolIdLabel'] }), 'protocol-source');
    await user.type(screen.getByRole('textbox', { name: en['causalCuration.effect.protocolLineageAria'] }), 'lineage-1');
    await user.type(screen.getByRole('textbox', { name: en['causalCuration.effect.protocolContentAria'] }), 'The synthetic metric shares selection but staffing cannot cause it.');
    await user.click(screen.getByRole('button', { name: en['causalCuration.effect.createProtocol'] }));
    await waitFor(() => expect(causalApi.addNegativeControlProtocol).toHaveBeenCalledWith(scope,
      expect.objectContaining({ external_id: 'protocol-source', lineage_id: 'lineage-1' })));
    await user.type(screen.getByRole('textbox', { name: en['causalCuration.effect.reviewRationaleAria'] }), 'Synthetic exclusion was reviewed against the generator.');
    await user.selectOptions(screen.getByRole('combobox', { name: en['causalCuration.effect.reviewVerdictAria'] }), 'pass');
    await user.click(screen.getByRole('button', { name: en['causalCuration.effect.submitReview'] }));
    await waitFor(() => expect(causalApi.reviewNegativeControl).toHaveBeenCalledWith(scope,
      expect.objectContaining({ model_id: 'model-1', variable_id: 'control',
        protocol_artifact_id: 'protocol-1', verdict: 'pass', expected_review_id: null })));
    await user.type(screen.getByRole('textbox', { name: en['causalCuration.effect.datasetIdLabel'] }), 'synthetic-data');
    await user.type(screen.getByRole('textbox', { name: en['causalCuration.effect.datasetLineageAria'] }), 'cohort-1');
    fireEvent.change(screen.getByRole('textbox', { name: en['causalCuration.effect.datasetJsonAria'] }), {
      target: { value: '{"adjustment_variable_ids":[],"units":[]}' },
    });
    await user.click(screen.getByRole('button', { name: en['causalCuration.effect.saveAndEstimate'] }));
    await waitFor(() => expect(causalApi.estimateEffect).toHaveBeenCalledWith(scope,
      expect.objectContaining({ model_id: 'model-1', external_id: 'synthetic-data',
        dataset: { adjustment_variable_ids: [], units: [] } })));
    expect(await screen.findByText(/Unknown: insufficient observations/)).toBeInTheDocument();
  });
});
