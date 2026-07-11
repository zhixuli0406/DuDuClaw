import { useMemo } from 'react';
import { useIntl } from 'react-intl';
import { Link } from 'react-router';
import { Bot, Inbox as InboxIcon, Wallet, CheckCircle2 } from 'lucide-react';
import { StatCard } from '@/components/ui';
import { formatCents } from '@/lib/format';
import { useAuthStore } from '@/stores/auth-store';
import { hasMinRole } from '@/lib/roles';
import { useTodayCost } from '@/components/growth/useTodayCost';
import { useTypewriter } from './useTypewriter';

/**
 * GreetingHud — the top band of Home (V3-T3.1). A state-aware typewriter greeting
 * on the left, the "today at a glance" 4-stat war-report on the right.
 *
 * The three rotating lines are DERIVED from live data, not canned: a time-of-day
 * hello, how many staff are busy right now, and the state of the inbox (empty →
 * praise, otherwise a nudge). The typewriter cadence matches openhuman
 * (55/36/1400) via `useTypewriter`, which also honours reduced-motion.
 *
 * Honest cost note: the gateway exposes no per-day spend on the wired RPC
 * surface (`accounts.budget_summary` is cumulative; `analytics.summary` reports
 * savings, not spend). So the cost tile shows the cumulative total and is
 * labelled 「累計」 — it never pretends to be a today-only figure.
 */
export interface GreetingHudProps {
  userName: string;
  /** Agents currently `active` (online). */
  busyCount: number;
  totalAgents: number;
  /** Inbox items that offer a primary action — the "待我拍板" count. */
  actionableCount: number;
  /** Tasks completed today, or null while loading / on error. */
  doneToday: number | null;
  /** Cumulative spend in cents, or null while loading / on error. */
  costCents: number | null;
}

/** Local-time hour → greeting bucket. Pure, exported for clarity. */
export function greetingBucket(hour: number): 'morning' | 'afternoon' | 'evening' | 'night' {
  if (hour >= 5 && hour < 12) return 'morning';
  if (hour >= 12 && hour < 18) return 'afternoon';
  if (hour >= 18 && hour < 23) return 'evening';
  return 'night';
}

export function GreetingHud({
  userName,
  busyCount,
  totalAgents,
  actionableCount,
  doneToday,
  costCents,
}: GreetingHudProps) {
  const intl = useIntl();
  const role = useAuthStore((s) => s.user?.role);
  const isManager = hasMinRole(role, 'manager');
  // Prefer today's live spend from growth.daily_report; fall back to the
  // cumulative figure passed in (labelled 「累計」) on error / for employees.
  const { cents: costValue, mode: costMode } = useTodayCost({
    enabled: isManager,
    fallbackCents: costCents,
  });

  const lines = useMemo(() => {
    const bucket = greetingBucket(new Date().getHours());
    const hello = intl.formatMessage({ id: `home.greeting.${bucket}` }, { name: userName });
    const busy =
      busyCount > 0
        ? intl.formatMessage({ id: 'home.hud.line.busy' }, { count: busyCount })
        : intl.formatMessage({ id: 'home.hud.line.calm' });
    const inbox =
      actionableCount > 0
        ? intl.formatMessage({ id: 'home.hud.line.needs' }, { count: actionableCount })
        : intl.formatMessage({ id: 'home.hud.line.clear' });
    return [hello, busy, inbox];
  }, [intl, userName, busyCount, actionableCount]);

  const typed = useTypewriter(lines);

  return (
    <div className="flex flex-col gap-5 lg:flex-row lg:items-center lg:justify-between">
      <div className="min-w-0">
        <h1 className="min-h-[2.25rem] text-2xl font-semibold tracking-tight text-stone-900 dark:text-stone-50">
          {typed}
          <span
            aria-hidden="true"
            className="ml-0.5 inline-block h-[1.05em] w-[2px] translate-y-[0.12em] animate-pulse rounded-full bg-amber-500/80 align-baseline dark:bg-amber-400/80"
          />
        </h1>
        <p className="mt-1 text-sm text-stone-500 dark:text-stone-400">
          {intl.formatMessage({ id: 'home.greeting.subtitle' })}
        </p>
      </div>

      <div className="grid grid-cols-2 gap-3 sm:grid-cols-4 lg:shrink-0">
        <StatCard
          icon={CheckCircle2}
          tone="success"
          label={intl.formatMessage({ id: 'home.stat.done' })}
          value={doneToday == null ? '—' : doneToday}
          hint={intl.formatMessage({ id: 'home.stat.doneHint' })}
          className="lg:w-36"
        />
        <Link to="/inbox" className="focus-visible:outline-none">
          <StatCard
            icon={InboxIcon}
            tone={actionableCount > 0 ? 'warning' : 'success'}
            label={intl.formatMessage({ id: 'home.stat.needsMe' })}
            value={actionableCount}
            hint={intl.formatMessage({ id: 'home.stat.needsMeHint' })}
            className="lg:w-36"
          />
        </Link>
        <StatCard
          icon={Wallet}
          tone="neutral"
          label={intl.formatMessage({ id: 'home.stat.cost' })}
          value={costMode === 'loading' || costValue == null ? '—' : formatCents(costValue)}
          hint={intl.formatMessage({
            id: costMode === 'today' ? 'home.stat.costTodayHint' : 'home.stat.costCumulativeHint',
          })}
          className="lg:w-36"
        />
        <StatCard
          icon={Bot}
          tone="accent"
          label={intl.formatMessage({ id: 'home.stat.busy' })}
          value={busyCount}
          hint={intl.formatMessage({ id: 'home.stat.busyHint' }, { total: totalAgents })}
          className="lg:w-36"
        />
      </div>
    </div>
  );
}
