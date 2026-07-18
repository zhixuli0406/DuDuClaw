import { useMemo } from 'react';
import { useIntl } from 'react-intl';
import { Link } from 'react-router';
import { ArrowRight } from 'lucide-react';
import { api, type TaskInfo } from '@/lib/api';
import {
  Card,
  CardHeader,
  CardTitle,
  CardAction,
  ActorAvatar,
} from '@/components/mds';
import { timeAgo } from '@/lib/format';
import { useSharedLeaderQuery } from '@/hooks/useSharedLeaderQuery';

/**
 * LiveCards — the "進行中" board on Home (WP1.5, spec §5.5). A slim row per AI
 * staff member with an in-progress task: avatar + name + a live status dot + the
 * task it's on (one line) + relative time. Each row deep-links to that staff
 * member's detail. No running work → a single calm line instead of a card wall.
 *
 * Polled through `useSharedLeaderQuery` so multiple open tabs share one RPC.
 */
const ROW_CAP = 6;
const POLL_MS = 8000;

async function fetchLive(): Promise<TaskInfo[]> {
  const res = await api.tasks.list({ status: 'in_progress' }).catch(() => null);
  return res?.tasks ?? [];
}

export interface LiveCardsProps {
  /** Visible (data-scoped) agents — a row only renders for an agent in this set. */
  agents: ReadonlyArray<{ name: string; display_name: string }>;
  enabled: boolean;
}

export function LiveCards({ agents, enabled }: LiveCardsProps) {
  const intl = useIntl();
  const { data } = useSharedLeaderQuery<TaskInfo[]>('home:live', fetchLive, POLL_MS, enabled);

  const nameOf = useMemo(() => {
    const m = new Map<string, string>();
    for (const a of agents) m.set(a.name, a.display_name || a.name);
    return m;
  }, [agents]);

  // Live rows: agents with an in-progress task AND inside the visible scope.
  const rows = useMemo(() => {
    const tasks = data ?? [];
    const seen = new Set<string>();
    const out: { agentId: string; task: TaskInfo }[] = [];
    for (const t of tasks) {
      const id = t.assigned_to;
      if (!id || seen.has(id) || !nameOf.has(id)) continue;
      seen.add(id);
      out.push({ agentId: id, task: t });
    }
    return out;
  }, [data, nameOf]);

  const overflow = rows.length - ROW_CAP;
  const shown = rows.slice(0, ROW_CAP);

  return (
    <Card>
      <CardHeader>
        <CardTitle>{intl.formatMessage({ id: 'home.live.title' })}</CardTitle>
        {overflow > 0 && (
          <CardAction>
            <Link
              to="/runs"
              className="flex items-center gap-1 text-xs text-muted-foreground transition-colors hover:text-foreground"
            >
              {intl.formatMessage({ id: 'home.live.more' }, { count: overflow })}
              <ArrowRight className="size-3" />
            </Link>
          </CardAction>
        )}
      </CardHeader>

      {shown.length === 0 ? (
        <p className="px-4 pb-1 text-sm text-muted-foreground">
          {intl.formatMessage({ id: 'home.live.idle' })}
        </p>
      ) : (
        <div className="px-2">
          {shown.map(({ agentId, task }) => (
            <Link
              key={agentId}
              to={`/agents/${encodeURIComponent(agentId)}`}
              className="flex h-12 items-center gap-3 rounded-md px-2 transition-colors hover:bg-surface-hover"
            >
              <ActorAvatar
                actorType="agent"
                size="lg"
                name={nameOf.get(agentId)}
                showStatusDot
                status="busy"
              />
              <div className="min-w-0 flex-1">
                <p className="truncate text-sm font-medium text-foreground">
                  {nameOf.get(agentId)}
                </p>
                <p className="truncate text-xs text-muted-foreground" title={task.title}>
                  {task.title}
                </p>
              </div>
              <span className="shrink-0 font-mono text-xs tabular-nums text-muted-foreground">
                {timeAgo(task.updated_at)}
              </span>
            </Link>
          ))}
        </div>
      )}
    </Card>
  );
}
