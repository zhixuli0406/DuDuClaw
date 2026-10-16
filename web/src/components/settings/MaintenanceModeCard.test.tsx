import { describe, it, expect, vi, beforeEach } from 'vitest';
import { act, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { renderWithProviders } from '@/test/render';
import { MaintenanceModeCard } from './MaintenanceModeCard';
import { api, type MaintenanceStatus } from '@/lib/api';
import { useMaintenanceStore, showDetailsUnlocked } from '@/stores/maintenance-store';

const INACTIVE_STATUS: MaintenanceStatus = { active: false, window: null, ssh_observed_active: null };

const ACTIVE_STATUS: MaintenanceStatus = {
  active: true,
  window: {
    id: 'w1',
    enabled_by: 'u1',
    enabled_by_email: 'admin@local',
    enabled_at: new Date().toISOString(),
    ttl_seconds: 4 * 3600,
    expires_at: new Date(Date.now() + 4 * 3600 * 1000).toISOString(),
    sub_capabilities: ['ssh', 'show_details'],
    remaining_seconds: 4 * 3600,
  },
  ssh_observed_active: true,
};

beforeEach(() => {
  vi.restoreAllMocks();
  useMaintenanceStore.setState({ status: null, loading: false, error: null });
});

describe('showDetailsUnlocked', () => {
  it('is false when no window is active', () => {
    expect(showDetailsUnlocked(null)).toBe(false);
    expect(showDetailsUnlocked(INACTIVE_STATUS)).toBe(false);
  });

  it('is false when active but show_details is not among the unlocked sub-capabilities', () => {
    const sshOnly: MaintenanceStatus = {
      ...ACTIVE_STATUS,
      window: { ...ACTIVE_STATUS.window!, sub_capabilities: ['ssh'] },
    };
    expect(showDetailsUnlocked(sshOnly)).toBe(false);
  });

  it('is true when active and show_details is unlocked', () => {
    expect(showDetailsUnlocked(ACTIVE_STATUS)).toBe(true);
  });
});

describe('<MaintenanceModeCard>', () => {
  it('renders the inactive state and fetches status on mount', async () => {
    vi.spyOn(api.maintenance, 'status').mockResolvedValue(INACTIVE_STATUS);

    renderWithProviders(<MaintenanceModeCard />);

    expect(await screen.findByText('Off')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /turn on maintenance mode/i })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /turn off maintenance mode/i })).not.toBeInTheDocument();
  });

  it('renders the active state with remaining time and unlocked capabilities', async () => {
    vi.spyOn(api.maintenance, 'status').mockResolvedValue(ACTIVE_STATUS);

    renderWithProviders(<MaintenanceModeCard />);

    expect(await screen.findByText('Active')).toBeInTheDocument();
    expect(screen.getByText(/admin@local/)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /turn off maintenance mode/i })).toBeInTheDocument();
  });

  it('shows the SSH-mismatch warning when the ledger says closed but the live probe says otherwise', async () => {
    vi.spyOn(api.maintenance, 'status').mockResolvedValue({
      active: false,
      window: null,
      ssh_observed_active: true,
    });

    renderWithProviders(<MaintenanceModeCard />);

    expect(await screen.findByText(/please contact technical support/i)).toBeInTheDocument();
  });

  it('the confirm button stays disabled until the exact type-to-confirm string is entered', async () => {
    vi.spyOn(api.maintenance, 'status').mockResolvedValue(INACTIVE_STATUS);
    const enable = vi.spyOn(api.maintenance, 'enable').mockResolvedValue({ window: ACTIVE_STATUS.window! });
    const user = userEvent.setup();

    renderWithProviders(<MaintenanceModeCard />);
    await user.click(await screen.findByRole('button', { name: /turn on maintenance mode/i }));

    const dialogConfirm = screen.getAllByRole('button', { name: /turn on maintenance mode/i }).at(-1)!;
    expect(dialogConfirm).toBeDisabled();

    await user.type(screen.getByPlaceholderText('MAINTENANCE'), 'wrong');
    expect(dialogConfirm).toBeDisabled();

    await user.clear(screen.getByPlaceholderText('MAINTENANCE'));
    await user.type(screen.getByPlaceholderText('MAINTENANCE'), 'MAINTENANCE');
    expect(dialogConfirm).not.toBeDisabled();

    await user.click(dialogConfirm);
    await waitFor(() => expect(enable).toHaveBeenCalledTimes(1));
    expect(enable.mock.calls[0][0]).toMatchObject({ confirmText: 'MAINTENANCE' });
  });

  it('disable requires no type-to-confirm text and calls the disable RPC', async () => {
    vi.spyOn(api.maintenance, 'status').mockResolvedValue(ACTIVE_STATUS);
    const disable = vi.spyOn(api.maintenance, 'disable').mockResolvedValue({ window: null });
    const user = userEvent.setup();

    renderWithProviders(<MaintenanceModeCard />);
    await user.click(await screen.findByRole('button', { name: /turn off maintenance mode/i }));

    const dialogConfirm = screen.getAllByRole('button', { name: /turn off maintenance mode/i }).at(-1)!;
    expect(dialogConfirm).not.toBeDisabled();

    await user.click(dialogConfirm);
    await waitFor(() => expect(disable).toHaveBeenCalledTimes(1));
  });
});

