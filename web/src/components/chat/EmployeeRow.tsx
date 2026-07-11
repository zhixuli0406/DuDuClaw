import { useIntl } from 'react-intl';
import { cn } from '@/lib/utils';
import { CharacterAvatar, agentPose, type AgentLifecycle } from '@/components/character';
import { DuDu } from '@/components/mascot';

/** Minimal agent shape the row needs (a slice of AgentInfo). */
export interface EmployeeRowAgent {
  readonly name: string;
  readonly display_name: string;
  readonly status: AgentLifecycle;
}

/**
 * EmployeeRow — the top staff strip on the conversation page (V7 / T7.2). A
 * horizontal row of CharacterAvatars; tapping one picks it as the conversation
 * partner (visual identity swap). The leading paw chip returns to DuDu, the
 * office assistant (the default, `selectedId === null`).
 */
export function EmployeeRow({
  agents,
  selectedId,
  onSelect,
}: {
  agents: readonly EmployeeRowAgent[];
  selectedId: string | null;
  onSelect: (id: string | null) => void;
}) {
  const intl = useIntl();

  return (
    <div
      role="radiogroup"
      aria-label={intl.formatMessage({ id: 'chat.employees.title', defaultMessage: 'AI staff' })}
      className="flex items-center gap-2 overflow-x-auto px-4 py-2"
    >
      {/* DuDu — the office assistant / default partner. */}
      <button
        type="button"
        role="radio"
        aria-checked={selectedId === null}
        onClick={() => onSelect(null)}
        title={intl.formatMessage({
          id: 'chat.employees.duduHint',
          defaultMessage: 'Back to DuDu (office assistant)',
        })}
        className={cn(
          'flex shrink-0 items-center gap-1.5 rounded-control px-1.5 py-1 transition-colors',
          'focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-500/40',
          selectedId === null
            ? 'bg-amber-500/15 ring-1 ring-inset ring-amber-500/40'
            : 'hover:bg-stone-500/10 dark:hover:bg-white/5',
        )}
      >
        <DuDu face="idle" size={24} animated={false} label="DuDu" />
      </button>

      {agents.map((a) => {
        const selected = a.name === selectedId;
        return (
          <button
            key={a.name}
            type="button"
            role="radio"
            aria-checked={selected}
            onClick={() => onSelect(a.name)}
            title={a.display_name}
            className={cn(
              'flex shrink-0 items-center rounded-control p-1 transition-colors',
              'focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-500/40',
              selected
                ? 'bg-amber-500/15 ring-1 ring-inset ring-amber-500/40'
                : 'hover:bg-stone-500/10 dark:hover:bg-white/5',
            )}
          >
            <CharacterAvatar
              agentId={a.name}
              name={a.display_name}
              size={28}
              variant="avatar"
              pose={agentPose(a.status)}
              animated={false}
            />
          </button>
        );
      })}
    </div>
  );
}
