import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { IntlProvider } from 'react-intl';
import en from '@/i18n/en.json';
import { ccrApi, type CcrReplayResult } from '@/lib/ccr-api';
import { CcrReplayPanel } from './CcrReplayPanel';

vi.mock('@/lib/ccr-api', () => ({ ccrApi: { replay: vi.fn() } }));

const arm = (name: CcrReplayResult['report']['arms'][number]['arm']) => ({
  arm: name, tasks: 2, successes: name === 'ccr' ? 2 : 1,
  success_rate: name === 'ccr' ? 1 : 0.5,
  total_cost_millicents: null, cost_per_success_millicents: null,
  p95_latency_millis: name === 'ccr' ? 142 : 200,
  output_tokens: name === 'ccr' ? 12 : 20,
  cache_read_tokens: name === 'ccr' ? 8 : 0,
  cache_input_ratio: name === 'ccr' ? 0.2 : 0,
  usage_complete_tasks: 2, retrieval_attempts: name === 'ccr' ? 3 : 0,
  retrieval_misses: name === 'ccr' ? 1 : 0,
  retrieval_miss_rate: name === 'ccr' ? 1 / 3 : null,
});

const result: CcrReplayResult = {
  evidence_origin: 'recorded_claim',
  quality_method: 'recorded_exact_value_check',
  billing_status: 'unavailable_without_verified_receipts',
  observations: [{ task_id: 'secret task label', final_response: 'secret source text' }],
  source_delivery: [
    { arm: 'raw', changed_tasks: 0 },
    { arm: 'lossless', changed_tasks: 0 },
    { arm: 'lossy', changed_tasks: 1 },
    { arm: 'ccr', changed_tasks: 2 },
  ],
  report: {
    version: 'ccr-paired-task-v2', cost_source: 'unbilled_replay', paired_tasks: 2,
    arms: ['raw', 'lossless', 'lossy', 'ccr'].map((name) => arm(name as CcrReplayResult['report']['arms'][number]['arm'])),
    ccr_success_delta_vs_raw: 0.5,
    ccr_cost_per_success_delta_vs_raw_millicents: null,
    limitations: [
      'No independently checked billing receipt was supplied; cost and cost-per-success are unavailable.',
      'Synthetic observations validate the evaluator only; they do not show live task quality or savings.',
      'A complete task comparison also needs identical prompts, fixtures, model settings, and source permissions across arms.',
    ],
  },
  limitations: [
    'This replays exact-value scoring and condition checks from local user-supplied evidence; it does not rerun a model.',
    'ChatResponse rounds, provider/model identity, upstream authorization, and elapsed time are not independently attested.',
    'Dashboard CCR metrics are tenant aggregates without task labels and are not substituted for paired observations.',
  ],
};

function renderPanel(tenant = 'tenant-a') {
  return render(<IntlProvider locale="en" messages={en}><CcrReplayPanel tenant={tenant} /></IntlProvider>);
}

function evidenceFile(content = '{"version":"recorded","secret":"source prompt"}') {
  const file = new File([content], 'private-ticket-label.json', { type: 'application/json' });
  Object.defineProperty(file, 'text', { value: async () => content });
  return file;
}

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(ccrApi.replay).mockResolvedValue(result);
});

