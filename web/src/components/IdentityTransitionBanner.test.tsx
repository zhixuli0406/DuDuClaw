import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { fireEvent, screen, waitFor } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import type { IdentityTokenStatus, SystemStatus } from '@/lib/api';
import { useAuthStore } from '@/stores/auth-store';
import { useSystemStore } from '@/stores/system-store';
import { IdentityTransitionBanner } from './IdentityTransitionBanner';

function status(identity?: Partial<IdentityTokenStatus>): IdentityTokenStatus | undefined {
  return identity && {
    mode: 'strict',
    explicit_setting: false,
    grace_until: null,
    days_left: null,
    notice_active: false,
    notice_until: null,
    untrusted_refusals_24h: 0,
    manual_grace_max_days: 30,
    ...identity,
  };
}

function setIdentity(identity?: Partial<IdentityTokenStatus>) {
  useSystemStore.setState({
    status: { version: '1.81.0', edition_profile: 'personal', identity_token: status(identity) } as SystemStatus,
  });
}

beforeEach(() => {
  mockWsClient.call.mockReset();
  mockWsClient.call.mockResolvedValue(null);
  useAuthStore.setState({ user: {
    id: 'admin', email: 'admin@test.invalid', display_name: 'Admin', role: 'admin', status: 'active',
  } });
});

afterEach(() => {
  useAuthStore.setState({ user: null });
  useSystemStore.setState({ status: null } as never);
});

describe('IdentityTransitionBanner', () => {
  it('is hidden on older gateways and for non-admins (field absent)', () => {
    setIdentity(undefined);
    const { container } = renderWithProviders(<IdentityTransitionBanner />);
    expect(container).toBeEmptyDOMElement();
  });

  it('is hidden in strict mode with no notice and no refusals', () => {
    setIdentity({ mode: 'strict' });
    const { container } = renderWithProviders(<IdentityTransitionBanner />);
    expect(container).toBeEmptyDOMElement();
  });

  it('is hidden for non-admins even if the field is present', () => {
    useAuthStore.setState({ user: { ...useAuthStore.getState().user!, role: 'manager' } });
    setIdentity({ mode: 'grace', days_left: 12, untrusted_refusals_24h: 3, notice_active: true });
    const { container } = renderWithProviders(<IdentityTransitionBanner />);
    expect(container).toBeEmptyDOMElement();
  });

  it('shows the strict-mode notice without a grace offer when nothing was refused', () => {
    setIdentity({ notice_active: true });
    renderWithProviders(<IdentityTransitionBanner />);
    expect(screen.getByText('Identity verification is now strict')).toBeInTheDocument();
    expect(screen.getByText(/no actions were refused/)).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /Relax temporarily/ })).not.toBeInTheDocument();
  });

  it('offers a manual grace with refusals, explains it, and requires the password', async () => {
    setIdentity({ notice_active: true, untrusted_refusals_24h: 4 });
    const graced = status({ mode: 'grace', days_left: 30, grace_until: '2026-12-09T00:00:00Z', explicit_setting: true });
    mockWsClient.call.mockResolvedValue({ grace_until: '2026-12-09T00:00:00Z', restart_required: false, identity_token: graced });
    renderWithProviders(<IdentityTransitionBanner />);
    expect(screen.getByText(/4 actions were refused/)).toBeInTheDocument();

    fireEvent.click(screen.getByRole('button', { name: 'Relax temporarily (up to 30 days)' }));
    expect(screen.getByText(/turns identity verification off/)).toBeInTheDocument();
    expect(screen.getByText(/security audit log/)).toBeInTheDocument();
    const confirm = screen.getByRole('button', { name: 'Relax for 30 days' });
    expect(confirm).toBeDisabled();
    expect(mockWsClient.call).not.toHaveBeenCalled();

    fireEvent.change(screen.getByLabelText(/current sign-in password/), { target: { value: 'pw' } });
    fireEvent.click(screen.getByRole('button', { name: 'Relax for 30 days' }));
    await waitFor(() =>
      expect(mockWsClient.call).toHaveBeenCalledWith('system.identity_grace_start', { password: 'pw', days: 30 }),
    );
    await waitFor(() => expect(screen.getByText(/30 days left/)).toBeInTheDocument());
    expect(useSystemStore.getState().status?.identity_token?.mode).toBe('grace');
  });

  it('cancel closes the confirmation without calling the gateway', () => {
    setIdentity({ notice_active: true, untrusted_refusals_24h: 1 });
    renderWithProviders(<IdentityTransitionBanner />);
    fireEvent.click(screen.getByRole('button', { name: /Relax temporarily/ }));
    fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(screen.queryByLabelText(/current sign-in password/)).not.toBeInTheDocument();
    expect(mockWsClient.call).not.toHaveBeenCalled();
  });

  it('shows a refused step-up inline', async () => {
    setIdentity({ notice_active: true, untrusted_refusals_24h: 1 });
    mockWsClient.call.mockRejectedValue(new Error('wrong password'));
    renderWithProviders(<IdentityTransitionBanner />);
    fireEvent.click(screen.getByRole('button', { name: /Relax temporarily/ }));
    fireEvent.change(screen.getByLabelText(/current sign-in password/), { target: { value: 'bad' } });
    fireEvent.click(screen.getByRole('button', { name: 'Relax for 30 days' }));
    expect(await screen.findByRole('alert')).toHaveTextContent(/not changed: wrong password/);
    expect(useSystemStore.getState().status?.identity_token?.mode).toBe('strict');
  });

  it('shows days left during a grace and ends it in one click', async () => {
    setIdentity({ mode: 'grace', days_left: 12, grace_until: '2026-12-09T00:00:00Z', explicit_setting: true });
    mockWsClient.call.mockResolvedValue({ changed: true, identity_token: status({ mode: 'strict', explicit_setting: true }) });
    renderWithProviders(<IdentityTransitionBanner />);
    expect(screen.getByText(/12 days left/)).toBeInTheDocument();
    expect(screen.getByText(/strict mode turns back on automatically/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'End the grace period now' }));
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('system.identity_grace_end', {}));
    await waitFor(() => expect(useSystemStore.getState().status?.identity_token?.mode).toBe('strict'));
  });

  it('warns about recent refusals outside the notice window, without a grace offer', () => {
    setIdentity({ mode: 'strict', untrusted_refusals_24h: 4 });
    renderWithProviders(<IdentityTransitionBanner />);
    expect(screen.getByText(/stopped 4 times/)).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /Relax temporarily/ })).not.toBeInTheDocument();
  });

  it('explains an unreadable setting', () => {
    setIdentity({ mode: 'error' });
    renderWithProviders(<IdentityTransitionBanner />);
    expect(screen.getByText(/identity setting cannot be read/)).toBeInTheDocument();
  });
});
