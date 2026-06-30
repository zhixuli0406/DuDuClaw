import { useIntl } from 'react-intl';
import { Sparkles, LayoutGrid } from 'lucide-react';
import { useUiModeStore, type UiMode } from '@/stores/ui-mode-store';
import { cn } from '@/lib/utils';

/**
 * Simple ⇄ Advanced shell switch (TODO-genspark-workspace-shell §P5.1).
 * A two-segment control that flips the whole shell between the workspace
 * (consumer) and dashboard (power-user) experiences.
 */
export function ModeToggle() {
  const intl = useIntl();
  const mode = useUiModeStore((s) => s.mode);
  const setMode = useUiModeStore((s) => s.setMode);

  const segments: ReadonlyArray<{ value: UiMode; icon: typeof Sparkles; label: string }> = [
    { value: 'workspace', icon: Sparkles, label: 'mode.workspace' },
    { value: 'dashboard', icon: LayoutGrid, label: 'mode.dashboard' },
  ];

  return (
    <div
      role="radiogroup"
      aria-label={intl.formatMessage({ id: 'mode.label', defaultMessage: '介面模式' })}
      className="flex items-center rounded-lg border border-[var(--panel-border)] p-0.5"
    >
      {segments.map(({ value, icon: Icon, label }) => {
        const active = mode === value;
        return (
          <button
            key={value}
            type="button"
            role="radio"
            aria-checked={active}
            onClick={() => setMode(value)}
            title={intl.formatMessage({ id: label })}
            className={cn(
              'flex items-center gap-1.5 rounded-md px-2.5 py-1 text-xs font-medium transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-500/40',
              active
                ? 'bg-amber-500/15 text-amber-700 dark:bg-amber-400/10 dark:text-amber-300'
                : 'text-stone-500 hover:text-stone-700 dark:text-stone-400 dark:hover:text-stone-200'
            )}
          >
            <Icon className="h-3.5 w-3.5" />
            <span className="hidden md:inline">{intl.formatMessage({ id: label })}</span>
          </button>
        );
      })}
    </div>
  );
}
