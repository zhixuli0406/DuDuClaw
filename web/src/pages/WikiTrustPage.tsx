import { useCallback, useEffect, useMemo, useState } from 'react';
import { useIntl } from 'react-intl';
import {
  ShieldIcon,
  ShieldAlertIcon,
  LockIcon,
  RotateCcwIcon,
  EyeIcon,
  EyeOffIcon,
  ActivityIcon,
} from 'lucide-react';
import { cn } from '@/lib/utils';
import { api, type WikiTrustHistoryRow, type WikiTrustRow } from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import { timeAgo } from '@/lib/format';
import {
  CollectionPageHeader,
  CollectionPageState,
  Card,
  CardContent,
  Button,
  Badge,
  Input,
  Checkbox,
  Select,
  SelectTrigger,
  SelectValue,
  SelectContent,
  SelectItem,
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogFooter,
  DialogClose,
  ListGridContainer,
  ListGridHeader,
  ListGridHeaderCell,
  ListGridRow,
  ListGridCell,
} from '@/components/mds';

const TRUST_COLUMNS = 'minmax(9rem,1fr) 9rem 3.5rem 3.5rem 3.5rem auto auto 4.5rem';

/**
 * WikiTrustPage — prediction-error-driven wiki self-cleaning audit, re-skinned
 * onto MDS (spec §4/§5.5). A CollectionPageHeader + KPI summary tiles + agent /
 * max-trust filters; trust rows render in a ListGrid (page path, a mini trust
 * bar + mono score, citations, error/success signals, flags, updated) and open
 * an MDS Dialog with the trust-history table and the manual-override form. Data
 * flow (audit / history / override RPCs) is unchanged.
 */
