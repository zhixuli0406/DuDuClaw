import { useEffect, useCallback, useState } from 'react';
import { useIntl } from 'react-intl';
import { cn } from '@/lib/utils';
import { useTasksStore } from '@/stores/tasks-store';
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
} from 'lucide-react';

const TYPE_CONFIG: Record<ActivityType, {
  icon: React.ComponentType<{ className?: string }>;
  color: string;
  bgColor: string;
}> = {
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
  error: {
    icon: AlertTriangle,
    color: 'text-rose-500',
    bgColor: 'bg-rose-100 dark:bg-rose-900/30',
  },
};

function ActivityItem({ event }: { event: ActivityEvent }) {
  const config = TYPE_CONFIG[event.type];
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
  const [visibleCount, setVisibleCount] = useState(limit);
  const [filterAgent, setFilterAgent] = useState<string>(agentId ?? '');

  useEffect(() => {
    fetchActivities({ limit: 50, agent_id: filterAgent || undefined });
  }, [fetchActivities, filterAgent]);

  const handleLoadMore = useCallback(() => {
    setVisibleCount((prev) => prev + 20);
  }, []);

  const visible = activities.slice(0, visibleCount);

  return (
    <div>
      {showFilter && agents.length > 0 && (
        <div className="mb-3">
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
        </div>
      )}
      {visible.length === 0 ? (
        <div className="flex items-center justify-center py-12 text-stone-400 dark:text-stone-500">
          <p>{intl.formatMessage({ id: 'activity.empty' })}</p>
        </div>
      ) : (
        <div className="divide-y divide-stone-100 dark:divide-stone-800">
          {visible.map((event) => (
            <ActivityItem key={event.id} event={event} />
          ))}
        </div>
      )}

      {activities.length > visibleCount && (
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
