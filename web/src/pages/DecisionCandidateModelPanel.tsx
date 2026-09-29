import { useState } from 'react';
import { Badge, Button, Card, CardContent, Input } from '@/components/mds';
import { decisionApi, type DecisionCandidateModel, type DecisionCandidateRun,
  type DecisionCandidateScore } from '@/lib/decision-api';

type Translate = (id: string, values?: Record<string, string | number>) => string;
type Scope = { tenant_id: string; acl: string };
const digest = (value: string) => /^[a-f0-9]{64}$/.test(value);
const decimal = (value: string | null | undefined) => typeof value === 'string' && /^\d+$/.test(value);
const signedDecimal = (value: string | null | undefined) => typeof value === 'string' && /^-?\d+$/.test(value);
const lineageTotal = (values: string[], total: number) => Number.isSafeInteger(total)
  && total >= values.length;

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function verifyCandidate(candidate: DecisionCandidateModel, id: string, screenHash?: string): boolean {
  return candidate.id === id && candidate.model.version === id
    && candidate.status === 'simulation_only_model_candidate'
    && (!screenHash || candidate.screen_replay_hash === screenHash)
    && digest(candidate.record_sha256) && digest(candidate.screen_replay_hash)
    && candidate.source_version_hashes.length <= 16
    && candidate.source_version_hashes.every(digest)
    && lineageTotal(candidate.source_version_hashes, candidate.source_version_hashes_total)
    && candidate.queue_id.length > 0;
}

function verifyRun(run: DecisionCandidateRun, candidate: DecisionCandidateModel | null,
  expectedHash?: string, target?: string, scenario?: string): boolean {
  return run.status === 'exploratory_model_assumption_sensitivity'
    && digest(run.replay_hash) && digest(run.record_sha256)
    && digest(run.candidate_record_sha256) && digest(run.target_snapshot_sha256)
    && digest(run.scenario_sha256) && digest(run.parent.replay_hash) && digest(run.candidate.replay_hash)
    && run.source_version_hashes.length <= 16 && run.source_version_hashes.every(digest)
    && lineageTotal(run.source_version_hashes, run.source_version_hashes_total)
    && [run.parent, run.candidate].every((item) => decimal(item.total_arrivals)
      && decimal(item.total_resolved) && decimal(item.final_backlog)
      && decimal(item.total_resolved_within_sla) && decimal(item.total_staff_cost_cents)
      && typeof item.engine_matches_current === 'boolean')
    && signedDecimal(run.final_backlog_delta) && signedDecimal(run.resolved_within_sla_delta)
    && run.parent.total_arrivals === run.candidate.total_arrivals
    && BigInt(run.final_backlog_delta)
      === BigInt(run.candidate.final_backlog) - BigInt(run.parent.final_backlog)
    && BigInt(run.resolved_within_sla_delta)
      === BigInt(run.candidate.total_resolved_within_sla) - BigInt(run.parent.total_resolved_within_sla)
    && (!expectedHash || run.replay_hash === expectedHash)
    && (!target || run.target_snapshot_id === target) && (!scenario || run.scenario_id === scenario)
    && (!candidate || (run.candidate_id === candidate.id
      && run.candidate_record_sha256 === candidate.record_sha256
      && run.parent.model_version === candidate.parent_model_version
      && run.candidate.model_version === candidate.model.version));
}

