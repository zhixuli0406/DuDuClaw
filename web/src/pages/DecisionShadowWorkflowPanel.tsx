import { useState } from 'react';
import { Badge, Button, Card, CardContent, Input } from '@/components/mds';
import { decisionApi, type DecisionShadowAssessment, type DecisionShadowErrorSums,
  type DecisionShadowForecast, type DecisionShadowPolicy, type DecisionShadowReview,
  type DecisionShadowScore, type DecisionShadowScreen } from '@/lib/decision-api';

type Scope = { tenant_id: string; acl: string };
type Translate = (id: string, values?: Record<string, string | number>) => string;
const digest = (value: unknown): value is string => typeof value === 'string' && /^[a-f0-9]{64}$/.test(value);
const count = (value: unknown, minimum = 0): value is number => Number.isSafeInteger(value) && (value as number) >= minimum;
const decimal = (value: unknown): value is string => typeof value === 'string' && /^\d+$/.test(value);
const utc = (value: string) => /^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d+)?(?:Z|\+00:00)$/.test(value)
  && Number.isFinite(Date.parse(value));
const utcMidnight = (value: string) => utc(value) && new Date(value).toISOString().slice(11, 23) === '00:00:00.000';
const sameInstant = (left: string, right: string) => utc(left) && utc(right) && Date.parse(left) === Date.parse(right);
const failure = (error: unknown) => error instanceof Error ? error.message : String(error);
const numeric = (value: string, minimum = 0) => {
  const parsed = Number(value);
  return count(parsed, minimum) && value.trim() !== '' ? parsed : null;
};
const validSums = (value: DecisionShadowErrorSums | null): boolean => value === null
  || [value.arrivals, value.backlog, value.no_change_backlog,
    value.seasonal_naive_backlog, value.mean_change_backlog].every(decimal);
const belowBaselines = (value: DecisionShadowErrorSums): boolean =>
  [value.no_change_backlog, value.seasonal_naive_backlog, value.mean_change_backlog]
    .every((baseline) => BigInt(value.backlog) < BigInt(baseline));

function validPolicy(value: DecisionShadowPolicy, id?: string): boolean {
  return (!id || value.id === id) && !!value.id && !!value.source_lineage
    && (value.queue_id === null || !!value.queue_id)
    && utcMidnight(value.effective_from_utc) && utcMidnight(value.effective_until_utc)
    && Date.parse(value.effective_from_utc) < Date.parse(value.effective_until_utc)
    && Date.parse(value.effective_until_utc) - Date.parse(value.effective_from_utc) <= 366 * 86_400_000
    && count(value.issue_deadline_seconds, 1) && value.issue_deadline_seconds <= 3600
    && count(value.min_training_days, 7) && value.min_training_days <= 366
    && count(value.min_saturated_days, 1) && value.min_saturated_days <= value.min_training_days
    && digest(value.calibration_engine_sha256)
    && digest(value.record_sha256) && count(value.registered_at)
    && value.registered_at * 1000 < Date.parse(value.effective_from_utc)
    && Array.isArray(value.limitations);
}

function validForecast(value: DecisionShadowForecast, policy: DecisionShadowPolicy,
  id?: string, target?: string): boolean {
  return (!id || value.id === id) && (!target || sameInstant(value.target_day_utc, target))
    && value.policy_id === policy.id && value.policy_sha256 === policy.record_sha256
    && value.source_lineage === policy.source_lineage && value.queue_id === policy.queue_id
    && value.calibration_engine_sha256 === policy.calibration_engine_sha256
    && value.min_saturated_days === policy.min_saturated_days
    && !!value.training_artifact_id && digest(value.training_sha256) && digest(value.record_sha256)
    && utcMidnight(value.training_window_start_utc) && utcMidnight(value.target_day_utc)
    && count(value.committed_at)
    && Date.parse(value.training_window_start_utc) < Date.parse(value.target_day_utc)
    && Date.parse(value.target_day_utc) >= Date.parse(policy.effective_from_utc)
    && Date.parse(value.target_day_utc) < Date.parse(policy.effective_until_utc)
    && value.committed_at * 1000 >= Date.parse(value.target_day_utc)
    && value.committed_at * 1000 <= Date.parse(value.target_day_utc)
      + policy.issue_deadline_seconds * 1000
    && decimal(value.known.opening_backlog) && count(value.known.planned_agents)
    && count(value.known.planned_fixed_extra_capacity)
    && count(value.forecast.predicted_arrivals)
    && [value.forecast.predicted_backlog_end, value.forecast.no_change_backlog_end,
      value.forecast.seasonal_naive_backlog_end, value.forecast.mean_change_backlog_end].every(decimal)
    && count(value.forecast.fitted_capacity_per_agent)
    && count(value.forecast.training_days, policy.min_training_days)
    && Array.isArray(value.limitations);
}

function validScore(value: DecisionShadowScore, forecast: DecisionShadowForecast, id?: string): boolean {
  return (!id || value.id === id) && value.forecast_id === forecast.id
    && value.forecast_sha256 === forecast.record_sha256 && !!value.observation_artifact_id
    && digest(value.observation_sha256) && digest(value.record_sha256) && count(value.scored_at)
    && value.scored_at * 1000 >= Date.parse(forecast.target_day_utc) + 86_400_000
    && [value.arrivals_abs_error, value.backlog_abs_error, value.no_change_abs_error,
      value.seasonal_naive_abs_error, value.mean_change_abs_error].every(decimal)
    && Array.isArray(value.limitations);
}

