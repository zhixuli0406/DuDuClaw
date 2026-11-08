import { mockWsClient } from '@/test/mocks';
import { mockCallableGrants } from '@/test/callable-grants';
import { useAgentsStore } from '@/stores/agents-store';
import type { ReactElement } from 'react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { act, screen, waitFor, within } from '@testing-library/react';
import { api } from '@/lib/api';
import { renderWithProviders } from '@/test/render';
import { useAuthStore } from '@/stores/auth-store';
import { useConnectionStore } from '@/stores/connection-store';
import type { OrgChartAgent } from '@/lib/api';
import { OrgChartPage } from './OrgChartPage';

const panel = vi.hoisted(() => ({ setPanel: vi.fn(), clearPanel: vi.fn(), setSheetOpen: vi.fn() }));
vi.mock('@/components/ui', () => ({ usePanel: () => panel }));

const chart = vi.hoisted(() => vi.fn());
vi.mock('@/components/OrgChart', () => ({
  OrgChart: (props: { agents: OrgChartAgent[]; onNodeClick?: unknown }) => {
    chart(props);
    return <div>{props.agents.map(a => <span key={a.name}>{a.display_name}</span>)}</div>;
  },
}));

const structure = [{ name: 'finance', display_name: 'Finance assistant', department: 'Finance',
  role: 'specialist', status: 'active', icon: '', reports_to: '' }];

beforeEach(() => {
  vi.clearAllMocks();
  vi.spyOn(api.agents, 'callable').mockResolvedValue({ agents: structure.map(a => ({ ...a, selectable: true, via: 'binding' })), default_agent: null } as never);
  useAuthStore.setState({ user: { id: 'employee-one', email: 'one@test.invalid', display_name: 'Reader',
    role: 'employee', status: 'active' }, bindings: [] });
  useConnectionStore.setState({ state: 'authenticated' });
  mockWsClient.call.mockResolvedValue({ agents: structure });
});

