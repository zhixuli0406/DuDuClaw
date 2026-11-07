import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, screen } from '@testing-library/react';
import '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { useAuthStore } from '@/stores/auth-store';
import { useSystemStore } from '@/stores/system-store';
import type { IsolationReadiness, SystemStatus } from '@/lib/api';
import { IsolationReadinessBanner } from './IsolationReadinessBanner';

const refresh = vi.fn(async () => {});
const originalRefresh = useSystemStore.getState().fetchStatus;
const readiness = (): IsolationReadiness => ({ required: true, ready: false, components: {
  launch_service: 'ready', sandbox: 'ready', network: 'ready', storage: 'ready',
  parse_worker: 'pending_integration', update_service: 'pending_integration',
  mcp_boundary: 'pending_integration', runtime_adapters: 'pending_integration',
} });
function setReadiness(value: IsolationReadiness | null | undefined) {
  const status: SystemStatus = { version: '1.80.0', uptime_seconds: 1, agents_count: 0,
    channels_connected: 0, gateway_address: 'localhost', isolation_readiness: value };
  useSystemStore.setState({ status, loading: false, fetchStatus: refresh });
}

beforeEach(() => {
  refresh.mockClear();
  useAuthStore.setState({ user: { id: 'admin', email: 'admin@test.invalid',
    display_name: 'Admin', role: 'admin', status: 'active' } });
  setReadiness(readiness());
});
afterEach(() => {
  useAuthStore.setState({ user: null });
  useSystemStore.setState({ status: null, loading: false, fetchStatus: originalRefresh });
});

describe('IsolationReadinessBanner', () => {
  it.each([undefined, null])('hides on installations without composite evidence: %s', value => {
    setReadiness(value);
    const { container } = renderWithProviders(<IsolationReadinessBanner />);
    expect(container).toBeEmptyDOMElement();
  });

  it.each([{ ...readiness(), required: false }, { ...readiness(), ready: true }])(
    'hides when the gateway does not require a warning', value => {
      setReadiness(value);
      const { container } = renderWithProviders(<IsolationReadinessBanner />);
      expect(container).toBeEmptyDOMElement();
    });

  it('shows unfinished isolation setup even when all host checks passed', () => {
    renderWithProviders(<IsolationReadinessBanner />);
    expect(screen.getByRole('alert')).toHaveTextContent('Protected TEAM work is paused');
    expect(screen.getByRole('alert')).toHaveTextContent('Employee work, including API-only model calls, cannot start');
    expect(screen.getByText(/Document processing/)).toHaveTextContent('Setup is not complete');
    expect(screen.queryByText(/Launch service/)).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Check again' }));
    expect(refresh).toHaveBeenCalledTimes(1);
    expect(screen.getByRole('alert')).toBeInTheDocument();
  });

  it('distinguishes missing current evidence from unfinished setup', () => {
    const value = readiness();
    value.components.network = 'unavailable';
    setReadiness(value);
    renderWithProviders(<IsolationReadinessBanner />);
    expect(screen.getByText(/Outbound network protection/)).toHaveTextContent('Current verification unavailable');
    expect(screen.getByText(/Tool authorization/)).toHaveTextContent('Setup is not complete');
  });

  it('explains the employee work block to non-admins without component details', () => {
    useAuthStore.setState({ user: { ...useAuthStore.getState().user!, role: 'employee' } });
    renderWithProviders(<IsolationReadinessBanner />);
    expect(screen.getByRole('alert')).toHaveTextContent('Protected TEAM work is paused');
    expect(screen.getByRole('alert')).toHaveTextContent('Employee work, including API-only model calls, cannot start');
    expect(screen.queryByText('Service details')).not.toBeInTheDocument();
  });
});