export function WikiTrustPage() {
  const intl = useIntl();
  const [agents, setAgents] = useState<ReadonlyArray<{ name: string; display_name: string }>>([]);
  const [selectedAgent, setSelectedAgent] = useState('');
  const [maxTrust, setMaxTrust] = useState(0.5);
  const [rows, setRows] = useState<ReadonlyArray<WikiTrustRow>>([]);
  const [loading, setLoading] = useState(false);
  const [available, setAvailable] = useState<boolean | null>(null);
  const [note, setNote] = useState<string | undefined>(undefined);
  const [activeRow, setActiveRow] = useState<WikiTrustRow | null>(null);

  useEffect(() => {
    api.agents.list().then((res) => {
      const list = res?.agents ?? [];
      setAgents(list);
      if (list.length > 0) setSelectedAgent((prev) => prev || list[0].name);
    }).catch(() => { /* ignore */ });
  }, []);

  const refresh = useCallback(async (agentId: string, max: number) => {
    if (!agentId) return;
    setLoading(true);
    try {
      const res = await api.wiki.trustAudit(agentId, max, 200);
      setRows(res.rows ?? []);
      setAvailable(res.available);
      setNote(res.note);
    } catch (err) {
      toast.error(formatError(err));
      setRows([]);
      setAvailable(false);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    if (selectedAgent) refresh(selectedAgent, maxTrust);
  }, [selectedAgent, maxTrust, refresh]);

  const summary = useMemo(() => {
    const archived = rows.filter((r) => r.do_not_inject).length;
    const locked = rows.filter((r) => r.locked).length;
    const totalErr = rows.reduce((acc, r) => acc + r.error_signal_count, 0);
    const totalOk = rows.reduce((acc, r) => acc + r.success_signal_count, 0);
    return { archived, locked, totalErr, totalOk };
  }, [rows]);

  return (
    <div className="-mx-4 -mt-4 flex flex-1 flex-col md:-mx-6 md:-mt-6">
      <CollectionPageHeader
        hideTrigger
        icon={ShieldIcon}
        title={intl.formatMessage({ id: 'wikiTrust.title' })}
        count={rows.length}
        description={intl.formatMessage({ id: 'nav.wikiTrust.desc' })}
        action={
          <Button variant="outline" size="sm" onClick={() => refresh(selectedAgent, maxTrust)} disabled={loading || !selectedAgent}>
            {intl.formatMessage({ id: loading ? 'common.loading' : 'common.refresh' })}
          </Button>
        }
      />

      {/* Filter control row. */}
      <div className="flex h-12 shrink-0 items-center gap-2 overflow-x-auto border-b border-surface-border px-4">
        <Select value={selectedAgent} onValueChange={(v) => setSelectedAgent(String(v))}>
          <SelectTrigger className="w-52 shrink-0">
            <SelectValue>
              {agents.find((a) => a.name === selectedAgent)?.display_name || selectedAgent || intl.formatMessage({ id: 'wikiTrust.agent' })}
            </SelectValue>
          </SelectTrigger>
          <SelectContent>
            {agents.map((a) => (
              <SelectItem key={a.name} value={a.name}>{a.display_name} ({a.name})</SelectItem>
            ))}
          </SelectContent>
        </Select>
        <Select value={String(maxTrust)} onValueChange={(v) => setMaxTrust(Number(v))}>
          <SelectTrigger className="w-44 shrink-0">
            <SelectValue>{intl.formatMessage({ id: 'wikiTrust.maxTrust' })}: {maxTrust.toFixed(2)}</SelectValue>
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="0.3">≤ 0.30</SelectItem>
            <SelectItem value="0.5">≤ 0.50</SelectItem>
            <SelectItem value="0.7">≤ 0.70</SelectItem>
            <SelectItem value="1">1.00 ({intl.formatMessage({ id: 'common.all', defaultMessage: 'All' })})</SelectItem>
          </SelectContent>
        </Select>
      </div>

      <div className="flex flex-1 flex-col gap-4 p-4 md:p-6">
        {/* Summary tiles. */}
        <div className="grid grid-cols-2 gap-3 lg:grid-cols-3">
          <KpiTile icon={ShieldAlertIcon} tone="destructive" label={intl.formatMessage({ id: 'wikiTrust.archived' })} value={String(summary.archived)} />
          <KpiTile icon={LockIcon} tone="success" label={intl.formatMessage({ id: 'wikiTrust.locked' })} value={String(summary.locked)} />
          <KpiTile icon={ActivityIcon} label={intl.formatMessage({ id: 'wikiTrust.signals' })} value={`${summary.totalErr} / ${summary.totalOk}`} className="col-span-2 lg:col-span-1" />
        </div>

        {available === false && note && (
          <Card data-size="sm" className="border-warning/40">
            <CardContent className="flex items-start gap-2 text-sm text-warning">
              <ShieldAlertIcon className="mt-0.5 size-4 shrink-0" />
              {note}
            </CardContent>
          </Card>
        )}

        {/* Trust list. */}
        {loading ? (
          <CollectionPageState state="loading" />
        ) : rows.length === 0 ? (
          <CollectionPageState state="empty" icon={ShieldIcon} title={intl.formatMessage({ id: 'wikiTrust.empty' })} />
        ) : (
          <div className="overflow-hidden rounded-xl border border-surface-border">
            <ListGridContainer
              columns={TRUST_COLUMNS}
              className="!h-auto"
              header={
                <ListGridHeader>
                  <ListGridHeaderCell>{intl.formatMessage({ id: 'wikiTrust.col.page' })}</ListGridHeaderCell>
                  <ListGridHeaderCell>{intl.formatMessage({ id: 'wikiTrust.col.trust' })}</ListGridHeaderCell>
                  <ListGridHeaderCell>{intl.formatMessage({ id: 'wikiTrust.col.cite' })}</ListGridHeaderCell>
                  <ListGridHeaderCell hideBelow>{intl.formatMessage({ id: 'wikiTrust.col.err' })}</ListGridHeaderCell>
                  <ListGridHeaderCell hideBelow>{intl.formatMessage({ id: 'wikiTrust.col.ok' })}</ListGridHeaderCell>
                  <ListGridHeaderCell hideBelow>{intl.formatMessage({ id: 'wikiTrust.col.flags' })}</ListGridHeaderCell>
                  <ListGridHeaderCell hideBelow>{intl.formatMessage({ id: 'sharedWiki.stats.lastUpdated' })}</ListGridHeaderCell>
                  <ListGridHeaderCell aria-hidden />
                </ListGridHeader>
              }
            >
              {rows.map((r) => (
                <ListGridRow
                  key={`${r.page_path}-${r.agent_id}`}
                  selected={r.do_not_inject}
                  onClick={() => setActiveRow(r)}
                >
                  <ListGridCell>
                    <span className="truncate font-mono text-xs text-foreground" title={r.page_path}>{r.page_path}</span>
                  </ListGridCell>
                  <ListGridCell className="gap-2">
                    <div className="h-1.5 w-16 overflow-hidden rounded-full bg-muted">
                      <div className="h-full rounded-full bg-chart-1" style={{ width: `${Math.max(0, Math.min(100, r.trust * 100))}%` }} />
                    </div>
                    <span className="font-mono text-xs tabular-nums text-foreground">{r.trust.toFixed(3)}</span>
                  </ListGridCell>
                  <ListGridCell className="font-mono text-xs tabular-nums text-muted-foreground">{r.citation_count}</ListGridCell>
                  <ListGridCell hideBelow className={cn('font-mono text-xs tabular-nums', r.error_signal_count > 0 ? 'text-destructive' : 'text-muted-foreground/50')}>
                    {r.error_signal_count}
                  </ListGridCell>
                  <ListGridCell hideBelow className={cn('font-mono text-xs tabular-nums', r.success_signal_count > 0 ? 'text-success' : 'text-muted-foreground/50')}>
                    {r.success_signal_count}
                  </ListGridCell>
                  <ListGridCell hideBelow className="gap-1">
                    {r.do_not_inject && (
                      <Badge variant="destructive"><EyeOffIcon className="size-3" />{intl.formatMessage({ id: 'wikiTrust.flag.archived' })}</Badge>
                    )}
                    {r.locked && (
                      <Badge variant="secondary" className="bg-success/15 text-success"><LockIcon className="size-3" />{intl.formatMessage({ id: 'wikiTrust.flag.locked' })}</Badge>
                    )}
                  </ListGridCell>
                  <ListGridCell hideBelow className="font-mono text-xs tabular-nums text-muted-foreground">
                    {r.updated_at ? timeAgo(r.updated_at) : '—'}
                  </ListGridCell>
                  <ListGridCell className="justify-end">
                    <Button
                      variant="ghost"
                      size="sm"
                      data-stop-row-nav
                      onClick={(e) => { e.stopPropagation(); setActiveRow(r); }}
                    >
                      {intl.formatMessage({ id: 'wikiTrust.action.inspect' })}
                    </Button>
                  </ListGridCell>
                </ListGridRow>
              ))}
            </ListGridContainer>
          </div>
        )}
      </div>

      {activeRow && (
        <TrustDetailDialog
          row={activeRow}
          agentId={selectedAgent}
          onClose={() => setActiveRow(null)}
          onChanged={() => { setActiveRow(null); refresh(selectedAgent, maxTrust); }}
        />
      )}
    </div>
  );
}

function KpiTile({
  icon: Icon,
  label,
  value,
  tone = 'default',
  className,
}: {
  icon: React.ComponentType<{ className?: string }>;
  label: string;
  value: string;
  tone?: 'default' | 'success' | 'destructive';
  className?: string;
}) {
  const toneClass = tone === 'success' ? 'text-success' : tone === 'destructive' ? 'text-destructive' : 'text-muted-foreground';
  return (
    <div className={cn('rounded-lg border border-surface-border bg-card p-4', className)}>
      <div className="flex items-center gap-1.5 text-xs text-muted-foreground">
        <Icon className={cn('size-3.5', toneClass)} />
        {label}
      </div>
      <p className={cn('mt-1 text-2xl font-medium tabular-nums', tone === 'default' ? 'text-foreground' : toneClass)}>{value}</p>
    </div>
  );
}

// ── Detail dialog (history + override) ──────────────────────

function TrustDetailDialog({
  row,
  agentId,
  onClose,
  onChanged,
}: {
  row: WikiTrustRow;
  agentId: string;
  onClose: () => void;
  onChanged: () => void;
}) {
  const intl = useIntl();
  const [history, setHistory] = useState<ReadonlyArray<WikiTrustHistoryRow>>([]);
  const [loadingHistory, setLoadingHistory] = useState(true);
  const [overrideOpen, setOverrideOpen] = useState(false);
  const [trust, setTrust] = useState(row.trust);
  const [lock, setLock] = useState(row.locked);
  const [doNotInject, setDoNotInject] = useState(row.do_not_inject);
  const [reason, setReason] = useState('');
  const [submitting, setSubmitting] = useState(false);

  useEffect(() => {
    api.wiki.trustHistory(agentId, row.page_path, 100)
      .then((res) => setHistory(res.rows ?? []))
      .catch((err) => toast.error(formatError(err)))
      .finally(() => setLoadingHistory(false));
  }, [agentId, row.page_path]);

  const submitOverride = async () => {
    if (!reason.trim()) {
      toast.error(intl.formatMessage({ id: 'wikiTrust.override.reasonRequired' }));
      return;
    }
    setSubmitting(true);
    try {
      const result = await api.wiki.trustOverride({
        agent_id: agentId,
        page_path: row.page_path,
        trust,
        lock,
        do_not_inject: doNotInject,
        reason: reason.trim(),
      });
      toast.success(intl.formatMessage(
        { id: 'wikiTrust.override.applied' },
        {
          old: result.old_trust.toFixed(3),
          next: result.new_trust.toFixed(3),
          delta: (result.applied_delta >= 0 ? '+' : '') + result.applied_delta.toFixed(3),
        },
      ));
      onChanged();
    } catch (err) {
      toast.error(formatError(err));
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="max-h-[90vh] overflow-y-auto sm:max-w-3xl">
        <DialogHeader>
          <DialogTitle className="font-mono text-sm">{row.page_path}</DialogTitle>
          <DialogDescription>
            <span className="flex flex-wrap items-center gap-3 text-xs">
              <span>agent: <strong className="text-foreground">{row.agent_id}</strong></span>
              <span>citations: {row.citation_count}</span>
              <span>updated: {new Date(row.updated_at).toLocaleString()}</span>
            </span>
          </DialogDescription>
        </DialogHeader>

        {/* History */}
        <div className="space-y-3">
          <h4 className="flex items-center gap-2 text-sm font-medium text-foreground">
            <ActivityIcon className="size-4" />
            {intl.formatMessage({ id: 'wikiTrust.detail.history' })}
          </h4>
          {loadingHistory ? (
            <p className="text-sm text-muted-foreground">{intl.formatMessage({ id: 'common.loading' })}</p>
          ) : history.length === 0 ? (
            <p className="text-sm text-muted-foreground">{intl.formatMessage({ id: 'wikiTrust.detail.historyEmpty' })}</p>
          ) : (
            <div className="overflow-x-auto rounded-lg border border-surface-border">
              <table className="min-w-full text-xs">
                <thead>
                  <tr className="border-b border-surface-border text-left text-muted-foreground">
                    <th className="px-3 py-2 font-medium">{intl.formatMessage({ id: 'wikiTrust.col.ts' })}</th>
                    <th className="px-3 py-2 font-medium">{intl.formatMessage({ id: 'wikiTrust.col.delta' })}</th>
                    <th className="px-3 py-2 font-medium">{intl.formatMessage({ id: 'wikiTrust.col.trigger' })}</th>
                    <th className="px-3 py-2 font-medium">{intl.formatMessage({ id: 'wikiTrust.col.signal' })}</th>
                    <th className="px-3 py-2 font-medium">{intl.formatMessage({ id: 'wikiTrust.col.error' })}</th>
                  </tr>
                </thead>
                <tbody>
                  {history.map((h, idx) => (
                    <tr key={`${h.ts}-${idx}`} className="border-b border-surface-border last:border-0">
                      <td className="px-3 py-1.5 text-muted-foreground">{new Date(h.ts).toLocaleString()}</td>
                      <td className="px-3 py-1.5 font-mono">
                        {h.old_trust.toFixed(3)} → {h.new_trust.toFixed(3)}{' '}
                        <span className={cn(h.applied_delta >= 0 ? 'text-success' : 'text-destructive')}>
                          ({h.applied_delta >= 0 ? '+' : ''}{h.applied_delta.toFixed(3)})
                        </span>
                      </td>
                      <td className="px-3 py-1.5"><Badge variant="secondary" className={triggerBadgeClass(h.trigger)}>{h.trigger}</Badge></td>
                      <td className="px-3 py-1.5 text-muted-foreground">{h.signal_kind}</td>
                      <td className="px-3 py-1.5 font-mono text-muted-foreground">{h.composite_error != null ? h.composite_error.toFixed(2) : '—'}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </div>

        {/* Override */}
        <div className="border-t border-surface-border pt-4">
          <button
            type="button"
            onClick={() => setOverrideOpen((v) => !v)}
            className="flex items-center gap-2 text-sm font-medium text-brand hover:underline"
          >
            <RotateCcwIcon className="size-4" />
            {intl.formatMessage({ id: 'wikiTrust.override.toggle' })}
          </button>

          {overrideOpen && (
            <div className="mt-4 space-y-4">
              <div>
                <label className="flex items-center justify-between text-xs font-medium text-muted-foreground">
                  <span>{intl.formatMessage({ id: 'wikiTrust.override.trust' })}</span>
                  <span className="font-mono text-foreground">{trust.toFixed(3)}</span>
                </label>
                <input
                  type="range"
                  min={0}
                  max={1}
                  step={0.01}
                  value={trust}
                  onChange={(e) => setTrust(Number(e.target.value))}
                  className="mt-2 w-full accent-[var(--brand)]"
                />
              </div>

              <div className="flex flex-wrap gap-6">
                <label className="flex items-center gap-2 text-sm text-foreground">
                  <Checkbox checked={lock} onCheckedChange={(v) => setLock(v === true)} />
                  <LockIcon className="size-4" />
                  {intl.formatMessage({ id: 'wikiTrust.override.lock' })}
                </label>
                <label className="flex items-center gap-2 text-sm text-foreground">
                  <Checkbox checked={doNotInject} onCheckedChange={(v) => setDoNotInject(v === true)} />
                  {doNotInject ? <EyeOffIcon className="size-4" /> : <EyeIcon className="size-4" />}
                  {intl.formatMessage({ id: 'wikiTrust.override.dni.label' })}
                </label>
              </div>

              <div className="space-y-1.5">
                <label className="text-xs font-medium text-muted-foreground">{intl.formatMessage({ id: 'wikiTrust.override.reason' })}</label>
                <Input
                  value={reason}
                  onChange={(e) => setReason(e.target.value)}
                  placeholder={intl.formatMessage({ id: 'wikiTrust.override.reasonPlaceholder' })}
                />
              </div>

              <div className="flex justify-end gap-2">
                <Button variant="ghost" onClick={() => setOverrideOpen(false)}>{intl.formatMessage({ id: 'common.cancel' })}</Button>
                <Button variant="brand" onClick={submitOverride} disabled={submitting}>
                  {intl.formatMessage({ id: submitting ? 'common.saving' : 'wikiTrust.override.apply' })}
                </Button>
              </div>
            </div>
          )}
        </div>

        <DialogFooter>
          <DialogClose render={<Button variant="outline">{intl.formatMessage({ id: 'common.close' })}</Button>} />
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function triggerBadgeClass(trigger: string): string {
  switch (trigger) {
    case 'prediction_error': return 'bg-warning/15 text-warning';
    case 'auto_correct': return 'bg-destructive/10 text-destructive';
    case 'manual': return 'bg-info/15 text-info';
    case 'federated_import': return 'bg-success/15 text-success';
    default: return '';
  }
}
