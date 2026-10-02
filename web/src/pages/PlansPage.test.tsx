import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { screen } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { PlansPage } from './PlansPage';

beforeEach(() => {
  vi.clearAllMocks();
  mockWsClient.call.mockResolvedValue({ plans: [], agents: [], steps: [] });
  try { localStorage.clear(); } catch { /* jsdom */ }
});

describe('PlansPage', () => {
  it('renders the collection header with the plan icon and title', () => {
    renderWithProviders(<PlansPage />);
    expect(screen.getByRole('heading', { name: 'Shared Plans' })).toBeInTheDocument();
  });

  it('offers a create-plan action', () => {
    renderWithProviders(<PlansPage />);
    expect(screen.getAllByRole('button', { name: 'New plan' }).length).toBeGreaterThan(0);
  });
});

// ── L2 round 3: fanned-out plans list — partial-load notice ──
import { waitFor } from '@testing-library/react';
import { useAuthStore } from '@/stores/auth-store';
import { usePlansStore } from '@/stores/plans-store';

const PARTIAL_NOTICE = "Some AI employees' data couldn't be loaded this time — showing what did load.";

describe('PlansPage for a non-admin (L2)', () => {
  beforeEach(() => {
    usePlansStore.setState({ plans: [], steps: {}, loading: false, error: null, partialError: null });
    useAuthStore.setState({
      user: { id: 'u1', email: 'u@x', display_name: 'U', role: 'employee', status: 'active' },
      bindings: ['a', 'b'].map((agent_name) => ({ user_id: 'u1', agent_name, access_level: 'viewer', bound_at: '2026-09-01T00:00:00Z' })),
      isAuthenticated: true,
    });
  });
  afterEach(() => {
    useAuthStore.setState({ user: null, bindings: [], isAuthenticated: false });
  });

  const plan = (agent: string) => ({
    id: `plan-${agent}`, title: `Plan of ${agent}`, description: '', agent_id: agent, status: 'active',
    created_by: 'u1', created_at: '2026-09-01T00:00:00Z', updated_at: '2026-09-01T00:00:00Z',
  });

  function gateway(failAgent?: string) {
    return (method: string, params?: Record<string, unknown>) => {
      if (method === 'plans.list') {
        const agent = params?.agent_id as string;
        if (agent === failAgent) return Promise.reject(new Error('backend hiccup'));
        return Promise.resolve({ plans: [plan(agent)] });
      }
      return Promise.resolve({ agents: [], steps: [] });
    };
  }

  it('partial result: renders the plans that loaded plus one status notice', async () => {
    mockWsClient.call.mockImplementation(gateway('b'));
    renderWithProviders(<PlansPage />);
    expect((await screen.findAllByText('Plan of a')).length).toBeGreaterThan(0);
    const notice = await screen.findByText(PARTIAL_NOTICE);
    expect(notice.closest('[role="status"]')).not.toBeNull();
  });

  it('clean result: no notice; a later clean load clears an earlier notice', async () => {
    usePlansStore.setState({ partialError: new Error('earlier') });
    mockWsClient.call.mockImplementation(gateway());
    renderWithProviders(<PlansPage />);
    expect((await screen.findAllByText('Plan of b')).length).toBeGreaterThan(0);
    await waitFor(() => expect(screen.queryByText(PARTIAL_NOTICE)).toBeNull());
  });
});
