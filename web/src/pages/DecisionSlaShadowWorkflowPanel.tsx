import { useState } from 'react';
import { Badge, Button, Card, CardContent, Input } from '@/components/mds';
import { decisionApi, type DecisionShadowSlaAssessment, type DecisionShadowSlaErrorSums,
  type DecisionShadowSlaForecast, type DecisionShadowSlaReview,
  type DecisionShadowSlaScore, type DecisionShadowSlaScreen } from '@/lib/decision-api';

type Scope = { tenant_id: string; acl: string };
type Translate = (id: string, values?: Record<string, string | number>) => string;
type SourceKind = 'opening' | 'observation';
type SourceMode = 'artifact' | 'file' | 'paste';

/// The store and CLI cap one inline ticket-SLA source at two mebibytes; the
/// request envelope adds the 64 KiB of headroom the gateway route allows.
const MAX_SOURCE_BYTES = 2 * 1024 * 1024;
const MAX_REQUEST_BYTES = 2 * 1024 * 1024 + 64 * 1024;
const SLA_INTERVAL_METHOD = 'fixed_prefix_sla_absolute_residual_rank90_v1';

const digest = (value: unknown): value is string => typeof value === 'string' && /^[a-f0-9]{64}$/.test(value);
const count = (value: unknown, minimum = 0): value is number => Number.isSafeInteger(value) && (value as number) >= minimum;
const decimal = (value: unknown): value is string => typeof value === 'string' && /^\d+$/.test(value);
const utc = (value: string) => /^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d+)?(?:Z|\+00:00)$/.test(value)
  && Number.isFinite(Date.parse(value));
const utcMidnight = (value: string) => utc(value) && new Date(value).toISOString().slice(11, 23) === '00:00:00.000';
const failure = (error: unknown) => error instanceof Error ? error.message : String(error);
const numeric = (value: string, minimum = 0) => {
  const parsed = Number(value);
  return count(parsed, minimum) && value.trim() !== '' ? parsed : null;
};
const absDiff = (left: string, right: string): string => {
  const a = BigInt(left); const b = BigInt(right);
  return (a > b ? a - b : b - a).toString();
};
const validSums = (value: DecisionShadowSlaErrorSums | null): boolean => value === null
  || [value.model, value.no_change, value.seasonal_naive, value.seven_day_mean].every(decimal);
const belowBaselines = (value: DecisionShadowSlaErrorSums): boolean =>
  [value.no_change, value.seasonal_naive, value.seven_day_mean]
    .every((baseline) => BigInt(value.model) < BigInt(baseline));

function validForecast(value: DecisionShadowSlaForecast, id?: string, forecastId?: string): boolean {
  return (!id || value.id === id) && (!forecastId || value.forecast_id === forecastId)
    && !!value.id && !!value.forecast_id && digest(value.forecast_sha256)
    && !!value.policy_id && !!value.model_version && digest(value.model_sha256)
    && !!value.opening_artifact_id && digest(value.opening_sha256)
    && !!value.source_lineage && !!value.queue_id
    && utcMidnight(value.target_day_utc) && count(value.committed_at)
    && value.committed_at * 1000 >= Date.parse(value.target_day_utc)
    && value.committed_at * 1000 < Date.parse(value.target_day_utc) + 86_400_000
    && decimal(value.opening.opening_backlog)
    && count(value.opening.opening_cohort_count) && count(value.opening.prior_resolved_ticket_count)
    && count(value.opening.planned_agents) && count(value.opening.planned_fixed_extra_capacity)
    && count(value.prediction.predicted_resolved_within_sla)
    && count(value.prediction.predicted_arrivals)
    && decimal(value.prediction.predicted_backlog_end)
    && count(value.prediction.fitted_capacity_per_agent)
    && count(value.prediction.training_days, 7)
    && [value.baselines.no_change, value.baselines.seasonal_naive, value.baselines.seven_day_mean]
      .every((baseline) => count(baseline))
    && digest(value.daily_engine_sha256) && digest(value.record_sha256)
    && Array.isArray(value.limitations);
}

function validScore(value: DecisionShadowSlaScore, forecast: DecisionShadowSlaForecast, id?: string): boolean {
  return (!id || value.id === id) && value.sla_forecast_id === forecast.id
    && value.sla_forecast_sha256 === forecast.record_sha256
    && !!value.aggregate_score_id && digest(value.aggregate_score_sha256)
    && !!value.observation_artifact_id && digest(value.observation_sha256)
    && digest(value.record_sha256) && count(value.scored_at)
    && value.scored_at * 1000 >= Date.parse(forecast.target_day_utc) + 86_400_000
    && [value.predicted_resolved_within_sla, value.observed_resolved_within_sla, value.abs_error,
      value.no_change_abs_error, value.seasonal_naive_abs_error, value.seven_day_mean_abs_error].every(decimal)
    && value.predicted_resolved_within_sla === String(forecast.prediction.predicted_resolved_within_sla)
    && value.abs_error === absDiff(value.predicted_resolved_within_sla, value.observed_resolved_within_sla)
    && value.no_change_abs_error
      === absDiff(String(forecast.baselines.no_change), value.observed_resolved_within_sla)
    && value.seasonal_naive_abs_error
      === absDiff(String(forecast.baselines.seasonal_naive), value.observed_resolved_within_sla)
    && value.seven_day_mean_abs_error
      === absDiff(String(forecast.baselines.seven_day_mean), value.observed_resolved_within_sla)
    && typeof value.correction_revision === 'boolean'
    && Array.isArray(value.limitations);
}

