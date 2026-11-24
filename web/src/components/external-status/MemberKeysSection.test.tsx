import { beforeEach, expect, it, vi } from 'vitest';
import { screen, within, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { useAuthStore } from '@/stores/auth-store';
import { MemberKeysSection } from './MemberKeysSection';

const secret = 'ddc_member_test-secret-not-a-real-key';
const key = { id: 'k1', name: 'External server', created_at: '2026-11-25T00:00:00Z', expires_at: '2027-02-23T00:00:00Z', last_used_at: null, revoked_at: null, active: true };
beforeEach(() => {
  vi.clearAllMocks();
  useAuthStore.setState({ user: { id: 'member', role: 'employee' } as never });
  mockWsClient.call.mockImplementation(async (method: string) => {
    if (method === 'me.member_keys.list' || method === 'member_keys.admin_list') return { keys: [key], max_active: 5 };
    if (method === 'me.member_keys.create') return { key, secret };
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
  await user.click(within(dialog).getByRole('button', { name: 'Create key' }));
  expect(await screen.findByDisplayValue(secret)).toBeInTheDocument();
  expect(mockWsClient.call).toHaveBeenCalledWith('me.member_keys.create', { name: 'External server', expires_in_days: 90 });
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
  expect(mockWsClient.call).toHaveBeenCalledWith('member_keys.admin_list');
});

it('does not show creation or retained keys when the master switch is off', async () => {
  mockWsClient.call.mockRejectedValue(Object.assign(new Error('disabled'), { code: 'member_keys_disabled' }));
  renderWithProviders(<MemberKeysSection />);
  await screen.findByText('Member keys are disabled. Key use and management are unavailable.');
  expect(screen.queryByRole('button', { name: 'Create key' })).not.toBeInTheDocument();
  expect(screen.queryByText('External server')).not.toBeInTheDocument();
});
