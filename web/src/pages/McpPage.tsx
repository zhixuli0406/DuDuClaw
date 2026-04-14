import { useEffect, useState, useCallback, useRef } from 'react';
import { useIntl, FormattedMessage } from 'react-intl';
import { cn } from '@/lib/utils';
import { useMcpStore } from '@/stores/mcp-store';
import { useAgentsStore } from '@/stores/agents-store';
import { useConnectionStore } from '@/stores/connection-store';
import { type McpServerDef, type McpCatalogItem, type McpOAuthProvider } from '@/lib/api';
import { Dialog, FormField, inputClass, selectClass, buttonPrimary, buttonSecondary } from '@/components/shared/Dialog';
import {
  Plug,
  Plus,
  Trash2,
  Package,
  ChevronRight,
  Search,
  Globe,
  Database,
  MessageSquare,
  CheckCircle2,
  AlertTriangle,
  X,
  Loader2,
  Shield,
  KeyRound,
} from 'lucide-react';

type Tab = 'agents' | 'marketplace' | 'oauth';

const categoryIcons: Record<string, typeof Globe> = {
  browser: Globe,
  data: Database,
  communication: MessageSquare,
  google: Globe,
};

function getCategoryIcon(category: string) {
  return categoryIcons[category] ?? Package;
}