function validAssessment(value: DecisionShadowAssessment, policy: DecisionShadowPolicy): boolean {
  const interval = value.fixed_prefix_interval;
  return value.policy_id === policy.id && value.policy_sha256 === policy.record_sha256
    && value.source_lineage === policy.source_lineage && value.queue_id === policy.queue_id
    && utc(value.assessed_at_utc) && count(value.due_days) && count(value.scored_days)
    && value.scored_days <= value.due_days && count(value.corrected_days)
    && (!value.complete || value.scored_days === value.due_days)
    && validSums(value.error_sums) && validSums(value.recent_7_day_error_sums)
    && value.backlog_abs_error_below_each_baseline === (value.error_sums === null
      ? null : belowBaselines(value.error_sums))
    && value.recent_7_day_backlog_abs_error_below_each_baseline === (value.recent_7_day_error_sums === null
      ? null : belowBaselines(value.recent_7_day_error_sums))
    && (interval === null || (!!interval.method && count(interval.target_coverage_basis_points)
      && count(interval.observed_coverage_basis_points) && count(interval.calibration_points)
      && count(interval.evaluated_points) && count(interval.recent_miss_count)
      && count(interval.recent_window_points) && count(interval.covered_points)
      && interval.covered_points <= interval.evaluated_points
      && interval.target_coverage_basis_points <= 10_000
      && interval.observed_coverage_basis_points <= 10_000))
    && Object.values(value.day_status_counts).every((item) => count(item))
    && Object.values(value.day_status_counts).reduce((sum, item) => sum + item, 0) === value.due_days
    && Array.isArray(value.limitations);
}

function validScreen(value: DecisionShadowScreen, policy: DecisionShadowPolicy, saved: boolean,
  hash?: string, criteria?: { min_complete_days: number; min_fixed_coverage_bps: number }): boolean {
  return (!hash || value.replay_hash === hash) && digest(value.replay_hash)
    && value.policy_id === policy.id && value.policy_sha256 === policy.record_sha256
    && (saved ? digest(value.record_sha256) : value.record_sha256 === null)
    && value.source_version_hashes.length <= 16 && value.source_version_hashes.every(digest)
    && Number.isSafeInteger(value.source_version_hashes_total)
    && value.source_version_hashes_total >= value.source_version_hashes.length
    && (!saved ? value.source_version_hashes.length === 0 : true)
    && !!value.report.status && digest(value.report.screen_engine_sha256)
    && count(value.report.criteria.min_complete_days, 21)
    && value.report.criteria.min_complete_days <= 366
    && count(value.report.criteria.min_fixed_coverage_bps)
    && value.report.criteria.min_fixed_coverage_bps <= 10_000
    && (!criteria || (value.report.criteria.min_complete_days === criteria.min_complete_days
      && value.report.criteria.min_fixed_coverage_bps === criteria.min_fixed_coverage_bps))
    && validAssessment(value.report.assessment, policy)
    && (value.report.coverage_evidence === null || (
      count(value.report.coverage_evidence.covered_days)
      && count(value.report.coverage_evidence.evaluated_days)
      && count(value.report.coverage_evidence.minimum_covered_days)
      && value.report.coverage_evidence.covered_days <= value.report.coverage_evidence.evaluated_days))
    && Array.isArray(value.report.failed_checks) && Array.isArray(value.report.limitations)
    && (!value.report.eligible_for_human_review || (
      value.report.failed_checks.length === 0
      && value.report.assessment.complete
      && value.report.assessment.due_days >= value.report.criteria.min_complete_days
      && value.report.assessment.backlog_abs_error_below_each_baseline === true
      && value.report.assessment.recent_7_day_backlog_abs_error_below_each_baseline === true
      && value.report.assessment.fixed_prefix_interval !== null
      && value.report.assessment.fixed_prefix_interval.drift_signal === false
      && value.report.assessment.fixed_prefix_interval.method === 'fixed_prefix_absolute_residual_rank90_v1'
      && value.report.assessment.fixed_prefix_interval.target_coverage_basis_points === 9000
      && value.report.assessment.fixed_prefix_interval.calibration_points === 14
      && value.report.assessment.fixed_prefix_interval.evaluated_points + 14
        === value.report.assessment.due_days
      && value.report.assessment.fixed_prefix_interval.evaluated_points >= 7
      && value.report.assessment.fixed_prefix_interval.evaluated_points <= 352
      && value.report.assessment.fixed_prefix_interval.observed_coverage_basis_points
        === Math.floor(value.report.assessment.fixed_prefix_interval.covered_points * 10000
          / value.report.assessment.fixed_prefix_interval.evaluated_points)
      && value.report.assessment.fixed_prefix_interval.observed_coverage_basis_points
        >= value.report.criteria.min_fixed_coverage_bps
      && value.report.coverage_evidence !== null
      && value.report.coverage_evidence.covered_days
        === value.report.assessment.fixed_prefix_interval.covered_points
      && value.report.coverage_evidence.evaluated_days
        === value.report.assessment.fixed_prefix_interval.evaluated_points
      && value.report.coverage_evidence.minimum_covered_days
        === Math.ceil(value.report.criteria.min_fixed_coverage_bps
          * value.report.coverage_evidence.evaluated_days / 10000)
      && value.report.coverage_evidence.covered_days >= value.report.coverage_evidence.minimum_covered_days));
}

