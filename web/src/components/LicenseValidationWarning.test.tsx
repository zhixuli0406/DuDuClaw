import { beforeEach, describe, expect, it, vi } from 'vitest';
import { act, screen } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { useAuthStore } from '@/stores/auth-store';
import { useConnectionStore } from '@/stores/connection-store';
import { useAgentsStore } from '@/stores/agents-store';
import type { LicenseSnapshot } from '@/lib/api';
import { LicenseValidationWarning } from './LicenseValidationWarning';
import { LicenseExpiryBanner } from './LicenseExpiryBanner';
import { LicensePage } from '@/pages/LicensePage';

const invalid: LicenseSnapshot = {
  tier: 'open_source', mode: 'opensource', installed: true,
  validation_warning: 'machine_mismatch',
};

beforeEach(() => {
  vi.clearAllMocks();
  useAuthStore.setState({ user: { id: 'admin', role: 'admin' } as never, bindings: [] });
  useConnectionStore.setState({ state: 'authenticated' });
  useAgentsStore.setState({ agents: [] });
  mockWsClient.call.mockImplementation(async (method: string) => {
    if (method === 'license.status') return invalid;
    if (method === 'license.fingerprint') return { fingerprint: 'admin-only-fingerprint' };
    return null;
  });
});

describe('installed license validation warning', () => {
  it('warns the admin about the machine mismatch and personal-edition fallback', () => {
    renderWithProviders(<LicenseValidationWarning snapshot={invalid} />);
    expect(screen.getByRole('alert')).toHaveTextContent('this machine does not match the licensed machine');
    expect(screen.getByRole('alert')).toHaveTextContent('core features remain available');
  });

  it.each(['manager', 'operator', 'viewer'])('hides even a supplied warning from %s', role => {
    useAuthStore.setState({ user: { role } as never });
    renderWithProviders(<LicenseValidationWarning snapshot={invalid} />);
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
  });

  it.each([
    null,
    { ...invalid, installed: false, validation_warning: null },
    { ...invalid, tier: 'self_host_pro', mode: 'commercial', validation_warning: null },
  ])('does not warn for an absent or healthy license: %j', snapshot => {
    renderWithProviders(<LicenseValidationWarning snapshot={snapshot as LicenseSnapshot | null} />);
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
  });

  it('tells admins a weak binding requires account-holder approval in the console', () => {
    renderWithProviders(<LicenseValidationWarning snapshot={{ ...invalid, validation_warning: 'weak_binding' }} />);
    expect(screen.getByRole('alert')).toHaveTextContent('commercial features are inactive');
    expect(screen.getByRole('alert')).toHaveTextContent('account holder to approve it in the console');
  });

  it.each(['manager', 'operator', 'viewer'])('hides weak-binding details from %s', role => {
    useAuthStore.setState({ user: { role } as never });
    renderWithProviders(<LicenseValidationWarning snapshot={{ ...invalid, validation_warning: 'weak_binding' }} />);
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
  });

  it('explains unreadable hardware separately from a mismatched machine', () => {
    renderWithProviders(<LicenseValidationWarning snapshot={{ ...invalid, validation_warning: 'hardware_identity_unavailable' }} />);
    expect(screen.getByRole('alert')).toHaveTextContent('machine identity could not be read');
    expect(screen.getByRole('alert')).not.toHaveTextContent('does not match');
  });

  it('never renders unrecognized server reason text or fingerprint values', () => {
    renderWithProviders(<LicenseValidationWarning snapshot={{ ...invalid, validation_warning: 'secret-machine-fingerprint' }} />);
    expect(screen.getByRole('alert')).toHaveTextContent('installed license failed validation');
    expect(screen.queryByText(/secret-machine-fingerprint/)).not.toBeInTheDocument();
  });

  it('shows the RPC warning in the cross-page banner and removes it when admin access is lost', async () => {
    renderWithProviders(<LicenseExpiryBanner />);
    expect(await screen.findByRole('alert')).toHaveTextContent('does not match');
    act(() => useAuthStore.setState({ user: { role: 'operator' } as never }));
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
  });

  it('loads the warning after reconnecting instead of missing it before authentication', async () => {
    useConnectionStore.setState({ state: 'disconnected' });
    renderWithProviders(<LicenseExpiryBanner />);
    expect(mockWsClient.call).not.toHaveBeenCalled();
    act(() => useConnectionStore.setState({ state: 'authenticated' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('does not match');
  });

  it('shows the same warning in license status and labels the active mode as personal', async () => {
    renderWithProviders(<LicensePage />);
    expect(await screen.findByRole('alert')).toHaveTextContent('does not match');
    expect(screen.queryByText('Commercial license')).not.toBeInTheDocument();
    expect(screen.getByText('Apache 2.0 open-source mode — core features fully unlocked')).toBeInTheDocument();
    expect(screen.getByLabelText('License key')).toBeInTheDocument();
  });
});
