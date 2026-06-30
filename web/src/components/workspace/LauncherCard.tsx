import { useIntl } from 'react-intl';
import { useNavigate } from 'react-router';
import { Badge } from '@/components/ui';
import { useUiModeStore } from '@/stores/ui-mode-store';
import { cn } from '@/lib/utils';
import { ACCENT_CLASS, type LauncherCardModel } from './launcher-model';

/**
 * A single capability launcher tile (TODO-genspark-workspace-shell §P3.2).
 * `ready` cards navigate (switching to dashboard mode for dashboard routes);
 * `coming-soon` cards render greyed and inert with a badge.
 */
export function LauncherCard({ card }: { card: LauncherCardModel }) {
  const intl = useIntl();
  const navigate = useNavigate();
  const setMode = useUiModeStore((s) => s.setMode);

  const label = intl.formatMessage({ id: `launcher.${card.id}.label` });
  const desc = intl.formatMessage({ id: `launcher.${card.id}.desc` });
  const Icon = card.icon;
  const disabled = card.status === 'coming-soon';

  const handleActivate = () => {
    if (disabled || !card.to) return;
    // Workspace stays mounted for in-workspace chat (/webchat is rendered
    // inline); dashboard routes flip the shell to dashboard mode.
    if (card.to !== '/webchat') setMode('dashboard');
    navigate(card.to);
  };

  return (
    <button
      type="button"
      onClick={handleActivate}
      disabled={disabled}
      aria-label={label}
      className={cn(
        'panel group flex h-full flex-col items-start gap-2 rounded-xl p-4 text-left',
        disabled
          ? 'cursor-not-allowed opacity-55'
          : 'panel-hover focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-500/40'
      )}
    >
      <span
        className={cn(
          'grid h-10 w-10 place-items-center rounded-xl',
          disabled ? 'bg-stone-500/10 text-stone-400' : ACCENT_CLASS[card.accent]
        )}
      >
        <Icon className="h-5 w-5" />
      </span>
      <div className="min-w-0">
        <div className="flex items-center gap-1.5">
          <span className="text-sm font-semibold text-stone-800 dark:text-stone-100">{label}</span>
          {disabled && (
            <Badge tone="neutral">
              {intl.formatMessage({ id: 'workspace.comingSoon', defaultMessage: '即將推出' })}
            </Badge>
          )}
        </div>
        <p className="mt-0.5 line-clamp-2 text-xs text-stone-500 dark:text-stone-400">{desc}</p>
      </div>
    </button>
  );
}
