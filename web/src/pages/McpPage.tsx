import { useEffect, useState, useCallback, useRef } from 'react';
import { useIntl } from 'react-intl';
import { cn } from '@/lib/utils';
import { isImeComposing } from '@/lib/keyboard';
import { useUrlState, useUrlStateNullable } from '@/lib/use-url-state';
import { openExternal } from '@/lib/external-link';
import { useMcpStore } from '@/stores/mcp-store';
import { useAgentsStore } from '@/stores/agents-store';
import { useAuthStore } from '@/stores/auth-store';
import { useConnectionStore } from '@/stores/connection-store';
import { toast } from '@/lib/toast';
import { api, type McpServerDef, type McpCatalogItem, type McpOAuthProvider, type McpImportCandidate, type McpServerEntry } from '@/lib/api';
import { DangerZone, ConfirmDialog } from '@/components/settings/controls';
import { AutonomyNote } from '@/components/AutonomyNote';
import { AgentSelect, type AgentLite } from '@/components/mcp/AgentSelect';
import { McpRegistryTab } from '@/components/mcp/McpRegistryTab';
import { RemoteServersTab, RemoteConnectDialog } from '@/components/mcp/RemoteServersTab';
import {
  RequiredEnvFields,
  missingRequiredEnv,
  requiredEnvPayload,
  unmaskedEnv,
} from '@/components/shared/RequiredEnvFields';
import {
  Button,
  Badge,
  Input,
  Textarea,
  Select,
  SelectTrigger,
  SelectValue,
  SelectContent,
  SelectItem,
  Switch,
  Segmented,
  Card,
  CardContent,
  Empty,
  ErrorState,
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogFooter,
  DialogClose,
  DropdownMenu,
  DropdownMenuTrigger,
  DropdownMenuContent,
  DropdownMenuItem,
  ListGridContainer,
  ListGridHeader,
  ListGridHeaderCell,
  ListGridRow,
  ListGridCell,
  type SegmentedOption,
} from '@/components/mds';
import {
  Plug,
  Plus,
  Pencil,
  Trash2,
  Package,
  Globe,
  MessageSquare,
  Database,
  CheckCircle2,
  AlertTriangle,
  Loader2,
  Shield,
  ShieldCheck,
  ShieldAlert,
  KeyRound,
  Link as LinkIcon,
  Download,
  RefreshCw,
  MoreHorizontal,
  Cloud,
} from 'lucide-react';

const TABS = ['agents', 'marketplace', 'registry', 'remote'] as const;
type Tab = (typeof TABS)[number];

const categoryIcons: Record<string, typeof Globe> = {
  browser: Globe,
  data: Database,
  communication: MessageSquare,
  google: Globe,
  aggregator: Cloud,
};

function getCategoryIcon(category: string) {
  return categoryIcons[category] ?? Package;
}

/** Derive a transport label from a stdio server def (spec §4 transport Badge). */
function transportOf(def: McpServerDef): 'stdio' | 'http' {
  const blob = [def.command, ...def.args].join(' ').toLowerCase();
  return blob.includes('mcp-remote') || blob.includes('http') ? 'http' : 'stdio';
}

/**
 * What the add/edit dialog opens with. Every install path (header button,
 * marketplace card, a row's 編輯) goes through the one dialog, so the
 * command/args the operator may want to change are always on screen.
 * `lock` pins the agent and server name (editing an installed row).
 */
type AddDialogPreset = {
  catalogId?: string;
  agentId?: string;
  serverName?: string;
  lock?: boolean;
};

/** Normalize an agent config's `servers` (array, or the older object shape). */
function serverEntriesOf(servers: McpServerEntry[] | Record<string, McpServerDef>): McpServerEntry[] {
  return Array.isArray(servers)
    ? servers
    : Object.entries(servers).map(([name, def]) => ({ name, ...def }));
}

/** Column template shared by the MCP-server ListGrid header + rows (spec §4). */
const SERVER_COLUMNS =
  'minmax(0,1.4fr) minmax(0,1.6fr) minmax(0,0.7fr) minmax(0,0.5fr) 2.5rem';

