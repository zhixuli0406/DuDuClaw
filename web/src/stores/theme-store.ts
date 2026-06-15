import { create } from 'zustand';

export type Theme = 'light' | 'dark' | 'system';

const STORAGE_KEY = 'duduclaw-theme';
const CYCLE: ReadonlyArray<Theme> = ['light', 'dark', 'system'];

function systemPrefersDark(): boolean {
  if (typeof window === 'undefined' || typeof window.matchMedia !== 'function') {
    return false;
  }
  return window.matchMedia('(prefers-color-scheme: dark)').matches;
}

function storedTheme(): Theme {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw === 'light' || raw === 'dark') return raw;
  } catch { /* storage unavailable */ }
  return 'system';
}

/** Toggle the `.dark` class that drives the Tailwind dark variant. */
export function applyTheme(theme: Theme): void {
  const dark = theme === 'dark' || (theme === 'system' && systemPrefersDark());
  document.documentElement.classList.toggle('dark', dark);
}

interface ThemeStore {
  theme: Theme;
  setTheme: (theme: Theme) => void;
  /** light → dark → system → light */
  cycleTheme: () => void;
}

export const useThemeStore = create<ThemeStore>((set, get) => ({
  theme: storedTheme(),
  setTheme: (theme: Theme) => {
    try {
      if (theme === 'system') {
        localStorage.removeItem(STORAGE_KEY);
      } else {
        localStorage.setItem(STORAGE_KEY, theme);
      }
    } catch { /* storage unavailable */ }
    applyTheme(theme);
    set({ theme });
  },
  cycleTheme: () => {
    const { theme, setTheme } = get();
    const next = CYCLE[(CYCLE.indexOf(theme) + 1) % CYCLE.length];
    setTheme(next);
  },
}));

// Track OS appearance changes while in `system` mode.
if (typeof window !== 'undefined' && typeof window.matchMedia === 'function') {
  window
    .matchMedia('(prefers-color-scheme: dark)')
    .addEventListener('change', () => {
      const { theme } = useThemeStore.getState();
      if (theme === 'system') applyTheme(theme);
    });
}
