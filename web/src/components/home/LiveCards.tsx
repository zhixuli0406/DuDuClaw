import { useMemo } from 'react';
import { useIntl } from 'react-intl';
import { Link } from 'react-router';
import { ArrowRight, Radio } from 'lucide-react';
import { api, type ActivityEvent, type TaskInfo } from '@/lib/api';
import { CharacterAvatar, Card, Mono } from '@/components/ui';
import { useSharedLeaderQuery } from '@/hooks/useSharedLeaderQuery';

/**
 * LiveCards — the "正在進行" live board on Home (V3-T3.3). One card per agent that
 * currently has an in-progress task: character bust + live pulse, the task it's
 * on (deep-linked to the task), and a live step-flow region.
 *
 * Step-flow honesty (C-P1): the real tool step tree streams over the *webchat*
 * WS, a source the dashboard's main WS doesn't carry. Until W6 wires those step
 * events in, this shows the agent's latest activity tail (activity.list, ≤5
 * lines) as a truthful stand-in — real signal, just coarser than per-tool steps.
 *
 * TODO(v2-W6): wire C-P1 step events (webchat `{type:"step",...}` frames) into
 * the step-flow region below, replacing the activity tail.
 */
const CARD_CAP = 4;
/** Lines of activity tail shown per card. */
const TAIL_LINES = 5;
const POLL_MS = 8000;

interface LivePayload {
  tasks: TaskInfo[];
  tails: Record<string, ActivityEvent[]>;
}

async function fetchLive(): Promise<LivePayload> {
  const res = await api.tasks.list({ status: 'in_progress' }).catch(() => null);
  const tasks = res?.tasks ?? [];
  const agentIds = [...new Set(tasks.map((t) => t.assigned_to).filter((x): x is string => !!x))].slice(0, CARD_CAP);
  const entries = await Promise.all(
    agentIds.map((id) =>
      api.activity
        .list({ agent_id: id, limit: TAIL_LINES })
        .then((r): [string, ActivityEvent[]] => [id, r?.events ?? []])
        .catch((): [string, ActivityEvent[]] => [id, []]),
    ),
  );
  return { tasks, tails: Object.fromEntries(entries) };
}

export interface LiveCardsProps {
  /** Visible (data-scoped) agents — a card only renders for an agent in this set. */
  agents: ReadonlyArray<{ name: string; display_name: string }>;
  enabled: boolean;
}

export function LiveCards({ agents, enabled }: LiveCardsProps) {
  const intl = useIntl();
  const { data } = useSharedLeaderQuery<LivePayload>('home:live', fetchLive, POLL_MS, enabled);

  const nameOf = useMemo(() => {
    const m = new Map<string, string>();
    for (const a of agents) m.set(a.name, a.display_name || a.name);
    return m;
  }, [agents]);

  // Live agents: those with an in-progress task AND inside the visible scope.
  const cards = useMemo(() => {
    const tasks = data?.tasks ?? [];
    const seen = new Set<string>();
    const out: { agentId: string; task: TaskInfo }[] = [];
    for (const t of tasks) {
      const id = t.assigned_to;
      if (!id || seen.has(id) || !nameOf.has(id)) continue;
      seen.add(id);
      out.push({ agentId: id, task: t });
      if (out.length >= CARD_CAP) break;
    }
    return out;
  }, [data, nameOf]);

  const liveTotal = useMemo(() => {
    const s = new Set<string>();
    for (const t of data?.tasks ?? []) if (t.assigned_to && nameOf.has(t.assigned_to)) s.add(t.assigned_to);
    return s.size;
  }, [data, nameOf]);

  const overflow = liveTotal - cards.length;

  // No active work → collapse the whole region to a single calm line.
  if (cards.length === 0) {
    return (
      <div className="flex items-center gap-2 rounded-xl border border-dashed border-stone-200 px-4 py-3 text-sm text-stone-400 dark:border-stone-700 dark:text-stone-500">
        <Radio className="h-4 w-4" />
        {intl.formatMessage({ id: 'home.live.idle' })}
      </div>
    );
  }

  return (
    <section aria-label={intl.formatMessage({ id: 'home.live.title' })} className="space-y-3">
      <div className="flex items-center justify-between">
        <h2 className="text-sm font-semibold text-stone-800 dark:text-stone-100">
          {intl.formatMessage({ id: 'home.live.title' })}
        </h2>
        {overflow > 0 && (
          <Link
            to="/tasks"
            className="flex items-center gap-1 text-xs text-stone-500 transition-colors hover:text-amber-600 dark:text-stone-400 dark:hover:text-amber-400"
          >
            {intl.formatMessage({ id: 'home.live.more' }, { count: overflow })}
            <ArrowRight className="h-3 w-3" />
          </Link>
        )}
      </div>

      <div className="grid gap-4 sm:grid-cols-2">
        {cards.map(({ agentId, task }) => {
          const tail = data?.tails?.[agentId] ?? [];
          return (
            <Card key={agentId} className="flex h-56 flex-col" bodyClassName="flex min-h-0 flex-1 flex-col gap-3">
              <div className="flex items-center gap-3">
                <CharacterAvatar agentId={agentId} name={nameOf.get(agentId)} size={44} variant="bust" pose="working" live />
                <div className="min-w-0 flex-1">
                  <p className="truncate text-sm font-medium text-stone-900 dark:text-stone-50">
                    {nameOf.get(agentId)}
                  </p>
                  <Link
                    to={`/tasks/${encodeURIComponent(task.id)}`}
                    className="block truncate text-xs text-stone-500 transition-colors hover:text-amber-600 dark:text-stone-400 dark:hover:text-amber-400"
                    title={task.title}
                  >
                    {task.title}
                  </Link>
                </div>
              </div>

              {/* Step-flow region — activity tail stand-in until C-P1 steps (W6). */}
              <div className="min-h-0 flex-1 overflow-hidden rounded-lg bg-stone-500/5 p-2.5 dark:bg-white/5">
                {tail.length === 0 ? (
                  <p className="text-[11px] text-stone-400 dark:text-stone-500">
                    {intl.formatMessage({ id: 'home.live.noSteps' })}
                  </p>
                ) : (
                  <ul className="space-y-1">
                    {tail.slice(0, TAIL_LINES).map((ev) => (
                      <li key={ev.id} className="flex items-start gap-1.5 text-[11px] leading-snug text-stone-500 dark:text-stone-400">
                        <span className="mt-1 h-1 w-1 shrink-0 rounded-full bg-stone-400/70 dark:bg-stone-500" />
                        <span className="min-w-0 flex-1 truncate font-mono" title={ev.summary}>
                          {ev.summary}
                        </span>
                        <Mono className="shrink-0">{shortTime(ev.timestamp)}</Mono>
                      </li>
                    ))}
                  </ul>
                )}
              </div>
            </Card>
          );
        })}
      </div>
    </section>
  );
}

/** Compact HH:MM for the tail rows. Invalid input → empty. */
function shortTime(ts: string): string {
  const t = new Date(ts).getTime();
  if (!Number.isFinite(t)) return '';
  const d = new Date(t);
  return `${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`;
}
