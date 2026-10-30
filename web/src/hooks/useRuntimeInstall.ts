import { useCallback, useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { api, type InstallableProvider, type RuntimeInstallOutput, type RuntimeInstallStatus } from '@/lib/api';
import { client } from '@/lib/ws-client';

type InstallPhase = 'idle' | 'running' | 'succeeded' | 'failed';

/** Keep the streamed transcript bounded — installers are chatty. */
const MAX_OUTPUT = 8000;

/** Vendor setup guides remain available before any install is attempted. */
const PROVIDER_DOCS: Record<InstallableProvider, string> = {
  claude: 'https://docs.anthropic.com/en/docs/claude-code',
  codex: 'https://github.com/openai/codex',
  grok: 'https://docs.x.ai/build/cli',
};

/** Unknown reasons use the generic failure message. */
const EXPLAINED_DECLINES = new Set([
  'already_running',
  'prerequisite_missing',
  'posix_script_on_windows',
  'install_target_not_on_probe_path',
  'spawn_failed',
]);

/** Shared fixed-provider install flow for onboarding and the shell update banner. */
export function useRuntimeInstall(provider: InstallableProvider, onDetect: () => void) {
  const intl = useIntl();
  const [phase, setPhase] = useState<InstallPhase>('idle');
  const [output, setOutput] = useState('');
  /** Command to paste when we can't (or shouldn't) run it ourselves. */
  const [manual, setManual] = useState<{ command: string; docsUrl: string; message: string } | null>(
    null,
  );
  const mountedRef = useRef(false);
  const activeRef = useRef(false);
  const terminalRef = useRef(false);
  const fallbackRef = useRef('');
  const sidRef = useRef<string | null>(null);
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
        activeRef.current = false;
        setPhase('failed');
        setManual((m) => ({
          command: m?.command ?? fallbackRef.current,
          docsUrl: m?.docsUrl ?? PROVIDER_DOCS[provider],
          message: intl.formatMessage({ id: 'welcome.runtime.unknown' }),
        }));
      }, graceMs);
    },
    [clearWatchdog, intl, provider],
  );

  // Never leave a timer behind on unmount.
  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      activeRef.current = false;
      clearWatchdog();
    };
  }, [clearWatchdog]);

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
      activeRef.current && pl.provider === provider && (pl.session_id === sidRef.current ||
      sidRef.current === null);

    const offOut = client.subscribe('runtime.install.output', (p) => {
      const pl = p as RuntimeInstallOutput;
      if (!mine(pl)) return;
      setOutput((o) => Array.from(o + pl.data).slice(-MAX_OUTPUT).join(''));
    });
    const offStatus = client.subscribe('runtime.install.status', (p) => {
      const pl = p as RuntimeInstallStatus;
      if (!mine(pl)) return;
      terminalRef.current = true;
      activeRef.current = false;
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
            id: pl.status === 'update_shadowed' ? 'runtimeUpdate.shadowed'
              : pl.status === 'version_unverified' ? 'runtimeUpdate.unverified'
              : pl.status === 'timeout' ? 'welcome.runtime.timeout' : 'welcome.runtime.installFailed',
          }, { path: pl.path ?? '' }),
        });
      }
    });
    return () => {
      offOut();
      offStatus();
    };
  }, [onDetect, intl, provider, clearWatchdog]);

  const startInstall = useCallback(async () => {
    activeRef.current = true;
    terminalRef.current = false;
    fallbackRef.current = provider === 'codex' ? 'npm install -g @openai/codex' : '';
    setOutput('');
    setManual(null);
    setPhase('running');
    sidRef.current = null;
    try {
      const res = await api.runtime.install(provider);
      if (!mountedRef.current) return;
      if (res.started) {
        sidRef.current = res.session_id;
        fallbackRef.current = res.command;
        if (!terminalRef.current) armWatchdog(res.timeout_secs);
        return;
      }
      activeRef.current = false;
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
      if (!mountedRef.current) return;
      activeRef.current = false;
      clearWatchdog();
      setPhase('failed');
      setManual({
        command: fallbackRef.current,
        docsUrl: '',
        message: e instanceof Error ? e.message : String(e),
      });
    }
  }, [provider, onDetect, intl, armWatchdog, clearWatchdog]);


  return { phase, output, manual, startInstall };
}
