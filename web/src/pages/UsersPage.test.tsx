import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { UsersPage } from './UsersPage';
import { EditUserDialog } from './users/UserFormDialogs';
import type { UserDetail } from '@/lib/api';
import { useConnectionStore } from '@/stores/connection-store';
import { useAgentsStore } from '@/stores/agents-store';
import { useAuthStore } from '@/stores/auth-store';

beforeEach(() => {
  vi.clearAllMocks();
  useConnectionStore.setState({ state: 'authenticated' as never, error: null });
  useAuthStore.setState({ user: { id: 'admin', role: 'admin' } as never });
  mockWsClient.call.mockResolvedValue({ users: [], departments: [], agents: [] });
});

describe('UsersPage', () => {
  it('renders the users heading', () => {
    renderWithProviders(<UsersPage />);

    expect(screen.getByRole('heading', { name: 'User Management' })).toBeInTheDocument();
  });

  it('shows empty state when no users', async () => {
    renderWithProviders(<UsersPage />);

    expect(await screen.findByText('User Management', { selector: 'p' })).toBeInTheDocument();
  });

  it('opens the single-page member form when button clicked', async () => {
    const user = userEvent.setup();
    renderWithProviders(<UsersPage />);

    const createButton = screen.getByRole('button', { name: /add members/i });
    await user.click(createButton);

    expect(screen.getByRole('heading', { name: 'Add members' })).toBeInTheDocument();
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  });
});

