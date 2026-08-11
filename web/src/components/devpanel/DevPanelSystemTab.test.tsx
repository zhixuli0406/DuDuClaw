import { describe, it, expect, vi, beforeEach } from 'vitest';
import { fireEvent, screen, waitFor } from '@testing-library/react';
import { useLocation } from 'react-router';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { DevPanelSystemTab } from './DevPanelSystemTab';
import { useConnectionStore } from '@/stores/connection-store';
import { useSystemStore } from '@/stores/system-store';

function LocationProbe() {
  const location = useLocation();
  return <div data-testid="location-probe">{location.pathname}</div>;
}

function mockRpc(overrides: Partial<Record<string, unknown>> = {}) {
  mockWsClient.call.mockImplementation((method: string) => {
    if (method in overrides) return Promise.resolve(overrides[method]);
    switch (method) {
      case 'runtime.detect':
        return Promise.resolve({
          claude_cli: true,
          codex: false,
          gemini: false,
          antigravity: false,
          claude_oauth: true,
          claude_subscription: 'max',
        });
      case 'takeover.list':
        return Promise.resolve({ count: 0, items: [] });
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
  useSystemStore.setState({
    status: {
      version: '1.54.0',
      uptime_seconds: 3725, // 1h 2m
      agents_count: 4,
      channels_connected: 2,
      gateway_address: '127.0.0.1:8787',
    },
  } as never);
  mockRpc();
});

describe('DevPanelSystemTab — system status', () => {
  it('renders version / uptime / agent / channel tiles from the cached system store', () => {
    renderWithProviders(<DevPanelSystemTab />);
    expect(screen.getByText('1.54.0')).toBeInTheDocument();
    expect(screen.getByText('1h 2m')).toBeInTheDocument();
    expect(screen.getByText('4')).toBeInTheDocument();
    expect(screen.getByText('2')).toBeInTheDocument();
  });
});

describe('DevPanelSystemTab — runtime detect', () => {
  it('marks installed providers and shows the OAuth subscription label', async () => {
    renderWithProviders(<DevPanelSystemTab />);
    expect(await screen.findByText('Claude')).toBeInTheDocument();
    expect(screen.getByText(/OAuth.*max/)).toBeInTheDocument();
  });
});

describe('DevPanelSystemTab — takeover list (W3-1, read-only)', () => {
  it('shows the empty state when nobody is currently taking over a conversation', async () => {
    renderWithProviders(<DevPanelSystemTab />);
    expect(await screen.findByText('No conversations are currently taken over')).toBeInTheDocument();
  });

  it('renders an active takeover row with holder, minutes left, claimed tasks, and jump links', async () => {
    mockRpc({
      'takeover.list': {
        count: 1,
        items: [
          {
            conversation: 'telegram:12345',
            channel: 'telegram',
            channel_label: 'Telegram',
            chat_id: '12345',
            agent_id: 'nova',
            holder_display: 'Boss',
            started_at: '2026-08-11T08:00:00Z',
            until: '2026-08-11T09:00:00Z',
            minutes_left: 42,
            claimed_task_ids: ['t1', 't2'],
          },
        ],
      },
      'chat.sessions.list': {
        sessions: [{ session_id: 'telegram:12345', agent_id: 'nova', title: 'x', updated_at: '2026-08-11T08:00:00Z' }],
      },
    });
    renderWithProviders(
      <>
        <DevPanelSystemTab />
        <LocationProbe />
      </>,
    );

    expect(await screen.findByText('Telegram')).toBeInTheDocument();
    expect(screen.getByText('42 min left')).toBeInTheDocument();
    expect(screen.getByText(/Boss/)).toBeInTheDocument();
    expect(screen.getByText('2 claimed tasks')).toBeInTheDocument();

    fireEvent.click(screen.getByText('nova'));
    await waitFor(() =>
      expect(screen.getByTestId('location-probe')).toHaveTextContent('/agents/nova'),
    );
  });
});
