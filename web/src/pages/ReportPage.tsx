import { useEffect, useState, useCallback } from 'react';
import { useIntl } from 'react-intl';
import { useConnectionStore } from '@/stores/connection-store';
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
import { Page, PageHeader, Card, StatCard, Tabs, Badge, EmptyState, Mono, type TabItem } from '@/components/ui';

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

function rateTone(rate: number): 'success' | 'warning' | 'danger' {
  if (rate >= 0.85) return 'success';
  if (rate >= 0.6) return 'warning';
  return 'danger';
}

function rateColor(rate: number): string {
  if (rate >= 0.85) return 'text-emerald-600 dark:text-emerald-400';
  if (rate >= 0.6) return 'text-amber-600 dark:text-amber-400';
  return 'text-rose-600 dark:text-rose-400';
}

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
        console.warn("[api]", e);
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

  const handlePeriodChange = (p: Period) => {
    setPeriod(p);
  };

  const maxCount = daily.length > 0 ? Math.max(...daily.map((d) => d.count), 1) : 1;

  // Show last N days based on period
  const visibleDays = period === 'day' ? 7 : period === 'week' ? 14 : 30;
  const visibleDaily = daily.slice(-visibleDays);

  const periodTabs: TabItem[] = (['day', 'week', 'month'] as const).map((p) => ({
    id: p,
    label: intl.formatMessage({ id: `reports.period.${p}` }),
  }));

  return (
    <Page>
      <PageHeader
        icon={BarChart3}
        title={intl.formatMessage({ id: 'nav.reports' })}
        subtitle={intl.formatMessage({ id: 'app.subtitle' })}
        actions={
          <Tabs
            items={periodTabs}
            value={period}
            onChange={(id) => handlePeriodChange(id as Period)}
          />
        }
      />

      {/* Summary cards */}
      {summary && (
        <div className="grid grid-cols-1 gap-4 sm:grid-cols-2 lg:grid-cols-4">
          <StatCard
            icon={MessageCircle}
            tone="accent"
            label={intl.formatMessage({ id: 'reports.conversations' })}
            value={summary.total_conversations.toLocaleString()}
            hint={`${summary.total_messages.toLocaleString()} messages`}
          />
          <StatCard
            icon={Zap}
            tone={rateTone(summary.auto_reply_rate)}
            label={intl.formatMessage({ id: 'reports.autoReplyRate' })}
            value={
              <span className={rateColor(summary.auto_reply_rate)}>
                {(summary.auto_reply_rate * 100).toFixed(1)}%
              </span>
            }
          />
          <StatCard
            icon={Clock}
            tone="neutral"
            label={intl.formatMessage({ id: 'reports.avgResponse' })}
            value={`${summary.avg_response_ms}ms`}
            hint={`P95: ${summary.p95_response_ms}ms`}
          />
          <StatCard
            icon={DollarSign}
            tone="success"
            label={intl.formatMessage({ id: 'reports.savings' })}
            value={`$${(summary.estimated_savings_cents / 100).toFixed(2)}`}
          />
        </div>
      )}

      {/* Cost & cache efficiency (CostTelemetry) */}
      <CostEfficiencySection hours={PERIOD_HOURS[period]} />

      {/* Zero-cost ratio */}
      {summary && (
        <Card title={intl.formatMessage({ id: 'reports.zeroCostRatio' })}>
          <div className="flex items-center gap-4">
            <div className="flex-1">
              <div className="h-6 w-full overflow-hidden rounded-full bg-stone-200 dark:bg-stone-700">
                <div
                  className="h-full rounded-full bg-gradient-to-r from-amber-400 to-amber-500 transition-all duration-500"
                  style={{ width: `${summary.zero_cost_ratio * 100}%` }}
                />
              </div>
            </div>
            <span className="min-w-[4rem] text-right text-2xl font-bold tabular-nums text-amber-600 dark:text-amber-400">
              {(summary.zero_cost_ratio * 100).toFixed(1)}%
            </span>
          </div>
          <p className="mt-2 text-sm text-stone-500 dark:text-stone-400">
            {intl.formatMessage({ id: 'reports.zeroCostDesc' })}
          </p>
        </Card>
      )}

      {/* Conversation trend (CSS bar chart) */}
      <Card title={intl.formatMessage({ id: 'reports.trend' })}>
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
                    className="w-full rounded-t bg-stone-300 dark:bg-stone-600"
                    style={{ height: `${manualPct > 0 ? (manualPct / totalPct) * 100 : 0}%`, minHeight: manualPct > 0 ? 2 : 0 }}
                  />
                  <div
                    className="w-full rounded-b bg-amber-400 dark:bg-amber-500"
                    style={{ flex: 1 }}
                  />
                </div>
              </div>
            );
          })}
        </div>
        <div className="mt-2 flex items-center gap-4 text-xs text-stone-500 dark:text-stone-400">
          <span className="flex items-center gap-1">
            <span className="inline-block h-2.5 w-2.5 rounded-sm bg-amber-400 dark:bg-amber-500" />
            {intl.formatMessage({ id: 'reports.autoReply' })}
          </span>
          <span className="flex items-center gap-1">
            <span className="inline-block h-2.5 w-2.5 rounded-sm bg-stone-300 dark:bg-stone-600" />
            {intl.formatMessage({ id: 'reports.manual' })}
          </span>
        </div>
      </Card>

      {/* Cost savings table */}
      <Card title={intl.formatMessage({ id: 'reports.costComparison' })} padded={false}>
        <div className="overflow-x-auto">
          <table className="w-full text-left text-sm">
            <thead>
              <tr className="border-b border-[var(--panel-border)]">
                <th className="px-5 pb-3 pt-4 font-medium text-stone-500 dark:text-stone-400">
                  {intl.formatMessage({ id: 'reports.period.month' })}
                </th>
                <th className="px-5 pb-3 pt-4 text-right font-medium text-stone-500 dark:text-stone-400">
                  {intl.formatMessage({ id: 'reports.humanCost' })}
                </th>
                <th className="px-5 pb-3 pt-4 text-right font-medium text-stone-500 dark:text-stone-400">
                  {intl.formatMessage({ id: 'reports.agentCost' })}
                </th>
                <th className="px-5 pb-3 pt-4 text-right font-medium text-stone-500 dark:text-stone-400">
                  {intl.formatMessage({ id: 'reports.netSavings' })}
                </th>
              </tr>
            </thead>
            <tbody>
              {costs.map((row) => (
                <tr
                  key={row.month}
                  className="border-b border-[var(--panel-border)] last:border-0"
                >
                  <td className="px-5 py-3 text-stone-900 dark:text-stone-100">{row.month}</td>
                  <td className="px-5 py-3 text-right tabular-nums text-stone-600 dark:text-stone-400">
                    ${(row.human_cost / 100).toFixed(2)}
                  </td>
                  <td className="px-5 py-3 text-right tabular-nums text-stone-600 dark:text-stone-400">
                    ${(row.agent_cost / 100).toFixed(2)}
                  </td>
                  <td className="px-5 py-3 text-right font-medium tabular-nums text-emerald-600 dark:text-emerald-400">
                    +${(row.savings / 100).toFixed(2)}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
          {costs.length === 0 && (
            <EmptyState
              icon={DollarSign}
              dudu="reading"
              title={intl.formatMessage({ id: 'common.noData' })}
            />
          )}
        </div>
      </Card>
    </Page>
  );
}

function cacheHealthTone(h: CacheHealth): 'success' | 'neutral' | 'warning' {
  if (h === 'healthy') return 'success';
  if (h === 'degraded') return 'warning';
  return 'neutral';
}

function effTone(eff: number): 'success' | 'warning' | 'danger' {
  if (eff >= 0.7) return 'success';
  if (eff >= 0.3) return 'warning';
  return 'danger';
}

/**
 * Cache-efficiency + cost telemetry (CostTelemetry). Three-state: telemetry
 * off → empty state; load error → inline notice (never crashes the page);
 * otherwise cache hit rate, total cost, savings, 200K price-cliff warning,
 * per-agent cache health, and a recent-usage table.
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
    Promise.all([
      api.cost.summary(hours),
      api.cost.agents(hours),
      api.cost.recent(20),
    ])
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

  const title = (
    <span className="flex items-center gap-2">
      <Gauge className="h-4 w-4 text-amber-600 dark:text-amber-400" />
      {intl.formatMessage({ id: 'reports.cache.title' })}
    </span>
  );

  if (loading) {
    return (
      <Card title={title}>
        <div className="py-10 text-center text-sm text-stone-400">
          {intl.formatMessage({ id: 'common.loading' })}
        </div>
      </Card>
    );
  }

  if (failed) {
    return (
      <Card title={title}>
        <div className="py-8 text-center text-sm text-stone-500 dark:text-stone-400">
          {intl.formatMessage({ id: 'reports.cache.loadError' })}
        </div>
      </Card>
    );
  }

  if (!summary?.available) {
    return (
      <Card title={title}>
        <EmptyState
          icon={Gauge}
          dudu="sleep"
          title={intl.formatMessage({ id: 'reports.cache.empty.title' })}
          hint={intl.formatMessage({ id: 'reports.cache.empty.desc' })}
        />
      </Card>
    );
  }

  const hitRate = summary.cache_hit_rate ?? 0;
  const cliff = summary.price_cliff;

  return (
    <div className="space-y-4">
      {/* 200K price-cliff warning */}
      {cliff?.warning && (
        <div className="flex items-start gap-3 rounded-lg border border-rose-300 bg-rose-50 px-4 py-3 dark:border-rose-800 dark:bg-rose-900/20">
          <TriangleAlert className="mt-0.5 h-5 w-5 shrink-0 text-rose-500" />
          <div className="text-sm text-rose-700 dark:text-rose-300">
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

      <div className="grid grid-cols-1 gap-4 sm:grid-cols-2 lg:grid-cols-4">
        <StatCard
          icon={Gauge}
          tone={effTone(hitRate)}
          label={intl.formatMessage({ id: 'reports.cache.hitRate' })}
          value={`${(hitRate * 100).toFixed(1)}%`}
          hint={intl.formatMessage(
            { id: 'reports.cache.avgEff' },
            { value: ((summary.avg_cache_efficiency ?? 0) * 100).toFixed(1) },
          )}
        />
        <StatCard
          icon={Coins}
          tone="neutral"
          label={intl.formatMessage({ id: 'reports.cache.totalCost' })}
          value={formatMillicents(summary.total_cost_millicents)}
          hint={intl.formatMessage(
            { id: 'reports.cache.requests' },
            { count: summary.total_requests ?? 0 },
          )}
        />
        <StatCard
          icon={DollarSign}
          tone="success"
          label={intl.formatMessage({ id: 'reports.cache.savings' })}
          value={formatMillicents(summary.total_cache_savings_millicents)}
        />
        <StatCard
          icon={Database}
          tone="accent"
          label={intl.formatMessage({ id: 'reports.cache.cacheReads' })}
          value={formatTokens(summary.total_cache_read_tokens)}
          hint={intl.formatMessage(
            { id: 'reports.cache.ofInput' },
            { value: formatTokens(summary.total_input_tokens) },
          )}
        />
      </div>

      {/* Per-agent cache health */}
      <Card title={intl.formatMessage({ id: 'reports.cache.byAgent' })} padded={false}>
        {agents.length === 0 ? (
          <EmptyState icon={Gauge} dudu="idle" title={intl.formatMessage({ id: 'common.noData' })} />
        ) : (
          <div className="overflow-x-auto">
            <table className="w-full text-left text-sm">
              <thead>
                <tr className="border-b border-[var(--panel-border)]">
                  <th className="px-5 pb-3 pt-4 font-medium text-stone-500 dark:text-stone-400">
                    {intl.formatMessage({ id: 'reports.cache.col.agent' })}
                  </th>
                  <th className="px-5 pb-3 pt-4 font-medium text-stone-500 dark:text-stone-400">
                    {intl.formatMessage({ id: 'reports.cache.col.health' })}
                  </th>
                  <th className="px-5 pb-3 pt-4 text-right font-medium text-stone-500 dark:text-stone-400">
                    {intl.formatMessage({ id: 'reports.cache.col.eff' })}
                  </th>
                  <th className="px-5 pb-3 pt-4 text-right font-medium text-stone-500 dark:text-stone-400">
                    {intl.formatMessage({ id: 'reports.cache.col.requests' })}
                  </th>
                  <th className="px-5 pb-3 pt-4 text-right font-medium text-stone-500 dark:text-stone-400">
                    {intl.formatMessage({ id: 'reports.cache.col.cost' })}
                  </th>
                  <th className="px-5 pb-3 pt-4 text-right font-medium text-stone-500 dark:text-stone-400">
                    {intl.formatMessage({ id: 'reports.cache.col.savings' })}
                  </th>
                </tr>
              </thead>
              <tbody>
                {agents.map((a) => (
                  <tr key={a.agent_id} className="border-b border-[var(--panel-border)] last:border-0">
                    <td className="px-5 py-3 text-stone-900 dark:text-stone-100">{a.agent_id}</td>
                    <td className="px-5 py-3">
                      <Badge tone={cacheHealthTone(a.cache_health)} dot>
                        {intl.formatMessage({ id: `reports.cache.health.${a.cache_health}` })}
                      </Badge>
                    </td>
                    <td className="px-5 py-3 text-right tabular-nums text-stone-600 dark:text-stone-400">
                      {(a.avg_cache_efficiency * 100).toFixed(1)}%
                    </td>
                    <td className="px-5 py-3 text-right tabular-nums text-stone-600 dark:text-stone-400">
                      {a.total_requests.toLocaleString()}
                    </td>
                    <td className="px-5 py-3 text-right tabular-nums text-stone-600 dark:text-stone-400">
                      {formatMillicents(a.total_cost_millicents)}
                    </td>
                    <td className="px-5 py-3 text-right font-medium tabular-nums text-emerald-600 dark:text-emerald-400">
                      {formatMillicents(a.total_cache_savings_millicents)}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </Card>

      {/* Recent usage */}
      {recent.length > 0 && (
        <Card title={intl.formatMessage({ id: 'reports.cache.recent' })} padded={false}>
          <div className="overflow-x-auto">
            <table className="w-full text-left text-sm">
              <thead>
                <tr className="border-b border-[var(--panel-border)]">
                  <th className="px-5 pb-3 pt-4 font-medium text-stone-500 dark:text-stone-400">
                    {intl.formatMessage({ id: 'reports.cache.col.agent' })}
                  </th>
                  <th className="px-5 pb-3 pt-4 font-medium text-stone-500 dark:text-stone-400">
                    {intl.formatMessage({ id: 'reports.cache.col.model' })}
                  </th>
                  <th className="px-5 pb-3 pt-4 text-right font-medium text-stone-500 dark:text-stone-400">
                    {intl.formatMessage({ id: 'reports.cache.col.input' })}
                  </th>
                  <th className="px-5 pb-3 pt-4 text-right font-medium text-stone-500 dark:text-stone-400">
                    {intl.formatMessage({ id: 'reports.cache.col.eff' })}
                  </th>
                  <th className="px-5 pb-3 pt-4 text-right font-medium text-stone-500 dark:text-stone-400">
                    {intl.formatMessage({ id: 'reports.cache.col.cost' })}
                  </th>
                  <th className="px-5 pb-3 pt-4 text-right font-medium text-stone-500 dark:text-stone-400">
                    {intl.formatMessage({ id: 'reports.cache.col.time' })}
                  </th>
                </tr>
              </thead>
              <tbody>
                {recent.map((r, i) => (
                  <tr key={`${r.created_at}-${i}`} className="border-b border-[var(--panel-border)] last:border-0">
                    <td className="px-5 py-3 text-stone-900 dark:text-stone-100">{r.agent_id}</td>
                    <td className="px-5 py-3 text-stone-600 dark:text-stone-400">
                      <Mono className="text-xs text-stone-500 dark:text-stone-400">{r.model}</Mono>
                    </td>
                    <td className="px-5 py-3 text-right tabular-nums text-stone-600 dark:text-stone-400">
                      {formatTokens(r.input_tokens)}
                    </td>
                    <td className="px-5 py-3 text-right tabular-nums text-stone-600 dark:text-stone-400">
                      {(r.cache_efficiency * 100).toFixed(0)}%
                    </td>
                    <td className="px-5 py-3 text-right tabular-nums text-stone-600 dark:text-stone-400">
                      {formatMillicents(r.cost_millicents)}
                    </td>
                    <td className="px-5 py-3 text-right text-xs text-stone-400 dark:text-stone-500">
                      <Mono className="text-xs text-stone-400 dark:text-stone-500">
                        {new Date(r.created_at).toLocaleString()}
                      </Mono>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </Card>
      )}
    </div>
  );
}