const SSH_STATUS: MaintenanceStatus = {
  ...INACTIVE_STATUS,
  ssh: {
    supported: true, reachable: true, reason: null, ready: true, key_installed: false,
    key_fingerprint: null, window_id: null, expires_in_s: null, fallback_timer_active: false,
    active_sessions: 0, sessions: [], auth_failures: 0, event_overflow: 0,
    pending_cleanup: false, pending_audit: false, problems: [], host_key_fingerprints: [], last_close: null,
  },
};
describe('keyed SSH window', () => {
  it('clears pasted private keys immediately and never sends them', async () => {
    vi.spyOn(api.maintenance, 'status').mockResolvedValue(SSH_STATUS);
    const enable = vi.spyOn(api.maintenance, 'enable');
    const user = userEvent.setup();
    renderWithProviders(<MaintenanceModeCard />);
    await user.click(await screen.findByRole('button', { name: /turn on maintenance mode/i }));
    await user.click(screen.getByRole('checkbox', { name: /remote technical support/i }));
    const field = screen.getByLabelText('Technician public key');
    await user.click(field);
    await user.paste('-----BEGIN OPENSSH PRIVATE KEY-----\nsecret');
    expect(field).toHaveValue('');
    expect(screen.getByText(/private key was cleared/i)).toBeInTheDocument();
    await user.type(screen.getByPlaceholderText('MAINTENANCE'), 'MAINTENANCE');
    expect(screen.getAllByRole('button', { name: /turn on maintenance mode/i }).at(-1)).toBeDisabled();
    expect(enable).not.toHaveBeenCalled();
  });
  it('disables unknown SSH support', async () => {
    vi.spyOn(api.maintenance, 'status').mockResolvedValue(INACTIVE_STATUS);
    const user = userEvent.setup();
    renderWithProviders(<MaintenanceModeCard />);
    await user.click(await screen.findByRole('button', { name: /turn on maintenance mode/i }));
    expect(screen.getByRole('checkbox', { name: /remote technical support/i })).toHaveAttribute('aria-disabled', 'true');
  });
  it('lets an Admin deselect SSH when support becomes unknown while the dialog is open', async () => {
    vi.spyOn(api.maintenance, 'status').mockResolvedValue(SSH_STATUS);
    const enable = vi.spyOn(api.maintenance, 'enable').mockResolvedValue({ window: ACTIVE_STATUS.window! });
    const user = userEvent.setup();
    renderWithProviders(<MaintenanceModeCard />);
    await user.click(await screen.findByRole('button', { name: /turn on maintenance mode/i }));
    const ssh = screen.getByRole('checkbox', { name: /remote technical support/i });
    await user.click(ssh);
    await user.type(screen.getByPlaceholderText('MAINTENANCE'), 'MAINTENANCE');
    act(() => useMaintenanceStore.setState({ status: INACTIVE_STATUS }));
    expect(ssh).toBeChecked();
    expect(ssh).not.toHaveAttribute('aria-disabled', 'true');
    await user.click(ssh);
    expect(ssh).not.toBeChecked();
    expect(ssh).toHaveAttribute('aria-disabled', 'true');
    await user.click(screen.getAllByRole('button', { name: /turn on maintenance mode/i }).at(-1)!);
    await waitFor(() => expect(enable).toHaveBeenCalledWith(expect.objectContaining({ subCapabilities: ['show_details'] })));
    expect(enable.mock.calls[0][0]).not.toHaveProperty('sshPublicKey');
  });
  it('honors the server reauthentication lock', async () => {
    vi.spyOn(api.maintenance, 'status').mockResolvedValue({ ...SSH_STATUS, reauth: { locked: true, retry_after_seconds: 900 } });
    renderWithProviders(<MaintenanceModeCard />);
    expect(await screen.findByText(/Try again in 15 minutes/)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /turn on maintenance mode/i })).toBeDisabled();
  });
  it.each([true, false])('uses the correct verification advice for push=%s', async (push) => {
    vi.spyOn(api.maintenance, 'status').mockResolvedValue({ ...SSH_STATUS, ...ACTIVE_STATUS, push_channel_available: push });
    renderWithProviders(<MaintenanceModeCard addresses={['192.168.1.2']} />);
    expect(await screen.findByText('ssh duduclaw-maint@192.168.1.2')).toBeInTheDocument();
    expect(screen.getByText(push ? /Compare both fingerprints with the Admin notification/ : /This dashboard alone cannot verify/)).toBeInTheDocument();
    expect(screen.getByText(/100.64.0.0\/10/)).toBeInTheDocument();
  });
  it('warns about an unconfirmed SSH close', async () => {
    vi.spyOn(api.maintenance, 'status').mockResolvedValue({ ...SSH_STATUS, close_unconfirmed: true });
    renderWithProviders(<MaintenanceModeCard />);
    expect(await screen.findByText(/SSH closure could not be confirmed/)).toBeInTheDocument();
  });
});

