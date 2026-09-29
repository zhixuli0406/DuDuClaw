import { useEffect, useRef, useState } from 'react';
import { Badge, Button, Card, CardContent, Input } from '@/components/mds';
import { decisionApi, type DecisionPilotUploadInput, type DecisionPilotUploadReceipt,
  type QueueModelInput, type StaffingScenarioInput, type SupportPilotExport } from '@/lib/decision-api';

type Translate = (id: string) => string;
type Scope = { tenant_id: string; acl: string };

const MAX_UPLOAD_BODY_BYTES = Math.floor(2.2 * 1024 * 1024);
const MAX_EXPORT_BYTES = 2 * 1024 * 1024 + 65_536;
const MAX_MODEL_BYTES = 16 * 1024;
const MAX_SCENARIO_BYTES = 16 * 1024;

function jsonObject(text: string): Record<string, unknown> {
  // All numeric fields in these three Rust inputs are integers. Browser JSON
  // parsing must not silently round an operator's u64 seed or staffing cost.
  const value: unknown = JSON.parse(text, (_key: string, part: unknown) => {
    if (typeof part === 'number' && !Number.isSafeInteger(part)) throw new Error('unsafe JSON integer');
    return part;
  });
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('invalid JSON object');
  return value as Record<string, unknown>;
}

function validReceipt(receipt: DecisionPilotUploadReceipt, input: DecisionPilotUploadInput): boolean {
  const digest = (value: string) => /^[a-f0-9]{64}$/.test(value);
  return receipt.status === 'exploratory_operator_upload'
    && receipt.queue_id === input.expected_queue_id
    && receipt.snapshot_id === input.export.snapshot_id
    && receipt.model_version === input.model.version
    && receipt.baseline_scenario_id === input.export.baseline_scenario_id
    && receipt.alternative_scenario_id === input.alternative_scenario.id
    && !!receipt.source_artifact_id && input.export.source_version_hashes?.length === 1
    && receipt.source_sha256 === input.export.source_version_hashes[0] && digest(receipt.source_sha256)
    && digest(receipt.baseline_replay_hash) && digest(receipt.alternative_replay_hash);
}

