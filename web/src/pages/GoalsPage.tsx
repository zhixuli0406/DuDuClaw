import { useCallback, useEffect, useMemo, useState } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate, useSearchParams } from 'react-router';
import {
  Target,
  Plus,
  Radar,
  RefreshCw,
  Check,
  Trash2,
  Hand,
  CircleDot,
  UserCheck,
} from 'lucide-react';

import { api, type TaskInfo, type GoalTimeline } from '@/lib/api';
import { timeAgo } from '@/lib/format';
import { toast, formatError } from '@/lib/toast';
import { useAgentsStore } from '@/stores/agents-store';
import { useAssignStore } from '@/stores/assign-store';
import { useAuthStore } from '@/stores/auth-store';
import { useConnectionStore } from '@/stores/connection-store';
import {
  ActorAvatar,
  Badge,
  Button,
  Card,
  CardContent,
  CollectionPageState,
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  Input,
  Select,
  SelectTrigger,
  SelectValue,
  SelectContent,
  SelectItem,
  Spinner,
  useErrorMessage,
} from '@/components/mds';

/**
 * GoalsPage (`/goals`) — assign and manage autonomous goal-loop tasks.
 *
 * Everything the channel `/goal` command can do, from the dashboard: assign
 * a goal to an AI employee (with acceptance criteria), watch every judge
 * round of the loop, and intervene at the human nodes — pending kickoff
 * approvals (collaborator/consultant autonomy) and `needs_human`
 * escalations (retry / mark done / abandon / take over). Interventions go
 * through the same fail-closed path as the channel buttons
 * (`tasks.goal_decide`), never a bare status write.
 */

const ACTIVE_STATUSES = ['todo', 'pending', 'in_progress', 'review', 'revising'] as const;
const WAITING_STATUSES = ['needs_human'] as const;
const FINISHED_STATUSES = ['done', 'cancelled', 'failed', 'blocked'] as const;

function statusTone(status: string): string {
  switch (status) {
    case 'needs_human':
      return 'text-amber-600 dark:text-amber-400';
    case 'done':
      return 'text-emerald-600 dark:text-emerald-400';
    case 'cancelled':
    case 'failed':
      return 'text-rose-600 dark:text-rose-400';
    case 'review':
    case 'revising':
      return 'text-chart-1';
    default:
      return 'text-muted-foreground';
  }
}

/** Compact remaining-time label for a FUTURE timestamp ("36h" / "3d") —
 *  `timeAgo` is past-oriented and would render a future deadline as "now". */
function timeLeft(iso: string): string {
  const ms = new Date(iso).getTime() - Date.now();
  if (!Number.isFinite(ms) || ms <= 0) return '0h';
  const hours = Math.round(ms / 3_600_000);
  if (hours < 48) return `${Math.max(hours, 1)}h`;
  return `${Math.round(hours / 24)}d`;
}

function GoalStatusBadge({ status }: { status: string }) {
  const intl = useIntl();
  return (
    <span className={`text-xs font-medium ${statusTone(status)}`}>
      {intl.formatMessage({ id: `goals.status.${status}`, defaultMessage: status })}
    </span>
  );
}

/** One merged timeline node (round or activity event). */
type TimelineNode = {
  key: string;
  at: string;
  kind: 'round' | 'event';
  round?: GoalTimeline['iterations'][number];
  event?: GoalTimeline['activity'][number];
};

