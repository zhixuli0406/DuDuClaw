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
import { toast, formatError } from '@/lib/toast';
import {
  Page,
  PageHeader,
  Card,
  Section,
  StatCard,
  Button,
  Badge,
  EmptyState,
  Toolbar,
  Tabs,
  controlClass,
  type TabItem,
} from '@/components/ui';

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
    }).catch((e) => {
      console.warn("[api]", e);
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
    });
    // Run once on mount; selectedAgent seeding doesn't warrant re-fetching.
  }, []);

  const tabs: TabItem[] = [
    { id: 'browse', label: intl.formatMessage({ id: 'wiki.tab.browse' }), icon: FolderTree },
    { id: 'search', label: intl.formatMessage({ id: 'wiki.tab.search' }), icon: Search },
    { id: 'graph', label: intl.formatMessage({ id: 'wiki.tab.graph' }), icon: Share2 },
    { id: 'health', label: intl.formatMessage({ id: 'wiki.tab.health' }), icon: BarChart3 },
  ];

  return (
    <Page wide>
      <PageHeader
        icon={BookOpen}
        title={intl.formatMessage({ id: 'nav.wiki' })}
        subtitle={intl.formatMessage({ id: 'app.subtitle' })}
        actions={
          <select
            value={selectedAgent}
            onChange={(e) => setSelectedAgent(e.target.value)}
            className={cn(controlClass, 'w-auto')}
          >
            {agents.map((a) => (
              <option key={a.name} value={a.name}>{a.display_name || a.name}</option>
            ))}
          </select>
        }
      />

      <Tabs
        items={tabs}
        value={activeTab}
        onChange={(id) => setActiveTab(id as TabId)}
      />

      <div role="tabpanel" id={`wiki-panel-${activeTab}`}>
        {selectedAgent && activeTab === 'browse' && <BrowseTab agentId={selectedAgent} />}
        {selectedAgent && activeTab === 'search' && <SearchTab agentId={selectedAgent} />}
        {selectedAgent && activeTab === 'graph' && <GraphTab agentId={selectedAgent} />}
        {selectedAgent && activeTab === 'health' && <HealthTab agentId={selectedAgent} />}
      </div>
    </Page>
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
      <Card padded={false}>
        <EmptyState icon={BookOpen} title={intl.formatMessage({ id: 'wiki.empty' })} />
      </Card>
    );
  }

  return (
    <div className="flex min-h-[500px] gap-4">
      {/* Sidebar: tree */}
      <Card
        className="w-72 shrink-0 max-h-[calc(100vh-16rem)] overflow-y-auto"
        bodyClassName="p-4"
      >
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
      </Card>

      {/* Content */}
      <Card
        className="flex-1 max-h-[calc(100vh-16rem)] overflow-y-auto"
        bodyClassName="p-6"
      >
        {selectedPath ? (
          <WikiPageView path={selectedPath} content={pageContent} />
        ) : (
          <EmptyState icon={FileText} title={intl.formatMessage({ id: 'wiki.selectPage' })} />
        )}
      </Card>
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
          className="flex w-full items-center gap-1.5 rounded-md px-2 py-1.5 text-sm text-stone-600 hover:bg-stone-500/8 dark:text-stone-400 dark:hover:bg-white/5"
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
          ? 'bg-amber-500/15 text-amber-700 dark:bg-amber-400/15 dark:text-amber-400'
          : 'text-stone-600 hover:bg-stone-500/8 dark:text-stone-400 dark:hover:bg-white/5'
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

  const maturityTone =
    meta.maturity === 'internalized'
      ? 'success'
      : meta.maturity === 'validated'
        ? 'info'
        : 'neutral';

  return (
    <div>
      {/* Header */}
      <div className="mb-4 border-b border-[var(--panel-border)] pb-4">
        <div className="flex items-start justify-between gap-3">
          <h3 className="text-xl font-semibold text-stone-900 dark:text-stone-50">
            {meta.title || path}
          </h3>
          {/* Maturity badge */}
          {meta.maturity && (
            <Badge tone={maturityTone}>{meta.maturity}</Badge>
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
      <Card padded={false}>
        <EmptyState icon={Share2} title={intl.formatMessage({ id: 'wiki.empty' })} />
      </Card>
    );
  }

  return (
    <Card padded={false} bodyClassName="p-4">
      <WikiGraph
        pages={pages}
        pageContents={pageContents}
        width={900}
        height={550}
      />
    </Card>
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
      <Toolbar
        search={query}
        onSearchChange={setQuery}
        onSearchEnter={handleSearch}
        searchPlaceholder={intl.formatMessage({ id: 'wiki.search.placeholder' })}
      >
        <Button
          variant="primary"
          icon={Search}
          onClick={handleSearch}
          disabled={loading || !query.trim()}
        />
      </Toolbar>

      {hits.length === 0 ? (
        <Card padded={false}>
          <EmptyState icon={Search} title={intl.formatMessage({ id: 'wiki.search.empty' })} />
        </Card>
      ) : (
        <div className="space-y-3">
          {hits.map((hit) => (
            <Card key={hit.path}>
              <div className="mb-2 flex items-center justify-between gap-3">
                <h3 className="font-medium text-stone-900 dark:text-stone-50">
                  {hit.title}
                </h3>
                <span className="shrink-0 text-xs font-medium text-amber-600 dark:text-amber-400">
                  {intl.formatMessage({ id: 'wiki.relevance' })}: {Number(hit.score).toFixed(1)}
                </span>
              </div>
              <p className="mb-2 text-xs text-stone-500 dark:text-stone-400">{hit.path}</p>
              {hit.context_lines.length > 0 && (
                <div className="rounded-lg bg-stone-500/5 p-3 dark:bg-white/5">
                  {hit.context_lines.map((line, i) => (
                    <p key={i} className="font-mono text-xs text-stone-600 dark:text-stone-400">
                      {line}
                    </p>
                  ))}
                </div>
              )}
            </Card>
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
      <Card padded={false}>
        <EmptyState icon={BookOpen} title={intl.formatMessage({ id: 'wiki.empty' })} />
      </Card>
    );
  }

  const orphanCount = lint?.orphan_pages.length ?? 0;

  return (
    <div className="space-y-4">
      {/* Stats overview */}
      <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-4">
        <StatCard
          tone="accent"
          icon={FileText}
          label={intl.formatMessage({ id: 'wiki.stats.totalPages' })}
          value={stats?.total_pages ?? 0}
        />
        <StatCard
          tone="accent"
          icon={FolderTree}
          label={intl.formatMessage({ id: 'wiki.stats.directories' })}
          value={Object.keys(stats?.by_directory ?? {}).length}
        />
        <StatCard
          tone={orphanCount > 0 ? 'warning' : 'success'}
          icon={AlertTriangle}
          label={intl.formatMessage({ id: 'wiki.stats.orphans' })}
          value={orphanCount}
        />
        <StatCard
          tone={lint?.healthy ? 'success' : 'danger'}
          icon={lint?.healthy ? CheckCircle2 : AlertTriangle}
          label={intl.formatMessage({ id: 'wiki.stats.health' })}
          value={lint?.healthy ? intl.formatMessage({ id: 'wiki.healthy' }) : intl.formatMessage({ id: 'wiki.unhealthy' })}
        />
      </div>

      {/* Refresh button */}
      <div className="flex justify-end">
        <Button
          variant="secondary"
          size="sm"
          icon={RefreshCw}
          onClick={fetchData}
          disabled={loading}
          className={cn(loading && '[&_svg]:animate-spin')}
        >
          {intl.formatMessage({ id: 'wiki.lint.refresh' })}
        </Button>
      </div>

      {/* Directory breakdown */}
      {stats?.by_directory && Object.keys(stats.by_directory).length > 0 && (
        <Card
          title={
            <span className="flex items-center gap-2">
              <BarChart3 className="h-4 w-4" />
              {intl.formatMessage({ id: 'wiki.stats.byDirectory' })}
            </span>
          }
        >
          <div className="space-y-2">
            {Object.entries(stats.by_directory)
              .sort(([, a], [, b]) => b - a)
              .map(([dir, count]) => (
                <div key={dir} className="flex items-center justify-between">
                  <span className="text-sm text-stone-600 dark:text-stone-400">{dir}/</span>
                  <div className="flex items-center gap-2">
                    <div className="h-2 rounded-full bg-amber-500" style={{ width: `${Math.max(20, (count / stats.total_pages) * 200)}px` }} />
                    <span className="text-sm font-medium tabular-nums text-stone-700 dark:text-stone-300">{count}</span>
                  </div>
                </div>
              ))}
          </div>
        </Card>
      )}

      {/* Lint issues */}
      {lint && !lint.healthy && (
        <Section
          title={
            <span className="flex items-center gap-2 text-amber-800 dark:text-amber-300">
              <AlertTriangle className="h-4 w-4" />
              {intl.formatMessage({ id: 'wiki.lint.issues' })}
            </span>
          }
        >
          <div className="rounded-xl border border-amber-500/30 bg-amber-500/8 p-5 dark:bg-amber-950/20">
            {lint.orphan_pages.length > 0 && (
              <div className="mb-3">
                <p className="mb-1 text-sm font-medium text-amber-700 dark:text-amber-400">
                  {intl.formatMessage({ id: 'wiki.lint.orphans' })} ({lint.orphan_pages.length})
                </p>
                {lint.orphan_pages.map((p) => (
                  <p key={p} className="ml-4 text-xs text-amber-600 dark:text-amber-500">- {p}</p>
                ))}
              </div>
            )}

            {lint.broken_links.length > 0 && (
              <div className="mb-3">
                <p className="mb-1 text-sm font-medium text-amber-700 dark:text-amber-400">
                  {intl.formatMessage({ id: 'wiki.lint.brokenLinks' })} ({lint.broken_links.length})
                </p>
                {lint.broken_links.map(([from, to], i) => (
                  <p key={i} className="ml-4 text-xs text-amber-600 dark:text-amber-500">
                    {from} → {to}
                  </p>
                ))}
              </div>
            )}

            {lint.stale_pages.length > 0 && (
              <div>
                <p className="mb-1 text-sm font-medium text-amber-700 dark:text-amber-400">
                  {intl.formatMessage({ id: 'wiki.lint.stale' })} ({lint.stale_pages.length})
                </p>
                {lint.stale_pages.map((p) => (
                  <p key={p} className="ml-4 text-xs text-amber-600 dark:text-amber-500">- {p}</p>
                ))}
              </div>
            )}
          </div>
        </Section>
      )}
    </div>
  );
}
