import { useState, useCallback, useEffect, useMemo } from 'react';
import { useIntl } from 'react-intl';
import { cn } from '@/lib/utils';
import {
  api,
  type WikiPageMeta,
  type WikiSearchHit,
  type SharedWikiStats,
} from '@/lib/api';
import {
  BookOpen,
  Search,
  FileText,
  Clock,
  Tag,
  BarChart3,
  ChevronRight,
  ChevronDown,
  Users,
} from 'lucide-react';

type TabId = 'browse' | 'search' | 'stats';

export function SharedWikiPage() {
  const intl = useIntl();
  const [activeTab, setActiveTab] = useState<TabId>('browse');

  const tabs: ReadonlyArray<{ id: TabId; label: string }> = [
    { id: 'browse', label: intl.formatMessage({ id: 'sharedWiki.tab.browse' }) },
    { id: 'search', label: intl.formatMessage({ id: 'sharedWiki.tab.search' }) },
    { id: 'stats', label: intl.formatMessage({ id: 'sharedWiki.tab.stats' }) },
  ];

  return (
    <div className="space-y-6">
      <div>
        <h2 className="text-2xl font-semibold text-stone-900 dark:text-stone-50">
          {intl.formatMessage({ id: 'sharedWiki.title' })}
        </h2>
        <p className="mt-1 text-sm text-stone-500 dark:text-stone-400">
          {intl.formatMessage({ id: 'sharedWiki.subtitle' })}
        </p>
      </div>

      <div role="tablist" className="flex gap-1 rounded-lg bg-stone-100 p-1 dark:bg-stone-800">
        {tabs.map((tab) => (
          <button
            key={tab.id}
            role="tab"
            aria-selected={activeTab === tab.id}
            aria-controls={`shared-wiki-panel-${tab.id}`}
            onClick={() => setActiveTab(tab.id)}
            className={cn(
              'flex-1 rounded-md px-4 py-2 text-sm font-medium transition-colors',
              activeTab === tab.id
                ? 'bg-white text-stone-900 shadow-sm dark:bg-stone-700 dark:text-stone-50'
                : 'text-stone-500 hover:text-stone-700 dark:text-stone-400 dark:hover:text-stone-300'
            )}
          >
            {tab.label}
          </button>
        ))}
      </div>

      <div role="tabpanel" id={`shared-wiki-panel-${activeTab}`}>
        {activeTab === 'browse' && <BrowseTab />}
        {activeTab === 'search' && <SearchTab />}
        {activeTab === 'stats' && <StatsTab />}
      </div>
    </div>
  );
}

// ── Tree helpers ───────────────────────────────────────────

interface TreeNode {
  name: string;
  path: string;
  isDir: boolean;
  children: TreeNode[];
  page?: WikiPageMeta;
}

function buildTree(pages: ReadonlyArray<WikiPageMeta>): TreeNode[] {
  const root: TreeNode = { name: '', path: '', isDir: true, children: [] };

  for (const page of pages) {
    const parts = page.path.split('/');
    let current = root;

    for (let i = 0; i < parts.length; i++) {
      const part = parts[i];
      const isLast = i === parts.length - 1;

      if (isLast) {
        if (current.children.some((c) => !c.isDir && c.path === page.path)) continue;
        current.children.push({
          name: part,
          path: page.path,
          isDir: false,
          children: [],
          page,
        });
      } else {
        let child = current.children.find((c) => c.isDir && c.name === part);
        if (!child) {
          child = { name: part, path: parts.slice(0, i + 1).join('/'), isDir: true, children: [] };
          current.children.push(child);
        }
        current = child;
      }
    }
  }

  const sortNodes = (nodes: TreeNode[]) => {
    nodes.sort((a, b) => {
      if (a.isDir !== b.isDir) return a.isDir ? -1 : 1;
      return a.name.localeCompare(b.name);
    });
    for (const n of nodes) {
      if (n.isDir) sortNodes(n.children);
    }
  };
  sortNodes(root.children);

  return root.children;
}

// ── Browse Tab ────────────────────────────────────────────

