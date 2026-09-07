import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import {
  AlertTriangle,
  Ban,
  Cpu,
  Download,
  FlaskConical,
  HardDriveDownload,
  Play,
  RefreshCw,
  Trash2,
} from 'lucide-react';

import {
  api,
  type FineTuneBackendId,
  type FineTuneDataset,
  type FineTuneFormat,
  type FineTuneJob,
  type FineTuneJobConfig,
  type FineTuneMethod,
} from '@/lib/api';
import { useConnectionStore } from '@/stores/connection-store';
import { toast } from '@/lib/toast';
import { useUrlState } from '@/lib/use-url-state';
import {
  Badge,
  Button,
  Card,
  CardContent,
  Checkbox,
  CollectionPageState,
  Input,
  Tabs,
  TabsList,
  TabsTab,
  useErrorMessage,
} from '@/components/mds';

/**
 * 微調與後訓練 (WP-E, `TODO-ai-runtimes-2026-09.md` decision 4C).
 *
 * The honest framing this page has to carry all the way through its copy:
 * **curate here, train elsewhere, deploy here.** This machine has integrated
 * graphics; it cannot train, and every 2026 post-training toolchain assumes
 * CUDA or ROCm. So the page builds a dataset out of the conversations, task
 * results and approval decisions already stored on the box, ships it to a
 * GPU the user supplies, and imports the resulting GGUF/LoRA back into the
 * local-models list.
 *
 * Two rules the UI must not break:
 *
 * 1. Never imply local training. Every tab says where the GPU is.
 * 2. Never show progress the gateway did not observe. Jobs display a state
 *    plus the verbatim tail of the real training log — there is no progress
 *    bar, because there is no measured percentage to put in one.
 */

type TabId = 'datasets' | 'jobs' | 'import';
const TABS: readonly TabId[] = ['datasets', 'jobs', 'import'];

const BACKENDS: readonly FineTuneBackendId[] = ['dry_run', 'remote_gpu_ssh', 'together'];
const FORMATS: readonly FineTuneFormat[] = ['sharegpt', 'alpaca'];
const METHODS: readonly FineTuneMethod[] = ['sft', 'dpo'];

const ACK_CODE = 'data_leaves_device_not_acknowledged';

/** The gateway rejects with the raw `{ code, message }` error object. */
function rpcError(e: unknown): { code?: string; message?: string } {
  if (e && typeof e === 'object') return e as { code?: string; message?: string };
  return { message: String(e) };
}

function isAckRequired(e: unknown): boolean {
  return rpcError(e).code === ACK_CODE;
}

const MB = 1024 * 1024;
function fmtBytes(n: number): string {
  if (n >= MB) return `${(n / MB).toFixed(1)} MB`;
  if (n >= 1024) return `${Math.round(n / 1024)} KB`;
  return `${n} B`;
}

function stateTone(state: FineTuneJob['state']): string {
  switch (state) {
    case 'succeeded':
      return 'bg-emerald-500';
    case 'running':
    case 'preparing':
      return 'bg-amber-500';
    case 'failed':
      return 'bg-rose-500';
    default:
      return 'bg-muted-foreground';
  }
}

