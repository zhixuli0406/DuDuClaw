import { describe, it, expect, beforeEach, vi } from 'vitest';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { MemoryRouter, Routes, Route } from 'react-router';
import { IntlProvider } from 'react-intl';
import '@/test/mocks';
import { AuthGuard, EditionGuard } from './AuthGuard';
import { useSystemStore } from '@/stores/system-store';
import { useAuthStore } from '@/stores/auth-store';
import { useConnectionStore } from '@/stores/connection-store';
import en from '@/i18n/en.json';
import type { SystemStatus } from '@/lib/api';

/**
 * EditionGuard (WP-A, 10-ia-scatter-audit D8/D10-B). The nav hid the
 * Enterprise-only surfaces from a Personal instance, but hiding a row never
 * closed the route — a bookmark, a typed URL, or one of the ungated legacy
 * aliases still rendered them. These lock in the three behaviours that matter:
 * blocked on Personal, allowed on Enterprise, and fail-open (never a redirect
 * flash) while `system.status` is still in flight.
 */
function renderGuarded(edition: string | undefined) {
  useSystemStore.setState({
    status: edition === undefined ? null : ({ edition_profile: edition } as unknown as SystemStatus),
  });
  return render(
    <MemoryRouter initialEntries={['/guarded']}>
      <Routes>
        <Route element={<EditionGuard enterprise />}>
          <Route path="/guarded" element={<div>enterprise-only</div>} />
        </Route>
        <Route path="/" element={<div>home</div>} />
      </Routes>
    </MemoryRouter>,
  );
}

beforeEach(() => {
  useSystemStore.setState({ status: null });
  useAuthStore.setState({ isAuthenticated: false, initialized: false, jwt: null });
  useConnectionStore.setState({ state: 'disconnected', everAuthenticated: false, mustChangePassword: false });
});

/**
 * AuthGuard's WS gate. The regression it exists to prevent: an authenticated
 * user whose gateway drops mid-session used to see a text-less spinner cover
 * the whole route tree, hiding every page's own error state for the single most
 * common failure ("the server went down"). The gate now splits the
 * WS-not-ready case by whether the socket had *ever* reached `authenticated`:
 * a first handshake keeps the brief spinner; a drop after a live connection
 * shows a reconnecting banner with copy + a manual retry.
 */
function renderAuthGuard() {
  return render(
    <IntlProvider messages={en} locale="en" defaultLocale="en">
      <MemoryRouter initialEntries={['/']}>
        <Routes>
          <Route element={<AuthGuard />}>
            <Route index element={<div>protected content</div>} />
          </Route>
          <Route path="/login" element={<div>login page</div>} />
        </Routes>
      </MemoryRouter>
    </IntlProvider>,
  );
}

describe('<AuthGuard> WS gate', () => {
  it('keeps a bare spinner on the FIRST handshake (never connected yet)', () => {
    useAuthStore.setState({ isAuthenticated: true, initialized: true });
    useConnectionStore.setState({ state: 'connecting', everAuthenticated: false });

    renderAuthGuard();

    // Spinner region present; no reconnect copy, no page content, no alert.
    expect(screen.getByRole('status')).toBeInTheDocument();
    expect(screen.queryByText('Lost connection to the server')).toBeNull();
    expect(screen.queryByText('protected content')).toBeNull();
    expect(screen.queryByRole('alert')).toBeNull();
  });

  it('shows a reconnecting banner with copy + retry after a mid-session drop', async () => {
    const connectSpy = vi.fn();
    useAuthStore.setState({ isAuthenticated: true, initialized: true, jwt: 'jwt.token.here' });
    useConnectionStore.setState({
      state: 'disconnected',
      everAuthenticated: true,
      // Selector-picked closure calls this; spy to prove the retry wires through.
      connectWithAuth: connectSpy,
    });

    renderAuthGuard();

    // Text, not a silent spinner.
    expect(screen.getByRole('alert')).toBeInTheDocument();
    expect(screen.getByText('Lost connection to the server')).toBeInTheDocument();
    expect(screen.queryByRole('status')).toBeNull();
    expect(screen.queryByText('protected content')).toBeNull();

    const retry = screen.getByRole('button', { name: 'Reconnect' });
    await userEvent.click(retry);
    expect(connectSpy).toHaveBeenCalledTimes(1);
  });

  it('renders the protected outlet once the WS is authenticated', () => {
    useAuthStore.setState({ isAuthenticated: true, initialized: true });
    useConnectionStore.setState({ state: 'authenticated', everAuthenticated: true });

    renderAuthGuard();

    expect(screen.getByText('protected content')).toBeInTheDocument();
    expect(screen.queryByRole('alert')).toBeNull();
    expect(screen.queryByRole('status')).toBeNull();
  });

  it('redirects an unauthenticated visitor to /login', () => {
    useAuthStore.setState({ isAuthenticated: false, initialized: true });

    renderAuthGuard();

    expect(screen.getByText('login page')).toBeInTheDocument();
    expect(screen.queryByText('protected content')).toBeNull();
  });
});

