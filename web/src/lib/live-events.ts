import { client } from '@/lib/ws-client';

export const TASK_INVALIDATIONS = [
  'task.created', 'task.updated', 'task.removed', 'task.needs_human',
  'goal.round_started', 'goal.round_finished', 'goal.done', 'goal.failed',
  'events_lost', 'connection.ready',
] as const;

export const INBOX_INVALIDATIONS = [
  ...TASK_INVALIDATIONS,
  'approval.created', 'approval.decided', 'approval.expired',
  'budget.breaker_open', 'autopilot.breaker_open',
] as const;

export const MIN_REFRESH_INTERVAL_MS = 2_000;
const MAX_RETRY_INTERVAL_MS = 60_000;

/** Leading + trailing invalidation reads, with a hard minimum start interval.
 * A continuous stream never postpones the trailing read. Rejected reads (or
 * false results from stores that retain errors in state) back off exponentially. */
export function subscribeInvalidations(
  events: readonly string[],
  refresh: () => Promise<void | boolean>,
  invalidate: () => void = () => {},
): () => void {
  let timer: ReturnType<typeof setTimeout> | undefined;
  let running = false;
  let dirty = false;
  let disposed = false;
  let nextAllowedAt = 0;
  let failures = 0;
  const schedule = () => {
    if (disposed || !dirty || timer !== undefined || running) return;
    timer = setTimeout(() => {
      timer = undefined;
      if (disposed) return;
      dirty = false;
      running = true;
      nextAllowedAt = Date.now() + MIN_REFRESH_INTERVAL_MS;
      void (async () => {
        let succeeded = false;
        try {
          succeeded = (await refresh()) !== false;
        } catch {
          // The caller owns its visible error state; this controls retry load.
        } finally {
          failures = succeeded ? 0 : Math.min(failures + 1, 6);
          if (!succeeded) {
            const backoff = Math.min(
              MAX_RETRY_INTERVAL_MS,
              MIN_REFRESH_INTERVAL_MS * 2 ** (failures - 1),
            );
            nextAllowedAt = Math.max(nextAllowedAt, Date.now() + backoff);
          }
          running = false;
          schedule();
        }
      })();
    }, Math.max(0, nextAllowedAt - Date.now()));
  };
  const unsubscribers = events.map((event) => client.subscribe(event, () => {
    invalidate();
    dirty = true;
    schedule();
  }));
  return () => {
    disposed = true;
    if (timer !== undefined) clearTimeout(timer);
    unsubscribers.forEach((unsubscribe) => unsubscribe());
  };
}
