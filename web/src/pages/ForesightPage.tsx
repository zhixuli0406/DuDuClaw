import { useCallback, useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { Radar, Target, ArrowRight } from 'lucide-react';
import { useNavigate } from 'react-router';

import {
  api,
  type ForwardAgentSummary,
  type ForwardPredictionRow,
  type ForwardChainRound,
  type ForwardCalibration,
  type ForwardStateRow,
  type BeliefRow,
  type BeliefStats,
} from '@/lib/api';
import { timeAgo } from '@/lib/format';
import { toast } from '@/lib/toast';
import { useAgentsStore } from '@/stores/agents-store';
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
  Select,
  SelectTrigger,
  SelectValue,
  SelectContent,
  SelectItem,
  Spinner,
  Tabs,
  TabsList,
  TabsTab,
  TabsPanel,
  useErrorMessage,
} from '@/components/mds';

/**
 * ForesightPage (`/foresight`) — the LLM→LWM loop as a first-class page.
 *
 * Shows the whole predict → act → observe → score cycle per task (the
 * v1.53 task forward model + v1.54 calibration layer), not just the
 * fragments the old Memory-page tab surfaced: every settled round can be
 * opened into an expected-vs-observed comparison, each agent gets a
 * query-time skill verdict (three honest states, never "seems to work"),
 * and the learned state buckets show what the world model has actually
 * accumulated. Plain-language labels throughout — engine enums never leak.
 */

const CATEGORY_ORDER = ['negligible', 'moderate', 'significant', 'critical'] as const;

function categoryTone(category: string | null): string {
  switch (category) {
    case 'negligible':
      return 'text-emerald-600 dark:text-emerald-400';
    case 'moderate':
      return 'text-foreground';
    case 'significant':
      return 'text-amber-600 dark:text-amber-400';
    case 'critical':
      return 'text-rose-600 dark:text-rose-400';
    default:
      return 'text-muted-foreground';
  }
}

const CATEGORY_FILL: Record<(typeof CATEGORY_ORDER)[number], string> = {
  negligible: 'bg-emerald-500/80',
  moderate: 'bg-chart-3',
  significant: 'bg-amber-500/80',
  critical: 'bg-rose-500/80',
};

function CategoryDistribution({
  categories,
  settled,
}: {
  categories: Record<string, number>;
  settled: number;
}) {
  const intl = useIntl();
  const parts = CATEGORY_ORDER.map((c) => ({ c, n: categories[c] ?? 0 })).filter((p) => p.n > 0);
  if (settled === 0 || parts.length === 0) return null;
  return (
    <div className="space-y-1.5 border-t border-surface-border pt-3">
      <div
        className="flex h-2 w-full overflow-hidden rounded-full bg-muted"
        role="img"
        aria-label={parts
          .map((p) => `${intl.formatMessage({ id: `forward.category.${p.c}` })} ${p.n}`)
          .join(', ')}
      >
        {parts.map((p) => (
          <div key={p.c} className={CATEGORY_FILL[p.c]} style={{ width: `${(p.n / settled) * 100}%` }} />
        ))}
      </div>
      <div className="flex flex-wrap gap-x-3 gap-y-0.5">
        {parts.map((p) => (
          <span key={p.c} className="flex items-center gap-1 text-xs">
            <span className={`size-1.5 rounded-full ${CATEGORY_FILL[p.c]}`} />
            <span className={categoryTone(p.c)}>{intl.formatMessage({ id: `forward.category.${p.c}` })}</span>
            <span className="font-mono tabular-nums text-muted-foreground">{p.n}</span>
          </span>
        ))}
      </div>
    </div>
  );
}

/** The loop, drawn as a 4-step strip with live counts under each stage. */
function LoopStrip({ summaries }: { summaries: ForwardAgentSummary[] }) {
  const intl = useIntl();
  const total = summaries.reduce((a, s) => a + s.total, 0);
  const settled = summaries.reduce((a, s) => a + s.settled, 0);
  const good = summaries.reduce(
    (a, s) => a + (s.categories['negligible'] ?? 0) + (s.categories['moderate'] ?? 0),
    0,
  );
  const observedFull = summaries.reduce((a, s) => a + (s.fidelity['full'] ?? 0), 0);
  const observedMcp = summaries.reduce((a, s) => a + (s.fidelity['mcp_only'] ?? 0), 0);
  const steps = [
    { id: 'predict', value: `${total}` },
    { id: 'act', value: `${total}` },
    { id: 'observe', value: `${observedFull + observedMcp}` },
    { id: 'score', value: settled > 0 ? `${good}/${settled}` : '0' },
  ];
  return (
    <Card data-size="sm">
      <CardContent className="flex flex-wrap items-center gap-2 py-3">
        {steps.map((s, i) => (
          <div key={s.id} className="flex items-center gap-2">
            <div className="rounded-lg bg-secondary px-3 py-1.5 text-center">
              <p className="text-xs text-muted-foreground">
                {intl.formatMessage({ id: `foresight.loop.${s.id}` })}
              </p>
              <p className="font-mono text-sm font-medium tabular-nums text-foreground">{s.value}</p>
            </div>
            {i < steps.length - 1 && <ArrowRight className="size-3.5 text-muted-foreground" />}
          </div>
        ))}
        <p className="ml-auto max-w-[26rem] text-xs text-muted-foreground">
          {intl.formatMessage({ id: 'foresight.loop.hint' })}
        </p>
      </CardContent>
    </Card>
  );
}

