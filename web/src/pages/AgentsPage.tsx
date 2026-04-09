import { useEffect, useState, useCallback } from 'react';
import { useIntl } from 'react-intl';
import { useAgentsStore } from '@/stores/agents-store';
import { cn } from '@/lib/utils';
import { api, type AgentDetail, type AgentUpdateParams } from '@/lib/api';
import { Dialog, FormField, inputClass, selectClass, buttonPrimary, buttonSecondary } from '@/components/shared/Dialog';
import { Bot, Pause, Play, Send, Eye, Plus, X, ShieldCheck, Pencil, Trash2 } from 'lucide-react';

function StatusBadge({ status }: { status: string }) {
  const intl = useIntl();
  const styles: Record<string, string> = {
    active: 'bg-emerald-100 text-emerald-700 dark:bg-emerald-900/30 dark:text-emerald-400',
    paused: 'bg-amber-100 text-amber-700 dark:bg-amber-900/30 dark:text-amber-400',
    terminated: 'bg-rose-100 text-rose-700 dark:bg-rose-900/30 dark:text-rose-400',
  };

  return (
    <span className={cn('inline-flex items-center rounded-full px-2.5 py-0.5 text-xs font-medium', styles[status] ?? 'bg-stone-100 text-stone-600')}>
      {intl.formatMessage({ id: `status.${status}` })}
    </span>
  );
}

function RoleBadge({ role }: { role: string }) {
  const intl = useIntl();
  return (
    <span className="inline-flex items-center rounded-full bg-stone-100 px-2 py-0.5 text-xs text-stone-600 dark:bg-stone-800 dark:text-stone-400">
      {intl.formatMessage({ id: `agents.role.${role}` })}
    </span>
  );
}

export function AgentsPage() {
  const intl = useIntl();
  const { agents, fetchAgents, pauseAgent, resumeAgent, removeAgent, loading } = useAgentsStore();
  const [showCreateDialog, setShowCreateDialog] = useState(false);
  const [delegateTarget, setDelegateTarget] = useState<string | null>(null);
  const [inspectTarget, setInspectTarget] = useState<AgentDetail | null>(null);
  const [editTarget, setEditTarget] = useState<AgentDetail | null>(null);
  const [removeTarget, setRemoveTarget] = useState<string | null>(null);

  useEffect(() => {
    fetchAgents();
  }, [fetchAgents]);

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <h2 className="text-2xl font-semibold text-stone-900 dark:text-stone-50">
          {intl.formatMessage({ id: 'agents.title' })}
        </h2>
        <button
          onClick={() => setShowCreateDialog(true)}
          className="inline-flex items-center gap-2 rounded-lg bg-amber-500 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-amber-600"
        >
          <Plus className="h-4 w-4" />
          {intl.formatMessage({ id: 'agents.create' })}
        </button>
      </div>

      {agents.length === 0 && !loading ? (
        <div className="flex flex-col items-center justify-center rounded-xl border border-dashed border-stone-300 bg-white py-16 dark:border-stone-700 dark:bg-stone-900">
          <Bot className="mb-4 h-12 w-12 text-stone-300 dark:text-stone-600" />
          <p className="text-stone-500 dark:text-stone-400">
            {intl.formatMessage({ id: 'agents.empty' })}
          </p>
        </div>
      ) : (
        <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
          {agents.map((agent) => (
            <div
              key={agent.name}
              className="rounded-xl border border-stone-200 bg-white p-5 transition-shadow hover:shadow-md dark:border-stone-800 dark:bg-stone-900"
            >
              <div className="flex items-start justify-between">
                <div className="flex items-center gap-3">
                  <span className="text-2xl">{agent.icon || '🤖'}</span>
                  <div>
                    <h3 className="font-semibold text-stone-900 dark:text-stone-50">{agent.display_name}</h3>
                    <p className="text-xs text-stone-500 dark:text-stone-400">{agent.trigger}</p>
                  </div>
                </div>
                <StatusBadge status={agent.status} />
              </div>

              <div className="mt-3 flex items-center gap-2">
                <RoleBadge role={agent.role} />
                {agent.sandbox_enabled && (
                  <span className="inline-flex items-center gap-1 rounded-full bg-blue-100 px-2 py-0.5 text-xs font-medium text-blue-700 dark:bg-blue-900/30 dark:text-blue-400">
                    <ShieldCheck className="h-3 w-3" />
                    {intl.formatMessage({ id: 'agents.sandboxed' })}
                  </span>
                )}
              </div>

              {agent.budget && (
                <div className="mt-4">
                  <div className="mb-1 flex justify-between text-xs text-stone-500 dark:text-stone-400">
                    <span>{intl.formatMessage({ id: 'dashboard.budget.title' })}</span>
                    <span>
                      ${(agent.budget.spent_cents / 100).toFixed(2)} / ${(agent.budget.monthly_limit_cents / 100).toFixed(2)}
                    </span>
                  </div>
                  <div className="h-1.5 overflow-hidden rounded-full bg-stone-200 dark:bg-stone-700">
                    <div
                      className="h-full rounded-full bg-amber-500 transition-all"
                      style={{ width: `${Math.min(100, (agent.budget.spent_cents / agent.budget.monthly_limit_cents) * 100)}%` }}
                    />
                  </div>
                </div>
              )}

              <div className="mt-4 flex gap-2 border-t border-stone-100 pt-3 dark:border-stone-800">
                {agent.status === 'active' ? (
                  <button
                    onClick={() => pauseAgent(agent.name)}
                    className="inline-flex items-center gap-1 rounded-md px-2.5 py-1.5 text-xs text-stone-600 hover:bg-stone-100 dark:text-stone-400 dark:hover:bg-stone-800"
                  >
                    <Pause className="h-3.5 w-3.5" />
                    {intl.formatMessage({ id: 'agents.pause' })}
                  </button>
                ) : (
                  <button
                    onClick={() => resumeAgent(agent.name)}
                    className="inline-flex items-center gap-1 rounded-md px-2.5 py-1.5 text-xs text-emerald-600 hover:bg-emerald-50 dark:text-emerald-400 dark:hover:bg-emerald-900/20"
                  >
                    <Play className="h-3.5 w-3.5" />
                    {intl.formatMessage({ id: 'agents.resume' })}
                  </button>
                )}
                <button
                  onClick={() => setDelegateTarget(agent.name)}
                  className="inline-flex items-center gap-1 rounded-md px-2.5 py-1.5 text-xs text-stone-600 hover:bg-stone-100 dark:text-stone-400 dark:hover:bg-stone-800"
                >
                  <Send className="h-3.5 w-3.5" />
                  {intl.formatMessage({ id: 'agents.delegate' })}
                </button>
                <button
                  onClick={() => setInspectTarget(agent)}
                  className="inline-flex items-center gap-1 rounded-md px-2.5 py-1.5 text-xs text-stone-600 hover:bg-stone-100 dark:text-stone-400 dark:hover:bg-stone-800"
                >
                  <Eye className="h-3.5 w-3.5" />
                  {intl.formatMessage({ id: 'agents.inspect' })}
                </button>
                <button
                  onClick={() => setEditTarget(agent)}
                  className="inline-flex items-center gap-1 rounded-md px-2.5 py-1.5 text-xs text-stone-600 hover:bg-stone-100 dark:text-stone-400 dark:hover:bg-stone-800"
                >
                  <Pencil className="h-3.5 w-3.5" />
                  {intl.formatMessage({ id: 'agents.edit' })}
                </button>
                {agent.role !== 'main' && (
                  <button
                    onClick={() => setRemoveTarget(agent.name)}
                    className="inline-flex items-center gap-1 rounded-md px-2.5 py-1.5 text-xs text-rose-600 hover:bg-rose-50 dark:text-rose-400 dark:hover:bg-rose-900/20"
                  >
                    <Trash2 className="h-3.5 w-3.5" />
                  </button>
                )}
              </div>
            </div>
          ))}
        </div>
      )}

      {/* Create Agent Dialog */}
      <CreateAgentDialog
        open={showCreateDialog}
        onClose={() => setShowCreateDialog(false)}
        onCreated={fetchAgents}
      />

      {/* Delegate Dialog */}
      <DelegateDialog
        open={delegateTarget !== null}
        agentName={delegateTarget ?? ''}
        onClose={() => setDelegateTarget(null)}
      />

      {/* Inspect Panel */}
      <InspectDialog
        agent={inspectTarget}
        onClose={() => setInspectTarget(null)}
        onEdit={(agent) => { setInspectTarget(null); setEditTarget(agent); }}
      />

      {/* Edit Agent Dialog */}
      <EditAgentDialog
        agent={editTarget}
        onClose={() => setEditTarget(null)}
        onSaved={() => { setEditTarget(null); fetchAgents(); }}
      />

      {/* Remove Confirm Dialog */}
      <RemoveConfirmDialog
        agentName={removeTarget}
        onClose={() => setRemoveTarget(null)}
        onConfirm={async () => {
          if (removeTarget) {
            await removeAgent(removeTarget);
            setRemoveTarget(null);
          }
        }}
      />
    </div>
  );
}

