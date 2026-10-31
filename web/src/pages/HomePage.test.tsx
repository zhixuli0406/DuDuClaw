import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import { MemoryRouter } from 'react-router';
import { IntlProvider } from 'react-intl';
import en from '@/i18n/en.json';
import userEvent from '@testing-library/user-event';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { HomePage, greetingName } from './HomePage';
import { useAuthStore } from '@/stores/auth-store';
import { useConnectionStore } from '@/stores/connection-store';
import { useAgentsStore } from '@/stores/agents-store';
import { useSystemStore } from '@/stores/system-store';

/**
 * WP1.5 — Multica-style home overview smoke tests. Locks in the reading-type
 * layout: the greeting line, the fixed 用量摘要 KPI strip, and the removal of the
 * PixiJS world stage from the home canvas (it now lives on `/world`).
 */
beforeEach(() => {
  vi.clearAllMocks();
  // A superset payload satisfies every home RPC accessor (each reads its own
  // field with a `?? []` guard); `available:false` keeps the KPI tiles honest.
  mockWsClient.call.mockResolvedValue({
    widgets: [],
    layout: null,
    tasks: [],
    approvals: [],
    incidents: [],
    events: [],
    channels: [],
    users: [],
    available: false,
  });
  useAgentsStore.setState({ agents: [], loading: false, error: null, fetchAgents: vi.fn() });
  useConnectionStore.setState({ state: 'authenticated' });
  useSystemStore.setState({ status: null } as never);
  useAuthStore.setState({
    user: { id: 'u1', display_name: 'Alice', role: 'employee' } as never,
    bindings: [],
  });
  try {
    localStorage.clear();
  } catch {
    /* jsdom */
  }
});

