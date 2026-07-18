import { useEffect, useState, useCallback, useRef } from 'react';
import { useIntl } from 'react-intl';
import { cn } from '@/lib/utils';
import { useMcpStore } from '@/stores/mcp-store';
import { useAgentsStore } from '@/stores/agents-store';
import { useAuthStore } from '@/stores/auth-store';
import { useConnectionStore } from '@/stores/connection-store';
import { toast } from '@/lib/toast';
import { api, type McpServerDef, type McpCatalogItem, type McpOAuthProvider, type McpImportCandidate, type McpServerEntry } from '@/lib/api';
import { DangerZone } from '@/components/settings/controls';
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
} from 'lucide-react';

type Tab = 'agents' | 'marketplace' | 'oauth';
type AgentLite = { name: string; display_name: string };

const categoryIcons: Record<string, typeof Globe> = {
  browser: Globe,
  data: Database,
  communication: MessageSquare,
  google: Globe,
};

function getCategoryIcon(category: string) {
  return categoryIcons[category] ?? Package;
}

/** Derive a transport label from a stdio server def (spec §4 transport Badge). */
function transportOf(def: McpServerDef): 'stdio' | 'http' {
  const blob = [def.command, ...def.args].join(' ').toLowerCase();
  return blob.includes('mcp-remote') || blob.includes('http') ? 'http' : 'stdio';
}

/** Column template shared by the MCP-server ListGrid header + rows (spec §4). */
const SERVER_COLUMNS =
  'minmax(0,1.4fr) minmax(0,1.6fr) minmax(0,0.7fr) minmax(0,0.5fr) 2.5rem';

/** Small agent picker (MDS Select) shared by the add/import/install dialogs. */
function AgentSelect({
  value,
  onChange,
  agents,
  placeholder,
  className,
}: {
  value: string;
  onChange: (v: string) => void;
  agents: ReadonlyArray<AgentLite>;
  placeholder?: string;
  className?: string;
}) {
  const current = agents.find((a) => a.name === value);
  return (
    <Select value={value} onValueChange={(v) => onChange(String(v))}>
      <SelectTrigger className={cn('w-full', className)}>
        <SelectValue placeholder={placeholder}>
          {current ? current.display_name || current.name : placeholder}
        </SelectValue>
      </SelectTrigger>
      <SelectContent>
        {agents.map((a) => (
          <SelectItem key={a.name} value={a.name}>
            {a.display_name || a.name}
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  );
}

export function McpPage() {
  const intl = useIntl();
  const connState = useConnectionStore((s) => s.state);
  const { agentConfigs, catalog, loading, fetchAll, oauthProviders, fetchOAuthProviders } = useMcpStore();
  const { agents, fetchAgents } = useAgentsStore();
  const [activeTab, setActiveTab] = useState<Tab>('agents');
  const [selectedAgentId, setSelectedAgentId] = useState<string | null>(null);
  const [showAddDialog, setShowAddDialog] = useState(false);
  const [showImportDialog, setShowImportDialog] = useState(false);
  const [catalogFilter, setCatalogFilter] = useState<string | null>(null);
  const [catalogSearch, setCatalogSearch] = useState('');

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
    ? Array.isArray(selectedConfig.servers)
      ? selectedConfig.servers
      : Object.entries(
          selectedConfig.servers as Record<string, McpServerDef>,
        ).map(([name, def]) => ({ name, ...def }))
    : [];

  const agentLabel = (agentId: string) =>
    agents.find((a) => a.name === agentId)?.display_name ?? agentId;

  const handleRemoveServer = async (agentId: string, serverName: string) => {
    if (!confirm(intl.formatMessage({ id: 'mcp.confirmRemove' }, { agent: agentLabel(agentId), server: serverName }))) return;
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
    agentConfigs.some((cfg) =>
      (Array.isArray(cfg.servers) ? cfg.servers.map((s) => s.name) : Object.keys(cfg.servers)).includes(catalogId),
    );

  const tabOptions: SegmentedOption<Tab>[] = [
    { value: 'agents', label: intl.formatMessage({ id: 'mcp.tab.agents' }) },
    { value: 'marketplace', label: intl.formatMessage({ id: 'mcp.tab.marketplace' }) },
    { value: 'oauth', label: intl.formatMessage({ id: 'mcp.tab.oauth' }) },
  ];

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
          <Button variant="brand" size="sm" onClick={() => setShowAddDialog(true)}>
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
                    onRemove={() => handleRemoveServer(selectedAgentId, entry.name)}
                  />
                ))}
              </ListGridContainer>
            </div>
          )}
        </div>
      ) : activeTab === 'marketplace' ? (
        <MarketplaceTab
          catalog={filteredCatalog}
          categories={categories}
          catalogFilter={catalogFilter}
          setCatalogFilter={setCatalogFilter}
          catalogSearch={catalogSearch}
          setCatalogSearch={setCatalogSearch}
          isInstalled={isInstalled}
          agents={agents}
          onInstall={async (agentId, item) => {
            try {
              await useMcpStore.getState().addServer(agentId, item.id, item.default_def);
              showToast('success', intl.formatMessage({ id: 'mcp.added' }, { server: item.name, agent: agentLabel(agentId) }));
            } catch {
              showToast('error', intl.formatMessage({ id: 'mcp.loadFailed' }));
            }
          }}
        />
      ) : (
        <OAuthTab providers={oauthProviders} showToast={showToast} />
      )}

      {/* Add Server Dialog */}
      <AddServerDialog
        open={showAddDialog}
        onClose={() => setShowAddDialog(false)}
        catalog={catalog}
        agents={agents}
        onAdded={(serverName, agentId) => {
          showToast('success', intl.formatMessage({ id: 'mcp.added' }, { server: serverName, agent: agentLabel(agentId) }));
        }}
        onError={() => {
          showToast('error', intl.formatMessage({ id: 'mcp.loadFailed' }));
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
    </div>
  );
}