// PR-E (human org): direct-manager picker, the list's manager line, the
// 清單 / 組織圖 toggle and plain-language reporting-line refusals.
describe('UsersPage — reporting lines', () => {
  const u = (id: string, name: string, role: string, manager: string | null, extra = {}) => ({
    id,
    email: `${id}@test.invalid`,
    display_name: name,
    role,
    status: 'active',
    department: null,
    manager_user_id: manager,
    created_at: '2026-10-01T00:00:00Z',
    updated_at: '2026-10-01T00:00:00Z',
    bindings: [] as Array<{ user_id: string; agent_name: string; access_level: string; bound_at: string }>,
    ...extra,
  });
  const USERS = [
    u('admin', 'Admin', 'admin', null),
    u('mgr', 'Chen', 'manager', null),
    u('lead', 'Lin', 'manager', 'mgr'),
    u('emp1', 'Wang', 'employee', 'lead', {
      bindings: [{ user_id: 'emp1', agent_name: 'sales', access_level: 'owner', bound_at: '' }],
    }),
    u('other', 'Chang', 'employee', null),
    u('gone', 'Gone', 'manager', null, { status: 'suspended' }),
  ];
  const updateCalls: unknown[] = [];

  beforeEach(() => {
    updateCalls.length = 0;
    mockWsClient.call.mockImplementation((method: string, params: unknown) => {
      if (method === 'users.list') return Promise.resolve({ users: USERS });
      if (method === 'users.update') {
        updateCalls.push(params);
        return Promise.reject({ code: 'manager_cycle', message: '這樣設定會讓主管關係繞成一圈' });
      }
      return Promise.resolve({ users: [], departments: [], agents: [], identities: [] });
    });
  });

  it('shows each person’s manager in the list', async () => {
    renderWithProviders(<UsersPage />);
    expect(await screen.findByText('Manager: Lin')).toBeInTheDocument();
    expect(screen.getByText('Manager: Chen')).toBeInTheDocument();
  });

  it('draws the org chart from manager_user_id, with a group for people without a manager', async () => {
    const user = userEvent.setup();
    renderWithProviders(<UsersPage />);
    await screen.findByText('Manager: Lin');
    await user.click(screen.getByRole('radio', { name: 'Org chart' }));
    const chart = screen.getByTestId('org-chart');
    const tree = within(chart).getByRole('tree', { name: 'Chen' });
    // Chen → Lin → Wang, nested.
    const lin = within(tree).getByRole('button', { name: /Lin/ }).closest('li')!;
    expect(within(lin as HTMLElement).getByRole('button', { name: /Wang/ })).toBeInTheDocument();
    expect(within(lin as HTMLElement).getByText('sales')).toBeInTheDocument();
    const unassigned = within(chart).getByText('No manager set').closest('section')!;
    for (const name of ['Admin', 'Chang', 'Gone']) {
      expect(within(unassigned as HTMLElement).getByRole('button', { name: new RegExp(name) })).toBeInTheDocument();
    }
  });

  it('offers only active managers outside the person’s own subtree', async () => {
    const user = userEvent.setup();
    renderWithProviders(<UsersPage />);
    await screen.findByText('Manager: Lin');
    await user.click(screen.getByRole('radio', { name: 'Org chart' }));
    // Edit Chen (the top manager): Lin and Wang are below Chen, Gone is suspended.
    await user.click(within(screen.getByTestId('org-chart')).getAllByRole('button', { name: /Chen/ })[0]);
    const dialog = await screen.findByRole('dialog');
    await user.click(within(dialog).getByRole('combobox', { name: 'Direct manager' }));
    await screen.findByRole('listbox');
    const options = screen.getAllByRole('option').map((o) => o.textContent);
    expect(options).toEqual(['None', 'Admin']);
  });

  it('sends the chosen manager and shows a refused reporting line in plain words', async () => {
    const user = userEvent.setup();
    renderWithProviders(<UsersPage />);
    await screen.findByText('Manager: Lin');
    await user.click(screen.getByRole('radio', { name: 'Org chart' }));
    await user.click(within(screen.getByTestId('org-chart')).getByRole('button', { name: /Chang/ }));
    const dialog = await screen.findByRole('dialog');
    await user.click(within(dialog).getByRole('combobox', { name: 'Direct manager' }));
    await user.click(await screen.findByRole('option', { name: 'Lin' }));
    await user.click(within(dialog).getByRole('button', { name: 'Save' }));
    expect(updateCalls).toEqual([{ user_id: 'other', manager_user_id: 'lead' }]);
    expect(
      await within(dialog).findByText('That would make the reporting line loop back on itself. Choose a different manager.'),
    ).toBeInTheDocument();
  });

  it('shows roles, statuses and AI staff in plain words, with display names', async () => {
    useAgentsStore.setState({
      agents: [{ name: 'sales', display_name: 'Sales Rep', status: 'active' }] as never[],
      loaded: true,
      loading: false,
    } as never);
    renderWithProviders(<UsersPage />);
    await screen.findByText('Manager: Lin');
    expect(screen.getAllByText('Active').length).toBeGreaterThan(0);
    expect(screen.getByText('Suspended')).toBeInTheDocument();
    expect(screen.getAllByText('Employee').length).toBeGreaterThan(0);
    expect(screen.queryByText('active')).not.toBeInTheDocument();
    expect(screen.queryByText('employee')).not.toBeInTheDocument();
    expect(screen.getByText('Sales Rep')).toBeInTheDocument();
    expect(screen.queryByText('sales')).not.toBeInTheDocument();
  });

  it('stacks each person into a card on a phone-width screen', async () => {
    const original = window.matchMedia;
    window.matchMedia = ((query: string) => ({
      matches: query.includes('max-width'),
      media: query,
      addEventListener: () => {},
      removeEventListener: () => {},
    })) as unknown as typeof window.matchMedia;
    try {
      const { container } = renderWithProviders(<UsersPage />);
      await screen.findByText('Manager: Lin');
      expect(container.querySelector('[role="grid"], [data-slot="list-grid"]')).toBeNull();
      const cards = container.querySelectorAll('li');
      expect(cards.length).toBe(USERS.length);
      expect(within(cards[2] as HTMLElement).getByText('Manager: Chen')).toBeInTheDocument();
      expect(within(cards[2] as HTMLElement).getByRole('button', { name: 'More' })).toBeInTheDocument();
    } finally {
      window.matchMedia = original;
    }
  });
});


describe('UsersPage — invitation eligibility', () => {
  it.each(['active', 'suspended'])('offers an initial invitation only for an active eligible user (%s)', async (status) => {
    const previousUser = useAuthStore.getState().user;
    useAuthStore.setState({ user: { id: 'admin', role: 'admin' } as never });
    mockWsClient.call.mockImplementation(async (method: string) => {
      if (method === 'users.list') return { users: [{
        id: 'new-person', email: 'person@example.com', display_name: 'New Person',
        role: 'employee', status, invite_eligible: true, bindings: [],
        created_at: '', updated_at: '',
      }] };
      if (method === 'users.invite_status') return { statuses: { 'new-person': 'none' } };
      return { users: [], departments: [], agents: [], identities: [] };
    });
    try {
      const user = userEvent.setup();
      renderWithProviders(<UsersPage />);
      await screen.findByText('New Person');
      await user.click(screen.getByRole('button', { name: 'More' }));
      await screen.findByRole('menuitem', { name: 'Edit' });
      if (status === 'active') {
        const invite = await screen.findByRole('menuitem', { name: 'Resend invitation' });
        expect(invite).toBeInTheDocument();
        await user.click(invite);
        expect(await screen.findByText('Email preview (sent in Traditional Chinese)')).toBeInTheDocument();
      } else {
        expect(screen.queryByRole('menuitem', { name: 'Resend invitation' })).not.toBeInTheDocument();
      }
    } finally {
      useAuthStore.setState({ user: previousUser });
    }
  });
});

