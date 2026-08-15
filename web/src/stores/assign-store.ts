import { create } from 'zustand';

/**
 * The single "交辦任務" entry point (UX plan I-1a).
 *
 * Before this store there were three different surfaces for the same intent —
 * the sidebar quick-create (task board `?new=1` → a passive card), the board's
 * own create modal, and `/goals` → 指派目標 (the only one that actually drove
 * `tasks.goal_create`) — and the sidebar one was gated off entirely on
 * Personal. Every entry now flips this store and the one `AssignSheet` mounted
 * in `MainLayout` opens, optionally prefilled.
 *
 * Kept intentionally tiny: the panel owns its own form state, this only carries
 * "open, with these seeds".
 */

/** Which of the two existing execution paths the panel submits to. */
export type AssignMode =
  /** 問一問 — a plain conversation on `/chat`; nothing is filed, nothing loops. */
  | 'ask'
  /** 交辦 — `tasks.goal_create`, i.e. the autonomous goal loop. */
  | 'assign';

export interface AssignPrefill {
  /** Preselect an AI employee (agent `name`). */
  readonly agentId?: string;
  /** Seed the goal text (e.g. "做一個同款" / a team task example). */
  readonly description?: string;
  /** Seed the acceptance criteria. */
  readonly acceptanceCriteria?: string;
  /** Open straight into a given mode; defaults to `assign`. */
  readonly mode?: AssignMode;
}

interface AssignStore {
  readonly open: boolean;
  readonly prefill: AssignPrefill | null;
  /** Open the panel, optionally seeded. Callers pass nothing for a blank one. */
  openAssign: (prefill?: AssignPrefill) => void;
  closeAssign: () => void;
}

export const useAssignStore = create<AssignStore>((set) => ({
  open: false,
  prefill: null,
  openAssign: (prefill) => set({ open: true, prefill: prefill ?? null }),
  closeAssign: () => set({ open: false, prefill: null }),
}));
