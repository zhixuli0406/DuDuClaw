import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen, waitFor } from '@testing-library/react';
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
