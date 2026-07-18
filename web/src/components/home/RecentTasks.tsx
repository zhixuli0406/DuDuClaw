import { useMemo } from 'react';
import { useIntl } from 'react-intl';
import { Link } from 'react-router';
import { ArrowRight, ListTodo } from 'lucide-react';
import { api, type TaskInfo } from '@/lib/api';
import {
  Card,
  CardHeader,
  CardTitle,
  CardFooter,
  ActorAvatar,
  Empty,
} from '@/components/mds';
import { StatusIcon } from '@/components/ui';
import { toStatusKey } from '@/lib/task-status';
import { timeAgo } from '@/lib/format';
import { useSharedLeaderQuery } from '@/hooks/useSharedLeaderQuery';

/**
 * RecentTasks — the "最近任務" card on Home (WP1.5, spec §5.4 list-row language).
 * The most-recently-updated tasks as slim `h-9` rows: status glyph + title +
 * assignee avatar + relative time — each row deep-links to the task detail, with
 * an "全部任務 →" footer into `/tasks`.
 *
 * Polled through `useSharedLeaderQuery` so multiple open tabs share one RPC.
 */
const RECENT_CAP = 8;
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
    <Card className="gap-0 py-0">
      <CardHeader className="pt-4 pb-2">
        <CardTitle>{intl.formatMessage({ id: 'home.recentTasks.title' })}</CardTitle>
      </CardHeader>

      {tasks.length === 0 ? (
        <Empty icon={ListTodo} title={intl.formatMessage({ id: 'home.recentTasks.empty' })} />
      ) : (
        <>
          <div className="px-2 pb-2">
            {tasks.map((t) => (
              <Link
                key={t.id}
                to={`/tasks/${encodeURIComponent(t.id)}`}
                className="flex h-9 items-center gap-2 rounded-md px-2 text-sm transition-colors hover:bg-surface-hover"
              >
                <StatusIcon status={toStatusKey(t.status)} size="sm" />
                <span className="min-w-0 flex-1 truncate text-foreground" title={t.title}>
                  {t.title}
                </span>
                {t.assigned_to && (
                  <span className="hidden shrink-0 sm:inline-flex" title={nameOf.get(t.assigned_to)}>
                    <ActorAvatar actorType="agent" size="sm" name={nameOf.get(t.assigned_to)} />
                  </span>
                )}
                <span className="shrink-0 font-mono text-xs tabular-nums text-muted-foreground">
                  {timeAgo(t.updated_at)}
                </span>
              </Link>
            ))}
          </div>
          <CardFooter className="justify-end p-0">
            <Link
              to="/tasks"
              className="flex items-center gap-1 px-4 py-3 text-xs text-muted-foreground transition-colors hover:text-foreground"
            >
              {intl.formatMessage({ id: 'home.recentTasks.viewAll' })}
              <ArrowRight className="size-3" />
            </Link>
          </CardFooter>
        </>
      )}
    </Card>
  );
}
