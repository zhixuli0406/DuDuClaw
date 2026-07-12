import { useEffect, useRef, useState, useCallback } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate } from 'react-router';
import { useAgentsStore } from '@/stores/agents-store';
import { useTasksStore } from '@/stores/tasks-store';
import { cn } from '@/lib/utils';
import { departmentsOf } from '@/lib/agents';
import {
  api,
  type AgentDetail,
  type AgentUpdateParams,
  type AgentCapabilities,
  type ComputerUseMode,
  type ComputerUseConfig,
  type ContractConfig,
  type AgentRuntime,
  type RuntimeProvider,
  type AgentEvolutionAdvanced,
  type ContainerMount,
  type ContainerEnvVar,
  type AgentOdooOverride,
  type ToolPolicyRule,
  type ToolPolicyWhen,
  type ToolPolicyEffect,
  type ToolPolicyOp,
} from '@/lib/api';
import { Dialog, FormField, inputClass } from '@/components/shared/Dialog';
import { ModelSelect } from '@/components/shared/ModelSelect';
import { useAvailableModels } from '@/hooks/useAvailableModels';
import { ChipEditor } from '@/components/shared/ChipEditor';
import {
  SettingField,
  OptionSelect,
  Switch as ControlSwitch,
  MoneyField,
  DurationField,
  ScheduleBuilder,
  DangerZone,
  type SelectOption,
} from '@/components/settings/controls';
import { toast, formatError } from '@/lib/toast';
import { Bot, Pause, Play, Send, Eye, Plus, X, ShieldCheck, Pencil, Trash2, LayoutGrid, Table2, Archive, RotateCcw, LogOut } from 'lucide-react';
import { Page, Card, Button, Badge, CharacterAvatar, EmptyState, Tabs } from '@/components/ui';
import { AgentStatusGlyph } from '@/components/AgentStatusGlyph';
import { RosterCard, HireSlotCard } from '@/components/agent';
import { OffboardDialog } from '@/components/agent/OffboardDialog';
import { useAgentGlyphState } from '@/stores/agent-activity-store';

/** Roster view mode — remembered in localStorage (§5.4 T6.1). */
type AgentsView = 'cards' | 'table';
const VIEW_KEY = 'duduclaw:agents:view';
function readView(): AgentsView {
  try {
    return localStorage.getItem(VIEW_KEY) === 'table' ? 'table' : 'cards';
  } catch {
    return 'cards';
  }
}

/** Live presence glyph for one roster card — reads the transient activity
 *  state derived from existing WS events (WP10-T10.2). For paused/terminated
 *  the label is suppressed (the StatusBadge already spells out lifecycle
 *  status); the glyph then only adds a subtle dot. */
function AgentLiveGlyph({ agent }: { agent: { name: string; status: string } }) {
  const state = useAgentGlyphState(agent.name, agent.status);
  const lifecycle = state === 'paused' || state === 'terminated';
  return <AgentStatusGlyph state={state} showLabel={!lifecycle} />;
}

function StatusBadge({ status }: { status: string }) {
  const intl = useIntl();
  const tones: Record<string, 'success' | 'warning' | 'danger' | 'neutral'> = {
    active: 'success',
    paused: 'warning',
    terminated: 'danger',
    archived: 'neutral',
  };

  return (
    <Badge tone={tones[status] ?? 'neutral'} dot>
      {intl.formatMessage({ id: `status.${status}` })}
    </Badge>
  );
}

function RoleBadge({ role }: { role: string }) {
  const intl = useIntl();
  return (
    <Badge tone="neutral">
      {intl.formatMessage({ id: `agents.role.${role}` })}
    </Badge>
  );
}

export function AgentsPage() {
  const intl = useIntl();
  const navigate = useNavigate();
  const { agents, fetchAgents, pauseAgent, resumeAgent, unarchiveAgent, includeArchived, setIncludeArchived, loading } = useAgentsStore();
  const { tasks, fetchTasks } = useTasksStore();
  const [view, setView] = useState<AgentsView>(readView);
  const [showCreateDialog, setShowCreateDialog] = useState(false);
  const [delegateTarget, setDelegateTarget] = useState<string | null>(null);
  const [inspectTarget, setInspectTarget] = useState<AgentDetail | null>(null);
  const [editTarget, setEditTarget] = useState<AgentDetail | null>(null);
  const [offboardTarget, setOffboardTarget] = useState<AgentDetail | null>(null);

  useEffect(() => {
    fetchAgents();
    // Tasks power the derived level + today's-tally on each roster card (§5.4).
    fetchTasks();
  }, [fetchAgents, fetchTasks]);

  const changeView = useCallback((next: AgentsView) => {
    setView(next);
    try {
      localStorage.setItem(VIEW_KEY, next);
    } catch {
      /* private mode — preference just won't persist */
    }
  }, []);

  const openDetail = useCallback(
    (name: string) => navigate(`/agents/${encodeURIComponent(name)}`),
    [navigate],
  );

  const empty = agents.length === 0 && !loading;

  return (
    <Page wide>
      {/* Inline header (mirrors <PageHeader> styling). The title <h1> and the
          create <Button> share a single parent <div> on purpose — the page test
          locates the create button via the title's parentElement. */}
      <header className="flex items-center gap-3">
        <span className="grid h-10 w-10 shrink-0 place-items-center rounded-xl bg-amber-500/12 text-amber-600 ring-1 ring-inset ring-amber-500/20 dark:bg-amber-400/10 dark:text-amber-400">
          <Bot className="h-5 w-5" />
        </span>
        <h1 className="min-w-0 flex-1 truncate text-2xl font-semibold tracking-tight text-stone-900 dark:text-stone-50">
          {intl.formatMessage({ id: 'agents.title' })}
        </h1>
        <Button variant="primary" icon={Plus} className="shrink-0" onClick={() => setShowCreateDialog(true)}>
          {intl.formatMessage({ id: 'agents.create' })}
        </Button>
      </header>

      {!empty && (
        <div className="flex flex-wrap items-center justify-end gap-3">
          <label className="flex items-center gap-2 text-sm text-stone-600 dark:text-stone-300">
            <input
              type="checkbox"
              checked={includeArchived}
              onChange={(e) => void setIncludeArchived(e.target.checked)}
              className="h-4 w-4 rounded border-stone-300 text-amber-500 focus-visible:ring-amber-500/50 dark:border-stone-600"
            />
            {intl.formatMessage({ id: 'agents.showArchived' })}
          </label>
          <ViewToggle view={view} onChange={changeView} />
        </div>
      )}

      {empty ? (
        <Card padded={false}>
          <EmptyState
            icon={Bot}
            title={intl.formatMessage({ id: 'agents.empty' })}
            action={
              <Button variant="primary" icon={Plus} onClick={() => navigate('/welcome')}>
                {intl.formatMessage({ id: 'agents.empty.cta' })}
              </Button>
            }
          />
        </Card>
      ) : view === 'cards' ? (
        /* Character roster (§5.4 T6.1). */
        <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3 xl:grid-cols-4">
          {agents.map((agent) => (
            <RosterCard
              key={agent.name}
              agent={agent}
              tasks={tasks}
              onOpen={openDetail}
              onUnarchive={agent.archived ? () => void unarchiveAgent(agent.name) : undefined}
            />
          ))}
          <HireSlotCard onClick={() => setShowCreateDialog(true)} />
        </div>
      ) : (
        /* Management view — the v1 roster cards, kept for full lifecycle control. */
        <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
          {agents.map((agent) => (
            <Card key={agent.name} interactive className={agent.archived ? 'opacity-70' : undefined}>
              <div className="flex items-start justify-between">
                <div className="flex items-center gap-3">
                  <CharacterAvatar agentId={agent.name} name={agent.display_name} size={32} />
                  <div className="min-w-0">
                    <h3 className="truncate font-semibold text-stone-900 dark:text-stone-50">{agent.display_name}</h3>
                    <p className="truncate text-xs text-stone-500 dark:text-stone-400">{agent.trigger}</p>
                  </div>
                </div>
                <div className="flex shrink-0 flex-col items-end gap-1.5">
                  <StatusBadge status={agent.status} />
                  <AgentLiveGlyph agent={agent} />
                </div>
              </div>

              <div className="mt-3 flex flex-wrap items-center gap-2">
                <RoleBadge role={agent.role} />
                {agent.department && (
                  <Badge tone="neutral">{intl.formatMessage({ id: 'agents.department.label' })}: {agent.department}</Badge>
                )}
                {agent.archived && (
                  <Badge tone="warning">
                    <Archive className="h-3 w-3" />
                    {intl.formatMessage({ id: 'agents.archived.badge' })}
                  </Badge>
                )}
                {agent.sandbox_enabled && (
                  <Badge tone="info">
                    <ShieldCheck className="h-3 w-3" />
                    {intl.formatMessage({ id: 'agents.sandboxed' })}
                  </Badge>
                )}
              </div>

              {agent.budget && (
                <div className="mt-4">
                  <div className="mb-1 flex justify-between text-xs text-stone-500 dark:text-stone-400">
                    <span>{intl.formatMessage({ id: 'dashboard.budget.title' })}</span>
                    <span className="tabular-nums">
                      ${(agent.budget.spent_cents / 100).toFixed(2)} / ${(agent.budget.monthly_limit_cents / 100).toFixed(2)}
                    </span>
                  </div>
                  <div className="h-1.5 overflow-hidden rounded-full bg-stone-500/15">
                    <div
                      className="h-full rounded-full bg-amber-500 transition-all"
                      style={{
                        width: `${
                          agent.budget.monthly_limit_cents > 0
                            ? Math.min(100, (agent.budget.spent_cents / agent.budget.monthly_limit_cents) * 100)
                            : 0
                        }%`,
                      }}
                    />
                  </div>
                </div>
              )}

              <div className="mt-4 flex flex-wrap gap-1.5 border-t border-[var(--panel-border)] pt-3">
                {agent.status === 'active' ? (
                  <Button size="sm" variant="ghost" icon={Pause} onClick={() => pauseAgent(agent.name)}>
                    {intl.formatMessage({ id: 'agents.pause' })}
                  </Button>
                ) : (
                  <Button
                    size="sm"
                    variant="ghost"
                    icon={Play}
                    className="text-emerald-600 hover:bg-emerald-500/10 hover:text-emerald-700 dark:text-emerald-400 dark:hover:bg-emerald-500/10"
                    onClick={() => resumeAgent(agent.name)}
                  >
                    {intl.formatMessage({ id: 'agents.resume' })}
                  </Button>
                )}
                <Button size="sm" variant="ghost" icon={Send} onClick={() => setDelegateTarget(agent.name)}>
                  {intl.formatMessage({ id: 'agents.delegate' })}
                </Button>
                <Button size="sm" variant="ghost" icon={Eye} onClick={() => navigate(`/agents/${encodeURIComponent(agent.name)}`)}>
                  {intl.formatMessage({ id: 'agents.inspect' })}
                </Button>
                <Button size="sm" variant="ghost" icon={Pencil} onClick={() => setEditTarget(agent)}>
                  {intl.formatMessage({ id: 'agents.edit' })}
                </Button>
                {agent.archived && (
                  <Button
                    size="sm"
                    variant="ghost"
                    icon={RotateCcw}
                    className="text-amber-600 hover:bg-amber-500/10 hover:text-amber-700 dark:text-amber-400"
                    onClick={() => void unarchiveAgent(agent.name)}
                  >
                    {intl.formatMessage({ id: 'agents.unarchive' })}
                  </Button>
                )}
                {agent.role === 'main' ? (
                  <span title={intl.formatMessage({ id: 'agents.offboard.mainBlocked' })}>
                    <Button
                      size="sm"
                      variant="ghost"
                      icon={LogOut}
                      disabled
                      aria-label={intl.formatMessage({ id: 'agents.offboard.mainBlocked' })}
                    />
                  </span>
                ) : !agent.archived ? (
                  <Button
                    size="sm"
                    variant="ghost"
                    icon={LogOut}
                    className="text-rose-600 hover:bg-rose-500/10 hover:text-rose-700 dark:text-rose-400 dark:hover:bg-rose-500/10"
                    aria-label={intl.formatMessage({ id: 'agents.remove' })}
                    onClick={() => setOffboardTarget(agent)}
                  >
                    {intl.formatMessage({ id: 'agentDetail.dismiss' })}
                  </Button>
                ) : null}
              </div>
            </Card>
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

      {/* Offboard Dialog (WP4 — archive / remove / handoff) */}
      {offboardTarget && (
        <OffboardDialog
          open
          agent={offboardTarget}
          candidates={agents.filter((a) => a.name !== offboardTarget.name && !a.archived)}
          onClose={() => setOffboardTarget(null)}
          onDone={() => { setOffboardTarget(null); fetchAgents(); }}
        />
      )}
    </Page>
  );
}

/** Segmented cards ⇄ table toggle for the roster (§5.4 T6.1). */
function ViewToggle({ view, onChange }: { view: AgentsView; onChange: (v: AgentsView) => void }) {
  const intl = useIntl();
  const opts: ReadonlyArray<{ id: AgentsView; icon: typeof LayoutGrid; label: string }> = [
    { id: 'cards', icon: LayoutGrid, label: intl.formatMessage({ id: 'agents.view.cards' }) },
    { id: 'table', icon: Table2, label: intl.formatMessage({ id: 'agents.view.table' }) },
  ];
  return (
    <div className="inline-flex rounded-control border border-[var(--panel-border)] p-0.5" role="tablist">
      {opts.map((o) => {
        const Icon = o.icon;
        const active = view === o.id;
        return (
          <button
            key={o.id}
            type="button"
            role="tab"
            aria-selected={active}
            onClick={() => onChange(o.id)}
            className={cn(
              'inline-flex items-center gap-1.5 rounded-[calc(var(--radius-control)-2px)] px-3 py-1.5 text-sm font-medium transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-500/50',
              active
                ? 'bg-amber-500/15 text-amber-700 dark:text-amber-300'
                : 'text-stone-500 hover:text-stone-800 dark:text-stone-400 dark:hover:text-stone-200',
            )}
          >
            <Icon className="h-4 w-4" />
            {o.label}
          </button>
        );
      })}
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
        <FormField label={intl.formatMessage({ id: 'agents.create.idLabel' })} hint={intl.formatMessage({ id: 'agents.create.idHint' })}>
          <input type="text" value={name} onChange={(e) => setName(e.target.value)} placeholder="coder" className={inputClass} />
        </FormField>
        <FormField label={intl.formatMessage({ id: 'agents.create.displayName' })}>
          <input type="text" value={displayName} onChange={(e) => setDisplayName(e.target.value)} placeholder="Coder" className={inputClass} />
        </FormField>
        <FormField label={intl.formatMessage({ id: 'orgchart.detail.role' })}>
          <OptionSelect
            value={role}
            onChange={setRole}
            options={['main', 'specialist', 'worker', 'developer', 'qa', 'planner'].map((r) => ({ value: r, label: intl.formatMessage({ id: `agents.role.${r}` }), raw: r }))}
          />
        </FormField>
        <FormField label={intl.formatMessage({ id: 'orgchart.detail.trigger' })} hint={intl.formatMessage({ id: 'agents.create.triggerHint' })}>
          <input type="text" value={trigger} onChange={(e) => setTrigger(e.target.value)} placeholder="@Coder" className={inputClass} />
        </FormField>
        {error && (
          <p className="text-sm text-rose-600 dark:text-rose-400">{error}</p>
        )}
        <div className="flex justify-end gap-3 pt-2">
          <Button variant="secondary" onClick={onClose}>{intl.formatMessage({ id: 'common.cancel' })}</Button>
          <Button variant="primary" onClick={handleSubmit} disabled={submitting || !name.trim() || !displayName.trim()}>
            {submitting ? intl.formatMessage({ id: 'common.loading' }) : intl.formatMessage({ id: 'agents.create' })}
          </Button>
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
          <Button variant="secondary" onClick={handleClose}>{intl.formatMessage({ id: 'agents.delegate.close' })}</Button>
          <Button variant="primary" onClick={handleSubmit} disabled={submitting || !prompt.trim()}>
            {submitting ? intl.formatMessage({ id: 'agents.delegate.submitting' }) : intl.formatMessage({ id: 'agents.delegate.submit' })}
          </Button>
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
                <Badge key={s} tone="accent">{s}</Badge>
              ))}
            </div>
          </Section>
        )}

        <div className="flex justify-end gap-3 pt-2">
          {onEdit && (
            <Button variant="primary" icon={Pencil} onClick={() => onEdit(agent)}>
              {intl.formatMessage({ id: 'agents.edit' })}
            </Button>
          )}
          <Button variant="secondary" icon={X} onClick={onClose}>
            {intl.formatMessage({ id: 'common.cancel' })}
          </Button>
        </div>
      </div>
    </Dialog>
  );
}

