import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { ChartGantt } from 'lucide-react';
import {
  api,
  type TimelineKind,
  type TimelineListResult,
  type TimelineRow,
} from '@/lib/api';
import {
  buildLanes,
  timeTicks,
  xForMs,
  type PlacedRow,
} from '@/lib/timeline-layout';
import { useConnectionStore } from '@/stores/connection-store';
import { useAgentsStore } from '@/stores/agents-store';
import { useDataScope, useVisibleAgents } from '@/lib/data-scope';
import {
  PageHeader,
  Card,
  CardContent,
  Segmented,
  Empty,
  Skeleton,
  ActorAvatar,
  Select,
  SelectTrigger,
  SelectValue,
  SelectContent,
  SelectItem,
  type SegmentedOption,
} from '@/components/mds';

/**
 * TimelinePage (G11 Work Timeline) — the company-level Gantt: every AI staff
 * member gets a lane; ranged work renders as bars (running work extends to
 * "now"), point events render as dots. Pure SVG — no chart dependency. Recolored
 * onto MDS tokens (spec §5.2 / §4); the layout math is untouched. Data comes from
 * `timeline.list`, which only ever reports REAL timestamps.
 */

type RangeKey = '1h' | '6h' | '24h' | '7d';
const RANGE_HOURS: Record<RangeKey, number> = { '1h': 1, '6h': 6, '24h': 24, '7d': 168 };

const REFRESH_MS = 30_000;
const AXIS_H = 28;
const SUBROW_PITCH = 22;
const BAR_H = 12;
const LANE_VPAD = 8;
const DOT_R = 4.5;
const LABEL_COL_W = 176;

/** Kind → CSS status token. Tokens only (theme-aware, AA-checked); no hex. */
function fillFor(row: TimelineRow): string {
  if (row.kind === 'task') {
    // `failed` (durable-dispatch terminal state) has no dedicated CSS token;
    // it shares the blocked token (red) — semantically "went wrong".
    if (row.status === 'failed') return 'var(--status-task-blocked)';
    const known = new Set([
      'backlog',
      'todo',
      'in_progress',
      'in_review',
      'done',
      'blocked',
      'cancelled',
    ]);
    const s = known.has(row.status) ? row.status : 'todo';
    return `var(--status-task-${s})`;
  }
  switch (row.kind) {
    case 'delegation':
      return 'var(--status-agent-running)';
    case 'heartbeat':
      return 'var(--status-agent-idle)';
    case 'skill':
      return 'var(--status-task-in_review)';
    case 'autopilot':
      return 'var(--status-task-done)';
    case 'governance':
      return 'var(--status-task-blocked)';
    default:
      return 'var(--status-task-todo)';
  }
}

const LEGEND_KINDS: readonly TimelineKind[] = [
  'task',
  'delegation',
  'heartbeat',
  'skill',
  'autopilot',
  'governance',
  'activity',
];

/** Measure a container's content width without layout jumps on refresh. */
function useElementWidth<T extends HTMLElement>(): [React.RefObject<T | null>, number] {
  const ref = useRef<T | null>(null);
  const [width, setWidth] = useState(0);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const ro = new ResizeObserver((entries) => {
      const w = entries[0]?.contentRect.width ?? 0;
      setWidth((prev) => (Math.abs(prev - w) > 0.5 ? w : prev));
    });
    ro.observe(el);
    setWidth(el.clientWidth);
    return () => ro.disconnect();
  }, []);
  return [ref, width];
}

interface Hover {
  readonly placed: PlacedRow;
  readonly x: number;
  readonly y: number;
}

