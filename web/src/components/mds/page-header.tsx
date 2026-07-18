import { forwardRef, type ComponentPropsWithoutRef } from 'react';
import { cn } from '@/lib/utils';
import { SidebarTrigger } from './sidebar';

/**
 * PageHeader — MDS per-page top bar (spec §5.2). Fixed `h-12`, bottom border.
 * On mobile it auto-inserts a `SidebarTrigger` (hidden ≥md) unless `hideTrigger`
 * is set — there is no global topbar, every page carries its own header.
 */
export const PageHeader = forwardRef<
  HTMLElement,
  ComponentPropsWithoutRef<'header'> & { hideTrigger?: boolean }
>(({ className, children, hideTrigger = false, ...props }, ref) => (
  <header
    ref={ref}
    data-slot="page-header"
    className={cn(
      'flex h-12 shrink-0 items-center gap-2 border-b border-surface-border px-4',
      className
    )}
    {...props}
  >
    {!hideTrigger && <SidebarTrigger className="md:hidden" />}
    {children}
  </header>
));
PageHeader.displayName = 'PageHeader';