// ── Edit Agent Dialog ──

/** Two-level edit structure (spec §4.2): a top "一般 / 進階" split, and inside
 *  進階 a second-level tab strip of four groups. */
type MainTab = 'general' | 'advanced';
type AdvGroup = 'run' | 'access' | 'integration' | 'evo';

const RUNTIME_PROVIDERS: ReadonlyArray<RuntimeProvider> = ['claude', 'codex', 'gemini', 'grok', 'openai_compat'];

const AGENT_ROLES: ReadonlyArray<string> = ['main', 'specialist', 'worker', 'developer', 'qa', 'planner'];

/** Labeled on/off row — SettingField + shared Switch with a one-line help. The
 *  single replacement for the ad-hoc <Toggle> across the edit dialog (spec:
 *  "所有 toggle 換 Switch"). */
function SwitchRow({
  label,
  help,
  checked,
  onChange,
  disabled,
}: {
  label: string;
  help?: string;
  checked: boolean;
  onChange: (v: boolean) => void;
  disabled?: boolean;
}) {
  return (
    <SettingField label={label} help={help} layout="row">
      <ControlSwitch checked={checked} onChange={onChange} disabled={disabled} label={label} />
    </SettingField>
  );
}

const POLICY_EFFECTS: readonly ToolPolicyEffect[] = ['allow', 'ask', 'forbid'];
const POLICY_OPS: readonly ToolPolicyOp[] = ['equals', 'contains', 'starts_with'];

/**
 * ToolPolicyEditor — a Progent-style tool-authorization rule builder. Each rule
 * names a tool (`*` = any), an effect (allow / ask / forbid), and optional
 * AND-ed argument conditions. All updates are immutable (fresh arrays). The
 * surrounding SettingField carries the "strict allowlist" explanation.
 */
function ToolPolicyEditor({
  value,
  onChange,
}: {
  value: ToolPolicyRule[];
  onChange: (next: ToolPolicyRule[]) => void;
}) {
  const intl = useIntl();
  const effectOptions: SelectOption[] = POLICY_EFFECTS.map((e) => ({
    value: e,
    label: intl.formatMessage({ id: `agents.cap.policy.effect.${e}` }),
  }));
  const opOptions: SelectOption[] = POLICY_OPS.map((o) => ({
    value: o,
    label: intl.formatMessage({ id: `agents.cap.policy.op.${o}` }),
  }));

  const updateRule = (idx: number, patch: Partial<ToolPolicyRule>) =>
    onChange(value.map((r, i) => (i === idx ? { ...r, ...patch } : r)));
  const removeRule = (idx: number) => onChange(value.filter((_, i) => i !== idx));
  const addRule = () => onChange([...value, { tool: '*', effect: 'allow', when: [] }]);

  const whenOf = (ri: number): ToolPolicyWhen[] => value[ri].when ?? [];
  const updateWhen = (ri: number, wi: number, patch: Partial<ToolPolicyWhen>) =>
    updateRule(ri, { when: whenOf(ri).map((w, i) => (i === wi ? { ...w, ...patch } : w)) });
  const addWhen = (ri: number) =>
    updateRule(ri, { when: [...whenOf(ri), { arg: '', op: 'contains', value: '' }] });
  const removeWhen = (ri: number, wi: number) =>
    updateRule(ri, { when: whenOf(ri).filter((_, i) => i !== wi) });

  return (
    <div className="space-y-3">
      {value.length === 0 ? (
        <p className="text-xs text-stone-400 dark:text-stone-500">
          {intl.formatMessage({ id: 'agents.cap.policy.empty' })}
        </p>
      ) : (
        value.map((rule, ri) => (
          <div key={ri} className="space-y-2 rounded-lg border border-[var(--panel-border)] p-3">
            <div className="flex flex-wrap items-center gap-2">
              <input
                className={cn(inputClass, 'min-w-[8rem] flex-1')}
                value={rule.tool}
                placeholder="*"
                aria-label={intl.formatMessage({ id: 'agents.cap.policy.tool' })}
                onChange={(e) => updateRule(ri, { tool: e.target.value })}
              />
              <div className="w-36">
                <OptionSelect
                  value={rule.effect}
                  onChange={(v) => updateRule(ri, { effect: v as ToolPolicyEffect })}
                  options={effectOptions}
                  showRaw={false}
                />
              </div>
              <button
                type="button"
                onClick={() => removeRule(ri)}
                className="rounded-md p-1.5 text-stone-400 hover:bg-rose-500/10 hover:text-rose-500"
                aria-label={intl.formatMessage({ id: 'agents.cap.policy.removeRule' })}
              >
                <X className="h-4 w-4" />
              </button>
            </div>

            {whenOf(ri).map((w, wi) => (
              <div key={wi} className="flex flex-wrap items-center gap-2 pl-3">
                <span className="text-xs text-stone-400 dark:text-stone-500">
                  {intl.formatMessage({ id: 'agents.cap.policy.when' })}
                </span>
                <input
                  className={cn(inputClass, 'min-w-[6rem] flex-1')}
                  value={w.arg}
                  placeholder={intl.formatMessage({ id: 'agents.cap.policy.arg' })}
                  aria-label={intl.formatMessage({ id: 'agents.cap.policy.arg' })}
                  onChange={(e) => updateWhen(ri, wi, { arg: e.target.value })}
                />
                <div className="w-32">
                  <OptionSelect
                    value={w.op}
                    onChange={(v) => updateWhen(ri, wi, { op: v as ToolPolicyOp })}
                    options={opOptions}
                    showRaw={false}
                  />
                </div>
                <input
                  className={cn(inputClass, 'min-w-[6rem] flex-1')}
                  value={w.value}
                  placeholder={intl.formatMessage({ id: 'agents.cap.policy.value' })}
                  aria-label={intl.formatMessage({ id: 'agents.cap.policy.value' })}
                  onChange={(e) => updateWhen(ri, wi, { value: e.target.value })}
                />
                <button
                  type="button"
                  onClick={() => removeWhen(ri, wi)}
                  className="rounded-md p-1.5 text-stone-400 hover:bg-rose-500/10 hover:text-rose-500"
                  aria-label={intl.formatMessage({ id: 'agents.cap.policy.removeCondition' })}
                >
                  <X className="h-3.5 w-3.5" />
                </button>
              </div>
            ))}

            <button
              type="button"
              onClick={() => addWhen(ri)}
              className="ml-3 inline-flex items-center gap-1 text-xs font-medium text-stone-500 hover:text-amber-600 dark:text-stone-400 dark:hover:text-amber-400"
            >
              <Plus className="h-3 w-3" />
              {intl.formatMessage({ id: 'agents.cap.policy.addCondition' })}
            </button>
          </div>
        ))
      )}

      <button
        type="button"
        onClick={addRule}
        className="inline-flex items-center gap-1.5 rounded-lg border border-dashed border-[var(--panel-border)] px-3 py-2 text-xs font-medium text-stone-600 hover:border-amber-400 hover:text-amber-600 dark:text-stone-300 dark:hover:text-amber-400"
      >
        <Plus className="h-3.5 w-3.5" />
        {intl.formatMessage({ id: 'agents.cap.policy.addRule' })}
      </button>
    </div>
  );
}

/** RT — runtime form defaults. `agents.inspect` does not return [runtime], so
 *  this tab is write-only: it shows defaults and writes a partial update only
 *  when the operator touches it. */
const DEFAULT_RUNTIME: Required<Omit<AgentRuntime, 'fallback'>> & { fallback: string } = {
  provider: 'claude',
  fallback: '',
  pty_pool_enabled: false,
  worker_managed: false,
};

/** EVO — advanced evolution form defaults (write-only tab). */
const DEFAULT_EVOLUTION_ADVANCED: {
  external_factors: Required<NonNullable<AgentEvolutionAdvanced['external_factors']>>;
  skill_synthesis_enabled: boolean;
  skill_synthesis_threshold: number;
  skill_synthesis_cooldown_hours: number;
  skill_trial_ttl: number;
  skill_graduation_enabled: boolean;
  skill_graduation_min_lift: number;
  skill_recommendation_enabled: boolean;
  skill_recommendation_threshold: number;
  curiosity_enabled: boolean;
  curiosity_threshold: number;
  curiosity_max_daily: number;
  skill_behavior_monitor_enabled: boolean;
  skill_behavior_drift_threshold: number;
} = {
  external_factors: {
    user_feedback: true,
    security_events: true,
    channel_metrics: false,
    business_context: false,
    peer_signals: false,
  },
  skill_synthesis_enabled: false,
  skill_synthesis_threshold: 3,
  skill_synthesis_cooldown_hours: 24,
  skill_trial_ttl: 7,
  skill_graduation_enabled: false,
  skill_graduation_min_lift: 0.1,
  skill_recommendation_enabled: false,
  skill_recommendation_threshold: 0.6,
  curiosity_enabled: false,
  curiosity_threshold: 0.5,
  curiosity_max_daily: 3,
  skill_behavior_monitor_enabled: false,
  skill_behavior_drift_threshold: 0.3,
};

/** CT — advanced container form defaults (write-only tab). */
const DEFAULT_CONTAINER_ADVANCED: {
  worktree_enabled: boolean;
  worktree_auto_merge: boolean;
  worktree_cleanup_on_exit: boolean;
  worktree_copy_files: string[];
  additional_mounts: ContainerMount[];
  cmd: string[];
  env: ContainerEnvVar[];
} = {
  worktree_enabled: false,
  worktree_auto_merge: false,
  worktree_cleanup_on_exit: true,
  worktree_copy_files: [],
  additional_mounts: [],
  cmd: [],
  env: [],
};

/** Default capability values, used until agents.inspect prefills the form on
 *  tab open. A partial update is written only for fields the operator changed. */
