import { useCallback, useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { CircleCheck, Cpu, Download, Play, Square } from 'lucide-react';

import { api, type LocalCatalogModel, type LocalInferenceStatus, type MarketFit } from '@/lib/api';
import { useConnectionStore } from '@/stores/connection-store';
import { useAuthStore } from '@/stores/auth-store';
import { hasMinRole } from '@/lib/roles';
import { toast } from '@/lib/toast';
import { Badge, Button, Card, CardContent, Skeleton, useErrorMessage } from '@/components/mds';

/**
 * WP-D — the built-in local model panel.
 *
 * The DuDuClaw OS device ships a local model engine but no weights. This
 * panel is the whole path from "nothing downloaded" to "a local model is
 * answering": a short verified list with a fit light for this machine,
 * one-click download with progress, and one click to make a downloaded
 * model the live one.
 *
 * It sits above the open-ended Hugging Face marketplace on the same page —
 * the marketplace stays for people who know exactly which repo they want.
 *
 * Honesty rules the copy follows:
 * - No speed numbers. Nobody has measured tokens/second on the target
 *   hardware, so the panel says what the model runs on and nothing more.
 * - The status line reflects a live probe, never configuration. "Running"
 *   means the engine answered just now.
 * - A model that could not be started says so, with the reason, instead of
 *   showing a success toast.
 */

const POLL_MS = 2000;

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

const GB = 1024 * 1024 * 1024;
const fmtGb = (n: number) => `${(n / GB).toFixed(1)} GB`;

/** Pure: the one status word the banner shows, derived from live probes. */
export function bannerState(
  s: LocalInferenceStatus | null,
): 'loading' | 'unsupported' | 'running' | 'nomodel' | 'stopped' {
  if (!s) return 'loading';
  if (!s.appliance && !s.llama_server_present) return 'unsupported';
  if (s.reachable) return 'running';
  if (!s.has_model) return 'nomodel';
  return 'stopped';
}

export function BuiltInLocalModel() {
  const intl = useIntl();
  const errorText = useErrorMessage();
  const connectionState = useConnectionStore((s) => s.state);
  const role = useAuthStore((s) => s.user?.role);
  const canManage = hasMinRole(role, 'admin');

  const [models, setModels] = useState<LocalCatalogModel[]>([]);
  const [status, setStatus] = useState<LocalInferenceStatus | null>(null);
  const [loading, setLoading] = useState(true);
  /** The RPCs are admin-gated, so a non-admin's first call is rejected.
   *  That is not an error worth a toast — the panel simply is not theirs. */
  const [unavailable, setUnavailable] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  const pollTimer = useRef<ReturnType<typeof setInterval> | null>(null);

  const refreshCatalog = useCallback(() => {
    api.inference.local
      .catalog()
      .then((r) => setModels(r?.models ?? []))
      .catch(() => {});
  }, []);

  const refreshStatus = useCallback(() => {
    api.inference.local
      .status()
      .then((s) => {
        setStatus(s);
        const active = (s.downloads ?? []).some(
          (j) => j.state === 'downloading' || j.state === 'queued',
        );
        if (!active && pollTimer.current) {
          clearInterval(pollTimer.current);
          pollTimer.current = null;
          refreshCatalog();
        }
      })
      .catch(() => {});
  }, [refreshCatalog]);

  const startPolling = useCallback(() => {
    if (pollTimer.current) return;
    pollTimer.current = setInterval(refreshStatus, POLL_MS);
  }, [refreshStatus]);

  /** A restarted engine needs a few seconds to map the weights before it
   *  answers. Without these follow-ups the banner would sit on "not
   *  switched on" right after a successful start, which is stale rather
   *  than wrong — worse to read than either. */
  const settleTimers = useRef<Array<ReturnType<typeof setTimeout>>>([]);
  const recheckAfterStart = useCallback(() => {
    for (const ms of [2000, 6000, 12000]) {
      settleTimers.current.push(setTimeout(refreshStatus, ms));
    }
  }, [refreshStatus]);

  useEffect(
    () => () => {
      if (pollTimer.current) clearInterval(pollTimer.current);
      for (const t of settleTimers.current) clearTimeout(t);
    },
    [],
  );

  useEffect(() => {
    if (connectionState !== 'authenticated') return;
    setLoading(true);
    Promise.all([api.inference.local.catalog(), api.inference.local.status()])
      .then(([c, s]) => {
        setModels(c?.models ?? []);
        setStatus(s);
        if ((s.downloads ?? []).some((j) => j.state === 'downloading' || j.state === 'queued')) {
          startPolling();
        }
      })
      .catch((e) => {
        console.warn('[api]', e);
        setUnavailable(true);
      })
      .finally(() => setLoading(false));
    // startPolling is stable via useCallback.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [connectionState]);

  const download = (m: LocalCatalogModel) => {
    setBusy(m.id);
    api.inference.local
      .download(m.id)
      .then(() => {
        toast.success(intl.formatMessage({ id: 'localmodels.builtin.download.started' }));
        refreshStatus();
        startPolling();
      })
      .catch((e) => toast.error(errorText(e)))
      .finally(() => setBusy(null));
  };

  const serve = (m: LocalCatalogModel) => {
    setBusy(m.id);
    api.inference.local
      .serve(m.file)
      .then((r) => {
        if (r.restarted) {
          toast.success(
            intl.formatMessage({ id: 'localmodels.builtin.started' }, { name: m.display_name }),
          );
          recheckAfterStart();
        } else {
          // Never a success toast for something that did not start.
          toast.error(
            intl.formatMessage(
              { id: 'localmodels.builtin.notStarted' },
              { detail: r.detail || '' },
            ),
          );
        }
        refreshStatus();
      })
      .catch((e) => toast.error(errorText(e)))
      .finally(() => setBusy(null));
  };

  const stop = () => {
    setBusy('__stop__');
    api.inference.local
      .stop()
      .then((r) => {
        if (r.stopped) toast.success(intl.formatMessage({ id: 'localmodels.builtin.stopped' }));
        else if (r.detail) toast.error(r.detail);
        refreshStatus();
      })
      .catch((e) => toast.error(errorText(e)))
      .finally(() => setBusy(null));
  };

  const state = bannerState(status);
  const jobFor = (m: LocalCatalogModel) =>
    (status?.downloads ?? []).find(
      (j) => j.filename === m.file && (j.state === 'downloading' || j.state === 'queued'),
    );

  if (loading) return <Skeleton className="h-48" />;
  // Nothing to show, two ways: the caller may not read this surface, or the
  // host has no built-in engine. Either way get out of the way silently —
  // the Hugging Face marketplace below still works.
  if (unavailable || state === 'unsupported') return null;

  return (
    <section className="space-y-3">
      <div className="space-y-0.5">
        <h2 className="text-sm font-medium text-foreground">
          {intl.formatMessage({ id: 'localmodels.builtin.title' })}
        </h2>
        <p className="text-xs text-muted-foreground">
          {intl.formatMessage({ id: 'localmodels.builtin.desc' })}
        </p>
      </div>

      {/* Status banner — every word here comes from a live probe. */}
      <Card data-size="sm">
        <CardContent className="flex flex-wrap items-center gap-x-3 gap-y-1.5 text-sm">
          <span
            className={`size-2 shrink-0 rounded-full ${
              state === 'running' ? 'bg-emerald-500' : 'bg-muted-foreground/50'
            }`}
            aria-hidden="true"
          />
          <span className="text-foreground">
            {state === 'running'
              ? intl.formatMessage(
                  { id: 'localmodels.builtin.status.running' },
                  { model: status?.loaded_model ?? status?.configured_model ?? '' },
                )
              : state === 'nomodel'
                ? intl.formatMessage({ id: 'localmodels.builtin.status.nomodel' })
                : intl.formatMessage({ id: 'localmodels.builtin.status.stopped' })}
          </span>
          {state === 'running' && canManage && (
            <Button
              variant="ghost"
              size="sm"
              className="ml-auto"
              disabled={busy === '__stop__'}
              onClick={stop}
            >
              <Square className="mr-1.5 size-3.5" />
              {intl.formatMessage({ id: 'localmodels.builtin.stop' })}
            </Button>
          )}
        </CardContent>
      </Card>

      <div className="grid gap-3 sm:grid-cols-2">
        {models.map((m) => {
          const job = jobFor(m);
          const live = state === 'running' && status?.configured_model === m.file;
          const pct =
            job && job.total_bytes > 0
              ? Math.min(100, Math.round((job.downloaded_bytes / job.total_bytes) * 100))
              : 0;
          return (
            <Card key={m.id} data-size="sm">
              <CardContent className="space-y-2">
                <div className="flex items-start gap-2">
                  <div className="min-w-0">
                    <h3 className="truncate text-sm font-medium text-foreground">
                      {m.display_name}
                    </h3>
                    <p className="truncate text-xs text-muted-foreground">
                      {m.quant} · {fmtGb(m.size_bytes)} ·{' '}
                      {intl.formatMessage(
                        { id: 'localmodels.builtin.ram' },
                        { gb: m.min_ram_gb.toFixed(1) },
                      )}
                    </p>
                  </div>
                  <span
                    className="ml-auto mt-1 flex shrink-0 items-center gap-1.5 text-xs text-muted-foreground"
                    title={intl.formatMessage({ id: `localmodels.fit.${m.fit}` })}
                  >
                    <span className={`size-2 rounded-full ${fitTone(m.fit)}`} />
                    {intl.formatMessage({ id: `localmodels.fit.${m.fit}` })}
                  </span>
                </div>

                <div className="flex flex-wrap gap-1.5">
                  {m.recommended_for.map((tag) => (
                    <Badge key={tag} variant="outline">
                      {intl.formatMessage({ id: `localmodels.builtin.use.${tag}` })}
                    </Badge>
                  ))}
                  {!m.verified && (
                    <Badge variant="outline">
                      {intl.formatMessage({ id: 'localmodels.builtin.unverified' })}
                    </Badge>
                  )}
                </div>

                {job ? (
                  <div className="space-y-1">
                    <div className="flex items-center gap-2 text-xs text-muted-foreground">
                      <Download className="size-3.5 animate-pulse text-brand" />
                      {intl.formatMessage({ id: 'localmodels.builtin.downloading' })}
                      <span className="ml-auto font-mono tabular-nums">{pct}%</span>
                    </div>
                    <div
                      className="h-1.5 w-full overflow-hidden rounded-full bg-muted"
                      role="progressbar"
                      aria-valuemin={0}
                      aria-valuemax={100}
                      aria-valuenow={job.total_bytes > 0 ? pct : undefined}
                    >
                      <div
                        className="h-full rounded-full bg-brand transition-all"
                        style={{ width: job.total_bytes > 0 ? `${pct}%` : '10%' }}
                      />
                    </div>
                  </div>
                ) : (
                  <div className="flex items-center gap-2 border-t border-surface-border pt-2">
                    {live && (
                      <span className="flex items-center gap-1.5 text-xs text-emerald-600 dark:text-emerald-400">
                        <CircleCheck className="size-3.5" />
                        {intl.formatMessage({ id: 'localmodels.builtin.inUse' })}
                      </span>
                    )}
                    <div className="ml-auto flex items-center gap-1.5">
                      {!m.installed && canManage && (
                        <Button size="sm" disabled={busy === m.id} onClick={() => download(m)}>
                          <Download className="mr-1.5 size-3.5" />
                          {intl.formatMessage({ id: 'localmodels.builtin.download' })}
                        </Button>
                      )}
                      {m.installed && !live && canManage && (
                        <Button size="sm" disabled={busy === m.id} onClick={() => serve(m)}>
                          <Play className="mr-1.5 size-3.5" />
                          {intl.formatMessage({ id: 'localmodels.builtin.serve' })}
                        </Button>
                      )}
                      {m.installed && !canManage && (
                        <span className="text-xs text-muted-foreground">
                          {intl.formatMessage({ id: 'localmodels.builtin.installed' })}
                        </span>
                      )}
                    </div>
                  </div>
                )}
              </CardContent>
            </Card>
          );
        })}
      </div>

      {/* The honest note: what it runs on, and no numbers nobody measured. */}
      <p className="flex items-start gap-2 text-xs text-muted-foreground">
        <Cpu className="mt-0.5 size-3.5 shrink-0" aria-hidden="true" />
        {intl.formatMessage({ id: 'localmodels.builtin.note' })}
      </p>
    </section>
  );
}

export default BuiltInLocalModel;
