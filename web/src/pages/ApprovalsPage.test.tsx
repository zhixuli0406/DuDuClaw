import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { ApprovalsPage } from './ApprovalsPage';
import { useConnectionStore } from '@/stores/connection-store';

beforeEach(() => {
  vi.clearAllMocks();
  useConnectionStore.setState({ state: 'authenticated' as never, error: null });
  // Benign empty object keeps every optional chain safe (approvals.list → {}).
  mockWsClient.call.mockResolvedValue({});
});

describe('ApprovalsPage (MDS)', () => {
  it('renders the header and the empty state when nothing is pending', async () => {
    renderWithProviders(<ApprovalsPage />);
    expect(await screen.findByText('Approval center')).toBeInTheDocument();
    expect(await screen.findByText('Nothing waiting for approval')).toBeInTheDocument();
  });

  it('renders the D1/D2 simulation narrative on a card when present, and omits it when absent', async () => {
    mockWsClient.call.mockImplementation((method: string) => {
      if (method === 'approvals.list') {
        return Promise.resolve({
          count: 2,
          approvals: [
            {
              id: 'apr-1',
              agent_id: 'agent-a',
              kind: 'tool_call',
              summary: 'Run a shell command',
              payload: {},
              created_at: '2026-07-11T00:00:00Z',
              ttl_seconds: 3600,
              simulation: {
                world_state_change: 'A refund email will be sent to the customer.',
                risk_points: ['Amount miscalculation is hard to reverse'],
              },
            },
            {
              id: 'apr-2',
              agent_id: 'agent-b',
              kind: 'wiki_ingest',
              summary: 'Write a page into the shared wiki',
              payload: {},
              created_at: '2026-07-11T00:00:00Z',
              ttl_seconds: 3600,
            },
          ],
        });
      }
      return Promise.resolve({});
    });
    renderWithProviders(<ApprovalsPage />);
    expect(await screen.findByText('A refund email will be sent to the customer.')).toBeInTheDocument();
    // The second card has no `simulation` field — nothing extra renders for it.
    expect(screen.getByText('Write a page into the shared wiki')).toBeInTheDocument();
  });
});
