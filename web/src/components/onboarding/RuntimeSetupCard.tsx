import { useCallback, useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { CheckCircle2, Download, Loader2, LogIn, RefreshCw, Terminal } from 'lucide-react';
import { type InstallableProvider } from '@/lib/api';
import { useRuntimeInstall } from '@/hooks/useRuntimeInstall';
import { Button, Card } from '@/components/mds';
import { CliLoginModal } from '@/components/CliLoginModal';
import { SubscriptionSetupWizard } from '@/components/SubscriptionSetupWizard';
import { SubscriptionRiskNote } from '@/components/SubscriptionRiskDisclosure';

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

const PROVIDER_DOCS: Record<RuntimeSetupProvider, string> = {
  claude: 'https://docs.anthropic.com/en/docs/claude-code',
  codex: 'https://github.com/openai/codex',
  grok: 'https://docs.x.ai/build/cli',
};

export function RuntimeSetupCard({ provider, installed, loggedIn, onDetect, detecting }: Props) {
  const intl = useIntl();
  const { phase, output, manual, startInstall } = useRuntimeInstall(provider, onDetect);
  const [copied, setCopied] = useState(false);
  const [loginOpen, setLoginOpen] = useState(false);
  const outRef = useRef<HTMLPreElement>(null);
  useEffect(() => {
    if (outRef.current) outRef.current.scrollTop = outRef.current.scrollHeight;
  }, [output]);
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
            <Button variant="brand" disabled={busy} onClick={() => { setCopied(false); void startInstall(); }}>
              {busy ? <Loader2 className="animate-spin motion-reduce:animate-none" /> : <Download />}
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
          {/* The login dialog shows the full disclosure first; Claude's
              policy line is also shown here, before the button. */}
          {provider === 'claude' && <SubscriptionRiskNote />}
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

      {/* WP-D: headless boxes can't complete `claude login`'s localhost-
          callback OAuth (the redirect targets the CLI's own machine, not
          the operator's browser) — `claude setup-token`'s paste-back flow
          is the only one that works there. The guided QR-code + validated
          wizard replaces the generic streamed-terminal modal for Claude
          specifically; the other CLIs keep the original picker. */}
      {loginOpen && provider === 'claude' && (
        <SubscriptionSetupWizard
          open={loginOpen}
          onClose={() => setLoginOpen(false)}
          onSuccess={onDetect}
        />
      )}
      {loginOpen && provider !== 'claude' && (
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
