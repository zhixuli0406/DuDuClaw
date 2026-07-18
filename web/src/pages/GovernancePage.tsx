import { useEffect, useState, useCallback, useMemo, type ReactNode } from 'react';
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
import { ChipEditor } from '@/components/shared/ChipEditor';
import { ConfirmDialog } from '@/components/settings/controls';
import { toast, formatError } from '@/lib/toast';
import { Scale, Plus, Trash2, Pencil, MoreHorizontal } from 'lucide-react';
import {
  Button,
  Badge,
  type BadgeProps,
  Input,
  Select,
  SelectTrigger,
  SelectValue,
  SelectContent,
  SelectItem,
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogFooter,
  DropdownMenu,
  DropdownMenuTrigger,
  DropdownMenuContent,
  DropdownMenuItem,
  Segmented,
  type SegmentedOption,
  Empty,
  ListGridContainer,
  ListGridHeader,
  ListGridHeaderCell,
  ListGridRow,
  ListGridCell,
} from '@/components/mds';

type TypeFilter = 'all' | GovPolicyType;

/** Badge tone per policy type (spec: rate=info, permission=accent, quota=warning, lifecycle=success). */
const TYPE_BADGE: Record<GovPolicyType, { variant: BadgeProps['variant']; className?: string }> = {
  rate: { variant: 'secondary' },
  permission: { variant: 'outline' },
  quota: { variant: 'secondary', className: 'bg-warning/15 text-warning' },
  lifecycle: { variant: 'secondary', className: 'bg-success/15 text-success' },
};