/** needs_human intervention buttons, shared by list cards and the detail. */
function InterventionButtons({
  task,
  compact,
  onDecided,
}: {
  task: TaskInfo;
  compact?: boolean;
  onDecided: () => void;
}) {
  const intl = useIntl();
  const [busy, setBusy] = useState<string | null>(null);
  const [noteFor, setNoteFor] = useState<'retry' | null>(null);
  const [note, setNote] = useState('');

  const decide = async (action: 'retry' | 'done' | 'abort' | 'takeover', withNote?: string) => {
    setBusy(action);
    try {
      const r = await api.tasks.goalDecide(task.id, action, withNote);
      toast.success(r.message);
      setNoteFor(null);
      setNote('');
      onDecided();
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
    } finally {
      setBusy(null);
    }
  };

  return (
    <div className="space-y-2">
      <div className="flex flex-wrap gap-1.5">
        <Button
          variant="outline"
          size="sm"
          disabled={busy !== null}
          onClick={() => setNoteFor(noteFor === 'retry' ? null : 'retry')}
        >
          <RefreshCw className="size-3.5" />
          {intl.formatMessage({ id: 'goals.decide.retry' })}
        </Button>
        <Button variant="outline" size="sm" disabled={busy !== null} onClick={() => decide('done')}>
          <Check className="size-3.5" />
          {intl.formatMessage({ id: 'goals.decide.done' })}
        </Button>
        <Button variant="outline" size="sm" disabled={busy !== null} onClick={() => decide('abort')}>
          <Trash2 className="size-3.5" />
          {intl.formatMessage({ id: 'goals.decide.abort' })}
        </Button>
        <Button variant="outline" size="sm" disabled={busy !== null} onClick={() => decide('takeover')}>
          <Hand className="size-3.5" />
          {intl.formatMessage({ id: 'goals.decide.takeover' })}
        </Button>
      </div>
      {noteFor === 'retry' && (
        <div className="flex gap-1.5">
          <Input
            value={note}
            placeholder={intl.formatMessage({ id: 'goals.decide.notePlaceholder' })}
            onChange={(e) => setNote(e.target.value)}
            className={compact ? 'h-8 text-xs' : ''}
          />
          <Button variant="brand" size="sm" disabled={busy !== null} onClick={() => decide('retry', note)}>
            {intl.formatMessage({ id: 'goals.decide.retryGo' })}
          </Button>
        </div>
      )}
    </div>
  );
}

