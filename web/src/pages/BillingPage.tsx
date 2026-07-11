import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { useConnectionStore } from '@/stores/connection-store';
import { api, type BillingUsage, type BudgetIncident, type BudgetByAgent } from '@/lib/api';
import { cn } from '@/lib/utils';
import { toast, formatError } from '@/lib/toast';
import {
  MessageCircle,
  Bot,
  Radio,
  Cpu,
  Wallet,
  GaugeCircle,
  AlertTriangle,
  Clock,
} from 'lucide-react';
import { Page, PageHeader, Card, Badge, EmptyState, Mono, CharacterAvatar } from '@/components/ui';

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
        <Mono className="text-sm text-stone-500 dark:text-stone-400">
          {used.toLocaleString()}
          {' / '}
          {unlimited
            ? intl.formatMessage({ id: 'billing.unlimited' })
            : limit.toLocaleString()}
        </Mono>
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

/** Format a cent amount as a USD string (e.g. 1234 → $12.34). */
function fmtCents(cents: number): string {
  return `$${(cents / 100).toFixed(2)}`;
}

// ── Budget console (WP14-T14.6) — open-events per AI staff + recent incidents ──

function BudgetConsole() {
  const intl = useIntl();
  const [incidents, setIncidents] = useState<BudgetIncident[]>([]);
  const [byAgent, setByAgent] = useState<BudgetByAgent[]>([]);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    setLoading(true);
    api.budget
      .incidents(50)
      .then((res) => {
        setIncidents(res?.incidents ?? []);
        setByAgent(res?.by_agent ?? []);
      })
      .catch((e) => {
        console.warn('[api]', e);
        toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
      })
      .finally(() => setLoading(false));
  }, [intl]);

  const openAgents = byAgent.filter((a) => a.open_events > 0);

  return (
    <Card
      title={
        <span className="flex items-center gap-2">
          <GaugeCircle className="h-4 w-4 text-amber-500" />
          {intl.formatMessage({ id: 'billing.budgetConsole' })}
        </span>
      }
    >
      {loading ? (
        <div className="py-8 text-center text-sm text-stone-400">
          {intl.formatMessage({ id: 'common.loading' })}
        </div>
      ) : incidents.length === 0 && openAgents.length === 0 ? (
        <EmptyState
          icon={GaugeCircle}
          dudu="idle"
          title={intl.formatMessage({ id: 'billing.budgetConsole.empty' })}
          hint={intl.formatMessage({ id: 'billing.budgetConsole.emptyHint' })}
        />
      ) : (
        <div className="space-y-5">
          {/* Per-agent open-event summary */}
          {openAgents.length > 0 && (
            <div>
              <p className="mb-2 text-xs font-medium text-stone-500 dark:text-stone-400">
                {intl.formatMessage({ id: 'billing.budgetConsole.openByAgent' })}
              </p>
              <div className="flex flex-wrap gap-2">
                {openAgents.map((a) => (
                  <Badge key={a.agent_id} tone="warning">
                    <CharacterAvatar agentId={a.agent_id} size={16} />
                    {a.agent_id}
                    <Mono className="ml-1 font-semibold text-inherit">{a.open_events}</Mono>
                  </Badge>
                ))}
              </div>
            </div>
          )}

          {/* Recent incidents */}
          {incidents.length > 0 && (
            <div>
              <p className="mb-2 text-xs font-medium text-stone-500 dark:text-stone-400">
                {intl.formatMessage({ id: 'billing.budgetConsole.recent' })}
              </p>
              <ul className="space-y-1.5">
                {incidents.map((inc, i) => {
                  const over = inc.cap_cents > 0 && inc.spent_cents > inc.cap_cents;
                  return (
                    <li
                      key={`${inc.ts}-${inc.agent_id}-${i}`}
                      className="flex flex-wrap items-center gap-x-3 gap-y-1 rounded-control border border-[var(--panel-border)] bg-[var(--panel-fill)] px-3 py-2 text-sm"
                    >
                      <AlertTriangle
                        className={cn('h-3.5 w-3.5 shrink-0', over ? 'text-rose-500' : 'text-amber-500')}
                      />
                      <CharacterAvatar agentId={inc.agent_id} size={24} />
                      <span className="font-medium text-stone-800 dark:text-stone-100">
                        {inc.agent_id}
                      </span>
                      <Badge tone="neutral">{inc.event}</Badge>
                      <span className="text-xs text-stone-500 dark:text-stone-400">{inc.scope}</span>
                      <Mono className="text-stone-600 dark:text-stone-300">
                        {fmtCents(inc.spent_cents)}
                        <span className="text-stone-400"> / {fmtCents(inc.cap_cents)}</span>
                      </Mono>
                      <Mono className="ml-auto flex items-center gap-1 text-xs text-stone-400">
                        <Clock className="h-3 w-3" />
                        {new Date(inc.ts).toLocaleString('zh-TW', {
                          month: 'short',
                          day: 'numeric',
                          hour: '2-digit',
                          minute: '2-digit',
                        })}
                      </Mono>
                    </li>
                  );
                })}
              </ul>
            </div>
          )}
        </div>
      )}
    </Card>
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
      .catch((e) => {
        console.warn('[api]', e);
        toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
      });
  }, [connectionState, intl]);

  return (
    <Page>
      <PageHeader
        icon={Wallet}
        title={intl.formatMessage({ id: 'billing.title' })}
      />

      {/* Usage Meters */}
      <Card title={intl.formatMessage({ id: 'billing.usage' })}>
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
      </Card>

      {/* Budget console — open budget events + recent incidents (WP14-T14.6) */}
      <BudgetConsole />
    </Page>
  );
}
