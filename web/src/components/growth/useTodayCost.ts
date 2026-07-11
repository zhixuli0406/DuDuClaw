import { useEffect, useState } from 'react';
import { useConnectionStore } from '@/stores/connection-store';
import { growthApi } from '@/lib/api-growth';
import { api } from '@/lib/api';

/**
 * Which figure the cost chip is currently showing:
 *  - `loading`     — first fetch in flight.
 *  - `today`       — live, still-changing today figure from `growth.daily_report`
 *                    (label it "今日（更新中）").
 *  - `cumulative`  — fell back to the account budget total (label it "累計").
 *  - `disabled`    — the viewer isn't allowed to see spend; shows `fallbackCents`.
 */
export type TodayCostMode = 'loading' | 'today' | 'cumulative' | 'disabled';

export interface TodayCostState {
  cents: number | null;
  mode: TodayCostMode;
}

/** UTC calendar day (`YYYY-MM-DD`) — matches the gateway's `chrono::Utc` cost math. */
function utcDayKey(now: Date = new Date()): string {
  return now.toISOString().slice(0, 10);
}

/**
 * useTodayCost — the shared today-spend source for the Header CoinChip and the
 * Home greeting cost tile (T10.5 / W3a follow-up). Primary source is
 * `growth.daily_report({date: today})`, which returns a live rolling figure; on
 * error it degrades to the cumulative account total (labelled "累計"), never a
 * misleading $0. When `enabled` is false (viewer can't see spend) it just
 * surfaces `fallbackCents` without hitting any RPC.
 */
export function useTodayCost(opts: {
  enabled: boolean;
  fallbackCents?: number | null;
}): TodayCostState {
  const { enabled, fallbackCents = null } = opts;
  const authed = useConnectionStore((s) => s.state === 'authenticated');
  const [state, setState] = useState<TodayCostState>({ cents: null, mode: 'loading' });

  useEffect(() => {
    if (!enabled) {
      setState({ cents: fallbackCents, mode: 'disabled' });
      return;
    }
    if (!authed) return;
    let alive = true;
    setState({ cents: null, mode: 'loading' });

    growthApi
      .dailyReport(utcDayKey())
      .then((r) => {
        if (!alive) return;
        setState({ cents: r?.cost_cents ?? 0, mode: 'today' });
      })
      .catch(() => {
        // Fall back to the cumulative spend total (honest "累計" label).
        if (!alive) return;
        if (fallbackCents != null) {
          setState({ cents: fallbackCents, mode: 'cumulative' });
          return;
        }
        api.accounts
          .budgetSummary()
          .then((b) => {
            if (alive) setState({ cents: b?.total_spent_cents ?? 0, mode: 'cumulative' });
          })
          .catch(() => {
            if (alive) setState({ cents: null, mode: 'cumulative' });
          });
      });

    return () => {
      alive = false;
    };
  }, [enabled, authed, fallbackCents]);

  return state;
}
