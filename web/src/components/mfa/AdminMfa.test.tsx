import { beforeEach, expect, it, vi } from 'vitest';
import { fireEvent, screen, waitFor, within } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { AdminMfa, AdminMfaPanel } from './AdminMfa';
import { mfaApi, type MfaOverview } from '@/lib/mfa-api';
import { api } from '@/lib/api';
import { useSystemStore } from '@/stores/system-store';
import { useAuthStore } from '@/stores/auth-store';
import { mockWsClient } from '@/test/mocks';

const overview: MfaOverview = { enforced: false, policy: { degraded: false }, users: [
  { user_id: 'admin', email: 'admin@example.com', enabled: true, channel_bindings: [] },
  { user_id: 'other', email: 'other@example.com', enabled: true, channel_bindings: [{ channel: 'telegram', channel_user_id: '123', verified: true, verified_at: 1793400000000000 }] },
  { user_id: 'new', email: 'new@example.com', enabled: false, channel_bindings: [] },
] };
beforeEach(() => {
  vi.restoreAllMocks(); vi.clearAllMocks();
  useSystemStore.setState({ status: { edition_profile: 'enterprise' } as never });
  vi.spyOn(mfaApi, 'overview').mockResolvedValue(overview);
});

it('shows enrollment/binding times and refuses self-reset in both UI and request helper', async () => {
  renderWithProviders(<AdminMfaPanel actor="admin" />);
  const self = (await screen.findByText(/admin@example.com/)).closest('li')!;
  expect(within(self).getByRole('button', { name: 'Reset two-factor setup' })).toBeDisabled();
  expect(screen.getByText(/Channel verified at/)).toHaveTextContent('2026');
  await expect(mfaApi.reset('admin', 'admin', '123456')).rejects.toThrow('MFA_SELF_RESET');
  expect(mockWsClient.call).not.toHaveBeenCalledWith('users.mfa_reset', expect.anything());
  const reset = vi.spyOn(mfaApi, 'reset').mockResolvedValue({ reset: true });
  const other = screen.getByText(/other@example.com/).closest('li')!;
  fireEvent.click(within(other).getByRole('button', { name: 'Reset two-factor setup' }));
  expect(within(other).getByRole('button', { name: 'Confirm' })).toBeDisabled();
  fireEvent.change(within(other).getByLabelText('Fresh authenticator code or unused recovery code'), { target: { value: '654321' } });
  fireEvent.click(within(other).getByRole('button', { name: 'Confirm' }));
  await waitFor(() => expect(reset).toHaveBeenCalledWith('admin', 'other', '654321'));
});

it('requires typed confirmation and reports the accounts enforcement would block', async () => {
  const update = vi.spyOn(api.system, 'updateConfig').mockResolvedValue({} as never);
  renderWithProviders(<AdminMfaPanel actor="admin" />);
  expect(await screen.findByText(/enforcement would block: 1/)).toBeInTheDocument();
  fireEvent.click(screen.getByRole('switch'));
  const confirm = screen.getByRole('button', { name: 'Confirm' });
  expect(confirm).toBeDisabled();
  fireEvent.change(screen.getByRole('textbox'), { target: { value: 'CHANGE TWO-FACTOR POLICY' } });
  fireEvent.click(confirm);
  await waitFor(() => expect(update).toHaveBeenCalledWith({ 'auth.mfa_enforced': true, expected_state: false }));
});

it('shows system.status degraded state and blocks policy changes', async () => {
  useSystemStore.setState({ status: { edition_profile: 'enterprise', mfa_policy: { degraded: true } } as never });
  renderWithProviders(<AdminMfaPanel actor="admin" />);
  expect(await screen.findByText(/could not reload/)).toBeInTheDocument();
  expect(await screen.findByRole('switch')).toBeDisabled();
});

it('issues a code only against the selected user and challenge reference', async () => {
  const issue = vi.spyOn(mfaApi, 'issueCode').mockResolvedValue({ enrollment_code: 'once-only', expires_in: 300 });
  renderWithProviders(<AdminMfaPanel actor="admin" />);
  const row = (await screen.findByText(/new@example.com/)).closest('li')!;
  fireEvent.click(within(row).getByRole('button', { name: 'Issue enrollment code' }));
  fireEvent.change(within(row).getByLabelText('Enrollment reference'), { target: { value: 'abcdef123456' } });
  fireEvent.click(within(row).getByRole('button', { name: 'Confirm' }));
  expect(await screen.findByText('once-only')).toBeInTheDocument();
  expect(issue).toHaveBeenCalledWith('new', 'abcdef123456');
  fireEvent.click(within(row).getByRole('button', { name: 'Close' }));
  expect(screen.queryByText('once-only')).toBeNull();
});

