import { useIntl } from 'react-intl';
import { Activity, CheckCircle2, Loader2, Ban, ListTodo } from 'lucide-react';
import type { ActivityEvent, TaskInfo } from '@/lib/api';
import {
  Card,
  Mono,
  StatusIcon,
  LiveBadge,
  EmptyState,
  SkeletonList,
} from '@/components/ui';
import { timeAgo } from '@/lib/format';
import type { AgentTaskStats } from './agent-stats';

/**
 * AgentOverviewTab — the enhanced 總覽 tab (§5.4 T6.2): a live activity-tail card
 * (same "latest activity" model the Home live card uses until C-P1 tool steps
 * land), the win tally (done / in-progress / blocked), and the agent's ten most
 * recent tasks.
 */
export function AgentOverviewTab({
  activities,
  tasks,
  stats,
  live,
}: {
  activities: ReadonlyArray<ActivityEvent> | null;
  tasks: ReadonlyArray<TaskInfo> | null;
  stats: AgentTaskStats;
  live: boolean;
}) {
  const intl = useIntl();

  const recentTasks = [...(tasks ?? [])]
    .sort((a, b) => new Date(b.updated_at).getTime() - new Date(a.updated_at).getTime())
    .slice(0, 10);

  return (
    <div className="space-y-4">
      {/* Live activity tail. */}
      <Card
        title={intl.formatMessage({ id: 'agentDetail.live.title' })}
        actions={live ? <LiveBadge /> : undefined}
      >
        {activities === null ? (
          <SkeletonList rows={3} rowClassName="h-8" />
        ) : activities.length === 0 ? (
          <EmptyState icon={Activity} title={intl.formatMessage({ id: 'agentDetail.live.empty' })} />
        ) : (
          <ul className="space-y-1.5">
            {activities.slice(0, 6).map((ev) => (
              <li key={ev.id} className="flex items-baseline gap-2 text-sm">
                <Mono className="shrink-0 text-xs text-stone-400 dark:text-stone-500">
                  {timeAgo(ev.timestamp)}
                </Mono>
                <span className="min-w-0 flex-1 truncate text-stone-700 dark:text-stone-300">
                  {ev.summary}
                </span>
              </li>
            ))}
          </ul>
        )}
      </Card>

      {/* Win tally. */}
      <div className="grid grid-cols-3 gap-3">
        <StatTile icon={CheckCircle2} tone="emerald" value={stats.done} label={intl.formatMessage({ id: 'agentDetail.stats.done' })} />
        <StatTile icon={Loader2} tone="sky" value={stats.inProgress} label={intl.formatMessage({ id: 'agentDetail.stats.inProgress' })} />
        <StatTile icon={Ban} tone="rose" value={stats.blocked} label={intl.formatMessage({ id: 'agentDetail.stats.blocked' })} />
      </div>

      {/* Recent tasks. */}
      <Card title={intl.formatMessage({ id: 'agentDetail.tasks.title' })}>
        {tasks === null ? (
          <SkeletonList rows={3} rowClassName="h-9" />
        ) : recentTasks.length === 0 ? (
          <EmptyState icon={ListTodo} title={intl.formatMessage({ id: 'agentDetail.tasks.empty' })} />
        ) : (
          <ul className="divide-y divide-[var(--panel-border)]">
            {recentTasks.map((t) => (
              <li key={t.id} className="flex items-center gap-2.5 py-2 first:pt-0 last:pb-0">
                <StatusIcon status={t.status} size="sm" />
                <span className="min-w-0 flex-1 truncate text-sm text-stone-800 dark:text-stone-100">
                  {t.title}
                </span>
                <Mono className="shrink-0 text-xs text-stone-400 dark:text-stone-500">
                  {timeAgo(t.updated_at)}
                </Mono>
              </li>
            ))}
          </ul>
        )}
      </Card>
    </div>
  );
}

const TONES: Record<string, string> = {
  emerald: 'text-emerald-600 dark:text-emerald-400',
  sky: 'text-sky-600 dark:text-sky-400',
  rose: 'text-rose-600 dark:text-rose-400',
};

function StatTile({
  icon: Icon,
  tone,
  value,
  label,
}: {
  icon: React.ComponentType<{ className?: string }>;
  tone: keyof typeof TONES | string;
  value: number;
  label: string;
}) {
  return (
    <div className="panel flex flex-col items-center gap-1 p-4 text-center">
      <Icon className={`h-5 w-5 ${TONES[tone] ?? ''}`} />
      <span className="text-2xl font-semibold tabular-nums text-stone-900 dark:text-stone-50">{value}</span>
      <span className="text-xs text-stone-500 dark:text-stone-400">{label}</span>
    </div>
  );
}