describe('employee organization chart', () => {
  it('uses the structural endpoint without requesting agent configuration or exposing actions', async () => {
    renderWithProviders(<OrgChartPage />);
    expect(await screen.findByText('Finance assistant')).toBeInTheDocument();
    expect(mockWsClient.call).toHaveBeenCalledWith('agents.org_chart', {});
    expect(mockWsClient.call.mock.calls.some(([method]) => ['agents.list', 'agents.inspect'].includes(String(method)))).toBe(false);
    expect(chart.mock.calls.at(-1)?.[0].onNodeClick).toBeUndefined();
    expect(screen.queryByRole('link')).not.toBeInTheDocument();
  });

  it('shows a failure instead of treating a rejected read as an empty organization', async () => {
    mockWsClient.call.mockRejectedValueOnce(new Error('Organization is unavailable'));
    renderWithProviders(<OrgChartPage />);
    expect(await screen.findByRole('alert')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Try again' })).toBeInTheDocument();
    expect(chart).not.toHaveBeenCalled();
  });

  it('drops structure on disconnect and reloads after authentication', async () => {
    renderWithProviders(<OrgChartPage />);
    await screen.findByText('Finance assistant');
    act(() => useConnectionStore.setState({ state: 'disconnected' }));
    expect(screen.queryByText('Finance assistant')).not.toBeInTheDocument();
    expect(mockWsClient.call.mock.calls.filter(([method]) => method === 'agents.org_chart')).toHaveLength(1);
    act(() => useConnectionStore.setState({ state: 'authenticated' }));
    await waitFor(() => expect(mockWsClient.call.mock.calls.filter(([method]) => method === 'agents.org_chart')).toHaveLength(2));
  });

  it('discards an old account response after switching accounts', async () => {
    let resolveOld!: (value: unknown) => void;
    mockWsClient.call.mockImplementationOnce(() => new Promise(resolve => { resolveOld = resolve; }));
    mockWsClient.call.mockResolvedValueOnce({ agents: [] });
    renderWithProviders(<OrgChartPage />);
    await waitFor(() => expect(mockWsClient.call.mock.calls.filter(([method]) => method === 'agents.org_chart')).toHaveLength(1));
    act(() => useAuthStore.setState(s => ({ user: { ...s.user!, id: 'employee-two' } })));
    await waitFor(() => expect(mockWsClient.call.mock.calls.filter(([method]) => method === 'agents.org_chart')).toHaveLength(2));
    await act(async () => resolveOld({ agents: structure }));
    expect(screen.queryByText('Finance assistant')).not.toBeInTheDocument();
  });
});

it('clears a selected manager panel and fences retained pause/resume callbacks after revocation', async () => {
  useAuthStore.setState({ user: { id: 'manager', role: 'manager', status: 'active' } } as never);
  mockCallableGrants(['finance']);
  useAgentsStore.setState({ agents: structure, loaded: true, loading: false } as never);
  renderWithProviders(<OrgChartPage />);
  await screen.findByText('Finance assistant');
  act(() => chart.mock.calls.at(-1)![0].onNodeClick('finance'));
  const content = panel.setPanel.mock.calls.at(-1)![0].content as ReactElement<{
    onPause: () => Promise<void>; onResume: () => Promise<void>;
  }>;
  panel.clearPanel.mockClear();
  await act(async () => {
    mockCallableGrants([]);
    await content.props.onPause();
    await content.props.onResume();
  });
  expect(panel.clearPanel).toHaveBeenCalled();
  expect(panel.setSheetOpen).toHaveBeenLastCalledWith(false);
  expect(mockWsClient.call.mock.calls.some(([m]) => m === 'agents.pause' || m === 'agents.resume')).toBe(false);
});

describe('org chart editing (RM-027)', () => {
  const roster = [
    { name: 'main', display_name: 'Main bot', role: 'main', status: 'active', icon: '', reports_to: '', department: '' },
    { name: 'finance', display_name: 'Finance assistant', role: 'specialist', status: 'active', icon: '', reports_to: '', department: 'Finance' },
  ];
  const waitingRow = (over: Record<string, unknown> = {}) => ({
    approval_id: 'a1', agent_id: 'finance', before: { reports_to: '', department: 'Finance' },
    after: { reports_to: 'main', department: 'Finance' }, base: 'h', stale: false, proposer: 'manager',
    proposer_email: 'm@test.invalid', reason: '', created_at: new Date().toISOString(), expires_at: null, ...over,
  });
  let pending: unknown[] = [];
  let currentRoster: typeof roster = roster;

  beforeEach(() => {
    pending = [];
    currentRoster = roster;
    useAuthStore.setState({ user: { id: 'manager', email: 'm@test.invalid', role: 'manager', status: 'active' } } as never);
    mockCallableGrants(['main', 'finance']);
    useAgentsStore.setState({ agents: roster, loaded: true, loading: false } as never);
    mockWsClient.call.mockImplementation(async (method: string) => {
      if (method === 'org.pending_changes') return { count: pending.length, pending };
      if (method === 'org.placement') return { agent_id: 'finance', placement: { reports_to: '', department: 'Finance' }, source: 'authority', mirror_drift: false, base: 'b1' };
      if (method === 'departments.list') return { departments: [] };
      return { agents: currentRoster };
    });
  });

  it('badges a node with a waiting change and lists it in the 待核准 table as 待核准', async () => {
    pending = [waitingRow()];
    renderWithProviders(<OrgChartPage />);
    const table = await screen.findByRole('table');
    expect(within(table).getByText('Finance assistant：manager Top level (no manager) → Main bot')).toBeInTheDocument();
    expect(within(table).getByText('Pending approval')).toBeInTheDocument();
    expect(within(table).getByText('m@test.invalid (you)')).toBeInTheDocument();
    await waitFor(() => expect(chart.mock.calls.at(-1)![0].pendingNames).toEqual(new Set(['finance'])));
  });

  it('shows a request made against a moved position as needing a new request', async () => {
    pending = [waitingRow({ stale: true })];
    renderWithProviders(<OrgChartPage />);
    const table = await screen.findByRole('table');
    expect(within(table).getByText('Resubmit needed')).toHaveAttribute('title', 'The organization has changed since this request. Please submit a new one.');
  });

  it('moves a request that left the waiting list to 已同意 once the employee sits where it asked', async () => {
    pending = [waitingRow()];
    renderWithProviders(<OrgChartPage />);
    await screen.findByRole('table');
    act(() => chart.mock.calls.at(-1)![0].onNodeClick('finance'));
    const content = panel.setPanel.mock.calls.at(-1)![0].content as ReactElement<{ placement: ReactElement<{ onChanged: () => void }> }>;
    pending = [];
    currentRoster = [roster[0], { ...roster[1], reports_to: 'main' }];
    await act(async () => content.props.placement.props.onChanged());
    expect(await within(await screen.findByRole('table')).findByText('Approved')).toBeInTheDocument();
  });

  it('hides other people’s waiting requests from a manager', async () => {
    pending = [waitingRow({ proposer: 'someone-else', proposer_email: 'x@test.invalid' })];
    renderWithProviders(<OrgChartPage />);
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('org.pending_changes', {}));
    expect(screen.queryByRole('table')).not.toBeInTheDocument();
  });

  it('opens 調整位置 for the selected node with the manager pre-set when a node is dropped on another', async () => {
    renderWithProviders(<OrgChartPage />);
    await screen.findByText('Finance assistant');
    act(() => chart.mock.calls.at(-1)![0].onReparent('finance', 'main'));
    expect(panel.setSheetOpen).toHaveBeenLastCalledWith(true);
    const content = panel.setPanel.mock.calls.at(-1)![0].content as ReactElement<{
      placement: ReactElement<{ draft: unknown; isAdmin: boolean }>;
    }>;
    expect(content.props.placement.props.draft).toMatchObject({ agent: 'finance', manager: 'main' });
    expect(content.props.placement.props.isAdmin).toBe(false);

    renderWithProviders(content);
    expect(await screen.findByLabelText('Manager')).toHaveValue('main');
    expect(screen.getByRole('button', { name: 'Submit request' })).toBeEnabled();
    // Dropping only proposes: nothing is sent until the form is submitted.
    expect(mockWsClient.call.mock.calls.some(([m]) => m === 'org.propose_change' || m === 'org.apply_change')).toBe(false);
  });

  it('lets an admin drag too and applies through the confirmation', async () => {
    useAuthStore.setState({ user: { id: 'admin', email: 'a@test.invalid', role: 'admin', status: 'active' } } as never);
    mockCallableGrants(['main', 'finance']);
    renderWithProviders(<OrgChartPage />);
    await screen.findByText('Finance assistant');
    expect(typeof chart.mock.calls.at(-1)![0].onReparent).toBe('function');
    act(() => chart.mock.calls.at(-1)![0].onReparent('finance', 'main'));
    const content = panel.setPanel.mock.calls.at(-1)![0].content as ReactElement<{ placement: ReactElement<{ isAdmin: boolean }> }>;
    expect(content.props.placement.props.isAdmin).toBe(true);
    renderWithProviders(content);
    expect(await screen.findByRole('button', { name: 'Apply change' })).toBeEnabled();
  });
});
