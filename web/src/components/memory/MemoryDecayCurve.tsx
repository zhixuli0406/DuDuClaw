import { useMemo, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { cn } from '@/lib/utils';
import {
  DEFAULT_ARCHIVE_THRESHOLD,
  FRESHNESS_BANDS,
  daysToReach,
  freshnessBand,
  freshnessLabelId,
  retrievabilityAt,
} from '@/lib/memory-freshness';

/**
 * MemoryDecayCurve — one memory's forgetting curve, drawn as plain SVG
 * (DESIGN.md §1.2: pure-SVG charts read `--chart-*` tokens, no chart library).
 *
 * The reading it supports: "where is this memory now, and when does it get
 * filed away if nobody uses it?" Two facts, both direct-labelled on the plot —
 * the marker for now, the crossing for the archive point — so nothing is
 * hover-gated. The y axis is *banded by the four freshness states* rather than
 * ticked with numbers: it ties the curve to the badge on the row and keeps the
 * underlying 0–1 figure off the surface entirely.
 *
 * The dashed line is the archive threshold. Dashing is reserved for exactly
 * this — a real threshold; grid and axis rules stay solid hairlines.
 */

/** Plot geometry in abstract viewBox units. */
const W = 520;
const H = 190;
const PAD = { top: 12, right: 76, bottom: 26, left: 68 };
const PLOT_W = W - PAD.left - PAD.right;
const PLOT_H = H - PAD.top - PAD.bottom;
/** Sample count along the curve — smooth at this size without a path essay. */
const SAMPLES = 72;

export function MemoryDecayCurve({
  stabilityDays,
  elapsedDays,
  archiveThreshold = DEFAULT_ARCHIVE_THRESHOLD,
  className,
}: {
  /** Days of silence it takes to fade to ~37%. */
  stabilityDays: number;
  /** Days since this memory was last recalled. */
  elapsedDays: number;
  /** Retrievability at which the platform files the memory away. */
  archiveThreshold?: number;
  className?: string;
}) {
  const intl = useIntl();
  const svgRef = useRef<SVGSVGElement>(null);
  const [hoverDay, setHoverDay] = useState<number | null>(null);

  const model = useMemo(() => {
    const archiveDay = daysToReach(archiveThreshold, stabilityDays);
    // Always show the whole story: last recall → a little past the archive
    // point, and never clip the current position off the right edge.
    const xMax = Math.max(archiveDay * 1.08, elapsedDays * 1.2, 1);
    const x = (day: number) => PAD.left + (Math.min(day, xMax) / xMax) * PLOT_W;
    const y = (r: number) => PAD.top + (1 - Math.min(1, Math.max(0, r))) * PLOT_H;

    const points = Array.from({ length: SAMPLES + 1 }, (_, i) => {
      const day = (i / SAMPLES) * xMax;
      return { day, r: retrievabilityAt(day, stabilityDays) };
    });
    const line = points.map((p, i) => `${i === 0 ? 'M' : 'L'}${x(p.day).toFixed(2)},${y(p.r).toFixed(2)}`).join(' ');
    const area = `${line} L${x(xMax).toFixed(2)},${(PAD.top + PLOT_H).toFixed(2)} L${PAD.left},${(PAD.top + PLOT_H).toFixed(2)} Z`;

    const current = retrievabilityAt(elapsedDays, stabilityDays);
    return {
      archiveDay,
      xMax,
      x,
      y,
      line,
      area,
      current,
      remainingDays: Math.max(0, archiveDay - elapsedDays),
    };
  }, [stabilityDays, elapsedDays, archiveThreshold]);

  const currentBand = freshnessBand(model.current);
  const currentLabel = currentBand ? intl.formatMessage({ id: freshnessLabelId(currentBand) }) : '';
  const elapsedRounded = Math.round(elapsedDays);
  const remainingRounded = Math.round(model.remainingDays);

  // The written twin of the plot. Every number the chart encodes is also here,
  // so nothing depends on reading a curve — or on hovering it.
  const summary =
    model.current <= archiveThreshold
      ? intl.formatMessage({ id: 'memory.decay.curve.summaryArchiving' }, { days: elapsedRounded })
      : intl.formatMessage(
          { id: 'memory.decay.curve.summary' },
          { state: currentLabel, days: elapsedRounded, remaining: remainingRounded },
        );

  const handleMove = (clientX: number) => {
    const svg = svgRef.current;
    if (!svg) return;
    const rect = svg.getBoundingClientRect();
    if (rect.width === 0) return;
    // Client px → viewBox units → day.
    const vx = ((clientX - rect.left) / rect.width) * W;
    const ratio = (vx - PAD.left) / PLOT_W;
    if (ratio < 0 || ratio > 1) {
      setHoverDay(null);
      return;
    }
    setHoverDay(ratio * model.xMax);
  };

  const hover =
    hoverDay === null
      ? null
      : (() => {
          const r = retrievabilityAt(hoverDay, stabilityDays);
          const band = freshnessBand(r);
          return {
            day: hoverDay,
            r,
            label: band ? intl.formatMessage({ id: freshnessLabelId(band) }) : '',
          };
        })();

  return (
    <figure className={cn('m-0 space-y-1.5', className)}>
      <div className="relative">
        <svg
          ref={svgRef}
          role="img"
          aria-label={summary}
          viewBox={`0 0 ${W} ${H}`}
          className="h-auto w-full touch-none"
          preserveAspectRatio="xMidYMid meet"
          onPointerMove={(e) => handleMove(e.clientX)}
          onPointerLeave={() => setHoverDay(null)}
        >
          <g aria-hidden="true">
            {/* Freshness bands: hairline separators + the state name each band
                stands for. This is the y axis — in words, not numbers. */}
            {FRESHNESS_BANDS.map((band, i) => {
              const upper = i === 0 ? 1 : FRESHNESS_BANDS[i - 1].min;
              const midY = model.y((upper + band.min) / 2);
              return (
                <g key={band.id}>
                  {band.min > 0 && (
                    <line
                      x1={PAD.left}
                      x2={PAD.left + PLOT_W}
                      y1={model.y(band.min)}
                      y2={model.y(band.min)}
                      className="stroke-border"
                      strokeWidth={1}
                    />
                  )}
                  <text
                    x={PAD.left - 8}
                    y={midY}
                    dy="0.32em"
                    textAnchor="end"
                    className="fill-muted-foreground text-[10px]"
                  >
                    {intl.formatMessage({ id: freshnessLabelId(band.id) })}
                  </text>
                </g>
              );
            })}

            {/* Baseline. */}
            <line
              x1={PAD.left}
              x2={PAD.left + PLOT_W}
              y1={PAD.top + PLOT_H}
              y2={PAD.top + PLOT_H}
              className="stroke-border"
              strokeWidth={1}
            />

            {/* The curve: 10% wash under a 2px stroke. */}
            <path d={model.area} className="fill-chart-1" fillOpacity={0.1} />
            <path
              d={model.line}
              className="stroke-chart-1"
              strokeWidth={2}
              fill="none"
              strokeLinecap="round"
              strokeLinejoin="round"
            />

            {/* The archive threshold — a genuine threshold, hence dashed, and
                labelled in place so it never needs a legend. */}
            <line
              x1={PAD.left}
              x2={PAD.left + PLOT_W}
              y1={model.y(archiveThreshold)}
              y2={model.y(archiveThreshold)}
              className="stroke-destructive"
              strokeOpacity={0.6}
              strokeWidth={1}
              strokeDasharray="4 3"
            />
            <text
              x={PAD.left + PLOT_W + 6}
              y={model.y(archiveThreshold)}
              dy="0.32em"
              className="fill-muted-foreground text-[10px]"
            >
              {intl.formatMessage({ id: 'memory.decay.curve.archiveLine' })}
            </text>

            {/* Hover crosshair — enhancement only; the summary below carries
                the same information without a pointer. */}
            {hover && (
              <line
                x1={model.x(hover.day)}
                x2={model.x(hover.day)}
                y1={PAD.top}
                y2={PAD.top + PLOT_H}
                className="stroke-muted-foreground"
                strokeOpacity={0.35}
                strokeWidth={1}
              />
            )}

            {/* Where this memory is right now: a marker with a 2px surface ring
                so it stays legible where it crosses the curve. */}
            <circle
              cx={model.x(elapsedDays)}
              cy={model.y(model.current)}
              r={4.5}
              className="fill-chart-1 stroke-card"
              strokeWidth={2}
            />
            <text
              x={model.x(elapsedDays)}
              y={model.y(model.current) - 10}
              textAnchor={model.x(elapsedDays) > PAD.left + PLOT_W * 0.7 ? 'end' : 'middle'}
              className="fill-foreground text-[10px]"
            >
              {intl.formatMessage({ id: 'memory.decay.curve.now' })}
            </text>

            {/* x anchors: where the clock started, and where it runs out. */}
            <text
              x={PAD.left}
              y={PAD.top + PLOT_H + 14}
              className="fill-muted-foreground text-[10px]"
            >
              {intl.formatMessage({ id: 'memory.decay.curve.lastRecall' })}
            </text>
            {model.archiveDay <= model.xMax && (
              <text
                x={Math.min(model.x(model.archiveDay), PAD.left + PLOT_W)}
                y={PAD.top + PLOT_H + 14}
                textAnchor="end"
                className="fill-muted-foreground text-[10px]"
              >
                {intl.formatMessage(
                  { id: 'memory.decay.curve.archiveIn' },
                  { days: Math.round(model.archiveDay) },
                )}
              </text>
            )}
          </g>
        </svg>

        {hover && (
          <div
            role="status"
            className="pointer-events-none absolute top-1 z-10 rounded-lg border border-border bg-popover px-2 py-1 text-xs text-popover-foreground shadow-[var(--menu-shadow)]"
            style={{ left: `${Math.min(84, (model.x(hover.day) / W) * 100)}%` }}
          >
            {intl.formatMessage(
              { id: 'memory.decay.curve.hover' },
              { days: Math.round(hover.day), state: hover.label },
            )}
          </div>
        )}
      </div>

      <figcaption className="text-xs text-muted-foreground">{summary}</figcaption>
    </figure>
  );
}
