/**
 * agent-stats — pure, testable derivations for the V6 staff面 (roster cards +
 * detail hero). No React/DOM so the level/tally logic can be unit-tested and
 * reused across the roster card, the detail hero, and the org panel.
 *
 * Honesty note (§5.4 / §6.2): a staff member's level is *derived on the front
 * end* from the tasks that member has completed — there is no per-agent XP
 * column in the backend today. The formula matches the global curve:
 *   Lv = floor(sqrt(done * 12 / 100))
 * i.e. every completed task is worth the same +12 XP as the global company XP
 * curve (§6.1). This is a display-only projection; it never drives behaviour.
 */

import type { TaskInfo } from '@/lib/api';
import { levelFromXp } from '@/components/ui';
import type { AgentGlyphState } from '@/stores/agent-activity-store';

/** XP each completed task is worth — same as the global company curve (§6.1). */
export const XP_PER_DONE_TASK = 12;

/** Whether a live glyph state means the agent is actively mid-run (working). */
export function isLiveState(state: AgentGlyphState): boolean {
  return (
    state === 'replying' ||
    state === 'tool_running' ||
    state === 'consolidating' ||
    state === 'awaiting_approval'
  );
}

export interface AgentTaskStats {
  /** Tasks assigned to this agent with status `done`. */
  readonly done: number;
  readonly inProgress: number;
  readonly blocked: number;
  readonly todo: number;
  readonly total: number;
  /** `done` tasks whose `completed_at` falls on the given day. */
  readonly todayDone: number;
}

/** True when an ISO timestamp lands on the same local calendar day as `nowMs`. */
export function isSameLocalDay(
  iso: string | null | undefined,
  nowMs: number = Date.now(),
): boolean {
  if (!iso) return false;
  const t = new Date(iso).getTime();
  if (!Number.isFinite(t)) return false;
  const a = new Date(t);
  const b = new Date(nowMs);
  return (
    a.getFullYear() === b.getFullYear() &&
    a.getMonth() === b.getMonth() &&
    a.getDate() === b.getDate()
  );
}

/**
 * Tally one agent's tasks by status. `todayDone` only counts a completed task
 * when it carries a `completed_at` that is today — a task done on a prior day
 * (or with no completion timestamp) is honestly excluded from "today".
 */
export function agentTaskStats(
  tasks: ReadonlyArray<TaskInfo>,
  agentName: string,
  nowMs: number = Date.now(),
): AgentTaskStats {
  let done = 0;
  let inProgress = 0;
  let blocked = 0;
  let todo = 0;
  let todayDone = 0;
  let total = 0;
  for (const t of tasks) {
    if (t.assigned_to !== agentName) continue;
    total += 1;
    switch (t.status) {
      case 'done':
        done += 1;
        if (isSameLocalDay(t.completed_at, nowMs)) todayDone += 1;
        break;
      case 'in_progress':
        inProgress += 1;
        break;
      case 'blocked':
        blocked += 1;
        break;
      case 'todo':
        todo += 1;
        break;
    }
  }
  return { done, inProgress, blocked, todo, total, todayDone };
}

/**
 * Derived staff level from completed-task count (see module note). Uses the same
 * `levelFromXp` curve as the global HUD so the two never disagree on maths.
 */
export function staffLevel(doneCount: number): number {
  return levelFromXp(Math.max(0, doneCount) * XP_PER_DONE_TASK);
}
