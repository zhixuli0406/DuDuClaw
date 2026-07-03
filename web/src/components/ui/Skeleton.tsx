import { cn } from '@/lib/utils';

/**
 * Calm Glass loading primitives. Skeletons preserve layout while data loads,
 * which reads far calmer than a bare centred spinner (DESIGN.md §7 — "every
 * async surface has loading + empty + error states"). Respects
 * `prefers-reduced-motion` via the shared `.animate-pulse` (Tailwind disables
 * it under the reduced-motion media query in index.css).
 */

/** A single shimmer block. Size via `className` (e.g. `h-4 w-32`). */
export function Skeleton({ className }: { className?: string }) {
  return (
    <div
      aria-hidden="true"
      className={cn('animate-pulse rounded-md bg-stone-500/10 dark:bg-white/10', className)}
    />
  );
}

/**
 * A vertical stack of skeleton "rows" for list/table placeholders. Announces a
 * polite busy status for assistive tech while the shimmer stays decorative.
 */
export function SkeletonList({
  rows = 4,
  className,
  rowClassName,
  label,
}: {
  rows?: number;
  className?: string;
  rowClassName?: string;
  /** Accessible loading label (already-localized string). */
  label?: string;
}) {
  return (
    <div
      className={cn('space-y-2.5', className)}
      role="status"
      aria-live="polite"
      aria-busy="true"
    >
      {label && <span className="sr-only">{label}</span>}
      {Array.from({ length: rows }, (_, i) => (
        <Skeleton key={i} className={cn('h-12 w-full', rowClassName)} />
      ))}
    </div>
  );
}
