import { type ComponentType, type ReactNode } from 'react';
import { cn } from '@/lib/utils';

/**
 * Empty — MDS collection empty / error state (spec §4 Empty / CollectionPageState).
 * `tone="destructive"` renders the error palette; `variant="dashed"` adds a
 * dashed bordered container for inline empty regions.
 */
export function Empty({
  icon: Icon,
  title,
  description,
  action,
  tone = 'default',
  variant = 'default',
  className,
}: {
  icon?: ComponentType<{ className?: string }>;
  title: ReactNode;
  description?: ReactNode;
  action?: ReactNode;
  tone?: 'default' | 'destructive';
  variant?: 'default' | 'dashed';
  className?: string;
}) {
  const destructive = tone === 'destructive';
  return (
    <div
      data-slot="empty"
      data-tone={tone}
      data-variant={variant}
      className={cn(
        'flex flex-col items-center justify-center text-center',
        variant === 'dashed'
          ? 'rounded-lg border border-dashed py-12'
          : 'py-16',
        className
      )}
    >
      {Icon && (
        <div
          className={cn(
            'mb-4 flex size-12 items-center justify-center rounded-full',
            destructive ? 'bg-destructive/10' : 'bg-muted'
          )}
        >
          <Icon
            className={cn(
              'size-6',
              destructive ? 'text-destructive' : 'text-muted-foreground/40'
            )}
          />
        </div>
      )}
      <p
        className={cn(
          'text-sm font-medium',
          destructive ? 'text-destructive' : 'text-foreground'
        )}
      >
        {title}
      </p>
      {description && (
        <p className="mt-1 max-w-md text-sm text-muted-foreground">
          {description}
        </p>
      )}
      {action && <div className="mt-4">{action}</div>}
    </div>
  );
}
