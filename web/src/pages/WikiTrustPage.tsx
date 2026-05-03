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
    <div className="space-y-6">
      <div className="flex items-center gap-3">
        <Shield className="h-6 w-6 text-amber-500" />
        <h2 className="text-2xl font-semibold text-stone-900 dark:text-stone-50">
          {intl.formatMessage({ id: 'wikiTrust.title', defaultMessage: 'Wiki Trust 反饋' })}
        </h2>
      </div>

      <p className="text-sm text-stone-600 dark:text-stone-400">
        {intl.formatMessage({
          id: 'wikiTrust.subtitle',
          defaultMessage:
            '由 prediction error 驅動的 wiki 自我清洗 — trust 過低的頁面會被自動隔離。可手動覆寫並鎖定可信頁面，避免被噪音壓抑。',
        })}
      </p>

      {/* Filters */}
      <div className="flex flex-wrap items-end gap-4 rounded-lg border border-stone-200 bg-white p-4 dark:border-stone-800 dark:bg-stone-900">
        <div className="flex flex-col gap-1">
          <label className="text-xs font-medium text-stone-500 dark:text-stone-400">
            {intl.formatMessage({ id: 'wikiTrust.agent', defaultMessage: 'Agent' })}
          </label>
          <select
            value={selectedAgent}
            onChange={(e) => setSelectedAgent(e.target.value)}
            className="rounded-md border border-stone-300 bg-white px-3 py-2 text-sm dark:border-stone-700 dark:bg-stone-800 dark:text-stone-100"
          >
            {agents.map((a) => (
              <option key={a.name} value={a.name}>
                {a.display_name} ({a.name})
              </option>
            ))}
          </select>
        </div>

        <div className="flex flex-col gap-1">
          <label className="text-xs font-medium text-stone-500 dark:text-stone-400">
            {intl.formatMessage({ id: 'wikiTrust.maxTrust', defaultMessage: 'Trust 上限' })}
          </label>
          <select
            value={maxTrust}
            onChange={(e) => setMaxTrust(Number(e.target.value))}
            className="rounded-md border border-stone-300 bg-white px-3 py-2 text-sm dark:border-stone-700 dark:bg-stone-800 dark:text-stone-100"
          >
            <option value={0.3}>≤ 0.30 (低)</option>
            <option value={0.5}>≤ 0.50 (中等)</option>
            <option value={0.7}>≤ 0.70 (含中高)</option>
            <option value={1.0}>1.00 (全部)</option>
          </select>
        </div>

        <button
          onClick={() => refresh(selectedAgent, maxTrust)}
          disabled={loading || !selectedAgent}
          className="rounded-md bg-amber-500 px-4 py-2 text-sm font-medium text-white hover:bg-amber-600 disabled:cursor-not-allowed disabled:opacity-50"
        >
          {loading
            ? intl.formatMessage({ id: 'common.loading', defaultMessage: '載入中...' })
            : intl.formatMessage({ id: 'common.refresh', defaultMessage: '重新整理' })}
        </button>

        <div className="ml-auto flex gap-3 text-sm">
          <SummaryStat
            icon={<ShieldAlert className="h-4 w-4 text-rose-500" />}
            label={intl.formatMessage({ id: 'wikiTrust.archived', defaultMessage: '已隔離' })}
            value={summary.archived}
          />
          <SummaryStat
            icon={<Lock className="h-4 w-4 text-emerald-500" />}
            label={intl.formatMessage({ id: 'wikiTrust.locked', defaultMessage: '鎖定' })}
            value={summary.locked}
          />
          <SummaryStat
            icon={<Activity className="h-4 w-4 text-stone-500" />}
            label={intl.formatMessage({ id: 'wikiTrust.signals', defaultMessage: '錯誤/成功' })}
            value={`${summary.totalErr} / ${summary.totalOk}`}
          />
        </div>
      </div>

      {available === false && note && (
        <div className="rounded-lg border border-amber-300 bg-amber-50 p-4 text-sm text-amber-900 dark:border-amber-700 dark:bg-amber-950/30 dark:text-amber-200">
          {note}
        </div>
      )}

      {/* Table */}
      <div className="overflow-hidden rounded-lg border border-stone-200 bg-white dark:border-stone-800 dark:bg-stone-900">
        <div className="overflow-x-auto">
          <table className="min-w-full divide-y divide-stone-200 text-sm dark:divide-stone-800">
            <thead className="bg-stone-50 dark:bg-stone-800/50">
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
            <tbody className="divide-y divide-stone-200 dark:divide-stone-800">
              {rows.length === 0 && !loading && (
                <tr>
                  <td colSpan={8} className="px-4 py-12 text-center text-stone-500 dark:text-stone-400">
                    {intl.formatMessage({
                      id: 'wikiTrust.empty',
                      defaultMessage: '沒有頁面落在這個 trust 範圍。',
                    })}
                  </td>
                </tr>
              )}
              {rows.map((r) => (
                <tr
                  key={`${r.page_path}-${r.agent_id}`}
                  className={cn(
                    'transition-colors hover:bg-stone-50 dark:hover:bg-stone-800/50',
                    r.do_not_inject && 'bg-rose-50/30 dark:bg-rose-950/10'
                  )}
                >
                  <td className="px-4 py-2 font-mono text-xs text-stone-700 dark:text-stone-300">
                    {r.page_path}
                  </td>
                  <td className="px-4 py-2">
                    <TrustBar value={r.trust} />
                  </td>
                  <td className="px-4 py-2 text-stone-600 dark:text-stone-400">{r.citation_count}</td>
                  <td className="px-4 py-2">
                    <span className={cn('text-rose-600 dark:text-rose-400', r.error_signal_count === 0 && 'text-stone-400 dark:text-stone-600')}>
                      {r.error_signal_count}
                    </span>
                  </td>
                  <td className="px-4 py-2">
                    <span className={cn('text-emerald-600 dark:text-emerald-400', r.success_signal_count === 0 && 'text-stone-400 dark:text-stone-600')}>
                      {r.success_signal_count}
                    </span>
                  </td>
                  <td className="px-4 py-2 text-xs text-stone-500 dark:text-stone-400">
                    {r.last_signal_at ? new Date(r.last_signal_at).toLocaleString() : '—'}
                  </td>
                  <td className="px-4 py-2">
                    <div className="flex gap-1">
                      {r.do_not_inject && (
                        <Badge tone="rose" icon={<EyeOff className="h-3 w-3" />}>
                          {intl.formatMessage({ id: 'wikiTrust.flag.archived', defaultMessage: '隔離' })}
                        </Badge>
                      )}
                      {r.locked && (
                        <Badge tone="emerald" icon={<Lock className="h-3 w-3" />}>
                          {intl.formatMessage({ id: 'wikiTrust.flag.locked', defaultMessage: '鎖定' })}
                        </Badge>
                      )}
                    </div>
                  </td>
                  <td className="px-4 py-2">
                    <button
                      onClick={() => setActiveRow(r)}
                      className="text-xs font-medium text-amber-600 hover:text-amber-700 dark:text-amber-400 dark:hover:text-amber-300"
                    >
                      {intl.formatMessage({ id: 'wikiTrust.action.inspect', defaultMessage: '詳情' })}
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </div>

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
    </div>
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
      className="fixed inset-0 z-50 flex items-center justify-center bg-stone-900/50 p-4 backdrop-blur-sm"
      onClick={onClose}
    >
      <div
        className="max-h-[90vh] w-full max-w-3xl overflow-y-auto rounded-xl border border-stone-200 bg-white shadow-2xl dark:border-stone-700 dark:bg-stone-900"
        onClick={(e) => e.stopPropagation()}
      >
        {/* Header */}
        <div className="flex items-start justify-between border-b border-stone-200 px-6 py-4 dark:border-stone-800">
          <div className="space-y-1">
            <h3 className="font-mono text-sm text-stone-700 dark:text-stone-300">{row.page_path}</h3>
            <div className="flex items-center gap-3 text-xs text-stone-500 dark:text-stone-400">
              <span>agent: <strong>{row.agent_id}</strong></span>
              <span>citations: {row.citation_count}</span>
              <span>updated: {new Date(row.updated_at).toLocaleString()}</span>
            </div>
          </div>
          <button
            onClick={onClose}
            className="rounded-md p-1 text-stone-400 hover:bg-stone-100 hover:text-stone-700 dark:hover:bg-stone-800"
          >
            <X className="h-5 w-5" />
          </button>
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
            <div className="overflow-hidden rounded-md border border-stone-200 dark:border-stone-800">
              <table className="min-w-full divide-y divide-stone-200 text-xs dark:divide-stone-800">
                <thead className="bg-stone-50 dark:bg-stone-800/50">
                  <tr>
                    <Th>{intl.formatMessage({ id: 'wikiTrust.col.ts', defaultMessage: '時間' })}</Th>
                    <Th>{intl.formatMessage({ id: 'wikiTrust.col.delta', defaultMessage: '變化' })}</Th>
                    <Th>{intl.formatMessage({ id: 'wikiTrust.col.trigger', defaultMessage: '觸發' })}</Th>
                    <Th>{intl.formatMessage({ id: 'wikiTrust.col.signal', defaultMessage: '信號' })}</Th>
                    <Th>{intl.formatMessage({ id: 'wikiTrust.col.error', defaultMessage: '誤差' })}</Th>
                  </tr>
                </thead>
                <tbody className="divide-y divide-stone-200 dark:divide-stone-800">
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
        <div className="border-t border-stone-200 px-6 py-4 dark:border-stone-800">
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
                    id: 'wikiTrust.override.dni',
                    defaultMessage: 'do_not_inject',
                  })}
                </label>
              </div>

              <div>
                <label className="text-xs font-medium text-stone-600 dark:text-stone-400">
                  {intl.formatMessage({
                    id: 'wikiTrust.override.reason',
                    defaultMessage: '修改原因（必填）',
                  })}
                </label>
                <input
                  type="text"
                  value={reason}
                  onChange={(e) => setReason(e.target.value)}
                  placeholder={intl.formatMessage({
                    id: 'wikiTrust.override.reasonPlaceholder',
                    defaultMessage: '例：人工審核確認此頁面為事實基準',
                  })}
                  className="mt-1 w-full rounded-md border border-stone-300 bg-white px-3 py-2 text-sm dark:border-stone-700 dark:bg-stone-800"
                />
              </div>

              <div className="flex justify-end gap-2">
                <button
                  onClick={() => setOverrideOpen(false)}
                  className="rounded-md px-4 py-2 text-sm font-medium text-stone-600 hover:bg-stone-100 dark:text-stone-300 dark:hover:bg-stone-800"
                >
                  {intl.formatMessage({ id: 'common.cancel', defaultMessage: '取消' })}
                </button>
                <button
                  onClick={submitOverride}
                  disabled={submitting}
                  className="rounded-md bg-amber-500 px-4 py-2 text-sm font-medium text-white hover:bg-amber-600 disabled:opacity-50"
                >
                  {submitting
                    ? intl.formatMessage({ id: 'common.saving', defaultMessage: '儲存中...' })
                    : intl.formatMessage({ id: 'wikiTrust.override.apply', defaultMessage: '套用覆寫' })}
                </button>
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
      <div className="h-1.5 w-24 overflow-hidden rounded-full bg-stone-200 dark:bg-stone-800">
        <div className={cn('h-full transition-all', tone)} style={{ width: `${pct}%` }} />
      </div>
      <span className="font-mono text-xs text-stone-700 dark:text-stone-300">{value.toFixed(3)}</span>
    </div>
  );
}