function verifyScore(score: DecisionCandidateScore, expectedHash?: string,
  runHash?: string, outcomeId?: string): boolean {
  const errors = [score.parent, score.candidate];
  return score.status === 'exploratory_locally_timed_forecast_diagnostic'
    && digest(score.replay_hash) && digest(score.record_sha256)
    && digest(score.comparison_run_hash) && digest(score.comparison_run_sha256)
    && digest(score.outcome_record_sha256)
    && (score.ticket_source_sha256 === null || digest(score.ticket_source_sha256))
    && score.source_version_hashes.length <= 16 && score.source_version_hashes.every(digest)
    && lineageTotal(score.source_version_hashes, score.source_version_hashes_total)
    && (!expectedHash || score.replay_hash === expectedHash)
    && (!runHash || score.comparison_run_hash === runHash)
    && (!outcomeId || score.outcome_id === outcomeId)
    && errors.every((item) => Number.isSafeInteger(item.days) && item.days > 0
      && decimal(item.arrivals_abs_error_sum) && decimal(item.resolved_abs_error_sum)
      && decimal(item.backlog_abs_error_sum)
      && (item.resolved_within_sla_abs_error_sum == null || decimal(item.resolved_within_sla_abs_error_sum)))
    && score.parent.days === score.candidate.days
    && (score.parent.resolved_within_sla_abs_error_sum == null)
      === (score.candidate.resolved_within_sla_abs_error_sum == null)
    && score.parent.arrivals_abs_error_sum === score.candidate.arrivals_abs_error_sum
    && score.candidate_backlog_error_below_parent
      === (BigInt(score.candidate.backlog_abs_error_sum) < BigInt(score.parent.backlog_abs_error_sum))
    && score.candidate_resolution_error_below_parent
      === (BigInt(score.candidate.resolved_abs_error_sum) < BigInt(score.parent.resolved_abs_error_sum))
    && score.candidate_sla_error_below_parent === (score.parent.resolved_within_sla_abs_error_sum == null
      ? null : BigInt(score.candidate.resolved_within_sla_abs_error_sum!)
        < BigInt(score.parent.resolved_within_sla_abs_error_sum))
    && Number.isSafeInteger(score.candidate_created_at_unix)
    && Number.isSafeInteger(score.comparison_created_at_unix)
    && score.candidate_created_at_unix <= score.comparison_created_at_unix
    && score.comparison_created_at_unix < Date.parse(score.observed_window_start_utc) / 1000;
}

function formatted(value: number, locale: string): string {
  return new Intl.NumberFormat(locale).format(value);
}

function exactCount(value: string | null | undefined, locale: string, unavailable: string): string {
  return value == null ? unavailable : new Intl.NumberFormat(locale).format(BigInt(value));
}

function dateTime(value: string | number, locale: string): string {
  const date = typeof value === 'number' ? new Date(value * 1000) : new Date(value);
  return Number.isNaN(date.getTime()) ? String(value) : new Intl.DateTimeFormat(locale, {
    dateStyle: 'medium', timeStyle: 'short', timeZone: 'UTC',
  }).format(date);
}

