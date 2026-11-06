import { useCallback, useEffect, useRef } from 'react';
import { flushSync } from 'react-dom';
import { create } from 'zustand';
import { api, type CallableAgent } from '@/lib/api';
import { subscribeInvalidations } from '@/lib/live-events';
import { useAuthStore } from './auth-store';
import { useSystemStore } from './system-store';

interface Entry {
  agents: readonly CallableAgent[] | null;
  loading: boolean;
  unrestricted: boolean;
  /** Monotonic response revision; retained display grants are stale while loading. */
  confirmedAt?: number;
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
let confirmation = 0;
export const CALLABLE_TIMEOUT_MS = 15_000;
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
    let timeout: ReturnType<typeof setTimeout> | undefined;
    try {
      const result = await Promise.race([
        api.agents.callable(sessionId),
        new Promise<never>((_, reject) => {
          timeout = setTimeout(() => reject(new Error('Callable grants timed out')), CALLABLE_TIMEOUT_MS);
        }),
      ]);
      if (Array.isArray(result?.agents)) {
        agents = result.agents;
        unrestricted = result.unrestricted === true;
      }
    } catch { /* Failed grants never fall back to the detailed roster. */ }
    finally { clearTimeout(timeout); }
    if (ticket !== generation) return;
    set(s => ({ entries: { ...s.entries, [key]: { agents, unrestricted, loading: false, confirmedAt: (confirmation = Math.max(Date.now(), confirmation + 1)) } } }));
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
    if (event.target instanceof Element && event.target.closest('option, [role="option"], [role="menuitemradio"]')) return;
    const trigger = event.target instanceof Element
      ? event.target.closest('select, [role="combobox"], [aria-haspopup="listbox"], [aria-haspopup="menu"]')
      : null;
    // Text comboboxes open suggestions through their own activation hook.
    if (trigger?.matches('input, textarea')) return;
    if (trigger && trigger.getAttribute('aria-expanded') !== 'true') {
      flushSync(() => { void refresh(); });
    }
  };
  document.addEventListener('click', onPickerOpen, true);
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

/** Imperative fence for callbacks that may retain a previously chosen agent. */
export function canSelectAgent(name: string, sessionId?: string | null): boolean {
  const key = JSON.stringify([callableSubject(), sessionId ?? null]);
  const entry = useCallableAgentsStore.getState().entries[key];
  return !!entry && !entry.loading && entry.agents !== null &&
    (entry.unrestricted || entry.agents.some(a => a.name === name && a.selectable === true));
}

/** A selection activation must observe a response newer than its opening render. */
export function useSelectableCallableAgents(sessionId?: string | null, active = true) {
  const grants = useCallableAgents(sessionId);
  const activation = useRef({ active: false, sessionId, after: 0 });
  if (active && (!activation.current.active || activation.current.sessionId !== sessionId)) {
    activation.current = { active, sessionId, after: grants.confirmedAt ?? 0 };
  }
  activation.current.active = active;
  useEffect(() => { if (active) void grants.refresh(); }, [active, grants.refresh]);
  const fresh = active && !grants.loading && (grants.confirmedAt ?? 0) > activation.current.after;
  return fresh ? grants : { ...empty, loading: active, refresh: grants.refresh };
}
