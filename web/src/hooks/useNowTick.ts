import { useEffect, useState } from 'react';

/**
 * A `Date.now()` snapshot that re-renders the caller every `intervalMs` while
 * `active` — the shared clock behind live countdowns (e.g. an approval's TTL
 * deadline in the Inbox). Idle (no interval, no re-renders) while `active` is
 * false, so a row without an expiry to track costs nothing.
 */
export function useNowTick(active: boolean, intervalMs = 10_000): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!active) return;
    const id = setInterval(() => setNow(Date.now()), intervalMs);
    return () => clearInterval(id);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [active, intervalMs]);
  return now;
}
