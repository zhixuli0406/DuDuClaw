import { cn } from '@/lib/utils';
import { formatXp } from '@/lib/format';

/**
 * XP / level display helpers (§6.1). The authoritative XP judging lives in the
 * gateway (`growth.*`, V10); these front-end helpers only turn an XP total into
 * the level + within-level progress the HUD capsule and `/growth` bar render.
 * Curve: `Lv = floor(sqrt(XP / 100))`, uncapped.
 */
export function levelFromXp(xp: number): number {
  const n = Number.isFinite(xp) ? Math.max(0, xp) : 0;
  return Math.floor(Math.sqrt(n / 100));
}

/** XP threshold at which a given level begins. */
export function xpForLevel(level: number): number {
  const l = Math.max(0, Math.floor(level));
  return l * l * 100;
}

/** Fraction [0,1] of progress from the current level toward the next. */
export function levelProgress(xp: number): number {
  const n = Number.isFinite(xp) ? Math.max(0, xp) : 0;
  const lvl = levelFromXp(n);
  const base = xpForLevel(lvl);
  const next = xpForLevel(lvl + 1);
  if (next <= base) return 0;
  return Math.min(1, Math.max(0, (n - base) / (next - base)));
}

export function XpBar({
  xp,
  showLevel = true,
  className,
}: {
  xp: number;
  showLevel?: boolean;
  className?: string;
}) {
  const lvl = levelFromXp(xp);
  const frac = levelProgress(xp);
  const pct = Math.round(frac * 100);
  const into = Math.max(0, Math.floor(xp) - xpForLevel(lvl));
  const span = xpForLevel(lvl + 1) - xpForLevel(lvl);

  return (
    <div className={cn('flex items-center gap-2', className)}>
      {showLevel && (
        <span className="shrink-0 text-xs font-semibold text-stone-700 tabular-nums dark:text-stone-200">
          Lv.{lvl}
        </span>
      )}
      <div
        className="relative h-2 min-w-16 flex-1 overflow-hidden rounded-full bg-stone-500/15 dark:bg-white/10"
        role="progressbar"
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={pct}
        aria-label={`Lv.${lvl} · ${formatXp(into)}/${formatXp(span)} XP`}
      >
        <div
          className="h-full rounded-full bg-[color:var(--xp)] transition-[width] duration-500"
          style={{ width: `${pct}%` }}
        />
      </div>
    </div>
  );
}
