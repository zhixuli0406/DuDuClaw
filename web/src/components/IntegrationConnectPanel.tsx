import { useCallback, useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import {
  Loader2,
  CheckCircle2,
  Plug,
  KeyRound,
  Trash2,
  ExternalLink,
  type LucideIcon,
} from 'lucide-react';
import { api } from '@/lib/api';
import { Card, CardHeader, CardTitle, CardContent, Button, Badge, Input } from '@/components/mds';

/** The OAuth redirect URI the gateway serves — users must register this exact
 *  value in each provider's OAuth app. Mirrors `handle_mcp_oauth_callback`. */
export const OAUTH_REDIRECT_URI = 'http://localhost:3000/api/mcp/oauth/callback';

type ConnState = 'loading' | 'unconfigured' | 'configured' | 'connected';

interface PanelState {
  conn: ConnState;
  scopes: string[];
  expiresAt: string | null;
  /**
   * The redirect URI the user must register, as reported by the gateway. The
   * module constant below is only a fallback for older gateways — it is wrong
   * whenever `DUDUCLAW_PORT` differs, which is exactly the mistake that made
   * every consent flow dead-end before this was derived server-side.
   */
  redirectUri: string;
}

export interface IntegrationConnectPanelProps {
  /** OAuth provider id in the vault (e.g. 'google', 'notion', 'github'). */
  providerId: string;
  /** i18n key prefix — the panel reads `${prefix}.title`, `${prefix}.action.connect`, … */
  prefix: string;
  /** Header icon for the integration. */
  headerIcon: LucideIcon;
  /** Where users register their OAuth app (opens in a new tab). */
  consoleUrl: string;
  /** Human label for the console link (proper noun, not translated). */
  consoleLabel: string;
  /** Placeholder for the client-id field. */
  clientIdPlaceholder: string;
  /** Placeholder for the client-secret field. */
  clientSecretPlaceholder: string;
  /** Capabilities unlocked once connected — each id is a full i18n key. */
  capabilities: ReadonlyArray<{ icon: LucideIcon; id: string }>;
  /** Extra setup steps inserted after step 1 — full i18n keys, in order. */
  extraSetupSteps?: readonly string[];
  /** Optional transform for a granted-scope badge label (Google strips its URL prefix). */
  formatScope?: (scope: string) => string;
}

/**
 * IntegrationConnectPanel — the reusable three-state OAuth connect surface used
 * by the Google / Notion / GitHub integration pages. States:
 *   1. unconfigured → guided credential form (console link + redirect URI + fields)
 *   2. configured   → one-click connect button
 *   3. connected     → status + granted access + disconnect
 *
 * All copy is driven by `${prefix}.*` i18n keys so each provider reads its own
 * user-facing strings; only the console link label and field placeholders (all
 * proper nouns) are passed in directly.
 */
export function IntegrationConnectPanel({
  providerId,
  prefix,
  headerIcon: HeaderIcon,
  consoleUrl,
  consoleLabel,
  clientIdPlaceholder,
  clientSecretPlaceholder,
  capabilities,
  extraSetupSteps = [],
  formatScope,
}: IntegrationConnectPanelProps) {
  const intl = useIntl();
  const t = useCallback((suffix: string) => intl.formatMessage({ id: `${prefix}.${suffix}` }), [intl, prefix]);
  const [state, setState] = useState<PanelState>({
    conn: 'loading',
    scopes: [],
    expiresAt: null,
    redirectUri: OAUTH_REDIRECT_URI,
  });
  const [clientId, setClientId] = useState('');
  const [clientSecret, setClientSecret] = useState('');
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<{ kind: 'error' | 'info'; text: string } | null>(null);
  const pollRef = useRef<ReturnType<typeof setInterval> | null>(null);

  const refresh = useCallback(async () => {
    try {
      const [providers, status] = await Promise.all([
        api.mcp.oauthProviders(),
        api.mcp.oauthStatus(providerId),
      ]);
      const provider = providers.providers.find((p) => p.provider_id === providerId);
      const configured = provider?.configured ?? false;
      const scopes = status.scopes ?? provider?.scopes ?? [];
      setState({
        conn: status.authenticated ? 'connected' : configured ? 'configured' : 'unconfigured',
        scopes,
        expiresAt: status.expires_at,
        redirectUri: provider?.redirect_uri || OAUTH_REDIRECT_URI,
      });
    } catch {
      setNotice({ kind: 'error', text: t('error.load') });
    }
  }, [providerId, t]);

  useEffect(() => {
    refresh();
    return () => {
      if (pollRef.current) clearInterval(pollRef.current);
    };
  }, [refresh]);

  /** Poll auth status after opening the consent window. */
  const startPolling = useCallback(() => {
    if (pollRef.current) clearInterval(pollRef.current);
    let attempts = 0;
    pollRef.current = setInterval(async () => {
      attempts += 1;
      if (attempts > 100) {
        if (pollRef.current) clearInterval(pollRef.current);
        return;
      }
      try {
        const status = await api.mcp.oauthStatus(providerId);
        if (status.authenticated) {
          if (pollRef.current) clearInterval(pollRef.current);
          setNotice({ kind: 'info', text: t('connected.toast') });
          await refresh();
        }
      } catch {
        /* keep polling until timeout */
      }
    }, 3000);
  }, [providerId, refresh, t]);

  const openConsent = useCallback(
    async (id?: string, secret?: string) => {
      setBusy(true);
      setNotice(null);
      try {
        const { auth_url } = await api.mcp.oauthStart(providerId, id, secret);
        window.open(auth_url, '_blank', 'noopener');
        setNotice({ kind: 'info', text: t('waiting') });
        startPolling();
      } catch (e) {
        setNotice({ kind: 'error', text: String(e) });
      } finally {
        setBusy(false);
      }
    },
    [providerId, startPolling, t],
  );

  const handleSaveAndConnect = async () => {
    if (!clientId.trim()) return;
    await openConsent(clientId.trim(), clientSecret.trim() || undefined);
  };

  const handleDisconnect = async () => {
    if (!confirm(t('disconnect.confirm'))) return;
    setBusy(true);
    try {
      await api.mcp.oauthRevoke(providerId);
      setNotice({ kind: 'info', text: t('disconnected.toast') });
      await refresh();
    } catch {
      setNotice({ kind: 'error', text: t('error.load') });
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="max-w-3xl space-y-6">
      {/* Header */}
      <div className="flex items-start gap-3">
        <div className="flex size-11 shrink-0 items-center justify-center rounded-xl bg-brand/10">
          <HeaderIcon className="size-6 text-brand" />
        </div>
        <div>
          <h2 className="text-lg font-semibold text-foreground">{t('title')}</h2>
          <p className="text-sm text-muted-foreground">{t('subtitle')}</p>
        </div>
      </div>

      {notice && (
        <div
          className={
            notice.kind === 'error'
              ? 'rounded-lg border border-destructive/30 bg-destructive/10 px-3 py-2 text-sm text-destructive'
              : 'rounded-lg border border-brand/30 bg-brand/10 px-3 py-2 text-sm text-brand'
          }
        >
          {notice.text}
        </div>
      )}

      {state.conn === 'loading' && (
        <div className="flex items-center gap-2 text-sm text-muted-foreground">
          <Loader2 className="size-4 animate-spin" />
          {t('loading')}
        </div>
      )}

      {/* Capabilities — always shown so users know what they're enabling. */}
      {state.conn !== 'loading' && (
        <Card>
          <CardHeader>
            <CardTitle className="text-sm">{t('capabilities.title')}</CardTitle>
          </CardHeader>
          <CardContent>
            <ul className="grid gap-3 sm:grid-cols-2">
              {capabilities.map(({ icon: Icon, id }) => (
                <li key={id} className="flex items-start gap-2.5">
                  <Icon className="mt-0.5 size-4 shrink-0 text-brand" />
                  <span className="text-sm text-muted-foreground">{intl.formatMessage({ id })}</span>
                </li>
              ))}
            </ul>
            <p className="mt-4 rounded-md bg-muted/60 px-3 py-2 text-xs text-muted-foreground">
              {t('safety.note')}
            </p>
          </CardContent>
        </Card>
      )}

      {/* State: unconfigured — guided credential setup. */}
      {state.conn === 'unconfigured' && (
        <Card>
          <CardHeader>
            <CardTitle className="text-sm">{t('setup.title')}</CardTitle>
          </CardHeader>
          <CardContent className="space-y-4">
            <ol className="list-decimal space-y-1.5 pl-5 text-sm text-muted-foreground">
              <li>
                {t('setup.step1')}{' '}
                <a
                  href={consoleUrl}
                  target="_blank"
                  rel="noreferrer"
                  className="inline-flex items-center gap-1 text-brand hover:underline"
                >
                  {consoleLabel}
                  <ExternalLink className="size-3" />
                </a>
              </li>
              {/* Providers whose console needs API enablement / a consent
                  screen before a client can be created supply these two extra
                  steps. Omitting them was a guaranteed dead end: unenabled APIs
                  403 every call, and an unconfigured consent screen blocks the
                  authorization outright. */}
              {extraSetupSteps.map((id) => (
                <li key={id}>{intl.formatMessage({ id })}</li>
              ))}
              <li>{t('setup.step2')}</li>
              <li>
                {t('setup.step3')}
                <code className="ml-1 select-all rounded bg-muted px-1.5 py-0.5 font-mono text-xs text-foreground">
                  {state.redirectUri}
                </code>
              </li>
              <li>{t('setup.step4')}</li>
            </ol>

            <div className="space-y-1.5">
              <label className="text-xs font-medium text-muted-foreground">{t('field.clientId')}</label>
              <Input
                type="text"
                value={clientId}
                onChange={(e) => setClientId(e.target.value)}
                placeholder={clientIdPlaceholder}
              />
            </div>
            <div className="space-y-1.5">
              <label className="text-xs font-medium text-muted-foreground">{t('field.clientSecret')}</label>
              <Input
                type="password"
                value={clientSecret}
                onChange={(e) => setClientSecret(e.target.value)}
                placeholder={clientSecretPlaceholder}
              />
            </div>

            <Button variant="brand" onClick={handleSaveAndConnect} disabled={busy || !clientId.trim()}>
              {busy ? <Loader2 className="animate-spin" /> : <Plug />}
              {t('action.connect')}
            </Button>
          </CardContent>
        </Card>
      )}

      {/* State: configured but not connected — one-click connect. */}
      {state.conn === 'configured' && (
        <Card>
          <CardContent className="flex flex-col items-start gap-3 pt-6">
            <p className="text-sm text-muted-foreground">{t('configured.hint')}</p>
            <div className="flex gap-2">
              <Button variant="brand" onClick={() => openConsent()} disabled={busy}>
                {busy ? <Loader2 className="animate-spin" /> : <Plug />}
                {t('action.connect')}
              </Button>
              <Button
                variant="outline"
                onClick={() => setState((s) => ({ ...s, conn: 'unconfigured' }))}
                disabled={busy}
              >
                <KeyRound />
                {t('action.editCredentials')}
              </Button>
            </div>
          </CardContent>
        </Card>
      )}

      {/* State: connected — status + scopes + disconnect. */}
      {state.conn === 'connected' && (
        <Card>
          <CardHeader>
            <CardTitle className="flex items-center gap-2 text-sm">
              <CheckCircle2 className="size-4 text-success" />
              {t('connected.title')}
            </CardTitle>
          </CardHeader>
          <CardContent className="space-y-4">
            {state.expiresAt && (
              <p className="text-xs text-muted-foreground">
                {intl.formatMessage(
                  { id: `${prefix}.connected.expires` },
                  { date: new Date(state.expiresAt).toLocaleString() },
                )}
              </p>
            )}
            {state.scopes.length > 0 && (
              <div>
                <p className="text-xs font-medium text-muted-foreground">{t('connected.access')}</p>
                <div className="mt-1.5 flex flex-wrap gap-1.5">
                  {state.scopes.map((s) => (
                    <Badge key={s} variant="secondary" className="font-mono text-[11px]">
                      {formatScope ? formatScope(s) : s}
                    </Badge>
                  ))}
                </div>
              </div>
            )}
            <Button variant="destructive" size="sm" onClick={handleDisconnect} disabled={busy}>
              {busy ? <Loader2 className="animate-spin" /> : <Trash2 />}
              {t('action.disconnect')}
            </Button>
          </CardContent>
        </Card>
      )}
    </div>
  );
}
