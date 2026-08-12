import { useEffect, useMemo, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { cn } from '@/lib/utils';
import { api, type MemoryDecayOverview as Overview, type MemoryTrendPoint } from '@/lib/api';
import { useConnectionStore } from '@/stores/connection-store';
import { summarizeMemory } from '@/lib/memory-format';
import {
  FRESHNESS_IDS,
  FRESHNESS_STYLES,
  freshnessLabelId,
  type FreshnessId,
} from '@/lib/memory-freshness';
import {
  Card,
  CardHeader,
  CardTitle,
  CardAction,
  CardContent,
  Segmented,
  Skeleton,
  type SegmentedOption,
} from '@/components/mds';
import { TrendingUpIcon, GaugeIcon } from 'lucide-react';

/**
 * MemoryDecayOverview — the two "how is this employee's memory doing?" charts
 * that sit above the memory list.
 *
 * Both are hand-drawn SVG reading `--chart-*` / semantic tokens, so light and
 * dark are handled entirely by the token layer and nothing here branches on
 * theme. Neither chart animates, so there is nothing for the reduced-motion
 * breaker to switch off.
 *
 * Split of labour between them: the trend answers "is it still learning?" (one
 * series over time — one hue, no legend, the endpoint direct-labelled); the
 * distribution answers "will it remember what it learned?" (four ordered
 * states, one row each, every row carrying its own written label and count so
 * colour is never the only channel).
 */

/**
 * Fill in anything the gateway did not send. A dashboard that talks to an older
 * binary (or to one where the RPC is unknown) gets an empty-but-complete shape
 * and renders nothing, instead of throwing on a missing array.
 */
function normalize(raw: Partial<Overview> | null | undefined): Overview {
  return {
    total: raw?.total ?? 0,
    scanned: raw?.scanned ?? 0,
    truncated: raw?.truncated ?? false,
    buckets: raw?.buckets ?? [],
    fading_soon: raw?.fading_soon ?? [],
    most_recalled: raw?.most_recalled ?? [],
    trend: raw?.trend ?? [],
    window_days: raw?.window_days ?? 30,
    archive_threshold: raw?.archive_threshold ?? 0.05,
  };
}

export function MemoryDecayOverview({ agentId }: { agentId: string }) {
  const intl = useIntl();
  const connectionState = useConnectionStore((s) => s.state);
  const [days, setDays] = useState<'7' | '30'>('30');
  const [data, setData] = useState<Overview | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    if (!agentId) return;
    if (connectionState !== 'authenticated') return;
    let alive = true;
    setLoading(true);
    api.memory
      .decayOverview(agentId, Number(days))
      .then((res) => {
        if (alive) setData(normalize(res));
      })
      .catch((e) => {
        console.warn('[api]', e);
        if (alive) setData(null);
      })
      .finally(() => {
        if (alive) setLoading(false);
      });
    return () => {
      alive = false;
    };
  }, [agentId, days, connectionState]);

  const rangeOptions: SegmentedOption<'7' | '30'>[] = [
    { value: '7', label: intl.formatMessage({ id: 'memory.decay.range.7d' }) },
    { value: '30', label: intl.formatMessage({ id: 'memory.decay.range.30d' }) },
  ];

  // Nothing learned yet — the list below already says so; a pair of empty
  // charts would only add noise.
  if (!loading && (!data || data.total === 0)) return null;

  // Only the very first read gets a skeleton. Switching the period holds the
  // previous render at reduced opacity instead: a skeleton there would flash
  // and jump the layout on every toggle.
  const firstLoad = loading && !data;
  const refetching = loading && !!data;

  return (
    <div className={cn('grid gap-4 lg:grid-cols-2', refetching && 'opacity-60')}>
      <Card data-size="sm">
        <CardHeader>
          <CardTitle className="flex items-center gap-2 text-sm">
            <TrendingUpIcon className="size-4 text-brand" aria-hidden />
            {intl.formatMessage({ id: 'memory.decay.trend.title' })}
          </CardTitle>
          <CardAction>
            <Segmented
              value={days}
              onValueChange={setDays}
              options={rangeOptions}
              aria-label={intl.formatMessage({ id: 'memory.decay.range.label' })}
            />
          </CardAction>
        </CardHeader>
        <CardContent>
          {firstLoad || !data ? (
            <Skeleton className="h-32 w-full" />
          ) : (
            <TrendChart points={data.trend} />
          )}
        </CardContent>
      </Card>

      <Card data-size="sm">
        <CardHeader>
          <CardTitle className="flex items-center gap-2 text-sm">
            <GaugeIcon className="size-4 text-brand" aria-hidden />
            {intl.formatMessage({ id: 'memory.decay.distribution.title' })}
          </CardTitle>
        </CardHeader>
        <CardContent className="space-y-3">
          {firstLoad || !data ? (
            <Skeleton className="h-32 w-full" />
          ) : (
            <>
              <FreshnessDistribution data={data} />
              {data.fading_soon.length > 0 && (
                <TopList
                  title={intl.formatMessage({ id: 'memory.decay.fadingSoon' })}
                  items={data.fading_soon.map((e) => summarizeMemory(e.content))}
                />
              )}
              {data.most_recalled.length > 0 && (
                <TopList
                  title={intl.formatMessage({ id: 'memory.decay.mostRecalled' })}
                  items={data.most_recalled.map((e) => summarizeMemory(e.content))}
                />
              )}
              {data.truncated && (
                <p className="text-xs text-muted-foreground">
                  {intl.formatMessage(
                    { id: 'memory.decay.truncated' },
                    { count: data.scanned },
                  )}
                </p>
              )}
            </>
          )}
        </CardContent>
      </Card>
    </div>
  );
}

