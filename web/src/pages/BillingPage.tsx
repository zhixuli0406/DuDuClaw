import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { useConnectionStore } from '@/stores/connection-store';
import { api, type BillingUsage, type BillingInvoice, type LicenseInfo } from '@/lib/api';
import { cn } from '@/lib/utils';
import {
  CreditCard,
  Crown,
  MessageCircle,
  Bot,
  Radio,
  Cpu,
  Check,
  Receipt,
} from 'lucide-react';

const PLAN_TIERS = ['community', 'pro', 'enterprise'] as const;

const PLAN_COLORS: Record<string, string> = {
  community: 'border-stone-200 dark:border-stone-700',
  pro: 'border-amber-300 dark:border-amber-700',
  enterprise: 'border-violet-300 dark:border-violet-700',
};

const PLAN_BADGE: Record<string, string> = {
  community: 'bg-stone-100 text-stone-700 dark:bg-stone-800 dark:text-stone-300',
  pro: 'bg-amber-100 text-amber-700 dark:bg-amber-900/30 dark:text-amber-400',
  enterprise: 'bg-violet-100 text-violet-700 dark:bg-violet-900/30 dark:text-violet-400',
};

const PLAN_PRICES: Record<string, string> = {
  community: 'NT$0',
  pro: 'NT$48,000',
  enterprise: 'NT$150,000',
};

const STATUS_COLORS: Record<string, string> = {
  paid: 'bg-emerald-100 text-emerald-700 dark:bg-emerald-900/30 dark:text-emerald-400',
  pending: 'bg-amber-100 text-amber-700 dark:bg-amber-900/30 dark:text-amber-400',
  failed: 'bg-rose-100 text-rose-700 dark:bg-rose-900/30 dark:text-rose-400',
};


function UsageMeter({
  label,
  icon: Icon,
  used,
  limit,
  unlimited,
}: {
  readonly label: string;
  readonly icon: React.ComponentType<{ className?: string }>;
  readonly used: number;
  readonly limit: number;
  readonly unlimited: boolean;
}) {
  const intl = useIntl();
  const pct = unlimited ? 0 : limit > 0 ? Math.min((used / limit) * 100, 100) : 0;
  const barColor =
    pct >= 90
      ? 'bg-rose-500'
      : pct >= 70
        ? 'bg-amber-500'
        : 'bg-emerald-500';

  return (
    <div className="space-y-2">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <Icon className="h-4 w-4 text-stone-500 dark:text-stone-400" />
          <span className="text-sm font-medium text-stone-700 dark:text-stone-300">{label}</span>
        </div>
        <span className="text-sm text-stone-500 dark:text-stone-400">
          {used.toLocaleString()}
          {' / '}
          {unlimited
            ? intl.formatMessage({ id: 'billing.unlimited' })
            : limit.toLocaleString()}
        </span>
      </div>
      <div className="h-2.5 w-full overflow-hidden rounded-full bg-stone-200 dark:bg-stone-700">
        <div
          className={cn('h-full rounded-full transition-all duration-500', barColor)}
          style={{ width: unlimited ? '0%' : `${pct}%` }}
        />
      </div>
    </div>
  );
}