export function DecisionPilotUploadPanel({ scope, t, onImported, blocked, onBusyChange }: {
  scope: Scope;
  t: Translate;
  onImported: (receipt: DecisionPilotUploadReceipt, current: () => boolean) => Promise<void>;
  blocked: boolean;
  onBusyChange: (busy: boolean) => void;
}) {
  const [exportFile, setExportFile] = useState<File | null>(null);
  const [modelFile, setModelFile] = useState<File | null>(null);
  const [scenarioFile, setScenarioFile] = useState<File | null>(null);
  const [queueId, setQueueId] = useState('');
  const [lineage, setLineage] = useState('');
  const [retention, setRetention] = useState('');
  const [receipt, setReceipt] = useState<DecisionPilotUploadReceipt | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const revision = useRef(0);
  const mounted = useRef(true);

  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; revision.current += 1; onBusyChange(false); };
  }, [onBusyChange]);

  function changed() {
    if (busy) return;
    revision.current += 1;
    setReceipt(null);
    setError('');
    setBusy(false);
  }

  async function upload() {
    if (busy || blocked) return;
    if (!exportFile || !modelFile || !scenarioFile || !queueId.trim() || !lineage.trim() || !retention.trim()) {
      setError(t('decisionLab.uploadRequired'));
      return;
    }
    if (queueId.trim() !== queueId || queueId.length > 128
      || exportFile.size > MAX_EXPORT_BYTES || modelFile.size > MAX_MODEL_BYTES
      || scenarioFile.size > MAX_SCENARIO_BYTES) {
      setError(t('decisionLab.uploadInvalid'));
      return;
    }
    const retentionDate = new Date(retention);
    if (!retention.endsWith('Z') || Number.isNaN(retentionDate.getTime())
      || retentionDate.getTime() <= Date.now()) {
      setError(t('decisionLab.uploadRetentionInvalid'));
      return;
    }
    const currentRevision = ++revision.current;
    const current = () => mounted.current && revision.current === currentRevision;
    setBusy(true); onBusyChange(true); setError(''); setReceipt(null);
    try {
      const [exportText, modelText, scenarioText] = await Promise.all([
        exportFile.text(), modelFile.text(), scenarioFile.text(),
      ]);
      if (!current()) return;
      const input: DecisionPilotUploadInput = {
        ...scope,
        expected_queue_id: queueId,
        source_lineage: lineage.trim(),
        retention_until_utc: retention,
        export: jsonObject(exportText) as unknown as SupportPilotExport,
        model: jsonObject(modelText) as unknown as QueueModelInput,
        alternative_scenario: jsonObject(scenarioText) as unknown as StaffingScenarioInput,
      };
      if (new TextEncoder().encode(JSON.stringify(input)).length > MAX_UPLOAD_BODY_BYTES) {
        setError(t('decisionLab.uploadInvalid'));
        return;
      }
      const nextReceipt = await decisionApi.importPilot(input);
      if (!current()) return;
      if (!validReceipt(nextReceipt, input)) {
        setError(t('decisionLab.uploadMismatch'));
        return;
      }
      await onImported(nextReceipt, current);
      if (current()) setReceipt(nextReceipt);
    } catch {
      if (current()) setError(t('decisionLab.uploadFailed'));
    } finally {
      if (mounted.current) { setBusy(false); onBusyChange(false); }
    }
  }

  const fileFields: Array<{ id: string; file: File | null; set: (file: File | null) => void }> = [
    { id: 'decisionLab.uploadExport', file: exportFile, set: setExportFile },
    { id: 'decisionLab.uploadModel', file: modelFile, set: setModelFile },
    { id: 'decisionLab.uploadAlternative', file: scenarioFile, set: setScenarioFile },
  ];

  return <Card><CardContent className="space-y-4">
    <div><h2 className="font-semibold">{t('decisionLab.uploadTitle')}</h2>
      <p className="mt-1 text-sm text-muted-foreground">{t('decisionLab.uploadHint')}</p></div>
    <div className="grid gap-3 sm:grid-cols-3">
      {fileFields.map(({ id, file, set }) => <label key={id} className="block space-y-1 text-sm">
        <span>{t(id)}</span>
        <input aria-label={t(id)} type="file" accept="application/json,.json" className="sr-only" disabled={busy || blocked}
          onChange={(event) => { changed(); set(event.target.files?.[0] ?? null); }} />
        <span className="block rounded-md border px-2 py-1">{file ? t('decisionLab.uploadSelected') : t('decisionLab.uploadChoose')}</span>
      </label>)}
    </div>
    <div className="grid gap-3 sm:grid-cols-3">
      <label className="space-y-1 text-sm">{t('decisionLab.uploadQueue')}<Input aria-label={t('decisionLab.uploadQueue')}
        value={queueId} disabled={busy || blocked} onChange={(event) => { changed(); setQueueId(event.target.value); }} /></label>
      <label className="space-y-1 text-sm">{t('decisionLab.uploadLineage')}<Input aria-label={t('decisionLab.uploadLineage')}
        value={lineage} disabled={busy || blocked} onChange={(event) => { changed(); setLineage(event.target.value); }} /></label>
      <label className="space-y-1 text-sm">{t('decisionLab.uploadRetention')}<Input aria-label={t('decisionLab.uploadRetention')}
        placeholder="2027-01-01T00:00:00Z" value={retention} disabled={busy || blocked}
        onChange={(event) => { changed(); setRetention(event.target.value); }} /></label>
    </div>
    <Button disabled={busy || blocked || !exportFile || !modelFile || !scenarioFile || !queueId || !lineage || !retention}
      onClick={upload}>{busy ? t('decisionLab.uploading') : t('decisionLab.uploadSubmit')}</Button>
    {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
    {receipt && <div className="space-y-2 rounded-md border p-3 text-sm" role="status">
      <Badge variant="secondary">{t('decisionLab.uploadExploratory')}</Badge>
      <p>{t('decisionLab.uploadSuccess')}</p>
      <dl className="grid gap-2 sm:grid-cols-2">
        {([
          ['decisionLab.uploadQueue', receipt.queue_id],
          ['decisionLab.uploadSnapshot', receipt.snapshot_id],
          ['decisionLab.uploadModelVersion', receipt.model_version],
          ['decisionLab.uploadBaseline', receipt.baseline_scenario_id],
          ['decisionLab.uploadAlternative', receipt.alternative_scenario_id],
          ['decisionLab.uploadSourceId', receipt.source_artifact_id],
          ['decisionLab.uploadSourceSha', receipt.source_sha256],
          ['decisionLab.uploadBaselineHash', receipt.baseline_replay_hash],
          ['decisionLab.uploadAlternativeHash', receipt.alternative_replay_hash],
        ] as const).map(([label, value]) => <div key={label} className="min-w-0"><dt className="text-xs text-muted-foreground">{t(label)}</dt>
          <dd className="break-all">{value}</dd></div>)}
      </dl>
      {receipt.sla_holdout_id && <p className="break-all">{t('decisionLab.uploadHoldout')}: {receipt.sla_holdout_id}</p>}
      {receipt.sla_holdout_sha256 && <p className="break-all">{t('decisionLab.uploadHoldoutSha')}: {receipt.sla_holdout_sha256}</p>}
      {receipt.sla_holdout_unavailable_reason && <p>{t('decisionLab.uploadHoldoutUnavailable')}: {receipt.sla_holdout_unavailable_reason}</p>}
      {receipt.limitations.length > 0 && <div><p className="font-medium">{t('decisionLab.limitations')}</p>
        <ul className="list-disc pl-5">{receipt.limitations.map((item) => <li key={item}>{item}</li>)}</ul></div>}
    </div>}
  </CardContent></Card>;
}