export function TimelinePage() {
  const intl = useIntl();
  const connectionState = useConnectionStore((s) => s.state);
  const agents = useAgentsStore((s) => s.agents);
  const fetchAgents = useAgentsStore((s) => s.fetchAgents);
  const scope = useDataScope();
  const visibleAgents = useVisibleAgents();

  const [range, setRange] = useState<RangeKey>('24h');
  const [agentFilter, setAgentFilter] = useState<string>('');
  const [result, setResult] = useState<TimelineListResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loaded, setLoaded] = useState(false);
  const [hover, setHover] = useState<Hover | null>(null);
  const [chartRef, chartWidth] = useElementWidth<HTMLDivElement>();

  // Non-admin scopes must query per agent (the gateway fails closed without
  // an agent_id) — default to the first AI staff member the viewer can see.
  const effectiveAgent =
    agentFilter || (scope !== 'all' ? (visibleAgents[0]?.name ?? '') : '');

  const fetchTimeline = useCallback(async () => {
    if (scope !== 'all' && !effectiveAgent) return; // nothing visible yet
    const toMs = Date.now();
    const fromMs = toMs - RANGE_HOURS[range] * 3_600_000;
    try {
      const res = await api.timeline.list({
        from: new Date(fromMs).toISOString(),
        to: new Date(toMs).toISOString(),
        ...(effectiveAgent ? { agent_id: effectiveAgent } : {}),
      });
      setResult(res);
      setError(null);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoaded(true);
    }
  }, [range, effectiveAgent, scope]);

  useEffect(() => {
    if (connectionState !== 'authenticated') return;
    if (agents.length === 0) void fetchAgents();
  }, [connectionState, agents.length, fetchAgents]);

  // Initial fetch + gentle auto-refresh. Refreshes replace data in place (no
  // skeleton after first load) so the layout never jumps under the cursor.
  useEffect(() => {
    if (connectionState !== 'authenticated') return;
    void fetchTimeline();
    const id = setInterval(() => void fetchTimeline(), REFRESH_MS);
    return () => clearInterval(id);
  }, [connectionState, fetchTimeline]);

  const windowMs = useMemo(() => {
    if (!result) return null;
    const fromMs = Date.parse(result.from);
    const toMs = Date.parse(result.to);
    if (!Number.isFinite(fromMs) || !Number.isFinite(toMs) || toMs <= fromMs) return null;
    return { fromMs, toMs };
  }, [result]);

  const lanes = useMemo(() => {
    if (!result || !windowMs) return [];
    return buildLanes(result.rows, {
      fromMs: windowMs.fromMs,
      toMs: windowMs.toMs,
      nowMs: windowMs.toMs,
      // Dots reserve ~0.8% of the window when packing so coincident instants
      // stack into sub-rows instead of drawing on top of each other.
      minPackMs: (windowMs.toMs - windowMs.fromMs) * 0.008,
    });
  }, [result, windowMs]);

  const ticks = useMemo(
    () => (windowMs ? timeTicks(windowMs.fromMs, windowMs.toMs, 8) : []),
    [windowMs],
  );

  const laneTops = useMemo(() => {
    let y = AXIS_H;
    return lanes.map((lane) => {
      const top = y;
      y += lane.subRowCount * SUBROW_PITCH + LANE_VPAD * 2;
      return top;
    });
  }, [lanes]);

  const laneHeights = useMemo(
    () => lanes.map((l) => l.subRowCount * SUBROW_PITCH + LANE_VPAD * 2),
    [lanes],
  );

  const totalH = AXIS_H + laneHeights.reduce((sum, h) => sum + h, 0);

  const agentName = useCallback(
    (id: string) => agents.find((a) => a.name === id)?.display_name || id,
    [agents],
  );

  const kindLabel = useCallback(
    (kind: string) =>
      intl.formatMessage({ id: `timeline.kind.${kind}`, defaultMessage: kind }),
    [intl],
  );

  const rangeOptions: SegmentedOption<RangeKey>[] = (['1h', '6h', '24h', '7d'] as const).map((r) => ({
    value: r,
    label: intl.formatMessage({ id: `timeline.range.${r}` }),
  }));

  const fmtTime = (ms: number) =>
    intl.formatTime(ms, { hour: '2-digit', minute: '2-digit' });
  const fmtTick = (ms: number, isDayStart: boolean) =>
    isDayStart || RANGE_HOURS[range] > 24
      ? intl.formatDate(ms, { month: 'numeric', day: 'numeric' })
      : fmtTime(ms);

  const tooltipText = (placed: PlacedRow): string => {
    const r = placed.row;
    const who = agentName(r.agent_id);
    const what = r.label || kindLabel(r.kind);
    const when = placed.instant
      ? fmtTime(placed.startMs)
      : `${fmtTime(placed.startMs)} – ${
          placed.running
            ? intl.formatMessage({ id: 'timeline.running' })
            : fmtTime(placed.endMs)
        }`;
    const status =
      r.kind === 'task'
        ? intl.formatMessage({ id: `taskStatus.${r.status}`, defaultMessage: r.status })
        : kindLabel(r.kind);
    return `${who} · ${what} · ${when} · ${status}`;
  };

  const hasRows = lanes.length > 0;
  const showAllOption = scope === 'all';
  const currentAgent = visibleAgents.find((a) => a.name === (agentFilter || effectiveAgent));

  return (
    <div className="-mx-4 -mt-4 flex flex-col md:-mx-6 md:-mt-6">
      <PageHeader hideTrigger className="justify-between">
        <div className="flex min-w-0 items-center gap-2">
          <ChartGantt className="size-4 shrink-0 text-muted-foreground" />
          <h1 className="truncate text-sm font-medium">{intl.formatMessage({ id: 'nav.timeline' })}</h1>
        </div>
        <Segmented
          value={range}
          onValueChange={setRange}
          options={rangeOptions}
          aria-label={intl.formatMessage({ id: 'nav.timeline' })}
        />
      </PageHeader>

      <div className="flex flex-1 flex-col p-4 md:p-6">
        <Card>
          <CardContent className="space-y-4">
            <div className="flex flex-wrap items-center gap-2">
              <Select
                value={agentFilter || effectiveAgent}
                onValueChange={(v) => setAgentFilter(String(v))}
              >
                <SelectTrigger className="w-56">
                  <SelectValue placeholder={intl.formatMessage({ id: 'timeline.filter.agent' })}>
                    {currentAgent
                      ? currentAgent.display_name || currentAgent.name
                      : intl.formatMessage({ id: 'timeline.allAgents' })}
                  </SelectValue>
                </SelectTrigger>
                <SelectContent>
                  {showAllOption && (
                    <SelectItem value="">{intl.formatMessage({ id: 'timeline.allAgents' })}</SelectItem>
                  )}
                  {visibleAgents.map((a) => (
                    <SelectItem key={a.name} value={a.name}>
                      {a.display_name || a.name}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>

              {/* Legend */}
              <div className="ml-auto flex flex-wrap items-center gap-3">
                {LEGEND_KINDS.map((k) => (
                  <span key={k} className="flex items-center gap-1.5 text-xs text-muted-foreground">
                    <span
                      aria-hidden="true"
                      className="size-2.5 rounded-full"
                      style={{
                        backgroundColor: fillFor({ kind: k, status: 'in_progress' } as TimelineRow),
                      }}
                    />
                    {kindLabel(k)}
                  </span>
                ))}
              </div>
            </div>

            {result?.truncated && (
              <p className="text-xs text-warning">
                {intl.formatMessage({ id: 'timeline.truncated' }, { cap: result.cap })}
              </p>
            )}

            <div>
              {!loaded ? (
                <div className="space-y-3">
                  <Skeleton className="h-8 w-full" />
                  <Skeleton className="h-8 w-full" />
                  <Skeleton className="h-8 w-2/3" />
                </div>
              ) : error ? (
                <Empty
                  tone="destructive"
                  icon={ChartGantt}
                  title={intl.formatMessage({ id: 'timeline.error' })}
                  description={error}
                />
              ) : !hasRows ? (
                <Empty
                  icon={ChartGantt}
                  title={intl.formatMessage({ id: 'timeline.empty' })}
                  description={intl.formatMessage({ id: 'timeline.empty.hint' })}
                />
              ) : (
                windowMs && (
                  <div className="flex overflow-x-auto">
                    {/* Lane labels */}
                    <div className="shrink-0" style={{ width: LABEL_COL_W }}>
                      <div style={{ height: AXIS_H }} aria-hidden="true" />
                      {lanes.map((lane) => (
                        <div
                          key={lane.agentId}
                          className="flex items-center gap-2 border-t border-surface-border pr-3"
                          style={{ height: lane.subRowCount * SUBROW_PITCH + LANE_VPAD * 2 }}
                        >
                          <ActorAvatar actorType="agent" size="sm" name={agentName(lane.agentId)} />
                          <span
                            className="truncate text-xs text-muted-foreground"
                            title={agentName(lane.agentId)}
                          >
                            {agentName(lane.agentId)}
                          </span>
                        </div>
                      ))}
                    </div>

                    {/* Chart */}
                    <div ref={chartRef} className="relative min-w-[320px] flex-1">
                      {chartWidth > 0 && (
                        <svg
                          width={chartWidth}
                          height={totalH}
                          role="img"
                          aria-label={intl.formatMessage({ id: 'timeline.chart.aria' })}
                          className="block"
                        >
                          {/* Lane bands (spec §5.2: bg-muted/40) */}
                          {lanes.map((lane, i) => (
                            <rect
                              key={`band-${lane.agentId}`}
                              x={0}
                              y={laneTops[i]}
                              width={chartWidth}
                              height={laneHeights[i]}
                              className="fill-muted"
                              opacity={0.4}
                            />
                          ))}

                          {/* Grid + axis labels */}
                          {ticks.map((t) => {
                            const x = xForMs(t.ms, windowMs.fromMs, windowMs.toMs, chartWidth);
                            return (
                              <g key={t.ms}>
                                <line
                                  x1={x}
                                  x2={x}
                                  y1={AXIS_H}
                                  y2={totalH}
                                  className="stroke-border"
                                  strokeWidth={1}
                                />
                                <text
                                  x={x}
                                  y={AXIS_H - 10}
                                  textAnchor="middle"
                                  className="fill-muted-foreground text-[10px] tabular-nums"
                                >
                                  {fmtTick(t.ms, t.isDayStart)}
                                </text>
                              </g>
                            );
                          })}

                          {/* Lane separators */}
                          {lanes.map((lane, i) => (
                            <line
                              key={lane.agentId}
                              x1={0}
                              x2={chartWidth}
                              y1={laneTops[i]}
                              y2={laneTops[i]}
                              className="stroke-border"
                              strokeWidth={1}
                            />
                          ))}

                          {/* Bars + dots */}
                          {lanes.map((lane, i) =>
                            lane.rows.map((placed) => {
                              const cy =
                                laneTops[i] +
                                LANE_VPAD +
                                placed.subRow * SUBROW_PITCH +
                                SUBROW_PITCH / 2;
                              const fill = fillFor(placed.row);
                              const label = tooltipText(placed);
                              const common = {
                                tabIndex: 0,
                                role: 'img' as const,
                                'aria-label': label,
                                className:
                                  'cursor-default focus-visible:outline-none focus-visible:stroke-brand',
                                onMouseEnter: (e: React.MouseEvent) => {
                                  const box = chartRef.current?.getBoundingClientRect();
                                  setHover({ placed, x: e.clientX - (box?.left ?? 0), y: cy });
                                },
                                onMouseLeave: () => setHover(null),
                                onFocus: () =>
                                  setHover({
                                    placed,
                                    x: xForMs(placed.startMs, windowMs.fromMs, windowMs.toMs, chartWidth),
                                    y: cy,
                                  }),
                                onBlur: () => setHover(null),
                              };
                              if (placed.instant) {
                                return (
                                  <circle
                                    key={placed.row.ref_id}
                                    cx={xForMs(placed.startMs, windowMs.fromMs, windowMs.toMs, chartWidth)}
                                    cy={cy}
                                    r={DOT_R}
                                    fill={fill}
                                    {...common}
                                  />
                                );
                              }
                              const x1 = xForMs(placed.startMs, windowMs.fromMs, windowMs.toMs, chartWidth);
                              const x2 = xForMs(placed.endMs, windowMs.fromMs, windowMs.toMs, chartWidth);
                              return (
                                <rect
                                  key={placed.row.ref_id}
                                  x={x1}
                                  y={cy - BAR_H / 2}
                                  width={Math.max(x2 - x1, 3)}
                                  height={BAR_H}
                                  rx={BAR_H / 2}
                                  fill={fill}
                                  opacity={placed.running ? undefined : 0.9}
                                  {...common}
                                  // Gentle presence pulse for live work — CSS-driven
                                  // so the reduced-motion breaker freezes it.
                                  className={`${common.className}${
                                    placed.running ? ' motion-safe:animate-pulse' : ''
                                  }`}
                                />
                              );
                            }),
                          )}

                          {/* "now" line (window end = fetch time) */}
                          <line
                            x1={chartWidth - 1}
                            x2={chartWidth - 1}
                            y1={AXIS_H - 4}
                            y2={totalH}
                            className="stroke-brand"
                            strokeWidth={1.5}
                            strokeDasharray="2 3"
                          />
                          <text
                            x={chartWidth - 4}
                            y={AXIS_H - 10}
                            textAnchor="end"
                            className="fill-brand text-[10px]"
                          >
                            {intl.formatMessage({ id: 'timeline.now' })}
                          </text>
                        </svg>
                      )}

                      {/* Hover tooltip */}
                      {hover && (
                        <div
                          role="status"
                          className="pointer-events-none absolute z-10 max-w-72 rounded-lg border border-border bg-popover px-2.5 py-1 text-xs text-popover-foreground shadow-[var(--menu-shadow)]"
                          style={{
                            left: Math.min(Math.max(hover.x, 0), Math.max(chartWidth - 160, 0)),
                            top: Math.max(hover.y - 44, 0),
                          }}
                        >
                          {tooltipText(hover.placed)}
                        </div>
                      )}
                    </div>
                  </div>
                )
              )}
            </div>
          </CardContent>
        </Card>
      </div>
    </div>
  );
}
