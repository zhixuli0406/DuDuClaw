import { beforeEach, expect, it, vi } from 'vitest';
import { screen, within, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { useAuthStore } from '@/stores/auth-store';
import { MemberKeysSection } from './MemberKeysSection';

const secret = 'ddc_member_test-secret-not-a-real-key';
const key = { include_conversations: true, conversations_migrated: true, mcp_scopes: ['tasks:read'], user_id: 'hidden-owner-id', user_email: 'owner@example.com', id: 'k1', name: 'External server', created_at: '2026-11-25T00:00:00Z', expires_at: '2027-02-23T00:00:00Z', last_used_at: null, revoked_at: null, active: true };
let policy: { allow_conversation_content: boolean; allow_mcp_scopes: string[] };
let connected: boolean;
let readable: boolean;
beforeEach(() => {
  policy = { allow_conversation_content: true, allow_mcp_scopes: ['tasks:read', 'agents:status', 'odoo:read', 'odoo:write'] };
  connected = true;
  readable = true;
  vi.clearAllMocks();
  useAuthStore.setState({ user: { id: 'member', role: 'employee' } as never });
  mockWsClient.call.mockImplementation(async (method: string) => {
    if (method === 'me.member_keys.list' || method === 'member_keys.admin_list') return { policy, keys: [key], next_cursor: null, max_active: 5, max_total: 50, reauthentication: 'password' };
    if (method === 'me.member_keys.create') return { key, secret };
    if (method === 'me.odoo.status') return { connected };
    if (method === 'users.list') return { users: [{ id: 'hidden-owner-id', display_name: 'Alice', email: 'owner@example.com' }] };
    if (method === 'member_keys.policy_get') return readable ? { readable: true, enabled: true, ...policy, available_mcp_scopes: ['tasks:read', 'agents:status', 'odoo:read', 'odoo:write'], risk: { conversation: 'Conversation risk from server', mcp_scopes: 'Tool risk from server' } } : { readable: false, message: 'Policy cannot be read.' };
    if (method === 'member_keys.policy_set') return {};
    if (method === 'system.config') return { member_api_keys_enabled: true };
    return { revoked: true };
  });
});

it('shows the secret only in its creation dialog and clears it on close', async () => {
  const user = userEvent.setup();
  renderWithProviders(<MemberKeysSection />);
  await user.click(await screen.findByRole('button', { name: 'Create key' }));
  const dialog = screen.getByRole('dialog');
  await user.type(within(dialog).getByLabelText('Name'), 'External server');
  expect(within(dialog).getByLabelText('Expiry')).toHaveValue('90');
  expect(within(dialog).getByRole('button', { name: 'Create key' })).toBeDisabled();
  await user.type(within(dialog).getByLabelText('Current password'), 'current-password');
  await user.click(within(dialog).getByRole('button', { name: 'Create key' }));
  expect(await screen.findByDisplayValue(secret)).toBeInTheDocument();
  expect(mockWsClient.call).toHaveBeenCalledWith('me.member_keys.create', { name: 'External server', expires_in_days: 90, password: 'current-password', include_conversations: false, mcp_scopes: [] });
  await user.click(screen.getByRole('button', { name: 'I have saved it' }));
  expect(screen.queryByDisplayValue(secret)).not.toBeInTheDocument();
  expect(document.body.textContent).not.toContain(secret);
  expect(localStorage.getItem('member_key')).toBeNull();
});

it('provides admin toggle and all-key revoke without a create button', async () => {
  useAuthStore.setState({ user: { id: 'admin', role: 'admin' } as never });
  const user = userEvent.setup();
  renderWithProviders(<MemberKeysSection />);
  await screen.findByText('External server');
  expect(screen.queryByRole('button', { name: 'Create key' })).not.toBeInTheDocument();
  await user.click(screen.getByRole('button', { name: 'Disable member keys' }));
  await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('system.update_config', { member_api_keys_enabled: false }));
  expect(mockWsClient.call).toHaveBeenCalledWith('member_keys.admin_list', { limit: 25 });
});

it('does not show creation or retained keys when the master switch is off', async () => {
  mockWsClient.call.mockRejectedValue(Object.assign(new Error('disabled'), { code: 'member_keys_disabled' }));
  renderWithProviders(<MemberKeysSection />);
  await screen.findByText('Member keys are disabled. Key use and management are unavailable.');
  expect(screen.queryByRole('button', { name: 'Create key' })).not.toBeInTheDocument();
  expect(screen.queryByText('External server')).not.toBeInTheDocument();
});

