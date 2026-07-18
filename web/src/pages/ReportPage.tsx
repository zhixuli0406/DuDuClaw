import { useEffect, useState, useCallback, type ComponentType, type ReactNode } from 'react';
import { useIntl } from 'react-intl';
import { useConnectionStore } from '@/stores/connection-store';
import { cn } from '@/lib/utils';
import {
  api,
  type CostSummary,
  type CostAgentRow,
  type CostRecentRow,
  type CacheHealth,
} from '@/lib/api';
import { formatMillicents, formatTokens } from '@/lib/format';
import {
  MessageCircle,
  Zap,
  Clock,
  DollarSign,
  BarChart3,
  Gauge,
  Coins,
  Database,
  TriangleAlert,
} from 'lucide-react';
import { toast, formatError } from '@/lib/toast';
import {
  PageHeader,
  Segmented,
  Empty,
  Badge,
  ActorAvatar,
  Table,
  TableHeader,
  TableBody,
  TableRow,
  TableHead,
  TableCell,
  type SegmentedOption,
} from '@/components/mds';

type Period = 'day' | 'week' | 'month';

const PERIOD_HOURS: Record<Period, number> = { day: 24, week: 168, month: 720 };

interface Summary {
  total_conversations: number;
  total_messages: number;
  auto_reply_rate: number;
  avg_response_ms: number;
  p95_response_ms: number;
  zero_cost_ratio: number;
  estimated_savings_cents: number;
  period: string;
}

interface DailyRow {
  date: string;
  count: number;
  auto_count: number;
}

interface CostRow {
  month: string;
  human_cost: number;
  agent_cost: number;
  savings: number;
}

/** rate → semantic value color (auto-reply / cache efficiency). */
function rateColorClass(rate: number): string {
  if (rate >= 0.85) return 'text-success';
  if (rate >= 0.6) return 'text-warning';
  return 'text-destructive';
}

function effColorClass(eff: number): string {
  if (eff >= 0.7) return 'text-success';
  if (eff >= 0.3) return 'text-warning';
  return 'text-destructive';
}

/** One KPI cell inside a divided KPI group (spec §5.5). */
function Kpi({
  icon: Icon,
  label,
  value,
  hint,
  valueClass,
}: {
  icon?: ComponentType<{ className?: string }>;
  label: ReactNode;
  value: ReactNode;
  hint?: ReactNode;
  valueClass?: string;
}) {
  return (
    <div className="bg-card p-4">
      <div className="flex items-center gap-1.5 text-xs text-muted-foreground">
        {Icon && <Icon className="size-3.5 shrink-0" />}
        <span className="truncate">{label}</span>
      </div>
      <p className={cn('mt-1.5 text-2xl font-semibold tabular-nums text-foreground', valueClass)}>
        {value}
      </p>
      {hint && <p className="mt-0.5 truncate text-xs text-muted-foreground">{hint}</p>}
    </div>
  );
}

/** A responsive divided KPI strip (`gap-px` reveals the card-colored gutters). */
function KpiGroup({ children }: { children: ReactNode }) {
  return (
    <div className="grid grid-cols-2 gap-px overflow-hidden rounded-lg border border-surface-border bg-surface-border lg:grid-cols-4">
      {children}
    </div>
  );
}

/** A titled report panel (spec §5.5 `rounded-lg border bg-card p-4`). */
function ReportCard({
  title,
  action,
  className,
  children,
}: {
  title?: ReactNode;
  action?: ReactNode;
  className?: string;
  children: ReactNode;
}) {
  return (
    <section className={cn('rounded-lg border border-surface-border bg-card p-4', className)}>
      {(title || action) && (
        <div className="mb-3 flex items-center justify-between gap-2">
          {title && <h2 className="text-sm font-medium text-foreground">{title}</h2>}
          {action}
        </div>
      )}
      {children}
    </section>
  );
}

/**
 * ReportPage (用量報表) — the Usage/report layout (spec §5.5). KPI strips, a
 * trend chart, per-agent cache ranking, cost comparison and recent-usage tables,
 * plus the 200K price-cliff warning. Same `analytics.*` / `cost.*` RPCs; only
 * the surface moved onto MDS. The SVG/CSS charts are recolored, not rebuilt.
 */
