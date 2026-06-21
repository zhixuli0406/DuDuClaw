import type { ReactNode } from 'react';
import { cn } from '@/lib/utils';

/**
 * Card — the default content surface (Calm Glass `panel`). Optional title +
 * actions header. Set `interactive`/`onClick` for clickable cards (adds hover
 * lift). Use `padded={false}` when the child manages its own padding (tables).
 */
export function Card({
  children,
  title,
  actions,
  className,
  bodyClassName,
  padded = true,
  interactive = false,
  onClick,
}: {
  children?: ReactNode;
  title?: ReactNode;
  actions?: ReactNode;
  className?: string;
  bodyClassName?: string;
  padded?: boolean;
  interactive?: boolean;
  onClick?: () => void;
}) {
  const clickable = interactive || !!onClick;
  return (
    <div
      className={cn('panel overflow-hidden', clickable && 'panel-hover', className)}
      onClick={onClick}
      role={onClick ? 'button' : undefined}
      tabIndex={onClick ? 0 : undefined}
      onKeyDown={
        onClick
          ? (e) => {
              if (e.key === 'Enter' || e.key === ' ') {
                e.preventDefault();
                onClick();
              }
            }
          : undefined
      }
    >
      {(title || actions) && (
        <div className="flex items-center justify-between gap-3 border-b border-[var(--panel-border)] px-5 py-3">
          {title && (
            <h2 className="text-sm font-semibold text-stone-800 dark:text-stone-100">
              {title}
            </h2>
          )}
          {actions && <div className="flex items-center gap-2">{actions}</div>}
        </div>
      )}
      <div className={cn(padded && 'p-5', bodyClassName)}>{children}</div>
    </div>
  );
}
