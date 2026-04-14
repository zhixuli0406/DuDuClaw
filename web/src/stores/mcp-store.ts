import { create } from 'zustand';
import { api, type McpAgentConfig, type McpCatalogItem, type McpServerDef, type McpOAuthProvider } from '@/lib/api';

interface McpStore {
  readonly agentConfigs: ReadonlyArray<McpAgentConfig>;
  readonly catalog: ReadonlyArray<McpCatalogItem>;
  readonly loading: boolean;
  readonly error: string | null;
  readonly oauthProviders: ReadonlyArray<McpOAuthProvider>;
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
            ? {
                ...cfg,
                servers: Object.fromEntries(
                  Object.entries(cfg.servers).filter(([k]) => k !== name)
                ),
              }
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
    const result = await api.mcp.oauthStart(providerId, clientId, clientSecret);
    const authUrl = result.auth_url;

    // Start polling for auth completion (every 3s, up to 5 minutes)
    const maxAttempts = 100;
    let attempt = 0;
    const poll = () => {
      attempt += 1;
      if (attempt > maxAttempts) return;
      setTimeout(async () => {
        try {
          const status = await api.mcp.oauthStatus(providerId);
          if (status.authenticated) {
            // Re-fetch all providers to update state
            await get().fetchOAuthProviders();
            return;
          }
          poll();
        } catch {
          // Stop polling on error
        }
      }, 3000);
    };
    poll();

    return authUrl;
  },
  revokeOAuth: async (providerId) => {
    await api.mcp.oauthRevoke(providerId);
    await get().fetchOAuthProviders();
  },
}));