/** Detail dialog: full loop timeline + human intervention nodes. */
function GoalDetailDialog({
  taskId,
  onClose,
  onChanged,
}: {
  taskId: string | null;
  onClose: () => void;
  onChanged: () => void;
}) {
  const intl = useIntl();
  const navigate = useNavigate();
  const [timeline, setTimeline] = useState<GoalTimeline | null>(null);
  const [kickoffBusy, setKickoffBusy] = useState(false);

  const reload = useCallback(() => {
    if (!taskId) return;
    api.tasks
      .timeline(taskId)
      .then(setTimeline)
      .catch((e) => {
        console.warn('[api]', e);
        toast.error(formatError(e));
      });
  }, [taskId]);

  useEffect(() => {
    setTimeline(null);
    reload();
  }, [reload]);

  const nodes: TimelineNode[] = useMemo(() => {
    if (!timeline) return [];
    const out: TimelineNode[] = [];
    for (const it of timeline.iterations) {
      out.push({ key: `r${it.round}`, at: it.dispatched_at, kind: 'round', round: it });
    }
    // Round rows already carry dispatch/submit/verdict; keep only the
    // activity events that add human/loop context beyond them.
    const interesting = /^goal_loop\.(created|kickoff_|needs_human|oscillation|human_decision|reassigned|dep_blocked|operator_skipped|observer_autoclose)/;
    for (const ev of timeline.activity) {
      if (interesting.test(ev.type)) {
        out.push({ key: `e${ev.id}`, at: ev.timestamp, kind: 'event', event: ev });
      }
    }
    return out.sort((a, b) => a.at.localeCompare(b.at));
  }, [timeline]);

  const decideKickoff = async (approve: boolean) => {
    if (!timeline?.pending_kickoff) return;
    setKickoffBusy(true);
    try {
      await api.approvals.decide(timeline.pending_kickoff.id, approve);
      toast.success(
        intl.formatMessage({ id: approve ? 'goals.kickoff.approved' : 'goals.kickoff.denied' }),
      );
      reload();
      onChanged();
    } catch (e) {
      toast.error(formatError(e));
    } finally {
      setKickoffBusy(false);
    }
  };

  const task = timeline?.task ?? null;
  return (
    <Dialog open={taskId !== null} onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle className="pr-6">
            {task ? task.title : intl.formatMessage({ id: 'goals.detail.title' })}
          </DialogTitle>
        </DialogHeader>
        {!timeline ? (
          <div className="flex justify-center py-10">
            <Spinner />
          </div>
        ) : (
          <div className="max-h-[65vh] space-y-4 overflow-y-auto pr-1">
            {/* Status strip */}
            <div className="flex flex-wrap items-center gap-2 text-xs">
              <GoalStatusBadge status={timeline.task.status} />
              <span className="flex items-center gap-1 text-muted-foreground">
                <ActorAvatar actorType="agent" size="xs" name={timeline.task.assigned_to} />
                {timeline.task.assigned_to}
              </span>
              <Badge variant="secondary">
                {intl.formatMessage(
                  { id: 'goals.roundBadge' },
                  { n: (timeline.task.revision_round ?? 0) + 1 },
                )}
              </Badge>
              {timeline.task.diminishing && (
                <Badge variant="secondary" className="text-amber-600 dark:text-amber-400">
                  {intl.formatMessage({ id: 'goals.diminishing' })}
                </Badge>
              )}
              {timeline.task.deadline_at && (
                <Badge
                  variant="secondary"
                  className={
                    new Date(timeline.task.deadline_at).getTime() < Date.now()
                      ? 'text-rose-600 dark:text-rose-400'
                      : 'text-muted-foreground'
                  }
                >
                  {new Date(timeline.task.deadline_at).getTime() < Date.now()
                    ? intl.formatMessage({ id: 'goals.detail.deadlinePast' })
                    : intl.formatMessage(
                        { id: 'goals.detail.deadline' },
                        { when: timeLeft(timeline.task.deadline_at) },
                      )}
                </Badge>
              )}
              <span className="ml-auto font-mono tabular-nums text-muted-foreground">
                {timeAgo(timeline.task.created_at)}
              </span>
            </div>

            {/* Goal contract v2: per-goal risk boundary (explicit only —
                baseline application is server-side and not echoed back). */}
            {timeline.task.risk_boundary && (
              <div className="rounded-lg bg-secondary px-3 py-2 text-xs">
                <p className="mb-0.5 font-medium text-foreground">
                  {intl.formatMessage({ id: 'goals.detail.riskBoundary' })}
                </p>
                <p className="whitespace-pre-wrap text-muted-foreground">
                  {timeline.task.risk_boundary}
                </p>
              </div>
            )}

            {/* Acceptance contract + latest result */}
            {timeline.task.acceptance_criteria && (
              <div className="rounded-lg bg-secondary px-3 py-2 text-xs">
                <p className="mb-0.5 font-medium text-foreground">
                  {intl.formatMessage({ id: 'goals.detail.criteria' })}
                </p>
                <p className="whitespace-pre-wrap text-muted-foreground">
                  {timeline.task.acceptance_criteria}
                </p>
              </div>
            )}
            {timeline.task.result_summary && (
              <div className="rounded-lg bg-secondary px-3 py-2 text-xs">
                <p className="mb-0.5 font-medium text-foreground">
                  {intl.formatMessage({ id: 'goals.detail.result' })}
                </p>
                <p className="whitespace-pre-wrap text-muted-foreground">{timeline.task.result_summary}</p>
              </div>
            )}
            {timeline.task.goal_state &&
              ((timeline.task.goal_state.confirmed_facts?.length ?? 0) > 0 ||
                (timeline.task.goal_state.pending_hypotheses?.length ?? 0) > 0) && (
                <div className="rounded-lg bg-secondary px-3 py-2 text-xs">
                  <p className="mb-0.5 font-medium text-foreground">
                    {intl.formatMessage({ id: 'goals.detail.state' })}
                  </p>
                  {(timeline.task.goal_state.confirmed_facts ?? []).map((f, i) => (
                    <p key={`f${i}`} className="text-muted-foreground">
                      ✓ {f}
                    </p>
                  ))}
                  {(timeline.task.goal_state.pending_hypotheses ?? []).map((h, i) => (
                    <p key={`h${i}`} className="text-muted-foreground/80">
                      ? {h}
                    </p>
                  ))}
                </div>
              )}

            {/* Pending kickoff approval — a human node the loop is blocked on. */}
            {timeline.pending_kickoff && (
              <div className="rounded-lg border border-amber-500/40 bg-amber-500/10 px-3 py-2">
                <p className="mb-1.5 flex items-center gap-1.5 text-xs font-medium text-amber-700 dark:text-amber-300">
                  <UserCheck className="size-3.5" />
                  {intl.formatMessage({ id: 'goals.kickoff.pending' })}
                </p>
                <p className="mb-2 text-xs text-muted-foreground">{timeline.pending_kickoff.summary}</p>
                <div className="flex gap-1.5">
                  <Button variant="brand" size="sm" disabled={kickoffBusy} onClick={() => decideKickoff(true)}>
                    {intl.formatMessage({ id: 'goals.kickoff.approve' })}
                  </Button>
                  <Button variant="outline" size="sm" disabled={kickoffBusy} onClick={() => decideKickoff(false)}>
                    {intl.formatMessage({ id: 'goals.kickoff.deny' })}
                  </Button>
                </div>
              </div>
            )}

            {/* needs_human intervention */}
            {timeline.task.status === 'needs_human' && (
              <div className="rounded-lg border border-amber-500/40 bg-amber-500/10 px-3 py-2">
                <p className="mb-1 text-xs font-medium text-amber-700 dark:text-amber-300">
                  {intl.formatMessage({ id: 'goals.needsHuman.title' })}
                </p>
                {timeline.task.judge_feedback && (
                  <p className="mb-2 whitespace-pre-wrap text-xs text-muted-foreground">
                    {timeline.task.judge_feedback}
                  </p>
                )}
                <InterventionButtons
                  task={timeline.task}
                  onDecided={() => {
                    reload();
                    onChanged();
                  }}
                />
              </div>
            )}

            {/* Loop timeline */}
            <div className="space-y-0">
              <h3 className="mb-2 text-sm font-medium text-foreground">
                {intl.formatMessage({ id: 'goals.detail.timeline' })}
              </h3>
              {nodes.length === 0 && (
                <p className="text-xs text-muted-foreground">
                  {intl.formatMessage({ id: 'goals.detail.timelineEmpty' })}
                </p>
              )}
              {nodes.map((n) => (
                <div key={n.key} className="relative border-l border-surface-border pb-3 pl-4 last:pb-0">
                  <CircleDot className="absolute -left-[7px] top-0.5 size-3.5 bg-background text-muted-foreground" />
                  {n.kind === 'round' && n.round ? (
                    <div className="space-y-1 text-xs">
                      <div className="flex flex-wrap items-center gap-2">
                        <span className="font-medium text-foreground">
                          {intl.formatMessage({ id: 'goals.roundBadge' }, { n: n.round.round })}
                        </span>
                        {n.round.verdict ? (
                          <span
                            className={
                              n.round.verdict === 'accepted'
                                ? 'text-emerald-600 dark:text-emerald-400'
                                : n.round.verdict === 'escalated'
                                  ? 'text-amber-600 dark:text-amber-400'
                                  : 'text-rose-600 dark:text-rose-400'
                            }
                          >
                            {intl.formatMessage({
                              id: `goals.verdict.${n.round.verdict}`,
                              defaultMessage: n.round.verdict,
                            })}
                          </span>
                        ) : n.round.submitted_at ? (
                          <span className="text-muted-foreground">
                            {intl.formatMessage({ id: 'goals.verdict.judging' })}
                          </span>
                        ) : (
                          <span className="text-muted-foreground">
                            {intl.formatMessage({ id: 'goals.verdict.working' })}
                          </span>
                        )}
                        {(n.round.dispatch_count ?? 1) > 1 && (
                          <span className="text-amber-600 dark:text-amber-400">
                            {intl.formatMessage(
                              { id: 'goals.redispatch' },
                              { n: n.round.dispatch_count },
                            )}
                          </span>
                        )}
                        {(n.round.repeat_streak ?? 0) >= 2 && (
                          <span className="text-amber-600 dark:text-amber-400">
                            {intl.formatMessage(
                              { id: 'goals.repeatState' },
                              { n: n.round.repeat_streak },
                            )}
                          </span>
                        )}
                        <span className="ml-auto font-mono tabular-nums text-muted-foreground">
                          {timeAgo(n.round.judged_at ?? n.round.submitted_at ?? n.round.dispatched_at)}
                        </span>
                      </div>
                      {/* Per-aspect MAV verdicts — the structured panel the
                          judge actually returned, not just the flattened
                          feedback string. */}
                      {n.round.aspects && n.round.aspects.length > 0 && (
                        <div className="flex flex-wrap gap-1.5">
                          {n.round.aspects.map((a) => (
                            <span
                              key={a.name}
                              title={a.reason}
                              className={`rounded-full px-2 py-0.5 ${
                                a.pass
                                  ? 'bg-emerald-500/10 text-emerald-600 dark:text-emerald-400'
                                  : 'bg-rose-500/10 text-rose-600 dark:text-rose-400'
                              }`}
                            >
                              {intl.formatMessage({
                                id: `goals.aspect.${a.name}`,
                                defaultMessage: a.name,
                              })}
                              {a.pass ? ' ✓' : ' ✗'}
                            </span>
                          ))}
                        </div>
                      )}
                      {n.round.judge_feedback && (
                        <p className="whitespace-pre-wrap text-muted-foreground">{n.round.judge_feedback}</p>
                      )}
                      {(() => {
                        const run = timeline.runs?.find((r) => r.round === n.round!.round);
                        return run ? (
                          <button
                            type="button"
                            onClick={() => navigate(`/runs?run=${encodeURIComponent(run.id)}`)}
                            className="text-chart-1 hover:underline"
                          >
                            {intl.formatMessage(
                              { id: 'goals.viewRun' },
                              { steps: run.step_count },
                            )}
                          </button>
                        ) : null;
                      })()}
                    </div>
                  ) : n.event ? (
                    <div className="flex flex-wrap items-center gap-2 text-xs">
                      <span className="text-muted-foreground">{n.event.summary}</span>
                      <span className="ml-auto font-mono tabular-nums text-muted-foreground/70">
                        {timeAgo(n.event.timestamp)}
                      </span>
                    </div>
                  ) : null}
                </div>
              ))}
            </div>

            <div className="flex justify-end">
              <Button variant="ghost" size="sm" onClick={() => navigate(`/foresight`)}>
                <Radar className="size-3.5" />
                {intl.formatMessage({ id: 'goals.detail.foresightLink' })}
              </Button>
            </div>
          </div>
        )}
      </DialogContent>
    </Dialog>
  );
}