/* ────────────────────────────── trend ────────────────────────────── */

const T = { w: 520, h: 150, top: 10, right: 56, bottom: 22, left: 40 };
const T_PLOT_W = T.w - T.left - T.right;
const T_PLOT_H = T.h - T.top - T.bottom;

/** Round an axis maximum up to a readable number (1 / 2 / 5 × 10ⁿ). */
function niceMax(value: number): number {
  if (value <= 1) return 1;
  const magnitude = 10 ** Math.floor(Math.log10(value));
  const normalized = value / magnitude;
  const step = normalized <= 1 ? 1 : normalized <= 2 ? 2 : normalized <= 5 ? 5 : 10;
  return step * magnitude;
}

/** `YYYY-MM-DD` → a short, locale-formatted day label. */
function shortDate(iso: string, intl: ReturnType<typeof useIntl>): string {
  const parsed = Date.parse(`${iso}T00:00:00`);
  if (Number.isNaN(parsed)) return iso;
  return intl.formatDate(parsed, { month: 'numeric', day: 'numeric' });
}

/**
 * Cumulative memories over the window: one series, one hue, a 10% wash under a
 * 2px line. The only direct label is the endpoint — the number the reader came
 * for — with the axis carrying the rest.
 */
function TrendChart({ points }: { points: ReadonlyArray<MemoryTrendPoint> }) {
  const intl = useIntl();
  const svgRef = useRef<SVGSVGElement>(null);
  const [hoverIndex, setHoverIndex] = useState<number | null>(null);

  const model = useMemo(() => {
    const max = niceMax(Math.max(1, ...points.map((p) => p.total)));
    const lastIndex = Math.max(0, points.length - 1);
    const x = (i: number) => T.left + (lastIndex === 0 ? 0 : (i / lastIndex) * T_PLOT_W);
    const y = (v: number) => T.top + (1 - v / max) * T_PLOT_H;
    const line = points
      .map((p, i) => `${i === 0 ? 'M' : 'L'}${x(i).toFixed(2)},${y(p.total).toFixed(2)}`)
      .join(' ');
    const area = points.length
      ? `${line} L${x(lastIndex).toFixed(2)},${(T.top + T_PLOT_H).toFixed(2)} L${T.left},${(T.top + T_PLOT_H).toFixed(2)} Z`
      : '';
    return { max, lastIndex, x, y, line, area };
  }, [points]);

  if (points.length === 0) {
    return (
      <p className="py-6 text-center text-xs text-muted-foreground">
        {intl.formatMessage({ id: 'memory.decay.trend.empty' })}
      </p>
    );
  }

  const last = points[model.lastIndex];
  const first = points[0];
  const grew = last.total - first.total;
  const ariaLabel = intl.formatMessage(
    { id: 'memory.decay.trend.aria' },
    { total: last.total, added: grew, days: points.length },
  );

  const handleMove = (clientX: number) => {
    const svg = svgRef.current;
    if (!svg) return;
    const rect = svg.getBoundingClientRect();
    if (rect.width === 0) return;
    const vx = ((clientX - rect.left) / rect.width) * T.w;
    const ratio = (vx - T.left) / T_PLOT_W;
    if (ratio < -0.05 || ratio > 1.05) {
      setHoverIndex(null);
      return;
    }
    const index = Math.round(Math.min(1, Math.max(0, ratio)) * model.lastIndex);
    setHoverIndex(index);
  };

  const hovered = hoverIndex === null ? null : points[hoverIndex];

  return (
    <figure className="m-0 space-y-1.5">
      <div className="relative">
        <svg
          ref={svgRef}
          role="img"
          aria-label={ariaLabel}
          viewBox={`0 0 ${T.w} ${T.h}`}
          className="h-auto w-full touch-none"
          preserveAspectRatio="xMidYMid meet"
          onPointerMove={(e) => handleMove(e.clientX)}
          onPointerLeave={() => setHoverIndex(null)}
        >
          <g aria-hidden="true">
            {/* Two solid hairline gridlines — enough to read the scale. */}
            {[0, model.max].map((tick) => (
              <g key={tick}>
                <line
                  x1={T.left}
                  x2={T.left + T_PLOT_W}
                  y1={model.y(tick)}
                  y2={model.y(tick)}
                  className="stroke-border"
                  strokeWidth={1}
                />
                <text
                  x={T.left - 6}
                  y={model.y(tick)}
                  dy="0.32em"
                  textAnchor="end"
                  className="fill-muted-foreground font-mono text-[10px] tabular-nums"
                >
                  {tick}
                </text>
              </g>
            ))}

            <path d={model.area} className="fill-chart-1" fillOpacity={0.1} />
            <path
              d={model.line}
              className="stroke-chart-1"
              strokeWidth={2}
              fill="none"
              strokeLinecap="round"
              strokeLinejoin="round"
            />

            {hovered && hoverIndex !== null && (
              <>
                <line
                  x1={model.x(hoverIndex)}
                  x2={model.x(hoverIndex)}
                  y1={T.top}
                  y2={T.top + T_PLOT_H}
                  className="stroke-muted-foreground"
                  strokeOpacity={0.35}
                  strokeWidth={1}
                />
                <circle
                  cx={model.x(hoverIndex)}
                  cy={model.y(hovered.total)}
                  r={4}
                  className="fill-chart-1 stroke-card"
                  strokeWidth={2}
                />
              </>
            )}

            {/* Endpoint marker + the one direct label on the plot. */}
            <circle
              cx={model.x(model.lastIndex)}
              cy={model.y(last.total)}
              r={4}
              className="fill-chart-1 stroke-card"
              strokeWidth={2}
            />
            <text
              x={model.x(model.lastIndex) + 8}
              y={model.y(last.total)}
              dy="0.32em"
              className="fill-foreground font-mono text-[11px] tabular-nums"
            >
              {last.total}
            </text>

            <text
              x={T.left}
              y={T.top + T_PLOT_H + 14}
              className="fill-muted-foreground text-[10px]"
            >
              {shortDate(first.date, intl)}
            </text>
            <text
              x={T.left + T_PLOT_W}
              y={T.top + T_PLOT_H + 14}
              textAnchor="end"
              className="fill-muted-foreground text-[10px]"
            >
              {shortDate(last.date, intl)}
            </text>
          </g>
        </svg>

        {hovered && hoverIndex !== null && (
          <div
            role="status"
            className="pointer-events-none absolute top-0 z-10 rounded-lg border border-border bg-popover px-2 py-1 text-xs text-popover-foreground shadow-[var(--menu-shadow)]"
            style={{ left: `${Math.min(72, (model.x(hoverIndex) / T.w) * 100)}%` }}
          >
            {intl.formatMessage(
              { id: 'memory.decay.trend.hover' },
              { date: shortDate(hovered.date, intl), total: hovered.total, added: hovered.added },
            )}
          </div>
        )}
      </div>
      <figcaption className="text-xs text-muted-foreground">
        {intl.formatMessage({ id: 'memory.decay.trend.caption' }, { added: grew })}
      </figcaption>
    </figure>
  );
}

