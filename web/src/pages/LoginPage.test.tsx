import { StrictMode } from 'react';
import { afterEach } from 'vitest';
import { useBrandStore } from '@/stores/brand-store';
import { describe, it, expect, beforeEach, vi } from 'vitest';
import { screen, fireEvent, waitFor } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { useAuthStore } from '@/stores/auth-store';
import { LoginPage } from './LoginPage';
import { MemoryRouter, useLocation } from 'react-router';
import { render } from '@testing-library/react';
import { IntlProvider } from 'react-intl';
import en from '@/i18n/en.json';

function LocationProbe() {
  const location = useLocation();
  return <span data-testid="location">{location.pathname}</span>;
}

/**
 * WP5.1 — LoginPage Multica migration smoke test. Locks in the centered-card
 * layout on the app-shell surface and the three-mode form (password default).
 */
beforeEach(() => {
    useBrandStore.getState().setBrand(null);
  useAuthStore.setState({
    loading: false,
    // Not a first-run instance → the password form is shown.
    firstRunStatus: (async () => false) as never,
    login: vi.fn(async () => undefined) as never,
    otpRequest: vi.fn() as never,
    otpVerify: vi.fn() as never,
    firstRunClaim: vi.fn() as never,
  });
});

describe('<LoginPage>', () => {
  it('uses an effective distributor brand without customer-side branding controls', async () => {
    useBrandStore.getState().setBrand({ product_name: 'Partner Desk', logo_light: 'data:image/png;base64,iVBORw0KGgo=' });
    renderWithProviders(<><LoginPage /><LocationProbe /></>);
    expect(await screen.findByText('Partner Desk')).toBeInTheDocument();
    expect(screen.getByRole('img', { name: 'Partner Desk' })).toHaveAttribute('src', 'data:image/png;base64,iVBORw0KGgo=');
    expect(screen.getByText('Sign in to your account')).toBeInTheDocument();
    expect(screen.queryByText(/change brand/i)).not.toBeInTheDocument();
  });

  it('renders the brand seal, subtitle and password sign-in form', async () => {
    renderWithProviders(<LoginPage />);

    // Product name + subtitle (§5.8 login card).
    expect(await screen.findByText('DuDuClaw')).toBeInTheDocument();
    expect(screen.getByText('Sign in to your account')).toBeInTheDocument();

    // Email + password fields and the brand submit button.
    expect(screen.getByLabelText('Email')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Sign In' })).toBeInTheDocument();
  });
});

describe('<LoginPage> first-run claim', () => {
  it('asks for the owner email and sends it with the password', async () => {
    const firstRunClaim = vi.fn(async () => undefined);
    useAuthStore.setState({
      firstRunStatus: (async () => true) as never,
      firstRunClaim: firstRunClaim as never,
    });
    renderWithProviders(<LoginPage />);

    const email = await screen.findByLabelText('Your email');
    const submit = screen.getByRole('button', { name: 'Set password & sign in' });
    fireEvent.change(screen.getByLabelText('Set a password'), { target: { value: 'a-strong-pw' } });
    fireEvent.change(screen.getByLabelText('Confirm password'), { target: { value: 'a-strong-pw' } });
    // No email yet → cannot submit.
    expect(submit).toBeDisabled();

    fireEvent.change(email, { target: { value: 'owner@example.com' } });
    expect(submit).toBeEnabled();
    fireEvent.click(submit);

    await waitFor(() =>
      expect(firstRunClaim).toHaveBeenCalledWith('owner@example.com', 'a-strong-pw'),
    );
    expect(screen.queryByPlaceholderText('admin@local')).not.toBeInTheDocument();
  });

  it('shows MFA enrollment after a successful first-run claim without navigating', async () => {
    const challenge = { mfa_enrollment_required: true as const, enrollment_token: 't', challenge_ref: 'r' };
    const firstRunClaim = vi.fn(async () => challenge);
    useAuthStore.setState({
      firstRunStatus: (async () => true) as never,
      firstRunClaim: firstRunClaim as never,
    });
    const { container } = renderWithProviders(<><LoginPage /><LocationProbe /></>);

    const initialPath = screen.getByTestId('location').textContent;
    fireEvent.change(await screen.findByLabelText('Your email'), { target: { value: 'owner@example.com' } });
    fireEvent.change(screen.getByLabelText('Set a password'), { target: { value: 'a-strong-pw' } });
    fireEvent.change(screen.getByLabelText('Confirm password'), { target: { value: 'a-strong-pw' } });
    fireEvent.click(screen.getByRole('button', { name: 'Set password & sign in' }));

    await screen.findByLabelText('Channel or Admin enrollment code');
    expect(container.textContent).toContain('Enrollment reference: r');
    expect(screen.getByText('r')).toBeInTheDocument();
    expect(screen.getByLabelText('Channel or Admin enrollment code')).toBeInTheDocument();
    expect(screen.queryByLabelText('Your email')).not.toBeInTheDocument();
    expect(firstRunClaim).toHaveBeenCalledWith('owner@example.com', 'a-strong-pw');
    expect(screen.getByTestId('location')).toHaveTextContent(initialPath ?? '');
  });

  it('navigates after a first-run claim with no MFA challenge', async () => {
    const firstRunClaim = vi.fn(async () => undefined);
    useAuthStore.setState({
      firstRunStatus: (async () => true) as never,
      firstRunClaim: firstRunClaim as never,
    });
    renderWithProviders(<><LoginPage /><LocationProbe /></>);

    fireEvent.change(await screen.findByLabelText('Your email'), { target: { value: 'owner@example.com' } });
    fireEvent.change(screen.getByLabelText('Set a password'), { target: { value: 'a-strong-pw' } });
    fireEvent.change(screen.getByLabelText('Confirm password'), { target: { value: 'a-strong-pw' } });
    fireEvent.click(screen.getByRole('button', { name: 'Set password & sign in' }));

    await waitFor(() => expect(firstRunClaim).toHaveBeenCalledWith('owner@example.com', 'a-strong-pw'));
    await waitFor(() => expect(screen.getByTestId('location')).toHaveTextContent('/'));
  });
});

it('keeps persistence opt-in and passes the checked choice to password login', async () => {
  const login = vi.fn().mockResolvedValue({ mfa_required: true, challenge_id: 'pending' });
  useAuthStore.setState({ login, tryLocalSession: vi.fn().mockResolvedValue(false) });
  renderWithProviders(<LoginPage />);
  const checkbox = await screen.findByLabelText('Keep me signed in (180 days by default)');
  expect(checkbox).not.toBeChecked();
  fireEvent.click(checkbox);
  fireEvent.change(screen.getByLabelText('Email'), { target: { value: 'owner@example.com' } });
  fireEvent.change(screen.getByLabelText('Password'), { target: { value: 'current-password' } });
  fireEvent.click(screen.getByRole('button', { name: 'Sign In' }));
  await waitFor(() => expect(login).toHaveBeenCalledWith('owner@example.com', 'current-password', true));
});

function ConfirmReturnProbe() { const loc = useLocation(); return <div data-testid="confirm-location">{loc.pathname + loc.hash}</div>; }
it.each([
  [`/approvals/confirm#ticket=${'b'.repeat(64)}`, `/approvals/confirm#ticket=${'b'.repeat(64)}`],
  ['/approvals/confirm#ticket=bad', '/'],
  ['/arbitrary', '/'],
  ['https://evil.test/approvals/confirm', '/'],
])('keeps only an allowlisted confirmation return after login: %s', async (from, expected) => {
  useAuthStore.setState({ tryLocalSession: vi.fn().mockResolvedValue(false) });
  render(<IntlProvider locale="en" messages={en}><MemoryRouter initialEntries={[{ pathname: '/login', state: { from } }]}>
    <LoginPage /><ConfirmReturnProbe />
  </MemoryRouter></IntlProvider>);
  await screen.findByLabelText('Email');
  if (expected !== '/') expect(screen.getByText(en['approvalConfirm.loginNote'])).toBeInTheDocument();
  else expect(screen.queryByText(en['approvalConfirm.loginNote'])).not.toBeInTheDocument();
  fireEvent.change(screen.getByLabelText('Email'), { target: { value: 'owner@example.com' } });
  fireEvent.change(screen.getByLabelText('Password'), { target: { value: 'password' } });
  fireEvent.click(screen.getByRole('button', { name: 'Sign In' }));
  await waitFor(() => expect(screen.getByTestId('confirm-location').textContent).toBe(expected));
});

const realGoogleComplete = useAuthStore.getState().googleComplete;

describe('Google sign-in (S-13)', () => {
  let fetchMock: ReturnType<typeof vi.fn>;
  const reply = (data: unknown, status = 200) => Promise.resolve({ ok: status === 200, status, json: async () => data } as Response);
  beforeEach(() => {
    window.history.replaceState(null, '', '/login');
    sessionStorage.clear();
    localStorage.clear();
    useAuthStore.setState({ googleComplete: realGoogleComplete, tryLocalSession: vi.fn().mockResolvedValue(false), isAuthenticated: false });
    fetchMock = vi.fn((url: string) => url.endsWith('/api/login/methods') ? reply({ google: 'available' }) : reply({}));
    vi.stubGlobal('fetch', fetchMock);
  });
  afterEach(() => {
    useAuthStore.getState().logout();
    window.history.replaceState(null, '', '/');
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  it.each(['available', 'off', 'needs_public_url', 'unknown'])('only offers an available method: %s', async (google) => {
    fetchMock.mockImplementation(() => reply({ google }));
    renderWithProviders(<LoginPage />);
    await waitFor(() => expect(fetchMock).toHaveBeenCalled());
    if (google === 'available') expect(await screen.findByRole('button', { name: en['login.google.button'] })).toBeInTheDocument();
    else expect(screen.queryByRole('button', { name: en['login.google.button'] })).toBeNull();
  });

  it('silently hides the method on a network failure', async () => {
    fetchMock.mockRejectedValue(new Error('offline'));
    renderWithProviders(<LoginPage />);
    await screen.findByLabelText('Email');
    expect(screen.queryByRole('button', { name: en['login.google.button'] })).toBeNull();
    expect(screen.queryByRole('alert')).toBeNull();
  });

  it.each([
    ['https://accounts.google.com/o/oauth2/v2/auth?state=state', true],
    ['https://accounts.google.com.evil.test/auth', false],
    ['http://accounts.google.com/auth', false],
    ['https://accounts.google.com:444/auth', false],
    ['/relative', false],
  ])('validates the exact authorization origin: %s', async (authorization_url, allowed) => {
    const assign = vi.fn();
    vi.stubGlobal('location', { ...window.location, assign });
    fetchMock.mockImplementation((url: string) => reply(url.endsWith('/methods') ? { google: 'available' } : { authorization_url }));
    renderWithProviders(<LoginPage />);
    fireEvent.click(await screen.findByRole('button', { name: en['login.google.button'] }));
    if (allowed) await waitFor(() => expect(assign).toHaveBeenCalledWith(authorization_url));
    else {
      expect(await screen.findByRole('alert')).toHaveTextContent(en['login.google.result.incomplete']);
      expect(assign).not.toHaveBeenCalled();
    }
    expect(fetchMock).toHaveBeenCalledWith('/api/login/google/start', expect.objectContaining({ method: 'POST', credentials: 'same-origin', body: '{}' }));
  });

  it.each([404, 429])('shows the generic message when start returns HTTP %s', async (status) => {
    fetchMock.mockImplementation((url: string) => url.endsWith('/methods')
      ? reply({ google: 'available' }) : reply({ error: 'internal detail' }, status));
    renderWithProviders(<LoginPage />);
    fireEvent.click(await screen.findByRole('button', { name: en['login.google.button'] }));
    expect(await screen.findByRole('alert')).toHaveTextContent(en['login.google.result.incomplete']);
    expect(screen.getByRole('button', { name: en['login.google.button'] })).toBeEnabled();
  });

  function callback() {
    window.history.replaceState({ usr: { marker: 'preserved' }, key: 'router-key', idx: 2 }, '', '/login?lang=en#gsi=secret_ticket-123');
  }

  it('strips the ticket immediately, keeps history state and completes once under StrictMode', async () => {
    callback();
    fetchMock.mockImplementation((url: string) => url.endsWith('/complete') ? new Promise(() => {}) : reply({ google: 'available' }));
    renderWithProviders(<StrictMode><LoginPage /></StrictMode>);
    expect(window.location.hash).toBe('');
    expect(window.location.search).toBe('?lang=en');
    expect(window.history.state).toEqual({ usr: { marker: 'preserved' }, key: 'router-key', idx: 2 });
    expect(screen.getByText(en['login.google.completing'])).toBeInTheDocument();
    expect(screen.getByText(en['login.google.wait'])).toBeInTheDocument();
    await waitFor(() => expect(fetchMock.mock.calls.filter(([url]) => url.endsWith('/complete'))).toHaveLength(1));
    expect(fetchMock).toHaveBeenCalledWith('/api/login/google/complete', expect.objectContaining({ credentials: 'same-origin', body: JSON.stringify({ ticket: 'secret_ticket-123' }) }));
    expect(fetchMock.mock.calls.every(([url]) => !url.includes('secret_ticket'))).toBe(true);
    for (const storage of [localStorage, sessionStorage]) {
      const entries = Array.from({ length: storage.length }, (_, i) => {
        const key = storage.key(i)!;
        return [key, storage.getItem(key)];
      });
      expect(JSON.stringify(entries)).not.toContain('secret_ticket');
    }
  });

  it('establishes tokens and lands on / even with a router return target', async () => {
    callback();
    fetchMock.mockImplementation((url: string) => reply(url.endsWith('/complete')
      ? { access_token: 'google-access', refresh_token: 'google-refresh', user: { id: 'google-user' } }
      : url.endsWith('/api/me') ? { user: { id: 'google-user' }, bindings: [] } : { google: 'available' }));
    render(<IntlProvider locale="en" messages={en}><MemoryRouter initialEntries={[{ pathname: '/login', state: { from: `/approvals/confirm#ticket=${'b'.repeat(64)}` } }]}>
      <LoginPage /><LocationProbe />
    </MemoryRouter></IntlProvider>);
    await waitFor(() => expect(useAuthStore.getState().isAuthenticated).toBe(true));
    expect(useAuthStore.getState().jwt).toBe('google-access');
    expect(useAuthStore.getState().refreshToken).toBe('google-refresh');
    await waitFor(() => expect(screen.getByTestId('location')).toHaveTextContent(/^\/$/));
  });

  it.each([
    { mfa_required: true, challenge_id: 'google-mfa' },
    { mfa_enrollment_required: true, enrollment_token: 'enroll', challenge_ref: 'google-ref' },
  ])('hands off the challenge to LoginMfa: %j', async (challenge) => {
    callback();
    fetchMock.mockImplementation((url: string) => reply(url.endsWith('/complete') ? challenge : { google: 'available' }));
    renderWithProviders(<LoginPage />);
    if ('mfa_required' in challenge) expect(await screen.findByText('Verify your sign-in')).toBeInTheDocument();
    else expect(await screen.findByLabelText('Channel or Admin enrollment code')).toBeInTheDocument();
    expect(useAuthStore.getState().isAuthenticated).toBe(false);
  });

  it.each([
    ['domain_not_allowed', 'domainNotAllowed'], ['personal_account', 'personalAccount'],
    ['existing_account', 'existingAccount'], ['pending_approval', 'pendingApproval'],
    ['account_disabled', 'cannotSignIn'], ['rejected', 'cannotSignIn'],
    ['daily_limit', 'dailyLimit'], ['expired', 'incomplete'], ['failed', 'incomplete'], ['unknown', 'incomplete'],
  ])('maps result %s to the canvas sentence', async (result, key) => {
    callback();
    fetchMock.mockImplementation((url: string) => reply(url.endsWith('/complete') ? { result } : { google: 'available' }));
    renderWithProviders(<LoginPage />);
    expect(await screen.findByRole('alert')).toHaveTextContent(en[`login.google.result.${key}` as keyof typeof en]);
    expect(useAuthStore.getState().isAuthenticated).toBe(false);
  });

  it('shows the generic message when complete fails', async () => {
    callback();
    fetchMock.mockImplementation((url: string) => url.endsWith('/complete')
      ? Promise.reject(new Error('private server detail')) : reply({ google: 'available' }));
    renderWithProviders(<LoginPage />);
    expect(await screen.findByRole('alert')).toHaveTextContent(en['login.google.result.incomplete']);
    expect(screen.queryByText('private server detail')).toBeNull();
  });

  it.each(['#gsi=', '#gsi=bad.ticket', '#gsi=ticket&other=value'])('does not complete an invalid fragment: %s', async (hash) => {
    window.history.replaceState(null, '', '/login' + hash);
    renderWithProviders(<LoginPage />);
    await screen.findByLabelText('Email');
    expect(fetchMock.mock.calls.some(([url]) => url.endsWith('/complete'))).toBe(false);
  });

  it('strips an expired redirect and shows the incomplete sentence without completing', async () => {
    window.history.replaceState(null, '', '/login#gsi_error=expired');
    renderWithProviders(<LoginPage />);
    expect(window.location.hash).toBe('');
    expect(await screen.findByRole('alert')).toHaveTextContent(en['login.google.result.incomplete']);
    expect(fetchMock.mock.calls.some(([url]) => url.endsWith('/complete'))).toBe(false);
  });
});
