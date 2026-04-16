import { create } from 'zustand';
import {
  api,
  type AutopilotRule,
  type AutopilotCreateParams,
  type AutopilotHistoryEntry,
} from '@/lib/api';

interface AutopilotStore {
  readonly rules: ReadonlyArray<AutopilotRule>;
  readonly history: ReadonlyArray<AutopilotHistoryEntry>;
  readonly loading: boolean;
  readonly error: string | null;
  fetchRules: () => Promise<void>;
  createRule: (params: AutopilotCreateParams) => Promise<AutopilotRule | null>;
  toggleRule: (ruleId: string, enabled: boolean) => Promise<void>;
  removeRule: (ruleId: string) => Promise<void>;
  fetchHistory: (ruleId?: string) => Promise<void>;
}

export const useAutopilotStore = create<AutopilotStore>((set, get) => ({
  rules: [],
  history: [],
  loading: false,
  error: null,

  fetchRules: async () => {
    set({ loading: true, error: null });
    try {
      const result = await api.autopilot.list();
      set({ rules: result?.rules ?? [], loading: false });
    } catch (e) {
      set({ error: String(e), loading: false });
    }
  },

  createRule: async (params) => {
    try {
      const result = await api.autopilot.create(params);
      const rule = result.rule;
      set({ rules: [...get().rules, rule] });
      return rule;
    } catch (e) {
      set({ error: String(e) });
      return null;
    }
  },

  toggleRule: async (ruleId, enabled) => {
    // Optimistic update
    const prev = get().rules;
    set({
      rules: prev.map((r) => (r.id === ruleId ? { ...r, enabled } : r)),
    });
    try {
      await api.autopilot.update(ruleId, { enabled });
    } catch (e) {
      set({ rules: prev, error: String(e) });
    }
  },

  removeRule: async (ruleId) => {
    try {
      await api.autopilot.remove(ruleId);
      set({ rules: get().rules.filter((r) => r.id !== ruleId) });
    } catch (e) {
      set({ error: String(e) });
    }
  },

  fetchHistory: async (ruleId) => {
    try {
      const result = await api.autopilot.history(ruleId);
      set({ history: result?.entries ?? [] });
    } catch (e) {
      set({ error: String(e) });
    }
  },
}));
