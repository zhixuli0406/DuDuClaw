import { create } from 'zustand';

/**
 * Top-level shell mode (TODO-genspark-workspace-shell §P0.1).
 *
 * - `workspace` — Genspark-style consumer shell: one prompt bar + launcher grid.
 * - `dashboard` — the full power-user Calm Glass dashboard (existing behavior).
 *
 * The preference is persisted; the default is decided by `defaultMode()` from
 * the edition profile when the user has never chosen (see §P0.2).
 */
export type UiMode = 'workspace' | 'dashboard';

const STORAGE_KEY = 'duduclaw-ui-mode';

/** Read the persisted choice, or `null` when the user has never chosen. */
export function storedMode(): UiMode | null {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw === 'workspace' || raw === 'dashboard') return raw;
  } catch {
    /* storage unavailable (private mode / quota) — fall through */
  }
  return null;
}

/**
 * Default mode when there is no stored preference.
 * Personal edition → `workspace` (zero-friction); enterprise / unknown →
 * `dashboard` (power users keep the full console). An absent profile is treated
 * as enterprise to avoid surprising existing multi-seat installs.
 */
export function defaultMode(editionProfile: string | null | undefined): UiMode {
  return editionProfile === 'personal' ? 'workspace' : 'dashboard';
}

/** Resolve the effective initial mode: stored choice wins over the default. */
export function resolveInitialMode(
  editionProfile: string | null | undefined,
): UiMode {
  return storedMode() ?? defaultMode(editionProfile);
}

interface UiModeStore {
  readonly mode: UiMode;
  /** True once a preference has been explicitly stored or set. */
  readonly chosen: boolean;
  setMode: (mode: UiMode) => void;
  toggle: () => void;
  /** Apply the edition-derived default only when the user hasn't chosen yet. */
  initFromEdition: (editionProfile: string | null | undefined) => void;
}

export const useUiModeStore = create<UiModeStore>((set, get) => {
  const stored = storedMode();
  return {
    // Before the edition profile is known we land on `dashboard` (the safe,
    // feature-complete surface); `initFromEdition` upgrades to `workspace` for
    // personal installs that haven't chosen yet.
    mode: stored ?? 'dashboard',
    chosen: stored !== null,

    setMode: (mode) => {
      try {
        localStorage.setItem(STORAGE_KEY, mode);
      } catch {
        /* ignore quota / private-mode failures */
      }
      set({ mode, chosen: true });
    },

    toggle: () => {
      get().setMode(get().mode === 'workspace' ? 'dashboard' : 'workspace');
    },

    initFromEdition: (editionProfile) => {
      // Respect an explicit choice; only seed the default once.
      if (get().chosen) return;
      set({ mode: defaultMode(editionProfile) });
    },
  };
});