/** Query-time skill verdict card for one agent. */
function CalibrationCard({ agentId }: { agentId: string }) {
  const intl = useIntl();
  const [cal, setCal] = useState<ForwardCalibration | null>(null);
  useEffect(() => {
    let alive = true;
    api.forward
      .calibration(agentId)
      .then((r) => alive && setCal(r.calibration))
      .catch((e) => console.warn('[api]', e));
    return () => {
      alive = false;
    };
  }, [agentId]);
  if (!cal) return null;
  const labelTone =
    cal.label === 'supported'
      ? 'text-emerald-600 dark:text-emerald-400'
      : cal.label === 'indistinguishable_from_luck'
        ? 'text-amber-600 dark:text-amber-400'
        : 'text-muted-foreground';
  return (
    <Card data-size="sm">
      <CardContent className="space-y-3">
        <div className="flex items-center justify-between gap-2">
          <h3 className="text-sm font-medium text-foreground">
            {intl.formatMessage({ id: 'foresight.calibration.title' })}
          </h3>
          <span className={`text-sm font-medium ${labelTone}`}>
            {intl.formatMessage({ id: `foresight.label.${cal.label}` })}
          </span>
        </div>
        <div className="grid grid-cols-4 gap-2 text-center">
          <div>
            <p className="font-mono text-lg tabular-nums text-foreground">{cal.n}</p>
            <p className="text-xs text-muted-foreground">{intl.formatMessage({ id: 'foresight.cal.n' })}</p>
          </div>
          <div>
            <p className="font-mono text-lg tabular-nums text-foreground">
              {cal.hit_rate != null ? `${Math.round(cal.hit_rate * 100)}%` : '—'}
            </p>
            <p className="text-xs text-muted-foreground">{intl.formatMessage({ id: 'foresight.cal.hitRate' })}</p>
          </div>
          <div>
            <p className="font-mono text-lg tabular-nums text-foreground">
              {cal.avg_brier != null ? cal.avg_brier.toFixed(2) : '—'}
            </p>
            <p className="text-xs text-muted-foreground">{intl.formatMessage({ id: 'forward.metric.brier' })}</p>
          </div>
          <div>
            <p className="font-mono text-lg tabular-nums text-foreground">
              {cal.brier_skill_score != null ? cal.brier_skill_score.toFixed(2) : '—'}
            </p>
            <p className="text-xs text-muted-foreground">{intl.formatMessage({ id: 'foresight.cal.bss' })}</p>
          </div>
        </div>
        {cal.bins.length > 0 && (
          <div className="space-y-1 border-t border-surface-border pt-3">
            <p className="text-xs text-muted-foreground">
              {intl.formatMessage({ id: 'foresight.cal.bins' })}
            </p>
            {cal.bins.map((b, i) => (
              <div key={i} className="flex items-center gap-2 text-xs">
                <span className="w-16 shrink-0 font-mono tabular-nums text-muted-foreground">
                  {Math.round(b.p_mean * 100)}%
                </span>
                <div className="h-1.5 flex-1 overflow-hidden rounded-full bg-muted">
                  <div
                    className="h-full rounded-full bg-chart-1"
                    style={{ width: `${Math.round(b.emp_rate * 100)}%` }}
                  />
                </div>
                <span className="w-20 shrink-0 text-right font-mono tabular-nums text-muted-foreground">
                  {Math.round(b.emp_rate * 100)}% · n={b.n}
                </span>
              </div>
            ))}
            <p className="text-[11px] text-muted-foreground/80">
              {intl.formatMessage({ id: 'foresight.cal.binsHint' })}
            </p>
          </div>
        )}
      </CardContent>
    </Card>
  );
}