/**
 * WP-0 (bootstrap-admin recovery): a `must_change_password`-flagged account
 * (the standard bootstrap `admin@local`, or any account an operator just
 * reset) is refused every RPC outside the self-service allowlist
 * (`handlers.rs::is_password_change_allowlisted`). Before this fix the
 * dashboard had no way to know this ahead of time — every page just failed
 * its first RPC with an opaque error. The `connect` handshake now surfaces
 * the flag on `useConnectionStore().mustChangePassword`, and `AuthGuard`
 * force-redirects to the account-settings password form until it clears.
 *
 * Route is `/app/system/settings` (not `/settings`) — N-3 relocated the page
 * there; `AuthGuard`'s `CHANGE_PASSWORD_PATH` constant points at the current
 * canonical path, not the now-redirecting legacy alias (see that constant's
 * own doc comment for why the alias would infinite-loop here).
 */
function renderAuthGuardWithSettingsRoute(initialPath = '/some/deep/page') {
  return render(
    <IntlProvider messages={en} locale="en" defaultLocale="en">
      <MemoryRouter initialEntries={[initialPath]}>
        <Routes>
          <Route element={<AuthGuard />}>
            <Route path="/some/deep/page" element={<div>protected content</div>} />
            <Route path="/app/system/settings" element={<div>account settings page</div>} />
          </Route>
        </Routes>
      </MemoryRouter>
    </IntlProvider>,
  );
}

describe('<AuthGuard> must-change-password gate (WP-0)', () => {
  it('redirects to the account-settings password form when the flag is set', () => {
    useAuthStore.setState({ isAuthenticated: true, initialized: true });
    useConnectionStore.setState({ state: 'authenticated', everAuthenticated: true, mustChangePassword: true });

    renderAuthGuardWithSettingsRoute('/some/deep/page');

    expect(screen.getByText('account settings page')).toBeInTheDocument();
    expect(screen.queryByText('protected content')).toBeNull();
  });

  it('does NOT redirect once the flag is clear — normal navigation resumes', () => {
    useAuthStore.setState({ isAuthenticated: true, initialized: true });
    useConnectionStore.setState({ state: 'authenticated', everAuthenticated: true, mustChangePassword: false });

    renderAuthGuardWithSettingsRoute('/some/deep/page');

    expect(screen.getByText('protected content')).toBeInTheDocument();
  });

  it('renders the settings route itself instead of looping when already there', () => {
    useAuthStore.setState({ isAuthenticated: true, initialized: true });
    useConnectionStore.setState({ state: 'authenticated', everAuthenticated: true, mustChangePassword: true });

    renderAuthGuardWithSettingsRoute('/app/system/settings?tab=account');

    expect(screen.getByText('account settings page')).toBeInTheDocument();
  });
});

describe('<EditionGuard>', () => {
  it('sends a Personal instance home instead of rendering an Enterprise-only page', () => {
    renderGuarded('personal');
    expect(screen.getByText('home')).toBeInTheDocument();
    expect(screen.queryByText('enterprise-only')).toBeNull();
  });

  it('renders the page on an Enterprise instance', () => {
    renderGuarded('enterprise');
    expect(screen.getByText('enterprise-only')).toBeInTheDocument();
  });

  it('renders while the edition is still unknown, so there is no redirect flash', () => {
    renderGuarded(undefined);
    expect(screen.getByText('enterprise-only')).toBeInTheDocument();
  });

  it('is a no-op without the `enterprise` flag (never gates a shared surface)', () => {
    useSystemStore.setState({ status: { edition_profile: 'personal' } as unknown as SystemStatus });
    render(
      <MemoryRouter initialEntries={['/shared']}>
        <Routes>
          <Route element={<EditionGuard />}>
            <Route path="/shared" element={<div>both-editions</div>} />
          </Route>
          <Route path="/" element={<div>home</div>} />
        </Routes>
      </MemoryRouter>,
    );
    expect(screen.getByText('both-editions')).toBeInTheDocument();
  });
});
