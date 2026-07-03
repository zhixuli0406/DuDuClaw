import { create } from 'zustand';

/**
 * Command-palette open/close state + a small MRU list of recently visited
 * routes (persisted) so an empty query surfaces "jump back" shortcuts.
 * Kept intentionally tiny — command *content* is derived in the component from
 * the nav model + live stores, not stored here.
 */

const RECENT_KEY = 'duduclaw-cmdk-recent';
const RECENT_MAX = 6;

function loadRecent(): string[] {
  try {
    const raw = JSON.parse(localStorage.getItem(RECENT_KEY) ?? '[]');
    return Array.isArray(raw) ? raw.filter((x) => typeof x === 'string').slice(0, RECENT_MAX) : [];
  } catch {
    return [];
  }
}

function persistRecent(routes: string[]): void {
  try {
    localStorage.setItem(RECENT_KEY, JSON.stringify(routes.slice(0, RECENT_MAX)));
  } catch {
    /* ignore quota / private-mode failures */
  }
}

interface CommandPaletteStore {
  readonly open: boolean;
  readonly recent: readonly string[];
  openPalette: () => void;
  closePalette: () => void;
  toggle: () => void;
  /** Record a visited route as most-recently-used (immutable, de-duped). */
  recordVisit: (route: string) => void;
}

export const useCommandPaletteStore = create<CommandPaletteStore>((set, get) => ({
  open: false,
  recent: loadRecent(),
  openPalette: () => set({ open: true }),
  closePalette: () => set({ open: false }),
  toggle: () => set((s) => ({ open: !s.open })),
  recordVisit: (route) => {
    const prev = get().recent;
    const next = [route, ...prev.filter((r) => r !== route)].slice(0, RECENT_MAX);
    persistRecent(next);
    set({ recent: next });
  },
}));