const DEFAULT_CAPABILITIES: Required<Omit<AgentCapabilities, 'computer_use_config'>> & {
  computer_use_config: Required<ComputerUseConfig>;
} = {
  computer_use: false,
  computer_use_mode: 'container',
  browser_via_bash: false,
  allowed_tools: [],
  denied_tools: [],
  wiki_visible_to: [],
  native_sandbox: false,
  policy: [],
  computer_use_config: {
    allowed_apps: [],
    blocked_actions: [],
    max_session_minutes: 30,
    max_actions: 100,
    display_width: 1280,
    display_height: 800,
    auto_confirm_trusted: false,
  },
};

/** ODO — per-agent Odoo override (write-only tab; inspect doesn't return it). */
const DEFAULT_ODOO: {
  profile: string;
  allowed_models: string[];
  allowed_actions: string[];
  company_ids: string; // comma-separated ints in the form
  url: string;
  db: string;
  username: string;
  api_key: string;
  password: string;
} = {
  profile: '',
  allowed_models: [],
  allowed_actions: [],
  company_ids: '',
  url: '',
  db: '',
  username: '',
  api_key: '',
  password: '',
};

/** Advanced (G.8 free-form scalar tables) — write-only. Stored as KV rows. */
interface KvRow { key: string; value: string }
const DEFAULT_ADVANCED: {
  account_pool: string[];
  utility: string;
  heartbeat_max_concurrent_runs: number;
  heartbeat_cron_timezone: string;
  proactive_token_budget_per_check: number;
  proactive_timezone: string;
  proactive_max_turns: number;
  stagnation_enabled: boolean;
  stagnation_window_seconds: number;
  stagnation_trigger_threshold: number;
  stagnation_action: 'log_only' | 'suppress';
  ptc: KvRow[];
  prompt: KvRow[];
  cultural_context: KvRow[];
} = {
  account_pool: [],
  utility: '',
  heartbeat_max_concurrent_runs: 1,
  heartbeat_cron_timezone: '',
  proactive_token_budget_per_check: 0,
  proactive_timezone: '',
  proactive_max_turns: 1,
  stagnation_enabled: false,
  stagnation_window_seconds: 3600,
  stagnation_trigger_threshold: 3,
  stagnation_action: 'log_only',
  ptc: [],
  prompt: [],
  cultural_context: [],
};

