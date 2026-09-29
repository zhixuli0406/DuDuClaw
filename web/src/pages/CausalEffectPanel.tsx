import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { Button, Input, Textarea } from '@/components/mds';
import { causalApi, type AssumptionKind, type AssumptionView, type CausalScope, type EffectResult, type ModelReview, type NegativeControlView, type ObservedDataset } from '@/lib/causal-api';

const ASSUMPTION_KINDS: AssumptionKind[] = [
  'data_availability', 'temporal_order', 'positivity', 'exchangeability', 'consistency', 'identifiability',
];

function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function expiry(days: number): number {
  return Math.floor(Date.now() / 1000) + days * 86_400;
}

function localDateTime(): string {
  const now = new Date();
  return new Date(now.getTime() - now.getTimezoneOffset() * 60_000).toISOString().slice(0, 16);
}

export function CausalEffectPanel({ scope, model }: { scope: CausalScope; model: ModelReview }) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);
  const assumptionKinds = ASSUMPTION_KINDS.map((kind) => ({ kind, label: t(`causalCuration.effect.assumption.${kind}`) }));
  const reasonSep = t('causalCuration.reasonSeparator');
  const candidates = model.variables.filter((variable) =>
    variable.id !== model.model.treatment_variable_id
    && variable.id !== model.model.outcome_variable_id
    && (variable.kind === 'continuous' || variable.kind === 'count'));
  const [variableId, setVariableId] = useState('');
  const [assumptions, setAssumptions] = useState<AssumptionView | null>(null);
  const [assumptionKind, setAssumptionKind] = useState<AssumptionKind>('data_availability');
  const [assumptionVerdict, setAssumptionVerdict] = useState<'pass' | 'fail' | 'unknown'>('unknown');
  const [assumptionRationale, setAssumptionRationale] = useState('');
  const [view, setView] = useState<NegativeControlView | null>(null);
  const [protocolId, setProtocolId] = useState('');
  const [protocolExcerpt, setProtocolExcerpt] = useState('');
  const [protocolExternalId, setProtocolExternalId] = useState('');
  const [protocolVersion, setProtocolVersion] = useState('v1');
  const [protocolLineage, setProtocolLineage] = useState('');
  const [protocolContent, setProtocolContent] = useState('');
  const [protocolOccurredAt, setProtocolOccurredAt] = useState(localDateTime);
  const [rationale, setRationale] = useState('');
  const [verdict, setVerdict] = useState<'pass' | 'fail' | 'unknown'>('unknown');
  const [datasetExternalId, setDatasetExternalId] = useState('');
  const [datasetVersion, setDatasetVersion] = useState('v1');
  const [datasetLineage, setDatasetLineage] = useState('');
  const [datasetJson, setDatasetJson] = useState('');
  const [observedAt, setObservedAt] = useState(localDateTime);
  const [retentionDays, setRetentionDays] = useState(365);
  const [retentionAt, setRetentionAt] = useState(() => expiry(365));
  const [estimateId, setEstimateId] = useState('');
  const [result, setResult] = useState<EffectResult | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [notice, setNotice] = useState('');

  useEffect(() => {
    setVariableId(candidates[0]?.id ?? '');
    setAssumptions(null); setView(null); setProtocolId(''); setProtocolExcerpt(''); setResult(null); setEstimateId('');
  }, [model.model.id, scope.tenant_id, scope.acl]);

  useEffect(() => {
    let active = true;
    causalApi.assumptions(scope, model.model.id)
      .then((next) => { if (active) setAssumptions(next); })
      .catch((err) => { if (active) setError(errorText(err)); });
    return () => { active = false; };
  }, [scope, model.model.id]);

  useEffect(() => {
    if (!variableId) { setView(null); setProtocolId(''); setProtocolExcerpt(''); return; }
    let active = true;
    setView(null); setProtocolId(''); setProtocolExcerpt('');
    causalApi.negativeControlReview(scope, model.model.id, variableId)
      .then((next) => {
        if (!active) return;
        setView(next);
        setProtocolId(next.review?.protocol_artifact_id ?? '');
      })
      .catch((err) => { if (active) setError(errorText(err)); });
    return () => { active = false; };
  }, [scope, model.model.id, variableId]);

  async function run(action: () => Promise<void>) {
    setBusy(true); setError(''); setNotice('');
    try { await action(); }
    catch (err) { setError(errorText(err)); }
    finally { setBusy(false); }
  }

  async function addProtocol() {
    const occurred = Math.floor(new Date(protocolOccurredAt).getTime() / 1000);
    if (!protocolExternalId.trim() || !protocolVersion.trim() || !protocolLineage.trim()
      || !protocolContent.trim() || !Number.isInteger(retentionDays) || retentionDays < 1
      || !Number.isFinite(occurred)) {
      setError(t('causalCuration.effect.protocolValidation')); return;
    }
    await run(async () => {
      const response = await causalApi.addNegativeControlProtocol(scope, {
        external_id: protocolExternalId.trim(), version: protocolVersion.trim(),
        lineage_id: protocolLineage.trim(), content: protocolContent.trim(),
        occurred_at: occurred, retention_at: retentionAt,
      });
      setProtocolId(response.artifact.id);
      setProtocolExcerpt(protocolContent.trim());
      setNotice(t('causalCuration.effect.protocolCreated', { id: response.artifact.id }));
    });
  }

  async function submitReview() {
    if (!variableId || !protocolId.trim() || rationale.trim().length < 20) {
      setError(t('causalCuration.effect.reviewValidation')); return;
    }
    await run(async () => {
      await causalApi.reviewNegativeControl(scope, {
        model_id: model.model.id, variable_id: variableId,
        protocol_artifact_id: protocolId.trim(), verdict, rationale: rationale.trim(),
        expected_review_id: view?.review?.id ?? null,
      });
      const current = await causalApi.negativeControlReview(scope, model.model.id, variableId);
      setView(current);
      setNotice(t('causalCuration.effect.reviewRecorded'));
    });
  }

  async function submitAssumption() {
    if (!assumptionRationale.trim()) { setError(t('causalCuration.effect.assumptionValidation')); return; }
    await run(async () => {
      const prior = assumptions?.reviews.find((item) => item.kind === assumptionKind);
      await causalApi.reviewAssumption(scope, {
        model_id: model.model.id, kind: assumptionKind, verdict: assumptionVerdict,
        rationale: assumptionRationale.trim(), expected_review_id: prior?.review_id ?? null,
      });
      setAssumptions(await causalApi.assumptions(scope, model.model.id));
      setNotice(t('causalCuration.effect.assumptionRecorded'));
    });
  }

  async function estimate() {
    if (!datasetExternalId.trim() || !datasetVersion.trim() || !datasetLineage.trim()
      || !Number.isInteger(retentionDays) || retentionDays < 1) {
      setError(t('causalCuration.effect.datasetValidation')); return;
    }
    const occurred = Math.floor(new Date(observedAt).getTime() / 1000);
    if (!Number.isFinite(occurred)) { setError(t('causalCuration.effect.observedAtValidation')); return; }
    let dataset: ObservedDataset;
    try {
      dataset = JSON.parse(datasetJson) as ObservedDataset;
      if (!dataset || !Array.isArray(dataset.units) || !Array.isArray(dataset.adjustment_variable_ids)) {
        throw new Error(t('causalCuration.effect.datasetShapeError'));
      }
    } catch (err) { setError(t('causalCuration.effect.datasetJsonInvalid', { error: errorText(err) })); return; }
    await run(async () => {
      const response = await causalApi.estimateEffect(scope, {
        model_id: model.model.id, external_id: datasetExternalId.trim(),
        version: datasetVersion.trim(), lineage_id: datasetLineage.trim(),
        occurred_at: occurred, retention_at: retentionAt, dataset,
      });
      setResult(response.result);
      setEstimateId(response.result.state === 'estimated' ? response.result.estimate.id : '');
      setNotice(t('causalCuration.effect.datasetSaved', { id: response.dataset_artifact_id }));
    });
  }

  return <section className="space-y-4 border-t pt-4" aria-label={t('causalCuration.effect.sectionTitle')}>
    <h3 className="font-semibold">{t('causalCuration.effect.sectionTitle')}</h3>
    <p className="text-xs text-muted-foreground">{t('causalCuration.effect.sectionNote')}</p>
    {error && <p role="alert" className="rounded-md bg-destructive/10 p-2 text-destructive">{error}</p>}
    {notice && <p role="status" className="rounded-md bg-emerald-500/10 p-2">{notice}</p>}
    <div className="space-y-2 rounded-md border p-3">
      <h4 className="font-medium">{t('causalCuration.effect.assumptionTitle')}</h4>
      <p className="text-xs text-muted-foreground">{t('causalCuration.effect.assumptionNote')}</p>
      <div className="grid gap-1 text-xs sm:grid-cols-2">{assumptionKinds.map(({ kind, label }) => {
        const review = assumptions?.reviews.find((item) => item.kind === kind);
        return <p key={kind}>{label}：{review?.verdict ?? t('causalCuration.effect.notReviewed')}{review ? ` · ${review.reviewer}` : ''}</p>;
      })}</div>
      <p className="text-xs">{t('causalCuration.effect.readinessPrefix')}{assumptions?.readiness.state ?? t('causalCuration.effect.loading')}{assumptions?.readiness.reasons?.length ? `（${assumptions.readiness.reasons.join(reasonSep)}）` : ''}</p>
      <div className="flex flex-wrap gap-2"><select aria-label={t('causalCuration.effect.assumptionKindAria')} className="rounded-md border bg-background p-2" value={assumptionKind} onChange={(event) => setAssumptionKind(event.target.value as AssumptionKind)}>
        {assumptionKinds.map(({ kind, label }) => <option key={kind} value={kind}>{label}</option>)}
      </select><select aria-label={t('causalCuration.effect.assumptionVerdictAria')} className="rounded-md border bg-background p-2" value={assumptionVerdict} onChange={(event) => setAssumptionVerdict(event.target.value as typeof assumptionVerdict)}>
        <option value="unknown">{t('causalCuration.effect.verdict.unknown')}</option><option value="pass">{t('causalCuration.effect.verdict.pass')}</option><option value="fail">{t('causalCuration.effect.verdict.fail')}</option>
      </select></div>
      <Textarea aria-label={t('causalCuration.effect.assumptionRationaleAria')} value={assumptionRationale} onChange={(event) => setAssumptionRationale(event.target.value)} />
      <Button disabled={busy || !assumptions} onClick={submitAssumption}>{t('causalCuration.effect.submitAssumption')}</Button>
    </div>
    <div className="space-y-2 rounded-md border p-3">
      <label className="block">{t('causalCuration.effect.controlVariableLabel')}
        <select aria-label={t('causalCuration.effect.controlVariableLabel')} className="mt-1 w-full rounded-md border bg-background p-2"
          value={variableId} onChange={(event) => setVariableId(event.target.value)}>
          {candidates.length === 0 && <option value="">{t('causalCuration.effect.noControlVariables')}</option>}
          {candidates.map((variable) => <option key={variable.id} value={variable.id}>{variable.name}@{variable.version}</option>)}
        </select>
      </label>
      {view?.review && <p className="text-xs">{t('causalCuration.effect.latestReviewPrefix')}{view.review.verdict} · {view.review.reviewer} · {view.review.id}<br />{view.review.rationale}</p>}
      {view && <p className="text-xs">{t('causalCuration.effect.currentStatePrefix')}{view.readiness.state}{view.readiness.reasons?.length ? `（${view.readiness.reasons.join(reasonSep)}）` : ''}</p>}
      <div className="grid gap-2 sm:grid-cols-3">
        <label>{t('causalCuration.effect.protocolIdLabel')}<Input aria-label={t('causalCuration.effect.protocolIdLabel')} value={protocolExternalId} onChange={(event) => setProtocolExternalId(event.target.value)} /></label>
        <label>{t('causalCuration.effect.versionLabel')}<Input aria-label={t('causalCuration.effect.protocolVersionAria')} value={protocolVersion} onChange={(event) => setProtocolVersion(event.target.value)} /></label>
        <label>{t('causalCuration.effect.lineageLabel')}<Input aria-label={t('causalCuration.effect.protocolLineageAria')} value={protocolLineage} onChange={(event) => setProtocolLineage(event.target.value)} /></label>
        <label>{t('causalCuration.effect.protocolOccurredLabel')}<Input aria-label={t('causalCuration.effect.protocolOccurredLabel')} type="datetime-local" value={protocolOccurredAt} onChange={(event) => setProtocolOccurredAt(event.target.value)} /></label>
      </div>
      <Textarea aria-label={t('causalCuration.effect.protocolContentAria')} placeholder={t('causalCuration.effect.protocolContentPlaceholder')} value={protocolContent} onChange={(event) => setProtocolContent(event.target.value)} />
      <Button variant="outline" disabled={busy} onClick={addProtocol}>{t('causalCuration.effect.createProtocol')}</Button>
      <label className="block">{t('causalCuration.effect.protocolArtifactIdLabel')}<Input aria-label={t('causalCuration.effect.protocolArtifactIdLabel')} value={protocolId} onChange={(event) => {
        setProtocolId(event.target.value); setProtocolExcerpt('');
      }} /></label>
      <Button variant="outline" disabled={busy || !protocolId.trim()} onClick={() => run(async () => {
        const chunk = await causalApi.source(scope, protocolId.trim(), 0);
        setProtocolExcerpt(chunk.text);
        setNotice(chunk.truncated ? t('causalCuration.effect.protocolTruncatedNotice') : t('causalCuration.effect.protocolReloadedNotice'));
      })}>{t('causalCuration.effect.viewProtocolSource')}</Button>
      {protocolExcerpt && <pre className="max-h-48 overflow-auto whitespace-pre-wrap break-words rounded-md bg-muted p-2 text-xs">{protocolExcerpt}</pre>}
      <Textarea aria-label={t('causalCuration.effect.reviewRationaleAria')} placeholder={t('causalCuration.effect.reviewRationalePlaceholder')} value={rationale} onChange={(event) => setRationale(event.target.value)} />
      <div className="flex items-center gap-2"><select aria-label={t('causalCuration.effect.reviewVerdictAria')} className="rounded-md border bg-background p-2" value={verdict} onChange={(event) => setVerdict(event.target.value as typeof verdict)}>
        <option value="unknown">{t('causalCuration.effect.verdict.unknown')}</option><option value="pass">{t('causalCuration.effect.verdict.pass')}</option><option value="fail">{t('causalCuration.effect.verdict.fail')}</option>
      </select><Button disabled={busy || !variableId || model.effective_state !== 'approved'} onClick={submitReview}>{t('causalCuration.effect.submitReview')}</Button></div>
    </div>
    <div className="space-y-2 rounded-md border p-3">
      <h4 className="font-medium">{t('causalCuration.effect.datasetTitle')}</h4>
      <p className="text-xs text-muted-foreground">{t('causalCuration.effect.datasetNote')}</p>
      <div className="grid gap-2 sm:grid-cols-3">
        <label>{t('causalCuration.effect.datasetIdLabel')}<Input aria-label={t('causalCuration.effect.datasetIdLabel')} value={datasetExternalId} onChange={(event) => setDatasetExternalId(event.target.value)} /></label>
        <label>{t('causalCuration.effect.versionLabel')}<Input aria-label={t('causalCuration.effect.datasetVersionAria')} value={datasetVersion} onChange={(event) => setDatasetVersion(event.target.value)} /></label>
        <label>{t('causalCuration.effect.lineageLabel')}<Input aria-label={t('causalCuration.effect.datasetLineageAria')} value={datasetLineage} onChange={(event) => setDatasetLineage(event.target.value)} /></label>
        <label>{t('causalCuration.effect.observedAtLabel')}<Input aria-label={t('causalCuration.effect.observedAtLabel')} type="datetime-local" value={observedAt} onChange={(event) => setObservedAt(event.target.value)} /></label>
        <label>{t('causalCuration.effect.retentionDaysLabel')}<Input aria-label={t('causalCuration.effect.retentionDaysLabel')} type="number" min={1} value={retentionDays} onChange={(event) => {
          const days = Number(event.target.value); setRetentionDays(days);
          if (Number.isInteger(days) && days > 0) setRetentionAt(expiry(days));
        }} /></label>
      </div>
      <Textarea aria-label={t('causalCuration.effect.datasetJsonAria')} rows={10} value={datasetJson} onChange={(event) => setDatasetJson(event.target.value)} placeholder='{"adjustment_variable_ids":[],"negative_control":null,"units":[]}' />
      <Button disabled={busy} onClick={estimate}>{t('causalCuration.effect.saveAndEstimate')}</Button>
      <div className="flex gap-2"><Input aria-label={t('causalCuration.effect.estimateIdAria')} placeholder={t('causalCuration.effect.estimateIdPlaceholder')} value={estimateId} onChange={(event) => setEstimateId(event.target.value)} />
        <Button variant="outline" disabled={busy || !estimateId.trim()} onClick={() => run(async () => setResult(await causalApi.effect(scope, estimateId.trim())))}>{t('causalCuration.effect.reload')}</Button></div>
      {result?.state === 'unknown' && <p role="status" className="text-amber-700">{t('causalCuration.effect.unknownPrefix')}{result.reasons.join(reasonSep)}</p>}
      {result?.state === 'estimated' && <div className="space-y-1 rounded-md bg-muted p-2">
        <p>{t('causalCuration.effect.contrastLine', { estimate: result.estimate.estimate, lower: result.estimate.lower_bound, upper: result.estimate.upper_bound })}</p>
        {result.estimate.negative_control && <p>{t('causalCuration.effect.negControlLine', {
          contrast: result.estimate.negative_control.contrast, lower: result.estimate.negative_control.lower_bound,
          upper: result.estimate.negative_control.upper_bound,
          flag: result.estimate.negative_control.imbalance_flag ? t('causalCuration.effect.yes') : t('causalCuration.effect.no'),
        })}</p>}
        <p className="text-xs text-muted-foreground">{result.estimate.identification_state}{t('causalCuration.effect.identificationNote')}</p>
      </div>}
    </div>
  </section>;
}