export function BillingPage() {
  const intl = useIntl();
  const connectionState = useConnectionStore((s) => s.state);
  const [usage, setUsage] = useState<BillingUsage | null>(null);
  const [invoices, setInvoices] = useState<readonly BillingInvoice[]>([]);
  const [currentPlan, setCurrentPlan] = useState<string>('community');
  const [licenseInfo, setLicenseInfo] = useState<LicenseInfo | null>(null);

  useEffect(() => {
    if (connectionState !== 'authenticated') return;
    api.billing
      .usage()
      .then(setUsage)
      .catch(() => {});
    api.billing
      .history()
      .then((r) => setInvoices(r.invoices))
      .catch(() => {});
    api.license
      .status()
      .then((info) => {
        setLicenseInfo(info);
        setCurrentPlan(info.tier?.toLowerCase() ?? 'community');
      })
      .catch(() => {});
  }, [connectionState]);

  const renewalDate = licenseInfo?.expires_at ?? null;

  return (
    <div className="space-y-6">
      <h2 className="text-2xl font-semibold text-stone-900 dark:text-stone-50">
        {intl.formatMessage({ id: 'billing.title' })}
      </h2>

      {/* Current Plan Card */}
      <div
        className={cn(
          'rounded-xl border-2 bg-white p-6 dark:bg-stone-900',
          PLAN_COLORS[currentPlan] ?? PLAN_COLORS.community,
        )}
      >
        <div className="flex items-center gap-3 mb-4">
          <div className="rounded-lg bg-amber-500 p-2.5">
            <Crown className="h-5 w-5 text-white" />
          </div>
          <div>
            <h3 className="text-lg font-medium text-stone-900 dark:text-stone-50">
              {intl.formatMessage({ id: 'billing.currentPlan' })}
            </h3>
            <div className="flex items-center gap-2 mt-1">
              <span
                className={cn(
                  'inline-flex rounded-full px-3 py-0.5 text-sm font-semibold',
                  PLAN_BADGE[currentPlan] ?? PLAN_BADGE.community,
                )}
              >
                {intl.formatMessage({ id: `license.${currentPlan}` })}
              </span>
              <span className="text-sm text-stone-500 dark:text-stone-400">
                {renewalDate
                  ? `${intl.formatMessage({ id: 'billing.renewsOn' })} ${new Date(renewalDate).toLocaleDateString()}`
                  : ''}
              </span>
            </div>
          </div>
        </div>
      </div>

      {/* Usage Meters */}
      <div className="rounded-xl border border-stone-200 bg-white p-6 dark:border-stone-800 dark:bg-stone-900">
        <h3 className="mb-5 text-lg font-medium text-stone-900 dark:text-stone-50">
          {intl.formatMessage({ id: 'billing.usage' })}
        </h3>
        <div className="space-y-5">
          <UsageMeter
            label={intl.formatMessage({ id: 'billing.conversations' })}
            icon={MessageCircle}
            used={usage?.conversations.used ?? 0}
            limit={usage?.conversations.limit ?? 0}
            unlimited={(usage?.conversations.limit ?? 0) < 0}
          />
          <UsageMeter
            label={intl.formatMessage({ id: 'billing.agents' })}
            icon={Bot}
            used={usage?.agents.used ?? 0}
            limit={usage?.agents.limit ?? 0}
            unlimited={(usage?.agents.limit ?? 0) < 0}
          />
          <UsageMeter
            label={intl.formatMessage({ id: 'billing.channels' })}
            icon={Radio}
            used={usage?.channels.used ?? 0}
            limit={usage?.channels.limit ?? 0}
            unlimited={(usage?.channels.limit ?? 0) < 0}
          />
          <UsageMeter
            label={intl.formatMessage({ id: 'billing.inferenceHours' })}
            icon={Cpu}
            used={usage?.inference_hours.used ?? 0}
            limit={usage?.inference_hours.limit ?? 0}
            unlimited={(usage?.inference_hours.limit ?? 0) < 0}
          />
        </div>
      </div>

      {/* Billing History */}
      <div className="rounded-xl border border-stone-200 bg-white p-6 dark:border-stone-800 dark:bg-stone-900">
        <div className="flex items-center gap-3 mb-5">
          <Receipt className="h-5 w-5 text-amber-600 dark:text-amber-400" />
          <h3 className="text-lg font-medium text-stone-900 dark:text-stone-50">
            {intl.formatMessage({ id: 'billing.history' })}
          </h3>
        </div>
        <div className="overflow-x-auto">
          <table className="w-full text-left text-sm">
            <thead>
              <tr className="border-b border-stone-200 dark:border-stone-700">
                <th className="pb-3 font-medium text-stone-500 dark:text-stone-400">
                  {intl.formatMessage({ id: 'billing.date' })}
                </th>
                <th className="pb-3 font-medium text-stone-500 dark:text-stone-400">
                  {intl.formatMessage({ id: 'billing.description' })}
                </th>
                <th className="pb-3 text-right font-medium text-stone-500 dark:text-stone-400">
                  {intl.formatMessage({ id: 'billing.amount' })}
                </th>
                <th className="pb-3 text-right font-medium text-stone-500 dark:text-stone-400">
                  {intl.formatMessage({ id: 'billing.status' })}
                </th>
              </tr>
            </thead>
            <tbody>
              {invoices.map((inv) => (
                <tr
                  key={inv.id}
                  className="border-b border-stone-100 last:border-0 dark:border-stone-800"
                >
                  <td className="py-3 text-stone-900 dark:text-stone-100">
                    {new Date(inv.date).toLocaleDateString()}
                  </td>
                  <td className="py-3 text-stone-600 dark:text-stone-400">
                    {inv.description}
                  </td>
                  <td className="py-3 text-right font-medium text-stone-900 dark:text-stone-100">
                    ${(inv.amount_cents / 100).toFixed(2)}
                  </td>
                  <td className="py-3 text-right">
                    <span
                      className={cn(
                        'inline-flex rounded-full px-2.5 py-0.5 text-xs font-medium',
                        STATUS_COLORS[inv.status] ?? STATUS_COLORS.pending,
                      )}
                    >
                      {inv.status}
                    </span>
                  </td>
                </tr>
              ))}
              {invoices.length === 0 && (
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

      {/* Payment Method */}
      <div className="rounded-xl border border-stone-200 bg-white p-6 dark:border-stone-800 dark:bg-stone-900">
        <div className="flex items-center gap-3 mb-4">
          <CreditCard className="h-5 w-5 text-amber-600 dark:text-amber-400" />
          <h3 className="text-lg font-medium text-stone-900 dark:text-stone-50">
            {intl.formatMessage({ id: 'billing.paymentMethod' })}
          </h3>
        </div>
        <p className="mb-4 text-sm text-stone-500 dark:text-stone-400">
          {intl.formatMessage({ id: 'billing.noPaymentMethod' })}
        </p>
        <button className="inline-flex items-center gap-2 rounded-lg border border-stone-300 px-4 py-2 text-sm font-medium text-stone-700 transition-colors hover:bg-stone-50 dark:border-stone-600 dark:text-stone-300 dark:hover:bg-stone-800">
          <CreditCard className="h-4 w-4" />
          {intl.formatMessage({ id: 'billing.addPayment' })}
        </button>
      </div>

      {/* Plan Comparison */}
      <div className="rounded-xl border border-stone-200 bg-white p-6 dark:border-stone-800 dark:bg-stone-900">
        <h3 className="mb-5 text-lg font-medium text-stone-900 dark:text-stone-50">
          {intl.formatMessage({ id: 'billing.plans' })}
        </h3>
        <div className="grid gap-4 sm:grid-cols-3">
          {PLAN_TIERS.map((tier) => {
            const isCurrent = tier === currentPlan;
            return (
              <div
                key={tier}
                className={cn(
                  'relative rounded-xl border-2 p-5 transition-shadow',
                  isCurrent
                    ? cn(PLAN_COLORS[tier], 'bg-stone-50 shadow-md dark:bg-stone-800/50')
                    : 'border-stone-200 bg-white dark:border-stone-700 dark:bg-stone-900',
                )}
              >
                {isCurrent && (
                  <span className="absolute -top-3 right-4 inline-flex items-center gap-1 rounded-full bg-amber-500 px-2.5 py-0.5 text-xs font-medium text-white">
                    <Check className="h-3 w-3" />
                    {intl.formatMessage({ id: 'billing.currentLabel' })}
                  </span>
                )}
                <div className="mb-3">
                  <span
                    className={cn(
                      'inline-flex rounded-full px-3 py-0.5 text-sm font-semibold',
                      PLAN_BADGE[tier],
                    )}
                  >
                    {intl.formatMessage({ id: `license.${tier}` })}
                  </span>
                </div>
                <p className="text-3xl font-bold text-stone-900 dark:text-stone-50">
                  {PLAN_PRICES[tier]}
                  <span className="text-sm font-normal text-stone-500 dark:text-stone-400">
                    {tier !== 'community' ? ` ${intl.formatMessage({ id: 'billing.oneTime' })}` : ''}
                  </span>
                </p>
                <div className="mt-4">
                  {!isCurrent && (
                    <button
                      className={cn(
                        'w-full rounded-lg px-4 py-2 text-sm font-medium transition-colors',
                        tier === 'enterprise'
                          ? 'bg-violet-500 text-white hover:bg-violet-600'
                          : 'bg-amber-500 text-white hover:bg-amber-600',
                      )}
                    >
                      {intl.formatMessage({ id: 'billing.selectPlan' })}
                    </button>
                  )}
                </div>
              </div>
            );
          })}
        </div>
      </div>
    </div>
  );
}
