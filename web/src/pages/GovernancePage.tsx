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
import { Dialog, FormField, inputClass, selectClass } from '@/components/shared/Dialog';
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
          <EmptyState icon={Scale} title={intl.formatMessage({ id: 'gov.empty' })} />
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
                    <td className="px-5 py-2.5 font-medium text-stone-800 dark:text-stone-200">{p.policy_id}</td>
                    <td className="px-5 py-2.5">
                      <Badge tone={TYPE_TONES[p.policy_type]}>{p.policy_type}</Badge>
                    </td>
                    <td className="px-5 py-2.5 text-stone-600 dark:text-stone-400">{p.scope ?? p.agent_id}</td>
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
          <FormField label={intl.formatMessage({ id: 'gov.field.id' })} hint={intl.formatMessage({ id: 'gov.field.id.hint' })}>
            <input
              type="text"
              value={policy.policy_id}
              onChange={(e) => set('policy_id', e.target.value)}
              disabled={!isNew}
              placeholder="default-rate-mcp"
              className={inputClass}
            />
          </FormField>
          <FormField label={intl.formatMessage({ id: 'gov.field.type' })}>
            <select value={policy.policy_type} onChange={(e) => changeType(e.target.value as GovPolicyType)} disabled={!isNew} className={selectClass}>
              {GOV_POLICY_TYPES.map((t) => (
                <option key={t} value={t}>{t}</option>
              ))}
            </select>
          </FormField>
        </div>

        <FormField label={intl.formatMessage({ id: 'gov.field.agentId' })} hint={intl.formatMessage({ id: 'gov.field.agentId.hint' })}>
          <input type="text" value={policy.agent_id} onChange={(e) => set('agent_id', e.target.value)} placeholder="*" className={inputClass} />
        </FormField>

        {policy.policy_type === 'rate' && (
          <>
            <FormField label={intl.formatMessage({ id: 'gov.rate.resource' })}>
              <select value={policy.resource ?? 'mcp_calls'} onChange={(e) => set('resource', e.target.value as GovRateResource)} className={selectClass}>
                {GOV_RATE_RESOURCES.map((r) => (
                  <option key={r} value={r}>{r}</option>
                ))}
              </select>
            </FormField>
            <div className="grid grid-cols-2 gap-3">
              <FormField label={intl.formatMessage({ id: 'gov.rate.limit' })}>
                <input type="number" min={1} value={policy.limit ?? 0} onChange={(e) => set('limit', Number(e.target.value))} className={inputClass} />
              </FormField>
              <FormField label={intl.formatMessage({ id: 'gov.rate.window' })}>
                <input type="number" min={1} value={policy.window_seconds ?? 0} onChange={(e) => set('window_seconds', Number(e.target.value))} className={inputClass} />
              </FormField>
            </div>
            <FormField label={intl.formatMessage({ id: 'gov.rate.action' })}>
              <select value={policy.action_on_violation ?? 'reject'} onChange={(e) => set('action_on_violation', e.target.value as GovAction)} className={selectClass}>
                {GOV_ACTIONS.map((a) => (
                  <option key={a} value={a}>{a}</option>
                ))}
              </select>
            </FormField>
          </>
        )}

        {policy.policy_type === 'permission' && (
          <>
            <FormField label={intl.formatMessage({ id: 'gov.perm.allowed' })} hint={intl.formatMessage({ id: 'gov.perm.scope.hint' })}>
              <ChipEditor values={policy.allowed_scopes ?? []} onChange={(v) => set('allowed_scopes', v)} placeholder="memory:read" addLabel={intl.formatMessage({ id: 'common.add' })} />
            </FormField>
            <FormField label={intl.formatMessage({ id: 'gov.perm.denied' })} hint={intl.formatMessage({ id: 'gov.perm.scope.hint' })}>
              <ChipEditor values={policy.denied_scopes ?? []} onChange={(v) => set('denied_scopes', v)} placeholder="odoo:write" addLabel={intl.formatMessage({ id: 'common.add' })} />
            </FormField>
            <FormField label={intl.formatMessage({ id: 'gov.perm.requiresApproval' })}>
              <ChipEditor values={policy.requires_approval ?? []} onChange={(v) => set('requires_approval', v)} placeholder="odoo:execute" addLabel={intl.formatMessage({ id: 'common.add' })} />
            </FormField>
          </>
        )}

        {policy.policy_type === 'quota' && (
          <>
            <div className="grid grid-cols-2 gap-3">
              <FormField label={intl.formatMessage({ id: 'gov.quota.dailyBudget' })}>
                <input type="number" min={1} value={policy.daily_token_budget ?? 0} onChange={(e) => set('daily_token_budget', Number(e.target.value))} className={inputClass} />
              </FormField>
              <FormField label={intl.formatMessage({ id: 'gov.quota.maxTasks' })}>
                <input type="number" min={1} value={policy.max_concurrent_tasks ?? 0} onChange={(e) => set('max_concurrent_tasks', Number(e.target.value))} className={inputClass} />
              </FormField>
              <FormField label={intl.formatMessage({ id: 'gov.quota.maxMemory' })} hint="0 = unlimited">
                <input type="number" min={0} value={policy.max_memory_entries ?? 0} onChange={(e) => set('max_memory_entries', Number(e.target.value))} className={inputClass} />
              </FormField>
              <FormField label={intl.formatMessage({ id: 'gov.quota.resetCron' })}>
                <input type="text" value={policy.reset_cron ?? '0 0 * * *'} onChange={(e) => set('reset_cron', e.target.value)} className={inputClass} />
              </FormField>
            </div>
          </>
        )}

        {policy.policy_type === 'lifecycle' && (
          <>
            <div className="grid grid-cols-2 gap-3">
              <FormField label={intl.formatMessage({ id: 'gov.lifecycle.maxIdle' })}>
                <input type="number" min={1} value={policy.max_idle_hours ?? 0} onChange={(e) => set('max_idle_hours', Number(e.target.value))} className={inputClass} />
              </FormField>
              <FormField label={intl.formatMessage({ id: 'gov.lifecycle.healthCheck' })}>
                <input type="number" min={1} value={policy.health_check_interval_seconds ?? 0} onChange={(e) => set('health_check_interval_seconds', Number(e.target.value))} className={inputClass} />
              </FormField>
              <FormField label={intl.formatMessage({ id: 'gov.lifecycle.autoSuspend' })} hint="0 = off">
                <input type="number" min={0} value={policy.auto_suspend_on_violation_count ?? 0} onChange={(e) => set('auto_suspend_on_violation_count', Number(e.target.value))} className={inputClass} />
              </FormField>
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
