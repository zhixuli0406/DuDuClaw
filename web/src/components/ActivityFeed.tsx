import { useEffect, useCallback, useState, useMemo } from 'react';
import { useIntl } from 'react-intl';
import { cn } from '@/lib/utils';
import { useTasksStore } from '@/stores/tasks-store';
import { useConnectionStore } from '@/stores/connection-store';
import type { ActivityEvent, ActivityType } from '@/lib/api';
import {
  Plus,
  CheckCircle2,
  Ban,
  UserCheck,
  MessageSquare,
  Sparkles,
  Zap,
  AlertTriangle,
  ChevronDown,
  Activity,
  Eye,
  EyeOff,
  Layers,
} from 'lucide-react';

type TypeConfig = {
  icon: React.ComponentType<{ className?: string }>;
  color: string;
  bgColor: string;
};

const FALLBACK_CONFIG: TypeConfig = {
  icon: Activity,
  color: 'text-stone-500',
  bgColor: 'bg-stone-100 dark:bg-stone-800',
};

const TYPE_CONFIG: Record<ActivityType, TypeConfig> = {
  task_created: {
    icon: Plus,
    color: 'text-blue-500',
    bgColor: 'bg-blue-100 dark:bg-blue-900/30',
  },
  task_completed: {
    icon: CheckCircle2,
    color: 'text-emerald-500',
    bgColor: 'bg-emerald-100 dark:bg-emerald-900/30',
  },
  task_blocked: {
    icon: Ban,
    color: 'text-rose-500',
    bgColor: 'bg-rose-100 dark:bg-rose-900/30',
  },
  task_assigned: {
    icon: UserCheck,
    color: 'text-amber-500',
    bgColor: 'bg-amber-100 dark:bg-amber-900/30',
  },
  agent_reply: {
    icon: MessageSquare,
    color: 'text-stone-500',
    bgColor: 'bg-stone-100 dark:bg-stone-800',
  },
  skill_learned: {
    icon: Sparkles,
    color: 'text-purple-500',
    bgColor: 'bg-purple-100 dark:bg-purple-900/30',
  },
  evolution_triggered: {
    icon: Zap,
    color: 'text-amber-500',
    bgColor: 'bg-amber-100 dark:bg-amber-900/30',
  },
  autopilot_triggered: {
    icon: Zap,
    color: 'text-amber-500',
    bgColor: 'bg-amber-100 dark:bg-amber-900/30',
  },
  autopilot_lag: {
    icon: AlertTriangle,
    color: 'text-amber-500',
    bgColor: 'bg-amber-100 dark:bg-amber-900/30',
  },
  error: {
    icon: AlertTriangle,
    color: 'text-rose-500',
    bgColor: 'bg-rose-100 dark:bg-rose-900/30',
  },
};

/**
 * Three-tier denoising (WP14-T14.3). The owner should see big events by
 * default and only drill into routine chatter on demand.
 *  Tier 1 — headline events (task lifecycle, learning, errors): always shown.
 *  Tier 2 — secondary signals (assignment, autopilot, evolution): always shown.
 *  Tier 3 — routine chatter (per-message replies): hidden until "show all".
 */
type Tier = 1 | 2 | 3;
const TIER: Record<ActivityType, Tier> = {
  task_created: 1,
  task_completed: 1,
  task_blocked: 1,
  error: 1,
  skill_learned: 1,
  task_assigned: 2,
  evolution_triggered: 2,
  autopilot_triggered: 2,
  autopilot_lag: 2,
  agent_reply: 3,
};

function ActivityItem({ event }: { event: ActivityEvent }) {
  const config = TYPE_CONFIG[event.type] ?? FALLBACK_CONFIG;
  const Icon = config.icon;

  const timeAgo = formatTimeAgo(event.timestamp);

  return (
    <div className="flex items-start gap-3 py-2">
      <div className={cn('mt-0.5 flex h-7 w-7 flex-shrink-0 items-center justify-center rounded-full', config.bgColor)}>
        <Icon className={cn('h-3.5 w-3.5', config.color)} />
      </div>
      <div className="min-w-0 flex-1">
        <p className="text-sm text-stone-700 dark:text-stone-300">{event.summary}</p>
        <div className="mt-0.5 flex items-center gap-2">
          <span className="text-xs text-stone-400 dark:text-stone-500">{event.agent_id}</span>
          <span className="text-xs text-stone-300 dark:text-stone-600">·</span>
          <span className="text-xs text-stone-400 dark:text-stone-500">{timeAgo}</span>
        </div>
      </div>
    </div>
  );
}

/** A run of ≥3 consecutive updates from the same agent, folded into one row. */
function CollapsedRun({ agentId, count }: { agentId: string; count: number }) {
  const intl = useIntl();
  return (
    <div className="flex items-center gap-3 py-2 pl-1 text-xs text-stone-400 dark:text-stone-500">
      <div className="flex h-7 w-7 flex-shrink-0 items-center justify-center rounded-full bg-stone-100 dark:bg-stone-800">
        <Layers className="h-3.5 w-3.5 text-stone-400" />
      </div>
      <span>
        <span className="text-stone-500 dark:text-stone-400">{agentId}</span>{' '}
        {intl.formatMessage({ id: 'activity.collapsed' }, { count })}
      </span>
    </div>
  );
}

