import { create } from 'zustand';
import { api, type McpAgentConfig, type McpCatalogItem, type McpServerDef, type McpOAuthProvider } from '@/lib/api';

interface McpStore {
  readonly agentConfigs: ReadonlyArray<McpAgentConfig>;
  readonly catalog: ReadonlyArray<McpCatalogItem>;
  readonly loading: boolean;
  readonly error: string | null;
  readonly oauthProviders: ReadonlyArray<McpOAuthProvider>;
  /** Internal: tracks the OAuth polling timer so it can be cancelled. */
  _oauthPollTimer?: ReturnType<typeof setTimeout>;
  fetchAll: () => Promise<void>;
  addServer: (agentId: string, name: string, def: McpServerDef) => Promise<void>;
  removeServer: (agentId: string, name: string) => Promise<void>;
  fetchOAuthProviders: () => Promise<void>;
  startOAuth: (providerId: string, clientId?: string, clientSecret?: string) => Promise<string>;
  revokeOAuth: (providerId: string) => Promise<void>;
}

export const useMcpStore = create<McpStore>((set, get) => ({
  agentConfigs: [],
  catalog: [],
  loading: false,
  error: null,
  oauthProviders: [],
  fetchAll: async () => {
    set({ loading: true, error: null });
    try {
      const result = await api.mcp.list();
      set({
        agentConfigs: result?.agents ?? [],
        catalog: result?.catalog ?? [],
        loading: false,
      });
    } catch (e) {
      set({ error: String(e), loading: false });
    }
  },
  addServer: async (agentId, name, def) => {
    try {
      await api.mcp.update(agentId, 'add', name, def);
      // Re-fetch to get the authoritative state after update
      await get().fetchAll();
    } catch {
      set({ error: 'mcp.loadFailed' });
    }
  },
  removeServer: async (agentId, name) => {
    try {
      await api.mcp.update(agentId, 'remove', name);
      set({
        agentConfigs: get().agentConfigs.map((cfg) =>
          cfg.agent_id === agentId
            ? { ...cfg, servers: cfg.servers.filter((s) => s.name !== name) }
            : cfg
        ),
      });
    } catch {
      set({ error: 'mcp.loadFailed' });
    }
  },
  fetchOAuthProviders: async () => {
    try {
      const result = await api.mcp.oauthProviders();
      set({ oauthProviders: result?.providers ?? [] });
    } catch {
      set({ error: 'mcp.loadFailed' });
    }
  },
  startOAuth: async (providerId, clientId?, clientSecret?) => {
    // Recorded before consent opens. `authenticated` is true for a stale but
    // refreshable token, so re-authorizing an already-connected provider would
    // otherwise report success on the first poll — off the old token that was
    // already on file. Success has to mean a token that actually moved.
    const baseline =
      get().oauthProviders.find((p) => p.provider_id === providerId)?.expires_at ?? null;
    const result = await api.mcp.oauthStart(providerId, clientId, clientSecret);
    const authUrl = result.auth_url;

    // Cancel any existing polling
    const prev = get()._oauthPollTimer;
    if (prev) clearTimeout(prev);

    // Start polling for auth completion (every 3s, up to 5 minutes)
    const maxAttempts = 100;
    let attempt = 0;
    const poll = () => {
      attempt += 1;
      if (attempt > maxAttempts) {
        set({ _oauthPollTimer: undefined });
        return;
      }
      const timer = setTimeout(async () => {
        try {
          const status = await api.mcp.oauthStatus(providerId);
          const fresh =
            status.access_token_valid === true ||
            (status.access_token_valid === undefined && status.authenticated) ||
            (!!status.expires_at && !!baseline && status.expires_at > baseline);
          if (fresh) {
            set({ _oauthPollTimer: undefined });
            await get().fetchOAuthProviders();
            return;
          }
          poll();
        } catch {
          set({ _oauthPollTimer: undefined });
        }
      }, 3000);
      set({ _oauthPollTimer: timer });
    };
    poll();

    return authUrl;
  },
  revokeOAuth: async (providerId) => {
    await api.mcp.oauthRevoke(providerId);
    await get().fetchOAuthProviders();
  },
}));
