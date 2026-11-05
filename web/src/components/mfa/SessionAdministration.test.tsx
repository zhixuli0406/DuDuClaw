import { beforeEach, expect, it, vi } from 'vitest';
import { fireEvent, screen, waitFor, within } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { mockWsClient } from '@/test/mocks';
import { useAuthStore } from '@/stores/auth-store';
import { sessionAdminApi } from '@/lib/session-admin-api';
import { RememberedSessionPolicy } from './RememberedSessionPolicy';
import { RevokeUserSessions } from './RevokeUserSessions';
import { EditUserDialog } from '@/pages/users/UserFormDialogs';
import type { UserDetail } from '@/lib/api';

beforeEach(() => {
  vi.restoreAllMocks(); vi.clearAllMocks();
  useAuthStore.setState({ user: { id: 'admin', role: 'admin' } as never });
  mockWsClient.call.mockImplementation(async (method: string) => method === 'sessions.policy' ? { days: 180 } : null);
});

it('loads persisted days, rejects invalid values and saves an integer policy', async () => {
  const update = vi.spyOn(sessionAdminApi, 'updatePolicy').mockResolvedValue({ days: 30 });
  renderWithProviders(<RememberedSessionPolicy />);
  const input = screen.getByRole('spinbutton', { name: 'Keep signed in for (days)' });
  await waitFor(() => expect(input).toHaveValue(180));
  expect(mockWsClient.call).toHaveBeenCalledWith('sessions.policy');
  for (const value of ['', '0', '366', '1.5']) {
    fireEvent.change(input, { target: { value } });
    expect(screen.getByRole('button', { name: 'Save' })).toBeDisabled();
  }
  for (const value of ['1', '365']) {
    fireEvent.change(input, { target: { value } });
    expect(screen.getByRole('button', { name: 'Save' })).toBeEnabled();
  }
  fireEvent.change(input, { target: { value: '30' } });
  fireEvent.click(screen.getByRole('button', { name: 'Save' }));
  await waitFor(() => expect(update).toHaveBeenCalledWith(30));
  expect(await screen.findByRole('status')).toHaveTextContent('Sign-in duration saved.');
});

it('keeps policy editing disabled after a failed read and allows retry', async () => {
  vi.spyOn(sessionAdminApi, 'policy').mockRejectedValueOnce(new Error('secret transport details')).mockResolvedValueOnce({ days: 90 });
  renderWithProviders(<RememberedSessionPolicy />);
  expect(await screen.findByRole('alert')).not.toHaveTextContent('secret transport details');
  expect(screen.getByRole('spinbutton')).toBeDisabled();
  fireEvent.click(screen.getByRole('button', { name: 'Refresh status' }));
  await waitFor(() => expect(screen.getByRole('spinbutton')).toHaveValue(90));
});

it('reports a save failure without claiming the policy changed', async () => {
  vi.spyOn(sessionAdminApi, 'updatePolicy').mockRejectedValue(new Error('denied'));
  renderWithProviders(<RememberedSessionPolicy />);
  await waitFor(() => expect(screen.getByRole('spinbutton')).toHaveValue(180));
  fireEvent.change(screen.getByRole('spinbutton'), { target: { value: '7' } });
  fireEvent.click(screen.getByRole('button', { name: 'Save' }));
  expect(await screen.findByRole('alert')).toBeInTheDocument();
  expect(screen.queryByRole('status')).toBeNull();
  expect(screen.getByRole('button', { name: 'Save' })).toBeEnabled();
});

it('hides both administration controls from non-Admins without loading policy', () => {
  useAuthStore.setState({ user: { id: 'employee', role: 'employee' } as never });
  renderWithProviders(<><RememberedSessionPolicy /><RevokeUserSessions userId="other" name="Other" /></>);
  expect(screen.queryByRole('button')).toBeNull();
  expect(mockWsClient.call).not.toHaveBeenCalled();
});

it('requires confirmation, supports cancellation, and revokes only the selected user', async () => {
  const revoke = vi.spyOn(sessionAdminApi, 'revokeUser').mockResolvedValue({ revoked: true });
  renderWithProviders(<RevokeUserSessions userId="other" name="Other Person" />);
  fireEvent.click(screen.getByRole('button', { name: 'Logout from all devices' }));
  expect(screen.getByRole('dialog')).toHaveTextContent('Other Person');
  expect(revoke).not.toHaveBeenCalled();
  fireEvent.click(within(screen.getByRole('dialog')).getByRole('button', { name: 'Cancel' }));
  expect(revoke).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole('button', { name: 'Logout from all devices' }));
  fireEvent.click(within(screen.getByRole('dialog')).getByRole('button', { name: 'Confirm' }));
  expect(await screen.findByRole('status')).toHaveTextContent('All devices have been signed out.');
  expect(revoke).toHaveBeenCalledExactlyOnceWith('other');
});

