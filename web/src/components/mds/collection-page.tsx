import {
  forwardRef,
  isValidElement,
  cloneElement,
  type ComponentPropsWithoutRef,
  type ComponentType,
  type ReactElement,
  type ReactNode,
} from 'react';
import { cn } from '@/lib/utils';
import { PageHeader } from './page-header';
import { Empty } from './empty';

/**
 * CollectionPageHeader — MDS list/collection header (spec §5.2). Left cluster:
 * entity icon + title + count (mono) + optional description (≥md). Right slot:
 * the primary action, which collapses to an icon-only button below `md` when
 * `actionCompact` is supplied.
 *
 * `hideTrigger` defaults to `true`: every collection page renders inside the app
 * shell, whose global mobile bar already carries the sole `SidebarTrigger`, so a
 * header-level trigger would double up on mobile. Matches the convention used by
 * every plain `<PageHeader hideTrigger>` caller. Pass `hideTrigger={false}` only
 * for a header used outside the shell.
 */
export const CollectionPageHeader = forwardRef<
  HTMLElement,
  Omit<ComponentPropsWithoutRef<typeof PageHeader>, 'title'> & {
    icon?: ComponentType<{ className?: string }>;
    title: ReactNode;
    count?: number;
    description?: ReactNode;
    action?: ReactNode;
    /** Icon-only action shown below md; when omitted `action` renders at all sizes. */
    actionCompact?: ReactNode;
  }
>(
  (
    {
      className,
      icon: Icon,
      title,
      count,
      description,
      action,
      actionCompact,
      hideTrigger = true,
      ...props
    },
    ref
  ) => (
    <PageHeader
      ref={ref}
      hideTrigger={hideTrigger}
      className={cn('justify-between px-5', className)}
      {...props}
    >
      <div className="flex min-w-0 items-center gap-2">
        {Icon && <Icon className="size-4 shrink-0 text-muted-foreground" />}
        <h1 className="truncate text-sm font-medium">{title}</h1>
        {count !== undefined && (
          <span className="font-mono text-xs tabular-nums text-muted-foreground">
            {count}
          </span>
        )}
        {description && (
          <span className="hidden truncate text-sm text-muted-foreground md:block">
            {description}
          </span>
        )}
      </div>
      {(action || actionCompact) && (
        <div className="flex shrink-0 items-center gap-2">
          {actionCompact ? (
            <>
              <span className="md:hidden">{actionCompact}</span>
              <span className="hidden md:inline-flex">{action}</span>
            </>
          ) : (
            action
          )}
        </div>
      )}
    </PageHeader>
  )
);
CollectionPageHeader.displayName = 'CollectionPageHeader';

/**
 * CollectionPageState — loading / empty / error convenience wrapper over `Empty`
 * (spec §5.2). `state="loading"` renders a lightweight skeleton stack; `empty`
 * and `error` defer to `Empty` (error uses the destructive tone).
 */
export function CollectionPageState({
  state,
  icon,
  title,
  description,
  action,
  className,
}: {
  state: 'loading' | 'empty' | 'error';
  icon?: ComponentType<{ className?: string }>;
  title?: ReactNode;
  description?: ReactNode;
  action?: ReactNode;
  className?: string;
}) {
  if (state === 'loading') {
    return (
      <div
        data-slot="collection-page-state"
        data-state="loading"
        role="status"
        aria-label={typeof title === 'string' ? title : 'Loading'}
        className={cn('flex flex-col gap-3 p-5', className)}
      >
        {Array.from({ length: 5 }).map((_, i) => (
          <div
            key={i}
            className="h-12 animate-pulse rounded-md bg-muted"
            data-slot="collection-page-skeleton"
          />
        ))}
      </div>
    );
  }
  return (
    <div
      data-slot="collection-page-state"
      data-state={state}
      className={cn('flex flex-1 items-center justify-center', className)}
    >
      <Empty
        icon={icon}
        title={title ?? (state === 'error' ? 'Something went wrong' : 'Nothing here yet')}
        description={description}
        action={action}
        tone={state === 'error' ? 'destructive' : 'default'}
      />
    </div>
  );
}

// Re-export a helper so callers can adapt an existing action element to compact
// form without hand-rolling the icon swap. Kept internal-friendly and pure.
export function toCompactAction(
  action: ReactNode,
  extraClassName = 'h-8 w-8'
): ReactNode {
  if (isValidElement(action)) {
    const el = action as ReactElement<{ className?: string }>;
    return cloneElement(el, {
      className: cn(el.props.className, extraClassName),
    });
  }
  return action;
}
