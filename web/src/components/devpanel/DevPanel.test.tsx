import { describe, it, expect, vi, beforeEach } from 'vitest';
import { fireEvent, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { DevPanel } from './DevPanel';
import { useAuthStore } from '@/stores/auth-store';
import { useConnectionStore } from '@/stores/connection-store';
import { useSystemStore } from '@/stores/system-store';
import { useDevPanelStore } from '@/stores/devpanel-store';

/**
 * W3-4 developer panel — three-tier collapse (expanded pane / minimized
 * taskbar / collapsed icon), the `~` hotkey, the manager+ role gate, the
 * critical-alert badge that survives collapsing, focus trap, and Esc.
 *
 * RPCs are mocked broadly — this suite cares about panel chrome behavior,
 * not the exact shape of `audit.unified_log`/`notify.stats`/`runtime.detect`
 * /`takeover.list` responses (those are covered where they're rendered).
 */

function mockAllRpcCalls(overrides: Partial<Record<string, unknown>> = {}) {
  mockWsClient.call.mockImplementation((method: string) => {
    if (method in overrides) return Promise.resolve(overrides[method]);
    switch (method) {
      case 'audit.unified_log':
        return Promise.resolve({
          events: [],
          source_counts: { security: 0, tool_call: 0, channel_failure: 0, feedback: 0 },
          total: 0,
        });
      case 'notify.stats':
        return Promise.resolve({ days: 30, broken_threshold: 0.5, min_sample: 5, types: [] });
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

function resetDevPanelStore() {
  localStorage.clear();
  useDevPanelStore.setState({
    visibility: 'collapsed',
    paneHeight: 360,
    activeTab: 'events',
    latestCriticalAt: null,
    lastSeenCriticalAt: null,
  });
}

beforeEach(() => {
  vi.clearAllMocks();
  resetDevPanelStore();
  useConnectionStore.setState({ state: 'authenticated' as never, error: null } as never);
  useSystemStore.setState({
    status: {
      version: '1.54.0',
      uptime_seconds: 3725,
      agents_count: 4,
      channels_connected: 2,
      gateway_address: '127.0.0.1:8787',
    },
  } as never);
  mockAllRpcCalls();
});

describe('DevPanel — role gate', () => {
  it('renders nothing for an employee (below manager)', () => {
    useAuthStore.setState({ user: { role: 'employee' } } as never);
    renderWithProviders(<DevPanel />);
    expect(screen.queryByLabelText('Open developer panel')).not.toBeInTheDocument();
  });

  it('renders the collapsed trigger for a manager', () => {
    useAuthStore.setState({ user: { role: 'manager' } } as never);
    renderWithProviders(<DevPanel />);
    expect(screen.getByLabelText('Open developer panel')).toBeInTheDocument();
  });

  it('renders the collapsed trigger for an admin', () => {
    useAuthStore.setState({ user: { role: 'admin' } } as never);
    renderWithProviders(<DevPanel />);
    expect(screen.getByLabelText('Open developer panel')).toBeInTheDocument();
  });
});

describe('DevPanel — three-tier collapse', () => {
  beforeEach(() => {
    useAuthStore.setState({ user: { role: 'manager' } } as never);
  });

  it('clicking the collapsed icon expands the pane with all three tabs', async () => {
    renderWithProviders(<DevPanel />);
    fireEvent.click(screen.getByLabelText('Open developer panel'));

    expect(await screen.findByRole('dialog', { name: 'Developer panel' })).toBeInTheDocument();
    expect(screen.getByRole('tab', { name: /Events/ })).toBeInTheDocument();
    expect(screen.getByRole('tab', { name: /Notifications/ })).toBeInTheDocument();
    expect(screen.getByRole('tab', { name: /System/ })).toBeInTheDocument();
  });

  it('Minimize button collapses the pane to the taskbar', async () => {
    useDevPanelStore.getState().expand();
    renderWithProviders(<DevPanel />);
    await screen.findByRole('dialog', { name: 'Developer panel' });

    fireEvent.click(screen.getByLabelText('Minimize to taskbar'));

    expect(useDevPanelStore.getState().visibility).toBe('minimized');
    expect(screen.queryByRole('dialog', { name: 'Developer panel' })).not.toBeInTheDocument();
    expect(screen.getByRole('toolbar', { name: 'Developer panel' })).toBeInTheDocument();
  });

  it('Collapse (X) button on the taskbar collapses to the icon', async () => {
    useDevPanelStore.getState().minimize();
    renderWithProviders(<DevPanel />);
    await screen.findByRole('toolbar', { name: 'Developer panel' });

    fireEvent.click(screen.getByLabelText('Collapse to icon'));

    expect(useDevPanelStore.getState().visibility).toBe('collapsed');
    expect(screen.getByLabelText('Open developer panel')).toBeInTheDocument();
  });

  it('Escape inside the expanded pane minimizes it (not a full collapse)', async () => {
    useDevPanelStore.getState().expand();
    renderWithProviders(<DevPanel />);
    const dialog = await screen.findByRole('dialog', { name: 'Developer panel' });

    fireEvent.keyDown(dialog, { key: 'Escape' });

    expect(useDevPanelStore.getState().visibility).toBe('minimized');
  });
});

describe('DevPanel — `~` hotkey', () => {
  beforeEach(() => {
    useAuthStore.setState({ user: { role: 'manager' } } as never);
  });

  it('toggles collapsed → expanded, then expanded → minimized', async () => {
    renderWithProviders(<DevPanel />);
    expect(useDevPanelStore.getState().visibility).toBe('collapsed');

    fireEvent.keyDown(document, { key: '~' });
    expect(useDevPanelStore.getState().visibility).toBe('expanded');

    fireEvent.keyDown(document, { key: '~' });
    expect(useDevPanelStore.getState().visibility).toBe('minimized');
  });

  it('is ignored while an input is focused', () => {
    renderWithProviders(
      <>
        <input data-testid="text-field" />
        <DevPanel />
      </>,
    );
    const input = screen.getByTestId('text-field');
    input.focus();

    fireEvent.keyDown(input, { key: '~' });

    expect(useDevPanelStore.getState().visibility).toBe('collapsed');
  });

  it('does not fire for a non-`~` key', () => {
    renderWithProviders(<DevPanel />);
    fireEvent.keyDown(document, { key: 'a' });
    expect(useDevPanelStore.getState().visibility).toBe('collapsed');
  });
});

describe('DevPanel — critical-alert badge', () => {
  beforeEach(() => {
    useAuthStore.setState({ user: { role: 'manager' } } as never);
  });

  it('shows on the collapsed icon when there is an unseen critical event', async () => {
    mockAllRpcCalls({
      'audit.unified_log': {
        events: [
          {
            timestamp: '2026-08-11T00:00:00Z',
            source: 'security',
            event_type: 'security.blocked',
            agent_id: 'nova',
            severity: 'critical',
            summary: 'blocked a dangerous write',
            details: {},
          },
        ],
        source_counts: { security: 1, tool_call: 0, channel_failure: 0, feedback: 0 },
        total: 1,
      },
    });
    renderWithProviders(<DevPanel />);

    await waitFor(() =>
      expect(screen.getByText('New critical events need attention')).toBeInTheDocument(),
    );
  });

  it('does not show when the background poll returns nothing critical', async () => {
    renderWithProviders(<DevPanel />);
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('audit.unified_log', expect.anything()));
    expect(screen.queryByText('New critical events need attention')).not.toBeInTheDocument();
  });
});

describe('DevPanel — tabs', () => {
  beforeEach(() => {
    useAuthStore.setState({ user: { role: 'manager' } } as never);
    useDevPanelStore.getState().expand();
  });

  it('switching to the System tab renders status / runtime / takeover sections', async () => {
    renderWithProviders(<DevPanel />);
    await screen.findByRole('dialog', { name: 'Developer panel' });

    fireEvent.click(screen.getByRole('tab', { name: /System/ }));

    expect(await screen.findByText('System status')).toBeInTheDocument();
    expect(screen.getByText('AI runtimes')).toBeInTheDocument();
    expect(screen.getByText('Active human takeovers')).toBeInTheDocument();
  });

  it('switching to the Notifications tab renders the stats + failures sections', async () => {
    renderWithProviders(<DevPanel />);
    await screen.findByRole('dialog', { name: 'Developer panel' });

    fireEvent.click(screen.getByRole('tab', { name: /Notifications/ }));

    expect(await screen.findByText('Notification action-rate detail')).toBeInTheDocument();
    expect(screen.getByText('Recent channel failures')).toBeInTheDocument();
  });
});

describe('DevPanel — focus trap (a11y)', () => {
  beforeEach(() => {
    useAuthStore.setState({ user: { role: 'manager' } } as never);
    useDevPanelStore.getState().expand();
  });

  it('keeps Tab focus inside the expanded pane, never reaching an outside element', async () => {
    const user = userEvent.setup();
    renderWithProviders(
      <>
        <button type="button">outside sentinel</button>
        <DevPanel />
      </>,
    );
    await screen.findByRole('dialog', { name: 'Developer panel' });

    const dialog = screen.getByRole('dialog', { name: 'Developer panel' });
    const outside = screen.getByText('outside sentinel');

    for (let i = 0; i < 12; i++) {
      await user.tab();
      expect(document.activeElement).not.toBe(outside);
      expect(dialog.contains(document.activeElement)).toBe(true);
    }
  });
});
