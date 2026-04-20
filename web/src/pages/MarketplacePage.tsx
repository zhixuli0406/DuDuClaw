import { useEffect, useMemo, useState } from 'react';
import { useIntl } from 'react-intl';
import { cn } from '@/lib/utils';
import { api, type MarketplaceServer } from '@/lib/api';
import {
  Search,
  Download,
  Check,
  MessageSquare,
  Database,
  Globe,
  Package,
  Sparkles,
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
    <div className="rounded-xl border border-stone-200 bg-white p-5 transition-shadow hover:shadow-md dark:border-stone-800 dark:bg-stone-900">
      <div className="flex items-start gap-3">
        <div className="rounded-lg bg-amber-100 p-2.5 dark:bg-amber-900/30">
          <Icon className="h-5 w-5 text-amber-600 dark:text-amber-400" />
        </div>
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2">
            <h4 className="font-medium text-stone-900 dark:text-stone-50 truncate">
              {server.name}
            </h4>
            {server.featured && (
              <span className="inline-flex items-center gap-1 rounded-full bg-amber-100 px-2 py-0.5 text-xs font-medium text-amber-700 dark:bg-amber-900/30 dark:text-amber-300">
                <Sparkles className="h-3 w-3" />
                {intl.formatMessage({ id: 'marketplace.featured' })}
              </span>
            )}
          </div>
          <div className="flex items-center gap-2 mt-0.5">
            <span className="text-xs text-stone-500 dark:text-stone-400">
              {server.author}
            </span>
          </div>
        </div>
      </div>

      <p className="mt-3 text-sm text-stone-600 dark:text-stone-400 line-clamp-2">
        {server.description}
      </p>

      {server.tags.length > 0 && (
        <div className="mt-3 flex flex-wrap gap-1.5">
          {server.tags.map((tag) => (
            <span
              key={tag}
              className="inline-flex rounded-full bg-stone-100 px-2 py-0.5 text-xs text-stone-600 dark:bg-stone-800 dark:text-stone-400"
            >
              {tag}
            </span>
          ))}
        </div>
      )}

      <div className="mt-4 flex items-center justify-end">
        <button
          onClick={onInstall}
          disabled={installed}
          className={cn(
            'inline-flex items-center gap-1.5 rounded-lg px-3.5 py-1.5 text-sm font-medium transition-colors',
            installed
              ? 'bg-emerald-100 text-emerald-700 dark:bg-emerald-900/30 dark:text-emerald-400'
              : 'bg-amber-500 text-white hover:bg-amber-600',
          )}
        >
          {installed ? (
            <>
              <Check className="h-3.5 w-3.5" />
              {intl.formatMessage({ id: 'marketplace.installed' })}
            </>
          ) : (
            <>
              <Download className="h-3.5 w-3.5" />
              {intl.formatMessage({ id: 'marketplace.install' })}
            </>
          )}
        </button>
      </div>
    </div>
  );
}

