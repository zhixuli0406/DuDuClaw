import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { useAuthStore } from '@/stores/auth-store';
import { useAgentsStore } from '@/stores/agents-store';
import type { SigninAvatar, UserDetail } from '@/lib/api';
import type { OdooMemberCredential } from '@/lib/member-signin';
import { UsersPage } from '../UsersPage';
import { MemberSigninBadges } from './UserRowParts';
import { EditUserDialog } from './UserFormDialogs';

const stepUp = vi.hoisted(() => ({ ensure: vi.fn(), invalidate: vi.fn() }));
vi.mock('@/components/step-up/useAdminStepUp', () => ({ useAdminStepUp: () => ({ ...stepUp, dialog: null }) }));
const token = Array.from({ length: 32 }, (_, i) => i);
const member: UserDetail = {
  id: 'private-member-id', email: 'member@example.com', display_name: 'Member', role: 'employee',
  status: 'active', created_at: '', updated_at: '', bindings: [],
};
const credential: OdooMemberCredential = {
  user_id: member.id, email: member.email, display_name: member.display_name, login: 'private-odoo-login',
  state: 'ok', verified_at: null, last_used_at: null,
};
let roster: UserDetail[];
let credentials: OdooMemberCredential[];
let failure: { method: string; error: unknown } | null;
beforeEach(() => {
  vi.clearAllMocks();
  stepUp.ensure.mockResolvedValue(token);
  roster = [{ ...member }]; credentials = []; failure = null;
  useAuthStore.setState({ user: { id: 'admin', role: 'admin' } as never });
  useAgentsStore.setState({ agents: [], loaded: true, loading: false });
  mockWsClient.call.mockImplementation(async (method: string) => {
    if (failure?.method === method) throw failure.error;
    if (method === 'users.list') return { users: roster };
    if (method === 'odoo.member_credentials.list') return { credentials, enabled: true };
    if (method === 'models.list') return { models: [] };
    return { users: [], agents: [], departments: [], conflicts: [], identities: [], requests: [] };
  });
});

const reasons = [
  ['template_changed', 'Template changed'], ['template_invalid', 'Template unavailable'],
  ['seats_full', 'No seats available'], ['failed', 'Creation failed'], ['protected_mode', 'Protected mode'],
  ['not_eligible', 'Not eligible for creation'], ['policy_changed', 'Settings changed'],
  ['name_taken', 'Name already in use'], ['unknown', 'Unknown reason'], [null, 'Unknown reason'],
] as const;
describe('member sign-in badges', () => {
  it.each(['pending', 'creating', 'retrying'] as const)('shows %s as preparing', state => {
    renderWithProviders(<MemberSigninBadges user={{ ...member, signin_avatar: { state, reason: null, retry_allowed: false } }} />);
    expect(screen.getByText('AI avatar preparing')).toBeInTheDocument();
  });
  it.each(reasons)('translates waiting reason %s', (reason, label) => {
    const { container } = renderWithProviders(<MemberSigninBadges user={{ ...member, signin_avatar: { state: 'waiting_admin', reason, retry_allowed: true } }} />);
    expect(screen.getByText(`AI avatar waiting for administrator: ${label}`)).toBeInTheDocument();
    expect(container.querySelector('.bg-destructive')).toBeInTheDocument();
  });
  it.each([null, { state: 'done', reason: 'done', retry_allowed: false }] as const)('hides finished/absent onboarding %s', signin_avatar => {
    const { container } = renderWithProviders(<MemberSigninBadges user={{ ...member, signin_avatar }} />);
    expect(container.textContent).toBe('');
  });
  it.each([['ok', 'Odoo connected'], ['needs_reverify', 'Odoo needs reverification'], ['invalid', 'Odoo invalid']] as const)('shows Odoo %s', (state, label) => {
    const { container } = renderWithProviders(<MemberSigninBadges user={{ ...member, google_signin_linked: true }} credential={{ ...credential, state }} />);
    expect(screen.getByText(label)).toBeInTheDocument();
    expect(screen.getByText('Google sign-in')).toBeInTheDocument();
    expect(container.innerHTML).not.toContain(member.id);
    expect(container.innerHTML).not.toContain(credential.login);
  });
});

