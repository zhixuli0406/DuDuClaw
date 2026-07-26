/**
 * Task-status mapping layer (dashboard-redesign-v2 W0 遺留⑤).
 *
 * The `StatusIcon` primitive speaks the full 7-state design vocabulary
 * (`TaskStatusKey`: backlog / todo / in_progress / in_review / done / blocked /
 * cancelled), but the gateway only persists 4 (`TaskStatus`: todo / in_progress
 * / done / blocked). This module is the single, pure, tested boundary between
 * the two so no page hard-codes the coercion.
 *
 * Discipline: `backlog` / `in_review` / `cancelled` are FORWARD-LOOKING — the UI
 * must never *produce* them (we don't surface them in pickers, and if one ever
 * reaches `toBackendStatus` it returns `null` = "not writable, ignore" rather
 * than silently coercing to a wrong real status). Fail-safe, never fail-wrong.
 */
import type { TaskStatus } from '@/lib/api';
import type { TaskStatusKey } from '@/components/ui';

/** The four statuses the UI may WRITE via drag/drop / picker. A subset of both
 *  `TaskStatus` and `TaskStatusKey` — the goal-loop-owned states (`review` /
 *  `revising` / `needs_human` / `failed` / `pending` / `cancelled`) are
 *  display-only and never produced by the UI. */
export type WritableStatus = 'todo' | 'in_progress' | 'done' | 'blocked';

/** The four statuses the UI may write (a subset of both unions). */
export const BACKEND_STATUSES: readonly WritableStatus[] = ['todo', 'in_progress', 'done', 'blocked'];

/**
 * The three UI-vocabulary statuses with no backend representation yet. Present
 * so `StatusIcon` renders the full design set, but excluded from every writer
 * path — the UI never generates them.
 */
export const FORWARD_LOOKING_STATUSES: readonly TaskStatusKey[] = ['backlog', 'in_review', 'cancelled'];

/** Type guard: is a UI status key one the UI may write? */
export function isBackendStatus(key: TaskStatusKey): key is WritableStatus {
  return key === 'todo' || key === 'in_progress' || key === 'done' || key === 'blocked';
}

/**
 * Backend status → UI status key. Most persisted statuses share their name with
 * a `TaskStatusKey`; the two exceptions are the Iterative Kanban `review`
 * (rendered as the `in_review` glyph) and the transient `pending` (shown as
 * `todo` — an unclaimed task the board treats like a fresh one). Everything the
 * `StatusIcon` vocabulary already covers (`revising` / `failed` / `needs_human`
 * / `cancelled`) maps through directly. These are all display-only — the UI
 * never *writes* them, so `toBackendStatus` still excludes them (returns null).
 */
export function toStatusKey(status: TaskStatus): TaskStatusKey {
  switch (status) {
    case 'review':
      return 'in_review';
    case 'pending':
      return 'todo';
    default:
      return status as TaskStatusKey;
  }
}

/**
 * UI status key → backend status, or `null` when the key is forward-looking
 * (`backlog` / `in_review` / `cancelled`). Callers MUST treat `null` as
 * "ignore this change" — never coerce it into a real write.
 */
export function toBackendStatus(key: TaskStatusKey): TaskStatus | null {
  return isBackendStatus(key) ? key : null;
}