export function McpPage() {
  const intl = useIntl();
  const connState = useConnectionStore((s) => s.state);
  // P05: `useMcpStore` has tracked `error` since it was written, but this page
  // destructured everything except that field — a failed `mcp.list` fell back
  // to the same "尚未設定任何 MCP Server" empty state as a clean install.
  const { agentConfigs, catalog, loading, error, fetchAll, oauthProviders, fetchOAuthProviders } =
    useMcpStore();
  const { agents, fetchAgents } = useAgentsStore();
  // P11 (state-as-URL). This page renders inside IntegrationsPage's `?tab=`
  // shell, so every param here is namespaced `mcp_*` — two components sharing
  // one query string must not fight over the same key.
  const [activeTab, setActiveTab] = useUrlState('mcp_tab', 'agents', { allowed: TABS });
  const [selectedAgentId, setSelectedAgentId] = useUrlStateNullable('mcp_agent');
  // `null` = closed. One dialog for every install / edit entry point.
  const [addPreset, setAddPreset] = useState<AddDialogPreset | null>(null);
  const isAdmin = useAuthStore((s) => s.user?.role === 'admin');
  const [showImportDialog, setShowImportDialog] = useState(false);
  const [catalogFilter, setCatalogFilter] = useUrlStateNullable('mcp_cat');
  const [catalogSearch, setCatalogSearch] = useUrlState('mcp_q', '');
  // Remove-server used to gate on the browser's native `window.confirm()`
  // (phase4 audit C06 Blocker) — route it through the shared ConfirmDialog.
  const [removeTarget, setRemoveTarget] = useState<{ agentId: string; serverName: string } | null>(null);

  const showToast = useCallback((type: 'success' | 'error', message: string) => {
    if (type === 'success') toast.success(message);
    else toast.error(message);
  }, []);

  useEffect(() => {
    if (connState === 'authenticated') {
      fetchAll();
      fetchAgents();
      fetchOAuthProviders();
    }
  }, [connState, fetchAll, fetchAgents, fetchOAuthProviders]);

  // Auto-select first agent
  useEffect(() => {
    if (!selectedAgentId && agentConfigs.length > 0) {
      setSelectedAgentId(agentConfigs[0].agent_id);
    }
  }, [agentConfigs, selectedAgentId]);

  const selectedConfig = agentConfigs.find((c) => c.agent_id === selectedAgentId);
  // Backend serializes servers as an array of `{ name, command, args, env }`
  // entries; normalize to McpServerEntry[]. The Array.isArray guard keeps older
  // object-shaped payloads working.
  const serverEntries: McpServerEntry[] = selectedConfig
    ? serverEntriesOf(selectedConfig.servers)
    : [];

  const installedServers = (agentId: string): McpServerEntry[] => {
    const cfg = agentConfigs.find((c) => c.agent_id === agentId);
    return cfg ? serverEntriesOf(cfg.servers) : [];
  };

  const agentLabel = (agentId: string) =>
    agents.find((a) => a.name === agentId)?.display_name ?? agentId;

  const handleRemoveServer = async (agentId: string, serverName: string) => {
    try {
      await useMcpStore.getState().removeServer(agentId, serverName);
      showToast('success', intl.formatMessage({ id: 'mcp.removed' }, { server: serverName, agent: agentLabel(agentId) }));
    } catch {
      showToast('error', intl.formatMessage({ id: 'mcp.loadFailed' }));
    }
  };

  const categories = [...new Set(catalog.map((c) => c.category))];

  const filteredCatalog = catalog.filter((item) => {
    const matchesCategory = !catalogFilter || item.category === catalogFilter;
    const matchesSearch =
      !catalogSearch ||
      item.name.toLowerCase().includes(catalogSearch.toLowerCase()) ||
      item.description.toLowerCase().includes(catalogSearch.toLowerCase());
    return matchesCategory && matchesSearch;
  });

  // Check if a catalog item is installed on any agent
  const isInstalled = (catalogId: string) =>
    agentConfigs.some((cfg) => serverEntriesOf(cfg.servers).some((s) => s.name === catalogId));

  // 2026-08-04 (D19): the separate 授權 sub-tab is gone. Services that need a
  // sign-in now show their state and their 授權 button on their own card, in
  // line under the servers they belong to — one place, not two.
  const tabOptions: SegmentedOption<Tab>[] = [
    { value: 'agents', label: intl.formatMessage({ id: 'mcp.tab.agents' }) },
    { value: 'marketplace', label: intl.formatMessage({ id: 'mcp.tab.marketplace' }) },
    { value: 'registry', label: intl.formatMessage({ id: 'mcp.tab.registry' }) },
    { value: 'remote', label: intl.formatMessage({ id: 'mcp.tab.remote' }) },
  ];

  // Google keeps its own dedicated tab on the integrations page (19 Workspace
  // tools, its own setup story), so it is not repeated in this list.
  const connectProviders = oauthProviders.filter((p) => p.provider_id !== 'google');

  return (
    <div className="space-y-6">
      {/* Slim header (inside the Integrations tab — no big page title). */}
      <div className="flex flex-wrap items-center justify-between gap-3">
        <p className="text-sm text-muted-foreground">
          {intl.formatMessage({ id: 'mcp.title' })}
        </p>
        <div className="flex gap-2">
          <Button
            variant="outline"
            size="sm"
            onClick={() => {
              fetchAll();
              fetchOAuthProviders();
            }}
          >
            <RefreshCw />
            <span className="hidden sm:inline">{intl.formatMessage({ id: 'common.refresh' })}</span>
          </Button>
          <Button variant="outline" size="sm" onClick={() => setShowImportDialog(true)}>
            <LinkIcon />
            <span className="hidden sm:inline">{intl.formatMessage({ id: 'mcp.import.fromUrl' })}</span>
          </Button>
          <Button variant="brand" size="sm" onClick={() => setAddPreset({})}>
            <Plus />
            <span className="hidden sm:inline">{intl.formatMessage({ id: 'mcp.add' })}</span>
          </Button>
        </div>
      </div>

      {/* Sub-navigation (agents / marketplace / oauth). */}
      <Segmented
        value={activeTab}
        onValueChange={setActiveTab}
        options={tabOptions}
        aria-label={intl.formatMessage({ id: 'mcp.title' })}
      />

      {loading ? (
        <div className="flex items-center justify-center py-16">
          <Loader2 className="size-6 animate-spin text-brand" />
        </div>
      ) : error && agentConfigs.length === 0 && catalog.length === 0 ? (
        <ErrorState
          error={error}
          title={intl.formatMessage({ id: 'errorState.manage.loadFailed' })}
          description={intl.formatMessage({ id: 'errorState.manage.notEmptyHint' })}
          onRetry={() => void fetchAll()}
        />
      ) : activeTab === 'agents' ? (
        <div className="space-y-4">
          {/* Agent picker → its configured servers. */}
          <div className="flex items-center justify-between gap-3">
            <h2 className="text-sm font-medium text-foreground">
              {intl.formatMessage({ id: 'mcp.targetAgent' })}
            </h2>
            {agentConfigs.length > 0 && (
              <AgentSelect
                value={selectedAgentId ?? ''}
                onChange={setSelectedAgentId}
                agents={agentConfigs.map((c) => ({ name: c.agent_id, display_name: agentLabel(c.agent_id) }))}
                placeholder={intl.formatMessage({ id: 'mcp.selectAgent' })}
                className="w-56"
              />
            )}
          </div>

          {agentConfigs.length === 0 ? (
            <Empty icon={Plug} title={intl.formatMessage({ id: 'mcp.empty' })} />
          ) : !selectedAgentId ? (
            <Empty icon={Plug} title={intl.formatMessage({ id: 'mcp.selectAgent' })} />
          ) : serverEntries.length === 0 ? (
            <Empty icon={Package} title={intl.formatMessage({ id: 'mcp.noServers' })} />
          ) : (
            <div className="overflow-hidden rounded-xl border border-surface-border">
              <ListGridContainer
                columns={SERVER_COLUMNS}
                className="!h-auto [&>[aria-hidden]]:hidden"
                header={
                  <ListGridHeader>
                    <ListGridHeaderCell>{intl.formatMessage({ id: 'mcp.serverName' })}</ListGridHeaderCell>
                    <ListGridHeaderCell>{intl.formatMessage({ id: 'mcp.command' })}</ListGridHeaderCell>
                    <ListGridHeaderCell>{intl.formatMessage({ id: 'mcp.col.transport' })}</ListGridHeaderCell>
                    <ListGridHeaderCell className="justify-end">
                      {intl.formatMessage({ id: 'mcp.col.env' })}
                    </ListGridHeaderCell>
                    <ListGridHeaderCell aria-hidden />
                  </ListGridHeader>
                }
              >
                {serverEntries.map((entry) => (
                  <ServerRow
                    key={entry.name}
                    entry={entry}
                    canEdit={isAdmin}
                    onEdit={() =>
                      selectedAgentId &&
                      setAddPreset({ agentId: selectedAgentId, serverName: entry.name, lock: true })
                    }
                    onRemove={() => selectedAgentId && setRemoveTarget({ agentId: selectedAgentId, serverName: entry.name })}
                  />
                ))}
              </ListGridContainer>
            </div>
          )}

          {/* Services that need a sign-in before their tools work (D19). */}
          <OAuthTab providers={connectProviders} showToast={showToast} />
        </div>
      ) : activeTab === 'registry' ? (
        <McpRegistryTab agents={agents} onInstalled={() => void fetchAll()} />
      ) : activeTab === 'remote' ? (
        <RemoteServersTab agents={agents} />
      ) : (
        <MarketplaceTab
          catalog={filteredCatalog}
          categories={categories}
          catalogFilter={catalogFilter}
          setCatalogFilter={setCatalogFilter}
          catalogSearch={catalogSearch}
          setCatalogSearch={setCatalogSearch}
          isInstalled={isInstalled}
          agents={agents}
          // 2026-10-30: the card used to open its own agent-only dialog, so
          // an operator who needed different arguments (e.g. Playwright
          // without `--headless`) had no field to change them. It now opens
          // the shared dialog with this item preselected.
          onOpenInstall={(item) => setAddPreset({ catalogId: item.id })}
        />
      )}

      {/* Add / edit server dialog — the only install form on this page. */}
      <AddServerDialog
        preset={addPreset}
        onClose={() => setAddPreset(null)}
        catalog={catalog}
        agents={agents}
        installedServers={installedServers}
        canEdit={isAdmin}
        onAdded={(serverName, agentId, replaced) => {
          showToast(
            'success',
            intl.formatMessage(
              { id: replaced ? 'mcp.edit.saved' : 'mcp.added' },
              { server: serverName, agent: agentLabel(agentId) },
            ),
          );
        }}
        onError={(message) => {
          showToast('error', intl.formatMessage({ id: 'mcp.installFailed' }, { message }));
        }}
      />

      {/* Import from GitHub / URL */}
      {showImportDialog && (
        <ImportFromUrlDialog
          agents={agents}
          onClose={() => setShowImportDialog(false)}
          onInstalled={(serverName, agentId) => {
            showToast('success', intl.formatMessage({ id: 'mcp.added' }, { server: serverName, agent: agentLabel(agentId) }));
            fetchAll();
          }}
        />
      )}

      {removeTarget && (
        <ConfirmDialog
          open
          title={intl.formatMessage({ id: 'mcp.remove' })}
          message={intl.formatMessage(
            { id: 'confirm.mcp.removeServer.message' },
            { server: removeTarget.serverName, agent: agentLabel(removeTarget.agentId) },
          )}
          confirmLabel={intl.formatMessage({ id: 'mcp.remove' })}
          onConfirm={() => {
            const target = removeTarget;
            setRemoveTarget(null);
            void handleRemoveServer(target.agentId, target.serverName);
          }}
          onClose={() => setRemoveTarget(null)}
        />
      )}
    </div>
  );
}

