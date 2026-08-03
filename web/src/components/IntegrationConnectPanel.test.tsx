import { describe, it, expect, beforeEach, vi } from 'vitest';
import { screen, waitFor } from '@testing-library/react';
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
