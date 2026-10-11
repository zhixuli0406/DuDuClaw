import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { DistributorsPage } from './DistributorsPage';

beforeEach(() => {
  vi.clearAllMocks();
  mockWsClient.call.mockImplementation(async (method: string) => {
    if (method === 'distributor.status') return {issuer_configured: true, stats: null};
    if (method === 'distributor.list') return {distributors: [{id: 'dealer', name: 'Future Corp', contact: '', note: '', status: 'active', licenses: []}]};
    if (method === 'distributor.issue') return {license_blob: 'signed-license'};
    return null;
  });
});

describe('internal distributor issuance', () => {
  it('shows issuance management without branding, partner sales or bundle controls', async () => {
    renderWithProviders(<DistributorsPage />);
    expect(await screen.findByText('Future Corp')).toBeInTheDocument();
    expect(screen.getByRole('heading', {name: 'Distributor licenses'})).toBeInTheDocument();
    expect(screen.queryByRole('tab')).not.toBeInTheDocument();
    expect(screen.queryByText(/branding bundle|white-label|go to partner/i)).not.toBeInTheDocument();
    expect(mockWsClient.call.mock.calls.some(([method]) => String(method).startsWith('branding.'))).toBe(false);
  });

  it('issues a license without editable branding scope', async () => {
    const user = userEvent.setup();
    renderWithProviders(<DistributorsPage />);
    await user.click(await screen.findByRole('button', {name: 'More'}));
    await user.click(await screen.findByRole('menuitem', {name: /issue/i}));
    expect(screen.queryByRole('checkbox')).not.toBeInTheDocument();
    await user.type(screen.getByLabelText('Machine fingerprint'), 'machine-123');
    await user.click(screen.getByRole('button', {name: 'Issue license'}));
    await screen.findByText('signed-license');
    const issue = mockWsClient.call.mock.calls.find(([method]) => method === 'distributor.issue');
    expect(issue?.[1]).toEqual({distributor_id: 'dealer', machine_fingerprint: 'machine-123', expires_days: 365, note: undefined});
  });
});