it('hides administration on unenforced Personal and from non-Admins', async () => {
  useAuthStore.setState({ user: { id: 'admin', role: 'admin' } as never });
  useSystemStore.setState({ status: { edition_profile: 'personal' } as never });
  const { rerender } = renderWithProviders(<AdminMfa />);
  await waitFor(() => expect(mfaApi.overview).toHaveBeenCalled());
  expect(screen.queryByText('Two-factor login management')).toBeNull();
  useSystemStore.setState({ status: { edition_profile: 'enterprise' } as never });
  useAuthStore.setState({ user: { id: 'employee', role: 'employee' } as never });
  rerender(<AdminMfa />);
  expect(screen.queryByText('Two-factor login management')).toBeNull();
});

it('clears typed confirmation on refresh before the opposite policy action', async () => {
  const update = vi.spyOn(api.system, 'updateConfig').mockResolvedValue({} as never);
  renderWithProviders(<AdminMfaPanel actor="admin" />);
  fireEvent.click(await screen.findByRole('switch'));
  fireEvent.change(screen.getByRole('textbox'), { target: { value: 'CHANGE TWO-FACTOR POLICY' } });
  vi.mocked(mfaApi.overview).mockResolvedValue({ ...overview, enforced: true });
  fireEvent.click(screen.getByRole('button', { name: 'Refresh status' }));
  await waitFor(() => expect(screen.getByRole('switch')).toHaveAttribute('aria-checked', 'true'));
  expect(screen.queryByRole('textbox')).toBeNull();
  fireEvent.click(screen.getByRole('switch'));
  expect(screen.getByRole('textbox')).toHaveValue('');
  expect(screen.getByRole('button', { name: 'Confirm' })).toBeDisabled();
  expect(update).not.toHaveBeenCalled();
  fireEvent.change(screen.getByRole('textbox'), { target: { value: 'CHANGE TWO-FACTOR POLICY' } });
  fireEvent.click(screen.getByRole('button', { name: 'Confirm' }));
  await waitFor(() => expect(update).toHaveBeenCalledWith({ 'auth.mfa_enforced': false, expected_state: true }));
});

it('lets a Personal Admin deliberately disable the stored policy with confirmation', async () => {
  useAuthStore.setState({ user: { id: 'admin', role: 'admin' } as never });
  useSystemStore.setState({ status: { edition_profile: 'personal' } as never });
  vi.mocked(mfaApi.overview).mockResolvedValue({ ...overview, enforced: true });
  const update = vi.spyOn(api.system, 'updateConfig').mockResolvedValue({} as never);
  renderWithProviders(<AdminMfa />);
  fireEvent.click(await screen.findByRole('switch', { name: 'Stop requiring two-factor login' }));
  expect(screen.getByRole('button', { name: 'Confirm' })).toBeDisabled();
  fireEvent.change(screen.getByRole('textbox'), { target: { value: 'CHANGE TWO-FACTOR POLICY' } });
  fireEvent.click(screen.getByRole('button', { name: 'Confirm' }));
  await waitFor(() => expect(update).toHaveBeenCalledWith({ 'auth.mfa_enforced': false, expected_state: true }));
});

it('does not expose enforced Personal policy controls to non-Admins', () => {
  useAuthStore.setState({ user: { id: 'employee', role: 'employee' } as never });
  useSystemStore.setState({ status: { edition_profile: 'personal' } as never });
  vi.mocked(mfaApi.overview).mockResolvedValue({ ...overview, enforced: true });
  renderWithProviders(<AdminMfa />);
  expect(screen.queryByText('Two-factor login management')).toBeNull();
  expect(mfaApi.overview).not.toHaveBeenCalled();
});

