import { mockWsClient } from '@/test/mocks';
import { mockCallableGrants } from '@/test/callable-grants';
import { useAgentsStore } from '@/stores/agents-store';
import type { ReactElement } from 'react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { act, screen, waitFor } from '@testing-library/react';
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
