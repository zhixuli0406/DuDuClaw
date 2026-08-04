import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { McpPage } from './McpPage';
import { useConnectionStore } from '@/stores/connection-store';

beforeEach(() => {
  vi.clearAllMocks();
  useConnectionStore.setState({ state: 'authenticated' as never, error: null });
  // Empty MCP config: mcp.list → { agents: [], catalog: [] }; oauth providers → [].
  mockWsClient.call.mockResolvedValue({ agents: [], catalog: [], providers: [] });
});

describe('McpPage', () => {
  it('renders the slim header description', async () => {
    renderWithProviders(<McpPage />);
    expect(await screen.findByText('MCP Server Management')).toBeInTheDocument();
  });

  it('shows the empty state when no agents are configured', async () => {
    renderWithProviders(<McpPage />);
    expect(
      await screen.findByText('No MCP servers configured yet'),
    ).toBeInTheDocument();
  });

  it('exposes the primary import action button', () => {
    renderWithProviders(<McpPage />);
    expect(
      screen.getByRole('button', { name: /import from url/i }),
    ).toBeInTheDocument();
  });
});

/**
 * Same "saved but invisible" class as the Google tab (WP13). The provider card
 * read `token_status` while the gateway only ever sent `status`, so every
 * connected provider rendered as unauthenticated, the card never showed which
 * client id it held, and the 設定 button disappeared once configured — leaving
 * revoke as the only way to fix a typo.
 */
describe('McpPage OAuth provider cards', () => {
  function mockProvider(extra: Record<string, unknown>) {
    mockWsClient.call.mockImplementation((method: string) => {
      if (method === 'mcp.oauth.providers') {
        return Promise.resolve({
          providers: [
            {
              provider_id: 'notion',
              name: 'Notion',
              auth_url: 'https://api.notion.com/v1/oauth/authorize',
              scopes: [],
              configured: true,
              expires_at: null,
              client_id: 'notion-client-1234',
              has_client_secret: true,
              client_secret_masked: '••••abcd',
              ...extra,
            },
          ],
        });
      }
      return Promise.resolve({ agents: [], catalog: [], providers: [] });
    });
  }

  it('shows the stored client id and masked secret on a configured provider', async () => {
    mockProvider({ token_status: 'authenticated' });
    renderWithProviders(<McpPage />);

    expect(await screen.findByText('notion-client-1234')).toBeInTheDocument();
    expect(screen.getByText('Saved ••••abcd')).toBeInTheDocument();
    // Editing credentials no longer requires revoking first.
    expect(screen.getByRole('button', { name: /edit credentials/i })).toBeInTheDocument();
  });

  it('reads the legacy `status` key so a connected provider is not shown as unauthenticated', async () => {
    mockProvider({ status: 'authenticated' });
    renderWithProviders(<McpPage />);

    expect(await screen.findByText('Authenticated')).toBeInTheDocument();
  });
});
