import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { useConnectionStore } from '@/stores/connection-store';
import { ForesightPage } from './ForesightPage';

const summary = {
  agent_id: 'agnes',
  total: 5,
  settled: 3,
  avg_brier: 0.12,
  avg_composite_error: 0.2,
  categories: { negligible: 2, critical: 1 },
  fidelity: { mcp_only: 3 },
  sources: { statistical: 4, prior: 1 },
  last_settled_at: '2026-08-14T00:00:00Z',
};

const recentRow = {
  prediction_id: 'p1',
  task_id: 'task-abc-123',
  agent_id: 'agnes',
  round: 1,
  source: 'statistical',
  fidelity: 'mcp_only',
  category: 'negligible',
  brier: 0.04,
  composite_error: 0.1,
  created_at: '2026-08-14T00:00:00Z',
  settled_at: '2026-08-14T01:00:00Z',
};

const chainRound = {
  ...recentRow,
  state_key: 'agnes|coding_simple|first|0',
  expected: {
    tool_classes: ['read', 'exec'],
    call_band: [2, 8],
    outcome: 'accept',
    artifact: 'text_only',
    confidence: 0.8,
  },
  observed: {
    tool_classes: ['read'],
    calls: 5,
    errors: 0,
    outcome: 'accepted',
    artifact: 'text_only',
    fidelity: 'mcp_only',
    window_start: '2026-08-14T00:00:00Z',
    window_end: '2026-08-14T01:00:00Z',
  },
};

beforeEach(() => {
  vi.clearAllMocks();
  useConnectionStore.setState({ state: 'authenticated' as never, error: null });
  mockWsClient.call.mockImplementation((method: string) => {
    switch (method) {
      case 'forward.summary':
        return Promise.resolve({ agents: [summary], window_scanned: 5, window_cap: 5000 });
      case 'forward.recent':
        return Promise.resolve({ predictions: [recentRow] });
      case 'forward.chain':
        return Promise.resolve({ rounds: [chainRound] });
      case 'forward.states':
        return Promise.resolve({ states: [] });
      case 'agents.list':
        return Promise.resolve({ agents: [{ name: 'agnes', display_name: 'Agnes' }] });
      default:
        return Promise.resolve({});
    }
  });
  try { localStorage.clear(); } catch { /* jsdom */ }
});

describe('ForesightPage', () => {
  it('renders the loop strip and per-agent summary', async () => {
    renderWithProviders(<ForesightPage />);
    expect(screen.getByRole('heading', { name: 'Predict & verify' })).toBeInTheDocument();
    expect(await screen.findByText('agnes')).toBeInTheDocument();
    // The four loop stages are drawn.
    for (const stage of ['Predict', 'Act', 'Observe', 'Score']) {
      expect(screen.getByText(stage)).toBeInTheDocument();
    }
  });

  it('opens the per-task chain with expected-vs-observed on row click', async () => {
    const user = userEvent.setup();
    renderWithProviders(<ForesightPage />);
    const row = await screen.findByText(/task-abc/);
    await user.click(row);
    expect(mockWsClient.call).toHaveBeenCalledWith('forward.chain', { task_id: 'task-abc-123' });
    expect(await screen.findByText('Predicted')).toBeInTheDocument();
    expect(screen.getByText('Actual')).toBeInTheDocument();
    // Call band predicted vs observed calls, compared side by side in the
    // chain table (the loop-strip also renders bare numbers, so scope the
    // observed-calls assertion to the table row).
    const bandCell = screen.getByText('2–8');
    expect(bandCell).toBeInTheDocument();
    const bandRow = bandCell.closest('tr');
    expect(bandRow).not.toBeNull();
    expect(bandRow!.textContent).toContain('5');
  });
});