function validAssessment(value: DecisionShadowSlaAssessment, policyId?: string): boolean {
  const interval = value.fixed_prefix_interval;
  return (!policyId || value.policy_id === policyId) && !!value.policy_id && digest(value.policy_sha256)
    && !!value.source_lineage && (value.queue_id === null || !!value.queue_id)
    && utc(value.assessed_at_utc) && count(value.due_days) && count(value.scored_days)
    && value.scored_days <= value.due_days && count(value.corrected_days)
    && (!value.complete || value.scored_days === value.due_days)
    && validSums(value.error_sums) && validSums(value.recent_7_day_error_sums)
    && value.model_abs_error_below_each_baseline === (value.error_sums === null
      ? null : belowBaselines(value.error_sums))
    && value.recent_7_day_model_abs_error_below_each_baseline === (value.recent_7_day_error_sums === null
      ? null : belowBaselines(value.recent_7_day_error_sums))
    && (value.total_predicted_resolved_within_sla === null
      || decimal(value.total_predicted_resolved_within_sla))
    && (value.total_observed_resolved_within_sla === null
      || decimal(value.total_observed_resolved_within_sla))
    && (value.error_sums === null) === (value.total_predicted_resolved_within_sla === null)
    && (interval === null || (interval.method === SLA_INTERVAL_METHOD
      && count(interval.target_coverage_basis_points) && count(interval.observed_coverage_basis_points)
      && count(interval.calibration_points, 14) && count(interval.evaluated_points)
      && decimal(interval.calibration_radius)
      && count(interval.recent_miss_count) && count(interval.recent_window_points)
      && count(interval.covered_points) && interval.covered_points <= interval.evaluated_points
      && interval.target_coverage_basis_points <= 10_000
      && interval.observed_coverage_basis_points <= 10_000))
    && Object.values(value.day_status_counts).every((item) => count(item))
    && Object.values(value.day_status_counts).reduce((sum, item) => sum + item, 0) === value.due_days
    && Array.isArray(value.limitations);
}

