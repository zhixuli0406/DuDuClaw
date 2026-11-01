import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { act, fireEvent, screen, waitFor } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { Routes, Route } from 'react-router';
import { AuthGuard } from '@/components/AuthGuard';
import { useConnectionStore } from '@/stores/connection-store';
import { client } from '@/lib/ws-client';
import { AccountMfa } from './AccountMfa';
import { mfaApi } from '@/lib/mfa-api';
import { useAuthStore } from '@/stores/auth-store';
import { useSystemStore } from '@/stores/system-store';

const originalAuth = useAuthStore.getState();
const tokens = { access_token: 'new-access', refresh_token: 'new-refresh' };
beforeEach(() => {
  useAuthStore.setState({ ...originalAuth, isAuthenticated: true }, true);
  sessionStorage.clear();
  vi.restoreAllMocks();
  useSystemStore.setState({ status: { edition_profile: 'enterprise' } as never });
});
afterEach(() => { originalAuth.logout(); });
it('hides MFA on unenrolled Personal but displays an existing enrollment', async () => {
  useSystemStore.setState({ status: { edition_profile: 'personal' } as never });
  const status = vi.spyOn(mfaApi, 'status').mockResolvedValue({ enabled: false, enforced: false });
  const first = renderWithProviders(<AccountMfa />);
  await waitFor(() => expect(status).toHaveBeenCalled());
  expect(screen.queryByText('Two-factor authentication')).toBeNull();
  first.unmount(); status.mockResolvedValue({ enabled: true, enforced: false });
  renderWithProviders(<AccountMfa />);
  expect(await screen.findByText('Enrolled')).toBeInTheDocument();
});
it('offers password enrollment for an unenrolled Enterprise account', async () => {
  vi.spyOn(mfaApi, 'status').mockResolvedValue({ enabled: false, enforced: false });
  renderWithProviders(<AccountMfa />);
  fireEvent.click(await screen.findByRole('button', { name: 'Set up two-factor login' }));
  expect(screen.getByLabelText('Current password')).toBeInTheDocument();
});
it('regenerates with a fresh factor and requires saving without ending the session', async () => {
  vi.spyOn(mfaApi, 'status').mockResolvedValue({ enabled: true, enforced: true });
  const regenerate = vi.spyOn(mfaApi, 'regenerate').mockResolvedValue({ recovery_codes: ['new-code'] });
  const logout = vi.fn(); useAuthStore.setState({ logout });
  renderWithProviders(<AccountMfa />);
  expect(await screen.findByRole('button', { name: 'Disable two-factor login' })).toBeDisabled();
  fireEvent.change(screen.getByLabelText('Fresh authenticator code or unused recovery code'), { target: { value: '123456' } });
  fireEvent.click(screen.getByRole('button', { name: 'Replace recovery codes' }));
  await screen.findByLabelText('Recovery codes');
  expect(regenerate).toHaveBeenCalledWith('123456'); expect(logout).not.toHaveBeenCalled();
  fireEvent.click(screen.getByLabelText('I saved my recovery codes'));
  fireEvent.click(screen.getByRole('button', { name: 'Continue' }));
  await waitFor(() => expect(screen.queryByLabelText('Recovery codes')).toBeNull());
  expect(logout).not.toHaveBeenCalled();
});
it('requires the current password and fresh factor when enforcement is off', async () => {
  vi.spyOn(mfaApi, 'status').mockResolvedValue({ enabled: true, enforced: false });
  const disable = vi.spyOn(mfaApi, 'disable').mockResolvedValue({ enabled: false, ...tokens });
  const logout = vi.fn(); useAuthStore.setState({ logout });
  renderWithProviders(<AccountMfa />);
  await screen.findByText('Enrolled');
  fireEvent.change(screen.getByLabelText('Fresh authenticator code or unused recovery code'), { target: { value: 'fresh-code' } });
  expect(screen.getByRole('button', { name: 'Disable two-factor login' })).toBeDisabled();
  fireEvent.change(screen.getByLabelText('Current password'), { target: { value: 'current-password' } });
  fireEvent.click(screen.getByRole('button', { name: 'Disable two-factor login' }));
  await waitFor(() => expect(disable).toHaveBeenCalledWith('fresh-code', 'current-password'));
  expect(logout).not.toHaveBeenCalled();
  expect(await screen.findByText('Not enrolled')).toBeInTheDocument();
  expect(useAuthStore.getState().jwt).toBe(tokens.access_token);
  expect(sessionStorage.getItem('duduclaw-refresh-token')).toBe(tokens.refresh_token);
});


it('replaces session tokens while AuthGuard keeps recovery codes mounted until saved', async () => {
  vi.spyOn(mfaApi, 'status').mockResolvedValue({ enabled: false, enforced: false });
  vi.spyOn(mfaApi, 'begin').mockResolvedValue({ enrollment_token: 'enroll', secret: 'SECRET' });
  vi.spyOn(mfaApi, 'confirm').mockResolvedValue({ ...tokens, recovery_codes: ['recovery-one', 'recovery-two'] });
  useAuthStore.setState({ isAuthenticated: true, initialized: true, jwt: 'old-access', refreshToken: 'old-refresh' });
  useConnectionStore.setState({ state: 'authenticated', everAuthenticated: true, mustChangePassword: false });
  const disconnect = vi.spyOn(client, 'disconnect');
  const fetcher = vi.spyOn(globalThis, 'fetch');
  renderWithProviders(<Routes><Route element={<AuthGuard />}><Route path="/" element={<AccountMfa />} /></Route></Routes>);
  fireEvent.click(await screen.findByRole('button', { name: 'Set up two-factor login' }));
  fireEvent.change(screen.getByLabelText('Current password'), { target: { value: 'password' } });
  fireEvent.click(screen.getByRole('button', { name: 'Continue setup' }));
  await screen.findByText('SECRET');
  fireEvent.change(screen.getByLabelText('Authenticator code'), { target: { value: '123456' } });
  fireEvent.click(screen.getByRole('button', { name: 'Confirm' }));
  const codes = await screen.findByLabelText('Recovery codes');
  expect(codes).toHaveTextContent('recovery-one');
  expect(useAuthStore.getState()).toMatchObject({ jwt: tokens.access_token, refreshToken: tokens.refresh_token, isAuthenticated: true });
  expect(sessionStorage.getItem('duduclaw-refresh-token')).toBe(tokens.refresh_token);
  expect(JSON.stringify(useAuthStore.getState())).not.toContain('recovery-one');
  expect(disconnect).not.toHaveBeenCalled();
  expect(fetcher).not.toHaveBeenCalled();
  // Deliver another authenticated-state notification after token handover.
  // Token replacement must not trigger teardown, reconnect or acknowledgement.
  await act(async () => { useConnectionStore.setState({ state: 'authenticated' }); });
  expect(screen.getByLabelText('Recovery codes')).toBe(codes);
  expect(screen.getByRole('button', { name: 'Continue' })).toBeDisabled();
  fireEvent.click(screen.getByLabelText('I saved my recovery codes'));
  fireEvent.click(screen.getByRole('button', { name: 'Continue' }));
  expect(await screen.findByText('Enrolled')).toBeInTheDocument();
  expect(screen.queryByLabelText('Recovery codes')).toBeNull();
  expect(useAuthStore.getState().isAuthenticated).toBe(true);
  expect(disconnect).not.toHaveBeenCalled();
});