function GoalCard({
  task,
  onOpen,
  onChanged,
}: {
  task: TaskInfo;
  onOpen: () => void;
  onChanged: () => void;
}) {
  const intl = useIntl();
  return (
    <Card data-size="sm" className="transition-colors hover:border-surface-border-strong">
      <CardContent className="space-y-2 py-3">
        <button type="button" onClick={onOpen} className="block w-full text-left">
          <div className="flex items-center gap-2">
            <ActorAvatar actorType="agent" size="xs" name={task.assigned_to} />
            <span className="min-w-0 flex-1 truncate text-sm font-medium text-foreground">{task.title}</span>
            <GoalStatusBadge status={task.status} />
          </div>
          <div className="mt-1 flex flex-wrap items-center gap-2 text-xs text-muted-foreground">
            <span>
              {intl.formatMessage({ id: 'goals.roundBadge' }, { n: (task.revision_round ?? 0) + 1 })}
            </span>
            {task.diminishing && (
              <span className="text-amber-600 dark:text-amber-400">
                {intl.formatMessage({ id: 'goals.diminishing' })}
              </span>
            )}
            {task.claimed_by?.startsWith('dashboard:') || task.claimed_by?.startsWith('channel:') ? (
              <span className="text-chart-1">{intl.formatMessage({ id: 'goals.takenOver' })}</span>
            ) : null}
            <span className="ml-auto font-mono tabular-nums">{timeAgo(task.updated_at)}</span>
          </div>
        </button>
        {task.status === 'needs_human' && (
          <div className="border-t border-surface-border pt-2">
            {task.judge_feedback && (
              <p className="mb-1.5 line-clamp-2 text-xs text-muted-foreground">{task.judge_feedback}</p>
            )}
            <InterventionButtons task={task} compact onDecided={onChanged} />
          </div>
        )}
      </CardContent>
    </Card>
  );
}