describe('CcrReplayPanel', () => {
  it('uploads only on explicit action and renders four aggregated arms without task content', async () => {
    const user = userEvent.setup();
    renderPanel();
    const file = evidenceFile();
    await user.upload(screen.getByLabelText(en['ccrReplay.fileInput']), file);
    expect(ccrApi.replay).not.toHaveBeenCalled();
    expect(screen.getByText(en['ccrReplay.fileSelected'])).toBeInTheDocument();
    expect(screen.queryByText('private-ticket-label.json')).not.toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: en['ccrReplay.run'] }));
    await waitFor(() => expect(ccrApi.replay).toHaveBeenCalledWith('tenant-a', expect.stringContaining('"secret":"source prompt"')));
    expect(await screen.findByRole('table', { name: en['ccrReplay.results'] })).toBeInTheDocument();
    expect(screen.getAllByRole('row')).toHaveLength(5);
    expect(screen.getByRole('rowheader', { name: en['ccrReplay.arm.ccr'] })).toBeInTheDocument();
    const lossless = screen.getByRole('rowheader', { name: en['ccrReplay.arm.lossless'] }).closest('tr')!;
    expect(within(lossless).getAllByRole('cell')[1]).toHaveTextContent('0 / 2');
    expect(screen.getByText(en['ccrReplay.origin.recorded_claim'])).toBeInTheDocument();
    expect(screen.getByText(en['ccrReplay.exactValueCheck'])).toBeInTheDocument();
    expect(screen.getAllByText(en['ccrReplay.unavailable']).length).toBeGreaterThan(0);
    expect(screen.getByText(en['ccrReplay.limitations'])).toBeInTheDocument();
    expect(screen.getByText(en['ccrReplay.limit.unattestedClaims'])).toBeInTheDocument();
    expect(screen.getByText(en['ccrReplay.limit.noBilling'])).toBeInTheDocument();
    expect(screen.queryByText(/secret task label|secret source text|source prompt|private-ticket-label/i)).not.toBeInTheDocument();
  });

  it('rejects oversize and malformed files before sending', async () => {
    const user = userEvent.setup();
    renderPanel();
    const oversized = evidenceFile();
    Object.defineProperty(oversized, 'size', { value: 16_000_001 });
    await user.upload(screen.getByLabelText(en['ccrReplay.fileInput']), oversized);
    expect(screen.getByRole('alert')).toHaveTextContent(en['ccrReplay.tooLarge']);
    expect(screen.getByRole('button', { name: en['ccrReplay.run'] })).toBeDisabled();
    expect(ccrApi.replay).not.toHaveBeenCalled();

    await user.upload(screen.getByLabelText(en['ccrReplay.fileInput']), evidenceFile('{bad'));
    await user.click(screen.getByRole('button', { name: en['ccrReplay.run'] }));
    expect(screen.getByRole('alert')).toHaveTextContent(en['ccrReplay.invalidJson']);
    expect(ccrApi.replay).not.toHaveBeenCalled();
  });

  it('clears result and rejects a stale replay response when tenant changes', async () => {
    const user = userEvent.setup();
    let complete!: (value: CcrReplayResult) => void;
    vi.mocked(ccrApi.replay).mockReturnValueOnce(new Promise((resolve) => { complete = resolve; }));
    const view = renderPanel('tenant-a');
    await user.upload(screen.getByLabelText(en['ccrReplay.fileInput']), evidenceFile());
    await user.click(screen.getByRole('button', { name: en['ccrReplay.run'] }));
    await waitFor(() => expect(ccrApi.replay).toHaveBeenCalledWith('tenant-a', expect.any(String)));
    view.rerender(<IntlProvider locale="en" messages={en}><CcrReplayPanel tenant="tenant-b" /></IntlProvider>);
    expect(screen.getByText(en['ccrReplay.noFile'])).toBeInTheDocument();
    complete(result);
    await waitFor(() => expect(screen.queryByRole('table')).not.toBeInTheDocument());
    expect(screen.getByRole('button', { name: en['ccrReplay.run'] })).toBeDisabled();
  });

  it('clears an already visible comparison and selected file on tenant change', async () => {
    const user = userEvent.setup();
    const view = renderPanel('tenant-a');
    await user.upload(screen.getByLabelText(en['ccrReplay.fileInput']), evidenceFile());
    await user.click(screen.getByRole('button', { name: en['ccrReplay.run'] }));
    expect(await screen.findByRole('table')).toBeInTheDocument();
    view.rerender(<IntlProvider locale="en" messages={en}><CcrReplayPanel tenant="tenant-b" /></IntlProvider>);
    expect(screen.queryByRole('table')).not.toBeInTheDocument();
    expect(screen.getByText(en['ccrReplay.noFile'])).toBeInTheDocument();
  });
});
