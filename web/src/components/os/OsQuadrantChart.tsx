import { cn } from '@/lib/utils';
import type { OsGateQuadrants } from '@/lib/api';

/**
 * OsQuadrantChart — pure-SVG horizontal bar chart of the proactivity gate's
 * four-quadrant (+ non-response / unknown) outcome tally (DESIGN.md §3.4:
 * "純 SVG 圖表一律讀 --chart-* token 上色，不引入圖表庫"). Bars are decorative
 * (`aria-hidden`); the container carries one summarizing `aria-label` per
 * DESIGN.md §5.3's SVG a11y convention.
 */

const QUADRANT_ORDER: ReadonlyArray<keyof OsGateQuadrants> = [
  'correct_detection',
  'false_alarm',
  'missed_need',
  'correct_silence',
  'non_response',
  'unknown',
];

/** chart-1..5 cycle through the six rows (fill-chart-5 repeats for the last,
 *  lowest-signal "unknown" row so it visually recedes). */
const QUADRANT_COLOR: Record<keyof OsGateQuadrants, string> = {
  correct_detection: 'fill-chart-1',
  false_alarm: 'fill-chart-2',
  missed_need: 'fill-chart-3',
  correct_silence: 'fill-chart-4',
  non_response: 'fill-chart-5',
  unknown: 'fill-chart-5',
};

export function OsQuadrantChart({
  quadrants,
  labels,
  titleForAria,
  className,
}: {
  quadrants: OsGateQuadrants;
  /** Localized label per quadrant key. */
  labels: Record<keyof OsGateQuadrants, string>;
  /** Prefix for the composed `aria-label` (e.g. the section title). */
  titleForAria: string;
  className?: string;
}) {
  const rowH = 26;
  const gap = 10;
  const width = 480;
  const labelW = 120;
  const valueW = 48;
  const barMaxW = width - labelW - valueW;
  const height = QUADRANT_ORDER.length * rowH + (QUADRANT_ORDER.length - 1) * gap;
  const max = Math.max(1, ...QUADRANT_ORDER.map((k) => quadrants[k] ?? 0));

  const ariaLabel = `${titleForAria}：${QUADRANT_ORDER.map(
    (k) => `${labels[k]} ${(quadrants[k] ?? 0).toLocaleString()}`,
  ).join('、')}`;

  return (
    <svg
      role="img"
      aria-label={ariaLabel}
      viewBox={`0 0 ${width} ${height}`}
      className={cn('h-auto w-full', className)}
      preserveAspectRatio="xMinYMid meet"
    >
      <g aria-hidden="true">
        {QUADRANT_ORDER.map((key, i) => {
          const value = quadrants[key] ?? 0;
          const barW = value > 0 ? Math.max(3, (value / max) * barMaxW) : 0;
          const y = i * (rowH + gap);
          return (
            <g key={key}>
              <text
                x={0}
                y={y + rowH / 2}
                dy="0.32em"
                className="fill-muted-foreground text-[11px]"
              >
                {labels[key]}
              </text>
              <rect
                x={labelW}
                y={y + 3}
                width={barMaxW}
                height={rowH - 6}
                rx={4}
                className="fill-muted"
              />
              {barW > 0 && (
                <rect
                  x={labelW}
                  y={y + 3}
                  width={barW}
                  height={rowH - 6}
                  rx={4}
                  className={QUADRANT_COLOR[key]}
                />
              )}
              <text
                x={labelW + barMaxW + valueW}
                y={y + rowH / 2}
                dy="0.32em"
                textAnchor="end"
                className="fill-foreground font-mono text-[11px] tabular-nums"
              >
                {value.toLocaleString()}
              </text>
            </g>
          );
        })}
      </g>
    </svg>
  );
}
