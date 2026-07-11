import { celebrate } from '@/components/ui';

/**
 * XP awarded on task completion — kept in lockstep with the gateway growth
 * engine (W5-BE: 完成任務 +12). This is a *display* constant only; the real XP
 * ledger lives server-side. Surfacing it here keeps the "+12 XP" float and the
 * engine from drifting.
 */
export const TASK_DONE_XP = 12;

/**
 * Fire the task-completion celebration (§5.5 / §6.5): a confetti burst plus a
 * calm reduced-motion fallback toast. The transient assignee "celebrating" pose
 * + "+N XP" float is a separate visual (see `TaskDoneBurst`) driven by the
 * caller's local state; this helper owns only the global CelebrationLayer poke.
 */
export function celebrateTaskDone(message: string): void {
  celebrate('confetti', { message });
}
