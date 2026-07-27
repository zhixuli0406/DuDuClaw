import { useCallback, useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { Navigate } from 'react-router';
import { Server, Wifi, RefreshCw, Plug, ArrowRight, AlertCircle, HardDrive } from 'lucide-react';
import { Button, Card, CardContent, Input, Badge, Spinner } from '@/components/mds';
import { toast } from '@/lib/toast';
import {
  isTauri,
  isSwitchIntent,
  decideAction,
  gatewayDiscover,
  gatewayHealth,
  gatewaySelect,
  gatewayLast,
  gatewayLocalStatus,
  gatewayStartLocal,
  type GatewayRecord,
  type LocalStatus,
} from '@/lib/gateway-picker';

/**
 * Gateway picker (WP-GW) — the desktop shell's pre-login landing page.
 *
 * Behaviour is **auto-select first** (design §2.5): on launch it resolves a
 * gateway without asking — a healthy remembered gateway connects straight away,
 * a single LAN gateway auto-connects with a toast, and nothing found falls back
 * to the local sidecar. The manual picker (local / LAN list / manual entry) is
 * only shown when the policy can't decide, or when the user reopens it via the
 * tray "切換 Gateway" item (which appends `switch=1`).
 *
 * Desktop-only: outside Tauri it redirects to `/` so a plain browser never sees
 * it. Calm Glass surfaces (web/DESIGN.md); zh-TW / en / ja-JP via react-intl.
 */

/** `resolving` = auto-select in progress (splash); `picker` = show the picker. */
type Phase = 'resolving' | 'picker';

export function GatewayPickerPage() {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) =>
    intl.formatMessage({ id }, values);

  // Reopening via the tray forces the picker; a fresh launch auto-resolves.
  const [phase, setPhase] = useState<Phase>(() =>
    isTauri() && !isSwitchIntent() ? 'resolving' : 'picker'
  );
  const [local, setLocal] = useState<LocalStatus | null>(null);
  const [discovered, setDiscovered] = useState<GatewayRecord[]>([]);
  const [scanning, setScanning] = useState(false);
  const [manual, setManual] = useState('');
  const [manualBusy, setManualBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [connectingUrl, setConnectingUrl] = useState<string | null>(null);

  const resolved = useRef(false);

  // ── Local sidecar status polling ──
  useEffect(() => {
    if (!isTauri()) return;
    let active = true;
    const poll = () => {
      gatewayLocalStatus()
        .then((s) => active && setLocal(s))
        .catch(() => {});
    };
    poll();
    const id = setInterval(poll, 1500);
    return () => {
      active = false;
      clearInterval(id);
    };
  }, []);

  // ── LAN discovery ──
  const scan = useCallback(() => {
    setScanning(true);
    return gatewayDiscover()
      .then((list) => {
        setDiscovered(list);
        return list;
      })
      .catch((e) => {
        setError(String(e?.message ?? e));
        return [] as GatewayRecord[];
      })
      .finally(() => setScanning(false));
  }, []);

  const connect = useCallback(
    async (record: GatewayRecord): Promise<boolean> => {
      setError(null);
      setConnectingUrl(record.url);
      try {
        // Validate reachability first so we never navigate into a dead gateway.
        const health = await gatewayHealth(record.url);
        if (!health.ok) {
          setError(t('gateway.error.unreachable', { detail: health.error ?? '' }));
          setConnectingUrl(null);
          return false;
        }
        const merged: GatewayRecord = {
          ...record,
          version: health.version ?? record.version,
          name: record.name || health.name || record.host,
        };
        await gatewaySelect(merged); // navigates the window on success
        return true;
      } catch (e) {
        setError(String((e as Error)?.message ?? e));
        setConnectingUrl(null);
        return false;
      }
    },
    [t]
  );

  const connectLocal = useCallback(async (): Promise<boolean> => {
    setError(null);
    try {
      // Ensure the sidecar is up (a prior remote pick may have stopped it),
      // then poll briefly for readiness before connecting.
      await gatewayStartLocal().catch(() => {});
      let status = await gatewayLocalStatus();
      for (let i = 0; i < 20 && status.status !== 'running'; i++) {
        await new Promise((r) => setTimeout(r, 300));
        status = await gatewayLocalStatus();
      }
      const record: GatewayRecord = {
        name: t('gateway.local.name'),
        host: '127.0.0.1',
        port: status.port,
        version: '',
        tls: false,
        url: status.url,
      };
      return await connect(record);
    } catch (e) {
      setError(String((e as Error)?.message ?? e));
      return false;
    }
  }, [connect, t]);

  // ── Auto-select on launch (design §2.5) ──
  useEffect(() => {
    if (!isTauri() || isSwitchIntent() || resolved.current) return;
    resolved.current = true;
    let cancelled = false;

    const fallToPicker = () => {
      if (cancelled) return;
      setPhase('picker');
      void scan();
    };

    (async () => {
      try {
        const state = await gatewayLast();
        const remembered = state.last_gateway ?? null;

        // Probe the remembered gateway; only scan the LAN when we still need to
        // decide (no remembered, or it's unreachable).
        let rememberedHealthy = false;
        if (remembered) {
          rememberedHealthy = await gatewayHealth(remembered.url)
            .then((h) => h.ok)
            .catch(() => false);
        }
        const discoveredNow =
          remembered && rememberedHealthy ? [] : await scan();
        if (cancelled) return;

        const action = decideAction({
          remembered,
          rememberedHealthy,
          discovered: discoveredNow,
        });
        if (action.kind === 'list') {
          setPhase('picker');
          return;
        }
        if (action.kind === 'local') {
          const ok = await connectLocal();
          if (!ok) fallToPicker();
          return;
        }
        // action.kind === 'auto'
        if (action.from === 'discovered') {
          toast.info(t('gateway.autoConnecting', { name: action.target.name }));
        }
        const ok = await connect(action.target);
        if (!ok) fallToPicker();
      } catch {
        fallToPicker();
      }
    })();

    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const connectManual = useCallback(async () => {
    setError(null);
    const raw = manual.trim();
    if (!raw) return;
    // Accept `host:port`, `host`, or a full URL; default to http:// when the
    // scheme is omitted. The Rust side re-validates the scheme (fail-closed).
    const url = /^https?:\/\//i.test(raw) ? raw : `http://${raw}`;
    setManualBusy(true);
    setConnectingUrl(url);
    try {
      const health = await gatewayHealth(url);
      if (!health.ok) {
        setError(t('gateway.error.unreachable', { detail: health.error ?? '' }));
        setConnectingUrl(null);
        return;
      }
      const u = new URL(url);
      const record: GatewayRecord = {
        name: health.name ?? u.hostname,
        host: u.hostname,
        port: u.port ? Number(u.port) : u.protocol === 'https:' ? 443 : 80,
        version: health.version ?? '',
        tls: u.protocol === 'https:',
        url,
      };
      await gatewaySelect(record);
    } catch (e) {
      setError(String((e as Error)?.message ?? e));
      setConnectingUrl(null);
    } finally {
      setManualBusy(false);
    }
  }, [manual, t]);

  // Non-desktop: never render the picker.
  if (!isTauri()) return <Navigate to="/" replace />;

  const localRunning = local?.status === 'running';

  // Auto-select splash: shown while the launch policy resolves a gateway. No
  // buttons — the page connects on its own, revealing the picker only on
  // fallback.
  if (phase === 'resolving') {
    return (
      <div className="min-h-screen w-full bg-app-shell text-foreground">
        <div className="mx-auto flex min-h-screen max-w-2xl flex-col items-center justify-center gap-4 px-6">
          <div className="flex items-center gap-2 text-brand">
            <span className="text-xl" aria-hidden>
              🐾
            </span>
            <span className="text-sm font-medium">{t('app.name')}</span>
          </div>
          <div className="flex items-center gap-2 text-sm text-muted-foreground">
            <Spinner /> {t('gateway.resolving')}
          </div>
        </div>
      </div>
    );
  }

  return (
    <div className="min-h-screen w-full bg-app-shell text-foreground">
      <div className="mx-auto flex min-h-screen max-w-2xl flex-col justify-center gap-6 px-6 py-12">
        {/* Header */}
        <header className="space-y-1.5">
          <div className="flex items-center gap-2 text-brand">
            <span className="text-xl" aria-hidden>
              🐾
            </span>
            <span className="text-sm font-medium">{t('app.name')}</span>
          </div>
          <h1 className="text-2xl font-semibold tracking-tight">{t('gateway.title')}</h1>
          <p className="text-sm text-muted-foreground">{t('gateway.subtitle')}</p>
        </header>

        {/* Error banner */}
        {error && (
          <div className="flex items-start gap-2 rounded-xl border border-destructive/30 bg-destructive/5 px-3 py-2.5 text-sm text-destructive">
            <AlertCircle className="mt-0.5 size-4 shrink-0" />
            <span className="min-w-0 break-words">{error}</span>
          </div>
        )}

        {/* Local card */}
        <section className="space-y-2">
          <h2 className="flex items-center gap-1.5 text-xs font-medium uppercase tracking-wide text-muted-foreground">
            <HardDrive className="size-3.5" /> {t('gateway.section.local')}
          </h2>
          <Card>
            <CardContent className="flex items-center justify-between gap-3 py-3.5">
              <div className="flex min-w-0 items-center gap-3">
                <div className="grid size-9 shrink-0 place-items-center rounded-lg bg-muted">
                  <Server className="size-4.5" />
                </div>
                <div className="min-w-0">
                  <div className="text-sm font-medium">{t('gateway.local.name')}</div>
                  <div className="flex items-center gap-1.5 text-xs text-muted-foreground">
                    <StatusDot status={local?.status ?? 'stopped'} />
                    {local
                      ? t(`gateway.local.status.${local.status}`)
                      : t('gateway.local.status.stopped')}
                    {local ? ` · :${local.port}` : ''}
                  </div>
                </div>
              </div>
              <Button size="sm" onClick={connectLocal} disabled={connectingUrl === local?.url}>
                {connectingUrl && connectingUrl === local?.url ? (
                  <Spinner />
                ) : (
                  <>
                    <Plug className="size-3.5" />
                    {localRunning ? t('gateway.connect') : t('gateway.start')}
                  </>
                )}
              </Button>
            </CardContent>
          </Card>
        </section>

        {/* Discovered gateways */}
        <section className="space-y-2">
          <div className="flex items-center justify-between">
            <h2 className="flex items-center gap-1.5 text-xs font-medium uppercase tracking-wide text-muted-foreground">
              <Wifi className="size-3.5" /> {t('gateway.section.discovered')}
            </h2>
            <Button variant="ghost" size="xs" onClick={scan} disabled={scanning}>
              <RefreshCw className={scanning ? 'size-3 animate-spin' : 'size-3'} />
              {t('gateway.rescan')}
            </Button>
          </div>
          <Card>
            <CardContent className="p-0">
              {scanning && discovered.length === 0 ? (
                <div className="flex items-center gap-2 px-4 py-6 text-sm text-muted-foreground">
                  <Spinner /> {t('gateway.scanning')}
                </div>
              ) : discovered.length === 0 ? (
                <div className="px-4 py-6 text-center text-sm text-muted-foreground">
                  {t('gateway.empty')}
                </div>
              ) : (
                <ul className="divide-y divide-border">
                  {discovered.map((g) => (
                    <li
                      key={g.url}
                      className="flex items-center justify-between gap-3 px-4 py-3"
                    >
                      <div className="flex min-w-0 items-center gap-3">
                        <div className="grid size-9 shrink-0 place-items-center rounded-lg bg-muted">
                          <Server className="size-4.5" />
                        </div>
                        <div className="min-w-0">
                          <div className="flex items-center gap-1.5">
                            <span className="truncate text-sm font-medium">{g.name}</span>
                            {g.tls && (
                              <Badge variant="secondary" className="shrink-0">
                                TLS
                              </Badge>
                            )}
                            {g.version && (
                              <Badge variant="outline" className="shrink-0">
                                v{g.version}
                              </Badge>
                            )}
                          </div>
                          <div className="truncate text-xs text-muted-foreground">
                            {g.host}:{g.port}
                          </div>
                        </div>
                      </div>
                      <Button
                        size="sm"
                        variant="outline"
                        onClick={() => connect(g)}
                        disabled={connectingUrl === g.url}
                      >
                        {connectingUrl === g.url ? (
                          <Spinner />
                        ) : (
                          <>
                            {t('gateway.connect')}
                            <ArrowRight className="size-3.5" />
                          </>
                        )}
                      </Button>
                    </li>
                  ))}
                </ul>
              )}
            </CardContent>
          </Card>
        </section>

        {/* Manual input */}
        <section className="space-y-2">
          <h2 className="flex items-center gap-1.5 text-xs font-medium uppercase tracking-wide text-muted-foreground">
            <Plug className="size-3.5" /> {t('gateway.section.manual')}
          </h2>
          <div className="flex items-center gap-2">
            <Input
              value={manual}
              onChange={(e) => setManual(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter') void connectManual();
              }}
              placeholder={t('gateway.manual.placeholder')}
              spellCheck={false}
              autoCapitalize="off"
              autoCorrect="off"
              className="font-mono"
            />
            <Button onClick={connectManual} disabled={manualBusy || !manual.trim()}>
              {manualBusy ? <Spinner /> : t('gateway.connect')}
            </Button>
          </div>
          <p className="text-xs text-muted-foreground">{t('gateway.manual.hint')}</p>
        </section>
      </div>
    </div>
  );
}

/** Small colored status dot for the local sidecar. */
function StatusDot({ status }: { status: string }) {
  const color =
    status === 'running'
      ? 'bg-success'
      : status === 'error'
        ? 'bg-destructive'
        : 'bg-muted-foreground/40';
  return <span className={`inline-block size-1.5 rounded-full ${color}`} aria-hidden />;
}
