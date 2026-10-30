import { describe, it, expect, beforeEach, vi } from 'vitest';
import { act, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { Mail } from 'lucide-react';
import '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { IntegrationConnectPanel } from './IntegrationConnectPanel';
import { api } from '@/lib/api';

function renderPanel(extraSetupSteps?: readonly string[]) {
  return renderWithProviders(
    <IntegrationConnectPanel
      providerId="google"
      prefix="google"
      headerIcon={Mail}
      consoleUrl="https://console.cloud.google.com/apis/credentials"
      consoleLabel="Google Cloud Console"
      clientIdPlaceholder="x.apps.googleusercontent.com"
      clientSecretPlaceholder="GOCSPX-..."
      capabilities={[]}
      extraSetupSteps={extraSetupSteps}
    />,
  );
}

function mockUnconfigured(redirectUri?: string) {
  vi.spyOn(api.mcp, 'oauthProviders').mockResolvedValue({
    providers: [
      {
        provider_id: 'google',
        name: 'Google',
        auth_url: 'https://accounts.google.com/o/oauth2/v2/auth',
        scopes: [],
        configured: false,
        token_status: 'none',
        expires_at: null,
        ...(redirectUri ? { redirect_uri: redirectUri } : {}),
      },
    ],
  });
  vi.spyOn(api.mcp, 'oauthStatus').mockResolvedValue({
    authenticated: false,
    expires_at: null,
    scopes: [],
  });
}

beforeEach(() => {
  vi.restoreAllMocks();
  vi.spyOn(api.googleCredentials, 'get').mockResolvedValue({ integration_enabled: true, effective: 'none', service_account: { configured: false, error: '' }, apps_script: { configured: false, error: '' }, required_scopes: [] });
  vi.spyOn(api.googleCredentials, 'set').mockResolvedValue({ ok: true, mode: 'oauth' });
});

describe('IntegrationConnectPanel setup guidance', () => {
  it('requires the Google data disclosure acknowledgement before starting OAuth', async () => {
    mockUnconfigured();
    const start = vi.spyOn(api.mcp, 'oauthStart').mockResolvedValue({ auth_url: 'https://accounts.google.com/consent', state: 's' });
    const user = userEvent.setup();
    renderPanel();

    const connect = await screen.findByRole('button',{name:'Connect Google'});
    expect(screen.getByText(/Google data may be stored in local conversation/i)).toBeInTheDocument();
    expect(connect).toBeDisabled();
    await user.type(screen.getByPlaceholderText('x.apps.googleusercontent.com'),'client-id');
    expect(connect).toBeDisabled();
    await user.click(screen.getByRole('checkbox',{name:/understand how my Google data is accessed/i}));
    expect(connect).toBeEnabled();
    await user.click(connect);
    await waitFor(()=>expect(start).toHaveBeenCalledWith('google','client-id',undefined,true));
  });

  it('shows the redirect URI the gateway reports, not a hardcoded one', async () => {
    // The gateway derives this from its live port. A UI constant is wrong the
    // moment DUDUCLAW_PORT differs — and a wrong URI dead-ends every consent
    // flow, because the browser is redirected to a port with nothing on it.
    mockUnconfigured('http://localhost:9999/api/mcp/oauth/callback');
    renderPanel();
    await waitFor(() =>
      expect(screen.getByText('http://localhost:9999/api/mcp/oauth/callback')).toBeInTheDocument(),
    );
  });

  it('shows a neutral message and disables copying when the gateway omits the URI', async () => {
    mockUnconfigured();
    renderPanel();
    await screen.findByPlaceholderText('x.apps.googleusercontent.com');
    await screen.findByText('The redirect URI is unavailable. Reopen setup to try loading it again.');
    expect(screen.queryByText('http://localhost:3000/api/mcp/oauth/callback')).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Copy' })).toBeDisabled();
  });

  it('renders provider-specific extra setup steps', async () => {
    // Google needs API enablement + a consent screen before a client works;
    // those steps lived only in the written guide, so the UI walked users into
    // a guaranteed 403 / blocked authorization.
    mockUnconfigured();
    renderPanel(['google.setup.enableApis', 'google.setup.consentScreen']);
    await waitFor(() => expect(screen.getByText(/Enable these eight APIs/i)).toBeInTheDocument());
    expect(screen.getByText(/Google Auth platform > Branding > Get Started/i)).toBeInTheDocument();
  });

  it('omits the extra steps for providers that do not supply any', async () => {
    mockUnconfigured();
    renderPanel();
    await screen.findByPlaceholderText('x.apps.googleusercontent.com');
    await screen.findByText('The redirect URI is unavailable. Reopen setup to try loading it again.');
    expect(screen.queryByText('http://localhost:3000/api/mcp/oauth/callback')).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Copy' })).toBeDisabled();
    expect(screen.queryByText(/Enable these eight APIs/i)).not.toBeInTheDocument();
  });
});

/**
 * WP13 — a customer whose Google integration was working ("測試連線" green, all
 * 19 tools reachable) reported that their saved credentials had disappeared.
 * Nothing was lost: the panel showed nothing but placeholders, because the
 * saved client id was never sent to the browser and an hour-old access token
 * was read as "not connected". These tests pin the visible contract.
 */
describe('IntegrationConnectPanel saved-credential display', () => {
  function mockSaved(overrides: {
    authenticated: boolean;
    clientId?: string;
    hasSecret?: boolean;
    secretMasked?: string;
  }) {
    vi.spyOn(api.mcp, 'oauthProviders').mockResolvedValue({
      providers: [
        {
          provider_id: 'google',
          name: 'Google',
          auth_url: 'https://accounts.google.com/o/oauth2/v2/auth',
          scopes: [],
          configured: true,
          token_status: overrides.authenticated ? 'authenticated' : 'none',
          expires_at: null,
          client_id: overrides.clientId ?? '1234567890-abcdef.apps.googleusercontent.com',
          has_client_secret: overrides.hasSecret ?? true,
          client_secret_masked: overrides.secretMasked ?? '••••9f3c',
        },
      ],
    });
    vi.spyOn(api.mcp, 'oauthStatus').mockResolvedValue({
      authenticated: overrides.authenticated,
      expires_at: null,
      scopes: [],
    });
  }

  it('shows the saved client id and a masked secret instead of placeholders', async () => {
    mockSaved({ authenticated: false });
    renderPanel();

    await waitFor(() =>
      expect(
        screen.getByText('1234567890-abcdef.apps.googleusercontent.com'),
      ).toBeInTheDocument(),
    );
    expect(screen.getByText('Saved ••••9f3c')).toBeInTheDocument();
    expect(screen.getByText('Credentials saved')).toBeInTheDocument();
    // The empty setup form is what read as "my settings are gone".
    expect(screen.queryByPlaceholderText('x.apps.googleusercontent.com')).not.toBeInTheDocument();
  });

  it('keeps announcing the real connection while credentials are being edited', async () => {
    // Opening the form used to rewrite the connection state to `unconfigured`,
    // so the header badge announced a disconnection that never happened.
    mockSaved({ authenticated: true });
    const user = userEvent.setup();
    renderPanel();

    await waitFor(() => expect(screen.getByText('Connected')).toBeInTheDocument());
    await user.click(screen.getByText('Edit credentials'));

    expect(screen.getByText('Connected')).toBeInTheDocument();
    expect(screen.getByText('Credentials saved')).toBeInTheDocument();
    expect(screen.queryByText('Not set up yet')).not.toBeInTheDocument();
    // The form really is open.
    expect(screen.getByPlaceholderText('Saved ••••9f3c, leave blank to keep it')).toBeInTheDocument();
  });

  it('returns to the previous view on cancel, leaving edits unsaved', async () => {
    mockSaved({ authenticated: true });
    const start = vi.spyOn(api.mcp, 'oauthStart');
    const user = userEvent.setup();
    renderPanel();

    await waitFor(() => expect(screen.getByText('Edit credentials')).toBeInTheDocument());
    await user.click(screen.getByText('Edit credentials'));

    const idField = screen.getByDisplayValue('1234567890-abcdef.apps.googleusercontent.com');
    await user.clear(idField);
    await user.type(idField, 'typo-client-id');
    await user.click(screen.getByText('Cancel'));

    // Back on the connected view, with the stored id intact and nothing sent.
    await waitFor(() => expect(screen.getByText('Disconnect')).toBeInTheDocument());
    expect(screen.getByText('1234567890-abcdef.apps.googleusercontent.com')).toBeInTheDocument();
    expect(screen.queryByDisplayValue('typo-client-id')).not.toBeInTheDocument();
    expect(start).not.toHaveBeenCalled();
  });

  it('keeps the stored secret when the field is left blank', async () => {
    mockSaved({ authenticated: false });
    const start = vi
      .spyOn(api.mcp, 'oauthStart')
      .mockResolvedValue({ auth_url: 'https://accounts.google.com/consent', state: 's' });
    vi.spyOn(window, 'open').mockReturnValue(null);
    const user = userEvent.setup();
    renderPanel();

    await waitFor(() => expect(screen.getByText('Edit credentials')).toBeInTheDocument());
    await user.click(screen.getByText('Edit credentials'));

    // The id field opens pre-filled with what is stored, not empty.
    const idField = screen.getByDisplayValue('1234567890-abcdef.apps.googleusercontent.com');
    expect(idField).toBeInTheDocument();
    expect(screen.getByPlaceholderText('Saved ••••9f3c, leave blank to keep it')).toBeInTheDocument();

    await user.click(screen.getByRole('checkbox',{name:/understand how my Google data is accessed/i}));
    await user.click(screen.getByText('Connect Google'));
    await waitFor(() => expect(start).toHaveBeenCalled());
    // No secret supplied ⇒ the gateway keeps the stored one. Sending an empty
    // string here would be the silent-wipe this test exists to prevent.
    expect(start).toHaveBeenCalledWith(
      'google',
      '1234567890-abcdef.apps.googleusercontent.com',
      undefined,
      true,
    );
  });

  it('shows the bound badge alongside the connected state', async () => {
    mockSaved({ authenticated: true });
    renderPanel();

    await waitFor(() => expect(screen.getByText('Credentials saved')).toBeInTheDocument());
    expect(screen.getByText('Connected')).toBeInTheDocument();
    expect(screen.getByText('Saved ••••9f3c')).toBeInTheDocument();
  });

  it('falls back to the setup form when nothing is stored', async () => {
    mockUnconfigured();
    renderPanel();

    await waitFor(() =>
      expect(screen.getByPlaceholderText('x.apps.googleusercontent.com')).toBeInTheDocument(),
    );
    expect(screen.getByPlaceholderText('GOCSPX-...')).toBeInTheDocument();
    expect(screen.queryByText('Credentials saved')).not.toBeInTheDocument();
    expect(screen.getByText('Not set up yet')).toBeInTheDocument();
  });
});

describe('Google OAuth actionable failures', () => {
  it.each(['access_denied', 'admin_policy_enforced', 'invalid_scope', 'other'] as const)('displays a closed callback error code: %s', async code => {
    mockUnconfigured();
    vi.mocked(api.mcp.oauthStatus).mockResolvedValue({ authenticated: false, expires_at: null, error_code: code });
    renderPanel();
    const messages = { access_denied: /authorization was declined/i, admin_policy_enforced: /organization blocked authorization/i, invalid_scope: /rejected the requested permissions/i, other: /authorization failed/i };
    expect(await screen.findByText(messages[code])).toBeInTheDocument();
  });
  it('shows reconnect and six-day Testing warnings from persisted connection state', async () => {
    mockUnconfigured();
    vi.mocked(api.mcp.oauthStatus).mockResolvedValue({ authenticated: false, expires_at: null, needs_reconnect: true });
    const view = renderPanel();
    expect(await screen.findByText(/authorization has expired or was revoked/i)).toBeInTheDocument();
    view.unmount();
    vi.mocked(api.mcp.oauthStatus).mockResolvedValue({ authenticated: true, expires_at: null, testing_warning: true });
    renderPanel();
    expect(await screen.findByText(/over six days old/i)).toBeInTheDocument();
  });
  it('does not switch a working path when replacement confirmation is declined', async () => {
    mockUnconfigured();
    vi.mocked(api.googleCredentials.get).mockResolvedValue({ integration_enabled: true, effective: 'apps_script', required_scopes: [], service_account: { configured: false, error: '' }, apps_script: { configured: true, error: '' } });
    vi.spyOn(window, 'confirm').mockReturnValue(false);
    const start = vi.spyOn(api.mcp, 'oauthStart');
    const user = userEvent.setup(); renderPanel();
    await user.type(await screen.findByPlaceholderText('x.apps.googleusercontent.com'), 'client-id');
    await user.click(screen.getByRole('checkbox', { name: /understand how my Google data is accessed/i }));
    await user.click(screen.getByRole('button', { name: 'Connect Google' }));
    await waitFor(() => expect(window.confirm).toHaveBeenCalled());
    expect(api.googleCredentials.set).not.toHaveBeenCalled();
    expect(start).not.toHaveBeenCalled();
  });
});


it('shows an actionable poll timeout instead of silently stopping', async () => {
  mockUnconfigured();
  vi.spyOn(api.mcp, 'oauthStart').mockResolvedValue({ auth_url: 'https://accounts.google.com/consent', state: 's' });
  let poll: (() => Promise<void>) | undefined;
  vi.spyOn(globalThis, 'setInterval').mockImplementation(((callback: () => Promise<void>, delay: number) => { if (delay === 3000) poll = callback; return 123; }) as typeof setInterval);
  const user = userEvent.setup(); renderPanel();
  await user.type(await screen.findByPlaceholderText('x.apps.googleusercontent.com'), 'client-id');
  await user.click(screen.getByRole('checkbox', { name: /understand how my Google data is accessed/i }));
  await user.click(screen.getByRole('button', { name: 'Connect Google' }));
  await waitFor(() => expect(poll).toBeDefined());
  await act(async () => { for (let attempt = 0; attempt < 101; attempt++) await poll!(); });
  expect(screen.getByText('Authorization timed out. Select Connect again to retry.')).toBeInTheDocument();
  expect(screen.getByRole('button', { name: 'Connect Google' })).toBeInTheDocument();
});


it('continues polling through an existing reconnect marker until the new token arrives', async () => {
  mockUnconfigured();
  vi.mocked(api.mcp.oauthStatus).mockResolvedValue({ authenticated: false, needs_reconnect: true, expires_at: '2026-10-29T00:00:00Z' });
  vi.spyOn(api.mcp, 'oauthStart').mockResolvedValue({ auth_url: 'https://accounts.google.com/consent', state: 's' });
  let poll: (() => Promise<void>) | undefined;
  const stop = vi.spyOn(globalThis, 'clearInterval');
  vi.spyOn(globalThis, 'setInterval').mockImplementation(((callback: () => Promise<void>, delay: number) => { if (delay === 3000) poll = callback; return 123; }) as typeof setInterval);
  const connected = vi.fn();
  const user = userEvent.setup();
  renderWithProviders(<IntegrationConnectPanel providerId="google" prefix="google" headerIcon={Mail} consoleUrl="https://console.cloud.google.com/auth/clients" consoleLabel="Google Cloud Console" clientIdPlaceholder="x.apps.googleusercontent.com" clientSecretPlaceholder="GOCSPX-..." capabilities={[]} onConnected={connected} />);
  await user.type(await screen.findByPlaceholderText('x.apps.googleusercontent.com'), 'client-id');
  await user.click(screen.getByRole('checkbox', { name: /understand how my Google data is accessed/i }));
  await user.click(screen.getByRole('button', { name: 'Connect Google' }));
  await waitFor(() => expect(poll).toBeDefined());
  stop.mockClear();
  await act(async () => { await poll!(); });
  expect(stop).not.toHaveBeenCalled();
  expect(connected).not.toHaveBeenCalled();
  // A retained token (including a refreshed expiry) is not a callback.
  vi.mocked(api.mcp.oauthStatus).mockResolvedValue({ authenticated: true, access_token_valid: true, expires_at: '2026-10-30T00:00:00Z' });
  await act(async () => { await poll!(); });
  expect(connected).not.toHaveBeenCalled();
  vi.mocked(api.mcp.oauthStatus).mockResolvedValue({ authenticated: true, access_token_valid: true, needs_reconnect: false, connected_at: '2026-10-30T00:00:00Z', expires_at: '2026-10-30T00:00:00Z' });
  await act(async () => { await poll!(); });
  expect(connected).toHaveBeenCalledOnce();
});

it('does not invent a redirect URI when provider loading fails', async () => {
  mockUnconfigured();
  vi.mocked(api.mcp.oauthProviders).mockRejectedValue(new Error('offline'));
  renderPanel();
  expect(await screen.findByText('The redirect URI is unavailable. Reopen setup to try loading it again.')).toBeInTheDocument();
  expect(await screen.findByText("Couldn't load the Google connection. Please try again.")).toBeInTheDocument();
  expect(screen.queryByText('http://localhost:3000/api/mcp/oauth/callback')).not.toBeInTheDocument();
  expect(screen.getByRole('button', { name: 'Copy' })).toBeDisabled();
});
