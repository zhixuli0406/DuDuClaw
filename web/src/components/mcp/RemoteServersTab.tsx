import { useCallback, useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { api, type McpRemoteAuth, type McpRemoteServerStatus } from '@/lib/api';
import { openExternal } from '@/lib/external-link';
import { toast } from '@/lib/toast';
import { useAuthStore } from '@/stores/auth-store';
import {
  Button,
  Badge,
  Input,
  Segmented,
  Empty,
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogFooter,
  DialogClose,
  type SegmentedOption,
} from '@/components/mds';
import { AlertTriangle, Cloud, Loader2, RefreshCw, Unplug, Trash2, ExternalLink } from 'lucide-react';
import { AgentSelect, type AgentLite } from './AgentSelect';
import { ConfirmDialog } from '@/components/settings/controls';
import { ToolEffectsPanel } from './ToolEffectsPanel';

/** The dashboard origin the OAuth redirect comes back to (validated by the gateway). */
export function dashboardOrigin(): string {
  return typeof window === 'undefined' ? '' : window.location.origin;
}

/** Same rule as the gateway's `sanitize_mcp_name`. */
export function sanitizeServerName(raw: string, fallback = 'remote'): string {
  const cleaned = raw
    .replace(/[^A-Za-z0-9._-]/g, '-')
    .slice(0, 64)
    .replace(/^-+|-+$/g, '');
  return cleaned === '' ? fallback : cleaned;
}

export function isValidServerName(name: string): boolean {
  return /^[A-Za-z0-9._-]{1,64}$/.test(name) && !name.toLowerCase().startsWith('duduclaw');
}

const POLL_MS = 3000;

export interface RemoteConnectInitial {
  agentId?: string;
  name?: string;
  url?: string;
  auth?: McpRemoteAuth;
  /** Fixed agent + name (reconnect): the stored URL is reused when `url` is empty. */
  locked?: boolean;
  /** Provider label for the third-party notice. */
  provider?: string;
  thirdParty?: boolean;
  docsUrl?: string;
}

/**
 * Connect a remote MCP server for one employee: OAuth sign-in (opens the
 * provider's page, then waits for the gateway callback), a bearer token, or
 * no authentication. Admin only (the gateway refuses everyone else).
 */
export function RemoteConnectDialog({
  open,
  onClose,
  agents,
  initial,
  onConnected,
}: {
  open: boolean;
  onClose: () => void;
  agents: ReadonlyArray<AgentLite>;
  initial: RemoteConnectInitial;
  onConnected?: (agentId: string, server: string) => void;
}) {
  const intl = useIntl();
  const [agentId, setAgentId] = useState(initial.agentId ?? '');
  const [name, setName] = useState(initial.name ?? '');
  const [url, setUrl] = useState(initial.url ?? '');
  const [auth, setAuth] = useState<McpRemoteAuth>(initial.auth ?? 'oauth');
  const [bearer, setBearer] = useState('');
  const [clientId, setClientId] = useState('');
  const [clientSecret, setClientSecret] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [authorizeUrl, setAuthorizeUrl] = useState<string | null>(null);
  // Set when the dashboard address cannot receive the OAuth redirect (plain
  // http on a LAN address): the provider sends the browser to this loopback
  // address and the operator pastes the address-bar URL back.
  const [pasteRedirect, setPasteRedirect] = useState<string | null>(null);
  const [pasted, setPasted] = useState('');
  const pollRef = useRef<ReturnType<typeof setInterval> | null>(null);

  const stopPolling = useCallback(() => {
    if (pollRef.current) clearInterval(pollRef.current);
    pollRef.current = null;
  }, []);

  useEffect(() => {
    if (!open) return;
    setAgentId(initial.agentId ?? '');
    setName(initial.name ?? '');
    setUrl(initial.url ?? '');
    setAuth(initial.auth ?? 'oauth');
    setBearer('');
    setClientId('');
    setClientSecret('');
    setError(null);
    setAuthorizeUrl(null);
    setPasteRedirect(null);
    setPasted('');
  }, [open, initial.agentId, initial.name, initial.url, initial.auth]);

  useEffect(() => stopPolling, [stopPolling]);

  const close = () => {
    stopPolling();
    setBearer('');
    setClientSecret('');
    onClose();
  };

  const nameOk = isValidServerName(name);
  const urlNeeded = !initial.locked;
  const canSubmit =
    !busy && agentId !== '' && nameOk && (!urlNeeded || url.trim() !== '') && (auth !== 'bearer' || bearer.trim() !== '');

  const waitForCallback = (expiresInSec: number, startedAt: number) => {
    stopPolling();
    const deadline = startedAt + expiresInSec * 1000;
    pollRef.current = setInterval(async () => {
      if (Date.now() > deadline) {
        stopPolling();
        setError(intl.formatMessage({ id: 'mcp.remote.signInExpired' }));
        return;
      }
      try {
        const res = await api.mcp.remoteStatus(agentId);
        const row = res.servers.find((s) => s.server === name);
        if (row && row.status === 'connected' && Date.parse(row.updated_at) >= startedAt - 2000) {
          stopPolling();
          toast.success(intl.formatMessage({ id: 'mcp.remote.connected' }, { server: name }));
          onConnected?.(agentId, name);
          close();
        }
      } catch {
        /* keep waiting; a transient error must not abort the sign-in */
      }
    }, POLL_MS);
  };

  const submit = async () => {
    if (!canSubmit) return;
    setBusy(true);
    setError(null);
    const startedAt = Date.now();
    try {
      const res = await api.mcp.remoteConnect({
        agent_id: agentId,
        name,
        ...(url.trim() ? { url: url.trim() } : {}),
        auth,
        ...(auth === 'bearer' ? { bearer: bearer.trim() } : {}),
        ...(auth === 'oauth' ? { redirect_origin: dashboardOrigin() } : {}),
        ...(auth === 'oauth' && clientId.trim() ? { client_id: clientId.trim() } : {}),
        ...(auth === 'oauth' && clientSecret.trim() ? { client_secret: clientSecret.trim() } : {}),
      });
      setBearer('');
      setClientSecret('');
      if (res.status === 'connected') {
        toast.success(intl.formatMessage({ id: 'mcp.remote.connected' }, { server: name }));
        onConnected?.(agentId, name);
        close();
      } else {
        setAuthorizeUrl(res.authorize_url);
        setPasteRedirect(res.completion === 'paste' ? (res.redirect_uri ?? '') : null);
        openExternal(res.authorize_url);
        // Polling also covers the paste case: a browser on the gateway host
        // lands on the gateway's own callback and finishes by itself.
        waitForCallback(res.expires_in, startedAt);
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const submitPasted = async () => {
    const value = pasted.trim();
    if (!value || busy) return;
    setBusy(true);
    setError(null);
    try {
      const res = await api.mcp.remoteComplete(value);
      stopPolling();
      toast.success(intl.formatMessage({ id: 'mcp.remote.connected' }, { server: res.server }));
      onConnected?.(res.agent_id, res.server);
      close();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const authOptions: SegmentedOption<McpRemoteAuth>[] = [
    { value: 'oauth', label: intl.formatMessage({ id: 'mcp.remote.auth.oauth' }) },
    { value: 'bearer', label: intl.formatMessage({ id: 'mcp.remote.auth.bearer' }) },
    { value: 'none', label: intl.formatMessage({ id: 'mcp.remote.auth.none' }) },
  ];

  return (
    <Dialog open={open} onOpenChange={(o) => { if (!o) close(); }}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{intl.formatMessage({ id: 'mcp.remote.connectTitle' })}</DialogTitle>
          <DialogDescription>{intl.formatMessage({ id: 'mcp.remote.connectDesc' })}</DialogDescription>
        </DialogHeader>

        {initial.thirdParty && (
          <div className="flex gap-2 rounded-lg border border-warning/30 bg-warning/10 p-3 text-xs text-foreground" role="note">
            <AlertTriangle className="mt-0.5 size-4 shrink-0 text-warning" />
            <p>
              {intl.formatMessage(
                { id: 'mcp.remote.thirdPartyNotice' },
                { provider: initial.provider ?? intl.formatMessage({ id: 'mcp.remote.thisProvider' }) },
              )}
            </p>
          </div>
        )}

        {authorizeUrl ? (
          <div className="space-y-3" data-testid="remote-waiting">
            <p className="flex items-center gap-2 text-sm text-foreground">
              <Loader2 className="size-4 animate-spin text-brand" />
              {intl.formatMessage({ id: 'mcp.remote.waiting' })}
            </p>
            <p className="text-xs text-muted-foreground">{intl.formatMessage({ id: 'mcp.remote.popupHint' })}</p>
            <a
              href={authorizeUrl}
              target="_blank"
              rel="noopener noreferrer"
              className="inline-flex items-center gap-1 text-xs text-brand underline-offset-2 hover:underline"
            >
              <ExternalLink className="size-3" />
              {intl.formatMessage({ id: 'mcp.remote.openSignIn' })}
            </a>
            {pasteRedirect !== null && (
              <div className="space-y-2 rounded-lg border border-border bg-muted/40 p-3" data-testid="remote-paste">
                <p className="text-xs text-foreground">
                  {intl.formatMessage({ id: 'mcp.remote.pasteHint' }, { address: pasteRedirect })}
                </p>
                <label className="sr-only" htmlFor="remote-pasted">
                  {intl.formatMessage({ id: 'mcp.remote.pasteLabel' })}
                </label>
                <Input
                  id="remote-pasted"
                  value={pasted}
                  inputMode="url"
                  spellCheck={false}
                  autoComplete="off"
                  placeholder={`${pasteRedirect}?code=…`}
                  onChange={(e) => setPasted(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter') void submitPasted();
                  }}
                />
              </div>
            )}
          </div>
        ) : (
          <div className="space-y-3">
            <div className="space-y-1.5">
              <label className="text-xs font-medium text-muted-foreground">
                {intl.formatMessage({ id: 'mcp.targetAgent' })}
              </label>
              {initial.locked ? (
                <p className="text-sm text-foreground">{agents.find((a) => a.name === agentId)?.display_name ?? agentId}</p>
              ) : (
                <AgentSelect
                  value={agentId}
                  onChange={setAgentId}
                  agents={agents}
                  placeholder={intl.formatMessage({ id: 'mcp.targetAgent' })}
                />
              )}
            </div>
            <div className="space-y-1.5">
              <label className="text-xs font-medium text-muted-foreground" htmlFor="remote-name">
                {intl.formatMessage({ id: 'mcp.serverName' })}
              </label>
              <Input
                id="remote-name"
                value={name}
                disabled={initial.locked}
                onChange={(e) => setName(e.target.value)}
                aria-invalid={name !== '' && !nameOk}
              />
              {name !== '' && !nameOk && (
                <p className="text-xs text-destructive">{intl.formatMessage({ id: 'mcp.remote.nameRule' })}</p>
              )}
            </div>
            <div className="space-y-1.5">
              <label className="text-xs font-medium text-muted-foreground" htmlFor="remote-url">
                {intl.formatMessage({ id: 'mcp.remote.url' })}
              </label>
              <Input
                id="remote-url"
                value={url}
                inputMode="url"
                spellCheck={false}
                placeholder={initial.locked ? intl.formatMessage({ id: 'mcp.remote.urlKeep' }) : 'https://'}
                onChange={(e) => setUrl(e.target.value)}
              />
              <p className="text-xs text-muted-foreground">{intl.formatMessage({ id: 'mcp.remote.urlHelp' })}</p>
              {initial.docsUrl && (
                <a
                  href={initial.docsUrl}
                  target="_blank"
                  rel="noopener noreferrer"
                  className="inline-flex items-center gap-1 text-xs text-brand underline-offset-2 hover:underline"
                >
                  <ExternalLink className="size-3" />
                  {intl.formatMessage({ id: 'mcp.remote.providerDocs' })}
                </a>
              )}
            </div>
            <div className="space-y-1.5">
              <span className="text-xs font-medium text-muted-foreground">{intl.formatMessage({ id: 'mcp.remote.auth' })}</span>
              <Segmented
                value={auth}
                onValueChange={setAuth}
                options={authOptions}
                aria-label={intl.formatMessage({ id: 'mcp.remote.auth' })}
              />
              <p className="text-xs text-muted-foreground">
                {intl.formatMessage({ id: `mcp.remote.authHelp.${auth}` })}
              </p>
            </div>
            {auth === 'bearer' && (
              <div className="space-y-1.5">
                <label className="text-xs font-medium text-muted-foreground" htmlFor="remote-bearer">
                  {intl.formatMessage({ id: 'mcp.remote.bearer' })}
                </label>
                <Input
                  id="remote-bearer"
                  type="password"
                  autoComplete="off"
                  spellCheck={false}
                  value={bearer}
                  onChange={(e) => setBearer(e.target.value)}
                />
              </div>
            )}
            {auth === 'oauth' && (
              <details className="text-xs">
                <summary className="cursor-pointer text-muted-foreground">
                  {intl.formatMessage({ id: 'mcp.remote.ownClient' })}
                </summary>
                <div className="mt-2 space-y-2">
                  <p className="text-muted-foreground">{intl.formatMessage({ id: 'mcp.remote.ownClientHelp' })}</p>
                  <Input
                    aria-label={intl.formatMessage({ id: 'mcp.oauth.clientId' })}
                    placeholder={intl.formatMessage({ id: 'mcp.oauth.clientId' })}
                    value={clientId}
                    onChange={(e) => setClientId(e.target.value)}
                  />
                  <Input
                    type="password"
                    autoComplete="off"
                    aria-label={intl.formatMessage({ id: 'mcp.oauth.clientSecret' })}
                    placeholder={intl.formatMessage({ id: 'mcp.oauth.clientSecret' })}
                    value={clientSecret}
                    onChange={(e) => setClientSecret(e.target.value)}
                  />
                </div>
              </details>
            )}
          </div>
        )}

        {error && (
          <p className="text-xs text-destructive" role="alert">
            {error}
          </p>
        )}

        <DialogFooter>
          <DialogClose render={<Button variant="outline">{intl.formatMessage({ id: 'mcp.cancel' })}</Button>} />
          {authorizeUrl && pasteRedirect !== null && (
            <Button variant="brand" onClick={submitPasted} disabled={busy || pasted.trim() === ''}>
              {busy ? <Loader2 className="animate-spin" /> : null}
              {intl.formatMessage({ id: 'mcp.remote.pasteSubmit' })}
            </Button>
          )}
          {!authorizeUrl && (
            <Button variant="brand" onClick={submit} disabled={!canSubmit}>
              {busy ? <Loader2 className="animate-spin" /> : null}
              {intl.formatMessage({ id: auth === 'oauth' ? 'mcp.remote.signIn' : 'mcp.remote.connect' })}
            </Button>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function statusBadge(row: McpRemoteServerStatus, intl: ReturnType<typeof useIntl>) {
  switch (row.status) {
    case 'connected':
      return <Badge variant="secondary" className="bg-success/15 text-success">{intl.formatMessage({ id: 'mcp.remote.status.connected' })}</Badge>;
    case 'needs_reauth':
      return <Badge variant="destructive">{intl.formatMessage({ id: 'mcp.remote.status.needs_reauth' })}</Badge>;
    default:
      return <Badge variant="secondary" className="bg-warning/15 text-warning">{intl.formatMessage({ id: 'mcp.remote.status.not_connected' })}</Badge>;
  }
}

/** Remote MCP servers connected through the native bridge (Admin). */
export function RemoteServersTab({ agents }: { agents: ReadonlyArray<AgentLite> }) {
  const intl = useIntl();
  const isAdmin = useAuthStore((s) => s.user?.role === 'admin');
  const [rows, setRows] = useState<McpRemoteServerStatus[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [dialog, setDialog] = useState<RemoteConnectInitial | null>(null);
  const [confirm, setConfirm] = useState<{ row: McpRemoteServerStatus; forget: boolean } | null>(null);

  const agentLabel = (id: string) => agents.find((a) => a.name === id)?.display_name ?? id;

  const load = useCallback(async () => {
    if (!isAdmin) return;
    setLoading(true);
    setError(null);
    try {
      const res = await api.mcp.remoteStatus();
      setRows(res.servers ?? []);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }, [isAdmin]);

  useEffect(() => {
    void load();
  }, [load]);

  if (!isAdmin) {
    return <Empty icon={Cloud} title={intl.formatMessage({ id: 'mcp.remote.adminOnly' })} />;
  }

  const disconnect = async (row: McpRemoteServerStatus, forget: boolean) => {
    try {
      await api.mcp.remoteDisconnect(row.agent_id, row.server, forget);
      toast.success(intl.formatMessage({ id: forget ? 'mcp.remote.removed' : 'mcp.remote.disconnected' }, { server: row.server }));
      await load();
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e));
    }
  };

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <p className="text-xs text-muted-foreground">{intl.formatMessage({ id: 'mcp.remote.sectionDesc' })}</p>
        <div className="flex gap-2">
          <Button variant="outline" size="sm" onClick={() => void load()}>
            <RefreshCw />
            <span className="hidden sm:inline">{intl.formatMessage({ id: 'common.refresh' })}</span>
          </Button>
          <Button variant="brand" size="sm" onClick={() => setDialog({})}>
            <Cloud />
            <span className="hidden sm:inline">{intl.formatMessage({ id: 'mcp.remote.add' })}</span>
          </Button>
        </div>
      </div>

      {loading && rows.length === 0 ? (
        <div className="flex justify-center py-10">
          <Loader2 className="size-5 animate-spin text-brand" />
        </div>
      ) : error ? (
        <p className="text-sm text-destructive" role="alert">{error}</p>
      ) : rows.length === 0 ? (
        <Empty icon={Cloud} title={intl.formatMessage({ id: 'mcp.remote.empty' })} />
      ) : (
        <div className="overflow-hidden rounded-xl border border-surface-border divide-y divide-surface-border">
          {rows.map((row) => (
            <div key={`${row.agent_id}/${row.server}`} className="flex flex-wrap items-center gap-3 px-4 py-3" data-testid="remote-row">
              <div className="min-w-0 flex-1">
                <div className="flex flex-wrap items-center gap-2">
                  <span className="truncate text-sm font-medium text-foreground">{row.server}</span>
                  {statusBadge(row, intl)}
                  <Badge variant="secondary">{intl.formatMessage({ id: `mcp.remote.auth.${row.auth}` })}</Badge>
                  {!row.installed && (
                    <Badge variant="secondary">{intl.formatMessage({ id: 'mcp.remote.notInstalled' })}</Badge>
                  )}
                </div>
                <p className="truncate text-xs text-muted-foreground">
                  {agentLabel(row.agent_id)} · {row.host}
                </p>
              </div>
              <div className="flex shrink-0 gap-1.5">
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => setDialog({ agentId: row.agent_id, name: row.server, auth: row.auth, locked: true })}
                >
                  {intl.formatMessage({ id: row.status === 'connected' ? 'mcp.remote.reconnect' : 'mcp.remote.connect' })}
                </Button>
                {row.status !== 'not_connected' && (
                  <Button
                    variant="ghost"
                    size="sm"
                    aria-label={intl.formatMessage({ id: 'mcp.remote.disconnect' })}
                    onClick={() => setConfirm({ row, forget: false })}
                  >
                    <Unplug />
                  </Button>
                )}
                <Button
                  variant="ghost"
                  size="sm"
                  aria-label={intl.formatMessage({ id: 'mcp.remote.remove' })}
                  onClick={() => setConfirm({ row, forget: true })}
                >
                  <Trash2 />
                </Button>
              </div>
            </div>
          ))}
        </div>
      )}

      <ToolEffectsPanel agents={agents} />

      <RemoteConnectDialog
        open={dialog !== null}
        onClose={() => setDialog(null)}
        agents={agents}
        initial={dialog ?? {}}
        onConnected={() => void load()}
      />

      {confirm && (
        <ConfirmDialog
          open
          title={intl.formatMessage({ id: confirm.forget ? 'mcp.remote.remove' : 'mcp.remote.disconnect' })}
          message={intl.formatMessage(
            { id: confirm.forget ? 'mcp.remote.removeConfirm' : 'mcp.remote.disconnectConfirm' },
            { server: confirm.row.server, agent: agentLabel(confirm.row.agent_id) },
          )}
          confirmLabel={intl.formatMessage({ id: confirm.forget ? 'mcp.remote.remove' : 'mcp.remote.disconnect' })}
          onConfirm={() => {
            const c = confirm;
            setConfirm(null);
            void disconnect(c.row, c.forget);
          }}
          onClose={() => setConfirm(null)}
        />
      )}
    </div>
  );
}