function validReview(value: DecisionShadowReview, screen: DecisionShadowScreen, approvalId?: string): boolean {
  return !!value.link.approval_id && !!value.link.agent_id
    && (!approvalId || value.link.approval_id === approvalId)
    && value.link.replay_hash === screen.replay_hash
    && value.link.screen_record_sha256 === screen.record_sha256
    && value.link.policy_id === screen.policy_id && value.link.policy_sha256 === screen.policy_sha256
    && ['pending', 'approved', 'denied', 'expired'].includes(value.status)
    && utc(value.expires_at_utc);
}

function dateTime(value: string | number, locale: string): string {
  const date = typeof value === 'number' ? new Date(value * 1000) : new Date(value);
  return !Number.isNaN(date.getTime()) ? new Intl.DateTimeFormat(locale, { dateStyle: 'medium', timeStyle: 'short',
    timeZone: 'UTC' }).format(date) : String(value);
}

function SourceInput({ artifact, source, retention, setArtifact, setSource, setRetention,
  disabled, t, kind }: { artifact: string; source: string; retention: string;
  setArtifact: (value: string) => void; setSource: (value: string) => void;
  setRetention: (value: string) => void; disabled: boolean; t: Translate; kind: 'training' | 'observation' }) {
  return <div className="grid gap-2 sm:grid-cols-2">
    <label className="space-y-1 text-sm">{t(`decisionLab.shadowFlow.${kind}Artifact`)}
      <Input aria-label={t(`decisionLab.shadowFlow.${kind}Artifact`)} value={artifact} maxLength={128}
        disabled={disabled} onChange={(event) => setArtifact(event.target.value)} /></label>
    <label className="space-y-1 text-sm">{t('decisionLab.shadowFlow.retention')}
      <Input aria-label={`${kind} ${t('decisionLab.shadowFlow.retention')}`} value={retention}
        placeholder="2099-01-01T00:00:00Z" disabled={disabled}
        onChange={(event) => setRetention(event.target.value)} /></label>
    <label className="space-y-1 text-sm sm:col-span-2">{t(`decisionLab.shadowFlow.${kind}Json`)}
      <textarea aria-label={t(`decisionLab.shadowFlow.${kind}Json`)} className="min-h-20 w-full rounded border bg-background p-2 font-mono text-xs"
        value={source} maxLength={12_000} disabled={disabled}
        onChange={(event) => setSource(event.target.value)} /></label>
    <p className="text-xs text-muted-foreground sm:col-span-2">{t('decisionLab.shadowFlow.sourceHint')}</p>
    <p className="text-xs text-muted-foreground sm:col-span-2">{t(`decisionLab.shadowFlow.${kind}Shape`)}</p>
  </div>;
}

function Limitations({ values, t }: { values: string[]; t: Translate }) {
  return <details className="text-xs"><summary>{t('decisionLab.shadowFlow.limitations')}</summary>
    <ul className="list-disc pl-5">{values.map((value, index) => <li key={`${index}:${value}`}>{value}</li>)}</ul>
  </details>;
}

function Sums({ values, locale, t }: { values: DecisionShadowErrorSums | null; locale: string; t: Translate }) {
  if (!values) return <p>{t('decisionLab.shadowFlow.unavailable')}</p>;
  const format = (value: string) => new Intl.NumberFormat(locale).format(BigInt(value));
  return <p>{t('decisionLab.shadowFlow.arrivalsError')}: {format(values.arrivals)} ·
    {t('decisionLab.shadowFlow.backlogError')}: {format(values.backlog)} ·
    {t('decisionLab.shadowFlow.baselines')}: {format(values.no_change_backlog)} / {format(values.seasonal_naive_backlog)} / {format(values.mean_change_backlog)}</p>;
}

