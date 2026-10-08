import { useState } from 'react';
import { useIntl } from 'react-intl';
import { api, type McpRegistryHit } from '@/lib/api';
import { isImeComposing } from '@/lib/keyboard';
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
import { Search, Loader2, Download, ExternalLink, Cloud, Package } from 'lucide-react';
import { RequiredEnvFields, missingRequiredEnv, requiredEnvPayload } from '@/components/shared/RequiredEnvFields';
import { AutonomyNote } from '@/components/AutonomyNote';
import { AgentSelect, type AgentLite } from './AgentSelect';
import { RemoteConnectDialog, isValidServerName, sanitizeServerName, type RemoteConnectInitial } from './RemoteServersTab';

/**
 * Search the official MCP Registry and install a server to one employee.
 * Packages go through the same scanned install path as an import (Admins
 * install, everyone else files an install request); a remote server is then
 * connected (sign-in) by an Admin.
 */
export function McpRegistryTab({
  agents,
  onInstalled,
}: {
  agents: ReadonlyArray<AgentLite>;
  onInstalled: () => void;
}) {
  const intl = useIntl();
  const [query, setQuery] = useState('');
  const [hits, setHits] = useState<McpRegistryHit[]>([]);
  const [cursor, setCursor] = useState<string | null>(null);
  const [searched, setSearched] = useState(false);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [installing, setInstalling] = useState<McpRegistryHit | null>(null);
  const [connect, setConnect] = useState<RemoteConnectInitial | null>(null);

  const run = async (more: boolean) => {
    setLoading(true);
    setError(null);
    try {
      const res = await api.mcp.registrySearch(query.trim(), more ? cursor : null);
      setHits(more ? [...hits, ...res.servers] : res.servers);
      setCursor(res.next_cursor);
      setSearched(true);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  };

  return (
    <div className="space-y-4">
      <p className="text-xs text-muted-foreground">{intl.formatMessage({ id: 'mcp.registry.desc' })}</p>
      <form
        className="flex gap-2"
        onSubmit={(e) => {
          e.preventDefault();
          void run(false);
        }}
      >
        <Input
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={(e) => {
            if (isImeComposing(e)) e.stopPropagation();
          }}
          placeholder={intl.formatMessage({ id: 'mcp.registry.placeholder' })}
          aria-label={intl.formatMessage({ id: 'mcp.registry.placeholder' })}
          className="w-full sm:w-80"
          maxLength={100}
        />
        <Button type="submit" variant="brand" size="sm" disabled={loading}>
          {loading ? <Loader2 className="animate-spin" /> : <Search />}
          <span className="hidden sm:inline">{intl.formatMessage({ id: 'mcp.registry.search' })}</span>
        </Button>
      </form>

      {error && (
        <p className="text-sm text-destructive" role="alert">
          {error}
        </p>
      )}

      {searched && hits.length === 0 && !loading && !error ? (
        <Empty icon={Search} title={intl.formatMessage({ id: 'mcp.registry.noResults' })} />
      ) : hits.length > 0 ? (
        <div className="overflow-hidden rounded-xl border border-surface-border divide-y divide-surface-border">
          {hits.map((hit) => (
            <RegistryRow key={`${hit.name}@${hit.version}`} hit={hit} onInstall={() => setInstalling(hit)} />
          ))}
        </div>
      ) : null}

      {cursor && hits.length > 0 && (
        <div className="flex justify-center">
          <Button variant="outline" size="sm" onClick={() => void run(true)} disabled={loading}>
            {intl.formatMessage({ id: 'mcp.registry.more' })}
          </Button>
        </div>
      )}

      {installing && (
        <RegistryInstallDialog
          hit={installing}
          agents={agents}
          onClose={() => setInstalling(null)}
          onDone={(result) => {
            setInstalling(null);
            onInstalled();
            if (result) setConnect(result);
          }}
        />
      )}

      <RemoteConnectDialog
        open={connect !== null}
        onClose={() => setConnect(null)}
        agents={agents}
        initial={connect ?? {}}
        onConnected={onInstalled}
      />
    </div>
  );
}

function RegistryRow({ hit, onInstall }: { hit: McpRegistryHit; onInstall: () => void }) {
  const intl = useIntl();
  const kinds = [...hit.package_kinds, ...(hit.has_remotes ? ['remote'] : [])];
  return (
    <div className="flex items-start gap-3 px-4 py-3" data-testid="registry-row">
      <div className="flex size-8 shrink-0 items-center justify-center rounded-lg bg-muted">
        {hit.install_is_remote ? <Cloud className="size-4 text-muted-foreground" /> : <Package className="size-4 text-muted-foreground" />}
      </div>
      <div className="min-w-0 flex-1 space-y-0.5">
        <div className="flex flex-wrap items-center gap-2">
          <span className="truncate text-sm font-medium text-foreground" title={hit.name}>
            {hit.title || hit.name}
          </span>
          {hit.version && <span className="font-mono text-xs text-muted-foreground">{hit.version}</span>}
          {kinds.map((k) => (
            <Badge key={k} variant="secondary">
              {k}
            </Badge>
          ))}
        </div>
        {hit.title && <p className="truncate font-mono text-xs text-muted-foreground/80">{hit.name}</p>}
        <p className="line-clamp-2 text-xs text-muted-foreground">{hit.description}</p>
        <div className="flex flex-wrap gap-x-3 text-xs text-muted-foreground/80">
          {hit.required_env.some((e) => e.required) && (
            <span>
              {intl.formatMessage(
                { id: 'mcp.catalog.requiresEnv' },
                { vars: hit.required_env.filter((e) => e.required).map((e) => e.name).join(', ') },
              )}
            </span>
          )}
          {hit.install_is_remote && <span>{intl.formatMessage({ id: 'mcp.registry.needsSignIn' })}</span>}
          {!hit.installable && hit.reason && (
            <span className="text-warning">{intl.formatMessage({ id: `mcp.registry.reason.${hit.reason}`, defaultMessage: hit.reason })}</span>
          )}
          {hit.repository_url && (
            <a
              href={hit.repository_url}
              target="_blank"
              rel="noopener noreferrer"
              className="inline-flex items-center gap-1 text-brand underline-offset-2 hover:underline"
            >
              <ExternalLink className="size-3" />
              {intl.formatMessage({ id: 'mcp.registry.repository' })}
            </a>
          )}
        </div>
      </div>
      <Button variant="outline" size="sm" className="shrink-0" disabled={!hit.installable} onClick={onInstall}>
        <Download />
        <span className="hidden sm:inline">{intl.formatMessage({ id: 'mcp.catalog.install' })}</span>
      </Button>
    </div>
  );
}

function RegistryInstallDialog({
  hit,
  agents,
  onClose,
  onDone,
}: {
  hit: McpRegistryHit;
  agents: ReadonlyArray<AgentLite>;
  onClose: () => void;
  /** Called after success; a value means "now connect this remote server". */
  onDone: (connect: RemoteConnectInitial | null) => void;
}) {
  const intl = useIntl();
  const isAdmin = useAuthStore((s) => s.user?.role === 'admin');
  const hasPackage = hit.installable && !hit.install_is_remote;
  const remoteOk = hit.has_remotes && hit.installable && (hit.install_is_remote || hit.remote_kinds.some((k) => k !== 'sse'));
  const [mode, setMode] = useState<'package' | 'remote'>(hasPackage ? 'package' : 'remote');
  const [agentId, setAgentId] = useState('');
  const [name, setName] = useState(sanitizeServerName(hit.name, 'registry-server'));
  const [env, setEnv] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const required = mode === 'package' ? hit.required_env.filter((e) => e.required).map((e) => e.name) : [];
  const optional = mode === 'package' ? hit.required_env.filter((e) => !e.required) : [];
  const nameOk = isValidServerName(name);
  const canSubmit = !busy && agentId !== '' && nameOk && missingRequiredEnv(required, env).length === 0;

  const close = () => {
    setEnv({});
    onClose();
  };

  const submit = async () => {
    if (!canSubmit) return;
    setBusy(true);
    setError(null);
    try {
      const res = await api.mcp.registryInstall({
        name: hit.name,
        ...(hit.version ? { version: hit.version } : {}),
        agent_id: agentId,
        remote: mode === 'remote',
        server_name: name,
        ...(required.length > 0 ? { env: requiredEnvPayload(required, env) } : {}),
      });
      setEnv({});
      if (res.mode === 'requested') {
        toast.success(intl.formatMessage({ id: 'mcp.registry.requested' }, { server: name }));
        onDone(null);
      } else if (res.remote && res.needs_connect) {
        toast.success(intl.formatMessage({ id: 'mcp.registry.installedConnect' }, { server: name }));
        onDone({
          agentId,
          name,
          auth: res.remote_needs_bearer ? 'bearer' : 'oauth',
          locked: true,
          declaredHeaders: res.remote_headers ?? hit.remote_headers ?? [],
        });
      } else {
        toast.success(intl.formatMessage({ id: 'mcp.added' }, { server: name, agent: agentId }));
        onDone(null);
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const modeOptions: SegmentedOption<'package' | 'remote'>[] = [
    { value: 'package', label: intl.formatMessage({ id: 'mcp.registry.mode.package' }) },
    { value: 'remote', label: intl.formatMessage({ id: 'mcp.registry.mode.remote' }) },
  ];

  return (
    <Dialog open onOpenChange={(o) => { if (!o) close(); }}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{intl.formatMessage({ id: 'mcp.catalog.install' })}</DialogTitle>
          <DialogDescription>
            {hit.title || hit.name} {hit.version && <span className="font-mono">{hit.version}</span>}
          </DialogDescription>
        </DialogHeader>
        <div className="space-y-3">
          {hasPackage && remoteOk && (
            <Segmented value={mode} onValueChange={setMode} options={modeOptions} aria-label={intl.formatMessage({ id: 'mcp.registry.mode' })} />
          )}
          <div className="space-y-1.5">
            <label className="text-xs font-medium text-muted-foreground">{intl.formatMessage({ id: 'mcp.targetAgent' })}</label>
            <AgentSelect value={agentId} onChange={setAgentId} agents={agents} placeholder={intl.formatMessage({ id: 'mcp.targetAgent' })} />
          </div>
          <div className="space-y-1.5">
            <label className="text-xs font-medium text-muted-foreground" htmlFor="registry-name">
              {intl.formatMessage({ id: 'mcp.serverName' })}
            </label>
            <Input id="registry-name" value={name} onChange={(e) => setName(e.target.value)} aria-invalid={!nameOk} />
            {!nameOk && <p className="text-xs text-destructive">{intl.formatMessage({ id: 'mcp.remote.nameRule' })}</p>}
          </div>
          <RequiredEnvFields required={required} values={env} onChange={setEnv} />
          {optional.length > 0 && (
            <p className="text-xs text-muted-foreground">
              {intl.formatMessage({ id: 'mcp.registry.optionalEnv' }, { vars: optional.map((e) => e.name).join(', ') })}
            </p>
          )}
          {mode === 'remote' && (
            <p className="text-xs text-muted-foreground">
              {intl.formatMessage({ id: isAdmin ? 'mcp.registry.remoteNext' : 'mcp.registry.remoteNextNonAdmin' })}
            </p>
          )}
          {mode === 'remote' && (hit.remote_headers?.length ?? 0) > 0 && (
            <p className="text-xs text-muted-foreground">
              {intl.formatMessage(
                { id: 'mcp.registry.remoteHeaders' },
                { headers: (hit.remote_headers ?? []).map((h) => h.name).join(', ') },
              )}
            </p>
          )}
          {!isAdmin && <p className="text-xs text-muted-foreground">{intl.formatMessage({ id: 'mcp.registry.requestNote' })}</p>}
          <AutonomyNote id="mcpInstall" />
          {error && (
            <p className="text-xs text-destructive" role="alert">
              {error}
            </p>
          )}
        </div>
        <DialogFooter>
          <DialogClose render={<Button variant="outline">{intl.formatMessage({ id: 'mcp.cancel' })}</Button>} />
          <Button variant="brand" onClick={submit} disabled={!canSubmit}>
            {busy ? <Loader2 className="animate-spin" /> : null}
            {intl.formatMessage({ id: isAdmin ? 'mcp.catalog.install' : 'install.request.submit' })}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
