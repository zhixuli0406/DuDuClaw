import { useCallback, useMemo, useState } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate } from 'react-router';
import { CircleDot, Hand, Radar, UserCheck } from 'lucide-react';
import { api, type GoalTimeline, type PauseReasonToken, type TaskInfo } from '@/lib/api';
import { timeAgo, formatHms, secondsBetween } from '@/lib/format';
import { toast, formatError } from '@/lib/toast';
import { Badge, Button } from '@/components/mds';

/**
 * GoalLoopPanel — I-2c: the goal-loop-specific content that used to live
 * ONLY inside `/goals?task=<id>`'s dialog (`GoalDetailDialog`, now removed)
 * — acceptance contract, risk boundary, confirmed facts, pending kickoff
 * approval, and the round-by-round judge timeline. `/tasks/:id` is now the
 * single detail surface for every task including goal-mode ones; these
 * pieces render there, gated by the caller on `task.goal_mode`, alongside
 * the page's own dual-clock + iteration list (which the old dialog never
 * had) instead of duplicating a second detail UI.
 */

/** The closed set the backend can send (`pause_reason::PauseReason`). Kept as
 *  a runtime list, not just a TS type, so a token this build has never heard
 *  of (older/newer gateway) falls back to `unknown` = 「需要人工確認」 rather
 *  than rendering a raw key. */
const PAUSE_REASONS: readonly PauseReasonToken[] = [
  'no_progress',
  'budget_exhausted',
  'blocked_needs_decision',
  'infra',
  'restart',
  'unknown',
];

/** H11 — why this goal stopped, in one chip. `judge_feedback` (rendered right
 *  below wherever this appears) is free text; this is the triage. Only
 *  meaningful for `needs_human` — callers should gate rendering on that. */
export function PauseReasonChip({ reason }: { reason?: string | null }) {
  const intl = useIntl();
  const token = (PAUSE_REASONS as readonly string[]).includes(reason ?? '')
    ? (reason as PauseReasonToken)
    : 'unknown';
  return (
    <span className="inline-flex items-center rounded-full border border-amber-500/40 bg-amber-500/10 px-2 py-0.5 text-[11px] font-medium text-amber-700 dark:text-amber-300">
      {intl.formatMessage({ id: `goals.pauseReason.${token}` })}
    </span>
  );
}

/**
 * GoalTakeoverButton — the fourth needs_human exit (channel decision cards
 * offer retry/done primary + abort/take-over secondary, `channel_format.rs`
 * `telegram_goal_buttons` et al.). The shared `NeedsHumanActions` component
 * (Inbox + `/tasks/:id`) intentionally stays a 3-button retry/done/abort
 * surface for every task type — this button is the one goal-mode-only
 * addition, rendered next to it rather than folded into that shared
 * component, so a non-goal `blocked` task's decision flow is untouched.
 */
export function GoalTakeoverButton({
  taskId,
  onDecided,
}: {
  taskId: string;
  onDecided: () => void;
}) {
  const intl = useIntl();
  const [busy, setBusy] = useState(false);

  const takeover = useCallback(async () => {
    if (busy) return;
    setBusy(true);
    try {
      const r = await api.tasks.goalDecide(taskId, 'takeover');
      toast.success(r.message);
      onDecided();
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
    } finally {
      setBusy(false);
    }
  }, [busy, taskId, intl, onDecided]);

  return (
    <Button variant="outline" size="sm" disabled={busy} onClick={() => void takeover()}>
      <Hand className="size-3.5" />
      {intl.formatMessage({ id: 'goals.decide.takeover' })}
    </Button>
  );
}

/** Goal contract cards: risk boundary / acceptance criteria / latest result /
 *  confirmed facts + pending hypotheses. Each renders only when present —
 *  a task with none of these (a plain board task, or a goal task with no
 *  contract fields set) contributes nothing to the DOM. */
