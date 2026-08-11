import { describe, it, expect, vi, beforeEach } from 'vitest';
import { fireEvent, screen, waitFor } from '@testing-library/react';
import { useLocation } from 'react-router';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { DevPanelNotificationsTab } from './DevPanelNotificationsTab';
import { useConnectionStore } from '@/stores/connection-store';

function LocationProbe() {
  const location = useLocation();
  return <div data-testid="location-probe">{location.pathname}</div>;
}

function mockRpc(overrides: Partial<Record<string, unknown>> = {}) {
  mockWsClient.call.mockImplementation((method: string) => {
    if (method in overrides) return Promise.resolve(overrides[method]);
    switch (method) {
      case 'notify.stats':
        return Promise.resolve({ days: 30, broken_threshold: 0.5, min_sample: 5, types: [] });
      case 'audit.unified_log':
        return Promise.resolve({
          events: [],
          source_counts: { security: 0, tool_call: 0, channel_failure: 0, feedback: 0 },
          total: 0,
        });
      case 'chat.sessions.list':
        return Promise.resolve({ sessions: [] });
      default:
        return Promise.resolve({});
    }
  });
}

beforeEach(() => {
  vi.clearAllMocks();
  useConnectionStore.setState({ state: 'authenticated' as never, error: null } as never);
  mockRpc();
});

describe('DevPanelNotificationsTab — notify.stats table', () => {
  it('shows the empty state when there is no notification data', async () => {
    renderWithProviders(<DevPanelNotificationsTab />);
    expect(await screen.findByText('No notification data in the last 30 days')).toBeInTheDocument();
  });

  it('renders a broken type with the warning icon and destructive action rate', async () => {
    mockRpc({
      'notify.stats': {
        days: 30,
        broken_threshold: 0.5,
        min_sample: 5,
        types: [
          { type: 'decision.approval', pushed: 10, actionable: 10, acted: 2, action_rate: 0.2, broken: true },
        ],
      },
    });
    renderWithProviders(<DevPanelNotificationsTab />);

    expect(await screen.findByText('decision.approval')).toBeInTheDocument();
    expect(screen.getByText('20%')).toBeInTheDocument();
    expect(screen.getByText('2/10')).toBeInTheDocument();
  });

  it('renders an FYI-only type without an action-rate percentage', async () => {
    mockRpc({
      'notify.stats': {
        days: 30,
        broken_threshold: 0.5,
        min_sample: 5,
        types: [{ type: 'evolution.stagnation', pushed: 3, actionable: 0, acted: 0, action_rate: 0, broken: false }],
      },
    });
    renderWithProviders(<DevPanelNotificationsTab />);

    expect(await screen.findByText('evolution.stagnation')).toBeInTheDocument();
    expect(screen.getAllByText('FYI only, nothing actionable').length).toBeGreaterThan(0);
  });
});

describe('DevPanelNotificationsTab — channel failures list', () => {
  it('shows the empty state when there are no channel failures', async () => {
    renderWithProviders(<DevPanelNotificationsTab />);
    expect(await screen.findByText('No channel failures recorded')).toBeInTheDocument();
  });

  it('renders console_url / doc_url as real links, and lets the agent id navigate', async () => {
    mockRpc({
      'audit.unified_log': {
        events: [
          {
            timestamp: '2026-08-11T09:00:00Z',
            source: 'channel_failure',
            event_type: 'channel.Billing',
            agent_id: 'nova',
            severity: 'warning',
            summary: 'billing exhausted',
            details: {
              channel_failure: {
                agent: 'nova',
                channel: 'telegram',
                reason: 'Billing',
                console_url: 'http://127.0.0.1:8787/manage/accounts',
                doc_url: 'https://github.com/zhixuli0406/DuDuClaw/blob/main/docs/features/zh-TW/07-account-rotation.md',
              },
            },
          },
        ],
        source_counts: { security: 0, tool_call: 0, channel_failure: 1, feedback: 0 },
        total: 1,
      },
    });
    renderWithProviders(
      <>
        <DevPanelNotificationsTab />
        <LocationProbe />
      </>,
    );

    const consoleLink = await screen.findByRole('link', { name: /Console/ });
    expect(consoleLink).toHaveAttribute('href', 'http://127.0.0.1:8787/manage/accounts');
    const docLink = screen.getByRole('link', { name: /Docs/ });
    expect(docLink).toHaveAttribute(
      'href',
      'https://github.com/zhixuli0406/DuDuClaw/blob/main/docs/features/zh-TW/07-account-rotation.md',
    );

    fireEvent.click(screen.getByText('nova'));
    await waitFor(() =>
      expect(screen.getByTestId('location-probe')).toHaveTextContent('/agents/nova'),
    );
  });

  it('omits the link row entirely when neither console_url nor doc_url is present', async () => {
    mockRpc({
      'audit.unified_log': {
        events: [
          {
            timestamp: '2026-08-11T09:00:00Z',
            source: 'channel_failure',
            event_type: 'channel.Timeout',
            agent_id: 'nova',
            severity: 'warning',
            summary: 'timed out',
            details: { channel_failure: { agent: 'nova', reason: 'Timeout', console_url: null, doc_url: null } },
          },
        ],
        source_counts: { security: 0, tool_call: 0, channel_failure: 1, feedback: 0 },
        total: 1,
      },
    });
    renderWithProviders(<DevPanelNotificationsTab />);

    await screen.findByText('timed out');
    expect(screen.queryByRole('link')).not.toBeInTheDocument();
  });
});
