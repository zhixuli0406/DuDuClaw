import { useIntl } from 'react-intl';
import { MessageSquare, Send, Pause, Play, Award } from 'lucide-react';
import type { AgentDetail } from '@/lib/api';
import {
  Button,
  Badge,
  CharacterAvatar,
  XpBar,
  agentPose,
  agentEmote,
} from '@/components/ui';
import type { AgentLifecycle } from '@/components/character';
import { computeMood } from '@/lib/mascot-mood';
import { XP_PER_DONE_TASK } from './agent-stats';

/**
 * AgentHero — the reworked staff-detail hero (§5.4 T6.2): a 128px bust posed by
 * real status, name + job title, a one-line mood, an XP bar (level derived from
 * completed tasks), a skill-badge row, and quick actions (chat / delegate /
 * rest·resume).
 *
 * Mood is an *honest approximation* of the whole-company `computeMood`: with a
 * single agent we treat it as a one-member company — a live run reads `focused`,
 * an idle-online agent `relaxed`; paused/terminated map to resting/offline
 * (states `computeMood` doesn't model). It carries no error/inbox signal per
 * agent, so it never shows `alert`/`poke`. This is display-only.
 */
export function AgentHero({
  detail,
  live,
  doneCount,
  busy,
  onChat,
  onDelegate,
  onPause,
  onResume,
}: {
  detail: AgentDetail;
  live: boolean;
  doneCount: number;
  busy: boolean;
  onChat: () => void;
  onDelegate: () => void;
  onPause: () => void;
  onResume: () => void;
}) {
  const intl = useIntl();
  const lifecycle = detail.status as AgentLifecycle;
  const pose = agentPose(lifecycle, live);
  const emote = agentEmote(lifecycle, live);

  const title = detail.role
    ? intl.formatMessage({ id: `agents.role.${detail.role}` })
    : detail.name;

  // Mood → i18n key + emoji.
  const moodKey =
    lifecycle === 'paused'
      ? 'resting'
      : lifecycle === 'terminated'
        ? 'offline'
        : computeMood({ total: 1, active: live ? 1 : 0, error: 0, inbox: 0 }); // focused | relaxed
  const moodEmoji: Record<string, string> = {
    focused: '🧐',
    relaxed: '😌',
    resting: '💤',
    offline: '😴',
  };

  const skills = detail.skills ?? [];
  const topSkills = skills.slice(0, 5);

  return (
    <div className="panel flex flex-col gap-5 p-5 sm:flex-row sm:items-center">
      <div className="flex shrink-0 justify-center">
        <CharacterAvatar
          agentId={detail.name}
          name={detail.display_name}
          size={128}
          variant="bust"
          pose={pose}
          emote={emote}
          live={live}
        />
      </div>

      <div className="min-w-0 flex-1 space-y-3">
        <div className="min-w-0">
          <div className="flex flex-wrap items-center gap-2">
            <h1 className="truncate text-xl font-semibold tracking-tight text-stone-900 dark:text-stone-50">
              {detail.display_name}
            </h1>
            <Badge tone={lifecycle === 'active' ? 'success' : lifecycle === 'paused' ? 'warning' : 'danger'} dot>
              {intl.formatMessage({ id: `status.${detail.status}` })}
            </Badge>
          </div>
          <p className="mt-0.5 truncate text-sm text-stone-500 dark:text-stone-400">{title}</p>
          <p className="mt-1 text-sm text-stone-600 dark:text-stone-300">
            <span aria-hidden="true">{moodEmoji[moodKey] ?? '😌'} </span>
            {intl.formatMessage({ id: `agentDetail.mood.${moodKey}` })}
          </p>
        </div>

        {/* XP bar — level derived from completed tasks (§6.2). */}
        <div className="max-w-sm">
          <XpBar xp={doneCount * XP_PER_DONE_TASK} />
          <p className="mt-1 text-[0.6875rem] text-stone-400 dark:text-stone-500">
            {intl.formatMessage({ id: 'agentDetail.xp.basis' }, { done: doneCount })}
          </p>
        </div>

        {/* Skill badges (top 5). */}
        {topSkills.length > 0 && (
          <div className="flex flex-wrap items-center gap-1.5">
            {topSkills.map((s) => (
              <span
                key={s}
                className="inline-flex items-center gap-1 rounded-full bg-amber-500/12 px-2.5 py-1 text-xs font-medium text-amber-700 ring-1 ring-inset ring-amber-500/25 dark:text-amber-300"
              >
                <Award className="h-3 w-3" aria-hidden="true" />
                {s}
              </span>
            ))}
            {skills.length > topSkills.length && (
              <span className="text-xs text-stone-400 tabular-nums dark:text-stone-500">
                +{skills.length - topSkills.length}
              </span>
            )}
          </div>
        )}

        {/* Quick actions. */}
        <div className="flex flex-wrap items-center gap-2 pt-1">
          <Button size="sm" variant="primary" icon={MessageSquare} onClick={onChat}>
            {intl.formatMessage({ id: 'agentDetail.action.chat' })}
          </Button>
          <Button size="sm" variant="secondary" icon={Send} onClick={onDelegate}>
            {intl.formatMessage({ id: 'agentDetail.action.delegate' })}
          </Button>
          {lifecycle === 'active' ? (
            <Button size="sm" variant="ghost" icon={Pause} disabled={busy} onClick={onPause}>
              {intl.formatMessage({ id: 'agentDetail.rest' })}
            </Button>
          ) : (
            <Button size="sm" variant="ghost" icon={Play} disabled={busy} onClick={onResume}>
              {intl.formatMessage({ id: 'agentDetail.resume' })}
            </Button>
          )}
        </div>
      </div>
    </div>
  );
}
