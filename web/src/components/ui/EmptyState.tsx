import type { ComponentType, ReactNode } from 'react';
import { cn } from '@/lib/utils';
import { DuDu } from '@/components/mascot';
import type { DuduFace } from '@/components/mascot/faces';

/**
 * Accepts either a bare DuDu face or an object form (the `pose` field is
 * accepted for call-site symmetry with the character system; DuDu folds pose
 * into its face preset so it is not read separately).
 */
export type EmptyStateDudu = DuduFace | { face: DuduFace; pose?: string };

function duduFace(d: EmptyStateDudu): DuduFace {
  return typeof d === 'string' ? d : d.face;
}

/**
 * EmptyState — the consistent "nothing here yet" surface: icon, title, hint,
 * and an optional primary action. Pass `dudu` to swap the neutral icon chip for
 * a small DuDu wearing the given face — the friendly empty state (§7.3). When
 * `dudu` is set it takes the icon slot; existing `icon`-only calls are unchanged.
 */
export function EmptyState({
  icon: Icon,
  dudu,
  title,
  hint,
  action,
  className,
}: {
  icon?: ComponentType<{ className?: string }>;
  dudu?: EmptyStateDudu;
  title: ReactNode;
  hint?: ReactNode;
  action?: ReactNode;
  className?: string;
}) {
  return (
    <div
      className={cn(
        'flex flex-col items-center justify-center gap-3 px-6 py-12 text-center',
        className
      )}
    >
      {dudu ? (
        <DuDu face={duduFace(dudu)} size="sm" />
      ) : (
        Icon && (
          <span className="grid h-12 w-12 place-items-center rounded-2xl bg-stone-500/8 text-stone-400 dark:bg-white/5">
            <Icon className="h-6 w-6" />
          </span>
        )
      )}
      <div className="space-y-1">
        <p className="text-sm font-medium text-stone-700 dark:text-stone-200">{title}</p>
        {hint && (
          <p className="mx-auto max-w-sm text-xs text-stone-500 dark:text-stone-400">{hint}</p>
        )}
      </div>
      {action}
    </div>
  );
}
