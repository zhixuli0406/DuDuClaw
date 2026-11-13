import type { McpKeyExpiry } from '@/lib/api';

/** RM-134: the expiry choices offered when creating or changing an MCP key. */
export type ExpiryChoice = '30' | '90' | '365' | 'custom' | 'never';
export const EXPIRY_CHOICES: ReadonlyArray<ExpiryChoice> = ['30', '90', '365', 'custom', 'never'];

export interface ExpiryState {
  choice: ExpiryChoice;
  /** `YYYY-MM-DD` for `custom`. */
  date: string;
  /** The no-expiry risk notice has been confirmed. */
  confirmed: boolean;
}

export const DEFAULT_EXPIRY: ExpiryState = { choice: '90', date: '', confirmed: false };

/** `YYYY-MM-DD` of the local day after `now` (the earliest custom date). */
export function tomorrow(now: Date = new Date()): string {
  const d = new Date(now.getTime());
  d.setDate(d.getDate() + 1);
  const pad = (n: number) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}

/**
 * The `mcp_keys.create` / `mcp_keys.set_expiry` payload for a complete
 * choice, or `null` while it is incomplete: a custom date must be a valid
 * future day (the key expires at the end of that local day), and "no
 * expiry" only once the risk notice is confirmed.
 */
export function expiryPayload(state: ExpiryState, now: number = Date.now()): McpKeyExpiry | null {
  switch (state.choice) {
    case '30':
    case '90':
    case '365':
      return { kind: 'days', days: Number(state.choice) as 30 | 90 | 365 };
    case 'custom': {
      const match = /^(\d{4})-(\d{2})-(\d{2})$/.exec(state.date);
      if (!match) return null;
      const end = new Date(Number(match[1]), Number(match[2]) - 1, Number(match[3]), 23, 59, 59);
      if (Number.isNaN(end.getTime()) || end.getTime() <= now) return null;
      return { kind: 'date', at: end.toISOString() };
    }
    case 'never':
      return state.confirmed ? { kind: 'never', confirm: true } : null;
  }
}