it('asks MFA accounts for a factor instead of a password', async () => {
  mockWsClient.call.mockImplementation(async (method: string) => {
    if (method === 'me.member_keys.list') return { keys: [], next_cursor: null, reauthentication: 'mfa' };
    if (method === 'me.member_keys.create') return { key, secret };
    return {};
  });
  const user = userEvent.setup();
  renderWithProviders(<MemberKeysSection />);
  await user.click(await screen.findByRole('button', { name: 'Create key' }));
  const dialog = screen.getByRole('dialog');
  expect(within(dialog).queryByLabelText('Current password')).not.toBeInTheDocument();
  await user.type(within(dialog).getByLabelText('Name'), 'MFA key');
  await user.type(within(dialog).getByLabelText(/Fresh MFA or recovery code/), '123456');
  await user.click(within(dialog).getByRole('button', { name: 'Create key' }));
  await screen.findByDisplayValue(secret);
  expect(mockWsClient.call).toHaveBeenCalledWith('me.member_keys.create', { name: 'MFA key', expires_in_days: 90, code: '123456', include_conversations: false, mcp_scopes: [] });
});

it('clears a refused password and requires fresh input before retry', async () => {
  mockWsClient.call.mockImplementation(async (method: string) => {
    if (method === 'me.member_keys.list') return { keys: [], next_cursor: null, reauthentication: 'password' };
    throw Object.assign(new Error('refused'), { code: 'member_key_step_up_required' });
  });
  const user = userEvent.setup();
  renderWithProviders(<MemberKeysSection />);
  await user.click(await screen.findByRole('button', { name: 'Create key' }));
  const dialog = screen.getByRole('dialog');
  await user.type(within(dialog).getByLabelText('Name'), 'Attempt');
  await user.type(within(dialog).getByLabelText('Current password'), 'wrong');
  await user.click(within(dialog).getByRole('button', { name: 'Create key' }));
  await waitFor(() => expect(within(dialog).getByLabelText('Current password')).toHaveValue(''));
  expect(within(dialog).getByRole('button', { name: 'Create key' })).toBeDisabled();
  expect(within(dialog).getByRole('alert')).toHaveTextContent('Enter your current password');
});

it.each(['employee', 'admin'])('paginates the %s list and refreshes to its first page', async role => {
  useAuthStore.setState({ user: { id: role, role } as never });
  const methodName = role === 'admin' ? 'member_keys.admin_list' : 'me.member_keys.list';
  mockWsClient.call.mockImplementation(async (method: string, params?: { cursor?: string }) => {
    if (method === 'system.config') return { member_api_keys_enabled: true };
    if (method === methodName) return params?.cursor
      ? { keys: [{ ...key, id: 'k2', name: 'Second page' }], next_cursor: null, reauthentication: 'password' }
      : { keys: [key], next_cursor: 'next-cursor', reauthentication: 'password' };
    return {};
  });
  const user = userEvent.setup();
  renderWithProviders(<MemberKeysSection />);
  await user.click(await screen.findByRole('button', { name: 'Next page' }));
  await screen.findByText('Second page');
  expect(screen.queryByText('External server')).not.toBeInTheDocument();
  expect(mockWsClient.call).toHaveBeenCalledWith(methodName, { limit: 25, cursor: 'next-cursor' });
  expect(screen.queryByRole('button', { name: 'Next page' })).not.toBeInTheDocument();
  await user.click(screen.getByRole('button', { name: 'Refresh' }));
  await screen.findByText('External server');
});

async function openCreate() {
  const user = userEvent.setup();
  renderWithProviders(<MemberKeysSection />);
  await user.click(await screen.findByRole('button', { name: 'Create key' }));
  return { user, dialog: within(screen.getByRole('dialog')) };
}

it('disables conversation and unapproved scopes and hides all scopes when none are allowed', async () => {
  policy = { allow_conversation_content: false, allow_mcp_scopes: [] };
  const { dialog } = await openCreate();
  expect(dialog.getByRole('checkbox', { name: 'Include my conversations' })).toHaveAttribute('aria-disabled', 'true');
  expect(dialog.getByText('The administrator has not opened any external tool scopes yet.')).toBeInTheDocument();
  expect(dialog.queryByRole('checkbox', { name: 'My tasks' })).not.toBeInTheDocument();
});