it('lets an admin confirm a staff reset with their current password', async () => {
  await expect(mfaApi.resetPassword('admin', 'admin', 'secret')).rejects.toThrow('MFA_SELF_RESET');
  expect(mockWsClient.call).not.toHaveBeenCalledWith('users.mfa_reset', expect.anything());
  const reset = vi.spyOn(mfaApi, 'resetPassword').mockResolvedValue({ reset: true });
  renderWithProviders(<AdminMfaPanel actor="admin" />);
  const row = (await screen.findByText(/other@example.com/)).closest('li')!;
  fireEvent.click(within(row).getByRole('button', { name: 'Reset two-factor setup' }));
  fireEvent.change(within(row).getByLabelText('Confirm administrator identity with'), { target: { value: 'password' } });
  fireEvent.change(within(row).getByLabelText('Current password'), { target: { value: 'admin-password' } });
  fireEvent.click(within(row).getByRole('button', { name: 'Confirm' }));
  await waitFor(() => expect(reset).toHaveBeenCalledWith('admin', 'other', 'admin-password'));
});

const withAdmins: MfaOverview = { ...overview, users: overview.users.map(u => u.user_id === 'new' ? u : { ...u, admin: true }) };

it('offers no password-only confirmation when the target is an administrator', async () => {
  vi.mocked(mfaApi.overview).mockResolvedValue(withAdmins);
  renderWithProviders(<AdminMfaPanel actor="admin" />);
  const row = (await screen.findByText(/other@example.com/)).closest('li')!;
  fireEvent.click(within(row).getByRole('button', { name: 'Reset two-factor setup' }));
  const select = within(row).getByRole('combobox');
  expect(within(select).queryByRole('option', { name: 'Current password' })).toBeNull();
  expect(within(row).getByText(/not just your password/)).toBeInTheDocument();
});

it('lets an admin without an authenticator app reset an administrator with password and email code', async () => {
  vi.mocked(mfaApi.overview).mockResolvedValue(withAdmins);
  vi.spyOn(mfaApi, 'status').mockResolvedValue({ enabled: true, enforced: true, factors: { totp: false, email: false, passkey: true } } as never);
  const begin = vi.spyOn(mfaApi, 'resetEmailBegin').mockResolvedValue({ email_code_sent: true, reset_token: 'capability', expires_in: 300 });
  const confirm = vi.spyOn(mfaApi, 'resetEmailConfirm').mockResolvedValue({ reset: true });
  renderWithProviders(<AdminMfaPanel actor="admin" />);
  const row = (await screen.findByText(/other@example.com/)).closest('li')!;
  fireEvent.click(within(row).getByRole('button', { name: 'Reset two-factor setup' }));
  await within(row).findByRole('option', { name: 'Password and a code sent to your email' });
  fireEvent.change(within(row).getByRole('combobox'), { target: { value: 'email' } });
  fireEvent.change(within(row).getByLabelText('Current password'), { target: { value: 'admin-password' } });
  fireEvent.click(within(row).getByRole('button', { name: 'Send code' }));
  await waitFor(() => expect(begin).toHaveBeenCalledWith('admin', 'other', 'admin-password'));
  fireEvent.change(await within(row).findByLabelText('Code from your email'), { target: { value: '123456' } });
  fireEvent.click(within(row).getByRole('button', { name: 'Confirm' }));
  await waitFor(() => expect(confirm).toHaveBeenCalledWith('admin', 'other', 'capability', '123456'));
});

it('shows the server refusal for a recently promoted administrator', async () => {
  vi.mocked(mfaApi.overview).mockResolvedValue(withAdmins);
  vi.spyOn(mfaApi, 'reset').mockRejectedValue({ code: 'MFA_RESET_COOLING', message: '成為管理員未滿 7 天' });
  renderWithProviders(<AdminMfaPanel actor="admin" />);
  const row = (await screen.findByText(/other@example.com/)).closest('li')!;
  fireEvent.click(within(row).getByRole('button', { name: 'Reset two-factor setup' }));
  fireEvent.change(within(row).getByLabelText('Fresh authenticator code or unused recovery code'), { target: { value: '654321' } });
  fireEvent.click(within(row).getByRole('button', { name: 'Confirm' }));
  expect(await screen.findByText(/for the last 7 days in a row can reset another administrator/)).toBeInTheDocument();
});

it('warns when a reset notice reached nobody and advises a second administrator', async () => {
  vi.mocked(mfaApi.overview).mockResolvedValue({ ...overview,
    users: overview.users.map(u => u.user_id === 'admin' ? { ...u, admin: true } : u),
    reset_notice_failures: [{ user_id: 'other', at: '2026-11-14T08:00:00Z' }] });
  renderWithProviders(<AdminMfaPanel actor="admin" />);
  expect(await screen.findByText(/other@example.com was reset on .+ but the notice could not be delivered/)).toBeInTheDocument();
  expect(screen.getByText(/You are the only administrator/)).toBeInTheDocument();
});
