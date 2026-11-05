import { beforeEach, describe, expect, it, vi } from 'vitest';
import { act, screen, waitFor } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { useAuthStore } from '@/stores/auth-store';
import { useConnectionStore } from '@/stores/connection-store';
import type { OrgChartAgent } from '@/lib/api';
import { OrgChartPage } from './OrgChartPage';

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
    expect(mockWsClient.call.mock.calls.every(([method]) => method === 'agents.org_chart')).toBe(true);
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
    expect(mockWsClient.call).toHaveBeenCalledTimes(1);
    act(() => useConnectionStore.setState({ state: 'authenticated' }));
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledTimes(2));
  });

  it('discards an old account response after switching accounts', async () => {
    let resolveOld!: (value: unknown) => void;
    mockWsClient.call.mockImplementationOnce(() => new Promise(resolve => { resolveOld = resolve; }));
    mockWsClient.call.mockResolvedValueOnce({ agents: [] });
    renderWithProviders(<OrgChartPage />);
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledTimes(1));
    act(() => useAuthStore.setState(s => ({ user: { ...s.user!, id: 'employee-two' } })));
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledTimes(2));
    await act(async () => resolveOld({ agents: structure }));
    expect(screen.queryByText('Finance assistant')).not.toBeInTheDocument();
  });
});