const POLICY_COLUMNS = 'minmax(0,1fr) minmax(0,0.7fr) minmax(0,0.9fr) minmax(0,1.6fr) 2.5rem';

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
  const [removeBusy, setRemoveBusy] = useState(false);
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

  const filterOptions: SegmentedOption<TypeFilter>[] = useMemo(
    () => [
      { value: 'all', label: `${intl.formatMessage({ id: 'tasks.filter.all' })} (${policies.length})` },
      ...GOV_POLICY_TYPES.map((t) => ({
        value: t,
        label: `${t} (${policies.filter((p) => p.policy_type === t).length})`,
      })),
    ],
    [intl, policies]
  );

  const visiblePolicies = useMemo(
    () => (filter === 'all' ? policies : policies.filter((p) => p.policy_type === filter)),
    [filter, policies]
  );

  const handleRemove = async () => {
    if (!removing) return;
    setRemoveBusy(true);
    try {
      await api.governance.remove(removing.policy_id, removing.agent_id);
      toast.success(intl.formatMessage({ id: 'gov.removed' }));
      setRemoving(null);
      fetchPolicies();
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
    } finally {
      setRemoveBusy(false);
    }
  };

  return (
    <div className="mx-auto w-full max-w-[1200px] space-y-6">
      {/* Slim header */}
      <div className="flex items-center justify-between gap-3">
        <div className="flex min-w-0 items-center gap-2">
          <Scale className="size-5 text-muted-foreground" />
          <div>
            <h1 className="text-base font-medium">{intl.formatMessage({ id: 'nav.governance' })}</h1>
            <p className="text-sm text-muted-foreground">{intl.formatMessage({ id: 'gov.desc' })}</p>
          </div>
        </div>
        <Button
          variant="brand"
          size="sm"
          onClick={() => setEditing({ policy: defaultPolicy('rate'), isNew: true })}
        >
          <Plus />
          <span className="hidden sm:inline">{intl.formatMessage({ id: 'gov.add' })}</span>
        </Button>
      </div>

      {/* Type filter (all / rate / permission / quota / lifecycle) with counts */}
      <Segmented
        value={filter}
        onValueChange={setFilter}
        options={filterOptions}
        aria-label={intl.formatMessage({ id: 'nav.governance' })}
      />

      {loading ? (
        <p className="py-8 text-center text-sm text-muted-foreground">{intl.formatMessage({ id: 'common.loading' })}</p>
      ) : visiblePolicies.length === 0 ? (
        <Empty icon={Scale} title={intl.formatMessage({ id: 'gov.empty' })} />
      ) : (
        <div className="overflow-hidden rounded-xl border border-surface-border">
          <ListGridContainer
            columns={POLICY_COLUMNS}
            className="!h-auto [&>[aria-hidden]]:hidden"
            header={
              <ListGridHeader>
                <ListGridHeaderCell>{intl.formatMessage({ id: 'gov.col.id' })}</ListGridHeaderCell>
                <ListGridHeaderCell>{intl.formatMessage({ id: 'gov.col.type' })}</ListGridHeaderCell>
                <ListGridHeaderCell>{intl.formatMessage({ id: 'gov.col.scope' })}</ListGridHeaderCell>
                <ListGridHeaderCell>{intl.formatMessage({ id: 'gov.col.detail' })}</ListGridHeaderCell>
                <ListGridHeaderCell aria-hidden />
              </ListGridHeader>
            }
          >
            {visiblePolicies.map((p) => (
              <PolicyRow
                key={`${p.scope ?? p.agent_id}:${p.policy_id}`}
                policy={p}
                onEdit={() => setEditing({ policy: { ...p }, isNew: false })}
                onRemove={() => setRemoving(p)}
              />
            ))}
          </ListGridContainer>
        </div>
      )}

      {editing && (
        <PolicyDialog
          initial={editing.policy}
          isNew={editing.isNew}
          onClose={() => setEditing(null)}
          onSaved={() => { setEditing(null); fetchPolicies(); }}
        />
      )}

      {/* Destructive remove confirmation */}
      <ConfirmDialog
        open={removing !== null}
        onClose={() => setRemoving(null)}
        onConfirm={handleRemove}
        title={intl.formatMessage({ id: 'gov.remove.title' })}
        message={
          removing
            ? `${intl.formatMessage({ id: 'gov.remove.confirm' })} ${removing.policy_id} (${removing.scope ?? removing.agent_id})`
            : ''
        }
        confirmLabel={intl.formatMessage({ id: 'common.delete' })}
        busy={removeBusy}
      />
    </div>
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

/** One policy row: id · type badge · scope · detail · kebab (edit/delete). */
function PolicyRow({
  policy,
  onEdit,
  onRemove,
}: {
  policy: GovPolicy;
  onEdit: () => void;
  onRemove: () => void;
}) {
  const intl = useIntl();
  const tone = TYPE_BADGE[policy.policy_type];
  const detail = policyDetail(policy);
  return (
    <ListGridRow className="cursor-default">
      <ListGridCell className="font-mono text-xs text-foreground" title={policy.policy_id}>
        {policy.policy_id}
      </ListGridCell>
      <ListGridCell>
        <Badge variant={tone.variant} className={tone.className}>{policy.policy_type}</Badge>
      </ListGridCell>
      <ListGridCell className="font-mono text-xs text-muted-foreground" title={policy.scope ?? policy.agent_id}>
        {policy.scope ?? policy.agent_id}
      </ListGridCell>
      <ListGridCell className="truncate text-xs text-muted-foreground" title={detail}>
        {detail}
      </ListGridCell>
      <ListGridCell className="justify-end">
        <DropdownMenu>
          <DropdownMenuTrigger
            render={
              <Button
                variant="ghost"
                size="icon-sm"
                aria-label={intl.formatMessage({ id: 'common.more' })}
                data-stop-row-nav
              />
            }
          >
            <MoreHorizontal />
          </DropdownMenuTrigger>
          <DropdownMenuContent>
            <DropdownMenuItem onClick={onEdit}>
              <Pencil />
              {intl.formatMessage({ id: 'common.edit' })}
            </DropdownMenuItem>
            <DropdownMenuItem variant="destructive" onClick={onRemove}>
              <Trash2 />
              {intl.formatMessage({ id: 'common.delete' })}
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      </ListGridCell>
    </ListGridRow>
  );
}

/** Stacked label + control block used across the policy dialog (spec §5.3). */
function DialogField({
  label,
  help,
  children,
}: {
  label: string;
  help?: string;
  children: ReactNode;
}) {
  return (
    <div className="space-y-1.5">
      <label className="text-sm font-medium text-foreground">{label}</label>
      {children}
      {help && <p className="text-xs text-muted-foreground">{help}</p>}
    </div>
  );
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

  // Plain-language labels for the technical enum values. The written payload
  // is unchanged — only the visible label is friendlier.
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
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>
            {isNew ? intl.formatMessage({ id: 'gov.add' }) : intl.formatMessage({ id: 'gov.edit' })}
          </DialogTitle>
        </DialogHeader>

        <div className="max-h-[60vh] space-y-4 overflow-y-auto">
          <div className="grid grid-cols-2 gap-3">
            <DialogField
              label={intl.formatMessage({ id: 'gov.field.id' })}
              help={intl.formatMessage({ id: 'gov.field.id.hint' })}
            >
              <Input
                type="text"
                value={policy.policy_id}
                onChange={(e) => set('policy_id', e.target.value)}
                disabled={!isNew}
                placeholder="default-rate-mcp"
              />
            </DialogField>
            <DialogField
              label={intl.formatMessage({ id: 'gov.field.type' })}
              help={intl.formatMessage({ id: 'gov.field.type.help', defaultMessage: '這條規則要管什麼：頻率、權限、配額或生命週期。建立後不可更改。' })}
            >
              <Select value={policy.policy_type} onValueChange={(v) => changeType(v as GovPolicyType)} disabled={!isNew}>
                <SelectTrigger className="w-full">
                  <SelectValue>{typeLabels[policy.policy_type]}</SelectValue>
                </SelectTrigger>
                <SelectContent>
                  {GOV_POLICY_TYPES.map((t) => (
                    <SelectItem key={t} value={t}>{typeLabels[t]}</SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </DialogField>
          </div>

          <DialogField
            label={intl.formatMessage({ id: 'gov.field.agentId' })}
            help={intl.formatMessage({ id: 'gov.field.agentId.hint' })}
          >
            <Input type="text" value={policy.agent_id} onChange={(e) => set('agent_id', e.target.value)} placeholder="*" />
          </DialogField>

          {policy.policy_type === 'rate' && (
            <>
              <DialogField
                label={intl.formatMessage({ id: 'gov.rate.resource' })}
                help={intl.formatMessage({ id: 'gov.rate.resource.help', defaultMessage: '要計數的動作。超過下面設定的次數時，就會觸發違規動作。' })}
              >
                <Select value={policy.resource ?? 'mcp_calls'} onValueChange={(v) => set('resource', v as GovRateResource)}>
                  <SelectTrigger className="w-full">
                    <SelectValue>{resourceLabels[policy.resource ?? 'mcp_calls']}</SelectValue>
                  </SelectTrigger>
                  <SelectContent>
                    {GOV_RATE_RESOURCES.map((r) => (
                      <SelectItem key={r} value={r}>{resourceLabels[r]}</SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </DialogField>
              <div className="grid grid-cols-2 gap-3">
                <DialogField
                  label={intl.formatMessage({ id: 'gov.rate.limit' })}
                  help={intl.formatMessage({ id: 'gov.rate.limit.help', defaultMessage: '在時間視窗內允許的最多次數。調高＝更寬鬆，調低＝更嚴格。' })}
                >
                  <Input type="number" min={1} value={policy.limit ?? 0} onChange={(e) => set('limit', Number(e.target.value))} />
                </DialogField>
                <DialogField
                  label={intl.formatMessage({ id: 'gov.rate.window' })}
                  help={intl.formatMessage({ id: 'gov.rate.window.help', defaultMessage: '計數的時間長度（秒）。例如 60 代表「每分鐘」計算一次上限。' })}
                >
                  <Input type="number" min={1} value={policy.window_seconds ?? 0} onChange={(e) => set('window_seconds', Number(e.target.value))} />
                </DialogField>
              </div>
              <DialogField
                label={intl.formatMessage({ id: 'gov.rate.action' })}
                help={intl.formatMessage({ id: 'gov.rate.action.help', defaultMessage: '超過上限時怎麼處理：直接擋下、只記錄警告、或放慢速度。' })}
              >
                <Select value={policy.action_on_violation ?? 'reject'} onValueChange={(v) => set('action_on_violation', v as GovAction)}>
                  <SelectTrigger className="w-full">
                    <SelectValue>{actionLabels[policy.action_on_violation ?? 'reject']}</SelectValue>
                  </SelectTrigger>
                  <SelectContent>
                    {GOV_ACTIONS.map((a) => (
                      <SelectItem key={a} value={a}>{actionLabels[a]}</SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </DialogField>
            </>
          )}

          {policy.policy_type === 'permission' && (
            <>
              <DialogField
                label={intl.formatMessage({ id: 'gov.perm.allowed' })}
                help={intl.formatMessage({ id: 'gov.perm.allowed.help', defaultMessage: '只允許使用這些權限；留空代表不特別限制。範例格式見下方。' })}
              >
                <ChipEditor values={policy.allowed_scopes ?? []} onChange={(v) => set('allowed_scopes', v)} placeholder="memory:read" addLabel={intl.formatMessage({ id: 'common.add' })} />
              </DialogField>
              <DialogField
                label={intl.formatMessage({ id: 'gov.perm.denied' })}
                help={intl.formatMessage({ id: 'gov.perm.denied.help', defaultMessage: '明確禁止的權限。即使在允許清單內，出現在這裡也會被擋。' })}
              >
                <ChipEditor values={policy.denied_scopes ?? []} onChange={(v) => set('denied_scopes', v)} placeholder="odoo:write" addLabel={intl.formatMessage({ id: 'common.add' })} />
              </DialogField>
              <DialogField
                label={intl.formatMessage({ id: 'gov.perm.requiresApproval' })}
                help={intl.formatMessage({ id: 'gov.perm.requiresApproval.help', defaultMessage: '這些權限每次使用前都要先經人工核准，適合高風險操作。' })}
              >
                <ChipEditor values={policy.requires_approval ?? []} onChange={(v) => set('requires_approval', v)} placeholder="odoo:execute" addLabel={intl.formatMessage({ id: 'common.add' })} />
              </DialogField>
            </>
          )}

          {policy.policy_type === 'quota' && (
            <div className="grid grid-cols-2 gap-3">
              <DialogField
                label={intl.formatMessage({ id: 'gov.quota.dailyBudget' })}
                help={intl.formatMessage({ id: 'gov.quota.dailyBudget.help', defaultMessage: '每天可用的 token 總量。用完後當天暫停，隔天依重置排程歸零。' })}
              >
                <Input type="number" min={1} value={policy.daily_token_budget ?? 0} onChange={(e) => set('daily_token_budget', Number(e.target.value))} />
              </DialogField>
              <DialogField
                label={intl.formatMessage({ id: 'gov.quota.maxTasks' })}
                help={intl.formatMessage({ id: 'gov.quota.maxTasks.help', defaultMessage: '同一時間最多可同時進行的任務數。調高會更快，但也更耗資源。' })}
              >
                <Input type="number" min={1} value={policy.max_concurrent_tasks ?? 0} onChange={(e) => set('max_concurrent_tasks', Number(e.target.value))} />
              </DialogField>
              <DialogField
                label={intl.formatMessage({ id: 'gov.quota.maxMemory' })}
                help={intl.formatMessage({ id: 'gov.quota.maxMemory.help', defaultMessage: '可保留的記憶筆數上限。填 0 代表不限制。' })}
              >
                <Input type="number" min={0} value={policy.max_memory_entries ?? 0} onChange={(e) => set('max_memory_entries', Number(e.target.value))} />
              </DialogField>
              <DialogField
                label={intl.formatMessage({ id: 'gov.quota.resetCron' })}
                help={intl.formatMessage({ id: 'gov.quota.resetCron.help', defaultMessage: '配額歸零的排程（cron 格式）。預設 0 0 * * * 代表每天午夜重置。' })}
              >
                <Input type="text" value={policy.reset_cron ?? '0 0 * * *'} onChange={(e) => set('reset_cron', e.target.value)} />
              </DialogField>
            </div>
          )}

          {policy.policy_type === 'lifecycle' && (
            <div className="grid grid-cols-2 gap-3">
              <DialogField
                label={intl.formatMessage({ id: 'gov.lifecycle.maxIdle' })}
                help={intl.formatMessage({ id: 'gov.lifecycle.maxIdle.help', defaultMessage: '閒置超過這麼多小時就視為停擺。預設 168 小時（7 天）。' })}
              >
                <Input type="number" min={1} value={policy.max_idle_hours ?? 0} onChange={(e) => set('max_idle_hours', Number(e.target.value))} />
              </DialogField>
              <DialogField
                label={intl.formatMessage({ id: 'gov.lifecycle.healthCheck' })}
                help={intl.formatMessage({ id: 'gov.lifecycle.healthCheck.help', defaultMessage: '多久檢查一次健康狀態（秒）。調短偵測更即時，但檢查較頻繁。' })}
              >
                <Input type="number" min={1} value={policy.health_check_interval_seconds ?? 0} onChange={(e) => set('health_check_interval_seconds', Number(e.target.value))} />
              </DialogField>
              <DialogField
                label={intl.formatMessage({ id: 'gov.lifecycle.autoSuspend' })}
                help={intl.formatMessage({ id: 'gov.lifecycle.autoSuspend.help', defaultMessage: '累積違規達這個次數就自動暫停這個 AI 員工。填 0 代表關閉此功能。' })}
              >
                <Input type="number" min={0} value={policy.auto_suspend_on_violation_count ?? 0} onChange={(e) => set('auto_suspend_on_violation_count', Number(e.target.value))} />
              </DialogField>
            </div>
          )}

          {error && <p className="text-sm text-destructive">{error}</p>}
        </div>

        <DialogFooter>
          <Button variant="outline" onClick={onClose}>{intl.formatMessage({ id: 'common.cancel' })}</Button>
          <Button variant="brand" onClick={handleSubmit} disabled={submitting}>
            {submitting ? intl.formatMessage({ id: 'common.saving' }) : intl.formatMessage({ id: 'common.save' })}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