it('sends only the selected public key and displays a server refusal without closing the dialog', async () => {
  vi.spyOn(api.maintenance, 'status').mockResolvedValue(SSH_STATUS);
  const enable = vi.spyOn(api.maintenance, 'enable').mockRejectedValue({ code: 'gateway_not_owner' });
  const user = userEvent.setup();
  renderWithProviders(<MaintenanceModeCard />);
  await user.click(await screen.findByRole('button', { name: /turn on maintenance mode/i }));
  await user.click(screen.getByRole('checkbox', { name: /remote technical support/i }));
  // A complete wire-format Ed25519 public key (the server remains authoritative).
  const type = 'ssh-ed25519';
  const bytes = String.fromCharCode(0, 0, 0, type.length) + type + String.fromCharCode(0, 0, 0, 32) + 'x'.repeat(32);
  const key = `ssh-ed25519 ${btoa(bytes)}`;
  await user.click(screen.getByLabelText('Technician public key'));
  await user.paste(key);
  await user.type(screen.getByPlaceholderText('MAINTENANCE'), 'MAINTENANCE');
  await user.click(screen.getAllByRole('button', { name: /turn on maintenance mode/i }).at(-1)!);
  await waitFor(() => expect(enable).toHaveBeenCalledWith(expect.objectContaining({ sshPublicKey: key, subCapabilities: ['ssh', 'show_details'] })));
  expect(await screen.findByText(/Another gateway process owns this device/)).toBeInTheDocument();
  expect(screen.getByRole('dialog')).toBeInTheDocument();
});
