import { useCompanyTimeStore } from '@/lib/company-time';
import { toast } from '@/lib/toast';
import { describe, it, expect, vi, beforeEach } from 'vitest';
import userEvent from '@testing-library/user-event';
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
  useCompanyTimeStore.setState({ zone: null, status: 'unset', source: null });
  useAuthStore.setState({ user: null, bindings: [], deptGrants: [], deptGroups: new Set() });
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

  it.each([true, false])('shows the skip status only for a skipped row (%s)', async (skipped) => {
    useConnectionStore.setState({ state: 'authenticated' });
    mockWsClient.call.mockImplementation((method: string) => Promise.resolve(method === 'cron.list' ? {
      tasks: [{ id: 'schedule', name: 'Morning report', agent_id: 'sales', cron: '0 9 * * *', enabled: true,
        last_status: skipped ? 'skipped' : 'success', skip_reason: skipped ? 'timezone_or_interval' : null,
        last_delivery_status: 'delivered' }],
    } : {}));
    renderWithProviders(<RoutinesPage />);
    await screen.findByText('Morning report');
    if (skipped) {
      expect(screen.getByText('Skipped')).toBeInTheDocument();
      expect(screen.getByText(/This schedule did not run because/)).toBeInTheDocument();
      expect(screen.queryByText('Delivered')).not.toBeInTheDocument();
    } else {
      expect(screen.queryByText('Skipped')).not.toBeInTheDocument();
      expect(screen.queryByText(/This schedule did not run because/)).not.toBeInTheDocument();
      expect(screen.getByText('Delivered')).toBeInTheDocument();
    }
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

describe('routine delivery status', () => {
  it('shows delivered and failed delivery independently of run time', async () => {
    useConnectionStore.setState({ state: 'authenticated' });
    mockWsClient.call.mockImplementation((method: string) => Promise.resolve(method === 'cron.list' ? {
      tasks: [
        { id: 'failed-delivery', name: 'Failed delivery', agent_id: 'sales', cron: '0 9 * * *', enabled: true,
          last_status: 'success', last_delivery_status: 'delivery_failed', last_delivery_reason: 'notification_delivery_failed' },
        { id: 'delivered', name: 'Delivered routine', agent_id: 'sales', cron: '0 9 * * *', enabled: true,
          last_delivery_status: 'delivered' },
        { id: 'refused', name: 'Refused routine', agent_id: 'sales', cron: '0 9 * * *', enabled: true,
          last_delivery_status: 'delivery_refused', last_delivery_reason: 'origin_access_revoked' },
        { id: 'old', name: 'Older routine', agent_id: 'sales', cron: '0 9 * * *', enabled: true },
      ],
    } : {}));
    renderWithProviders(<RoutinesPage />);
    expect(await screen.findByText('Delivery failed')).toBeInTheDocument();
    expect(screen.getByText('Notification could not be delivered. Check the channel settings.')).toBeInTheDocument();
    expect(screen.getByText('Delivered')).toBeInTheDocument();
    expect(screen.getByText('No delivery recorded')).toBeInTheDocument();
    expect(screen.getByText('Delivery refused')).toBeInTheDocument();
    expect(screen.getByText('Destination access changed. Review the notification destination.')).toBeInTheDocument();
  });
});


describe('routine time zones', () => {
  it('shows the stored zone and UTC for legacy rows', async () => {
    useConnectionStore.setState({ state: 'authenticated' });
    mockWsClient.call.mockImplementation((method: string) => Promise.resolve(method === 'cron.list' ? {
      tasks: [
        { id: 'new', name: 'New', agent_id: 'sales', cron: '0 9 * * *', enabled: true, cron_timezone: 'Asia/Taipei' },
        { id: 'old', name: 'Old', agent_id: 'sales', cron: '0 8 * * *', enabled: true, cron_timezone: null },
      ],
    } : {}));
    renderWithProviders(<RoutinesPage />);
    expect(await screen.findByText('0 9 * * * (Asia/Taipei)')).toBeInTheDocument();
    expect(screen.getByText('0 8 * * * (UTC)')).toBeInTheDocument();
  });

  it.each(['set', 'unset'] as const)('explains the create zone (%s) and shows the server notice', async (status) => {
    const success = vi.spyOn(toast, 'success');
    try {
      useCompanyTimeStore.setState({ zone: 'Asia/Taipei', status, source: null });
      useConnectionStore.setState({ state: 'authenticated' });
      mockWsClient.call.mockImplementation((method: string) => Promise.resolve(
        method === 'cron.list' ? { tasks: [] } : method === 'cron.add' ? { zone_note: 'Server fallback notice' } : {},
      ));
      renderWithProviders(<RoutinesPage />);
      fireEvent.click((await screen.findAllByText('New automation'))[0]);
      expect(await screen.findByText(status === 'set' ? 'Runs in the company time zone (Asia/Taipei)' : 'Company time zone is not set; runs in UTC')).toBeInTheDocument();
      fireEvent.change(screen.getByLabelText('Task Name'), { target: { value: 'Morning' } });
      fireEvent.click(screen.getByRole('button', { name: 'Save' }));
      await waitFor(() => expect(success).toHaveBeenCalledWith(expect.stringContaining('Server fallback notice')));
    } finally {
      success.mockRestore();
    }
  });
});

it('filters department schedules using granted employees and reports employees needing sync', async () => {
  useAuthStore.setState({ user: { id: 'manager', role: 'manager' } as never, bindings: [],
    deptGrants: [{ department: 'Sales', groups: ['schedules'] }, { department: 'Support', groups: ['schedules'] }] });
  useConnectionStore.setState({ state: 'authenticated' });
  mockWsClient.call.mockImplementation((method: string) => Promise.resolve(
    method === 'users.dept_grants.mine' ? { agents: [{ id: 'agnes', display_name: 'Agnes', groups: ['schedules'] }, { id: 'bruno', display_name: 'Bruno', groups: ['schedules'] }], needs_sync: [{ id: 'drift', display_name: 'Drift' }] }
    : method === 'agents.list' ? { agents: [{ name: 'agnes', department: 'Sales' }, { name: 'bruno', department: 'Support' }] }
    : method === 'cron.list' ? { tasks: [{ id: 'a', name: 'Sales report', agent_id: 'agnes', enabled: true, can_manage: true }, { id: 'b', name: 'Support report', agent_id: 'bruno', enabled: true, can_manage: false }] } : {}));
  renderWithProviders(<RoutinesPage />);
  await screen.findByText('Sales report');
  await screen.findByText('1 AI employee needs an administrator to sync their department details before you can manage them.');
  fireEvent.change(screen.getByRole('combobox', { name: 'Department' }), { target: { value: 'Sales' } });
  await waitFor(() => expect(screen.queryByText('Support report')).not.toBeInTheDocument());
  expect(screen.getByText('Sales report')).toBeInTheDocument();
  expect(screen.getByRole('switch')).not.toHaveAttribute('data-disabled');
  expect(screen.getByText('New automation')).toBeInTheDocument();
  expect(mockWsClient.call).toHaveBeenCalledWith('users.dept_grants.mine', { group: 'schedules' });
});


it('uses each server action flag for a department manager', async () => {
  const user = userEvent.setup();
  useAuthStore.setState({ user: { id: 'manager', role: 'manager' } as never, bindings: [],
    deptGrants: [{ department: 'Sales', groups: ['schedules'] }] });
  useConnectionStore.setState({ state: 'authenticated' });
  mockWsClient.call.mockImplementation((method: string) => Promise.resolve(method === 'cron.list' ? {
    tasks: [{ id: 'restricted', name: 'Restricted schedule', agent_id: 'agnes', enabled: false,
      can_manage: true, can_edit: false, can_resume: false, can_run: false }],
  } : {}));
  renderWithProviders(<RoutinesPage />);
  await screen.findByText('Restricted schedule');
  expect(screen.getByRole('switch')).toHaveAttribute('data-disabled');
  await user.click(screen.getByRole('button', { name: 'More actions' }));
  expect(await screen.findByRole('menuitem', { name: 'Test run' })).toHaveAttribute('data-disabled');
  expect(screen.getByRole('menuitem', { name: 'Edit' })).toHaveAttribute('data-disabled');
  expect(screen.getByRole('menuitem', { name: 'Resume' })).toHaveAttribute('data-disabled');
  expect(screen.getByRole('menuitem', { name: 'Remove' })).not.toHaveAttribute('data-disabled');
});
