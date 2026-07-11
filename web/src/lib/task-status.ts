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

/** The four statuses the backend actually persists (a subset of TaskStatusKey). */
export const BACKEND_STATUSES: readonly TaskStatus[] = ['todo', 'in_progress', 'done', 'blocked'];

/**
 * The three UI-vocabulary statuses with no backend representation yet. Present
 * so `StatusIcon` renders the full design set, but excluded from every writer
 * path — the UI never generates them.
 */
export const FORWARD_LOOKING_STATUSES: readonly TaskStatusKey[] = ['backlog', 'in_review', 'cancelled'];

/** Type guard: is a UI status key one the backend can store? */
export function isBackendStatus(key: TaskStatusKey): key is TaskStatus {
  return key === 'todo' || key === 'in_progress' || key === 'done' || key === 'blocked';
}

/**
 * Backend status → UI status key. Identity on the four shared names (they are
 * all valid `TaskStatusKey` members); typed so call sites feed `StatusIcon`.
 */
export function toStatusKey(status: TaskStatus): TaskStatusKey {
  return status;
}

/**
 * UI status key → backend status, or `null` when the key is forward-looking
 * (`backlog` / `in_review` / `cancelled`). Callers MUST treat `null` as
 * "ignore this change" — never coerce it into a real write.
 */
export function toBackendStatus(key: TaskStatusKey): TaskStatus | null {
  return isBackendStatus(key) ? key : null;
}
