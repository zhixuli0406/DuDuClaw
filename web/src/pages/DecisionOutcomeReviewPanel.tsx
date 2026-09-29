import { useState } from 'react';
import { Badge, Button, Card, CardContent, Input } from '@/components/mds';
import { decisionApi, type DecisionOutcomeFit, type DecisionOutcomeHoldout,
  type DecisionOutcomeReview, type DecisionOutcomeScreen } from '@/lib/decision-api';

type Scope = { tenant_id: string; acl: string };
type Translate = (id: string, values?: Record<string, string | number>) => string;
const digest = (value: string) => /^[a-f0-9]{64}$/.test(value);
const count = (value: number, minimum = 0) => Number.isSafeInteger(value) && value >= minimum;
const decimal = (value: string) => /^\d+$/.test(value);

function holdoutValid(value: DecisionOutcomeHoldout | null): boolean {
  if (value === null) return true;
  return count(value.training_days, 3) && count(value.holdout_days, 3)
    && decimal(value.candidate_abs_error_sum) && decimal(value.parent_abs_error_sum)
    && decimal(value.no_change_abs_error_sum)
    && value.candidate_beats_parent
      === (BigInt(value.candidate_abs_error_sum) < BigInt(value.parent_abs_error_sum))
    && value.candidate_beats_no_change
      === (BigInt(value.candidate_abs_error_sum) < BigInt(value.no_change_abs_error_sum));
}

function fitValid(fit: DecisionOutcomeFit, id: string, outcome?: string): boolean {
  return fit.id === id && (!outcome || fit.outcome_id === outcome)
    && [fit.replay_hash, fit.observed_source_sha256, fit.parent_model_sha256,
      fit.fit_engine_sha256, fit.record_sha256].every(digest)
    && count(fit.min_saturated_days, 3) && fit.min_saturated_days <= 366
    && count(fit.training_days, fit.min_saturated_days) && fit.training_days <= 366
    && typeof fit.engine_matches_current === 'boolean'
    && count(fit.fit.service_per_agent_day, 1) && count(fit.fit.saturated_days, 3)
    && fit.fit.training_days === fit.training_days
    && fit.fit.saturated_days >= fit.min_saturated_days
    && fit.holdout !== undefined && holdoutValid(fit.holdout)
    && (fit.holdout === null || fit.holdout.training_days === fit.training_days);
}

function screenValid(screen: DecisionOutcomeScreen, fit: DecisionOutcomeFit, hash?: string): boolean {
  const report = screen.report;
  return (!hash || screen.replay_hash === hash)
    && screen.fit_id === fit.id && screen.fit_record_sha256 === fit.record_sha256
    && report.status === 'exploratory_outcome_model_review_screen'
    && report.replay_hash === screen.replay_hash && report.fit_id === fit.id
    && report.fit_record_sha256 === fit.record_sha256 && report.outcome_id === fit.outcome_id
    && report.replayed_run_hash === fit.replay_hash
    && report.observed_source_sha256 === fit.observed_source_sha256
    && report.parent_model_version === fit.parent_model_version
    && report.parent_model_sha256 === fit.parent_model_sha256
    && report.fitted_capacity_per_agent_day === fit.fit.service_per_agent_day
    && report.saturated_training_days === fit.fit.saturated_days
    && JSON.stringify(report.holdout) === JSON.stringify(fit.holdout)
    && [screen.replay_hash, screen.record_sha256, screen.fit_record_sha256,
      report.review_engine_sha256].every(digest)
    && screen.source_version_hashes.length <= 16 && screen.source_version_hashes.every(digest)
    && typeof screen.engine_matches_current === 'boolean'
    && Number.isSafeInteger(screen.source_version_hashes_total)
    && screen.source_version_hashes_total >= screen.source_version_hashes.length
    && count(report.criteria.min_saturated_days, 3) && count(report.criteria.min_holdout_days, 3)
    && report.criteria.min_saturated_days <= 366 && report.criteria.min_holdout_days <= 366
    && holdoutValid(report.holdout)
    && report.eligible_for_human_review === (report.failed_checks.length === 0)
    && (!report.eligible_for_human_review || (report.holdout !== null
      && report.holdout.holdout_days >= report.criteria.min_holdout_days
      && report.holdout.candidate_beats_parent && report.holdout.candidate_beats_no_change
      && report.saturated_training_days >= report.criteria.min_saturated_days
      && report.local_run_precedes_window === true && !!report.queue_id))
    && (report.queue_id === null || report.queue_id.length > 0);
}

