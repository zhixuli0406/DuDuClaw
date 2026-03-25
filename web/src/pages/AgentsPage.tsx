import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { useAgentsStore } from '@/stores/agents-store';
import { cn } from '@/lib/utils';
import { api, type AgentDetail } from '@/lib/api';
import { Dialog, FormField, inputClass, selectClass, buttonPrimary, buttonSecondary } from '@/components/shared/Dialog';
import { Bot, Pause, Play, Send, Eye, Plus, X, ShieldCheck } from 'lucide-react';

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
  const { agents, fetchAgents, pauseAgent, resumeAgent, loading } = useAgentsStore();
  const [showCreateDialog, setShowCreateDialog] = useState(false);
  const [delegateTarget, setDelegateTarget] = useState<string | null>(null);
  const [inspectTarget, setInspectTarget] = useState<AgentDetail | null>(null);

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
      setError('Agent 建立失敗，請確認名稱格式正確');
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
            <option value="specialist">{intl.formatMessage({ id: 'agents.role.specialist' })}</option>
            <option value="worker">{intl.formatMessage({ id: 'agents.role.worker' })}</option>
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
  const [prompt, setPrompt] = useState('');
  const [submitting, setSubmitting] = useState(false);
  const [result, setResult] = useState<string | null>(null);

  const handleSubmit = async () => {
    if (!prompt.trim()) return;
    setSubmitting(true);
    try {
      const res = await api.agents.delegate(agentName, prompt.trim());
      setResult(`已委派任務 (ID: ${res.message_id})`);
      setPrompt('');
    } catch {
      setResult('委派失敗');
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
    <Dialog open={open} onClose={handleClose} title={`委派任務給 ${agentName}`}>
      <div className="space-y-4">
        {result && (
          <div className="rounded-lg bg-emerald-50 p-3 text-sm text-emerald-700 dark:bg-emerald-900/20 dark:text-emerald-400">
            {result}
          </div>
        )}
        <FormField label="任務描述">
          <textarea
            value={prompt}
            onChange={(e) => setPrompt(e.target.value)}
            placeholder="請描述你要委派的任務..."
            rows={4}
            className={cn(inputClass, 'resize-none')}
          />
        </FormField>
        <div className="flex justify-end gap-3 pt-2">
          <button onClick={handleClose} className={buttonSecondary}>關閉</button>
          <button onClick={handleSubmit} disabled={submitting || !prompt.trim()} className={buttonPrimary}>
            {submitting ? '委派中...' : '委派'}
          </button>
        </div>
      </div>
    </Dialog>
  );
}

function InspectDialog({ agent, onClose }: { agent: AgentDetail | null; onClose: () => void }) {
  if (!agent) return null;

  return (
    <Dialog open={agent !== null} onClose={onClose} title={`${agent.icon || '🤖'} ${agent.display_name}`} className="max-w-2xl">
      <div className="space-y-4 max-h-[60vh] overflow-y-auto">
        <Section title="基本資訊">
          <InfoRow label="名稱" value={agent.name} />
          <InfoRow label="角色" value={agent.role} />
          <InfoRow label="狀態" value={agent.status} />
          <InfoRow label="觸發詞" value={agent.trigger} />
          <InfoRow label="上級" value={agent.reports_to || '(無)'} />
        </Section>

        <Section title="模型設定">
          <InfoRow label="首選模型" value={agent.model?.preferred ?? '—'} />
          <InfoRow label="備用模型" value={agent.model?.fallback ?? '—'} />
          <InfoRow label="帳號池" value={agent.model?.account_pool?.join(', ') ?? '—'} />
        </Section>

        {agent.budget && (
          <Section title="預算">
            <InfoRow label="月限額" value={`$${(agent.budget.monthly_limit_cents / 100).toFixed(2)}`} />
            <InfoRow label="已使用" value={`$${(agent.budget.spent_cents / 100).toFixed(2)}`} />
            <InfoRow label="警告閾值" value={`${agent.budget.warn_threshold_percent}%`} />
            <InfoRow label="硬停機" value={agent.budget.hard_stop ? '是' : '否'} />
          </Section>
        )}

        {agent.skills && agent.skills.length > 0 && (
          <Section title="技能">
            <div className="flex flex-wrap gap-2">
              {agent.skills.map((s) => (
                <span key={s} className="rounded-full bg-amber-100 px-2.5 py-0.5 text-xs text-amber-700 dark:bg-amber-900/30 dark:text-amber-400">
                  {s}
                </span>
              ))}
            </div>
          </Section>
        )}

        <div className="flex justify-end pt-2">
          <button onClick={onClose} className={buttonSecondary}>
            <X className="h-4 w-4" /> 關閉
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
