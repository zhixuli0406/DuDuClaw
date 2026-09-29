import { useState } from 'react';
import { Badge, Button, Card, CardContent } from '@/components/mds';
import { decisionApi, type DecisionEventComparison, type DecisionEventScenario } from '@/lib/decision-api';

type Translate = (id: string, values?: Record<string, string | number>) => string;
type Selection = {
  snapshot_id: string;
  model_version: string;
  baseline_scenario_id: string;
  alternative_scenario_id: string;
};

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function seconds(value: number | null, locale: string, unavailable: string): string {
  return value === null ? unavailable : new Intl.NumberFormat(locale, { maximumFractionDigits: 1 }).format(value / 60);
}

function metric(value: number, locale: string): string {
  return new Intl.NumberFormat(locale).format(value);
}

export function DecisionEventComparePanel({ scope, selection, origin, blocked, locale, t, onComparison, onBusyChange }: {
  scope: { tenant_id: string; acl: string };
  selection: Selection;
  origin: 'synthetic' | 'uploaded';
  blocked: boolean;
  locale: string;
  t: Translate;
  onComparison?: (report: DecisionEventComparison | null) => void;
  onBusyChange?: (busy: boolean) => void;
}) {
  const [busy, setBusy] = useState(false);
  const [report, setReport] = useState<DecisionEventComparison | null>(null);
  const [error, setError] = useState('');

  async function run() {
    setBusy(true); setReport(null); setError('');
    onBusyChange?.(true);
    onComparison?.(null);
    try {
      const { report: next } = await decisionApi.eventCompare({ ...scope, ...selection });
      if (next.source_origin !== origin
        || next.status !== (origin === 'synthetic' ? 'synthetic_only_exploratory' : 'operator_supplied_exploratory')
        || next.baseline.scenario_id !== selection.baseline_scenario_id
        || next.alternative.scenario_id !== selection.alternative_scenario_id
        || ![next.source_sha256, next.event_engine_sha256, next.baseline.replay_hash,
          next.alternative.replay_hash].every((hash) => /^[a-f0-9]{64}$/.test(hash))) {
        throw new Error(t('decisionLab.eventMismatch'));
      }
      setReport(next);
      onComparison?.(next);
    } catch (cause) { setError(errorMessage(cause)); }
    finally { setBusy(false); onBusyChange?.(false); }
  }

  const rows: Array<{ key: string; value: (item: DecisionEventScenario) => string }> = [
    { key: 'decisionLab.eventWithinSla', value: (item) => metric(item.total_resolved_within_sla, locale) },
    { key: 'decisionLab.eventP50', value: (item) => seconds(item.wait_seconds_p50, locale, t('decisionLab.eventUnavailable')) },
    { key: 'decisionLab.eventP95', value: (item) => seconds(item.wait_seconds_p95, locale, t('decisionLab.eventUnavailable')) },
    { key: 'decisionLab.eventBacklog', value: (item) => metric(item.final_backlog, locale) },
    { key: 'decisionLab.eventCost', value: (item) => metric(item.total_staff_cost_cents, locale) },
  ];

  return <Card><CardContent className="space-y-3" aria-label={t('decisionLab.eventTitle')}>
    <div className="flex flex-wrap items-center gap-2">
      <h2 className="font-semibold">{t('decisionLab.eventTitle')}</h2>
      <Badge variant="secondary">{t(origin === 'synthetic' ? 'decisionLab.syntheticBadge' : 'decisionLab.uploadBadge')}</Badge>
    </div>
    <p className="text-sm text-muted-foreground">{t('decisionLab.eventHint')}</p>
    <Button disabled={blocked || busy} onClick={run}>{busy ? t('decisionLab.eventRunning') : t('decisionLab.eventRun')}</Button>
    {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
    {report && <div className="space-y-3">
      <div className="overflow-x-auto"><table className="w-full text-sm"><thead><tr className="border-b text-left">
        <th scope="col" className="py-2 pr-4">{t('decisionLab.metric')}</th>
        <th scope="col" className="py-2 pr-4">{t('decisionLab.baseline')}</th>
        <th scope="col" className="py-2">{t('decisionLab.alternative')}</th>
      </tr></thead><tbody>{rows.map((row) => <tr key={row.key} className="border-b last:border-0">
        <th scope="row" className="py-2 pr-4 text-left font-normal">{t(row.key)}</th>
        <td className="py-2 pr-4 tabular-nums">{row.value(report.baseline)}</td>
        <td className="py-2 tabular-nums">{row.value(report.alternative)}</td>
      </tr>)}</tbody></table></div>
      <div className="space-y-1 break-all font-mono text-xs text-muted-foreground">
        <p>{t('decisionLab.uploadSourceSha')}: {report.source_sha256}</p>
        <p>{t('decisionLab.eventEngineHash')}: {report.event_engine_sha256}</p>
        <p>{t('decisionLab.baselineHash')}: {report.baseline.replay_hash}</p>
        <p>{t('decisionLab.alternativeHash')}: {report.alternative.replay_hash}</p>
      </div>
      <p className="text-xs text-muted-foreground">{t('decisionLab.eventInterpretation')}</p>
      {report.limitations.length > 0 && <ul className="list-disc space-y-1 pl-5 text-xs text-muted-foreground">
        {report.limitations.map((limit, index) => <li key={index}>{limit}</li>)}
      </ul>}
    </div>}
  </CardContent></Card>;
}
