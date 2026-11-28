import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen, within, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { UsersPage } from './UsersPage';
import { EditUserDialog } from './users/UserFormDialogs';
import { IdentityChips } from './users/UserRowParts';
import type { ChannelIdentity } from '@/lib/channel-identity-api';
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
  it('shows authoritative seats separately from exempt avatars', async () => {
    mockWsClient.call.mockImplementation((method: string) => Promise.resolve(method === 'license.status'
      ? { seats_used: 5, seat_limit: 5, avatars_exempt: 2 }
      : { users: [], departments: [], agents: [] }));
    renderWithProviders(<UsersPage />);
    expect(await screen.findByText('AI employee seats: 5 / 5 · AI avatars: 2')).toBeInTheDocument();
    expect(screen.getByText('AI avatars do not use employee seats')).toBeInTheDocument();
  });

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

describe('UsersPage — create a member avatar', () => {
  const member: UserDetail = {
    id: 'member', email: 'member@example.com', display_name: 'Member', role: 'employee', status: 'active',
    avatar_agent_id: null, created_at: '', updated_at: '', bindings: [],
  };
  const newAvatar = { name: 'created-avatar', display_name: 'Member avatar', status: 'active' };
  let currentMember: UserDetail;
  let created: boolean;
  let createError: unknown;

  beforeEach(() => {
    currentMember = { ...member };
    created = false;
    createError = null;
    useAgentsStore.setState({ agents: [], loaded: true, loading: false });
    mockWsClient.call.mockImplementation(async (method: string) => {
      if (method === 'users.list') return { users: [currentMember] };
      if (method === 'license.status') return { seats_used: 5, seat_limit: 5, avatars_exempt: created ? 1 : 0 };
      if (method === 'models.list') return { models: [{ id: 'local-model', label: 'Local model', type: 'local' }] };
      if (method === 'agents.list') return { agents: created ? [newAvatar] : [] };
      if (method === 'agents.create') {
        if (createError) throw createError;
        created = true;
        currentMember = { ...currentMember, avatar_agent_id: newAvatar.name };
        return { success: true, agent: newAvatar };
      }
      return { users: [], departments: [], agents: [], identities: [], conflicts: [], statuses: {} };
    });
  });

  it.each([
    ['admin', 'employee', 'active', null, true],
    ['admin', 'manager', 'active', null, true],
    ['manager', 'employee', 'active', null, false],
    ['employee', 'employee', 'active', null, false],
    ['admin', 'admin', 'active', null, false],
    ['admin', 'employee', 'suspended', null, false],
    ['admin', 'employee', 'offboarded', null, false],
    ['admin', 'employee', 'active', 'existing-avatar', false],
  ])('gates creation for viewer=%s, member role=%s, status=%s, avatar=%s', async (viewer, role, status, avatar, visible) => {
    useAuthStore.setState({ user: { id: 'viewer', role: viewer } as never });
    currentMember = { ...member, role, status, avatar_agent_id: avatar } as UserDetail;
    renderWithProviders(<UsersPage />);
    await screen.findByText('Member');
    expect(screen.queryByRole('button', { name: 'Create AI avatar' }) !== null).toBe(visible);
  });

  async function openDialog() {
    const user = userEvent.setup();
    renderWithProviders(<UsersPage />);
    await user.click(await screen.findByRole('button', { name: 'Create AI avatar' }));
    const dialog = await screen.findByRole('dialog', { name: 'Create AI avatar' });
    await within(dialog).findByRole('option', { name: 'Local model' });
    return { user, dialog };
  }

  it('creates with avatar_for_user at full capacity, then refreshes the avatar badge and seats', async () => {
    const { user, dialog } = await openDialog();
    expect(screen.getByText('AI employee seats: 5 / 5 · AI avatars: 0')).toBeInTheDocument();
    expect(within(dialog).getByText('AI avatars do not use licensed seats and can only be used by this member.')).toBeInTheDocument();
    const name = within(dialog).getByRole('textbox', { name: 'Display Name' });
    expect(name).toHaveValue('Member’s AI avatar');
    expect(within(dialog).getByRole('button', { name: 'Confirm' })).toBeDisabled();
    await user.selectOptions(within(dialog).getByLabelText('Model'), 'local-model');
    await user.clear(name);
    await user.type(name, '  ');
    expect(within(dialog).getByRole('button', { name: 'Confirm' })).toBeDisabled();
    await user.clear(name);
    await user.type(name, ' Member avatar ');
    // Enter submits the form through the same path as the confirm button.
    await user.type(name, '{Enter}');
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('agents.create', {
      name: expect.stringMatching(/^avatar-[a-f0-9]{32}$/),
      display_name: 'Member avatar', avatar_for_user: 'member', role: 'specialist', model_preferred: 'local-model',
    }));
    expect(await screen.findByText('AI employee seats: 5 / 5 · AI avatars: 1')).toBeInTheDocument();
    expect((await screen.findByText('AI avatar: Member avatar')).closest('[data-slot="badge"]')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Create AI avatar' })).not.toBeInTheDocument();
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    expect(mockWsClient.call.mock.calls.filter(([method]) => method === 'agents.create')).toHaveLength(1);
  });

  it.each([
    new Error('成員必須啟用且尚未設定 AI 分身。'),
    '這位成員已有 AI 分身。',
    { code: 'edition_not_allowed', message: 'Edition not allowed', message_zh_TW: '目前版本無法建立 AI 分身。' },
  ])('shows the server explanation and leaves the dialog open on refusal (%j)', async failure => {
    createError = failure;
    const { user, dialog } = await openDialog();
    await user.selectOptions(within(dialog).getByLabelText('Model'), 'local-model');
    await user.click(within(dialog).getByRole('button', { name: 'Confirm' }));
    const expected = typeof failure === 'string' ? failure : failure instanceof Error ? failure.message : failure.message_zh_TW;
    expect(await within(dialog).findByRole('alert')).toHaveTextContent(expected);
    expect(within(dialog).getByRole('button', { name: 'Confirm' })).toBeEnabled();
    expect(created).toBe(false);
  });

  it('cancels with the keyboard without creating an avatar', async () => {
    const { user, dialog } = await openDialog();
    within(dialog).getByRole('button', { name: 'Cancel' }).focus();
    await user.keyboard('{Enter}');
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
    expect(mockWsClient.call.mock.calls.some(([method]) => method === 'agents.create')).toBe(false);
  });

  it('offers the same action on mobile cards', async () => {
    const original = window.matchMedia;
    window.matchMedia = ((query: string) => ({
      matches: query.includes('max-width'), media: query,
      addEventListener: () => {}, removeEventListener: () => {},
    })) as unknown as typeof window.matchMedia;
    try {
      renderWithProviders(<UsersPage />);
      const card = (await screen.findByText('Member')).closest('li')!;
      expect(within(card).getByRole('button', { name: 'Create AI avatar' })).toBeEnabled();
    } finally { window.matchMedia = original; }
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

describe('IdentityChips — Google Chat (RM-152)', () => {
  const identities: ChannelIdentity[] = [
    { user_id: 'u', channel: 'telegram', channel_user_id: '42', verified: true, created_at: '' },
    { user_id: 'u', channel: 'googlechat', channel_user_id: 'users/1', verified: true, created_at: '' },
    { user_id: 'u', channel: 'googlechat', channel_user_id: 'users/2', verified: true, created_at: '', source: 'google_account' },
  ];

  it('removes only an administrator Google Chat binding; the Google-account one is read-only', async () => {
    const user = userEvent.setup();
    const onRemove = vi.fn();
    renderWithProviders(<IdentityChips state={identities} onAdd={vi.fn()} onRemove={onRemove} />);
    const remove = screen.getAllByRole('button', { name: /^Remove binding/ });
    expect(remove).toHaveLength(1);
    await user.click(remove[0]);
    expect(onRemove).toHaveBeenCalledWith(identities[1]);
    expect(screen.getByText('Google Chat · rs/2')).toBeInTheDocument();
    expect(screen.getByText('Telegram · 42')).toBeInTheDocument();
  });

  it('offers no removal without an admin handler', () => {
    renderWithProviders(<IdentityChips state={identities} onAdd={vi.fn()} />);
    expect(screen.queryByRole('button', { name: /^Remove binding/ })).not.toBeInTheDocument();
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

  it('lists shared-primary conflicts for administrators', async () => {
    mockWsClient.call.mockImplementation(async (method: string) => {
      if (method === 'users.list') return { users: [member] };
      if (method === 'users.avatar_conflicts') return { conflicts: [{ agent_id: 'shared-ai', user_ids: ['member', 'another-member'] }] };
      return { identities: [], departments: [], agents: [] };
    });
    renderWithProviders(<UsersPage />);
    expect(await screen.findByText('AI avatar conflicts to resolve')).toBeInTheDocument();
    expect(screen.getByText('shared-ai: Member, another-member')).toBeInTheDocument();
    expect(mockWsClient.call).toHaveBeenCalledWith('users.avatar_conflicts');
  });

  it('clears an assigned avatar with a separate save action', async () => {
    const user = userEvent.setup();
    renderWithProviders(<EditUserDialog user={{ ...member, avatar_agent_id: 'first-ai', primary_agent_id: 'first-ai' }} users={[member]} onClose={vi.fn()} onUpdated={vi.fn()} />);
    await user.click(screen.getByRole('combobox', { name: 'AI avatar' }));
    await user.click(await screen.findByRole('option', { name: 'None' }));
    await user.click(screen.getByRole('button', { name: 'Set AI avatar' }));
    expect(mockWsClient.call).toHaveBeenCalledWith('users.set_avatar', { user_id: 'member', agent_id: null });
    expect(mockWsClient.call.mock.calls.some(([method]) => method === 'users.update')).toBe(false);
  });

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
    expect(within(dialog).getByText('Some Personal AI employees cannot be assigned here. Contact an administrator to check.')).toBeInTheDocument();
    expect(mockWsClient.call).toHaveBeenCalledWith('users.repair_primary_agents', {});
    expect(mockWsClient.call).not.toHaveBeenCalledWith('users.repair_primary_agents', { preview_digest: 'digest-1' });
    await user.click(within(dialog).getByRole('button', { name: 'Confirm' }));
    expect(mockWsClient.call).toHaveBeenCalledWith('users.repair_primary_agents', { preview_digest: 'digest-1' });
  });

  it('offers only eligible bindings and saves the selected avatar for that member', async () => {
    const user = userEvent.setup();
    const onUpdated = vi.fn();
    renderWithProviders(<EditUserDialog user={{ ...member, primary_personal_excluded: true, bindings: [...member.bindings, { user_id: 'member', agent_name: 'private-ai', access_level: 'owner', bound_at: '' }] }} users={[member]} onClose={vi.fn()} onUpdated={onUpdated} />);
    await user.click(screen.getByRole('combobox', { name: 'AI avatar' }));
    expect(screen.queryByRole('option', { name: 'viewer-ai' })).not.toBeInTheDocument();
    expect(screen.queryByRole('option', { name: 'private-ai' })).not.toBeInTheDocument();
    expect(screen.getByText('Some Personal AI employees cannot be assigned here. Contact an administrator to check.')).toBeInTheDocument();
    await user.click(await screen.findByRole('option', { name: 'first-ai' }));
    await user.click(screen.getByRole('button', { name: 'Set AI avatar' }));
    expect(mockWsClient.call).toHaveBeenCalledWith('users.set_avatar', { agent_id: 'first-ai', user_id: 'member' });
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


describe('IdentityChips — proof status', () => {
  it('shows proof tags and time, masks ids, and retains the full removal identity', async () => {
    const identity: ChannelIdentity = { user_id: 'private-user', channel: 'googlechat', channel_user_id: 'users/123456789', verified: true, created_at: '', proof_status: 'self_confirmed', proven_at: '2026-11-27T08:52:00Z' };
    const onRemove = vi.fn();
    const { container } = renderWithProviders(<IdentityChips state={[identity, { ...identity, channel: 'telegram', proof_status: 'admin_prefilled', proven_at: null }]} onAdd={vi.fn()} onRemove={onRemove} />);
    expect(screen.getByText('Self-confirmed')).toBeInTheDocument();
    expect(screen.getByText('Admin-prefilled')).toBeInTheDocument();
    expect(screen.getByText('Google Chat · 6789')).toBeInTheDocument();
    expect(screen.getByText('Telegram · 6789')).toBeInTheDocument();
    expect(screen.getByTitle(/Nov 27, 2026, 8:52 AM Self-confirmed/)).toBeInTheDocument();
    expect(container.innerHTML).not.toContain(identity.channel_user_id);
    expect(container.innerHTML).not.toContain(identity.user_id);
    await userEvent.click(screen.getByRole('button', { name: 'Remove binding Google Chat · 6789' }));
    expect(onRemove).toHaveBeenCalledWith(identity);
  });
});


it('sends the full channel id when a masked member binding is removed', async () => {
  const identity: ChannelIdentity = { user_id: 'member-private', channel: 'googlechat', channel_user_id: 'users/987654321', verified: true, created_at: '', proof_status: 'admin_prefilled' };
  const fetchMock = vi.spyOn(globalThis, 'fetch').mockResolvedValue({ ok: true, json: async () => ({ identities: [identity] }) } as Response);
  mockWsClient.call.mockImplementation(async (method: string) => method === 'users.list'
    ? { users: [{ id: identity.user_id, email: 'member@example.com', display_name: 'Member', role: 'employee', status: 'active', bindings: [] }] }
    : { users: [], agents: [], departments: [], conflicts: [], requests: [] });
  try {
    renderWithProviders(<UsersPage />);
    await userEvent.click(await screen.findByRole('button', { name: 'Remove binding Google Chat · 4321' }));
    await waitFor(() => expect(fetchMock).toHaveBeenCalledWith('/api/channel-identity/unbind', expect.objectContaining({
      method: 'POST', body: JSON.stringify({ user_id: identity.user_id, channel: identity.channel, channel_user_id: identity.channel_user_id }),
    })));
  } finally { fetchMock.mockRestore(); }
});
