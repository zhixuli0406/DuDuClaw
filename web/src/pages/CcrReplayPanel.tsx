import { useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { FileJson, Play } from 'lucide-react';
import { Button, Card, CardContent } from '@/components/mds';
import { ccrApi, type CcrReplayArmReport, type CcrReplayResult } from '@/lib/ccr-api';

const MAX_REPLAY_BYTES = 16_000_000;

const limitationKeys: Record<string, string> = {
  'This replays exact-value scoring and condition checks from local user-supplied evidence; it does not rerun a model.': 'ccrReplay.limit.modelNotRerun',
  'ChatResponse rounds, provider/model identity, upstream authorization, and elapsed time are not independently attested.': 'ccrReplay.limit.unattestedClaims',
  'Dashboard CCR metrics are tenant aggregates without task labels and are not substituted for paired observations.': 'ccrReplay.limit.dashboardNotPaired',
  'No independently checked billing receipt was supplied; cost and cost-per-success are unavailable.': 'ccrReplay.limit.noBilling',
  'Synthetic observations validate the evaluator only; they do not show live task quality or savings.': 'ccrReplay.limit.syntheticOnly',
  'A complete task comparison also needs identical prompts, fixtures, model settings, and source permissions across arms.': 'ccrReplay.limit.fixedConditions',
};

export function CcrReplayPanel({ tenant }: { tenant: string }) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const number = (value: number) => new Intl.NumberFormat(intl.locale).format(value);
  const percent = (value: number) => new Intl.NumberFormat(intl.locale, {
    style: 'percent', maximumFractionDigits: 1,
  }).format(value);
  const [file, setFile] = useState<File | null>(null);
  const [result, setResult] = useState<CcrReplayResult | null>(null);
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);
  const generation = useRef(0);
  const input = useRef<HTMLInputElement>(null);

  useEffect(() => {
    generation.current += 1;
    setFile(null);
    setResult(null);
    setError('');
    setBusy(false);
    return () => { generation.current += 1; };
  }, [tenant]);

  function selectFile(next: File | null) {
    generation.current += 1;
    setFile(next);
    setResult(null);
    setError(next && next.size > MAX_REPLAY_BYTES ? t('ccrReplay.tooLarge') : '');
    setBusy(false);
  }

  async function runReplay() {
    const exactTenant = tenant.trim();
    if (!exactTenant) { setError(t('ccrDashboard.tenantRequired')); return; }
    if (!file) { setError(t('ccrReplay.fileRequired')); return; }
    if (file.size > MAX_REPLAY_BYTES) { setError(t('ccrReplay.tooLarge')); return; }
    const request = ++generation.current;
    setBusy(true);
    setResult(null);
    setError('');
    try {
      const evidenceJson = await file.text();
      if (generation.current !== request) return;
      try { JSON.parse(evidenceJson); }
      catch { setError(t('ccrReplay.invalidJson')); return; }
      const report = await ccrApi.replay(exactTenant, evidenceJson);
      if (generation.current === request) setResult(report);
    } catch {
      if (generation.current === request) setError(t('ccrReplay.requestFailed'));
    } finally {
      if (generation.current === request) setBusy(false);
    }
  }

  const armName = (arm: CcrReplayArmReport['arm']) => t(`ccrReplay.arm.${arm}`);
  const nullable = (value: number | null) => value === null ? t('ccrReplay.unavailable') : number(value);
  const nullablePercent = (value: number | null) => value === null ? t('ccrReplay.unavailable') : percent(value);
  const changedTasks = (arm: CcrReplayArmReport) => {
    const count = result?.source_delivery?.find((item) => item.arm === arm.arm)?.changed_tasks;
    return count === undefined ? t('ccrReplay.unavailable') : `${number(count)} / ${number(arm.tasks)}`;
  };

  return <Card><CardContent className="space-y-4">
    <div>
      <h2 className="text-lg font-semibold">{t('ccrReplay.title')}</h2>
      <p className="text-sm text-muted-foreground">{t('ccrReplay.description')}</p>
    </div>
    <div className="flex flex-wrap items-center gap-3">
      <input ref={input} type="file" accept=".json,application/json" className="sr-only"
        aria-label={t('ccrReplay.fileInput')}
        onChange={(event) => selectFile(event.target.files?.[0] ?? null)} />
      <Button type="button" variant="outline" onClick={() => input.current?.click()}>
        <FileJson className="mr-2 size-4" />{t('ccrReplay.chooseFile')}
      </Button>
      <span className="text-sm text-muted-foreground" role="status">
        {file ? t('ccrReplay.fileSelected') : t('ccrReplay.noFile')}
      </span>
      <Button type="button" disabled={busy || !file || file.size > MAX_REPLAY_BYTES || !tenant.trim()}
        onClick={runReplay}>
        <Play className="mr-2 size-4" />{busy ? t('ccrReplay.running') : t('ccrReplay.run')}
      </Button>
    </div>
    <p className="text-xs text-muted-foreground">{t('ccrReplay.uploadNote')}</p>
    {error && <p role="alert" className="rounded-md bg-destructive/10 p-3 text-sm text-destructive">{error}</p>}
    {result && <section aria-label={t('ccrReplay.results')} className="space-y-4">
      <div className="grid gap-3 text-sm sm:grid-cols-2 lg:grid-cols-4">
        <Fact label={t('ccrReplay.origin')} value={t(`ccrReplay.origin.${result.evidence_origin}`)} />
        <Fact label={t('ccrReplay.qualityMethod')} value={t('ccrReplay.exactValueCheck')} />
        <Fact label={t('ccrReplay.pairedTasks')} value={number(result.report.paired_tasks)} />
        <Fact label={t('ccrReplay.billing')} value={t('ccrReplay.billingUnavailable')} />
      </div>
      <div className="overflow-x-auto">
        <table className="w-full min-w-[1050px] text-left text-sm">
          <caption className="mb-2 text-left font-semibold">{t('ccrReplay.results')}</caption>
          <thead><tr className="border-b">
            <th className="p-2">{t('ccrReplay.arm')}</th>
            <th className="p-2">{t('ccrReplay.success')}</th>
            <th className="p-2">{t('ccrReplay.deliveryChanged')}</th>
            <th className="p-2">{t('ccrReplay.retrieval')}</th>
            <th className="p-2">{t('ccrReplay.p95')}</th>
            <th className="p-2">{t('ccrReplay.outputTokens')}</th>
            <th className="p-2">{t('ccrReplay.cacheRead')}</th>
            <th className="p-2">{t('ccrReplay.usageCoverage')}</th>
            <th className="p-2">{t('ccrReplay.totalCost')}</th>
            <th className="p-2">{t('ccrReplay.costPerSuccess')}</th>
          </tr></thead>
          <tbody>{result.report.arms.map((arm) => <tr key={arm.arm} className="border-b align-top">
            <th scope="row" className="p-2 font-medium">{armName(arm.arm)}</th>
            <td className="p-2">{number(arm.successes)} / {number(arm.tasks)}<br />{percent(arm.success_rate)}</td>
            <td className="p-2">{changedTasks(arm)}</td>
            <td className="p-2">{number(arm.retrieval_misses)} / {number(arm.retrieval_attempts)}<br />{nullablePercent(arm.retrieval_miss_rate)}</td>
            <td className="p-2">{number(arm.p95_latency_millis)}</td>
            <td className="p-2">{number(arm.output_tokens)}</td>
            <td className="p-2">{number(arm.cache_read_tokens)}<br />{nullablePercent(arm.cache_input_ratio)}</td>
            <td className="p-2">{number(arm.usage_complete_tasks)} / {number(arm.tasks)}</td>
            <td className="p-2">{nullable(arm.total_cost_millicents)}</td>
            <td className="p-2">{nullable(arm.cost_per_success_millicents)}</td>
          </tr>)}</tbody>
        </table>
      </div>
      <p className="text-xs text-muted-foreground">{t('ccrReplay.deliveryNote')}</p>
      <div className="grid gap-3 text-sm sm:grid-cols-2">
        <Fact label={t('ccrReplay.successDelta')} value={percent(result.report.ccr_success_delta_vs_raw)} />
        <Fact label={t('ccrReplay.costDelta')} value={nullable(result.report.ccr_cost_per_success_delta_vs_raw_millicents)} />
      </div>
      <div className="rounded-md border border-amber-500/40 bg-amber-500/5 p-3 text-sm">
        <h3 className="font-medium">{t('ccrReplay.limitations')}</h3>
        <p className="mt-1">{t('ccrReplay.claimWarning')}</p>
        <ul className="mt-2 list-disc space-y-1 pl-5">
          {[...result.limitations, ...result.report.limitations].map((item, index) =>
            <li key={index}>{t(limitationKeys[item] ?? 'ccrReplay.limit.additional')}</li>)}
        </ul>
      </div>
    </section>}
  </CardContent></Card>;
}

function Fact({ label, value }: { label: string; value: string }) {
  return <div><p className="text-xs text-muted-foreground">{label}</p><p className="font-medium tabular-nums">{value}</p></div>;
}
