import { cn } from '@/lib/utils';
import {
  MARK_ACCENT_PATH,
  MARK_INK_BODY_PATH,
  MARK_INK_HEAD_PATH,
  MARK_POTRACE_TRANSFORM,
  MARK_VIEWBOX,
} from './mark-paths';

export interface BrandMarkProps {
  /** Rendered width and height in px. Brand rule: 32 px minimum on screen
   *  (28 px is allowed in navigation, per the C1 web spec). */
  size?: number;
  /** Accessible name. Omit when a visible product name sits next to the mark;
   *  the mark is then hidden from assistive tech. */
  title?: string;
  /** One-colour version: both layers take `currentColor`. */
  mono?: boolean;
  className?: string;
}

/**
 * The DuDuClaw C1 mark as an inline SVG. The ink layer follows the theme via
 * `--logo-ink` (Navy on light, Cream on dark); the tail keeps the fixed Amber
 * accent (`--logo-accent`). Never placed inside a coloured chip or circle.
 */
export function BrandMark({ size = 32, title, mono = false, className }: BrandMarkProps) {
  const ink = mono ? 'currentColor' : 'var(--logo-ink)';
  const accent = mono ? 'currentColor' : 'var(--logo-accent)';
  const a11y = title
    ? ({ role: 'img', 'aria-label': title } as const)
    : ({ 'aria-hidden': true } as const);
  return (
    <svg
      viewBox={MARK_VIEWBOX}
      width={size}
      height={size}
      className={cn('shrink-0', className)}
      data-brand-mark=""
      {...a11y}
    >
      <g transform={MARK_POTRACE_TRANSFORM} style={{ fill: ink }}>
        <path d={MARK_INK_HEAD_PATH} />
        <path d={MARK_INK_BODY_PATH} />
      </g>
      <g transform={MARK_POTRACE_TRANSFORM} style={{ fill: accent }}>
        <path d={MARK_ACCENT_PATH} />
      </g>
    </svg>
  );
}
