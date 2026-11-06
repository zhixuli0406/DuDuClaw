import { useCallback, useEffect } from 'react';
import { create } from 'zustand';
import { api, type CallableAgent } from '@/lib/api';
import { subscribeInvalidations } from '@/lib/live-events';
import { useAuthStore } from './auth-store';
import { useSystemStore } from './system-store';

interface Entry {
  agents: readonly CallableAgent[] | null;
  loading: boolean;
  unrestricted: boolean;
}
interface CallableStore {
  entries: Record<string, Entry>;
  load: (key: string, sessionId?: string | null, refresh?: boolean) => Promise<void>;
}
const empty: Entry = { agents: null, loading: false, unrestricted: false };
export const CALLABLE_REFRESH_INTERVAL_MS = 60_000;
let refreshTimer: ReturnType<typeof setInterval> | undefined;
const sessions = new Map<string, string | null | undefined>();
let generation = 0;
export const useCallableAgentsStore = create<CallableStore>((set, get) => ({
  entries: {},
  load: async (key, sessionId, refresh = false) => {
    const entry = get().entries[key];
    if (entry?.loading || (entry && !refresh)) return;
    sessions.set(key, sessionId);
    syncRefreshTimer();
    const ticket = generation;
    set(s => ({ entries: { ...s.entries, [key]: { ...(entry ?? empty), loading: true } } }));
    let agents: readonly CallableAgent[] | null = null;
    let unrestricted = false;
    try {
      const result = await api.agents.callable(sessionId);
      if (Array.isArray(result?.agents)) {
        agents = result.agents;
        unrestricted = result.unrestricted === true;
      }
    } catch { /* Failed grants never fall back to the detailed roster. */ }
    if (ticket !== generation) return;
    set(s => ({ entries: { ...s.entries, [key]: { agents, unrestricted, loading: false } } }));
  },
}));

export function callableSubject() {
  const { user, bindings, jwt } = useAuthStore.getState();
  return JSON.stringify([user?.id, user?.role, user?.status, user?.primary_agent_id,
    bindings.map(b => [b.agent_name, b.access_level]).sort(), jwt,
    useSystemStore.getState().status?.edition_profile]);
}
function invalidate() {
  generation++;
  sessions.clear();
  syncRefreshTimer();
  useCallableAgentsStore.setState({ entries: {} });
}
useAuthStore.subscribe((next, prev) => {
  if (next.user !== prev.user || next.bindings !== prev.bindings || next.jwt !== prev.jwt) invalidate();
});
useSystemStore.subscribe((next, prev) => {
  if (next.status?.edition_profile !== prev.status?.edition_profile) invalidate();
});
async function refresh() {
  await Promise.all([...sessions].map(([key, session]) => useCallableAgentsStore.getState().load(key, session, true)));
}
subscribeInvalidations(['agent.status_changed', 'events_lost', 'connection.ready'], refresh);
function syncRefreshTimer() {
  const visible = typeof document !== 'undefined' && document.visibilityState === 'visible';
  if ((!visible || sessions.size === 0) && refreshTimer !== undefined) {
    clearInterval(refreshTimer);
    refreshTimer = undefined;
  }
  if (visible && sessions.size > 0 && refreshTimer === undefined) {
    refreshTimer = setInterval(() => { void refresh(); }, CALLABLE_REFRESH_INTERVAL_MS);
  }
}
if (typeof window !== 'undefined') window.addEventListener('focus', () => {
  if (document.visibilityState === 'visible') void refresh();
});
if (typeof document !== 'undefined') {
  document.addEventListener('visibilitychange', () => {
    syncRefreshTimer();
    if (document.visibilityState === 'visible') void refresh();
  });
  // Native selects, MDS selects and custom agent popovers share these semantics.
  // Refresh on opening gestures, not on option selection or text entry.
  const onPickerOpen = (event: Event) => {
    if (event instanceof KeyboardEvent && !['Enter', ' ', 'ArrowDown', 'ArrowUp'].includes(event.key)) return;
    const trigger = event.target instanceof Element
      ? event.target.closest('select, [role="combobox"], [aria-haspopup="listbox"], [aria-haspopup="menu"]')
      : null;
    if (trigger && trigger.getAttribute('aria-expanded') !== 'true') void refresh();
  };
  document.addEventListener('pointerdown', onPickerOpen, true);
  document.addEventListener('keydown', onPickerOpen, true);
}

/** Identity-fenced grants. Initial loads and failures expose no grants; refreshes retain confirmed grants. */
export function useCallableAgents(sessionId?: string | null) {
  useAuthStore(s => s.user);
  useAuthStore(s => s.bindings);
  useAuthStore(s => s.jwt);
  useSystemStore(s => s.status?.edition_profile);
  const key = JSON.stringify([callableSubject(), sessionId ?? null]);
  const entry = useCallableAgentsStore(s => s.entries[key]);
  const load = useCallableAgentsStore(s => s.load);
  useEffect(() => { void load(key, sessionId); }, [key, sessionId, entry === undefined, load]);
  const refresh = useCallback(() => load(key, sessionId, true), [key, sessionId, load]);
  return { ...(entry ?? empty), refresh };
}
