import { useCallback, useEffect, useMemo, useState } from 'react';
import { useIntl } from 'react-intl';
import { usePlansStore } from '@/stores/plans-store';
import { useAgentsStore } from '@/stores/agents-store';
import { useAuthStore } from '@/stores/auth-store';
import { cn } from '@/lib/utils';
import {
  CollectionPageHeader,
  CollectionPageState,
  Card,
  Button,
  Badge,
  Empty,
  Input,
  ActorAvatar,
  ListGridContainer,
  ListGridHeader,
  ListGridHeaderCell,
  ListGridRow,
  ListGridCell,
  DropdownMenu,
  DropdownMenuTrigger,
  DropdownMenuContent,
  DropdownMenuItem,
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogFooter,
  DialogClose,
  Select,
  SelectTrigger,
  SelectValue,
  SelectContent,
  SelectItem,
} from '@/components/mds';
import { InlineEditor } from '@/components/ui';
import { cycleStepStatus, planProgress, stepMoveTarget } from '@/lib/plan-utils';
import { timeAgo } from '@/lib/format';
import type { PlanInfo, PlanStep, PlanStepStatus } from '@/lib/api';
import {
  ClipboardList,
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
  MoreHorizontal,
  Archive,
  ArchiveRestore,
} from 'lucide-react';

const STATUS_ICON: Record<PlanStepStatus, typeof Circle> = {
  todo: Circle,
  doing: CircleDotDashed,
  done: CheckCircle2,
  skipped: MinusCircle,
};

const STATUS_CLASS: Record<PlanStepStatus, string> = {
  todo: 'text-muted-foreground',
  doing: 'text-brand',
  done: 'text-success',
  skipped: 'text-muted-foreground/50',
};

