import type { ReactNode } from 'react';
import { cn } from '@/lib/utils';

/**
 * EntityCard — the personified card used across Zone C (AI staff, skills, …):
 * an avatar/glyph, a name, a one-line "what it is / what it's doing", an
 * optional status glyph, and a footer meta row (dashboard-redesign §8). Built on
 * the Calm Glass `panel` + `panel-hover`; clickable when `onClick` is set.
 */
export function EntityCard({
  avatar,
  title,
  subtitle,
  status,
  meta,
  onClick,
  className,
}: {
  /** Emoji, image, or glyph node shown in the leading avatar slot. */
  avatar: ReactNode;
  title: ReactNode;
  subtitle?: ReactNode;
  /** Small status node (e.g. AgentStatusGlyph) rendered top-right. */
  status?: ReactNode;
  /** Footer meta row (cost, counts) — usually <Mono> values. */
  meta?: ReactNode;
  onClick?: () => void;
  className?: string;
}) {
  const interactive = !!onClick;
  const Wrapper = interactive ? 'button' : 'div';
  return (
    <Wrapper
      type={interactive ? 'button' : undefined}
      onClick={onClick}
      className={cn(
        'panel flex w-full flex-col gap-3 p-4 text-left',
        interactive && 'panel-hover focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-500/40',
        className,
      )}
    >
      <div className="flex items-start gap-3">
        <span className="grid h-11 w-11 shrink-0 place-items-center rounded-xl bg-stone-500/10 text-xl dark:bg-white/5">
          {avatar}
        </span>
        <div className="min-w-0 flex-1">
          <div className="flex items-center justify-between gap-2">
            <span className="truncate font-medium text-stone-900 dark:text-stone-50">{title}</span>
            {status && <span className="shrink-0">{status}</span>}
          </div>
          {subtitle && (
            <p className="mt-0.5 line-clamp-2 text-xs text-stone-500 dark:text-stone-400">{subtitle}</p>
          )}
        </div>
      </div>
      {meta && (
        <div className="flex flex-wrap items-center gap-x-3 gap-y-1 border-t border-[var(--panel-border)] pt-2 text-xs text-stone-400 dark:text-stone-500">
          {meta}
        </div>
      )}
    </Wrapper>
  );
}