interface BadgeProps {
  tone: 'rose' | 'emerald' | 'amber' | 'stone' | 'sky';
  icon?: React.ReactNode;
  children: React.ReactNode;
}

function Badge({ tone, icon, children }: BadgeProps) {
  const tones: Record<BadgeProps['tone'], string> = {
    rose: 'bg-rose-100 text-rose-700 dark:bg-rose-950/40 dark:text-rose-300',
    emerald: 'bg-emerald-100 text-emerald-700 dark:bg-emerald-950/40 dark:text-emerald-300',
    amber: 'bg-amber-100 text-amber-700 dark:bg-amber-950/40 dark:text-amber-300',
    stone: 'bg-stone-100 text-stone-700 dark:bg-stone-800 dark:text-stone-300',
    sky: 'bg-sky-100 text-sky-700 dark:bg-sky-950/40 dark:text-sky-300',
  };
  return (
    <span
      className={cn(
        'inline-flex items-center gap-1 rounded-full px-2 py-0.5 text-xs font-medium',
        tones[tone]
      )}
    >
      {icon}
      {children}
    </span>
  );
}

function SummaryStat({ icon, label, value }: { icon: React.ReactNode; label: string; value: number | string }) {
  return (
    <div className="flex items-center gap-2 rounded-md bg-stone-100 px-3 py-1.5 dark:bg-stone-800">
      {icon}
      <span className="text-xs text-stone-500 dark:text-stone-400">{label}</span>
      <span className="font-mono text-sm font-semibold text-stone-900 dark:text-stone-100">{value}</span>
    </div>
  );
}

function triggerTone(trigger: string): BadgeProps['tone'] {
  switch (trigger) {
    case 'prediction_error':
      return 'amber';
    case 'auto_correct':
      return 'rose';
    case 'manual':
      return 'sky';
    case 'rollback':
      return 'stone';
    case 'federated_import':
      return 'emerald';
    default:
      return 'stone';
  }
}
