import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { Cpu, Download, HardDrive, Trash2, X } from 'lucide-react';

import {
  api,
  type MarketFit,
  type MarketHardware,
  type MarketInstallJob,
  type MarketModel,
  type MarketQuant,
} from '@/lib/api';
import { useConnectionStore } from '@/stores/connection-store';
import { useAuthStore } from '@/stores/auth-store';
import { hasMinRole } from '@/lib/roles';
import { toast } from '@/lib/toast';
import { useUrlState } from '@/lib/use-url-state';
import {
  Badge,
  Button,
  Card,
  CardContent,
  CollectionPageState,
  Input,
  Segmented,
  Skeleton,
  useErrorMessage,
  type SegmentedOption,
} from '@/components/mds';

/**
 * 本地模型市集 (design: DESIGN-local-model-marketplace-2026-08-13).
 *
 * Replaces the old curated-list UX: pick an INTENT, the gateway sweeps a
 * publisher-whitelisted HF index, and every card carries a tri-state
 * hardware-fit light computed for THIS machine — including the MoE
 * expert-offload dual track (the turbo-fieldfare lesson) that lets a 16GB
 * machine run a 30B-A3B. One click installs the auto-picked quant; the
 * advanced drawer exposes every quant + manual repo install.
 */

type IntentId = 'chat' | 'code' | 'long_context' | 'chinese';
const INTENTS: readonly IntentId[] = ['chat', 'code', 'long_context', 'chinese'];

const GB = 1024 * 1024 * 1024;

function fmtBytes(n: number): string {
  if (n >= GB) return `${(n / GB).toFixed(1)} GB`;
  return `${Math.max(1, Math.round(n / (1024 * 1024)))} MB`;
}

function fitTone(fit: MarketFit): string {
  switch (fit) {
    case 'comfortable':
      return 'bg-emerald-500';
    case 'tight':
      return 'bg-amber-500';
    default:
      return 'bg-rose-500';
  }
}

/** The better of full-load fit and MoE-offload fit — what the card leads with. */
function effectiveFit(q: MarketQuant): { fit: MarketFit; viaOffload: boolean } {
  const rank = (f: MarketFit) => (f === 'comfortable' ? 0 : f === 'tight' ? 1 : 2);
  if (q.fit_offload != null && rank(q.fit_offload) < rank(q.fit)) {
    return { fit: q.fit_offload, viaOffload: true };
  }
  return { fit: q.fit, viaOffload: false };
}

