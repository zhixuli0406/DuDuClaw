import { useEffect, useState, useCallback } from 'react';
import { useIntl } from 'react-intl';
import { useConnectionStore } from '@/stores/connection-store';
import { api } from '@/lib/api';
import { MessageCircle, Zap, Clock, DollarSign } from 'lucide-react';
import { cn } from '@/lib/utils';

type Period = 'day' | 'week' | 'month';

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

function StatCard({
  icon: Icon,
  title,
  value,
  subtitle,
  color,
}: {
  icon: React.ComponentType<{ className?: string }>;
  title: string;
  value: string | number;
  subtitle?: string;
  color: string;
}) {
  return (
    <div className="rounded-xl border border-stone-200 bg-white p-5 dark:border-stone-800 dark:bg-stone-900">
      <div className="flex items-center gap-3">
        <div className={cn('rounded-lg p-2.5', color)}>
          <Icon className="h-5 w-5 text-white" />
        </div>
        <div>
          <p className="text-sm text-stone-500 dark:text-stone-400">{title}</p>
          <p className="text-2xl font-semibold text-stone-900 dark:text-stone-50">
            {value}
          </p>
          {subtitle && (
            <p className="text-xs text-stone-400 dark:text-stone-500">
              {subtitle}
            </p>
          )}
        </div>
      </div>
    </div>
  );
}

function rateColor(rate: number): string {
  if (rate >= 0.85) return 'text-emerald-600 dark:text-emerald-400';
  if (rate >= 0.6) return 'text-amber-600 dark:text-amber-400';
  return 'text-rose-600 dark:text-rose-400';
}