function reviewValid(review: DecisionOutcomeReview, screen: DecisionOutcomeScreen,
  approvalId?: string): boolean {
  return (!approvalId || review.link.approval_id === approvalId)
    && review.link.approval_id.length > 0 && review.link.agent_id.length > 0
    && review.link.replay_hash === screen.replay_hash
    && review.link.screen_record_sha256 === screen.record_sha256
    && review.link.fit_id === screen.fit_id
    && review.link.fit_record_sha256 === screen.fit_record_sha256
    && ['pending', 'approved', 'denied', 'expired'].includes(review.status)
    && !Number.isNaN(Date.parse(review.expires_at_utc));
}

function number(value: number, locale: string): string {
  return new Intl.NumberFormat(locale).format(value);
}

function exact(value: string, locale: string): string {
  return new Intl.NumberFormat(locale).format(BigInt(value));
}

function Holdout({ value, locale, t }: { value: DecisionOutcomeHoldout | null;
  locale: string; t: Translate }) {
  if (!value) return <p>{t('decisionLab.outcomeHoldoutUnavailable')}</p>;
  return <div className="space-y-1">
    <p>{t('decisionLab.outcomeHoldoutDays')}: {number(value.holdout_days, locale)}</p>
    <p>{t('decisionLab.outcomeCandidateError')}: {exact(value.candidate_abs_error_sum, locale)} · {t('decisionLab.outcomeParentError')}: {exact(value.parent_abs_error_sum, locale)} · {t('decisionLab.outcomeNoChangeError')}: {exact(value.no_change_abs_error_sum, locale)}</p>
    <p>{t('decisionLab.outcomeBeatsParent')}: {String(value.candidate_beats_parent)} · {t('decisionLab.outcomeBeatsNoChange')}: {String(value.candidate_beats_no_change)}</p>
  </div>;
}

function Limitations({ values, t }: { values: string[]; t: Translate }) {
  return <details className="text-xs text-muted-foreground"><summary>{t('decisionLab.outcomeLimitations')}</summary>
    <ul className="list-disc pl-5">{values.map((value) => <li key={value}>{value}</li>)}</ul>
  </details>;
}

