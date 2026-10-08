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

  it('falls back to pasting the callback address when the dashboard address cannot receive the redirect', async () => {
    const onConnected = vi.fn();
    mockWsClient.call.mockImplementation((method: string) => {
      if (method === 'mcp.remote_connect') {
        return Promise.resolve({
          status: 'authorize', authorize_url: 'https://auth.example.com/authorize?x=2', expires_in: 600,
          completion: 'paste', redirect_uri: 'http://127.0.0.1:18789/oauth/mcp/callback',
          agent_id: 'nova', server: 'zapier',
        });
      }
      if (method === 'mcp.remote_complete') return Promise.resolve({ status: 'connected', agent_id: 'nova', server: 'zapier' });
      if (method === 'mcp.remote_status') return Promise.resolve({ servers: [] });
      return Promise.resolve({});
    });
    renderWithProviders(
      <RemoteConnectDialog
        open
        onClose={() => {}}
        onConnected={onConnected}
        agents={agents}
        initial={{ agentId: 'nova', name: 'zapier', url: 'https://mcp.zapier.com/api/v1/connect', auth: 'oauth' }}
      />,
    );
    fireEvent.click(screen.getByRole('button', { name: /^sign in$/i }));
    const box = await screen.findByTestId('remote-paste');
    expect(box).toHaveTextContent('http://127.0.0.1:18789/oauth/mcp/callback');
    const finish = screen.getByRole('button', { name: /finish sign-in/i });
    expect(finish).toBeDisabled();
    const pasted = 'http://127.0.0.1:18789/oauth/mcp/callback?code=c1&state=s1';
    fireEvent.change(screen.getByLabelText(/address bar/i), { target: { value: pasted } });
    fireEvent.click(finish);
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('mcp.remote_complete', { callback_url: pasted }));
    await waitFor(() => expect(onConnected).toHaveBeenCalledWith('nova', 'zapier'));
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

describe('ToolEffectsPanel', () => {
  it('lists third-party tools with their effect class and verdict', async () => {
    mockWsClient.call.mockImplementation((method: string) => {
      if (method === 'mcp.tool_effects') {
        return Promise.resolve({
          servers: [
            {
              server: 'crm', seen_by: 'proxy', observed_at: '2026-10-08T00:00:00Z', read_hint_trusted: false,
              tools: [
                { name: 'get', description: '', annotations: { readOnlyHint: true }, effect: 'modify', verdict: 'ask', explore_visible: false },
                { name: 'drop', description: '', annotations: { destructiveHint: true }, effect: 'delete', verdict: 'block', explore_visible: false },
              ],
            },
          ],
        });
      }
      return Promise.resolve({});
    });
    const { ToolEffectsPanel } = await import('./ToolEffectsPanel');
    renderWithProviders(<ToolEffectsPanel agents={agents} />);
    const rows = await screen.findAllByTestId('tool-effect-row');
    expect(rows).toHaveLength(2);
    expect(mockWsClient.call).toHaveBeenCalledWith('mcp.tool_effects', { agent_id: 'nova' });
    expect(screen.getByText(/labels not trusted/i)).toBeInTheDocument();
    expect(screen.getByText('Never')).toBeInTheDocument();
  });
});

describe('McpEventsPanel', () => {
  it('lists subscriptions and subscribes in read-only mode by default', async () => {
    mockWsClient.call.mockImplementation((method: string) => {
      if (method === 'mcp.events_list') {
        return Promise.resolve({
          public_base_url_set: false,
          public_base_url_problem: 'set it',
          subscriptions: [
            {
              id: 'mev_1', agent_id: 'nova', server: 'pager', event_types: ['incident.created'], mode: 'explore',
              status: 'active', callback_url: null, upstream: [], created_at: '', updated_at: '', rotated_at: null,
              last_delivery_at: null, deliveries: 2,
            },
          ],
        });
      }
      if (method === 'mcp.events_subscribe') return Promise.resolve({ subscription: {} });
      return Promise.resolve({});
    });
    const { McpEventsPanel } = await import('./McpEventsPanel');
    const servers = [
      {
        agent_id: 'nova', server: 'pager', auth: 'none' as const, host: 'x', status: 'connected' as const,
        access_expires_at: null, access_token_expired: false, has_refresh_token: false, installed: true,
        created_at: '', updated_at: '',
      },
    ];
    renderWithProviders(<McpEventsPanel servers={servers} />);
    expect(await screen.findAllByTestId('event-sub-row')).toHaveLength(1);
    expect(screen.getByText(/public_base_url/)).toBeInTheDocument();
    fireEvent.change(screen.getByRole('combobox'), { target: { value: 'nova/pager' } });
    fireEvent.change(screen.getByPlaceholderText(/incident.created/), { target: { value: 'a.b, c' } });
    fireEvent.click(screen.getByRole('button', { name: /^subscribe$/i }));
    await waitFor(() =>
      expect(mockWsClient.call).toHaveBeenCalledWith('mcp.events_subscribe', {
        agent_id: 'nova', server: 'pager', event_types: ['a.b', 'c'], mode: 'explore',
      }),
    );
  });
});

describe('parseHeaderLines', () => {
  it('parses Name: value lines and refuses lines without a colon', async () => {
    const { parseHeaderLines } = await import('./RemoteServersTab');
    expect(parseHeaderLines('X-Workspace: acme\n\nX-Team:  t1 ')).toEqual({ 'X-Workspace': 'acme', 'X-Team': 't1' });
    expect(parseHeaderLines('nocolon')).toBeNull();
    expect(parseHeaderLines(': empty name')).toBeNull();
  });
});