function waiting(reason: SigninAvatar['reason'], retry_allowed = true) {
  roster = [{ ...member, signin_avatar: { state: 'waiting_admin', reason, retry_allowed } }];
}
async function chooseMember() {
  fireEvent.click(await screen.findByRole('checkbox', { name: 'Select Member' }));
}
async function openRisk() {
  await chooseMember();
  fireEvent.click(screen.getByRole('button', { name: 'Confirm Google sign-in in bulk' }));
  return screen.findByRole('dialog');
}
async function confirmRisk() {
  const dialog = await openRisk();
  const button = within(dialog).getByRole('button', { name: 'Confirm 1 members' });
  expect(button).toBeDisabled();
  expect(stepUp.ensure).not.toHaveBeenCalled();
  expect(within(dialog).getByText(/within 14 days/)).toBeInTheDocument();
  await userEvent.click(within(dialog).getByRole('checkbox', { name: 'I understand the effects of confirming' }));
  await userEvent.click(button);
}

describe('member actions', () => {
  it('retries onboarding and refreshes the roster', async () => {
    waiting('template_changed'); renderWithProviders(<UsersPage />);
    await userEvent.click(await screen.findByRole('button', { name: 'Retry' }));
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('users.signin_onboarding.retry_avatar', { user_id: member.id }));
    await waitFor(() => expect(mockWsClient.call.mock.calls.filter(([m]) => m === 'users.list')).toHaveLength(2));
  });
  it('shows retry refusals from the server', async () => {
    waiting('failed'); failure = { method: 'users.signin_onboarding.retry_avatar', error: { code: 'signin_avatar_not_waiting', message: '目前沒有等待建立的分身。' } };
    renderWithProviders(<UsersPage />);
    await userEvent.click(await screen.findByRole('button', { name: 'Retry' }));
    expect(await screen.findByText('目前沒有等待建立的分身。')).toBeInTheDocument();
  });
  it('does not offer retry without permission', async () => {
    waiting('failed', false); renderWithProviders(<UsersPage />);
    await screen.findByText(/AI avatar waiting/);
    expect(screen.queryByRole('button', { name: 'Retry' })).not.toBeInTheDocument();
  });
  it('opens the existing create dialog in protected mode, even without retry permission', async () => {
    waiting('protected_mode', false); renderWithProviders(<UsersPage />);
    await userEvent.click(await screen.findByRole('button', { name: 'Create an avatar for this member' }));
    expect(await screen.findByRole('dialog')).toBeInTheDocument();
    expect(screen.getByRole('heading', { name: 'Create AI avatar' })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Retry' })).not.toBeInTheDocument();
  });
  it.each(['unlink', 'revoke'] as const)('confirms before %s and refreshes', async action => {
    roster = [{ ...member, google_signin_linked: true }]; credentials = [credential];
    const label = action === 'unlink' ? 'Unlink Google sign-in' : 'Revoke Odoo account';
    const method = action === 'unlink' ? 'users.google_signin.unlink' : 'odoo.member_credentials.revoke';
    const { container } = renderWithProviders(<UsersPage />);
    await screen.findByText('Odoo connected');
    expect(container.innerHTML).not.toContain(member.id);
    await userEvent.click(screen.getByRole('button', { name: 'More' }));
    await userEvent.click(await screen.findByRole('menuitem', { name: label }));
    const dialog = await screen.findByRole('dialog');
    expect(mockWsClient.call.mock.calls.some(([m]) => m === method)).toBe(false);
    expect(within(dialog).getByText(action === 'unlink' ? /All signed-in devices/ : /can reconnect later/)).toBeInTheDocument();
    await userEvent.click(within(dialog).getByRole('button', { name: label }));
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith(method, { user_id: member.id }));
    await waitFor(() => expect(mockWsClient.call.mock.calls.filter(([m]) => m === 'users.list')).toHaveLength(2));
  });
  it('hides Odoo badges and actions when credentials cannot be loaded', async () => {
    credentials = [credential]; failure = { method: 'odoo.member_credentials.list', error: { code: 'denied', message: '拒絕存取。' } };
    renderWithProviders(<UsersPage />);
    await userEvent.click(await screen.findByRole('button', { name: 'More' }));
    expect(screen.queryByText('Odoo connected')).not.toBeInTheDocument();
    expect(screen.queryByRole('menuitem', { name: 'Revoke Odoo account' })).not.toBeInTheDocument();
  });
});

