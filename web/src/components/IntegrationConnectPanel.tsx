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
import { formatError } from '@/lib/toast';
import { openExternal } from '@/lib/external-link';
import { Card, CardHeader, CardTitle, CardContent, Button, Badge, Input } from '@/components/mds';
import { GoogleDataDisclosure } from '@/components/GoogleDataDisclosure';

type ConnState = 'loading' | 'unconfigured' | 'configured' | 'connected';

interface PanelState {
  conn: ConnState;
  scopes: string[];
  expiresAt: string | null;
  /** The exact redirect URI reported by the gateway; empty when unavailable. */
  redirectUri: string;
  /** The saved OAuth client id (public value, shown in full). */
  savedClientId: string;
  /** A client secret is on file — the panel shows proof, never the value. */
  hasSecret: boolean;
  /** Tail-masked hint for the saved secret, e.g. `••••9f3c`. */
  secretMasked: string;
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
  /** Notify the host wizard after fresh authorization. */
  onConnected?: () => void;
  /** The wizard renders the same URI above every step. */
  hideRedirect?: boolean;
  testing?: boolean;
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
  onConnected,
  testing,
  hideRedirect = false,
}: IntegrationConnectPanelProps) {
  const intl = useIntl();
  const t = useCallback((suffix: string) => intl.formatMessage({ id: `${prefix}.${suffix}` }), [intl, prefix]);
  const [state, setState] = useState<PanelState>({
    conn: 'loading',
    scopes: [],
    expiresAt: null,
    redirectUri: '',
    savedClientId: '',
    hasSecret: false,
    secretMasked: '',
  });
  const [clientId, setClientId] = useState('');
  const [clientSecret, setClientSecret] = useState('');
  /**
   * Whether the credential form is open, tracked separately from `state.conn`.
   * "Edit credentials" used to rewrite the connection state to `unconfigured`,
   * which made the header badge announce a disconnection that never happened —
   * the same class of lie this whole change set exists to remove. Opening a form
   * is a view, not a state transition.
   */
  const [editing, setEditing] = useState(false);
  /** True once the user edits the id field — stops a refresh from overwriting it. */
  const clientIdTouched = useRef(false);
  const [busy, setBusy] = useState(false);
  const [googleDataConfirmed, setGoogleDataConfirmed] = useState(false);
  const [notice, setNotice] = useState<{ kind: 'error' | 'info'; text: string } | null>(null);
  /**
   * The consent URL of the in-flight authorization, kept on screen as
   * select-all text while polling. This is the escape hatch when no browser
   * window appears (an outdated desktop shell, a popup blocker): the user can
   * copy it into any browser and the poll below still detects success.
   */
  const [authUrl, setAuthUrl] = useState<string | null>(null);
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
      const savedClientId = provider?.client_id ?? '';
      if (providerId === 'google') {
        if (status.needs_reconnect) setNotice({ kind: 'error', text: intl.formatMessage({ id: 'google.oauth.needsReconnect' }) });
        else if (status.error_code) setNotice({ kind: 'error', text: intl.formatMessage({ id: `google.oauth.${status.error_code}` }) });
        else if (status.testing_warning) setNotice({ kind: 'info', text: intl.formatMessage({ id: 'google.oauth.testingWarning' }) });
      }
      setState({
        conn: status.authenticated && !status.needs_reconnect ? 'connected' : configured ? 'configured' : 'unconfigured',
        scopes,
        expiresAt: status.expires_at,
        redirectUri: provider?.redirect_uri || '',
        savedClientId,
        hasSecret: provider?.has_client_secret ?? false,
        secretMasked: provider?.client_secret_masked ?? '',
      });
      // Seed the form with what is already stored. An empty field next to a
      // placeholder is indistinguishable from a wiped setting — this is the
      // whole reason a customer with a working integration reported that their
      // Google credentials had disappeared.
      if (!clientIdTouched.current && savedClientId) setClientId(savedClientId);
    } catch {
      setState(current => ({ ...current, redirectUri: '' }));
      setNotice({ kind: 'error', text: t('error.load') });
    }
  }, [providerId, t, intl]);

  useEffect(() => {
    refresh();
    return () => {
      if (pollRef.current) clearInterval(pollRef.current);
    };
  }, [refresh]);

  /** Google must observe a fresh callback; an old or refreshed token is insufficient. */
  const startPolling = useCallback((baseline: string | null, connectedBaseline?: string | null) => {
    if (pollRef.current) clearInterval(pollRef.current);
    let attempts = 0;
    pollRef.current = setInterval(async () => {
      attempts += 1;
      if (attempts > 100) {
        if (pollRef.current) clearInterval(pollRef.current);
        setEditing(true);
        setNotice({ kind: 'error', text: intl.formatMessage({ id: 'integration.oauth.timeout' }) });
        return;
      }
      try {
        const status = await api.mcp.oauthStatus(providerId);
        if (status.error_code) {
          if (pollRef.current) clearInterval(pollRef.current);
          setNotice({ kind: 'error', text: intl.formatMessage({ id: `google.oauth.${status.error_code}` }) });
          return;
        }
        const fresh = providerId === 'google'
          ? status.authenticated && !!status.connected_at && status.connected_at !== connectedBaseline
          : (!baseline && (status.access_token_valid === true || (status.access_token_valid === undefined && status.authenticated))) ||
          (!!status.expires_at && !!baseline && status.expires_at > baseline);
        if (fresh) {
          if (pollRef.current) clearInterval(pollRef.current);
          setAuthUrl(null);
          setNotice({ kind: 'info', text: t('connected.toast') });
          await refresh();
          onConnected?.();
        }
      } catch {
        /* keep polling until timeout */
      }
    }, 3000);
  }, [providerId, refresh, t, intl, onConnected]);

  /** Returns whether the consent window was actually opened. */
  const openConsent = useCallback(
    async (id?: string, secret?: string): Promise<boolean> => {
      if (providerId === 'google' && !googleDataConfirmed) return false;
      if (providerId === 'google') {
        try {
          const current = await api.googleCredentials.get();
          const replacing = ['service_account', 'apps_script'].includes(current.effective);
          if (replacing && !confirm(intl.formatMessage({ id: 'google.cred.oauthReplaceConfirm' }))) return false;
          await api.googleCredentials.set({ mode: 'oauth', ...(testing === undefined ? {} : { testing }), ...(replacing ? { replace_confirmed: true } : {}) });
        } catch (e) {
          setNotice({ kind: 'error', text: formatError(e) });
          return false;
        }
      }
      setBusy(true);
      setNotice(null);
      try {
        const connectedBaseline = providerId === 'google' ? (await api.mcp.oauthStatus(providerId)).connected_at : undefined;
        const { auth_url } = await api.mcp.oauthStart(
          providerId,
          id,
          secret,
          providerId === 'google' ? googleDataConfirmed : undefined,
        );
        if (providerId === 'google') setGoogleDataConfirmed(false);
        // Desktop-shell aware: window.open is a no-op in the wry webview.
        openExternal(auth_url);
        setAuthUrl(auth_url);
        setNotice({ kind: 'info', text: t('waiting') });
        startPolling(state.expiresAt, connectedBaseline);
        return true;
      } catch (e) {
        setNotice({ kind: 'error', text: formatError(e) });
        return false;
      } finally {
        setBusy(false);
      }
    },
    [googleDataConfirmed, providerId, startPolling, state.expiresAt, t, testing, intl],
  );

  const handleSaveAndConnect = async () => {
    if (!clientId.trim()) return;
    // The form stays open when the gateway rejects the credentials — closing it
    // would drop what the user typed next to an error telling them to fix it.
    if (await openConsent(clientId.trim(), clientSecret.trim() || undefined)) {
      setEditing(false);
    }
  };

  /** Leave the form without touching anything that was saved. */
  const cancelEditing = () => {
    setEditing(false);
    setClientSecret('');
    setClientId(state.savedClientId);
    clientIdTouched.current = false;
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

  /** Credentials are on file — independent of whether a token is current. */
  const isBound = state.savedClientId.length > 0;

  /**
   * What is already stored, in plain sight. Rendered in both the configured and
   * connected states: knowing *which* client id is saved is the difference
   * between "my settings are here" and "my settings are gone".
   */
  const savedSummary = (
    <dl className="grid gap-2 rounded-lg bg-muted/50 px-3 py-2.5 text-xs">
      <div className="flex flex-wrap items-baseline gap-x-2 gap-y-0.5">
        <dt className="font-medium text-muted-foreground">{t('field.clientId')}</dt>
        <dd className="min-w-0 break-all font-mono text-foreground">{state.savedClientId}</dd>
      </div>
      <div className="flex flex-wrap items-baseline gap-x-2 gap-y-0.5">
        <dt className="font-medium text-muted-foreground">{t('field.clientSecret')}</dt>
        <dd className="min-w-0 text-foreground">
          {state.hasSecret
            ? intl.formatMessage(
                { id: `${prefix}.saved.secretStored` },
                { mask: state.secretMasked || '••••' },
              )
            : t('saved.secretNone')}
        </dd>
      </div>
    </dl>
  );

  return (
    <div className="max-w-3xl space-y-6">
      {/* Header — the state badges sit here so "is this set up?" is answered
          before any form is read. */}
      <div className="flex items-start gap-3">
        <div className="flex size-11 shrink-0 items-center justify-center rounded-xl bg-brand/10">
          <HeaderIcon className="size-6 text-brand" />
        </div>
        <div>
          <h2 className="text-lg font-semibold text-foreground">{t('title')}</h2>
          <p className="text-sm text-muted-foreground">{t('subtitle')}</p>
          {state.conn !== 'loading' && (
            <div className="mt-2 flex flex-wrap items-center gap-1.5">
              {isBound && (
                <Badge className="bg-success/15 text-success">{t('badge.bound')}</Badge>
              )}
              <Badge variant={state.conn === 'connected' ? 'default' : 'secondary'}>
                {state.conn === 'connected'
                  ? t('badge.connected')
                  : isBound
                    ? t('badge.pendingAuth')
                    : t('badge.notConfigured')}
              </Badge>
            </div>
          )}
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

      {providerId === 'google' && !hideRedirect && (
        <div className="space-y-1 rounded-lg border border-surface-border p-3 text-xs">
          <p>{intl.formatMessage({ id: 'google.wizard.redirect' })}</p>
          <code className="block select-all break-all">{state.redirectUri || intl.formatMessage({ id: 'google.wizard.redirectUnavailable' })}</code>
          <Button variant="outline" size="sm" disabled={!state.redirectUri} onClick={() => { void navigator.clipboard.writeText(state.redirectUri).catch(() => setNotice({ kind: 'error', text: intl.formatMessage({ id: 'google.wizard.copyFailed' }) })); }}>
            {intl.formatMessage({ id: 'common.copy' })}
          </Button>
        </div>
      )}

      {/* Manual fallback while an authorization is in flight — if no consent
          window appeared, the URL itself completes the flow in any browser. */}
      {authUrl && (
        <div className="space-y-1 rounded-lg border border-surface-border bg-muted/40 px-3 py-2">
          <p className="text-xs text-muted-foreground">
            {intl.formatMessage({ id: 'mcp.oauth.manualUrlHint' })}
          </p>
          <p className="select-all break-all font-mono text-[11px] text-foreground">{authUrl}</p>
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

      {/* Credential form: shown for a provider with nothing saved, or whenever
          the user asks to edit. `editing` is deliberately not folded into
          `state.conn` — the badges above must keep reporting the real
          connection while the form is open. */}
      {state.conn !== 'loading' && (editing || state.conn === 'unconfigured') && (
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

              </li>
              <li>{t('setup.step4')}</li>
            </ol>

            <div className="space-y-1.5">
              <label className="text-xs font-medium text-muted-foreground">{t('field.clientId')}</label>
              <Input
                type="text"
                value={clientId}
                onChange={(e) => {
                  clientIdTouched.current = true;
                  setClientId(e.target.value);
                }}
                placeholder={clientIdPlaceholder}
              />
            </div>
            <div className="space-y-1.5">
              <label className="text-xs font-medium text-muted-foreground">{t('field.clientSecret')}</label>
              {/* A stored secret is never sent to the browser, so the field
                  starts empty either way. The placeholder is what tells the two
                  cases apart — and leaving it blank keeps the stored secret
                  rather than erasing it (the gateway falls back to the saved
                  client config when no secret is supplied). */}
              <Input
                type="password"
                value={clientSecret}
                onChange={(e) => setClientSecret(e.target.value)}
                placeholder={
                  state.hasSecret
                    ? intl.formatMessage(
                        { id: `${prefix}.field.clientSecretKeep` },
                        { mask: state.secretMasked || '••••' },
                      )
                    : clientSecretPlaceholder
                }
              />
              {state.hasSecret && (
                <p className="text-xs text-muted-foreground">{t('field.clientSecretKeepHint')}</p>
              )}
            </div>

            {providerId === 'google' && (
              <div className="space-y-3">
                <p className="text-sm text-muted-foreground">{intl.formatMessage({ id: 'google.wizard.consentHint' })}</p>
                <GoogleDataDisclosure
                  confirmed={googleDataConfirmed}
                  onConfirmedChange={setGoogleDataConfirmed}
                />
              </div>
            )}

            <div className="flex flex-wrap gap-2">
              <Button variant="brand" onClick={handleSaveAndConnect} disabled={busy || !clientId.trim() || (providerId === 'google' && !googleDataConfirmed)}>
                {busy ? <Loader2 className="animate-spin" /> : <Plug />}
                {t('action.connect')}
              </Button>
              {/* Only offered when there is something to go back to — a provider
                  with nothing saved has no prior view to cancel into. */}
              {editing && (
                <Button variant="outline" onClick={cancelEditing} disabled={busy}>
                  {t('action.cancelEdit')}
                </Button>
              )}
            </div>
          </CardContent>
        </Card>
      )}

      {/* State: configured but not connected — one-click connect. */}
      {state.conn === 'configured' && !editing && (
        <Card>
          <CardContent className="flex flex-col items-stretch gap-3 pt-6">
            <p className="text-sm text-muted-foreground">{t('configured.hint')}</p>
            {savedSummary}
            {providerId === 'google' && (
              <div className="space-y-3">
                <p className="text-sm text-muted-foreground">{intl.formatMessage({ id: 'google.wizard.consentHint' })}</p>
                <GoogleDataDisclosure
                  confirmed={googleDataConfirmed}
                  onConfirmedChange={setGoogleDataConfirmed}
                />
              </div>
            )}
            <div className="flex gap-2">
              <Button variant="brand" onClick={() => openConsent()} disabled={busy || (providerId === 'google' && !googleDataConfirmed)}>
                {busy ? <Loader2 className="animate-spin" /> : <Plug />}
                {t('action.connect')}
              </Button>
              <Button variant="outline" onClick={() => setEditing(true)} disabled={busy}>
                <KeyRound />
                {t('action.editCredentials')}
              </Button>
            </div>
          </CardContent>
        </Card>
      )}

      {/* State: connected — status + scopes + disconnect. */}
      {state.conn === 'connected' && !editing && (
        <Card>
          <CardHeader>
            <CardTitle className="flex items-center gap-2 text-sm">
              <CheckCircle2 className="size-4 text-success" />
              {t('connected.title')}
            </CardTitle>
          </CardHeader>
          <CardContent className="space-y-4">
            {isBound && savedSummary}
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
            <div className="flex flex-wrap gap-2">
              {/* Editing credentials while connected used to require
                  disconnecting first — a destructive step to change a typo. */}
              <Button variant="outline" size="sm" onClick={() => setEditing(true)} disabled={busy}>
                <KeyRound />
                {t('action.editCredentials')}
              </Button>
              <Button variant="destructive" size="sm" onClick={handleDisconnect} disabled={busy}>
                {busy ? <Loader2 className="animate-spin" /> : <Trash2 />}
                {t('action.disconnect')}
              </Button>
            </div>
          </CardContent>
        </Card>
      )}
    </div>
  );
}
