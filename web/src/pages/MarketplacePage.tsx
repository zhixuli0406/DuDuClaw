import { useCallback, useEffect, useMemo, useState } from 'react';
import { useIntl } from 'react-intl';
import { api, type AgentInfo, type MarketplaceServer } from '@/lib/api';
import { Dialog, FormField, selectClass } from '@/components/shared/Dialog';
import {
  Page,
  PageHeader,
  Card,
  Section,
  Tabs,
  Button,
  Badge,
  EmptyState,
  Toolbar,
  type TabItem,
} from '@/components/ui';
import {
  Store,
  Download,
  Check,
  MessageSquare,
  Database,
  Globe,
  Package,
  Sparkles,
  X,
} from 'lucide-react';

type Category = 'all' | 'featured' | 'browser' | 'data' | 'communication';

const CATEGORIES: ReadonlyArray<Category> = [
  'all',
  'featured',
  'browser',
  'data',
  'communication',
];

/** Map a backend `category` string to a lucide icon component. */
function iconForCategory(category: string): React.ComponentType<{ className?: string }> {
  switch (category) {
    case 'browser':
      return Globe;
    case 'data':
      return Database;
    case 'communication':
      return MessageSquare;
    default:
      return Package;
  }
}

function ServerCard({
  server,
  installed,
  onInstall,
}: {
  readonly server: MarketplaceServer;
  readonly installed: boolean;
  readonly onInstall: () => void;
}) {
  const intl = useIntl();
  const Icon = iconForCategory(server.category);

  return (
    <Card interactive className="flex h-full flex-col">
      <div className="flex items-start gap-3">
        <span className="grid h-10 w-10 shrink-0 place-items-center rounded-lg bg-amber-500/12 text-amber-600 ring-1 ring-inset ring-amber-500/20 dark:bg-amber-400/10 dark:text-amber-400">
          <Icon className="h-5 w-5" />
        </span>
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2">
            <h3 className="truncate font-medium text-stone-900 dark:text-stone-50">
              {server.name}
            </h3>
            {server.featured && (
              <Badge tone="accent">
                <Sparkles className="h-3 w-3" />
                {intl.formatMessage({ id: 'marketplace.featured' })}
              </Badge>
            )}
          </div>
          <p className="mt-0.5 truncate text-xs text-stone-500 dark:text-stone-400">
            {server.author}
          </p>
        </div>
      </div>

      <p className="mt-3 line-clamp-2 text-sm text-stone-600 dark:text-stone-400">
        {server.description}
      </p>

      {server.tags.length > 0 && (
        <div className="mt-3 flex flex-wrap gap-1.5">
          {server.tags.map((tag) => (
            <Badge key={tag} tone="neutral">
              {tag}
            </Badge>
          ))}
        </div>
      )}

      <div className="mt-4 flex items-center justify-end">
        {installed ? (
          <Button variant="secondary" size="sm" icon={Check} disabled>
            {intl.formatMessage({ id: 'marketplace.installed' })}
            {server.installed_by.length > 1 && (
              <span className="ml-0.5 opacity-70">({server.installed_by.length})</span>
            )}
          </Button>
        ) : (
          <Button variant="primary" size="sm" icon={Download} onClick={onInstall}>
            {intl.formatMessage({ id: 'marketplace.install' })}
          </Button>
        )}
      </div>
    </Card>
  );
}

