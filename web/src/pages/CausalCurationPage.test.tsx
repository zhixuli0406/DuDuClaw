import { beforeEach, describe, expect, it, vi } from 'vitest';
import { act, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { renderWithProviders } from '@/test/render';
import en from '@/i18n/en.json';
import { causalApi } from '@/lib/causal-api';
import { CausalCurationPage } from './CausalCurationPage';

vi.mock('@/lib/causal-api', () => ({ causalApi: {
  claims: vi.fn(), claim: vi.fn(), aliases: vi.fn(), models: vi.fn(),
  importMemory: vi.fn(),
  importWiki: vi.fn(),
  importSharedWiki: vi.fn(),
  extract: vi.fn(),
  evaluateExtractionSynthetic: vi.fn(),
  evaluateExtractionScoped: vi.fn(),
  reviewClaim: vi.fn(), reviseClaim: vi.fn(), source: vi.fn(),
  invalidateSource: vi.fn(), eraseSource: vi.fn(), model: vi.fn(),
  setAlias: vi.fn(), revokeAlias: vi.fn(), reviewModel: vi.fn(),
} }));
vi.mock('./CausalEffectPanel', () => ({ CausalEffectPanel: () => null }));

const claim = {
  id: 'c1', cause_variable: 'A', effect_variable: 'B', lag_min_seconds: 0,
  lag_max_seconds: 60, modality: 'asserted', context_json: '{}',
  review_state: 'candidate', reviewer: null, created_at: 1,
};
const detail = {
  claim, effective_state: 'candidate', lineages: {
    independent_supporting_lineages: 1, independent_opposing_lineages: 0,
  },
  evidence: [{ span: { id: 'e1', artifact_id: 's1', span_start: 0, span_end: 1,
    excerpt: 'A', stance: 'supports', speaker_id: null, extractor_version: 'test' },
    source_lineage_id: 'thread', source_active: true }],
  revisions: [],
};
const modelReview = {
  model: { id: 'm1', name: 'Candidate', version: 'v1', review_state: 'candidate', treatment_variable_id: 'A', outcome_variable_id: 'B' },
  effective_state: 'candidate',
  variables: [
    { id: 'va', name: 'A', version: 'v1', kind: 'binary' },
    { id: 'vb', name: 'B', version: 'v1', kind: 'continuous' },
  ],
  edges: [claim], active_opposition_count: 1, active_opposition_digest: 'digest',
};
const evalCount = { true_positive: 1, predicted: 1, gold: 1, precision: 1, recall: 1, f1: 1 };
const evalResponse = {
  scope: { tenant_id: 'tenant-a', acl: 'private' },
  mode: 'bundled_synthetic',
  source_provenance: 'bundled_synthetic_unbound',
  family_provenance: 'bundled_synthetic_annotations',
  report: {
    dataset_sha256: 'a'.repeat(64), training_cases: 1, held_out_cases: 4, held_out_families: 3,
    invalid_responses: 1, ungrounded_predictions: 0, reviewed_cases: 4, reviewed_families: 3,
    rejected_edges_proposed: 0, full_claim: evalCount, exact_span: evalCount,
    direction_claim: evalCount, stance_claim: evalCount, modality_claim: evalCount,
    lag_claim: evalCount, negation_claim: evalCount, independent_lineage_claim: evalCount,
    reviewed_edge_case: evalCount, limitations: ['Synthetic data cannot prove real provider quality.'],
  },
};

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(causalApi.claims).mockResolvedValue({ claims: [claim] } as never);
  vi.mocked(causalApi.claim).mockResolvedValue(detail as never);
  vi.mocked(causalApi.aliases).mockResolvedValue({ aliases: [] });
  vi.mocked(causalApi.models).mockResolvedValue({ models: [] });
  vi.mocked(causalApi.model).mockResolvedValue({ review: modelReview, parent_adjustment: { status: 'unknown', reasons: [] } } as never);
  vi.mocked(causalApi.reviewClaim).mockResolvedValue({ claim } as never);
  vi.mocked(causalApi.reviseClaim).mockResolvedValue({ claim: { ...claim, id: 'c2' } } as never);
  vi.mocked(causalApi.importMemory).mockResolvedValue({ source: { id: 's1', version: 'v1' } });
  vi.mocked(causalApi.importWiki).mockResolvedValue({ source: { id: 's2', version: 'v2' } });
  vi.mocked(causalApi.importSharedWiki).mockResolvedValue({ source: { id: 's3', version: 'v3' } });
  vi.mocked(causalApi.extract).mockResolvedValue({ run: { candidates: [[claim, detail.evidence[0].span]], provider_id: 'anthropic', model_used: 'claude-test' } } as never);
  vi.mocked(causalApi.evaluateExtractionSynthetic).mockResolvedValue(evalResponse as never);
  vi.mocked(causalApi.source).mockResolvedValue({ text: 'A affects B', byte_offset: 0, next_offset: 11, total_bytes: 11, truncated: false });
  vi.mocked(causalApi.invalidateSource).mockResolvedValue({ result: { demoted_claims: 1, scrubbed_snapshots: 2, scrubbed_ccr_originals: 1 } });
  vi.mocked(causalApi.eraseSource).mockResolvedValue({ result: { demoted_claims: 1, scrubbed_snapshots: 2, scrubbed_ccr_originals: 1 } });
});

