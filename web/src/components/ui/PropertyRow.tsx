import type { ComponentType, ReactNode } from 'react';
import { cn } from '@/lib/utils';

/**
 * PropertySection / PropertyRow — the labeled key→value rows that fill the
 * right-hand PropertiesPanel (paperclip P2 "Triage / Relationships / Execution
 * / About"). PropertySection groups rows under a small caption; PropertyRow is
 * one label-left / value-right line, optionally interactive (opens a popover).
 */
export function PropertySection({
  title,
  children,
  className,
}: {
  title?: ReactNode;
  children: ReactNode;
  className?: string;
}) {
  return (
    <section className={cn('space-y-0.5', className)}>
      {title && (
        <h3 className="px-1 pb-1 text-[0.6875rem] font-semibold uppercase tracking-wide text-muted-foreground">
          {title}
        </h3>
      )}
      <div className="space-y-0.5">{children}</div>
    </section>
  );
}

export function PropertyRow({
  label,
  icon: Icon,
  children,
  onClick,
  className,
}: {
  label: ReactNode;
  icon?: ComponentType<{ className?: string }>;
  /** The value slot (right-aligned). */
  children: ReactNode;
  /** When set the whole row is a button (inline edit / popover trigger). */
  onClick?: () => void;
  className?: string;
}) {
  const inner = (
    <>
      <span className="flex min-w-0 shrink-0 items-center gap-1.5 text-xs text-muted-foreground">
        {Icon && <Icon className="h-3.5 w-3.5 shrink-0" />}
        {label}
      </span>
      <span className="ml-auto flex min-w-0 items-center gap-1.5 text-right text-sm text-foreground">
        {children}
      </span>
    </>
  );

  if (onClick) {
    return (
      <button
        type="button"
        onClick={onClick}
        className={cn(
          'flex w-full items-center gap-2 rounded-xl px-1.5 py-1.5 text-left outline-none hover:bg-muted focus-visible:ring-3 focus-visible:ring-ring/50',
          className,
        )}
      >
        {inner}
      </button>
    );
  }
  return <div className={cn('flex items-center gap-2 px-1.5 py-1.5', className)}>{inner}</div>;
}