export function MarketplacePage() {
  const intl = useIntl();
  const [query, setQuery] = useState('');
  const [category, setCategory] = useState<Category>('all');
  const [installError, setInstallError] = useState<string | null>(null);
  const [servers, setServers] = useState<ReadonlyArray<MarketplaceServer>>([]);
  const [loading, setLoading] = useState(true);
  const [agents, setAgents] = useState<ReadonlyArray<AgentInfo>>([]);
  const [installTarget, setInstallTarget] = useState<string | null>(null);
  const [installAgent, setInstallAgent] = useState('');
  const [installing, setInstalling] = useState(false);

  useEffect(() => {
    api.agents.list().then((res) => {
      const list = res?.agents ?? [];
      setAgents(list);
      if (list.length > 0) {
        setInstallAgent((prev) => prev || list[0].name);
      }
    }).catch(() => {
      // Agent list failure surfaces when the install dialog opens (empty select).
    });
  }, []);
  const [loadError, setLoadError] = useState<string | null>(null);

  // Fetch the catalog (including backend-derived `installed_by`) so the
  // "installed" state survives reloads and reflects real `.mcp.json` content.
  const load = useCallback(async () => {
    setLoading(true);
    setLoadError(null);
    try {
      const res = await api.marketplace.list();
      setServers(res.servers ?? []);
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      setLoadError(message);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  const featuredServers = useMemo(
    () => servers.filter((s) => s.featured),
    [servers],
  );

  const filtered = useMemo(() => {
    const q = query.toLowerCase();
    return servers.filter((s) => {
      if (category === 'featured' && !s.featured) return false;
      if (category !== 'all' && category !== 'featured' && s.category !== category) return false;
      if (q) {
        return (
          s.name.toLowerCase().includes(q) ||
          s.description.toLowerCase().includes(q) ||
          s.tags.some((t) => t.toLowerCase().includes(q)) ||
          s.author.toLowerCase().includes(q)
        );
      }
      return true;
    });
  }, [query, category, servers]);

  // Installing requires a target agent — open a picker dialog first.
  const handleInstall = (serverId: string) => {
    setInstallError(null);
    setInstallTarget(serverId);
  };

  const confirmInstall = async () => {
    if (!installTarget || !installAgent) return;
    setInstalling(true);
    setInstallError(null);
    try {
      await api.marketplace.install(installTarget, installAgent);
      setInstallTarget(null);
      // Refetch so installed_by reflects the new `.mcp.json` state.
      await load();
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      setInstallError(
        intl.formatMessage({ id: 'marketplace.installError' }, { message }),
      );
    } finally {
      setInstalling(false);
    }
  };

  const categoryTabs: TabItem[] = CATEGORIES.map((cat) => ({
    id: cat,
    label: intl.formatMessage({ id: `marketplace.categories.${cat}` }),
  }));

  return (
    <Page wide>
      <PageHeader
        icon={Store}
        title={intl.formatMessage({ id: 'nav.marketplace' })}
        subtitle={intl.formatMessage({ id: 'marketplace.subtitle' })}
      />

      {/* Install Error Alert */}
      {installError && (
        <div
          role="alert"
          className="flex items-start justify-between gap-3 rounded-lg border border-rose-200 bg-rose-50 px-4 py-3 text-sm text-rose-700 dark:border-rose-800 dark:bg-rose-900/20 dark:text-rose-300"
        >
          <span className="flex-1">{installError}</span>
          <button
            type="button"
            onClick={() => setInstallError(null)}
            className="shrink-0 text-rose-500 hover:text-rose-700 dark:text-rose-400 dark:hover:text-rose-200"
            aria-label="Dismiss"
          >
            <X className="h-4 w-4" />
          </button>
        </div>
      )}

      {/* Load Error */}
      {loadError && (
        <div
          role="alert"
          className="rounded-lg border border-rose-200 bg-rose-50 px-4 py-3 text-sm text-rose-700 dark:border-rose-800 dark:bg-rose-900/20 dark:text-rose-300"
        >
          {loadError}
        </div>
      )}

      {/* Search */}
      <Toolbar
        search={query}
        onSearchChange={setQuery}
        searchPlaceholder={intl.formatMessage({ id: 'marketplace.search' })}
      />

      {/* Category Tabs */}
      <Tabs
        items={categoryTabs}
        value={category}
        onChange={(id) => setCategory(id as Category)}
      />

      {/* Loading */}
      {loading && (
        <p className="py-12 text-center text-stone-400 dark:text-stone-500">
          {intl.formatMessage({ id: 'common.loading' })}
        </p>
      )}

      {/* Featured Section (only on `all` tab with no search) */}
      {!loading && category === 'all' && !query && featuredServers.length > 0 && (
        <Section
          title={
            <span className="flex items-center gap-2">
              <Sparkles className="h-4 w-4 text-amber-500" />
              {intl.formatMessage({ id: 'marketplace.featured' })}
            </span>
          }
        >
          <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
            {featuredServers.map((server) => (
              <ServerCard
                key={server.id}
                server={server}
                installed={server.installed_by.length > 0}
                onInstall={() => handleInstall(server.id)}
              />
            ))}
          </div>
        </Section>
      )}

      {/* All Servers Grid */}
      {!loading && (
        <Section
          title={
            category !== 'all' || query
              ? intl.formatMessage({ id: `marketplace.categories.${category}` })
              : undefined
          }
        >
          {filtered.length === 0 ? (
            <Card>
              <EmptyState
                icon={Package}
                title={
                  servers.length === 0
                    ? intl.formatMessage({ id: 'marketplace.empty' })
                    : intl.formatMessage({ id: 'common.noData' })
                }
              />
            </Card>
          ) : (
            <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
              {filtered.map((server) => (
                <ServerCard
                  key={server.id}
                  server={server}
                  installed={server.installed_by.length > 0}
                  onInstall={() => handleInstall(server.id)}
                />
              ))}
            </div>
          )}
        </Section>
      )}

      {/* Install target agent picker */}
      {installTarget && (
        <Dialog
          open
          onClose={() => setInstallTarget(null)}
          title={intl.formatMessage({ id: 'marketplace.install' })}
        >
          <div className="space-y-4">
            <p className="text-sm text-stone-600 dark:text-stone-400">
              {intl.formatMessage(
                { id: 'marketplace.installTo' },
                { server: servers.find((s) => s.id === installTarget)?.name ?? installTarget },
              )}
            </p>
            <FormField label={intl.formatMessage({ id: 'marketplace.targetAgent' })} htmlFor="marketplace-install-agent">
              <select
                id="marketplace-install-agent"
                value={installAgent}
                onChange={(e) => setInstallAgent(e.target.value)}
                className={selectClass}
              >
                {agents.length === 0 && (
                  <option value="">{intl.formatMessage({ id: 'common.noData' })}</option>
                )}
                {agents.map((a) => (
                  <option key={a.name} value={a.name}>{a.display_name || a.name}</option>
                ))}
              </select>
            </FormField>
            <div className="flex justify-end gap-2 pt-1">
              <Button variant="secondary" onClick={() => setInstallTarget(null)}>
                {intl.formatMessage({ id: 'common.cancel' })}
              </Button>
              <Button
                variant="primary"
                onClick={confirmInstall}
                disabled={installing || !installAgent}
              >
                {installing
                  ? intl.formatMessage({ id: 'common.saving' })
                  : intl.formatMessage({ id: 'marketplace.install' })}
              </Button>
            </div>
          </div>
        </Dialog>
      )}
    </Page>
  );
}