export function ReportPage() {
  const intl = useIntl();
  const connectionState = useConnectionStore((s) => s.state);
  const [period, setPeriod] = useState<Period>('month');
  const [summary, setSummary] = useState<Summary | null>(null);
  const [daily, setDaily] = useState<readonly DailyRow[]>([]);
  const [costs, setCosts] = useState<readonly CostRow[]>([]);

  const fetchData = useCallback(
    (p: Period) => {
      // One aggregate toast if anything in the group fails so three parallel
      // errors don't stack three notifications on the user.
      let notified = false;
      const onFailure = (e: unknown) => {
        console.warn('[api]', e);
        if (notified) return;
        notified = true;
        toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
      };
      api.analytics.summary(p).then(setSummary).catch(onFailure);
      api.analytics.conversations().then((r) => setDaily(r?.daily ?? [])).catch(onFailure);
      api.analytics.costSavings().then((r) => setCosts(r?.monthly ?? [])).catch(onFailure);
    },
    [intl],
  );

  useEffect(() => {
    if (connectionState !== 'authenticated') return;
    fetchData(period);
  }, [connectionState, period, fetchData]);

  const maxCount = daily.length > 0 ? Math.max(...daily.map((d) => d.count), 1) : 1;

  // Show last N days based on period.
  const visibleDays = period === 'day' ? 7 : period === 'week' ? 14 : 30;
  const visibleDaily = daily.slice(-visibleDays);

  const periodOptions: SegmentedOption<Period>[] = (['day', 'week', 'month'] as const).map((p) => ({
    value: p,
    label: intl.formatMessage({ id: `reports.period.${p}` }),
  }));

  return (
    <div className="-mx-4 -mt-4 flex flex-col md:-mx-6 md:-mt-6">
      <PageHeader hideTrigger className="h-auto min-h-12 flex-wrap justify-between gap-2 px-5 py-1.5">
        <div className="flex min-w-0 items-center gap-2">
          <BarChart3 className="size-4 shrink-0 text-muted-foreground" />
          <h1 className="truncate text-sm font-medium">{intl.formatMessage({ id: 'nav.reports' })}</h1>
        </div>
        <Segmented
          value={period}
          onValueChange={setPeriod}
          options={periodOptions}
          aria-label={intl.formatMessage({ id: 'nav.reports' })}
        />
      </PageHeader>

      <div className="mx-auto w-full max-w-6xl space-y-5 p-6">
        {/* Conversation KPI strip. */}
        {summary && (
          <KpiGroup>
            <Kpi
              icon={MessageCircle}
              label={intl.formatMessage({ id: 'reports.conversations' })}
              value={summary.total_conversations.toLocaleString()}
              hint={`${summary.total_messages.toLocaleString()} ${intl.formatMessage({ id: 'reports.messages' })}`}
            />
            <Kpi
              icon={Zap}
              label={intl.formatMessage({ id: 'reports.autoReplyRate' })}
              value={`${(summary.auto_reply_rate * 100).toFixed(1)}%`}
              valueClass={rateColorClass(summary.auto_reply_rate)}
            />
            <Kpi
              icon={Clock}
              label={intl.formatMessage({ id: 'reports.avgResponse' })}
              value={`${summary.avg_response_ms}ms`}
              hint={`P95: ${summary.p95_response_ms}ms`}
            />
            <Kpi
              icon={DollarSign}
              label={intl.formatMessage({ id: 'reports.savings' })}
              value={`$${(summary.estimated_savings_cents / 100).toFixed(2)}`}
              valueClass="text-success"
            />
          </KpiGroup>
        )}

        {/* Cost & cache efficiency (CostTelemetry). */}
        <CostEfficiencySection hours={PERIOD_HOURS[period]} />

        {/* Zero-cost ratio. */}
        {summary && (
          <ReportCard title={intl.formatMessage({ id: 'reports.zeroCostRatio' })}>
            <div className="flex items-center gap-4">
              <div className="h-2 flex-1 overflow-hidden rounded-full bg-muted">
                <div
                  className="h-full rounded-full bg-chart-1 transition-all duration-500"
                  style={{ width: `${summary.zero_cost_ratio * 100}%` }}
                />
              </div>
              <span className="min-w-[4rem] text-right font-mono text-xl font-semibold tabular-nums text-foreground">
                {(summary.zero_cost_ratio * 100).toFixed(1)}%
              </span>
            </div>
            <p className="mt-2 text-sm text-muted-foreground">
              {intl.formatMessage({ id: 'reports.zeroCostDesc' })}
            </p>
          </ReportCard>
        )}

        {/* Conversation trend (recolored CSS bar chart). */}
        <ReportCard title={intl.formatMessage({ id: 'reports.trend' })} className="min-h-[240px]">
          <div className="flex items-end gap-1" style={{ height: 180 }}>
            {visibleDaily.map((day) => {
              const totalPct = (day.count / maxCount) * 100;
              const autoPct = (day.auto_count / maxCount) * 100;
              const manualPct = totalPct - autoPct;
              return (
                <div
                  key={day.date}
                  className="group relative flex flex-1 flex-col items-center justify-end"
                  style={{ height: '100%' }}
                  title={`${day.date}: ${day.count} total, ${day.auto_count} auto`}
                >
                  <div className="flex w-full flex-col items-stretch" style={{ height: `${totalPct}%` }}>
                    <div
                      className="w-full rounded-t bg-muted-foreground/30"
                      style={{
                        height: `${manualPct > 0 ? (manualPct / totalPct) * 100 : 0}%`,
                        minHeight: manualPct > 0 ? 2 : 0,
                      }}
                    />
                    <div className="w-full rounded-b bg-chart-1" style={{ flex: 1 }} />
                  </div>
                </div>
              );
            })}
          </div>
          <div className="mt-2 flex items-center gap-4 text-xs text-muted-foreground">
            <span className="flex items-center gap-1">
              <span className="inline-block size-2.5 rounded-sm bg-chart-1" />
              {intl.formatMessage({ id: 'reports.autoReply' })}
            </span>
            <span className="flex items-center gap-1">
              <span className="inline-block size-2.5 rounded-sm bg-muted-foreground/30" />
              {intl.formatMessage({ id: 'reports.manual' })}
            </span>
          </div>
        </ReportCard>

        {/* Cost savings table. */}
        <ReportCard title={intl.formatMessage({ id: 'reports.costComparison' })}>
          {costs.length === 0 ? (
            <Empty icon={DollarSign} title={intl.formatMessage({ id: 'common.noData' })} />
          ) : (
            <div className="overflow-x-auto">
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead>{intl.formatMessage({ id: 'reports.period.month' })}</TableHead>
                    <TableHead className="text-right">{intl.formatMessage({ id: 'reports.humanCost' })}</TableHead>
                    <TableHead className="text-right">{intl.formatMessage({ id: 'reports.agentCost' })}</TableHead>
                    <TableHead className="text-right">{intl.formatMessage({ id: 'reports.netSavings' })}</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {costs.map((row) => (
                    <TableRow key={row.month}>
                      <TableCell className="text-foreground">{row.month}</TableCell>
                      <TableCell className="text-right font-mono tabular-nums text-muted-foreground">
                        ${(row.human_cost / 100).toFixed(2)}
                      </TableCell>
                      <TableCell className="text-right font-mono tabular-nums text-muted-foreground">
                        ${(row.agent_cost / 100).toFixed(2)}
                      </TableCell>
                      <TableCell className="text-right font-mono font-medium tabular-nums text-success">
                        +${(row.savings / 100).toFixed(2)}
                      </TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            </div>
          )}
        </ReportCard>
      </div>
    </div>
  );
}

function cacheHealthTone(h: CacheHealth): 'default' | 'secondary' {
  return h === 'healthy' ? 'default' : 'secondary';
}

function cacheHealthClass(h: CacheHealth): string | undefined {
  if (h === 'healthy') return 'bg-success/15 text-success';
  if (h === 'degraded') return 'bg-warning/15 text-warning';
  return undefined;
}

/**
 * Cache-efficiency + cost telemetry (CostTelemetry). Three-state: telemetry
 * off → empty state; load error → inline notice (never crashes the page);
 * otherwise cache hit rate, total cost, savings, 200K price-cliff warning,
 * per-agent cache ranking, and a recent-usage table.
 */
function CostEfficiencySection({ hours }: { hours: number }) {
  const intl = useIntl();
  const connectionState = useConnectionStore((s) => s.state);
  const [summary, setSummary] = useState<CostSummary | null>(null);
  const [agents, setAgents] = useState<readonly CostAgentRow[]>([]);
  const [recent, setRecent] = useState<readonly CostRecentRow[]>([]);
  const [loading, setLoading] = useState(true);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    if (connectionState !== 'authenticated') return;
    let cancelled = false;
    setLoading(true);
    setFailed(false);
    Promise.all([api.cost.summary(hours), api.cost.agents(hours), api.cost.recent(20)])
      .then(([s, a, r]) => {
        if (cancelled) return;
        setSummary(s);
        setAgents(a?.available ? a.agents ?? [] : []);
        setRecent(r?.available ? r.records ?? [] : []);
      })
      .catch((e) => {
        if (cancelled) return;
        console.warn('[api]', e);
        setFailed(true);
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [connectionState, hours]);

  const title = intl.formatMessage({ id: 'reports.cache.title' });

  if (loading) {
    return (
      <ReportCard title={title}>
        <div className="py-10 text-center text-sm text-muted-foreground">
          {intl.formatMessage({ id: 'common.loading' })}
        </div>
      </ReportCard>
    );
  }

  if (failed) {
    return (
      <ReportCard title={title}>
        <div className="py-8 text-center text-sm text-muted-foreground">
          {intl.formatMessage({ id: 'reports.cache.loadError' })}
        </div>
      </ReportCard>
    );
  }

  if (!summary?.available) {
    return (
      <ReportCard title={title}>
        <Empty
          icon={Gauge}
          title={intl.formatMessage({ id: 'reports.cache.empty.title' })}
          description={intl.formatMessage({ id: 'reports.cache.empty.desc' })}
        />
      </ReportCard>
    );
  }

  const hitRate = summary.cache_hit_rate ?? 0;
  const cliff = summary.price_cliff;
  const maxCost = agents.reduce((m, a) => Math.max(m, a.total_cost_millicents), 0);

  return (
    <div className="space-y-5">
      {/* 200K price-cliff warning. */}
      {cliff?.warning && (
        <div className="flex items-start gap-3 rounded-lg border border-warning/40 bg-warning/10 px-4 py-3">
          <TriangleAlert className="mt-0.5 size-5 shrink-0 text-warning" />
          <div className="text-sm text-warning">
            <p className="font-medium">{intl.formatMessage({ id: 'reports.cache.cliff.title' })}</p>
            <p className="mt-0.5 text-xs">
              {intl.formatMessage(
                { id: 'reports.cache.cliff.desc' },
                {
                  count: cliff.requests_near_cliff,
                  threshold: formatTokens(cliff.threshold_input_tokens),
                  max: formatTokens(cliff.max_input_tokens),
                },
              )}
            </p>
          </div>
        </div>
      )}

      <KpiGroup>
        <Kpi
          icon={Gauge}
          label={intl.formatMessage({ id: 'reports.cache.hitRate' })}
          value={`${(hitRate * 100).toFixed(1)}%`}
          valueClass={effColorClass(hitRate)}
          hint={intl.formatMessage(
            { id: 'reports.cache.avgEff' },
            { value: ((summary.avg_cache_efficiency ?? 0) * 100).toFixed(1) },
          )}
        />
        <Kpi
          icon={Coins}
          label={intl.formatMessage({ id: 'reports.cache.totalCost' })}
          value={formatMillicents(summary.total_cost_millicents)}
          hint={intl.formatMessage(
            { id: 'reports.cache.requests' },
            { count: summary.total_requests ?? 0 },
          )}
        />
        <Kpi
          icon={DollarSign}
          label={intl.formatMessage({ id: 'reports.cache.savings' })}
          value={formatMillicents(summary.total_cache_savings_millicents)}
          valueClass="text-success"
        />
        <Kpi
          icon={Database}
          label={intl.formatMessage({ id: 'reports.cache.cacheReads' })}
          value={formatTokens(summary.total_cache_read_tokens)}
          hint={intl.formatMessage(
            { id: 'reports.cache.ofInput' },
            { value: formatTokens(summary.total_input_tokens) },
          )}
        />
      </KpiGroup>

      {/* Per-agent cache ranking (spec §5.5 ranking card). */}
      <ReportCard title={intl.formatMessage({ id: 'reports.cache.byAgent' })}>
        {agents.length === 0 ? (
          <Empty icon={Gauge} title={intl.formatMessage({ id: 'common.noData' })} />
        ) : (
          <div className="divide-y divide-surface-border">
            {agents.map((a) => {
              const pct = maxCost > 0 ? Math.max(4, Math.round((a.total_cost_millicents / maxCost) * 100)) : 0;
              return (
                <div key={a.agent_id} className="py-3 first:pt-0 last:pb-0">
                  <div className="flex items-center gap-2">
                    <ActorAvatar actorType="agent" size="sm" name={a.agent_id} />
                    <span className="min-w-0 flex-1 truncate text-sm font-medium text-foreground" title={a.agent_id}>
                      {a.agent_id}
                    </span>
                    <Badge variant={cacheHealthTone(a.cache_health)} className={cacheHealthClass(a.cache_health)}>
                      {intl.formatMessage({ id: `reports.cache.health.${a.cache_health}` })}
                    </Badge>
                    <span className="shrink-0 font-mono text-xs tabular-nums text-muted-foreground">
                      {formatMillicents(a.total_cost_millicents)}
                    </span>
                  </div>
                  <div className="mt-1.5 h-2 overflow-hidden rounded-full bg-muted">
                    <div className="h-full rounded-full bg-chart-1" style={{ width: `${pct}%` }} />
                  </div>
                  <div className="mt-1.5 flex flex-wrap items-center gap-x-3 gap-y-0.5 text-xs text-muted-foreground">
                    <span>
                      {intl.formatMessage({ id: 'reports.cache.col.eff' })}:{' '}
                      <span className="font-mono tabular-nums">{(a.avg_cache_efficiency * 100).toFixed(1)}%</span>
                    </span>
                    <span>
                      {intl.formatMessage({ id: 'reports.cache.col.requests' })}:{' '}
                      <span className="font-mono tabular-nums">{a.total_requests.toLocaleString()}</span>
                    </span>
                    <span className="text-success">
                      {intl.formatMessage({ id: 'reports.cache.col.savings' })}:{' '}
                      <span className="font-mono tabular-nums">
                        {formatMillicents(a.total_cache_savings_millicents)}
                      </span>
                    </span>
                  </div>
                </div>
              );
            })}
          </div>
        )}
      </ReportCard>

      {/* Recent usage. */}
      {recent.length > 0 && (
        <ReportCard title={intl.formatMessage({ id: 'reports.cache.recent' })}>
          <div className="overflow-x-auto">
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>{intl.formatMessage({ id: 'reports.cache.col.agent' })}</TableHead>
                  <TableHead>{intl.formatMessage({ id: 'reports.cache.col.model' })}</TableHead>
                  <TableHead className="text-right">{intl.formatMessage({ id: 'reports.cache.col.input' })}</TableHead>
                  <TableHead className="text-right">{intl.formatMessage({ id: 'reports.cache.col.eff' })}</TableHead>
                  <TableHead className="text-right">{intl.formatMessage({ id: 'reports.cache.col.cost' })}</TableHead>
                  <TableHead className="text-right">{intl.formatMessage({ id: 'reports.cache.col.time' })}</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {recent.map((r, i) => (
                  <TableRow key={`${r.created_at}-${i}`}>
                    <TableCell className="text-foreground">{r.agent_id}</TableCell>
                    <TableCell>
                      <span className="font-mono text-xs text-muted-foreground">{r.model}</span>
                    </TableCell>
                    <TableCell className="text-right font-mono tabular-nums text-muted-foreground">
                      {formatTokens(r.input_tokens)}
                    </TableCell>
                    <TableCell className="text-right font-mono tabular-nums text-muted-foreground">
                      {(r.cache_efficiency * 100).toFixed(0)}%
                    </TableCell>
                    <TableCell className="text-right font-mono tabular-nums text-muted-foreground">
                      {formatMillicents(r.cost_millicents)}
                    </TableCell>
                    <TableCell className="text-right font-mono text-xs text-muted-foreground">
                      {new Date(r.created_at).toLocaleString()}
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </div>
        </ReportCard>
      )}
    </div>
  );
}