it.each([false, 'failure'])('disables Odoo scopes when connection is %s', async connection => {
  connected = false;
  if (connection === 'failure') {
    const implementation = mockWsClient.call.getMockImplementation()!;
    mockWsClient.call.mockImplementation((method, ...args) => method === 'me.odoo.status' ? Promise.reject(new Error('offline')) : implementation(method, ...args));
  }
  const { dialog } = await openCreate();
  expect(dialog.getByRole('checkbox', { name: 'Query using my Odoo account' })).toHaveAttribute('aria-disabled', 'true');
  expect(dialog.getByRole('checkbox', { name: 'Modify using my Odoo account' })).toHaveAttribute('aria-disabled', 'true');
  expect(dialog.getAllByRole('link')[0]).toHaveAttribute('href', '/me/connections');
});

it('disables scopes outside policy even when Odoo is connected', async () => {
  policy.allow_mcp_scopes = ['tasks:read'];
  const { dialog } = await openCreate();
  expect(dialog.getByRole('checkbox', { name: 'My tasks' })).toBeEnabled();
  expect(dialog.getByRole('checkbox', { name: 'AI employees I can use and their status' })).toHaveAttribute('aria-disabled', 'true');
  expect(dialog.getByRole('checkbox', { name: 'Modify using my Odoo account' })).toHaveAttribute('aria-disabled', 'true');
});

it('creates with selected conversation and tool scopes and maps scope refusals', async () => {
  const implementation = mockWsClient.call.getMockImplementation()!;
  mockWsClient.call.mockImplementation((method, ...args) => method === 'me.member_keys.create'
    ? Promise.reject(Object.assign(new Error('raw'), { code: 'member_key_scope_not_allowed' })) : implementation(method, ...args));
  const { user, dialog } = await openCreate();
  await user.click(dialog.getByRole('checkbox', { name: 'Include my conversations' }));
  await user.click(dialog.getByRole('checkbox', { name: 'My tasks' }));
  await user.type(dialog.getByLabelText('Name'), 'Selected');
  await user.type(dialog.getByLabelText('Current password'), 'password');
  await user.click(dialog.getByRole('button', { name: 'Create key' }));
  expect(mockWsClient.call).toHaveBeenCalledWith('me.member_keys.create', { name: 'Selected', expires_in_days: 90, password: 'password', include_conversations: true, mcp_scopes: ['tasks:read'] });
  expect(dialog.getByRole('alert')).toHaveTextContent('This key requests scopes beyond what the administrator currently allows. Please select again.');
});

it.each(['employee', 'admin'])('shows badges and narrows the %s key without allowing additions', async role => {
  useAuthStore.setState({ user: { id: role, role } as never });
  const user = userEvent.setup();
  renderWithProviders(<MemberKeysSection />);
  await screen.findByText('Includes conversations (retained on upgrade)');
  expect(screen.getByText('My tasks')).toBeInTheDocument();
  if (role === 'admin') expect(await screen.findByText('Alice')).toBeInTheDocument();
  expect(document.body.textContent).not.toContain('hidden-owner-id');
  expect(document.body.textContent).not.toContain('k1');
  await user.click(screen.getByRole('button', { name: 'Narrow' }));
  const dialog = within(screen.getByRole('dialog'));
  const conversation = dialog.getByRole('checkbox', { name: 'Include my conversations' });
  const tasks = dialog.getByRole('checkbox', { name: 'My tasks' });
  expect(conversation).toBeChecked();
  expect(tasks).toBeChecked();
  expect(dialog.getByRole('checkbox', { name: 'AI employees I can use and their status' })).toHaveAttribute('aria-disabled', 'true');
  await user.click(conversation);
  await user.click(tasks);
  await user.click(dialog.getByRole('button', { name: 'Apply' }));
  await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith(role === 'admin' ? 'member_keys.admin_narrow' : 'me.member_keys.narrow', { id: 'k1', include_conversations: false, mcp_scopes: [] }));
});