export function LocalModelsPage() {
  const intl = useIntl();
  const errorText = useErrorMessage();
  const connectionState = useConnectionStore((s) => s.state);
  const role = useAuthStore((s) => s.user?.role);
  const canInstall = hasMinRole(role, 'manager');

  const [intent, setIntent] = useUrlState('intent', 'chat');
  const [models, setModels] = useState<MarketModel[]>([]);
  const [hardware, setHardware] = useState<MarketHardware | null>(null);
  const [loading, setLoading] = useState(true);
  const [failed, setFailed] = useState(false);
  const [jobs, setJobs] = useState<MarketInstallJob[]>([]);
  const [installed, setInstalled] = useState<Array<{ filename: string; size_bytes: number }>>([]);
  const [expanded, setExpanded] = useState<string | null>(null);
  const [manualRepo, setManualRepo] = useState('');
  const pollTimer = useRef<ReturnType<typeof setInterval> | null>(null);

  const refreshInstalled = useCallback(() => {
    api.localmodels.installed().then((r) => setInstalled(r?.models ?? [])).catch(() => {});
  }, []);

  const refreshJobs = useCallback(() => {
    api.localmodels
      .installStatus()
      .then((r) => {
        const list = r?.jobs ?? [];
        setJobs(list);
        // Poll only while something is moving; a finished job refreshes the
        // installed listing once.
        const active = list.some((j) => j.state === 'downloading' || j.state === 'queued');
        if (!active && pollTimer.current) {
          clearInterval(pollTimer.current);
          pollTimer.current = null;
          refreshInstalled();
        }
      })
      .catch(() => {});
  }, [refreshInstalled]);

  const startPolling = useCallback(() => {
    if (pollTimer.current) return;
    pollTimer.current = setInterval(refreshJobs, 2000);
  }, [refreshJobs]);

  useEffect(() => () => {
    if (pollTimer.current) clearInterval(pollTimer.current);
  }, []);

  useEffect(() => {
    if (connectionState !== 'authenticated') return;
    let alive = true;
    setLoading(true);
    setFailed(false);
    api.localmodels
      .search(intent)
      .then((r) => {
        if (!alive) return;
        setModels(r?.models ?? []);
        setHardware(r?.hardware ?? null);
      })
      .catch((e) => {
        console.warn('[api]', e);
        if (alive) {
          setFailed(true);
          toast.error(
            intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: errorText(e) }),
          );
        }
      })
      .finally(() => alive && setLoading(false));
    refreshJobs();
    refreshInstalled();
    return () => {
      alive = false;
    };
    // errorText/intl stable; refresh fns stable via useCallback.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [connectionState, intent]);

  const install = (repo: string, q: MarketQuant) => {
    api.localmodels
      .install(repo, q.filename, q.shards ?? [], q.size_bytes)
      .then(() => {
        toast.success(intl.formatMessage({ id: 'localmodels.install.started' }));
        refreshJobs();
        startPolling();
      })
      .catch((e) => toast.error(errorText(e)));
  };

  const intentOptions: SegmentedOption<string>[] = useMemo(
    () =>
      INTENTS.map((id) => ({
        value: id,
        label: intl.formatMessage({ id: `localmodels.intent.${id}` }),
      })),
    [intl],
  );

  const activeJobs = jobs.filter((j) => j.state === 'downloading' || j.state === 'queued');

  return (
    <div className="mx-auto w-full max-w-[1200px] space-y-4">
      {/* Slim header — manage-pane idiom (matches InferencePage), not the
          collection-page banner. */}
      <div className="flex items-center gap-3">
        <HardDrive className="size-5 shrink-0 text-brand" />
        <div className="space-y-0.5">
          <h1 className="text-lg font-semibold">
            {intl.formatMessage({ id: 'manage.localModels' })}
          </h1>
          <p className="text-sm text-muted-foreground">
            {intl.formatMessage({ id: 'manage.localModels.desc' })}
          </p>
        </div>
      </div>

      {/* Hardware banner — the fit lights below are computed against this. */}
      {hardware && (
        <Card data-size="sm">
          <CardContent className="flex flex-wrap items-center gap-x-4 gap-y-1 text-sm">
            <span className="flex items-center gap-2 text-foreground">
              <Cpu className="size-4 text-brand" />
              {hardware.gpu_name}
            </span>
            <span className="text-muted-foreground">
              {intl.formatMessage(
                { id: 'localmodels.hw.summary' },
                {
                  ram: Math.round(hardware.ram_total_mb / 1024),
                  avail: Math.round(
                    Math.max(hardware.vram_available_mb, hardware.ram_available_mb) / 1024,
                  ),
                },
              )}
            </span>
          </CardContent>
        </Card>
      )}

      <div className="overflow-x-auto">
        <Segmented
          value={intent}
          onValueChange={setIntent}
          options={intentOptions}
          aria-label={intl.formatMessage({ id: 'manage.localModels' })}
        />
      </div>

      {/* Active installs. */}
      {activeJobs.map((j) => (
        <Card key={j.id} data-size="sm">
          <CardContent className="space-y-1.5">
            <div className="flex items-center gap-2 text-sm">
              <Download className="size-4 animate-pulse text-brand" />
              <span className="truncate text-foreground">{j.filename}</span>
              <span className="ml-auto font-mono text-xs tabular-nums text-muted-foreground">
                {fmtBytes(j.downloaded_bytes)}
                {j.total_bytes > 0 ? ` / ${fmtBytes(j.total_bytes)}` : ''}
              </span>
              <Button
                variant="ghost"
                size="icon-sm"
                aria-label={intl.formatMessage({ id: 'common.cancel' })}
                onClick={() =>
                  api.localmodels.cancel(j.id).then(refreshJobs).catch((e) => toast.error(errorText(e)))
                }
              >
                <X className="size-3.5" />
              </Button>
            </div>
            <div
              className="h-1.5 w-full overflow-hidden rounded-full bg-muted"
              role="progressbar"
              aria-valuemin={0}
              aria-valuemax={100}
              aria-valuenow={
                j.total_bytes > 0
                  ? Math.min(100, Math.round((j.downloaded_bytes / j.total_bytes) * 100))
                  : undefined
              }
            >
              <div
                className="h-full rounded-full bg-brand transition-all"
                style={{
                  width:
                    j.total_bytes > 0
                      ? `${Math.min(100, (j.downloaded_bytes / j.total_bytes) * 100)}%`
                      : '10%',
                }}
              />
            </div>
          </CardContent>
        </Card>
      ))}

      {/* Marketplace cards. */}
      {loading ? (
        <div className="grid gap-4 sm:grid-cols-2">
          <Skeleton className="h-40" />
          <Skeleton className="h-40" />
        </div>
      ) : failed ? (
        <CollectionPageState
          state="error"
          title={intl.formatMessage({ id: 'localmodels.error.title' })}
        />
      ) : models.length === 0 ? (
        <CollectionPageState
          state="empty"
          icon={HardDrive}
          title={intl.formatMessage({ id: 'localmodels.empty.title' })}
          description={intl.formatMessage({ id: 'localmodels.empty.desc' })}
        />
      ) : (
        <div className="grid gap-4 sm:grid-cols-2">
          {models.map((m) => {
            const rec = m.recommended;
            const eff = rec ? effectiveFit(rec) : null;
            return (
              <Card key={m.repo} data-size="sm">
                <CardContent className="space-y-2.5">
                  <div className="flex items-start gap-2">
                    <div className="min-w-0">
                      <h3 className="truncate text-sm font-medium text-foreground">{m.name}</h3>
                      <p className="truncate text-xs text-muted-foreground">
                        {m.publisher}
                        {m.params_b != null && ` · ${m.params_b.toFixed(0)}B`}
                        {m.moe &&
                          m.active_params_b != null &&
                          ` (${intl.formatMessage({ id: 'localmodels.moe.active' }, { n: m.active_params_b })})`}
                        {` · ${Intl.NumberFormat().format(m.downloads)} ⬇`}
                      </p>
                    </div>
                    {eff && (
                      <span
                        className="ml-auto mt-1 flex shrink-0 items-center gap-1.5 text-xs text-muted-foreground"
                        title={intl.formatMessage({ id: `localmodels.fit.${eff.fit}` })}
                      >
                        <span className={`size-2 rounded-full ${fitTone(eff.fit)}`} />
                        {intl.formatMessage({ id: `localmodels.fit.${eff.fit}` })}
                      </span>
                    )}
                  </div>

                  <div className="flex flex-wrap gap-1.5">
                    {m.moe && (
                      <Badge variant="outline">
                        {intl.formatMessage({ id: 'localmodels.badge.moe' })}
                      </Badge>
                    )}
                    {eff?.viaOffload && (
                      <Badge variant="outline">
                        {intl.formatMessage({ id: 'localmodels.badge.offload' })}
                      </Badge>
                    )}
                    {m.gated && (
                      <Badge variant="outline">
                        {intl.formatMessage({ id: 'localmodels.badge.gated' })}
                      </Badge>
                    )}
                    {m.context_length != null && m.context_length >= 131072 && (
                      <Badge variant="outline">
                        {intl.formatMessage({ id: 'localmodels.badge.longctx' })}
                      </Badge>
                    )}
                    {m.languages.includes('zh') && <Badge variant="outline">中文</Badge>}
                  </div>

                  <div className="flex items-center gap-2 border-t border-surface-border pt-2.5">
                    {rec ? (
                      <span className="text-xs text-muted-foreground">
                        {rec.quant} · {fmtBytes(rec.size_bytes)}
                      </span>
                    ) : (
                      <span className="text-xs text-muted-foreground">
                        {intl.formatMessage({ id: 'localmodels.nofit' })}
                      </span>
                    )}
                    <div className="ml-auto flex items-center gap-1.5">
                      <Button
                        variant="ghost"
                        size="sm"
                        onClick={() => setExpanded(expanded === m.repo ? null : m.repo)}
                      >
                        {intl.formatMessage({ id: 'localmodels.advanced' })}
                      </Button>
                      {rec && canInstall && !m.gated && (
                        <Button size="sm" onClick={() => install(m.repo, rec)}>
                          {intl.formatMessage({ id: 'localmodels.install' })}
                        </Button>
                      )}
                    </div>
                  </div>

                  {/* Advanced drawer: every quant with both fit tracks. */}
                  {expanded === m.repo && (
                    <div className="space-y-1 border-t border-surface-border pt-2">
                      {m.quants.map((q) => (
                        <div key={q.filename} className="flex items-center gap-2 text-xs">
                          <span className={`size-1.5 shrink-0 rounded-full ${fitTone(effectiveFit(q).fit)}`} />
                          <span className="font-mono text-foreground">{q.quant}</span>
                          {q.imatrix && <Badge variant="outline">imatrix</Badge>}
                          <span className="text-muted-foreground">{fmtBytes(q.size_bytes)}</span>
                          {q.fit_offload != null && (
                            <span className="text-muted-foreground">
                              {intl.formatMessage({ id: 'localmodels.offload.hint' })}
                              <span
                                className={`ml-1 inline-block size-1.5 rounded-full ${fitTone(q.fit_offload)}`}
                              />
                            </span>
                          )}
                          {canInstall && !m.gated && effectiveFit(q).fit !== 'too_big' && (
                            <Button
                              variant="ghost"
                              size="sm"
                              className="ml-auto"
                              onClick={() => install(m.repo, q)}
                            >
                              {intl.formatMessage({ id: 'localmodels.install' })}
                            </Button>
                          )}
                        </div>
                      ))}
                    </div>
                  )}
                </CardContent>
              </Card>
            );
          })}
        </div>
      )}

      {/* Installed models. */}
      {installed.length > 0 && (
        <section className="space-y-2">
          <h2 className="text-sm font-medium text-foreground">
            {intl.formatMessage({ id: 'localmodels.installed.title' })}
          </h2>
          <Card data-size="sm">
            <CardContent className="divide-y divide-surface-border">
              {installed.map((f) => (
                <div key={f.filename} className="flex items-center gap-2 py-1.5 text-xs">
                  <span className="truncate font-mono text-foreground">{f.filename}</span>
                  <span className="ml-auto text-muted-foreground">{fmtBytes(f.size_bytes)}</span>
                  {canInstall && (
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      aria-label={intl.formatMessage({ id: 'common.delete' })}
                      onClick={() =>
                        api.localmodels
                          .remove(f.filename)
                          .then(refreshInstalled)
                          .catch((e) => toast.error(errorText(e)))
                      }
                    >
                      <Trash2 className="size-3.5" />
                    </Button>
                  )}
                </div>
              ))}
            </CardContent>
          </Card>
        </section>
      )}

      {/* Manual repo install — the escape hatch replacing the old list UX. */}
      {canInstall && (
        <section className="space-y-2">
          <h2 className="text-sm font-medium text-foreground">
            {intl.formatMessage({ id: 'localmodels.manual.title' })}
          </h2>
          <div className="flex items-center gap-2">
            <Input
              value={manualRepo}
              onChange={(e) => setManualRepo(e.target.value)}
              placeholder="org/Model-GGUF"
              className="max-w-sm"
            />
            <Button
              variant="outline"
              size="sm"
              disabled={!manualRepo.includes('/')}
              onClick={() => {
                api.localmodels
                  .quants(manualRepo.trim())
                  .then((r) => {
                    setModels((prev) => [r.model, ...prev.filter((m) => m.repo !== r.model.repo)]);
                    setExpanded(r.model.repo);
                    setManualRepo('');
                  })
                  .catch((e) => toast.error(errorText(e)));
              }}
            >
              {intl.formatMessage({ id: 'localmodels.manual.load' })}
            </Button>
          </div>
        </section>
      )}
    </div>
  );
}

export default LocalModelsPage;
