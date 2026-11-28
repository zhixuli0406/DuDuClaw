import { beforeEach, expect, it, vi } from 'vitest';
import { screen, within, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { renderWithProviders } from '@/test/render';
import { mockWsClient } from '@/test/mocks';
import { useAuthStore } from '@/stores/auth-store';
import { UsersPage } from '../UsersPage';
import { formatDateTime } from '@/lib/company-time';

const request = { id: 'private-request', display_name: 'Mei', email: 'mei@example.com', hd: 'example.com', requested_at: 1795737600, expires_at: 1796947200 };
let requests = [request];
let mfa = true;
let refusal: unknown;
let listError: unknown;
beforeEach(() => {
  vi.clearAllMocks();
  requests = [request]; mfa = true; refusal = null; listError = null;
  useAuthStore.setState({ user: { id: 'admin', role: 'admin' } as never });
  mockWsClient.call.mockImplementation(async (method: string) => {
    if (method === 'users.signin_requests.list') { if (listError) throw listError; return { requests }; }
    if (method === 'google.signin.get') return { settings: { approval_grants_mfa_enrollment: mfa } };
    if (method === 'users.signin_requests.approve' || method === 'users.signin_requests.reject') {
      if (refusal) throw refusal;
      requests = []; return { user_id: 'private-user', rejected: true };
    }
    return { users: [], agents: [], departments: [], identities: [], conflicts: [] };
  });
});
async function openPanel() {
  const user = userEvent.setup();
  renderWithProviders(<UsersPage />);
  await user.click(await screen.findByRole('radio', { name: 'Pending activation 1' }));
  return user;
}
it('shows the count with no members and renders request details without ids', async () => {
  await openPanel();
  expect(screen.getByRole('cell', { name: 'Mei' })).toBeInTheDocument();
  expect(screen.getByText(request.email)).toBeInTheDocument();
  expect(screen.getByText(request.hd)).toBeInTheDocument();
  expect(screen.getByText(formatDateTime(request.requested_at * 1000, { locale: 'en' }))).toBeInTheDocument();
  expect(screen.queryByText(request.id)).not.toBeInTheDocument();
});
it.each([true, false])('approves with MFA selected=%s and refreshes both lists', async (checked) => {
  const user = await openPanel();
  await user.click(screen.getByRole('button', { name: 'Approve' }));
  const dialog = screen.getByRole('dialog', { name: 'Approve Mei’s activation request?' });
  const checkbox = within(dialog).getByRole('checkbox');
  expect(checkbox).not.toBeChecked();
  if (checked) await user.click(checkbox);
  await user.click(within(dialog).getByRole('button', { name: 'Approve' }));
  await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('users.signin_requests.approve', { id: request.id, ...(checked ? { mfa_preauthorize: true } : {}) }));
  expect(await screen.findByText('There are no pending activation requests')).toBeInTheDocument();
  expect(mockWsClient.call.mock.calls.filter(([method]) => method === 'users.list')).toHaveLength(2);
  expect(screen.getByRole('radio', { name: 'Pending activation 0' })).toBeInTheDocument();
});
it('hides MFA when disabled and omits the parameter', async () => {
  mfa = false;
  const user = await openPanel();
  await user.click(screen.getByRole('button', { name: 'Approve' }));
  const dialog = screen.getByRole('dialog');
  expect(within(dialog).queryByRole('checkbox')).not.toBeInTheDocument();
  await user.click(within(dialog).getByRole('button', { name: 'Approve' }));
  await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('users.signin_requests.approve', { id: request.id }));
});
it('rejects only after confirmation with a seven day explanation', async () => {
  const user = await openPanel();
  await user.click(screen.getByRole('button', { name: 'Reject' }));
  expect(mockWsClient.call).not.toHaveBeenCalledWith('users.signin_requests.reject', expect.anything());
  const dialog = screen.getByRole('dialog');
  expect(within(dialog).getByText(/7 days/)).toBeInTheDocument();
  await user.click(within(dialog).getByRole('button', { name: 'Reject' }));
  expect(await screen.findByText('There are no pending activation requests')).toBeInTheDocument();
  expect(mockWsClient.call).toHaveBeenCalledWith('users.signin_requests.reject', { id: request.id });
  expect(mockWsClient.call.mock.calls.filter(([method]) => method === 'users.list')).toHaveLength(2);
});
it.each(['signin_admin_required', 'signin_personal_edition'])('hides the segment on %s', async code => {
  listError = { code, message: '無法使用' };
  renderWithProviders(<UsersPage />);
  await screen.findByText('User Management', { selector: 'p' });
  expect(screen.queryByRole('radio', { name: /Pending activation/ })).not.toBeInTheDocument();
});
it('shows the server refusal and leaves the request available', async () => {
  refusal = { code: 'signin_preauthorize_disabled', message: '目前不允許預先授權兩步驟驗證。' };
  const user = await openPanel();
  await user.click(screen.getByRole('button', { name: 'Approve' }));
  await user.click(within(screen.getByRole('dialog')).getByRole('checkbox'));
  await user.click(within(screen.getByRole('dialog')).getByRole('button', { name: 'Approve' }));
  expect(await screen.findByRole('alert')).toHaveTextContent('目前不允許預先授權兩步驟驗證。');
});


it('shows the initial empty state and its explanation', async () => {
  requests = [];
  const user = userEvent.setup();
  renderWithProviders(<UsersPage />);
  await user.click(await screen.findByRole('radio', { name: 'Pending activation 0' }));
  expect(screen.getByText('There are no pending activation requests')).toBeInTheDocument();
  expect(screen.getByText(/Requests appear here when a colleague/)).toBeInTheDocument();
});
it('shows a list refusal from the server', async () => {
  listError = { code: 'signin_storage_unavailable', message: '暫時無法讀取開通申請。' };
  renderWithProviders(<UsersPage />);
  expect(await screen.findByText('暫時無法讀取開通申請。')).toHaveAttribute('role', 'alert');
});
