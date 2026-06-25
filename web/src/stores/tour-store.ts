import { create } from 'zustand';
import { useAuthStore } from '@/stores/auth-store';

/**
 * Guided-tour state. The tour walks a new user through the important pages once,
 * is skippable at any time, and shows only once per user (persisted to
 * localStorage, keyed by user id). `promptPending` is set right after the
 * first agent is created so MainLayout can offer "要不要帶你逛一圈?".
 *
 * Only terminal states (`completed` / `skipped`) are persisted — `running` is
 * transient so an interrupted tour can be re-offered or restarted from Settings.
 */
export type TourStatus = 'unset' | 'running' | 'completed' | 'skipped';

const KEY_PREFIX = 'ddc.tour.v1.';

function tourKey(): string | null {
  const id = useAuthStore.getState().user?.id;
  return id ? KEY_PREFIX + id : null;
}

function persist(status: TourStatus): void {
  const key = tourKey();
  if (!key) return;
  try {
    localStorage.setItem(key, status);
  } catch {
    // localStorage unavailable (private mode / quota) — show-once degrades to
    // per-session only, which is acceptable.
  }
}

interface TourStore {
  readonly status: TourStatus;
  readonly stepIndex: number;
  readonly promptPending: boolean;
  /** User id the current status was hydrated for (avoids cross-account bleed). */
  readonly hydratedFor: string | null;
  hydrate: () => void;
  requestPrompt: () => void;
  dismissPrompt: () => void;
  start: () => void;
  next: () => void;
  back: () => void;
  skip: () => void;
  finish: () => void;
}

export const useTourStore = create<TourStore>((set, get) => ({
  status: 'unset',
  stepIndex: 0,
  promptPending: false,
  hydratedFor: null,

  hydrate: () => {
    const id = useAuthStore.getState().user?.id ?? null;
    if (!id || get().hydratedFor === id) return;
    let status: TourStatus = 'unset';
    try {
      const raw = localStorage.getItem(KEY_PREFIX + id);
      if (raw === 'completed' || raw === 'skipped') status = raw;
    } catch {
      // ignore
    }
    set({ status, hydratedFor: id, stepIndex: 0 });
  },

  // Called after the first agent is created. Only meaningful when the user
  // hasn't already finished/skipped the tour.
  requestPrompt: () => {
    if (get().status === 'unset') set({ promptPending: true });
  },

  dismissPrompt: () => {
    set({ promptPending: false, status: 'skipped' });
    persist('skipped');
  },

  start: () => set({ status: 'running', stepIndex: 0, promptPending: false }),

  next: () => set((s) => ({ stepIndex: s.stepIndex + 1 })),

  back: () => set((s) => ({ stepIndex: Math.max(0, s.stepIndex - 1) })),

  skip: () => {
    set({ status: 'skipped', promptPending: false });
    persist('skipped');
  },

  finish: () => {
    set({ status: 'completed', promptPending: false });
    persist('completed');
  },
}));
