import { create } from 'zustand';

/**
 * Developer panel (W3-4, Stripe Workbench pattern E1/E2) open/collapse state.
 *
 * Three-tier collapse, mirroring Stripe's "Maximize → pane → Minimize taskbar
 * → Collapse icon": `expanded` (full pane, resizable height) → `minimized`
 * (a slim bottom taskbar) → `collapsed` (one floating icon, bottom-right).
 * Persisted like `command-palette-store` — manual localStorage read/write,
 * not zustand's `persist` middleware, to match this codebase's existing
 * pattern and keep the failure mode explicit (quota / private-mode writes
 * are swallowed, never thrown into a render).
 *
 * `lastSeenCriticalAt` is the only alert-related field persisted — it is the
 * user's acknowledgement watermark, so a badge cleared before a reload stays
 * cleared. `latestCriticalAt` is re-derived every poll tick and intentionally
 * NOT persisted (a stale "critical" badge surviving a reload after the
 * underlying event aged out of the log would be a false alarm).
 */

export type DevPanelVisibility = 'expanded' | 'minimized' | 'collapsed';
export type DevPanelTab = 'events' | 'notifications' | 'system';

const VISIBILITY_KEY = 'duduclaw-devpanel-visibility';
const HEIGHT_KEY = 'duduclaw-devpanel-height';
const TAB_KEY = 'duduclaw-devpanel-tab';
const LAST_SEEN_CRITICAL_KEY = 'duduclaw-devpanel-critical-seen-at';

export const DEVPANEL_MIN_HEIGHT = 240;
export const DEVPANEL_MAX_HEIGHT = 720;
const DEFAULT_HEIGHT = 360;

function readStorage(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

function writeStorage(key: string, value: string): void {
  try {
    localStorage.setItem(key, value);
  } catch {
    /* ignore quota / private-mode failures — visibility just won't persist */
  }
}

function loadVisibility(): DevPanelVisibility {
  const raw = readStorage(VISIBILITY_KEY);
  if (raw === 'expanded' || raw === 'minimized' || raw === 'collapsed') return raw;
  return 'collapsed';
}

/** Clamp to the panel's supported height range. Exported so callers (the
 *  drag-resize handler) can preview a clamped value without a store round-trip. */
export function clampDevPanelHeight(px: number): number {
  return Math.min(DEVPANEL_MAX_HEIGHT, Math.max(DEVPANEL_MIN_HEIGHT, Math.round(px)));
}

function loadHeight(): number {
  const raw = Number(readStorage(HEIGHT_KEY));
  if (Number.isFinite(raw) && raw > 0) return clampDevPanelHeight(raw);
  return DEFAULT_HEIGHT;
}

function loadTab(): DevPanelTab {
  const raw = readStorage(TAB_KEY);
  if (raw === 'events' || raw === 'notifications' || raw === 'system') return raw;
  return 'events';
}

interface DevPanelStore {
  readonly visibility: DevPanelVisibility;
  readonly paneHeight: number;
  readonly activeTab: DevPanelTab;
  /** Newest critical-event timestamp from the last background poll, or
   *  `null` before the first poll / when none qualify. Not persisted. */
  readonly latestCriticalAt: string | null;
  /** Persisted acknowledgement watermark — see module doc. */
  readonly lastSeenCriticalAt: string | null;
  setVisibility: (v: DevPanelVisibility) => void;
  setPaneHeight: (px: number) => void;
  setActiveTab: (t: DevPanelTab) => void;
  expand: () => void;
  minimize: () => void;
  collapse: () => void;
  /** The `~` hotkey: collapsed/minimized → expanded; expanded → minimized.
   *  Deliberately never jumps straight to fully collapsed — that's a
   *  separate, more deliberate "get out of my way" action via its own button. */
  toggleQuick: () => void;
  /** Called by the background alert poller with the newest critical
   *  timestamp it observed (or `null` if none qualified this tick). */
  reportCriticalPoll: (latestAt: string | null) => void;
  /** Marks everything critical up to now as seen — clears the badge. */
  acknowledgeCritical: () => void;
}

export const useDevPanelStore = create<DevPanelStore>((set, get) => ({
  visibility: loadVisibility(),
  paneHeight: loadHeight(),
  activeTab: loadTab(),
  latestCriticalAt: null,
  lastSeenCriticalAt: readStorage(LAST_SEEN_CRITICAL_KEY),

  setVisibility: (v) => {
    writeStorage(VISIBILITY_KEY, v);
    set({ visibility: v });
  },
  setPaneHeight: (px) => {
    const clamped = clampDevPanelHeight(px);
    writeStorage(HEIGHT_KEY, String(clamped));
    set({ paneHeight: clamped });
  },
  setActiveTab: (t) => {
    writeStorage(TAB_KEY, t);
    set({ activeTab: t });
  },
  expand: () => get().setVisibility('expanded'),
  minimize: () => get().setVisibility('minimized'),
  collapse: () => get().setVisibility('collapsed'),
  toggleQuick: () => {
    const { visibility, setVisibility } = get();
    setVisibility(visibility === 'expanded' ? 'minimized' : 'expanded');
  },
  reportCriticalPoll: (latestAt) => set({ latestCriticalAt: latestAt }),
  acknowledgeCritical: () => {
    const now = new Date().toISOString();
    writeStorage(LAST_SEEN_CRITICAL_KEY, now);
    set({ lastSeenCriticalAt: now });
  },
}));

/**
 * Derived "is the badge on" predicate. Exported as a plain function (not a
 * store selector) so the component and its unit tests compute the exact same
 * thing from the exact same two fields.
 */
export function hasUnseenCritical(state: {
  readonly latestCriticalAt: string | null;
  readonly lastSeenCriticalAt: string | null;
}): boolean {
  if (state.latestCriticalAt === null) return false;
  if (state.lastSeenCriticalAt === null) return true;
  return state.latestCriticalAt > state.lastSeenCriticalAt;
}