const PLAN_COLUMNS = 'minmax(0,1fr) 8rem auto 5rem 2.5rem';

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

  const agent = step.assignee_kind === 'agent' ? agents.find((a) => a.name === step.assignee) : undefined;
  const label =
    step.assignee_kind === 'user'
      ? intl.formatMessage({ id: 'plans.step.assignee.user' })
      : agent?.display_name || step.assignee || intl.formatMessage({ id: 'plans.step.assignee.unassigned' });

  return (
    <DropdownMenu>
      <DropdownMenuTrigger
        render={
          <button
            type="button"
            data-stop-row-nav
            title={intl.formatMessage({ id: 'plans.step.assignee.pick' })}
            className="flex shrink-0 items-center gap-1.5 rounded-4xl border border-border px-2 py-0.5 text-xs text-muted-foreground outline-none transition-colors hover:bg-surface-hover focus-visible:ring-3 focus-visible:ring-ring/50"
          />
        }
      >
        {step.assignee_kind === 'user' ? (
          <User className="size-3.5 text-muted-foreground" aria-hidden />
        ) : (
          <ActorAvatar actorType="agent" size="xs" name={label} />
        )}
        <span className="max-w-24 truncate">{label}</span>
      </DropdownMenuTrigger>
      <DropdownMenuContent className="max-h-64 min-w-44 overflow-y-auto">
        <DropdownMenuItem onClick={() => onPick('user', currentUser?.id ?? '')}>
          <User className="size-4 text-muted-foreground" aria-hidden />
          {intl.formatMessage({ id: 'plans.step.assignee.user' })}
        </DropdownMenuItem>
        {agents.map((a) => (
          <DropdownMenuItem key={a.name} onClick={() => onPick('agent', a.name)}>
            <ActorAvatar actorType="agent" size="xs" name={a.display_name} />
            <span className="truncate">{a.display_name}</span>
          </DropdownMenuItem>
        ))}
      </DropdownMenuContent>
    </DropdownMenu>
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
  const [removeTarget, setRemoveTarget] = useState<PlanInfo | null>(null);
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
    <div className="-mx-4 -mt-4 flex flex-col md:-mx-6 md:-mt-6">
      <CollectionPageHeader
        hideTrigger
        icon={ClipboardList}
        title={intl.formatMessage({ id: 'plans.title' })}
        count={plans.length}
        description={intl.formatMessage({ id: 'plans.subtitle' })}
        action={
          <Button variant="brand" size="sm" onClick={() => setCreateOpen(true)}>
            <Plus />
            <span className="hidden sm:inline">{intl.formatMessage({ id: 'plans.new' })}</span>
          </Button>
        }
      />

      <div className="flex flex-1 flex-col gap-6 p-4 md:p-6">
        {loading && plans.length === 0 ? (
          <CollectionPageState state="loading" />
        ) : plans.length === 0 ? (
          <CollectionPageState
            state="empty"
            icon={ClipboardList}
            title={intl.formatMessage({ id: 'plans.empty.title' })}
            description={intl.formatMessage({ id: 'plans.empty.hint' })}
            action={
              <Button variant="brand" size="sm" onClick={() => setCreateOpen(true)}>
                <Plus />
                {intl.formatMessage({ id: 'plans.new' })}
              </Button>
            }
          />
        ) : (
          <>
            {/* ── Plan list ── */}
            <div className="overflow-hidden rounded-xl border border-surface-border">
              <ListGridContainer
                columns={PLAN_COLUMNS}
                className="!h-auto"
                header={
                  <ListGridHeader>
                    <ListGridHeaderCell>{intl.formatMessage({ id: 'plans.col.name' })}</ListGridHeaderCell>
                    <ListGridHeaderCell>{intl.formatMessage({ id: 'plans.col.progress' })}</ListGridHeaderCell>
                    <ListGridHeaderCell hideBelow>{intl.formatMessage({ id: 'plans.col.people' })}</ListGridHeaderCell>
                    <ListGridHeaderCell hideBelow>{intl.formatMessage({ id: 'plans.col.updated' })}</ListGridHeaderCell>
                    <ListGridHeaderCell aria-hidden />
                  </ListGridHeader>
                }
              >
                {plans.map((p) => {
                  const a = agents.find((ag) => ag.name === p.agent_id);
                  const total = p.steps_total ?? 0;
                  const done = p.steps_done ?? 0;
                  const pct = total > 0 ? Math.round((done / total) * 100) : 0;
                  const active = p.id === selectedId;
                  return (
                    <ListGridRow key={p.id} selected={active} onClick={() => setSelectedId(p.id)}>
                      <ListGridCell className="gap-2">
                        <button
                          type="button"
                          onClick={() => setSelectedId(p.id)}
                          className="min-w-0 truncate text-left text-sm font-medium text-foreground outline-none hover:underline focus-visible:underline"
                          title={p.title}
                        >
                          {p.title}
                        </button>
                        {p.status !== 'active' && (
                          <Badge variant="secondary">
                            {intl.formatMessage({ id: `plans.status.${p.status}` })}
                          </Badge>
                        )}
                      </ListGridCell>
                      <ListGridCell className="gap-2">
                        <span className="shrink-0 font-mono text-xs tabular-nums text-muted-foreground">
                          {done}/{total}
                        </span>
                        <span className="h-1.5 w-14 shrink-0 overflow-hidden rounded-full bg-muted">
                          <span className="block h-full rounded-full bg-chart-1" style={{ width: `${pct}%` }} />
                        </span>
                      </ListGridCell>
                      <ListGridCell hideBelow>
                        <span className="flex -space-x-1.5">
                          <ActorAvatar
                            actorType="agent"
                            size="sm"
                            name={a?.display_name ?? p.agent_id}
                            className="ring-2 ring-page-canvas"
                          />
                          <ActorAvatar actorType="user" size="sm" className="ring-2 ring-page-canvas" />
                        </span>
                      </ListGridCell>
                      <ListGridCell hideBelow>
                        <span className="font-mono text-xs tabular-nums text-muted-foreground">
                          {timeAgo(p.updated_at)}
                        </span>
                      </ListGridCell>
                      <ListGridCell className="justify-end">
                        <DropdownMenu>
                          <DropdownMenuTrigger
                            render={
                              <Button
                                variant="ghost"
                                size="icon-sm"
                                data-stop-row-nav
                                onClick={(e) => e.stopPropagation()}
                                aria-label={intl.formatMessage({ id: 'plans.row.actions' })}
                              />
                            }
                          >
                            <MoreHorizontal />
                          </DropdownMenuTrigger>
                          <DropdownMenuContent>
                            <DropdownMenuItem
                              onClick={() =>
                                updatePlan(p.id, { status: p.status === 'active' ? 'archived' : 'active' })
                              }
                            >
                              {p.status === 'active' ? <Archive /> : <ArchiveRestore />}
                              {intl.formatMessage({ id: p.status === 'active' ? 'plans.archive' : 'plans.unarchive' })}
                            </DropdownMenuItem>
                            <DropdownMenuItem variant="destructive" onClick={() => setRemoveTarget(p)}>
                              <Trash2 />
                              {intl.formatMessage({ id: 'plans.remove' })}
                            </DropdownMenuItem>
                          </DropdownMenuContent>
                        </DropdownMenu>
                      </ListGridCell>
                    </ListGridRow>
                  );
                })}
              </ListGridContainer>
            </div>

            {/* ── Selected plan (expands in-page) ── */}
            {plan && (
              <div className="min-w-0 space-y-4">
                <div className="flex flex-wrap items-center gap-2">
                  {planAgent && (
                    <span className="flex items-center gap-1.5" title={planAgent.display_name}>
                      <ActorAvatar actorType="agent" size="lg" name={planAgent.display_name} />
                      <span className="text-xs text-muted-foreground">{planAgent.display_name}</span>
                    </span>
                  )}
                  <span className="font-mono text-xs tabular-nums text-muted-foreground">
                    {intl.formatMessage({ id: 'plans.progress' }, { done: progress.settled, total: progress.total })}
                  </span>
                </div>

                {/* progress bar */}
                <div
                  role="progressbar"
                  aria-valuenow={progress.pct}
                  aria-valuemin={0}
                  aria-valuemax={100}
                  className="h-1.5 w-full overflow-hidden rounded-full bg-muted"
                >
                  <div className="h-full rounded-full bg-chart-1 transition-all" style={{ width: `${progress.pct}%` }} />
                </div>

                <InlineEditor
                  value={plan.title}
                  onCommit={(next) => updatePlan(plan.id, { title: next })}
                  ariaLabel={intl.formatMessage({ id: 'plans.field.title' })}
                  textClassName="text-xl font-semibold text-foreground sm:text-2xl"
                />
                <InlineEditor
                  value={plan.description}
                  onCommit={(next) => updatePlan(plan.id, { description: next })}
                  multiline
                  placeholder={intl.formatMessage({ id: 'plans.noDescription' })}
                  ariaLabel={intl.formatMessage({ id: 'plans.field.description' })}
                  textClassName="whitespace-pre-wrap text-sm text-muted-foreground"
                />

                {/* ── Steps ── */}
                <Card className="gap-0 py-0">
                  {planSteps.length === 0 ? (
                    <Empty
                      icon={ClipboardList}
                      title={intl.formatMessage({ id: 'plans.steps.empty' })}
                      className="py-8"
                    />
                  ) : (
                    <ul className="divide-y divide-surface-border">
                      {planSteps.map((s, i) => {
                        const Icon = STATUS_ICON[s.status];
                        return (
                          <li key={s.id} className="group flex items-center gap-2 px-4 py-2">
                            <button
                              type="button"
                              onClick={() => updateStep(plan.id, s.id, { status: cycleStepStatus(s.status) })}
                              title={intl.formatMessage({ id: `plans.step.status.${s.status}` })}
                              aria-label={`${intl.formatMessage({ id: `plans.step.status.${s.status}` })} — ${intl.formatMessage({ id: 'plans.step.cycle' })}`}
                              className={cn(
                                'shrink-0 rounded-full p-0.5 outline-none focus-visible:ring-3 focus-visible:ring-ring/50',
                                STATUS_CLASS[s.status],
                              )}
                            >
                              <Icon className="size-[18px]" aria-hidden />
                            </button>
                            <div className="min-w-0 flex-1">
                              <InlineEditor
                                value={s.text}
                                onCommit={(next) => updateStep(plan.id, s.id, { text: next })}
                                ariaLabel={intl.formatMessage({ id: 'plans.step.text' })}
                                textClassName={cn(
                                  'text-sm',
                                  s.status === 'done' || s.status === 'skipped'
                                    ? 'text-muted-foreground line-through'
                                    : 'text-foreground',
                                )}
                              />
                            </div>
                            <AssigneePicker
                              step={s}
                              onPick={(kind, assignee) => updateStep(plan.id, s.id, { assignee_kind: kind, assignee })}
                            />
                            <div className="flex shrink-0 items-center gap-0.5 opacity-0 transition-opacity group-hover:opacity-100 focus-within:opacity-100 pointer-coarse:opacity-100">
                              <Button
                                variant="ghost"
                                size="icon-xs"
                                disabled={i === 0}
                                title={intl.formatMessage({ id: 'plans.step.moveUp' })}
                                aria-label={intl.formatMessage({ id: 'plans.step.moveUp' })}
                                onClick={() => move(s, i, 'up')}
                              >
                                <ChevronUp />
                              </Button>
                              <Button
                                variant="ghost"
                                size="icon-xs"
                                disabled={i === planSteps.length - 1}
                                title={intl.formatMessage({ id: 'plans.step.moveDown' })}
                                aria-label={intl.formatMessage({ id: 'plans.step.moveDown' })}
                                onClick={() => move(s, i, 'down')}
                              >
                                <ChevronDown />
                              </Button>
                              <Button
                                variant="ghost"
                                size="icon-xs"
                                title={intl.formatMessage({ id: s.status === 'skipped' ? 'plans.step.unskip' : 'plans.step.skip' })}
                                aria-label={intl.formatMessage({ id: s.status === 'skipped' ? 'plans.step.unskip' : 'plans.step.skip' })}
                                onClick={() =>
                                  updateStep(plan.id, s.id, { status: s.status === 'skipped' ? 'todo' : 'skipped' })
                                }
                              >
                                {s.status === 'skipped' ? <Undo2 /> : <SkipForward />}
                              </Button>
                              <Button
                                variant="ghost"
                                size="icon-xs"
                                title={intl.formatMessage({ id: 'plans.step.remove' })}
                                aria-label={intl.formatMessage({ id: 'plans.step.remove' })}
                                onClick={() => removeStep(plan.id, s.id)}
                              >
                                <Trash2 />
                              </Button>
                            </div>
                          </li>
                        );
                      })}
                    </ul>
                  )}
                  {/* add step */}
                  <form
                    className="flex items-center gap-2 border-t border-surface-border px-4 py-2"
                    onSubmit={(e) => {
                      e.preventDefault();
                      void handleAddStep();
                    }}
                  >
                    <Plus className="size-4 shrink-0 text-muted-foreground" aria-hidden />
                    <input
                      value={newStepText}
                      onChange={(e) => setNewStepText(e.target.value)}
                      placeholder={intl.formatMessage({ id: 'plans.step.addPlaceholder' })}
                      aria-label={intl.formatMessage({ id: 'plans.step.add' })}
                      className="h-8 min-w-0 flex-1 bg-transparent text-sm text-foreground outline-none placeholder:text-muted-foreground"
                    />
                    <Button variant="secondary" size="sm" type="submit" disabled={!newStepText.trim()}>
                      {intl.formatMessage({ id: 'plans.step.add' })}
                    </Button>
                  </form>
                </Card>

                <p className="text-xs text-muted-foreground">
                  {intl.formatMessage({ id: 'plans.updatedAgo' })}{' '}
                  <span className="font-mono tabular-nums">{timeAgo(plan.updated_at)}</span>
                </p>
              </div>
            )}
          </>
        )}
      </div>

      <CreatePlanDialog
        open={createOpen}
        onClose={() => setCreateOpen(false)}
        onCreate={async (title, agentId) => {
          const created = await createPlan({ title, agent_id: agentId });
          if (created) setSelectedId(created.id);
          setCreateOpen(false);
        }}
      />

      <Dialog open={removeTarget !== null} onOpenChange={(o) => !o && setRemoveTarget(null)}>
        <DialogContent className="sm:max-w-sm">
          <DialogHeader>
            <DialogTitle>{intl.formatMessage({ id: 'plans.remove' })}</DialogTitle>
            <DialogDescription>
              {removeTarget &&
                intl.formatMessage({ id: 'plans.remove.confirm' }, { title: removeTarget.title })}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <DialogClose
              render={<Button variant="outline">{intl.formatMessage({ id: 'plans.cancel' })}</Button>}
            />
            <Button
              variant="destructive"
              onClick={async () => {
                if (removeTarget) await removePlan(removeTarget.id);
                setRemoveTarget(null);
              }}
            >
              {intl.formatMessage({ id: 'plans.remove' })}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
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

  const current = agents.find((a) => a.name === agentId);

  return (
    <Dialog open={open} onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>{intl.formatMessage({ id: 'plans.create.title' })}</DialogTitle>
        </DialogHeader>
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
          <div className="space-y-1.5">
            <label className="text-xs font-medium text-muted-foreground">
              {intl.formatMessage({ id: 'plans.create.name' })}
            </label>
            <Input
              value={title}
              onChange={(e) => setTitle(e.target.value)}
              placeholder={intl.formatMessage({ id: 'plans.create.namePlaceholder' })}
              /* eslint-disable-next-line jsx-a11y/no-autofocus */
              autoFocus
            />
          </div>
          <div className="space-y-1.5">
            <label className="text-xs font-medium text-muted-foreground">
              {intl.formatMessage({ id: 'plans.create.agent' })}
            </label>
            <Select value={agentId} onValueChange={(v) => setAgentId(String(v))}>
              <SelectTrigger className="w-full">
                <SelectValue>{current?.display_name ?? ''}</SelectValue>
              </SelectTrigger>
              <SelectContent>
                {agents.map((a) => (
                  <SelectItem key={a.name} value={a.name}>
                    {a.display_name}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
          <DialogFooter>
            <Button variant="outline" type="button" onClick={onClose}>
              {intl.formatMessage({ id: 'plans.cancel' })}
            </Button>
            <Button variant="brand" type="submit" disabled={!title.trim() || !agentId || busy}>
              {intl.formatMessage({ id: 'plans.create.submit' })}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