/** Learned state buckets — what the world model has accumulated. */
function StatesCard({ agentId }: { agentId: string }) {
  const intl = useIntl();
  const [states, setStates] = useState<ForwardStateRow[]>([]);
  useEffect(() => {
    let alive = true;
    api.forward
      .states(agentId || undefined, 8)
      .then((r) => alive && setStates(r.states))
      .catch((e) => console.warn('[api]', e));
    return () => {
      alive = false;
    };
  }, [agentId]);
  if (states.length === 0) return null;
  const max = Math.max(...states.map((s) => s.n_samples), 1);
  return (
    <Card data-size="sm">
      <CardContent className="space-y-2">
        <h3 className="text-sm font-medium text-foreground">
          {intl.formatMessage({ id: 'foresight.states.title' })}
        </h3>
        <p className="text-xs text-muted-foreground">
          {intl.formatMessage({ id: 'foresight.states.desc' })}
        </p>
        {states.map((s) => {
          // canonical key: agent|goal_kind|phase|has_spec → plain words
          const parts = s.state_key.split('|');
          const kind = parts[1] ?? s.state_key;
          const phase = parts[2] ?? '';
          return (
            <div key={s.state_key} className="flex items-center gap-2 text-xs">
              <span className="w-40 shrink-0 truncate text-foreground">
                {intl.formatMessage({ id: `foresight.goalKind.${kind}`, defaultMessage: kind })}
                {phase && (
                  <span className="text-muted-foreground">
                    {' · '}
                    {intl.formatMessage({ id: `foresight.phase.${phase}`, defaultMessage: phase })}
                  </span>
                )}
              </span>
              <div className="h-1.5 flex-1 overflow-hidden rounded-full bg-muted">
                <div
                  className="h-full rounded-full bg-chart-2"
                  style={{ width: `${(s.n_samples / max) * 100}%` }}
                />
              </div>
              <span className="w-14 shrink-0 text-right font-mono tabular-nums text-muted-foreground">
                {s.n_samples}
              </span>
            </div>
          );
        })}
      </CardContent>
    </Card>
  );
}

