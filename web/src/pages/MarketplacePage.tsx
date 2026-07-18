import { useCallback, useEffect, useMemo, useState, type ComponentType, type ReactNode } from 'react';
import { useIntl } from 'react-intl';
import { api, type AgentInfo, type MarketplaceServer } from '@/lib/api';
import {
  Badge,
  Button,
  Empty,
  Input,
  Tabs,
  TabsList,
  TabsTab,
  Select,
  SelectTrigger,
  SelectValue,
  SelectContent,
  SelectItem,
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogFooter,
} from '@/components/mds';
import {
  Store,
  Download,
  Check,
  MessageSquare,
  Database,
  Globe,
  Package,
  Sparkles,
  Search,
  X,
} from 'lucide-react';

type Category = 'all' | 'featured' | 'browser' | 'data' | 'communication';

const CATEGORIES: ReadonlyArray<Category> = ['all', 'featured', 'browser', 'data', 'communication'];

/** Map a backend `category` string to a lucide icon component. */
function iconForCategory(category: string): ComponentType<{ className?: string }> {
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

/** Local labeled-field wrapper (spec §4 form pattern). */
function Field({ label, htmlFor, children }: { label: string; htmlFor?: string; children: ReactNode }) {
  return (
    <div className="space-y-1.5">
      <label htmlFor={htmlFor} className="text-xs font-medium text-muted-foreground">
        {label}
      </label>
      {children}
    </div>
  );
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
    <div className="flex h-full flex-col rounded-xl border border-surface-border bg-surface p-4 shadow-[var(--surface-shadow)] transition-colors hover:bg-surface-hover">
      <div className="flex items-start gap-3">
        <span className="grid size-10 shrink-0 place-items-center rounded-lg bg-brand/12 text-brand ring-1 ring-inset ring-brand/20">
          <Icon className="size-5" />
        </span>
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2">
            <h3 className="truncate font-medium text-foreground">{server.name}</h3>
            {server.featured && (
              <Badge variant="secondary" className="bg-brand/15 text-brand">
                <Sparkles />
                {intl.formatMessage({ id: 'marketplace.featured' })}
              </Badge>
            )}
          </div>
          <p className="mt-0.5 truncate text-xs text-muted-foreground">{server.author}</p>
        </div>
      </div>

      <p className="mt-3 line-clamp-2 text-sm text-muted-foreground">{server.description}</p>

      {server.tags.length > 0 && (
        <div className="mt-3 flex flex-wrap gap-1.5">
          {server.tags.map((tag) => (
            <Badge key={tag} variant="secondary">
              {tag}
            </Badge>
          ))}
        </div>
      )}

      <div className="mt-4 flex items-center justify-end">
        {installed ? (
          <Button variant="outline" size="sm" disabled>
            <Check />
            {intl.formatMessage({ id: 'marketplace.installed' })}
            {server.installed_by.length > 1 && (
              <span className="ml-0.5 opacity-70">({server.installed_by.length})</span>
            )}
          </Button>
        ) : (
          <Button variant="brand" size="sm" onClick={onInstall}>
            <Download />
            {intl.formatMessage({ id: 'marketplace.install' })}
          </Button>
        )}
      </div>
    </div>
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
  const [loadError, setLoadError] = useState<string | null>(null);

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

  const featuredServers = useMemo(() => servers.filter((s) => s.featured), [servers]);

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
      setInstallError(intl.formatMessage({ id: 'marketplace.installError' }, { message }));
    } finally {
      setInstalling(false);
    }
  };

  return (
    <div className="space-y-6">
      {/* Slim page header (spec §5.2). */}
      <div className="flex items-center gap-2">
        <Store className="size-5 text-muted-foreground" />
        <div>
          <h1 className="text-base font-medium">{intl.formatMessage({ id: 'nav.marketplace' })}</h1>
          <p className="text-sm text-muted-foreground">{intl.formatMessage({ id: 'marketplace.subtitle' })}</p>
        </div>
      </div>

      {/* Install Error Alert */}
      {installError && (
        <div
          role="alert"
          className="flex items-start justify-between gap-3 rounded-lg border border-destructive/30 bg-destructive/10 px-4 py-3 text-sm text-destructive"
        >
          <span className="flex-1">{installError}</span>
          <button
            type="button"
            onClick={() => setInstallError(null)}
            className="shrink-0 text-destructive/70 hover:text-destructive"
            aria-label="Dismiss"
          >
            <X className="size-4" />
          </button>
        </div>
      )}

      {/* Load Error */}
      {loadError && (
        <div
          role="alert"
          className="rounded-lg border border-destructive/30 bg-destructive/10 px-4 py-3 text-sm text-destructive"
        >
          {loadError}
        </div>
      )}

      {/* Search + category tabs */}
      <div className="flex flex-wrap items-center gap-3">
        <div className="relative">
          <Search className="pointer-events-none absolute left-2.5 top-1/2 size-3.5 -translate-y-1/2 text-muted-foreground" />
          <Input
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder={intl.formatMessage({ id: 'marketplace.search' })}
            className="w-56 pl-8"
          />
        </div>
        <Tabs value={category} onValueChange={(v) => setCategory(v as Category)} variant="line">
          <TabsList>
            {CATEGORIES.map((cat) => (
              <TabsTab key={cat} value={cat}>
                {intl.formatMessage({ id: `marketplace.categories.${cat}` })}
              </TabsTab>
            ))}
          </TabsList>
        </Tabs>
      </div>

      {/* Loading */}
      {loading && (
        <p className="py-12 text-center text-sm text-muted-foreground">
          {intl.formatMessage({ id: 'common.loading' })}
        </p>
      )}

      {/* Featured Section (only on `all` tab with no search) */}
      {!loading && category === 'all' && !query && featuredServers.length > 0 && (
        <section className="space-y-3">
          <h2 className="flex items-center gap-2 text-base font-medium">
            <Sparkles className="size-4 text-brand" />
            {intl.formatMessage({ id: 'marketplace.featured' })}
          </h2>
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
        </section>
      )}

      {/* All Servers Grid */}
      {!loading && (
        <section className="space-y-3">
          {(category !== 'all' || query) && (
            <h2 className="text-base font-medium">
              {intl.formatMessage({ id: `marketplace.categories.${category}` })}
            </h2>
          )}
          {filtered.length === 0 ? (
            <Empty
              icon={Package}
              title={
                servers.length === 0
                  ? intl.formatMessage({ id: 'marketplace.empty' })
                  : intl.formatMessage({ id: 'common.noData' })
              }
              variant="dashed"
            />
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
        </section>
      )}

      {/* Install target agent picker */}
      <Dialog open={installTarget !== null} onOpenChange={(o) => !o && setInstallTarget(null)}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{intl.formatMessage({ id: 'marketplace.install' })}</DialogTitle>
          </DialogHeader>
          <div className="space-y-4">
            <p className="text-sm text-muted-foreground">
              {installTarget &&
                intl.formatMessage(
                  { id: 'marketplace.installTo' },
                  { server: servers.find((s) => s.id === installTarget)?.name ?? installTarget },
                )}
            </p>
            <Field label={intl.formatMessage({ id: 'marketplace.targetAgent' })} htmlFor="marketplace-install-agent">
              <Select value={installAgent} onValueChange={(v) => setInstallAgent(String(v))}>
                <SelectTrigger id="marketplace-install-agent" className="w-full">
                  <SelectValue>
                    {agents.find((a) => a.name === installAgent)?.display_name ||
                      installAgent ||
                      intl.formatMessage({ id: 'common.noData' })}
                  </SelectValue>
                </SelectTrigger>
                <SelectContent>
                  {agents.length === 0 && (
                    <SelectItem value="">{intl.formatMessage({ id: 'common.noData' })}</SelectItem>
                  )}
                  {agents.map((a) => (
                    <SelectItem key={a.name} value={a.name}>
                      {a.display_name || a.name}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </Field>
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => setInstallTarget(null)}>
              {intl.formatMessage({ id: 'common.cancel' })}
            </Button>
            <Button variant="brand" onClick={confirmInstall} disabled={installing || !installAgent}>
              {installing
                ? intl.formatMessage({ id: 'common.saving' })
                : intl.formatMessage({ id: 'marketplace.install' })}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
