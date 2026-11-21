import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { useAuthStore } from '@/stores/auth-store';
import { useConnectionStore } from '@/stores/connection-store';
import { useAgentsStore } from '@/stores/agents-store';
import type { LicenseSnapshot, LicenseSyncResult } from '@/lib/api';
import { classifyExpiry, LicensePage, seatLimitLabel } from './LicensePage';

describe('classifyExpiry', () => {
  it('returns ok when expiry is months away', () => {
    expect(classifyExpiry(120).tone).toBe('ok');
    expect(classifyExpiry(31).tone).toBe('ok');
  });

  it('returns warning at the 30-day threshold', () => {
    expect(classifyExpiry(30).tone).toBe('warning');
    expect(classifyExpiry(15).tone).toBe('warning');
    expect(classifyExpiry(8).tone).toBe('warning');
  });

  it('returns critical at the 7-day threshold', () => {
    expect(classifyExpiry(7).tone).toBe('critical');
    expect(classifyExpiry(1).tone).toBe('critical');
    expect(classifyExpiry(0).tone).toBe('critical');
  });

  it('returns expired for negative days', () => {
    expect(classifyExpiry(-1).tone).toBe('expired');
    expect(classifyExpiry(-30).tone).toBe('expired');
  });

  it('returns unknown when value is null or undefined', () => {
    expect(classifyExpiry(null).tone).toBe('unknown');
    expect(classifyExpiry(undefined).tone).toBe('unknown');
  });

  it('emits a stable label id per bucket', () => {
    expect(classifyExpiry(60).labelId).toBe('license.expiry.ok');
    expect(classifyExpiry(20).labelId).toBe('license.expiry.warning');
    expect(classifyExpiry(3).labelId).toBe('license.expiry.critical');
    expect(classifyExpiry(-1).labelId).toBe('license.expiry.expired');
    expect(classifyExpiry(undefined).labelId).toBe('license.expiry.unknown');
  });
});

describe('seatLimitLabel', () => {
  it('shows the enforced number, "unlimited" only when the gateway says so, and nothing otherwise', () => {
    expect(seatLimitLabel({ seat_limit: 5, seat_unlimited: false })).toEqual({ kind: 'number', value: 5 });
    expect(seatLimitLabel({ seat_limit: null, seat_unlimited: true })).toEqual({ kind: 'unlimited' });
    // Unknown (older gateway / failed lookup): no row, never a made-up number.
    expect(seatLimitLabel({ seat_limit: null, seat_unlimited: false })).toBeNull();
    expect(seatLimitLabel({ seat_limit: null })).toBeNull();
    expect(seatLimitLabel({})).toBeNull();
  });
});

const licensed: LicenseSnapshot = {
  tier: 'self_host_pro',
  mode: 'commercial',
  installed: true,
  customer_id: 'cus_1',
  subscription_id: 'sub_1',
  days_until_expiry: 120,
  days_since_phone_home: 1,
  max_agents: 5,
  seat_limit: 5,
  seat_unlimited: false,
  seats_used: 3,
};

function syncReply(
  outcome: LicenseSyncResult['sync']['outcome'],
  extra: Partial<LicenseSyncResult> = {},
): LicenseSyncResult {
  return {
    ...licensed,
    sync: { outcome, message: 'gateway text' },
    restart_recommended: false,
    ...extra,
  };
}

function mockGateway(status: LicenseSnapshot, sync?: LicenseSyncResult) {
  mockWsClient.call.mockImplementation(async (method: string) => {
    if (method === 'license.status') return status;
    if (method === 'license.sync') return sync ?? syncReply('unchanged');
    if (method === 'license.fingerprint') return { fingerprint: 'machine-123' };
    return null;
  });
}

const callsOf = (method: string) =>
  mockWsClient.call.mock.calls.filter(([m]) => m === method).length;

