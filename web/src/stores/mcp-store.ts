import { create } from 'zustand';
import { api, type McpAgentConfig, type McpCatalogItem, type McpServerDef } from '@/lib/api';

interface McpStore {
  readonly agentConfigs: ReadonlyArray<McpAgentConfig>;
  readonly catalog: ReadonlyArray<McpCatalogItem>;
  readonly loading: boolean;
  readonly error: string | null;
  fetchAll: () => Promise<void>;
  addServer: (agentId: string, name: string, def: McpServerDef) => Promise<void>;
  removeServer: (agentId: string, name: string) => Promise<void>;
}

export const useMcpStore = create<McpStore>((set, get) => ({
  agentConfigs: [],
  catalog: [],
  loading: false,
  error: null,
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
}));
