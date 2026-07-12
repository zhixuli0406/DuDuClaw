import { useIntl } from 'react-intl';
import { Archive, RotateCcw } from 'lucide-react';
import { cn } from '@/lib/utils';
import type { AgentDetail, TaskInfo } from '@/lib/api';
import { Badge, CharacterAvatar, agentPose, agentEmote } from '@/components/ui';
import { useAgentGlyphState } from '@/stores/agent-activity-store';
import { agentTaskStats, isLiveState, staffLevel } from './agent-stats';
import type { AgentLifecycle } from '@/components/character';

/**
 * RosterCard — one AI-staff character card on `/agents` (§5.4 T6.1). A bust
 * portrait posed by real status, the display name, a job title, a live dot, a
 * derived level pill, and a "done today" line. The whole card is the click
 * target into the staff detail page.
 *
 * Level + today's tally are *front-end derivations* from the tasks store — see
 * `agent-stats.ts` for the honesty note (there is no per-agent XP column).
 */
export function RosterCard({
  agent,
  tasks,
  onOpen,
  onUnarchive,
}: {
  agent: AgentDetail;
  tasks: ReadonlyArray<TaskInfo>;
  onOpen: (name: string) => void;
  /** Present only for archived staff — renders a restore affordance. */
  onUnarchive?: () => void;
}) {
  const intl = useIntl();
  const glyph = useAgentGlyphState(agent.name, agent.status);
  const live = isLiveState(glyph);
  const lifecycle = agent.status as AgentLifecycle;
  const pose = agentPose(lifecycle, live);
  const emote = agentEmote(lifecycle, live);
  const stats = agentTaskStats(tasks, agent.name);
  const level = staffLevel(stats.done);

  // Job title: use the localized role — the closest config field to a "title"
  // (§5.4 says fall back to the agent id when no title field exists; role is a
  // real config field so it wins over the raw id).
  const title = agent.role
    ? intl.formatMessage({ id: `agents.role.${agent.role}` })
    : agent.name;

  return (
    <div className="relative">
      {agent.archived && onUnarchive && (
        <button
          type="button"
          onClick={onUnarchive}
          title={intl.formatMessage({ id: 'agents.unarchive' })}
          aria-label={intl.formatMessage({ id: 'agents.unarchive' })}
          className="absolute right-2 top-2 z-10 grid h-8 w-8 place-items-center rounded-full bg-white/80 text-amber-600 shadow-soft ring-1 ring-inset ring-amber-500/30 backdrop-blur transition-colors hover:bg-amber-500/15 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-500/50 dark:bg-stone-800/80 dark:text-amber-400"
        >
          <RotateCcw className="h-4 w-4" />
        </button>
      )}
    <button
      type="button"
      onClick={() => onOpen(agent.name)}
      className={cn(
        'panel group flex w-full flex-col items-center gap-2 p-5 text-center',
        'transition-[transform,box-shadow] duration-200 hover:-translate-y-1',
        'hover:shadow-[var(--shadow-pop)] focus-visible:outline-none',
        'focus-visible:ring-2 focus-visible:ring-amber-500/50',
        agent.archived && 'opacity-70',
      )}
      aria-label={agent.display_name}
    >
      <CharacterAvatar
        agentId={agent.name}
        name={agent.display_name}
        size={96}
        variant="bust"
        pose={pose}
        emote={emote}
        live={live}
      />

      <div className="min-w-0">
        <h3 className="truncate text-sm font-semibold text-stone-900 dark:text-stone-50">
          {agent.display_name}
        </h3>
        <p className="truncate text-xs text-stone-500 dark:text-stone-400">{title}</p>
        {agent.department && (
          <p className="mt-0.5 truncate text-[0.6875rem] text-stone-400 dark:text-stone-500">
            {agent.department}
          </p>
        )}
      </div>

      {agent.archived && (
        <Badge tone="warning">
          <Archive className="h-3 w-3" />
          {intl.formatMessage({ id: 'agents.archived.badge' })}
        </Badge>
      )}

      <div className="flex items-center gap-1.5">
        <span
          className="rounded-full bg-[color:var(--xp)]/15 px-2 py-0.5 text-xs font-semibold tabular-nums text-amber-700 dark:text-amber-300"
          title={intl.formatMessage(
            { id: 'agents.roster.lvBasis' },
            { done: stats.done },
          )}
        >
          Lv.{level}
        </span>
        {live && (
          <span
            className="inline-flex h-2 w-2 rounded-full bg-[color:var(--status-agent-running)]"
            aria-label={intl.formatMessage({ id: 'live.badge', defaultMessage: 'Live' })}
          />
        )}
      </div>

      <p className="text-xs text-stone-400 tabular-nums dark:text-stone-500">
        {intl.formatMessage(
          { id: 'agents.roster.todayDone' },
          { count: stats.todayDone },
        )}
      </p>
    </button>
    </div>
  );
}