// ── Agent-config server row (spec §4 ListGrid) ─────────────────

function ServerRow({ entry, onRemove }: { entry: McpServerEntry; onRemove: () => void }) {
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
  onInstall,
}: {
  catalog: ReadonlyArray<McpCatalogItem>;
  categories: string[];
  catalogFilter: string | null;
  setCatalogFilter: (v: string | null) => void;
  catalogSearch: string;
  setCatalogSearch: (v: string) => void;
  isInstalled: (id: string) => boolean;
  agents: ReadonlyArray<AgentLite>;
  onInstall: (agentId: string, item: McpCatalogItem) => Promise<void>;
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
              onInstall={(agentId) => onInstall(agentId, item)}
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
  onInstall,
}: {
  item: McpCatalogItem;
  installed: boolean;
  agents: ReadonlyArray<AgentLite>;
  onInstall: (agentId: string) => Promise<void>;
}) {
  const intl = useIntl();
  const CatIcon = getCategoryIcon(item.category);
  const [showInstall, setShowInstall] = useState(false);
  const [targetAgent, setTargetAgent] = useState('');
  const [installing, setInstalling] = useState(false);

  const handleInstall = async () => {
    if (!targetAgent) return;
    setInstalling(true);
    try {
      await onInstall(targetAgent);
      setShowInstall(false);
      setTargetAgent('');
    } finally {
      setInstalling(false);
    }
  };

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
      </div>
      <Button variant="outline" size="sm" onClick={() => setShowInstall(true)} className="shrink-0">
        <Plus />
        <span className="hidden sm:inline">{intl.formatMessage({ id: 'mcp.catalog.install' })}</span>
      </Button>

      <Dialog open={showInstall} onOpenChange={(o) => { if (!o) { setShowInstall(false); setTargetAgent(''); } }}>
        <DialogContent className="sm:max-w-md">
          <DialogHeader>
            <DialogTitle>{intl.formatMessage({ id: 'mcp.catalog.install' })}</DialogTitle>
            <DialogDescription>{item.name}</DialogDescription>
          </DialogHeader>
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
          <DialogFooter>
            <DialogClose render={<Button variant="outline">{intl.formatMessage({ id: 'mcp.cancel' })}</Button>} />
            <Button variant="brand" onClick={handleInstall} disabled={installing || !targetAgent}>
              {installing ? intl.formatMessage({ id: 'mcp.adding' }) : intl.formatMessage({ id: 'mcp.catalog.install' })}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
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
                onKeyDown={(e) => { if (e.key === 'Enter') handleFetch(); }}
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
  switch (provider.token_status) {
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
  const [configureProvider, setConfigureProvider] = useState<McpOAuthProvider | null>(null);
  const pollTimerRef = useRef<ReturnType<typeof setInterval> | null>(null);

  useEffect(() => {
    return () => {
      if (pollTimerRef.current) clearInterval(pollTimerRef.current);
    };
  }, []);

  const handleAuthenticate = async (provider: McpOAuthProvider) => {
    setPendingProvider(provider.provider_id);
    try {
      const authUrl = await useMcpStore.getState().startOAuth(provider.provider_id);
      window.open(authUrl, '_blank');

      // Start local polling for UI feedback
      let attempts = 0;
      if (pollTimerRef.current) clearInterval(pollTimerRef.current);
      pollTimerRef.current = setInterval(async () => {
        attempts += 1;
        if (attempts > 100) {
          if (pollTimerRef.current) clearInterval(pollTimerRef.current);
          setPendingProvider(null);
          return;
        }
        const updatedProviders = useMcpStore.getState().oauthProviders;
        const updated = updatedProviders.find((p) => p.provider_id === provider.provider_id);
        if (updated?.token_status === 'authenticated') {
          if (pollTimerRef.current) clearInterval(pollTimerRef.current);
          setPendingProvider(null);
          showToast('success', intl.formatMessage({ id: 'mcp.oauth.success' }, { provider: provider.name }));
        }
      }, 3000);
    } catch {
      setPendingProvider(null);
      showToast('error', intl.formatMessage({ id: 'mcp.loadFailed' }));
    }
  };

  const handleRevoke = async (provider: McpOAuthProvider) => {
    if (!confirm(intl.formatMessage({ id: 'mcp.oauth.revokeConfirm' }, { provider: provider.name }))) return;
    try {
      await useMcpStore.getState().revokeOAuth(provider.provider_id);
      showToast('success', intl.formatMessage({ id: 'mcp.oauth.revoked' }, { provider: provider.name }));
    } catch {
      showToast('error', intl.formatMessage({ id: 'mcp.loadFailed' }));
    }
  };

  return (
    <div className="space-y-4">
      <h2 className="text-sm font-medium text-foreground">{intl.formatMessage({ id: 'mcp.oauth.title' })}</h2>
      {providers.length === 0 ? (
        <Empty icon={Shield} title={intl.formatMessage({ id: 'mcp.empty' })} />
      ) : (
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
                      <h3 className="truncate text-base font-medium text-foreground">{provider.name}</h3>
                      <Badge variant={status.variant} className={status.className}>{status.label}</Badge>
                    </div>
                  </div>

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
                </CardContent>

                {/* Actions */}
                <div className="flex gap-2 border-t border-surface-border px-4 pt-3">
                  {!provider.configured && (
                    <Button variant="brand" size="sm" onClick={() => setConfigureProvider(provider)}>
                      <KeyRound />
                      {intl.formatMessage({ id: 'mcp.oauth.configure' })}
                    </Button>
                  )}
                  {provider.configured && provider.token_status !== 'authenticated' && !isPending && (
                    <Button variant="brand" size="sm" onClick={() => handleAuthenticate(provider)}>
                      <Shield />
                      {intl.formatMessage({ id: 'mcp.oauth.authenticate' })}
                    </Button>
                  )}
                  {provider.token_status === 'authenticated' && (
                    <Button variant="destructive" size="sm" onClick={() => handleRevoke(provider)}>
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
        />
      )}
    </div>
  );
}

function ConfigureOAuthDialog({
  provider,
  onClose,
  showToast,
}: {
  provider: McpOAuthProvider;
  onClose: () => void;
  showToast: (type: 'success' | 'error', message: string) => void;
}) {
  const intl = useIntl();
  const [clientId, setClientId] = useState('');
  const [clientSecret, setClientSecret] = useState('');
  const [submitting, setSubmitting] = useState(false);

  const helpKey = `mcp.oauth.help${provider.provider_id.charAt(0).toUpperCase()}${provider.provider_id.slice(1)}` as const;

  const handleSubmit = async () => {
    if (!clientId.trim()) return;
    setSubmitting(true);
    try {
      const authUrl = await useMcpStore.getState().startOAuth(provider.provider_id, clientId.trim(), clientSecret.trim() || undefined);
      window.open(authUrl, '_blank');
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
            <Input type="text" value={provider.name} readOnly className="bg-muted/50" />
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
            <Input
              type="password"
              value={clientSecret}
              onChange={(e) => setClientSecret(e.target.value)}
              placeholder="Client Secret"
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

// ── Add server dialog (catalog / custom) ───────────────────────

function AddServerDialog({
  open,
  onClose,
  catalog,
  agents,
  onAdded,
  onError,
}: {
  open: boolean;
  onClose: () => void;
  catalog: ReadonlyArray<McpCatalogItem>;
  agents: ReadonlyArray<AgentLite>;
  onAdded: (serverName: string, agentId: string) => void;
  onError: () => void;
}) {
  const intl = useIntl();
  const [mode, setMode] = useState<'catalog' | 'custom'>('catalog');
  const [selectedCatalogId, setSelectedCatalogId] = useState('');
  const [targetAgent, setTargetAgent] = useState('');
  const [serverName, setServerName] = useState('');
  const [command, setCommand] = useState('');
  const [args, setArgs] = useState('');
  const [envText, setEnvText] = useState('');
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // When a catalog item is selected, auto-fill fields
  useEffect(() => {
    if (mode === 'catalog' && selectedCatalogId) {
      const item = catalog.find((c) => c.id === selectedCatalogId);
      if (item) {
        setServerName(item.id);
        setCommand(item.default_def.command);
        setArgs(item.default_def.args.join(' '));
        setEnvText(
          Object.entries(item.default_def.env)
            .map(([k, v]) => `${k}=${v}`)
            .join('\n'),
        );
      }
    }
  }, [mode, selectedCatalogId, catalog]);

  const resetForm = () => {
    setSelectedCatalogId('');
    setTargetAgent('');
    setServerName('');
    setCommand('');
    setArgs('');
    setEnvText('');
    setError(null);
  };

  const handleSubmit = async () => {
    if (!targetAgent || !serverName.trim() || !command.trim()) return;
    setSubmitting(true);
    setError(null);
    try {
      const parsedEnv: Record<string, string> = {};
      for (const line of envText.split('\n')) {
        const t = line.trim();
        if (!t) continue;
        const eqIdx = t.indexOf('=');
        if (eqIdx > 0) {
          parsedEnv[t.slice(0, eqIdx)] = t.slice(eqIdx + 1);
        }
      }
      const def: McpServerDef = {
        command: command.trim(),
        args: args.trim() ? args.trim().split(/\s+/) : [],
        env: parsedEnv,
      };
      await useMcpStore.getState().addServer(targetAgent, serverName.trim(), def);
      onAdded(serverName.trim(), targetAgent);
      onClose();
      resetForm();
    } catch {
      setError(intl.formatMessage({ id: 'mcp.loadFailed' }));
      onError();
    } finally {
      setSubmitting(false);
    }
  };

  const catalogModeOptions: SegmentedOption<'catalog' | 'custom'>[] = [
    { value: 'catalog', label: intl.formatMessage({ id: 'mcp.fromCatalog' }) },
    { value: 'custom', label: intl.formatMessage({ id: 'mcp.custom' }) },
  ];
  const selectedCatalogItem = catalog.find((c) => c.id === selectedCatalogId);

  return (
    <Dialog open={open} onOpenChange={(o) => { if (!o) { onClose(); resetForm(); } }}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{intl.formatMessage({ id: 'mcp.addTitle' })}</DialogTitle>
        </DialogHeader>

        <div className="space-y-4">
          {/* Mode toggle */}
          <Segmented value={mode} onValueChange={setMode} options={catalogModeOptions} className="w-full" />

          {/* Catalog selector */}
          {mode === 'catalog' && (
            <div className="space-y-1.5">
              <label className="text-xs font-medium text-muted-foreground">
                {intl.formatMessage({ id: 'mcp.fromCatalog' })}
              </label>
              <Select value={selectedCatalogId} onValueChange={(v) => setSelectedCatalogId(String(v))}>
                <SelectTrigger className="w-full">
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
              readOnly={mode === 'catalog' && !!selectedCatalogId}
            />
          </div>

          {/* Command / Args / Env — spawning an MCP server runs a real process on
              the host, so these live inside a DangerZone. */}
          <DangerZone
            title={intl.formatMessage({ id: 'mcp.custom.dangerTitle' })}
            description={intl.formatMessage({ id: 'mcp.custom.dangerDesc' })}
          >
            <div className="space-y-1.5">
              <label className="text-xs font-medium text-muted-foreground">
                {intl.formatMessage({ id: 'mcp.command' })}
              </label>
              <Input
                type="text"
                value={command}
                onChange={(e) => setCommand(e.target.value)}
                placeholder="e.g. npx"
              />
            </div>

            <div className="space-y-1.5">
              <label className="text-xs font-medium text-muted-foreground">
                {intl.formatMessage({ id: 'mcp.args' })}
              </label>
              <Input
                type="text"
                value={args}
                onChange={(e) => setArgs(e.target.value)}
                placeholder="e.g. -y @modelcontextprotocol/server-filesystem /path"
              />
            </div>

            <div className="space-y-1.5">
              <label className="text-xs font-medium text-muted-foreground">
                {intl.formatMessage({ id: 'mcp.env' })}
              </label>
              <Textarea
                value={envText}
                onChange={(e) => setEnvText(e.target.value)}
                placeholder={'API_KEY=your-key\nANOTHER_VAR=value'}
                rows={3}
                className="resize-none font-mono text-xs"
              />
              <p className="text-xs text-muted-foreground">{intl.formatMessage({ id: 'mcp.env.help' })}</p>
            </div>
          </DangerZone>

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
            disabled={submitting || !targetAgent || !serverName.trim() || !command.trim()}
          >
            {submitting ? intl.formatMessage({ id: 'mcp.adding' }) : intl.formatMessage({ id: 'mcp.add' })}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
