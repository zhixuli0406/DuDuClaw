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
  /**
   * The raw thrown value from the last failed call. Pages render it through
   * `ErrorState` / `useErrorMessage` (plain language + sanitized detail), so it
   * is deliberately not pre-flattened here.
   */
  readonly error: unknown;
  clearError: () => void;
  fetchAgents: (includeArchived?: boolean) => Promise<void>;
  setIncludeArchived: (v: boolean) => Promise<void>;
  selectAgent: (id: string | null) => void;
  pauseAgent: (id: string) => Promise<void>;
  resumeAgent: (id: string) => Promise<void>;
  updateAgent: (
    id: string,
    fields: AgentUpdateParams,
  ) => Promise<{ success: boolean; runtime_provider_aligned?: string | null } | undefined>;
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
        set({ error: e, loading: false, loaded: true });
      }
    },
    setIncludeArchived: async (v) => {
      set({ includeArchived: v });
      await get().fetchAgents(v);
    },
    clearError: () => set({ error: null }),
    selectAgent: (id) => set({ selectedAgentId: id }),
    pauseAgent: async (id) => {
      try {
        await api.agents.pause(id);
        set({
          agents: get().agents.map((a) =>
            a.name === id ? { ...a, status: 'paused' } : a
          ),
        });
      } catch (e) {
        // Swallowing this used to resolve normally, which let the detail page
        // toast "已讓他休息" for a call that never landed (P05 Blocker).
        set({ error: e });
        throw e;
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
      } catch (e) {
        set({ error: e });
        throw e;
      }
    },
    updateAgent: async (id, fields) => {
      try {
        const res = await api.agents.update(id, fields);
        // Re-fetch to get the authoritative state after update
        await get().fetchAgents();
        return res;
      } catch (e) {
        set({ error: e });
        return undefined;
      }
    },
    removeAgent: async (id) => {
      try {
        await api.agents.remove(id);
        set({ agents: get().agents.filter((a) => a.name !== id) });
      } catch (e) {
        set({ error: e });
        throw e;
      }
    },
    archiveAgent: async (id) => {
      // Previously had no try/catch at all: a rejected archive surfaced as an
      // unhandled rejection and the user saw nothing (P05 Blocker).
      try {
        await api.agents.archive(id);
      } catch (e) {
        set({ error: e });
        throw e;
      }
      // Re-fetch so the archived state (and visibility) is authoritative.
      await get().fetchAgents();
    },
    unarchiveAgent: async (id) => {
      try {
        await api.agents.unarchive(id);
      } catch (e) {
        set({ error: e });
        throw e;
      }
      await get().fetchAgents();
    },
    handoffAgent: async (params) => {
      const res = await api.agents.handoff(params);
      await get().fetchAgents();
      return res;
    },
  };
});
