import { create } from 'zustand';

/**
 * Drives the top-of-page indeterminate NavProgress sweep (spec §3). A lazy route
 * chunk that is still loading suspends the shell's inner boundary; that fallback
 * flips `active` on mount and off on unmount, so the bar reflects *real* pending
 * navigation rather than a fixed timer. Kept as a tiny module-level store so both
 * the fallback and the bar can read it without prop-drilling through the shell.
 */
interface NavProgressStore {
  readonly active: boolean;
  setActive: (active: boolean) => void;
}

export const useNavProgressStore = create<NavProgressStore>((set) => ({
  active: false,
  setActive: (active) => set({ active }),
}));