function BrowseTab() {
  const intl = useIntl();
  const [pages, setPages] = useState<ReadonlyArray<WikiPageMeta>>([]);
  const [wikiExists, setWikiExists] = useState(false);
  const [loading, setLoading] = useState(false);
  const [selectedPath, setSelectedPath] = useState('');
  const [pageContent, setPageContent] = useState('');

  useEffect(() => {
    setLoading(true);
    api.sharedWiki.pages().then((res) => {
      setPages(res?.pages ?? []);
      setWikiExists(res?.exists ?? false);
    }).catch(() => {
      setPages([]);
      setWikiExists(false);
    }).finally(() => setLoading(false));
  }, []);

  const handleSelectPage = useCallback((path: string) => {
    setSelectedPath(path);
    setPageContent('');
    api.sharedWiki.read(path).then((res) => {
      setPageContent(res?.content ?? '');
    }).catch(() => setPageContent('Error loading page'));
  }, []);

  const tree = useMemo(() => buildTree(pages), [pages]);

  if (loading) {
    return (
      <div className="flex items-center justify-center py-12 text-stone-400">
        <BookOpen className="mr-2 h-5 w-5 animate-pulse" />
        Loading...
      </div>
    );
  }

  if (!wikiExists || pages.length === 0) {
    return (
      <div className="rounded-xl border border-stone-200 bg-stone-50/50 p-8 text-center dark:border-stone-700 dark:bg-stone-800/50">
        <BookOpen className="mx-auto h-12 w-12 text-stone-300 dark:text-stone-600" />
        <p className="mt-4 text-stone-500 dark:text-stone-400">
          {intl.formatMessage({ id: 'sharedWiki.empty' })}
        </p>
      </div>
    );
  }

  return (
    <div className="grid grid-cols-12 gap-4">
      {/* Tree sidebar */}
      <div className="col-span-4 rounded-xl border border-stone-200 bg-white p-4 dark:border-stone-700 dark:bg-stone-800">
        <h3 className="mb-3 text-sm font-medium text-stone-500 dark:text-stone-400">
          {intl.formatMessage({ id: 'sharedWiki.pages' })} ({pages.length})
        </h3>
        <div className="max-h-[60vh] overflow-y-auto">
          {tree.map((node) => (
            <TreeItem
              key={node.path}
              node={node}
              selectedPath={selectedPath}
              onSelect={handleSelectPage}
              depth={0}
            />
          ))}
        </div>
      </div>

      {/* Content viewer */}
      <div className="col-span-8 rounded-xl border border-stone-200 bg-white p-6 dark:border-stone-700 dark:bg-stone-800">
        {selectedPath ? (
          <div>
            <h3 className="mb-4 text-lg font-semibold text-stone-900 dark:text-stone-50">
              {selectedPath}
            </h3>
            <pre className="max-h-[60vh] overflow-auto whitespace-pre-wrap rounded-lg bg-stone-50 p-4 font-mono text-sm text-stone-800 dark:bg-stone-900 dark:text-stone-200">
              {pageContent || 'Loading...'}
            </pre>
          </div>
        ) : (
          <div className="flex h-full items-center justify-center py-16 text-stone-400">
            <FileText className="mr-2 h-5 w-5" />
            {intl.formatMessage({ id: 'sharedWiki.selectPage' })}
          </div>
        )}
      </div>
    </div>
  );
}

// ── Tree Item ─────────────────────────────────────────────

function TreeItem({
  node,
  selectedPath,
  onSelect,
  depth,
}: {
  node: TreeNode;
  selectedPath: string;
  onSelect: (path: string) => void;
  depth: number;
}) {
  const [expanded, setExpanded] = useState(depth < 1);

  if (node.isDir) {
    return (
      <div>
        <button
          onClick={() => setExpanded((prev) => !prev)}
          className="flex w-full items-center gap-1 rounded px-2 py-1 text-sm text-stone-600 hover:bg-stone-100 dark:text-stone-300 dark:hover:bg-stone-700"
          style={{ paddingLeft: `${depth * 12 + 8}px` }}
        >
          {expanded ? <ChevronDown className="h-3.5 w-3.5" /> : <ChevronRight className="h-3.5 w-3.5" />}
          <span className="font-medium">{node.name}/</span>
        </button>
        {expanded && node.children.map((child) => (
          <TreeItem
            key={child.path}
            node={child}
            selectedPath={selectedPath}
            onSelect={onSelect}
            depth={depth + 1}
          />
        ))}
      </div>
    );
  }

  return (
    <button
      onClick={() => onSelect(node.path)}
      className={cn(
        'flex w-full items-center gap-1.5 rounded px-2 py-1 text-sm transition-colors',
        selectedPath === node.path
          ? 'bg-amber-50 text-amber-700 dark:bg-amber-900/20 dark:text-amber-400'
          : 'text-stone-600 hover:bg-stone-100 dark:text-stone-300 dark:hover:bg-stone-700'
      )}
      style={{ paddingLeft: `${depth * 12 + 8}px` }}
    >
      <FileText className="h-3.5 w-3.5 flex-shrink-0" />
      <span className="truncate">{node.page?.title ?? node.name}</span>
    </button>
  );
}

