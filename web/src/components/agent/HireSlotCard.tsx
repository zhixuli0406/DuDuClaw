import { useIntl } from 'react-intl';
import { UserPlus } from 'lucide-react';

/**
 * HireSlotCard — the dashed "hire a new employee" slot at the end of the roster
 * grid (§5.4 T6.1, paperclip AgentCapsule empty-slot concept). Opens the
 * existing create-agent flow.
 */
export function HireSlotCard({ onClick }: { onClick: () => void }) {
  const intl = useIntl();
  return (
    <button
      type="button"
      onClick={onClick}
      className={
        'group flex min-h-[13rem] flex-col items-center justify-center gap-3 rounded-card ' +
        'border-2 border-dashed border-[var(--panel-border)] p-5 text-center ' +
        'text-stone-400 transition-colors hover:border-amber-500/50 hover:text-amber-600 ' +
        'focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-500/50 ' +
        'dark:text-stone-500 dark:hover:text-amber-400'
      }
    >
      <span className="grid h-14 w-14 place-items-center rounded-full bg-stone-500/10 transition-colors group-hover:bg-amber-500/15 dark:bg-white/5">
        <UserPlus className="h-6 w-6" />
      </span>
      <span className="text-sm font-medium">
        {intl.formatMessage({ id: 'agents.roster.hire' })}
      </span>
    </button>
  );
}