function formatTimeAgo(timestamp: string): string {
  const now = Date.now();
  const then = new Date(timestamp).getTime();
  const diffMs = now - then;
  const diffMin = Math.floor(diffMs / 60_000);

  if (diffMin < 1) return 'just now';
  if (diffMin < 60) return `${diffMin}m ago`;
  const diffHr = Math.floor(diffMin / 60);
  if (diffHr < 24) return `${diffHr}h ago`;
  const diffDay = Math.floor(diffHr / 24);
  return `${diffDay}d ago`;
}

// A rendered row is either a single event or a collapsed run of same-agent updates.
type Row =
  | { kind: 'event'; event: ActivityEvent }
  | { kind: 'run'; agentId: string; count: number; key: string };

/**
 * Fold runs of ≥3 consecutive same-agent events (within 5 minutes of each
 * other) into a single "N consecutive updates" row so a chatty agent doesn't
 * bury everyone else.
 */
function foldRuns(events: ReadonlyArray<ActivityEvent>): Row[] {
  const rows: Row[] = [];
  let i = 0;
  while (i < events.length) {
    let j = i + 1;
    while (
      j < events.length &&
      events[j].agent_id === events[i].agent_id &&
      Math.abs(
        new Date(events[i].timestamp).getTime() - new Date(events[j].timestamp).getTime(),
      ) <= 5 * 60_000
    ) {
      j++;
    }
    const runLen = j - i;
    if (runLen >= 3) {
      rows.push({ kind: 'run', agentId: events[i].agent_id, count: runLen, key: events[i].id });
    } else {
      for (let k = i; k < j; k++) rows.push({ kind: 'event', event: events[k] });
    }
    i = j;
  }
  return rows;
}

export function ActivityFeed({
  limit = 20,
  agentId,
  showFilter = false,
  agents = [],
}: {
  limit?: number;
  agentId?: string;
  showFilter?: boolean;
  agents?: ReadonlyArray<{ name: string; display_name: string; icon: string }>;
}) {
  const intl = useIntl();
  const { activities, fetchActivities } = useTasksStore();
  const connectionState = useConnectionStore((s) => s.state);
  const [visibleCount, setVisibleCount] = useState(limit);
  const [filterAgent, setFilterAgent] = useState<string>(agentId ?? '');
  // Tier 3 (routine chatter) hidden by default — the owner opts into detail.
  const [showAll, setShowAll] = useState(false);

  useEffect(() => {
    if (connectionState !== 'authenticated') return;
    fetchActivities({ limit: 50, agent_id: filterAgent || undefined });
  }, [connectionState, fetchActivities, filterAgent]);

  const handleLoadMore = useCallback(() => {
    setVisibleCount((prev) => prev + 20);
  }, []);

  // Tier filter → slice → fold consecutive same-agent runs.
  const rows = useMemo(() => {
    const tierFiltered = activities.filter(
      (e) => showAll || (TIER[e.type] ?? 1) <= 2,
    );
    return { total: tierFiltered.length, rows: foldRuns(tierFiltered.slice(0, visibleCount)) };
  }, [activities, showAll, visibleCount]);

  return (
    <div>
      <div className="mb-3 flex flex-wrap items-center gap-2">
        {showFilter && agents.length > 0 && (
          <select
            value={filterAgent}
            onChange={(e) => { setFilterAgent(e.target.value); setVisibleCount(limit); }}
            className="rounded-lg border border-stone-200 bg-white px-3 py-1.5 text-xs text-stone-700 focus:border-amber-400 focus:outline-none dark:border-stone-700 dark:bg-stone-800 dark:text-stone-300"
          >
            <option value="">{intl.formatMessage({ id: 'activity.filter.all' })}</option>
            {agents.map((a) => (
              <option key={a.name} value={a.name}>{a.icon || '🤖'} {a.display_name}</option>
            ))}
          </select>
        )}
        <button
          onClick={() => setShowAll((v) => !v)}
          className="ml-auto inline-flex items-center gap-1.5 rounded-lg px-2.5 py-1.5 text-xs font-medium text-stone-500 transition-colors hover:bg-stone-500/8 hover:text-stone-700 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-500/40 dark:text-stone-400 dark:hover:bg-white/5 dark:hover:text-stone-200"
          aria-pressed={showAll}
        >
          {showAll ? <EyeOff className="h-3.5 w-3.5" /> : <Eye className="h-3.5 w-3.5" />}
          {intl.formatMessage({ id: showAll ? 'activity.showLess' : 'activity.showAll' })}
        </button>
      </div>
      {rows.rows.length === 0 ? (
        <div className="flex items-center justify-center py-12 text-stone-400 dark:text-stone-500">
          <p>{intl.formatMessage({ id: 'activity.empty' })}</p>
        </div>
      ) : (
        <div className="divide-y divide-stone-100 dark:divide-stone-800">
          {rows.rows.map((row) =>
            row.kind === 'event' ? (
              <ActivityItem key={row.event.id} event={row.event} />
            ) : (
              <CollapsedRun key={`run-${row.key}`} agentId={row.agentId} count={row.count} />
            ),
          )}
        </div>
      )}

      {rows.total > visibleCount && (
        <button
          onClick={handleLoadMore}
          className="mt-3 flex w-full items-center justify-center gap-1 rounded-lg bg-stone-50 py-2 text-xs text-stone-500 transition-colors hover:bg-stone-100 dark:bg-stone-800 dark:text-stone-400 dark:hover:bg-stone-700"
        >
          <ChevronDown className="h-3 w-3" />
          {intl.formatMessage({ id: 'activity.loadMore' })}
        </button>
      )}
    </div>
  );
}
