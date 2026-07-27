import { useCallback, useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { Navigate } from 'react-router';
import { Server, Wifi, RefreshCw, Plug, ArrowRight, AlertCircle, HardDrive } from 'lucide-react';
import { Button, Card, CardContent, Input, Badge, Spinner } from '@/components/mds';
import {
  isTauri,
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
 * Gateway picker (WP-GW) — the desktop shell's pre-login landing page. Lets the
 * user connect to the local sidecar, a mDNS-discovered LAN gateway, or a
 * manually-entered host, and auto-connects to the last choice after a 3s
 * countdown (cancelled by any interaction).
 *
 * Desktop-only: outside Tauri it redirects to `/` so a plain browser never sees
 * it. Calm Glass surfaces (web/DESIGN.md); zh-TW / en / ja-JP via react-intl.
 */
const COUNTDOWN_SECONDS = 3;

export function GatewayPickerPage() {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) =>
    intl.formatMessage({ id }, values);

  const [local, setLocal] = useState<LocalStatus | null>(null);
  const [discovered, setDiscovered] = useState<GatewayRecord[]>([]);
  const [scanning, setScanning] = useState(false);
  const [manual, setManual] = useState('');
  const [manualBusy, setManualBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [connectingUrl, setConnectingUrl] = useState<string | null>(null);
  const [countdown, setCountdown] = useState<number | null>(null);
  const [lastGateway, setLastGateway] = useState<GatewayRecord | null>(null);

  const countdownTimer = useRef<ReturnType<typeof setInterval> | null>(null);
  const interacted = useRef(false);

  const stopCountdown = useCallback(() => {
    interacted.current = true;
    if (countdownTimer.current) {
      clearInterval(countdownTimer.current);
      countdownTimer.current = null;
    }
    setCountdown(null);
  }, []);

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
    gatewayDiscover()
      .then((list) => setDiscovered(list))
      .catch((e) => setError(String(e?.message ?? e)))
      .finally(() => setScanning(false));
  }, []);

  const connect = useCallback(
    async (record: GatewayRecord) => {
      stopCountdown();
      setError(null);
      setConnectingUrl(record.url);
      try {
        // Validate reachability first so we never navigate into a dead gateway.
        const health = await gatewayHealth(record.url);
        if (!health.ok) {
          setError(t('gateway.error.unreachable', { detail: health.error ?? '' }));
          setConnectingUrl(null);
          return;
        }
        const merged: GatewayRecord = {
          ...record,
          version: health.version ?? record.version,
          name: record.name || health.name || record.host,
        };
        await gatewaySelect(merged); // navigates the window on success
      } catch (e) {
        setError(String((e as Error)?.message ?? e));
        setConnectingUrl(null);
      }
    },
    [stopCountdown, t]
  );

  // ── Initial load: discovery + last-selection countdown ──
  useEffect(() => {
    if (!isTauri()) return;
    scan();
    gatewayLast()
      .then((state) => {
        const last = state.last_gateway ?? null;
        if (!last) return;
        setLastGateway(last);
        if (interacted.current) return;
        // Start the auto-connect countdown.
        setCountdown(COUNTDOWN_SECONDS);
        countdownTimer.current = setInterval(() => {
          setCountdown((c) => {
            if (c === null) return null;
            if (c <= 1) {
              if (countdownTimer.current) clearInterval(countdownTimer.current);
              countdownTimer.current = null;
              // Fire the auto-connect (guarded against prior interaction).
              if (!interacted.current) void connect(last);
              return null;
            }
            return c - 1;
          });
        }, 1000);
      })
      .catch(() => {});
    return () => {
      if (countdownTimer.current) clearInterval(countdownTimer.current);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const connectLocal = useCallback(async () => {
    stopCountdown();
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
      await connect(record);
    } catch (e) {
      setError(String((e as Error)?.message ?? e));
    }
  }, [connect, stopCountdown, t]);

  const connectManual = useCallback(async () => {
    stopCountdown();
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
  }, [manual, stopCountdown, t]);

  // Non-desktop: never render the picker.
  if (!isTauri()) return <Navigate to="/" replace />;

  const localRunning = local?.status === 'running';

  return (
    <div
      className="min-h-screen w-full bg-app-shell text-foreground"
      onPointerDown={() => {
        if (countdown !== null) stopCountdown();
      }}
    >
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

        {/* Auto-connect countdown */}
        {countdown !== null && lastGateway && (
          <Card className="border-brand/30 bg-brand/5">
            <CardContent className="flex items-center justify-between gap-3 py-3">
              <div className="min-w-0">
                <div className="text-sm font-medium">
                  {t('gateway.countdown.title', { seconds: countdown })}
                </div>
                <div className="truncate text-xs text-muted-foreground">
                  {lastGateway.name} · {lastGateway.host}:{lastGateway.port}
                </div>
              </div>
              <Button variant="outline" size="sm" onClick={stopCountdown}>
                {t('gateway.countdown.cancel')}
              </Button>
            </CardContent>
          </Card>
        )}

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