export function McpPage() {
  const intl = useIntl();
  const connState = useConnectionStore((s) => s.state);
  const { agentConfigs, catalog, loading, fetchAll, oauthProviders, fetchOAuthProviders } = useMcpStore();
  const { agents, fetchAgents } = useAgentsStore();
  const [activeTab, setActiveTab] = useState<Tab>('agents');
  const [selectedAgentId, setSelectedAgentId] = useState<string | null>(null);
  const [showAddDialog, setShowAddDialog] = useState(false);
  const [catalogFilter, setCatalogFilter] = useState<string | null>(null);
  const [catalogSearch, setCatalogSearch] = useState('');
  const [toast, setToast] = useState<{ type: 'success' | 'error'; message: string } | null>(null);

  const toastTimerRef = useRef<ReturnType<typeof setTimeout>>(null);
  const showToast = useCallback((type: 'success' | 'error', message: string) => {
    if (toastTimerRef.current) clearTimeout(toastTimerRef.current);
    setToast({ type, message });
    toastTimerRef.current = setTimeout(() => setToast(null), type === 'error' ? 8000 : 4000);
  }, []);
  const dismissToast = useCallback(() => {
    if (toastTimerRef.current) clearTimeout(toastTimerRef.current);
    setToast(null);
  }, []);
  useEffect(() => {
    return () => { if (toastTimerRef.current) clearTimeout(toastTimerRef.current); };
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
  const serverEntries = selectedConfig ? Object.entries(selectedConfig.servers) : [];

  const handleRemoveServer = async (agentId: string, serverName: string) => {
    const agentLabel = agents.find((a) => a.name === agentId)?.display_name ?? agentId;
    if (!confirm(intl.formatMessage({ id: 'mcp.confirmRemove' }, { agent: agentLabel, server: serverName }))) return;
    try {
      await useMcpStore.getState().removeServer(agentId, serverName);
      showToast('success', intl.formatMessage({ id: 'mcp.removed' }, { server: serverName, agent: agentLabel }));
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

  // Group filtered catalog by category
  const groupedCatalog = filteredCatalog.reduce<Record<string, ReadonlyArray<McpCatalogItem>>>((acc, item) => {
    const group = acc[item.category] ?? [];
    return { ...acc, [item.category]: [...group, item] };
  }, {});

  // Check if a catalog item is installed on any agent
  const isInstalled = (catalogId: string) =>
    agentConfigs.some((cfg) => Object.keys(cfg.servers).includes(catalogId));

  return (
    <div className="space-y-6">
      {/* Toast */}
      {toast && (
        <div className={cn(
          'flex items-start gap-3 rounded-lg px-4 py-3 text-sm shadow-sm transition-all',
          toast.type === 'success'
            ? 'bg-emerald-50 text-emerald-700 dark:bg-emerald-900/20 dark:text-emerald-400'
            : 'bg-rose-50 text-rose-700 dark:bg-rose-900/20 dark:text-rose-400'
        )}>
          {toast.type === 'success' ? (
            <CheckCircle2 className="mt-0.5 h-4 w-4 shrink-0" />
          ) : (
            <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0" />
          )}
          <span className="flex-1">{toast.message}</span>
          <button
            onClick={dismissToast}
            className="shrink-0 rounded p-0.5 opacity-60 transition-opacity hover:opacity-100"
          >
            <X className="h-3.5 w-3.5" />
          </button>
        </div>
      )}

      {/* Header */}
      <div className="flex items-center justify-between">
        <h2 className="text-2xl font-semibold text-stone-900 dark:text-stone-50">
          {intl.formatMessage({ id: 'mcp.title' })}
        </h2>
        <button
          onClick={() => setShowAddDialog(true)}
          className="inline-flex items-center gap-2 rounded-lg bg-amber-500 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-amber-600"
        >
          <Plus className="h-4 w-4" />
          {intl.formatMessage({ id: 'mcp.add' })}
        </button>
      </div>

      {/* Tabs */}
      <div className="flex gap-1 rounded-lg bg-stone-100 p-1 dark:bg-stone-800">
        <button
          onClick={() => setActiveTab('agents')}
          className={cn(
            'flex-1 rounded-md px-4 py-2 text-sm font-medium transition-colors',
            activeTab === 'agents'
              ? 'bg-white text-stone-900 shadow-sm dark:bg-stone-700 dark:text-stone-50'
              : 'text-stone-500 hover:text-stone-700 dark:text-stone-400 dark:hover:text-stone-300'
          )}
        >
          {intl.formatMessage({ id: 'mcp.tab.agents' })}
        </button>
        <button
          onClick={() => setActiveTab('marketplace')}
          className={cn(
            'flex-1 rounded-md px-4 py-2 text-sm font-medium transition-colors',
            activeTab === 'marketplace'
              ? 'bg-white text-stone-900 shadow-sm dark:bg-stone-700 dark:text-stone-50'
              : 'text-stone-500 hover:text-stone-700 dark:text-stone-400 dark:hover:text-stone-300'
          )}
        >
          {intl.formatMessage({ id: 'mcp.tab.marketplace' })}
        </button>
        <button
          onClick={() => setActiveTab('oauth')}
          className={cn(
            'flex-1 rounded-md px-4 py-2 text-sm font-medium transition-colors',
            activeTab === 'oauth'
              ? 'bg-white text-stone-900 shadow-sm dark:bg-stone-700 dark:text-stone-50'
              : 'text-stone-500 hover:text-stone-700 dark:text-stone-400 dark:hover:text-stone-300'
          )}
        >
          {intl.formatMessage({ id: 'mcp.tab.oauth' })}
        </button>
      </div>

      {loading && (
        <div className="flex items-center justify-center py-12">
          <Loader2 className="h-6 w-6 animate-spin text-amber-500" />
        </div>
      )}

      {/* Tab 1: Agent Config */}
      {!loading && activeTab === 'agents' && (
        <div className="flex gap-6">
          {/* Left panel: agent list */}
          <div className="w-56 shrink-0 space-y-1">
            {agentConfigs.length === 0 ? (
              <div className="rounded-lg border border-dashed border-stone-300 px-4 py-8 text-center text-sm text-stone-400 dark:border-stone-700 dark:text-stone-500">
                {intl.formatMessage({ id: 'mcp.empty' })}
              </div>
            ) : (
              agentConfigs.map((cfg) => {
                const agent = agents.find((a) => a.name === cfg.agent_id);
                const serverCount = Object.keys(cfg.servers).length;
                return (
                  <button
                    key={cfg.agent_id}
                    onClick={() => setSelectedAgentId(cfg.agent_id)}
                    className={cn(
                      'flex w-full items-center justify-between rounded-lg px-3 py-2.5 text-left text-sm transition-colors',
                      selectedAgentId === cfg.agent_id
                        ? 'bg-amber-100 text-amber-700 dark:bg-amber-900/30 dark:text-amber-400'
                        : 'text-stone-600 hover:bg-stone-100 dark:text-stone-400 dark:hover:bg-stone-800'
                    )}
                  >
                    <span className="truncate font-medium">{agent?.display_name ?? cfg.agent_id}</span>
                    <div className="flex items-center gap-1">
                      <span className="text-xs text-stone-400 dark:text-stone-500">{serverCount}</span>
                      <ChevronRight className="h-3.5 w-3.5 text-stone-400 dark:text-stone-500" />
                    </div>
                  </button>
                );
              })
            )}
          </div>

          {/* Right panel: server cards */}
          <div className="flex-1">
            {!selectedAgentId ? (
              <div className="flex flex-col items-center justify-center rounded-xl border border-dashed border-stone-300 bg-white py-16 dark:border-stone-700 dark:bg-stone-900">
                <Plug className="mb-4 h-12 w-12 text-stone-300 dark:text-stone-600" />
                <p className="text-stone-500 dark:text-stone-400">
                  {intl.formatMessage({ id: 'mcp.selectAgent' })}
                </p>
              </div>
            ) : serverEntries.length === 0 ? (
              <div className="flex flex-col items-center justify-center rounded-xl border border-dashed border-stone-300 bg-white py-16 dark:border-stone-700 dark:bg-stone-900">
                <Package className="mb-4 h-12 w-12 text-stone-300 dark:text-stone-600" />
                <p className="text-stone-500 dark:text-stone-400">
                  {intl.formatMessage({ id: 'mcp.noServers' })}
                </p>
              </div>
            ) : (
              <div className="grid gap-4 sm:grid-cols-2">
                {serverEntries.map(([name, def]) => (
                  <div
                    key={name}
                    className="rounded-xl border border-stone-200 bg-white p-5 transition-shadow hover:shadow-md dark:border-stone-800 dark:bg-stone-900"
                  >
                    <div className="flex items-start justify-between">
                      <div className="flex items-center gap-3">
                        <div className="rounded-lg bg-amber-100 p-2.5 dark:bg-amber-900/30">
                          <Plug className="h-5 w-5 text-amber-600 dark:text-amber-400" />
                        </div>
                        <div>
                          <h3 className="font-semibold text-stone-900 dark:text-stone-50">{name}</h3>
                          <p className="text-xs text-stone-500 dark:text-stone-400 font-mono">{def.command}</p>
                        </div>
                      </div>
                    </div>

                    {/* Args */}
                    {def.args.length > 0 && (
                      <div className="mt-3">
                        <p className="text-xs font-medium text-stone-500 dark:text-stone-400">
                          {intl.formatMessage({ id: 'mcp.args' })}
                        </p>
                        <p className="mt-0.5 text-xs font-mono text-stone-600 dark:text-stone-300 break-all">
                          {def.args.join(' ')}
                        </p>
                      </div>
                    )}

                    {/* Env keys (values masked) */}
                    {Object.keys(def.env).length > 0 && (
                      <div className="mt-3">
                        <p className="text-xs font-medium text-stone-500 dark:text-stone-400">
                          {intl.formatMessage({ id: 'mcp.env' })}
                        </p>
                        <div className="mt-1 flex flex-wrap gap-1">
                          {Object.keys(def.env).map((key) => (
                            <span
                              key={key}
                              className="inline-flex items-center rounded bg-stone-100 px-1.5 py-0.5 text-xs font-mono text-stone-600 dark:bg-stone-800 dark:text-stone-400"
                            >
                              {key}=***
                            </span>
                          ))}
                        </div>
                      </div>
                    )}

                    {/* Actions */}
                    <div className="mt-4 flex border-t border-stone-100 pt-3 dark:border-stone-800">
                      <button
                        onClick={() => handleRemoveServer(selectedAgentId!, name)}
                        className="inline-flex items-center gap-1 rounded-md px-2.5 py-1.5 text-xs text-rose-600 hover:bg-rose-50 dark:text-rose-400 dark:hover:bg-rose-900/20"
                      >
                        <Trash2 className="h-3.5 w-3.5" />
                        {intl.formatMessage({ id: 'mcp.remove' })}
                      </button>
                    </div>
                  </div>
                ))}
              </div>
            )}
          </div>
        </div>
      )}

      {/* Tab 2: Marketplace */}
      {!loading && activeTab === 'marketplace' && (
        <div className="space-y-4">
          {/* Search + category filters */}
          <div className="flex flex-wrap items-center gap-3">
            <div className="relative flex-1 min-w-[200px]">
              <Search className="absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-stone-400" />
              <input
                type="text"
                value={catalogSearch}
                onChange={(e) => setCatalogSearch(e.target.value)}
                placeholder={intl.formatMessage({ id: 'mcp.serverName' })}
                className={cn(inputClass, 'pl-9')}
              />
            </div>
            <div className="flex gap-1">
              <button
                onClick={() => setCatalogFilter(null)}
                className={cn(
                  'rounded-full px-3 py-1.5 text-xs font-medium transition-colors',
                  !catalogFilter
                    ? 'bg-amber-100 text-amber-700 dark:bg-amber-900/30 dark:text-amber-400'
                    : 'text-stone-500 hover:bg-stone-100 dark:text-stone-400 dark:hover:bg-stone-800'
                )}
              >
                All
              </button>
              {categories.map((cat) => {
                const CatIcon = getCategoryIcon(cat);
                const labelKey = `mcp.catalog.${cat}` as const;
                return (
                  <button
                    key={cat}
                    onClick={() => setCatalogFilter(cat === catalogFilter ? null : cat)}
                    className={cn(
                      'inline-flex items-center gap-1 rounded-full px-3 py-1.5 text-xs font-medium transition-colors',
                      catalogFilter === cat
                        ? 'bg-amber-100 text-amber-700 dark:bg-amber-900/30 dark:text-amber-400'
                        : 'text-stone-500 hover:bg-stone-100 dark:text-stone-400 dark:hover:bg-stone-800'
                    )}
                  >
                    <CatIcon className="h-3 w-3" />
                    {intl.formatMessage({ id: labelKey, defaultMessage: cat })}
                  </button>
                );
              })}
            </div>
          </div>

          {/* Catalog grid */}
          {Object.keys(groupedCatalog).length === 0 ? (
            <div className="flex flex-col items-center justify-center rounded-xl border border-dashed border-stone-300 bg-white py-16 dark:border-stone-700 dark:bg-stone-900">
              <Package className="mb-4 h-12 w-12 text-stone-300 dark:text-stone-600" />
              <p className="text-stone-500 dark:text-stone-400">
                {intl.formatMessage({ id: 'mcp.empty' })}
              </p>
            </div>
          ) : (
            Object.entries(groupedCatalog).map(([category, items]) => (
              <div key={category}>
                <h3 className="mb-3 text-sm font-semibold text-stone-500 uppercase tracking-wider dark:text-stone-400">
                  {intl.formatMessage({ id: `mcp.catalog.${category}`, defaultMessage: category })}
                </h3>
                <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
                  {items.map((item) => {
                    const installed = isInstalled(item.id);
                    const CatIcon = getCategoryIcon(item.category);
                    return (
                      <CatalogCard
                        key={item.id}
                        item={item}
                        installed={installed}
                        CatIcon={CatIcon}
                        agents={agents}
                        onInstall={async (agentId) => {
                          try {
                            await useMcpStore.getState().addServer(agentId, item.id, item.default_def);
                            const agentLabel = agents.find((a) => a.name === agentId)?.display_name ?? agentId;
                            showToast('success', intl.formatMessage({ id: 'mcp.added' }, { server: item.name, agent: agentLabel }));
                          } catch {
                            showToast('error', intl.formatMessage({ id: 'mcp.loadFailed' }));
                          }
                        }}
                      />
                    );
                  })}
                </div>
              </div>
            ))
          )}
        </div>
      )}

      {/* Tab 3: OAuth */}
      {!loading && activeTab === 'oauth' && (
        <OAuthTab providers={oauthProviders} showToast={showToast} />
      )}

      {/* Add Server Dialog */}
      <AddServerDialog
        open={showAddDialog}
        onClose={() => setShowAddDialog(false)}
        catalog={catalog}
        agents={agents}
        onAdded={(serverName, agentId) => {
          const agentLabel = agents.find((a) => a.name === agentId)?.display_name ?? agentId;
          showToast('success', intl.formatMessage({ id: 'mcp.added' }, { server: serverName, agent: agentLabel }));
        }}
        onError={() => {
          showToast('error', intl.formatMessage({ id: 'mcp.loadFailed' }));
        }}
      />
    </div>
  );
}

function getStatusBadge(provider: McpOAuthProvider, intl: ReturnType<typeof useIntl>) {
  if (!provider.configured) {
    return {
      label: intl.formatMessage({ id: 'mcp.oauth.notConfigured' }),
      className: 'bg-stone-100 text-stone-500 dark:bg-stone-800 dark:text-stone-400',
    };
  }
  switch (provider.token_status) {
    case 'authenticated':
      return {
        label: intl.formatMessage({ id: 'mcp.oauth.authenticated' }),
        className: 'bg-emerald-100 text-emerald-700 dark:bg-emerald-900/30 dark:text-emerald-400',
      };
    case 'expired':
      return {
        label: intl.formatMessage({ id: 'mcp.oauth.expired' }),
        className: 'bg-rose-100 text-rose-700 dark:bg-rose-900/30 dark:text-rose-400',
      };
    default:
      return {
        label: intl.formatMessage({ id: 'mcp.oauth.notAuthenticated' }),
        className: 'bg-amber-100 text-amber-700 dark:bg-amber-900/30 dark:text-amber-400',
      };
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

  // Cleanup polling on unmount
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
      <h3 className="text-lg font-semibold text-stone-900 dark:text-stone-50">
        {intl.formatMessage({ id: 'mcp.oauth.title' })}
      </h3>

      {providers.length === 0 ? (
        <div className="flex flex-col items-center justify-center rounded-xl border border-dashed border-stone-300 bg-white py-16 dark:border-stone-700 dark:bg-stone-900">
          <Shield className="mb-4 h-12 w-12 text-stone-300 dark:text-stone-600" />
          <p className="text-stone-500 dark:text-stone-400">
            {intl.formatMessage({ id: 'mcp.empty' })}
          </p>
        </div>
      ) : (
        <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
          {providers.map((provider) => {
            const status = getStatusBadge(provider, intl);
            const isPending = pendingProvider === provider.provider_id;

            return (
              <div
                key={provider.provider_id}
                className="rounded-xl border border-stone-200 bg-white p-5 transition-shadow hover:shadow-md dark:border-stone-800 dark:bg-stone-900"
              >
                <div className="flex items-start justify-between">
                  <div className="flex items-center gap-3">
                    <div className="rounded-lg bg-amber-100 p-2.5 dark:bg-amber-900/30">
                      <Globe className="h-5 w-5 text-amber-600 dark:text-amber-400" />
                    </div>
                    <div>
                      <h3 className="font-semibold text-stone-900 dark:text-stone-50">{provider.name}</h3>
                      <span className={cn('inline-flex items-center rounded-full px-2 py-0.5 text-xs font-medium', status.className)}>
                        {status.label}
                      </span>
                    </div>
                  </div>
                </div>

                {/* Scopes */}
                {provider.scopes.length > 0 && (
                  <div className="mt-3">
                    <p className="text-xs font-medium text-stone-500 dark:text-stone-400">
                      {intl.formatMessage({ id: 'mcp.oauth.scopes' })}
                    </p>
                    <div className="mt-1 flex flex-wrap gap-1">
                      {provider.scopes.map((scope) => (
                        <span
                          key={scope}
                          className="inline-flex items-center rounded bg-stone-100 px-1.5 py-0.5 text-xs font-mono text-stone-600 dark:bg-stone-800 dark:text-stone-400"
                        >
                          {scope}
                        </span>
                      ))}
                    </div>
                  </div>
                )}

                {/* Expires at */}
                {provider.token_status === 'authenticated' && provider.expires_at && (
                  <p className="mt-2 text-xs text-stone-400 dark:text-stone-500">
                    {intl.formatMessage({ id: 'mcp.oauth.expiresAt' }, { date: new Date(provider.expires_at).toLocaleDateString() })}
                  </p>
                )}

                {/* Pending state */}
                {isPending && (
                  <div className="mt-3 flex items-center gap-2 text-sm text-amber-600 dark:text-amber-400">
                    <Loader2 className="h-4 w-4 animate-spin" />
                    {intl.formatMessage({ id: 'mcp.oauth.waiting' })}
                  </div>
                )}

                {/* Actions */}
                <div className="mt-4 flex gap-2 border-t border-stone-100 pt-3 dark:border-stone-800">
                  {!provider.configured && (
                    <button
                      onClick={() => setConfigureProvider(provider)}
                      className="inline-flex items-center gap-1.5 rounded-lg bg-amber-500 px-3 py-1.5 text-xs font-medium text-white transition-colors hover:bg-amber-600"
                    >
                      <KeyRound className="h-3.5 w-3.5" />
                      {intl.formatMessage({ id: 'mcp.oauth.configure' })}
                    </button>
                  )}
                  {provider.configured && provider.token_status !== 'authenticated' && !isPending && (
                    <button
                      onClick={() => handleAuthenticate(provider)}
                      className="inline-flex items-center gap-1.5 rounded-lg bg-amber-500 px-3 py-1.5 text-xs font-medium text-white transition-colors hover:bg-amber-600"
                    >
                      <Shield className="h-3.5 w-3.5" />
                      {intl.formatMessage({ id: 'mcp.oauth.authenticate' })}
                    </button>
                  )}
                  {provider.token_status === 'authenticated' && (
                    <button
                      onClick={() => handleRevoke(provider)}
                      className="inline-flex items-center gap-1 rounded-md px-2.5 py-1.5 text-xs text-rose-600 hover:bg-rose-50 dark:text-rose-400 dark:hover:bg-rose-900/20"
                    >
                      <Trash2 className="h-3.5 w-3.5" />
                      {intl.formatMessage({ id: 'mcp.oauth.revoke' })}
                    </button>
                  )}
                </div>
              </div>
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
    <Dialog open onClose={onClose} title={intl.formatMessage({ id: 'mcp.oauth.configureTitle' })}>
      <div className="space-y-4">
        <FormField label="Provider">
          <input type="text" value={provider.name} readOnly className={cn(inputClass, 'bg-stone-50 dark:bg-stone-800')} />
        </FormField>

        <FormField label={intl.formatMessage({ id: 'mcp.oauth.clientId' })}>
          <input
            type="text"
            value={clientId}
            onChange={(e) => setClientId(e.target.value)}
            placeholder="Client ID"
            className={inputClass}
          />
        </FormField>

        <FormField label={intl.formatMessage({ id: 'mcp.oauth.clientSecret' })}>
          <input
            type="password"
            value={clientSecret}
            onChange={(e) => setClientSecret(e.target.value)}
            placeholder="Client Secret"
            className={inputClass}
          />
        </FormField>

        <p className="text-xs text-stone-400 dark:text-stone-500">
          {intl.formatMessage({ id: helpKey, defaultMessage: '' })}
        </p>

        <div className="flex justify-end gap-3 pt-2">
          <button onClick={onClose} className={buttonSecondary}>
            {intl.formatMessage({ id: 'mcp.cancel' })}
          </button>
          <button
            onClick={handleSubmit}
            disabled={submitting || !clientId.trim()}
            className={buttonPrimary}
          >
            {submitting ? intl.formatMessage({ id: 'mcp.adding' }) : intl.formatMessage({ id: 'mcp.oauth.authenticate' })}
          </button>
        </div>
      </div>
    </Dialog>
  );
}

function CatalogCard({
  item,
  installed,
  CatIcon,
  agents,
  onInstall,
}: {
  item: McpCatalogItem;
  installed: boolean;
  CatIcon: typeof Globe;
  agents: ReadonlyArray<{ name: string; display_name: string }>;
  onInstall: (agentId: string) => Promise<void>;
}) {
  const intl = useIntl();
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
    <>
      <div className="rounded-xl border border-stone-200 bg-white p-5 transition-shadow hover:shadow-md dark:border-stone-800 dark:bg-stone-900">
        <div className="flex items-start justify-between">
          <div className="flex items-center gap-3">
            <div className="rounded-lg bg-stone-100 p-2.5 dark:bg-stone-800">
              <CatIcon className="h-5 w-5 text-stone-600 dark:text-stone-400" />
            </div>
            <div>
              <h3 className="font-semibold text-stone-900 dark:text-stone-50">{item.name}</h3>
              <span className="inline-flex items-center rounded-full bg-stone-100 px-2 py-0.5 text-xs text-stone-500 dark:bg-stone-800 dark:text-stone-400">
                {intl.formatMessage({ id: `mcp.catalog.${item.category}`, defaultMessage: item.category })}
              </span>
            </div>
          </div>
          {installed && (
            <CheckCircle2 className="h-5 w-5 text-emerald-500" />
          )}
        </div>

        <p className="mt-3 text-sm text-stone-600 dark:text-stone-400">{item.description}</p>

        {item.required_env.length > 0 && (
          <p className="mt-2 text-xs text-stone-400 dark:text-stone-500">
            <FormattedMessage id="mcp.catalog.requiresEnv" values={{ vars: item.required_env.join(', ') }} />
          </p>
        )}

        <div className="mt-4 border-t border-stone-100 pt-3 dark:border-stone-800">
          <button
            onClick={() => setShowInstall(true)}
            className="inline-flex items-center gap-1.5 rounded-lg bg-amber-500 px-3 py-1.5 text-xs font-medium text-white transition-colors hover:bg-amber-600"
          >
            <Plus className="h-3.5 w-3.5" />
            {intl.formatMessage({ id: 'mcp.catalog.install' })}
          </button>
        </div>
      </div>

      {/* Install target agent dialog */}
      <Dialog
        open={showInstall}
        onClose={() => { setShowInstall(false); setTargetAgent(''); }}
        title={intl.formatMessage({ id: 'mcp.catalog.install' })}
      >
        <div className="space-y-4">
          <FormField label={intl.formatMessage({ id: 'mcp.targetAgent' })}>
            <select value={targetAgent} onChange={(e) => setTargetAgent(e.target.value)} className={selectClass}>
              <option value="">--</option>
              {agents.map((a) => (
                <option key={a.name} value={a.name}>{a.display_name || a.name}</option>
              ))}
            </select>
          </FormField>
          <div className="flex justify-end gap-3 pt-2">
            <button onClick={() => { setShowInstall(false); setTargetAgent(''); }} className={buttonSecondary}>
              {intl.formatMessage({ id: 'mcp.cancel' })}
            </button>
            <button onClick={handleInstall} disabled={installing || !targetAgent} className={buttonPrimary}>
              {installing ? intl.formatMessage({ id: 'mcp.adding' }) : intl.formatMessage({ id: 'mcp.catalog.install' })}
            </button>
          </div>
        </div>
      </Dialog>
    </>
  );
}

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
  agents: ReadonlyArray<{ name: string; display_name: string }>;
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
            .join('\n')
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
        const trimmed = line.trim();
        if (!trimmed) continue;
        const eqIdx = trimmed.indexOf('=');
        if (eqIdx > 0) {
          parsedEnv[trimmed.slice(0, eqIdx)] = trimmed.slice(eqIdx + 1);
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

  return (
    <Dialog open={open} onClose={() => { onClose(); resetForm(); }} title={intl.formatMessage({ id: 'mcp.addTitle' })}>
      <div className="space-y-4">
        {/* Mode toggle */}
        <div className="flex gap-1 rounded-lg bg-stone-100 p-1 dark:bg-stone-800">
          <button
            onClick={() => setMode('catalog')}
            className={cn(
              'flex-1 rounded-md px-3 py-1.5 text-xs font-medium transition-colors',
              mode === 'catalog'
                ? 'bg-white text-stone-900 shadow-sm dark:bg-stone-700 dark:text-stone-50'
                : 'text-stone-500 dark:text-stone-400'
            )}
          >
            {intl.formatMessage({ id: 'mcp.fromCatalog' })}
          </button>
          <button
            onClick={() => setMode('custom')}
            className={cn(
              'flex-1 rounded-md px-3 py-1.5 text-xs font-medium transition-colors',
              mode === 'custom'
                ? 'bg-white text-stone-900 shadow-sm dark:bg-stone-700 dark:text-stone-50'
                : 'text-stone-500 dark:text-stone-400'
            )}
          >
            {intl.formatMessage({ id: 'mcp.custom' })}
          </button>
        </div>

        {/* Catalog selector */}
        {mode === 'catalog' && (
          <FormField label={intl.formatMessage({ id: 'mcp.fromCatalog' })}>
            <select
              value={selectedCatalogId}
              onChange={(e) => setSelectedCatalogId(e.target.value)}
              className={selectClass}
            >
              <option value="">--</option>
              {catalog.map((item) => (
                <option key={item.id} value={item.id}>{item.name}</option>
              ))}
            </select>
          </FormField>
        )}

        {/* Target agent */}
        <FormField label={intl.formatMessage({ id: 'mcp.targetAgent' })}>
          <select value={targetAgent} onChange={(e) => setTargetAgent(e.target.value)} className={selectClass}>
            <option value="">--</option>
            {agents.map((a) => (
              <option key={a.name} value={a.name}>{a.display_name || a.name}</option>
            ))}
          </select>
        </FormField>

        {/* Server name */}
        <FormField label={intl.formatMessage({ id: 'mcp.serverName' })}>
          <input
            type="text"
            value={serverName}
            onChange={(e) => setServerName(e.target.value)}
            placeholder="e.g. filesystem"
            className={inputClass}
            readOnly={mode === 'catalog' && !!selectedCatalogId}
          />
        </FormField>

        {/* Command */}
        <FormField label={intl.formatMessage({ id: 'mcp.command' })}>
          <input
            type="text"
            value={command}
            onChange={(e) => setCommand(e.target.value)}
            placeholder="e.g. npx"
            className={inputClass}
          />
        </FormField>

        {/* Args */}
        <FormField label={intl.formatMessage({ id: 'mcp.args' })}>
          <input
            type="text"
            value={args}
            onChange={(e) => setArgs(e.target.value)}
            placeholder="e.g. -y @modelcontextprotocol/server-filesystem /path"
            className={inputClass}
          />
        </FormField>

        {/* Env vars */}
        <FormField label={intl.formatMessage({ id: 'mcp.env' })}>
          <textarea
            value={envText}
            onChange={(e) => setEnvText(e.target.value)}
            placeholder={"API_KEY=your-key\nANOTHER_VAR=value"}
            rows={3}
            className={cn(inputClass, 'resize-none font-mono text-xs')}
          />
        </FormField>

        {error && (
          <div className="flex items-start gap-2 rounded-lg bg-rose-50 px-3 py-2 text-xs text-rose-600 dark:bg-rose-900/20 dark:text-rose-400">
            <AlertTriangle className="mt-0.5 h-3 w-3 shrink-0" />
            <span>{error}</span>
          </div>
        )}

        <div className="flex justify-end gap-3 pt-2">
          <button onClick={() => { onClose(); resetForm(); }} className={buttonSecondary}>
            {intl.formatMessage({ id: 'mcp.cancel' })}
          </button>
          <button
            onClick={handleSubmit}
            disabled={submitting || !targetAgent || !serverName.trim() || !command.trim()}
            className={buttonPrimary}
          >
            {submitting ? intl.formatMessage({ id: 'mcp.adding' }) : intl.formatMessage({ id: 'mcp.add' })}
          </button>
        </div>
      </div>
    </Dialog>
  );
}
