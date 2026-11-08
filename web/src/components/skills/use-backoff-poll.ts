import { useEffect, useRef } from 'react';
import { pollDelay } from './wizard-machine';

/**
 * Poll `tick` with exponential backoff (4s → 8s → 16s … max 60s) while `enabled`.
 *
 * - One timer at a time (a setTimeout chain), cleared on unmount / when disabled.
 * - Pauses while `document.hidden`; resumes (backoff reset) when the tab is shown.
 * - Re-keying `resetKey` restarts the chain from the base delay. `tick` is read
 *   through a ref, so a new function identity never restarts polling.
 */
export function useBackoffPoll(
  enabled: boolean,
  tick: () => Promise<unknown> | unknown,
  resetKey?: unknown,
): void {
  const tickRef = useRef(tick);
  tickRef.current = tick;

  useEffect(() => {
    if (!enabled) return;
    let stopped = false;
    let paused = false;
    let attempt = 0;
    let timer: ReturnType<typeof setTimeout> | null = null;

    const schedule = () => {
      timer = setTimeout(run, pollDelay(attempt));
    };
    const run = async () => {
      timer = null;
      if (stopped) return;
      if (document.hidden) {
        paused = true; // resumed by the visibilitychange handler
        return;
      }
      try {
        await tickRef.current();
      } catch {
        /* the tick reports its own errors */
      }
      attempt += 1;
      if (!stopped) schedule();
    };
    const onVisibility = () => {
      if (!document.hidden && paused && !stopped) {
        paused = false;
        attempt = 0;
        void run();
      }
    };

    document.addEventListener('visibilitychange', onVisibility);
    schedule();
    return () => {
      stopped = true;
      if (timer) clearTimeout(timer);
      document.removeEventListener('visibilitychange', onVisibility);
    };
  }, [enabled, resetKey]);
}
