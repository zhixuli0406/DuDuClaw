import { beforeEach, expect, it, vi } from 'vitest';
import { fireEvent, screen, waitFor } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { useAuthStore } from '@/stores/auth-store';
import { api, type PersonalGoogleStatus } from '@/lib/api';
import { ConnectionsChecklist } from './ConnectionsChecklist';

const status = (connected: boolean): PersonalGoogleStatus => ({
  connection: connected ? { owner_id: 'person', account_email: 'person@example.com', granted_scopes: [], status: 'connected', connected_at: '' } : null,
  login_email: 'person@example.com', client_configured: true, integration_enabled: true, requires_login: false, requests: [], delegated: [], recent_usage: [],
});
beforeEach(() => {
  vi.restoreAllMocks(); localStorage.clear();
  useAuthStore.setState({ user: { id: 'person', role: 'employee', email: 'person@example.com', display_name: 'Person' } as never });
});
it('uses the personal connections API and links missing items to the connections page', async () => {
  const load = vi.spyOn(api.personalGoogle, 'status').mockResolvedValue(status(false));
  renderWithProviders(<ConnectionsChecklist enabled />);
  expect(await screen.findByRole('link', { name: 'Connect your Google account' })).toHaveAttribute('href', '/me/connections');
  expect(load).toHaveBeenCalledOnce();
  fireEvent.click(screen.getByRole('button', { name: 'Dismiss' }));
  expect(screen.queryByText('A few steps left')).toBeNull();
  expect(localStorage.getItem('duduclaw.connectionsChecklist.dismissed.person')).toBe('1');
});
it('stays hidden when all items are connected', async () => {
  const load = vi.spyOn(api.personalGoogle, 'status').mockResolvedValue(status(true));
  renderWithProviders(<ConnectionsChecklist enabled />);
  await waitFor(() => expect(load).toHaveBeenCalledOnce());
  expect(screen.queryByText('A few steps left')).toBeNull();
});
it('stays hidden on load failure and for administrators', async () => {
  const load = vi.spyOn(api.personalGoogle, 'status').mockRejectedValue(new Error('private server error'));
  const view = renderWithProviders(<ConnectionsChecklist enabled />);
  await waitFor(() => expect(load).toHaveBeenCalledOnce());
  expect(screen.queryByText('A few steps left')).toBeNull();
  view.unmount(); load.mockClear(); useAuthStore.setState({ user: { id: 'admin', role: 'admin' } as never });
  renderWithProviders(<ConnectionsChecklist enabled />);
  expect(load).not.toHaveBeenCalled();
});
it('continues to work when localStorage is unavailable', async () => {
  vi.spyOn(Storage.prototype, 'getItem').mockImplementation(() => { throw new Error('blocked'); });
  vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => { throw new Error('blocked'); });
  vi.spyOn(api.personalGoogle, 'status').mockResolvedValue(status(false));
  renderWithProviders(<ConnectionsChecklist enabled />);
  fireEvent.click(await screen.findByRole('button', { name: 'Dismiss' }));
  expect(screen.queryByText('A few steps left')).toBeNull();
});