it.each(['conversation', 'scope'])('requires acknowledgement to open %s policy, but closes directly', async kind => {
  useAuthStore.setState({ user: { id: 'admin', role: 'admin' } as never });
  policy = { allow_conversation_content: false, allow_mcp_scopes: [] };
  const user = userEvent.setup();
  renderWithProviders(<MemberKeysSection />);
  const label = kind === 'conversation' ? 'New keys may include conversations' : 'Member tasks';
  const checkbox = await screen.findByRole('checkbox', { name: label });
  await waitFor(() => expect(checkbox).toBeEnabled());
  await user.click(checkbox);
  const dialog = within(screen.getByRole('dialog'));
  expect(dialog.getByText(kind === 'conversation' ? 'Conversation risk from server' : 'Tool risk from server')).toBeInTheDocument();
  expect(dialog.getByRole('button', { name: 'Open' })).toBeDisabled();
  expect(mockWsClient.call).not.toHaveBeenCalledWith('member_keys.policy_set', expect.anything());
  await user.click(dialog.getByRole('checkbox', { name: 'I understand the effects of opening this' }));
  policy = { allow_conversation_content: kind === 'conversation', allow_mcp_scopes: kind === 'scope' ? ['tasks:read'] : [] };
  await user.click(dialog.getByRole('button', { name: 'Open' }));
  const opened = kind === 'conversation' ? { allow_conversation_content: true } : { allow_mcp_scopes: ['tasks:read'] };
  await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('member_keys.policy_set', { ...opened, acknowledge_risk: true }));
  await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
  await user.click(screen.getByRole('checkbox', { name: label }));
  await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('member_keys.policy_set', kind === 'conversation' ? { allow_conversation_content: false } : { allow_mcp_scopes: [] }));
});

it('fails closed when the admin policy is unreadable', async () => {
  useAuthStore.setState({ user: { id: 'admin', role: 'admin' } as never });
  readable = false;
  renderWithProviders(<MemberKeysSection />);
  await screen.findByText('Policy cannot be read.');
  for (const checkbox of screen.getAllByRole('checkbox')) expect(checkbox).toHaveAttribute('aria-disabled', 'true');
});

it('renders ordinary conversation badges without the upgrade label', async () => {
  const implementation = mockWsClient.call.getMockImplementation()!;
  mockWsClient.call.mockImplementation((method, ...args) => method === 'me.member_keys.list'
    ? Promise.resolve({ keys: [{ ...key, conversations_migrated: false }], policy, reauthentication: 'password' }) : implementation(method, ...args));
  renderWithProviders(<MemberKeysSection />);
  expect(await screen.findByText('Includes conversations')).toBeInTheDocument();
  expect(screen.queryByText('Includes conversations (retained on upgrade)')).not.toBeInTheDocument();
});

it('loads and changes policy even while member keys are disabled', async () => {
  useAuthStore.setState({ user: { id: 'admin', role: 'admin' } as never });
  const implementation = mockWsClient.call.getMockImplementation()!;
  mockWsClient.call.mockImplementation((method, ...args) => method === 'system.config'
    ? Promise.resolve({ member_api_keys_enabled: false }) : implementation(method, ...args));
  const user = userEvent.setup();
  renderWithProviders(<MemberKeysSection />);
  const checkbox = await screen.findByRole('checkbox', { name: 'New keys may include conversations' });
  await waitFor(() => expect(checkbox).toBeChecked());
  await user.click(checkbox);
  await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('member_keys.policy_set', { allow_conversation_content: false }));
  expect(mockWsClient.call).not.toHaveBeenCalledWith('member_keys.admin_list', expect.anything());
});

it.each(['failure', 'missing'])('falls back to owner email when the directory entry is %s', async state => {
  useAuthStore.setState({ user: { id: 'admin', role: 'admin' } as never });
  const implementation = mockWsClient.call.getMockImplementation()!;
  mockWsClient.call.mockImplementation((method, ...args) => method === 'users.list'
    ? (state === 'failure' ? Promise.reject(new Error('offline')) : Promise.resolve({ users: [] })) : implementation(method, ...args));
  renderWithProviders(<MemberKeysSection />);
  expect(await screen.findByText('owner@example.com')).toBeInTheDocument();
  expect(document.body.textContent).not.toContain('hidden-owner-id');
});

it('keeps disallowed narrow options unchecked when clicked', async () => {
  const user = userEvent.setup();
  renderWithProviders(<MemberKeysSection />);
  await user.click(await screen.findByRole('button', { name: 'Narrow' }));
  const dialog = within(screen.getByRole('dialog'));
  const checkbox = dialog.getByRole('checkbox', { name: 'AI employees I can use and their status' });
  await user.click(checkbox);
  expect(checkbox).not.toBeChecked();
});