async function loadClaim() {
  const user = userEvent.setup();
  renderWithProviders(<CausalCurationPage />);
  await user.type(screen.getByRole('textbox', { name: en['causalCuration.tenant'] }), 'tenant-a');
  await user.type(screen.getByRole('textbox', { name: en['causalCuration.acl'] }), 'private');
  await user.click(screen.getByRole('button', { name: en['causalCuration.loadScope'] }));
  await user.click(await screen.findByRole('button', { name: /A → B/ }));
  await screen.findByText(en['causalCuration.evidence.title']);
  return user;
}

describe('CausalCurationPage', () => {
  it('revokes a scoped source and clears source text and stale claim selection', async () => {
    const user = await loadClaim();
    await user.click(screen.getByRole('button', { name: en['causalCuration.evidence.viewSource'] }));
    expect(await screen.findByText('A affects B')).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: en['causalCuration.source.revoke'] }));
    await waitFor(() => expect(causalApi.invalidateSource).toHaveBeenCalledWith(
      { tenant_id: 'tenant-a', acl: 'private' }, 's1',
    ));
    expect(screen.queryByText('A affects B')).not.toBeInTheDocument();
    expect(screen.queryByText(en['causalCuration.evidence.title'])).not.toBeInTheDocument();
    expect(await screen.findByText(/2 decision snapshot\(s\), and 1 CCR original\(s\) were cleared/)).toBeInTheDocument();
    expect(causalApi.claims).toHaveBeenCalledTimes(2);
  });

  it('hides cached source wording while a revocation fails after a partial commit', async () => {
    let rejectRemoval!: (reason: Error) => void;
    vi.mocked(causalApi.invalidateSource).mockImplementation(() => new Promise((_, reject) => {
      rejectRemoval = reject;
    }) as never);
    const user = await loadClaim();
    await user.click(screen.getByRole('button', { name: en['causalCuration.evidence.viewSource'] }));
    expect(await screen.findByText('A affects B')).toBeInTheDocument();
    expect(screen.getByText('A', { selector: 'blockquote' })).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: en['causalCuration.source.revoke'] }));
    expect(screen.queryByText('A affects B')).not.toBeInTheDocument();
    expect(screen.queryByText('A', { selector: 'blockquote' })).not.toBeInTheDocument();
    await act(async () => rejectRemoval(new Error('post-commit failure')));
    expect(await screen.findByRole('alert')).toHaveTextContent('post-commit failure');
    expect(screen.queryByText('A affects B')).not.toBeInTheDocument();
    expect(screen.queryByText('A', { selector: 'blockquote' })).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: en['causalCuration.pendingRemoval.retryRevoke'] })).toBeInTheDocument();
  });

  it('requires confirmation before permanently erasing source wording', async () => {
    const confirm = vi.spyOn(window, 'confirm').mockReturnValueOnce(false).mockReturnValueOnce(true);
    try {
      const user = await loadClaim();
      await user.click(screen.getByRole('button', { name: en['causalCuration.evidence.viewSource'] }));
      await screen.findByText('A affects B');
      await user.click(screen.getByRole('button', { name: en['causalCuration.evidence.deleteForever'] }));
      expect(causalApi.eraseSource).not.toHaveBeenCalled();
      expect(screen.getByText('A affects B')).toBeInTheDocument();
      await user.click(screen.getByRole('button', { name: en['causalCuration.evidence.deleteForever'] }));
      await waitFor(() => expect(causalApi.eraseSource).toHaveBeenCalledWith(
        { tenant_id: 'tenant-a', acl: 'private' }, 's1',
      ));
      expect(screen.queryByText('A affects B')).not.toBeInTheDocument();
      expect(await screen.findByText(/Source wording deleted/)).toBeInTheDocument();
      expect(confirm).toHaveBeenCalledTimes(2);
    } finally { confirm.mockRestore(); }
  });

  it('can erase wording after the source has already been invalidated', async () => {
    vi.mocked(causalApi.claim).mockResolvedValue({
      ...detail,
      evidence: [{ ...detail.evidence[0], source_active: false }],
    } as never);
    const confirm = vi.spyOn(window, 'confirm').mockReturnValue(true);
    try {
      const user = await loadClaim();
      expect(screen.getByText(en['causalCuration.evidence.revokedOrExpired'])).toBeInTheDocument();
      expect(screen.queryByRole('button', { name: en['causalCuration.evidence.viewSource'] })).not.toBeInTheDocument();
      await user.click(screen.getByRole('button', { name: en['causalCuration.evidence.deleteForever'] }));
      await waitFor(() => expect(causalApi.eraseSource).toHaveBeenCalledWith(
        { tenant_id: 'tenant-a', acl: 'private' }, 's1',
      ));
    } finally { confirm.mockRestore(); }
  });
  it('submits scoped source extraction and shows new candidates for review', async () => {
    const user = userEvent.setup();
    renderWithProviders(<CausalCurationPage />);
    await user.type(screen.getByRole('textbox', { name: en['causalCuration.tenant'] }), 'tenant-a');
    await user.type(screen.getByRole('textbox', { name: en['causalCuration.acl'] }), 'private');
    await user.click(screen.getByRole('button', { name: en['causalCuration.loadScope'] }));
    await user.type(await screen.findByRole('textbox', { name: en['causalCuration.extract.sourceIdAria'] }), 's1');
    await user.type(screen.getByRole('textbox', { name: en['causalCuration.extract.questionAria'] }), 'Why did wait rise?');
    await user.click(screen.getByRole('button', { name: en['causalCuration.extract.button'] }));
    await waitFor(() => expect(causalApi.extract).toHaveBeenCalledWith(
      { tenant_id: 'tenant-a', acl: 'private' }, 's1', 'Why did wait rise?',
    ));
    expect(await screen.findByText(/Created 1 candidate claim\(s\) pending review/)).toBeInTheDocument();
  });

  it('runs the admin scoped synthetic evaluator and clears its report on scope change', async () => {
    const user = userEvent.setup();
    renderWithProviders(<CausalCurationPage />);
    await user.type(screen.getByRole('textbox', { name: en['causalCuration.tenant'] }), 'tenant-a');
    await user.type(screen.getByRole('textbox', { name: en['causalCuration.acl'] }), 'private');
    await user.click(screen.getByRole('button', { name: en['causalCuration.loadScope'] }));
    await user.click(screen.getByRole('button', { name: en['causalCuration.nav.eval'] }));
    await user.click(screen.getByRole('button', { name: en['causalCuration.eval.runBundled'] }));
    await waitFor(() => expect(causalApi.evaluateExtractionSynthetic).toHaveBeenCalledWith({
      tenant_id: 'tenant-a', acl: 'private',
    }));
    expect(await screen.findByText(/held-out 4 cases \/ 3 source families/)).toBeInTheDocument();
    expect(screen.getByText(/not bound to a database source/)).toBeInTheDocument();
    await user.clear(screen.getByRole('textbox', { name: en['causalCuration.acl'] }));
    await user.type(screen.getByRole('textbox', { name: en['causalCuration.acl'] }), 'private-b');
    await user.click(screen.getByRole('button', { name: en['causalCuration.loadScope'] }));
    await waitFor(() => expect(causalApi.claims).toHaveBeenCalledWith({
      tenant_id: 'tenant-a', acl: 'private-b',
    }));
    expect(screen.queryByText(/held-out 4 cases \/ 3 source families/)).not.toBeInTheDocument();
  });

  it('ignores a late evaluation response from a previous scope', async () => {
    let finish!: (result: typeof evalResponse) => void;
    vi.mocked(causalApi.evaluateExtractionSynthetic).mockImplementation(
      () => new Promise((resolve) => { finish = resolve; }) as never,
    );
    const user = userEvent.setup();
    renderWithProviders(<CausalCurationPage />);
    await user.type(screen.getByRole('textbox', { name: en['causalCuration.tenant'] }), 'tenant-a');
    await user.type(screen.getByRole('textbox', { name: en['causalCuration.acl'] }), 'private');
    await user.click(screen.getByRole('button', { name: en['causalCuration.loadScope'] }));
    await user.click(screen.getByRole('button', { name: en['causalCuration.nav.eval'] }));
    await user.click(screen.getByRole('button', { name: en['causalCuration.eval.runBundled'] }));
    await waitFor(() => expect(causalApi.evaluateExtractionSynthetic).toHaveBeenCalledTimes(1));
    await user.clear(screen.getByRole('textbox', { name: en['causalCuration.acl'] }));
    await user.type(screen.getByRole('textbox', { name: en['causalCuration.acl'] }), 'private-b');
    await user.click(screen.getByRole('button', { name: en['causalCuration.loadScope'] }));
    await waitFor(() => expect(causalApi.claims).toHaveBeenCalledWith({
      tenant_id: 'tenant-a', acl: 'private-b',
    }));
    await act(async () => finish(evalResponse));
    expect(screen.queryByText(en['causalCuration.eval.resultTitle'])).not.toBeInTheDocument();
  });

  it('submits uploaded v2 cases with exact artifact IDs from the selected scope', async () => {
    const user = userEvent.setup();
    const dataset = { version: 'causal-extraction-eval-v2', cutoff_unix: 100, cases: [] };
    const case_artifacts = { 'case-1': 'artifact-1' };
    vi.mocked(causalApi.evaluateExtractionScoped).mockResolvedValue({
      ...evalResponse, mode: 'scoped_dataset',
      source_provenance: 'active_scoped_artifacts_at_evaluation',
      family_provenance: 'caller_supplied_unverified',
    } as never);
    renderWithProviders(<CausalCurationPage />);
    await user.type(screen.getByRole('textbox', { name: en['causalCuration.tenant'] }), 'tenant-a');
    await user.type(screen.getByRole('textbox', { name: en['causalCuration.acl'] }), 'private');
    await user.click(screen.getByRole('button', { name: en['causalCuration.loadScope'] }));
    await user.click(screen.getByRole('button', { name: en['causalCuration.nav.eval'] }));
    const file = new File([JSON.stringify({ dataset, case_artifacts })], 'labels.json', { type: 'application/json' });
    Object.defineProperty(file, 'text', { value: async () => JSON.stringify({ dataset, case_artifacts }) });
    await user.upload(screen.getByLabelText(en['causalCuration.eval.uploadAria']), file);
    await user.click(screen.getByRole('button', { name: en['causalCuration.eval.runScoped'] }));
    await waitFor(() => expect(causalApi.evaluateExtractionScoped).toHaveBeenCalledWith(
      { tenant_id: 'tenant-a', acl: 'private' }, dataset, case_artifacts,
    ));
    expect(await screen.findByText(/Checked against the exact-scope sources valid at the time/)).toBeInTheDocument();
  });

  it('reviews the stored candidate state in the selected scope', async () => {
    const user = await loadClaim();
    await user.click(screen.getByRole('button', { name: en['causalCuration.review.accept'] }));
    await waitFor(() => expect(causalApi.reviewClaim).toHaveBeenCalledWith(
      { tenant_id: 'tenant-a', acl: 'private' }, 'c1', 'candidate', true,
    ));
  });

  it('submits a new candidate revision with selected source evidence', async () => {
    const user = await loadClaim();
    await user.type(screen.getByRole('textbox', { name: en['causalCuration.revision.reasonAria'] }), 'direction corrected');
    await user.click(screen.getByRole('button', { name: en['causalCuration.revision.submit'] }));
    await waitFor(() => expect(causalApi.reviseClaim).toHaveBeenCalledWith(
      { tenant_id: 'tenant-a', acl: 'private' }, 'c1', expect.objectContaining({
        expected_state: 'candidate', evidence_ids: ['e1'], note: 'direction corrected',
      }),
    ));
  });

  it('imports memory only through the exact agent-private scope', async () => {
    const user = userEvent.setup();
    renderWithProviders(<CausalCurationPage />);
    await user.type(screen.getByRole('textbox', { name: en['causalCuration.tenant'] }), 'agent-a');
    await user.type(screen.getByRole('textbox', { name: en['causalCuration.acl'] }), 'agent-private');
    await user.click(screen.getByRole('button', { name: en['causalCuration.loadScope'] }));
    await user.type(await screen.findByRole('textbox', { name: en['causalCuration.memory.idLabel'] }), 'm1');
    await user.click(screen.getByRole('button', { name: en['causalCuration.memory.importButton'] }));
    await waitFor(() => expect(causalApi.importMemory).toHaveBeenCalledWith('agent-a', 'm1'));
    await user.type(screen.getByRole('textbox', { name: en['causalCuration.wiki.pathLabel'] }), 'sources/queue.md');
    await user.click(screen.getByRole('button', { name: en['causalCuration.wiki.importButton'] }));
    await waitFor(() => expect(causalApi.importWiki).toHaveBeenCalledWith('agent-a', 'sources/queue.md'));
  });

  it('imports a shared page only through the workspace-shared scope', async () => {
    const user = userEvent.setup();
    renderWithProviders(<CausalCurationPage />);
    await user.type(screen.getByRole('textbox', { name: en['causalCuration.tenant'] }), 'workspace');
    await user.type(screen.getByRole('textbox', { name: en['causalCuration.acl'] }), 'shared-wiki');
    await user.click(screen.getByRole('button', { name: en['causalCuration.loadScope'] }));
    await user.type(await screen.findByRole('textbox', { name: en['causalCuration.sharedWiki.pathLabel'] }), 'sources/queue.md');
    await user.click(screen.getByRole('button', { name: en['causalCuration.sharedWiki.importButton'] }));
    await waitFor(() => expect(causalApi.importSharedWiki).toHaveBeenCalledWith('sources/queue.md'));
  });

  it('compares stored model review indicators and observed active source lineages', async () => {
    vi.mocked(causalApi.models).mockResolvedValue({ models: [{ model: modelReview.model, effective_state: 'candidate' }] } as never);
    const user = userEvent.setup();
    renderWithProviders(<CausalCurationPage />);
    await user.type(screen.getByRole('textbox', { name: en['causalCuration.tenant'] }), 'tenant-a');
    await user.type(screen.getByRole('textbox', { name: en['causalCuration.acl'] }), 'private');
    await user.click(screen.getByRole('button', { name: en['causalCuration.loadScope'] }));
    await user.click(await screen.findByRole('button', { name: /Models 1/ }));
    await user.click(await screen.findByRole('checkbox', { name: /Candidate · v1/ }));
    expect(await screen.findByText('Review state: candidate')).toBeInTheDocument();
    expect(screen.getByText('Active opposing evidence: 1')).toBeInTheDocument();
    expect(await screen.findByText('Active supporting source lineages: 1')).toBeInTheDocument();
  });

  it('opens claim evidence when a DAG edge is selected', async () => {
    vi.mocked(causalApi.models).mockResolvedValue({ models: [{ model: modelReview.model, effective_state: 'candidate' }] } as never);
    const user = userEvent.setup();
    renderWithProviders(<CausalCurationPage />);
    await user.type(screen.getByRole('textbox', { name: en['causalCuration.tenant'] }), 'tenant-a');
    await user.type(screen.getByRole('textbox', { name: en['causalCuration.acl'] }), 'private');
    await user.click(screen.getByRole('button', { name: en['causalCuration.loadScope'] }));
    await user.click(await screen.findByRole('button', { name: /Models 1/ }));
    await user.click(await screen.findByRole('button', { name: /Candidate · v1/ }));
    await user.click(await screen.findByRole('button', { name: /A leads to B/ }));
    expect(await screen.findByText(en['causalCuration.evidence.title'])).toBeInTheDocument();
    await waitFor(() => expect(causalApi.claim).toHaveBeenCalledWith({ tenant_id: 'tenant-a', acl: 'private' }, 'c1'));
  });

  it('ignores source responses after the claim or scope changes', async () => {
    const secondClaim = { ...claim, id: 'c2', effect_variable: 'C' };
    const secondDetail = { ...detail, claim: secondClaim };
    let resolveFirst!: (value: { text: string; byte_offset: number; next_offset: number; total_bytes: number; truncated: boolean }) => void;
    let resolveSecond!: (value: { text: string; byte_offset: number; next_offset: number; total_bytes: number; truncated: boolean }) => void;
    vi.mocked(causalApi.claims).mockResolvedValue({ claims: [claim, secondClaim] } as never);
    vi.mocked(causalApi.claim).mockImplementation(async (_scope, id) => (id === 'c2' ? secondDetail : detail) as never);
    vi.mocked(causalApi.source)
      .mockImplementationOnce(() => new Promise((resolve) => { resolveFirst = resolve; }) as never)
      .mockImplementationOnce(() => new Promise((resolve) => { resolveSecond = resolve; }) as never);
    const user = userEvent.setup();
    renderWithProviders(<CausalCurationPage />);
    await user.type(screen.getByRole('textbox', { name: en['causalCuration.tenant'] }), 'tenant-a');
    await user.type(screen.getByRole('textbox', { name: en['causalCuration.acl'] }), 'private-a');
    await user.click(screen.getByRole('button', { name: en['causalCuration.loadScope'] }));
    await user.click(await screen.findByRole('button', { name: /A → B/ }));
    await user.click(await screen.findByRole('button', { name: en['causalCuration.evidence.viewSource'] }));
    await waitFor(() => expect(causalApi.source).toHaveBeenCalledTimes(1));

    await user.click(screen.getByRole('button', { name: /A → C/ }));
    await screen.findByRole('heading', { name: 'A → C' });
    await act(async () => resolveFirst({ text: 'STALE CLAIM SOURCE', byte_offset: 0, next_offset: 20, total_bytes: 20, truncated: false }));
    expect(screen.queryByText('STALE CLAIM SOURCE')).not.toBeInTheDocument();

    await user.click(await screen.findByRole('button', { name: en['causalCuration.evidence.viewSource'] }));
    await waitFor(() => expect(causalApi.source).toHaveBeenCalledTimes(2));
    const acl = screen.getByRole('textbox', { name: en['causalCuration.acl'] });
    await user.clear(acl);
    await user.type(acl, 'private-b');
    await user.click(screen.getByRole('button', { name: en['causalCuration.loadScope'] }));
    await waitFor(() => expect(causalApi.claims).toHaveBeenCalledWith({ tenant_id: 'tenant-a', acl: 'private-b' }));
    await act(async () => resolveSecond({ text: 'STALE SCOPE SOURCE', byte_offset: 0, next_offset: 20, total_bytes: 20, truncated: false }));
    expect(screen.queryByText('STALE SCOPE SOURCE')).not.toBeInTheDocument();
  });

  it('limits comparison claim requests and stops queued work after selection changes', async () => {
    const manyEdges = Array.from({ length: 12 }, (_, index) => ({ ...claim, id: `edge-${index}` }));
    const manyEdgeReview = { ...modelReview, edges: manyEdges };
    vi.mocked(causalApi.models).mockResolvedValue({ models: [{ model: modelReview.model, effective_state: 'candidate' }] } as never);
    vi.mocked(causalApi.model).mockResolvedValue({ review: manyEdgeReview, parent_adjustment: { status: 'unknown', reasons: [] } } as never);
    const pendingResolvers: Array<(value: typeof detail) => void> = [];
    let inFlight = 0;
    let peak = 0;
    vi.mocked(causalApi.claim).mockImplementation(() => {
      inFlight += 1;
      peak = Math.max(peak, inFlight);
      return new Promise((resolve) => pendingResolvers.push((value) => { inFlight -= 1; resolve(value); })) as never;
    });
    const user = userEvent.setup();
    renderWithProviders(<CausalCurationPage />);
    await user.type(screen.getByRole('textbox', { name: en['causalCuration.tenant'] }), 'tenant-a');
    await user.type(screen.getByRole('textbox', { name: en['causalCuration.acl'] }), 'private');
    await user.click(screen.getByRole('button', { name: en['causalCuration.loadScope'] }));
    await user.click(await screen.findByRole('button', { name: /Models 1/ }));
    const compare = await screen.findByRole('checkbox', { name: /Candidate · v1/ });
    await user.click(compare);
    await waitFor(() => expect(causalApi.claim).toHaveBeenCalledTimes(6));
    expect(peak).toBe(6);
    await user.click(compare);
    await act(async () => { pendingResolvers.splice(0).forEach((resolve) => resolve(detail)); });
    await act(async () => { await Promise.resolve(); });
    expect(causalApi.claim).toHaveBeenCalledTimes(6);
  });

  it('ignores a comparison error that arrives after the scope changes', async () => {
    vi.mocked(causalApi.models).mockResolvedValue({ models: [{ model: modelReview.model, effective_state: 'candidate' }] } as never);
    let rejectClaim!: (reason: unknown) => void;
    vi.mocked(causalApi.claim).mockImplementation(() => new Promise((_resolve, reject) => { rejectClaim = reject; }) as never);
    const user = userEvent.setup();
    renderWithProviders(<CausalCurationPage />);
    await user.type(screen.getByRole('textbox', { name: en['causalCuration.tenant'] }), 'tenant-a');
    await user.type(screen.getByRole('textbox', { name: en['causalCuration.acl'] }), 'private-a');
    await user.click(screen.getByRole('button', { name: en['causalCuration.loadScope'] }));
    await user.click(await screen.findByRole('button', { name: /Models 1/ }));
    await user.click(await screen.findByRole('checkbox', { name: /Candidate · v1/ }));
    await waitFor(() => expect(causalApi.claim).toHaveBeenCalledTimes(1));
    const acl = screen.getByRole('textbox', { name: en['causalCuration.acl'] });
    await user.clear(acl);
    await user.type(acl, 'private-b');
    await user.click(screen.getByRole('button', { name: en['causalCuration.loadScope'] }));
    await waitFor(() => expect(causalApi.claims).toHaveBeenCalledWith({ tenant_id: 'tenant-a', acl: 'private-b' }));
    await act(async () => rejectClaim(new Error('stale comparison failure')));
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
  });
});
