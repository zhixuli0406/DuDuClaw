import { Fragment, forwardRef, type ComponentPropsWithoutRef, type ReactNode } from 'react';
import { ChevronRightIcon } from 'lucide-react';
import { cn } from '@/lib/utils';
import { PageHeader } from './page-header';

/**
 * BreadcrumbHeader — MDS detail-page top bar (spec §5.3 式1). Each `segment`
 * renders as a clickable button (or plain text when it has no `onClick`/`href`);
 * segments are joined by a `ChevronRight` (h-3 w-3). The final (leaf) segment
 * truncates at `max-w-72`. An optional `actions` slot floats to the right.
 *
 * `hideTrigger` defaults to `true`: detail pages render inside the app shell,
 * whose global mobile bar already carries the sole `SidebarTrigger`, so a
 * header-level trigger would double up on mobile. Pass `hideTrigger={false}`
 * only for a header used outside the shell.
 */
export interface BreadcrumbSegment {
  label: ReactNode;
  href?: string;
  onClick?: () => void;
}

export const BreadcrumbHeader = forwardRef<
  HTMLElement,
  Omit<ComponentPropsWithoutRef<typeof PageHeader>, 'children'> & {
    segments: BreadcrumbSegment[];
    actions?: ReactNode;
  }
>(({ className, segments, actions, hideTrigger = true, ...props }, ref) => (
  <PageHeader
    ref={ref}
    hideTrigger={hideTrigger}
    className={cn('justify-between', className)}
    {...props}
  >
    <nav
      aria-label="Breadcrumb"
      className="flex min-w-0 items-center gap-1 text-sm"
    >
      {segments.map((seg, i) => {
        const isLeaf = i === segments.length - 1;
        const interactive = !isLeaf && (seg.href || seg.onClick);
        const content = (
          <span
            className={cn(
              'truncate',
              isLeaf
                ? 'max-w-72 font-medium text-foreground'
                : 'text-muted-foreground'
            )}
          >
            {seg.label}
          </span>
        );
        return (
          <Fragment key={i}>
            {i > 0 && (
              <ChevronRightIcon
                aria-hidden
                className="size-3 shrink-0 text-muted-foreground/60"
              />
            )}
            {interactive ? (
              <a
                href={seg.href}
                onClick={(e) => {
                  if (seg.onClick) {
                    e.preventDefault();
                    seg.onClick();
                  }
                }}
                className="min-w-0 rounded-sm outline-none hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring/50"
              >
                {content}
              </a>
            ) : (
              content
            )}
          </Fragment>
        );
      })}
    </nav>
    {actions && (
      <div className="flex shrink-0 items-center gap-1">{actions}</div>
    )}
  </PageHeader>
));
BreadcrumbHeader.displayName = 'BreadcrumbHeader';
