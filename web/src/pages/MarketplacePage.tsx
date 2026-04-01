import { useState, useMemo } from 'react';
import { useIntl } from 'react-intl';
import { cn } from '@/lib/utils';
import { api } from '@/lib/api';
import {
  Search,
  Star,
  Download,
  Check,
  Calendar,
  ShoppingCart,
  FileText,
  MessageSquare,
  Database,
  Zap,
  BarChart3,
  CloudSun,
  Sparkles,
} from 'lucide-react';

type Category = 'all' | 'messaging' | 'data' | 'ai' | 'automation' | 'business';

interface MockServer {
  readonly id: string;
  readonly name: string;
  readonly author: string;
  readonly stars: number;
  readonly description: string;
  readonly tags: readonly string[];
  readonly price_cents: number;
  readonly icon: React.ComponentType<{ className?: string }>;
  readonly category: Category;
  readonly featured: boolean;
}

const MOCK_SERVERS: ReadonlyArray<MockServer> = [
  {
    id: 'google-calendar-sync',
    name: 'Google Calendar Sync',
    author: 'duduclaw',
    stars: 342,
    description: 'Sync events, create meetings, and manage calendars directly from your agent.',
    tags: ['calendar', 'google', 'productivity'],
    price_cents: 0,
    icon: Calendar,
    category: 'automation',
    featured: true,
  },
  {
    id: 'shopify-orders',
    name: 'Shopify Orders',
    author: 'ecommerce-tools',
    stars: 218,
    description: 'Manage Shopify orders, inventory, and customer data through MCP tools.',
    tags: ['shopify', 'e-commerce', 'orders'],
    price_cents: 999,
    icon: ShoppingCart,
    category: 'business',
    featured: true,
  },
  {
    id: 'notion-database',
    name: 'Notion Database',
    author: 'notion-labs',
    stars: 567,
    description: 'Read, write, and query Notion databases. Create pages and manage properties.',
    tags: ['notion', 'database', 'wiki'],
    price_cents: 0,
    icon: Database,
    category: 'data',
    featured: true,
  },
  {
    id: 'slack-advanced',
    name: 'Slack Advanced',
    author: 'slack-community',
    stars: 189,
    description: 'Advanced Slack integration with thread management, reactions, and workflow triggers.',
    tags: ['slack', 'messaging', 'workflow'],
    price_cents: 499,
    icon: MessageSquare,
    category: 'messaging',
    featured: false,
  },
  {
    id: 'weather-api',
    name: 'Weather API',
    author: 'open-weather',
    stars: 423,
    description: 'Get current weather, forecasts, and historical data for any location worldwide.',
    tags: ['weather', 'api', 'location'],
    price_cents: 0,
    icon: CloudSun,
    category: 'data',
    featured: false,
  },
  {
    id: 'pdf-generator',
    name: 'PDF Generator',
    author: 'doc-tools',
    stars: 156,
    description: 'Generate professional PDF reports, invoices, and documents from templates.',
    tags: ['pdf', 'document', 'template'],
    price_cents: 1499,
    icon: FileText,
    category: 'automation',
    featured: false,
  },
  {
    id: 'line-rich-menu',
    name: 'LINE Rich Menu Builder',
    author: 'line-tw',
    stars: 97,
    description: 'Design and deploy LINE rich menus with visual editor and A/B testing.',
    tags: ['line', 'menu', 'messaging'],
    price_cents: 799,
    icon: Zap,
    category: 'messaging',
    featured: false,
  },
  {
    id: 'odoo-advanced-reports',
    name: 'Odoo Advanced Reports',
    author: 'erp-studio',
    stars: 64,
    description: 'Generate custom Odoo reports with charts, pivot tables, and scheduled exports.',
    tags: ['odoo', 'erp', 'reports'],
    price_cents: 1999,
    icon: BarChart3,
    category: 'business',
    featured: false,
  },
];

const CATEGORIES: ReadonlyArray<Category> = [
  'all',
  'messaging',
  'data',
  'ai',
  'automation',
  'business',
];

function ServerCard({
  server,
  installed,
  onInstall,
}: {
  readonly server: MockServer;
  readonly installed: boolean;
  readonly onInstall: () => void;
}) {
  const intl = useIntl();
  const Icon = server.icon;

  return (
    <div className="rounded-xl border border-stone-200 bg-white p-5 transition-shadow hover:shadow-md dark:border-stone-800 dark:bg-stone-900">
      <div className="flex items-start gap-3">
        <div className="rounded-lg bg-amber-100 p-2.5 dark:bg-amber-900/30">
          <Icon className="h-5 w-5 text-amber-600 dark:text-amber-400" />
        </div>
        <div className="flex-1 min-w-0">
          <h4 className="font-medium text-stone-900 dark:text-stone-50 truncate">
            {server.name}
          </h4>
          <div className="flex items-center gap-2 mt-0.5">
            <span className="text-xs text-stone-500 dark:text-stone-400">
              {server.author}
            </span>
            <span className="flex items-center gap-0.5 text-xs text-amber-600 dark:text-amber-400">
              <Star className="h-3 w-3 fill-current" />
              {server.stars}
            </span>
          </div>
        </div>
      </div>

      <p className="mt-3 text-sm text-stone-600 dark:text-stone-400 line-clamp-2">
        {server.description}
      </p>

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

      <div className="mt-4 flex items-center justify-between">
        <span className="text-sm font-semibold text-stone-900 dark:text-stone-50">
          {server.price_cents === 0
            ? intl.formatMessage({ id: 'marketplace.free' })
            : `$${(server.price_cents / 100).toFixed(2)}${intl.formatMessage({ id: 'marketplace.perMonth' })}`}
        </span>
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

  const featured = MOCK_SERVERS.filter((s) => s.featured);

  const filtered = useMemo(() => {
    const q = query.toLowerCase();
    return MOCK_SERVERS.filter((s) => {
      if (category !== 'all' && s.category !== category) return false;
      if (q) {
        return (
          s.name.toLowerCase().includes(q) ||
          s.description.toLowerCase().includes(q) ||
          s.tags.some((t) => t.toLowerCase().includes(q))
        );
      }
      return true;
    });
  }, [query, category]);

  const handleInstall = async (serverId: string) => {
    try {
      await api.marketplace.install(serverId);
      setInstalledIds((prev) => new Set([...prev, serverId]));
    } catch {
      // Installation failed — UI stays unchanged
    }
  };

  return (
    <div className="space-y-6">
      <h2 className="text-2xl font-semibold text-stone-900 dark:text-stone-50">
        {intl.formatMessage({ id: 'marketplace.title' })}
      </h2>

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

      {/* Featured Section */}
      {category === 'all' && !query && (
        <div>
          <div className="flex items-center gap-2 mb-4">
            <Sparkles className="h-5 w-5 text-amber-500" />
            <h3 className="text-lg font-medium text-stone-900 dark:text-stone-50">
              {intl.formatMessage({ id: 'marketplace.featured' })}
            </h3>
          </div>
          <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
            {featured.map((server) => (
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
            {intl.formatMessage({ id: 'common.noData' })}
          </p>
        )}
      </div>
    </div>
  );
}