function EditAgentDialog({ agent, onClose, onSaved }: { agent: AgentDetail | null; onClose: () => void; onSaved: () => void }) {
  const intl = useIntl();
  const { updateAgent, agents } = useAgentsStore();
  const [mainTab, setMainTab] = useState<MainTab>('general');
  const [advGroup, setAdvGroup] = useState<AdvGroup>('run');
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Available models (cloud + local) — live from the registry, deduped/cached.
  const {
    models: availableModels,
    loading: modelsLoading,
    error: modelsError,
    discoveredAt: modelsDiscoveredAt,
    refreshing: modelsRefreshing,
    refresh: modelsRefresh,
  } = useAvailableModels();

  // Local form state — initialized from agent when dialog opens
  const [form, setForm] = useState<AgentUpdateParams>({});

  // CAP — capabilities form. Prefilled from agents.inspect on tab open (see the
  // lazy effect below); a partial update is still written only when touched.
  const [caps, setCaps] = useState<typeof DEFAULT_CAPABILITIES>(DEFAULT_CAPABILITIES);
  // Tracks whether the operator touched the Capabilities tab — if untouched we
  // omit `capabilities` from the update so we don't overwrite existing config.
  const [capsDirty, setCapsDirty] = useState(false);
  // Mirror capsDirty into a ref so the async prefill can tell, at resolution
  // time, whether the operator already edited the tab (avoid clobbering edits).
  const capsDirtyRef = useRef(false);
  useEffect(() => { capsDirtyRef.current = capsDirty; }, [capsDirty]);
  // Prefill the capability form (incl. the Progent policy rules) from
  // agents.inspect the first time the tab opens, so existing values are visible
  // and editable rather than reset to defaults.
  const [capsLoaded, setCapsLoaded] = useState(false);

  // CON — contract form, loaded lazily via contract.get on first tab open
  const [contract, setContract] = useState<ContractConfig>({ must_not: [], must_always: [], max_tool_calls_per_turn: 0 });
  const [contractLoaded, setContractLoaded] = useState(false);
  const [contractSaving, setContractSaving] = useState(false);

  // RT — runtime form (write-only; inspect doesn't return [runtime])
  const [runtime, setRuntime] = useState<typeof DEFAULT_RUNTIME>(DEFAULT_RUNTIME);
  const [runtimeDirty, setRuntimeDirty] = useState(false);

  // EVO — advanced evolution form (write-only)
  const [evoAdv, setEvoAdv] = useState<typeof DEFAULT_EVOLUTION_ADVANCED>(DEFAULT_EVOLUTION_ADVANCED);
  const [evoAdvDirty, setEvoAdvDirty] = useState(false);

  // CT — advanced container form (write-only)
  const [ctAdv, setCtAdv] = useState<typeof DEFAULT_CONTAINER_ADVANCED>(DEFAULT_CONTAINER_ADVANCED);
  const [ctAdvDirty, setCtAdvDirty] = useState(false);

  // ODO — per-agent Odoo override form (write-only)
  const [odoo, setOdoo] = useState<typeof DEFAULT_ODOO>(DEFAULT_ODOO);
  const [odooDirty, setOdooDirty] = useState(false);

  // Advanced — G.8 scattered fields (write-only); account_pool prefilled from inspect.
  const [adv, setAdv] = useState<typeof DEFAULT_ADVANCED>(DEFAULT_ADVANCED);
  const [advDirty, setAdvDirty] = useState(false);

  useEffect(() => {
    if (agent) {
      // Determine current preferred/fallback as unified IDs. No hardcoded model
      // default — fall back to empty so ModelSelect prompts a live choice rather
      // than fabricating a model that may not exist for this deployment.
      const localModel = agent.model?.local?.model ?? '';
      const preferLocal = agent.model?.local?.prefer_local ?? false;
      const currentPreferred = preferLocal && localModel
        ? `local:${localModel}`
        : agent.model?.preferred ?? '';
      const currentFallback = agent.model?.fallback ?? '';

      setForm({
        display_name: agent.display_name,
        role: agent.role,
        trigger: agent.trigger,
        icon: agent.icon,
        reports_to: agent.reports_to,
        department: agent.department ?? '',
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
        skill_auto_activate: agent.evolution?.skill_auto_activate ?? false,
        skill_security_scan: agent.evolution?.skill_security_scan ?? true,
        gvu_enabled: agent.evolution?.gvu_enabled ?? true,
        cognitive_memory: agent.evolution?.cognitive_memory ?? true,
        sticker_enabled: agent.sticker?.enabled ?? false,
        sticker_probability: agent.sticker?.probability ?? 0.3,
        sticker_intensity_threshold: agent.sticker?.intensity_threshold ?? 0.7,
        sticker_cooldown_messages: agent.sticker?.cooldown_messages ?? 5,
        sticker_expressiveness: (agent.sticker?.expressiveness ?? 'moderate') as 'minimal' | 'moderate' | 'expressive',
      });
      setMainTab('general');
      setAdvGroup('run');
      setError(null);
      // Reset CAP/CON state for the newly-opened agent.
      setCaps(DEFAULT_CAPABILITIES);
      setCapsDirty(false);
      setCapsLoaded(false);
      setContract({ must_not: [], must_always: [], max_tool_calls_per_turn: 0 });
      setContractLoaded(false);
      // RT/EVO/CT — reset write-only advanced forms.
      setRuntime(DEFAULT_RUNTIME);
      setRuntimeDirty(false);
      setEvoAdv(DEFAULT_EVOLUTION_ADVANCED);
      setEvoAdvDirty(false);
      setCtAdv(DEFAULT_CONTAINER_ADVANCED);
      setCtAdvDirty(false);
      // ODO — reset write-only Odoo override form.
      setOdoo(DEFAULT_ODOO);
      setOdooDirty(false);
      // Advanced — seed account_pool from inspect; rest are write-only defaults.
      setAdv({ ...DEFAULT_ADVANCED, account_pool: agent.model?.account_pool ?? [] });
      setAdvDirty(false);
    }
  }, [agent]);

  // CON — lazily load CONTRACT.toml when the 能力與權限 group (which hosts the
  // contract editor) is first opened.
  useEffect(() => {
    if (mainTab !== 'advanced' || advGroup !== 'access' || !agent || contractLoaded) return;
    api.contract.get(agent.name).then((res) => {
      setContract({
        must_not: res.must_not ?? [],
        must_always: res.must_always ?? [],
        max_tool_calls_per_turn: res.max_tool_calls_per_turn ?? 0,
      });
      setContractLoaded(true);
    }).catch((e) => {
      console.warn('[api]', e);
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
      setContractLoaded(true);
    });
  }, [mainTab, advGroup, agent, contractLoaded, intl]);

  // CAP — lazily prefill the [capabilities] form (incl. Progent policy rules)
  // from agents.inspect when the 能力與權限 group first opens. Keeps capsDirty
  // false so an untouched tab still omits `capabilities` from the update.
  useEffect(() => {
    if (mainTab !== 'advanced' || advGroup !== 'access' || !agent || capsLoaded) return;
    // Guard both races: (1) cross-agent — if the dialog switches agents while
    // this inspect is in flight, `cancelled` (set by cleanup) drops the stale
    // result so agent A's policy never lands in agent B's form; (2) operator
    // edits made during the load window are preserved by skipping the merge
    // when the tab is already dirty.
    let cancelled = false;
    api.agents.inspect(agent.name).then((detail) => {
      if (cancelled) return;
      const c = detail.capabilities;
      if (c && !capsDirtyRef.current) {
        setCaps((prev) => ({
          ...prev,
          ...c,
          computer_use_config: { ...prev.computer_use_config, ...(c.computer_use_config ?? {}) },
        }));
      }
      setCapsLoaded(true);
    }).catch((e) => {
      if (cancelled) return;
      console.warn('[api]', e);
      setCapsLoaded(true);
    });
    return () => { cancelled = true; };
  }, [mainTab, advGroup, agent, capsLoaded]);

  const updateCap = useCallback(<K extends keyof typeof DEFAULT_CAPABILITIES>(key: K, value: (typeof DEFAULT_CAPABILITIES)[K]) => {
    setCapsDirty(true);
    setCaps((prev) => ({ ...prev, [key]: value }));
  }, []);

  const updateCapConfig = useCallback(<K extends keyof Required<ComputerUseConfig>>(key: K, value: Required<ComputerUseConfig>[K]) => {
    setCapsDirty(true);
    setCaps((prev) => ({ ...prev, computer_use_config: { ...prev.computer_use_config, [key]: value } }));
  }, []);

  // RT — runtime field updater.
  const updateRuntime = useCallback(<K extends keyof typeof DEFAULT_RUNTIME>(key: K, value: (typeof DEFAULT_RUNTIME)[K]) => {
    setRuntimeDirty(true);
    setRuntime((prev) => ({ ...prev, [key]: value }));
  }, []);

  // EVO — advanced evolution field updater.
  const updateEvoAdv = useCallback(<K extends keyof typeof DEFAULT_EVOLUTION_ADVANCED>(key: K, value: (typeof DEFAULT_EVOLUTION_ADVANCED)[K]) => {
    setEvoAdvDirty(true);
    setEvoAdv((prev) => ({ ...prev, [key]: value }));
  }, []);

  const updateEvoFactor = useCallback((key: keyof typeof DEFAULT_EVOLUTION_ADVANCED.external_factors, value: boolean) => {
    setEvoAdvDirty(true);
    setEvoAdv((prev) => ({ ...prev, external_factors: { ...prev.external_factors, [key]: value } }));
  }, []);

  // CT — advanced container field updater.
  const updateCtAdv = useCallback(<K extends keyof typeof DEFAULT_CONTAINER_ADVANCED>(key: K, value: (typeof DEFAULT_CONTAINER_ADVANCED)[K]) => {
    setCtAdvDirty(true);
    setCtAdv((prev) => ({ ...prev, [key]: value }));
  }, []);

  // ODO — per-agent Odoo override field updater.
  const updateOdoo = useCallback(<K extends keyof typeof DEFAULT_ODOO>(key: K, value: (typeof DEFAULT_ODOO)[K]) => {
    setOdooDirty(true);
    setOdoo((prev) => ({ ...prev, [key]: value }));
  }, []);

  // Advanced — G.8 field updater.
  const updateAdv = useCallback(<K extends keyof typeof DEFAULT_ADVANCED>(key: K, value: (typeof DEFAULT_ADVANCED)[K]) => {
    setAdvDirty(true);
    setAdv((prev) => ({ ...prev, [key]: value }));
  }, []);

  const handleContractSave = async () => {
    if (!agent) return;
    setContractSaving(true);
    try {
      await api.contract.update(agent.name, contract);
      toast.success(intl.formatMessage({ id: 'agents.contract.saved' }));
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.saveFailed' }, { message: formatError(e) }));
    } finally {
      setContractSaving(false);
    }
  };

  const updateField = useCallback(<K extends keyof AgentUpdateParams>(key: K, value: AgentUpdateParams[K]) => {
    setForm((prev) => ({ ...prev, [key]: value }));
  }, []);

  const handleSave = async () => {
    if (!agent) return;
    setSaving(true);
    setError(null);
    try {
      // Decompose unified model IDs into cloud preferred + local config.
      const submitForm = { ...form };
      const pref = submitForm.preferred ?? '';
      const fb = submitForm.fallback ?? '';

      // When a local model occupies the preferred/fallback slot the backend still
      // needs a cloud model in the cloud slot. Derive it from live data — the
      // agent's existing cloud preferred/fallback, else the first cloud model the
      // registry reports — instead of hardcoding a model id.
      const firstCloud = availableModels.find((m) => m.type === 'cloud')?.id ?? '';
      const existingCloudPref = agent.model?.preferred && !agent.model.preferred.startsWith('local:')
        ? agent.model.preferred : '';
      const existingCloudFb = agent.model?.fallback && !agent.model.fallback.startsWith('local:')
        ? agent.model.fallback : '';
      const cloudPrefSlot = existingCloudPref || firstCloud;
      const cloudFbSlot = existingCloudFb || firstCloud;

      if (pref.startsWith('local:')) {
        // Local model as preferred: set prefer_local + local_model, keep a cloud fallback
        submitForm.local_model = pref.replace('local:', '');
        submitForm.prefer_local = true;
        submitForm.preferred = fb.startsWith('local:') ? cloudPrefSlot : (fb || cloudPrefSlot);
      } else {
        // Cloud model as preferred
        submitForm.prefer_local = false;
      }

      if (fb.startsWith('local:')) {
        submitForm.local_model = submitForm.local_model || fb.replace('local:', '');
        submitForm.fallback = cloudFbSlot;
      }

      // CAP — only include capabilities when the operator edited that tab, so we
      // never clobber an existing [capabilities] block with defaults.
      if (capsDirty) {
        submitForm.capabilities = {
          computer_use: caps.computer_use,
          computer_use_mode: caps.computer_use_mode,
          browser_via_bash: caps.browser_via_bash,
          allowed_tools: caps.allowed_tools,
          denied_tools: caps.denied_tools,
          wiki_visible_to: caps.wiki_visible_to,
          native_sandbox: caps.native_sandbox,
          policy: caps.policy,
          computer_use_config: { ...caps.computer_use_config },
        };
      }

      // RT — only include runtime when the operator edited that tab.
      if (runtimeDirty) {
        submitForm.runtime = {
          provider: runtime.provider,
          fallback: runtime.fallback,
          pty_pool_enabled: runtime.pty_pool_enabled,
          worker_managed: runtime.worker_managed,
        };
      }

      // EVO — only include evolution_advanced when edited.
      if (evoAdvDirty) {
        submitForm.evolution_advanced = {
          external_factors: { ...evoAdv.external_factors },
          skill_synthesis_enabled: evoAdv.skill_synthesis_enabled,
          skill_synthesis_threshold: evoAdv.skill_synthesis_threshold,
          skill_synthesis_cooldown_hours: evoAdv.skill_synthesis_cooldown_hours,
          skill_trial_ttl: evoAdv.skill_trial_ttl,
          skill_graduation_enabled: evoAdv.skill_graduation_enabled,
          skill_graduation_min_lift: evoAdv.skill_graduation_min_lift,
          skill_recommendation_enabled: evoAdv.skill_recommendation_enabled,
          skill_recommendation_threshold: evoAdv.skill_recommendation_threshold,
          curiosity_enabled: evoAdv.curiosity_enabled,
          curiosity_threshold: evoAdv.curiosity_threshold,
          curiosity_max_daily: evoAdv.curiosity_max_daily,
          skill_behavior_monitor_enabled: evoAdv.skill_behavior_monitor_enabled,
          skill_behavior_drift_threshold: evoAdv.skill_behavior_drift_threshold,
        };
      }

      // CT — only include container_advanced when edited. Drop env vars with an
      // empty key (backend rejects them).
      if (ctAdvDirty) {
        submitForm.container_advanced = {
          worktree_enabled: ctAdv.worktree_enabled,
          worktree_auto_merge: ctAdv.worktree_auto_merge,
          worktree_cleanup_on_exit: ctAdv.worktree_cleanup_on_exit,
          worktree_copy_files: ctAdv.worktree_copy_files,
          additional_mounts: ctAdv.additional_mounts.filter(
            (m) => m.host.trim() !== '' && m.container.trim() !== ''
          ),
          cmd: ctAdv.cmd,
          env: ctAdv.env.filter((e) => e.key.trim() !== ''),
        };
      }

      // ODO — only include odoo when the operator edited that tab. company_ids
      // are parsed from the comma-separated form. api_key/password are sent only
      // when non-empty (write-only — never echoed back).
      if (odooDirty) {
        const companyIds = odoo.company_ids
          .split(',')
          .map((s) => s.trim())
          .filter((s) => s !== '')
          .map((s) => Number(s))
          .filter((n) => Number.isInteger(n) && n >= 0);
        const odooPayload: AgentOdooOverride = {
          profile: odoo.profile,
          allowed_models: odoo.allowed_models,
          allowed_actions: odoo.allowed_actions,
          company_ids: companyIds,
          url: odoo.url,
          db: odoo.db,
          username: odoo.username,
        };
        if (odoo.api_key.trim() !== '') odooPayload.api_key = odoo.api_key;
        if (odoo.password.trim() !== '') odooPayload.password = odoo.password;
        submitForm.odoo = odooPayload;
      }

      // Advanced — G.8 scattered fields. Only include when edited.
      if (advDirty) {
        submitForm.account_pool = adv.account_pool;
        submitForm.utility = adv.utility;
        submitForm.heartbeat_max_concurrent_runs = adv.heartbeat_max_concurrent_runs;
        if (adv.heartbeat_cron_timezone.trim() !== '') submitForm.heartbeat_cron_timezone = adv.heartbeat_cron_timezone.trim();
        // proactive extras go under the nested proactive object.
        submitForm.proactive = {
          ...(submitForm.proactive ?? {}),
          token_budget_per_check: adv.proactive_token_budget_per_check,
          max_turns: adv.proactive_max_turns,
          ...(adv.proactive_timezone.trim() !== '' ? { timezone: adv.proactive_timezone.trim() } : {}),
        };
        // UI.3 — stagnation detection.
        submitForm.stagnation_enabled = adv.stagnation_enabled;
        submitForm.stagnation_window_seconds = adv.stagnation_window_seconds;
        submitForm.stagnation_trigger_threshold = adv.stagnation_trigger_threshold;
        submitForm.stagnation_action = adv.stagnation_action;
        // Free-form scalar tables — drop empty keys.
        const kvToObj = (rows: ReadonlyArray<KvRow>): Record<string, string> =>
          Object.fromEntries(rows.filter((r) => r.key.trim() !== '').map((r) => [r.key.trim(), r.value]));
        const ptc = kvToObj(adv.ptc);
        const prompt = kvToObj(adv.prompt);
        const cultural = kvToObj(adv.cultural_context);
        if (Object.keys(ptc).length > 0) submitForm.ptc = ptc;
        if (Object.keys(prompt).length > 0) submitForm.prompt = prompt;
        if (Object.keys(cultural).length > 0) submitForm.cultural_context = cultural;
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

  // Top-level "一般 / 進階" split, and the 進階 second-level group strip.
  const mainTabs: { id: MainTab; label: string }[] = [
    { id: 'general', label: intl.formatMessage({ id: 'agents.edit.tab.general' }) },
    { id: 'advanced', label: intl.formatMessage({ id: 'agents.edit.tab.advanced' }) },
  ];
  const advTabs: { id: AdvGroup; label: string }[] = [
    { id: 'run', label: intl.formatMessage({ id: 'agents.edit.group.run' }) },
    { id: 'access', label: intl.formatMessage({ id: 'agents.edit.group.access' }) },
    { id: 'integration', label: intl.formatMessage({ id: 'agents.edit.group.integration' }) },
    { id: 'evo', label: intl.formatMessage({ id: 'agents.edit.group.evo' }) },
  ];

  // Plain-language option sets (label + raw technical value).
  const roleOptions: SelectOption[] = AGENT_ROLES.map((r) => ({ value: r, label: intl.formatMessage({ id: `agents.role.${r}` }), raw: r }));
  const apiModeOptions: SelectOption[] = [
    { value: 'cli', label: intl.formatMessage({ id: 'agents.apiMode.cli' }), raw: 'cli' },
    { value: 'direct', label: intl.formatMessage({ id: 'agents.apiMode.direct' }), raw: 'direct' },
    { value: 'auto', label: intl.formatMessage({ id: 'agents.apiMode.auto' }), raw: 'auto' },
  ];
  const providerOptions: SelectOption[] = RUNTIME_PROVIDERS.map((p) => ({ value: p, label: intl.formatMessage({ id: `agents.runtime.provider.${p}` }), raw: p }));
  const fallbackProviderOptions: SelectOption[] = [
    { value: '', label: intl.formatMessage({ id: 'agents.runtime.fallback.none' }), raw: '' },
    ...providerOptions,
  ];
  const localBackendOptions: SelectOption[] = [
    { value: 'llama_cpp', label: intl.formatMessage({ id: 'agents.backend.llamaCpp' }), raw: 'llama_cpp' },
    { value: 'mistral_rs', label: intl.formatMessage({ id: 'agents.backend.mistralRs' }), raw: 'mistral_rs' },
    { value: 'openai_compat', label: intl.formatMessage({ id: 'agents.backend.openaiCompat' }), raw: 'openai_compat' },
  ];
  const expressivenessOptions: SelectOption[] = [
    { value: 'minimal', label: intl.formatMessage({ id: 'agents.edit.stickerMinimal' }), raw: 'minimal' },
    { value: 'moderate', label: intl.formatMessage({ id: 'agents.edit.stickerModerate' }), raw: 'moderate' },
    { value: 'expressive', label: intl.formatMessage({ id: 'agents.edit.stickerExpressive' }), raw: 'expressive' },
  ];
  const computerUseModeOptions: SelectOption[] = [
    { value: 'container', label: intl.formatMessage({ id: 'agents.cap.mode.container' }), raw: 'container' },
    { value: 'native', label: intl.formatMessage({ id: 'agents.cap.mode.native' }), raw: 'native' },
    { value: 'auto', label: intl.formatMessage({ id: 'agents.cap.mode.auto' }), raw: 'auto' },
  ];
  const stagnationActionOptions: SelectOption[] = [
    { value: 'log_only', label: intl.formatMessage({ id: 'agents.adv.stagnation.logOnly' }), raw: 'log_only' },
    { value: 'suppress', label: intl.formatMessage({ id: 'agents.adv.stagnation.suppress' }), raw: 'suppress' },
  ];
  const statusOptions: SelectOption[] = ['active', 'paused', 'terminated'].map((s) => ({ value: s, label: intl.formatMessage({ id: `status.${s}` }), raw: s }));

  // 上級 dropdown — existing agents (excluding self); keep the current value even
  // if it isn't in the live roster so it is never silently dropped.
  const reportsToOptions: SelectOption[] = [
    { value: '', label: intl.formatMessage({ id: 'agents.edit.reportsTo.none' }), raw: '' },
    ...agents.filter((a) => a.name !== agent.name).map((a) => ({ value: a.name, label: a.display_name || a.name, raw: a.name })),
  ];
  if (form.reports_to && !reportsToOptions.some((o) => o.value === form.reports_to)) {
    reportsToOptions.push({ value: form.reports_to, label: form.reports_to, raw: form.reports_to });
  }

  // WP7 — the set of departments already in use, for the free-input datalist.
  const departmentOptions: string[] = departmentsOf(agents);

  return (
    <Dialog open={agent !== null} onClose={onClose} title={`${agent.icon || '🤖'} ${intl.formatMessage({ id: 'agents.edit' })}`} className="max-w-2xl">
      <div className="space-y-4">
        {/* Top-level tab bar: 一般 / 進階 */}
        <Tabs items={mainTabs} value={mainTab} onChange={(id) => setMainTab(id as MainTab)} />

        {/* Content */}
        <div className="max-h-[50vh] overflow-y-auto space-y-4 pr-1">
          {mainTab === 'general' && (
            <div className="space-y-6">
              {/* 身分 */}
              <section className="space-y-1">
                <h4 className="text-xs font-semibold uppercase tracking-wide text-stone-500 dark:text-stone-400">{intl.formatMessage({ id: 'agents.edit.section.identity' })}</h4>
                <SettingField label={intl.formatMessage({ id: 'agents.edit.displayName' })} help={intl.formatMessage({ id: 'agents.edit.displayName.help' })}>
                  <input type="text" value={form.display_name ?? ''} onChange={(e) => updateField('display_name', e.target.value)} className={inputClass} />
                </SettingField>
                <SettingField label={intl.formatMessage({ id: 'agents.edit.icon' })} help={intl.formatMessage({ id: 'agents.edit.icon.help' })}>
                  <input type="text" value={form.icon ?? ''} onChange={(e) => updateField('icon', e.target.value)} className={inputClass} />
                </SettingField>
                <SettingField label={intl.formatMessage({ id: 'agents.edit.role' })} help={intl.formatMessage({ id: 'agents.edit.role.help' })}>
                  <OptionSelect value={form.role ?? 'specialist'} onChange={(v) => updateField('role', v)} options={roleOptions} />
                </SettingField>
                <SettingField label={intl.formatMessage({ id: 'agents.edit.trigger' })} help={intl.formatMessage({ id: 'agents.edit.trigger.help' })}>
                  <input type="text" value={form.trigger ?? ''} onChange={(e) => updateField('trigger', e.target.value)} className={inputClass} />
                </SettingField>
                <SettingField label={intl.formatMessage({ id: 'agents.edit.reportsTo' })} help={intl.formatMessage({ id: 'agents.edit.reportsTo.help' })}>
                  <OptionSelect value={form.reports_to ?? ''} onChange={(v) => updateField('reports_to', v)} options={reportsToOptions} />
                </SettingField>
                <SettingField label={intl.formatMessage({ id: 'agents.department.label' })} help={intl.formatMessage({ id: 'agents.department.help' })}>
                  <input
                    type="text"
                    list="agent-department-options"
                    value={form.department ?? ''}
                    onChange={(e) => updateField('department', e.target.value)}
                    placeholder={intl.formatMessage({ id: 'agents.department.placeholder' })}
                    className={inputClass}
                  />
                  <datalist id="agent-department-options">
                    {departmentOptions.map((d) => (
                      <option key={d} value={d} />
                    ))}
                  </datalist>
                </SettingField>
              </section>

              {/* 模型 */}
              <section className="space-y-1 border-t border-[var(--panel-border)] pt-4">
                <h4 className="text-xs font-semibold uppercase tracking-wide text-stone-500 dark:text-stone-400">{intl.formatMessage({ id: 'agents.edit.section.model' })}</h4>
                <SettingField label={intl.formatMessage({ id: 'agents.edit.preferredModel' })} help={intl.formatMessage({ id: 'agents.edit.preferredModel.help' })}>
                  <ModelSelect value={form.preferred ?? ''} onChange={(v) => updateField('preferred', v)} models={availableModels} loading={modelsLoading} error={modelsError} discoveredAt={modelsDiscoveredAt} refreshing={modelsRefreshing} onRefresh={modelsRefresh} ariaLabel={intl.formatMessage({ id: 'agents.edit.preferredModel' })} />
                </SettingField>
                <SettingField label={intl.formatMessage({ id: 'agents.edit.fallbackModel' })} help={intl.formatMessage({ id: 'agents.edit.fallbackModel.help' })}>
                  <ModelSelect value={form.fallback ?? ''} onChange={(v) => updateField('fallback', v)} models={availableModels} loading={modelsLoading} error={modelsError} discoveredAt={modelsDiscoveredAt} refreshing={modelsRefreshing} onRefresh={modelsRefresh} ariaLabel={intl.formatMessage({ id: 'agents.edit.fallbackModel' })} />
                </SettingField>
                <SettingField label={intl.formatMessage({ id: 'agents.edit.apiMode' })} help={intl.formatMessage({ id: 'agents.edit.apiMode.help' })}>
                  <OptionSelect value={form.api_mode ?? 'cli'} onChange={(v) => updateField('api_mode', v as 'cli' | 'direct' | 'auto')} options={apiModeOptions} />
                </SettingField>
                <SwitchRow label={intl.formatMessage({ id: 'agents.edit.confidenceRouter' })} help={intl.formatMessage({ id: 'agents.edit.confidenceRouter.help' })} checked={form.use_router ?? false} onChange={(v) => updateField('use_router', v)} />
                {((form.preferred ?? '').startsWith('local:') || (form.fallback ?? '').startsWith('local:')) && (
                  <div className="rounded-lg border border-amber-200 bg-amber-50/50 p-4 dark:border-amber-800 dark:bg-amber-900/10 space-y-1">
                    <h5 className="mb-2 text-xs font-semibold uppercase text-amber-700 dark:text-amber-400">{intl.formatMessage({ id: 'agents.edit.localInference' })}</h5>
                    <SettingField label={intl.formatMessage({ id: 'agents.edit.inferenceBackend' })}>
                      <OptionSelect value={form.local_backend ?? 'llama_cpp'} onChange={(v) => updateField('local_backend', v)} options={localBackendOptions} />
                    </SettingField>
                    <div className="grid grid-cols-2 gap-3">
                      <SettingField label={intl.formatMessage({ id: 'agents.edit.contextLength' })}>
                        <input type="number" min={512} value={form.local_context_length ?? 4096} onChange={(e) => updateField('local_context_length', Number(e.target.value))} className={inputClass} />
                      </SettingField>
                      <SettingField label={intl.formatMessage({ id: 'agents.edit.gpuLayers' })}>
                        <input type="number" min={-1} value={form.local_gpu_layers ?? -1} onChange={(e) => updateField('local_gpu_layers', Number(e.target.value))} className={inputClass} />
                      </SettingField>
                    </div>
                  </div>
                )}
              </section>

              {/* 預算 */}
              <section className="space-y-1 border-t border-[var(--panel-border)] pt-4">
                <h4 className="text-xs font-semibold uppercase tracking-wide text-stone-500 dark:text-stone-400">{intl.formatMessage({ id: 'agents.edit.section.budget' })}</h4>
                <SettingField label={intl.formatMessage({ id: 'agents.edit.budgetLimit' })} help={intl.formatMessage({ id: 'agents.edit.budgetLimit.help' })}>
                  <MoneyField cents={form.monthly_limit_cents ?? 5000} onChange={(c) => updateField('monthly_limit_cents', c)} />
                </SettingField>
                <SettingField label={intl.formatMessage({ id: 'agents.edit.warnThreshold' })} help={intl.formatMessage({ id: 'agents.edit.warnThreshold.help' })}>
                  <input type="number" min={0} max={100} value={form.warn_threshold_percent ?? 80} onChange={(e) => updateField('warn_threshold_percent', Number(e.target.value))} className={inputClass} />
                </SettingField>
                <SwitchRow label={intl.formatMessage({ id: 'agents.edit.hardStop' })} help={intl.formatMessage({ id: 'agents.edit.hardStop.help' })} checked={form.hard_stop ?? true} onChange={(v) => updateField('hard_stop', v)} />
              </section>

              {/* 貼圖 */}
              <section className="space-y-1 border-t border-[var(--panel-border)] pt-4">
                <h4 className="text-xs font-semibold uppercase tracking-wide text-stone-500 dark:text-stone-400">{intl.formatMessage({ id: 'agents.edit.sticker' })}</h4>
                <p className="text-xs text-stone-400 dark:text-stone-500">{intl.formatMessage({ id: 'agents.edit.stickerDesc' })}</p>
                <SwitchRow label={intl.formatMessage({ id: 'agents.edit.stickerEnabled' })} checked={form.sticker_enabled ?? false} onChange={(v) => updateField('sticker_enabled', v)} />
                <SettingField label={intl.formatMessage({ id: 'agents.edit.stickerProbability' })}>
                  <div className="flex items-center">
                    <input type="range" min={0} max={1} step={0.05} value={form.sticker_probability ?? 0.3} onChange={(e) => updateField('sticker_probability', Number(e.target.value))} className="w-full accent-amber-500" />
                    <span className="ml-2 text-xs text-stone-500 dark:text-stone-400">{((form.sticker_probability ?? 0.3) * 100).toFixed(0)}%</span>
                  </div>
                </SettingField>
                <SettingField label={intl.formatMessage({ id: 'agents.edit.stickerIntensity' })}>
                  <div className="flex items-center">
                    <input type="range" min={0} max={1} step={0.05} value={form.sticker_intensity_threshold ?? 0.7} onChange={(e) => updateField('sticker_intensity_threshold', Number(e.target.value))} className="w-full accent-amber-500" />
                    <span className="ml-2 text-xs text-stone-500 dark:text-stone-400">{((form.sticker_intensity_threshold ?? 0.7) * 100).toFixed(0)}%</span>
                  </div>
                </SettingField>
                <SettingField label={intl.formatMessage({ id: 'agents.edit.stickerCooldown' })}>
                  <input type="number" min={0} max={100} value={form.sticker_cooldown_messages ?? 5} onChange={(e) => updateField('sticker_cooldown_messages', Number(e.target.value))} className={inputClass} />
                </SettingField>
                <SettingField label={intl.formatMessage({ id: 'agents.edit.stickerExpressiveness' })}>
                  <OptionSelect value={form.sticker_expressiveness ?? 'moderate'} onChange={(v) => updateField('sticker_expressiveness', v as 'minimal' | 'moderate' | 'expressive')} options={expressivenessOptions} />
                </SettingField>
              </section>
            </div>
          )}

          {mainTab === 'advanced' && (
            <div className="space-y-4">
              {/* Second-level group strip */}
              <Tabs items={advTabs} value={advGroup} onChange={(id) => setAdvGroup(id as AdvGroup)} />

          {advGroup === 'run' && (
            <div className="space-y-4">
              <p className="text-xs text-stone-400 dark:text-stone-500">{intl.formatMessage({ id: 'agents.runtime.desc' })}</p>
              <SettingField label={intl.formatMessage({ id: 'agents.runtime.provider' })} help={intl.formatMessage({ id: 'agents.runtime.provider.hint' })}>
                <OptionSelect value={runtime.provider} onChange={(v) => updateRuntime('provider', v as RuntimeProvider)} options={providerOptions} />
              </SettingField>
              <SettingField label={intl.formatMessage({ id: 'agents.runtime.fallback' })} help={intl.formatMessage({ id: 'agents.runtime.fallback.hint' })}>
                <OptionSelect value={runtime.fallback} onChange={(v) => updateRuntime('fallback', v)} options={fallbackProviderOptions} />
              </SettingField>
              <div className="border-t border-stone-200 pt-4 dark:border-stone-700 space-y-1">
                <h4 className="mb-2 text-xs font-semibold uppercase text-stone-500 dark:text-stone-400">{intl.formatMessage({ id: 'agents.runtime.ptyTitle' })}</h4>
                <SwitchRow label={intl.formatMessage({ id: 'agents.runtime.ptyPoolEnabled' })} checked={runtime.pty_pool_enabled} onChange={(v) => updateRuntime('pty_pool_enabled', v)} />
                <SwitchRow label={intl.formatMessage({ id: 'agents.runtime.workerManaged' })} checked={runtime.worker_managed} onChange={(v) => updateRuntime('worker_managed', v)} />
                <p className="text-xs text-stone-400 dark:text-stone-500">{intl.formatMessage({ id: 'agents.runtime.pty.hint' })}</p>
              </div>

              {/* Heartbeat — 自動巡邏 */}
              <div className="border-t border-stone-200 pt-4 dark:border-stone-700 space-y-1">
                <h4 className="mb-2 text-xs font-semibold uppercase text-stone-500 dark:text-stone-400">{intl.formatMessage({ id: 'agents.edit.heartbeat' })}</h4>
                <SwitchRow label={intl.formatMessage({ id: 'agents.edit.heartbeatEnabled' })} help={intl.formatMessage({ id: 'agents.edit.heartbeatEnabled.help' })} checked={form.heartbeat_enabled ?? false} onChange={(v) => updateField('heartbeat_enabled', v)} />
                <SettingField label={intl.formatMessage({ id: 'agents.edit.heartbeatInterval' })} help={intl.formatMessage({ id: 'agents.edit.heartbeatInterval.help' })}>
                  <DurationField seconds={form.heartbeat_interval ?? 3600} onChange={(s) => updateField('heartbeat_interval', s)} units={['sec', 'min', 'hour']} min={60} />
                </SettingField>
                <SettingField label={intl.formatMessage({ id: 'agents.edit.heartbeatCron' })} help={intl.formatMessage({ id: 'agents.edit.heartbeatCron.help' })}>
                  <ScheduleBuilder value={form.heartbeat_cron ?? ''} onChange={(c) => updateField('heartbeat_cron', c)} />
                </SettingField>
              </div>

              {/* Container */}
              <div className="border-t border-stone-200 pt-4 dark:border-stone-700 space-y-1">
                <h4 className="mb-2 text-xs font-semibold uppercase text-stone-500 dark:text-stone-400">{intl.formatMessage({ id: 'settings.container' })}</h4>
                <SwitchRow label={intl.formatMessage({ id: 'agents.edit.sandbox' })} help={intl.formatMessage({ id: 'agents.edit.sandbox.help' })} checked={form.sandbox_enabled ?? false} onChange={(v) => updateField('sandbox_enabled', v)} />
                <SwitchRow label={intl.formatMessage({ id: 'agents.edit.readonlyProject' })} help={intl.formatMessage({ id: 'agents.edit.readonlyProject.help' })} checked={form.readonly_project ?? true} onChange={(v) => updateField('readonly_project', v)} />
                <SettingField label={intl.formatMessage({ id: 'agents.edit.taskTimeout' })} help={intl.formatMessage({ id: 'agents.edit.taskTimeout.help' })}>
                  <DurationField seconds={Math.round((form.timeout_ms ?? 1800000) / 1000)} onChange={(s) => updateField('timeout_ms', s * 1000)} units={['sec', 'min', 'hour']} min={0} />
                </SettingField>
                <SettingField label={intl.formatMessage({ id: 'agents.edit.maxConcurrent' })} help={intl.formatMessage({ id: 'agents.edit.maxConcurrent.help' })}>
                  <input type="number" min={1} max={10} value={form.max_concurrent ?? 1} onChange={(e) => updateField('max_concurrent', Number(e.target.value))} className={inputClass} />
                </SettingField>

                <div className="pt-2 space-y-1">
                  <h5 className="mb-1 text-xs font-semibold uppercase text-stone-500 dark:text-stone-400">{intl.formatMessage({ id: 'agents.container.worktreeTitle' })}</h5>
                  <SwitchRow label={intl.formatMessage({ id: 'agents.container.worktreeEnabled' })} checked={ctAdv.worktree_enabled} onChange={(v) => updateCtAdv('worktree_enabled', v)} />
                  <SwitchRow label={intl.formatMessage({ id: 'agents.container.worktreeCleanup' })} checked={ctAdv.worktree_cleanup_on_exit} onChange={(v) => updateCtAdv('worktree_cleanup_on_exit', v)} />
                  <SettingField label={intl.formatMessage({ id: 'agents.container.worktreeCopyFiles' })} help={intl.formatMessage({ id: 'agents.container.worktreeCopyFiles.hint' })}>
                    <ChipEditor values={ctAdv.worktree_copy_files} onChange={(v) => updateCtAdv('worktree_copy_files', v)} placeholder=".env" addLabel={intl.formatMessage({ id: 'common.add' })} />
                  </SettingField>
                </div>

                <SettingField label={intl.formatMessage({ id: 'agents.container.cmd' })} help={intl.formatMessage({ id: 'agents.container.cmd.hint' })}>
                  <ChipEditor values={ctAdv.cmd} onChange={(v) => updateCtAdv('cmd', v)} placeholder="bash" addLabel={intl.formatMessage({ id: 'common.add' })} />
                </SettingField>

                <EnvTable env={ctAdv.env} onChange={(v) => updateCtAdv('env', v)} />
              </div>

              {/* DangerZone — host access / mounts / auto-merge */}
              <DangerZone title={intl.formatMessage({ id: 'agents.container.danger.title' })} description={intl.formatMessage({ id: 'agents.container.danger.desc' })}>
                <SwitchRow label={intl.formatMessage({ id: 'agents.edit.networkAccess' })} help={intl.formatMessage({ id: 'agents.edit.networkAccess.help' })} checked={form.network_access ?? false} onChange={(v) => updateField('network_access', v)} />
                <SwitchRow label={intl.formatMessage({ id: 'agents.container.worktreeAutoMerge' })} help={intl.formatMessage({ id: 'agents.container.worktreeAutoMerge.help' })} checked={ctAdv.worktree_auto_merge} onChange={(v) => updateCtAdv('worktree_auto_merge', v)} />
                <MountTable mounts={ctAdv.additional_mounts} onChange={(v) => updateCtAdv('additional_mounts', v)} />
              </DangerZone>
            </div>
          )}

          {advGroup === 'evo' && (
            <div className="space-y-4">
              <p className="text-xs text-stone-400 dark:text-stone-500">{intl.formatMessage({ id: 'agents.evo.desc' })}</p>

              <div className="space-y-1">
                <h4 className="mb-1 text-xs font-semibold uppercase text-stone-500 dark:text-stone-400">{intl.formatMessage({ id: 'agents.evo.externalFactors' })}</h4>
                <SwitchRow checked={evoAdv.external_factors.user_feedback} onChange={(v) => updateEvoFactor('user_feedback', v)} label={intl.formatMessage({ id: 'agents.evo.userFeedback' })} />
                <SwitchRow checked={evoAdv.external_factors.security_events} onChange={(v) => updateEvoFactor('security_events', v)} label={intl.formatMessage({ id: 'agents.evo.securityEvents' })} />
                <SwitchRow checked={evoAdv.external_factors.channel_metrics} onChange={(v) => updateEvoFactor('channel_metrics', v)} label={intl.formatMessage({ id: 'agents.evo.channelMetrics' })} />
                <SwitchRow checked={evoAdv.external_factors.business_context} onChange={(v) => updateEvoFactor('business_context', v)} label={intl.formatMessage({ id: 'agents.evo.businessContext' })} />
                <SwitchRow checked={evoAdv.external_factors.peer_signals} onChange={(v) => updateEvoFactor('peer_signals', v)} label={intl.formatMessage({ id: 'agents.evo.peerSignals' })} />
              </div>

              <div className="border-t border-stone-200 pt-4 dark:border-stone-700 space-y-2">
                <h4 className="text-xs font-semibold uppercase text-stone-500 dark:text-stone-400">{intl.formatMessage({ id: 'agents.evo.skillSynthesis' })}</h4>
                <SwitchRow checked={evoAdv.skill_synthesis_enabled} onChange={(v) => updateEvoAdv('skill_synthesis_enabled', v)} label={intl.formatMessage({ id: 'agents.evo.enabled' })} />
                <div className="grid grid-cols-2 gap-3">
                  <FormField label={intl.formatMessage({ id: 'agents.evo.threshold' })} hint={intl.formatMessage({ id: 'agents.evo.synthesisThreshold.hint' })}>
                    <input type="number" min={1} step={1} value={evoAdv.skill_synthesis_threshold} onChange={(e) => updateEvoAdv('skill_synthesis_threshold', Math.round(Number(e.target.value)))} className={inputClass} />
                  </FormField>
                  <FormField label={intl.formatMessage({ id: 'agents.evo.cooldownHours' })}>
                    <input type="number" min={0} value={evoAdv.skill_synthesis_cooldown_hours} onChange={(e) => updateEvoAdv('skill_synthesis_cooldown_hours', Number(e.target.value))} className={inputClass} />
                  </FormField>
                  <FormField label={intl.formatMessage({ id: 'agents.evo.trialTtl' })} hint={intl.formatMessage({ id: 'agents.evo.trialTtl.hint' })}>
                    <input type="number" min={0} value={evoAdv.skill_trial_ttl} onChange={(e) => updateEvoAdv('skill_trial_ttl', Number(e.target.value))} className={inputClass} />
                  </FormField>
                </div>
              </div>

              <div className="border-t border-stone-200 pt-4 dark:border-stone-700 space-y-2">
                <h4 className="text-xs font-semibold uppercase text-stone-500 dark:text-stone-400">{intl.formatMessage({ id: 'agents.evo.skillGraduation' })}</h4>
                <SwitchRow checked={evoAdv.skill_graduation_enabled} onChange={(v) => updateEvoAdv('skill_graduation_enabled', v)} label={intl.formatMessage({ id: 'agents.evo.enabled' })} />
                <FormField label={intl.formatMessage({ id: 'agents.evo.minLift' })} hint="0.0-1.0">
                  <input type="number" min={0} max={1} step={0.05} value={evoAdv.skill_graduation_min_lift} onChange={(e) => updateEvoAdv('skill_graduation_min_lift', Number(e.target.value))} className={inputClass} />
                </FormField>
              </div>

              <div className="border-t border-stone-200 pt-4 dark:border-stone-700 space-y-2">
                <h4 className="text-xs font-semibold uppercase text-stone-500 dark:text-stone-400">{intl.formatMessage({ id: 'agents.evo.skillRecommendation' })}</h4>
                <SwitchRow checked={evoAdv.skill_recommendation_enabled} onChange={(v) => updateEvoAdv('skill_recommendation_enabled', v)} label={intl.formatMessage({ id: 'agents.evo.enabled' })} />
                <FormField label={intl.formatMessage({ id: 'agents.evo.threshold' })} hint="0.0-1.0">
                  <input type="number" min={0} max={1} step={0.05} value={evoAdv.skill_recommendation_threshold} onChange={(e) => updateEvoAdv('skill_recommendation_threshold', Number(e.target.value))} className={inputClass} />
                </FormField>
              </div>

              <div className="border-t border-stone-200 pt-4 dark:border-stone-700 space-y-2">
                <h4 className="text-xs font-semibold uppercase text-stone-500 dark:text-stone-400">{intl.formatMessage({ id: 'agents.evo.curiosity' })}</h4>
                <SwitchRow checked={evoAdv.curiosity_enabled} onChange={(v) => updateEvoAdv('curiosity_enabled', v)} label={intl.formatMessage({ id: 'agents.evo.enabled' })} />
                <div className="grid grid-cols-2 gap-3">
                  <FormField label={intl.formatMessage({ id: 'agents.evo.threshold' })} hint="0.0-1.0">
                    <input type="number" min={0} max={1} step={0.05} value={evoAdv.curiosity_threshold} onChange={(e) => updateEvoAdv('curiosity_threshold', Number(e.target.value))} className={inputClass} />
                  </FormField>
                  <FormField label={intl.formatMessage({ id: 'agents.evo.maxDaily' })}>
                    <input type="number" min={0} value={evoAdv.curiosity_max_daily} onChange={(e) => updateEvoAdv('curiosity_max_daily', Number(e.target.value))} className={inputClass} />
                  </FormField>
                </div>
              </div>

              <div className="border-t border-stone-200 pt-4 dark:border-stone-700 space-y-2">
                <h4 className="text-xs font-semibold uppercase text-stone-500 dark:text-stone-400">{intl.formatMessage({ id: 'agents.evo.behaviorMonitor' })}</h4>
                <SwitchRow checked={evoAdv.skill_behavior_monitor_enabled} onChange={(v) => updateEvoAdv('skill_behavior_monitor_enabled', v)} label={intl.formatMessage({ id: 'agents.evo.enabled' })} />
                <FormField label={intl.formatMessage({ id: 'agents.evo.driftThreshold' })} hint="0.0-1.0">
                  <input type="number" min={0} max={1} step={0.05} value={evoAdv.skill_behavior_drift_threshold} onChange={(e) => updateEvoAdv('skill_behavior_drift_threshold', Number(e.target.value))} className={inputClass} />
                </FormField>
              </div>
            </div>
          )}

          {advGroup === 'access' && (
            <div className="space-y-4">
              <div className="space-y-1">
                <h4 className="text-xs font-semibold uppercase text-stone-500 dark:text-stone-400 mb-2">{intl.formatMessage({ id: 'agents.edit.section.permissions' })}</h4>
                <SwitchRow label={intl.formatMessage({ id: 'agents.edit.canSendCrossAgent' })} help={intl.formatMessage({ id: 'agents.edit.canSendCrossAgent.help' })} checked={form.can_send_cross_agent ?? true} onChange={(v) => updateField('can_send_cross_agent', v)} />
                <SwitchRow label={intl.formatMessage({ id: 'agents.edit.canModifySkills' })} help={intl.formatMessage({ id: 'agents.edit.canModifySkills.help' })} checked={form.can_modify_own_skills ?? true} onChange={(v) => updateField('can_modify_own_skills', v)} />
                <SwitchRow label={intl.formatMessage({ id: 'agents.edit.canScheduleTasks' })} help={intl.formatMessage({ id: 'agents.edit.canScheduleTasks.help' })} checked={form.can_schedule_tasks ?? false} onChange={(v) => updateField('can_schedule_tasks', v)} />
                <SwitchRow label={intl.formatMessage({ id: 'agents.edit.skillSecurityScan' })} help={intl.formatMessage({ id: 'agents.edit.skillSecurityScan.help' })} checked={form.skill_security_scan ?? true} onChange={(v) => updateField('skill_security_scan', v)} />
                <SwitchRow label={intl.formatMessage({ id: 'agents.edit.gvuEnabled' })} help={intl.formatMessage({ id: 'agents.edit.gvuEnabled.help' })} checked={form.gvu_enabled ?? true} onChange={(v) => updateField('gvu_enabled', v)} />
                <SwitchRow label={intl.formatMessage({ id: 'agents.edit.cognitiveMemory' })} help={intl.formatMessage({ id: 'agents.edit.cognitiveMemory.help' })} checked={form.cognitive_memory ?? false} onChange={(v) => updateField('cognitive_memory', v)} />
                <SettingField label={intl.formatMessage({ id: 'agents.edit.maxActiveSkills' })}>
                  <input type="number" min={1} max={20} value={form.max_active_skills ?? 5} onChange={(e) => updateField('max_active_skills', Number(e.target.value))} className={inputClass} />
                </SettingField>
                <SettingField label={intl.formatMessage({ id: 'agents.edit.maxSilenceHours' })}>
                  <input type="number" min={1} step={0.5} value={form.max_silence_hours ?? 12} onChange={(e) => updateField('max_silence_hours', Number(e.target.value))} className={inputClass} />
                </SettingField>
              </div>

              {/* DangerZone — privilege escalation */}
              <DangerZone title={intl.formatMessage({ id: 'agents.perm.danger.title' })} description={intl.formatMessage({ id: 'agents.perm.danger.desc' })}>
                <SwitchRow label={intl.formatMessage({ id: 'agents.edit.canCreateAgents' })} help={intl.formatMessage({ id: 'agents.edit.canCreateAgents.help' })} checked={form.can_create_agents ?? false} onChange={(v) => updateField('can_create_agents', v)} />
                <SwitchRow label={intl.formatMessage({ id: 'agents.edit.canModifySoul' })} help={intl.formatMessage({ id: 'agents.edit.canModifySoul.help' })} checked={form.can_modify_own_soul ?? false} onChange={(v) => updateField('can_modify_own_soul', v)} />
                <SwitchRow label={intl.formatMessage({ id: 'agents.edit.skillAutoActivate' })} help={intl.formatMessage({ id: 'agents.edit.skillAutoActivate.help' })} checked={form.skill_auto_activate ?? false} onChange={(v) => updateField('skill_auto_activate', v)} />
              </DangerZone>

              {/* Capabilities */}
              <div className="border-t border-stone-200 pt-4 dark:border-stone-700 space-y-4">
                <h4 className="text-xs font-semibold uppercase text-stone-500 dark:text-stone-400">{intl.formatMessage({ id: 'agents.edit.capabilities' })}</h4>
                <p className="text-xs text-stone-400 dark:text-stone-500">{intl.formatMessage({ id: 'agents.cap.desc' })}</p>
                <SettingField label={intl.formatMessage({ id: 'agents.cap.allowedTools' })} help={intl.formatMessage({ id: 'agents.cap.allowedTools.hint' })}>
                  <ChipEditor values={caps.allowed_tools} onChange={(v) => updateCap('allowed_tools', v)} placeholder="Read" addLabel={intl.formatMessage({ id: 'common.add' })} />
                </SettingField>
                <SettingField label={intl.formatMessage({ id: 'agents.cap.deniedTools' })} help={intl.formatMessage({ id: 'agents.cap.deniedTools.hint' })}>
                  <ChipEditor values={caps.denied_tools} onChange={(v) => updateCap('denied_tools', v)} placeholder="Bash" addLabel={intl.formatMessage({ id: 'common.add' })} />
                </SettingField>
                <SettingField label={intl.formatMessage({ id: 'agents.cap.wikiVisibleTo' })} help={intl.formatMessage({ id: 'agents.cap.wikiVisibleTo.hint' })}>
                  <ChipEditor values={caps.wiki_visible_to} onChange={(v) => updateCap('wiki_visible_to', v)} placeholder="coder" addLabel={intl.formatMessage({ id: 'common.add' })} />
                </SettingField>
                <SwitchRow
                  label={intl.formatMessage({ id: 'agents.cap.nativeSandbox' })}
                  help={intl.formatMessage({ id: 'agents.cap.nativeSandbox.help' })}
                  checked={caps.native_sandbox}
                  onChange={(v) => updateCap('native_sandbox', v)}
                />
                <SettingField label={intl.formatMessage({ id: 'agents.cap.policy' })} help={intl.formatMessage({ id: 'agents.cap.policy.help' })}>
                  <ToolPolicyEditor value={caps.policy} onChange={(v) => updateCap('policy', v)} />
                </SettingField>
              </div>

              {/* DangerZone — Computer Use */}
              <DangerZone title={intl.formatMessage({ id: 'agents.cap.danger.title' })} description={intl.formatMessage({ id: 'agents.cap.danger.desc' })}>
                <SwitchRow label={intl.formatMessage({ id: 'agents.cap.computerUse' })} help={intl.formatMessage({ id: 'agents.cap.computerUse.help' })} checked={caps.computer_use} onChange={(v) => updateCap('computer_use', v)} />
                <SettingField label={intl.formatMessage({ id: 'agents.cap.computerUseMode' })} help={intl.formatMessage({ id: 'agents.cap.computerUseMode.help' })}>
                  <OptionSelect value={caps.computer_use_mode} onChange={(v) => updateCap('computer_use_mode', v as ComputerUseMode)} options={computerUseModeOptions} />
                </SettingField>
                {caps.computer_use_mode === 'native' && (
                  <p className="rounded-md bg-rose-500/10 px-3 py-2 text-xs text-rose-700 dark:text-rose-300">{intl.formatMessage({ id: 'agents.cap.nativeWarning' })}</p>
                )}
                <SwitchRow label={intl.formatMessage({ id: 'agents.cap.browserViaBash' })} help={intl.formatMessage({ id: 'agents.cap.browserViaBash.help' })} checked={caps.browser_via_bash} onChange={(v) => updateCap('browser_via_bash', v)} />
                <div className="space-y-3 border-t border-rose-300/40 pt-3">
                  <h5 className="text-xs font-semibold uppercase text-rose-700 dark:text-rose-300">{intl.formatMessage({ id: 'agents.cap.computerUseConfig' })}</h5>
                  <SettingField label={intl.formatMessage({ id: 'agents.cap.allowedApps' })}>
                    <ChipEditor values={caps.computer_use_config.allowed_apps ?? []} onChange={(v) => updateCapConfig('allowed_apps', v)} placeholder="Safari" addLabel={intl.formatMessage({ id: 'common.add' })} />
                  </SettingField>
                  <SettingField label={intl.formatMessage({ id: 'agents.cap.blockedActions' })}>
                    <ChipEditor values={caps.computer_use_config.blocked_actions ?? []} onChange={(v) => updateCapConfig('blocked_actions', v)} placeholder="key:cmd+q" addLabel={intl.formatMessage({ id: 'common.add' })} />
                  </SettingField>
                  <div className="grid grid-cols-2 gap-3">
                    <FormField label={intl.formatMessage({ id: 'agents.cap.maxSessionMinutes' })} hint="1-1440">
                      <input type="number" min={1} max={1440} value={caps.computer_use_config.max_session_minutes} onChange={(e) => updateCapConfig('max_session_minutes', Number(e.target.value))} className={inputClass} />
                    </FormField>
                    <FormField label={intl.formatMessage({ id: 'agents.cap.maxActions' })} hint="1-10000">
                      <input type="number" min={1} max={10000} value={caps.computer_use_config.max_actions} onChange={(e) => updateCapConfig('max_actions', Number(e.target.value))} className={inputClass} />
                    </FormField>
                    <FormField label={intl.formatMessage({ id: 'agents.cap.displayWidth' })} hint="320-7680">
                      <input type="number" min={320} max={7680} value={caps.computer_use_config.display_width} onChange={(e) => updateCapConfig('display_width', Number(e.target.value))} className={inputClass} />
                    </FormField>
                    <FormField label={intl.formatMessage({ id: 'agents.cap.displayHeight' })} hint="240-4320">
                      <input type="number" min={240} max={4320} value={caps.computer_use_config.display_height} onChange={(e) => updateCapConfig('display_height', Number(e.target.value))} className={inputClass} />
                    </FormField>
                  </div>
                  <SwitchRow label={intl.formatMessage({ id: 'agents.cap.autoConfirmTrusted' })} help={intl.formatMessage({ id: 'agents.cap.autoConfirmTrusted.help' })} checked={caps.computer_use_config.auto_confirm_trusted ?? false} onChange={(v) => updateCapConfig('auto_confirm_trusted', v)} />
                </div>
              </DangerZone>
            </div>
          )}

          {advGroup === 'access' && (
            <div className="space-y-4">
              <p className="text-xs text-stone-400 dark:text-stone-500">
                {intl.formatMessage({ id: 'agents.contract.desc' })}
              </p>
              {!contractLoaded ? (
                <p className="py-8 text-center text-sm text-stone-400">{intl.formatMessage({ id: 'common.loading' })}</p>
              ) : (
                <>
                  <FormField label={intl.formatMessage({ id: 'agents.contract.mustNot' })} hint={intl.formatMessage({ id: 'agents.contract.mustNot.hint' })}>
                    <textarea
                      value={contract.must_not.join('\n')}
                      onChange={(e) => setContract((p) => ({ ...p, must_not: e.target.value.split('\n').map((s) => s.trimEnd()).filter((s) => s.trim() !== '') }))}
                      rows={4}
                      placeholder={intl.formatMessage({ id: 'agents.contract.mustNot.placeholder' })}
                      className={cn(inputClass, 'resize-none font-mono')}
                    />
                  </FormField>
                  <FormField label={intl.formatMessage({ id: 'agents.contract.mustAlways' })} hint={intl.formatMessage({ id: 'agents.contract.mustAlways.hint' })}>
                    <textarea
                      value={contract.must_always.join('\n')}
                      onChange={(e) => setContract((p) => ({ ...p, must_always: e.target.value.split('\n').map((s) => s.trimEnd()).filter((s) => s.trim() !== '') }))}
                      rows={4}
                      placeholder={intl.formatMessage({ id: 'agents.contract.mustAlways.placeholder' })}
                      className={cn(inputClass, 'resize-none font-mono')}
                    />
                  </FormField>
                  <FormField label={intl.formatMessage({ id: 'agents.contract.maxToolCalls' })} hint={intl.formatMessage({ id: 'agents.contract.maxToolCalls.hint' })}>
                    <input type="number" min={0} max={1000} value={contract.max_tool_calls_per_turn} onChange={(e) => setContract((p) => ({ ...p, max_tool_calls_per_turn: Number(e.target.value) }))} className={inputClass} />
                  </FormField>
                  <div className="flex justify-end">
                    <Button variant="primary" onClick={handleContractSave} disabled={contractSaving}>
                      {contractSaving ? intl.formatMessage({ id: 'common.saving' }) : intl.formatMessage({ id: 'agents.contract.save' })}
                    </Button>
                  </div>
                </>
              )}
            </div>
          )}

          {advGroup === 'integration' && (
            <div className="space-y-4">
              <h4 className="text-xs font-semibold uppercase text-stone-500 dark:text-stone-400">Odoo</h4>
              <p className="text-xs text-stone-400 dark:text-stone-500">
                {intl.formatMessage({ id: 'agents.odoo.desc' })}
              </p>
              <FormField label={intl.formatMessage({ id: 'agents.odoo.profile' })} hint={intl.formatMessage({ id: 'agents.odoo.profile.hint' })}>
                <input type="text" value={odoo.profile} onChange={(e) => updateOdoo('profile', e.target.value)} placeholder="default" className={inputClass} />
              </FormField>
              <FormField label={intl.formatMessage({ id: 'agents.odoo.allowedModels' })} hint={intl.formatMessage({ id: 'agents.odoo.allowedModels.hint' })}>
                <ChipEditor values={odoo.allowed_models} onChange={(v) => updateOdoo('allowed_models', v)} placeholder="crm.lead" addLabel={intl.formatMessage({ id: 'common.add' })} />
              </FormField>
              <FormField label={intl.formatMessage({ id: 'agents.odoo.allowedActions' })} hint={intl.formatMessage({ id: 'agents.odoo.allowedActions.hint' })}>
                <ChipEditor values={odoo.allowed_actions} onChange={(v) => updateOdoo('allowed_actions', v)} placeholder="write:crm.lead" addLabel={intl.formatMessage({ id: 'common.add' })} />
              </FormField>
              <FormField label={intl.formatMessage({ id: 'agents.odoo.companyIds' })} hint={intl.formatMessage({ id: 'agents.odoo.companyIds.hint' })}>
                <input type="text" value={odoo.company_ids} onChange={(e) => updateOdoo('company_ids', e.target.value)} placeholder="1, 2" className={inputClass} />
              </FormField>
              <div className="border-t border-stone-200 pt-4 dark:border-stone-700 space-y-4">
                <h4 className="text-xs font-semibold uppercase text-stone-500 dark:text-stone-400">{intl.formatMessage({ id: 'agents.odoo.connection' })}</h4>
                <FormField label="URL">
                  <input type="text" value={odoo.url} onChange={(e) => updateOdoo('url', e.target.value)} placeholder="https://erp.example.com" className={inputClass} />
                </FormField>
                <div className="grid grid-cols-2 gap-3">
                  <FormField label="DB">
                    <input type="text" value={odoo.db} onChange={(e) => updateOdoo('db', e.target.value)} className={inputClass} />
                  </FormField>
                  <FormField label={intl.formatMessage({ id: 'agents.odoo.username' })}>
                    <input type="text" value={odoo.username} onChange={(e) => updateOdoo('username', e.target.value)} className={inputClass} />
                  </FormField>
                </div>
                <FormField label={intl.formatMessage({ id: 'agents.odoo.apiKey' })} hint={intl.formatMessage({ id: 'agents.odoo.secret.hint' })}>
                  <input type="password" value={odoo.api_key} onChange={(e) => updateOdoo('api_key', e.target.value)} className={inputClass} autoComplete="off" />
                </FormField>
                <FormField label={intl.formatMessage({ id: 'agents.odoo.password' })} hint={intl.formatMessage({ id: 'agents.odoo.secret.hint' })}>
                  <input type="password" value={odoo.password} onChange={(e) => updateOdoo('password', e.target.value)} className={inputClass} autoComplete="off" />
                </FormField>
              </div>
            </div>
          )}

          {advGroup === 'integration' && (
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
                <FormField label="Verification Token">
                  <input type="password" value={form.feishu_verification_token ?? ''} onChange={(e) => updateField('feishu_verification_token', e.target.value)} className={inputClass} autoComplete="off" />
                </FormField>
              </div>

              {/* UI.3 — WhatsApp App Secret (already-supported field) */}
              <div className="space-y-2 border-t border-stone-200 pt-4 dark:border-stone-700">
                <h4 className="text-xs font-semibold uppercase text-stone-500 dark:text-stone-400">WhatsApp (extra)</h4>
                <FormField label="App Secret">
                  <input type="password" value={form.whatsapp_app_secret ?? ''} onChange={(e) => updateField('whatsapp_app_secret', e.target.value)} className={inputClass} autoComplete="off" />
                </FormField>
              </div>
            </div>
          )}

          {advGroup === 'evo' && (
            <div className="space-y-5 border-t border-stone-200 pt-4 dark:border-stone-700">
              <p className="text-xs text-stone-400 dark:text-stone-500">{intl.formatMessage({ id: 'agents.adv.desc' })}</p>

              {/* UI.3 — already-supported scalar fields */}
              <div className="space-y-3">
                <h4 className="text-xs font-semibold uppercase text-stone-500 dark:text-stone-400">{intl.formatMessage({ id: 'agents.adv.status' })}</h4>
                <SettingField label={intl.formatMessage({ id: 'agents.adv.statusField' })}>
                  <OptionSelect value={form.status ?? 'active'} onChange={(v) => updateField('status', v)} options={statusOptions} />
                </SettingField>
                <div className="grid grid-cols-2 gap-3">
                  <FormField label={intl.formatMessage({ id: 'agents.adv.maxGvuGenerations' })}>
                    <input type="number" min={0} value={form.max_gvu_generations ?? 3} onChange={(e) => updateField('max_gvu_generations', Number(e.target.value))} className={inputClass} />
                  </FormField>
                  <FormField label={intl.formatMessage({ id: 'agents.adv.observationHours' })}>
                    <input type="number" min={0} step={0.5} value={form.observation_period_hours ?? 24} onChange={(e) => updateField('observation_period_hours', Number(e.target.value))} className={inputClass} />
                  </FormField>
                  <FormField label={intl.formatMessage({ id: 'agents.adv.skillTokenBudget' })}>
                    <input type="number" min={0} value={form.skill_token_budget ?? 0} onChange={(e) => updateField('skill_token_budget', Number(e.target.value))} className={inputClass} />
                  </FormField>
                </div>
              </div>

              {/* UI.3 — stagnation detection */}
              <div className="border-t border-stone-200 pt-4 dark:border-stone-700 space-y-2">
                <h4 className="text-xs font-semibold uppercase text-stone-500 dark:text-stone-400">{intl.formatMessage({ id: 'agents.adv.stagnation' })}</h4>
                <SwitchRow checked={adv.stagnation_enabled} onChange={(v) => updateAdv('stagnation_enabled', v)} label={intl.formatMessage({ id: 'agents.evo.enabled' })} />
                <div className="grid grid-cols-2 gap-3">
                  <FormField label={intl.formatMessage({ id: 'agents.adv.stagnationWindow' })}>
                    <input type="number" min={1} value={adv.stagnation_window_seconds} onChange={(e) => updateAdv('stagnation_window_seconds', Number(e.target.value))} className={inputClass} />
                  </FormField>
                  <FormField label={intl.formatMessage({ id: 'agents.adv.stagnationThreshold' })}>
                    <input type="number" min={1} value={adv.stagnation_trigger_threshold} onChange={(e) => updateAdv('stagnation_trigger_threshold', Number(e.target.value))} className={inputClass} />
                  </FormField>
                </div>
                <SettingField label={intl.formatMessage({ id: 'agents.adv.stagnationAction' })}>
                  <OptionSelect value={adv.stagnation_action} onChange={(v) => updateAdv('stagnation_action', v as 'log_only' | 'suppress')} options={stagnationActionOptions} />
                </SettingField>
              </div>

              {/* G.8 — model extras */}
              <div className="border-t border-stone-200 pt-4 dark:border-stone-700 space-y-3">
                <h4 className="text-xs font-semibold uppercase text-stone-500 dark:text-stone-400">{intl.formatMessage({ id: 'agents.adv.modelExtras' })}</h4>
                <FormField label={intl.formatMessage({ id: 'agents.adv.accountPool' })} hint={intl.formatMessage({ id: 'agents.adv.accountPool.hint' })}>
                  <ChipEditor values={adv.account_pool} onChange={(v) => updateAdv('account_pool', v)} placeholder="oauth-pro" addLabel={intl.formatMessage({ id: 'common.add' })} />
                </FormField>
                <FormField label={intl.formatMessage({ id: 'agents.adv.utility' })} hint={intl.formatMessage({ id: 'agents.adv.utility.hint' })}>
                  <ModelSelect
                    value={adv.utility}
                    onChange={(v) => updateAdv('utility', v)}
                    models={availableModels}
                    loading={modelsLoading}
                    error={modelsError}
                    discoveredAt={modelsDiscoveredAt}
                    refreshing={modelsRefreshing}
                    onRefresh={modelsRefresh}
                    ariaLabel={intl.formatMessage({ id: 'agents.adv.utility' })}
                  />
                </FormField>
              </div>

              {/* G.8 — heartbeat extras */}
              <div className="border-t border-stone-200 pt-4 dark:border-stone-700 grid grid-cols-2 gap-3">
                <FormField label={intl.formatMessage({ id: 'agents.adv.maxConcurrentRuns' })}>
                  <input type="number" min={1} max={64} value={adv.heartbeat_max_concurrent_runs} onChange={(e) => updateAdv('heartbeat_max_concurrent_runs', Number(e.target.value))} className={inputClass} />
                </FormField>
                <FormField label={intl.formatMessage({ id: 'agents.adv.cronTimezone' })} hint="Asia/Taipei">
                  <input type="text" value={adv.heartbeat_cron_timezone} onChange={(e) => updateAdv('heartbeat_cron_timezone', e.target.value)} placeholder="Asia/Taipei" className={inputClass} />
                </FormField>
              </div>

              {/* G.8 — proactive extras */}
              <div className="border-t border-stone-200 pt-4 dark:border-stone-700 grid grid-cols-2 gap-3">
                <FormField label={intl.formatMessage({ id: 'agents.adv.tokenBudgetPerCheck' })}>
                  <input type="number" min={0} value={adv.proactive_token_budget_per_check} onChange={(e) => updateAdv('proactive_token_budget_per_check', Number(e.target.value))} className={inputClass} />
                </FormField>
                <FormField label={intl.formatMessage({ id: 'agents.adv.proactiveMaxTurns' })}>
                  <input type="number" min={1} max={100} value={adv.proactive_max_turns} onChange={(e) => updateAdv('proactive_max_turns', Number(e.target.value))} className={inputClass} />
                </FormField>
                <FormField label={intl.formatMessage({ id: 'agents.adv.proactiveTimezone' })} hint="Asia/Taipei">
                  <input type="text" value={adv.proactive_timezone} onChange={(e) => updateAdv('proactive_timezone', e.target.value)} placeholder="Asia/Taipei" className={inputClass} />
                </FormField>
              </div>

              {/* G.8 — free-form scalar tables */}
              <div className="border-t border-stone-200 pt-4 dark:border-stone-700 space-y-2">
                <p className="rounded-md bg-amber-500/10 px-3 py-2 text-xs text-amber-700 dark:text-amber-300">{intl.formatMessage({ id: 'agents.adv.kv.warning' })}</p>
                <KvTable title={intl.formatMessage({ id: 'agents.adv.ptc' })} rows={adv.ptc} onChange={(v) => updateAdv('ptc', v)} />
                <KvTable title={intl.formatMessage({ id: 'agents.adv.prompt' })} rows={adv.prompt} onChange={(v) => updateAdv('prompt', v)} />
                <KvTable title={intl.formatMessage({ id: 'agents.adv.culturalContext' })} rows={adv.cultural_context} onChange={(v) => updateAdv('cultural_context', v)} />
              </div>
            </div>
          )}
            </div>
          )}
        </div>

        {/* Error + Actions */}
        {error && <p className="text-sm text-rose-600 dark:text-rose-400">{error}</p>}
        <div className="flex justify-end gap-3 border-t border-[var(--panel-border)] pt-4">
          <Button variant="secondary" onClick={onClose}>{intl.formatMessage({ id: 'common.cancel' })}</Button>
          <Button variant="primary" onClick={handleSave} disabled={saving}>
            {saving ? intl.formatMessage({ id: 'common.saving' }) : intl.formatMessage({ id: 'common.save' })}
          </Button>
        </div>
      </div>
    </Dialog>
  );
}

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div>
      <h4 className="mb-2 text-sm font-semibold text-stone-700 dark:text-stone-300">{title}</h4>
      <div className="rounded-lg border border-[var(--panel-border)] bg-stone-500/5 p-3 dark:bg-white/5">{children}</div>
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

// ── CT — additional_mounts table editor ──

function MountTable({ mounts, onChange }: { mounts: ReadonlyArray<ContainerMount>; onChange: (next: ContainerMount[]) => void }) {
  const intl = useIntl();
  const update = (idx: number, patch: Partial<ContainerMount>) =>
    onChange(mounts.map((m, i) => (i === idx ? { ...m, ...patch } : m)));
  const remove = (idx: number) => onChange(mounts.filter((_, i) => i !== idx));
  const add = () => onChange([...mounts, { host: '', container: '', readonly: true }]);

  return (
    <div className="space-y-2">
      <div className="flex items-center justify-between">
        <h4 className="text-xs font-semibold uppercase text-stone-500 dark:text-stone-400">{intl.formatMessage({ id: 'agents.container.mounts' })}</h4>
        <Button type="button" size="sm" variant="ghost" icon={Plus} onClick={add}>
          {intl.formatMessage({ id: 'common.add' })}
        </Button>
      </div>
      <p className="text-xs text-stone-400 dark:text-stone-500">{intl.formatMessage({ id: 'agents.container.mounts.hint' })}</p>
      {mounts.length === 0 ? (
        <p className="py-2 text-center text-xs text-stone-400">{intl.formatMessage({ id: 'agents.container.mounts.empty' })}</p>
      ) : (
        <div className="space-y-2">
          {mounts.map((m, idx) => (
            <div key={idx} className="flex items-center gap-2">
              <input type="text" value={m.host} onChange={(e) => update(idx, { host: e.target.value })} placeholder={intl.formatMessage({ id: 'agents.container.mounts.host' })} className={cn(inputClass, 'flex-1')} />
              <input type="text" value={m.container} onChange={(e) => update(idx, { container: e.target.value })} placeholder={intl.formatMessage({ id: 'agents.container.mounts.container' })} className={cn(inputClass, 'flex-1')} />
              <label className="flex shrink-0 items-center gap-1 text-xs text-stone-500 dark:text-stone-400">
                <input type="checkbox" checked={m.readonly} onChange={(e) => update(idx, { readonly: e.target.checked })} className="accent-amber-500" />
                {intl.formatMessage({ id: 'agents.container.mounts.readonly' })}
              </label>
              <Button type="button" size="sm" variant="ghost" icon={Trash2} onClick={() => remove(idx)} className="shrink-0 text-rose-500 hover:bg-rose-500/10 hover:text-rose-600 dark:text-rose-400" aria-label="remove mount" />
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

// ── Advanced — generic key/value scalar table editor (G.8 ptc/prompt/cultural) ──

function KvTable({ title, rows, onChange }: { title: string; rows: ReadonlyArray<KvRow>; onChange: (next: KvRow[]) => void }) {
  const intl = useIntl();
  const update = (idx: number, patch: Partial<KvRow>) =>
    onChange(rows.map((r, i) => (i === idx ? { ...r, ...patch } : r)));
  const remove = (idx: number) => onChange(rows.filter((_, i) => i !== idx));
  const add = () => onChange([...rows, { key: '', value: '' }]);

  return (
    <div className="space-y-2 border-t border-stone-200 pt-4 dark:border-stone-700">
      <div className="flex items-center justify-between">
        <h4 className="text-xs font-semibold uppercase text-stone-500 dark:text-stone-400">{title}</h4>
        <Button type="button" size="sm" variant="ghost" icon={Plus} onClick={add}>
          {intl.formatMessage({ id: 'common.add' })}
        </Button>
      </div>
      {rows.length === 0 ? (
        <p className="py-1 text-center text-xs text-stone-400">{intl.formatMessage({ id: 'agents.adv.kv.empty' })}</p>
      ) : (
        <div className="space-y-2">
          {rows.map((r, idx) => (
            <div key={idx} className="flex items-center gap-2">
              <input type="text" value={r.key} onChange={(e) => update(idx, { key: e.target.value })} placeholder="key" className={cn(inputClass, 'flex-1')} />
              <input type="text" value={r.value} onChange={(e) => update(idx, { value: e.target.value })} placeholder="value" className={cn(inputClass, 'flex-1')} />
              <Button type="button" size="sm" variant="ghost" icon={Trash2} onClick={() => remove(idx)} className="shrink-0 text-rose-500 hover:bg-rose-500/10 hover:text-rose-600 dark:text-rose-400" aria-label="remove row" />
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

// ── CT — env table editor ──

function EnvTable({ env, onChange }: { env: ReadonlyArray<ContainerEnvVar>; onChange: (next: ContainerEnvVar[]) => void }) {
  const intl = useIntl();
  const update = (idx: number, patch: Partial<ContainerEnvVar>) =>
    onChange(env.map((e, i) => (i === idx ? { ...e, ...patch } : e)));
  const remove = (idx: number) => onChange(env.filter((_, i) => i !== idx));
  const add = () => onChange([...env, { key: '', value: '' }]);

  return (
    <div className="space-y-2">
      <div className="flex items-center justify-between">
        <h4 className="text-xs font-semibold uppercase text-stone-500 dark:text-stone-400">{intl.formatMessage({ id: 'agents.container.env' })}</h4>
        <Button type="button" size="sm" variant="ghost" icon={Plus} onClick={add}>
          {intl.formatMessage({ id: 'common.add' })}
        </Button>
      </div>
      {env.length === 0 ? (
        <p className="py-2 text-center text-xs text-stone-400">{intl.formatMessage({ id: 'agents.container.env.empty' })}</p>
      ) : (
        <div className="space-y-2">
          {env.map((e, idx) => (
            <div key={idx} className="flex items-center gap-2">
              <input type="text" value={e.key} onChange={(ev) => update(idx, { key: ev.target.value })} placeholder="KEY" className={cn(inputClass, 'flex-1')} />
              <input type="text" value={e.value} onChange={(ev) => update(idx, { value: ev.target.value })} placeholder="value" className={cn(inputClass, 'flex-1')} />
              <Button type="button" size="sm" variant="ghost" icon={Trash2} onClick={() => remove(idx)} className="shrink-0 text-rose-500 hover:bg-rose-500/10 hover:text-rose-600 dark:text-rose-400" aria-label="remove env" />
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
