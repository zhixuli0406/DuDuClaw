import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import { IntlProvider } from 'react-intl';
import { MemoryRouter, Routes, Route } from 'react-router';
import en from '@/i18n/en.json';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { InboxPage } from './InboxPage';
import { useConnectionStore } from '@/stores/connection-store';

beforeEach(() => {
  vi.clearAllMocks();
});

describe('InboxPage (Multica list+detail split)', () => {
  it('renders the list column header + scope tabs', () => {
    renderWithProviders(<InboxPage />);
    // Page title in the left list header.
    expect(screen.getByRole('heading', { name: 'Inbox' })).toBeInTheDocument();
    // The five scope tabs.
    expect(screen.getByRole('button', { name: /Mine/ })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /^All/ })).toBeInTheDocument();
  });

  it('shows the empty detail placeholder when nothing is selected', () => {
    renderWithProviders(<InboxPage />);
    expect(screen.getByText('Select an item to see its details')).toBeInTheDocument();
  });
});

/** Renders InboxPage at an explicit path (`renderWithProviders`'s router has
 *  no way to seed a starting location/query string). */
function renderAt(path: string) {
  return render(
    <IntlProvider messages={en} locale="en" defaultLocale="en">
      <MemoryRouter initialEntries={[path]}>
        <Routes>
          <Route path="/inbox" element={<InboxPage />} />
        </Routes>
      </MemoryRouter>
    </IntlProvider>,
  );
}

const APPROVAL_FIXTURE = {
  agent_id: 'agent-a',
  kind: 'tool_call',
  payload: {},
  created_at: '2026-07-11T00:00:00Z',
  ttl_seconds: 3600,
};

describe('InboxPage deep link (W2-5 H5, ?item=<id>)', () => {
  beforeEach(() => {
    useConnectionStore.setState({ state: 'authenticated' as never, error: null });
  });

  it('opens the linked approval from a bare gateway-style id (no type prefix)', async () => {
    mockWsClient.call.mockImplementation((method: string) => {
      if (method === 'approvals.list') {
        return Promise.resolve({
          count: 1,
          approvals: [{ ...APPROVAL_FIXTURE, id: 'apr-1', summary: 'Run a shell command' }],
        });
      }
      return Promise.resolve({});
    });

    // `deep_link.rs` (gateway) only ever has the bare broker id — never the
    // frontend's `approval:<id>` prefix. The suffix-match fallback must
    // still resolve it.
    renderAt('/inbox?item=apr-1');

    await waitFor(() => {
      expect(screen.queryByText('Select an item to see its details')).toBeNull();
    });
    expect(screen.getAllByText('Run a shell command').length).toBeGreaterThan(0);
  });

  it('opens the linked item from an already-prefixed id (dashboard-originated link)', async () => {
    mockWsClient.call.mockImplementation((method: string) => {
      if (method === 'approvals.list') {
        return Promise.resolve({
          count: 1,
          approvals: [{ ...APPROVAL_FIXTURE, id: 'apr-2', summary: 'Send a refund' }],
        });
      }
      return Promise.resolve({});
    });

    renderAt(`/inbox?item=${encodeURIComponent('approval:apr-2')}`);

    await waitFor(() => {
      expect(screen.queryByText('Select an item to see its details')).toBeNull();
    });
    expect(screen.getAllByText('Send a refund').length).toBeGreaterThan(0);
  });

  it('leaves the empty-state placeholder when the linked id matches nothing loaded', async () => {
    mockWsClient.call.mockResolvedValue({});
    renderAt('/inbox?item=does-not-exist');
    await waitFor(() => {
      expect(screen.getByText('Select an item to see its details')).toBeInTheDocument();
    });
  });
});

// ── L2: non-admin viewers (agent_id contract + role-expected denials) ──
import { useAuthStore, type UserRole } from '@/stores/auth-store';

