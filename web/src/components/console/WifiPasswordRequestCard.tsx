import { useEffect, useId, useState } from 'react';
import { useIntl } from 'react-intl';
import { Wifi, Check, X } from 'lucide-react';
import { Button, Input } from '@/components/mds';
import { api } from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import { useChatStore } from '@/stores/chat-store';
import type { WifiPasswordRequestArtifact } from './artifact-types';
import { ArtifactShell } from './ArtifactShell';

type CardStatus = 'idle' | 'busy' | 'done' | 'cancelled';

/** Closed vocabulary from `crates/duduclaw-gateway/src/network/mod.rs`'s
 *  `WifiNetwork.security` doc comment — display labels only, never used for
 *  any authorization decision. An unrecognized future value (or a scan that
 *  never found this SSID at all — see the effect below) simply omits the
 *  hint line rather than guessing. */
const SECURITY_LABEL_KEY: Record<string, string> = {
  open: 'console.artifact.wifiPasswordRequest.securityLabel.open',
  wep: 'console.artifact.wifiPasswordRequest.securityLabel.wep',
  psk: 'console.artifact.wifiPasswordRequest.securityLabel.psk',
  '8021x': 'console.artifact.wifiPasswordRequest.securityLabel.8021x',
  unknown: 'console.artifact.wifiPasswordRequest.securityLabel.unknown',
};

/**
 * 密碼輸入卡 (T1, `commercial/docs/DESIGN-agent-body-network-2026-08.md`
 * §5.2/§12) — the ONE O-3 card whose entire reason to exist is a security
 * boundary, not a UX convenience. Rendered when a `system_operator` agent's
 * `os_wifi_connect(ssid, None)` failed with `wrong_password` (a new secured
 * network iwd has no stored credential for, design §3.3's second branch).
 *
 * ## Security design (written up here, not just in the design doc, because
 * this is the file a future editor is most likely to touch without reading
 * §5 first)
 *
 * 1. **The payload this component receives has no password in it, and never
 *    could** — `os_wifi_connect`'s MCP schema has no `psk` field at all
 *    (`mcp_os_ops.rs`), so the agent that triggered this card structurally
 *    never possessed one to leak. `payload.ssid` is the only field; see
 *    `WifiPasswordRequestArtifact`'s own doc comment.
 * 2. **The password the human types here never becomes agent input.** It
 *    lives in ONE local `useState` in this component, is read once into a
 *    local variable at submit time, cleared from state (and thus eligible
 *    for GC) in the same tick, and is never passed to `send()`, never
 *    logged, never put in a store any other component reads, and never
 *    round-trips through `react-intl` (which would require it to be a
 *    string interpolated into a translated sentence — it never is).
 * 3. **The submit button calls `api.network.wifiConnect` directly** — the
 *    SAME `network.wifi_connect` dashboard RPC `/device`'s Settings-page
 *    Wi-Fi UI would call, over the SAME already-Ed25519-authenticated admin
 *    WebSocket this very console page (`/console`) is running on. This is
 *    O-3's existing "structured confirmation calls the RPC directly, never
 *    round-trips through chat text" convention (`ConfirmActionCard`'s typed
 *    "RESET" / `UpdateConfirmCard`'s one-click confirm) applied to a field
 *    that happens to be secret — not a new mechanism.
 * 4. **Only the OUTCOME (never the attempt, never the password) is reported
 *    back to the agent's context**, via a plain-language `send()` chat
 *    message the agent's NEXT turn sees like any other user message (design
 *    §5.3) — see `sendOutcomeToAgent` below. The text is built entirely from
 *    values already known to (or safe to share with) the agent: `ssid`
 *    (the agent already saw it — it made the failing `os_wifi_connect` call)
 *    and a closed success/cancelled outcome. `formatError()`'s classified
 *    failure text is deliberately NOT sent to the agent on a retry — see
 *    the `runConnect` catch branch — only a human decision to give up
 *    (Cancel) or a final success closes the loop back to the agent, so a
 *    wrong-password retry-in-place never spams the transcript.
 * 5. **`autoComplete="new-password"` and no `<form>` element** — this is
 *    intentionally not a browser-native form submission (which could
 *    prompt "save this password?" tied to this dashboard's own origin, a
 *    persistence surface this design does not want to create); the connect
 *    button is a plain `onClick` handler, same as every other O-3 card.
 */
