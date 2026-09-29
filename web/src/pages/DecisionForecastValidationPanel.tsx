import { useState } from 'react';
import { Badge, Button, Card, CardContent } from '@/components/mds';
import { decisionApi, type DecisionForecastValidation } from '@/lib/decision-api';

type Translate = (id: string, values?: Record<string, string | number>) => string;
type Selection = { snapshot_id: string; model_version: string;
  baseline_scenario_id: string; alternative_scenario_id: string };

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function coverage(bps: number | null, locale: string, unavailable: string): string {
  return bps === null ? unavailable
    : `${new Intl.NumberFormat(locale, { maximumFractionDigits: 1 }).format(bps / 100)}%`;
}

export function DecisionForecastValidationPanel({ scope, selection, origin, blocked, locale, t,
  onValidation, onBusyChange }: {
  scope: { tenant_id: string; acl: string };
  selection: Selection;
  origin: 'synthetic' | 'uploaded';
  blocked: boolean;
  locale: string;
  t: Translate;
  onValidation: (report: DecisionForecastValidation | null) => void;
  onBusyChange: (busy: boolean) => void;
}) {
  const [busy, setBusy] = useState(false);
  const [report, setReport] = useState<DecisionForecastValidation | null>(null);
  const [error, setError] = useState('');

  async function run() {
    setBusy(true); onBusyChange(true);
    setReport(null); onValidation(null); setError('');
    try {
      const { report: next } = await decisionApi.forecastValidation({ ...scope, ...selection });
      const aggregateErrors = [next.arrival_abs_error_sum, next.model_abs_error_sum,
        next.no_change_abs_error_sum, next.seasonal_naive_abs_error_sum,
        next.mean_change_abs_error_sum, next.model_beats_all_baselines];
      // The sums arrive as decimal strings: they can exceed the exact integer
      // range of a JSON number, so they are never parsed as one.
      const decimalSums = [next.arrival_abs_error_sum, next.model_abs_error_sum,
        next.no_change_abs_error_sum, next.seasonal_naive_abs_error_sum,
        next.mean_change_abs_error_sum];
      const intervalPairs = [
        [next.rolling_interval_evaluated_points, next.rolling_observed_coverage_basis_points],
        [next.fixed_interval_evaluated_points, next.fixed_observed_coverage_basis_points],
      ];
      if (next.source_origin !== origin
        || next.status !== (origin === 'synthetic' ? 'synthetic_only_exploratory' : 'operator_supplied_exploratory')
        || next.snapshot_id !== selection.snapshot_id || next.model_version !== selection.model_version
        || next.baseline_scenario_id !== selection.baseline_scenario_id
        || next.alternative_scenario_id !== selection.alternative_scenario_id
        || !/^[a-f0-9]{64}$/.test(next.source_sha256)
        || (next.record_id === null) !== (next.record_sha256 === null)
        || (next.record_sha256 !== null && !/^[a-f0-9]{64}$/.test(next.record_sha256))
        || (next.record_id === null && (!next.unavailable_reason || next.forecast_evaluation_days !== null
          || aggregateErrors.some((value) => value !== null)))
        || (next.record_id !== null && (next.unavailable_reason !== null
          || !next.forecast_evaluation_days || aggregateErrors.some((value) => value === null)))
        || decimalSums.some((value) => value !== null && !/^\d+$/.test(value))
        || intervalPairs.some(([count, bps]) => (count === null) !== (bps === null)
          || (bps !== null && (bps < 0 || bps > 10_000)))) {
        throw new Error(t('decisionLab.forecastMismatch'));
      }
      setReport(next); onValidation(next);
    } catch (cause) { setError(errorMessage(cause)); }
    finally { setBusy(false); onBusyChange(false); }
  }

  const unavailable = t('decisionLab.eventUnavailable');
  return <Card><CardContent className="space-y-3" aria-label={t('decisionLab.forecastTitle')}>
    <div className="flex flex-wrap items-center gap-2">
      <h2 className="font-semibold">{t('decisionLab.forecastTitle')}</h2>
      <Badge variant="secondary">{t(origin === 'synthetic' ? 'decisionLab.syntheticBadge' : 'decisionLab.uploadBadge')}</Badge>
    </div>
    <p className="text-sm text-muted-foreground">{t('decisionLab.forecastHint')}</p>
    <Button disabled={blocked || busy} onClick={run}>{busy ? t('decisionLab.forecastRunning') : t('decisionLab.forecastRun')}</Button>
    {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
    {report && <div className="space-y-2 rounded border p-3 text-sm">
      {report.unavailable_reason ? <p>{t('decisionLab.forecastUnavailable')}: {report.unavailable_reason}</p>
        : <div className="grid gap-2 sm:grid-cols-3">
          <div><p className="text-xs text-muted-foreground">{t('decisionLab.forecastEvaluationDays')}</p>
            <p className="font-medium tabular-nums">{report.forecast_evaluation_days ?? unavailable}</p></div>
          <div><p className="text-xs text-muted-foreground">{t('decisionLab.forecastRollingCoverage')}</p>
            <p className="font-medium tabular-nums">{coverage(report.rolling_observed_coverage_basis_points, locale, unavailable)}</p>
            <p className="text-xs text-muted-foreground">{t('decisionLab.forecastEvaluatedPoints')}: {report.rolling_interval_evaluated_points ?? unavailable}</p></div>
          <div><p className="text-xs text-muted-foreground">{t('decisionLab.forecastFixedCoverage')}</p>
            <p className="font-medium tabular-nums">{coverage(report.fixed_observed_coverage_basis_points, locale, unavailable)}</p>
            <p className="text-xs text-muted-foreground">{t('decisionLab.forecastEvaluatedPoints')}: {report.fixed_interval_evaluated_points ?? unavailable}</p></div>
        </div>}
      {report.record_id && <div className="space-y-2">
        <p className="text-xs text-muted-foreground">{t('decisionLab.forecastArrivalError')}: {report.arrival_abs_error_sum === null
          ? unavailable : new Intl.NumberFormat(locale).format(BigInt(report.arrival_abs_error_sum))}</p>
        <div className="overflow-x-auto"><table className="w-full min-w-[28rem] text-left text-xs">
          <caption className="pb-1 text-left font-medium">{t('decisionLab.forecastBacklogErrors')}</caption>
          <thead><tr><th scope="col">{t('decisionLab.forecastModel')}</th><th scope="col">{t('decisionLab.forecastNoChange')}</th>
            <th scope="col">{t('decisionLab.forecastSeasonal')}</th><th scope="col">{t('decisionLab.forecastMeanChange')}</th></tr></thead>
          <tbody><tr>{[report.model_abs_error_sum, report.no_change_abs_error_sum,
            report.seasonal_naive_abs_error_sum, report.mean_change_abs_error_sum].map((value, index) =>
            <td key={index} className="tabular-nums">{value === null ? unavailable : new Intl.NumberFormat(locale).format(BigInt(value))}</td>)}</tr></tbody>
        </table></div>
        <p className="text-xs">{t('decisionLab.forecastBeatsBaselines')}: {report.model_beats_all_baselines === null
          ? unavailable : t(report.model_beats_all_baselines ? 'decisionLab.yes' : 'decisionLab.no')}</p>
      </div>}
      {report.interval_unavailable_reason && <p className="text-xs text-muted-foreground">{report.interval_unavailable_reason}</p>}
      {report.record_id && <p className="break-all font-mono text-xs">{t('decisionLab.forecastRecordId')}: {report.record_id} · {t('decisionLab.forecastRecordSha')}: {report.record_sha256}</p>}
      <p className="break-all font-mono text-xs">{t('decisionLab.uploadSourceSha')}: {report.source_sha256}</p>
      {report.limitations.length > 0 && <ul className="list-disc space-y-1 pl-5 text-xs text-muted-foreground">
        {report.limitations.map((limit, index) => <li key={index}>{limit}</li>)}
      </ul>}
    </div>}
  </CardContent></Card>;
}