export function MarketplacePage() {
  const intl = useIntl();
  const [query, setQuery] = useState('');
  const [category, setCategory] = useState<Category>('all');
  const [installedIds, setInstalledIds] = useState<ReadonlySet<string>>(new Set());
  const [installError, setInstallError] = useState<string | null>(null);
  const [servers, setServers] = useState<ReadonlyArray<MarketplaceServer>>([]);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;

    const load = async () => {
      setLoading(true);
      setLoadError(null);
      try {
        const res = await api.marketplace.list();
        if (!cancelled) {
          setServers(res.servers ?? []);
        }
      } catch (err) {
        if (!cancelled) {
          const message = err instanceof Error ? err.message : String(err);
          setLoadError(message);
        }
      } finally {
        if (!cancelled) {
          setLoading(false);
        }
      }
    };

    load();
    return () => {
      cancelled = true;
    };
  }, []);

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

  const handleInstall = async (serverId: string) => {
    setInstallError(null);
    try {
      await api.marketplace.install(serverId);
      setInstalledIds((prev) => new Set([...prev, serverId]));
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      setInstallError(
        intl.formatMessage({ id: 'marketplace.installError' }, { message }),
      );
    }
  };

  return (
    <div className="space-y-6">
      <h2 className="text-2xl font-semibold text-stone-900 dark:text-stone-50">
        {intl.formatMessage({ id: 'marketplace.title' })}
      </h2>

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
            <svg
              xmlns="http://www.w3.org/2000/svg"
              width="16"
              height="16"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth="2"
              strokeLinecap="round"
              strokeLinejoin="round"
            >
              <line x1="18" y1="6" x2="6" y2="18"></line>
              <line x1="6" y1="6" x2="18" y2="18"></line>
            </svg>
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

      {/* Search Bar */}
      <div className="relative">
        <Search className="absolute left-3.5 top-1/2 h-4 w-4 -translate-y-1/2 text-stone-400" />
        <input
          type="text"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder={intl.formatMessage({ id: 'marketplace.search' })}
          className="w-full rounded-xl border border-stone-300 bg-white py-2.5 pl-10 pr-4 text-sm text-stone-900 placeholder:text-stone-400 focus:border-amber-500 focus:outline-none dark:border-stone-600 dark:bg-stone-800 dark:text-stone-50 dark:placeholder:text-stone-500"
        />
      </div>

      {/* Category Tabs */}
      <div className="flex gap-1 rounded-lg border border-stone-200 bg-stone-100 p-1 dark:border-stone-700 dark:bg-stone-800">
        {CATEGORIES.map((cat) => (
          <button
            key={cat}
            onClick={() => setCategory(cat)}
            className={cn(
              'rounded-md px-4 py-1.5 text-sm font-medium transition-colors',
              category === cat
                ? 'bg-amber-500 text-white shadow-sm'
                : 'text-stone-600 hover:text-stone-900 dark:text-stone-400 dark:hover:text-stone-200',
            )}
          >
            {intl.formatMessage({ id: `marketplace.categories.${cat}` })}
          </button>
        ))}
      </div>

      {/* Loading */}
      {loading && (
        <p className="py-12 text-center text-stone-400 dark:text-stone-500">
          {intl.formatMessage({ id: 'common.loading' })}
        </p>
      )}

      {/* Featured Section (only on `all` tab with no search) */}
      {!loading && category === 'all' && !query && featuredServers.length > 0 && (
        <div>
          <div className="flex items-center gap-2 mb-4">
            <Sparkles className="h-5 w-5 text-amber-500" />
            <h3 className="text-lg font-medium text-stone-900 dark:text-stone-50">
              {intl.formatMessage({ id: 'marketplace.featured' })}
            </h3>
          </div>
          <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
            {featuredServers.map((server) => (
              <ServerCard
                key={server.id}
                server={server}
                installed={installedIds.has(server.id)}
                onInstall={() => handleInstall(server.id)}
              />
            ))}
          </div>
        </div>
      )}

      {/* All Servers Grid */}
      {!loading && (
        <div>
          {(category !== 'all' || query) && (
            <h3 className="mb-4 text-lg font-medium text-stone-900 dark:text-stone-50">
              {intl.formatMessage({ id: `marketplace.categories.${category}` })}
            </h3>
          )}
          <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
            {filtered.map((server) => (
              <ServerCard
                key={server.id}
                server={server}
                installed={installedIds.has(server.id)}
                onInstall={() => handleInstall(server.id)}
              />
            ))}
          </div>
          {filtered.length === 0 && (
            <p className="py-12 text-center text-stone-400 dark:text-stone-500">
              {servers.length === 0
                ? intl.formatMessage({ id: 'marketplace.empty' })
                : intl.formatMessage({ id: 'common.noData' })}
            </p>
          )}
        </div>
      )}
    </div>
  );
}