export function DecisionCandidateModelPanel({ scope, blocked, onBusyChange, locale, t, screenPrefill = '' }: {
  scope: Scope; blocked: boolean; onBusyChange: (busy: boolean) => void; locale: string; t: Translate;
  screenPrefill?: string;
}) {
  const [candidateId, setCandidateId] = useState('');
  const [screenHash, setScreenHash] = useState(screenPrefill);
  const [candidate, setCandidate] = useState<DecisionCandidateModel | null>(null);
  const [targetSnapshotId, setTargetSnapshotId] = useState('');
  const [scenarioId, setScenarioId] = useState('');
  const [runHash, setRunHash] = useState('');
  const [run, setRun] = useState<DecisionCandidateRun | null>(null);
  const [outcomeId, setOutcomeId] = useState('');
  const [scoreHash, setScoreHash] = useState('');
  const [score, setScore] = useState<DecisionCandidateScore | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const disabled = blocked || busy;
  const clearScore = () => { setScore(null); setScoreHash(''); setError(''); };
  const clearRun = () => { setRun(null); setRunHash(''); clearScore(); };
  const clearCandidate = () => { setCandidate(null); clearRun(); };

  async function perform(action: () => Promise<void>) {
    setBusy(true); onBusyChange(true); setError('');
    try { await action(); }
    catch (cause) { setError(errorMessage(cause)); }
    finally { setBusy(false); onBusyChange(false); }
  }

  function createCandidate() {
    const id = candidateId.trim();
    const hash = screenHash.trim();
    if (!id || !hash) { setError(t('decisionLab.candidateMissing')); return; }
    clearCandidate();
    void perform(async () => {
      const { candidate: next } = await decisionApi.createCandidateModel({ ...scope,
        candidate_id: id, screen_replay_hash: hash });
      if (!verifyCandidate(next, id, hash)) throw new Error(t('decisionLab.candidateMismatch'));
      setCandidate(next);
    });
  }

  function loadCandidate() {
    const id = candidateId.trim();
    if (!id) { setError(t('decisionLab.candidateMissing')); return; }
    clearCandidate();
    void perform(async () => {
      const { candidate: next } = await decisionApi.loadCandidateModel({ ...scope, candidate_id: id });
      if (!verifyCandidate(next, id)) throw new Error(t('decisionLab.candidateMismatch'));
      setCandidate(next); setScreenHash(next.screen_replay_hash);
    });
  }

  function compareCandidate() {
    const target = targetSnapshotId.trim();
    const scenario = scenarioId.trim();
    if (!candidate || !target || !scenario) { setError(t('decisionLab.candidateMissing')); return; }
    clearRun();
    void perform(async () => {
      const { run: next } = await decisionApi.compareCandidateModel({ ...scope, candidate_id: candidate.id,
        target_snapshot_id: target, scenario_id: scenario });
      if (!verifyRun(next, candidate, undefined, target, scenario)) throw new Error(t('decisionLab.candidateMismatch'));
      setRun(next); setRunHash(next.replay_hash);
    });
  }

  function loadRun() {
    const hash = runHash.trim();
    if (!hash) { setError(t('decisionLab.candidateMissing')); return; }
    setRun(null); clearScore();
    void perform(async () => {
      const { run: next } = await decisionApi.loadCandidateRun({ ...scope, replay_hash: hash });
      if (!verifyRun(next, candidate, hash, targetSnapshotId.trim() || undefined, scenarioId.trim() || undefined)
        || (candidateId.trim() && next.candidate_id !== candidateId.trim())) {
        throw new Error(t('decisionLab.candidateMismatch'));
      }
      setRun(next); setCandidateId(next.candidate_id);
      setTargetSnapshotId(next.target_snapshot_id); setScenarioId(next.scenario_id);
    });
  }

  function scoreCandidate() {
    const outcome = outcomeId.trim();
    if (!run || !outcome) { setError(t('decisionLab.candidateMissing')); return; }
    clearScore();
    void perform(async () => {
      const { score: next } = await decisionApi.scoreCandidateModel({ ...scope,
        comparison_run_hash: run.replay_hash, outcome_id: outcome });
      if (!verifyScore(next, undefined, run.replay_hash, outcome)
        || next.comparison_run_sha256 !== run.record_sha256) {
        throw new Error(t('decisionLab.candidateMismatch'));
      }
      setScore(next); setScoreHash(next.replay_hash);
    });
  }

  function loadScore() {
    const hash = scoreHash.trim();
    if (!hash) { setError(t('decisionLab.candidateMissing')); return; }
    setScore(null);
    void perform(async () => {
      const { score: next } = await decisionApi.loadCandidateScore({ ...scope, replay_hash: hash });
      if (!verifyScore(next, hash, run?.replay_hash ?? (runHash.trim() || undefined), outcomeId.trim() || undefined)) {
        throw new Error(t('decisionLab.candidateMismatch'));
      }
      const { run: linked } = await decisionApi.loadCandidateRun({ ...scope, replay_hash: next.comparison_run_hash });
      if (!verifyRun(linked, candidate, next.comparison_run_hash,
        targetSnapshotId.trim() || undefined, scenarioId.trim() || undefined)
        || next.comparison_run_sha256 !== linked.record_sha256
        || (candidateId.trim() && linked.candidate_id !== candidateId.trim())) {
        throw new Error(t('decisionLab.candidateMismatch'));
      }
      setRun(linked); setRunHash(linked.replay_hash);
      setCandidateId(linked.candidate_id); setTargetSnapshotId(linked.target_snapshot_id);
      setScenarioId(linked.scenario_id); setOutcomeId(next.outcome_id);
      setScore(next);
    });
  }

  const unavailable = t('decisionLab.eventUnavailable');
  return <Card><CardContent className="space-y-5" aria-label={t('decisionLab.candidateTitle')}>
    <div><h2 className="font-semibold">{t('decisionLab.candidateTitle')}</h2>
      <p className="mt-1 text-sm text-muted-foreground">{t('decisionLab.candidateHint')}</p></div>

    <section className="space-y-2 rounded border p-3" aria-label={t('decisionLab.candidateStage')}>
      <h3 className="font-medium">{t('decisionLab.candidateStage')}</h3>
      <div className="grid gap-2 sm:grid-cols-2">
        <label className="space-y-1 text-sm">{t('decisionLab.candidateId')}
          <Input aria-label={t('decisionLab.candidateId')} maxLength={128} disabled={disabled} value={candidateId}
            onChange={(event) => { setCandidateId(event.target.value); clearCandidate(); }} /></label>
        <label className="space-y-1 text-sm">{t('decisionLab.candidateScreenHash')}
          <Input aria-label={t('decisionLab.candidateScreenHash')} maxLength={128} disabled={disabled} value={screenHash}
            onChange={(event) => { setScreenHash(event.target.value); clearCandidate(); }} /></label>
      </div>
      <div className="flex flex-wrap gap-2"><Button disabled={disabled || !candidateId.trim() || !screenHash.trim()}
        onClick={createCandidate}>{t('decisionLab.candidateCreate')}</Button>
      <Button variant="outline" disabled={disabled || !candidateId.trim()} onClick={loadCandidate}>
        {t('decisionLab.candidateLoad')}</Button></div>
      {candidate && <div className="space-y-1 text-sm" aria-label={t('decisionLab.candidateLoaded')}>
        <Badge variant="secondary">{t('decisionLab.candidateSimulationOnly')}</Badge>
        <p>{candidate.queue_id} · {candidate.parent_model_version} → {candidate.model.version}</p>
        <p>{t('decisionLab.candidateCapacity')}: {formatted(candidate.model.service_capacity_per_agent_day, locale)} · {t('decisionLab.candidateSlaDays')}: {formatted(candidate.model.sla_days, locale)} · {t('decisionLab.candidateCost')}: {formatted(candidate.model.staff_cost_cents_per_agent_day, locale)}</p>
        <p>{t('decisionLab.candidateObservedThrough')}: {dateTime(candidate.observed_through_utc, locale)}</p>
        <p className="break-all font-mono text-xs">{t('decisionLab.candidateRecordSha')}: {candidate.record_sha256} · {t('decisionLab.candidateFit')}: {candidate.fit_id} · {t('decisionLab.candidateTrainingOutcome')}: {candidate.outcome_id}</p>
        <details className="text-xs"><summary>{t('decisionLab.candidateProvenance')}</summary>
          <p className="break-all">{t('decisionLab.candidateScreenHash')}: {candidate.screen_replay_hash}</p>
          <DigestList values={candidate.source_version_hashes} total={candidate.source_version_hashes_total} t={t} />
        </details>
        <Limitations values={candidate.limitations} t={t} />
      </div>}
    </section>

    <section className="space-y-2 rounded border p-3" aria-label={t('decisionLab.candidateCompareStage')}>
      <h3 className="font-medium">{t('decisionLab.candidateCompareStage')}</h3>
      <div className="grid gap-2 sm:grid-cols-3">
        <label className="space-y-1 text-sm">{t('decisionLab.candidateTargetSnapshot')}
          <Input aria-label={t('decisionLab.candidateTargetSnapshot')} maxLength={192} disabled={disabled} value={targetSnapshotId}
            onChange={(event) => { setTargetSnapshotId(event.target.value); clearRun(); }} /></label>
        <label className="space-y-1 text-sm">{t('decisionLab.candidateScenario')}
          <Input aria-label={t('decisionLab.candidateScenario')} maxLength={192} disabled={disabled} value={scenarioId}
            onChange={(event) => { setScenarioId(event.target.value); clearRun(); }} /></label>
        <label className="space-y-1 text-sm">{t('decisionLab.candidateRunHash')}
          <Input aria-label={t('decisionLab.candidateRunHash')} maxLength={128} disabled={disabled} value={runHash}
            onChange={(event) => { setRunHash(event.target.value); setRun(null); clearScore(); }} /></label>
      </div>
      <div className="flex flex-wrap gap-2"><Button disabled={disabled || !candidate || !targetSnapshotId.trim() || !scenarioId.trim()}
        onClick={compareCandidate}>{t('decisionLab.candidateCompare')}</Button>
      <Button variant="outline" disabled={disabled || !runHash.trim()} onClick={loadRun}>{t('decisionLab.candidateLoadRun')}</Button></div>
      {run && <div className="space-y-2 text-sm" aria-label={t('decisionLab.candidateRunLoaded')}>
        {/* Either simulated side can carry an older engine digest; say so
            rather than presenting the comparison as currently reproducible. */}
        {(!run.parent.engine_matches_current || !run.candidate.engine_matches_current)
          && <Badge variant="outline">{t('decisionLab.staleEngineBadge')}</Badge>}
        <p className="break-all font-mono text-xs">{t('decisionLab.candidateRunHash')}: {run.replay_hash} · {t('decisionLab.candidateRecordSha')}: {run.record_sha256}</p>
        <p>{t('decisionLab.candidateRunContext')}: {run.candidate_id} · {run.target_snapshot_id} · {run.scenario_id}</p>
        <details className="text-xs"><summary>{t('decisionLab.candidateProvenance')}</summary>
          <p className="break-all">{t('decisionLab.candidateRecordSha')}: {run.candidate_record_sha256}</p>
          <p className="break-all">{t('decisionLab.candidateTargetSnapshotSha')}: {run.target_snapshot_sha256}</p>
          <p className="break-all">{t('decisionLab.candidateScenarioSha')}: {run.scenario_sha256}</p>
          <DigestList values={run.source_version_hashes} total={run.source_version_hashes_total} t={t} />
        </details>
        <div className="overflow-x-auto"><table className="w-full min-w-[28rem] text-left text-xs">
          <thead><tr><th scope="col">{t('decisionLab.metric')}</th><th scope="col">{t('decisionLab.candidateParent')}</th><th scope="col">{t('decisionLab.candidateModel')}</th></tr></thead>
          <tbody>{([
            ['candidateFinalBacklog', run.parent.final_backlog, run.candidate.final_backlog],
            ['candidateResolvedSla', run.parent.total_resolved_within_sla, run.candidate.total_resolved_within_sla],
            ['candidateStaffCost', run.parent.total_staff_cost_cents, run.candidate.total_staff_cost_cents],
          ] as const).map(([key, parent, proposed]) => <tr key={key}><th scope="row">{t(`decisionLab.${key}`)}</th>
            <td className="tabular-nums">{exactCount(parent, locale, t('decisionLab.none'))}</td>
            <td className="tabular-nums">{exactCount(proposed, locale, t('decisionLab.none'))}</td></tr>)}</tbody>
        </table></div>
        <p>{t('decisionLab.candidateBacklogDelta')}: {exactCount(run.final_backlog_delta, locale, t('decisionLab.none'))} · {t('decisionLab.candidateSlaDelta')}: {exactCount(run.resolved_within_sla_delta, locale, t('decisionLab.none'))}</p>
        <Limitations values={run.limitations} t={t} />
      </div>}
    </section>

    <section className="space-y-2 rounded border p-3" aria-label={t('decisionLab.candidateScoreStage')}>
      <h3 className="font-medium">{t('decisionLab.candidateScoreStage')}</h3>
      <p className="text-xs text-muted-foreground">{t('decisionLab.candidateScoreHint')}</p>
      <div className="grid gap-2 sm:grid-cols-2">
        <label className="space-y-1 text-sm">{t('decisionLab.candidateOutcome')}
          <Input aria-label={t('decisionLab.candidateOutcome')} maxLength={192} disabled={disabled} value={outcomeId}
            onChange={(event) => { setOutcomeId(event.target.value); clearScore(); }} /></label>
        <label className="space-y-1 text-sm">{t('decisionLab.candidateScoreHash')}
          <Input aria-label={t('decisionLab.candidateScoreHash')} maxLength={128} disabled={disabled} value={scoreHash}
            onChange={(event) => { setScoreHash(event.target.value); setScore(null); }} /></label>
      </div>
      <div className="flex flex-wrap gap-2"><Button disabled={disabled || !run || !outcomeId.trim()} onClick={scoreCandidate}>
        {t('decisionLab.candidateScore')}</Button>
      <Button variant="outline" disabled={disabled || !scoreHash.trim()} onClick={loadScore}>{t('decisionLab.candidateLoadScore')}</Button></div>
      {score && <div className="space-y-2 text-sm" aria-label={t('decisionLab.candidateScoreLoaded')}>
        <p className="break-all font-mono text-xs">{t('decisionLab.candidateScoreHash')}: {score.replay_hash} · {t('decisionLab.candidateRecordSha')}: {score.record_sha256}</p>
        <p className="break-all text-xs font-medium">{t('decisionLab.candidateScoreIdentity')}: {run?.candidate_id} · {score.comparison_run_hash} · {score.outcome_id}</p>
        <p>{t('decisionLab.candidateScoreWindow')}: {dateTime(score.observed_window_start_utc, locale)} · {formatted(score.parent.days, locale)} {t('decisionLab.candidateDays')}</p>
        <p>{t('decisionLab.candidateLocalTiming')}: {dateTime(score.candidate_created_at_unix, locale)} → {dateTime(score.comparison_created_at_unix, locale)}</p>
        <details className="text-xs"><summary>{t('decisionLab.candidateProvenance')}</summary>
          <p className="break-all">{t('decisionLab.candidateRunHash')}: {score.comparison_run_hash} · {t('decisionLab.candidateComparisonSha')}: {score.comparison_run_sha256}</p>
          <p className="break-all">{t('decisionLab.candidateOutcome')}: {score.outcome_id} · {t('decisionLab.candidateOutcomeSha')}: {score.outcome_record_sha256}</p>
          <DigestList values={score.source_version_hashes} total={score.source_version_hashes_total} t={t} />
        </details>
        <div className="overflow-x-auto"><table className="w-full min-w-[28rem] text-left text-xs">
          <thead><tr><th scope="col">{t('decisionLab.metric')}</th><th scope="col">{t('decisionLab.candidateParent')}</th><th scope="col">{t('decisionLab.candidateModel')}</th></tr></thead>
          <tbody>{([
            ['candidateArrivalError', score.parent.arrivals_abs_error_sum, score.candidate.arrivals_abs_error_sum],
            ['candidateResolvedError', score.parent.resolved_abs_error_sum, score.candidate.resolved_abs_error_sum],
            ['candidateBacklogError', score.parent.backlog_abs_error_sum, score.candidate.backlog_abs_error_sum],
            ['candidateSlaError', score.parent.resolved_within_sla_abs_error_sum, score.candidate.resolved_within_sla_abs_error_sum],
          ] as const).map(([key, parent, proposed]) => <tr key={key}><th scope="row">{t(`decisionLab.${key}`)}</th>
            <td className="tabular-nums">{exactCount(parent, locale, unavailable)}</td>
            <td className="tabular-nums">{exactCount(proposed, locale, unavailable)}</td></tr>)}</tbody>
        </table></div>
        <p>{t('decisionLab.candidateBacklogWin')}: {t(score.candidate_backlog_error_below_parent ? 'decisionLab.yes' : 'decisionLab.no')}
          {' · '}{t('decisionLab.candidateResolutionWin')}: {t(score.candidate_resolution_error_below_parent ? 'decisionLab.yes' : 'decisionLab.no')}
          {' · '}{t('decisionLab.candidateSlaWin')}: {score.candidate_sla_error_below_parent === null ? unavailable
            : t(score.candidate_sla_error_below_parent ? 'decisionLab.yes' : 'decisionLab.no')}</p>
        <Badge variant="outline">{t(score.ticket_source_sha256 ? 'decisionLab.candidateTicketBacked' : 'decisionLab.candidateAggregateLabels')}</Badge>
        {score.ticket_source_sha256 && <p className="break-all font-mono text-xs">{t('decisionLab.uploadSourceSha')}: {score.ticket_source_sha256}</p>}
        {score.ticket_label_engine_sha256 && <p className="break-all font-mono text-xs">{t('decisionLab.candidateTicketLabelEngineSha')}: {score.ticket_label_engine_sha256}</p>}
        {score.ticket_source_retention_until_utc && <p className="text-xs">{t('decisionLab.candidateTicketRetention')}: {dateTime(score.ticket_source_retention_until_utc, locale)}</p>}
        <Limitations values={score.limitations} t={t} />
      </div>}
    </section>
    {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
  </CardContent></Card>;
}

function Limitations({ values, t }: { values: string[]; t: Translate }) {
  return values.length > 0 && <div className="text-xs text-muted-foreground"><p>{t('decisionLab.limitations')}</p>
    <ul className="list-disc pl-5">{values.map((value, index) => <li key={index}>{value}</li>)}</ul>
  </div>;
}

function DigestList({ values, total, t }: { values: string[]; total: number; t: Translate }) {
  // The list is capped at 16 server-side. Showing "16 / 42" keeps a truncated
  // preview from reading like the complete provenance of the receipt.
  const shown = total > values.length ? ` (${values.length} / ${total})` : '';
  return <p className="break-all">{t('decisionLab.candidateSourceVersions')}{shown}: {values.length
    ? values.join(' · ') : t('decisionLab.none')}</p>;
}
