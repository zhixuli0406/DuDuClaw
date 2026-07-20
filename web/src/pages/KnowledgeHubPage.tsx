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
  BookOpenIcon,
  SearchIcon,
  FileTextIcon,
  AlertTriangleIcon,
  CheckCircle2Icon,
  BarChart3Icon,
  RefreshCwIcon,
  Share2Icon,
  TagIcon,
  Link2Icon,
} from 'lucide-react';
import { WikiGraph } from '@/components/WikiGraph';
import { KnowledgeCuration } from './KnowledgeCuration';
import { timeAgo } from '@/lib/format';
import {
  CollectionPageHeader,
  CollectionPageState,
  Card,
  CardContent,
  BreadcrumbHeader,
  Segmented,
  Button,
  Badge,
  Input,
  Skeleton,
  Select,
  SelectTrigger,
  SelectValue,
  SelectContent,
  SelectItem,
  ListGridContainer,
  ListGridHeader,
  ListGridHeaderCell,
  ListGridRow,
  ListGridCell,
  type SegmentedOption,
} from '@/components/mds';

type ViewId = 'browse' | 'search' | 'graph' | 'health' | 'curate';

/** Namespace (top-level dir) of a wiki path; '' for root-level pages. */
function namespaceOf(path: string): string {
  const idx = path.indexOf('/');
  return idx > 0 ? path.slice(0, idx) : '';
}

/**
 * KnowledgeHubPage — the personal wiki surface, re-skinned onto MDS (spec §5.2 /
 * §5.3). A Segmented view switcher (browse / search / graph / health) + an agent
 * picker; browse lists pages as a flat ListGrid and opens a max-w-4xl prose
 * reading view with a BreadcrumbHeader. Search / graph / health / lint behaviour
 * is unchanged. Renders header-less when `embedded` (KnowledgeShell owns the
 * page header); standalone (legacy /wiki route) carries its own header.
 */
export function KnowledgeHubPage({ embedded = false }: { embedded?: boolean }) {
  const intl = useIntl();
  const [view, setView] = useState<ViewId>('browse');
  const [agents, setAgents] = useState<ReadonlyArray<{ name: string; display_name: string }>>([]);
  const [selectedAgent, setSelectedAgent] = useState('');

  useEffect(() => {
    api.agents.list().then((res) => {
      const list = res?.agents ?? [];
      setAgents(list);
      if (list.length > 0) setSelectedAgent((prev) => prev || list[0].name);
    }).catch(() => { /* ignore — empty state covers it */ });
  }, []);

  const viewOptions: SegmentedOption<ViewId>[] = [
    { value: 'browse', label: intl.formatMessage({ id: 'wiki.tab.browse' }) },
    { value: 'search', label: intl.formatMessage({ id: 'wiki.tab.search' }) },
    { value: 'graph', label: intl.formatMessage({ id: 'wiki.tab.graph' }) },
    { value: 'health', label: intl.formatMessage({ id: 'wiki.tab.health' }) },
    { value: 'curate', label: intl.formatMessage({ id: 'wiki.tab.curate' }) },
  ];

  const inner = (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center gap-2">
        <Segmented
          value={view}
          onValueChange={setView}
          options={viewOptions}
          aria-label={intl.formatMessage({ id: 'nav.wiki' })}
        />
        <AgentSelect
          className="ml-auto"
          value={selectedAgent}
          onValueChange={setSelectedAgent}
          agents={agents}
        />
      </div>
      {selectedAgent && view === 'browse' && <BrowseView agentId={selectedAgent} />}
      {selectedAgent && view === 'search' && <SearchView agentId={selectedAgent} />}
      {selectedAgent && view === 'graph' && <GraphView agentId={selectedAgent} />}
      {selectedAgent && view === 'health' && <HealthView agentId={selectedAgent} />}
      {selectedAgent && view === 'curate' && <KnowledgeCuration agentId={selectedAgent} />}
    </div>
  );

  if (embedded) return inner;

  return (
    <div className="-mx-4 -mt-4 flex flex-1 flex-col md:-mx-6 md:-mt-6">
      <CollectionPageHeader
        hideTrigger
        icon={BookOpenIcon}
        title={intl.formatMessage({ id: 'nav.wiki' })}
        description={intl.formatMessage({ id: 'nav.wiki.desc' })}
      />
      <div className="flex-1 p-4 md:p-6">{inner}</div>
    </div>
  );
}