export function DecisionShadowWorkflowPanel({ scope, blocked, onBusyChange, locale, t }: {
  scope: Scope; blocked: boolean; onBusyChange: (busy: boolean) => void; locale: string; t: Translate;
}) {
  const [policyId, setPolicyId] = useState('');
  const [lineage, setLineage] = useState('');
  const [queue, setQueue] = useState('');
  const [from, setFrom] = useState('');
  const [until, setUntil] = useState('');
  const [deadline, setDeadline] = useState('3600');
  const [trainingDays, setTrainingDays] = useState('14');
  const [saturatedDays, setSaturatedDays] = useState('3');
  const [policy, setPolicy] = useState<DecisionShadowPolicy | null>(null);
  const [forecastId, setForecastId] = useState('');
  const [target, setTarget] = useState('');
  const [backlog, setBacklog] = useState('0');
  const [agents, setAgents] = useState('1');
  const [extra, setExtra] = useState('0');
  const [trainingArtifact, setTrainingArtifact] = useState('');
  const [trainingSource, setTrainingSource] = useState('');
  const [trainingRetention, setTrainingRetention] = useState('');
  const [forecast, setForecast] = useState<DecisionShadowForecast | null>(null);
  const [scoreId, setScoreId] = useState('');
  const [observationArtifact, setObservationArtifact] = useState('');
  const [observationSource, setObservationSource] = useState('');
  const [observationRetention, setObservationRetention] = useState('');
  const [score, setScore] = useState<DecisionShadowScore | null>(null);
  const [assessment, setAssessment] = useState<DecisionShadowAssessment | null>(null);
  const [minComplete, setMinComplete] = useState('21');
  const [minCoverage, setMinCoverage] = useState('8000');
  const [screenHash, setScreenHash] = useState('');
  const [reviewScreen, setReviewScreen] = useState<DecisionShadowScreen | null>(null);
  const [summary, setSummary] = useState('');
  const [approvalId, setApprovalId] = useState('');
  const [review, setReview] = useState<DecisionShadowReview | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const disabled = blocked || busy;
  const screenDays = numeric(minComplete, 21);
  const screenCoverage = numeric(minCoverage);
  const screenCriteriaValid = screenDays !== null && screenDays <= 366
    && screenCoverage !== null && screenCoverage <= 10_000;

  const clearReview = () => { setReview(null); setApprovalId(''); setError(''); };
  const clearScreen = () => { setReviewScreen(null); setScreenHash(''); clearReview(); };
  const clearAssessment = () => { setAssessment(null); clearScreen(); };
  const clearScore = () => { setScore(null); clearAssessment(); };
  const clearForecast = () => { setForecast(null); clearScore(); };
  const clearPolicy = () => { setPolicy(null); clearForecast(); };
  async function perform(action: () => Promise<void>) {
    setBusy(true); onBusyChange(true); setError('');
    try { await action(); } catch (cause) { setError(failure(cause)); }
    finally { setBusy(false); onBusyChange(false); }
  }
  function requireInput(ok: boolean): boolean {
    if (!ok) setError(t('decisionLab.shadowFlow.invalidInput'));
    return ok;
  }
  function sourceFields(artifact: string, source: string, retention: string,
    artifactKey: 'training_artifact_id' | 'observation_artifact_id',
    sourceKey: 'training_source_json' | 'observation_source_json') {
    const id = artifact.trim(); const raw = source.trim();
    if (!!id === !!raw || (raw && (!utc(retention) || new TextEncoder().encode(raw).length > 12_000))) {
      throw new Error(t('decisionLab.shadowFlow.sourceInvalid'));
    }
    if (raw) {
      try { JSON.parse(raw); } catch { throw new Error(t('decisionLab.shadowFlow.sourceInvalid')); }
    }
    return id ? { [artifactKey]: id } : { [sourceKey]: raw, retention_until_utc: retention };
  }
  function requestSize(input: object) {
    if (new TextEncoder().encode(JSON.stringify(input)).length > 16_000)
      throw new Error(t('decisionLab.shadowFlow.sourceTooLarge'));
  }

  function createPolicy() {
    const id = policyId.trim(); const d = numeric(deadline, 1);
    const train = numeric(trainingDays, 7); const sat = numeric(saturatedDays, 1);
    if (!requireInput(!!id && !!lineage.trim() && !!queue.trim() && utcMidnight(from) && utcMidnight(until)
      && Date.parse(from) < Date.parse(until)
      && Date.parse(until) - Date.parse(from) <= 366 * 86_400_000
      && d !== null && d <= 3600 && train !== null && train <= 366
      && sat !== null && sat <= train)) return;
    clearPolicy();
    void perform(async () => {
      const { policy: next } = await decisionApi.createShadowPolicy({ ...scope, policy_id: id,
        source_lineage: lineage.trim(), queue_id: queue.trim(), effective_from_utc: from,
        effective_until_utc: until, issue_deadline_seconds: d!, min_training_days: train!, min_saturated_days: sat! });
      if (!validPolicy(next, id) || next.source_lineage !== lineage.trim() || next.queue_id !== queue.trim()
        || !sameInstant(next.effective_from_utc, from) || !sameInstant(next.effective_until_utc, until)
        || next.issue_deadline_seconds !== d || next.min_training_days !== train
        || next.min_saturated_days !== sat) throw new Error(t('decisionLab.shadowFlow.mismatch'));
      setPolicy(next);
    });
  }
  function loadPolicy() {
    const id = policyId.trim(); if (!requireInput(!!id)) return;
    clearPolicy();
    void perform(async () => {
      const { policy: next } = await decisionApi.loadShadowPolicy({ ...scope, policy_id: id });
      if (!validPolicy(next, id)) throw new Error(t('decisionLab.shadowFlow.mismatch'));
      setPolicy(next); setLineage(next.source_lineage); setQueue(next.queue_id ?? '');
      setFrom(next.effective_from_utc); setUntil(next.effective_until_utc);
      setDeadline(String(next.issue_deadline_seconds)); setTrainingDays(String(next.min_training_days));
      setSaturatedDays(String(next.min_saturated_days));
    });
  }
  function createForecast() {
    const id = forecastId.trim(); const opening = numeric(backlog);
    const planned = numeric(agents); const fixed = numeric(extra);
    if (!requireInput(!!policy && !!id && utcMidnight(target) && opening !== null
      && planned !== null && fixed !== null)) return;
    clearForecast();
    void perform(async () => {
      const source = sourceFields(trainingArtifact, trainingSource, trainingRetention,
        'training_artifact_id', 'training_source_json');
      const input = { ...scope, forecast_id: id, policy_id: policy!.id, target_day_utc: target,
        known: { opening_backlog: opening!, planned_agents: planned!, planned_fixed_extra_capacity: fixed! }, ...source };
      requestSize(input);
      const { forecast: next } = await decisionApi.createShadowForecast(input);
      if (!validForecast(next, policy!, id, target)
        || next.known.opening_backlog !== String(opening) || next.known.planned_agents !== planned
        || next.known.planned_fixed_extra_capacity !== fixed
        || (trainingArtifact.trim() && next.training_artifact_id !== trainingArtifact.trim()))
        throw new Error(t('decisionLab.shadowFlow.mismatch'));
      setForecast(next); setTrainingArtifact(next.training_artifact_id); setTrainingSource('');
      setTrainingRetention('');
    });
  }
  function loadForecast() {
    const id = forecastId.trim(); if (!requireInput(!!id)) return;
    clearForecast();
    void perform(async () => {
      const { forecast: next } = await decisionApi.loadShadowForecast({ ...scope, forecast_id: id });
      if (policyId.trim() && next.policy_id !== policyId.trim()) throw new Error(t('decisionLab.shadowFlow.mismatch'));
      const linked = policy ?? (await decisionApi.loadShadowPolicy({ ...scope, policy_id: next.policy_id })).policy;
      if (!validPolicy(linked, next.policy_id) || !validForecast(next, linked, id))
        throw new Error(t('decisionLab.shadowFlow.mismatch'));
      setPolicy(linked); setPolicyId(linked.id); setForecast(next); setTarget(next.target_day_utc);
      setTrainingArtifact(next.training_artifact_id); setBacklog(String(next.known.opening_backlog));
      setAgents(String(next.known.planned_agents)); setExtra(String(next.known.planned_fixed_extra_capacity));
    });
  }
  function createScore() {
    const id = scoreId.trim(); if (!requireInput(!!forecast && !!id)) return;
    clearScore();
    void perform(async () => {
      const source = sourceFields(observationArtifact, observationSource, observationRetention,
        'observation_artifact_id', 'observation_source_json');
      const input = { ...scope, score_id: id, forecast_id: forecast!.id, ...source };
      requestSize(input);
      const { score: next } = await decisionApi.createShadowScore(input);
      if (!validScore(next, forecast!, id)
        || (observationArtifact.trim() && next.observation_artifact_id !== observationArtifact.trim()))
        throw new Error(t('decisionLab.shadowFlow.mismatch'));
      setScore(next); setObservationArtifact(next.observation_artifact_id);
      setObservationSource(''); setObservationRetention('');
    });
  }
  function loadScore() {
    const id = scoreId.trim(); if (!requireInput(!!id)) return;
    clearScore();
    void perform(async () => {
      const { score: next } = await decisionApi.loadShadowScore({ ...scope, score_id: id });
      if (forecastId.trim() && next.forecast_id !== forecastId.trim()) throw new Error(t('decisionLab.shadowFlow.mismatch'));
      const linked = forecast ?? (await decisionApi.loadShadowForecast({ ...scope, forecast_id: next.forecast_id })).forecast;
      if (policyId.trim() && linked.policy_id !== policyId.trim()) throw new Error(t('decisionLab.shadowFlow.mismatch'));
      const linkedPolicy = policy ?? (await decisionApi.loadShadowPolicy({ ...scope, policy_id: linked.policy_id })).policy;
      if (!validPolicy(linkedPolicy, linked.policy_id) || !validForecast(linked, linkedPolicy, next.forecast_id)
        || !validScore(next, linked, id)) throw new Error(t('decisionLab.shadowFlow.mismatch'));
      setPolicy(linkedPolicy); setPolicyId(linkedPolicy.id); setForecast(linked); setForecastId(linked.id);
      setScore(next); setObservationArtifact(next.observation_artifact_id);
    });
  }
  function assess() {
    if (!requireInput(!!policy)) return;
    clearAssessment();
    void perform(async () => {
      const { assessment: next } = await decisionApi.assessShadowPolicy({ ...scope, policy_id: policy!.id });
      if (!validAssessment(next, policy!)) throw new Error(t('decisionLab.shadowFlow.mismatch'));
      setAssessment(next);
    });
  }
  function screenPolicy(save: boolean) {
    const days = screenDays; const coverage = screenCoverage;
    if (!requireInput(!!policy && screenCriteriaValid)) return;
    clearScreen();
    void perform(async () => {
      const input = { ...scope, policy_id: policy!.id,
        min_complete_days: days!, min_fixed_coverage_bps: coverage! };
      const { screen: next } = save ? await decisionApi.saveShadowScreen(input)
        : await decisionApi.evaluateShadowScreen(input);
      if (!validScreen(next, policy!, save, undefined,
        { min_complete_days: days!, min_fixed_coverage_bps: coverage! }))
        throw new Error(t('decisionLab.shadowFlow.mismatch'));
      setReviewScreen(next); setAssessment(next.report.assessment);
      if (save) setScreenHash(next.replay_hash);
    });
  }
  function loadScreen() {
    const hash = screenHash.trim(); if (!requireInput(digest(hash))) return;
    setReviewScreen(null); setAssessment(null); clearReview();
    void perform(async () => {
      const { screen: next } = await decisionApi.loadShadowScreen({ ...scope, replay_hash: hash });
      if (policyId.trim() && next.policy_id !== policyId.trim()) throw new Error(t('decisionLab.shadowFlow.mismatch'));
      const linked = policy ?? (await decisionApi.loadShadowPolicy({ ...scope, policy_id: next.policy_id })).policy;
      if (!validPolicy(linked, next.policy_id) || !validScreen(next, linked, true, hash))
        throw new Error(t('decisionLab.shadowFlow.mismatch'));
      setPolicy(linked); setPolicyId(linked.id); setReviewScreen(next);
      setAssessment(next.report.assessment);
      setMinComplete(String(next.report.criteria.min_complete_days));
      setMinCoverage(String(next.report.criteria.min_fixed_coverage_bps));
    });
  }
  function requestReview() {
    if (!requireInput(!!reviewScreen && digest(reviewScreen.record_sha256)
      && reviewScreen.report.eligible_for_human_review && !!summary.trim() && summary.trim().length <= 240)) return;
    clearReview();
    void perform(async () => {
      const { review: next } = await decisionApi.requestShadowReview({ ...scope,
        replay_hash: reviewScreen!.replay_hash, summary: summary.trim(), ttl_seconds: 3600 });
      if (!validReview(next, reviewScreen!)) throw new Error(t('decisionLab.shadowFlow.mismatch'));
      setReview(next); setApprovalId(next.link.approval_id);
    });
  }
  function checkReview() {
    const id = approvalId.trim(); if (!requireInput(!!reviewScreen && digest(reviewScreen.record_sha256) && !!id)) return;
    setReview(null);
    void perform(async () => {
      const { review: next } = await decisionApi.shadowReviewStatus({ ...scope,
        replay_hash: reviewScreen!.replay_hash, approval_id: id });
      if (!validReview(next, reviewScreen!, id)) throw new Error(t('decisionLab.shadowFlow.mismatch'));
      setReview(next);
    });
  }

  const policyFields: Array<[string, (value: string) => void, string]> = [
    [policyId, setPolicyId, 'policyId'], [lineage, setLineage, 'lineage'], [queue, setQueue, 'queue'],
    [from, setFrom, 'from'], [until, setUntil, 'until'], [deadline, setDeadline, 'deadline'],
    [trainingDays, setTrainingDays, 'trainingDays'], [saturatedDays, setSaturatedDays, 'saturatedDays'],
  ];
  const forecastFields: Array<[string, (value: string) => void, string]> = [
    [forecastId, setForecastId, 'forecastId'], [target, setTarget, 'target'],
    [backlog, setBacklog, 'backlog'], [agents, setAgents, 'agents'], [extra, setExtra, 'extra'],
  ];
  const screenFields: Array<[string, (value: string) => void, string]> = [
    [minComplete, setMinComplete, 'minComplete'], [minCoverage, setMinCoverage, 'minCoverage'],
  ];

  return <Card><CardContent className="space-y-5" aria-label={t('decisionLab.shadowFlow.title')}>
    <div><h2 className="font-semibold">{t('decisionLab.shadowFlow.title')}</h2>
      <p className="mt-1 text-sm text-muted-foreground">{t('decisionLab.shadowFlow.hint')}</p></div>
    <section className="space-y-2 rounded border p-3" aria-label={t('decisionLab.shadowFlow.policyStage')}>
      <h3 className="font-medium">{t('decisionLab.shadowFlow.policyStage')}</h3>
      <div className="grid gap-2 sm:grid-cols-3">
        {policyFields.map(([value, setter, label]) =>
          <label key={label} className="space-y-1 text-sm">{t(`decisionLab.shadowFlow.${label}`)}
            <Input aria-label={t(`decisionLab.shadowFlow.${label}`)} value={value} disabled={disabled}
              onChange={(event) => { setter(event.target.value); clearPolicy(); }} /></label>)}
      </div>
      <div className="flex gap-2"><Button disabled={disabled} onClick={createPolicy}>{t('decisionLab.shadowFlow.create')}</Button>
        <Button variant="outline" disabled={disabled || !policyId.trim()} onClick={loadPolicy}>{t('decisionLab.shadowFlow.load')}</Button></div>
      {policy && <div className="space-y-1 text-xs" aria-label={t('decisionLab.shadowFlow.policyLoaded')}>
        <Badge variant="secondary">{t('decisionLab.shadowFlow.registered')}</Badge>
        <p>{policy.id} · {policy.queue_id} · {policy.source_lineage}</p>
        <p>{t('decisionLab.shadowFlow.window')}: {dateTime(policy.effective_from_utc, locale)} – {dateTime(policy.effective_until_utc, locale)} UTC</p>
        <p>{t('decisionLab.shadowFlow.deadline')}: {policy.issue_deadline_seconds}s · {t('decisionLab.shadowFlow.registeredAt')}: {dateTime(policy.registered_at, locale)} UTC</p>
        <p className="break-all font-mono">{t('decisionLab.shadowFlow.record')}: {policy.record_sha256}</p>
        <Limitations values={policy.limitations} t={t} /></div>}
    </section>

    <section className="space-y-2 rounded border p-3" aria-label={t('decisionLab.shadowFlow.forecastStage')}>
      <h3 className="font-medium">{t('decisionLab.shadowFlow.forecastStage')}</h3>
      <p className="text-xs text-muted-foreground">{t('decisionLab.shadowFlow.forecastTiming')}</p>
      <div className="grid gap-2 sm:grid-cols-3">
        {forecastFields.map(([value, setter, label]) =>
          <label key={label} className="space-y-1 text-sm">{t(`decisionLab.shadowFlow.${label}`)}
            <Input aria-label={t(`decisionLab.shadowFlow.${label}`)} value={value} disabled={disabled}
              onChange={(event) => { setter(event.target.value); clearForecast(); }} /></label>)}
      </div>
      <SourceInput kind="training" artifact={trainingArtifact} source={trainingSource}
        retention={trainingRetention} setArtifact={(value) => { setTrainingArtifact(value); clearForecast(); }}
        setSource={(value) => { setTrainingSource(value); clearForecast(); }}
        setRetention={(value) => { setTrainingRetention(value); clearForecast(); }} disabled={disabled} t={t} />
      <div className="flex gap-2"><Button disabled={disabled || !policy} onClick={createForecast}>{t('decisionLab.shadowFlow.create')}</Button>
        <Button variant="outline" disabled={disabled || !forecastId.trim()} onClick={loadForecast}>{t('decisionLab.shadowFlow.load')}</Button></div>
      {forecast && <div className="space-y-1 text-xs" aria-label={t('decisionLab.shadowFlow.forecastLoaded')}>
        <p>{forecast.id} · {t('decisionLab.shadowFlow.target')}: {dateTime(forecast.target_day_utc, locale)} UTC · {t('decisionLab.shadowFlow.committed')}: {dateTime(forecast.committed_at, locale)} UTC</p>
        <p>{t('decisionLab.shadowFlow.prediction')}: {forecast.forecast.predicted_arrivals} / {forecast.forecast.predicted_backlog_end} · {t('decisionLab.shadowFlow.baselines')}: {forecast.forecast.no_change_backlog_end} / {forecast.forecast.seasonal_naive_backlog_end} / {forecast.forecast.mean_change_backlog_end}</p>
        <p>{t('decisionLab.shadowFlow.fittedCapacity')}: {forecast.forecast.fitted_capacity_per_agent} · {t('decisionLab.shadowFlow.trainingDays')}: {forecast.forecast.training_days}</p>
        <p className="break-all font-mono">{t('decisionLab.shadowFlow.record')}: {forecast.record_sha256} · {t('decisionLab.shadowFlow.sourceDigest')}: {forecast.training_sha256}</p>
        <Limitations values={forecast.limitations} t={t} /></div>}
    </section>

    <section className="space-y-2 rounded border p-3" aria-label={t('decisionLab.shadowFlow.scoreStage')}>
      <h3 className="font-medium">{t('decisionLab.shadowFlow.scoreStage')}</h3>
      <p className="text-xs text-muted-foreground">{t('decisionLab.shadowFlow.scoreTiming')}</p>
      <label className="block max-w-md space-y-1 text-sm">{t('decisionLab.shadowFlow.scoreId')}
        <Input aria-label={t('decisionLab.shadowFlow.scoreId')} value={scoreId} disabled={disabled}
          onChange={(event) => { setScoreId(event.target.value); clearScore(); }} /></label>
      <SourceInput kind="observation" artifact={observationArtifact} source={observationSource}
        retention={observationRetention} setArtifact={(value) => { setObservationArtifact(value); clearScore(); }}
        setSource={(value) => { setObservationSource(value); clearScore(); }}
        setRetention={(value) => { setObservationRetention(value); clearScore(); }} disabled={disabled} t={t} />
      <div className="flex gap-2"><Button disabled={disabled || !forecast} onClick={createScore}>{t('decisionLab.shadowFlow.create')}</Button>
        <Button variant="outline" disabled={disabled || !scoreId.trim()} onClick={loadScore}>{t('decisionLab.shadowFlow.load')}</Button></div>
      {score && <div className="space-y-1 text-xs" aria-label={t('decisionLab.shadowFlow.scoreLoaded')}>
        <p>{score.id} · {t('decisionLab.shadowFlow.scoredAt')}: {dateTime(score.scored_at, locale)} UTC</p>
        <p>{t('decisionLab.shadowFlow.arrivalsError')}: {new Intl.NumberFormat(locale).format(BigInt(score.arrivals_abs_error))} · {t('decisionLab.shadowFlow.backlogError')}: {new Intl.NumberFormat(locale).format(BigInt(score.backlog_abs_error))}</p>
        <p>{t('decisionLab.shadowFlow.baselines')}: {[score.no_change_abs_error, score.seasonal_naive_abs_error, score.mean_change_abs_error].map((v) => new Intl.NumberFormat(locale).format(BigInt(v))).join(' / ')}</p>
        <p className="break-all font-mono">{t('decisionLab.shadowFlow.record')}: {score.record_sha256} · {t('decisionLab.shadowFlow.sourceDigest')}: {score.observation_sha256}</p>
        <Limitations values={score.limitations} t={t} /></div>}
    </section>

    <section className="space-y-2 rounded border p-3" aria-label={t('decisionLab.shadowFlow.assessStage')}>
      <h3 className="font-medium">{t('decisionLab.shadowFlow.assessStage')}</h3>
      <Button variant="outline" disabled={disabled || !policy} onClick={assess}>{t('decisionLab.shadowFlow.assess')}</Button>
      {assessment && <div className="space-y-1 text-xs" aria-label={t('decisionLab.shadowFlow.assessmentLoaded')}>
        <p>{t('decisionLab.shadowFlow.progress')}: {assessment.scored_days} / {assessment.due_days} · {t('decisionLab.shadowFlow.corrected')}: {assessment.corrected_days} · {assessment.complete ? t('decisionLab.shadowFlow.complete') : t('decisionLab.shadowFlow.incomplete')}</p>
        <Sums values={assessment.error_sums} locale={locale} t={t} />
        <p>{t('decisionLab.shadowFlow.wholeSkill')}: {String(assessment.backlog_abs_error_below_each_baseline)} · {t('decisionLab.shadowFlow.recentSkill')}: {String(assessment.recent_7_day_backlog_abs_error_below_each_baseline)}</p>
        <p>{t('decisionLab.shadowFlow.fixedCoverage')}: {assessment.fixed_prefix_interval ? `${assessment.fixed_prefix_interval.covered_points}/${assessment.fixed_prefix_interval.evaluated_points} · ${assessment.fixed_prefix_interval.observed_coverage_basis_points}/10000 · ${t('decisionLab.shadowFlow.drift')}: ${String(assessment.fixed_prefix_interval.drift_signal)}` : t('decisionLab.shadowFlow.unavailable')}</p>
        <p>{t('decisionLab.shadowFlow.dayStatuses')}: {Object.entries(assessment.day_status_counts).map(([key, value]) => `${key}: ${value}`).join(' · ')}</p>
        <Limitations values={assessment.limitations} t={t} /></div>}
    </section>

    <section className="space-y-2 rounded border p-3" aria-label={t('decisionLab.shadowFlow.screenStage')}>
      <h3 className="font-medium">{t('decisionLab.shadowFlow.screenStage')}</h3>
      <div className="grid gap-2 sm:grid-cols-2">
        {screenFields.map(([value, setter, label]) =>
          <label key={label} className="space-y-1 text-sm">{t(`decisionLab.shadowFlow.${label}`)}
            <Input type="number" min={label === 'minComplete' ? 21 : 0}
              max={label === 'minComplete' ? 366 : 10000}
              aria-label={t(`decisionLab.shadowFlow.${label}`)} value={value} disabled={disabled}
              onChange={(event) => { setter(event.target.value); clearScreen(); }} /></label>)}
      </div>
      <div className="flex flex-wrap gap-2"><Button variant="outline" disabled={disabled || !policy || !screenCriteriaValid} onClick={() => screenPolicy(false)}>{t('decisionLab.shadowFlow.evaluate')}</Button>
        <Button disabled={disabled || !policy || !screenCriteriaValid} onClick={() => screenPolicy(true)}>{t('decisionLab.shadowFlow.save')}</Button></div>
      <label className="block space-y-1 text-sm">{t('decisionLab.shadowFlow.screenHash')}
        <Input aria-label={t('decisionLab.shadowFlow.screenHash')} value={screenHash} disabled={disabled}
          onChange={(event) => { setScreenHash(event.target.value); setReviewScreen(null);
            // The assessment on screen was frozen by the screen being replaced.
            // Leaving it visible reads as a live assessment of the current
            // policy. (`clearScreen()` itself would blank the field being typed.)
            setAssessment(null); clearReview(); }} /></label>
      <Button variant="outline" disabled={disabled || !screenHash.trim()} onClick={loadScreen}>{t('decisionLab.shadowFlow.load')}</Button>
      {reviewScreen && <div className="space-y-1 text-xs" aria-label={t('decisionLab.shadowFlow.screenLoaded')}>
        <Badge variant="secondary">{reviewScreen.record_sha256 ? t('decisionLab.shadowFlow.saved') : t('decisionLab.shadowFlow.preview')}</Badge>
        <p>{reviewScreen.report.eligible_for_human_review ? t('decisionLab.shadowFlow.eligible') : t('decisionLab.shadowFlow.ineligible')}</p>
        <p>{t('decisionLab.shadowFlow.failed')}: {reviewScreen.report.failed_checks.length ? reviewScreen.report.failed_checks.join(' · ') : t('decisionLab.shadowFlow.none')}</p>
        <p>{t('decisionLab.shadowFlow.fixedCoverage')}: {reviewScreen.report.coverage_evidence ? `${reviewScreen.report.coverage_evidence.covered_days}/${reviewScreen.report.coverage_evidence.evaluated_days} (${reviewScreen.report.coverage_evidence.minimum_covered_days} ${t('decisionLab.shadowFlow.minimum')})` : t('decisionLab.shadowFlow.unavailable')}</p>
        <p className="break-all font-mono">{t('decisionLab.shadowFlow.screenHash')}: {reviewScreen.replay_hash} · {t('decisionLab.shadowFlow.record')}: {reviewScreen.record_sha256 ?? t('decisionLab.shadowFlow.preview')}</p>
        <Limitations values={reviewScreen.report.limitations} t={t} /></div>}
    </section>

    <section className="space-y-2 rounded border p-3" aria-label={t('decisionLab.shadowFlow.reviewStage')}>
      <h3 className="font-medium">{t('decisionLab.shadowFlow.reviewStage')}</h3>
      <p className="text-xs text-muted-foreground">{t('decisionLab.shadowFlow.reviewHint')}</p>
      <label className="block space-y-1 text-sm">{t('decisionLab.shadowFlow.summary')}
        <Input aria-label={t('decisionLab.shadowFlow.summary')} value={summary} maxLength={240} disabled={disabled}
          onChange={(event) => { setSummary(event.target.value); clearReview(); }} /></label>
      <Button disabled={disabled || !reviewScreen?.record_sha256 || !reviewScreen.report.eligible_for_human_review}
        onClick={requestReview}>{t('decisionLab.shadowFlow.requestReview')}</Button>
      <label className="block space-y-1 text-sm">{t('decisionLab.shadowFlow.approvalId')}
        <Input aria-label={t('decisionLab.shadowFlow.approvalId')} value={approvalId} disabled={disabled}
          onChange={(event) => { setApprovalId(event.target.value); setReview(null); }} /></label>
      <Button variant="outline" disabled={disabled || !reviewScreen?.record_sha256 || !approvalId.trim()}
        onClick={checkReview}>{t('decisionLab.shadowFlow.checkReview')}</Button>
      {review && <p className="text-xs" aria-label={t('decisionLab.shadowFlow.reviewLoaded')}>
        {review.status} · {t('decisionLab.shadowFlow.expires')}: {dateTime(review.expires_at_utc, locale)} UTC ·
        {t('decisionLab.shadowFlow.decidedBy')}: {review.decided_by ?? t('decisionLab.shadowFlow.unavailable')}</p>}
    </section>
    {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
    <p className="text-xs text-muted-foreground">{t('decisionLab.shadowFlow.finalLimit')}</p>
  </CardContent></Card>;
}
