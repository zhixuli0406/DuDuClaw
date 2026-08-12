import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { useConnectionStore } from '@/stores/connection-store';
import { MemoryDecayOverview } from './MemoryDecayOverview';

/** A gateway response with one memory in every freshness state. */
const OVERVIEW = {
  total: 12,
  scanned: 12,
  truncated: false,
  buckets: [
    { key: 'fresh', count: 5, min_retrievability: 0.7 },
    { key: 'stable', count: 4, min_retrievability: 0.4 },
    { key: 'fading', count: 2, min_retrievability: 0.15 },
    { key: 'archiving', count: 1, min_retrievability: 0 },
  ],
  fading_soon: [
    {
      id: 'f1',
      agent_id: 'agnes',
      content: '客戶偏好下午開會',
      timestamp: '2026-06-01T10:00:00Z',
      tags: [],
      retrievability: 0.08,
      stability_days: 14,
    },
  ],
  most_recalled: [
    {
      id: 'r1',
      agent_id: 'agnes',
      content: '老闆喜歡簡短的回覆語氣',
      timestamp: '2026-08-01T10:00:00Z',
      tags: [],
      access_count: 40,
      retrievability: 0.92,
      stability_days: 70,
    },
  ],
  trend: [
    { date: '2026-08-10', added: 0, total: 8 },
    { date: '2026-08-11', added: 2, total: 10 },
    { date: '2026-08-12', added: 2, total: 12 },
  ],
  window_days: 30,
  archive_threshold: 0.05,
};

function mockOverview(payload: unknown) {
  mockWsClient.call.mockImplementation((method: string) => {
    if (method === 'memory.decay_overview') return Promise.resolve(payload);
    return Promise.resolve({});
  });
}

beforeEach(() => {
  vi.clearAllMocks();
  mockOverview(OVERVIEW);
  useConnectionStore.setState({ state: 'authenticated' });
});

describe('MemoryDecayOverview', () => {
  it('draws both charts and describes them for screen readers', async () => {
    renderWithProviders(<MemoryDecayOverview agentId="agnes" />);

    // Trend: one series, so its only direct label is the endpoint total, and
    // the whole reading is also available as an aria-label and a caption.
    const trend = await screen.findByRole('img', { name: /What it has learned/ });
    expect(trend).toHaveAccessibleName(
      'What it has learned: 12 in total, 4 added over 3 days',
    );
    expect(screen.getByText('4 added in this period')).toBeInTheDocument();

    // Distribution: every state is named and counted, so the colour is never
    // the only channel carrying identity.
    const distribution = screen.getByRole('img', { name: /How well it remembers/ });
    expect(distribution).toHaveAccessibleName(
      'How well it remembers: Fresh 5 · Steady 4 · Fading 2 · Nearly gone 1',
    );
    for (const state of ['Fresh', 'Steady', 'Fading', 'Nearly gone']) {
      expect(screen.getByText(state)).toBeInTheDocument();
    }
  });

  it('lists the memories closest to being forgotten and the most used ones', async () => {
    renderWithProviders(<MemoryDecayOverview agentId="agnes" />);

    expect(await screen.findByText('Closest to being forgotten')).toBeInTheDocument();
    expect(screen.getByText(/客戶偏好下午開會/)).toBeInTheDocument();
    expect(screen.getByText('Used most often')).toBeInTheDocument();
    expect(screen.getByText(/老闆喜歡簡短的回覆語氣/)).toBeInTheDocument();
  });

  it('renders nothing at all before anything has been learned', async () => {
    mockOverview({ ...OVERVIEW, total: 0, buckets: [], trend: [], fading_soon: [], most_recalled: [] });
    const { container } = renderWithProviders(<MemoryDecayOverview agentId="agnes" />);

    // An empty pair of charts would be noise on top of the list's own empty
    // state, which already explains what puts things here.
    await waitFor(() => expect(container).toBeEmptyDOMElement());
  });

  it('survives a gateway that does not know the call yet', async () => {
    mockOverview({});
    const { container } = renderWithProviders(<MemoryDecayOverview agentId="agnes" />);
    await waitFor(() => expect(container).toBeEmptyDOMElement());
  });

  it('refetches when the period is switched', async () => {
    const user = userEvent.setup();
    renderWithProviders(<MemoryDecayOverview agentId="agnes" />);
    await screen.findByRole('img', { name: /What it has learned/ });

    expect(mockWsClient.call).toHaveBeenCalledWith('memory.decay_overview', {
      agent_id: 'agnes',
      days: 30,
      top_n: 5,
    });

    await user.click(screen.getByRole('radio', { name: 'Last 7 days' }));

    await waitFor(() => {
      expect(mockWsClient.call).toHaveBeenCalledWith('memory.decay_overview', {
        agent_id: 'agnes',
        days: 7,
        top_n: 5,
      });
    });
  });

  it('says so when the numbers describe a recent slice only', async () => {
    mockOverview({ ...OVERVIEW, truncated: true, scanned: 5000 });
    renderWithProviders(<MemoryDecayOverview agentId="agnes" />);

    // Silently reporting a capped scan as the whole pile would misstate the
    // distribution for a heavy agent.
    expect(await screen.findByText('Counting the 5,000 most recent only')).toBeInTheDocument();
  });
});
