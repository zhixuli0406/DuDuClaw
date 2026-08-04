import { describe, it, expect, beforeEach, vi } from 'vitest';
import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { Mail } from 'lucide-react';
import '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { IntegrationConnectPanel, OAUTH_REDIRECT_URI } from './IntegrationConnectPanel';
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
});

describe('IntegrationConnectPanel setup guidance', () => {
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

  it('falls back to the built-in URI when an older gateway omits it', async () => {
    mockUnconfigured();
    renderPanel();
    await waitFor(() => expect(screen.getByText(OAUTH_REDIRECT_URI)).toBeInTheDocument());
  });

  it('renders provider-specific extra setup steps', async () => {
    // Google needs API enablement + a consent screen before a client works;
    // those steps lived only in the written guide, so the UI walked users into
    // a guaranteed 403 / blocked authorization.
    mockUnconfigured();
    renderPanel(['google.setup.enableApis', 'google.setup.consentScreen']);
    await waitFor(() => expect(screen.getByText(/Enable these eight APIs/i)).toBeInTheDocument());
    expect(screen.getByText(/OAuth consent screen/i)).toBeInTheDocument();
  });

  it('omits the extra steps for providers that do not supply any', async () => {
    mockUnconfigured();
    renderPanel();
    await waitFor(() => expect(screen.getByText(OAUTH_REDIRECT_URI)).toBeInTheDocument());
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

    await user.click(screen.getByText('Connect Google'));
    await waitFor(() => expect(start).toHaveBeenCalled());
    // No secret supplied ⇒ the gateway keeps the stored one. Sending an empty
    // string here would be the silent-wipe this test exists to prevent.
    expect(start).toHaveBeenCalledWith(
      'google',
      '1234567890-abcdef.apps.googleusercontent.com',
      undefined,
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
