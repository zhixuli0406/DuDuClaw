import { useState, useCallback, useEffect, useMemo, useRef } from 'react';
import { useIntl } from 'react-intl';
import { cn } from '@/lib/utils';
import {
  api,
  type WikiPageMeta,
  type WikiSearchHit,
  type WikiLintReport,
  type WikiStats,
} from '@/lib/api';
import {
  BookOpen,
  Search,
  FolderTree,
  FileText,
  AlertTriangle,
  CheckCircle2,
  Clock,
  Tag,
  Link2,
  BarChart3,
  RefreshCw,
  ChevronRight,
  ChevronDown,
  Share2,
} from 'lucide-react';
import { WikiGraph } from '@/components/WikiGraph';

type TabId = 'browse' | 'search' | 'graph' | 'health';

export function KnowledgeHubPage() {
  const intl = useIntl();
  const [activeTab, setActiveTab] = useState<TabId>('browse');
  const [agents, setAgents] = useState<ReadonlyArray<{ name: string; display_name: string }>>([]);
  const [selectedAgent, setSelectedAgent] = useState('');

  useEffect(() => {
    api.agents.list().then((res) => {
      const list = res?.agents ?? [];
      setAgents(list);
      if (list.length > 0 && !selectedAgent) {
        setSelectedAgent(list[0].name);
      }
    }).catch(() => {});
  }, []);

  const tabs: ReadonlyArray<{ id: TabId; label: string }> = [
    { id: 'browse', label: intl.formatMessage({ id: 'wiki.tab.browse' }) },
    { id: 'search', label: intl.formatMessage({ id: 'wiki.tab.search' }) },
    { id: 'graph', label: intl.formatMessage({ id: 'wiki.tab.graph' }) },
    { id: 'health', label: intl.formatMessage({ id: 'wiki.tab.health' }) },
  ];

  const selectStyle = 'rounded-lg border border-stone-200 bg-white px-3 py-2.5 text-sm text-stone-900 focus:border-amber-500 focus:outline-none dark:border-stone-700 dark:bg-stone-800 dark:text-stone-50';

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <h2 className="text-2xl font-semibold text-stone-900 dark:text-stone-50">
          {intl.formatMessage({ id: 'wiki.title' })}
        </h2>
        <select
          value={selectedAgent}
          onChange={(e) => setSelectedAgent(e.target.value)}
          className={selectStyle}
        >
          {agents.map((a) => (
            <option key={a.name} value={a.name}>{a.display_name || a.name}</option>
          ))}
        </select>
      </div>

      {/* Tabs — WAI-ARIA Tabs Pattern */}
      <div role="tablist" className="flex gap-1 rounded-lg bg-stone-100 p-1 dark:bg-stone-800">
        {tabs.map((tab) => (
          <button
            key={tab.id}
            role="tab"
            aria-selected={activeTab === tab.id}
            aria-controls={`wiki-panel-${tab.id}`}
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

      <div role="tabpanel" id={`wiki-panel-${activeTab}`}>
        {selectedAgent && activeTab === 'browse' && <BrowseTab agentId={selectedAgent} />}
        {selectedAgent && activeTab === 'search' && <SearchTab agentId={selectedAgent} />}
        {selectedAgent && activeTab === 'graph' && <GraphTab agentId={selectedAgent} />}
        {selectedAgent && activeTab === 'health' && <HealthTab agentId={selectedAgent} />}
      </div>
    </div>
  );
}

// ── Browse Tab ──────────────────────────────────────────────

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
        // Deduplicate: skip if this path already exists as a leaf
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

  // Sort: dirs first, then files
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

function BrowseTab({ agentId }: { agentId: string }) {
  const intl = useIntl();
  const [pages, setPages] = useState<ReadonlyArray<WikiPageMeta>>([]);
  const [wikiExists, setWikiExists] = useState(false);
  const [loading, setLoading] = useState(false);
  const [selectedPath, setSelectedPath] = useState('');
  const [pageContent, setPageContent] = useState('');

  useEffect(() => {
    setLoading(true);
    setSelectedPath('');
    setPageContent('');
    api.wiki.pages(agentId).then((res) => {
      setPages(res?.pages ?? []);
      setWikiExists(res?.exists ?? false);
    }).catch(() => {
      setPages([]);
      setWikiExists(false);
    }).finally(() => setLoading(false));
  }, [agentId]);

  // Use ref to track latest selection, preventing stale response from overwriting
  const latestPathRef = useRef('');
  const handleSelect = useCallback(async (path: string) => {
    latestPathRef.current = path;
    setSelectedPath(path);
    try {
      const res = await api.wiki.read(agentId, path);
      // Only update if this is still the selected page
      if (latestPathRef.current === path) {
        setPageContent(res?.content ?? '');
      }
    } catch {
      if (latestPathRef.current === path) {
        setPageContent('Failed to load page.');
      }
    }
  }, [agentId]);

  const tree = useMemo(() => buildTree(pages), [pages]);

  if (!wikiExists && !loading) {
    return (
      <div className="flex flex-col items-center justify-center rounded-xl border border-dashed border-stone-300 bg-white py-16 dark:border-stone-700 dark:bg-stone-900">
        <BookOpen className="mb-4 h-12 w-12 text-stone-300 dark:text-stone-600" />
        <p className="text-stone-500 dark:text-stone-400">
          {intl.formatMessage({ id: 'wiki.empty' })}
        </p>
      </div>
    );
  }

  return (
    <div className="flex gap-4 min-h-[500px]">
      {/* Sidebar: tree */}
      <div className="w-72 shrink-0 rounded-xl border border-stone-200 bg-white p-4 dark:border-stone-800 dark:bg-stone-900 overflow-y-auto max-h-[calc(100vh-16rem)]">
        <div className="mb-3 flex items-center gap-2 text-sm font-medium text-stone-700 dark:text-stone-300">
          <FolderTree className="h-4 w-4" />
          {intl.formatMessage({ id: 'wiki.pages' })} ({pages.length})
        </div>
        {tree.map((node) => (
          <TreeNodeItem
            key={node.path}
            node={node}
            depth={0}
            selectedPath={selectedPath}
            onSelect={handleSelect}
          />
        ))}
      </div>

      {/* Content */}
      <div className="flex-1 rounded-xl border border-stone-200 bg-white p-6 dark:border-stone-800 dark:bg-stone-900 overflow-y-auto max-h-[calc(100vh-16rem)]">
        {selectedPath ? (
          <WikiPageView path={selectedPath} content={pageContent} />
        ) : (
          <div className="flex flex-col items-center justify-center h-full text-stone-400 dark:text-stone-500">
            <FileText className="mb-3 h-10 w-10" />
            <p>{intl.formatMessage({ id: 'wiki.selectPage' })}</p>
          </div>
        )}
      </div>
    </div>
  );
}

function TreeNodeItem({
  node,
  depth,
  selectedPath,
  onSelect,
}: {
  node: TreeNode;
  depth: number;
  selectedPath: string;
  onSelect: (path: string) => void;
}) {
  const [expanded, setExpanded] = useState(depth === 0);
  const MAX_TREE_DEPTH = 20;

  if (depth > MAX_TREE_DEPTH) {
    return <p className="ml-4 text-xs text-stone-400">(...)</p>;
  }

  if (node.isDir) {
    return (
      <div>
        <button
          onClick={() => setExpanded(!expanded)}
          className="flex w-full items-center gap-1.5 rounded-md px-2 py-1.5 text-sm text-stone-600 hover:bg-stone-100 dark:text-stone-400 dark:hover:bg-stone-800"
          style={{ paddingLeft: `${depth * 12 + 8}px` }}
        >
          {expanded ? (
            <ChevronDown className="h-3.5 w-3.5 shrink-0" />
          ) : (
            <ChevronRight className="h-3.5 w-3.5 shrink-0" />
          )}
          <FolderTree className="h-3.5 w-3.5 shrink-0 text-amber-500" />
          <span className="truncate font-medium">{node.name}</span>
        </button>
        {expanded && (
          <div>
            {node.children.map((child) => (
              <TreeNodeItem
                key={child.path}
                node={child}
                depth={depth + 1}
                selectedPath={selectedPath}
                onSelect={onSelect}
              />
            ))}
          </div>
        )}
      </div>
    );
  }

  const isSelected = node.path === selectedPath;
  return (
    <button
      onClick={() => onSelect(node.path)}
      className={cn(
        'flex w-full items-center gap-1.5 rounded-md px-2 py-1.5 text-sm transition-colors',
        isSelected
          ? 'bg-amber-100 text-amber-700 dark:bg-amber-900/30 dark:text-amber-400'
          : 'text-stone-600 hover:bg-stone-100 dark:text-stone-400 dark:hover:bg-stone-800'
      )}
      style={{ paddingLeft: `${depth * 12 + 8}px` }}
    >
      <FileText className="h-3.5 w-3.5 shrink-0" />
      <span className="truncate">{node.page?.title ?? node.name}</span>
    </button>
  );
}

function WikiPageView({ path, content }: { path: string; content: string }) {
  // Simple markdown-ish rendering: split frontmatter + body
  const { frontmatter, body } = useMemo(() => {
    const trimmed = content.trim();
    if (!trimmed.startsWith('---')) return { frontmatter: '', body: trimmed };
    const rest = trimmed.slice(3);
    const end = rest.indexOf('\n---');
    if (end < 0) return { frontmatter: '', body: trimmed };
    return {
      frontmatter: rest.slice(0, end).trim(),
      body: rest.slice(end + 4).trim(),
    };
  }, [content]);

  // Parse frontmatter fields
  const meta = useMemo(() => {
    const result: Record<string, string> = {};
    for (const line of frontmatter.split('\n')) {
      const idx = line.indexOf(':');
      if (idx > 0) {
        const key = line.slice(0, idx).trim();
        const val = line.slice(idx + 1).trim().replace(/^["']|["']$/g, '');
        result[key] = val;
      }
    }
    return result;
  }, [frontmatter]);

  return (
    <div>
      {/* Header */}
      <div className="mb-4 border-b border-stone-200 pb-4 dark:border-stone-700">
        <div className="flex items-start justify-between">
          <h3 className="text-xl font-semibold text-stone-900 dark:text-stone-50">
            {meta.title || path}
          </h3>
          {/* Maturity badge */}
          {meta.maturity && (
            <span className={cn(
              'inline-flex items-center rounded-full px-2.5 py-0.5 text-xs font-medium',
              meta.maturity === 'internalized'
                ? 'bg-emerald-100 text-emerald-700 dark:bg-emerald-900/30 dark:text-emerald-400'
                : meta.maturity === 'validated'
                ? 'bg-blue-100 text-blue-700 dark:bg-blue-900/30 dark:text-blue-400'
                : 'bg-stone-100 text-stone-600 dark:bg-stone-800 dark:text-stone-400'
            )}>
              {meta.maturity}
            </span>
          )}
        </div>
        <div className="mt-2 flex flex-wrap items-center gap-3 text-xs text-stone-500 dark:text-stone-400">
          {meta.updated && (
            <span className="flex items-center gap-1">
              <Clock className="h-3 w-3" />
              {meta.updated}
            </span>
          )}
          {meta.internalized_from && (
            <span className="flex items-center gap-1 text-amber-600 dark:text-amber-400">
              <BookOpen className="h-3 w-3" />
              from: {meta.internalized_from}
            </span>
          )}
          {meta.tags && (
            <span className="flex items-center gap-1">
              <Tag className="h-3 w-3" />
              {meta.tags}
            </span>
          )}
          {meta.related && (
            <span className="flex items-center gap-1">
              <Link2 className="h-3 w-3" />
              {meta.related}
            </span>
          )}
        </div>
      </div>

      {/* Body */}
      <div className="prose prose-stone max-w-none dark:prose-invert prose-sm">
        {body.split('\n').map((line, i) => {
          if (line.startsWith('# ')) return <h1 key={i} className="text-xl font-bold mt-6 mb-2 text-stone-900 dark:text-stone-50">{line.slice(2)}</h1>;
          if (line.startsWith('## ')) return <h2 key={i} className="text-lg font-semibold mt-5 mb-2 text-stone-900 dark:text-stone-50">{line.slice(3)}</h2>;
          if (line.startsWith('### ')) return <h3 key={i} className="text-base font-medium mt-4 mb-1 text-stone-900 dark:text-stone-50">{line.slice(4)}</h3>;
          if (line.startsWith('- ')) return <li key={i} className="ml-4 text-stone-700 dark:text-stone-300">{line.slice(2)}</li>;
          if (line.trim() === '') return <div key={i} className="h-2" />;
          return <p key={i} className="text-stone-700 dark:text-stone-300 leading-relaxed">{line}</p>;
        })}
      </div>
    </div>
  );
}

// ── Graph Tab ──────────────────────────────────────────────

function GraphTab({ agentId }: { agentId: string }) {
  const intl = useIntl();
  const [pages, setPages] = useState<ReadonlyArray<WikiPageMeta>>([]);
  const [pageContents, setPageContents] = useState<Record<string, string>>({});
  const [loading, setLoading] = useState(false);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);

    api.wiki.pages(agentId).then(async (res) => {
      if (cancelled) return;
      const pageList = res?.pages ?? [];
      setPages(pageList);

      // Fetch content in batches of 10 to avoid overwhelming the backend
      const contents: Record<string, string> = {};
      const BATCH_SIZE = 10;
      for (let i = 0; i < pageList.length; i += BATCH_SIZE) {
        if (cancelled) return;
        const batch = pageList.slice(i, i + BATCH_SIZE);
        const fetches = batch.map(async (p) => {
          try {
            const r = await api.wiki.read(agentId, p.path);
            contents[p.path] = r?.content ?? '';
          } catch {
            // skip failed reads
          }
        });
        await Promise.all(fetches);
      }
      if (!cancelled) {
        setPageContents({ ...contents });
      }
    }).catch(() => {
      if (!cancelled) setPages([]);
    }).finally(() => {
      if (!cancelled) setLoading(false);
    });

    return () => { cancelled = true; };
  }, [agentId]);

  if (!loading && pages.length === 0) {
    return (
      <div className="flex flex-col items-center justify-center rounded-xl border border-dashed border-stone-300 bg-white py-16 dark:border-stone-700 dark:bg-stone-900">
        <Share2 className="mb-4 h-12 w-12 text-stone-300 dark:text-stone-600" />
        <p className="text-stone-500 dark:text-stone-400">
          {intl.formatMessage({ id: 'wiki.empty' })}
        </p>
      </div>
    );
  }

  return (
    <WikiGraph
      pages={pages}
      pageContents={pageContents}
      width={900}
      height={550}
    />
  );
}

// ── Search Tab ──────────────────────────────────────────────

function SearchTab({ agentId }: { agentId: string }) {
  const intl = useIntl();
  const [query, setQuery] = useState('');
  const [hits, setHits] = useState<ReadonlyArray<WikiSearchHit>>([]);
  const [loading, setLoading] = useState(false);

  const handleSearch = useCallback(async () => {
    if (!query.trim()) return;
    setLoading(true);
    try {
      const res = await api.wiki.search(agentId, query);
      setHits(res?.hits ?? []);
    } catch {
      setHits([]);
    } finally {
      setLoading(false);
    }
  }, [agentId, query]);

  return (
    <div className="space-y-4">
      <div className="flex gap-2">
        <div className="relative flex-1">
          <Search className="absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-stone-400" />
          <input
            type="text"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => e.key === 'Enter' && handleSearch()}
            placeholder={intl.formatMessage({ id: 'wiki.search.placeholder' })}
            className="w-full rounded-lg border border-stone-200 bg-white py-2.5 pl-10 pr-4 text-sm text-stone-900 placeholder:text-stone-400 focus:border-amber-500 focus:outline-none focus:ring-1 focus:ring-amber-500 dark:border-stone-700 dark:bg-stone-800 dark:text-stone-50 dark:placeholder:text-stone-500"
          />
        </div>
        <button
          onClick={handleSearch}
          disabled={loading || !query.trim()}
          className="rounded-lg bg-amber-500 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-amber-600 disabled:opacity-50"
        >
          <Search className="h-4 w-4" />
        </button>
      </div>

      {hits.length === 0 ? (
        <div className="flex flex-col items-center justify-center rounded-xl border border-dashed border-stone-300 bg-white py-16 dark:border-stone-700 dark:bg-stone-900">
          <Search className="mb-4 h-12 w-12 text-stone-300 dark:text-stone-600" />
          <p className="text-stone-500 dark:text-stone-400">
            {intl.formatMessage({ id: 'wiki.search.empty' })}
          </p>
        </div>
      ) : (
        <div className="space-y-3">
          {hits.map((hit) => (
            <div
              key={hit.path}
              className="rounded-xl border border-stone-200 bg-white p-5 dark:border-stone-800 dark:bg-stone-900"
            >
              <div className="flex items-center justify-between mb-2">
                <h3 className="font-medium text-stone-900 dark:text-stone-50">
                  {hit.title}
                </h3>
                <span className="text-xs text-amber-600 dark:text-amber-400 font-medium">
                  {intl.formatMessage({ id: 'wiki.relevance' })}: {Number(hit.score).toFixed(1)}
                </span>
              </div>
              <p className="text-xs text-stone-500 dark:text-stone-400 mb-2">{hit.path}</p>
              {hit.context_lines.length > 0 && (
                <div className="rounded-lg bg-stone-50 p-3 dark:bg-stone-800">
                  {hit.context_lines.map((line, i) => (
                    <p key={i} className="text-xs text-stone-600 dark:text-stone-400 font-mono">
                      {line}
                    </p>
                  ))}
                </div>
              )}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

// ── Health Tab ──────────────────────────────────────────────

function HealthTab({ agentId }: { agentId: string }) {
  const intl = useIntl();
  const [stats, setStats] = useState<WikiStats | null>(null);
  const [lint, setLint] = useState<WikiLintReport | null>(null);
  const [loading, setLoading] = useState(false);

  const fetchData = useCallback(async () => {
    setLoading(true);
    try {
      const [statsRes, lintRes] = await Promise.all([
        api.wiki.stats(agentId),
        api.wiki.lint(agentId),
      ]);
      setStats(statsRes);
      setLint(lintRes);
    } catch {
      // handled by empty state
    } finally {
      setLoading(false);
    }
  }, [agentId]);

  useEffect(() => {
    fetchData();
  }, [fetchData]);

  if (!stats?.exists && !loading) {
    return (
      <div className="flex flex-col items-center justify-center rounded-xl border border-dashed border-stone-300 bg-white py-16 dark:border-stone-700 dark:bg-stone-900">
        <BookOpen className="mb-4 h-12 w-12 text-stone-300 dark:text-stone-600" />
        <p className="text-stone-500 dark:text-stone-400">
          {intl.formatMessage({ id: 'wiki.empty' })}
        </p>
      </div>
    );
  }

  return (
    <div className="space-y-4">
      {/* Stats overview */}
      <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-4">
        <StatCard
          label={intl.formatMessage({ id: 'wiki.stats.totalPages' })}
          value={stats?.total_pages ?? 0}
          icon={<FileText className="h-5 w-5 text-amber-600 dark:text-amber-400" />}
        />
        <StatCard
          label={intl.formatMessage({ id: 'wiki.stats.directories' })}
          value={Object.keys(stats?.by_directory ?? {}).length}
          icon={<FolderTree className="h-5 w-5 text-amber-600 dark:text-amber-400" />}
        />
        <StatCard
          label={intl.formatMessage({ id: 'wiki.stats.orphans' })}
          value={lint?.orphan_pages.length ?? 0}
          icon={<AlertTriangle className={cn('h-5 w-5', (lint?.orphan_pages.length ?? 0) > 0 ? 'text-amber-500' : 'text-emerald-500')} />}
        />
        <StatCard
          label={intl.formatMessage({ id: 'wiki.stats.health' })}
          value={lint?.healthy ? intl.formatMessage({ id: 'wiki.healthy' }) : intl.formatMessage({ id: 'wiki.unhealthy' })}
          icon={lint?.healthy
            ? <CheckCircle2 className="h-5 w-5 text-emerald-500" />
            : <AlertTriangle className="h-5 w-5 text-rose-500" />}
        />
      </div>

      {/* Refresh button */}
      <div className="flex justify-end">
        <button
          onClick={fetchData}
          disabled={loading}
          className="flex items-center gap-2 rounded-lg bg-stone-100 px-3 py-2 text-sm text-stone-700 transition-colors hover:bg-stone-200 disabled:opacity-50 dark:bg-stone-800 dark:text-stone-300 dark:hover:bg-stone-700"
        >
          <RefreshCw className={cn('h-4 w-4', loading && 'animate-spin')} />
          {intl.formatMessage({ id: 'wiki.lint.refresh' })}
        </button>
      </div>

      {/* Directory breakdown */}
      {stats?.by_directory && Object.keys(stats.by_directory).length > 0 && (
        <div className="rounded-xl border border-stone-200 bg-white p-5 dark:border-stone-800 dark:bg-stone-900">
          <h3 className="mb-3 flex items-center gap-2 font-medium text-stone-900 dark:text-stone-50">
            <BarChart3 className="h-4 w-4" />
            {intl.formatMessage({ id: 'wiki.stats.byDirectory' })}
          </h3>
          <div className="space-y-2">
            {Object.entries(stats.by_directory)
              .sort(([, a], [, b]) => b - a)
              .map(([dir, count]) => (
                <div key={dir} className="flex items-center justify-between">
                  <span className="text-sm text-stone-600 dark:text-stone-400">{dir}/</span>
                  <div className="flex items-center gap-2">
                    <div className="h-2 rounded-full bg-amber-500" style={{ width: `${Math.max(20, (count / stats.total_pages) * 200)}px` }} />
                    <span className="text-sm font-medium text-stone-700 dark:text-stone-300">{count}</span>
                  </div>
                </div>
              ))}
          </div>
        </div>
      )}

      {/* Lint issues */}
      {lint && !lint.healthy && (
        <div className="rounded-xl border border-amber-200 bg-amber-50 p-5 dark:border-amber-900/30 dark:bg-amber-950/20">
          <h3 className="mb-3 flex items-center gap-2 font-medium text-amber-800 dark:text-amber-300">
            <AlertTriangle className="h-4 w-4" />
            {intl.formatMessage({ id: 'wiki.lint.issues' })}
          </h3>

          {lint.orphan_pages.length > 0 && (
            <div className="mb-3">
              <p className="text-sm font-medium text-amber-700 dark:text-amber-400 mb-1">
                {intl.formatMessage({ id: 'wiki.lint.orphans' })} ({lint.orphan_pages.length})
              </p>
              {lint.orphan_pages.map((p) => (
                <p key={p} className="text-xs text-amber-600 dark:text-amber-500 ml-4">- {p}</p>
              ))}
            </div>
          )}

          {lint.broken_links.length > 0 && (
            <div className="mb-3">
              <p className="text-sm font-medium text-amber-700 dark:text-amber-400 mb-1">
                {intl.formatMessage({ id: 'wiki.lint.brokenLinks' })} ({lint.broken_links.length})
              </p>
              {lint.broken_links.map(([from, to], i) => (
                <p key={i} className="text-xs text-amber-600 dark:text-amber-500 ml-4">
                  {from} → {to}
                </p>
              ))}
            </div>
          )}

          {lint.stale_pages.length > 0 && (
            <div>
              <p className="text-sm font-medium text-amber-700 dark:text-amber-400 mb-1">
                {intl.formatMessage({ id: 'wiki.lint.stale' })} ({lint.stale_pages.length})
              </p>
              {lint.stale_pages.map((p) => (
                <p key={p} className="text-xs text-amber-600 dark:text-amber-500 ml-4">- {p}</p>
              ))}
            </div>
          )}
        </div>
      )}
    </div>
  );
}

function StatCard({
  label,
  value,
  icon,
}: {
  label: string;
  value: string | number;
  icon: React.ReactNode;
}) {
  return (
    <div className="rounded-xl border border-stone-200 bg-white p-4 dark:border-stone-800 dark:bg-stone-900">
      <div className="flex items-center gap-3">
        <div className="rounded-lg bg-amber-100 p-2 dark:bg-amber-900/30">
          {icon}
        </div>
        <div>
          <p className="text-2xl font-semibold text-stone-900 dark:text-stone-50">{value}</p>
          <p className="text-xs text-stone-500 dark:text-stone-400">{label}</p>
        </div>
      </div>
    </div>
  );
}
