import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { usePlansStore } from '@/stores/plans-store';
import { useAgentsStore } from '@/stores/agents-store';
import { useAuthStore } from '@/stores/auth-store';
import { Dialog } from '@/components/shared/Dialog';
import {
  Page,
  PageHeader,
  Card,
  Button,
  Badge,
  EmptyState,
  Field,
  controlClass,
  InlineEditor,
  CharacterAvatar,
  Mono,
} from '@/components/ui';
import { cycleStepStatus, planProgress, stepMoveTarget } from '@/lib/plan-utils';
import { timeAgo } from '@/lib/format';
import type { PlanInfo, PlanStep, PlanStepStatus } from '@/lib/api';
import {
  ListChecks,
  Plus,
  Circle,
  CircleDotDashed,
  CheckCircle2,
  MinusCircle,
  ChevronUp,
  ChevronDown,
  Trash2,
  User,
  SkipForward,
  Undo2,
} from 'lucide-react';

const STATUS_ICON: Record<PlanStepStatus, typeof Circle> = {
  todo: Circle,
  doing: CircleDotDashed,
  done: CheckCircle2,
  skipped: MinusCircle,
};

const STATUS_CLASS: Record<PlanStepStatus, string> = {
  todo: 'text-stone-400 dark:text-stone-500',
  doing: 'text-sky-500 dark:text-sky-400',
  done: 'text-emerald-500 dark:text-emerald-400',
  skipped: 'text-stone-300 dark:text-stone-600',
};

