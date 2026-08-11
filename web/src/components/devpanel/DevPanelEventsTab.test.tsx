import { describe, it, expect, vi, beforeEach } from 'vitest';
import { fireEvent, screen, waitFor } from '@testing-library/react';
import { useLocation } from 'react-router';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { DevPanelEventsTab } from './DevPanelEventsTab';
import { useConnectionStore } from '@/stores/connection-store';

function LocationProbe() {
  const location = useLocation();
  return <div data-testid="location-probe">{location.pathname}</div>;
}

const ONE_EVENT = {
  events: [
    {
      timestamp: '2026-08-11T09:00:00Z',
      source: 'channel_failure',
      event_type: 'channel.Billing',
      agent_id: 'nova',
      severity: 'warning',
      summary: 'billing exhausted',
      details: { channel_failure: { reason: 'Billing' } },
    },
  ],
  source_counts: { security: 0, tool_call: 0, channel_failure: 1, feedback: 0 },
  total: 1,
};

beforeEach(() => {
  vi.clearAllMocks();
  useConnectionStore.setState({ state: 'authenticated' as never, error: null } as never);
  mockWsClient.call.mockResolvedValue({
    events: [],
    source_counts: { security: 0, tool_call: 0, channel_failure: 0, feedback: 0 },
    total: 0,
  });
});

describe('DevPanelEventsTab', () => {
  it('shows the empty state when there are no events', async () => {
    renderWithProviders(<DevPanelEventsTab />);
    expect(await screen.findByText('No matching events')).toBeInTheDocument();
  });

  it('renders a row with source badge, agent id, and summary', async () => {
    mockWsClient.call.mockResolvedValue(ONE_EVENT);
    renderWithProviders(<DevPanelEventsTab />);

    expect(await screen.findByText('billing exhausted')).toBeInTheDocument();
    expect(screen.getByText('nova')).toBeInTheDocument();
    // "Channel failure" appears twice: the source filter chip and the row's
    // badge — both are expected, not ambiguous.
    expect(screen.getAllByText('Channel failure')).toHaveLength(2);
  });

  it('expands to show raw JSON details on click', async () => {
    mockWsClient.call.mockResolvedValue(ONE_EVENT);
    renderWithProviders(<DevPanelEventsTab />);
    const row = await screen.findByText('billing exhausted');

    expect(screen.queryByText(/"reason"/)).not.toBeInTheDocument();
    fireEvent.click(row);
    expect(screen.getByText(/"reason"/)).toBeInTheDocument();

    fireEvent.click(row);
    expect(screen.queryByText(/"reason"/)).not.toBeInTheDocument();
  });

  it('clicking the agent id navigates to the agent detail page', async () => {
    mockWsClient.call.mockResolvedValue(ONE_EVENT);
    renderWithProviders(
      <>
        <DevPanelEventsTab />
        <LocationProbe />
      </>,
    );
    fireEvent.click(await screen.findByText('nova'));

    await waitFor(() =>
      expect(screen.getByTestId('location-probe')).toHaveTextContent('/agents/nova'),
    );
  });

  it('clicking a single source chip narrows the RPC to just that source', async () => {
    mockWsClient.call.mockResolvedValue(ONE_EVENT);
    renderWithProviders(<DevPanelEventsTab />);
    await screen.findByText('billing exhausted');

    fireEvent.click(screen.getByText('Security'));

    await waitFor(() => {
      const call = mockWsClient.call.mock.calls.find(
        (c) => c[0] === 'audit.unified_log' && Array.isArray((c[1] as { sources?: string[] })?.sources),
      );
      expect(call).toBeDefined();
      const sources = (call![1] as { sources: string[] }).sources;
      expect(sources).toContain('security');
      expect(sources).not.toContain('channel_failure');
    });
  });
});
