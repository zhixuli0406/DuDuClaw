import { create } from 'zustand';
import { api, type AgentDetail, type AgentUpdateParams, type AgentHandoffParams, type AgentHandoffResult } from '@/lib/api';
import { client } from '@/lib/ws-client';
import { useAgentAvatarStore } from './agent-avatar-store';

interface AgentsStore {
  readonly agents: ReadonlyArray<AgentDetail>;
  readonly selectedAgentId: string | null;
  readonly loading: boolean;
  /** WP4 — whether the roster currently includes archived AI staff. */
  readonly includeArchived: boolean;
  /** True once `fetchAgents` has resolved at least once. Distinguishes
   *  "never loaded" from "loaded empty" so the first-run gate never redirects
   *  on the initial empty array before the first list call returns. */
  readonly loaded: boolean;
  readonly error: string | null;
  fetchAgents: (includeArchived?: boolean) => Promise<void>;
  setIncludeArchived: (v: boolean) => Promise<void>;
  selectAgent: (id: string | null) => void;
  pauseAgent: (id: string) => Promise<void>;
  resumeAgent: (id: string) => Promise<void>;
  updateAgent: (id: string, fields: AgentUpdateParams) => Promise<void>;
  removeAgent: (id: string) => Promise<void>;
  /** WP4 — archive (recoverable). Rejected by the backend for the main agent. */
  archiveAgent: (id: string) => Promise<void>;
  /** WP4 — restore an archived agent. */
  unarchiveAgent: (id: string) => Promise<void>;
  /** WP4 — transfer memory/wiki/tasks then archive. Returns the raw result so
   *  callers can honestly surface a PARTIAL outcome. */
  handoffAgent: (params: AgentHandoffParams) => Promise<AgentHandoffResult>;
}

export const useAgentsStore = create<AgentsStore>((set, get) => {
  // Subscribe to agent status change events
  client.subscribe('agent.status_changed', (payload) => {
    const data = payload as { agent_id: string; new_status: string };
    set({
      agents: get().agents.map((a) =>
        a.name === data.agent_id
          ? { ...a, status: data.new_status as AgentDetail['status'] }
          : a
      ),
    });
  });

  return {
    agents: [],
    selectedAgentId: null,
    loading: false,
    includeArchived: false,
    loaded: false,
    error: null,
    fetchAgents: async (includeArchived) => {
      const withArchived = includeArchived ?? get().includeArchived;
      set({ loading: true, error: null, includeArchived: withArchived });
      try {
        const result = await api.agents.list({ include_archived: withArchived });
        const agents = result?.agents ?? [];
        // Seed the avatar cache so uploaded images resolve everywhere.
        useAgentAvatarStore.getState().seed(agents);
        set({ agents, loading: false, loaded: true });
      } catch (e) {
        set({ error: String(e), loading: false, loaded: true });
      }
    },
    setIncludeArchived: async (v) => {
      set({ includeArchived: v });
      await get().fetchAgents(v);
    },
    selectAgent: (id) => set({ selectedAgentId: id }),
    pauseAgent: async (id) => {
      try {
        await api.agents.pause(id);
        set({
          agents: get().agents.map((a) =>
            a.name === id ? { ...a, status: 'paused' } : a
          ),
        });
      } catch {
        set({ error: 'agents.error.pause' });
      }
    },
    resumeAgent: async (id) => {
      try {
        await api.agents.resume(id);
        set({
          agents: get().agents.map((a) =>
            a.name === id ? { ...a, status: 'active' } : a
          ),
        });
      } catch {
        set({ error: 'agents.error.resume' });
      }
    },
    updateAgent: async (id, fields) => {
      try {
        await api.agents.update(id, fields);
        // Re-fetch to get the authoritative state after update
        await get().fetchAgents();
      } catch {
        set({ error: 'agents.error.update' });
      }
    },
    removeAgent: async (id) => {
      try {
        await api.agents.remove(id);
        set({ agents: get().agents.filter((a) => a.name !== id) });
      } catch (e) {
        set({ error: 'agents.error.remove' });
        throw e;
      }
    },
    archiveAgent: async (id) => {
      await api.agents.archive(id);
      // Re-fetch so the archived state (and visibility) is authoritative.
      await get().fetchAgents();
    },
    unarchiveAgent: async (id) => {
      await api.agents.unarchive(id);
      await get().fetchAgents();
    },
    handoffAgent: async (params) => {
      const res = await api.agents.handoff(params);
      await get().fetchAgents();
      return res;
    },
  };
});
