import { useCallback, useEffect, useMemo, useState } from 'react';
import { useIntl } from 'react-intl';
import {
  Shield,
  ShieldAlert,
  Lock,
  RotateCcw,
  Eye,
  EyeOff,
  Activity,
  X,
} from 'lucide-react';
import { cn } from '@/lib/utils';
import { api, type WikiTrustHistoryRow, type WikiTrustRow } from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import {
  Page,
  PageHeader,
  Card,
  StatCard,
  Badge,
  Button,
  EmptyState,
  Field,
  controlClass,
} from '@/components/ui';

// ───────────────────────────────────────────────────────────────────────
// Page
// ───────────────────────────────────────────────────────────────────────

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

  // Load agents on mount.
  useEffect(() => {
    api.agents.list().then((res) => {
      const list = res?.agents ?? [];
      setAgents(list);
      if (list.length > 0) {
        setSelectedAgent(list[0].name);
      }
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
    if (selectedAgent) {
      refresh(selectedAgent, maxTrust);
    }
  }, [selectedAgent, maxTrust, refresh]);

  const summary = useMemo(() => {
    const archived = rows.filter((r) => r.do_not_inject).length;
    const locked = rows.filter((r) => r.locked).length;
    const totalErr = rows.reduce((acc, r) => acc + r.error_signal_count, 0);
    const totalOk = rows.reduce((acc, r) => acc + r.success_signal_count, 0);
    return { archived, locked, totalErr, totalOk };
  }, [rows]);

  return (
    <Page wide>
      <PageHeader
        icon={Shield}
        title={intl.formatMessage({ id: 'wikiTrust.title', defaultMessage: 'Wiki Trust 反饋' })}
        subtitle={intl.formatMessage({
          id: 'wikiTrust.subtitle',
          defaultMessage:
            '由 prediction error 驅動的 wiki 自我清洗 — trust 過低的頁面會被自動隔離。可手動覆寫並鎖定可信頁面，避免被噪音壓抑。',
        })}
        actions={
          <Button
            variant="primary"
            onClick={() => refresh(selectedAgent, maxTrust)}
            disabled={loading || !selectedAgent}
          >
            {loading
              ? intl.formatMessage({ id: 'common.loading', defaultMessage: '載入中...' })
              : intl.formatMessage({ id: 'common.refresh', defaultMessage: '重新整理' })}
          </Button>
        }
      />

      {/* Summary metrics */}
      <div className="grid grid-cols-2 gap-4 lg:grid-cols-3">
        <StatCard
          icon={ShieldAlert}
          tone="danger"
          label={intl.formatMessage({ id: 'wikiTrust.archived', defaultMessage: '已隔離' })}
          value={summary.archived}
        />
        <StatCard
          icon={Lock}
          tone="success"
          label={intl.formatMessage({ id: 'wikiTrust.locked', defaultMessage: '鎖定' })}
          value={summary.locked}
        />
        <StatCard
          icon={Activity}
          tone="neutral"
          label={intl.formatMessage({ id: 'wikiTrust.signals', defaultMessage: '錯誤/成功' })}
          value={`${summary.totalErr} / ${summary.totalOk}`}
          className="col-span-2 lg:col-span-1"
        />
      </div>

      {/* Filters */}
      <Card>
        <div className="flex flex-wrap items-end gap-4">
          <Field
            label={intl.formatMessage({ id: 'wikiTrust.agent', defaultMessage: 'Agent' })}
            className="min-w-[14rem]"
          >
            <select
              value={selectedAgent}
              onChange={(e) => setSelectedAgent(e.target.value)}
              className={controlClass}
            >
              {agents.map((a) => (
                <option key={a.name} value={a.name}>
                  {a.display_name} ({a.name})
                </option>
              ))}
            </select>
          </Field>

          <Field
            label={intl.formatMessage({ id: 'wikiTrust.maxTrust', defaultMessage: 'Trust 上限' })}
            className="min-w-[12rem]"
          >
            <select
              value={maxTrust}
              onChange={(e) => setMaxTrust(Number(e.target.value))}
              className={controlClass}
            >
              <option value={0.3}>≤ 0.30 (低)</option>
              <option value={0.5}>≤ 0.50 (中等)</option>
              <option value={0.7}>≤ 0.70 (含中高)</option>
              <option value={1.0}>1.00 (全部)</option>
            </select>
          </Field>
        </div>
      </Card>

      {available === false && note && (
        <Card>
          <p className="flex items-start gap-2 text-sm text-amber-700 dark:text-amber-300">
            <ShieldAlert className="mt-0.5 h-4 w-4 shrink-0" />
            {note}
          </p>
        </Card>
      )}

      {/* Table */}
      <Card padded={false}>
        <div className="overflow-x-auto">
          <table className="min-w-full divide-y divide-[var(--panel-border)] text-sm">
            <thead>
              <tr>
                <Th>{intl.formatMessage({ id: 'wikiTrust.col.page', defaultMessage: '頁面' })}</Th>
                <Th>{intl.formatMessage({ id: 'wikiTrust.col.trust', defaultMessage: 'Trust' })}</Th>
                <Th>{intl.formatMessage({ id: 'wikiTrust.col.cite', defaultMessage: '引用' })}</Th>
                <Th>{intl.formatMessage({ id: 'wikiTrust.col.err', defaultMessage: '錯誤' })}</Th>
                <Th>{intl.formatMessage({ id: 'wikiTrust.col.ok', defaultMessage: '成功' })}</Th>
                <Th>{intl.formatMessage({ id: 'wikiTrust.col.lastSignal', defaultMessage: '最近信號' })}</Th>
                <Th>{intl.formatMessage({ id: 'wikiTrust.col.flags', defaultMessage: '狀態' })}</Th>
                <Th>{intl.formatMessage({ id: 'wikiTrust.col.actions', defaultMessage: '操作' })}</Th>
              </tr>
            </thead>
            <tbody className="divide-y divide-[var(--panel-border)]">
              {rows.length === 0 && !loading && (
                <tr>
                  <td colSpan={8} className="px-4 py-2">
                    <EmptyState
                      icon={Shield}
                      title={intl.formatMessage({
                        id: 'wikiTrust.empty',
                        defaultMessage: '沒有頁面落在這個 trust 範圍。',
                      })}
                    />
                  </td>
                </tr>
              )}
              {rows.map((r) => (
                <tr
                  key={`${r.page_path}-${r.agent_id}`}
                  className={cn(
                    'transition-colors hover:bg-stone-500/5 dark:hover:bg-white/5',
                    r.do_not_inject && 'bg-rose-50/30 dark:bg-rose-950/10'
                  )}
                >
                  <td className="px-4 py-2 font-mono text-xs text-stone-700 dark:text-stone-300">
                    {r.page_path}
                  </td>
                  <td className="px-4 py-2">
                    <TrustBar value={r.trust} />
                  </td>
                  <td className="px-4 py-2 tabular-nums text-stone-600 dark:text-stone-400">{r.citation_count}</td>
                  <td className="px-4 py-2">
                    <span className={cn('tabular-nums text-rose-600 dark:text-rose-400', r.error_signal_count === 0 && 'text-stone-400 dark:text-stone-600')}>
                      {r.error_signal_count}
                    </span>
                  </td>
                  <td className="px-4 py-2">
                    <span className={cn('tabular-nums text-emerald-600 dark:text-emerald-400', r.success_signal_count === 0 && 'text-stone-400 dark:text-stone-600')}>
                      {r.success_signal_count}
                    </span>
                  </td>
                  <td className="px-4 py-2 text-xs text-stone-500 dark:text-stone-400">
                    {r.last_signal_at ? new Date(r.last_signal_at).toLocaleString() : '—'}
                  </td>
                  <td className="px-4 py-2">
                    <div className="flex gap-1">
                      {r.do_not_inject && (
                        <Badge tone="danger">
                          <EyeOff className="h-3 w-3" />
                          {intl.formatMessage({ id: 'wikiTrust.flag.archived', defaultMessage: '隔離' })}
                        </Badge>
                      )}
                      {r.locked && (
                        <Badge tone="success">
                          <Lock className="h-3 w-3" />
                          {intl.formatMessage({ id: 'wikiTrust.flag.locked', defaultMessage: '鎖定' })}
                        </Badge>
                      )}
                    </div>
                  </td>
                  <td className="px-4 py-2">
                    <Button variant="ghost" size="sm" onClick={() => setActiveRow(r)}>
                      {intl.formatMessage({ id: 'wikiTrust.action.inspect', defaultMessage: '詳情' })}
                    </Button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </Card>

      {activeRow && (
        <TrustDetailModal
          row={activeRow}
          agentId={selectedAgent}
          onClose={() => setActiveRow(null)}
          onChanged={() => {
            setActiveRow(null);
            refresh(selectedAgent, maxTrust);
          }}
        />
      )}
    </Page>
  );
}

// ───────────────────────────────────────────────────────────────────────
// Detail Modal
// ───────────────────────────────────────────────────────────────────────

interface TrustDetailModalProps {
  row: WikiTrustRow;
  agentId: string;
  onClose: () => void;
  onChanged: () => void;
}

function TrustDetailModal({ row, agentId, onClose, onChanged }: TrustDetailModalProps) {
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
    api.wiki
      .trustHistory(agentId, row.page_path, 100)
      .then((res) => setHistory(res.rows ?? []))
      .catch((err) => toast.error(formatError(err)))
      .finally(() => setLoadingHistory(false));
  }, [agentId, row.page_path]);

  const submitOverride = async () => {
    if (!reason.trim()) {
      toast.error(intl.formatMessage({
        id: 'wikiTrust.override.reasonRequired',
        defaultMessage: '請填寫修改原因',
      }));
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
        {
          id: 'wikiTrust.override.applied',
          defaultMessage: 'Trust 已從 {old} → {next}（Δ={delta}）',
        },
        {
          old: result.old_trust.toFixed(3),
          next: result.new_trust.toFixed(3),
          delta: (result.applied_delta >= 0 ? '+' : '') + result.applied_delta.toFixed(3),
        }
      ));
      onChanged();
    } catch (err) {
      toast.error(formatError(err));
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-stone-950/45 p-4 backdrop-blur-sm"
      onClick={onClose}
    >
      <div
        className="glass-overlay max-h-[90vh] w-full max-w-3xl overflow-y-auto rounded-2xl"
        onClick={(e) => e.stopPropagation()}
      >
        {/* Header */}
        <div className="flex items-start justify-between border-b border-[var(--panel-border)] px-6 py-4">
          <div className="space-y-1">
            <h3 className="font-mono text-sm text-stone-700 dark:text-stone-300">{row.page_path}</h3>
            <div className="flex items-center gap-3 text-xs text-stone-500 dark:text-stone-400">
              <span>agent: <strong>{row.agent_id}</strong></span>
              <span>citations: {row.citation_count}</span>
              <span>updated: {new Date(row.updated_at).toLocaleString()}</span>
            </div>
          </div>
          <Button variant="ghost" size="sm" icon={X} onClick={onClose} aria-label="close" />
        </div>

        {/* History */}
        <div className="px-6 py-4">
          <h4 className="mb-3 flex items-center gap-2 text-sm font-semibold text-stone-700 dark:text-stone-300">
            <Activity className="h-4 w-4" />
            {intl.formatMessage({ id: 'wikiTrust.detail.history', defaultMessage: 'Trust 歷史' })}
          </h4>
          {loadingHistory ? (
            <p className="text-sm text-stone-500">
              {intl.formatMessage({ id: 'common.loading', defaultMessage: '載入中...' })}
            </p>
          ) : history.length === 0 ? (
            <p className="text-sm text-stone-500">
              {intl.formatMessage({ id: 'wikiTrust.detail.historyEmpty', defaultMessage: '無歷史記錄' })}
            </p>
          ) : (
            <div className="overflow-hidden rounded-lg border border-[var(--panel-border)]">
              <table className="min-w-full divide-y divide-[var(--panel-border)] text-xs">
                <thead>
                  <tr>
                    <Th>{intl.formatMessage({ id: 'wikiTrust.col.ts', defaultMessage: '時間' })}</Th>
                    <Th>{intl.formatMessage({ id: 'wikiTrust.col.delta', defaultMessage: '變化' })}</Th>
                    <Th>{intl.formatMessage({ id: 'wikiTrust.col.trigger', defaultMessage: '觸發' })}</Th>
                    <Th>{intl.formatMessage({ id: 'wikiTrust.col.signal', defaultMessage: '信號' })}</Th>
                    <Th>{intl.formatMessage({ id: 'wikiTrust.col.error', defaultMessage: '誤差' })}</Th>
                  </tr>
                </thead>
                <tbody className="divide-y divide-[var(--panel-border)]">
                  {history.map((h, idx) => (
                    <tr key={`${h.ts}-${idx}`}>
                      <td className="px-3 py-1.5 text-stone-600 dark:text-stone-400">
                        {new Date(h.ts).toLocaleString()}
                      </td>
                      <td className="px-3 py-1.5 font-mono">
                        {h.old_trust.toFixed(3)} → {h.new_trust.toFixed(3)}{' '}
                        <span className={cn(h.applied_delta >= 0 ? 'text-emerald-600' : 'text-rose-600')}>
                          ({h.applied_delta >= 0 ? '+' : ''}
                          {h.applied_delta.toFixed(3)})
                        </span>
                      </td>
                      <td className="px-3 py-1.5">
                        <Badge tone={triggerTone(h.trigger)}>{h.trigger}</Badge>
                      </td>
                      <td className="px-3 py-1.5 text-stone-500">{h.signal_kind}</td>
                      <td className="px-3 py-1.5 font-mono text-stone-500">
                        {h.composite_error != null ? h.composite_error.toFixed(2) : '—'}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </div>

        {/* Override */}
        <div className="border-t border-[var(--panel-border)] px-6 py-4">
          <button
            onClick={() => setOverrideOpen((v) => !v)}
            className="flex items-center gap-2 text-sm font-semibold text-amber-600 hover:text-amber-700 dark:text-amber-400"
          >
            <RotateCcw className="h-4 w-4" />
            {intl.formatMessage({
              id: 'wikiTrust.override.toggle',
              defaultMessage: '手動覆寫 Trust',
            })}
          </button>

          {overrideOpen && (
            <div className="mt-4 space-y-4">
              <div>
                <label className="flex items-center justify-between text-xs font-medium text-stone-600 dark:text-stone-400">
                  <span>
                    {intl.formatMessage({ id: 'wikiTrust.override.trust', defaultMessage: 'Trust 值' })}
                  </span>
                  <span className="font-mono text-stone-700 dark:text-stone-300">{trust.toFixed(3)}</span>
                </label>
                <input
                  type="range"
                  min={0}
                  max={1}
                  step={0.01}
                  value={trust}
                  onChange={(e) => setTrust(Number(e.target.value))}
                  className="mt-2 w-full accent-amber-500"
                />
              </div>

              <div className="flex flex-wrap gap-4">
                <label className="flex items-center gap-2 text-sm text-stone-700 dark:text-stone-300">
                  <input
                    type="checkbox"
                    checked={lock}
                    onChange={(e) => setLock(e.target.checked)}
                    className="accent-amber-500"
                  />
                  <Lock className="h-4 w-4" />
                  {intl.formatMessage({
                    id: 'wikiTrust.override.lock',
                    defaultMessage: '鎖定 — 免疫自動調整',
                  })}
                </label>
                <label className="flex items-center gap-2 text-sm text-stone-700 dark:text-stone-300">
                  <input
                    type="checkbox"
                    checked={doNotInject}
                    onChange={(e) => setDoNotInject(e.target.checked)}
                    className="accent-amber-500"
                  />
                  {doNotInject ? <EyeOff className="h-4 w-4" /> : <Eye className="h-4 w-4" />}
                  {intl.formatMessage({
                    id: 'wikiTrust.override.dni.label',
                    defaultMessage: '不注入到 AI 員工提示',
                  })}
                </label>
              </div>

              <Field
                label={intl.formatMessage({
                  id: 'wikiTrust.override.reason',
                  defaultMessage: '修改原因（必填）',
                })}
              >
                <input
                  type="text"
                  value={reason}
                  onChange={(e) => setReason(e.target.value)}
                  placeholder={intl.formatMessage({
                    id: 'wikiTrust.override.reasonPlaceholder',
                    defaultMessage: '例：人工審核確認此頁面為事實基準',
                  })}
                  className={controlClass}
                />
              </Field>

              <div className="flex justify-end gap-2">
                <Button variant="ghost" onClick={() => setOverrideOpen(false)}>
                  {intl.formatMessage({ id: 'common.cancel', defaultMessage: '取消' })}
                </Button>
                <Button variant="primary" onClick={submitOverride} disabled={submitting}>
                  {submitting
                    ? intl.formatMessage({ id: 'common.saving', defaultMessage: '儲存中...' })
                    : intl.formatMessage({ id: 'wikiTrust.override.apply', defaultMessage: '套用覆寫' })}
                </Button>
              </div>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

// ───────────────────────────────────────────────────────────────────────
// Subcomponents
// ───────────────────────────────────────────────────────────────────────

function Th({ children }: { children: React.ReactNode }) {
  return (
    <th className="px-4 py-2 text-left text-xs font-medium uppercase tracking-wide text-stone-500 dark:text-stone-400">
      {children}
    </th>
  );
}

function TrustBar({ value }: { value: number }) {
  const pct = Math.max(0, Math.min(100, value * 100));
  const tone =
    value >= 0.7 ? 'bg-emerald-500' : value >= 0.4 ? 'bg-amber-500' : value >= 0.2 ? 'bg-orange-500' : 'bg-rose-500';
  return (
    <div className="flex items-center gap-2">
      <div className="h-1.5 w-24 overflow-hidden rounded-full bg-stone-500/15">
        <div className={cn('h-full transition-all', tone)} style={{ width: `${pct}%` }} />
      </div>
      <span className="font-mono text-xs tabular-nums text-stone-700 dark:text-stone-300">{value.toFixed(3)}</span>
    </div>
  );
}

type BadgeTone = 'neutral' | 'success' | 'warning' | 'danger' | 'info' | 'accent';

function triggerTone(trigger: string): BadgeTone {
  switch (trigger) {
    case 'prediction_error':
      return 'warning';
    case 'auto_correct':
      return 'danger';
    case 'manual':
      return 'info';
    case 'rollback':
      return 'neutral';
    case 'federated_import':
      return 'success';
    default:
      return 'neutral';
  }
}