/* ─────────────────────────── distribution ─────────────────────────── */

const D_ROW = 22;
const D_GAP = 8;
const D_W = 520;
const D_LABEL_W = 108;
const D_VALUE_W = 44;
const D_BAR_W = D_W - D_LABEL_W - D_VALUE_W;

/**
 * How the agent's memories are spread across the four freshness states — one
 * row per state, always all four (a missing row would read as "that state
 * cannot happen"), each with its own written label and count.
 *
 * The rows never touch, and every one is labelled, so the colour is redundant
 * reinforcement of an ordered status scale rather than the identity channel.
 */
function FreshnessDistribution({ data }: { data: Overview }) {
  const intl = useIntl();
  const counts = useMemo(() => {
    const map = new Map<string, number>(data.buckets.map((b) => [b.key, b.count]));
    return FRESHNESS_IDS.map((id) => ({ id, count: map.get(id) ?? 0 }));
  }, [data.buckets]);

  const max = Math.max(1, ...counts.map((c) => c.count));
  const height = counts.length * D_ROW + (counts.length - 1) * D_GAP;
  const label = (id: FreshnessId) => intl.formatMessage({ id: freshnessLabelId(id) });
  const ariaLabel = `${intl.formatMessage({ id: 'memory.decay.distribution.title' })}: ${counts
    .map((c) => `${label(c.id)} ${c.count}`)
    .join(' · ')}`;

  return (
    <svg
      role="img"
      aria-label={ariaLabel}
      viewBox={`0 0 ${D_W} ${height}`}
      className="h-auto w-full"
      preserveAspectRatio="xMinYMid meet"
    >
      <g aria-hidden="true">
        {counts.map((row, i) => {
          const style = FRESHNESS_STYLES[row.id];
          const y = i * (D_ROW + D_GAP);
          const barW = row.count > 0 ? Math.max(3, (row.count / max) * D_BAR_W) : 0;
          return (
            <g key={row.id}>
              <text
                x={0}
                y={y + D_ROW / 2}
                dy="0.32em"
                className="fill-muted-foreground text-[11px]"
              >
                {label(row.id)}
              </text>
              <rect
                x={D_LABEL_W}
                y={y + 5}
                width={D_BAR_W}
                height={D_ROW - 10}
                rx={4}
                className="fill-muted"
              />
              {barW > 0 && (
                <rect
                  x={D_LABEL_W}
                  y={y + 5}
                  width={barW}
                  height={D_ROW - 10}
                  rx={4}
                  className={style.swatch}
                  fillOpacity={style.opacity}
                />
              )}
              <text
                x={D_W}
                y={y + D_ROW / 2}
                dy="0.32em"
                textAnchor="end"
                className="fill-foreground font-mono text-[11px] tabular-nums"
              >
                {row.count}
              </text>
            </g>
          );
        })}
      </g>
    </svg>
  );
}

/** A short, plain list — the actionable half of the distribution. */
function TopList({ title, items }: { title: string; items: ReadonlyArray<string> }) {
  return (
    <div className="border-t border-surface-border pt-2.5">
      <p className="mb-1 text-xs font-medium text-muted-foreground">{title}</p>
      <ul className="space-y-0.5">
        {items.map((text, i) => (
          <li key={`${i}-${text}`} className={cn('truncate text-xs text-foreground')} title={text}>
            {text}
          </li>
        ))}
      </ul>
    </div>
  );
}