export function FineTunePage() {
  const intl = useIntl();
  const t = useCallback(
    (id: string, values?: Record<string, string | number>) =>
      intl.formatMessage({ id }, values),
    [intl],
  );
  const errorText = useErrorMessage();
  const connectionState = useConnectionStore((s) => s.state);
  const [tab, setTab] = useUrlState('tab', 'datasets');
  const activeTab = (TABS as readonly string[]).includes(tab) ? (tab as TabId) : 'datasets';

  // Shared: the RPC message when present (it is written for operators), the
  // generic classifier otherwise.
  const showError = useCallback(
    (e: unknown) => toast.error(rpcError(e).message ?? errorText(e)),
    [errorText],
  );

  // ── datasets ──
  const [datasets, setDatasets] = useState<FineTuneDataset[]>([]);
  const [loading, setLoading] = useState(true);
  const [failed, setFailed] = useState(false);
  const [newName, setNewName] = useState('');
  const [newFormat, setNewFormat] = useState<FineTuneFormat>('sharegpt');
  const [selected, setSelected] = useState<string>('');
  const [buildAgents, setBuildAgents] = useState('');
  const [buildSince, setBuildSince] = useState('');
  const [withApprovals, setWithApprovals] = useState(true);
  const [withTasks, setWithTasks] = useState(true);
  const [withPrefs, setWithPrefs] = useState(true);
  const [preview, setPreview] = useState<unknown[] | null>(null);
  const [exportAck, setExportAck] = useState(false);
  const [exported, setExported] = useState<string | null>(null);

  const refreshDatasets = useCallback(() => {
    api.finetune.datasets
      .list()
      .then((r) => {
        setDatasets(r?.datasets ?? []);
        setFailed(false);
      })
      .catch((e) => {
        setFailed(true);
        showError(e);
      })
      .finally(() => setLoading(false));
  }, [showError]);

  // ── jobs ──
  const [jobs, setJobs] = useState<FineTuneJob[]>([]);
  const [backend, setBackend] = useState<FineTuneBackendId>('dry_run');
  const [method, setMethod] = useState<FineTuneMethod>('sft');
  const [baseModel, setBaseModel] = useState('Qwen/Qwen3-4B-Instruct');
  const [template, setTemplate] = useState('qwen');
  const [loraRank, setLoraRank] = useState(16);
  const [epochs, setEpochs] = useState(3);
  const [lr, setLr] = useState(0.00005);
  const [sshHost, setSshHost] = useState('');
  const [sshUser, setSshUser] = useState('');
  const [sshKey, setSshKey] = useState('');
  const [sshWorkdir, setSshWorkdir] = useState('/srv/duduclaw-train');
  const [sshPython, setSshPython] = useState('/opt/llamafactory/venv/bin/python');
  const [sshLlamaCpp, setSshLlamaCpp] = useState('');
  const [togetherModel, setTogetherModel] = useState('');
  const [jobAck, setJobAck] = useState(false);
  const [openLog, setOpenLog] = useState<string | null>(null);
  const pollTimer = useRef<ReturnType<typeof setInterval> | null>(null);

  const refreshJobs = useCallback(() => {
    api.finetune.jobs
      .list()
      .then((r) => setJobs(r?.jobs ?? []))
      .catch(() => {});
  }, []);

  // Poll only the jobs that are actually moving, and only while some are.
  const tickJobs = useCallback(() => {
    api.finetune.jobs
      .list()
      .then(async (r) => {
        const list = r?.jobs ?? [];
        const live = list.filter((j) => j.state === 'running' || j.state === 'preparing');
        if (live.length === 0) {
          if (pollTimer.current) {
            clearInterval(pollTimer.current);
            pollTimer.current = null;
          }
          setJobs(list);
          return;
        }
        const refreshed = await Promise.all(
          live.map((j) => api.finetune.jobs.status(j.id).then((s) => s.job).catch(() => j)),
        );
        setJobs(list.map((j) => refreshed.find((x) => x.id === j.id) ?? j));
      })
      .catch(() => {});
  }, []);

  const startPolling = useCallback(() => {
    if (pollTimer.current) return;
    pollTimer.current = setInterval(tickJobs, 5000);
  }, [tickJobs]);

  useEffect(
    () => () => {
      if (pollTimer.current) clearInterval(pollTimer.current);
    },
    [],
  );

  useEffect(() => {
    if (connectionState !== 'authenticated') return;
    setLoading(true);
    refreshDatasets();
    refreshJobs();
  }, [connectionState, refreshDatasets, refreshJobs]);

  useEffect(() => {
    if (jobs.some((j) => j.state === 'running' || j.state === 'preparing')) startPolling();
  }, [jobs, startPolling]);

  const current = useMemo(
    () => datasets.find((d) => d.id === selected) ?? datasets[0] ?? null,
    [datasets, selected],
  );

  // ── dataset actions ──

  const createDataset = () => {
    api.finetune.datasets
      .create(newName.trim(), newFormat)
      .then((r) => {
        setNewName('');
        setSelected(r.dataset.id);
        refreshDatasets();
      })
      .catch(showError);
  };

  const buildDataset = () => {
    if (!current) return;
    api.finetune.datasets
      .build(
        current.id,
        {
          agent_ids: buildAgents
            .split(',')
            .map((s) => s.trim())
            .filter(Boolean),
          since: buildSince.trim() || null,
          include_approvals: withApprovals,
          include_task_results: withTasks,
        },
        current.format,
        withPrefs,
      )
      .then((r) => {
        toast.success(
          t('finetune.dataset.built', {
            sft: r.dataset.counts.sft_rows,
            dpo: r.dataset.counts.preference_rows,
          }),
        );
        setPreview(null);
        setExported(null);
        refreshDatasets();
      })
      .catch(showError);
  };

  const previewDataset = () => {
    if (!current) return;
    api.finetune.datasets
      .preview(current.id, 5)
      .then((r) => setPreview([...(r.train ?? []), ...(r.preference ?? [])]))
      .catch(showError);
  };

  const exportDataset = () => {
    if (!current) return;
    api.finetune.datasets
      .export(current.id, exportAck)
      .then((r) => setExported(r.path))
      .catch((e) => {
        if (isAckRequired(e)) {
          toast.error(t('finetune.privacy.needAck'));
          return;
        }
        showError(e);
      });
  };

  const deleteDataset = (id: string) => {
    api.finetune.datasets
      .remove(id)
      .then(() => {
        if (selected === id) setSelected('');
        refreshDatasets();
      })
      .catch(showError);
  };

  // ── job actions ──

  const remoteRequired = backend !== 'dry_run';
  // The SSH block is shown for `dry_run` too, so a dry run can plan the exact
  // remote command rather than a local stand-in — but only once every field
  // it needs is filled, otherwise the gateway rejects the half-built block.
  const sshComplete =
    !!sshHost.trim() && !!sshUser.trim() && !!sshKey.trim() && !!sshWorkdir.trim() && !!sshPython.trim();

  const buildJobConfig = (): FineTuneJobConfig | null => {
    if (!current) return null;
    return {
      dataset_id: current.id,
      backend,
      base_model: backend === 'together' ? togetherModel.trim() : baseModel.trim(),
      method,
      template: template.trim() || 'default',
      lora_rank: loraRank,
      lora_alpha: loraRank * 2,
      epochs,
      learning_rate: lr,
      cutoff_len: 2048,
      per_device_batch_size: 1,
      gradient_accumulation_steps: 8,
      remote:
        backend !== 'together' && sshComplete
          ? {
              host: sshHost.trim(),
              user: sshUser.trim(),
              key_path: sshKey.trim(),
              workdir: sshWorkdir.trim(),
              python: sshPython.trim(),
              llama_cpp_dir: sshLlamaCpp.trim() || null,
            }
          : null,
      together:
        backend === 'together' ? { model: togetherModel.trim(), suffix: 'duduclaw' } : null,
    };
  };

  const createJob = () => {
    const config = buildJobConfig();
    if (!config) return;
    api.finetune.jobs
      .create(config, jobAck)
      .then((r) => {
        setJobs((prev) => [r.job, ...prev]);
        toast.success(t(`finetune.job.started.${r.job.state === 'planned' ? 'dry' : 'remote'}`));
        startPolling();
      })
      .catch((e) => {
        if (isAckRequired(e)) {
          toast.error(t('finetune.privacy.needAck'));
          return;
        }
        showError(e);
      });
  };

  const cancelJob = (id: string) => {
    api.finetune.jobs
      .cancel(id)
      .then((r) => setJobs((prev) => prev.map((j) => (j.id === id ? r.job : j))))
      .catch(showError);
  };

  // ── import ──
  const [importPath, setImportPath] = useState('');
  const [imported, setImported] = useState<string | null>(null);

  const runImport = () => {
    api.finetune
      .import(importPath.trim())
      .then((r) => {
        setImported(r.path);
        setImportPath('');
        toast.success(t('finetune.import.done', { name: r.filename }));
      })
      .catch(showError);
  };

  return (
    <div className="mx-auto w-full max-w-[1200px] space-y-4">
      <div className="flex items-center gap-3">
        <FlaskConical className="size-5 shrink-0 text-brand" />
        <div className="space-y-0.5">
          <h1 className="text-lg font-semibold">{t('manage.finetune')}</h1>
          <p className="text-sm text-muted-foreground">{t('manage.finetune.desc')}</p>
        </div>
      </div>

      {/* The one thing this page must never let a user misunderstand. */}
      <Card data-size="sm">
        <CardContent className="flex items-start gap-2.5 text-sm">
          <Cpu className="mt-0.5 size-4 shrink-0 text-brand" aria-hidden="true" />
          <p className="text-muted-foreground">{t('finetune.banner.noLocalTraining')}</p>
        </CardContent>
      </Card>

      <Tabs value={activeTab} onValueChange={(v) => setTab(String(v))} variant="line">
        <TabsList>
          {TABS.map((id) => (
            <TabsTab key={id} value={id}>
              {t(`finetune.tab.${id}`)}
            </TabsTab>
          ))}
        </TabsList>
      </Tabs>

      {/* ─────────────── 資料集 ─────────────── */}
      {activeTab === 'datasets' && (
        <div className="space-y-4">
          <p className="text-sm text-muted-foreground">{t('finetune.datasets.intro')}</p>

          <Card data-size="sm">
            <CardContent className="flex flex-wrap items-center gap-2">
              <Input
                value={newName}
                onChange={(e) => setNewName(e.target.value)}
                placeholder={t('finetune.datasets.namePlaceholder')}
                className="max-w-xs"
              />
              <select
                value={newFormat}
                onChange={(e) => setNewFormat(e.target.value as FineTuneFormat)}
                aria-label={t('finetune.field.format')}
                className="rounded-md border border-input bg-transparent px-2 py-1 text-sm"
              >
                {FORMATS.map((f) => (
                  <option key={f} value={f}>
                    {t(`finetune.format.${f}`)}
                  </option>
                ))}
              </select>
              <Button size="sm" disabled={!newName.trim()} onClick={createDataset}>
                {t('finetune.datasets.create')}
              </Button>
            </CardContent>
          </Card>

          {loading ? (
            <CollectionPageState state="loading" />
          ) : failed ? (
            <CollectionPageState state="error" title={t('finetune.error.load')} />
          ) : datasets.length === 0 ? (
            <CollectionPageState
              state="empty"
              icon={FlaskConical}
              title={t('finetune.datasets.empty.title')}
              description={t('finetune.datasets.empty.desc')}
            />
          ) : (
            <div className="space-y-2">
              {datasets.map((d) => (
                <Card
                  key={d.id}
                  data-size="sm"
                  className={current?.id === d.id ? 'ring-1 ring-brand' : undefined}
                >
                  <CardContent className="space-y-2">
                    <div className="flex flex-wrap items-center gap-2">
                      <button
                        type="button"
                        className="text-sm font-medium text-foreground hover:underline"
                        onClick={() => setSelected(d.id)}
                      >
                        {d.name}
                      </button>
                      <Badge variant="outline">{t(`finetune.format.${d.format}`)}</Badge>
                      {d.built_at ? (
                        <span className="text-xs text-muted-foreground">
                          {t('finetune.dataset.counts', {
                            sft: d.counts.sft_rows,
                            dpo: d.counts.preference_rows,
                            size: fmtBytes(d.counts.bytes),
                          })}
                        </span>
                      ) : (
                        <span className="text-xs text-muted-foreground">
                          {t('finetune.dataset.notBuilt')}
                        </span>
                      )}
                      <Button
                        variant="ghost"
                        size="icon-sm"
                        className="ml-auto"
                        aria-label={t('common.delete')}
                        onClick={() => deleteDataset(d.id)}
                      >
                        <Trash2 className="size-3.5" />
                      </Button>
                    </div>
                    {d.built_at && (
                      <p className="text-xs text-muted-foreground">
                        {t('finetune.dataset.sourceCounts', {
                          conv: d.counts.conversations,
                          tasks: d.counts.task_results,
                          approvals: d.counts.approvals,
                        })}
                      </p>
                    )}
                  </CardContent>
                </Card>
              ))}
            </div>
          )}

          {current && (
            <Card data-size="sm">
              <CardContent className="space-y-3">
                <h2 className="text-sm font-medium text-foreground">
                  {t('finetune.build.title', { name: current.name })}
                </h2>
                <div className="grid gap-2 sm:grid-cols-2">
                  <label className="space-y-1 text-xs text-muted-foreground">
                    {t('finetune.build.agents')}
                    <Input
                      value={buildAgents}
                      onChange={(e) => setBuildAgents(e.target.value)}
                      placeholder={t('finetune.build.agentsPlaceholder')}
                    />
                  </label>
                  <label className="space-y-1 text-xs text-muted-foreground">
                    {t('finetune.build.since')}
                    <Input
                      value={buildSince}
                      onChange={(e) => setBuildSince(e.target.value)}
                      placeholder="2026-01-01"
                    />
                  </label>
                </div>
                <div className="flex flex-wrap gap-x-4 gap-y-2">
                  <label className="flex items-center gap-2 text-xs text-muted-foreground">
                    <Checkbox
                      checked={withTasks}
                      onCheckedChange={(c) => setWithTasks(c === true)}
                    />
                    {t('finetune.build.includeTasks')}
                  </label>
                  <label className="flex items-center gap-2 text-xs text-muted-foreground">
                    <Checkbox
                      checked={withApprovals}
                      onCheckedChange={(c) => setWithApprovals(c === true)}
                    />
                    {t('finetune.build.includeApprovals')}
                  </label>
                  <label className="flex items-center gap-2 text-xs text-muted-foreground">
                    <Checkbox
                      checked={withPrefs}
                      onCheckedChange={(c) => setWithPrefs(c === true)}
                    />
                    {t('finetune.build.preferencePairs')}
                  </label>
                </div>
                <div className="flex flex-wrap items-center gap-2">
                  <Button size="sm" onClick={buildDataset}>
                    <RefreshCw className="size-3.5" />
                    {t('finetune.build.run')}
                  </Button>
                  <Button variant="outline" size="sm" onClick={previewDataset}>
                    {t('finetune.build.preview')}
                  </Button>
                </div>

                {preview && (
                  <pre className="max-h-72 overflow-auto rounded-md bg-muted p-2 text-xs text-foreground">
                    {preview.length === 0
                      ? t('finetune.preview.empty')
                      : preview.map((row) => JSON.stringify(row, null, 2)).join('\n')}
                  </pre>
                )}

                {/* Export — data leaves the machine, so the consent is a
                    checkbox the user ticks, not a toast they dismiss. */}
                <div className="space-y-2 border-t border-surface-border pt-3">
                  <div className="flex items-start gap-2 text-xs text-amber-600 dark:text-amber-500">
                    <AlertTriangle className="mt-0.5 size-3.5 shrink-0" aria-hidden="true" />
                    <p>{t('finetune.privacy.exportWarning')}</p>
                  </div>
                  <label className="flex items-start gap-2 text-xs text-muted-foreground">
                    <Checkbox
                      className="mt-0.5"
                      checked={exportAck}
                      onCheckedChange={(c) => setExportAck(c === true)}
                    />
                    {t('finetune.privacy.ack')}
                  </label>
                  <Button
                    variant="outline"
                    size="sm"
                    disabled={!exportAck || !current.built_at}
                    onClick={exportDataset}
                  >
                    <Download className="size-3.5" />
                    {t('finetune.build.export')}
                  </Button>
                  {exported && (
                    <p className="break-all font-mono text-xs text-muted-foreground">{exported}</p>
                  )}
                </div>
              </CardContent>
            </Card>
          )}
        </div>
      )}

      {/* ─────────────── 訓練工作 ─────────────── */}
      {activeTab === 'jobs' && (
        <div className="space-y-4">
          <p className="text-sm text-muted-foreground">{t('finetune.jobs.intro')}</p>

          <Card data-size="sm">
            <CardContent className="space-y-3">
              <div className="grid gap-2 sm:grid-cols-2">
                <label className="space-y-1 text-xs text-muted-foreground">
                  {t('finetune.field.dataset')}
                  <select
                    value={current?.id ?? ''}
                    onChange={(e) => setSelected(e.target.value)}
                    className="w-full rounded-md border border-input bg-transparent px-2 py-1 text-sm"
                  >
                    {datasets.map((d) => (
                      <option key={d.id} value={d.id}>
                        {d.name}
                      </option>
                    ))}
                  </select>
                </label>
                <label className="space-y-1 text-xs text-muted-foreground">
                  {t('finetune.field.backend')}
                  <select
                    value={backend}
                    onChange={(e) => setBackend(e.target.value as FineTuneBackendId)}
                    className="w-full rounded-md border border-input bg-transparent px-2 py-1 text-sm"
                  >
                    {BACKENDS.map((b) => (
                      <option key={b} value={b}>
                        {t(`finetune.backend.${b}`)}
                      </option>
                    ))}
                  </select>
                </label>
              </div>
              <p className="text-xs text-muted-foreground">{t(`finetune.backend.${backend}.desc`)}</p>

              <div className="grid gap-2 sm:grid-cols-3">
                <label className="space-y-1 text-xs text-muted-foreground">
                  {t('finetune.field.method')}
                  <select
                    value={method}
                    onChange={(e) => setMethod(e.target.value as FineTuneMethod)}
                    className="w-full rounded-md border border-input bg-transparent px-2 py-1 text-sm"
                  >
                    {METHODS.map((m) => (
                      <option key={m} value={m}>
                        {t(`finetune.method.${m}`)}
                      </option>
                    ))}
                  </select>
                </label>
                <label className="space-y-1 text-xs text-muted-foreground">
                  {t('finetune.field.loraRank')}
                  <Input
                    type="number"
                    min={1}
                    max={256}
                    value={loraRank}
                    onChange={(e) => setLoraRank(Number(e.target.value) || 16)}
                  />
                </label>
                <label className="space-y-1 text-xs text-muted-foreground">
                  {t('finetune.field.epochs')}
                  <Input
                    type="number"
                    min={1}
                    max={100}
                    value={epochs}
                    onChange={(e) => setEpochs(Number(e.target.value) || 3)}
                  />
                </label>
              </div>

              {backend === 'together' ? (
                <label className="space-y-1 text-xs text-muted-foreground">
                  {t('finetune.field.togetherModel')}
                  <Input
                    value={togetherModel}
                    onChange={(e) => setTogetherModel(e.target.value)}
                    placeholder="meta-llama/Meta-Llama-3.1-8B-Instruct-Reference"
                  />
                </label>
              ) : (
                <div className="grid gap-2 sm:grid-cols-2">
                  <label className="space-y-1 text-xs text-muted-foreground">
                    {t('finetune.field.baseModel')}
                    <Input value={baseModel} onChange={(e) => setBaseModel(e.target.value)} />
                  </label>
                  <label className="space-y-1 text-xs text-muted-foreground">
                    {t('finetune.field.template')}
                    <Input value={template} onChange={(e) => setTemplate(e.target.value)} />
                  </label>
                </div>
              )}

              {backend !== 'together' && (
                <div className="space-y-2 rounded-md border border-surface-border p-2.5">
                  <h3 className="text-xs font-medium text-foreground">
                    {t('finetune.ssh.title')}
                  </h3>
                  <p className="text-xs text-muted-foreground">{t('finetune.ssh.hint')}</p>
                  <div className="grid gap-2 sm:grid-cols-2">
                    <Input
                      value={sshHost}
                      onChange={(e) => setSshHost(e.target.value)}
                      placeholder={t('finetune.ssh.host')}
                    />
                    <Input
                      value={sshUser}
                      onChange={(e) => setSshUser(e.target.value)}
                      placeholder={t('finetune.ssh.user')}
                    />
                    <Input
                      value={sshKey}
                      onChange={(e) => setSshKey(e.target.value)}
                      placeholder={t('finetune.ssh.key')}
                    />
                    <Input
                      value={sshWorkdir}
                      onChange={(e) => setSshWorkdir(e.target.value)}
                      placeholder={t('finetune.ssh.workdir')}
                    />
                    <Input
                      value={sshPython}
                      onChange={(e) => setSshPython(e.target.value)}
                      placeholder={t('finetune.ssh.python')}
                    />
                    <Input
                      value={sshLlamaCpp}
                      onChange={(e) => setSshLlamaCpp(e.target.value)}
                      placeholder={t('finetune.ssh.llamaCpp')}
                    />
                  </div>
                </div>
              )}

              <div className="space-y-1">
                <label className="space-y-1 text-xs text-muted-foreground">
                  {t('finetune.field.lr')}
                  <Input
                    type="number"
                    step="0.00001"
                    value={lr}
                    onChange={(e) => setLr(Number(e.target.value) || 0.00005)}
                    className="max-w-[12rem]"
                  />
                </label>
              </div>

              {remoteRequired && (
                <div className="space-y-2 border-t border-surface-border pt-3">
                  <div className="flex items-start gap-2 text-xs text-amber-600 dark:text-amber-500">
                    <AlertTriangle className="mt-0.5 size-3.5 shrink-0" aria-hidden="true" />
                    <p>{t(`finetune.privacy.job.${backend}`)}</p>
                  </div>
                  <label className="flex items-start gap-2 text-xs text-muted-foreground">
                    <Checkbox
                      className="mt-0.5"
                      checked={jobAck}
                      onCheckedChange={(c) => setJobAck(c === true)}
                    />
                    {t('finetune.privacy.ack')}
                  </label>
                </div>
              )}

              <Button
                size="sm"
                disabled={
                  !current ||
                  (remoteRequired && !jobAck) ||
                  (backend === 'remote_gpu_ssh' && !sshComplete) ||
                  (backend === 'together' && !togetherModel.trim())
                }
                onClick={createJob}
              >
                <Play className="size-3.5" />
                {t(backend === 'dry_run' ? 'finetune.jobs.dryRun' : 'finetune.jobs.start')}
              </Button>
            </CardContent>
          </Card>

          {jobs.length === 0 ? (
            <CollectionPageState
              state="empty"
              icon={Play}
              title={t('finetune.jobs.empty.title')}
              description={t('finetune.jobs.empty.desc')}
            />
          ) : (
            <div className="space-y-2">
              {jobs.map((j) => (
                <Card key={j.id} data-size="sm">
                  <CardContent className="space-y-2">
                    <div className="flex flex-wrap items-center gap-2 text-sm">
                      <span className={`size-2 shrink-0 rounded-full ${stateTone(j.state)}`} />
                      <span className="font-medium text-foreground">
                        {t(`finetune.state.${j.state}`)}
                      </span>
                      <Badge variant="outline">{t(`finetune.backend.${j.config.backend}`)}</Badge>
                      <Badge variant="outline">{t(`finetune.method.${j.config.method}`)}</Badge>
                      <span className="truncate text-xs text-muted-foreground">
                        {j.config.base_model}
                      </span>
                      <div className="ml-auto flex items-center gap-1.5">
                        {j.log_tail.length > 0 && (
                          <Button
                            variant="ghost"
                            size="sm"
                            onClick={() => setOpenLog(openLog === j.id ? null : j.id)}
                          >
                            {t('finetune.jobs.log')}
                          </Button>
                        )}
                        {(j.state === 'running' || j.state === 'preparing') && (
                          <Button
                            variant="ghost"
                            size="icon-sm"
                            aria-label={t('common.cancel')}
                            onClick={() => cancelJob(j.id)}
                          >
                            <Ban className="size-3.5" />
                          </Button>
                        )}
                      </div>
                    </div>
                    {j.detail && <p className="text-xs text-muted-foreground">{j.detail}</p>}
                    {j.error && (
                      <p className="text-xs text-rose-600 dark:text-rose-400">{j.error}</p>
                    )}
                    {j.artifacts.length > 0 && (
                      <ul className="space-y-0.5">
                        {j.artifacts.map((a) => (
                          <li
                            key={a.path}
                            className="flex items-center gap-2 text-xs text-muted-foreground"
                          >
                            <span className="truncate font-mono">{a.name}</span>
                            <span>{fmtBytes(a.size_bytes)}</span>
                            <Button
                              variant="ghost"
                              size="sm"
                              className="ml-auto"
                              onClick={() => {
                                setImportPath(a.path);
                                setTab('import');
                              }}
                            >
                              {t('finetune.jobs.sendToImport')}
                            </Button>
                          </li>
                        ))}
                      </ul>
                    )}
                    {openLog === j.id && (
                      <pre className="max-h-64 overflow-auto rounded-md bg-muted p-2 text-xs text-foreground">
                        {j.log_tail.join('\n')}
                      </pre>
                    )}
                  </CardContent>
                </Card>
              ))}
            </div>
          )}
        </div>
      )}

      {/* ─────────────── 匯入 ─────────────── */}
      {activeTab === 'import' && (
        <div className="space-y-4">
          <p className="text-sm text-muted-foreground">{t('finetune.import.intro')}</p>
          <Card data-size="sm">
            <CardContent className="space-y-2">
              <Input
                value={importPath}
                onChange={(e) => setImportPath(e.target.value)}
                placeholder={t('finetune.import.placeholder')}
              />
              <p className="text-xs text-muted-foreground">{t('finetune.import.hint')}</p>
              <Button size="sm" disabled={!importPath.trim()} onClick={runImport}>
                <HardDriveDownload className="size-3.5" />
                {t('finetune.import.run')}
              </Button>
              {imported && (
                <p className="break-all font-mono text-xs text-muted-foreground">{imported}</p>
              )}
            </CardContent>
          </Card>
        </div>
      )}
    </div>
  );
}

export default FineTunePage;
