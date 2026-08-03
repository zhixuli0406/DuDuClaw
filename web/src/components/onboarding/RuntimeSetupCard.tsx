import { useCallback, useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { CheckCircle2, Download, Loader2, LogIn, RefreshCw, Terminal } from 'lucide-react';
import {
  api,
  type InstallableProvider,
  type RuntimeInstallOutput,
  type RuntimeInstallStatus,
} from '@/lib/api';
import { client } from '@/lib/ws-client';
import { Button, Card } from '@/components/mds';
import { CliLoginModal } from '@/components/CliLoginModal';

/**
 * WP2 / D16 — the onboarding wizard's "get this CLI working" card.
 *
 * The wizard used to stop at a dead "未安裝" label, which is exactly where a
 * non-technical first-time user gives up: they are told a thing is missing and
 * then handed a terminal. This card closes both gaps in place —
 *
 *  - **not installed** → 「幫我安裝」 runs `runtime.install` on the gateway and
 *    streams the installer's output here; when it finishes the card re-detects
 *    itself, so success is proven by a fresh probe rather than an exit code.
 *  - **installed but not signed in** → 「立即登入」 opens the same
 *    {@link CliLoginModal} the accounts page uses (PTY-driven native login);
 *    on success it re-detects too.
 *
 * Every failure path still ends with the exact command to paste plus a link to
 * the vendor's own docs — the flow degrades to "do it yourself", never to a
 * dead end.
 */

export type RuntimeSetupProvider = InstallableProvider;

interface Props {
  provider: RuntimeSetupProvider;
  /** From `runtime.detect`. `undefined` while the first probe is in flight. */
  installed: boolean | undefined;
  /** Sign-in state, when the provider has one we can observe (Claude OAuth).
   *  `undefined` ⇒ don't render the login row. */
  loggedIn?: boolean;
  /** Re-run `runtime.detect` in the parent and push fresh props down. */
  onDetect: () => void;
  detecting?: boolean;
}

type InstallPhase = 'idle' | 'running' | 'succeeded' | 'failed';

/** Keep the streamed transcript bounded — installers are chatty. */
const MAX_OUTPUT = 8000;

/** Decline reasons with a dedicated zh-TW/en/ja explanation. A reason added
 *  server-side but not listed here degrades to the generic failure copy rather
 *  than rendering a raw machine code at the user. */
/** Vendor "how do I set this up myself" page, for users who would rather not
 *  hand the install to the dashboard. Mirrors the `docs_url` the gateway
 *  returns, so the link is available before any install is attempted. */
const PROVIDER_DOCS: Record<RuntimeSetupProvider, string> = {
  claude: 'https://docs.anthropic.com/en/docs/claude-code',
  codex: 'https://github.com/openai/codex',
  gemini: 'https://github.com/google-gemini/gemini-cli',
  antigravity: 'https://antigravity.google/docs/cli',
  grok: 'https://docs.x.ai/build/cli',
};

const EXPLAINED_DECLINES = new Set([
  'already_running',
  'prerequisite_missing',
  'posix_script_on_windows',
  'install_target_not_on_probe_path',
  'spawn_failed',
]);

export function RuntimeSetupCard({ provider, installed, loggedIn, onDetect, detecting }: Props) {
  const intl = useIntl();
  const [phase, setPhase] = useState<InstallPhase>('idle');
  const [output, setOutput] = useState('');
  /** Command to paste when we can't (or shouldn't) run it ourselves. */
  const [manual, setManual] = useState<{ command: string; docsUrl: string; message: string } | null>(
    null,
  );
  const [copied, setCopied] = useState(false);
  const [loginOpen, setLoginOpen] = useState(false);
  const sidRef = useRef<string | null>(null);
  const outRef = useRef<HTMLPreElement>(null);
  /** Local watchdog — see `armWatchdog`. */
  const watchdogRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const clearWatchdog = useCallback(() => {
    if (watchdogRef.current !== null) {
      clearTimeout(watchdogRef.current);
      watchdogRef.current = null;
    }
  }, []);

  /**
   * The gateway's terminal `runtime.install.status` event is the only thing
   * that ends the "installing…" state, so anything that stops it from arriving
   * — a dropped WebSocket, a gateway restart mid-install, an event bus that
   * wasn't wired — leaves the button disabled and the spinner turning forever.
   *
   * This arms a local deadline just past the server's own `timeout_secs` (the
   * gateway kills the child at that point, so a status frame is always *due*
   * shortly after). If the deadline passes with nothing received, the card
   * admits it doesn't know the outcome and hands control back rather than
   * claiming either success or failure.
   */
  const armWatchdog = useCallback(
    (timeoutSecs: number) => {
      clearWatchdog();
      const graceMs = (timeoutSecs + 20) * 1000;
      watchdogRef.current = setTimeout(() => {
        watchdogRef.current = null;
        setPhase('failed');
        setManual((m) => ({
          command: m?.command ?? '',
          docsUrl: m?.docsUrl ?? PROVIDER_DOCS[provider],
          message: intl.formatMessage({ id: 'welcome.runtime.unknown' }),
        }));
      }, graceMs);
    },
    [clearWatchdog, intl, provider],
  );

  // Never leave a timer behind on unmount.
  useEffect(() => clearWatchdog, [clearWatchdog]);

  // Stream installer output + the single terminal status frame.
  useEffect(() => {
    /**
     * Does this frame belong to us?
     *
     * Normally: session id match. The `sidRef.current === null` arm closes a
     * real race — the gateway spawns the child and starts emitting *before* the
     * `runtime.install` response has made it back to us, so the first lines of
     * a fast-failing install would otherwise vanish. Falling back to the
     * provider is safe: the gateway refuses a second concurrent install of the
     * same provider, so there is no other session to confuse ours with.
     */
    const mine = (pl: { session_id: string; provider: string }) =>
      pl.session_id === sidRef.current ||
      (sidRef.current === null && pl.provider === provider);

    const offOut = client.subscribe('runtime.install.output', (p) => {
      const pl = p as RuntimeInstallOutput;
      if (!mine(pl)) return;
      setOutput((o) => (o + pl.data).slice(-MAX_OUTPUT));
    });
    const offStatus = client.subscribe('runtime.install.status', (p) => {
      const pl = p as RuntimeInstallStatus;
      if (!mine(pl)) return;
      clearWatchdog();
      // `detected` is authoritative: a zero exit that left no usable binary is
      // still a failure as far as the wizard is concerned.
      const ok = pl.status === 'succeeded' && pl.detected;
      setPhase(ok ? 'succeeded' : 'failed');
      if (ok) {
        onDetect();
      } else {
        setManual({
          command: pl.command,
          docsUrl: pl.docs_url,
          message: intl.formatMessage({
            id: pl.status === 'timeout' ? 'welcome.runtime.timeout' : 'welcome.runtime.installFailed',
          }),
        });
      }
    });
    return () => {
      offOut();
      offStatus();
    };
  }, [onDetect, intl, provider, clearWatchdog]);

  // Auto-scroll the transcript.
  useEffect(() => {
    if (outRef.current) outRef.current.scrollTop = outRef.current.scrollHeight;
  }, [output]);

  const startInstall = useCallback(async () => {
    setOutput('');
    setManual(null);
    setCopied(false);
    setPhase('running');
    sidRef.current = null;
    try {
      const res = await api.runtime.install(provider);
      if (res.started) {
        sidRef.current = res.session_id;
        armWatchdog(res.timeout_secs);
        return;
      }
      // Declined — a normal, actionable answer. Nothing is running, so the
      // watchdog must not be armed.
      clearWatchdog();
      sidRef.current = null;
      if (res.reason === 'already_installed') {
        setPhase('succeeded');
        onDetect();
        return;
      }
      setPhase('failed');
      setManual({
        command: res.command,
        docsUrl: res.docs_url,
        message: intl.formatMessage(
          {
            id: EXPLAINED_DECLINES.has(res.reason)
              ? `welcome.runtime.decline.${res.reason}`
              : 'welcome.runtime.installFailed',
          },
          { prerequisite: res.prerequisite ?? '' },
        ),
      });
    } catch (e) {
      clearWatchdog();
      setPhase('failed');
      setManual({
        command: '',
        docsUrl: '',
        message: e instanceof Error ? e.message : String(e),
      });
    }
  }, [provider, onDetect, intl, armWatchdog, clearWatchdog]);

  const copyCommand = useCallback(async () => {
    if (!manual?.command) return;
    try {
      await navigator.clipboard?.writeText(manual.command);
      setCopied(true);
    } catch {
      /* clipboard blocked — the command is selectable on screen anyway */
    }
  }, [manual]);

  // First probe still running — render nothing rather than a wrong state.
  if (installed === undefined) return null;

  const busy = phase === 'running';

  return (
    <Card className="gap-3 p-4" data-testid={`runtime-setup-${provider}`}>
      {/* ── Not installed ───────────────────────────────────────── */}
      {!installed && (
        <>
          <p className="text-sm text-muted-foreground">
            {intl.formatMessage({ id: 'welcome.runtime.missing' }, { cli: provider })}
          </p>
          <div className="flex flex-wrap items-center gap-2">
            <Button variant="brand" disabled={busy} onClick={() => void startInstall()}>
              {busy ? <Loader2 className="animate-spin" /> : <Download />}
              {intl.formatMessage({
                id: busy ? 'welcome.runtime.installing' : 'welcome.runtime.install',
              })}
            </Button>
            <Button variant="outline" disabled={detecting} onClick={onDetect}>
              <RefreshCw className={detecting ? 'animate-spin' : undefined} />
              {intl.formatMessage({ id: 'welcome.runtime.redetect' })}
            </Button>
            {/* Escape hatch for users who'd rather not let us run an installer. */}
            <a
              href={PROVIDER_DOCS[provider]}
              target="_blank"
              rel="noreferrer"
              className="text-xs text-muted-foreground underline underline-offset-2 hover:text-foreground"
            >
              {intl.formatMessage({ id: 'welcome.runtime.selfSetup' })}
            </a>
          </div>
        </>
      )}

      {/* ── Installed, but the provider has a sign-in we can see ─── */}
      {installed && loggedIn === false && (
        <>
          <p className="text-sm text-muted-foreground">
            {intl.formatMessage({ id: 'welcome.runtime.notLoggedIn' }, { cli: provider })}
          </p>
          <div className="flex flex-wrap items-center gap-2">
            <Button variant="brand" onClick={() => setLoginOpen(true)}>
              <LogIn />
              {intl.formatMessage({ id: 'welcome.runtime.login' })}
            </Button>
            <Button variant="outline" disabled={detecting} onClick={onDetect}>
              <RefreshCw className={detecting ? 'animate-spin' : undefined} />
              {intl.formatMessage({ id: 'welcome.runtime.redetect' })}
            </Button>
          </div>
        </>
      )}

      {/* ── All good ────────────────────────────────────────────── */}
      {installed && loggedIn !== false && (
        <p className="flex items-center gap-2 text-sm text-success">
          <CheckCircle2 className="size-4 shrink-0" />
          {intl.formatMessage({ id: 'welcome.runtime.ready' }, { cli: provider })}
        </p>
      )}

      {/* ── Live installer transcript ───────────────────────────── */}
      {(busy || output) && (
        <pre
          ref={outRef}
          data-testid="runtime-install-output"
          className="h-32 overflow-auto whitespace-pre-wrap break-all rounded-lg border border-surface-border bg-stone-950/90 p-3 font-mono text-[11px] leading-relaxed text-stone-100"
        >
          {output.trim() || intl.formatMessage({ id: 'welcome.runtime.installStarting' })}
        </pre>
      )}

      {/* ── Fallback: the exact command + vendor docs ───────────── */}
      {manual && (
        <div className="space-y-2 rounded-lg border border-warning/30 bg-warning/5 p-3">
          <p className="text-xs text-warning">{manual.message}</p>
          {manual.command && (
            <>
              <p className="text-xs font-medium text-muted-foreground">
                {intl.formatMessage({ id: 'welcome.runtime.manualHint' })}
              </p>
              <code className="block select-all break-all rounded-md bg-muted px-2 py-1.5 font-mono text-[11px] text-foreground">
                {manual.command}
              </code>
              <div className="flex flex-wrap items-center gap-2">
                <Button variant="outline" size="sm" onClick={() => void copyCommand()}>
                  <Terminal />
                  {intl.formatMessage({
                    id: copied ? 'welcome.runtime.copied' : 'welcome.runtime.copy',
                  })}
                </Button>
                {manual.docsUrl && (
                  <a
                    href={manual.docsUrl}
                    target="_blank"
                    rel="noreferrer"
                    className="text-xs text-brand underline underline-offset-2"
                  >
                    {intl.formatMessage({ id: 'welcome.runtime.docs' })}
                  </a>
                )}
                <Button variant="ghost" size="sm" disabled={detecting} onClick={onDetect}>
                  <RefreshCw className={detecting ? 'animate-spin' : undefined} />
                  {intl.formatMessage({ id: 'welcome.runtime.redetect' })}
                </Button>
              </div>
            </>
          )}
        </div>
      )}

      {loginOpen && (
        <CliLoginModal
          open={loginOpen}
          runtime={provider}
          onClose={() => setLoginOpen(false)}
          onSuccess={onDetect}
        />
      )}
    </Card>
  );
}