/** Expected-vs-observed comparison for one settled round. */
function ChainRoundView({ round }: { round: ForwardChainRound }) {
  const intl = useIntl();
  const t = (id: string, dm?: string) => intl.formatMessage({ id, defaultMessage: dm });
  const rows: Array<{ label: string; expected: string; observed: string }> = [];
  if (round.expected || round.observed) {
    rows.push({
      label: t('foresight.chain.outcome'),
      expected: round.expected ? t(`foresight.outcome.${round.expected.outcome}`, round.expected.outcome) : '—',
      observed: round.observed ? t(`foresight.outcome.${round.observed.outcome}`, round.observed.outcome) : '—',
    });
    rows.push({
      label: t('foresight.chain.tools'),
      expected: round.expected?.tool_classes.map((c) => t(`foresight.tool.${c}`, c)).join('、') || '—',
      observed: round.observed?.tool_classes.map((c) => t(`foresight.tool.${c}`, c)).join('、') || '—',
    });
    rows.push({
      label: t('foresight.chain.calls'),
      expected: round.expected ? `${round.expected.call_band[0]}–${round.expected.call_band[1]}` : '—',
      observed: round.observed
        ? `${round.observed.calls}${round.observed.errors > 0 ? `（${intl.formatMessage({ id: 'foresight.chain.errors' }, { n: round.observed.errors })}）` : ''}`
        : '—',
    });
    rows.push({
      label: t('foresight.chain.artifact'),
      expected: round.expected
        ? t(`foresight.artifact.${round.expected.artifact}`, round.expected.artifact)
        : '—',
      observed: round.observed
        ? t(`foresight.artifact.${round.observed.artifact}`, round.observed.artifact)
        : '—',
    });
  }
  return (
    <div className="space-y-2 rounded-lg border border-surface-border p-3">
      <div className="flex flex-wrap items-center gap-2 text-xs">
        <Badge variant="secondary">R{round.round}</Badge>
        <span className="text-muted-foreground">
          {t(`forward.source.${round.source}`, round.source)}
        </span>
        {round.expected && (
          <span className="font-mono tabular-nums text-muted-foreground">
            {intl.formatMessage({ id: 'foresight.chain.confidence' })}{' '}
            {Math.round(round.expected.confidence * 100)}%
          </span>
        )}
        <span className={`ml-auto ${categoryTone(round.category)}`}>
          {round.settled_at == null
            ? intl.formatMessage({ id: 'forward.pending' })
            : round.category
              ? t(`forward.category.${round.category}`, round.category)
              : '—'}
        </span>
        {round.brier != null && (
          <span className="font-mono tabular-nums text-muted-foreground">{round.brier.toFixed(2)}</span>
        )}
      </div>
      {rows.length > 0 && (
        <table className="w-full text-xs">
          <thead>
            <tr className="text-muted-foreground">
              <th className="w-24 pb-1 text-left font-normal" />
              <th className="pb-1 text-left font-normal">
                {intl.formatMessage({ id: 'foresight.chain.expected' })}
              </th>
              <th className="pb-1 text-left font-normal">
                {intl.formatMessage({ id: 'foresight.chain.observed' })}
              </th>
            </tr>
          </thead>
          <tbody>
            {rows.map((r) => (
              <tr key={r.label} className="border-t border-surface-border/60">
                <td className="py-1 pr-2 text-muted-foreground">{r.label}</td>
                <td className="py-1 pr-2 text-foreground">{r.expected}</td>
                <td className="py-1 text-foreground">{r.observed}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      {/* Which dimension the prediction missed on (settle-time breakdown —
          persisted since 2026-08-14; older rows have no chips). */}
      {round.error_breakdown && (
        <div className="flex flex-wrap gap-1.5 text-[11px]">
          {(
            [
              ['tools', round.error_breakdown.tool_set_error, true],
              ['volume', round.error_breakdown.volume_error, true],
              ['outcome', round.error_breakdown.outcome_error, round.error_breakdown.outcome_error_applicable],
              ['artifact', round.error_breakdown.artifact_error, true],
            ] as Array<[string, number, boolean]>
          ).map(([dim, err, applicable]) => (
            <span
              key={dim}
              className={`rounded-full px-2 py-0.5 ${
                !applicable
                  ? 'bg-muted text-muted-foreground/60'
                  : err <= 0.2
                    ? 'bg-emerald-500/10 text-emerald-600 dark:text-emerald-400'
                    : err <= 0.5
                      ? 'bg-amber-500/10 text-amber-600 dark:text-amber-400'
                      : 'bg-rose-500/10 text-rose-600 dark:text-rose-400'
              }`}
              title={applicable ? err.toFixed(2) : undefined}
            >
              {intl.formatMessage({ id: `foresight.dim.${dim}` })}
              {applicable ? (err <= 0.2 ? ' ✓' : ` ${Math.round(err * 100)}%`) : ' —'}
            </span>
          ))}
        </div>
      )}
      {round.observed && (
        <p className="text-[11px] text-muted-foreground/80">
          {intl.formatMessage(
            { id: 'foresight.chain.fidelityNote' },
            {
              fidelity: intl.formatMessage({
                id: `foresight.fidelity.${round.observed.fidelity}`,
                defaultMessage: round.observed.fidelity,
              }),
            },
          )}
        </p>
      )}
    </div>
  );
}

/** Drill-down dialog: the full loop for one task. */
function ChainDialog({ taskId, onClose }: { taskId: string | null; onClose: () => void }) {
  const intl = useIntl();
  const [rounds, setRounds] = useState<ForwardChainRound[] | null>(null);
  useEffect(() => {
    if (!taskId) {
      setRounds(null);
      return;
    }
    let alive = true;
    api.forward
      .chain(taskId)
      .then((r) => alive && setRounds(r.rounds))
      .catch((e) => {
        console.warn('[api]', e);
        if (alive) setRounds([]);
      });
    return () => {
      alive = false;
    };
  }, [taskId]);
  return (
    <Dialog open={taskId !== null} onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle>
            {intl.formatMessage({ id: 'foresight.chain.title' })}
            {taskId && <span className="ml-2 font-mono text-sm text-muted-foreground">{taskId.slice(0, 8)}</span>}
          </DialogTitle>
        </DialogHeader>
        <div className="max-h-[65vh] space-y-3 overflow-y-auto">
          {rounds === null ? (
            <div className="flex justify-center py-8">
              <Spinner />
            </div>
          ) : rounds.length === 0 ? (
            <p className="py-6 text-center text-sm text-muted-foreground">
              {intl.formatMessage({ id: 'foresight.chain.empty' })}
            </p>
          ) : (
            rounds.map((r) => <ChainRoundView key={r.prediction_id} round={r} />)
          )}
        </div>
      </DialogContent>
    </Dialog>
  );
}

/** Predicted/realized direction label — up|down|flat → 看漲/看跌/持平. */
function directionLabel(intl: ReturnType<typeof useIntl>, direction: string | null): string {
  if (!direction) return '—';
  return intl.formatMessage({ id: `belief.direction.${direction}`, defaultMessage: direction });
}

/** Calibration stats card for the "信念與驗證" tab — honest three-state
 *  posture (no agent picked / n<30 counts-only / full stats), same spirit
 *  as `CalibrationCard` above but against the belief store instead
 *  of the task forward model. */
function BeliefStatsCard({ agentId, stats }: { agentId: string; stats: BeliefStats | null }) {
  const intl = useIntl();
  if (!agentId) {
    return (
      <Card data-size="sm">
        <CardContent className="py-4 text-center text-sm text-muted-foreground">
          {intl.formatMessage({ id: 'belief.stats.selectAgent' })}
        </CardContent>
      </Card>
    );
  }
  if (!stats) return null;
  if (stats.insufficient_samples) {
    return (
      <Card data-size="sm">
        <CardContent className="space-y-2">
          <h3 className="text-sm font-medium text-foreground">
            {intl.formatMessage({ id: 'belief.stats.title' })}
          </h3>
          <p className="text-sm text-muted-foreground">
            {intl.formatMessage({ id: 'belief.stats.insufficient' }, { n: stats.n_settled })}
          </p>
        </CardContent>
      </Card>
    );
  }
  const overPct = stats.overconfidence != null ? Math.round(stats.overconfidence * 100) : null;
  return (
    <Card data-size="sm">
      <CardContent className="space-y-3">
        <h3 className="text-sm font-medium text-foreground">
          {intl.formatMessage({ id: 'belief.stats.title' })}
        </h3>
        <div className="grid grid-cols-4 gap-2 text-center">
          <div>
            <p className="font-mono text-lg tabular-nums text-foreground">{stats.n_settled}</p>
            <p className="text-xs text-muted-foreground">{intl.formatMessage({ id: 'belief.stats.nSettled' })}</p>
          </div>
          <div>
            <p className="font-mono text-lg tabular-nums text-foreground">
              {stats.hit_rate_wilson_low != null ? `${Math.round(stats.hit_rate_wilson_low * 100)}%` : '—'}
            </p>
            <p className="text-xs text-muted-foreground">
              {intl.formatMessage({ id: 'belief.stats.hitRateWilson' })}
            </p>
          </div>
          <div>
            <p className="font-mono text-lg tabular-nums text-foreground">
              {stats.mean_brier != null ? stats.mean_brier.toFixed(2) : '—'}
            </p>
            <p className="text-xs text-muted-foreground">{intl.formatMessage({ id: 'belief.stats.meanBrier' })}</p>
          </div>
          <div>
            <p
              className={`font-mono text-lg tabular-nums ${
                overPct != null && overPct > 0 ? 'text-amber-600 dark:text-amber-400' : 'text-foreground'
              }`}
            >
              {overPct != null ? `${overPct > 0 ? '+' : ''}${overPct}%` : '—'}
            </p>
            <p className="text-xs text-muted-foreground">
              {intl.formatMessage({ id: 'belief.stats.overconfidence' })}
            </p>
          </div>
        </div>
        {stats.per_subject.length > 0 && (
          <div className="space-y-1 border-t border-surface-border pt-3">
            <p className="text-xs text-muted-foreground">
              {intl.formatMessage({ id: 'belief.stats.perSubject' })}
            </p>
            {stats.per_subject.map((s) => (
              <div key={s.subject} className="flex items-center gap-2 text-xs">
                <span className="w-20 shrink-0 truncate text-foreground">{s.subject}</span>
                <span className="font-mono tabular-nums text-muted-foreground">
                  {s.hits}/{s.n_settled}
                </span>
                {s.mean_brier != null && (
                  <span className="ml-auto font-mono tabular-nums text-muted-foreground">
                    {s.mean_brier.toFixed(2)}
                  </span>
                )}
              </div>
            ))}
          </div>
        )}
      </CardContent>
    </Card>
  );
}

/** One belief row: subject · predicted direction+confidence →
 *  realized direction ✓/✗ (flat_band shows "持平") · Brier · time. */
function BeliefRowView({ belief }: { belief: BeliefRow }) {
  const intl = useIntl();
  const settled = belief.settled_at != null;
  const isFlatBand = belief.outcome === 'flat_band';
  const isHit = belief.outcome === 'hit';
  return (
    <div className="flex flex-wrap items-center gap-x-3 gap-y-1 py-2 text-xs">
      <span className="font-mono tabular-nums text-muted-foreground">
        {timeAgo(belief.settled_at ?? belief.predicted_at)}
      </span>
      <span className="max-w-[10rem] truncate font-medium text-foreground" title={belief.subject}>
        {belief.subject}
      </span>
      <span className="text-muted-foreground">
        {directionLabel(intl, belief.direction)} {Math.round(belief.prob * 100)}%
      </span>
      <ArrowRight className="size-3 shrink-0 text-muted-foreground" />
      {!settled ? (
        <span className="text-muted-foreground">{intl.formatMessage({ id: 'belief.pending' })}</span>
      ) : isFlatBand ? (
        <span className="text-muted-foreground">{intl.formatMessage({ id: 'belief.direction.flat' })}</span>
      ) : (
        <span className={isHit ? 'text-emerald-600 dark:text-emerald-400' : 'text-rose-600 dark:text-rose-400'}>
          {directionLabel(intl, belief.realized_direction)} {isHit ? '✓' : '✗'}
        </span>
      )}
      {belief.brier != null && (
        <span className="ml-auto font-mono tabular-nums text-muted-foreground">{belief.brier.toFixed(2)}</span>
      )}
    </div>
  );
}

/**
 * BeliefTab — the "信念與驗證" panel: agent-submitted directional
 * calls (up/down/flat + confidence) about any external subject, recorded
 * before the fact and settled against the realized value. A separate loop
 * from the task forward model above (`api.belief.*` / `belief_log`, not
 * `task_prediction_log`) — dedicated load/loading/error/empty state so it
 * works independently of whether the "任務預測" tab has any data.
 */
function BeliefTab({ agentId }: { agentId: string }) {
  const intl = useIntl();
  const errorText = useErrorMessage();
  const [beliefs, setBeliefs] = useState<BeliefRow[]>([]);
  const [stats, setStats] = useState<BeliefStats | null>(null);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<string | null>(null);

  const load = useCallback(() => {
    let alive = true;
    setLoading(true);
    let notified = false;
    const onFailure = (e: unknown) => {
      console.warn('[api]', e);
      if (!notified) {
        notified = true;
        if (alive) setLoadError(errorText(e));
      }
      return null;
    };
    Promise.all([
      api.belief.recent(agentId || undefined, 50).catch(onFailure),
      // Summary requires an agent_id server-side; skipping it when none is
      // selected is intentional, not a failure — resolves to `undefined`
      // (vs. `null` from a real failure) so the two cases stay distinguishable.
      agentId ? api.belief.summary(agentId).catch(onFailure) : Promise.resolve(undefined),
    ])
      .then(([recentRes, summaryRes]) => {
        if (!alive) return;
        if (recentRes !== null && summaryRes !== null) setLoadError(null);
        setBeliefs(recentRes?.beliefs ?? []);
        setStats(summaryRes ? summaryRes.stats : null);
      })
      .finally(() => alive && setLoading(false));
    return () => {
      alive = false;
    };
    // errorText stable from hook; intl from context.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [agentId]);

  useEffect(() => load(), [load]);

  if (loading) return <CollectionPageState state="loading" />;
  if (loadError) {
    return (
      <CollectionPageState
        state="error"
        icon={Radar}
        title={intl.formatMessage({ id: 'belief.error.title' })}
        description={loadError}
        action={
          <Button variant="outline" size="sm" onClick={() => load()}>
            {intl.formatMessage({ id: 'foresight.error.retry' })}
          </Button>
        }
      />
    );
  }
  if (beliefs.length === 0) {
    return (
      <CollectionPageState
        state="empty"
        icon={Radar}
        title={intl.formatMessage({ id: 'belief.empty.title' })}
        description={intl.formatMessage({ id: 'belief.empty.desc' })}
      />
    );
  }

  return (
    <div className="space-y-4">
      <BeliefStatsCard agentId={agentId} stats={stats} />
      <section className="space-y-2">
        <h2 className="text-sm font-medium text-foreground">
          {intl.formatMessage({ id: 'belief.list.title' })}
        </h2>
        <Card data-size="sm">
          <CardContent className="divide-y divide-surface-border">
            {beliefs.map((b) => (
              <BeliefRowView key={b.belief_id} belief={b} />
            ))}
          </CardContent>
        </Card>
      </section>
    </div>
  );
}

export function ForesightPage() {
  const intl = useIntl();
  const errorText = useErrorMessage();
  const navigate = useNavigate();
  const connState = useConnectionStore((s) => s.state);
  const { agents, fetchAgents } = useAgentsStore();
  const [selectedAgent, setSelectedAgent] = useState('');
  const [summaries, setSummaries] = useState<ForwardAgentSummary[]>([]);
  const [rows, setRows] = useState<ForwardPredictionRow[]>([]);
  const [windowScanned, setWindowScanned] = useState(0);
  const [windowCap, setWindowCap] = useState(0);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [chainTask, setChainTask] = useState<string | null>(null);
  const [tab, setTab] = useState<'predictions' | 'beliefs'>('predictions');

  const load = useCallback(() => {
    let alive = true;
    setLoading(true);
    let notified = false;
    // A failed RPC (e.g. insufficient role — forward.* requires manager) must
    // render as an error state, never as the same empty state a data-less
    // agent sees; otherwise permission problems masquerade as "no data".
    const onFailure = (e: unknown) => {
      console.warn('[api]', e);
      if (!notified) {
        notified = true;
        if (alive) setLoadError(errorText(e));
        toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: errorText(e) }));
      }
      return null;
    };
    Promise.all([
      api.forward.summary(selectedAgent || undefined).catch(onFailure),
      api.forward.recent(selectedAgent || undefined, 50).catch(onFailure),
    ])
      .then(([summary, recent]) => {
        if (!alive) return;
        if (summary && recent) setLoadError(null);
        setSummaries(summary?.agents ?? []);
        setWindowScanned(summary?.window_scanned ?? 0);
        setWindowCap(summary?.window_cap ?? 0);
        setRows(recent?.predictions ?? []);
      })
      .finally(() => alive && setLoading(false));
    return () => {
      alive = false;
    };
    // errorText stable from hook; intl from context.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selectedAgent]);

  useEffect(() => {
    if (connState === 'authenticated') {
      fetchAgents();
      return load();
    }
  }, [connState, fetchAgents, load]);

  const agentLabel = selectedAgent
    ? agents.find((a) => a.name === selectedAgent)?.display_name || selectedAgent
    : intl.formatMessage({ id: 'foresight.allAgents' });

  return (
    <div className="mx-auto w-full max-w-[1200px] space-y-4">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="flex min-w-0 items-center gap-2">
          <Radar className="size-5 text-muted-foreground" />
          <div>
            <h1 className="text-base font-medium">{intl.formatMessage({ id: 'foresight.title' })}</h1>
            <p className="text-sm text-muted-foreground">
              {intl.formatMessage({ id: 'foresight.subtitle' })}
            </p>
          </div>
        </div>
        <div className="flex items-center gap-2">
          <Select value={selectedAgent} onValueChange={(v) => setSelectedAgent(String(v))}>
            <SelectTrigger className="w-44">
              <SelectValue>{agentLabel}</SelectValue>
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="">{intl.formatMessage({ id: 'foresight.allAgents' })}</SelectItem>
              {agents.map((a) => (
                <SelectItem key={a.name} value={a.name}>
                  {a.display_name || a.name}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
          <Button variant="outline" size="sm" onClick={() => navigate('/goals')}>
            <Target className="size-3.5" />
            {intl.formatMessage({ id: 'foresight.assignGoal' })}
          </Button>
        </div>
      </div>

      <Tabs
        variant="line"
        value={tab}
        onValueChange={(v) => setTab(v as 'predictions' | 'beliefs')}
        className="flex flex-col gap-4"
      >
        <TabsList>
          <TabsTab value="predictions">{intl.formatMessage({ id: 'foresight.tabs.predictions' })}</TabsTab>
          <TabsTab value="beliefs">{intl.formatMessage({ id: 'foresight.tabs.beliefs' })}</TabsTab>
        </TabsList>

        <TabsPanel value="predictions" className="space-y-4">
          {loading ? (
            <CollectionPageState state="loading" />
          ) : loadError ? (
            <CollectionPageState
              state="error"
              icon={Radar}
              title={intl.formatMessage({ id: 'foresight.error.title' })}
              description={loadError}
              action={
                <Button variant="outline" size="sm" onClick={() => load()}>
                  {intl.formatMessage({ id: 'foresight.error.retry' })}
                </Button>
              }
            />
          ) : summaries.length === 0 ? (
            <CollectionPageState
              state="empty"
              icon={Radar}
              title={intl.formatMessage({ id: 'forward.empty.title' })}
              description={intl.formatMessage({ id: 'foresight.empty.desc' })}
            />
          ) : (
            <>
              <LoopStrip summaries={summaries} />

              <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
                {summaries.map((s) => (
                  <Card key={s.agent_id} data-size="sm">
                    <CardContent className="space-y-3">
                      <div className="flex items-center gap-2">
                        <ActorAvatar actorType="agent" size="sm" name={s.agent_id} />
                        <h3 className="truncate text-sm font-medium text-foreground">{s.agent_id}</h3>
                        {s.last_settled_at && (
                          <span className="ml-auto font-mono text-xs tabular-nums text-muted-foreground">
                            {timeAgo(s.last_settled_at)}
                          </span>
                        )}
                      </div>
                      <div className="grid grid-cols-3 gap-2">
                        <div className="text-center">
                          <p className="font-mono text-lg font-medium tabular-nums text-foreground">{s.total}</p>
                          <p className="text-xs text-muted-foreground">
                            {intl.formatMessage({ id: 'forward.metric.total' })}
                          </p>
                        </div>
                        <div className="text-center">
                          <p className="font-mono text-lg font-medium tabular-nums text-foreground">{s.settled}</p>
                          <p className="text-xs text-muted-foreground">
                            {intl.formatMessage({ id: 'forward.metric.settled' })}
                          </p>
                        </div>
                        <div className="text-center">
                          <p className="font-mono text-lg font-medium tabular-nums text-foreground">
                            {s.avg_brier != null ? s.avg_brier.toFixed(2) : '—'}
                          </p>
                          <p className="text-xs text-muted-foreground">
                            {intl.formatMessage({ id: 'forward.metric.brier' })}
                          </p>
                        </div>
                      </div>
                      <CategoryDistribution categories={s.categories} settled={s.settled} />
                    </CardContent>
                  </Card>
                ))}
              </div>

              {selectedAgent && (
                <div className="grid gap-4 lg:grid-cols-2">
                  <CalibrationCard agentId={selectedAgent} />
                  <StatesCard agentId={selectedAgent} />
                </div>
              )}
              {!selectedAgent && <StatesCard agentId="" />}

              {rows.length > 0 && (
                <section className="space-y-2">
                  <h2 className="text-sm font-medium text-foreground">
                    {intl.formatMessage({ id: 'forward.recent.title' })}
                  </h2>
                  <Card data-size="sm">
                    <CardContent className="divide-y divide-surface-border">
                      {rows.map((r) => {
                        const label = (id: string | null | undefined, prefix: string) =>
                          id
                            ? intl.formatMessage({ id: `${prefix}.${id}`, defaultMessage: id })
                            : null;
                        const exp = label(r.expected_outcome, 'foresight.outcome');
                        const obs = label(r.observed_outcome, 'foresight.outcome');
                        // accept/accepted etc. share display labels — compare the
                        // rendered labels, not the raw enum tokens.
                        const outcomeHit = exp != null && obs != null && exp === obs;
                        return (
                          <button
                            key={r.prediction_id}
                            type="button"
                            onClick={() => setChainTask(r.task_id)}
                            className="flex w-full flex-wrap items-center gap-x-3 gap-y-1 py-2 text-left text-xs hover:bg-secondary/50"
                          >
                            <span className="font-mono tabular-nums text-muted-foreground">
                              {timeAgo(r.settled_at ?? r.created_at)}
                            </span>
                            <span className="max-w-[18rem] truncate text-foreground" title={r.task_id}>
                              {r.task_title || r.task_id.slice(0, 8)} · R{r.round}
                            </span>
                            {exp && (
                              <span className="text-muted-foreground">
                                {intl.formatMessage(
                                  { id: 'foresight.recent.pv' },
                                  { expected: exp, observed: obs ?? intl.formatMessage({ id: 'forward.pending' }) },
                                )}
                                {obs != null && (
                                  <span
                                    className={
                                      outcomeHit
                                        ? 'ml-1 text-emerald-600 dark:text-emerald-400'
                                        : 'ml-1 text-rose-600 dark:text-rose-400'
                                    }
                                  >
                                    {outcomeHit ? '✓' : '✗'}
                                  </span>
                                )}
                              </span>
                            )}
                            <span className={`ml-auto ${categoryTone(r.category)}`}>
                              {r.settled_at == null
                                ? intl.formatMessage({ id: 'forward.pending' })
                                : r.category
                                  ? intl.formatMessage({
                                      id: `forward.category.${r.category}`,
                                      defaultMessage: r.category,
                                    })
                                  : '—'}
                            </span>
                            {r.brier != null && (
                              <span className="font-mono tabular-nums text-muted-foreground">
                                {r.brier.toFixed(2)}
                              </span>
                            )}
                          </button>
                        );
                      })}
                    </CardContent>
                  </Card>
                  <p className="text-xs text-muted-foreground">
                    {intl.formatMessage({ id: 'foresight.recent.hint' })}
                  </p>
                </section>
              )}

              {windowCap > 0 && windowScanned >= windowCap && (
                <p className="text-xs text-muted-foreground">
                  {intl.formatMessage({ id: 'forward.window.note' }, { cap: windowCap })}
                </p>
              )}
            </>
          )}
        </TabsPanel>

        <TabsPanel value="beliefs">
          <BeliefTab agentId={selectedAgent} />
        </TabsPanel>
      </Tabs>

      <ChainDialog taskId={chainTask} onClose={() => setChainTask(null)} />
    </div>
  );
}