export function GoalContractCards({ task }: { task: TaskInfo }) {
  const intl = useIntl();
  const hasState =
    !!task.goal_state &&
    ((task.goal_state.confirmed_facts?.length ?? 0) > 0 ||
      (task.goal_state.pending_hypotheses?.length ?? 0) > 0);

  if (!task.risk_boundary && !task.acceptance_criteria && !task.result_summary && !hasState) {
    return null;
  }

  return (
    <div className="space-y-2">
      {task.risk_boundary && (
        <div className="rounded-lg bg-secondary px-3 py-2 text-xs">
          <p className="mb-0.5 font-medium text-foreground">
            {intl.formatMessage({ id: 'goals.detail.riskBoundary' })}
          </p>
          <p className="whitespace-pre-wrap text-muted-foreground">{task.risk_boundary}</p>
        </div>
      )}
      {task.acceptance_criteria && (
        <div className="rounded-lg bg-secondary px-3 py-2 text-xs">
          <p className="mb-0.5 font-medium text-foreground">
            {intl.formatMessage({ id: 'goals.detail.criteria' })}
          </p>
          <p className="whitespace-pre-wrap text-muted-foreground">{task.acceptance_criteria}</p>
        </div>
      )}
      {task.result_summary && (
        <div className="rounded-lg bg-secondary px-3 py-2 text-xs">
          <p className="mb-0.5 font-medium text-foreground">
            {intl.formatMessage({ id: 'goals.detail.result' })}
          </p>
          <p className="whitespace-pre-wrap text-muted-foreground">{task.result_summary}</p>
        </div>
      )}
      {hasState && (
        <div className="rounded-lg bg-secondary px-3 py-2 text-xs">
          <p className="mb-0.5 font-medium text-foreground">
            {intl.formatMessage({ id: 'goals.detail.state' })}
          </p>
          {(task.goal_state?.confirmed_facts ?? []).map((f, i) => (
            <p key={`f${i}`} className="text-muted-foreground">
              ✓ {f}
            </p>
          ))}
          {(task.goal_state?.pending_hypotheses ?? []).map((h, i) => (
            <p key={`h${i}`} className="text-muted-foreground/80">
              ? {h}
            </p>
          ))}
        </div>
      )}
    </div>
  );
}

/** Pending kickoff approval — a human node the loop is blocked on before its
 *  first dispatch (collaborator/consultant autonomy gate). Distinct from
 *  `needs_human`: the task hasn't started its loop yet. */
