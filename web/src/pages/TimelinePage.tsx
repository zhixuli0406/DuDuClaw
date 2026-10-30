import { useCallback, useEffect, useMemo, useState } from 'react';
import { Link } from 'react-router';
import { useIntl } from 'react-intl';
import { ChartGantt } from 'lucide-react';
import { useUrlState } from '@/lib/use-url-state';
import { api, type TimelineListResult, type TimelineRow } from '@/lib/api';
import { buildLanes, timeTicks, xForMs } from '@/lib/timeline-layout';
import { useConnectionStore } from '@/stores/connection-store';
import { useAgentsStore } from '@/stores/agents-store';
import { useDataScope, useVisibleAgents } from '@/lib/data-scope';
import {
  PageHeader, Card, CardContent, CardHeader, CardTitle, CardDescription,
  Segmented, Empty, ErrorState, Skeleton, ActorAvatar, Badge, Button,
  Select, SelectTrigger, SelectValue, SelectContent, SelectItem,
  type SegmentedOption,
} from '@/components/mds';

const RANGE_KEYS = ['1h', '6h', '24h', '7d'] as const;
type RangeKey = (typeof RANGE_KEYS)[number];
const RANGE_HOURS: Record<RangeKey, number> = { '1h': 1, '6h': 6, '24h': 24, '7d': 168 };
const BUCKETS = ['done', 'active', 'waiting', 'unfinished'] as const;
type Bucket = (typeof BUCKETS)[number];
const KNOWN_STATUSES = new Set([
  'backlog', 'todo', 'pending', 'pending_approval', 'in_progress', 'review',
  'in_review', 'revising', 'needs_human', 'done', 'blocked', 'failed', 'cancelled',
]);

function bucketFor(row: TimelineRow): Bucket | null {
  if (row.kind === 'problem') return 'unfinished';
  if (row.kind !== 'task') return null;
  if (row.status === 'done') return 'done';
  if (['needs_human', 'pending_approval'].includes(row.status)) return 'waiting';
  if (['blocked', 'failed', 'cancelled'].includes(row.status)) return 'unfinished';
  return 'active';
}

function lastUpdateMs(row: TimelineRow): number {
  const parsed = Date.parse(row.updated_at || row.started_at);
  return Number.isFinite(parsed) ? parsed : Date.parse(row.started_at) || 0;
}

function taskId(row: TimelineRow): string | null {
  return row.task_id || (row.kind === 'task' ? row.ref_id : null);
}

function chartColor(row: TimelineRow): string {
  const bucket = bucketFor(row);
  return `var(--chart-${bucket === 'waiting' ? 1 : bucket === 'unfinished' ? 2 : bucket === 'active' ? 3 : bucket === 'done' ? 4 : 5})`;
}

/** Shapes repeat in the legend and bars so status does not rely on colour. */
function BucketMarker({ bucket, x = 0, y = 0 }: { bucket: Bucket | 'other'; x?: number; y?: number }) {
  return <g transform={`translate(${x} ${y})`} fill={`var(--chart-${bucket === 'waiting' ? 1 : bucket === 'unfinished' ? 2 : bucket === 'active' ? 3 : bucket === 'done' ? 4 : 5})`} className="stroke-foreground" strokeWidth={0.7} aria-hidden="true" data-status-marker={bucket}>
    {bucket === 'done' ? <circle cx={5} cy={5} r={4} />
      : bucket === 'active' ? <rect x={1} y={1} width={8} height={8} />
        : bucket === 'waiting' ? <path d="M5 0 10 5 5 10 0 5Z" />
          : bucket === 'unfinished' ? <path d="M5 0 10 10 0 10Z" />
            : <circle cx={5} cy={5} r={4} fill="none" stroke="currentColor" strokeWidth={2} />}
  </g>;
}

/** Count a task once; a linked failure is already represented by its task. */
function workItems(rows: readonly TimelineRow[]): TimelineRow[] {
  const tasks = new Map<string, TimelineRow>();
  for (const row of rows) {
    if (row.kind === 'task') tasks.set(row.ref_id, row);
  }
  return [
    ...tasks.values(),
    ...rows.filter((row) => row.kind === 'problem' && (!row.task_id || !tasks.has(row.task_id))),
  ];
}

