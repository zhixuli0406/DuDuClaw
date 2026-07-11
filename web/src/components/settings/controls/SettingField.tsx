import { useId, type ReactNode } from 'react';
import { cn } from '@/lib/utils';

/**
 * SettingField — the standard "everyday" settings row: a plain-language label,
 * a one-line description of what it does (and what changes when you touch it),
 * then the control. Use this instead of hand-rolling label + help markup so
 * every settings screen reads consistently for non-technical owners.
 *
 * `layout="row"` places the control to the right of the label (good for
 * toggles / short selects); the default stacks control under the label.
 */
export function SettingField({
  label,
  help,
  htmlFor,
  layout = 'stack',
  children,
  className,
}: {
  label: ReactNode;
  help?: ReactNode;
  htmlFor?: string;
  layout?: 'stack' | 'row';
  children: ReactNode;
  className?: string;
}) {
  const autoId = useId();
  const id = htmlFor ?? autoId;

  if (layout === 'row') {
    return (
      <div
        className={cn(
          'flex items-center justify-between gap-4 border-b border-[var(--panel-border)] py-3 last:border-0',
          className,
        )}
      >
        <div className="min-w-0">
          <label htmlFor={id} className="block text-sm font-medium text-stone-700 dark:text-stone-200">
            {label}
          </label>
          {help && <p className="mt-0.5 text-xs text-stone-400 dark:text-stone-500">{help}</p>}
        </div>
        <div className="shrink-0">{children}</div>
      </div>
    );
  }

  return (
    <div className={cn('space-y-1.5', className)}>
      <label htmlFor={id} className="block text-sm font-medium text-stone-700 dark:text-stone-200">
        {label}
      </label>
      {help && <p className="text-xs text-stone-400 dark:text-stone-500">{help}</p>}
      {children}
    </div>
  );
}
