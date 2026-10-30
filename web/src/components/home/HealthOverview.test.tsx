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


it('shows plain failed-work titles and keeps an expiring approval in the top slice', async () => {
  useAuthStore.setState({ user: null, bindings: [], isAuthenticated: false });
  mockWsClient.call.mockReset();
  mockWsClient.call.mockImplementation((method: string) => {
    if (method === 'approvals.list') return Promise.resolve({ approvals: [
      ...Array.from({ length: 7 }, (_, i) => ({ id: `a-${i}`, agent_id: 'staff', kind: 'tool_call', summary: `Later request ${i}`, payload: {}, created_at: new Date().toISOString(), expires_at: Date.now() / 1000 + 3600 })),
      { id: 'urgent', agent_id: 'staff', kind: 'tool_call', summary: 'Expiring request', payload: {}, created_at: new Date().toISOString(), expires_at: Date.now() / 1000 + 30 },
    ] });
    if (method === 'audit.unified_log') return Promise.resolve({ events: [{ agent_id: 'staff', timestamp: new Date().toISOString(), event_type: 'channel_reply_fallback', summary: '{"origin":"Claude Code","preflight":null}', details: {} }] });
    return Promise.resolve({});
  });
  renderWithProviders(<HealthOverview agents={[]} enabled />);
  const urgent = await screen.findByText('Expiring request');
  expect(urgent.closest('li') ?? urgent.closest('a')).toBeInTheDocument();
  expect(screen.queryByText(/preflight|Claude Code/)).toBeNull();
});

it('never renders the raw spawn failure payload on Home', async () => {
  useAuthStore.setState({ user: null, bindings: [], isAuthenticated: false });
  mockWsClient.call.mockReset();
  mockWsClient.call.mockImplementation((method: string) => method === 'audit.unified_log'
    ? Promise.resolve({ events: [{ agent_id: 'staff', timestamp: new Date().toISOString(), event_type: 'channel_reply_fallback', summary: '{"origin":"Claude Code","preflight":null}', details: {} }] })
    : Promise.resolve({}));
  renderWithProviders(<HealthOverview agents={[]} enabled />);
  expect(await screen.findByText("staff's work did not finish")).toBeInTheDocument();
  expect(screen.queryByText(/preflight|Claude Code/)).toBeNull();
});


it('links webchat failures to the same run as Inbox', async () => {
  useAuthStore.setState({ user: null, bindings: [], isAuthenticated: false });
  mockWsClient.call.mockReset();
  mockWsClient.call.mockImplementation((method: string) => method === 'audit.unified_log'
    ? Promise.resolve({ events: [{ agent_id: 'staff', timestamp: new Date().toISOString(), event_type: 'channel_reply_fallback', summary: '{}', details: { channel_failure: { channel: 'webchat', run_id: 'run:42' } } }] })
    : Promise.resolve({}));
  renderWithProviders(<HealthOverview agents={[]} enabled />);
  expect(await screen.findByRole('link', { name: /staff's work did not finish/ })).toHaveAttribute('href', '/runs?run=run%3A42');
});
