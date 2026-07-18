import { type ComponentPropsWithoutRef } from 'react';
import { cn } from '@/lib/utils';

/**
 * NavProgress — MDS top-of-page indeterminate navigation bar (spec §3): a 2px
 * `bg-brand` sweep animated by the `nav-progress-sweep` keyframe (index.css,
 * gated behind `prefers-reduced-motion`). Renders nothing when `active` is false.
 */
export function NavProgress({
  active,
  className,
  ...props
}: ComponentPropsWithoutRef<'div'> & { active: boolean }) {
  if (!active) return null;
  return (
    <div
      data-slot="nav-progress"
      role="progressbar"
      aria-busy="true"
      aria-label="Loading"
      className={cn(
        'pointer-events-none absolute inset-x-0 top-0 z-50 h-0.5 overflow-hidden bg-transparent',
        className
      )}
      {...props}
    >
      <div className="nav-progress-sweep h-full w-full bg-brand" />
    </div>
  );
}
