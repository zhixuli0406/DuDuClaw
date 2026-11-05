import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen, waitFor, act, fireEvent } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { useConnectionStore } from '@/stores/connection-store';
import { useAuthStore } from '@/stores/auth-store';
import { RoutinesPage } from './RoutinesPage';

/** Handlers registered via `client.subscribe`, keyed by event name — lets a
 *  test fire a server push the way the gateway would. */
const wsHandlers = new Map<string, (payload: unknown) => void>();

beforeEach(() => {
  vi.clearAllMocks();
  useAuthStore.setState({ user: null, bindings: [] });
  wsHandlers.clear();
  mockWsClient.call.mockResolvedValue({ tasks: [] });
  mockWsClient.subscribe.mockImplementation((event: string, handler: (p: unknown) => void) => {
    wsHandlers.set(event, handler);
    return () => wsHandlers.delete(event);
  });
  useConnectionStore.setState({ state: 'disconnected' });
});

describe('RoutinesPage', () => {
  it('renders the collection header with the create action', () => {
    renderWithProviders(<RoutinesPage />);
    expect(screen.getByRole('heading', { name: 'Automations' })).toBeInTheDocument();
    // Primary create CTA (routines.add).
    expect(screen.getAllByText('New automation').length).toBeGreaterThan(0);
  });

  // ── WP6: channel-action → dashboard live feedback ──────────────────────

  it('subscribes to cron.changed once authenticated', async () => {
    useConnectionStore.setState({ state: 'authenticated' });
    renderWithProviders(<RoutinesPage />);
    await waitFor(() => expect(wsHandlers.has('cron.changed')).toBe(true));
  });

  it('refetches the routine list when a cron.changed push arrives', async () => {
    useConnectionStore.setState({ state: 'authenticated' });
    renderWithProviders(<RoutinesPage />);

    await waitFor(() =>
      expect(mockWsClient.call.mock.calls.some((c) => c[0] === 'cron.list')).toBe(true),
    );
    const before = mockWsClient.call.mock.calls.filter((c) => c[0] === 'cron.list').length;

    // A routine created from Telegram: the gateway pushes `cron.changed`.
    wsHandlers.get('cron.changed')?.({ action: 'created', name: '每日晨報' });

    await waitFor(
      () => {
        const after = mockWsClient.call.mock.calls.filter((c) => c[0] === 'cron.list').length;
        expect(after).toBeGreaterThan(before);
      },
      { timeout: 2000 },
    );
  });

  // M4 — a bulk cron edit raises one event per row; the page must not fire one
  // RPC per event.
  it('collapses a burst of cron.changed pushes into a single refetch', async () => {
    vi.useFakeTimers();
    try {
      useConnectionStore.setState({ state: 'authenticated' });
      renderWithProviders(<RoutinesPage />);
      await vi.waitFor(() =>
        expect(mockWsClient.call.mock.calls.some((c) => c[0] === 'cron.list')).toBe(true),
      );
      const before = mockWsClient.call.mock.calls.filter((c) => c[0] === 'cron.list').length;

      const push = wsHandlers.get('cron.changed');
      expect(push).toBeDefined();
      for (let i = 0; i < 6; i++) push?.({ action: 'updated', id: `t${i}` });

      // Nothing yet — the trailing window is still open.
      expect(mockWsClient.call.mock.calls.filter((c) => c[0] === 'cron.list').length).toBe(before);

      await vi.advanceTimersByTimeAsync(500);
      expect(mockWsClient.call.mock.calls.filter((c) => c[0] === 'cron.list').length).toBe(
        before + 1,
      );
    } finally {
      vi.useRealTimers();
    }
  });

  // M3 — a reconnect must re-read: pushes raised while the socket was down
  // are gone, so without this the list is stranded on pre-outage state.
  it('refetches after the socket reconnects', async () => {
    useConnectionStore.setState({ state: 'authenticated' });
    renderWithProviders(<RoutinesPage />);
    await waitFor(() =>
      expect(mockWsClient.call.mock.calls.some((c) => c[0] === 'cron.list')).toBe(true),
    );
    const before = mockWsClient.call.mock.calls.filter((c) => c[0] === 'cron.list').length;

    // Two separate renders — batching them into one would keep the effect
    // dependency at 'authenticated' and the test would prove nothing.
    await act(async () => {
      useConnectionStore.setState({ state: 'disconnected' });
    });
    await act(async () => {
      useConnectionStore.setState({ state: 'authenticated' });
    });

    await waitFor(() => {
      const after = mockWsClient.call.mock.calls.filter((c) => c[0] === 'cron.list').length;
      expect(after).toBeGreaterThan(before);
    });
  });

  it('does not subscribe while the socket is unauthenticated', () => {
    renderWithProviders(<RoutinesPage />);
    expect(wsHandlers.has('cron.changed')).toBe(false);
  });
});


describe('employee automations', () => {
  const employee = { id: 'viewer', email: 'viewer@example.invalid', display_name: 'Viewer', role: 'employee' as const, status: 'active' };
  it('shows viewer rows without mutation controls or create action', async () => {
    useAuthStore.setState({ user: employee, bindings: [{ user_id: employee.id, agent_name: 'sales', access_level: 'viewer', bound_at: '' }] });
    useConnectionStore.setState({ state: 'authenticated' });
    mockWsClient.call.mockImplementation((method: string) => Promise.resolve(method === 'cron.list' ? {
      tasks: [{ id: 'read-only', name: 'Visible schedule', agent_id: 'sales', cron: '0 9 * * *', enabled: true,
        can_manage: false, can_edit: false, can_resume: false, can_run: false }],
    } : {}));
    renderWithProviders(<RoutinesPage />);
    await screen.findByText('Visible schedule');
    expect(screen.queryByText('New automation')).not.toBeInTheDocument();
    // Base UI renders a span switch, whose disabled state is expressed through ARIA.
    const toggle = screen.getByRole('switch');
    expect(toggle).toHaveAttribute('aria-disabled', 'true');
    fireEvent.click(toggle);
    expect(toggle).toHaveAttribute('aria-checked', 'true');
    expect(mockWsClient.call.mock.calls.some((c) => c[0] === 'cron.pause' || c[0] === 'cron.resume')).toBe(false);
    expect(screen.queryByRole('button', { name: 'More actions' })).not.toBeInTheDocument();
    expect(mockWsClient.call).toHaveBeenCalledWith('assign_schedules.list', { scope: 'accessible' });
    expect(mockWsClient.call.mock.calls.some((c) => c[0] === 'reminders.list')).toBe(true);
  });

  it('drops cached admin rows and late replies after role downgrade', async () => {
    useAuthStore.setState({ user: { ...employee, role: 'admin' }, bindings: [] });
    useConnectionStore.setState({ state: 'authenticated' });
    let resolveOld!: (value: unknown) => void;
    let requests = 0;
    mockWsClient.call.mockImplementation((method: string) => {
      if (method !== 'cron.list') return Promise.resolve({});
      requests += 1;
      return requests === 1 ? new Promise((resolve) => { resolveOld = resolve; }) : Promise.resolve({ tasks: [] });
    });
    renderWithProviders(<RoutinesPage />);
    await waitFor(() => expect(requests).toBe(1));
    act(() => useAuthStore.setState({ user: employee, bindings: [] }));
    await waitFor(() => expect(requests).toBe(2));
    await act(async () => resolveOld({ tasks: [{ id: 'private', name: 'Admin confidential', agent_id: 'private', cron: '0 9 * * *', enabled: true }] }));
    expect(screen.queryByText('Admin confidential')).not.toBeInTheDocument();
  });
});
