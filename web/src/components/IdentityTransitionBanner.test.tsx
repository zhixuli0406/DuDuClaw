import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { screen } from '@testing-library/react';
import '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import type { IdentityTokenStatus, SystemStatus } from '@/lib/api';
import { useAuthStore } from '@/stores/auth-store';
import { useSystemStore } from '@/stores/system-store';
import { IdentityTransitionBanner } from './IdentityTransitionBanner';

function setIdentity(identity?: Partial<IdentityTokenStatus>) {
  const full: IdentityTokenStatus | undefined = identity && {
    mode: 'strict',
    explicit_setting: false,
    grace_until: null,
    days_left: null,
    upgrade_grace: false,
    untrusted_refusals_24h: 0,
    ...identity,
  };
  useSystemStore.setState({
    status: { version: '1.81.0', edition_profile: 'personal', identity_token: full } as SystemStatus,
  });
}

beforeEach(() => {
  useAuthStore.setState({ user: {
    id: 'admin', email: 'admin@test.invalid', display_name: 'Admin', role: 'admin', status: 'active',
  } });
});

afterEach(() => {
  useAuthStore.setState({ user: null });
  useSystemStore.setState({ status: null } as never);
});

describe('IdentityTransitionBanner', () => {
  it('is hidden on older gateways (field absent)', () => {
    setIdentity(undefined);
    const { container } = renderWithProviders(<IdentityTransitionBanner />);
    expect(container).toBeEmptyDOMElement();
  });

  it('is hidden in strict mode with no refusals', () => {
    setIdentity({ mode: 'strict' });
    const { container } = renderWithProviders(<IdentityTransitionBanner />);
    expect(container).toBeEmptyDOMElement();
  });

  it('is hidden for non-admins', () => {
    useAuthStore.setState({ user: { ...useAuthStore.getState().user!, role: 'manager' } });
    setIdentity({ mode: 'grace', days_left: 12, untrusted_refusals_24h: 3 });
    const { container } = renderWithProviders(<IdentityTransitionBanner />);
    expect(container).toBeEmptyDOMElement();
  });

  it('shows the days left during the transition period', () => {
    setIdentity({ mode: 'grace', days_left: 12, grace_until: '2026-12-09T00:00:00Z', explicit_setting: true });
    renderWithProviders(<IdentityTransitionBanner />);
    expect(screen.getByText(/12 days left/)).toBeInTheDocument();
    expect(screen.getByText(/strict mode turns on/)).toBeInTheDocument();
    expect(screen.queryByText(/were stopped/)).not.toBeInTheDocument();
  });

  it('warns about recent refusals, also in strict mode', () => {
    setIdentity({ mode: 'strict', untrusted_refusals_24h: 4 });
    renderWithProviders(<IdentityTransitionBanner />);
    expect(screen.getByText(/stopped 4 times/)).toBeInTheDocument();
    expect(screen.queryByText(/days left/)).not.toBeInTheDocument();
  });

  it('shows both the countdown and the refusal warning', () => {
    setIdentity({ mode: 'grace', days_left: 1, untrusted_refusals_24h: 1 });
    renderWithProviders(<IdentityTransitionBanner />);
    expect(screen.getByText(/1 day left/)).toBeInTheDocument();
    expect(screen.getByText(/stopped 1 time from/)).toBeInTheDocument();
  });

  it('explains an unreadable setting', () => {
    setIdentity({ mode: 'error' });
    renderWithProviders(<IdentityTransitionBanner />);
    expect(screen.getByText(/identity setting cannot be read/)).toBeInTheDocument();
  });
});