export function DecisionOutcomeReviewPanel({ scope, blocked, onBusyChange, locale, t, onUseScreen }:
  { scope: Scope; blocked: boolean; onBusyChange: (busy: boolean) => void; locale: string;
    t: Translate; onUseScreen: (hash: string) => void }) {
  const [fitId, setFitId] = useState('');
  const [outcomeId, setOutcomeId] = useState('');
  const [trainingDays, setTrainingDays] = useState(14);
  const [minSaturatedDays, setMinSaturatedDays] = useState(3);
  const [fit, setFit] = useState<DecisionOutcomeFit | null>(null);
  const [minScreenSaturated, setMinScreenSaturated] = useState(3);
  const [minHoldout, setMinHoldout] = useState(3);
  const [screenHash, setScreenHash] = useState('');
  const [screen, setScreen] = useState<DecisionOutcomeScreen | null>(null);
  const [summary, setSummary] = useState('');
  const [approvalId, setApprovalId] = useState('');
  const [review, setReview] = useState<DecisionOutcomeReview | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const disabled = blocked || busy;
  const clearReview = () => { setReview(null); setApprovalId(''); setError(''); };
  const clearScreen = () => { setScreen(null); setScreenHash(''); clearReview(); };
  const clearFit = () => { setFit(null); clearScreen(); };

  async function perform(action: () => Promise<void>) {
    setBusy(true); onBusyChange(true); setError('');
    try { await action(); }
    catch (cause) { setError(cause instanceof Error ? cause.message : String(cause)); }
    finally { setBusy(false); onBusyChange(false); }
  }

  function saveFit() {
    const id = fitId.trim(); const outcome = outcomeId.trim();
    if (!id || !outcome || !count(minSaturatedDays, 3) || minSaturatedDays > 366
      || !count(trainingDays, minSaturatedDays) || trainingDays > 366) {
      setError(t('decisionLab.outcomeMissing')); return;
    }
    clearFit();
    void perform(async () => {
      const { fit: next } = await decisionApi.createOutcomeFit({ ...scope, fit_id: id,
        outcome_id: outcome, min_saturated_days: minSaturatedDays, training_days: trainingDays });
      if (!fitValid(next, id, outcome) || next.training_days !== trainingDays
        || next.min_saturated_days !== minSaturatedDays) throw new Error(t('decisionLab.outcomeMismatch'));
      setFit(next);
    });
  }

  function loadFit() {
    const id = fitId.trim();
    if (!id) { setError(t('decisionLab.outcomeMissing')); return; }
    clearFit();
    void perform(async () => {
      const { fit: next } = await decisionApi.loadOutcomeFit({ ...scope, fit_id: id });
      if (!fitValid(next, id, outcomeId.trim() || undefined)) throw new Error(t('decisionLab.outcomeMismatch'));
      setFit(next); setOutcomeId(next.outcome_id); setTrainingDays(next.training_days);
      setMinSaturatedDays(next.min_saturated_days);
    });
  }

  function saveScreen() {
    if (!fit || !count(minScreenSaturated, 3) || minScreenSaturated > 366
      || !count(minHoldout, 3) || minHoldout > 366) {
      setError(t('decisionLab.outcomeMissing')); return;
    }
    clearScreen();
    void perform(async () => {
      const { screen: next } = await decisionApi.saveOutcomeScreen({ ...scope, fit_id: fit.id,
        min_saturated_days: minScreenSaturated, min_holdout_days: minHoldout });
      if (!screenValid(next, fit) || next.report.criteria.min_saturated_days !== minScreenSaturated
        || next.report.criteria.min_holdout_days !== minHoldout) throw new Error(t('decisionLab.outcomeMismatch'));
      setScreen(next); setScreenHash(next.replay_hash);
    });
  }

  function loadScreen() {
    const hash = screenHash.trim();
    if (!digest(hash)) { setError(t('decisionLab.outcomeMissing')); return; }
    setScreen(null); clearReview();
    void perform(async () => {
      const { screen: next } = await decisionApi.loadOutcomeScreen({ ...scope, replay_hash: hash });
      if (fitId.trim() && next.fit_id !== fitId.trim()) throw new Error(t('decisionLab.outcomeMismatch'));
      const { fit: linked } = await decisionApi.loadOutcomeFit({ ...scope, fit_id: next.fit_id });
      if (!fitValid(linked, next.fit_id, outcomeId.trim() || undefined)
        || !screenValid(next, linked, hash)) throw new Error(t('decisionLab.outcomeMismatch'));
      setFit(linked); setFitId(linked.id); setOutcomeId(linked.outcome_id);
      setTrainingDays(linked.training_days); setMinSaturatedDays(linked.min_saturated_days);
      setMinScreenSaturated(next.report.criteria.min_saturated_days);
      setMinHoldout(next.report.criteria.min_holdout_days); setScreen(next);
    });
  }

  function requestReview() {
    if (!screen?.report.eligible_for_human_review || !summary.trim()) {
      setError(t('decisionLab.outcomeMissing')); return;
    }
    clearReview();
    void perform(async () => {
      const { review: next } = await decisionApi.requestOutcomeReview({ ...scope,
        replay_hash: screen.replay_hash, summary: summary.trim(), ttl_seconds: 3600 });
      if (!reviewValid(next, screen) || next.status !== 'pending') throw new Error(t('decisionLab.outcomeMismatch'));
      setReview(next); setApprovalId(next.link.approval_id);
    });
  }

  function checkReview() {
    const id = approvalId.trim();
    if (!screen || !id) { setError(t('decisionLab.outcomeMissing')); return; }
    setReview(null);
    void perform(async () => {
      const { review: next } = await decisionApi.outcomeReviewStatus({ ...scope,
        replay_hash: screen.replay_hash, approval_id: id });
      if (!reviewValid(next, screen, id)) throw new Error(t('decisionLab.outcomeMismatch'));
      setReview(next);
    });
  }

  return <Card><CardContent className="space-y-5" aria-label={t('decisionLab.outcomeTitle')}>
    <div><h2 className="font-semibold">{t('decisionLab.outcomeTitle')}</h2>
      <p className="mt-1 text-sm text-muted-foreground">{t('decisionLab.outcomeHint')}</p></div>
    {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
    <section className="space-y-2 rounded border p-3" aria-label={t('decisionLab.outcomeFitStage')}>
      <h3 className="font-medium">{t('decisionLab.outcomeFitStage')}</h3>
      <div className="grid gap-2 sm:grid-cols-2 lg:grid-cols-4">
        <label className="space-y-1 text-sm">{t('decisionLab.outcomeFitId')}
          <Input aria-label={t('decisionLab.outcomeFitId')} value={fitId} maxLength={128} disabled={disabled}
            onChange={(event) => { setFitId(event.target.value); clearFit(); }} /></label>
        <label className="space-y-1 text-sm">{t('decisionLab.outcomeOutcomeId')}
          <Input aria-label={t('decisionLab.outcomeOutcomeId')} value={outcomeId} maxLength={128} disabled={disabled}
            onChange={(event) => { setOutcomeId(event.target.value); clearFit(); }} /></label>
        <label className="space-y-1 text-sm">{t('decisionLab.outcomeTrainingDays')}
          <Input aria-label={t('decisionLab.outcomeTrainingDays')} type="number" min={3} max={366} value={trainingDays} disabled={disabled}
            onChange={(event) => { setTrainingDays(Number(event.target.value)); clearFit(); }} /></label>
        <label className="space-y-1 text-sm">{t('decisionLab.outcomeMinSaturated')}
          <Input aria-label={t('decisionLab.outcomeMinSaturated')} type="number" min={3} max={366} value={minSaturatedDays} disabled={disabled}
            onChange={(event) => { setMinSaturatedDays(Number(event.target.value)); clearFit(); }} /></label>
      </div>
      <div className="flex flex-wrap gap-2"><Button disabled={disabled || !fitId.trim() || !outcomeId.trim() || !count(minSaturatedDays, 3) || minSaturatedDays > 366 || !count(trainingDays, minSaturatedDays) || trainingDays > 366} onClick={saveFit}>{t('decisionLab.outcomeSaveFit')}</Button>
        <Button variant="outline" disabled={disabled || !fitId.trim()} onClick={loadFit}>{t('decisionLab.outcomeLoadFit')}</Button></div>
      {fit && <div className="space-y-1 text-sm" aria-label={t('decisionLab.outcomeLoadedFit')}>
        {/* The scored run is never recomputed on load, so say plainly when
            the installed simulator no longer produces it. */}
        {!fit.engine_matches_current && <Badge variant="outline">{t('decisionLab.staleEngineBadge')}</Badge>}
        <p>{t('decisionLab.outcomeFitIdentity')}: {fit.id} · {fit.outcome_id} · {fit.parent_model_version}</p>
        <p>{t('decisionLab.outcomeCapacity')}: {number(fit.fit.service_per_agent_day, locale)} · {t('decisionLab.outcomeSaturatedDays')}: {number(fit.fit.saturated_days, locale)} · {t('decisionLab.outcomeTrainingDays')}: {number(fit.training_days, locale)}</p>
        <Holdout value={fit.holdout} locale={locale} t={t} />
        <details className="text-xs"><summary>{t('decisionLab.outcomeProvenance')}</summary>
          <p className="break-all">{t('decisionLab.outcomeRecordSha')}: {fit.record_sha256}</p>
          <p className="break-all">{t('decisionLab.outcomeRunHash')}: {fit.replay_hash}</p>
          <p className="break-all">{t('decisionLab.outcomeSourceSha')}: {fit.observed_source_sha256}</p>
          <p className="break-all">{t('decisionLab.outcomeEngineSha')}: {fit.fit_engine_sha256}</p>
        </details><Limitations values={fit.limitations} t={t} />
      </div>}
    </section>
    <section className="space-y-2 rounded border p-3" aria-label={t('decisionLab.outcomeScreenStage')}>
      <h3 className="font-medium">{t('decisionLab.outcomeScreenStage')}</h3>
      <div className="grid gap-2 sm:grid-cols-3">
        <label className="space-y-1 text-sm">{t('decisionLab.outcomeScreenMinSaturated')}
          <Input aria-label={t('decisionLab.outcomeScreenMinSaturated')} type="number" min={3} max={366} value={minScreenSaturated} disabled={disabled}
            onChange={(event) => { setMinScreenSaturated(Number(event.target.value)); clearScreen(); }} /></label>
        <label className="space-y-1 text-sm">{t('decisionLab.outcomeMinHoldout')}
          <Input aria-label={t('decisionLab.outcomeMinHoldout')} type="number" min={3} max={366} value={minHoldout} disabled={disabled}
            onChange={(event) => { setMinHoldout(Number(event.target.value)); clearScreen(); }} /></label>
        <label className="space-y-1 text-sm">{t('decisionLab.outcomeScreenHash')}
          <Input aria-label={t('decisionLab.outcomeScreenHash')} value={screenHash} maxLength={128} disabled={disabled}
            onChange={(event) => { setScreenHash(event.target.value); setScreen(null); clearReview(); }} /></label>
      </div>
      <div className="flex flex-wrap gap-2"><Button disabled={disabled || !fit || !count(minScreenSaturated, 3) || minScreenSaturated > 366 || !count(minHoldout, 3) || minHoldout > 366} onClick={saveScreen}>{t('decisionLab.outcomeSaveScreen')}</Button>
        <Button variant="outline" disabled={disabled || !digest(screenHash.trim())} onClick={loadScreen}>{t('decisionLab.outcomeLoadScreen')}</Button></div>
      {screen && <div className="space-y-1 text-sm" aria-label={t('decisionLab.outcomeLoadedScreen')}>
        <Badge variant={screen.report.eligible_for_human_review ? 'secondary' : 'outline'}>{screen.report.eligible_for_human_review ? t('decisionLab.outcomeEligible') : t('decisionLab.outcomeIneligible')}</Badge>
        {!screen.engine_matches_current && <Badge variant="outline">{t('decisionLab.staleEngineBadge')}</Badge>}
        <p>{t('decisionLab.outcomeScreenIdentity')}: {screen.report.fit_id} · {screen.report.outcome_id} · {screen.report.queue_id ?? t('decisionLab.eventUnavailable')}</p>
        <p>{t('decisionLab.outcomeTiming')}: {String(screen.report.local_run_precedes_window)}</p>
        <Holdout value={screen.report.holdout} locale={locale} t={t} />
        {screen.report.failed_checks.length > 0 && <p>{t('decisionLab.outcomeFailedChecks')}: {screen.report.failed_checks.join(', ')}</p>}
        <p className="break-all font-mono text-xs">{t('decisionLab.outcomeScreenHash')}: {screen.replay_hash}</p>
        <details className="text-xs"><summary>{t('decisionLab.outcomeProvenance')}</summary>
          <p className="break-all">{t('decisionLab.outcomeRecordSha')}: {screen.record_sha256}</p>
          <p className="break-all">{t('decisionLab.outcomeFitRecordSha')}: {screen.fit_record_sha256}</p>
          <p className="break-all">{t('decisionLab.outcomeEngineSha')}: {screen.report.review_engine_sha256}</p>
          {/* The list is capped at 16 server-side; show "16 / 42" so a
              truncated preview is not read as the complete provenance. */}
          <p>{t('decisionLab.outcomeSourceVersions')}
            {screen.source_version_hashes_total > screen.source_version_hashes.length
              ? ` (${screen.source_version_hashes.length} / ${screen.source_version_hashes_total})` : ''}
            : {screen.source_version_hashes.join(', ')}</p>
        </details><Limitations values={screen.report.limitations} t={t} />
        {screen.report.eligible_for_human_review && <Button variant="outline" disabled={disabled}
          onClick={() => onUseScreen(screen.replay_hash)}>{t('decisionLab.outcomeUseScreen')}</Button>}
      </div>}
    </section>
    <section className="space-y-2 rounded border p-3" aria-label={t('decisionLab.outcomeReviewStage')}>
      <h3 className="font-medium">{t('decisionLab.outcomeReviewStage')}</h3>
      <p className="text-sm text-muted-foreground">{t('decisionLab.outcomeReviewHint')}</p>
      <div className="grid gap-2 sm:grid-cols-2">
        <label className="space-y-1 text-sm">{t('decisionLab.outcomeSummary')}
          <Input aria-label={t('decisionLab.outcomeSummary')} value={summary} maxLength={240} disabled={disabled}
            onChange={(event) => setSummary(event.target.value)} /></label>
        <label className="space-y-1 text-sm">{t('decisionLab.outcomeApprovalId')}
          <Input aria-label={t('decisionLab.outcomeApprovalId')} value={approvalId} maxLength={128} disabled={disabled}
            onChange={(event) => { setApprovalId(event.target.value); setReview(null); }} /></label>
      </div>
      <div className="flex flex-wrap gap-2"><Button disabled={disabled || !screen?.report.eligible_for_human_review || !summary.trim()} onClick={requestReview}>{t('decisionLab.outcomeRequestReview')}</Button>
        <Button variant="outline" disabled={disabled || !screen || !approvalId.trim()} onClick={checkReview}>{t('decisionLab.outcomeCheckReview')}</Button></div>
      {review && <div className="space-y-1 text-sm" aria-label={t('decisionLab.outcomeReviewLoaded')}>
        <Badge variant="secondary">{review.status}</Badge>
        <p>{t('decisionLab.outcomeApprovalId')}: {review.link.approval_id}</p>
        <p>{t('decisionLab.outcomeReviewExpiry')}: {review.expires_at_utc} · {t('decisionLab.outcomeDecidedBy')}: {review.decided_by ?? t('decisionLab.eventUnavailable')}</p>
        <p className="break-all font-mono text-xs">{t('decisionLab.outcomeScreenHash')}: {review.link.replay_hash} · {t('decisionLab.outcomeRecordSha')}: {review.link.screen_record_sha256}</p>
      </div>}
    </section>
  </CardContent></Card>;
}