// ── Agent-config server row (spec §4 ListGrid) ─────────────────

function ServerRow({
  entry,
  canEdit,
  onEdit,
  onRemove,
}: {
  entry: McpServerEntry;
  canEdit: boolean;
  onEdit: () => void;
  onRemove: () => void;
}) {
  const intl = useIntl();
  const transport = transportOf(entry);
  const envCount = Object.keys(entry.env).length;
  return (
    <ListGridRow className="cursor-default">
      <ListGridCell className="gap-2">
        <span className="size-1.5 shrink-0 rounded-full bg-success" title={intl.formatMessage({ id: 'common.enabled' })} />
        <Plug className="size-4 shrink-0 text-muted-foreground" />
        <span className="truncate text-sm font-medium text-foreground" title={entry.name}>
          {entry.name}
        </span>
      </ListGridCell>
      <ListGridCell>
        <span className="truncate font-mono text-xs text-muted-foreground" title={`${entry.command} ${entry.args.join(' ')}`}>
          {entry.command} {entry.args.join(' ')}
        </span>
      </ListGridCell>
      <ListGridCell>
        <Badge variant="secondary">{transport}</Badge>
      </ListGridCell>
      <ListGridCell className="justify-end">
        <span className="font-mono text-xs tabular-nums text-muted-foreground">{envCount}</span>
      </ListGridCell>
      <ListGridCell className="justify-end">
        <DropdownMenu>
          <DropdownMenuTrigger
            render={
              <Button
                variant="ghost"
                size="icon-sm"
                aria-label={intl.formatMessage({ id: 'common.more' })}
                data-stop-row-nav
              />
            }
          >
            <MoreHorizontal />
          </DropdownMenuTrigger>
          <DropdownMenuContent>
            <DropdownMenuItem onClick={onEdit} disabled={!canEdit}>
              <Pencil />
              {intl.formatMessage({ id: 'common.edit' })}
            </DropdownMenuItem>
            <DropdownMenuItem variant="destructive" onClick={onRemove}>
              <Trash2 />
              {intl.formatMessage({ id: 'mcp.remove' })}
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      </ListGridCell>
    </ListGridRow>
  );
}

// ── Marketplace tab (tool catalog, slim row list) ──────────────

function MarketplaceTab({
  catalog,
  categories,
  catalogFilter,
  setCatalogFilter,
  catalogSearch,
  setCatalogSearch,
  isInstalled,
  agents,
  onOpenInstall,
}: {
  catalog: ReadonlyArray<McpCatalogItem>;
  categories: string[];
  catalogFilter: string | null;
  setCatalogFilter: (v: string | null) => void;
  catalogSearch: string;
  setCatalogSearch: (v: string) => void;
  isInstalled: (id: string) => boolean;
  agents: ReadonlyArray<AgentLite>;
  onOpenInstall: (item: McpCatalogItem) => void;
}) {
  const intl = useIntl();
  return (
    <div className="space-y-4">
      {/* Search + category filters. */}
      <div className="flex flex-wrap items-center gap-2">
        <Input
          value={catalogSearch}
          onChange={(e) => setCatalogSearch(e.target.value)}
          placeholder={intl.formatMessage({ id: 'mcp.serverName' })}
          className="w-full sm:w-64"
        />
        <div className="flex flex-wrap gap-1.5">
          <Button
            variant={catalogFilter === null ? 'brandSubtle' : 'ghost'}
            size="xs"
            onClick={() => setCatalogFilter(null)}
          >
            {intl.formatMessage({ id: 'common.all' })}
          </Button>
          {categories.map((cat) => {
            const CatIcon = getCategoryIcon(cat);
            return (
              <Button
                key={cat}
                variant={catalogFilter === cat ? 'brandSubtle' : 'ghost'}
                size="xs"
                onClick={() => setCatalogFilter(cat === catalogFilter ? null : cat)}
              >
                <CatIcon />
                {intl.formatMessage({ id: `mcp.catalog.${cat}`, defaultMessage: cat })}
              </Button>
            );
          })}
        </div>
      </div>

      {catalog.length === 0 ? (
        <Empty icon={Package} title={intl.formatMessage({ id: 'mcp.empty' })} />
      ) : (
        <div className="overflow-hidden rounded-xl border border-surface-border divide-y divide-surface-border">
          {catalog.map((item) => (
            <CatalogRow
              key={item.id}
              item={item}
              installed={isInstalled(item.id)}
              agents={agents}
              onOpenInstall={() => onOpenInstall(item)}
            />
          ))}
        </div>
      )}
    </div>
  );
}

function CatalogRow({
  item,
  installed,
  agents,
  onOpenInstall,
}: {
  item: McpCatalogItem;
  installed: boolean;
  agents: ReadonlyArray<AgentLite>;
  onOpenInstall: () => void;
}) {
  const intl = useIntl();
  const CatIcon = getCategoryIcon(item.category);
  // E3: a remote card (hosted aggregator) is connected, not installed.
  const [showConnect, setShowConnect] = useState(false);
  const isAdmin = useAuthStore((s) => s.user?.role === 'admin');

  return (
    <div className="flex items-center gap-3 px-4 py-3">
      <div className="flex size-8 shrink-0 items-center justify-center rounded-lg bg-muted">
        <CatIcon className="size-4 text-muted-foreground" />
      </div>
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-2">
          <span className="truncate text-sm font-medium text-foreground" title={item.name}>
            {item.name}
          </span>
          <Badge variant="secondary">
            {intl.formatMessage({ id: `mcp.catalog.${item.category}`, defaultMessage: item.category })}
          </Badge>
          {installed && <CheckCircle2 className="size-4 shrink-0 text-success" />}
        </div>
        <p className="truncate text-xs text-muted-foreground">{item.description}</p>
        {item.required_env.length > 0 && (
          <p className="mt-0.5 truncate text-xs text-muted-foreground/80">
            {intl.formatMessage({ id: 'mcp.catalog.requiresEnv' }, { vars: item.required_env.join(', ') })}
          </p>
        )}
        {item.remote?.third_party && (
          <p className="mt-0.5 text-xs text-muted-foreground/80">
            {intl.formatMessage({ id: 'mcp.catalog.remoteThirdParty' }, { provider: item.name })}
          </p>
        )}
      </div>
      {item.remote ? (
        <Button
          variant="outline"
          size="sm"
          onClick={() => setShowConnect(true)}
          className="shrink-0"
          disabled={!isAdmin}
          title={isAdmin ? undefined : intl.formatMessage({ id: 'mcp.remote.adminOnly' })}
        >
          <Cloud />
          <span className="hidden sm:inline">{intl.formatMessage({ id: 'mcp.remote.connect' })}</span>
        </Button>
      ) : (
        <Button variant="outline" size="sm" onClick={onOpenInstall} className="shrink-0">
          <Plus />
          <span className="hidden sm:inline">{intl.formatMessage({ id: 'mcp.catalog.install' })}</span>
        </Button>
      )}
      {item.remote && (
        <RemoteConnectDialog
          open={showConnect}
          onClose={() => setShowConnect(false)}
          agents={agents}
          initial={{
            name: item.id,
            url: item.remote.url_hint,
            auth: item.remote.auth,
            provider: item.name,
            thirdParty: item.remote.third_party,
            docsUrl: item.remote.docs_url,
          }}
          onConnected={() => void useMcpStore.getState().fetchAll()}
        />
      )}

    </div>
  );
}

