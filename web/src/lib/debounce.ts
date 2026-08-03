/**
 * Trailing debounce for WP6 live-refresh subscriptions.
 *
 * Server pushes arrive in bursts: distilling one pasted document raises a
 * `memory.changed` per fact, a synthesis run graduates several skills in a row,
 * and a bulk cron edit touches many rows. Refetching per event turns one user
 * action into a dozen RPCs on every open tab. Trailing (not leading) is what we
 * want here: the *last* event in a burst is the one whose state we should read,
 * and one extra half-second of latency is invisible next to the ~2s the
 * `events.db` tail already costs.
 */

/** Default burst window. Comfortably inside the "feels instant" budget while
 *  still collapsing a multi-fact distill into a single refetch. */
export const REFRESH_DEBOUNCE_MS = 400;

export interface DebouncedFn<A extends unknown[]> {
  (...args: A): void;
  /** Drop any pending trailing call. Call from effect cleanup so a unmounted
   *  component never fires a refetch (and React never warns). */
  cancel: () => void;
}

/**
 * Wrap `fn` so it runs once, `waitMs` after the last call.
 *
 * Deliberately dependency-free and untyped-timer-agnostic so it works the same
 * under jsdom fake timers as in the browser.
 */
export function debounceTrailing<A extends unknown[]>(
  fn: (...args: A) => void,
  waitMs: number = REFRESH_DEBOUNCE_MS,
): DebouncedFn<A> {
  let timer: ReturnType<typeof setTimeout> | null = null;

  const debounced = ((...args: A) => {
    if (timer !== null) clearTimeout(timer);
    timer = setTimeout(() => {
      timer = null;
      fn(...args);
    }, waitMs);
  }) as DebouncedFn<A>;

  debounced.cancel = () => {
    if (timer !== null) {
      clearTimeout(timer);
      timer = null;
    }
  };

  return debounced;
}
