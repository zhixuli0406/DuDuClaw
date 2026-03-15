import { create } from 'zustand';
import { client } from '@/lib/ws-client';
import { api, type LogEntry } from '@/lib/api';

const MAX_LOGS = 5000;

interface LogsStore {
  readonly entries: ReadonlyArray<LogEntry>;
  readonly paused: boolean;
  readonly filter: { level: string | null; agentId: string | null; keyword: string };
  subscribe: () => void;
  unsubscribe: () => void;
  togglePause: () => void;
  setFilter: (filter: Partial<LogsStore['filter']>) => void;
  clear: () => void;
  readonly filteredEntries: ReadonlyArray<LogEntry>;
}

export const useLogsStore = create<LogsStore>((set, get) => {
  let unsubscribeFn: (() => void) | null = null;

  return {
    entries: [],
    paused: false,
    filter: { level: null, agentId: null, keyword: '' },
    subscribe: () => {
      api.logs.subscribe().catch(() => {});
      unsubscribeFn = client.subscribe('logs.entry', (payload) => {
        if (get().paused) return;
        const entry = payload as LogEntry;
        const entries = [...get().entries, entry];
        // Keep ring buffer
        if (entries.length > MAX_LOGS) entries.splice(0, entries.length - MAX_LOGS);
        set({ entries });
      });
    },
    unsubscribe: () => {
      api.logs.unsubscribe().catch(() => {});
      unsubscribeFn?.();
      unsubscribeFn = null;
    },
    togglePause: () => set({ paused: !get().paused }),
    setFilter: (f) => set({ filter: { ...get().filter, ...f } }),
    clear: () => set({ entries: [] }),
    get filteredEntries() {
      const { entries, filter } = get();
      return entries.filter((e) => {
        if (filter.level && e.level !== filter.level) return false;
        if (filter.agentId && e.agent_id !== filter.agentId) return false;
        if (filter.keyword && !e.message.toLowerCase().includes(filter.keyword.toLowerCase())) return false;
        return true;
      });
    },
  };
});