describe('LicensePage refresh and seats', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useConnectionStore.setState({ state: 'authenticated' });
    useAgentsStore.setState({ agents: [] });
    useAuthStore.setState({ user: { id: 'admin', role: 'admin' } as never, bindings: [] });
    mockGateway(licensed);
  });

  it('shows the AI employee limit and current use', async () => {
    renderWithProviders(<LicensePage />);
    expect(await screen.findByText('AI employee limit')).toBeInTheDocument();
    expect(screen.getByTestId('license-seat-limit')).toHaveTextContent('5');
    expect(screen.getByText('In use')).toBeInTheDocument();
    expect(screen.getByTestId('license-seats-used')).toHaveTextContent('3');
  });

  it('shows "Unlimited" only when the gateway reports no cap', async () => {
    mockGateway({ ...licensed, max_agents: null, seat_limit: null, seat_unlimited: true });
    renderWithProviders(<LicensePage />);
    expect(await screen.findByTestId('license-seat-limit')).toHaveTextContent('Unlimited');
  });

  it('hides the rows when the gateway gives no numbers', async () => {
    mockGateway({ ...licensed, seat_limit: null, seat_unlimited: false, seats_used: null });
    renderWithProviders(<LicensePage />);
    await screen.findByText('Customer ID');
    expect(screen.queryByText('AI employee limit')).not.toBeInTheDocument();
    expect(screen.queryByText('In use')).not.toBeInTheDocument();
    expect(screen.queryByText('Unlimited')).not.toBeInTheDocument();
  });

  it('admin refresh syncs with the vendor and reports the outcome', async () => {
    const user = userEvent.setup();
    mockGateway(licensed, syncReply('updated', { seat_limit: 8, seats_used: 3 }));
    renderWithProviders(<LicensePage />);
    await screen.findByText('AI employee limit');
    await user.click(screen.getByRole('button', { name: 'Refresh' }));
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('license.sync', {}));
    expect(await screen.findByText('Updated')).toBeInTheDocument();
    expect(screen.getByTestId('license-seat-limit')).toHaveTextContent('8');
    expect(callsOf('license.status')).toBe(1);
    expect(screen.queryByText(/Restart the service/)).not.toBeInTheDocument();
  });

  it.each([
    ['unchanged', 'Already up to date'],
    ['offline', "Can't reach the vendor right now. It will retry automatically."],
    ['revoked', 'The license has been revoked'],
    ['pending_rebind', 'Waiting for the vendor to approve the machine change'],
    ['failed', 'This sync did not finish. It will retry automatically.'],
  ] as const)('admin sync outcome %s is shown in plain words', async (outcome, text) => {
    const user = userEvent.setup();
    mockGateway(licensed, syncReply(outcome));
    renderWithProviders(<LicensePage />);
    await screen.findByText('AI employee limit');
    await user.click(screen.getByRole('button', { name: 'Refresh' }));
    expect(await screen.findByText(text)).toBeInTheDocument();
    expect(screen.queryByText('gateway text')).not.toBeInTheDocument();
  });

  it('says how long to wait when a sync just ran', async () => {
    const user = userEvent.setup();
    mockGateway(licensed, {
      ...syncReply('busy'),
      sync: { outcome: 'busy', message: 'gateway text', retry_after_secs: 12 },
    });
    renderWithProviders(<LicensePage />);
    await screen.findByText('AI employee limit');
    await user.click(screen.getByRole('button', { name: 'Refresh' }));
    expect(await screen.findByText('Synced moments ago. Try again in 12 seconds.')).toBeInTheDocument();
  });

  it('recommends a restart when the plan resolves a different edition', async () => {
    const user = userEvent.setup();
    mockGateway(licensed, syncReply('updated', { restart_recommended: true }));
    renderWithProviders(<LicensePage />);
    await screen.findByText('AI employee limit');
    await user.click(screen.getByRole('button', { name: 'Refresh' }));
    expect(
      await screen.findByText(
        'Your plan has changed. Restart the service to apply it fully. AI employee seats and the expiry date already apply.',
      ),
    ).toBeInTheDocument();
  });

  it.each(['manager', 'operator', 'viewer'])('%s refresh only re-reads the status, never syncs', async (role) => {
    const user = userEvent.setup();
    useAuthStore.setState({ user: { id: 'u', role } as never, bindings: [] });
    renderWithProviders(<LicensePage />);
    await screen.findByText('AI employee limit');
    await user.click(screen.getByRole('button', { name: 'Refresh' }));
    await waitFor(() => expect(callsOf('license.status')).toBe(2));
    expect(callsOf('license.sync')).toBe(0);
    expect(screen.queryByText('Already up to date')).not.toBeInTheDocument();
  });
});