describe('HomePage (WP1.5)', () => {
  it('never renders viewer widgets while another dashboard is loading or unavailable', async () => {
    useAuthStore.setState({ user: { id: 'u1', display_name: 'Alice', role: 'admin' } as never });
    let resolveView!: (value: unknown) => void;
    const pendingView = new Promise((resolve) => { resolveView = resolve; });
    mockWsClient.call.mockImplementation((method) => method === 'dashboard.layout.view'
      ? pendingView
      : Promise.resolve({ widgets: [{ id: 'needs_me' }, { id: 'my_tasks' }], layout: null, tasks: [], users: [], count: 99, available: false }));
    render(
      <IntlProvider messages={en} locale="en">
        <MemoryRouter initialEntries={['/?view_as=bob']}><HomePage /></MemoryRouter>
      </IntlProvider>,
    );
    expect(screen.getByText('Loading...')).toBeInTheDocument();
    expect(screen.queryByText('Cost (24h)')).not.toBeInTheDocument();
    expect(screen.queryByText('Edit layout')).not.toBeInTheDocument();
    resolveView({
      user: { display_name: 'Bob', role: 'employee' },
      widgets: [{ id: 'needs_me' }, { id: 'my_tasks' }],
      layout: { widgets: [{ id: 'needs_me', hidden: false }, { id: 'my_tasks', hidden: false }] },
      bound_agents: [], custom_widgets: [],
    });
    expect(await screen.findByText("This member's dashboard data is not available here.")).toBeInTheDocument();
    expect(screen.getByText(/Viewing Bob's personal dashboard/)).toBeInTheDocument();
    for (const label of ['Alice', 'Cost (24h)', 'Edit layout', 'Recent tasks']) {
      expect(screen.queryByText(label)).not.toBeInTheDocument();
    }
    expect(mockWsClient.call.mock.calls.map(([method]) => method)).not.toContain('dashboard.layout.get');
  });

  it('renders the greeting line and the usage KPI strip for an admin', async () => {
    useAuthStore.setState({ user: { id: 'u1', display_name: 'Alice', role: 'admin' } as never });
    renderWithProviders(<HomePage />);

    // Admin-only KPI tiles remain visible even without telemetry.
    expect(await screen.findByText('Cost (24h)')).toBeInTheDocument();
    expect(screen.getByText('Tokens (24h)')).toBeInTheDocument();
    expect(screen.getByText('Runs (24h)')).toBeInTheDocument();
    expect(screen.getByText('Cache efficiency')).toBeInTheDocument();

    // Greeting heading carries the user's name (bucket varies by time of day).
    const heading = screen.getByRole('heading', { level: 1 });
    expect(heading.textContent).toContain('Alice');
  });

  it.each(['employee', 'manager'] as const)('loads the %s home without cost tiles or restricted RPCs', async (role) => {
    useAuthStore.setState({ user: { id: 'u1', display_name: 'Alice', role } as never });
    renderWithProviders(<HomePage />);
    expect(await screen.findByRole('heading', { level: 1 })).toHaveTextContent('Alice');
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('dashboard.layout.get'));
    expect(screen.getByText('Edit layout')).toBeInTheDocument();
    for (const label of ['Cost (24h)', 'Tokens (24h)', 'Runs (24h)', 'Cache efficiency']) {
      expect(screen.queryByText(label)).not.toBeInTheDocument();
    }
    const methods = mockWsClient.call.mock.calls.map(([method]) => method);
    expect(methods).not.toContain('cost.summary');
    expect(methods).not.toContain('billing.usage');
    if (role === 'employee') expect(methods).not.toContain('growth.daily_report');
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
  });

  it('does not mount the world stage on the home canvas', async () => {
    renderWithProviders(<HomePage />);
    await screen.findByRole('heading', { level: 1 });

    // The world stage moved to its own `/world` page — no office scene here.
    expect(screen.queryByLabelText('Office scene')).toBeNull();
    expect(document.querySelector('canvas')).toBeNull();
  });

  it('offers the layout edit affordance', async () => {
    const user = userEvent.setup();
    renderWithProviders(<HomePage />);
    await user.click(await screen.findByRole('button', { name: 'Edit layout' }));
    expect(screen.getByRole('button', { name: 'Done' })).toBeInTheDocument();
    expect(screen.queryByText('Cost (24h)')).not.toBeInTheDocument();
  });
});

/**
 * WP18-C (2026-08-04 client report): a fresh single-owner install greeted its
 * owner with 「晚安 Administrator」 — the seeded DB placeholder read back as a
 * name. The onboarding wizard now asks 「怎麼稱呼你」 and writes it to the
 * account; when it is left blank the greeting falls back to the warm default.
 */
describe('HomePage greeting name (WP18-C)', () => {
  it('shows the name the owner set, and falls back when only the placeholder is stored', () => {
    // Set → shown, on both editions.
    expect(greetingName('Louis', true, 'there')).toBe('Louis');
    expect(greetingName('Louis', false, 'there')).toBe('Louis');
    // Not set → the seeded placeholder is treated as absent on Personal…
    expect(greetingName('Administrator', true, 'there')).toBe('there');
    expect(greetingName('  ', true, 'there')).toBe('there');
    expect(greetingName(undefined, true, 'there')).toBe('there');
    // …but Enterprise is untouched: an admin typed that name on purpose.
    expect(greetingName('Administrator', false, 'there')).toBe('Administrator');
  });

  it('personal edition: renders the fallback instead of the seeded placeholder', async () => {
    useSystemStore.setState({ status: { edition_profile: 'personal' } as never });
    useAuthStore.setState({
      user: { user_id: 'u1', id: 'u1', display_name: 'Administrator', role: 'admin' } as never,
    });
    renderWithProviders(<HomePage />);
    await screen.findByText('Cost (24h)');

    const heading = screen.getByRole('heading', { level: 1 });
    expect(heading.textContent).not.toContain('Administrator');
    expect(heading.textContent).toContain('there');
  });

  it('personal edition: renders the name saved by the onboarding wizard', async () => {
    useSystemStore.setState({ status: { edition_profile: 'personal' } as never });
    useAuthStore.setState({
      user: { user_id: 'u1', id: 'u1', display_name: '小李', role: 'admin' } as never,
    });
    renderWithProviders(<HomePage />);
    await screen.findByText('Cost (24h)');

    expect(screen.getByRole('heading', { level: 1 }).textContent).toContain('小李');
  });
});
