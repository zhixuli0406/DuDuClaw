import type { PlanStep, PlanStepStatus } from '@/lib/api';

/**
 * Pure helpers for the co-edited plan panel (U4). Kept out of the page
 * component so ordering / progress semantics are unit-testable.
 */

/** Click-to-advance status cycle: todo → doing → done → todo. A skipped step
 *  re-enters the cycle at todo (un-skip). */
export function cycleStepStatus(status: PlanStepStatus): PlanStepStatus {
  switch (status) {
    case 'todo':
      return 'doing';
    case 'doing':
      return 'done';
    case 'done':
      return 'todo';
    case 'skipped':
      return 'todo';
  }
}

/** Progress of a plan: done + skipped both count as settled. */
export function planProgress(steps: ReadonlyArray<PlanStep>): {
  settled: number;
  total: number;
  pct: number;
} {
  const settled = steps.filter((s) => s.status === 'done' || s.status === 'skipped').length;
  const total = steps.length;
  return { settled, total, pct: total === 0 ? 0 : Math.round((settled / total) * 100) };
}

/**
 * Target index for a one-slot move. Returns null when the move is a no-op
 * (already at the edge) so callers can skip the RPC entirely.
 */
export function stepMoveTarget(
  index: number,
  direction: 'up' | 'down',
  length: number,
): number | null {
  if (direction === 'up') return index > 0 ? index - 1 : null;
  return index < length - 1 ? index + 1 : null;
}
