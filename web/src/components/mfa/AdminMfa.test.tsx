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

it('hides administration on Personal and from non-Admins', () => {
  useAuthStore.setState({ user: { id: 'admin', role: 'admin' } as never });
  useSystemStore.setState({ status: { edition_profile: 'personal' } as never });
  const { rerender } = renderWithProviders(<AdminMfa />);
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
