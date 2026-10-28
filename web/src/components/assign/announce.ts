import type { IntlShape } from 'react-intl';
import type { GoalCreateResult } from '@/lib/api';
import { toast, formatError } from '@/lib/toast';

/** The toasts after a goal was created — shared by both tabs of /assign. */
export function announceGoalCreated(intl: IntlShape, result: GoalCreateResult): void {
  if (result.plan_first) {
    toast.success(intl.formatMessage({ id: 'assign.mode.plan.success' }));
    return;
  }
  toast.success(intl.formatMessage({ id: 'goals.create.success' }, { cap: result.iteration_cap }));
  if (!result.dispatch_enabled) {
    toast.error(intl.formatMessage({ id: 'goals.create.dispatchOff' }));
  }
}

/** Where the person lands after a goal was created. */
export function createdGoalPath(result: GoalCreateResult): string {
  return `/goals?task=${encodeURIComponent(result.task.id)}`;
}

/** Gateway error prefixes that have their own sentence. */
const KNOWN_ERRORS: ReadonlyArray<readonly [string, string]> = [
  ['skill_not_enabled', 'assign.skill.error.notEnabled'],
  ['skill_not_found', 'assign.skill.error.notFound'],
  ['permission denied', 'assign.skill.error.denied'],
  ['attachments_too_many', 'assign.attach.error.tooMany'],
  ['attachments_too_large', 'assign.attach.error.tooLarge'],
  ['attachments_invalid', 'assign.attach.error.invalid'],
];

/** A failed assign as one sentence: known gateway refusals get their own. */
export function assignErrorText(intl: IntlShape, e: unknown): string {
  // The socket rejects with the gateway's string, the HTTP path with an Error.
  const raw = typeof e === 'string' ? e : e instanceof Error ? e.message : '';
  const hit = KNOWN_ERRORS.find(([prefix]) => raw.startsWith(prefix));
  return hit
    ? intl.formatMessage({ id: hit[1] })
    : intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) });
}
