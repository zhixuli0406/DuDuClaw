import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen, fireEvent, waitFor } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { useAuthStore } from '@/stores/auth-store';
import { McpRegistryTab } from './McpRegistryTab';
import { RemoteConnectDialog, isValidServerName, sanitizeServerName } from './RemoteServersTab';

vi.mock('@/lib/external-link', () => ({ openExternal: vi.fn() }));
import { openExternal } from '@/lib/external-link';

const agents = [{ name: 'nova', display_name: 'Nova' }];

beforeEach(() => {
  vi.clearAllMocks();
  useAuthStore.setState({ user: { display_name: 'Boss', role: 'admin' }, bindings: [] } as never);
});

describe('server name helpers', () => {
  it('sanitizes like the gateway and reserves duduclaw*', () => {
    expect(sanitizeServerName('com.zapier/mcp')).toBe('com.zapier-mcp');
    expect(sanitizeServerName('///')).toBe('remote');
    expect(isValidServerName('zapier')).toBe(true);
    expect(isValidServerName('duduclaw-x')).toBe(false);
    expect(isValidServerName('a b')).toBe(false);
  });
});

describe('McpRegistryTab', () => {
  it('searches and shows installable and non-installable results', async () => {
    mockWsClient.call.mockImplementation((method: string) => {
      if (method === 'mcp.registry_search') {
        return Promise.resolve({
          servers: [
            {
              name: 'com.zapier/mcp', title: 'Zapier', description: 'Hosted', version: '1.0.1',
              package_kinds: [], has_remotes: true, remote_kinds: ['streamable-http'], remote_needs_bearer: false,
              repository_url: 'https://github.com/zapier/zapier-mcp', website_url: null, required_env: [],
              installable: true, reason: null, install_is_remote: true,
            },
            {
              name: 'io.example/legacy', title: '', description: 'Old', version: '0.1',
              package_kinds: [], has_remotes: true, remote_kinds: ['sse'], remote_needs_bearer: false,
              repository_url: null, website_url: null, required_env: [],
              installable: false, reason: 'sse_remote_only', install_is_remote: false,
            },
          ],
          next_cursor: null,
          cached: false,
        });
      }
      return Promise.resolve({});
    });
    renderWithProviders(<McpRegistryTab agents={agents} onInstalled={() => {}} />);
    fireEvent.change(screen.getByRole('textbox'), { target: { value: 'zapier' } });
    fireEvent.click(screen.getByRole('button', { name: /search/i }));
    const rows = await screen.findAllByTestId('registry-row');
    expect(rows).toHaveLength(2);
    expect(mockWsClient.call).toHaveBeenCalledWith('mcp.registry_search', { query: 'zapier' });
    expect(screen.getByText('Zapier')).toBeInTheDocument();
    expect(screen.getByText(/legacy SSE/i)).toBeInTheDocument();
    const installButtons = screen.getAllByRole('button', { name: /install to agent/i });
    expect(installButtons[0]).not.toBeDisabled();
    expect(installButtons[1]).toBeDisabled();
  });
});

describe('RemoteConnectDialog', () => {
  it('starts OAuth with the dashboard origin, opens the sign-in page and shows the third-party notice', async () => {
    mockWsClient.call.mockImplementation((method: string) => {
      if (method === 'mcp.remote_connect') {
        return Promise.resolve({
          status: 'authorize', authorize_url: 'https://auth.example.com/authorize?x=1', expires_in: 600,
          agent_id: 'nova', server: 'zapier',
        });
      }
      if (method === 'mcp.remote_status') return Promise.resolve({ servers: [] });
      return Promise.resolve({});
    });
    renderWithProviders(
      <RemoteConnectDialog
        open
        onClose={() => {}}
        agents={agents}
        initial={{ agentId: 'nova', name: 'zapier', url: 'https://mcp.zapier.com/api/v1/connect', auth: 'oauth', provider: 'Zapier', thirdParty: true }}
      />,
    );
    expect(screen.getByRole('note')).toHaveTextContent(/passes through Zapier/);
    fireEvent.click(screen.getByRole('button', { name: /^sign in$/i }));
    await waitFor(() => expect(openExternal).toHaveBeenCalledWith('https://auth.example.com/authorize?x=1'));
    expect(mockWsClient.call).toHaveBeenCalledWith('mcp.remote_connect', expect.objectContaining({
      agent_id: 'nova',
      name: 'zapier',
      auth: 'oauth',
      url: 'https://mcp.zapier.com/api/v1/connect',
      redirect_origin: window.location.origin,
    }));
    expect(await screen.findByTestId('remote-waiting')).toBeInTheDocument();
  });

  it('requires a token for bearer and never sends one otherwise', async () => {
    mockWsClient.call.mockResolvedValue({ status: 'connected', agent_id: 'nova', server: 'srv' });
    renderWithProviders(
      <RemoteConnectDialog open onClose={() => {}} agents={agents} initial={{ agentId: 'nova', name: 'srv', url: 'https://x.example/mcp', auth: 'bearer' }} />,
    );
    const submit = screen.getByRole('button', { name: /^connect$/i });
    expect(submit).toBeDisabled();
    fireEvent.change(screen.getByLabelText(/token/i), { target: { value: 'tok-1' } });
    expect(submit).not.toBeDisabled();
    fireEvent.click(submit);
    await waitFor(() =>
      expect(mockWsClient.call).toHaveBeenCalledWith('mcp.remote_connect', expect.objectContaining({ auth: 'bearer', bearer: 'tok-1' })),
    );
    const call = mockWsClient.call.mock.calls.find((c) => c[0] === 'mcp.remote_connect');
    expect(call?.[1]).not.toHaveProperty('redirect_origin');
  });
});

describe('McpPage remote catalogue cards (E3)', () => {
  it('renders an aggregator card with Connect instead of Install', async () => {
    const { McpPage } = await import('@/pages/McpPage');
    const { useConnectionStore } = await import('@/stores/connection-store');
    useConnectionStore.setState({ state: 'authenticated' as never, error: null });
    mockWsClient.call.mockImplementation((method: string) => {
      if (method === 'mcp.list') {
        return Promise.resolve({
          agents: [],
          catalog: [
            {
              id: 'zapier', name: 'Zapier', description: 'Thousands of apps via Zapier (hosted)', category: 'aggregator',
              requires_oauth: true, default_def: { command: 'duduclaw', args: ['mcp-remote-bridge'], env: {} }, required_env: [],
              remote: { url_hint: 'https://mcp.zapier.com/api/v1/connect', auth: 'oauth', docs_url: 'https://docs.zapier.com/mcp/home', third_party: true },
            },
          ],
        });
      }
      return Promise.resolve({ providers: [], servers: [] });
    });
    renderWithProviders(<McpPage />);
    fireEvent.click(await screen.findByRole('radio', { name: /marketplace/i }).catch(() => screen.getByText('Marketplace')));
    expect(await screen.findByText('Zapier')).toBeInTheDocument();
    expect(screen.getByText(/Hosted by Zapier/)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /^connect$/i })).not.toBeDisabled();
    expect(screen.queryByRole('button', { name: /install to agent/i })).toBeNull();
  });
});
