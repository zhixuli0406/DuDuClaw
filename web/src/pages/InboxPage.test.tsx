import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor, fireEvent, within } from '@testing-library/react';
import { IntlProvider } from 'react-intl';
import { MemoryRouter, Routes, Route, useLocation } from 'react-router';
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
    expect(screen.getByRole('tab', { name: /To do/ })).toBeInTheDocument();
    expect(screen.getByRole('tab', { name: /^All/ })).toBeInTheDocument();
  });

  it('shows the empty detail placeholder when nothing is selected', () => {
    renderWithProviders(<InboxPage />);
    expect(screen.getByText('Select an item to see its details')).toBeInTheDocument();
  });
});

/** Renders InboxPage at an explicit path (`renderWithProviders`'s router has
 *  no way to seed a starting location/query string). */
function RunDestination() {
  const location = useLocation();
  return <p>{location.pathname}{location.search}</p>;
}

function renderAt(path: string) {
  return render(
    <IntlProvider messages={en} locale="en" defaultLocale="en">
      <MemoryRouter initialEntries={[path]}>
        <Routes>
          <Route path="/inbox" element={<InboxPage />} />
          <Route path="/runs" element={<RunDestination />} />
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

  it.each(['a'.repeat(64), null, undefined])('a failed approve preserves the item with digest %s', async (digest) => {
    mockWsClient.call.mockImplementation((method: string) => {
      if (method === 'approvals.list') {
        return Promise.resolve({
          count: 1,
          approvals: [{ ...APPROVAL_FIXTURE, id: 'apr-9', summary: 'Hold this knowledge', digest }],
        });
      }
      if (method === 'approvals.decide') {
        return Promise.reject(new Error('decided, but quarantine side-effect failed: db locked'));
      }
      return Promise.resolve({});
    });

    renderAt('/inbox?item=apr-9');
    const approve = await screen.findByRole('button', { name: 'Approve' });
    approve.click();

    const alert = await screen.findByRole('alert');
    expect(mockWsClient.call).toHaveBeenCalledWith('approvals.decide', {
      id: 'apr-9', approve: true, ...(digest ? { expected_digest: digest } : {}),
    });
    expect(alert.textContent).toContain('db locked');
    // Still selected, still decidable: nothing was removed optimistically.
    expect(screen.getAllByText('Hold this knowledge').length).toBeGreaterThan(0);
    expect(screen.getByRole('button', { name: 'Approve' })).toBeInTheDocument();
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

describe('Inbox triage safety', () => {
  beforeEach(() => {
    localStorage.clear();
    signIn('admin');
    useConnectionStore.setState({ state: 'authenticated' as never, error: null });
  });

  it('reports the employee question cap and a failed question source', async () => {
    mockWsClient.call.mockImplementation((method: string) => {
      if (method === 'agents.list') return Promise.resolve({ agents: Array.from({ length: 13 }, (_, i) => ({ name: `staff-${i}` })) });
      if (method === 'decisions.list') return Promise.reject(new Error('offline'));
      return Promise.resolve({});
    });
    renderAt('/inbox');
    expect(await screen.findByText(/first 12 of 13 employees/)).toBeInTheDocument();
    expect(await screen.findByText("Some items couldn't be loaded")).toBeInTheDocument();
    expect(mockWsClient.call.mock.calls.filter(([method]) => method === 'decisions.list')).toHaveLength(12);
  });

  it('shows an outcome and removes decision buttons when a resolved approval is reopened', async () => {
    mockWsClient.call.mockImplementation((method: string) => method === 'approvals.list'
      ? Promise.resolve({ approvals: [{ ...APPROVAL_FIXTURE, id: 'receipt', summary: 'A safe request', expires_at: Date.now() / 1000 + 600 }] }) : Promise.resolve({}));
    renderAt('/inbox?item=receipt');
    fireEvent.click(await screen.findByRole('button', { name: 'Approve' }));
    await waitFor(() => expect(screen.queryByRole('button', { name: 'Approve' })).not.toBeInTheDocument());
    fireEvent.click(screen.getByRole('tab', { name: /^All/ }));
    fireEvent.click(screen.getByText('A safe request'));
    expect(screen.getByRole('status')).toHaveTextContent('Approved');
    const row = screen.getByRole('option');
    expect(within(row).getByText('Approved')).toBeInTheDocument();
    expect(within(row).queryByTitle(en['inbox.approval.nearExpiryTooltip'])).toBeNull();
    expect(screen.queryByRole('button', { name: 'Approve' })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Reject' })).not.toBeInTheDocument();
  });

  it('shows a readable failed-work detail with its run link, without dumping data', async () => {
    mockWsClient.call.mockImplementation((method: string) => method === 'audit.unified_log'
      ? Promise.resolve({ events: [{ timestamp: '2026-10-31T00:00:00Z', agent_id: 'staff', severity: 'critical', event_type: 'channel_send_failure_alert', summary: 'INTERNAL RAW SUMMARY', details: { run_id: 'run-42', token: 'private-debug' } }] }) : Promise.resolve({}));
    renderAt('/inbox?item=failed_run:staff:2026-10-31T00:00:00Z');
    expect(await screen.findByText(/This work did not finish successfully/)).toBeInTheDocument();
    expect(screen.queryByText(/private-debug|INTERNAL RAW SUMMARY/)).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'View work record' })).toBeInTheDocument();
  });
});


describe('Inbox browser regressions', () => {
  beforeEach(() => {
    localStorage.clear();
    signIn('admin');
    useConnectionStore.setState({ state: 'authenticated' as never, error: null });
  });

  function loadApprovals() {
    mockWsClient.call.mockImplementation((method: string) => method === 'approvals.list'
      ? Promise.resolve({ approvals: ['first', 'second'].map((id) => ({ ...APPROVAL_FIXTURE, id, summary: `Request ${id}`, expires_at: Date.now() / 1000 + 600 })) })
      : Promise.resolve({}));
  }

  it('constrains both scroll regions and replaces the detail scroll state on selection', async () => {
    loadApprovals();
    renderAt('/inbox?item=first');
    await screen.findByRole('button', { name: 'Approve' });
    expect(screen.getByTestId('inbox-page')).toHaveClass('min-h-0', 'flex-1', 'overflow-hidden');
    expect(screen.getByRole('listbox').parentElement).toHaveClass('min-h-0', 'overflow-y-auto');
    for (const tab of screen.getAllByRole('tab')) expect(tab).toHaveClass('whitespace-nowrap', 'shrink-0');
    const previous = screen.getByTestId('inbox-detail-content');
    expect(previous).toHaveClass('flex', 'flex-col', 'min-h-0', 'overflow-hidden');
    const actions = screen.getByTestId('approval-actions');
    expect(actions).toHaveClass('shrink-0');
    expect(actions.previousElementSibling).toHaveClass('flex-1', 'min-h-0', 'overflow-y-auto');
    actions.previousElementSibling!.scrollTop = 400;
    fireEvent.click(screen.getByText('Request second'));
    expect(screen.getByTestId('inbox-detail-content')).not.toBe(previous);
    expect(screen.getByTestId('approval-actions').previousElementSibling!.scrollTop).toBe(0);
  });

  it('uses the same constrained detail and footer on mobile', async () => {
    const original = window.matchMedia;
    Object.defineProperty(window, 'matchMedia', { configurable: true, writable: true, value: vi.fn(() => ({ matches: true, addEventListener: vi.fn(), removeEventListener: vi.fn() })) });
    try {
      loadApprovals();
      renderAt('/inbox?item=first');
      await screen.findByRole('button', { name: 'Approve' });
      expect(screen.queryByTestId('inbox-split')).toBeNull();
      expect(screen.getByTestId('inbox-page').firstElementChild).toHaveClass('h-full', 'min-h-0', 'overflow-hidden');
      expect(screen.getByTestId('approval-actions')).toHaveClass('shrink-0');
      expect(screen.getByTestId('approval-actions').previousElementSibling).toHaveClass('min-h-0', 'flex-1', 'overflow-y-auto');
    } finally {
      window.matchMedia = original;
    }
  });

  it.each(['mouse', 'keyboard'])('undo works from BODY after archiving by %s, while typing keeps native undo', async (via) => {
    mockWsClient.call.mockImplementation((method: string) => method === 'audit.unified_log'
      ? Promise.resolve({ events: [{ timestamp: '2026-10-31T00:00:00Z', agent_id: 'staff', severity: 'critical', event_type: 'channel_send_failure_alert', summary: '{}' }] }) : Promise.resolve({}));
    renderAt('/inbox');
    fireEvent.click(await screen.findByRole('tab', { name: /^All/ }));
    const row = await screen.findByRole('option');
    fireEvent.click(row);
    if (via === 'mouse') fireEvent.click(within(row).getByRole('button', { name: 'Archive' }));
    else fireEvent.keyDown(row, { key: 'a' });
    await waitFor(() => expect(screen.queryByRole('option')).toBeNull());
    const input = document.createElement('input');
    document.body.append(input);
    input.focus();
    expect(fireEvent.keyDown(input, { key: 'z', ctrlKey: true })).toBe(true);
    expect(screen.queryByRole('option')).toBeNull();
    input.remove();
    document.body.focus();
    fireEvent.keyDown(document.body, { key: 'z', [via === 'mouse' ? 'metaKey' : 'ctrlKey']: true });
    expect(await screen.findByRole('option')).toBeInTheDocument();
  });

  it.each([
    ['Friendly employee', [{ name: 'staff-id', display_name: 'Friendly employee' }], '/runs?agent=staff-id'],
    ['staff-id', [{ name: 'staff-id', display_name: 'Friendly employee' }], '/runs?agent=staff-id'],
    ['Unknown employee', [], '/runs'],
    ['Duplicate', [{ name: 'a', display_name: 'Duplicate' }, { name: 'b', display_name: 'Duplicate' }], '/runs'],
  ])('opens a safe run link for %s', async (agent, agents, destination) => {
    mockWsClient.call.mockImplementation((method: string) => {
      if (method === 'agents.list') return Promise.resolve({ agents });
      if (method === 'audit.unified_log') return Promise.resolve({ events: [{ timestamp: '2026-10-31T00:00:00Z', agent_id: agent, severity: 'critical', event_type: 'channel_send_failure_alert', summary: '{}' }] });
      return Promise.resolve({});
    });
    renderAt(`/inbox?item=${encodeURIComponent(`failed_run:${agent}:2026-10-31T00:00:00Z`)}`);
    const link = await screen.findByRole('button', { name: 'View work record' });
    const label = agents.find((a) => a.name === agent)?.display_name || agent;
    expect(screen.getAllByText(`${label}'s work did not finish`).length).toBeGreaterThan(0);
    fireEvent.click(link);
    expect(await screen.findByText(destination)).toBeInTheDocument();
  });
});
