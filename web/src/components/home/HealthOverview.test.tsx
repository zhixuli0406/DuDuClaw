import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import { screen } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { HealthOverview } from './HealthOverview';
import { useAuthStore } from '@/stores/auth-store';

// L2 round 3: the home "needs you" area says so when the needs_human query
// came back partial for a non-admin (one bound agent's request failed).
const PARTIAL_LINE = "Some AI employees' data couldn't be loaded this time — showing what did load.";

function gateway(failAgent?: string) {
  return (method: string, params?: Record<string, unknown>) => {
    if (method === 'tasks.list') {
      const agent = params?.agent_id as string;
      if (agent === failAgent) return Promise.reject(new Error('backend hiccup'));
      return Promise.resolve({
        tasks: [{
          id: `t-${agent}`, title: `Decide for ${agent}`, description: '', status: 'needs_human', priority: 'medium',
          assigned_to: agent, created_by: 'x', created_at: '2026-09-30T00:00:00Z', updated_at: '2026-09-30T00:00:00Z',
        }],
      });
    }
    return Promise.resolve({});
  };
}

describe('HealthOverview partial-load line (L2)', () => {
  beforeEach(() => {
    mockWsClient.call.mockReset();
    useAuthStore.setState({
      user: { id: 'u1', email: 'u@x', display_name: 'U', role: 'employee', status: 'active' },
      bindings: ['a', 'b'].map((agent_name) => ({ user_id: 'u1', agent_name, access_level: 'viewer', bound_at: '2026-09-01T00:00:00Z' })),
      isAuthenticated: true,
    });
  });
  afterEach(() => {
    useAuthStore.setState({ user: null, bindings: [], isAuthenticated: false });
  });

  it('partial result: lists what arrived plus one small status line', async () => {
    mockWsClient.call.mockImplementation(gateway('b'));
    renderWithProviders(<HealthOverview agents={[]} enabled />);
    expect(await screen.findByText('Decide for a')).toBeInTheDocument();
    expect(screen.getByText(PARTIAL_LINE).closest('[role="status"]')).not.toBeNull();
  });

  it('clean result: no line', async () => {
    mockWsClient.call.mockImplementation(gateway());
    renderWithProviders(<HealthOverview agents={[]} enabled />);
    expect(await screen.findByText('Decide for b')).toBeInTheDocument();
    expect(screen.queryByText(PARTIAL_LINE)).toBeNull();
  });
});

describe('HealthOverview total failure of the needs_human query (L2 round 7)', () => {
  beforeEach(() => {
    mockWsClient.call.mockReset();
    useAuthStore.setState({ user: null, bindings: [], isAuthenticated: false });
  });

  it('shows the could-not-load line and never claims all clear', async () => {
    mockWsClient.call.mockImplementation((method: string) =>
      method === 'tasks.list' ? Promise.reject(new Error('down')) : Promise.resolve({}),
    );
    renderWithProviders(<HealthOverview agents={[]} enabled />);
    expect(await screen.findByText(PARTIAL_LINE)).toBeInTheDocument();
    expect(screen.queryByText(/all clear|Nothing needs you/i)).toBeNull();
  });
});