// ── Search Tab ────────────────────────────────────────────

function SearchTab() {
  const intl = useIntl();
  const [query, setQuery] = useState('');
  const [hits, setHits] = useState<ReadonlyArray<WikiSearchHit>>([]);
  const [searching, setSearching] = useState(false);

  const handleSearch = useCallback(() => {
    if (!query.trim()) return;
    setSearching(true);
    api.sharedWiki.search(query.trim()).then((res) => {
      setHits(res?.hits ?? []);
    }).catch(() => setHits([])).finally(() => setSearching(false));
  }, [query]);

  return (
    <div className="space-y-4">
      <div className="flex gap-2">
        <div className="relative flex-1">
          <Search className="absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-stone-400" />
          <input
            type="text"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => { if (e.key === 'Enter') handleSearch(); }}
            placeholder={intl.formatMessage({ id: 'sharedWiki.search.placeholder' })}
            className="w-full rounded-lg border border-stone-200 bg-white py-2.5 pl-10 pr-4 text-sm focus:border-amber-500 focus:outline-none dark:border-stone-700 dark:bg-stone-800 dark:text-stone-50"
          />
        </div>
        <button
          onClick={handleSearch}
          disabled={searching || !query.trim()}
          className="rounded-lg bg-amber-500 px-4 py-2.5 text-sm font-medium text-white hover:bg-amber-600 disabled:opacity-50"
        >
          <Search className="h-4 w-4" />
        </button>
      </div>

      {hits.length > 0 ? (
        <div className="space-y-3">
          {hits.map((hit) => (
            <div
              key={hit.path}
              className="rounded-xl border border-stone-200 bg-white p-4 dark:border-stone-700 dark:bg-stone-800"
            >
              <div className="flex items-center gap-2">
                <FileText className="h-4 w-4 text-amber-500" />
                <span className="font-medium text-stone-900 dark:text-stone-50">{hit.title}</span>
                <span className="text-xs text-stone-400">({hit.path})</span>
                <span className="ml-auto rounded-full bg-amber-100 px-2 py-0.5 text-xs font-medium text-amber-700 dark:bg-amber-900/30 dark:text-amber-400">
                  {hit.score}
                </span>
              </div>
              {hit.context_lines.length > 0 && (
                <div className="mt-2 rounded-lg bg-stone-50 p-2 text-xs text-stone-600 dark:bg-stone-900 dark:text-stone-400">
                  {hit.context_lines.map((line, i) => (
                    <div key={i} className="truncate">{line}</div>
                  ))}
                </div>
              )}
            </div>
          ))}
        </div>
      ) : (
        <div className="py-12 text-center text-stone-400">
          <Search className="mx-auto h-8 w-8 opacity-30" />
          <p className="mt-2">{intl.formatMessage({ id: 'sharedWiki.search.empty' })}</p>
        </div>
      )}
    </div>
  );
}

// ── Stats Tab ─────────────────────────────────────────────

