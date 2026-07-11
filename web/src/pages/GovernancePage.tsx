import { useEffect, useState, useCallback, useMemo } from 'react';
import { useIntl } from 'react-intl';
import {
  api,
  GOV_POLICY_TYPES,
  GOV_RATE_RESOURCES,
  GOV_ACTIONS,
  type GovPolicy,
  type GovPolicyType,
  type GovRateResource,
  type GovAction,
} from '@/lib/api';
import { Dialog, inputClass } from '@/components/shared/Dialog';
import { SettingField, OptionSelect } from '@/components/settings/controls';
import { ChipEditor } from '@/components/shared/ChipEditor';
import { toast, formatError } from '@/lib/toast';
import { Scale, Plus, Trash2, Pencil } from 'lucide-react';
import {
  Page,
  PageHeader,
  Card,
  Button,
  Badge,
  EmptyState,
  Tabs,
  Mono,
  type TabItem,
} from '@/components/ui';

type BadgeTone = 'info' | 'accent' | 'warning' | 'success';

const TYPE_TONES: Record<GovPolicyType, BadgeTone> = {
  rate: 'info',
  permission: 'accent',
  quota: 'warning',
  lifecycle: 'success',
};

type TypeFilter = 'all' | GovPolicyType;

/** Build a fresh, validation-friendly default policy of the given type. */
function defaultPolicy(type: GovPolicyType): GovPolicy {
  const base = { policy_type: type, policy_id: '', agent_id: '*' };
  switch (type) {
    case 'rate':
      return { ...base, resource: 'mcp_calls', limit: 200, window_seconds: 60, action_on_violation: 'reject' };
    case 'permission':
      return { ...base, allowed_scopes: [], denied_scopes: [], requires_approval: [] };
    case 'quota':
      return { ...base, daily_token_budget: 100000, max_concurrent_tasks: 4, max_memory_entries: 0, reset_cron: '0 0 * * *' };
    case 'lifecycle':
      return { ...base, max_idle_hours: 168, health_check_interval_seconds: 300, auto_suspend_on_violation_count: 0 };
  }
}

