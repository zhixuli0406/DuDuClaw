import { create } from 'zustand';

/**
 * Settings saved from the dashboard that only take effect after the gateway
 * restarts (v1.68, W2). Every save that returns `restart_required` adds its
 * keys here; the banner stays up across pages and reloads until the gateway
 * has actually restarted (uptime shorter than the time since the first
 * pending save) or the operator dismisses it.
 */

const STORAGE_KEY = 'duduclaw.restartRequired';

interface Persisted {
  keys: string[];
  since: number | null;
}

function readPersisted(): Persisted {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return { keys: [], since: null };
    const parsed = JSON.parse(raw) as Partial<Persisted>;
    const keys = Array.isArray(parsed.keys) ? parsed.keys.filter((k) => typeof k === 'string') : [];
    const since = typeof parsed.since === 'number' ? parsed.since : null;
    return { keys, since: keys.length > 0 ? since : null };
  } catch {
    return { keys: [], since: null };
  }
}

function writePersisted(p: Persisted) {
  try {
    if (p.keys.length === 0) localStorage.removeItem(STORAGE_KEY);
    else localStorage.setItem(STORAGE_KEY, JSON.stringify(p));
  } catch {
    /* private mode / quota — the banner still works for this session */
  }
}

interface RestartRequiredState extends Persisted {
  add: (keys: readonly string[]) => void;
  clear: () => void;
  /** Clear when the gateway's uptime shows it restarted after the saves. */
  reconcileUptime: (uptimeSeconds: number, now?: number) => void;
}

export const useRestartRequiredStore = create<RestartRequiredState>((set, get) => ({
  ...readPersisted(),
  add: (keys) => {
    const fresh = keys.filter((k) => k !== '');
    if (fresh.length === 0) return;
    const cur = get();
    const merged = [...cur.keys, ...fresh.filter((k) => !cur.keys.includes(k))];
    const next = { keys: merged, since: cur.since ?? Date.now() };
    writePersisted(next);
    set(next);
  },
  clear: () => {
    writePersisted({ keys: [], since: null });
    set({ keys: [], since: null });
  },
  reconcileUptime: (uptimeSeconds, now = Date.now()) => {
    const { since } = get();
    if (since === null || !Number.isFinite(uptimeSeconds)) return;
    const startedAt = now - uptimeSeconds * 1000;
    if (startedAt > since) get().clear();
  },
}));
