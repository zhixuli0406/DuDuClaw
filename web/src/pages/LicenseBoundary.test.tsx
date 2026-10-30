import { useBrandStore } from '@/stores/brand-store';
import {describe, it, expect, vi, beforeEach} from 'vitest';
import {screen, waitFor} from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import {mockWsClient} from '@/test/mocks';
import {renderWithProviders} from '@/test/render';
import {useConnectionStore} from '@/stores/connection-store';
import {useAgentsStore} from '@/stores/agents-store';
import {useSystemStore} from '@/stores/system-store';
import {useAuthStore} from '@/stores/auth-store';
import {LicenseShell} from './LicenseShell';
import {api} from '@/lib/api';
import {SoftLimitBanner} from '@/components/SoftLimitBanner';

beforeEach(() => {
    useBrandStore.getState().setBrand(null);
  vi.clearAllMocks();
  useConnectionStore.setState({state: 'authenticated'});
  useAgentsStore.setState({agents: []});
  useSystemStore.setState({status: null});
  useAuthStore.setState({user: {id: 'admin', display_name: 'Admin', role: 'admin'} as never, bindings: []});
  mockWsClient.call.mockImplementation(async (method: string) => {
    if (method === 'license.status') return {tier: 'open_source', mode: 'opensource', installed: false};
    if (method === 'license.fingerprint') return {fingerprint: 'machine-123'};
    if (method === 'license.activate') return {success: true};
    if (method === 'system.version') return {edition: 'hobby'};
    return null;
  });
});

describe('license management without direct sales', () => {
  it('keeps license activation and the general distributor contact, without sales or customer-side branding controls', async () => {
    const user = userEvent.setup();
    renderWithProviders(<LicenseShell />);
    await screen.findByText('machine-123');
    expect(screen.queryByRole('tab')).not.toBeInTheDocument();
    expect(screen.queryAllByText(/redeem code|free partner/i)).toHaveLength(0);
    expect(api.license).not.toHaveProperty('redeem');
    expect(screen.queryByText(/Need more commercial modules|Unlocked commercial modules|white-label/i)).not.toBeInTheDocument();
    const links = screen.getAllByRole('link');
    expect(links.length).toBeGreaterThan(0);
    for (const link of links) expect(link).toHaveAttribute('href', 'https://www.futurecorp.tw/');
    await user.type(screen.getByLabelText('License key'), 'signed-license');
    await user.click(screen.getByRole('button', {name: 'Activate'}));
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('license.activate', {key: 'signed-license'}));
    expect(mockWsClient.call.mock.calls.some(([method]) => /^(partner|branding)\./.test(String(method)))).toBe(false);
  });

  it('reports soft-limit usage without an upgrade or sales link', async () => {
    useAgentsStore.setState({agents: [{name: 'staff'}] as never});
    renderWithProviders(<SoftLimitBanner />);
    expect(await screen.findByText(/hobby/i)).toBeInTheDocument();
    expect(screen.queryByRole('link')).not.toBeInTheDocument();
    expect(screen.queryByText(/upgrade|distributor/i)).not.toBeInTheDocument();
  });
});