export function GovernancePage() {
  const intl = useIntl();
  const [policies, setPolicies] = useState<ReadonlyArray<GovPolicy>>([]);
  const [loading, setLoading] = useState(false);
  const [editing, setEditing] = useState<{ policy: GovPolicy; isNew: boolean } | null>(null);
  const [removing, setRemoving] = useState<GovPolicy | null>(null);
  const [filter, setFilter] = useState<TypeFilter>('all');

  const fetchPolicies = useCallback(async () => {
    setLoading(true);
    try {
      const res = await api.governance.list();
      setPolicies(res?.policies ?? []);
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
    } finally {
      setLoading(false);
    }
  }, [intl]);

  useEffect(() => {
    fetchPolicies();
  }, [fetchPolicies]);

  const tabItems: TabItem[] = useMemo(
    () => [
      { id: 'all', label: intl.formatMessage({ id: 'tasks.filter.all' }), badge: policies.length },
      ...GOV_POLICY_TYPES.map((t) => ({
        id: t,
        label: t,
        badge: policies.filter((p) => p.policy_type === t).length,
      })),
    ],
    [intl, policies]
  );

  const visiblePolicies = useMemo(
    () => (filter === 'all' ? policies : policies.filter((p) => p.policy_type === filter)),
    [filter, policies]
  );

  return (
    <Page>
      <PageHeader
        icon={Scale}
        title={intl.formatMessage({ id: 'nav.governance' })}
        subtitle={intl.formatMessage({ id: 'gov.desc' })}
        actions={
          <Button
            variant="primary"
            icon={Plus}
            onClick={() => setEditing({ policy: defaultPolicy('rate'), isNew: true })}
          >
            {intl.formatMessage({ id: 'gov.add' })}
          </Button>
        }
      />

      <Tabs items={tabItems} value={filter} onChange={(id) => setFilter(id as TypeFilter)} />

      <Card padded={false}>
        {loading ? (
          <p className="py-8 text-center text-sm text-stone-400">{intl.formatMessage({ id: 'common.loading' })}</p>
        ) : visiblePolicies.length === 0 ? (
          <EmptyState icon={Scale} dudu="idle" title={intl.formatMessage({ id: 'gov.empty' })} />
        ) : (
          <div className="overflow-x-auto">
            <table className="w-full text-sm">
              <thead>
                <tr className="border-b border-[var(--panel-border)]">
                  <th className="px-5 py-2.5 text-left font-medium text-stone-500 dark:text-stone-400">{intl.formatMessage({ id: 'gov.col.id' })}</th>
                  <th className="px-5 py-2.5 text-left font-medium text-stone-500 dark:text-stone-400">{intl.formatMessage({ id: 'gov.col.type' })}</th>
                  <th className="px-5 py-2.5 text-left font-medium text-stone-500 dark:text-stone-400">{intl.formatMessage({ id: 'gov.col.scope' })}</th>
                  <th className="px-5 py-2.5 text-left font-medium text-stone-500 dark:text-stone-400">{intl.formatMessage({ id: 'gov.col.detail' })}</th>
                  <th className="px-5 py-2.5 text-right font-medium text-stone-500 dark:text-stone-400" />
                </tr>
              </thead>
              <tbody>
                {visiblePolicies.map((p) => (
                  <tr key={`${p.scope ?? p.agent_id}:${p.policy_id}`} className="border-b border-[var(--panel-border)] last:border-0">
                    <td className="px-5 py-2.5 font-medium text-stone-800 dark:text-stone-200">
                      <Mono>{p.policy_id}</Mono>
                    </td>
                    <td className="px-5 py-2.5">
                      <Badge tone={TYPE_TONES[p.policy_type]}>{p.policy_type}</Badge>
                    </td>
                    <td className="px-5 py-2.5 text-stone-600 dark:text-stone-400">
                      <Mono>{p.scope ?? p.agent_id}</Mono>
                    </td>
                    <td className="px-5 py-2.5 text-xs text-stone-500 dark:text-stone-400">{policyDetail(p)}</td>
                    <td className="px-5 py-2.5 text-right">
                      <div className="flex items-center justify-end gap-1">
                        <Button
                          variant="ghost"
                          size="sm"
                          icon={Pencil}
                          onClick={() => setEditing({ policy: { ...p }, isNew: false })}
                          title={intl.formatMessage({ id: 'common.edit' })}
                          aria-label={intl.formatMessage({ id: 'common.edit' })}
                        />
                        <Button
                          variant="ghost"
                          size="sm"
                          icon={Trash2}
                          onClick={() => setRemoving(p)}
                          title={intl.formatMessage({ id: 'common.delete' })}
                          aria-label={intl.formatMessage({ id: 'common.delete' })}
                          className="text-rose-500 hover:bg-rose-500/10 hover:text-rose-600 dark:text-rose-400"
                        />
                      </div>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </Card>

      {editing && (
        <PolicyDialog
          initial={editing.policy}
          isNew={editing.isNew}
          onClose={() => setEditing(null)}
          onSaved={() => { setEditing(null); fetchPolicies(); }}
        />
      )}

      <RemoveDialog
        policy={removing}
        onClose={() => setRemoving(null)}
        onRemoved={() => { setRemoving(null); fetchPolicies(); }}
      />
    </Page>
  );
}

function policyDetail(p: GovPolicy): string {
  switch (p.policy_type) {
    case 'rate':
      return `${p.resource} ≤ ${p.limit}/${p.window_seconds}s → ${p.action_on_violation}`;
    case 'permission':
      return `+${(p.allowed_scopes ?? []).length} / -${(p.denied_scopes ?? []).length}`;
    case 'quota':
      return `${p.daily_token_budget} tok/day · ${p.max_concurrent_tasks} tasks`;
    case 'lifecycle':
      return `idle ${p.max_idle_hours}h · hc ${p.health_check_interval_seconds}s`;
    default:
      return '';
  }
}

function PolicyDialog({
  initial,
  isNew,
  onClose,
  onSaved,
}: {
  initial: GovPolicy;
  isNew: boolean;
  onClose: () => void;
  onSaved: () => void;
}) {
  const intl = useIntl();
  const [policy, setPolicy] = useState<GovPolicy>(initial);
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Plain-language labels for the technical enum values. The raw value is still
  // shown after a middle dot by OptionSelect, and the written payload is
  // unchanged — only the visible label is friendlier.
  const typeLabels: Record<GovPolicyType, string> = {
    rate: intl.formatMessage({ id: 'gov.type.opt.rate', defaultMessage: '頻率限制' }),
    permission: intl.formatMessage({ id: 'gov.type.opt.permission', defaultMessage: '權限範圍' }),
    quota: intl.formatMessage({ id: 'gov.type.opt.quota', defaultMessage: '用量配額' }),
    lifecycle: intl.formatMessage({ id: 'gov.type.opt.lifecycle', defaultMessage: '生命週期' }),
  };
  const resourceLabels: Record<GovRateResource, string> = {
    mcp_calls: intl.formatMessage({ id: 'gov.rate.resource.opt.mcp_calls', defaultMessage: '工具呼叫次數' }),
    memory_writes: intl.formatMessage({ id: 'gov.rate.resource.opt.memory_writes', defaultMessage: '記憶寫入次數' }),
    wiki_writes: intl.formatMessage({ id: 'gov.rate.resource.opt.wiki_writes', defaultMessage: '知識庫寫入次數' }),
    message_sends: intl.formatMessage({ id: 'gov.rate.resource.opt.message_sends', defaultMessage: '訊息傳送次數' }),
  };
  const actionLabels: Record<GovAction, string> = {
    reject: intl.formatMessage({ id: 'gov.rate.action.opt.reject', defaultMessage: '直接拒絕' }),
    warn: intl.formatMessage({ id: 'gov.rate.action.opt.warn', defaultMessage: '僅記錄警告、放行' }),
    throttle: intl.formatMessage({ id: 'gov.rate.action.opt.throttle', defaultMessage: '限流降速' }),
  };

  const set = <K extends keyof GovPolicy>(key: K, value: GovPolicy[K]) =>
    setPolicy((prev) => ({ ...prev, [key]: value }));

  const changeType = (type: GovPolicyType) => {
    // Preserve id/agent_id; reset per-type fields to defaults.
    setPolicy((prev) => ({ ...defaultPolicy(type), policy_id: prev.policy_id, agent_id: prev.agent_id }));
  };

  const handleSubmit = async () => {
    if (!policy.policy_id.trim()) {
      setError(intl.formatMessage({ id: 'gov.error.idRequired' }));
      return;
    }
    setSubmitting(true);
    setError(null);
    try {
      // Strip the read-only `scope` field before sending.
      const { scope: _scope, ...payload } = policy;
      void _scope;
      await api.governance.upsert(payload);
      onSaved();
    } catch (e) {
      setError(formatError(e));
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <Dialog
      open
      onClose={onClose}
      title={isNew ? intl.formatMessage({ id: 'gov.add' }) : intl.formatMessage({ id: 'gov.edit' })}
    >
      <div className="space-y-4">
        <div className="grid grid-cols-2 gap-3">
          <SettingField
            label={intl.formatMessage({ id: 'gov.field.id' })}
            help={intl.formatMessage({ id: 'gov.field.id.hint' })}
          >
            <input
              type="text"
              value={policy.policy_id}
              onChange={(e) => set('policy_id', e.target.value)}
              disabled={!isNew}
              placeholder="default-rate-mcp"
              className={inputClass}
            />
          </SettingField>
          <SettingField
            label={intl.formatMessage({ id: 'gov.field.type' })}
            help={intl.formatMessage({ id: 'gov.field.type.help', defaultMessage: '這條規則要管什麼：頻率、權限、配額或生命週期。建立後不可更改。' })}
          >
            <OptionSelect
              value={policy.policy_type}
              onChange={(v) => changeType(v as GovPolicyType)}
              disabled={!isNew}
              options={GOV_POLICY_TYPES.map((t) => ({ value: t, label: typeLabels[t], raw: t }))}
            />
          </SettingField>
        </div>

        <SettingField
          label={intl.formatMessage({ id: 'gov.field.agentId' })}
          help={intl.formatMessage({ id: 'gov.field.agentId.hint' })}
        >
          <input type="text" value={policy.agent_id} onChange={(e) => set('agent_id', e.target.value)} placeholder="*" className={inputClass} />
        </SettingField>

        {policy.policy_type === 'rate' && (
          <>
            <SettingField
              label={intl.formatMessage({ id: 'gov.rate.resource' })}
              help={intl.formatMessage({ id: 'gov.rate.resource.help', defaultMessage: '要計數的動作。超過下面設定的次數時，就會觸發違規動作。' })}
            >
              <OptionSelect
                value={policy.resource ?? 'mcp_calls'}
                onChange={(v) => set('resource', v as GovRateResource)}
                options={GOV_RATE_RESOURCES.map((r) => ({ value: r, label: resourceLabels[r], raw: r }))}
              />
            </SettingField>
            <div className="grid grid-cols-2 gap-3">
              <SettingField
                label={intl.formatMessage({ id: 'gov.rate.limit' })}
                help={intl.formatMessage({ id: 'gov.rate.limit.help', defaultMessage: '在時間視窗內允許的最多次數。調高＝更寬鬆，調低＝更嚴格。' })}
              >
                <input type="number" min={1} value={policy.limit ?? 0} onChange={(e) => set('limit', Number(e.target.value))} className={inputClass} />
              </SettingField>
              <SettingField
                label={intl.formatMessage({ id: 'gov.rate.window' })}
                help={intl.formatMessage({ id: 'gov.rate.window.help', defaultMessage: '計數的時間長度（秒）。例如 60 代表「每分鐘」計算一次上限。' })}
              >
                <input type="number" min={1} value={policy.window_seconds ?? 0} onChange={(e) => set('window_seconds', Number(e.target.value))} className={inputClass} />
              </SettingField>
            </div>
            <SettingField
              label={intl.formatMessage({ id: 'gov.rate.action' })}
              help={intl.formatMessage({ id: 'gov.rate.action.help', defaultMessage: '超過上限時怎麼處理：直接擋下、只記錄警告、或放慢速度。' })}
            >
              <OptionSelect
                value={policy.action_on_violation ?? 'reject'}
                onChange={(v) => set('action_on_violation', v as GovAction)}
                options={GOV_ACTIONS.map((a) => ({ value: a, label: actionLabels[a], raw: a }))}
              />
            </SettingField>
          </>
        )}

        {policy.policy_type === 'permission' && (
          <>
            <SettingField
              label={intl.formatMessage({ id: 'gov.perm.allowed' })}
              help={intl.formatMessage({ id: 'gov.perm.allowed.help', defaultMessage: '只允許使用這些權限；留空代表不特別限制。範例格式見下方。' })}
            >
              <ChipEditor values={policy.allowed_scopes ?? []} onChange={(v) => set('allowed_scopes', v)} placeholder="memory:read" addLabel={intl.formatMessage({ id: 'common.add' })} />
            </SettingField>
            <SettingField
              label={intl.formatMessage({ id: 'gov.perm.denied' })}
              help={intl.formatMessage({ id: 'gov.perm.denied.help', defaultMessage: '明確禁止的權限。即使在允許清單內，出現在這裡也會被擋。' })}
            >
              <ChipEditor values={policy.denied_scopes ?? []} onChange={(v) => set('denied_scopes', v)} placeholder="odoo:write" addLabel={intl.formatMessage({ id: 'common.add' })} />
            </SettingField>
            <SettingField
              label={intl.formatMessage({ id: 'gov.perm.requiresApproval' })}
              help={intl.formatMessage({ id: 'gov.perm.requiresApproval.help', defaultMessage: '這些權限每次使用前都要先經人工核准，適合高風險操作。' })}
            >
              <ChipEditor values={policy.requires_approval ?? []} onChange={(v) => set('requires_approval', v)} placeholder="odoo:execute" addLabel={intl.formatMessage({ id: 'common.add' })} />
            </SettingField>
          </>
        )}

        {policy.policy_type === 'quota' && (
          <>
            <div className="grid grid-cols-2 gap-3">
              <SettingField
                label={intl.formatMessage({ id: 'gov.quota.dailyBudget' })}
                help={intl.formatMessage({ id: 'gov.quota.dailyBudget.help', defaultMessage: '每天可用的 token 總量。用完後當天暫停，隔天依重置排程歸零。' })}
              >
                <input type="number" min={1} value={policy.daily_token_budget ?? 0} onChange={(e) => set('daily_token_budget', Number(e.target.value))} className={inputClass} />
              </SettingField>
              <SettingField
                label={intl.formatMessage({ id: 'gov.quota.maxTasks' })}
                help={intl.formatMessage({ id: 'gov.quota.maxTasks.help', defaultMessage: '同一時間最多可同時進行的任務數。調高會更快，但也更耗資源。' })}
              >
                <input type="number" min={1} value={policy.max_concurrent_tasks ?? 0} onChange={(e) => set('max_concurrent_tasks', Number(e.target.value))} className={inputClass} />
              </SettingField>
              <SettingField
                label={intl.formatMessage({ id: 'gov.quota.maxMemory' })}
                help={intl.formatMessage({ id: 'gov.quota.maxMemory.help', defaultMessage: '可保留的記憶筆數上限。填 0 代表不限制。' })}
              >
                <input type="number" min={0} value={policy.max_memory_entries ?? 0} onChange={(e) => set('max_memory_entries', Number(e.target.value))} className={inputClass} />
              </SettingField>
              <SettingField
                label={intl.formatMessage({ id: 'gov.quota.resetCron' })}
                help={intl.formatMessage({ id: 'gov.quota.resetCron.help', defaultMessage: '配額歸零的排程（cron 格式）。預設 0 0 * * * 代表每天午夜重置。' })}
              >
                <input type="text" value={policy.reset_cron ?? '0 0 * * *'} onChange={(e) => set('reset_cron', e.target.value)} className={inputClass} />
              </SettingField>
            </div>
          </>
        )}

        {policy.policy_type === 'lifecycle' && (
          <>
            <div className="grid grid-cols-2 gap-3">
              <SettingField
                label={intl.formatMessage({ id: 'gov.lifecycle.maxIdle' })}
                help={intl.formatMessage({ id: 'gov.lifecycle.maxIdle.help', defaultMessage: '閒置超過這麼多小時就視為停擺。預設 168 小時（7 天）。' })}
              >
                <input type="number" min={1} value={policy.max_idle_hours ?? 0} onChange={(e) => set('max_idle_hours', Number(e.target.value))} className={inputClass} />
              </SettingField>
              <SettingField
                label={intl.formatMessage({ id: 'gov.lifecycle.healthCheck' })}
                help={intl.formatMessage({ id: 'gov.lifecycle.healthCheck.help', defaultMessage: '多久檢查一次健康狀態（秒）。調短偵測更即時，但檢查較頻繁。' })}
              >
                <input type="number" min={1} value={policy.health_check_interval_seconds ?? 0} onChange={(e) => set('health_check_interval_seconds', Number(e.target.value))} className={inputClass} />
              </SettingField>
              <SettingField
                label={intl.formatMessage({ id: 'gov.lifecycle.autoSuspend' })}
                help={intl.formatMessage({ id: 'gov.lifecycle.autoSuspend.help', defaultMessage: '累積違規達這個次數就自動暫停這個 AI 員工。填 0 代表關閉此功能。' })}
              >
                <input type="number" min={0} value={policy.auto_suspend_on_violation_count ?? 0} onChange={(e) => set('auto_suspend_on_violation_count', Number(e.target.value))} className={inputClass} />
              </SettingField>
            </div>
          </>
        )}

        {error && <p className="text-sm text-rose-600 dark:text-rose-400">{error}</p>}
        <div className="flex justify-end gap-3 border-t border-[var(--panel-border)] pt-4">
          <Button variant="secondary" onClick={onClose}>{intl.formatMessage({ id: 'common.cancel' })}</Button>
          <Button variant="primary" onClick={handleSubmit} disabled={submitting}>
            {submitting ? intl.formatMessage({ id: 'common.saving' }) : intl.formatMessage({ id: 'common.save' })}
          </Button>
        </div>
      </div>
    </Dialog>
  );
}

function RemoveDialog({
  policy,
  onClose,
  onRemoved,
}: {
  policy: GovPolicy | null;
  onClose: () => void;
  onRemoved: () => void;
}) {
  const intl = useIntl();
  const [confirming, setConfirming] = useState(false);

  if (!policy) return null;

  const handleConfirm = async () => {
    setConfirming(true);
    try {
      await api.governance.remove(policy.policy_id, policy.agent_id);
      toast.success(intl.formatMessage({ id: 'gov.removed' }));
      onRemoved();
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
    } finally {
      setConfirming(false);
    }
  };

  return (
    <Dialog open onClose={onClose} title={intl.formatMessage({ id: 'gov.remove.title' })}>
      <div className="space-y-4">
        <p className="text-sm text-stone-600 dark:text-stone-400">{intl.formatMessage({ id: 'gov.remove.confirm' })}</p>
        <p className="text-sm font-medium text-stone-900 dark:text-stone-50">{policy.policy_id} ({policy.scope ?? policy.agent_id})</p>
        <div className="flex justify-end gap-3 pt-2">
          <Button variant="secondary" onClick={onClose}>{intl.formatMessage({ id: 'common.cancel' })}</Button>
          <Button variant="danger" onClick={handleConfirm} disabled={confirming}>
            {confirming ? intl.formatMessage({ id: 'common.loading' }) : intl.formatMessage({ id: 'common.delete' })}
          </Button>
        </div>
      </div>
    </Dialog>
  );
}