export function WifiPasswordRequestCard({ payload }: { payload: WifiPasswordRequestArtifact['payload'] }) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);
  const { ssid } = payload;
  const inputId = useId();
  const send = useChatStore((s) => s.send);

  const [status, setStatus] = useState<CardStatus>('idle');
  const [password, setPassword] = useState('');
  const [error, setError] = useState<string | null>(null);
  /** Best-effort display-only enrichment — never gates anything, never
   *  blocks the form. `null` (not found / scan failed / off-appliance dev
   *  box) simply means the security-label hint line is omitted. */
  const [security, setSecurity] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    // `rescan: false` — this is a display nicety, not a fresh radio scan;
    // reads whatever iwd already knows (which already includes `ssid`,
    // since the agent's own `os_wifi_scan`/`os_wifi_connect` turn is what
    // produced this card in the first place).
    api.network
      .wifiScan(false)
      .then((result) => {
        if (cancelled) return;
        const match = result.networks.find((n) => n.ssid === ssid);
        if (match) setSecurity(match.security);
      })
      .catch(() => {
        // Best-effort only — an off-appliance dev box, a transient D-Bus
        // hiccup, or the network having dropped out of range since the
        // agent's scan must never block password entry.
      });
    return () => {
      cancelled = true;
    };
  }, [ssid]);

  /** Design §5.3: report ONLY the outcome (never the attempt text, never
   *  the password) to the agent's next turn, via the same `send()` path the
   *  composer itself uses — see this component's doc comment point 4. */
  const sendOutcomeToAgent = (outcome: 'success' | 'cancelled') => {
    const key =
      outcome === 'success'
        ? 'console.artifact.wifiPasswordRequest.followupSuccess'
        : 'console.artifact.wifiPasswordRequest.followupCancelled';
    send(t(key, { ssid }), []);
  };

  const canSubmit = status === 'idle' && password.length > 0;

  const runConnect = async () => {
    setStatus('busy');
    setError(null);
    // Read once, clear immediately — see doc comment point 2. Everything
    // below this line closes over `pwd`, never `password` (state), so the
    // cleared state can never accidentally be re-read.
    const pwd = password;
    setPassword('');
    try {
      await api.network.wifiConnect(ssid, pwd, 'operator_console_prompted');
      toast.success(t('console.artifact.wifiPasswordRequest.done', { ssid }));
      setStatus('done');
      sendOutcomeToAgent('success');
    } catch (e) {
      console.warn('[console.artifact.wifi_password_request]', ssid, e);
      const message = formatError(e);
      toast.error(message);
      setError(message);
      setStatus('idle'); // allow an immediate retry without re-arming the gate
    }
  };

  const cancel = () => {
    setPassword('');
    setStatus('cancelled');
    sendOutcomeToAgent('cancelled');
  };

  if (status === 'done') {
    return (
      <ArtifactShell icon={Check} title={t('console.artifact.wifiPasswordRequest.title')}>
        <p className="flex items-center gap-2 text-sm text-success">
          <Check className="size-4 shrink-0" aria-hidden="true" />
          {t('console.artifact.wifiPasswordRequest.done', { ssid })}
        </p>
      </ArtifactShell>
    );
  }

  if (status === 'cancelled') {
    return (
      <ArtifactShell icon={X} title={t('console.artifact.wifiPasswordRequest.title')}>
        <p className="text-sm text-muted-foreground">{t('console.artifact.confirmAction.cancelled')}</p>
      </ArtifactShell>
    );
  }

  const securityLabelKey = security ? SECURITY_LABEL_KEY[security] : undefined;

  return (
    <ArtifactShell icon={Wifi} title={t('console.artifact.wifiPasswordRequest.title')}>
      <div className="space-y-3">
        <p className="text-sm text-muted-foreground">{t('console.artifact.wifiPasswordRequest.desc', { ssid })}</p>
        {securityLabelKey && (
          <p className="text-xs text-muted-foreground">
            {t('console.artifact.wifiPasswordRequest.securityHint', { label: t(securityLabelKey) })}
          </p>
        )}

        {error && (
          <p role="alert" className="rounded-lg border border-destructive/30 bg-destructive/10 px-2.5 py-1.5 text-xs text-destructive">
            {error}
          </p>
        )}

        <div className="space-y-1.5">
          <label htmlFor={inputId} className="text-xs text-muted-foreground">
            {t('console.artifact.wifiPasswordRequest.placeholder')}
          </label>
          <Input
            id={inputId}
            type="password"
            autoComplete="new-password"
            autoFocus
            value={password}
            onChange={(e) => setPassword(e.target.value)}
            placeholder={t('console.artifact.wifiPasswordRequest.placeholder')}
            disabled={status === 'busy'}
            onKeyDown={(e) => {
              if (e.key === 'Enter' && canSubmit) void runConnect();
            }}
          />
        </div>

        <div className="flex gap-2">
          <Button variant="outline" size="sm" onClick={cancel} disabled={status === 'busy'}>
            {t('common.cancel')}
          </Button>
          <Button variant="brand" size="sm" onClick={() => void runConnect()} disabled={!canSubmit}>
            {status === 'busy'
              ? t('console.artifact.wifiPasswordRequest.connecting')
              : t('console.artifact.wifiPasswordRequest.connect')}
          </Button>
        </div>
      </div>
    </ArtifactShell>
  );
}