// ── Import from GitHub / URL ────────────────────────────────
//
// URL → mcp.import.fetch (server-side fetch, SSRF-gated, per-server security
// scan) → admin reviews command/args/env + findings → mcp.import.install
// (re-scans fail-closed). A failed scan disables the install button. Non-admins
// file an approval request (manager → admin chain) via mcp.install_request.

const IMPORT_SEVERITY_COLORS: Record<string, string> = {
  critical: 'text-destructive',
  error: 'text-warning',
  warning: 'text-warning',
  info: 'text-muted-foreground',
};

function ImportFromUrlDialog({
  agents,
  onClose,
  onInstalled,
}: {
  agents: ReadonlyArray<AgentLite>;
  onClose: () => void;
  onInstalled: (serverName: string, agentId: string) => void;
}) {
  const intl = useIntl();
  const isAdmin = useAuthStore((s) => s.user?.role === 'admin');
  const [url, setUrl] = useState('');
  const [fetching, setFetching] = useState(false);
  const [candidates, setCandidates] = useState<McpImportCandidate[] | null>(null);
  const [sourceUrl, setSourceUrl] = useState('');
  const [targetAgent, setTargetAgent] = useState('');
  const [addToCatalog, setAddToCatalog] = useState(true);
  const [installing, setInstalling] = useState<string | null>(null);
  const [installedNames, setInstalledNames] = useState<string[]>([]);
  const [requestedNames, setRequestedNames] = useState<string[]>([]);
  const [error, setError] = useState<string | null>(null);

  const trimmed = url.trim();
  const validUrl = /^https?:\/\/\S+$/i.test(trimmed);

  const handleFetch = async () => {
    if (!validUrl) return;
    setFetching(true);
    setError(null);
    setCandidates(null);
    setInstalledNames([]);
    try {
      const res = await api.mcp.importFetch(trimmed);
      setCandidates(res.servers);
      setSourceUrl(res.source_url);
    } catch (e) {
      setError(String(e));
    } finally {
      setFetching(false);
    }
  };

  const handleInstall = async (candidate: McpImportCandidate) => {
    if (!targetAgent || !candidate.passed) return;
    setInstalling(candidate.name);
    setError(null);
    try {
      const req = {
        agent_id: targetAgent,
        server_name: candidate.name,
        server_def: { command: candidate.command, args: candidate.args, env: candidate.env },
        add_to_catalog: addToCatalog,
        description: candidate.description || undefined,
        source_url: sourceUrl || undefined,
      };
      if (isAdmin) {
        await api.mcp.importInstall(req);
        setInstalledNames((prev) => [...prev, candidate.name]);
        onInstalled(candidate.name, targetAgent);
      } else {
        // Non-admin: file an approval request (manager → admin chain).
        await api.mcp.installRequest(req);
        setRequestedNames((prev) => [...prev, candidate.name]);
      }
    } catch (e) {
      setError(String(e));
    } finally {
      setInstalling(null);
    }
  };

  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{intl.formatMessage({ id: 'mcp.import.title' })}</DialogTitle>
          <DialogDescription>{intl.formatMessage({ id: 'mcp.import.desc' })}</DialogDescription>
        </DialogHeader>

        <div className="space-y-4">
          {/* URL + fetch */}
          <div className="space-y-1.5">
            <label className="text-xs font-medium text-muted-foreground">URL</label>
            <div className="flex gap-2">
              <Input
                type="url"
                value={url}
                onChange={(e) => setUrl(e.target.value)}
                onKeyDown={(e) => { if (e.key === 'Enter' && !isImeComposing(e)) handleFetch(); }}
                placeholder="https://github.com/user/mcp-server-repo"
                autoFocus
              />
              <Button variant="outline" onClick={handleFetch} disabled={fetching || !validUrl} className="shrink-0">
                {fetching
                  ? intl.formatMessage({ id: 'mcp.import.fetching' })
                  : intl.formatMessage({ id: 'mcp.import.fetch' })}
              </Button>
            </div>
            <p className="text-xs text-muted-foreground">{intl.formatMessage({ id: 'mcp.import.hint' })}</p>
          </div>

          {/* Candidates */}
          {candidates && (
            <>
              <div className="space-y-1.5">
                <label className="text-xs font-medium text-muted-foreground">
                  {intl.formatMessage({ id: 'mcp.targetAgent' })}
                </label>
                <AgentSelect
                  value={targetAgent}
                  onChange={setTargetAgent}
                  agents={agents}
                  placeholder={intl.formatMessage({ id: 'mcp.targetAgent' })}
                />
              </div>

              <label className="flex items-center gap-2 text-sm text-muted-foreground">
                <Switch checked={addToCatalog} onCheckedChange={setAddToCatalog} />
                {intl.formatMessage({ id: 'mcp.import.addToCatalog' })}
              </label>

              {!isAdmin && (
                <p className="text-xs text-muted-foreground">
                  {intl.formatMessage({ id: 'install.request.nonAdminNotice' })}
                </p>
              )}

              <div className="max-h-80 space-y-3 overflow-y-auto pr-1">
                {candidates.map((c) => {
                  const isInstalled = installedNames.includes(c.name);
                  const isRequested = requestedNames.includes(c.name);
                  return (
                    <div key={c.name} className="rounded-lg border border-surface-border p-3">
                      <div className="flex items-start justify-between gap-2">
                        <div className="min-w-0">
                          <div className="flex items-center gap-2">
                            <h4 className="truncate text-sm font-medium text-foreground">{c.name}</h4>
                            {c.passed ? (
                              <span className="flex items-center gap-1 text-xs font-medium text-success">
                                <ShieldCheck className="size-3.5" />
                                {intl.formatMessage({ id: 'mcp.import.scanPassed' })}
                              </span>
                            ) : (
                              <span className="flex items-center gap-1 text-xs font-medium text-destructive">
                                <ShieldAlert className="size-3.5" />
                                {intl.formatMessage({ id: 'mcp.import.scanFailed' })}
                              </span>
                            )}
                          </div>
                          {c.description && (
                            <p className="mt-0.5 text-xs text-muted-foreground">{c.description}</p>
                          )}
                          <p className="mt-1 break-all font-mono text-xs text-muted-foreground">
                            {c.command} {c.args.join(' ')}
                          </p>
                          {Object.keys(c.env).length > 0 && (
                            <div className="mt-1 flex flex-wrap gap-1">
                              {Object.keys(c.env).map((key) => (
                                <span key={key} className="inline-flex items-center rounded bg-muted px-1.5 py-0.5 font-mono text-xs text-muted-foreground">
                                  {key}=***
                                </span>
                              ))}
                            </div>
                          )}
                        </div>
                        <Button
                          variant="brand"
                          size="sm"
                          onClick={() => handleInstall(c)}
                          disabled={!c.passed || !targetAgent || installing === c.name || isInstalled || isRequested}
                          className="shrink-0"
                          title={!c.passed
                            ? intl.formatMessage({ id: 'mcp.import.blockedHint' })
                            : !targetAgent
                              ? intl.formatMessage({ id: 'mcp.targetAgent' })
                              : undefined}
                        >
                          {isInstalled ? (
                            <><CheckCircle2 />{intl.formatMessage({ id: 'mcp.import.installed' })}</>
                          ) : isRequested ? (
                            <><CheckCircle2 />{intl.formatMessage({ id: 'install.request.filedShort' })}</>
                          ) : installing === c.name ? (
                            <Loader2 className="animate-spin" />
                          ) : (
                            <><Download />{intl.formatMessage({ id: isAdmin ? 'mcp.catalog.install' : 'install.request.submit' })}</>
                          )}
                        </Button>
                      </div>

                      {/* Findings */}
                      {c.scan.findings.length > 0 && (
                        <ul className="mt-2 space-y-1 border-t border-surface-border pt-2">
                          {c.scan.findings.map((f, i) => (
                            <li key={i} className="flex items-start gap-1.5 text-xs">
                              <AlertTriangle className={cn('mt-0.5 size-3 shrink-0', IMPORT_SEVERITY_COLORS[f.severity] ?? IMPORT_SEVERITY_COLORS.info)} />
                              <span className="text-muted-foreground">
                                <span className={cn('font-medium uppercase', IMPORT_SEVERITY_COLORS[f.severity] ?? IMPORT_SEVERITY_COLORS.info)}>{f.severity}</span>
                                {' · '}{f.description}
                              </span>
                            </li>
                          ))}
                        </ul>
                      )}
                    </div>
                  );
                })}
              </div>
            </>
          )}

          {error && (
            <div className="flex items-start gap-2 rounded-lg bg-destructive/10 px-3 py-2 text-xs text-destructive">
              <AlertTriangle className="mt-0.5 size-3 shrink-0" />
              <span className="break-all">{error}</span>
            </div>
          )}
        </div>

        <DialogFooter>
          <DialogClose render={<Button variant="outline">{intl.formatMessage({ id: 'mcp.cancel' })}</Button>} />
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

// ── OAuth tab ──────────────────────────────────────────────

function statusBadge(
  provider: McpOAuthProvider,
  intl: ReturnType<typeof useIntl>,
): { label: string; variant: 'secondary' | 'destructive'; className?: string } {
  if (!provider.configured) {
    return { label: intl.formatMessage({ id: 'mcp.oauth.notConfigured' }), variant: 'secondary' };
  }
  // Older gateways sent only `status`; both are accepted so a connected
  // provider never renders as unauthenticated because of a key-name mismatch.
  switch (provider.token_status ?? provider.status) {
    case 'authenticated':
      return { label: intl.formatMessage({ id: 'mcp.oauth.authenticated' }), variant: 'secondary', className: 'bg-success/15 text-success' };
    case 'expired':
      return { label: intl.formatMessage({ id: 'mcp.oauth.expired' }), variant: 'destructive' };
    default:
      return { label: intl.formatMessage({ id: 'mcp.oauth.notAuthenticated' }), variant: 'secondary', className: 'bg-warning/15 text-warning' };
  }
}

function OAuthTab({
  providers,
  showToast,
}: {
  providers: ReadonlyArray<McpOAuthProvider>;
  showToast: (type: 'success' | 'error', message: string) => void;
}) {
  const intl = useIntl();
  const [pendingProvider, setPendingProvider] = useState<string | null>(null);
  /** Consent URL of the in-flight authorization — kept on the card as a
      select-all manual fallback in case no browser window appeared. */
  const [pendingAuthUrl, setPendingAuthUrl] = useState<string | null>(null);
  const [configureProvider, setConfigureProvider] = useState<McpOAuthProvider | null>(null);
  // Revoke used to gate on the browser's native `window.confirm()` (phase4
  // audit C06 Blocker) — route it through the shared ConfirmDialog.
  const [revokeTarget, setRevokeTarget] = useState<McpOAuthProvider | null>(null);
  const pollTimerRef = useRef<ReturnType<typeof setInterval> | null>(null);

  useEffect(() => {
    return () => {
      if (pollTimerRef.current) clearInterval(pollTimerRef.current);
    };
  }, []);

  /**
   * Track an in-flight authorization: open the consent page (desktop-shell
   * aware — `window.open` is a no-op in the wry webview), surface the URL as
   * a manual fallback, and poll until the token lands. Shared by the card's
   * authenticate button and the credentials dialog so both get the same
   * pending UI + polling.
   */
  const beginAuthorization = (provider: McpOAuthProvider, authUrl: string) => {
    setPendingProvider(provider.provider_id);
    setPendingAuthUrl(authUrl);
    openExternal(authUrl);

    // Start local polling for UI feedback
    let attempts = 0;
    if (pollTimerRef.current) clearInterval(pollTimerRef.current);
    pollTimerRef.current = setInterval(async () => {
      attempts += 1;
      if (attempts > 100) {
        if (pollTimerRef.current) clearInterval(pollTimerRef.current);
        setPendingProvider(null);
        setPendingAuthUrl(null);
        return;
      }
      const updatedProviders = useMcpStore.getState().oauthProviders;
      const updated = updatedProviders.find((p) => p.provider_id === provider.provider_id);
      if (updated?.token_status === 'authenticated') {
        if (pollTimerRef.current) clearInterval(pollTimerRef.current);
        setPendingProvider(null);
        setPendingAuthUrl(null);
        showToast('success', intl.formatMessage({ id: 'mcp.oauth.success' }, { provider: provider.name }));
      }
    }, 3000);
  };

  const handleAuthenticate = async (provider: McpOAuthProvider) => {
    setPendingProvider(provider.provider_id);
    try {
      const authUrl = await useMcpStore.getState().startOAuth(provider.provider_id);
      beginAuthorization(provider, authUrl);
    } catch {
      setPendingProvider(null);
      showToast('error', intl.formatMessage({ id: 'mcp.loadFailed' }));
    }
  };

  const handleRevoke = async (provider: McpOAuthProvider) => {
    try {
      await useMcpStore.getState().revokeOAuth(provider.provider_id);
      showToast('success', intl.formatMessage({ id: 'mcp.oauth.revoked' }, { provider: provider.name }));
    } catch {
      showToast('error', intl.formatMessage({ id: 'mcp.loadFailed' }));
    }
  };

  // Rendered inline under the server list since 2026-08-04 (D19). With nothing
  // to connect there is nothing to say — an empty-state card here would just be
  // noise between two real sections.
  if (providers.length === 0) return null;

  return (
    <div className="space-y-4 pt-2">
      <div>
        <h2 className="text-sm font-medium text-foreground">{intl.formatMessage({ id: 'mcp.oauth.title' })}</h2>
        <p className="text-xs text-muted-foreground">{intl.formatMessage({ id: 'mcp.oauth.sectionDesc' })}</p>
      </div>
      {(
        <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
          {providers.map((provider) => {
            const status = statusBadge(provider, intl);
            const isPending = pendingProvider === provider.provider_id;
            return (
              <Card key={provider.provider_id}>
                <CardContent className="space-y-3">
                  <div className="flex items-center gap-3">
                    <div className="flex size-9 shrink-0 items-center justify-center rounded-lg bg-muted">
                      <Globe className="size-5 text-muted-foreground" />
                    </div>
                    <div className="min-w-0">
                      <h3 className="truncate text-base font-medium text-foreground">
                        {provider.name || provider.provider_id}
                      </h3>
                      <Badge variant={status.variant} className={status.className}>{status.label}</Badge>
                    </div>
                  </div>

                  {/* What is stored. Without this the card is silent about the
                      credentials it holds, and a saved integration is
                      indistinguishable from an empty one. */}
                  {provider.configured && provider.client_id && (
                    <dl className="space-y-1 rounded-md bg-muted/50 px-2.5 py-2 text-xs">
                      <div className="flex flex-wrap items-baseline gap-x-2">
                        <dt className="text-muted-foreground">
                          {intl.formatMessage({ id: 'mcp.oauth.clientId' })}
                        </dt>
                        <dd className="min-w-0 break-all font-mono text-foreground">
                          {provider.client_id}
                        </dd>
                      </div>
                      <div className="flex flex-wrap items-baseline gap-x-2">
                        <dt className="text-muted-foreground">
                          {intl.formatMessage({ id: 'mcp.oauth.clientSecret' })}
                        </dt>
                        <dd className="min-w-0 text-foreground">
                          {provider.has_client_secret
                            ? intl.formatMessage(
                                { id: 'mcp.oauth.secretStored' },
                                { mask: provider.client_secret_masked || '••••' },
                              )
                            : intl.formatMessage({ id: 'mcp.oauth.secretNone' })}
                        </dd>
                      </div>
                    </dl>
                  )}

                  {/* Scopes */}
                  {provider.scopes.length > 0 && (
                    <div>
                      <p className="text-xs font-medium text-muted-foreground">
                        {intl.formatMessage({ id: 'mcp.oauth.scopes' })}
                      </p>
                      <div className="mt-1 flex flex-wrap gap-1">
                        {provider.scopes.map((scope) => (
                          <span key={scope} className="inline-flex items-center rounded bg-muted px-1.5 py-0.5 font-mono text-xs text-muted-foreground">
                            {scope}
                          </span>
                        ))}
                      </div>
                    </div>
                  )}

                  {/* Expires at */}
                  {provider.token_status === 'authenticated' && provider.expires_at && (
                    <p className="text-xs text-muted-foreground">
                      {intl.formatMessage({ id: 'mcp.oauth.expiresAt' }, { date: new Date(provider.expires_at).toLocaleDateString() })}
                    </p>
                  )}

                  {/* Pending state */}
                  {isPending && (
                    <div className="flex items-center gap-2 text-sm text-brand">
                      <Loader2 className="size-4 animate-spin" />
                      {intl.formatMessage({ id: 'mcp.oauth.waiting' })}
                    </div>
                  )}
                  {/* Manual fallback while waiting — completes the flow in any
                      browser if no consent window appeared. */}
                  {isPending && pendingAuthUrl && (
                    <div className="space-y-1 rounded-md bg-muted/50 px-2.5 py-2">
                      <p className="text-xs text-muted-foreground">
                        {intl.formatMessage({ id: 'mcp.oauth.manualUrlHint' })}
                      </p>
                      <p className="select-all break-all font-mono text-[11px] text-foreground">
                        {pendingAuthUrl}
                      </p>
                    </div>
                  )}
                </CardContent>

                {/* Actions */}
                <div className="flex flex-wrap gap-2 border-t border-surface-border px-4 pt-3">
                  {/* Always offered: once configured, the button used to vanish,
                      so a wrong client id could only be corrected by revoking. */}
                  <Button
                    variant={provider.configured ? 'outline' : 'brand'}
                    size="sm"
                    onClick={() => setConfigureProvider(provider)}
                  >
                    <KeyRound />
                    {intl.formatMessage({
                      id: provider.configured ? 'mcp.oauth.editCredentials' : 'mcp.oauth.configure',
                    })}
                  </Button>
                  {provider.configured && provider.token_status !== 'authenticated' && !isPending && (
                    <Button variant="brand" size="sm" onClick={() => handleAuthenticate(provider)}>
                      <Shield />
                      {intl.formatMessage({ id: 'mcp.oauth.authenticate' })}
                    </Button>
                  )}
                  {provider.token_status === 'authenticated' && (
                    <Button variant="destructive" size="sm" onClick={() => setRevokeTarget(provider)}>
                      <Trash2 />
                      {intl.formatMessage({ id: 'mcp.oauth.revoke' })}
                    </Button>
                  )}
                </div>
              </Card>
            );
          })}
        </div>
      )}

      {/* Configure OAuth Credentials Dialog */}
      {configureProvider && (
        <ConfigureOAuthDialog
          provider={configureProvider}
          onClose={() => setConfigureProvider(null)}
          showToast={showToast}
          onAuthStarted={(authUrl) => beginAuthorization(configureProvider, authUrl)}
        />
      )}

      {revokeTarget && (
        <ConfirmDialog
          open
          title={intl.formatMessage({ id: 'mcp.oauth.revoke' })}
          message={intl.formatMessage(
            { id: 'confirm.mcp.revokeOAuth.message' },
            { provider: revokeTarget.name || revokeTarget.provider_id },
          )}
          confirmLabel={intl.formatMessage({ id: 'mcp.oauth.revoke' })}
          onConfirm={() => {
            const target = revokeTarget;
            setRevokeTarget(null);
            void handleRevoke(target);
          }}
          onClose={() => setRevokeTarget(null)}
        />
      )}
    </div>
  );
}

function ConfigureOAuthDialog({
  provider,
  onClose,
  showToast,
  onAuthStarted,
}: {
  provider: McpOAuthProvider;
  onClose: () => void;
  showToast: (type: 'success' | 'error', message: string) => void;
  /** The consent flow started — the parent opens the URL + owns the pending
      UI/polling (the dialog closes right away, so it can't host either). */
  onAuthStarted: (authUrl: string) => void;
}) {
  const intl = useIntl();
  const [clientId, setClientId] = useState(provider.client_id ?? '');
  const [clientSecret, setClientSecret] = useState('');
  const [submitting, setSubmitting] = useState(false);

  const helpKey = `mcp.oauth.help${provider.provider_id.charAt(0).toUpperCase()}${provider.provider_id.slice(1)}` as const;

  const handleSubmit = async () => {
    if (!clientId.trim()) return;
    setSubmitting(true);
    try {
      const authUrl = await useMcpStore.getState().startOAuth(provider.provider_id, clientId.trim(), clientSecret.trim() || undefined);
      onAuthStarted(authUrl);
      showToast('success', intl.formatMessage({ id: 'mcp.oauth.waiting' }));
      onClose();
    } catch {
      showToast('error', intl.formatMessage({ id: 'mcp.loadFailed' }));
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>{intl.formatMessage({ id: 'mcp.oauth.configureTitle' })}</DialogTitle>
        </DialogHeader>
        <div className="space-y-4">
          <div className="space-y-1.5">
            <label className="text-xs font-medium text-muted-foreground">Provider</label>
            <Input
              type="text"
              value={provider.name ?? provider.provider_id}
              readOnly
              className="bg-muted/50"
            />
          </div>

          {/* Show only the callback address reported by the gateway. */}
          <div className="space-y-1.5">
            <label className="text-xs font-medium text-muted-foreground">
              {intl.formatMessage({ id: 'mcp.oauth.redirectUri' })}
            </label>
            {provider.redirect_uri ? (
              <Input
                type="text"
                value={provider.redirect_uri}
                readOnly
                onFocus={(e) => e.currentTarget.select()}
                className="bg-muted/50 font-mono text-xs"
              />
            ) : (
              <p className="text-sm text-muted-foreground">
                {intl.formatMessage({ id: 'google.wizard.redirectUnavailable' })}
              </p>
            )}
          </div>

          <div className="space-y-1.5">
            <label className="text-xs font-medium text-muted-foreground">
              {intl.formatMessage({ id: 'mcp.oauth.clientId' })}
            </label>
            <Input
              type="text"
              value={clientId}
              onChange={(e) => setClientId(e.target.value)}
              placeholder="Client ID"
            />
          </div>

          <div className="space-y-1.5">
            <label className="text-xs font-medium text-muted-foreground">
              {intl.formatMessage({ id: 'mcp.oauth.clientSecret' })}
            </label>
            {/* Blank keeps the stored secret — the gateway falls back to the
                saved client config when none is supplied. */}
            <Input
              type="password"
              value={clientSecret}
              onChange={(e) => setClientSecret(e.target.value)}
              placeholder={
                provider.has_client_secret
                  ? intl.formatMessage(
                      { id: 'mcp.oauth.secretKeep' },
                      { mask: provider.client_secret_masked || '••••' },
                    )
                  : 'Client Secret'
              }
            />
          </div>

          <p className="text-xs text-muted-foreground">
            {intl.formatMessage({ id: helpKey, defaultMessage: '' })}
          </p>
        </div>

        <DialogFooter>
          <DialogClose render={<Button variant="outline">{intl.formatMessage({ id: 'mcp.cancel' })}</Button>} />
          <Button variant="brand" onClick={handleSubmit} disabled={submitting || !clientId.trim()}>
            {submitting ? intl.formatMessage({ id: 'mcp.adding' }) : intl.formatMessage({ id: 'mcp.oauth.authenticate' })}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

// ── Add / edit server dialog (catalog / custom) ────────────────

/**
 * Prefill for an already-installed server. `mcp.list` answers env values as
 * status words: `set` is sent back verbatim so `mcp.update` keeps the stored
 * secret; `reference` / `not_set` cannot be carried over, so the line is left
 * empty and the name is reported for re-entry. A required name that is not
 * `set` gets no line at all: the required-value field asks for it instead.
 */
function prefillFromInstalled(
  entry: McpServerEntry,
  required: ReadonlyArray<string>,
): {
  command: string;
  args: string;
  envText: string;
  reenter: string[];
} {
  const reenter: string[] = [];
  const lines: string[] = [];
  for (const [name, status] of Object.entries(entry.env)) {
    if (status === 'set') {
      lines.push(`${name}=set`);
    } else if (!required.includes(name)) {
      if (status === 'reference') reenter.push(name);
      lines.push(`${name}=`);
    }
  }
  return { command: entry.command, args: entry.args.join(' '), envText: lines.join('\n'), reenter };
}

function parseEnvText(envText: string): Record<string, string> {
  const parsed: Record<string, string> = {};
  for (const line of envText.split('\n')) {
    const t = line.trim();
    if (!t) continue;
    const eqIdx = t.indexOf('=');
    if (eqIdx > 0) {
      parsed[t.slice(0, eqIdx)] = t.slice(eqIdx + 1);
    }
  }
  return parsed;
}

function AddServerDialog({
  preset,
  onClose,
  catalog,
  agents,
  installedServers,
  canEdit,
  onAdded,
  onError,
}: {
  /** `null` = closed. */
  preset: AddDialogPreset | null;
  onClose: () => void;
  catalog: ReadonlyArray<McpCatalogItem>;
  agents: ReadonlyArray<AgentLite>;
  installedServers: (agentId: string) => ReadonlyArray<McpServerEntry>;
  canEdit: boolean;
  onAdded: (serverName: string, agentId: string, replaced: boolean) => void;
  onError: (message: string) => void;
}) {
  const intl = useIntl();
  const open = preset !== null;
  const locked = preset?.lock === true;
  const [mode, setMode] = useState<'catalog' | 'custom'>('catalog');
  const [selectedCatalogId, setSelectedCatalogId] = useState('');
  const [targetAgent, setTargetAgent] = useState('');
  const [serverName, setServerName] = useState('');
  const [command, setCommand] = useState('');
  const [args, setArgs] = useState('');
  const [envText, setEnvText] = useState('');
  // v1.68: typed values for the catalogue item's required env names.
  const [requiredValues, setRequiredValues] = useState<Record<string, string>>({});
  // Env names of the installed server whose value cannot be carried over.
  const [reenter, setReenter] = useState<string[]>([]);
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // Which source last filled command/args/env, so a refreshed `catalog`
  // array (new identity after fetchAll) never overwrites the operator's edits.
  const prefilledKey = useRef('');
  // Set once the operator edits command / args / env: from then on nothing
  // refills those fields behind their back (picking another catalogue item
  // clears it — that is an explicit request for that item's defaults).
  const touched = useRef(false);

  const resetForm = () => {
    setMode('catalog');
    setSelectedCatalogId('');
    setTargetAgent('');
    setServerName('');
    setCommand('');
    setArgs('');
    setEnvText('');
    setRequiredValues({});
    setReenter([]);
    setError(null);
    prefilledKey.current = '';
    touched.current = false;
  };

  // Apply the preset each time the dialog opens.
  useEffect(() => {
    if (!preset) return;
    resetForm();
    const catalogId =
      preset.catalogId ??
      (preset.serverName && catalog.some((c) => c.id === preset.serverName) ? preset.serverName : undefined);
    if (catalogId) {
      setMode('catalog');
      setSelectedCatalogId(catalogId);
    } else if (preset.serverName) {
      setMode('custom');
    }
    if (preset.serverName) setServerName(preset.serverName);
    if (preset.agentId) setTargetAgent(preset.agentId);
    // eslint-disable-next-line react-hooks/exhaustive-deps -- only on open
  }, [preset]);

  // Catalogue item → its server name.
  useEffect(() => {
    if (mode !== 'catalog' || !selectedCatalogId) return;
    const item = catalog.find((c) => c.id === selectedCatalogId);
    if (item) setServerName(item.id);
  }, [mode, selectedCatalogId, catalog]);

  const trimmedName = serverName.trim();
  const existing = targetAgent && trimmedName
    ? installedServers(targetAgent).find((s) => s.name === trimmedName)
    : undefined;
  const selectedCatalogItem =
    mode === 'catalog' ? catalog.find((c) => c.id === selectedCatalogId) : undefined;

  // Fill command / args / env: from the installed definition when this agent
  // already has a server of this name (so 送出 edits it in place), otherwise
  // from the catalogue defaults. A name typed in custom mode never pulls the
  // installed definition in (it would overwrite what is being typed); only a
  // catalogue pick or the row's 編輯 does.
  const prefillFromExisting = existing !== undefined && (mode === 'catalog' || !!preset?.serverName);
  useEffect(() => {
    if (touched.current) return;
    let key = '';
    if (existing && prefillFromExisting) key = `installed:${targetAgent}/${existing.name}`;
    else if (selectedCatalogItem) key = `catalog:${selectedCatalogItem.id}`;
    if (!key || key === prefilledKey.current) return;
    prefilledKey.current = key;
    if (existing && prefillFromExisting) {
      const fill = prefillFromInstalled(existing, selectedCatalogItem?.required_env ?? []);
      setCommand(fill.command);
      setArgs(fill.args);
      setEnvText(fill.envText);
      setReenter(fill.reenter);
    } else if (selectedCatalogItem) {
      setCommand(selectedCatalogItem.default_def.command);
      setArgs(selectedCatalogItem.default_def.args.join(' '));
      // v1.68: the catalogue env comes back masked (set / not_set /
      // reference). Prefill only real values; required names are typed
      // separately below.
      setEnvText(
        Object.entries(unmaskedEnv(selectedCatalogItem.default_def.env))
          .filter(([k]) => !selectedCatalogItem.required_env.includes(k))
          .map(([k, v]) => `${k}=${v}`)
          .join('\n'),
      );
      setReenter([]);
    }
    setRequiredValues({});
  }, [existing, prefillFromExisting, selectedCatalogItem, targetAgent]);

  const parsedNow = parseEnvText(envText);
  // Required names still to be typed: an installed server keeps the ones it
  // already holds, as long as their `NAME=set` line is still in the form.
  const requiredToType = (selectedCatalogItem?.required_env ?? []).filter(
    (name) => !(existing?.env[name] === 'set' && parsedNow[name]?.trim() === 'set'),
  );
  // Stored values that saving would remove (their line was deleted or never
  // filled); `mcp.update` replaces the whole definition.
  const dropped = existing
    ? Object.keys(existing.env).filter(
        (name) =>
          existing.env[name] !== 'not_set' &&
          !parsedNow[name]?.trim() &&
          !requiredValues[name]?.trim() &&
          // An emptied re-entry line is refused at save; a deleted one is dropped.
          !(reenter.includes(name) && name in parsedNow) &&
          !requiredToType.includes(name),
      )
    : [];

  const close = () => {
    onClose();
    resetForm();
  };

  const handleSubmit = async () => {
    if (!targetAgent || !trimmedName || !command.trim()) return;
    setError(null);
    const parsedEnv = parseEnvText(envText);
    const missing = missingRequiredEnv(requiredToType, requiredValues);
    if (missing.length > 0) {
      setError(intl.formatMessage({ id: 'mcp.requiredEnv.missing' }, { vars: missing.join(', ') }));
      return;
    }
    const stillEmpty = reenter.filter(
      (name) => name in parsedEnv && !parsedEnv[name].trim() && !requiredValues[name]?.trim(),
    );
    if (stillEmpty.length > 0) {
      setError(intl.formatMessage({ id: 'mcp.edit.reenterMissing' }, { vars: stillEmpty.join(', ') }));
      return;
    }
    setSubmitting(true);
    try {
      const argList = args.trim() ? args.trim().split(/\s+/) : [];
      const item = selectedCatalogItem;
      const untouched =
        !existing &&
        item !== undefined &&
        trimmedName === item.id &&
        command.trim() === item.default_def.command &&
        argList.join(' ') === item.default_def.args.join(' ') &&
        Object.keys(parsedEnv).length ===
          Object.keys(unmaskedEnv(item.default_def.env)).filter((k) => !item.required_env.includes(k)).length;
      if (item && untouched) {
        // Unchanged catalogue item: let the gateway use its own definition.
        await api.marketplace.install(item.id, targetAgent, requiredEnvPayload(item.required_env, requiredValues));
        await useMcpStore.getState().fetchAll();
      } else {
        // `mcp.update` add replaces a same-name server, so an installed
        // server is edited in place.
        const def: McpServerDef = {
          command: command.trim(),
          args: argList,
          env: { ...parsedEnv, ...requiredEnvPayload(requiredToType, requiredValues) },
        };
        await useMcpStore.getState().addServer(targetAgent, trimmedName, def);
      }
      onAdded(trimmedName, targetAgent, existing !== undefined);
      close();
    } catch (e) {
      const message = e instanceof Error ? e.message : String(e);
      setError(intl.formatMessage({ id: 'mcp.installFailed' }, { message }));
      onError(message);
    } finally {
      setSubmitting(false);
    }
  };

  const catalogModeOptions: SegmentedOption<'catalog' | 'custom'>[] = [
    { value: 'catalog', label: intl.formatMessage({ id: 'mcp.fromCatalog' }) },
    { value: 'custom', label: intl.formatMessage({ id: 'mcp.custom' }) },
  ];

  return (
    <Dialog open={open} onOpenChange={(o) => { if (!o) close(); }}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>
            {intl.formatMessage({ id: existing ? 'mcp.edit.title' : 'mcp.addTitle' })}
          </DialogTitle>
        </DialogHeader>

        <div className="space-y-4">
          {/* Mode toggle — not offered when editing an installed row. */}
          {!locked && (
            <Segmented value={mode} onValueChange={setMode} options={catalogModeOptions} className="w-full" />
          )}

          {/* Catalog selector */}
          {mode === 'catalog' && !locked && (
            <div className="space-y-1.5">
              <label className="text-xs font-medium text-muted-foreground">
                {intl.formatMessage({ id: 'mcp.fromCatalog' })}
              </label>
              <Select
                value={selectedCatalogId}
                onValueChange={(v) => {
                  touched.current = false;
                  setSelectedCatalogId(String(v));
                }}
              >
                <SelectTrigger className="w-full" aria-label={intl.formatMessage({ id: 'mcp.fromCatalog' })}>
                  <SelectValue placeholder="--">{selectedCatalogItem?.name}</SelectValue>
                </SelectTrigger>
                <SelectContent>
                  {catalog.map((item) => (
                    <SelectItem key={item.id} value={item.id}>{item.name}</SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
          )}

          {/* Target agent */}
          <div className="space-y-1.5">
            <label className="text-xs font-medium text-muted-foreground">
              {intl.formatMessage({ id: 'mcp.targetAgent' })}
            </label>
            <AgentSelect
              value={targetAgent}
              onChange={setTargetAgent}
              agents={agents}
              placeholder={intl.formatMessage({ id: 'mcp.targetAgent' })}
              disabled={locked}
            />
          </div>

          {/* Server name */}
          <div className="space-y-1.5">
            <label className="text-xs font-medium text-muted-foreground">
              {intl.formatMessage({ id: 'mcp.serverName' })}
            </label>
            <Input
              type="text"
              value={serverName}
              onChange={(e) => setServerName(e.target.value)}
              placeholder="e.g. filesystem"
              readOnly={locked || (mode === 'catalog' && !!selectedCatalogId)}
            />
          </div>

          <AutonomyNote id="mcpInstall" />

          {/* Command / Args / Env — spawning an MCP server runs a real process on
              the host, so these live inside a DangerZone. */}
          <DangerZone
            title={intl.formatMessage({ id: 'mcp.custom.dangerTitle' })}
            description={intl.formatMessage({ id: 'mcp.custom.dangerDesc' })}
          >
            <div className="space-y-1.5">
              <label htmlFor="mcp-add-command" className="text-xs font-medium text-muted-foreground">
                {intl.formatMessage({ id: 'mcp.command' })}
              </label>
              <Input
                id="mcp-add-command"
                type="text"
                value={command}
                onChange={(e) => {
                  touched.current = true;
                  setCommand(e.target.value);
                }}
                placeholder="e.g. npx"
                disabled={!canEdit}
              />
            </div>

            <div className="space-y-1.5">
              <label htmlFor="mcp-add-args" className="text-xs font-medium text-muted-foreground">
                {intl.formatMessage({ id: 'mcp.args' })}
              </label>
              <Input
                id="mcp-add-args"
                type="text"
                value={args}
                onChange={(e) => {
                  touched.current = true;
                  setArgs(e.target.value);
                }}
                placeholder="e.g. -y @modelcontextprotocol/server-filesystem /path"
                disabled={!canEdit}
              />
            </div>

            <div className="space-y-1.5">
              <label htmlFor="mcp-add-env" className="text-xs font-medium text-muted-foreground">
                {intl.formatMessage({ id: 'mcp.env' })}
              </label>
              <Textarea
                id="mcp-add-env"
                value={envText}
                onChange={(e) => {
                  touched.current = true;
                  setEnvText(e.target.value);
                }}
                placeholder={'API_KEY=your-key\nANOTHER_VAR=value'}
                rows={3}
                className="resize-none font-mono text-xs"
                disabled={!canEdit}
              />
              <p className="text-xs text-muted-foreground">{intl.formatMessage({ id: 'mcp.env.help' })}</p>
              {reenter.length > 0 && (
                <p className="text-xs text-warning">
                  {intl.formatMessage({ id: 'mcp.edit.reenter' }, { vars: reenter.join(', ') })}
                </p>
              )}
              {dropped.length > 0 && (
                <p className="text-xs text-warning">
                  {intl.formatMessage({ id: 'mcp.edit.dropped' }, { vars: dropped.join(', ') })}
                </p>
              )}
            </div>
          </DangerZone>

          {selectedCatalogItem && requiredToType.length > 0 && (
            <RequiredEnvFields
              required={requiredToType}
              values={requiredValues}
              onChange={setRequiredValues}
            />
          )}

          {existing && (
            <p role="status" className="rounded-lg bg-muted px-3 py-2 text-xs text-muted-foreground">
              {intl.formatMessage({ id: 'mcp.edit.replaceNotice' }, { server: existing.name })}
            </p>
          )}

          {error && (
            <div className="flex items-start gap-2 rounded-lg bg-destructive/10 px-3 py-2 text-xs text-destructive">
              <AlertTriangle className="mt-0.5 size-3 shrink-0" />
              <span>{error}</span>
            </div>
          )}
        </div>

        <DialogFooter>
          <DialogClose render={<Button variant="outline">{intl.formatMessage({ id: 'mcp.cancel' })}</Button>} />
          <Button
            variant="brand"
            onClick={handleSubmit}
            disabled={submitting || !canEdit || !targetAgent || !trimmedName || !command.trim()}
          >
            {submitting
              ? intl.formatMessage({ id: 'mcp.adding' })
              : intl.formatMessage({ id: existing ? 'mcp.edit.save' : 'mcp.add' })}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
