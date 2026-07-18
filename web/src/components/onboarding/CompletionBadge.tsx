import { cn } from '@/lib/utils';

/**
 * CompletionBadge — the onboarding "done" mark (spec §5.8). A brand-tinted disc
 * that springs in with a 500ms overshoot, then an SVG check draws itself via
 * stroke-dashoffset. Both animations are CSS classes gated behind
 * `prefers-reduced-motion` (see index.css §5.8 onboarding motion) — in
 * reduced-motion mode the disc is at rest and the check renders fully drawn.
 */
export function CompletionBadge({
  size = 72,
  className,
  label = 'Done',
}: {
  size?: number;
  className?: string;
  label?: string;
}) {
  return (
    <span
      role="img"
      aria-label={label}
      className={cn(
        'mds-badge-pop inline-grid place-items-center rounded-full bg-success/12 text-success',
        className
      )}
      style={{ width: size, height: size }}
    >
      <svg
        viewBox="0 0 24 24"
        width={size * 0.5}
        height={size * 0.5}
        fill="none"
        stroke="currentColor"
        strokeWidth={2.5}
        strokeLinecap="round"
        strokeLinejoin="round"
        aria-hidden="true"
      >
        {/* pathLength=1 normalises the stroke so dasharray/offset:1 = full length. */}
        <path className="mds-check-draw" d="M5 13l4 4L19 7" pathLength={1} />
      </svg>
    </span>
  );
}
