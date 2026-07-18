import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { useConnectionStore } from '@/stores/connection-store';
import { api, type BillingUsage, type BudgetIncident, type BudgetByAgent } from '@/lib/api';
import { cn } from '@/lib/utils';
import { toast, formatError } from '@/lib/toast';
import { Wallet, GaugeCircle } from 'lucide-react';
import {
  Card,
  CardHeader,
  CardTitle,
  CardContent,
  Badge,
  Empty,
  Segmented,
  Table,
  TableHeader,
  TableBody,
  TableRow,
  TableHead,
  TableCell,
  ActorAvatar,
} from '@/components/mds';

/** A single usage KPI tile: label, big value, `used / limit` sub, progress bar. */
function UsageTile({
  label,
  used,
  limit,
}: {
  readonly label: string;
  readonly used: number;
  readonly limit: number;
}) {
  const intl = useIntl();
  const unlimited = limit < 0;
  const pct = unlimited ? 0 : limit > 0 ? Math.min((used / limit) * 100, 100) : 0;
  const barColor = pct >= 90 ? 'bg-destructive' : pct >= 70 ? 'bg-warning' : 'bg-success';

  return (
    <div className="space-y-2 p-4">
      <p className="text-sm text-muted-foreground">{label}</p>
      <p className="text-2xl font-semibold tabular-nums">{used.toLocaleString()}</p>
      <p className="font-mono text-xs tabular-nums text-muted-foreground">
        {used.toLocaleString()}
        {' / '}
        {unlimited ? intl.formatMessage({ id: 'billing.unlimited' }) : limit.toLocaleString()}
      </p>
      <div className="h-2 overflow-hidden rounded-full bg-muted">
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
  const [filter, setFilter] = useState<'all' | 'over'>('all');

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
  const isOver = (inc: BudgetIncident) => inc.cap_cents > 0 && inc.spent_cents > inc.cap_cents;
  const filtered = filter === 'over' ? incidents.filter(isOver) : incidents;

  return (
    <section className="space-y-4">
      <div className="flex items-center gap-2">
        <GaugeCircle className="size-5 text-muted-foreground" />
        <h2 className="text-base font-medium">{intl.formatMessage({ id: 'billing.budgetConsole' })}</h2>
      </div>

      {loading ? (
        <p className="py-6 text-center text-sm text-muted-foreground">
          {intl.formatMessage({ id: 'common.loading' })}
        </p>
      ) : incidents.length === 0 && openAgents.length === 0 ? (
        <Empty
          icon={GaugeCircle}
          title={intl.formatMessage({ id: 'billing.budgetConsole.empty' })}
          description={intl.formatMessage({ id: 'billing.budgetConsole.emptyHint' })}
        />
      ) : (
        <div className="space-y-5">
          {/* Per-agent open-event summary */}
          {openAgents.length > 0 && (
            <div className="space-y-2">
              <p className="text-xs font-medium text-muted-foreground">
                {intl.formatMessage({ id: 'billing.budgetConsole.openByAgent' })}
              </p>
              <div className="flex flex-wrap gap-2">
                {openAgents.map((a) => (
                  <Badge key={a.agent_id} variant="secondary" className="bg-warning/15 text-warning">
                    <ActorAvatar actorType="agent" name={a.agent_id} size="xs" />
                    {a.agent_id}
                    <span className="ml-1 font-mono font-semibold">{a.open_events}</span>
                  </Badge>
                ))}
              </div>
            </div>
          )}

          {/* Recent incidents */}
          {incidents.length > 0 && (
            <div className="space-y-2">
              <div className="flex flex-wrap items-center justify-between gap-2">
                <p className="text-xs font-medium text-muted-foreground">
                  {intl.formatMessage({ id: 'billing.budgetConsole.recent' })}
                </p>
                <Segmented
                  value={filter}
                  onValueChange={setFilter}
                  aria-label={intl.formatMessage({ id: 'billing.budgetConsole.recent' })}
                  options={[
                    { value: 'all', label: intl.formatMessage({ id: 'billing.incidents.filter.all' }) },
                    { value: 'over', label: intl.formatMessage({ id: 'billing.incidents.filter.over' }) },
                  ]}
                />
              </div>
              <div className="overflow-hidden rounded-xl border border-surface-border">
                <Table>
                  <TableHeader>
                    <TableRow>
                      <TableHead>{intl.formatMessage({ id: 'billing.col.agent' })}</TableHead>
                      <TableHead>{intl.formatMessage({ id: 'billing.col.event' })}</TableHead>
                      <TableHead>{intl.formatMessage({ id: 'billing.col.scope' })}</TableHead>
                      <TableHead>{intl.formatMessage({ id: 'billing.col.spent' })}</TableHead>
                      <TableHead>{intl.formatMessage({ id: 'billing.col.time' })}</TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {filtered.map((inc, i) => {
                      const over = isOver(inc);
                      return (
                        <TableRow key={`${inc.ts}-${inc.agent_id}-${i}`}>
                          <TableCell>
                            <span className="flex items-center gap-2">
                              <ActorAvatar actorType="agent" name={inc.agent_id} size="xs" />
                              <span className="font-medium">{inc.agent_id}</span>
                            </span>
                          </TableCell>
                          <TableCell>
                            <Badge variant="outline">{inc.event}</Badge>
                          </TableCell>
                          <TableCell className="text-xs text-muted-foreground">{inc.scope}</TableCell>
                          <TableCell className={cn('font-mono text-xs', over && 'text-destructive')}>
                            {fmtCents(inc.spent_cents)}
                            <span className="text-muted-foreground"> / {fmtCents(inc.cap_cents)}</span>
                          </TableCell>
                          <TableCell className="font-mono text-xs text-muted-foreground">
                            {new Date(inc.ts).toLocaleString('zh-TW', {
                              month: 'short',
                              day: 'numeric',
                              hour: '2-digit',
                              minute: '2-digit',
                            })}
                          </TableCell>
                        </TableRow>
                      );
                    })}
                  </TableBody>
                </Table>
              </div>
            </div>
          )}
        </div>
      )}
    </section>
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
    <div className="mx-auto w-full max-w-6xl space-y-5">
      {/* Header */}
      <div className="flex min-w-0 items-center gap-2">
        <Wallet className="size-5 text-muted-foreground" />
        <div>
          <h1 className="text-base font-medium">{intl.formatMessage({ id: 'billing.title' })}</h1>
          <p className="text-sm text-muted-foreground">{intl.formatMessage({ id: 'billing.subtitle' })}</p>
        </div>
      </div>

      {/* Usage KPI group */}
      <Card>
        <CardHeader>
          <CardTitle>{intl.formatMessage({ id: 'billing.usage' })}</CardTitle>
        </CardHeader>
        <CardContent className="p-0">
          <div className="grid grid-cols-2 divide-x divide-y divide-surface-border border-t border-surface-border lg:grid-cols-4">
            <UsageTile
              label={intl.formatMessage({ id: 'billing.conversations' })}
              used={usage?.conversations?.used ?? 0}
              limit={usage?.conversations?.limit ?? 0}
            />
            <UsageTile
              label={intl.formatMessage({ id: 'billing.agents' })}
              used={usage?.agents?.used ?? 0}
              limit={usage?.agents?.limit ?? 0}
            />
            <UsageTile
              label={intl.formatMessage({ id: 'billing.channels' })}
              used={usage?.channels?.used ?? 0}
              limit={usage?.channels?.limit ?? 0}
            />
            <UsageTile
              label={intl.formatMessage({ id: 'billing.inferenceHours' })}
              used={usage?.inference_hours?.used ?? 0}
              limit={usage?.inference_hours?.limit ?? 0}
            />
          </div>
        </CardContent>
      </Card>

      {/* Budget console — open budget events + recent incidents (WP14-T14.6) */}
      <BudgetConsole />
    </div>
  );
}