describe('UsersPage — primary AI', () => {
  const member = {
    id: 'member', email: 'member@example.com', display_name: 'Member', role: 'employee', status: 'active',
    primary_agent_id: null, created_at: '', updated_at: '', assignable_primary_agents: ['first-ai'],
    bindings: [
      { user_id: 'member', agent_name: 'first-ai', access_level: 'operator', bound_at: '2026-01-01' },
      { user_id: 'member', agent_name: 'viewer-ai', access_level: 'viewer', bound_at: '2025-01-01' },
    ],
  } as UserDetail;
  const preview = [{ user_id: member.id, display_name: member.display_name, agent_id: 'first-ai' }];

  it('previews the named members and repairs only after confirmation', async () => {
    const user = userEvent.setup();
    mockWsClient.call.mockImplementation(async (method: string) => {
      if (method === 'users.list') return { users: [member] };
      if (method === 'users.repair_primary_agents') return { members: preview, preview_digest: 'digest-1', excluded_personal_count: 1, changed: false };
      return { identities: [], departments: [], agents: [] };
    });
    renderWithProviders(<UsersPage />);
    await user.click(await screen.findByRole('button', { name: 'Set to their first AI' }));
    const dialog = await screen.findByRole('dialog');
    expect(within(dialog).getByText(/Member → first-ai/)).toBeInTheDocument();
    expect(within(dialog).getByText('Personal AI for these members must be set by the members themselves.')).toBeInTheDocument();
    expect(mockWsClient.call).toHaveBeenCalledWith('users.repair_primary_agents', {});
    expect(mockWsClient.call).not.toHaveBeenCalledWith('users.repair_primary_agents', { preview_digest: 'digest-1' });
    await user.click(within(dialog).getByRole('button', { name: 'Confirm' }));
    expect(mockWsClient.call).toHaveBeenCalledWith('users.repair_primary_agents', { preview_digest: 'digest-1' });
  });

  it('offers only eligible bindings and saves the selected primary for that member', async () => {
    const user = userEvent.setup();
    const onUpdated = vi.fn();
    renderWithProviders(<EditUserDialog user={{ ...member, primary_personal_excluded: true, bindings: [...member.bindings, { user_id: 'member', agent_name: 'private-ai', access_level: 'owner', bound_at: '' }] }} users={[member]} onClose={vi.fn()} onUpdated={onUpdated} />);
    await user.click(screen.getByRole('combobox', { name: 'Primary AI' }));
    expect(screen.queryByRole('option', { name: 'viewer-ai' })).not.toBeInTheDocument();
    expect(screen.queryByRole('option', { name: 'private-ai' })).not.toBeInTheDocument();
    expect(screen.getByText('Personal AI for these members must be set by the members themselves.')).toBeInTheDocument();
    await user.click(await screen.findByRole('option', { name: 'first-ai' }));
    await user.click(screen.getByRole('button', { name: 'Set primary AI' }));
    expect(mockWsClient.call).toHaveBeenCalledWith('users.set_primary_agent', { agent_id: 'first-ai', user_id: 'member' });
    expect(onUpdated).toHaveBeenCalled();
  });

  it('uses the preview count even when users.list has no visible eligible bindings', async () => {
    mockWsClient.call.mockImplementation(async (method: string) => {
      if (method === 'users.list') return { users: [{ ...member, bindings: [] }] };
      if (method === 'users.repair_primary_agents') return { members: preview, preview_digest: 'digest-1', excluded_personal_count: 0, changed: false };
      return { identities: [], departments: [], agents: [] };
    });
    renderWithProviders(<UsersPage />);
    expect(await screen.findByRole('button', { name: 'Set to their first AI' })).toBeInTheDocument();
  });

  it('hides the repair action from a non-admin', async () => {
    useAuthStore.setState({ user: { id: 'member', role: 'employee' } as never });
    mockWsClient.call.mockImplementation(async (method: string) => method === 'users.list' ? { users: [member] } : { identities: [] });
    renderWithProviders(<UsersPage />);
    await screen.findByText('Member');
    expect(screen.queryByRole('button', { name: 'Set to their first AI' })).not.toBeInTheDocument();
  });
});