function rateBg(rate: number): string {
  if (rate >= 0.85) return 'bg-emerald-500';
  if (rate >= 0.6) return 'bg-amber-500';
  return 'bg-rose-500';
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
      api.analytics.summary(p).then(setSummary).catch((e) => console.warn("[api]", e));
      api.analytics.conversations().then((r) => setDaily(r?.daily ?? [])).catch((e) => console.warn("[api]", e));
      api.analytics.costSavings().then((r) => setCosts(r?.monthly ?? [])).catch((e) => console.warn("[api]", e));
    },
    [],
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

  return (
    <div className="space-y-6">
      {/* Header */}
      <div className="flex items-center justify-between">
        <h2 className="text-2xl font-semibold text-stone-900 dark:text-stone-50">
          {intl.formatMessage({ id: 'reports.title' })}
        </h2>

        {/* Period selector */}
        <div className="flex gap-1 rounded-lg border border-stone-200 bg-stone-100 p-1 dark:border-stone-700 dark:bg-stone-800">
          {(['day', 'week', 'month'] as const).map((p) => (
            <button
              key={p}
              onClick={() => handlePeriodChange(p)}
              className={cn(
                'rounded-md px-4 py-1.5 text-sm font-medium transition-colors',
                period === p
                  ? 'bg-amber-500 text-white shadow-sm'
                  : 'text-stone-600 hover:text-stone-900 dark:text-stone-400 dark:hover:text-stone-200',
              )}
            >
              {intl.formatMessage({ id: `reports.period.${p}` })}
            </button>
          ))}
        </div>
      </div>

      {/* Summary cards */}
      {summary && (
        <div className="grid grid-cols-1 gap-4 sm:grid-cols-2 lg:grid-cols-4">
          <StatCard
            icon={MessageCircle}
            title={intl.formatMessage({ id: 'reports.conversations' })}
            value={summary.total_conversations.toLocaleString()}
            subtitle={`${summary.total_messages.toLocaleString()} messages`}
            color="bg-amber-500"
          />
          <div className="rounded-xl border border-stone-200 bg-white p-5 dark:border-stone-800 dark:bg-stone-900">
            <div className="flex items-center gap-3">
              <div className={cn('rounded-lg p-2.5', rateBg(summary.auto_reply_rate))}>
                <Zap className="h-5 w-5 text-white" />
              </div>
              <div>
                <p className="text-sm text-stone-500 dark:text-stone-400">
                  {intl.formatMessage({ id: 'reports.autoReplyRate' })}
                </p>
                <p className={cn('text-2xl font-semibold', rateColor(summary.auto_reply_rate))}>
                  {(summary.auto_reply_rate * 100).toFixed(1)}%
                </p>
              </div>
            </div>
          </div>
          <StatCard
            icon={Clock}
            title={intl.formatMessage({ id: 'reports.avgResponse' })}
            value={`${summary.avg_response_ms}ms`}
            subtitle={`P95: ${summary.p95_response_ms}ms`}
            color="bg-stone-500"
          />
          <StatCard
            icon={DollarSign}
            title={intl.formatMessage({ id: 'reports.savings' })}
            value={`$${(summary.estimated_savings_cents / 100).toFixed(2)}`}
            color="bg-emerald-500"
          />
        </div>
      )}

      {/* Zero-cost ratio */}
      {summary && (
        <div className="rounded-xl border border-stone-200 bg-white p-6 dark:border-stone-800 dark:bg-stone-900">
          <h3 className="mb-4 text-lg font-medium text-stone-900 dark:text-stone-50">
            {intl.formatMessage({ id: 'reports.zeroCostRatio' })}
          </h3>
          <div className="flex items-center gap-4">
            <div className="flex-1">
              <div className="h-6 w-full overflow-hidden rounded-full bg-stone-200 dark:bg-stone-700">
                <div
                  className="h-full rounded-full bg-gradient-to-r from-amber-400 to-amber-500 transition-all duration-500"
                  style={{ width: `${summary.zero_cost_ratio * 100}%` }}
                />
              </div>
            </div>
            <span className="min-w-[4rem] text-right text-2xl font-bold text-amber-600 dark:text-amber-400">
              {(summary.zero_cost_ratio * 100).toFixed(1)}%
            </span>
          </div>
          <p className="mt-2 text-sm text-stone-500 dark:text-stone-400">
            {intl.formatMessage({ id: 'reports.zeroCostDesc' })}
          </p>
        </div>
      )}

      {/* Conversation trend (CSS bar chart) */}
      <div className="rounded-xl border border-stone-200 bg-white p-6 dark:border-stone-800 dark:bg-stone-900">
        <h3 className="mb-4 text-lg font-medium text-stone-900 dark:text-stone-50">
          {intl.formatMessage({ id: 'reports.trend' })}
        </h3>
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
      </div>

      {/* Cost savings table */}
      <div className="rounded-xl border border-stone-200 bg-white p-6 dark:border-stone-800 dark:bg-stone-900">
        <h3 className="mb-4 text-lg font-medium text-stone-900 dark:text-stone-50">
          {intl.formatMessage({ id: 'reports.costComparison' })}
        </h3>
        <div className="overflow-x-auto">
          <table className="w-full text-left text-sm">
            <thead>
              <tr className="border-b border-stone-200 dark:border-stone-700">
                <th className="pb-3 font-medium text-stone-500 dark:text-stone-400">
                  {intl.formatMessage({ id: 'reports.period.month' })}
                </th>
                <th className="pb-3 text-right font-medium text-stone-500 dark:text-stone-400">
                  {intl.formatMessage({ id: 'reports.humanCost' })}
                </th>
                <th className="pb-3 text-right font-medium text-stone-500 dark:text-stone-400">
                  {intl.formatMessage({ id: 'reports.agentCost' })}
                </th>
                <th className="pb-3 text-right font-medium text-stone-500 dark:text-stone-400">
                  {intl.formatMessage({ id: 'reports.netSavings' })}
                </th>
              </tr>
            </thead>
            <tbody>
              {costs.map((row) => (
                <tr
                  key={row.month}
                  className="border-b border-stone-100 last:border-0 dark:border-stone-800"
                >
                  <td className="py-3 text-stone-900 dark:text-stone-100">{row.month}</td>
                  <td className="py-3 text-right text-stone-600 dark:text-stone-400">
                    ${(row.human_cost / 100).toFixed(2)}
                  </td>
                  <td className="py-3 text-right text-stone-600 dark:text-stone-400">
                    ${(row.agent_cost / 100).toFixed(2)}
                  </td>
                  <td className="py-3 text-right font-medium text-emerald-600 dark:text-emerald-400">
                    +${(row.savings / 100).toFixed(2)}
                  </td>
                </tr>
              ))}
              {costs.length === 0 && (
                <tr>
                  <td colSpan={4} className="py-8 text-center text-stone-400">
                    {intl.formatMessage({ id: 'common.noData' })}
                  </td>
                </tr>
              )}
            </tbody>
          </table>
        </div>
      </div>
    </div>
  );
}