export function TimelinePage() {
  const intl = useIntl();
  const connectionState = useConnectionStore((s) => s.state);
  const agents = useAgentsStore((s) => s.agents);
  const fetchAgents = useAgentsStore((s) => s.fetchAgents);
  const scope = useDataScope();
  const visibleAgents = useVisibleAgents();
  const [range, setRange] = useUrlState('range', '24h', { allowed: RANGE_KEYS });
  const [agentFilter, setAgentFilter] = useUrlState('agent', '');
  const [filter, setFilter] = useState<Bucket | null>(null);
  const [chartOpen, setChartOpen] = useState(false);
  const [result, setResult] = useState<TimelineListResult | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [loaded, setLoaded] = useState(false);
  const effectiveAgent = agentFilter || (scope !== 'all' ? (visibleAgents[0]?.name ?? '') : '');

  const fetchTimeline = useCallback(async () => {
    if (scope !== 'all' && !effectiveAgent) {
      setLoaded(true);
      return;
    }
    const toMs = Date.now();
    try {
      const res = await api.timeline.list({
        from: new Date(toMs - RANGE_HOURS[range] * 3_600_000).toISOString(),
        to: new Date(toMs).toISOString(),
        ...(effectiveAgent ? { agent_id: effectiveAgent } : {}),
      });
      setResult(res);
      setError(null);
    } catch (e) {
      setError(e);
    } finally {
      setLoaded(true);
    }
  }, [range, effectiveAgent, scope]);

  useEffect(() => {
    if (connectionState === 'authenticated' && agents.length === 0) void fetchAgents();
  }, [connectionState, agents.length, fetchAgents]);
  useEffect(() => {
    if (connectionState !== 'authenticated') return;
    void fetchTimeline();
    const id = setInterval(() => void fetchTimeline(), 30_000);
    return () => clearInterval(id);
  }, [connectionState, fetchTimeline]);

  const rows = useMemo(() => (result?.rows ?? []).filter((row) => row.agent_id !== 'channel_alert'), [result]);
  const items = useMemo(() => workItems(rows), [rows]);
  const counts = Object.fromEntries(BUCKETS.map((bucket) => [bucket, items.filter((row) => bucketFor(row) === bucket).length])) as Record<Bucket, number>;
  const nameFor = (id: string) => id && id !== 'unassigned'
    ? agents.find((agent) => agent.name === id)?.display_name || id
    : intl.formatMessage({ id: 'timeline.unassigned', defaultMessage: 'Unassigned' });
  const groups = useMemo(() => {
    const ids = new Set(rows.map((row) => row.agent_id));
    for (const agent of visibleAgents) {
      if (agent.name === 'channel_alert') continue;
      if (!effectiveAgent || effectiveAgent === agent.name) ids.add(agent.name);
    }
    const groups = [...ids].map((id) => {
      const all = items.filter((row) => row.agent_id === id);
      const shown = filter ? all.filter((row) => bucketFor(row) === filter) : all;
      const priority = all.some((row) => bucketFor(row) === 'waiting') ? 0
        : all.some((row) => bucketFor(row) === 'unfinished') ? 1
          : all.some((row) => bucketFor(row) === 'active') ? 2 : 3;
      const oldestWait = Math.min(...all.filter((row) => bucketFor(row) === 'waiting').map((row) => lastUpdateMs(row)));
      return { id, all, shown, priority, oldestWait };
    });
    return groups.filter((group) => !filter || group.shown.length > 0)
      .sort((a, b) => a.priority - b.priority || (a.priority === 0 ? a.oldestWait - b.oldestWait : 0) || a.id.localeCompare(b.id));
  }, [rows, items, visibleAgents, effectiveAgent, filter]);

  const statusLabel = (row: TimelineRow) => intl.formatMessage({
    id: row.kind === 'problem' ? 'timeline.status.problem'
      : KNOWN_STATUSES.has(row.status) ? `timeline.status.${row.status}` : 'timeline.status.other',
    defaultMessage: row.kind === 'problem' ? 'Needs attention' : 'Update recorded',
  });
  const bucketLabel = (bucket: Bucket) => intl.formatMessage({ id: `timeline.summary.${bucket}`, defaultMessage: { done: 'Completed', active: 'In progress', waiting: 'Needs your decision', unfinished: 'Unfinished' }[bucket] });
  const waitLabel = (row: TimelineRow) => {
    const sinceUpdate = Boolean(row.updated_at);
    const elapsed = Math.max(0, (Date.parse(result?.to ?? '') || Date.now()) - lastUpdateMs(row));
    const minutes = Math.max(1, Math.floor(elapsed / 60_000));
    const unit = minutes >= 1440 ? 'days' : minutes >= 60 ? 'hours' : 'minutes';
    const count = unit === 'days' ? Math.floor(minutes / 1440) : unit === 'hours' ? Math.floor(minutes / 60) : minutes;
    return intl.formatMessage({ id: `timeline.${sinceUpdate ? bucketFor(row) === 'waiting' ? 'waitingSinceUpdate' : 'sinceUpdate' : 'elapsed'}.${unit}`, defaultMessage: `${sinceUpdate ? 'Last updated' : 'Open for'} {count} ${unit}${sinceUpdate ? ' ago' : ''}` }, { count });
  };
  const windowMs = result ? { fromMs: Date.parse(result.from), toMs: Date.parse(result.to) } : null;
  const validWindow = windowMs && Number.isFinite(windowMs.fromMs) && windowMs.toMs > windowMs.fromMs;
  const lanes = validWindow ? buildLanes(rows, { ...windowMs, nowMs: windowMs.toMs, minPackMs: (windowMs.toMs - windowMs.fromMs) * 0.008 }) : [];
  const rangeOptions: SegmentedOption<RangeKey>[] = RANGE_KEYS.map((value) => ({ value, label: intl.formatMessage({ id: `timeline.range.${value}` }) }));
  const currentAgent = visibleAgents.find((agent) => agent.name === (agentFilter || effectiveAgent));

  return (
    <div className="-mx-4 -mt-4 flex flex-col md:-mx-6 md:-mt-6">
      <PageHeader hideTrigger className="h-auto min-h-12 flex-wrap justify-between gap-2">
        <div className="flex min-w-0 items-center gap-2">
          <ChartGantt className="size-4 shrink-0 text-muted-foreground" aria-hidden="true" />
          <h1 className="truncate text-sm font-medium">{intl.formatMessage({ id: 'nav.timeline' })}</h1>
        </div>
        <Segmented value={range} onValueChange={setRange} options={rangeOptions} aria-label={intl.formatMessage({ id: 'nav.timeline' })} />
      </PageHeader>
      <div className="mx-auto w-full max-w-6xl space-y-5 p-4 md:p-6">
        <p className="text-sm text-muted-foreground">{intl.formatMessage({ id: 'timeline.subtitle', defaultMessage: 'See who completed what, who is stuck, and what needs your decision in this time range.' })}</p>
        <Select value={agentFilter || effectiveAgent} onValueChange={(value) => setAgentFilter(String(value))}>
          <SelectTrigger className="w-56" aria-label={intl.formatMessage({ id: 'timeline.filter.agent' })}>
            <SelectValue placeholder={intl.formatMessage({ id: 'timeline.filter.agent' })}>{currentAgent ? currentAgent.display_name || currentAgent.name : intl.formatMessage({ id: 'timeline.allAgents' })}</SelectValue>
          </SelectTrigger>
          <SelectContent>
            {scope === 'all' && <SelectItem value="">{intl.formatMessage({ id: 'timeline.allAgents' })}</SelectItem>}
            {visibleAgents.filter((agent) => agent.name !== 'channel_alert').map((agent) => <SelectItem key={agent.name} value={agent.name}>{agent.display_name || agent.name}</SelectItem>)}
          </SelectContent>
        </Select>
        {!loaded ? <div className="space-y-3" aria-busy="true"><Skeleton className="h-16 w-full" /><Skeleton className="h-32 w-full" /></div>
          : error ? <ErrorState icon={ChartGantt} title={intl.formatMessage({ id: 'timeline.error' })} error={error} onRetry={() => void fetchTimeline()} />
            : <>
              <div className="grid grid-cols-2 gap-3 lg:grid-cols-4" aria-label={intl.formatMessage({ id: 'timeline.summary.aria', defaultMessage: 'Work in this time range' })}>
                {BUCKETS.map((bucket) => <Card key={bucket} data-size="sm"><CardContent><Button variant={filter === bucket ? 'secondary' : 'ghost'} aria-pressed={filter === bucket} onClick={() => setFilter(filter === bucket ? null : bucket)}>
                  {bucketLabel(bucket)} <span className="ml-2 font-mono tabular-nums">{counts[bucket]}</span>
                </Button></CardContent></Card>)}
              </div>
              {result?.truncated && <p role="status" className="text-xs text-warning">{intl.formatMessage({ id: 'timeline.truncated' }, { cap: result.cap })}</p>}
              {rows.length === 0 ? <Empty icon={ChartGantt} title={intl.formatMessage({ id: 'timeline.empty' })} description={intl.formatMessage({ id: 'timeline.empty.hint' })} />
                : groups.length === 0 ? <Empty icon={ChartGantt} title={intl.formatMessage({ id: 'timeline.filter.empty', defaultMessage: 'No tasks match this status' })} />
                  : <div className="space-y-4" aria-label={intl.formatMessage({ id: 'timeline.staff.aria', defaultMessage: 'Work by employee' })}>
                    {groups.map((group) => {
                      const done = group.all.filter((row) => bucketFor(row) === 'done').length;
                      const waiting = group.all.filter((row) => bucketFor(row) === 'waiting');
                      const unfinished = group.all.filter((row) => bucketFor(row) === 'unfinished').length;
                      const active = group.all.filter((row) => bucketFor(row) === 'active').length;
                      const oldestWaiting = [...waiting].sort((a, b) => lastUpdateMs(a) - lastUpdateMs(b))[0];
                      const attention = [...group.shown].sort((a, b) => {
                        const rank = (row: TimelineRow) => bucketFor(row) === 'waiting' ? 0 : bucketFor(row) === 'unfinished' ? 1 : 2;
                        return rank(a) - rank(b) || lastUpdateMs(a) - lastUpdateMs(b);
                      });
                      return <Card key={group.id} data-testid="timeline-employee-card">
                        <CardHeader>
                          <CardTitle><span className="flex items-center gap-2"><ActorAvatar actorType={group.id && group.id !== 'unassigned' ? 'agent' : 'system'} size="md" name={nameFor(group.id)} /><span>{nameFor(group.id)}</span></span></CardTitle>
                          <CardDescription>{intl.formatMessage({ id: 'timeline.staff.summary', defaultMessage: '{done} completed, {active} in progress, {waiting} need your decision, {unfinished} unfinished' }, { done, active, waiting: waiting.length, unfinished })}{oldestWaiting ? ` · ${waitLabel(oldestWaiting)}` : ''}</CardDescription>
                        </CardHeader>
                        <CardContent>{attention.length === 0 ? <p className="text-sm text-muted-foreground">{intl.formatMessage({ id: 'timeline.staff.idle', defaultMessage: 'No tasks to follow up in this time range.' })}</p>
                          : <ul className="divide-y divide-surface-border">{attention.map((row) => <li key={`${row.kind}:${row.ref_id}`} className="flex flex-wrap items-center justify-between gap-2 py-3">
                            <div className="min-w-0 flex-1">{taskId(row) ? <Link className="block truncate rounded-md text-sm underline-offset-4 hover:underline focus-visible:ring-3 focus-visible:ring-ring/50" to={`/tasks/${encodeURIComponent(taskId(row)!)}`} title={row.label || statusLabel(row)}>{row.label || statusLabel(row)}</Link> : <span className="block truncate text-sm" title={row.label || statusLabel(row)}>{row.label || statusLabel(row)}</span>}
                              {bucketFor(row) !== 'done' && <p className="mt-1 text-xs text-muted-foreground">{waitLabel(row)}</p>}
                            </div><Badge variant={bucketFor(row) === 'unfinished' ? 'destructive' : 'secondary'}>{statusLabel(row)}</Badge>
                          </li>)}</ul>}</CardContent>
                      </Card>;
                    })}
                  </div>}
              {rows.length > 0 && validWindow && <details open={chartOpen} onToggle={(event) => setChartOpen(event.currentTarget.open)} className="rounded-xl border border-surface-border bg-surface p-4">
                <summary className="cursor-pointer rounded-md text-sm font-medium focus-visible:ring-3 focus-visible:ring-ring/50">{intl.formatMessage({ id: 'timeline.chart.toggle', defaultMessage: 'View time distribution' })}</summary>
                {chartOpen && <div className="mt-4 space-y-4">
                  <p id="timeline-chart-note" className="text-sm text-muted-foreground">{intl.formatMessage({ id: 'timeline.chart.note', defaultMessage: 'Each bar shows how long a task was open, not how long the employee worked. Open a task to see its details.' })}</p>
                  <div className="flex flex-wrap gap-3">{[...BUCKETS, 'other' as const].map((bucket) => <span key={bucket} className="flex items-center gap-1.5 text-xs text-muted-foreground"><svg viewBox="0 0 10 10" className="size-2.5" aria-hidden="true"><BucketMarker bucket={bucket} /></svg>{bucket === 'other' ? intl.formatMessage({ id: 'timeline.status.other', defaultMessage: 'Update recorded' }) : bucketLabel(bucket)}</span>)}</div>
                  <div className="overflow-x-auto"><svg viewBox={`0 0 960 ${40 + lanes.reduce((height, lane) => height + Math.max(lane.subRowCount * 32, 48), 0)}`} className="block min-w-3xl w-full" role="group" aria-label={intl.formatMessage({ id: 'timeline.chart.aria' })} aria-describedby="timeline-chart-note">
                    {timeTicks(windowMs.fromMs, windowMs.toMs, 6).map((tick) => <text key={tick.ms} x={180 + xForMs(tick.ms, windowMs.fromMs, windowMs.toMs, 760)} y={20} className="fill-muted-foreground text-xs" textAnchor="middle">{RANGE_HOURS[range] > 24 ? intl.formatDate(tick.ms, { month: 'numeric', day: 'numeric' }) : intl.formatTime(tick.ms, { hour: '2-digit', minute: '2-digit' })}</text>)}
                    {lanes.map((lane, index) => {
                      const top = 40 + lanes.slice(0, index).reduce((height, previous) => height + Math.max(previous.subRowCount * 32, 48), 0);
                      return <g key={lane.agentId}><text x={0} y={top + 20} className="fill-muted-foreground text-xs">{nameFor(lane.agentId)}</text>
                        {lane.rows.map((placed) => {
                          const row = placed.row;
                          const x = 180 + xForMs(placed.startMs, windowMs.fromMs, windowMs.toMs, 760);
                          const width = Math.max(6, xForMs(placed.endMs, windowMs.fromMs, windowMs.toMs, 760) - (x - 180));
                          const y = top + placed.subRow * 32;
                          const kind = intl.formatMessage({ id: `timeline.kind.${row.kind}`, defaultMessage: 'Update' });
                          const label = `${nameFor(row.agent_id)} · ${row.label || kind} · ${statusLabel(row)} · ${intl.formatDate(placed.startMs, { month: 'numeric', day: 'numeric' })} ${intl.formatTime(placed.startMs, { hour: '2-digit', minute: '2-digit' })}`;
                          const shape = <><title>{label}</title><rect x={x} y={y} width={Math.max(width, 24)} height={28} fill="transparent" /><rect x={x} y={y + 8} width={width} height={12} rx={4} fill={chartColor(row)} /><BucketMarker bucket={bucketFor(row) ?? 'other'} x={x} y={y + 1} /></>;
                          return taskId(row) ? <a key={`${row.kind}:${row.ref_id}`} href={`/tasks/${encodeURIComponent(taskId(row)!)}`} aria-label={label} className="focus-visible:outline focus-visible:outline-2 focus-visible:outline-ring">{shape}</a> : <g key={`${row.kind}:${row.ref_id}`} aria-label={label}>{shape}</g>;
                        })}</g>;
                    })}
                  </svg></div>
                </div>}
              </details>}
            </>}
      </div>
    </div>
  );
}