export function GoalPendingKickoff({
  pendingKickoff,
  onDecided,
}: {
  pendingKickoff: GoalTimeline['pending_kickoff'];
  onDecided: () => void;
}) {
  const intl = useIntl();
  const [busy, setBusy] = useState(false);

  if (!pendingKickoff) return null;

  const decide = async (approve: boolean) => {
    setBusy(true);
    try {
      await api.approvals.decide(pendingKickoff.id, approve);
      toast.success(intl.formatMessage({ id: approve ? 'goals.kickoff.approved' : 'goals.kickoff.denied' }));
      onDecided();
    } catch (e) {
      toast.error(formatError(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="rounded-lg border border-amber-500/40 bg-amber-500/10 px-3 py-2">
      <p className="mb-1.5 flex items-center gap-1.5 text-xs font-medium text-amber-700 dark:text-amber-300">
        <UserCheck className="size-3.5" />
        {intl.formatMessage({ id: 'goals.kickoff.pending' })}
      </p>
      <p className="mb-2 text-xs text-muted-foreground">{pendingKickoff.summary}</p>
      <div className="flex gap-1.5">
        <Button variant="brand" size="sm" disabled={busy} onClick={() => void decide(true)}>
          {intl.formatMessage({ id: 'goals.kickoff.approve' })}
        </Button>
        <Button variant="outline" size="sm" disabled={busy} onClick={() => void decide(false)}>
          {intl.formatMessage({ id: 'goals.kickoff.deny' })}
        </Button>
      </div>
    </div>
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

/** Round-by-round judge timeline, merged with the human/loop-relevant
 *  activity events (kickoff / needs_human / oscillation / reassignment /
 *  ...) — the richer view `GoalDetailDialog` used to own exclusively. */
export function GoalRoundTimeline({ task, timeline }: { task: TaskInfo; timeline: GoalTimeline | null }) {
  const intl = useIntl();
  const navigate = useNavigate();

  const nodes: TimelineNode[] = useMemo(() => {
    if (!timeline) return [];
    const out: TimelineNode[] = [];
    for (const it of timeline.iterations ?? []) {
      out.push({ key: `r${it.round}`, at: it.dispatched_at, kind: 'round', round: it });
    }
    const interesting = /^goal_loop\.(created|kickoff_|needs_human|oscillation|human_decision|reassigned|dep_blocked|operator_skipped|observer_autoclose)/;
    for (const ev of timeline.activity ?? []) {
      if (interesting.test(ev.type)) {
        out.push({ key: `e${ev.id}`, at: ev.timestamp, kind: 'event', event: ev });
      }
    }
    return out.sort((a, b) => a.at.localeCompare(b.at));
  }, [timeline]);

  return (
    <div>
      <h2 className="mb-2 px-1.5 text-xs font-medium text-muted-foreground">
        {intl.formatMessage({ id: 'goals.detail.timeline' })}
      </h2>
      {nodes.length === 0 ? (
        <p className="px-1.5 text-xs text-muted-foreground">
          {intl.formatMessage({ id: 'goals.detail.timelineEmpty' })}
        </p>
      ) : (
        <div className="space-y-0 px-1.5">
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
                        {intl.formatMessage({ id: 'goals.redispatch' }, { n: n.round.dispatch_count })}
                      </span>
                    )}
                    {(n.round.repeat_streak ?? 0) >= 2 && (
                      <span className="text-amber-600 dark:text-amber-400">
                        {intl.formatMessage({ id: 'goals.repeatState' }, { n: n.round.repeat_streak })}
                      </span>
                    )}
                    <span className="ml-auto font-mono tabular-nums text-muted-foreground">
                      {n.round.submitted_at
                        ? formatHms(secondsBetween(n.round.dispatched_at, n.round.submitted_at))
                        : timeAgo(n.round.judged_at ?? n.round.submitted_at ?? n.round.dispatched_at)}
                    </span>
                  </div>
                  {/* Dispatched / submitted / judged breakdown — the Iterative
                      Kanban stage timestamps `/tasks/:id` used to own alone. */}
                  <div className="flex flex-wrap gap-x-3 gap-y-0.5 text-muted-foreground">
                    <span>
                      {intl.formatMessage({ id: 'tasks.iterations.dispatched' })}{' '}
                      <span className="font-mono tabular-nums">{timeAgo(n.round.dispatched_at)}</span>
                    </span>
                    <span>
                      {intl.formatMessage({ id: 'tasks.iterations.submitted' })}{' '}
                      <span className="font-mono tabular-nums">
                        {n.round.submitted_at
                          ? timeAgo(n.round.submitted_at)
                          : intl.formatMessage({ id: 'tasks.iterations.pending' })}
                      </span>
                    </span>
                    {n.round.judged_at && (
                      <span>
                        {intl.formatMessage({ id: 'tasks.iterations.judged' })}{' '}
                        <span className="font-mono tabular-nums">{timeAgo(n.round.judged_at)}</span>
                      </span>
                    )}
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
                          {intl.formatMessage({ id: `goals.aspect.${a.name}`, defaultMessage: a.name })}
                          {a.pass ? ' ✓' : ' ✗'}
                        </span>
                      ))}
                    </div>
                  )}
                  {n.round.judge_feedback && (
                    <p className="whitespace-pre-wrap text-muted-foreground">{n.round.judge_feedback}</p>
                  )}
                  {(() => {
                    const run = timeline?.runs?.find((r) => r.round === n.round!.round);
                    return run ? (
                      <button
                        type="button"
                        onClick={() => navigate(`/runs?run=${encodeURIComponent(run.id)}`)}
                        className="text-chart-1 hover:underline"
                      >
                        {intl.formatMessage({ id: 'goals.viewRun' }, { steps: run.step_count })}
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
      )}
      {task.goal_mode && (
        <div className="flex justify-end pt-2">
          <Button variant="ghost" size="sm" onClick={() => navigate('/foresight')}>
            <Radar className="size-3.5" />
            {intl.formatMessage({ id: 'goals.detail.foresightLink' })}
          </Button>
        </div>
      )}
    </div>
  );
}

/** Small badge form of the round counter, used near the title meta row —
 *  matches the dialog's old status-strip badge (`goals.roundBadge`). */
export function GoalRoundBadge({ task }: { task: TaskInfo }) {
  const intl = useIntl();
  if (!task.goal_mode) return null;
  return (
    <Badge variant="secondary">
      {intl.formatMessage({ id: 'goals.roundBadge' }, { n: (task.revision_round ?? 0) + 1 })}
    </Badge>
  );
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

/** Soft-cap warning — the revision round is approaching `iteration_cap`
 *  (diminishing returns). Was part of the old dialog's status strip. */
export function GoalDiminishingBadge({ task }: { task: TaskInfo }) {
  const intl = useIntl();
  if (!task.goal_mode || !task.diminishing) return null;
  return (
    <Badge variant="secondary" className="text-amber-600 dark:text-amber-400">
      {intl.formatMessage({ id: 'goals.diminishing' })}
    </Badge>
  );
}

/** Goal assignment form v2's per-goal wall-clock deadline badge — was part of
 *  the old dialog's status strip, now next to `GoalRoundBadge`. */
export function GoalDeadlineBadge({ task }: { task: TaskInfo }) {
  const intl = useIntl();
  if (!task.goal_mode || !task.deadline_at) return null;
  const past = new Date(task.deadline_at).getTime() < Date.now();
  return (
    <Badge variant="secondary" className={past ? 'text-rose-600 dark:text-rose-400' : 'text-muted-foreground'}>
      {past
        ? intl.formatMessage({ id: 'goals.detail.deadlinePast' })
        : intl.formatMessage({ id: 'goals.detail.deadline' }, { when: timeLeft(task.deadline_at) })}
    </Badge>
  );
}