/** Assignee chip + picker: a person (the user) or an AI employee. */
function AssigneePicker({
  step,
  onPick,
}: {
  step: PlanStep;
  onPick: (kind: 'user' | 'agent', assignee: string) => void;
}) {
  const intl = useIntl();
  const { agents } = useAgentsStore();
  const currentUser = useAuthStore((s) => s.user);
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const onDoc = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener('mousedown', onDoc);
    return () => document.removeEventListener('mousedown', onDoc);
  }, [open]);

  const agent = step.assignee_kind === 'agent' ? agents.find((a) => a.name === step.assignee) : undefined;
  const label =
    step.assignee_kind === 'user'
      ? intl.formatMessage({ id: 'plans.step.assignee.user' })
      : agent?.display_name || step.assignee || intl.formatMessage({ id: 'plans.step.assignee.unassigned' });

  return (
    <div ref={ref} className="relative shrink-0">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        aria-haspopup="listbox"
        aria-expanded={open}
        title={intl.formatMessage({ id: 'plans.step.assignee.pick' })}
        className="flex items-center gap-1.5 rounded-full border border-[var(--panel-border)] px-2 py-0.5 text-xs text-stone-600 hover:border-amber-500/50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-500/50 dark:text-stone-300"
      >
        {step.assignee_kind === 'user' ? (
          <User className="h-3.5 w-3.5 text-stone-400" aria-hidden />
        ) : (
          <CharacterAvatar agentId={step.assignee || 'unassigned'} name={label} size={16} animated={false} />
        )}
        <span className="max-w-24 truncate">{label}</span>
      </button>
      {open && (
        <ul
          role="listbox"
          aria-label={intl.formatMessage({ id: 'plans.step.assignee.pick' })}
          className="glass-overlay absolute right-0 top-full z-50 mt-1 max-h-64 min-w-44 overflow-y-auto rounded-control p-1"
        >
          <li>
            <button
              type="button"
              role="option"
              aria-selected={step.assignee_kind === 'user'}
              onClick={() => {
                onPick('user', currentUser?.id ?? '');
                setOpen(false);
              }}
              className="flex w-full items-center gap-2 rounded-[calc(var(--radius-control)-2px)] px-2 py-1.5 text-left text-sm text-stone-700 hover:bg-stone-500/10 dark:text-stone-200 dark:hover:bg-white/10"
            >
              <User className="h-4 w-4 text-stone-400" aria-hidden />
              {intl.formatMessage({ id: 'plans.step.assignee.user' })}
            </button>
          </li>
          {agents.map((a) => (
            <li key={a.name}>
              <button
                type="button"
                role="option"
                aria-selected={step.assignee_kind === 'agent' && step.assignee === a.name}
                onClick={() => {
                  onPick('agent', a.name);
                  setOpen(false);
                }}
                className="flex w-full items-center gap-2 rounded-[calc(var(--radius-control)-2px)] px-2 py-1.5 text-left text-sm text-stone-700 hover:bg-stone-500/10 dark:text-stone-200 dark:hover:bg-white/10"
              >
                <CharacterAvatar agentId={a.name} name={a.display_name} size={18} animated={false} />
                <span className="truncate">{a.display_name}</span>
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

/** `/plans` — the shared user ↔ AI-employee co-edited plan (U4, Cocoa). */
export function PlansPage() {
  const intl = useIntl();
  const { plans, steps, loading, fetchPlans, fetchPlan, createPlan, updatePlan, removePlan, addStep, updateStep, removeStep } =
    usePlansStore();
  const { agents, fetchAgents } = useAgentsStore();

  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [createOpen, setCreateOpen] = useState(false);
  const [confirmRemove, setConfirmRemove] = useState(false);
  const [newStepText, setNewStepText] = useState('');

  // Initial load + 30s live-ish refresh (the `plan.updated` broadcast in the
  // store covers same-gateway edits instantly; polling covers the rest).
  useEffect(() => {
    fetchPlans();
    fetchAgents();
  }, [fetchPlans, fetchAgents]);
  useEffect(() => {
    const t = window.setInterval(() => {
      void fetchPlans();
      const id = usePlansStore.getState().plans.find((p) => p.id === selectedId)?.id;
      if (id) void fetchPlan(id);
    }, 30_000);
    return () => window.clearInterval(t);
  }, [fetchPlans, fetchPlan, selectedId]);

  // Keep a valid selection: default to the newest plan.
  useEffect(() => {
    if (plans.length === 0) {
      setSelectedId(null);
      return;
    }
    if (!selectedId || !plans.some((p) => p.id === selectedId)) {
      setSelectedId(plans[0].id);
    }
  }, [plans, selectedId]);
  useEffect(() => {
    if (selectedId) void fetchPlan(selectedId);
  }, [selectedId, fetchPlan]);

  const plan: PlanInfo | undefined = useMemo(
    () => plans.find((p) => p.id === selectedId),
    [plans, selectedId],
  );
  const planSteps = useMemo(() => (selectedId ? steps[selectedId] ?? [] : []), [steps, selectedId]);
  const progress = planProgress(planSteps);
  const planAgent = agents.find((a) => a.name === plan?.agent_id);

  const handleAddStep = useCallback(async () => {
    const text = newStepText.trim();
    if (!plan || !text) return;
    setNewStepText('');
    await addStep(plan.id, { text });
  }, [plan, newStepText, addStep]);

  const move = useCallback(
    (step: PlanStep, index: number, dir: 'up' | 'down') => {
      if (!plan) return;
      const target = stepMoveTarget(index, dir, planSteps.length);
      if (target === null) return;
      void updateStep(plan.id, step.id, { position: target });
    },
    [plan, planSteps.length, updateStep],
  );

  return (
    <Page>
      <PageHeader
        title={intl.formatMessage({ id: 'plans.title' })}
        subtitle={intl.formatMessage({ id: 'plans.subtitle' })}
        actions={
          <Button variant="primary" icon={Plus} onClick={() => setCreateOpen(true)}>
            {intl.formatMessage({ id: 'plans.new' })}
          </Button>
        }
      />

      {plans.length === 0 ? (
        <Card padded={false}>
          <EmptyState
            dudu="idle"
            icon={ListChecks}
            title={intl.formatMessage({ id: loading ? 'common.loading' : 'plans.empty.title' })}
            hint={loading ? undefined : intl.formatMessage({ id: 'plans.empty.hint' })}
            action={
              !loading && (
                <Button variant="primary" icon={Plus} onClick={() => setCreateOpen(true)}>
                  {intl.formatMessage({ id: 'plans.new' })}
                </Button>
              )
            }
          />
        </Card>
      ) : (
        <div className="grid gap-6 lg:grid-cols-[280px_minmax(0,1fr)]">
          {/* ── Plan list ── */}
          <nav aria-label={intl.formatMessage({ id: 'plans.title' })} className="space-y-2">
            {plans.map((p) => {
              const a = agents.find((ag) => ag.name === p.agent_id);
              const active = p.id === selectedId;
              return (
                <button
                  key={p.id}
                  type="button"
                  onClick={() => setSelectedId(p.id)}
                  aria-current={active ? 'true' : undefined}
                  className={`panel panel-hover w-full px-3 py-2.5 text-left focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-500/50 ${
                    active ? 'ring-2 ring-amber-500/40' : ''
                  }`}
                >
                  <div className="flex items-center gap-2">
                    <span className="min-w-0 flex-1 truncate text-sm font-medium text-stone-800 dark:text-stone-100">
                      {p.title}
                    </span>
                    {p.status !== 'active' && (
                      <Badge tone="neutral">{intl.formatMessage({ id: `plans.status.${p.status}` })}</Badge>
                    )}
                  </div>
                  <div className="mt-1 flex items-center gap-2 text-xs text-stone-500 dark:text-stone-400">
                    {a && <CharacterAvatar agentId={a.name} name={a.display_name} size={16} animated={false} />}
                    <span className="truncate">{a?.display_name ?? p.agent_id}</span>
                    <Mono className="ml-auto text-[0.6875rem] tabular-nums">
                      {p.steps_done ?? 0}/{p.steps_total ?? 0}
                    </Mono>
                  </div>
                </button>
              );
            })}
          </nav>

          {/* ── Selected plan ── */}
          {plan ? (
            <div className="min-w-0 space-y-4">
              <div className="flex flex-wrap items-center gap-2">
                {planAgent && (
                  <span className="flex items-center gap-1.5" title={planAgent.display_name}>
                    <CharacterAvatar agentId={planAgent.name} name={planAgent.display_name} size={26} animated={false} />
                    <span className="text-xs text-stone-500 dark:text-stone-400">{planAgent.display_name}</span>
                  </span>
                )}
                <Mono className="text-[0.6875rem] tabular-nums">
                  {intl.formatMessage({ id: 'plans.progress' }, { done: progress.settled, total: progress.total })}
                </Mono>
                <div className="ml-auto flex items-center gap-1">
                  <Button
                    variant="ghost"
                    size="sm"
                    onClick={() => updatePlan(plan.id, { status: plan.status === 'active' ? 'archived' : 'active' })}
                  >
                    {intl.formatMessage({ id: plan.status === 'active' ? 'plans.archive' : 'plans.unarchive' })}
                  </Button>
                  <Button
                    variant="ghost"
                    size="sm"
                    icon={Trash2}
                    title={intl.formatMessage({ id: 'plans.remove' })}
                    onClick={() => setConfirmRemove(true)}
                  />
                </div>
              </div>

              {/* progress bar */}
              <div
                role="progressbar"
                aria-valuenow={progress.pct}
                aria-valuemin={0}
                aria-valuemax={100}
                className="h-1.5 w-full overflow-hidden rounded-full bg-stone-500/10 dark:bg-white/10"
              >
                <div className="h-full rounded-full bg-emerald-500/80 transition-all" style={{ width: `${progress.pct}%` }} />
              </div>

              <InlineEditor
                value={plan.title}
                onCommit={(next) => updatePlan(plan.id, { title: next })}
                ariaLabel={intl.formatMessage({ id: 'plans.field.title' })}
                textClassName="text-2xl font-semibold tracking-tight text-stone-900 dark:text-stone-50"
              />
              <InlineEditor
                value={plan.description}
                onCommit={(next) => updatePlan(plan.id, { description: next })}
                multiline
                placeholder={intl.formatMessage({ id: 'plans.noDescription' })}
                ariaLabel={intl.formatMessage({ id: 'plans.field.description' })}
                textClassName="whitespace-pre-wrap text-sm text-stone-700 dark:text-stone-300"
              />

              {/* ── Steps ── */}
              <Card padded={false}>
                {planSteps.length === 0 ? (
                  <EmptyState
                    icon={ListChecks}
                    title={intl.formatMessage({ id: 'plans.steps.empty' })}
                    className="py-8"
                  />
                ) : (
                  <ul className="divide-y divide-stone-200/60 dark:divide-white/5">
                    {planSteps.map((s, i) => {
                      const Icon = STATUS_ICON[s.status];
                      return (
                        <li key={s.id} className="group flex items-center gap-2 px-3 py-2">
                          <button
                            type="button"
                            onClick={() => updateStep(plan.id, s.id, { status: cycleStepStatus(s.status) })}
                            title={intl.formatMessage({ id: `plans.step.status.${s.status}` })}
                            aria-label={`${intl.formatMessage({ id: `plans.step.status.${s.status}` })} — ${intl.formatMessage({ id: 'plans.step.cycle' })}`}
                            className={`shrink-0 rounded-full p-0.5 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-500/50 ${STATUS_CLASS[s.status]}`}
                          >
                            <Icon className="h-4.5 w-4.5" aria-hidden />
                          </button>
                          <div className="min-w-0 flex-1">
                            <InlineEditor
                              value={s.text}
                              onCommit={(next) => updateStep(plan.id, s.id, { text: next })}
                              ariaLabel={intl.formatMessage({ id: 'plans.step.text' })}
                              textClassName={`text-sm ${
                                s.status === 'done' || s.status === 'skipped'
                                  ? 'text-stone-400 line-through dark:text-stone-500'
                                  : 'text-stone-700 dark:text-stone-200'
                              }`}
                            />
                          </div>
                          <AssigneePicker
                            step={s}
                            onPick={(kind, assignee) => updateStep(plan.id, s.id, { assignee_kind: kind, assignee })}
                          />
                          <div className="flex shrink-0 items-center gap-0.5">
                            <Button
                              variant="ghost"
                              size="sm"
                              icon={ChevronUp}
                              disabled={i === 0}
                              title={intl.formatMessage({ id: 'plans.step.moveUp' })}
                              onClick={() => move(s, i, 'up')}
                            />
                            <Button
                              variant="ghost"
                              size="sm"
                              icon={ChevronDown}
                              disabled={i === planSteps.length - 1}
                              title={intl.formatMessage({ id: 'plans.step.moveDown' })}
                              onClick={() => move(s, i, 'down')}
                            />
                            <Button
                              variant="ghost"
                              size="sm"
                              icon={s.status === 'skipped' ? Undo2 : SkipForward}
                              title={intl.formatMessage({ id: s.status === 'skipped' ? 'plans.step.unskip' : 'plans.step.skip' })}
                              onClick={() =>
                                updateStep(plan.id, s.id, { status: s.status === 'skipped' ? 'todo' : 'skipped' })
                              }
                            />
                            <Button
                              variant="ghost"
                              size="sm"
                              icon={Trash2}
                              title={intl.formatMessage({ id: 'plans.step.remove' })}
                              onClick={() => removeStep(plan.id, s.id)}
                            />
                          </div>
                        </li>
                      );
                    })}
                  </ul>
                )}
                {/* add step */}
                <form
                  className="flex items-center gap-2 border-t border-stone-200/60 px-3 py-2 dark:border-white/5"
                  onSubmit={(e) => {
                    e.preventDefault();
                    void handleAddStep();
                  }}
                >
                  <Plus className="h-4 w-4 shrink-0 text-stone-400" aria-hidden />
                  <input
                    value={newStepText}
                    onChange={(e) => setNewStepText(e.target.value)}
                    placeholder={intl.formatMessage({ id: 'plans.step.addPlaceholder' })}
                    aria-label={intl.formatMessage({ id: 'plans.step.add' })}
                    className="h-8 min-w-0 flex-1 bg-transparent text-sm text-stone-800 placeholder:text-stone-400 focus-visible:outline-none dark:text-stone-100"
                  />
                  <Button variant="secondary" size="sm" type="submit" disabled={!newStepText.trim()}>
                    {intl.formatMessage({ id: 'plans.step.add' })}
                  </Button>
                </form>
              </Card>

              <p className="px-1.5 text-xs text-stone-400 dark:text-stone-500">
                {intl.formatMessage({ id: 'plans.updatedAgo' })}{' '}
                <Mono className="text-[0.6875rem]">{timeAgo(plan.updated_at)}</Mono>
              </p>
            </div>
          ) : (
            <Card padded={false}>
              <EmptyState icon={ListChecks} title={intl.formatMessage({ id: 'plans.detail.select' })} />
            </Card>
          )}
        </div>
      )}

      <CreatePlanDialog
        open={createOpen}
        onClose={() => setCreateOpen(false)}
        onCreate={async (title, agentId) => {
          const created = await createPlan({ title, agent_id: agentId });
          if (created) setSelectedId(created.id);
          setCreateOpen(false);
        }}
      />

      {plan && (
        <Dialog
          open={confirmRemove}
          title={intl.formatMessage({ id: 'plans.remove' })}
          onClose={() => setConfirmRemove(false)}
        >
          <div className="space-y-4">
            <p className="text-sm text-stone-600 dark:text-stone-400">
              {intl.formatMessage({ id: 'plans.remove.confirm' }, { title: plan.title })}
            </p>
            <div className="flex justify-end gap-3">
              <Button variant="secondary" onClick={() => setConfirmRemove(false)}>
                {intl.formatMessage({ id: 'plans.cancel' })}
              </Button>
              <Button
                variant="danger"
                onClick={async () => {
                  await removePlan(plan.id);
                  setConfirmRemove(false);
                }}
              >
                {intl.formatMessage({ id: 'plans.remove' })}
              </Button>
            </div>
          </div>
        </Dialog>
      )}
    </Page>
  );
}

function CreatePlanDialog({
  open,
  onClose,
  onCreate,
}: {
  open: boolean;
  onClose: () => void;
  onCreate: (title: string, agentId: string) => Promise<void>;
}) {
  const intl = useIntl();
  const { agents } = useAgentsStore();
  const [title, setTitle] = useState('');
  const [agentId, setAgentId] = useState('');
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (open) {
      setTitle('');
      setAgentId(agents[0]?.name ?? '');
    }
  }, [open, agents]);

  return (
    <Dialog open={open} title={intl.formatMessage({ id: 'plans.create.title' })} onClose={onClose}>
      <form
        className="space-y-4"
        onSubmit={async (e) => {
          e.preventDefault();
          if (!title.trim() || !agentId || busy) return;
          setBusy(true);
          try {
            await onCreate(title.trim(), agentId);
          } finally {
            setBusy(false);
          }
        }}
      >
        <Field label={intl.formatMessage({ id: 'plans.create.name' })} required>
          <input
            value={title}
            onChange={(e) => setTitle(e.target.value)}
            placeholder={intl.formatMessage({ id: 'plans.create.namePlaceholder' })}
            className={controlClass}
            /* eslint-disable-next-line jsx-a11y/no-autofocus */
            autoFocus
          />
        </Field>
        <Field label={intl.formatMessage({ id: 'plans.create.agent' })} required>
          <select value={agentId} onChange={(e) => setAgentId(e.target.value)} className={controlClass}>
            {agents.map((a) => (
              <option key={a.name} value={a.name}>
                {a.display_name}
              </option>
            ))}
          </select>
        </Field>
        <div className="flex justify-end gap-3">
          <Button variant="secondary" type="button" onClick={onClose}>
            {intl.formatMessage({ id: 'plans.cancel' })}
          </Button>
          <Button variant="primary" type="submit" disabled={!title.trim() || !agentId} pending={busy}>
            {intl.formatMessage({ id: 'plans.create.submit' })}
          </Button>
        </div>
      </form>
    </Dialog>
  );
}
