import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { fireEvent, screen, waitFor } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { useAuthStore } from '@/stores/auth-store';
import { SignedInDevices } from './SignedInDevices';

const original = useAuthStore.getState();
beforeEach(() => useAuthStore.setState({ jwt: 'access', logout: vi.fn() }));
afterEach(() => { vi.restoreAllMocks(); useAuthStore.setState(original); });
const device = { id: 'family', label: 'My browser', created_at: 1700000000, last_used_at: 1700000100, expires_at: 1800000000, current: true };
it('shows device metadata and revokes the current device before clearing local auth', async () => {
  const fetcher = vi.spyOn(globalThis, 'fetch')
    .mockResolvedValueOnce(new Response(JSON.stringify({ devices: [device] }), { status: 200 }))
    .mockResolvedValueOnce(new Response(JSON.stringify({ revoked: true }), { status: 200 }));
  const logout = useAuthStore.getState().logout;
  renderWithProviders(<SignedInDevices />);
  expect(await screen.findByText(/My browser/)).toBeInTheDocument();
  fireEvent.click(screen.getByRole('button', { name: 'Sign out this device' }));
  await waitFor(() => expect(logout).toHaveBeenCalledOnce());
  expect(fetcher).toHaveBeenLastCalledWith('/api/session/revoke', expect.objectContaining({ method: 'POST', body: JSON.stringify({ id: 'family' }) }));
});
it('keeps the current session when server revocation fails', async () => {
  vi.spyOn(globalThis, 'fetch')
    .mockResolvedValueOnce(new Response(JSON.stringify({ devices: [device] }), { status: 200 }))
    .mockResolvedValueOnce(new Response('{}', { status: 500 }));
  const logout = useAuthStore.getState().logout;
  renderWithProviders(<SignedInDevices />);
  await screen.findByText(/My browser/);
  fireEvent.click(screen.getByRole('button', { name: 'Sign out everywhere' }));
  await screen.findByRole('alert');
  expect(logout).not.toHaveBeenCalled();
});