/** Simulates the real gateway's per-role answers (verified live, L2). */
function gatewayFor(role: UserRole, overrides: Record<string, () => Promise<unknown>> = {}) {
  const denied: Record<UserRole, string[]> = {
    admin: [],
    manager: ['audit.unified_log'],
    employee: ['approvals.list', 'budget.incidents', 'audit.unified_log', 'install_requests.list'],
  };
  return (method: string, params?: Record<string, unknown>) => {
    if (overrides[method]) return overrides[method]();
    if (denied[role].includes(method)) return Promise.reject(new Error('permission denied'));
    if (method === 'tasks.list') {
      if (role !== 'admin' && !params?.agent_id) {
        return Promise.reject(new Error('agent_id parameter is required'));
      }
      const status = params?.status as string;
      return Promise.resolve({
        tasks: [
          {
            id: `${status}-1`,
            title: `Stuck ${status} task`,
            description: '',
            status,
            priority: 'medium',
            assigned_to: 'discovery-live',
            created_by: 'x',
            created_at: '2026-09-30T00:00:00Z',
            updated_at: '2026-09-30T00:00:00Z',
          },
        ],
      });
    }
    if (method === 'agents.list') {
      return Promise.resolve({ agents: [{ name: 'discovery-live', display_name: 'Discovery Live' }] });
    }
    if (method === 'decisions.list') return Promise.resolve({ decisions: [] });
    return Promise.resolve({});
  };
}

function signIn(role: UserRole) {
  useAuthStore.setState({
    user: { id: 'u1', email: 'u@x', display_name: 'U', role, status: 'active' },
    bindings: [{ user_id: 'u1', agent_name: 'discovery-live', access_level: 'operator', bound_at: '2026-09-01T00:00:00Z' }],
    isAuthenticated: true,
  });
}

describe('InboxPage for non-admin viewers (L2)', () => {
  beforeEach(() => {
    useConnectionStore.setState({ state: 'authenticated' as never, error: null });
  });

  it('manager: lists blocked + needs_human tasks of bound agents, no error banner', async () => {
    signIn('manager');
    mockWsClient.call.mockImplementation(gatewayFor('manager'));
    renderAt('/inbox');
    await waitFor(() => {
      expect(screen.getAllByText('Stuck needs_human task').length).toBeGreaterThan(0);
    });
    expect(screen.getAllByText('Stuck blocked task').length).toBeGreaterThan(0);
    expect(screen.queryByText("Some items couldn't be loaded")).toBeNull();
  });

  it('employee: role-expected denials raise no banner', async () => {
    signIn('employee');
    mockWsClient.call.mockImplementation(gatewayFor('employee'));
    renderAt('/inbox');
    await waitFor(() => {
      expect(screen.getAllByText('Stuck needs_human task').length).toBeGreaterThan(0);
    });
    expect(screen.queryByText("Some items couldn't be loaded")).toBeNull();
  });

  it('employee: an unexpected failure still raises the banner', async () => {
    signIn('employee');
    mockWsClient.call.mockImplementation(
      gatewayFor('employee', { 'agents.list': () => Promise.reject(new Error('network down')) }),
    );
    renderAt('/inbox');
    await waitFor(() => {
      expect(screen.getByText("Some items couldn't be loaded")).toBeInTheDocument();
    });
  });

  it('manager: a denial NOT expected for the role still raises the banner', async () => {
    signIn('manager');
    mockWsClient.call.mockImplementation(
      gatewayFor('manager', { 'approvals.list': () => Promise.reject(new Error('permission denied')) }),
    );
    renderAt('/inbox');
    await waitFor(() => {
      expect(screen.getByText("Some items couldn't be loaded")).toBeInTheDocument();
    });
  });
});

describe('InboxPage fan-out partial failure (L2 round 2)', () => {
  beforeEach(() => {
    useConnectionStore.setState({ state: 'authenticated' as never, error: null });
  });

  it('manager with two agents: one agent failing keeps the other agent\'s tasks AND shows the banner', async () => {
    useAuthStore.setState({
      user: { id: 'u1', email: 'u@x', display_name: 'U', role: 'manager', status: 'active' },
      bindings: [
        { user_id: 'u1', agent_name: 'ok-agent', access_level: 'viewer', bound_at: '2026-09-01T00:00:00Z' },
        { user_id: 'u1', agent_name: 'bad-agent', access_level: 'viewer', bound_at: '2026-09-01T00:00:00Z' },
      ],
      isAuthenticated: true,
    });
    const base = gatewayFor('manager');
    mockWsClient.call.mockImplementation((method: string, params?: Record<string, unknown>) => {
      if (method === 'tasks.list' && params?.agent_id === 'bad-agent') {
        return Promise.reject(new Error('backend hiccup'));
      }
      return base(method, params);
    });
    renderAt('/inbox');
    await waitFor(() => {
      expect(screen.getAllByText('Stuck needs_human task').length).toBeGreaterThan(0);
    });
    expect(screen.getAllByText('Stuck blocked task').length).toBeGreaterThan(0);
    expect(screen.getByText("Some items couldn't be loaded")).toBeInTheDocument();
  });
});
