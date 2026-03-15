import { create } from 'zustand';
import { api, type AgentDetail } from '@/lib/api';
import { client } from '@/lib/ws-client';

interface AgentsStore {
  readonly agents: ReadonlyArray<AgentDetail>;
  readonly selectedAgentId: string | null;
  readonly loading: boolean;
  readonly error: string | null;
  fetchAgents: () => Promise<void>;
  selectAgent: (id: string | null) => void;
  pauseAgent: (id: string) => Promise<void>;
  resumeAgent: (id: string) => Promise<void>;
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
    error: null,
    fetchAgents: async () => {
      set({ loading: true, error: null });
      try {
        const result = await api.agents.list();
        set({ agents: result.agents, loading: false });
      } catch (e) {
        set({ error: String(e), loading: false });
      }
    },
    selectAgent: (id) => set({ selectedAgentId: id }),
    pauseAgent: async (id) => {
      await api.agents.pause(id);
      set({
        agents: get().agents.map((a) =>
          a.name === id ? { ...a, status: 'paused' } : a
        ),
      });
    },
    resumeAgent: async (id) => {
      await api.agents.resume(id);
      set({
        agents: get().agents.map((a) =>
          a.name === id ? { ...a, status: 'active' } : a
        ),
      });
    },
  };
});