/** Small agent picker shared across the wiki views. */
function AgentSelect({
  value,
  onValueChange,
  agents,
  className,
}: {
  value: string;
  onValueChange: (v: string) => void;
  agents: ReadonlyArray<{ name: string; display_name: string }>;
  className?: string;
}) {
  const current = agents.find((a) => a.name === value);
  if (agents.length === 0) return null;
  return (
    <Select value={value} onValueChange={(v) => onValueChange(String(v))}>
      <SelectTrigger className={cn('w-44 shrink-0', className)}>
        <SelectValue>{current ? current.display_name || current.name : value}</SelectValue>
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

// ── Browse view (flat page list → prose reading view) ───────

const WIKI_COLUMNS = 'minmax(0,1fr) auto auto';

function BrowseView({ agentId }: { agentId: string }) {
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

  const latestPathRef = useRef('');
  const handleSelect = useCallback(async (path: string) => {
    latestPathRef.current = path;
    setSelectedPath(path);
    setPageContent('');
    try {
      const res = await api.wiki.read(agentId, path);
      if (latestPathRef.current === path) setPageContent(res?.content ?? '');
    } catch {
      if (latestPathRef.current === path) setPageContent('Failed to load page.');
    }
  }, [agentId]);

  const sortedPages = useMemo(
    () => [...pages].sort((a, b) => a.path.localeCompare(b.path)),
    [pages],
  );

  if (loading) return <CollectionPageState state="loading" />;

  if (!wikiExists || pages.length === 0) {
    return (
      <CollectionPageState state="empty" icon={BookOpenIcon} title={intl.formatMessage({ id: 'wiki.empty' })} />
    );
  }

  // Reading view.
  if (selectedPath) {
    const page = pages.find((p) => p.path === selectedPath);
    return (
      <div className="-mx-4 md:-mx-6">
        <BreadcrumbHeader
          hideTrigger
          segments={[
            { label: intl.formatMessage({ id: 'wiki.pages' }), onClick: () => setSelectedPath('') },
            { label: page?.title ?? selectedPath },
          ]}
        />
        <div className="mx-auto max-w-4xl px-8 py-8">
          {pageContent ? (
            <WikiPageBody path={selectedPath} content={pageContent} />
          ) : (
            <div className="space-y-3">
              <Skeleton className="h-6 w-1/2" />
              <Skeleton className="h-4 w-full" />
              <Skeleton className="h-4 w-5/6" />
            </div>
          )}
        </div>
      </div>
    );
  }

  return (
    <div className="overflow-hidden rounded-xl border border-surface-border">
      <ListGridContainer
        columns={WIKI_COLUMNS}
        className="!h-auto"
        header={
          <ListGridHeader>
            <ListGridHeaderCell>{intl.formatMessage({ id: 'wiki.pages' })}</ListGridHeaderCell>
            <ListGridHeaderCell hideBelow>{intl.formatMessage({ id: 'scp.col.namespace' })}</ListGridHeaderCell>
            <ListGridHeaderCell hideBelow>{intl.formatMessage({ id: 'sharedWiki.stats.lastUpdated' })}</ListGridHeaderCell>
          </ListGridHeader>
        }
      >
        {sortedPages.map((page) => {
          const ns = namespaceOf(page.path);
          return (
            <ListGridRow key={page.path} onClick={() => handleSelect(page.path)}>
              <ListGridCell className="gap-2">
                <FileTextIcon className="size-4 shrink-0 text-muted-foreground" />
                <button
                  type="button"
                  className="truncate text-left text-sm font-medium text-foreground hover:text-brand hover:underline"
                  title={page.title || page.path}
                  onClick={(e) => {
                    e.stopPropagation();
                    handleSelect(page.path);
                  }}
                >
                  {page.title || page.path}
                </button>
              </ListGridCell>
              <ListGridCell hideBelow>
                {ns ? <Badge variant="outline">{ns}</Badge> : <span className="text-xs text-muted-foreground">—</span>}
              </ListGridCell>
              <ListGridCell hideBelow className="font-mono text-xs tabular-nums text-muted-foreground">
                {page.updated ? timeAgo(page.updated) : '—'}
              </ListGridCell>
            </ListGridRow>
          );
        })}
      </ListGridContainer>
    </div>
  );
}

/** Frontmatter-aware, lightweight markdown reader for a wiki page. */
function WikiPageBody({ path, content }: { path: string; content: string }) {
  const { meta, body } = useMemo(() => {
    const trimmed = content.trim();
    let frontmatter = '';
    let rest = trimmed;
    if (trimmed.startsWith('---')) {
      const after = trimmed.slice(3);
      const end = after.indexOf('\n---');
      if (end >= 0) {
        frontmatter = after.slice(0, end).trim();
        rest = after.slice(end + 4).trim();
      }
    }
    const parsed: Record<string, string> = {};
    for (const line of frontmatter.split('\n')) {
      const idx = line.indexOf(':');
      if (idx > 0) {
        const key = line.slice(0, idx).trim();
        parsed[key] = line.slice(idx + 1).trim().replace(/^["']|["']$/g, '');
      }
    }
    return { meta: parsed, body: rest };
  }, [content]);

  return (
    <article>
      <header className="mb-6 border-b border-surface-border pb-4">
        <div className="flex items-start justify-between gap-3">
          <h1 className="text-xl font-semibold text-foreground sm:text-2xl">{meta.title || path}</h1>
          {meta.maturity && <Badge variant="secondary">{meta.maturity}</Badge>}
        </div>
        <div className="mt-2 flex flex-wrap items-center gap-3 text-xs text-muted-foreground">
          {meta.updated && <span>{meta.updated}</span>}
          {meta.internalized_from && (
            <span className="flex items-center gap-1 text-brand">
              <BookOpenIcon className="size-3" />
              {meta.internalized_from}
            </span>
          )}
          {meta.tags && (
            <span className="flex items-center gap-1">
              <TagIcon className="size-3" />
              {meta.tags}
            </span>
          )}
          {meta.related && (
            <span className="flex items-center gap-1">
              <Link2Icon className="size-3" />
              {meta.related}
            </span>
          )}
        </div>
      </header>
      <div className="prose prose-stone max-w-none text-sm dark:prose-invert">
        {body.split('\n').map((line, i) => {
          if (line.startsWith('# ')) return <h1 key={i} className="mt-6 mb-2 text-xl font-semibold text-foreground">{line.slice(2)}</h1>;
          if (line.startsWith('## ')) return <h2 key={i} className="mt-5 mb-2 text-lg font-medium text-foreground">{line.slice(3)}</h2>;
          if (line.startsWith('### ')) return <h3 key={i} className="mt-4 mb-1 text-base font-medium text-foreground">{line.slice(4)}</h3>;
          if (line.startsWith('- ')) return <li key={i} className="ml-4 text-foreground">{line.slice(2)}</li>;
          if (line.trim() === '') return <div key={i} className="h-2" />;
          return <p key={i} className="leading-relaxed text-foreground">{line}</p>;
        })}
      </div>
    </article>
  );
}

// ── Search view ─────────────────────────────────────────────

function SearchView({ agentId }: { agentId: string }) {
  const intl = useIntl();
  const [query, setQuery] = useState('');
  const [hits, setHits] = useState<ReadonlyArray<WikiSearchHit>>([]);
  const [loading, setLoading] = useState(false);
  const [searched, setSearched] = useState(false);

  const handleSearch = useCallback(async () => {
    if (!query.trim()) return;
    setLoading(true);
    setSearched(true);
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
      <div className="relative max-w-md">
        <SearchIcon className="pointer-events-none absolute top-1/2 left-2.5 size-3.5 -translate-y-1/2 text-muted-foreground" />
        <Input
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={(e) => e.key === 'Enter' && handleSearch()}
          placeholder={intl.formatMessage({ id: 'wiki.search.placeholder' })}
          className="pl-8"
        />
      </div>

      {loading ? (
        <CollectionPageState state="loading" />
      ) : hits.length === 0 ? (
        <CollectionPageState
          state="empty"
          icon={SearchIcon}
          title={intl.formatMessage({ id: searched ? 'wiki.search.empty' : 'wiki.search.empty' })}
        />
      ) : (
        <div className="space-y-3">
          {hits.map((hit) => (
            <Card key={hit.path} data-size="sm">
              <CardContent className="space-y-2">
                <div className="flex items-center justify-between gap-3">
                  <h3 className="truncate text-sm font-medium text-foreground">{hit.title}</h3>
                  <span className="shrink-0 font-mono text-xs tabular-nums text-brand">
                    {intl.formatMessage({ id: 'wiki.relevance' })}: {Number(hit.score).toFixed(1)}
                  </span>
                </div>
                <p className="font-mono text-xs text-muted-foreground">{hit.path}</p>
                {hit.context_lines.length > 0 && (
                  <div className="rounded-lg bg-muted p-3">
                    {hit.context_lines.map((line, i) => (
                      <p key={i} className="font-mono text-xs text-muted-foreground">{line}</p>
                    ))}
                  </div>
                )}
              </CardContent>
            </Card>
          ))}
        </div>
      )}
    </div>
  );
}

// ── Graph view ──────────────────────────────────────────────

function GraphView({ agentId }: { agentId: string }) {
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
      const contents: Record<string, string> = {};
      const BATCH_SIZE = 10;
      for (let i = 0; i < pageList.length; i += BATCH_SIZE) {
        if (cancelled) return;
        const batch = pageList.slice(i, i + BATCH_SIZE);
        await Promise.all(
          batch.map(async (p) => {
            try {
              const r = await api.wiki.read(agentId, p.path);
              contents[p.path] = r?.content ?? '';
            } catch { /* skip failed reads */ }
          }),
        );
      }
      if (!cancelled) setPageContents({ ...contents });
    }).catch(() => {
      if (!cancelled) setPages([]);
    }).finally(() => {
      if (!cancelled) setLoading(false);
    });
    return () => { cancelled = true; };
  }, [agentId]);

  if (!loading && pages.length === 0) {
    return <CollectionPageState state="empty" icon={Share2Icon} title={intl.formatMessage({ id: 'wiki.empty' })} />;
  }

  return (
    <Card>
      <CardContent>
        <WikiGraph pages={pages} pageContents={pageContents} width={900} height={550} />
      </CardContent>
    </Card>
  );
}

// ── Health view ─────────────────────────────────────────────

function HealthView({ agentId }: { agentId: string }) {
  const intl = useIntl();
  const [stats, setStats] = useState<WikiStats | null>(null);
  const [lint, setLint] = useState<WikiLintReport | null>(null);
  const [loading, setLoading] = useState(false);

  const fetchData = useCallback(async () => {
    setLoading(true);
    try {
      const [statsRes, lintRes] = await Promise.all([api.wiki.stats(agentId), api.wiki.lint(agentId)]);
      setStats(statsRes);
      setLint(lintRes);
    } catch { /* handled by empty state */ } finally {
      setLoading(false);
    }
  }, [agentId]);

  useEffect(() => { fetchData(); }, [fetchData]);

  if (loading && !stats) return <CollectionPageState state="loading" />;

  if (!stats?.exists) {
    return <CollectionPageState state="empty" icon={BookOpenIcon} title={intl.formatMessage({ id: 'wiki.empty' })} />;
  }

  const orphanCount = lint?.orphan_pages.length ?? 0;

  return (
    <div className="space-y-4">
      <div className="grid grid-cols-2 gap-3 lg:grid-cols-4">
        <KpiTile icon={FileTextIcon} label={intl.formatMessage({ id: 'wiki.stats.totalPages' })} value={String(stats.total_pages)} />
        <KpiTile icon={BarChart3Icon} label={intl.formatMessage({ id: 'wiki.stats.directories' })} value={String(Object.keys(stats.by_directory ?? {}).length)} />
        <KpiTile
          icon={AlertTriangleIcon}
          label={intl.formatMessage({ id: 'wiki.stats.orphans' })}
          value={String(orphanCount)}
          tone={orphanCount > 0 ? 'warning' : 'success'}
        />
        <KpiTile
          icon={lint?.healthy ? CheckCircle2Icon : AlertTriangleIcon}
          label={intl.formatMessage({ id: 'wiki.stats.health' })}
          value={lint?.healthy ? intl.formatMessage({ id: 'wiki.healthy' }) : intl.formatMessage({ id: 'wiki.unhealthy' })}
          tone={lint?.healthy ? 'success' : 'destructive'}
        />
      </div>

      <div className="flex justify-end">
        <Button
          variant="outline"
          size="sm"
          onClick={fetchData}
          disabled={loading}
        >
          <RefreshCwIcon className={cn(loading && 'animate-spin')} />
          {intl.formatMessage({ id: 'wiki.lint.refresh' })}
        </Button>
      </div>

      {stats.by_directory && Object.keys(stats.by_directory).length > 0 && (
        <Card>
          <CardContent className="space-y-2">
            <h3 className="flex items-center gap-2 text-sm font-medium text-foreground">
              <BarChart3Icon className="size-4" />
              {intl.formatMessage({ id: 'wiki.stats.byDirectory' })}
            </h3>
            {Object.entries(stats.by_directory)
              .sort(([, a], [, b]) => b - a)
              .map(([dir, count]) => (
                <div key={dir} className="flex items-center gap-3">
                  <span className="w-40 truncate text-sm text-muted-foreground">{dir}/</span>
                  <div className="h-2 flex-1 overflow-hidden rounded-full bg-muted">
                    <div className="h-full rounded-full bg-chart-1" style={{ width: `${Math.max(4, (count / stats.total_pages) * 100)}%` }} />
                  </div>
                  <span className="w-8 text-right font-mono text-xs tabular-nums text-muted-foreground">{count}</span>
                </div>
              ))}
          </CardContent>
        </Card>
      )}

      {lint && !lint.healthy && (
        <Card className="border-warning/40">
          <CardContent className="space-y-3">
            <h3 className="flex items-center gap-2 text-sm font-medium text-warning">
              <AlertTriangleIcon className="size-4" />
              {intl.formatMessage({ id: 'wiki.lint.issues' })}
            </h3>
            {lint.orphan_pages.length > 0 && (
              <LintGroup title={`${intl.formatMessage({ id: 'wiki.lint.orphans' })} (${lint.orphan_pages.length})`} items={lint.orphan_pages} />
            )}
            {lint.broken_links.length > 0 && (
              <LintGroup
                title={`${intl.formatMessage({ id: 'wiki.lint.brokenLinks' })} (${lint.broken_links.length})`}
                items={lint.broken_links.map(([from, to]) => `${from} → ${to}`)}
              />
            )}
            {lint.stale_pages.length > 0 && (
              <LintGroup title={`${intl.formatMessage({ id: 'wiki.lint.stale' })} (${lint.stale_pages.length})`} items={lint.stale_pages} />
            )}
          </CardContent>
        </Card>
      )}
    </div>
  );
}

function KpiTile({
  icon: Icon,
  label,
  value,
  tone = 'default',
}: {
  icon: React.ComponentType<{ className?: string }>;
  label: string;
  value: string;
  tone?: 'default' | 'success' | 'warning' | 'destructive';
}) {
  const toneClass =
    tone === 'success' ? 'text-success'
    : tone === 'warning' ? 'text-warning'
    : tone === 'destructive' ? 'text-destructive'
    : 'text-muted-foreground';
  return (
    <div className="rounded-lg border border-surface-border bg-card p-4">
      <div className="flex items-center gap-1.5 text-xs text-muted-foreground">
        <Icon className={cn('size-3.5', toneClass)} />
        {label}
      </div>
      <p className={cn('mt-1 text-2xl font-medium tabular-nums', toneClass === 'text-muted-foreground' ? 'text-foreground' : toneClass)}>
        {value}
      </p>
    </div>
  );
}

function LintGroup({ title, items }: { title: string; items: string[] }) {
  return (
    <div>
      <p className="mb-1 text-sm font-medium text-warning">{title}</p>
      {items.map((it) => (
        <p key={it} className="ml-4 font-mono text-xs text-muted-foreground">- {it}</p>
      ))}
    </div>
  );
}
