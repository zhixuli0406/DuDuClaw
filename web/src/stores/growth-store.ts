import { create } from 'zustand';
import { celebrate } from '@/components/ui/CelebrationLayer';
import type { GrowthSnapshot } from '@/lib/api-growth';

/**
 * growth-store — the single client-side cache of the company's `growth.snapshot`
 * (dashboard-redesign-v2 §6, V10). The gateway owns all XP/unlock truth; this
 * store only holds the latest read and, crucially, DIFFS consecutive snapshots
 * to fire the §6.5 celebration moments exactly once per genuine change:
 *
 *  - a newly-unlocked achievement → `celebrate('badge')` (visual burst) plus a
 *    `growthEventBus` event the React layer turns into a localized toast and
 *    (via the same bus) a DuDu `proud` face.
 *  - a level increase → a `growthEventBus` `level_up` event and a bumped
 *    `levelUpNonce` the HUD XP capsule watches to play a badge-pop.
 *
 * Baseline rule: the FIRST snapshot after a page load never celebrates — with
 * no previous snapshot to diff against, every already-unlocked achievement
 * would otherwise fire at once. Celebrations only trigger on a real transition
 * from a known previous state.
 */

/** Events the store emits on a genuine snapshot transition. */
export type GrowthEvent =
  | { type: 'achievement_unlocked'; id: string }
  | { type: 'level_up'; level: number };

type GrowthListener = (ev: GrowthEvent) => void;
let growthListeners: ReadonlyArray<GrowthListener> = [];

/**
 * A tiny module-level event bus (mirrors the toast/celebrate bus pattern) so
 * non-React callers (the store) can notify React consumers — the localized
 * toast and DuDu's transient `proud` face — without a circular import.
 */
export const growthEventBus = {
  subscribe(listener: GrowthListener): () => void {
    growthListeners = [...growthListeners, listener];
    return () => {
      growthListeners = growthListeners.filter((l) => l !== listener);
    };
  },
  emit(ev: GrowthEvent): void {
    for (const l of growthListeners) {
      try {
        l(ev);
      } catch {
        /* a bad listener must not break dispatch */
      }
    }
  },
};

interface GrowthStore {
  /** Latest snapshot, or null before the first successful read. */
  snapshot: GrowthSnapshot | null;
  /** True once at least one snapshot has been applied. */
  loaded: boolean;
  /** Bumped on every level increase — the HUD capsule watches it to pop. */
  levelUpNonce: number;
  /** Apply a fresh snapshot, diffing against the previous to fire moments. */
  applySnapshot: (next: GrowthSnapshot) => void;
}

export const useGrowthStore = create<GrowthStore>((set, get) => ({
  snapshot: null,
  loaded: false,
  levelUpNonce: 0,
  applySnapshot: (next) => {
    const { snapshot: prev, levelUpNonce } = get();
    let nonce = levelUpNonce;

    // Only diff when we have a known previous state — never firehose on first load.
    if (prev) {
      const prevUnlocked = new Set(
        prev.achievements.filter((a) => a.unlocked).map((a) => a.id),
      );
      for (const a of next.achievements) {
        if (a.unlocked && !prevUnlocked.has(a.id)) {
          celebrate('badge');
          growthEventBus.emit({ type: 'achievement_unlocked', id: a.id });
        }
      }
      if (next.level > prev.level) {
        growthEventBus.emit({ type: 'level_up', level: next.level });
        nonce += 1;
      }
    }

    set({ snapshot: next, loaded: true, levelUpNonce: nonce });
  },
}));
