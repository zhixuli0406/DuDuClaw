import { useEffect, useState } from 'react';
import { api } from '@/lib/api';

/**
 * Progressive disclosure for the `/forks` nav entry: the page is a dead end
 * until the very first fork ever runs (`fork_store.db` is only created then),
 * so the sidebar / command palette hide it while `fork.list` comes back empty.
 *
 * Cache semantics: "forks exist" can only flip false→true (the store file is
 * never deleted at runtime), so a positive result is cached for the whole
 * session and never re-fetched. A negative result is re-checked on the next
 * consumer mount, and a fetch error is NOT cached (fail-closed: hidden now,
 * retried later). Deduped across concurrent consumers.
 */
let knownTrue = false;
let inflight: Promise<boolean> | null = null;

function fetchForksExist(): Promise<boolean> {
  if (knownTrue) return Promise.resolve(true);
  if (inflight) return inflight;
  inflight = api.fork
    .list(1)
    .then((res) => {
      const exists = (res?.forks?.length ?? 0) > 0;
      if (exists) knownTrue = true;
      return exists;
    })
    .finally(() => {
      inflight = null;
    });
  return inflight;
}

/**
 * True once at least one fork record exists. `enabled=false` (viewer can't see
 * the entry anyway, e.g. below manager) skips the RPC entirely and returns
 * false.
 */
export function useForksExist(enabled: boolean): boolean {
  const [exists, setExists] = useState(knownTrue);

  useEffect(() => {
    if (!enabled || knownTrue) return;
    let alive = true;
    fetchForksExist()
      .then((v) => {
        if (alive && v) setExists(true);
      })
      .catch(() => {
        /* fail-closed: stay hidden, retry on next mount */
      });
    return () => {
      alive = false;
    };
  }, [enabled]);

  return enabled && (exists || knownTrue);
}
