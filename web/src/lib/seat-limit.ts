/**
 * RM-149: the structured refusal the gateway returns when creating or
 * restoring an AI employee would go over the licence seat limit
 * (`duduclaw_core::agent_seats::SEAT_LIMIT_ERROR_CODE`). Pure helpers — no
 * React, no RPC.
 */

export const SEAT_LIMIT_ERROR_CODE = 'agent_seat_limit_reached';

/** One employee counted against the licence. */
export interface SeatHolder {
  id: string;
  display_name: string;
  /** `[agent] status` as written, or `unreadable` / `unknown`. */
  status: string;
}

export interface SeatLimitInfo {
  message: string;
  max: number | null;
  current: number | null;
  holders: SeatHolder[];
}

function holderOf(value: unknown): SeatHolder | null {
  if (!value || typeof value !== 'object') return null;
  const v = value as Record<string, unknown>;
  if (typeof v.id !== 'string' || !v.id) return null;
  return {
    id: v.id,
    display_name: typeof v.display_name === 'string' && v.display_name ? v.display_name : v.id,
    status: typeof v.status === 'string' ? v.status : 'unknown',
  };
}

/**
 * The seat refusal carried by `err` (a rejected RPC's `{ code, … }` object,
 * or a `templates.apply` result's `seat_limit`), or `null` for anything else.
 */
export function seatLimitOf(err: unknown): SeatLimitInfo | null {
  if (!err || typeof err !== 'object') return null;
  const e = err as Record<string, unknown>;
  if (e.code !== SEAT_LIMIT_ERROR_CODE) return null;
  const holders = Array.isArray(e.seat_holders)
    ? e.seat_holders.map(holderOf).filter((h): h is SeatHolder => h !== null)
    : [];
  return {
    message: typeof e.message === 'string' ? e.message : '',
    max: typeof e.max === 'number' ? e.max : null,
    current: typeof e.current === 'number' ? e.current : null,
    holders,
  };
}

/** Status values with their own label; anything else shows as written. */
export const SEAT_STATUS_KEYS = ['active', 'paused', 'terminated', 'archived', 'unreadable', 'unknown'] as const;

export function seatStatusKey(status: string): string | null {
  return (SEAT_STATUS_KEYS as readonly string[]).includes(status) ? `agents.seatLimit.status.${status}` : null;
}
