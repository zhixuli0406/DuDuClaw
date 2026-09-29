import { useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { Button, Card, CardContent } from '@/components/mds';
import {
  causalApi,
  type CausalScope,
  type ExtractionEvalCount,
  type ExtractionEvalDataset,
  type ExtractionEvalResult,
} from '@/lib/causal-api';

const MAX_UPLOAD_BYTES = 1_800_000;

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function ratio(value: number | null): string {
  return value === null ? '—' : `${(value * 100).toFixed(1)}%`;
}

const METRIC_KEYS: Array<[string, keyof Pick<ExtractionEvalResult['report'],
  'full_claim' | 'exact_span' | 'direction_claim' | 'stance_claim' | 'modality_claim'
  | 'lag_claim' | 'negation_claim' | 'independent_lineage_claim' | 'reviewed_edge_case'>]> = [
  ['causalCuration.eval.metric.fullClaim', 'full_claim'],
  ['causalCuration.eval.metric.exactSpan', 'exact_span'],
  ['causalCuration.eval.metric.direction', 'direction_claim'],
  ['causalCuration.eval.metric.stance', 'stance_claim'],
  ['causalCuration.eval.metric.modality', 'modality_claim'],
  ['causalCuration.eval.metric.lag', 'lag_claim'],
  ['causalCuration.eval.metric.negation', 'negation_claim'],
  ['causalCuration.eval.metric.independentLineage', 'independent_lineage_claim'],
  ['causalCuration.eval.metric.reviewedEdge', 'reviewed_edge_case'],
];

function MetricRow({ label, count }: { label: string; count: ExtractionEvalCount }) {
  return <tr className="border-t">
    <th scope="row" className="py-2 pr-3 text-left font-medium">{label}</th>
    <td className="py-2 pr-3 text-right">{count.true_positive}/{count.predicted}/{count.gold}</td>
    <td className="py-2 pr-3 text-right">{ratio(count.precision)}</td>
    <td className="py-2 text-right">{ratio(count.recall)}</td>
  </tr>;
}

export function CausalExtractionEvalPanel({ scope }: { scope: CausalScope }) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);
  const [file, setFile] = useState<File | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [result, setResult] = useState<ExtractionEvalResult | null>(null);
  const requestSequence = useRef(0);

  useEffect(() => () => { requestSequence.current += 1; }, []);

  async function run(mode: 'bundled_synthetic' | 'scoped_dataset') {
    const sequence = ++requestSequence.current;
    setBusy(true);
    setError('');
    setResult(null);
    try {
      let next: ExtractionEvalResult;
      if (mode === 'bundled_synthetic') {
        next = await causalApi.evaluateExtractionSynthetic(scope);
      } else {
        if (!file || file.size > MAX_UPLOAD_BYTES) {
          throw new Error(t('causalCuration.eval.fileTooLarge'));
        }
        const uploaded: unknown = JSON.parse(await file.text());
        if (!isRecord(uploaded) || !isRecord(uploaded.dataset) || !isRecord(uploaded.case_artifacts)) {
          throw new Error(t('causalCuration.eval.jsonShapeError'));
        }
        next = await causalApi.evaluateExtractionScoped(
          scope,
          uploaded.dataset as unknown as ExtractionEvalDataset,
          uploaded.case_artifacts as Record<string, string>,
        );
      }
      if (sequence !== requestSequence.current) return;
      if (next.scope.tenant_id !== scope.tenant_id || next.scope.acl !== scope.acl || next.mode !== mode) {
        throw new Error(t('causalCuration.eval.scopeMismatch'));
      }
      setResult(next);
    } catch (cause) {
      if (sequence === requestSequence.current) setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      if (sequence === requestSequence.current) setBusy(false);
    }
  }

  return <div className="space-y-4">
    <Card><CardContent className="space-y-3">
      <h2 className="font-semibold">{t('causalCuration.eval.title')}</h2>
      <p className="text-sm text-muted-foreground">
        {t('causalCuration.eval.note')}
      </p>
      <div className="flex flex-wrap items-center gap-2">
        <Button disabled={busy} onClick={() => void run('bundled_synthetic')}>{t('causalCuration.eval.runBundled')}</Button>
        <label className="text-sm">
          {t('causalCuration.eval.uploadLabel')}
          <input
            aria-label={t('causalCuration.eval.uploadAria')}
            className="ml-2 max-w-64 text-xs"
            type="file"
            accept=".json,application/json"
            disabled={busy}
            onChange={(event) => { setFile(event.target.files?.[0] ?? null); setResult(null); setError(''); }}
          />
        </label>
        <Button variant="outline" disabled={busy || !file} onClick={() => void run('scoped_dataset')}>{t('causalCuration.eval.runScoped')}</Button>
      </div>
      <p className="text-xs text-muted-foreground">
        {t('causalCuration.eval.uploadFormatNote', {
          example: `{ "dataset": { ...v2 cases... }, "case_artifacts": { "case-id": "${t('causalCuration.eval.uploadFormatCaseIdHint')}" } }`,
        })}
      </p>
    </CardContent></Card>
    {error && <p role="alert" className="rounded-md bg-destructive/10 p-3 text-sm text-destructive">{error}</p>}
    {result && <Card><CardContent className="space-y-3">
      <h2 className="font-semibold">{t('causalCuration.eval.resultTitle')}</h2>
      <p className="text-xs text-muted-foreground">
        {result.mode === 'bundled_synthetic'
          ? t('causalCuration.eval.bundledNote')
          : t('causalCuration.eval.scopedNote')}
      </p>
      <p className="text-sm">
        {t('causalCuration.eval.summaryLine', {
          training: result.report.training_cases, heldOut: result.report.held_out_cases,
          heldOutFamilies: result.report.held_out_families, reviewedFamilies: result.report.reviewed_families,
        })}
      </p>
      <p className="text-xs text-muted-foreground">
        {t('causalCuration.eval.detailLine', {
          invalid: result.report.invalid_responses, ungrounded: result.report.ungrounded_predictions,
          rejected: result.report.rejected_edges_proposed,
        })}
      </p>
      <div className="overflow-x-auto">
        <table className="w-full min-w-[560px] text-sm">
          <thead><tr className="text-muted-foreground">
            <th className="py-2 text-left">{t('causalCuration.eval.header.metric')}</th>
            <th className="py-2 text-right">{t('causalCuration.eval.header.tpPredictedGold')}</th>
            <th className="py-2 text-right">{t('causalCuration.eval.header.precision')}</th>
            <th className="py-2 text-right">{t('causalCuration.eval.header.recall')}</th>
          </tr></thead>
          <tbody>{METRIC_KEYS.map(([labelKey, key]) => <MetricRow key={key} label={t(labelKey)} count={result.report[key]} />)}</tbody>
        </table>
      </div>
      <p className="break-all text-xs text-muted-foreground">{t('causalCuration.eval.datasetFingerprint', { sha: result.report.dataset_sha256 })}</p>
      <ul className="list-disc space-y-1 pl-5 text-xs text-muted-foreground">
        {result.report.limitations.map((item) => <li key={item}>{item}</li>)}
      </ul>
    </CardContent></Card>}
  </div>;
}
