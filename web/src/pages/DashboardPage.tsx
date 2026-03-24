import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { useAgentsStore } from '@/stores/agents-store';
import { useSystemStore } from '@/stores/system-store';
import { api, type BudgetSummary, type DoctorCheck } from '@/lib/api';
import { Bot, Radio, Wallet, HeartPulse } from 'lucide-react';
import { cn } from '@/lib/utils';

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
  subtitle: string;
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
          <p className="text-xs text-stone-400 dark:text-stone-500">
            {subtitle}
          </p>
        </div>
      </div>
    </div>
  );
}

export function DashboardPage() {
  const intl = useIntl();
  const { agents, fetchAgents } = useAgentsStore();
  const { status, fetchStatus } = useSystemStore();
  const [budget, setBudget] = useState<BudgetSummary | null>(null);
  const [doctor, setDoctor] = useState<{ checks: DoctorCheck[]; summary: { pass: number; warn: number; fail: number } } | null>(null);

  useEffect(() => {
    // Initial fetch
    fetchAgents();
    fetchStatus();
    api.accounts.budgetSummary().then(setBudget).catch(() => {});
    api.system.doctor().then(setDoctor).catch(() => {});

    // Poll every 30s to keep stats fresh
    const interval = setInterval(() => {
      fetchAgents();
      fetchStatus();
      api.accounts.budgetSummary().then(setBudget).catch(() => {});
    }, 30_000);
    return () => clearInterval(interval);
  }, [fetchAgents, fetchStatus]);

  const activeAgents = agents.filter((a) => a.status === 'active').length;
  const totalBudget = budget?.total_budget_cents ?? 0;
  const totalSpent = budget?.total_spent_cents ?? 0;

  const checks = doctor?.checks ?? [];
  const summary = doctor?.summary ?? { pass: 0, warn: 0, fail: 0 };
  const healthValue = doctor
    ? `${summary.pass}/${checks.length}`
    : '—';
  const healthSubtitle = doctor
    ? summary.fail > 0
      ? `${summary.fail} 項失敗`
      : summary.warn > 0
        ? `${summary.warn} 項警告`
        : intl.formatMessage({ id: 'dashboard.health.allPassed' })
    : intl.formatMessage({ id: 'common.loading' });

  return (
    <div className="space-y-6">
      <h2 className="text-2xl font-semibold text-stone-900 dark:text-stone-50">
        {intl.formatMessage({ id: 'nav.dashboard' })}
      </h2>

      {/* Stat Cards */}
      <div className="grid grid-cols-1 gap-4 sm:grid-cols-2 lg:grid-cols-4">
        <StatCard
          icon={Bot}
          title={intl.formatMessage({ id: 'dashboard.agents.title' })}
          value={agents.length}
          subtitle={intl.formatMessage({ id: 'dashboard.agents.active' }, { count: activeAgents })}
          color="bg-amber-500"
        />
        <StatCard
          icon={Radio}
          title={intl.formatMessage({ id: 'dashboard.channels.title' })}
          value={status?.channels_connected ?? 0}
          subtitle={intl.formatMessage({ id: 'dashboard.channels.connected' }, { count: status?.channels_connected ?? 0 })}
          color="bg-emerald-500"
        />
        <StatCard
          icon={Wallet}
          title={intl.formatMessage({ id: 'dashboard.budget.title' })}
          value={`$${(totalSpent / 100).toFixed(2)}`}
          subtitle={`/ $${(totalBudget / 100).toFixed(2)}`}
          color="bg-orange-400"
        />
        <StatCard
          icon={HeartPulse}
          title={intl.formatMessage({ id: 'dashboard.health.title' })}
          value={healthValue}
          subtitle={healthSubtitle}
          color={summary.fail ? 'bg-rose-500' : summary.warn ? 'bg-amber-500' : 'bg-emerald-500'}
        />
      </div>

      {/* Doctor Checks */}
      {checks.length > 0 && (
        <div className="rounded-xl border border-stone-200 bg-white p-6 dark:border-stone-800 dark:bg-stone-900">
          <h3 className="mb-4 text-lg font-medium text-stone-900 dark:text-stone-50">
            {intl.formatMessage({ id: 'dashboard.health.title' })}
          </h3>
          <div className="space-y-2">
            {checks.map((check) => (
              <div key={check.name} className="flex items-center justify-between rounded-lg bg-stone-50 px-4 py-2.5 dark:bg-stone-800/50">
                <span className="text-sm text-stone-700 dark:text-stone-300">{check.name}</span>
                <div className="flex items-center gap-2">
                  <span className="text-xs text-stone-500 dark:text-stone-400">{check.message}</span>
                  <span className={cn(
                    'inline-block h-2 w-2 rounded-full',
                    check.status === 'pass' ? 'bg-emerald-500' : check.status === 'warn' ? 'bg-amber-500' : 'bg-rose-500'
                  )} />
                </div>
              </div>
            ))}
          </div>
        </div>
      )}

      {/* Activity Feed */}
      <div className="rounded-xl border border-stone-200 bg-white p-6 dark:border-stone-800 dark:bg-stone-900">
        <h3 className="mb-4 text-lg font-medium text-stone-900 dark:text-stone-50">
          {intl.formatMessage({ id: 'dashboard.activity.title' })}
        </h3>
        <div className="flex items-center justify-center py-12 text-stone-400 dark:text-stone-500">
          <p>{intl.formatMessage({ id: 'common.noData' })}</p>
        </div>
      </div>
    </div>
  );
}
