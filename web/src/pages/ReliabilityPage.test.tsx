import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { ReliabilityPage } from './ReliabilityPage';
import { useConnectionStore } from '@/stores/connection-store';
import { useAgentsStore } from '@/stores/agents-store';

beforeEach(() => {
  vi.clearAllMocks();
  useConnectionStore.setState({ state: 'authenticated' as never, error: null });
  useAgentsStore.setState({ agents: [], loading: false, loaded: false, error: null });
});

describe('ReliabilityPage', () => {
  it('renders the reliability heading with no agents selected', async () => {
    // agents.list() → { agents: [] } (no agent auto-selected, so the
    // reliability summary / gauges never fetch); evolution_query() also
    // resolves through the same mocked shape but is never called without a
    // selected agent. The page must still render its header.
    mockWsClient.call.mockResolvedValue({ agents: [], events: [], total: 0 });

    renderWithProviders(<ReliabilityPage />);

    expect(await screen.findByText('Reliability')).toBeInTheDocument();
  });

  it('renders gauges and evolution events once an agent is selected', async () => {
    mockWsClient.call.mockImplementation((method: string) => {
      if (method === 'agents.list') {
        return Promise.resolve({ agents: [{ name: 'lab-bot', display_name: 'Lab Bot' }] });
      }
      if (method === 'audit.reliability_summary') {
        return Promise.resolve({
          agent_id: 'lab-bot',
          window_days: 7,
          consistency_score: 0.9,
          task_success_rate: 0.8,
          skill_adoption_rate: 0.5,
          fallback_trigger_rate: 0.05,
          total_events: 12,
          generated_at: new Date().toISOString(),
        });
      }
      if (method === 'audit.evolution_query') {
        return Promise.resolve({ events: [], total: 0, limit: 25, offset: 0 });
      }
      return Promise.resolve(null);
    });

    renderWithProviders(<ReliabilityPage />);

    expect(await screen.findByText('Reliability')).toBeInTheDocument();
    expect(await screen.findByText('Consistency Score')).toBeInTheDocument();
    expect(await screen.findByText('No evolution events yet')).toBeInTheDocument();
  });
});