function StatsTab() {
  const intl = useIntl();
  const [stats, setStats] = useState<SharedWikiStats | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    api.sharedWiki.stats().then((res) => {
      setStats(res);
    }).catch(() => setStats(null)).finally(() => setLoading(false));
  }, []);

  if (loading) {
    return (
      <div className="flex items-center justify-center py-12 text-stone-400">
        <BarChart3 className="mr-2 h-5 w-5 animate-pulse" />
        Loading...
      </div>
    );
  }

  if (!stats?.exists) {
    return (
      <div className="rounded-xl border border-stone-200 bg-stone-50/50 p-8 text-center dark:border-stone-700 dark:bg-stone-800/50">
        <BarChart3 className="mx-auto h-12 w-12 text-stone-300 dark:text-stone-600" />
        <p className="mt-4 text-stone-500">{intl.formatMessage({ id: 'sharedWiki.empty' })}</p>
      </div>
    );
  }

  const authorEntries = Object.entries(stats.by_author ?? {}).sort(([, a], [, b]) => b - a);
  const dirEntries = Object.entries(stats.by_directory ?? {}).sort(([, a], [, b]) => b - a);

  return (
    <div className="space-y-6">
      {/* Summary cards */}
      <div className="grid grid-cols-3 gap-4">
        <div className="rounded-xl border border-stone-200 bg-white p-4 dark:border-stone-700 dark:bg-stone-800">
          <div className="flex items-center gap-2 text-stone-500 dark:text-stone-400">
            <FileText className="h-4 w-4" />
            <span className="text-sm">{intl.formatMessage({ id: 'sharedWiki.stats.totalPages' })}</span>
          </div>
          <p className="mt-1 text-2xl font-semibold text-stone-900 dark:text-stone-50">{stats.total_pages}</p>
        </div>
        <div className="rounded-xl border border-stone-200 bg-white p-4 dark:border-stone-700 dark:bg-stone-800">
          <div className="flex items-center gap-2 text-stone-500 dark:text-stone-400">
            <Users className="h-4 w-4" />
            <span className="text-sm">{intl.formatMessage({ id: 'sharedWiki.stats.contributors' })}</span>
          </div>
          <p className="mt-1 text-2xl font-semibold text-stone-900 dark:text-stone-50">{authorEntries.length}</p>
        </div>
        <div className="rounded-xl border border-stone-200 bg-white p-4 dark:border-stone-700 dark:bg-stone-800">
          <div className="flex items-center gap-2 text-stone-500 dark:text-stone-400">
            <Clock className="h-4 w-4" />
            <span className="text-sm">Last Updated</span>
          </div>
          <p className="mt-1 text-sm font-medium text-stone-900 dark:text-stone-50">
            {stats.most_recent?.updated
              ? new Date(stats.most_recent.updated).toLocaleDateString()
              : '—'}
          </p>
          {stats.most_recent?.title && (
            <p className="mt-0.5 truncate text-xs text-stone-400">{stats.most_recent.title}</p>
          )}
        </div>
      </div>

      {/* By author */}
      {authorEntries.length > 0 && (
        <div className="rounded-xl border border-stone-200 bg-white p-4 dark:border-stone-700 dark:bg-stone-800">
          <h3 className="mb-3 text-sm font-medium text-stone-500 dark:text-stone-400">
            {intl.formatMessage({ id: 'sharedWiki.stats.byAuthor' })}
          </h3>
          <div className="space-y-2">
            {authorEntries.map(([author, count]) => (
              <div key={author} className="flex items-center gap-2">
                <span className="flex-1 text-sm text-stone-700 dark:text-stone-300">{author}</span>
                <div className="h-2 flex-1 overflow-hidden rounded-full bg-stone-100 dark:bg-stone-700">
                  <div
                    className="h-full rounded-full bg-amber-400"
                    style={{ width: `${(count / stats.total_pages) * 100}%` }}
                  />
                </div>
                <span className="min-w-[2rem] text-right text-sm font-medium text-stone-500">{count}</span>
              </div>
            ))}
          </div>
        </div>
      )}

      {/* By directory */}
      {dirEntries.length > 0 && (
        <div className="rounded-xl border border-stone-200 bg-white p-4 dark:border-stone-700 dark:bg-stone-800">
          <h3 className="mb-3 text-sm font-medium text-stone-500 dark:text-stone-400">
            {intl.formatMessage({ id: 'sharedWiki.stats.byDirectory' })}
          </h3>
          <div className="space-y-2">
            {dirEntries.map(([dir, count]) => (
              <div key={dir} className="flex items-center gap-2">
                <Tag className="h-3.5 w-3.5 text-stone-400" />
                <span className="flex-1 text-sm text-stone-700 dark:text-stone-300">{dir}/</span>
                <span className="text-sm font-medium text-stone-500">{count}</span>
              </div>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}