function validScreen(value: DecisionShadowSlaScreen, saved: boolean, hash?: string,
  criteria?: { min_complete_days: number; min_fixed_coverage_bps: number }, policyId?: string): boolean {
  return (!hash || value.replay_hash === hash) && digest(value.replay_hash)
    && (!policyId || value.policy_id === policyId) && !!value.policy_id
    && value.policy_sha256 === value.report.assessment.policy_sha256
    && (saved ? digest(value.record_sha256) : value.record_sha256 === null)
    && value.source_version_hashes.length <= 16 && value.source_version_hashes.every(digest)
    && (!saved ? value.source_version_hashes.length === 0 : true)
    && !!value.report.status && digest(value.report.screen_engine_sha256)
    && count(value.report.criteria.min_complete_days, 21)
    && value.report.criteria.min_complete_days <= 366
    && count(value.report.criteria.min_fixed_coverage_bps)
    && value.report.criteria.min_fixed_coverage_bps <= 10_000
    && (!criteria || (value.report.criteria.min_complete_days === criteria.min_complete_days
      && value.report.criteria.min_fixed_coverage_bps === criteria.min_fixed_coverage_bps))
    && validAssessment(value.report.assessment, value.policy_id)
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
      && value.report.assessment.model_abs_error_below_each_baseline === true
      && value.report.assessment.recent_7_day_model_abs_error_below_each_baseline === true
      && value.report.assessment.fixed_prefix_interval !== null
      && value.report.assessment.fixed_prefix_interval.drift_signal === false
      && value.report.assessment.fixed_prefix_interval.evaluated_points + 14
        === value.report.assessment.due_days
      && value.report.assessment.fixed_prefix_interval.evaluated_points >= 7
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

function validReview(value: DecisionShadowSlaReview, screen: DecisionShadowSlaScreen, approvalId?: string): boolean {
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

function Limitations({ values, t }: { values: string[]; t: Translate }) {
  return <details className="text-xs"><summary>{t('decisionLab.slaShadowFlow.limitations')}</summary>
    <ul className="list-disc pl-5">{values.map((value, index) => <li key={`${index}:${value}`}>{value}</li>)}</ul>
  </details>;
}

type SourceState = {
  mode: SourceMode; artifact: string; source: string; retention: string; bytes: number;
};

const emptySource = (): SourceState => ({ mode: 'artifact', artifact: '', source: '', retention: '', bytes: 0 });

function TicketSourceInput({ kind, value, onChange, onError, disabled, t }: {
  kind: SourceKind; value: SourceState; onChange: (next: SourceState) => void;
  onError: (message: string) => void; disabled: boolean; t: Translate;
}) {
  const modes: SourceMode[] = ['artifact', 'file', 'paste'];
  const label = (suffix: string) => t(`decisionLab.slaShadowFlow.${kind}${suffix}`);
  async function readFile(file: File | undefined) {
    if (!file) { onChange({ ...value, source: '', bytes: 0 }); return; }
    if (file.size > MAX_SOURCE_BYTES) {
      onChange({ ...value, source: '', bytes: file.size });
      onError(t('decisionLab.slaShadowFlow.sourceTooLarge'));
      return;
    }
    const text = await file.text();
    const bytes = new TextEncoder().encode(text).length;
    // A replacement character means the bytes were not valid UTF-8 JSON.
    if (text.includes('\uFFFD')) {
      onChange({ ...value, source: '', bytes });
      onError(t('decisionLab.slaShadowFlow.fileUnreadable'));
      return;
    }
    if (bytes > MAX_SOURCE_BYTES) {
      onChange({ ...value, source: '', bytes });
      onError(t('decisionLab.slaShadowFlow.sourceTooLarge'));
      return;
    }
    try { JSON.parse(text); } catch {
      onChange({ ...value, source: '', bytes });
      onError(t('decisionLab.slaShadowFlow.fileUnreadable'));
      return;
    }
    onChange({ ...value, source: text, bytes });
  }
  return <div className="space-y-2 rounded border border-dashed p-2">
    <div className="flex flex-wrap gap-3 text-sm" role="radiogroup" aria-label={`${label('Artifact')} ${t('decisionLab.slaShadowFlow.sourceMode')}`}>
      {modes.map((mode) => <label key={mode} className="flex items-center gap-1">
        <input type="radio" name={`${kind}-source-mode`} value={mode} checked={value.mode === mode}
          disabled={disabled}
          aria-label={t(`decisionLab.slaShadowFlow.sourceMode${mode.charAt(0).toUpperCase()}${mode.slice(1)}`)}
          onChange={() => onChange({ ...emptySource(), retention: value.retention, mode })} />
        {t(`decisionLab.slaShadowFlow.sourceMode${mode.charAt(0).toUpperCase()}${mode.slice(1)}`)}
      </label>)}
    </div>
    {value.mode === 'artifact' && <label className="block space-y-1 text-sm">{label('Artifact')}
      <Input aria-label={label('Artifact')} value={value.artifact} maxLength={128} disabled={disabled}
        onChange={(event) => onChange({ ...value, artifact: event.target.value })} /></label>}
    {value.mode === 'file' && <label className="block space-y-1 text-sm">{label('File')}
      <input type="file" accept=".json,application/json" aria-label={label('File')} disabled={disabled}
        onChange={(event) => { void readFile(event.target.files?.[0]); }} /></label>}
    {value.mode === 'paste' && <label className="block space-y-1 text-sm">{label('Json')}
      <textarea aria-label={label('Json')} className="min-h-20 w-full rounded border bg-background p-2 font-mono text-xs"
        value={value.source} disabled={disabled}
        onChange={(event) => onChange({ ...value, source: event.target.value,
          bytes: new TextEncoder().encode(event.target.value).length })} /></label>}
    {value.mode !== 'artifact' && <>
      <label className="block space-y-1 text-sm">{t('decisionLab.slaShadowFlow.retention')}
        <Input aria-label={`${kind} ${t('decisionLab.slaShadowFlow.retention')}`} value={value.retention}
          placeholder="2099-01-01T00:00:00Z" disabled={disabled}
          onChange={(event) => onChange({ ...value, retention: event.target.value })} /></label>
      <p className="text-xs text-muted-foreground">{t('decisionLab.slaShadowFlow.fileBytes')}: {value.bytes}</p>
    </>}
    <p className="text-xs text-muted-foreground">{t('decisionLab.slaShadowFlow.sourceHint')}</p>
    <p className="text-xs text-muted-foreground">{label('Shape')}</p>
  </div>;
}

export function DecisionSlaShadowWorkflowPanel({ scope, blocked, onBusyChange, locale, t }: {
  scope: Scope; blocked: boolean; onBusyChange: (busy: boolean) => void; locale: string; t: Translate;
}) {
  const [slaId, setSlaId] = useState('');
  const [forecastId, setForecastId] = useState('');
  const [modelVersion, setModelVersion] = useState('');
  const [opening, setOpening] = useState<SourceState>(emptySource);
  const [forecast, setForecast] = useState<DecisionShadowSlaForecast | null>(null);
  const [scoreId, setScoreId] = useState('');
  const [aggregateScoreId, setAggregateScoreId] = useState('');
  const [observation, setObservation] = useState<SourceState>(emptySource);
  const [score, setScore] = useState<DecisionShadowSlaScore | null>(null);
  const [policyId, setPolicyId] = useState('');
  const [assessment, setAssessment] = useState<DecisionShadowSlaAssessment | null>(null);
  const [minComplete, setMinComplete] = useState('21');
  const [minCoverage, setMinCoverage] = useState('8000');
  const [screenHash, setScreenHash] = useState('');
  const [reviewScreen, setReviewScreen] = useState<DecisionShadowSlaScreen | null>(null);
  const [summary, setSummary] = useState('');
  const [approvalId, setApprovalId] = useState('');
  const [review, setReview] = useState<DecisionShadowSlaReview | null>(null);
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

  async function perform(action: () => Promise<void>) {
    setBusy(true); onBusyChange(true); setError('');
    try { await action(); } catch (cause) { setError(failure(cause)); }
    finally { setBusy(false); onBusyChange(false); }
  }
  function requireInput(ok: boolean): boolean {
    if (!ok) setError(t('decisionLab.slaShadowFlow.invalidInput'));
    return ok;
  }
  function sourceFields(state: SourceState, artifactKey: 'opening_artifact_id' | 'observation_artifact_id',
    sourceKey: 'opening_source_json' | 'observation_source_json') {
    const id = state.artifact.trim();
    const raw = state.mode === 'artifact' ? '' : state.source;
    if (state.mode === 'artifact' ? !id : !raw || !utc(state.retention)) {
      throw new Error(t('decisionLab.slaShadowFlow.sourceInvalid'));
    }
    if (raw) {
      if (new TextEncoder().encode(raw).length > MAX_SOURCE_BYTES) {
        throw new Error(t('decisionLab.slaShadowFlow.sourceTooLarge'));
      }
      try { JSON.parse(raw); } catch { throw new Error(t('decisionLab.slaShadowFlow.sourceInvalid')); }
    }
    return state.mode === 'artifact'
      ? { [artifactKey]: id }
      : { [sourceKey]: raw, retention_until_utc: state.retention };
  }
  function requestSize(input: object) {
    if (new TextEncoder().encode(JSON.stringify(input)).length > MAX_REQUEST_BYTES)
      throw new Error(t('decisionLab.slaShadowFlow.sourceTooLarge'));
  }

  function createForecast() {
    const id = slaId.trim(); const linked = forecastId.trim(); const model = modelVersion.trim();
    if (!requireInput(!!id && !!linked && !!model)) return;
    clearForecast();
    void perform(async () => {
      const source = sourceFields(opening, 'opening_artifact_id', 'opening_source_json');
      const input = { ...scope, sla_id: id, forecast_id: linked, model_version: model, ...source };
      requestSize(input);
      const { sla_forecast: next } = await decisionApi.createShadowSlaForecast(input);
      if (!validForecast(next, id, linked) || next.model_version !== model
        || (opening.mode === 'artifact' && next.opening_artifact_id !== opening.artifact.trim()))
        throw new Error(t('decisionLab.slaShadowFlow.mismatch'));
      setForecast(next);
      setOpening({ ...emptySource(), mode: 'artifact', artifact: next.opening_artifact_id });
    });
  }
  function loadForecast() {
    const id = slaId.trim(); if (!requireInput(!!id)) return;
    clearForecast();
    void perform(async () => {
      const { sla_forecast: next } = await decisionApi.loadShadowSlaForecast({ ...scope, sla_id: id });
      if (forecastId.trim() && next.forecast_id !== forecastId.trim())
        throw new Error(t('decisionLab.slaShadowFlow.mismatch'));
      if (policyId.trim() && next.policy_id !== policyId.trim())
        throw new Error(t('decisionLab.slaShadowFlow.mismatch'));
      if (!validForecast(next, id)) throw new Error(t('decisionLab.slaShadowFlow.mismatch'));
      setForecast(next); setForecastId(next.forecast_id); setModelVersion(next.model_version);
      setPolicyId(next.policy_id);
      setOpening({ ...emptySource(), mode: 'artifact', artifact: next.opening_artifact_id });
    });
  }
  function createScore() {
    const id = scoreId.trim(); const aggregate = aggregateScoreId.trim();
    if (!requireInput(!!forecast && !!id && !!aggregate)) return;
    clearScore();
    void perform(async () => {
      const source = sourceFields(observation, 'observation_artifact_id', 'observation_source_json');
      const input = { ...scope, score_id: id, sla_forecast_id: forecast!.id,
        aggregate_score_id: aggregate, ...source };
      requestSize(input);
      const { sla_score: next } = await decisionApi.createShadowSlaScore(input);
      if (!validScore(next, forecast!, id) || next.aggregate_score_id !== aggregate
        || (observation.mode === 'artifact' && next.observation_artifact_id !== observation.artifact.trim()))
        throw new Error(t('decisionLab.slaShadowFlow.mismatch'));
      setScore(next);
      setObservation({ ...emptySource(), mode: 'artifact', artifact: next.observation_artifact_id });
    });
  }
  function loadScore() {
    const id = scoreId.trim(); if (!requireInput(!!forecast && !!id)) return;
    clearScore();
    void perform(async () => {
      const { sla_score: next } = await decisionApi.loadShadowSlaScore({ ...scope, score_id: id });
      if (!validScore(next, forecast!, id)) throw new Error(t('decisionLab.slaShadowFlow.mismatch'));
      setScore(next); setAggregateScoreId(next.aggregate_score_id);
      setObservation({ ...emptySource(), mode: 'artifact', artifact: next.observation_artifact_id });
    });
  }
  function loadCurrentScore() {
    if (!requireInput(!!forecast)) return;
    clearScore();
    void perform(async () => {
      const { sla_score: next } = await decisionApi.loadCurrentShadowSlaScore({
        ...scope, sla_forecast_id: forecast!.id });
      if (!validScore(next, forecast!)) throw new Error(t('decisionLab.slaShadowFlow.mismatch'));
      setScore(next); setScoreId(next.id); setAggregateScoreId(next.aggregate_score_id);
      setObservation({ ...emptySource(), mode: 'artifact', artifact: next.observation_artifact_id });
    });
  }
  function assess() {
    const id = policyId.trim(); if (!requireInput(!!id)) return;
    clearAssessment();
    void perform(async () => {
      const { assessment: next } = await decisionApi.assessShadowSlaPolicy({ ...scope, policy_id: id });
      if (!validAssessment(next, id)) throw new Error(t('decisionLab.slaShadowFlow.mismatch'));
      setAssessment(next);
    });
  }
  function screenPolicy(save: boolean) {
    const id = policyId.trim(); const days = screenDays; const coverage = screenCoverage;
    if (!requireInput(!!id && screenCriteriaValid)) return;
    clearScreen();
    void perform(async () => {
      const input = { ...scope, policy_id: id, min_complete_days: days!, min_fixed_coverage_bps: coverage! };
      const { screen: next } = save ? await decisionApi.saveSlaShadowScreen(input)
        : await decisionApi.evaluateSlaShadowScreen(input);
      if (!validScreen(next, save, undefined,
        { min_complete_days: days!, min_fixed_coverage_bps: coverage! }, id))
        throw new Error(t('decisionLab.slaShadowFlow.mismatch'));
      setReviewScreen(next); setAssessment(next.report.assessment);
      if (save) setScreenHash(next.replay_hash);
    });
  }
  function loadScreen() {
    const hash = screenHash.trim(); if (!requireInput(digest(hash))) return;
    setReviewScreen(null); setAssessment(null); clearReview();
    void perform(async () => {
      const { screen: next } = await decisionApi.loadSlaShadowScreen({ ...scope, replay_hash: hash });
      if (policyId.trim() && next.policy_id !== policyId.trim())
        throw new Error(t('decisionLab.slaShadowFlow.mismatch'));
      if (!validScreen(next, true, hash)) throw new Error(t('decisionLab.slaShadowFlow.mismatch'));
      setReviewScreen(next); setAssessment(next.report.assessment); setPolicyId(next.policy_id);
      setMinComplete(String(next.report.criteria.min_complete_days));
      setMinCoverage(String(next.report.criteria.min_fixed_coverage_bps));
    });
  }
  function requestReview() {
    if (!requireInput(!!reviewScreen && digest(reviewScreen.record_sha256)
      && reviewScreen.report.eligible_for_human_review
      && !!summary.trim() && summary.trim().length <= 240)) return;
    clearReview();
    void perform(async () => {
      const { review: next } = await decisionApi.requestSlaShadowReview({ ...scope,
        replay_hash: reviewScreen!.replay_hash, summary: summary.trim(), ttl_seconds: 3600 });
      if (!validReview(next, reviewScreen!)) throw new Error(t('decisionLab.slaShadowFlow.mismatch'));
      setReview(next); setApprovalId(next.link.approval_id);
    });
  }
  function checkReview() {
    const id = approvalId.trim();
    if (!requireInput(!!reviewScreen && digest(reviewScreen.record_sha256) && !!id)) return;
    setReview(null);
    void perform(async () => {
      const { review: next } = await decisionApi.slaShadowReviewStatus({ ...scope,
        replay_hash: reviewScreen!.replay_hash, approval_id: id });
      if (!validReview(next, reviewScreen!, id)) throw new Error(t('decisionLab.slaShadowFlow.mismatch'));
      setReview(next);
    });
  }

  const forecastFields: Array<[string, (value: string) => void, string]> = [
    [slaId, setSlaId, 'slaId'], [forecastId, setForecastId, 'forecastId'],
    [modelVersion, setModelVersion, 'modelVersion'],
  ];
  const scoreFields: Array<[string, (value: string) => void, string]> = [
    [scoreId, setScoreId, 'scoreId'], [aggregateScoreId, setAggregateScoreId, 'aggregateScoreId'],
  ];
  const screenFields: Array<[string, (value: string) => void, string]> = [
    [minComplete, setMinComplete, 'minComplete'], [minCoverage, setMinCoverage, 'minCoverage'],
  ];
  const number = (value: string) => new Intl.NumberFormat(locale).format(BigInt(value));

  return <Card><CardContent className="space-y-5" aria-label={t('decisionLab.slaShadowFlow.title')}>
    <div><h2 className="font-semibold">{t('decisionLab.slaShadowFlow.title')}</h2>
      <p className="mt-1 text-sm text-muted-foreground">{t('decisionLab.slaShadowFlow.hint')}</p></div>

    <section className="space-y-2 rounded border p-3" aria-label={t('decisionLab.slaShadowFlow.forecastStage')}>
      <h3 className="font-medium">{t('decisionLab.slaShadowFlow.forecastStage')}</h3>
      <p className="text-xs text-muted-foreground">{t('decisionLab.slaShadowFlow.forecastTiming')}</p>
      <div className="grid gap-2 sm:grid-cols-3">
        {forecastFields.map(([value, setter, label]) =>
          <label key={label} className="space-y-1 text-sm">{t(`decisionLab.slaShadowFlow.${label}`)}
            <Input aria-label={t(`decisionLab.slaShadowFlow.${label}`)} value={value} disabled={disabled}
              onChange={(event) => { setter(event.target.value); clearForecast(); }} /></label>)}
      </div>
      <TicketSourceInput kind="opening" value={opening} disabled={disabled} t={t}
        onError={setError} onChange={(next) => { setOpening(next); clearForecast(); }} />
      <div className="flex gap-2">
        <Button disabled={disabled} onClick={createForecast}>{t('decisionLab.slaShadowFlow.create')}</Button>
        <Button variant="outline" disabled={disabled || !slaId.trim()} onClick={loadForecast}>{t('decisionLab.slaShadowFlow.load')}</Button>
      </div>
      {forecast && <div className="space-y-1 text-xs" aria-label={t('decisionLab.slaShadowFlow.forecastLoaded')}>
        <p>{forecast.id} · {t('decisionLab.slaShadowFlow.queue')}: {forecast.queue_id} · {t('decisionLab.slaShadowFlow.target')}: {dateTime(forecast.target_day_utc, locale)} UTC · {t('decisionLab.slaShadowFlow.committed')}: {dateTime(forecast.committed_at, locale)} UTC</p>
        <p>{t('decisionLab.slaShadowFlow.openingBacklog')}: {number(forecast.opening.opening_backlog)} · {t('decisionLab.slaShadowFlow.cohorts')}: {forecast.opening.opening_cohort_count} · {t('decisionLab.slaShadowFlow.priorResolved')}: {forecast.opening.prior_resolved_ticket_count}</p>
        <p>{t('decisionLab.slaShadowFlow.predictedWithinSla')}: {forecast.prediction.predicted_resolved_within_sla} · {t('decisionLab.slaShadowFlow.predictedBacklog')}: {number(forecast.prediction.predicted_backlog_end)}</p>
        <p>{t('decisionLab.slaShadowFlow.baselines')}: {forecast.baselines.no_change} / {forecast.baselines.seasonal_naive} / {forecast.baselines.seven_day_mean} · {t('decisionLab.slaShadowFlow.fittedCapacity')}: {forecast.prediction.fitted_capacity_per_agent} · {t('decisionLab.slaShadowFlow.trainingDays')}: {forecast.prediction.training_days}</p>
        <p className="break-all font-mono">{t('decisionLab.slaShadowFlow.record')}: {forecast.record_sha256} · {t('decisionLab.slaShadowFlow.sourceDigest')}: {forecast.opening_sha256}</p>
        <p className="break-all font-mono">{t('decisionLab.slaShadowFlow.engineDigest')}: {forecast.daily_engine_sha256}</p>
        <Limitations values={forecast.limitations} t={t} /></div>}
    </section>

    <section className="space-y-2 rounded border p-3" aria-label={t('decisionLab.slaShadowFlow.scoreStage')}>
      <h3 className="font-medium">{t('decisionLab.slaShadowFlow.scoreStage')}</h3>
      <p className="text-xs text-muted-foreground">{t('decisionLab.slaShadowFlow.scoreTiming')}</p>
      <div className="grid gap-2 sm:grid-cols-2">
        {scoreFields.map(([value, setter, label]) =>
          <label key={label} className="space-y-1 text-sm">{t(`decisionLab.slaShadowFlow.${label}`)}
            <Input aria-label={t(`decisionLab.slaShadowFlow.${label}`)} value={value} disabled={disabled}
              onChange={(event) => { setter(event.target.value); clearScore(); }} /></label>)}
      </div>
      <TicketSourceInput kind="observation" value={observation} disabled={disabled} t={t}
        onError={setError} onChange={(next) => { setObservation(next); clearScore(); }} />
      <div className="flex flex-wrap gap-2">
        <Button disabled={disabled || !forecast} onClick={createScore}>{t('decisionLab.slaShadowFlow.create')}</Button>
        <Button variant="outline" disabled={disabled || !forecast || !scoreId.trim()} onClick={loadScore}>{t('decisionLab.slaShadowFlow.load')}</Button>
        <Button variant="outline" disabled={disabled || !forecast} onClick={loadCurrentScore}>{t('decisionLab.slaShadowFlow.loadCurrent')}</Button>
      </div>
      {score && <div className="space-y-1 text-xs" aria-label={t('decisionLab.slaShadowFlow.scoreLoaded')}>
        <Badge variant="secondary">{score.correction_revision ? t('decisionLab.slaShadowFlow.correctionRevision') : t('decisionLab.slaShadowFlow.initialRevision')}</Badge>
        <p>{score.id} · {t('decisionLab.slaShadowFlow.scoredAt')}: {dateTime(score.scored_at, locale)} UTC</p>
        <p>{t('decisionLab.slaShadowFlow.predicted')}: {number(score.predicted_resolved_within_sla)} · {t('decisionLab.slaShadowFlow.observed')}: {number(score.observed_resolved_within_sla)} · {t('decisionLab.slaShadowFlow.slaError')}: {number(score.abs_error)}</p>
        <p>{t('decisionLab.slaShadowFlow.baselines')}: {[score.no_change_abs_error, score.seasonal_naive_abs_error, score.seven_day_mean_abs_error].map(number).join(' / ')}</p>
        <p className="break-all font-mono">{t('decisionLab.slaShadowFlow.record')}: {score.record_sha256} · {t('decisionLab.slaShadowFlow.sourceDigest')}: {score.observation_sha256}</p>
        <Limitations values={score.limitations} t={t} /></div>}
    </section>

    <section className="space-y-2 rounded border p-3" aria-label={t('decisionLab.slaShadowFlow.assessStage')}>
      <h3 className="font-medium">{t('decisionLab.slaShadowFlow.assessStage')}</h3>
      <label className="block max-w-md space-y-1 text-sm">{t('decisionLab.slaShadowFlow.policyId')}
        <Input aria-label={t('decisionLab.slaShadowFlow.policyId')} value={policyId} disabled={disabled}
          onChange={(event) => { setPolicyId(event.target.value); clearAssessment(); }} /></label>
      <Button variant="outline" disabled={disabled || !policyId.trim()} onClick={assess}>{t('decisionLab.slaShadowFlow.assess')}</Button>
      {assessment && <div className="space-y-1 text-xs" aria-label={t('decisionLab.slaShadowFlow.assessmentLoaded')}>
        <p>{t('decisionLab.slaShadowFlow.progress')}: {assessment.scored_days} / {assessment.due_days} · {t('decisionLab.slaShadowFlow.corrected')}: {assessment.corrected_days} · {assessment.complete ? t('decisionLab.slaShadowFlow.complete') : t('decisionLab.slaShadowFlow.incomplete')}</p>
        <p>{t('decisionLab.slaShadowFlow.slaErrors')}: {assessment.error_sums ? `${number(assessment.error_sums.model)} · ${t('decisionLab.slaShadowFlow.baselines')}: ${number(assessment.error_sums.no_change)} / ${number(assessment.error_sums.seasonal_naive)} / ${number(assessment.error_sums.seven_day_mean)}` : t('decisionLab.slaShadowFlow.unavailable')}</p>
        <p>{t('decisionLab.slaShadowFlow.totals')}: {assessment.total_predicted_resolved_within_sla && assessment.total_observed_resolved_within_sla ? `${number(assessment.total_predicted_resolved_within_sla)} / ${number(assessment.total_observed_resolved_within_sla)}` : t('decisionLab.slaShadowFlow.unavailable')}</p>
        <p>{t('decisionLab.slaShadowFlow.wholeSkill')}: {String(assessment.model_abs_error_below_each_baseline)} · {t('decisionLab.slaShadowFlow.recentSkill')}: {String(assessment.recent_7_day_model_abs_error_below_each_baseline)}</p>
        <p>{t('decisionLab.slaShadowFlow.fixedCoverage')}: {assessment.fixed_prefix_interval ? `${assessment.fixed_prefix_interval.covered_points}/${assessment.fixed_prefix_interval.evaluated_points} · ${assessment.fixed_prefix_interval.observed_coverage_basis_points}/10000 · ${t('decisionLab.slaShadowFlow.drift')}: ${String(assessment.fixed_prefix_interval.drift_signal)}` : t('decisionLab.slaShadowFlow.unavailable')}</p>
        <p>{t('decisionLab.slaShadowFlow.dayStatuses')}: {Object.entries(assessment.day_status_counts).map(([key, value]) => `${key}: ${value}`).join(' · ')}</p>
        <Limitations values={assessment.limitations} t={t} /></div>}
    </section>

    <section className="space-y-2 rounded border p-3" aria-label={t('decisionLab.slaShadowFlow.screenStage')}>
      <h3 className="font-medium">{t('decisionLab.slaShadowFlow.screenStage')}</h3>
      <div className="grid gap-2 sm:grid-cols-2">
        {screenFields.map(([value, setter, label]) =>
          <label key={label} className="space-y-1 text-sm">{t(`decisionLab.slaShadowFlow.${label}`)}
            <Input type="number" min={label === 'minComplete' ? 21 : 0}
              max={label === 'minComplete' ? 366 : 10000}
              aria-label={t(`decisionLab.slaShadowFlow.${label}`)} value={value} disabled={disabled}
              onChange={(event) => { setter(event.target.value); clearScreen(); }} /></label>)}
      </div>
      <div className="flex flex-wrap gap-2">
        <Button variant="outline" disabled={disabled || !policyId.trim() || !screenCriteriaValid} onClick={() => screenPolicy(false)}>{t('decisionLab.slaShadowFlow.evaluate')}</Button>
        <Button disabled={disabled || !policyId.trim() || !screenCriteriaValid} onClick={() => screenPolicy(true)}>{t('decisionLab.slaShadowFlow.save')}</Button>
      </div>
      <label className="block space-y-1 text-sm">{t('decisionLab.slaShadowFlow.screenHash')}
        <Input aria-label={t('decisionLab.slaShadowFlow.screenHash')} value={screenHash} disabled={disabled}
          onChange={(event) => { setScreenHash(event.target.value); setReviewScreen(null); clearReview(); }} /></label>
      <Button variant="outline" disabled={disabled || !screenHash.trim()} onClick={loadScreen}>{t('decisionLab.slaShadowFlow.load')}</Button>
      {reviewScreen && <div className="space-y-1 text-xs" aria-label={t('decisionLab.slaShadowFlow.screenLoaded')}>
        <Badge variant="secondary">{reviewScreen.record_sha256 ? t('decisionLab.slaShadowFlow.saved') : t('decisionLab.slaShadowFlow.preview')}</Badge>
        <p>{reviewScreen.report.eligible_for_human_review ? t('decisionLab.slaShadowFlow.eligible') : t('decisionLab.slaShadowFlow.ineligible')}</p>
        <p>{t('decisionLab.slaShadowFlow.failed')}: {reviewScreen.report.failed_checks.length ? reviewScreen.report.failed_checks.join(' · ') : t('decisionLab.slaShadowFlow.none')}</p>
        <p>{t('decisionLab.slaShadowFlow.fixedCoverage')}: {reviewScreen.report.coverage_evidence ? `${reviewScreen.report.coverage_evidence.covered_days}/${reviewScreen.report.coverage_evidence.evaluated_days} (${reviewScreen.report.coverage_evidence.minimum_covered_days} ${t('decisionLab.slaShadowFlow.minimum')})` : t('decisionLab.slaShadowFlow.unavailable')}</p>
        <p className="break-all font-mono">{t('decisionLab.slaShadowFlow.screenHash')}: {reviewScreen.replay_hash} · {t('decisionLab.slaShadowFlow.record')}: {reviewScreen.record_sha256 ?? t('decisionLab.slaShadowFlow.preview')}</p>
        <Limitations values={reviewScreen.report.limitations} t={t} /></div>}
    </section>

    <section className="space-y-2 rounded border p-3" aria-label={t('decisionLab.slaShadowFlow.reviewStage')}>
      <h3 className="font-medium">{t('decisionLab.slaShadowFlow.reviewStage')}</h3>
      <p className="text-xs text-muted-foreground">{t('decisionLab.slaShadowFlow.reviewHint')}</p>
      <label className="block space-y-1 text-sm">{t('decisionLab.slaShadowFlow.summary')}
        <Input aria-label={t('decisionLab.slaShadowFlow.summary')} value={summary} maxLength={240} disabled={disabled}
          onChange={(event) => { setSummary(event.target.value); clearReview(); }} /></label>
      <Button disabled={disabled || !reviewScreen?.record_sha256 || !reviewScreen.report.eligible_for_human_review}
        onClick={requestReview}>{t('decisionLab.slaShadowFlow.requestReview')}</Button>
      <label className="block space-y-1 text-sm">{t('decisionLab.slaShadowFlow.approvalId')}
        <Input aria-label={t('decisionLab.slaShadowFlow.approvalId')} value={approvalId} disabled={disabled}
          onChange={(event) => { setApprovalId(event.target.value); setReview(null); }} /></label>
      <Button variant="outline" disabled={disabled || !reviewScreen?.record_sha256 || !approvalId.trim()}
        onClick={checkReview}>{t('decisionLab.slaShadowFlow.checkReview')}</Button>
      {review && <p className="text-xs" aria-label={t('decisionLab.slaShadowFlow.reviewLoaded')}>
        {review.status} · {t('decisionLab.slaShadowFlow.expires')}: {dateTime(review.expires_at_utc, locale)} UTC ·
        {t('decisionLab.slaShadowFlow.decidedBy')}: {review.decided_by ?? t('decisionLab.slaShadowFlow.unavailable')}</p>}
    </section>
    {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
    <p className="text-xs text-muted-foreground">{t('decisionLab.slaShadowFlow.finalLimit')}</p>
  </CardContent></Card>;
}