function CreateAgentDialog({ open, onClose, onCreated }: { open: boolean; onClose: () => void; onCreated: () => void }) {
  const intl = useIntl();
  const [name, setName] = useState('');
  const [displayName, setDisplayName] = useState('');
  const [role, setRole] = useState('specialist');
  const [trigger, setTrigger] = useState('');
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const handleSubmit = async () => {
    if (!name.trim() || !displayName.trim()) return;
    setError(null);
    setSubmitting(true);
    try {
      await api.agents.create({ name: name.trim(), display_name: displayName.trim(), role, trigger: trigger || `@${displayName.trim()}` });
      onCreated();
      onClose();
      setName('');
      setDisplayName('');
      setRole('specialist');
      setTrigger('');
    } catch {
      setError(intl.formatMessage({ id: 'agents.create.error' }));
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <Dialog open={open} onClose={onClose} title={intl.formatMessage({ id: 'agents.create' })}>
      <div className="space-y-4">
        <FormField label="Agent ID" hint={intl.formatMessage({ id: 'agents.create.idHint' })}>
          <input type="text" value={name} onChange={(e) => setName(e.target.value)} placeholder="coder" className={inputClass} />
        </FormField>
        <FormField label={intl.formatMessage({ id: 'agents.create.displayName' })}>
          <input type="text" value={displayName} onChange={(e) => setDisplayName(e.target.value)} placeholder="Coder" className={inputClass} />
        </FormField>
        <FormField label={intl.formatMessage({ id: 'orgchart.detail.role' })}>
          <select value={role} onChange={(e) => setRole(e.target.value)} className={selectClass}>
            {['main', 'specialist', 'worker', 'developer', 'qa', 'planner'].map((r) => (
              <option key={r} value={r}>{intl.formatMessage({ id: `agents.role.${r}` })}</option>
            ))}
          </select>
        </FormField>
        <FormField label={intl.formatMessage({ id: 'orgchart.detail.trigger' })} hint={intl.formatMessage({ id: 'agents.create.triggerHint' })}>
          <input type="text" value={trigger} onChange={(e) => setTrigger(e.target.value)} placeholder="@Coder" className={inputClass} />
        </FormField>
        {error && (
          <p className="text-sm text-rose-600 dark:text-rose-400">{error}</p>
        )}
        <div className="flex justify-end gap-3 pt-2">
          <button onClick={onClose} className={buttonSecondary}>{intl.formatMessage({ id: 'common.cancel' })}</button>
          <button onClick={handleSubmit} disabled={submitting || !name.trim() || !displayName.trim()} className={buttonPrimary}>
            {submitting ? intl.formatMessage({ id: 'common.loading' }) : intl.formatMessage({ id: 'agents.create' })}
          </button>
        </div>
      </div>
    </Dialog>
  );
}

function DelegateDialog({ open, agentName, onClose }: { open: boolean; agentName: string; onClose: () => void }) {
  const intl = useIntl();
  const [prompt, setPrompt] = useState('');
  const [submitting, setSubmitting] = useState(false);
  const [result, setResult] = useState<string | null>(null);

  const handleSubmit = async () => {
    if (!prompt.trim()) return;
    setSubmitting(true);
    try {
      const res = await api.agents.delegate(agentName, prompt.trim());
      setResult(intl.formatMessage({ id: 'agents.delegate.success' }, { id: res.message_id }));
      setPrompt('');
    } catch {
      setResult(intl.formatMessage({ id: 'agents.delegate.error' }));
    } finally {
      setSubmitting(false);
    }
  };

  const handleClose = () => {
    setResult(null);
    setPrompt('');
    onClose();
  };

  return (
    <Dialog open={open} onClose={handleClose} title={intl.formatMessage({ id: 'agents.delegate.title' }, { name: agentName })}>
      <div className="space-y-4">
        {result && (
          <div className="rounded-lg bg-emerald-50 p-3 text-sm text-emerald-700 dark:bg-emerald-900/20 dark:text-emerald-400">
            {result}
          </div>
        )}
        <FormField label={intl.formatMessage({ id: 'agents.delegate.taskLabel' })}>
          <textarea
            value={prompt}
            onChange={(e) => setPrompt(e.target.value)}
            placeholder={intl.formatMessage({ id: 'agents.delegate.placeholder' })}
            rows={4}
            className={cn(inputClass, 'resize-none')}
          />
        </FormField>
        <div className="flex justify-end gap-3 pt-2">
          <button onClick={handleClose} className={buttonSecondary}>{intl.formatMessage({ id: 'agents.delegate.close' })}</button>
          <button onClick={handleSubmit} disabled={submitting || !prompt.trim()} className={buttonPrimary}>
            {submitting ? intl.formatMessage({ id: 'agents.delegate.submitting' }) : intl.formatMessage({ id: 'agents.delegate.submit' })}
          </button>
        </div>
      </div>
    </Dialog>
  );
}

function InspectDialog({ agent, onClose, onEdit }: { agent: AgentDetail | null; onClose: () => void; onEdit?: (agent: AgentDetail) => void }) {
  const intl = useIntl();
  if (!agent) return null;

  return (
    <Dialog open={agent !== null} onClose={onClose} title={`${agent.icon || '🤖'} ${agent.display_name}`} className="max-w-2xl">
      <div className="space-y-4 max-h-[60vh] overflow-y-auto">
        <Section title={intl.formatMessage({ id: 'agents.inspect.basicInfo' })}>
          <InfoRow label={intl.formatMessage({ id: 'agents.inspect.name' })} value={agent.name} />
          <InfoRow label={intl.formatMessage({ id: 'agents.inspect.role' })} value={agent.role} />
          <InfoRow label={intl.formatMessage({ id: 'agents.inspect.status' })} value={agent.status} />
          <InfoRow label={intl.formatMessage({ id: 'agents.inspect.trigger' })} value={agent.trigger} />
          <InfoRow label={intl.formatMessage({ id: 'agents.inspect.reportsTo' })} value={agent.reports_to || intl.formatMessage({ id: 'agents.inspect.noParent' })} />
        </Section>

        <Section title={intl.formatMessage({ id: 'agents.inspect.modelConfig' })}>
          <InfoRow label={intl.formatMessage({ id: 'agents.inspect.preferred' })} value={agent.model?.preferred ?? '—'} />
          <InfoRow label={intl.formatMessage({ id: 'agents.inspect.fallback' })} value={agent.model?.fallback ?? '—'} />
          <InfoRow label={intl.formatMessage({ id: 'agents.inspect.accountPool' })} value={agent.model?.account_pool?.join(', ') ?? '—'} />
        </Section>

        {agent.budget && (
          <Section title={intl.formatMessage({ id: 'agents.inspect.budget' })}>
            <InfoRow label={intl.formatMessage({ id: 'agents.inspect.monthlyLimit' })} value={`$${(agent.budget.monthly_limit_cents / 100).toFixed(2)}`} />
            <InfoRow label={intl.formatMessage({ id: 'agents.inspect.spent' })} value={`$${(agent.budget.spent_cents / 100).toFixed(2)}`} />
            <InfoRow label={intl.formatMessage({ id: 'agents.inspect.warnThreshold' })} value={`${agent.budget.warn_threshold_percent}%`} />
            <InfoRow label={intl.formatMessage({ id: 'agents.inspect.hardStop' })} value={agent.budget.hard_stop ? intl.formatMessage({ id: 'agents.inspect.hardStop.yes' }) : intl.formatMessage({ id: 'agents.inspect.hardStop.no' })} />
          </Section>
        )}

        {agent.skills && agent.skills.length > 0 && (
          <Section title={intl.formatMessage({ id: 'agents.inspect.skills' })}>
            <div className="flex flex-wrap gap-2">
              {agent.skills.map((s) => (
                <span key={s} className="rounded-full bg-amber-100 px-2.5 py-0.5 text-xs text-amber-700 dark:bg-amber-900/30 dark:text-amber-400">
                  {s}
                </span>
              ))}
            </div>
          </Section>
        )}

        <div className="flex justify-end gap-3 pt-2">
          {onEdit && (
            <button onClick={() => onEdit(agent)} className={buttonPrimary}>
              <Pencil className="h-4 w-4" /> {intl.formatMessage({ id: 'agents.edit' })}
            </button>
          )}
          <button onClick={onClose} className={buttonSecondary}>
            <X className="h-4 w-4" /> {intl.formatMessage({ id: 'common.cancel' })}
          </button>
        </div>
      </div>
    </Dialog>
  );
}

// ── Toggle component ──

function Toggle({ checked, onChange, label }: { checked: boolean; onChange: (v: boolean) => void; label: string }) {
  return (
    <label className="flex items-center justify-between py-1.5">
      <span className="text-sm text-stone-700 dark:text-stone-300">{label}</span>
      <button
        type="button"
        role="switch"
        aria-checked={checked}
        onClick={() => onChange(!checked)}
        className={cn(
          'relative inline-flex h-5 w-9 shrink-0 cursor-pointer rounded-full transition-colors',
          checked ? 'bg-amber-500' : 'bg-stone-300 dark:bg-stone-600'
        )}
      >
        <span
          className={cn(
            'pointer-events-none inline-block h-4 w-4 rounded-full bg-white shadow-sm transition-transform mt-0.5',
            checked ? 'translate-x-4 ml-0.5' : 'translate-x-0.5'
          )}
        />
      </button>
    </label>
  );
}

// ── Edit Agent Dialog ──

type EditTab = 'identity' | 'model' | 'heartbeat' | 'container' | 'permissions' | 'sticker' | 'channels';

function EditAgentDialog({ agent, onClose, onSaved }: { agent: AgentDetail | null; onClose: () => void; onSaved: () => void }) {
  const intl = useIntl();
  const { updateAgent } = useAgentsStore();
  const [tab, setTab] = useState<EditTab>('identity');
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Available models (cloud + local)
  const [availableModels, setAvailableModels] = useState<ReadonlyArray<{ id: string; label: string; type: 'cloud' | 'local'; file?: string }>>([]);

  // Local form state — initialized from agent when dialog opens
  const [form, setForm] = useState<AgentUpdateParams>({});

  useEffect(() => {
    if (agent) {
      // Fetch available models
      api.models.list().then((res) => setAvailableModels(res?.models ?? [])).catch(() => {});

      // Determine current preferred/fallback as unified IDs
      const localModel = agent.model?.local?.model ?? '';
      const preferLocal = agent.model?.local?.prefer_local ?? false;
      const currentPreferred = preferLocal && localModel
        ? `local:${localModel}`
        : agent.model?.preferred ?? 'claude-sonnet-4-6';
      const currentFallback = agent.model?.fallback ?? 'claude-haiku-4-5';

      setForm({
        display_name: agent.display_name,
        role: agent.role,
        trigger: agent.trigger,
        icon: agent.icon,
        reports_to: agent.reports_to,
        preferred: currentPreferred,
        fallback: currentFallback,
        api_mode: (agent.model?.api_mode ?? 'cli') as 'cli' | 'direct' | 'auto',
        local_model: localModel,
        local_backend: agent.model?.local?.backend ?? 'llama_cpp',
        local_context_length: agent.model?.local?.context_length ?? 4096,
        local_gpu_layers: agent.model?.local?.gpu_layers ?? -1,
        prefer_local: preferLocal,
        use_router: agent.model?.local?.use_router ?? false,
        monthly_limit_cents: agent.budget?.monthly_limit_cents ?? 5000,
        warn_threshold_percent: agent.budget?.warn_threshold_percent ?? 80,
        hard_stop: agent.budget?.hard_stop ?? true,
        heartbeat_enabled: agent.heartbeat?.enabled ?? false,
        heartbeat_interval: agent.heartbeat?.interval_seconds ?? 3600,
        heartbeat_cron: '',
        can_create_agents: agent.permissions?.can_create_agents ?? false,
        can_send_cross_agent: agent.permissions?.can_send_cross_agent ?? true,
        can_modify_own_skills: agent.permissions?.can_modify_own_skills ?? true,
        can_modify_own_soul: agent.permissions?.can_modify_own_soul ?? false,
        can_schedule_tasks: agent.permissions?.can_schedule_tasks ?? false,
        skill_auto_activate: false,
        skill_security_scan: true,
        gvu_enabled: true,
        cognitive_memory: false,
        sticker_enabled: agent.sticker?.enabled ?? false,
        sticker_probability: agent.sticker?.probability ?? 0.3,
        sticker_intensity_threshold: agent.sticker?.intensity_threshold ?? 0.7,
        sticker_cooldown_messages: agent.sticker?.cooldown_messages ?? 5,
        sticker_expressiveness: (agent.sticker?.expressiveness ?? 'moderate') as 'minimal' | 'moderate' | 'expressive',
      });
      setTab('identity');
      setError(null);
    }
  }, [agent]);

  const updateField = useCallback(<K extends keyof AgentUpdateParams>(key: K, value: AgentUpdateParams[K]) => {
    setForm((prev) => ({ ...prev, [key]: value }));
  }, []);

  const handleSave = async () => {
    if (!agent) return;
    setSaving(true);
    setError(null);
    try {
      // Decompose unified model IDs into cloud preferred + local config
      const submitForm = { ...form };
      const pref = submitForm.preferred ?? '';
      const fb = submitForm.fallback ?? '';

      if (pref.startsWith('local:')) {
        // Local model as preferred: set prefer_local + local_model, keep a cloud fallback
        submitForm.local_model = pref.replace('local:', '');
        submitForm.prefer_local = true;
        submitForm.preferred = fb.startsWith('local:') ? 'claude-sonnet-4-6' : (fb || 'claude-sonnet-4-6');
      } else {
        // Cloud model as preferred
        submitForm.prefer_local = false;
      }

      if (fb.startsWith('local:')) {
        submitForm.local_model = submitForm.local_model || fb.replace('local:', '');
        submitForm.fallback = 'claude-haiku-4-5';
      }

      await updateAgent(agent.name, submitForm);
      onSaved();
    } catch {
      setError(intl.formatMessage({ id: 'common.saveError' }));
    } finally {
      setSaving(false);
    }
  };

  if (!agent) return null;

  const tabs: ReadonlyArray<{ id: EditTab; label: string }> = [
    { id: 'identity', label: intl.formatMessage({ id: 'agents.edit.identity' }) },
    { id: 'model', label: intl.formatMessage({ id: 'agents.edit.model' }) },
    { id: 'heartbeat', label: intl.formatMessage({ id: 'agents.edit.heartbeat' }) },
    { id: 'container', label: intl.formatMessage({ id: 'settings.container' }) },
    { id: 'permissions', label: intl.formatMessage({ id: 'agents.edit.permissions' }) },
    { id: 'sticker', label: intl.formatMessage({ id: 'agents.edit.sticker' }) },
    { id: 'channels', label: intl.formatMessage({ id: 'channels.title' }) },
  ];

  return (
    <Dialog open={agent !== null} onClose={onClose} title={`${agent.icon || '🤖'} ${intl.formatMessage({ id: 'agents.edit' })}`} className="max-w-2xl">
      <div className="space-y-4">
        {/* Tab bar */}
        <div className="flex gap-1 rounded-lg bg-stone-100 p-1 dark:bg-stone-800">
          {tabs.map((t) => (
            <button
              key={t.id}
              onClick={() => setTab(t.id)}
              className={cn(
                'flex-1 rounded-md px-3 py-1.5 text-xs font-medium transition-colors',
                tab === t.id
                  ? 'bg-white text-stone-900 shadow-sm dark:bg-stone-700 dark:text-stone-50'
                  : 'text-stone-500 hover:text-stone-700 dark:text-stone-400'
              )}
            >
              {t.label}
            </button>
          ))}
        </div>

        {/* Tab content */}
        <div className="max-h-[50vh] overflow-y-auto space-y-4 pr-1">
          {tab === 'identity' && (
            <>
              <FormField label={intl.formatMessage({ id: 'agents.edit.displayName' })}>
                <input type="text" value={form.display_name ?? ''} onChange={(e) => updateField('display_name', e.target.value)} className={inputClass} />
              </FormField>
              <FormField label={intl.formatMessage({ id: 'agents.edit.role' })}>
                <select value={form.role ?? 'specialist'} onChange={(e) => updateField('role', e.target.value)} className={selectClass}>
                  {['main', 'specialist', 'worker', 'developer', 'qa', 'planner'].map((r) => (
                    <option key={r} value={r}>{intl.formatMessage({ id: `agents.role.${r}` })}</option>
                  ))}
                </select>
              </FormField>
              <FormField label={intl.formatMessage({ id: 'agents.edit.trigger' })}>
                <input type="text" value={form.trigger ?? ''} onChange={(e) => updateField('trigger', e.target.value)} className={inputClass} />
              </FormField>
              <FormField label={intl.formatMessage({ id: 'agents.edit.icon' })}>
                <input type="text" value={form.icon ?? ''} onChange={(e) => updateField('icon', e.target.value)} className={inputClass} />
              </FormField>
              <FormField label={intl.formatMessage({ id: 'agents.edit.reportsTo' })}>
                <input type="text" value={form.reports_to ?? ''} onChange={(e) => updateField('reports_to', e.target.value)} className={inputClass} />
              </FormField>
            </>
          )}

          {tab === 'model' && (() => {
            const cloudModels = availableModels.filter((m) => m.type === 'cloud');
            const localModels = availableModels.filter((m) => m.type === 'local');
            const prefIsLocal = (form.preferred ?? '').startsWith('local:');
            const fbIsLocal = (form.fallback ?? '').startsWith('local:');
            const hasLocalSelected = prefIsLocal || fbIsLocal;

            return (
              <>
                <FormField label={intl.formatMessage({ id: 'agents.edit.preferredModel' })}>
                  <select value={form.preferred ?? ''} onChange={(e) => updateField('preferred', e.target.value)} className={selectClass}>
                    <optgroup label="Cloud">
                      {cloudModels.map((m) => <option key={m.id} value={m.id}>{m.label}</option>)}
                    </optgroup>
                    {localModels.length > 0 && (
                      <optgroup label="Local">
                        {localModels.map((m) => <option key={m.id} value={m.id}>{m.label}</option>)}
                      </optgroup>
                    )}
                  </select>
                </FormField>
                <FormField label={intl.formatMessage({ id: 'agents.edit.fallbackModel' })}>
                  <select value={form.fallback ?? ''} onChange={(e) => updateField('fallback', e.target.value)} className={selectClass}>
                    <optgroup label="Cloud">
                      {cloudModels.map((m) => <option key={m.id} value={m.id}>{m.label}</option>)}
                    </optgroup>
                    {localModels.length > 0 && (
                      <optgroup label="Local">
                        {localModels.map((m) => <option key={m.id} value={m.id}>{m.label}</option>)}
                      </optgroup>
                    )}
                  </select>
                </FormField>

                <FormField label={intl.formatMessage({ id: 'agents.edit.apiMode' })}>
                  <select value={form.api_mode ?? 'cli'} onChange={(e) => updateField('api_mode', e.target.value as 'cli' | 'direct' | 'auto')} className={selectClass}>
                    <option value="cli">CLI (OAuth)</option>
                    <option value="direct">Direct API</option>
                    <option value="auto">Auto</option>
                  </select>
                </FormField>

                <Toggle checked={form.use_router ?? false} onChange={(v) => updateField('use_router', v)} label={intl.formatMessage({ id: 'agents.edit.confidenceRouter' })} />

                {/* Local model advanced config — shown when a local model is selected */}
                {hasLocalSelected && (
                  <div className="rounded-lg border border-amber-200 bg-amber-50/50 p-4 dark:border-amber-800 dark:bg-amber-900/10">
                    <h4 className="mb-3 text-xs font-semibold uppercase text-amber-700 dark:text-amber-400">{intl.formatMessage({ id: 'agents.edit.localInference' })}</h4>
                    <FormField label={intl.formatMessage({ id: 'agents.edit.inferenceBackend' })}>
                      <select value={form.local_backend ?? 'llama_cpp'} onChange={(e) => updateField('local_backend', e.target.value)} className={selectClass}>
                        <option value="llama_cpp">llama.cpp (Metal/CUDA)</option>
                        <option value="mistral_rs">mistral.rs (Rust-native)</option>
                        <option value="openai_compat">OpenAI-compatible (Exo/vLLM)</option>
                      </select>
                    </FormField>
                    <div className="grid grid-cols-2 gap-3">
                      <FormField label={intl.formatMessage({ id: 'agents.edit.contextLength' })}>
                        <input type="number" min={512} value={form.local_context_length ?? 4096} onChange={(e) => updateField('local_context_length', Number(e.target.value))} className={inputClass} />
                      </FormField>
                      <FormField label={intl.formatMessage({ id: 'agents.edit.gpuLayers' })}>
                        <input type="number" min={-1} value={form.local_gpu_layers ?? -1} onChange={(e) => updateField('local_gpu_layers', Number(e.target.value))} className={inputClass} />
                      </FormField>
                    </div>
                  </div>
                )}

                <div className="border-t border-stone-200 pt-4 dark:border-stone-700">
                  <h4 className="mb-3 text-xs font-semibold uppercase text-stone-500 dark:text-stone-400">{intl.formatMessage({ id: 'dashboard.budget.title' })}</h4>
                  <FormField label={intl.formatMessage({ id: 'agents.edit.budgetLimit' })}>
                    <input type="number" min={0} value={form.monthly_limit_cents ?? 5000} onChange={(e) => updateField('monthly_limit_cents', Number(e.target.value))} className={inputClass} />
                  </FormField>
                  <FormField label={intl.formatMessage({ id: 'agents.edit.warnThreshold' })}>
                    <input type="number" min={0} max={100} value={form.warn_threshold_percent ?? 80} onChange={(e) => updateField('warn_threshold_percent', Number(e.target.value))} className={inputClass} />
                  </FormField>
                  <Toggle checked={form.hard_stop ?? true} onChange={(v) => updateField('hard_stop', v)} label={intl.formatMessage({ id: 'agents.edit.hardStop' })} />
                </div>
              </>
            );
          })()}

          {tab === 'heartbeat' && (
            <>
              <Toggle checked={form.heartbeat_enabled ?? false} onChange={(v) => updateField('heartbeat_enabled', v)} label={intl.formatMessage({ id: 'agents.edit.heartbeatEnabled' })} />
              <FormField label={intl.formatMessage({ id: 'agents.edit.heartbeatInterval' })}>
                <input type="number" min={60} value={form.heartbeat_interval ?? 3600} onChange={(e) => updateField('heartbeat_interval', Number(e.target.value))} className={inputClass} />
              </FormField>
              <FormField label={intl.formatMessage({ id: 'agents.edit.heartbeatCron' })} hint="e.g. 0 * * * * (every hour)">
                <input type="text" value={form.heartbeat_cron ?? ''} onChange={(e) => updateField('heartbeat_cron', e.target.value)} placeholder="0 * * * *" className={inputClass} />
              </FormField>
            </>
          )}

          {tab === 'container' && (
            <>
              <Toggle checked={form.sandbox_enabled ?? false} onChange={(v) => updateField('sandbox_enabled', v)} label={intl.formatMessage({ id: 'agents.edit.sandbox' })} />
              <Toggle checked={form.network_access ?? false} onChange={(v) => updateField('network_access', v)} label={intl.formatMessage({ id: 'agents.edit.networkAccess' })} />
              <Toggle checked={form.readonly_project ?? true} onChange={(v) => updateField('readonly_project', v)} label={intl.formatMessage({ id: 'agents.edit.readonlyProject' })} />
              <FormField label={intl.formatMessage({ id: 'agents.edit.taskTimeout' })}>
                <input type="number" min={0} value={form.timeout_ms ?? 1800000} onChange={(e) => updateField('timeout_ms', Number(e.target.value))} className={inputClass} />
              </FormField>
              <FormField label={intl.formatMessage({ id: 'agents.edit.maxConcurrent' })}>
                <input type="number" min={1} max={10} value={form.max_concurrent ?? 1} onChange={(e) => updateField('max_concurrent', Number(e.target.value))} className={inputClass} />
              </FormField>
            </>
          )}

          {tab === 'permissions' && (
            <>
              <div className="space-y-1">
                <h4 className="text-xs font-semibold uppercase text-stone-500 dark:text-stone-400 mb-2">
                  {intl.formatMessage({ id: 'agents.edit.permissions' }).split('&')[0]?.trim() ?? 'Permissions'}
                </h4>
                <Toggle checked={form.can_create_agents ?? false} onChange={(v) => updateField('can_create_agents', v)} label={intl.formatMessage({ id: 'agents.edit.canCreateAgents' })} />
                <Toggle checked={form.can_send_cross_agent ?? true} onChange={(v) => updateField('can_send_cross_agent', v)} label={intl.formatMessage({ id: 'agents.edit.canSendCrossAgent' })} />
                <Toggle checked={form.can_modify_own_skills ?? true} onChange={(v) => updateField('can_modify_own_skills', v)} label={intl.formatMessage({ id: 'agents.edit.canModifySkills' })} />
                <Toggle checked={form.can_modify_own_soul ?? false} onChange={(v) => updateField('can_modify_own_soul', v)} label={intl.formatMessage({ id: 'agents.edit.canModifySoul' })} />
                <Toggle checked={form.can_schedule_tasks ?? false} onChange={(v) => updateField('can_schedule_tasks', v)} label={intl.formatMessage({ id: 'agents.edit.canScheduleTasks' })} />
              </div>
              <div className="border-t border-stone-200 pt-4 dark:border-stone-700 space-y-1">
                <h4 className="text-xs font-semibold uppercase text-stone-500 dark:text-stone-400 mb-2">Evolution</h4>
                <Toggle checked={form.skill_auto_activate ?? false} onChange={(v) => updateField('skill_auto_activate', v)} label={intl.formatMessage({ id: 'agents.edit.skillAutoActivate' })} />
                <Toggle checked={form.skill_security_scan ?? true} onChange={(v) => updateField('skill_security_scan', v)} label={intl.formatMessage({ id: 'agents.edit.skillSecurityScan' })} />
                <Toggle checked={form.gvu_enabled ?? true} onChange={(v) => updateField('gvu_enabled', v)} label={intl.formatMessage({ id: 'agents.edit.gvuEnabled' })} />
                <Toggle checked={form.cognitive_memory ?? false} onChange={(v) => updateField('cognitive_memory', v)} label={intl.formatMessage({ id: 'agents.edit.cognitiveMemory' })} />
                <FormField label={intl.formatMessage({ id: 'agents.edit.maxActiveSkills' })}>
                  <input type="number" min={1} max={20} value={form.max_active_skills ?? 5} onChange={(e) => updateField('max_active_skills', Number(e.target.value))} className={inputClass} />
                </FormField>
                <FormField label={intl.formatMessage({ id: 'agents.edit.maxSilenceHours' })}>
                  <input type="number" min={1} step={0.5} value={form.max_silence_hours ?? 12} onChange={(e) => updateField('max_silence_hours', Number(e.target.value))} className={inputClass} />
                </FormField>
              </div>
            </>
          )}

          {tab === 'sticker' && (
            <div className="space-y-4">
              <p className="text-xs text-stone-400 dark:text-stone-500">
                {intl.formatMessage({ id: 'agents.edit.stickerDesc' })}
              </p>
              <Toggle checked={form.sticker_enabled ?? false} onChange={(v) => updateField('sticker_enabled', v)} label={intl.formatMessage({ id: 'agents.edit.stickerEnabled' })} />
              <FormField label={intl.formatMessage({ id: 'agents.edit.stickerProbability' })}>
                <input type="range" min={0} max={1} step={0.05} value={form.sticker_probability ?? 0.3} onChange={(e) => updateField('sticker_probability', Number(e.target.value))} className="w-full accent-amber-500" />
                <span className="text-xs text-stone-500 dark:text-stone-400 ml-2">{((form.sticker_probability ?? 0.3) * 100).toFixed(0)}%</span>
              </FormField>
              <FormField label={intl.formatMessage({ id: 'agents.edit.stickerIntensity' })}>
                <input type="range" min={0} max={1} step={0.05} value={form.sticker_intensity_threshold ?? 0.7} onChange={(e) => updateField('sticker_intensity_threshold', Number(e.target.value))} className="w-full accent-amber-500" />
                <span className="text-xs text-stone-500 dark:text-stone-400 ml-2">{((form.sticker_intensity_threshold ?? 0.7) * 100).toFixed(0)}%</span>
              </FormField>
              <FormField label={intl.formatMessage({ id: 'agents.edit.stickerCooldown' })}>
                <input type="number" min={0} max={100} value={form.sticker_cooldown_messages ?? 5} onChange={(e) => updateField('sticker_cooldown_messages', Number(e.target.value))} className={inputClass} />
              </FormField>
              <FormField label={intl.formatMessage({ id: 'agents.edit.stickerExpressiveness' })}>
                <select value={form.sticker_expressiveness ?? 'moderate'} onChange={(e) => updateField('sticker_expressiveness', e.target.value as 'minimal' | 'moderate' | 'expressive')} className={selectClass}>
                  <option value="minimal">{intl.formatMessage({ id: 'agents.edit.stickerMinimal' })}</option>
                  <option value="moderate">{intl.formatMessage({ id: 'agents.edit.stickerModerate' })}</option>
                  <option value="expressive">{intl.formatMessage({ id: 'agents.edit.stickerExpressive' })}</option>
                </select>
              </FormField>
            </div>
          )}

          {tab === 'channels' && (
            <div className="space-y-5">
              <p className="text-xs text-stone-400 dark:text-stone-500">
                {intl.formatMessage({ id: 'agents.edit.channelsDesc' })}
              </p>

              {/* Discord */}
              <div className="space-y-2 border-b border-stone-200 pb-4 dark:border-stone-700">
                <h4 className="text-xs font-semibold uppercase text-stone-500 dark:text-stone-400">Discord</h4>
                <FormField label="Bot Token">
                  <input type="password" value={form.discord_bot_token ?? ''} onChange={(e) => updateField('discord_bot_token', e.target.value)} placeholder="MTIzNDU2Nzg5..." className={inputClass} autoComplete="off" />
                </FormField>
              </div>

              {/* Telegram */}
              <div className="space-y-2 border-b border-stone-200 pb-4 dark:border-stone-700">
                <h4 className="text-xs font-semibold uppercase text-stone-500 dark:text-stone-400">Telegram</h4>
                <FormField label="Bot Token">
                  <input type="password" value={form.telegram_bot_token ?? ''} onChange={(e) => updateField('telegram_bot_token', e.target.value)} placeholder="123456:ABC-DEF..." className={inputClass} autoComplete="off" />
                </FormField>
              </div>

              {/* LINE */}
              <div className="space-y-2 border-b border-stone-200 pb-4 dark:border-stone-700">
                <h4 className="text-xs font-semibold uppercase text-stone-500 dark:text-stone-400">LINE</h4>
                <FormField label="Channel Token">
                  <input type="password" value={form.line_channel_token ?? ''} onChange={(e) => updateField('line_channel_token', e.target.value)} className={inputClass} autoComplete="off" />
                </FormField>
                <FormField label="Channel Secret">
                  <input type="password" value={form.line_channel_secret ?? ''} onChange={(e) => updateField('line_channel_secret', e.target.value)} className={inputClass} autoComplete="off" />
                </FormField>
              </div>

              {/* Slack */}
              <div className="space-y-2 border-b border-stone-200 pb-4 dark:border-stone-700">
                <h4 className="text-xs font-semibold uppercase text-stone-500 dark:text-stone-400">Slack</h4>
                <FormField label="App Token (xapp-...)">
                  <input type="password" value={form.slack_app_token ?? ''} onChange={(e) => updateField('slack_app_token', e.target.value)} placeholder="xapp-1-..." className={inputClass} autoComplete="off" />
                </FormField>
                <FormField label="Bot Token (xoxb-...)">
                  <input type="password" value={form.slack_bot_token ?? ''} onChange={(e) => updateField('slack_bot_token', e.target.value)} placeholder="xoxb-..." className={inputClass} autoComplete="off" />
                </FormField>
              </div>

              {/* WhatsApp */}
              <div className="space-y-2 border-b border-stone-200 pb-4 dark:border-stone-700">
                <h4 className="text-xs font-semibold uppercase text-stone-500 dark:text-stone-400">WhatsApp</h4>
                <FormField label="Access Token">
                  <input type="password" value={form.whatsapp_access_token ?? ''} onChange={(e) => updateField('whatsapp_access_token', e.target.value)} className={inputClass} autoComplete="off" />
                </FormField>
                <FormField label="Verify Token">
                  <input type="text" value={form.whatsapp_verify_token ?? ''} onChange={(e) => updateField('whatsapp_verify_token', e.target.value)} className={inputClass} />
                </FormField>
                <FormField label="Phone Number ID">
                  <input type="text" value={form.whatsapp_phone_number_id ?? ''} onChange={(e) => updateField('whatsapp_phone_number_id', e.target.value)} className={inputClass} />
                </FormField>
              </div>

              {/* Feishu */}
              <div className="space-y-2">
                <h4 className="text-xs font-semibold uppercase text-stone-500 dark:text-stone-400">Feishu</h4>
                <FormField label="App ID">
                  <input type="password" value={form.feishu_app_id ?? ''} onChange={(e) => updateField('feishu_app_id', e.target.value)} className={inputClass} autoComplete="off" />
                </FormField>
                <FormField label="App Secret">
                  <input type="password" value={form.feishu_app_secret ?? ''} onChange={(e) => updateField('feishu_app_secret', e.target.value)} className={inputClass} autoComplete="off" />
                </FormField>
              </div>
            </div>
          )}
        </div>

        {/* Error + Actions */}
        {error && <p className="text-sm text-rose-600 dark:text-rose-400">{error}</p>}
        <div className="flex justify-end gap-3 border-t border-stone-200 pt-4 dark:border-stone-700">
          <button onClick={onClose} className={buttonSecondary}>{intl.formatMessage({ id: 'common.cancel' })}</button>
          <button onClick={handleSave} disabled={saving} className={buttonPrimary}>
            {saving ? intl.formatMessage({ id: 'common.saving' }) : intl.formatMessage({ id: 'common.save' })}
          </button>
        </div>
      </div>
    </Dialog>
  );
}

// ── Remove Confirm Dialog ──

function RemoveConfirmDialog({ agentName, onClose, onConfirm }: { agentName: string | null; onClose: () => void; onConfirm: () => void }) {
  const intl = useIntl();
  const [confirming, setConfirming] = useState(false);

  const handleConfirm = async () => {
    setConfirming(true);
    try {
      await onConfirm();
    } finally {
      setConfirming(false);
    }
  };

  if (!agentName) return null;

  return (
    <Dialog open={agentName !== null} onClose={onClose} title={intl.formatMessage({ id: 'agents.remove' })}>
      <div className="space-y-4">
        <p className="text-sm text-stone-600 dark:text-stone-400">
          {intl.formatMessage({ id: 'agents.remove.confirm' })}
        </p>
        <p className="text-sm font-medium text-stone-900 dark:text-stone-50">Agent: {agentName}</p>
        <div className="flex justify-end gap-3 pt-2">
          <button onClick={onClose} className={buttonSecondary}>{intl.formatMessage({ id: 'common.cancel' })}</button>
          <button
            onClick={handleConfirm}
            disabled={confirming}
            className="inline-flex items-center justify-center gap-2 rounded-lg bg-rose-500 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-rose-600 disabled:opacity-50"
          >
            {confirming ? intl.formatMessage({ id: 'common.loading' }) : intl.formatMessage({ id: 'common.delete' })}
          </button>
        </div>
      </div>
    </Dialog>
  );
}

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div>
      <h4 className="mb-2 text-sm font-semibold text-stone-700 dark:text-stone-300">{title}</h4>
      <div className="rounded-lg bg-stone-50 p-3 dark:bg-stone-800/50">{children}</div>
    </div>
  );
}

function InfoRow({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex justify-between py-1 text-sm">
      <span className="text-stone-500 dark:text-stone-400">{label}</span>
      <span className="font-medium text-stone-900 dark:text-stone-50">{value}</span>
    </div>
  );
}
