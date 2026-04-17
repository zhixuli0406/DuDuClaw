import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { useConnectionStore } from '@/stores/connection-store';
import { api, type BillingUsage } from '@/lib/api';
import { cn } from '@/lib/utils';
import {
  MessageCircle,
  Bot,
  Radio,
  Cpu,
} from 'lucide-react';

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

  useEffect(() => {
    if (connectionState !== 'authenticated') return;
    api.billing
      .usage()
      .then(setUsage)
      .catch((e) => console.warn('[api]', e));
  }, [connectionState]);

  return (
    <div className="space-y-6">
      <h2 className="text-2xl font-semibold text-stone-900 dark:text-stone-50">
        {intl.formatMessage({ id: 'billing.title' })}
      </h2>

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
    </div>
  );
}
