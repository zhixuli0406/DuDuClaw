import { useCallback, useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { api, type McpEventDescriptor, type McpEventSubscription, type McpRemoteServerStatus } from '@/lib/api';
import { toast } from '@/lib/toast';
import { Badge, Button, Empty, Input, Switch } from '@/components/mds';
import { BellRing, Loader2, RefreshCw, RotateCw, Search, Trash2 } from 'lucide-react';
import { ConfirmDialog } from '@/components/settings/controls';

const STATUS_CLASS: Record<string, string> = {
  active: 'bg-success/15 text-success',
  pending: 'bg-warning/15 text-warning',
  failed: 'bg-destructive/15 text-destructive',
  terminated: 'bg-destructive/15 text-destructive',
};

/**
 * MCP Events subscriptions (2026-10-08): a connected remote server delivers
 * events to this gateway's webhook, and autopilot rules or responsibilities
 * can react. Admin only, like the rest of the remote servers tab.
 */
export function McpEventsPanel({ servers }: { servers: ReadonlyArray<McpRemoteServerStatus> }) {
  const intl = useIntl();
  const [subs, setSubs] = useState<McpEventSubscription[]>([]);
  const [baseProblem, setBaseProblem] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [target, setTarget] = useState('');
  const [types, setTypes] = useState('');
  const [normal, setNormal] = useState(false);
  const [delivery, setDelivery] = useState<'webhook' | 'poll' | 'auto'>('webhook');
  const [argsText, setArgsText] = useState('');
  const [discovered, setDiscovered] = useState<McpEventDescriptor[] | null>(null);
  const [discovering, setDiscovering] = useState(false);
  const [busy, setBusy] = useState(false);
  const [confirm, setConfirm] = useState<McpEventSubscription | null>(null);

  const connected = servers.filter((s) => s.status === 'connected');

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const res = await api.mcp.eventsList();
      setSubs(res.subscriptions ?? []);
      setBaseProblem(res.public_base_url_set ? null : res.public_base_url_problem ?? '');
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const discover = async () => {
    const row = connected.find((s) => `${s.agent_id}/${s.server}` === target);
    if (!row) return;
    setDiscovering(true);
    try {
      const res = await api.mcp.eventsDiscover(row.agent_id, row.server);
      setDiscovered(res.events ?? []);
    } catch (e) {
      setDiscovered(null);
      toast.error(e instanceof Error ? e.message : String(e));
    } finally {
      setDiscovering(false);
    }
  };

  const addType = (name: string) => {
    const have = types.split(',').map((t) => t.trim()).filter(Boolean);
    if (!have.includes(name)) setTypes([...have, name].join(', '));
  };

  const subscribe = async () => {
    const row = connected.find((s) => `${s.agent_id}/${s.server}` === target);
    const names = types.split(',').map((t) => t.trim()).filter(Boolean);
    if (!row || names.length === 0) return;
    let args: Record<string, Record<string, unknown>> | undefined;
    if (argsText.trim() !== '') {
      try {
        const parsed: unknown = JSON.parse(argsText);
        if (parsed === null || typeof parsed !== 'object' || Array.isArray(parsed)) throw new Error('shape');
        args = parsed as Record<string, Record<string, unknown>>;
      } catch {
        toast.error(intl.formatMessage({ id: 'mcp.events.argumentsInvalid' }));
        return;
      }
    }
    setBusy(true);
    try {
      await api.mcp.eventsSubscribe({
        agent_id: row.agent_id,
        server: row.server,
        event_types: names,
        mode: normal ? 'normal' : 'explore',
        ...(delivery !== 'webhook' ? { delivery } : {}),
        ...(args ? { arguments: args } : {}),
      });
      toast.success(intl.formatMessage({ id: 'mcp.events.subscribed' }, { server: row.server }));
      setTypes('');
      setArgsText('');
      setNormal(false);
      await load();
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const rotate = async (s: McpEventSubscription) => {
    try {
      await api.mcp.eventsRotate(s.id);
      toast.success(intl.formatMessage({ id: 'mcp.events.rotated' }));
      await load();
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e));
    }
  };

  const remove = async (s: McpEventSubscription) => {
    try {
      await api.mcp.eventsUnsubscribe(s.id);
      await load();
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e));
    }
  };

  return (
    <section className="space-y-3" data-testid="mcp-events">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="min-w-0">
          <h3 className="flex items-center gap-1.5 text-sm font-medium text-foreground">
            <BellRing className="size-4 text-brand" />
            {intl.formatMessage({ id: 'mcp.events.title' })}
          </h3>
          <p className="text-xs text-muted-foreground">{intl.formatMessage({ id: 'mcp.events.desc' })}</p>
        </div>
        <Button variant="outline" size="sm" onClick={() => void load()} aria-label={intl.formatMessage({ id: 'common.refresh' })}>
          <RefreshCw />
        </Button>
      </div>

      {baseProblem !== null && delivery !== 'poll' && (
        <p className="rounded-lg bg-warning/10 px-3 py-2 text-xs text-warning" role="status">
          {intl.formatMessage({ id: 'mcp.events.baseUrlMissing' })}
        </p>
      )}

      <div className="flex flex-wrap items-end gap-2 rounded-xl border border-surface-border p-3">
        <label className="flex min-w-40 flex-col gap-1 text-xs text-muted-foreground">
          {intl.formatMessage({ id: 'mcp.events.server' })}
          <select
            className="h-9 rounded-md border border-input bg-background px-2 text-sm text-foreground"
            value={target}
            onChange={(e) => {
              setTarget(e.target.value);
              setDiscovered(null);
            }}
          >
            <option value="">—</option>
            {connected.map((s) => (
              <option key={`${s.agent_id}/${s.server}`} value={`${s.agent_id}/${s.server}`}>
                {s.server} · {s.agent_id}
              </option>
            ))}
          </select>
        </label>
        <label className="flex min-w-32 flex-col gap-1 text-xs text-muted-foreground">
          {intl.formatMessage({ id: 'mcp.events.delivery' })}
          <select
            className="h-9 rounded-md border border-input bg-background px-2 text-sm text-foreground"
            value={delivery}
            onChange={(e) => setDelivery(e.target.value as 'webhook' | 'poll' | 'auto')}
          >
            <option value="webhook">{intl.formatMessage({ id: 'mcp.events.delivery.webhook' })}</option>
            <option value="poll">{intl.formatMessage({ id: 'mcp.events.delivery.poll' })}</option>
            <option value="auto">{intl.formatMessage({ id: 'mcp.events.delivery.auto' })}</option>
          </select>
        </label>
        <label className="flex min-w-56 flex-1 flex-col gap-1 text-xs text-muted-foreground">
          {intl.formatMessage({ id: 'mcp.events.types' })}
          <Input value={types} onChange={(e) => setTypes(e.target.value)} placeholder="incident.created, email.received" />
        </label>
        <Button variant="outline" size="sm" onClick={() => void discover()} disabled={discovering || !target}>
          {discovering ? <Loader2 className="animate-spin" /> : <Search />}
          {intl.formatMessage({ id: 'mcp.events.discover' })}
        </Button>
        {discovered !== null && (
          <div className="basis-full space-y-1" data-testid="events-discovered">
            {discovered.length === 0 ? (
              <p className="text-xs text-muted-foreground">{intl.formatMessage({ id: 'mcp.events.discoverEmpty' })}</p>
            ) : (
              discovered.map((d) => (
                <button
                  type="button"
                  key={d.name}
                  className="flex w-full flex-wrap items-center gap-2 rounded-md px-2 py-1 text-left text-xs hover:bg-muted"
                  onClick={() => addType(d.name)}
                  title={d.description}
                >
                  <span className="font-mono text-foreground">{d.name}</span>
                  {d.delivery.map((m) => (
                    <Badge key={m} variant="secondary">{m}</Badge>
                  ))}
                  {d.input_schema?.properties && Object.keys(d.input_schema.properties).length > 0 && (
                    <span className="text-muted-foreground">
                      {intl.formatMessage({ id: 'mcp.events.argumentNames' }, { names: Object.keys(d.input_schema.properties).join(', ') })}
                    </span>
                  )}
                </button>
              ))
            )}
          </div>
        )}
        <label className="flex basis-full flex-col gap-1 text-xs text-muted-foreground">
          {intl.formatMessage({ id: 'mcp.events.arguments' })}
          <textarea
            className="min-h-16 rounded-md border border-input bg-background px-2 py-1 font-mono text-xs text-foreground"
            value={argsText}
            onChange={(e) => setArgsText(e.target.value)}
            placeholder='{"incident.created": {"severity": "P1"}}'
            spellCheck={false}
          />
        </label>
        <label className="flex items-center gap-2 text-xs text-muted-foreground">
          <Switch checked={normal} onCheckedChange={(v) => setNormal(Boolean(v))} aria-label={intl.formatMessage({ id: 'mcp.events.normalMode' })} />
          {intl.formatMessage({ id: 'mcp.events.normalMode' })}
        </label>
        <Button variant="brand" size="sm" onClick={() => void subscribe()} disabled={busy || !target || types.trim() === ''}>
          {busy ? <Loader2 className="animate-spin" /> : null}
          {intl.formatMessage({ id: 'mcp.events.subscribe' })}
        </Button>
        <p className="basis-full text-xs text-muted-foreground">
          {intl.formatMessage({ id: normal ? 'mcp.events.normalHelp' : 'mcp.events.exploreHelp' })}
        </p>
      </div>

      {loading && subs.length === 0 ? (
        <div className="flex justify-center py-6">
          <Loader2 className="size-5 animate-spin text-brand" />
        </div>
      ) : subs.length === 0 ? (
        <Empty icon={BellRing} title={intl.formatMessage({ id: 'mcp.events.empty' })} />
      ) : (
        <div className="overflow-hidden rounded-xl border border-surface-border divide-y divide-surface-border">
          {subs.map((s) => (
            <div key={s.id} className="flex flex-wrap items-center gap-3 px-4 py-3" data-testid="event-sub-row">
              <div className="min-w-0 flex-1">
                <div className="flex flex-wrap items-center gap-2">
                  <span className="text-sm font-medium text-foreground">{s.server}</span>
                  <Badge variant="secondary" className={STATUS_CLASS[s.status]}>
                    {intl.formatMessage({ id: `mcp.events.status.${s.status}` })}
                  </Badge>
                  <Badge variant="secondary">{intl.formatMessage({ id: `mcp.events.mode.${s.mode}` })}</Badge>
                  {s.delivery === 'poll' && <Badge variant="secondary">{intl.formatMessage({ id: 'mcp.events.delivery.poll' })}</Badge>}
                </div>
                <p className="truncate text-xs text-muted-foreground">
                  {s.agent_id} · {s.event_types.join(', ')} ·{' '}
                  {intl.formatMessage({ id: 'mcp.events.deliveries' }, { n: s.deliveries })}
                </p>
                {s.upstream.some((u) => u.truncated) && (
                  <p className="text-xs text-warning" data-testid="event-gap">
                    {intl.formatMessage({ id: 'mcp.events.gap' })}
                  </p>
                )}
                {s.upstream.some((u) => u.delivery_status && (!u.delivery_status.active || u.delivery_status.last_error)) && (
                  <p className="text-xs text-warning" data-testid="event-delivery-problem">
                    {intl.formatMessage(
                      { id: 'mcp.events.deliveryProblem' },
                      { reason: s.upstream.map((u) => u.delivery_status?.last_error).find(Boolean) ?? '—' },
                    )}
                  </p>
                )}
              </div>
              <div className="flex shrink-0 gap-1.5">
                {s.delivery !== 'poll' && (
                  <Button variant="ghost" size="sm" aria-label={intl.formatMessage({ id: 'mcp.events.rotate' })} onClick={() => void rotate(s)}>
                    <RotateCw />
                  </Button>
                )}
                <Button variant="ghost" size="sm" aria-label={intl.formatMessage({ id: 'mcp.events.unsubscribe' })} onClick={() => setConfirm(s)}>
                  <Trash2 />
                </Button>
              </div>
            </div>
          ))}
        </div>
      )}

      {confirm && (
        <ConfirmDialog
          open
          title={intl.formatMessage({ id: 'mcp.events.unsubscribe' })}
          message={intl.formatMessage({ id: 'mcp.events.unsubscribeConfirm' }, { server: confirm.server })}
          confirmLabel={intl.formatMessage({ id: 'mcp.events.unsubscribe' })}
          onConfirm={() => {
            const c = confirm;
            setConfirm(null);
            void remove(c);
          }}
          onClose={() => setConfirm(null)}
        />
      )}
    </section>
  );
}