export function GoalsPage() {
  const intl = useIntl();
  const errorText = useErrorMessage();
  // UX plan I-1a: 交辦 opens the one shared panel. The seven-field
  // `CreateGoalDialog` that used to live here was the only form that actually
  // drove `tasks.goal_create` and nothing in the app linked to it; the
  // AssignSheet carries every one of its fields (the advanced four behind
  // 更多設定) and is reachable from every entry point.
  const openAssign = useAssignStore((s) => s.openAssign);
  const connState = useConnectionStore((s) => s.state);
  const { agents, fetchAgents, loaded: agentsLoaded } = useAgentsStore();
  const isAdmin = useAuthStore((s) => s.user?.role === 'admin');
  const [searchParams, setSearchParams] = useSearchParams();
  const [tasks, setTasks] = useState<TaskInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [agentFilter, setAgentFilter] = useState('');
  const detailTask = searchParams.get('task');

  // Non-admins must scope tasks.list to a bound agent (check_agent_filter) —
  // an unscoped first load would error out. `agents.list` is already
  // binding-filtered server-side, so its first entry is a valid default scope.
  useEffect(() => {
    if (!isAdmin && !agentFilter && agents.length > 0) {
      setAgentFilter(agents[0].name);
    }
  }, [isAdmin, agentFilter, agents]);

  const load = useCallback(async () => {
    if (!isAdmin && !agentFilter) {
      // Waiting for the default scope above; a non-admin with zero bound
      // agents will never get one — settle into the empty state instead of
      // an endless skeleton.
      if (agentsLoaded && agents.length === 0) setLoading(false);
      return;
    }
    try {
      const r = await api.tasks.list({
        goal_mode: true,
        ...(agentFilter ? { agent_id: agentFilter } : {}),
      });
      setTasks((r.tasks ?? []).filter((t) => t.goal_mode));
      setLoadError(null);
    } catch (e) {
      console.warn('[api]', e);
      setLoadError(errorText(e));
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: errorText(e) }));
    } finally {
      setLoading(false);
    }
    // errorText stable; intl from context.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [agentFilter, isAdmin, agentsLoaded, agents.length]);

  useEffect(() => {
    if (connState === 'authenticated') {
      fetchAgents();
      void load();
    }
  }, [connState, fetchAgents, load]);

  const openDetail = (id: string | null) => {
    const next = new URLSearchParams(searchParams);
    if (id) next.set('task', id);
    else next.delete('task');
    setSearchParams(next, { replace: true });
  };

  const waiting = tasks.filter((t) => (WAITING_STATUSES as readonly string[]).includes(t.status));
  const active = tasks.filter((t) => (ACTIVE_STATUSES as readonly string[]).includes(t.status));
  const finished = tasks
    .filter((t) => (FINISHED_STATUSES as readonly string[]).includes(t.status))
    .slice(0, 20);

  const agentLabel = agentFilter
    ? agents.find((a) => a.name === agentFilter)?.display_name || agentFilter
    : intl.formatMessage({ id: 'foresight.allAgents' });

  return (
    <div className="mx-auto w-full max-w-[1200px] space-y-5">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="flex min-w-0 items-center gap-2">
          <Target className="size-5 text-muted-foreground" />
          <div>
            <h1 className="text-base font-medium">{intl.formatMessage({ id: 'goals.title' })}</h1>
            <p className="text-sm text-muted-foreground">{intl.formatMessage({ id: 'goals.subtitle' })}</p>
          </div>
        </div>
        <div className="flex items-center gap-2">
          <Select value={agentFilter} onValueChange={(v) => setAgentFilter(String(v))}>
            <SelectTrigger className="w-44">
              <SelectValue>{agentLabel}</SelectValue>
            </SelectTrigger>
            <SelectContent>
              {/* Non-admins cannot query unscoped (check_agent_filter) — hide "all". */}
              {isAdmin && (
                <SelectItem value="">{intl.formatMessage({ id: 'foresight.allAgents' })}</SelectItem>
              )}
              {agents.map((a) => (
                <SelectItem key={a.name} value={a.name}>
                  {a.display_name || a.name}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
          <Button variant="brand" size="sm" onClick={() => openAssign()}>
            <Plus />
            <span className="hidden sm:inline">{intl.formatMessage({ id: 'goals.assign' })}</span>
          </Button>
        </div>
      </div>

      {loading ? (
        <CollectionPageState state="loading" />
      ) : loadError ? (
        <CollectionPageState
          state="error"
          icon={Target}
          title={intl.formatMessage({ id: 'goals.error.title' })}
          description={loadError}
          action={
            <Button variant="outline" size="sm" onClick={() => void load()}>
              {intl.formatMessage({ id: 'foresight.error.retry' })}
            </Button>
          }
        />
      ) : tasks.length === 0 ? (
        <CollectionPageState
          state="empty"
          icon={Target}
          title={intl.formatMessage({ id: 'goals.empty.title' })}
          description={intl.formatMessage({ id: 'goals.empty.desc' })}
        />
      ) : (
        <>
          {waiting.length > 0 && (
            <section className="space-y-2">
              <h2 className="text-sm font-medium text-amber-600 dark:text-amber-400">
                {intl.formatMessage({ id: 'goals.section.waiting' }, { n: waiting.length })}
              </h2>
              <div className="grid gap-3 lg:grid-cols-2">
                {waiting.map((t) => (
                  <GoalCard key={t.id} task={t} onOpen={() => openDetail(t.id)} onChanged={load} />
                ))}
              </div>
            </section>
          )}

          <section className="space-y-2">
            <h2 className="text-sm font-medium text-foreground">
              {intl.formatMessage({ id: 'goals.section.active' }, { n: active.length })}
            </h2>
            {active.length === 0 ? (
              <p className="text-xs text-muted-foreground">
                {intl.formatMessage({ id: 'goals.section.activeEmpty' })}
              </p>
            ) : (
              <div className="grid gap-3 lg:grid-cols-2">
                {active.map((t) => (
                  <GoalCard key={t.id} task={t} onOpen={() => openDetail(t.id)} onChanged={load} />
                ))}
              </div>
            )}
          </section>

          {finished.length > 0 && (
            <section className="space-y-2">
              <h2 className="text-sm font-medium text-muted-foreground">
                {intl.formatMessage({ id: 'goals.section.finished' })}
              </h2>
              <Card data-size="sm">
                <CardContent className="divide-y divide-surface-border">
                  {finished.map((t) => (
                    <button
                      key={t.id}
                      type="button"
                      onClick={() => openDetail(t.id)}
                      className="flex w-full items-center gap-2 py-2 text-left text-xs hover:bg-secondary/50"
                    >
                      <span className="min-w-0 flex-1 truncate text-foreground">{t.title}</span>
                      <GoalStatusBadge status={t.status} />
                      <span className="font-mono tabular-nums text-muted-foreground">
                        {timeAgo(t.completed_at ?? t.updated_at)}
                      </span>
                    </button>
                  ))}
                </CardContent>
              </Card>
            </section>
          )}
        </>
      )}

      <GoalDetailDialog taskId={detailTask} onClose={() => openDetail(null)} onChanged={load} />
    </div>
  );
}
