import { useMemo } from 'react';
import { useIntl } from 'react-intl';
import { Link } from 'react-router';
import { api, type TaskInfo } from '@/lib/api';
import { CharacterAvatar, Card, Mono, StatusIcon, EmptyState } from '@/components/ui';
import { ListTodo } from 'lucide-react';
import { toStatusKey } from '@/lib/task-status';
import { formatId, timeAgo } from '@/lib/format';
import { useSharedLeaderQuery } from '@/hooks/useSharedLeaderQuery';

/**
 * RecentTasks — the right column of Home's "近期" row (V3-T3.4). The 10
 * most-recently-updated tasks: status glyph, title, assignee character avatar,
 * short id, relative time — each row deep-links to the task detail.
 *
 * Polled through `useSharedLeaderQuery` so multiple open tabs share one RPC.
 */
const RECENT_CAP = 10;
const POLL_MS = 15000;

async function fetchRecent(): Promise<TaskInfo[]> {
  const res = await api.tasks.list({}).catch(() => null);
  const tasks = [...(res?.tasks ?? [])];
  tasks.sort((a, b) => new Date(b.updated_at).getTime() - new Date(a.updated_at).getTime());
  return tasks.slice(0, RECENT_CAP);
}

export interface RecentTasksProps {
  agents: ReadonlyArray<{ name: string; display_name: string }>;
  enabled: boolean;
}

export function RecentTasks({ agents, enabled }: RecentTasksProps) {
  const intl = useIntl();
  const { data } = useSharedLeaderQuery<TaskInfo[]>('home:recent-tasks', fetchRecent, POLL_MS, enabled);

  const nameOf = useMemo(() => {
    const m = new Map<string, string>();
    for (const a of agents) m.set(a.name, a.display_name || a.name);
    return m;
  }, [agents]);

  const tasks = data ?? [];

  return (
    <Card title={intl.formatMessage({ id: 'home.recentTasks.title' })}>
      {tasks.length === 0 ? (
        <EmptyState icon={ListTodo} title={intl.formatMessage({ id: 'home.recentTasks.empty' })} />
      ) : (
        <div className="divide-y divide-stone-100 dark:divide-stone-800">
          {tasks.map((t) => (
            <Link
              key={t.id}
              to={`/tasks/${encodeURIComponent(t.id)}`}
              className="-mx-2 flex items-center gap-3 rounded-lg px-2 py-2 transition-colors hover:bg-stone-500/8 dark:hover:bg-white/5"
            >
              <StatusIcon status={toStatusKey(t.status)} size="sm" />
              <span className="min-w-0 flex-1 truncate text-sm text-stone-800 dark:text-stone-100" title={t.title}>
                {t.title}
              </span>
              {t.assigned_to && (
                <CharacterAvatar
                  agentId={t.assigned_to}
                  name={nameOf.get(t.assigned_to)}
                  size={22}
                  className="shrink-0"
                />
              )}
              <Mono className="hidden shrink-0 sm:inline">{formatId(t.id)}</Mono>
              <Mono className="shrink-0">{timeAgo(t.updated_at)}</Mono>
            </Link>
          ))}
        </div>
      )}
    </Card>
  );
}
