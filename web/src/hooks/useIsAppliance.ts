import { useEffect, useState } from 'react';
import { api } from '@/lib/api';

/**
 * Progressive disclosure for appliance-only surfaces (WP-C, 2026-08; widened
 * R2, 2026-08): whether this gateway is the DuDuClaw appliance image.
 *
 * Reads `system.status`'s `is_appliance` field rather than probing
 * `device.status` (R2 fix): `device.status` is gated `require_admin!()` +
 * `require_appliance!()` server-side, so a manager/employee caller could
 * never learn the answer and — for `App.tsx::HomeLanding` specifically —
 * never got redirected to the conversational console on an appliance box,
 * even though `/console` itself is open to every authenticated role.
 * `system.status` carries no admin gate, and now forwards the same
 * `duduclaw_core::is_appliance()` authority as a plain boolean, so this hook
 * works identically for every role. Callers that gate an admin-only surface
 * (the `/device` nav entry in `AppSidebar`/`CommandPalette`) keep doing so
 * via their own `enabled = hasMinRole(role, 'admin')` argument — this hook
 * only answers "is this the appliance image", never "may this caller see
 * appliance detail".
 *
 * Cache semantics: whether this gateway IS the appliance image never
 * changes at runtime (it's baked into the image), so BOTH a positive and a
 * negative result are cached for the whole session and never re-fetched —
 * unlike `useForksExist` (where only "exists" is a permanent fact). A
 * network/auth error is NOT cached (fail-closed: hidden now, retried on the
 * next mount). Deduped across concurrent consumers.
 */
let knownResult: boolean | null = null;
let inflight: Promise<boolean> | null = null;

function fetchIsAppliance(): Promise<boolean> {
  if (knownResult !== null) return Promise.resolve(knownResult);
  if (inflight) return inflight;
  inflight = api.system
    .status()
    .then((status) => {
      const v = status.is_appliance === true;
      knownResult = v;
      return v;
    })
    .catch(() => {
      // Any failure (network blip, gateway still booting) — fail closed
      // without caching, so the next mount retries.
      return false;
    })
    .finally(() => {
      inflight = null;
    });
  return inflight;
}

/**
 * True once `system.status` confirms this gateway is the appliance image.
 * `enabled=false` (caller decided this viewer shouldn't see the entry
 * anyway) skips the RPC entirely and returns false.
 */
export function useIsAppliance(enabled: boolean): boolean {
  const [isAppliance, setIsAppliance] = useState(knownResult === true);

  useEffect(() => {
    if (!enabled || knownResult !== null) return;
    let alive = true;
    fetchIsAppliance()
      .then((v) => {
        if (alive && v) setIsAppliance(true);
      })
      .catch(() => {
        /* fail-closed: stay hidden, retry on next mount */
      });
    return () => {
      alive = false;
    };
  }, [enabled]);

  return enabled && (isAppliance || knownResult === true);
}