describe('batch Google sign-in', () => {
  it('disables linked and inactive rows with explanations', async () => {
    roster = [member, { ...member, id: 'linked', display_name: 'Linked', google_signin_linked: true }, { ...member, id: 'inactive', display_name: 'Inactive', status: 'suspended' }];
    renderWithProviders(<UsersPage />); await screen.findByText('Member');
    expect(screen.getByRole('checkbox', { name: 'Select Linked' })).toBeDisabled();
    expect(screen.getByRole('checkbox', { name: 'Select Linked' })).toHaveAttribute('title', 'Google sign-in is already linked.');
    expect(screen.getByRole('checkbox', { name: 'Select Inactive' })).toBeDisabled();
    expect(screen.getByRole('checkbox', { name: 'Select Inactive' })).toHaveAttribute('title', 'Only active members can be selected.');
    await chooseMember(); expect(screen.getByText('Selected 1 members')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Clear selection' }));
    expect(screen.queryByText('Selected 1 members')).not.toBeInTheDocument();
  });
  it('blocks the 101st selection', async () => {
    roster = Array.from({ length: 101 }, (_, i) => ({ ...member, id: `private-${i}`, display_name: `Person ${i}` }));
    renderWithProviders(<UsersPage />); await screen.findByText('Person 100');
    const boxes = screen.getAllByRole('checkbox', { name: /^Select Person / });
    for (const box of boxes) fireEvent.click(box);
    expect(screen.getByText('Selected 100 members')).toBeInTheDocument();
    expect(screen.getByText('You can confirm up to 100 members at a time.')).toHaveAttribute('role', 'alert');
    expect(boxes[100]).not.toBeChecked();
  }, 20000);
  it('requires risk acknowledgement, then step-up, then sends the exact token and selection', async () => {
    renderWithProviders(<UsersPage />); await confirmRisk();
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('users.google_signin.attest', { user_ids: [member.id], step_up_csrf: token }));
    expect(stepUp.ensure).toHaveBeenCalledWith('Confirm Google sign-in in bulk');
    expect(stepUp.ensure.mock.invocationCallOrder[0]).toBeLessThan(mockWsClient.call.mock.invocationCallOrder[mockWsClient.call.mock.calls.findIndex(([m]) => m === 'users.google_signin.attest')]);
    await waitFor(() => expect(screen.queryByText('Selected 1 members')).not.toBeInTheDocument());
  });
  it('does not attest when step-up is cancelled', async () => {
    stepUp.ensure.mockResolvedValue(null); renderWithProviders(<UsersPage />); await confirmRisk();
    expect(mockWsClient.call.mock.calls.some(([m]) => m === 'users.google_signin.attest')).toBe(false);
    expect(screen.getByText('Selected 1 members')).toBeInTheDocument();
  });
  it('invalidates an expired token and shows the refusal', async () => {
    failure = { method: 'users.google_signin.attest', error: { code: 'signin_step_up_required', message: '請重新驗證身分。' } };
    renderWithProviders(<UsersPage />); await confirmRisk();
    expect(await screen.findByText('請重新驗證身分。')).toBeInTheDocument();
    expect(stepUp.invalidate).toHaveBeenCalledOnce();
    expect(screen.getByText('Selected 1 members')).toBeInTheDocument();
  });
  it('does not show selection to a non-admin', async () => {
    useAuthStore.setState({ user: { id: member.id, role: 'employee' } as never });
    renderWithProviders(<UsersPage />); await screen.findByText('Member');
    expect(screen.queryByRole('checkbox')).not.toBeInTheDocument();
  });
  it('hides selection and the action bar in the org view', async () => {
    renderWithProviders(<UsersPage />); await chooseMember();
    await userEvent.click(screen.getByRole('radio', { name: 'Org chart' }));
    expect(screen.queryByRole('checkbox')).not.toBeInTheDocument();
    expect(screen.queryByText('Selected 1 members')).not.toBeInTheDocument();
  });
});

it('shows the no-password hint and the server role refusal under the role selector', async () => {
  failure = { method: 'users.update', error: { code: 'role_requires_local_password', message: '請先設定密碼再變更角色。' } };
  renderWithProviders(<EditUserDialog user={{ ...member, has_password: false }} users={[member]} onClose={vi.fn()} onUpdated={vi.fn()} />);
  const hint = screen.getByText('This account has no password and cannot be made an administrator or manager.');
  expect(within(hint.parentElement!).getByRole('combobox')).toBeEnabled();
  await userEvent.click(within(hint.parentElement!).getByRole('combobox'));
  await userEvent.click(await screen.findByRole('option', { name: 'Admin (manages everything)' }));
  await userEvent.click(screen.getByRole('button', { name: 'Save' }));
  const refusal = await screen.findByText('請先設定密碼再變更角色。');
  expect(refusal.parentElement).toBe(hint.parentElement);
  expect(mockWsClient.call).toHaveBeenCalledWith('users.update', expect.objectContaining({ user_id: member.id, role: 'admin' }));
});
