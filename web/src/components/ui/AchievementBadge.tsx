import type { ComponentType } from 'react';
import { Lock } from 'lucide-react';
import { cn } from '@/lib/utils';

/**
 * AchievementBadge — one entry on the `/growth` achievement wall (openhuman O4,
 * §6.3). Unlocked badges glow in amber; locked ones desaturate and show a small
 * progress bar toward unlocking. The unlock burst animation is CelebrationLayer's
 * job — this is the resting card.
 */
export function AchievementBadge({
  icon: Icon,
  name,
  description,
  unlocked,
  progress,
  unlockedAt,
  className,
}: {
  icon: ComponentType<{ className?: string }>;
  name: string;
  description?: string;
  unlocked: boolean;
  /** Fraction [0,1] toward unlocking; shown only while locked. */
  progress?: number;
  /** Human-readable unlock date (already formatted). */
  unlockedAt?: string;
  className?: string;
}) {
  const pct = Math.round(Math.min(1, Math.max(0, progress ?? 0)) * 100);
  return (
    <div
      className={cn(
        'panel flex items-start gap-3 p-3',
        !unlocked && 'opacity-70',
        className,
      )}
      aria-label={name}
    >
      <span
        className={cn(
          'grid h-11 w-11 shrink-0 place-items-center rounded-card ring-1 ring-inset',
          unlocked
            ? 'bg-amber-500/15 text-amber-600 ring-amber-500/30 dark:text-amber-300'
            : 'bg-stone-500/10 text-stone-400 ring-stone-500/15 grayscale dark:text-stone-500',
        )}
      >
        {unlocked ? <Icon className="h-6 w-6" /> : <Lock className="h-5 w-5" aria-hidden="true" />}
      </span>
      <div className="min-w-0 flex-1">
        <p className="truncate text-sm font-semibold text-stone-900 dark:text-stone-50">{name}</p>
        {description && (
          <p className="mt-0.5 line-clamp-2 text-xs text-stone-500 dark:text-stone-400">{description}</p>
        )}
        {unlocked ? (
          unlockedAt && (
            <p className="mt-1 font-mono text-[0.6875rem] tabular-nums text-stone-400 dark:text-stone-500">
              {unlockedAt}
            </p>
          )
        ) : (
          <div className="mt-2 h-1.5 overflow-hidden rounded-full bg-stone-500/15 dark:bg-white/10">
            <div className="h-full rounded-full bg-[color:var(--xp)]" style={{ width: `${pct}%` }} />
          </div>
        )}
      </div>
    </div>
  );
}