it('keeps a failed revocation in the confirmation dialog without reporting success', async () => {
  vi.spyOn(sessionAdminApi, 'revokeUser').mockRejectedValue(new Error('private details'));
  renderWithProviders(<RevokeUserSessions userId="other" name="Other" />);
  fireEvent.click(screen.getByRole('button', { name: 'Logout from all devices' }));
  fireEvent.click(within(screen.getByRole('dialog')).getByRole('button', { name: 'Confirm' }));
  expect(await screen.findByRole('alert')).not.toHaveTextContent('private details');
  expect(screen.getByRole('dialog')).toBeInTheDocument();
  expect(screen.queryByRole('status')).toBeNull();
});

it('uses the explicitly named administration RPCs', async () => {
  await sessionAdminApi.updatePolicy(365);
  await sessionAdminApi.revokeUser('other');
  expect(mockWsClient.call).toHaveBeenCalledWith('sessions.policy_update', { days: 365 });
  expect(mockWsClient.call).toHaveBeenCalledWith('users.revoke_sessions', { user_id: 'other' });
});

it('offers confirmed logout inside the selected user detail dialog', async () => {
  mockWsClient.call.mockResolvedValue({ departments: [] });
  const revoke = vi.spyOn(sessionAdminApi, 'revokeUser').mockResolvedValue({ revoked: true });
  const user = { id: 'other', email: 'other@example.com', display_name: 'Other Person', role: 'employee', status: 'active', agent_bindings: [] } as unknown as UserDetail;
  renderWithProviders(<EditUserDialog user={user} users={[user]} onClose={vi.fn()} onUpdated={vi.fn()} />);
  fireEvent.click(screen.getByRole('button', { name: 'Logout from all devices' }));
  fireEvent.click(within(screen.getByRole('dialog', { name: 'Logout from all devices' })).getByRole('button', { name: 'Confirm' }));
  await waitFor(() => expect(revoke).toHaveBeenCalledExactlyOnceWith('other'));
});

it('clears the current browser login when an Admin revokes their own devices', async () => {
  const revoke = vi.spyOn(sessionAdminApi, 'revokeUser');
  const fetcher = vi.spyOn(globalThis, 'fetch').mockResolvedValue(new Response(JSON.stringify({ ok: true }), { status: 200 }));
  const logout = vi.fn();
  const originalLogout = useAuthStore.getState().logout;
  const originalJwt = useAuthStore.getState().jwt;
  useAuthStore.setState({ logout, jwt: 'current-access' });
  try {
    renderWithProviders(<RevokeUserSessions userId="admin" name="Admin" />);
    fireEvent.click(screen.getByRole('button', { name: 'Logout from all devices' }));
    fireEvent.click(within(screen.getByRole('dialog')).getByRole('button', { name: 'Confirm' }));
    await waitFor(() => expect(logout).toHaveBeenCalledOnce());
    expect(fetcher).toHaveBeenCalledExactlyOnceWith('/api/session/revoke', expect.objectContaining({
      method: 'POST', body: JSON.stringify({ all: true }),
      headers: expect.objectContaining({ Authorization: 'Bearer current-access' }),
    }));
    expect(revoke).not.toHaveBeenCalled();
  } finally { useAuthStore.setState({ logout: originalLogout, jwt: originalJwt }); }
});

it.each(['server', 'transport', 'response'] as const)('retains local login when self-revocation fails at the %s', async failure => {
  const revoke = vi.spyOn(sessionAdminApi, 'revokeUser');
  const fetcher = vi.spyOn(globalThis, 'fetch');
  if (failure === 'server') fetcher.mockResolvedValue(new Response('{}', { status: 500 }));
  else if (failure === 'response') fetcher.mockResolvedValue(new Response(JSON.stringify({ revoked: true }), { status: 200 }));
  else fetcher.mockRejectedValue(new Error('connection lost'));
  const logout = vi.fn();
  const originalLogout = useAuthStore.getState().logout;
  const originalJwt = useAuthStore.getState().jwt;
  useAuthStore.setState({ logout, jwt: 'current-access' });
  try {
    renderWithProviders(<RevokeUserSessions userId="admin" name="Admin" />);
    fireEvent.click(screen.getByRole('button', { name: 'Logout from all devices' }));
    fireEvent.click(within(screen.getByRole('dialog')).getByRole('button', { name: 'Confirm' }));
    await screen.findByRole('alert');
    expect(screen.getByRole('dialog')).toBeInTheDocument();
    expect(screen.queryByRole('status')).toBeNull();
    expect(logout).not.toHaveBeenCalled();
    expect(revoke).not.toHaveBeenCalled();
    expect(useAuthStore.getState().jwt).toBe('current-access');
  } finally { useAuthStore.setState({ logout: originalLogout, jwt: originalJwt }); }
});
