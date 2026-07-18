import { useState, useCallback, useEffect, useMemo } from 'react';
import { useIntl } from 'react-intl';
import { cn } from '@/lib/utils';
import {
  api,
  type WikiPageMeta,
  type WikiSearchHit,
  type SharedWikiStats,
  type WikiScopeNamespace,
  type WikiScopeMode,
} from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
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
  Select,
  SelectTrigger,
  SelectValue,
  SelectContent,
  SelectItem,
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
  ActorAvatar,
  type SegmentedOption,
} from '@/components/mds';
import {
  GlobeIcon,
  BookOpenIcon,
  SearchIcon,
  FileTextIcon,
  BarChart3Icon,
  UsersIcon,
  LockIcon,
  PlusIcon,
  PencilIcon,
  Trash2Icon,
  Building2Icon,
  MoreHorizontalIcon,
} from 'lucide-react';

type ViewId = 'browse' | 'search' | 'stats' | 'policy';

/** Namespace (top-level dir) of a wiki path; '' for root-level pages. */
function namespaceOf(path: string): string {
  const idx = path.indexOf('/');
  return idx > 0 ? path.slice(0, idx) : '';
}

/**
 * SharedWikiPage — the cross-agent shared wiki, re-skinned onto MDS (spec §5.2).
 * A Segmented view switcher (browse / search / stats / namespace policy); browse
 * lists pages as a flat ListGrid and opens a max-w-4xl reading view. Namespace
 * policy modes render as tone-coded badges (read_only=warning / operator_only=
 * destructive / agent_writable=secondary). Data flow is unchanged. Renders
 * header-less when `embedded` (KnowledgeShell owns the header).
 */
export function SharedWikiPage({ embedded = false }: { embedded?: boolean }) {
  const intl = useIntl();
  const [view, setView] = useState<ViewId>('browse');

  const viewOptions: SegmentedOption<ViewId>[] = [
    { value: 'browse', label: intl.formatMessage({ id: 'sharedWiki.tab.browse' }) },
    { value: 'search', label: intl.formatMessage({ id: 'sharedWiki.tab.search' }) },
    { value: 'stats', label: intl.formatMessage({ id: 'sharedWiki.tab.stats' }) },
    { value: 'policy', label: intl.formatMessage({ id: 'sharedWiki.tab.policy' }) },
  ];

  const inner = (
    <div className="space-y-4">
      <Segmented
        value={view}
        onValueChange={setView}
        options={viewOptions}
        aria-label={intl.formatMessage({ id: 'nav.sharedWiki' })}
      />
      {view === 'browse' && <BrowseView />}
      {view === 'search' && <SearchView />}
      {view === 'stats' && <StatsView />}
      {view === 'policy' && <PolicyView />}
    </div>
  );

  if (embedded) return inner;

  return (
    <div className="-mx-4 -mt-4 flex flex-1 flex-col md:-mx-6 md:-mt-6">
      <CollectionPageHeader
        hideTrigger
        icon={GlobeIcon}
        title={intl.formatMessage({ id: 'nav.sharedWiki' })}
        description={intl.formatMessage({ id: 'sharedWiki.subtitle' })}
      />
      <div className="flex-1 p-4 md:p-6">{inner}</div>
    </div>
  );
}

// ── Browse view (flat page list → reading view) ─────────────

const SHARED_WIKI_COLUMNS = 'minmax(0,1fr) auto auto';

function BrowseView() {
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

  const handleSelect = useCallback((path: string) => {
    setSelectedPath(path);
    setPageContent('');
    api.sharedWiki.read(path).then((res) => setPageContent(res?.content ?? '')).catch(() => setPageContent('Error loading page'));
  }, []);

  const sortedPages = useMemo(
    () => [...pages].sort((a, b) => a.path.localeCompare(b.path)),
    [pages],
  );

  if (loading) return <CollectionPageState state="loading" />;

  if (!wikiExists || pages.length === 0) {
    return <CollectionPageState state="empty" icon={BookOpenIcon} title={intl.formatMessage({ id: 'sharedWiki.empty' })} />;
  }

  if (selectedPath) {
    const page = pages.find((p) => p.path === selectedPath);
    return (
      <div className="-mx-4 md:-mx-6">
        <BreadcrumbHeader
          hideTrigger
          segments={[
            { label: intl.formatMessage({ id: 'sharedWiki.pages' }), onClick: () => setSelectedPath('') },
            { label: page?.title ?? selectedPath },
          ]}
        />
        <div className="mx-auto max-w-4xl px-8 py-8">
          <h1 className="mb-4 text-xl font-semibold text-foreground sm:text-2xl">{page?.title ?? selectedPath}</h1>
          <pre className="overflow-auto whitespace-pre-wrap rounded-xl border border-surface-border bg-muted p-4 font-mono text-sm text-foreground">
            {pageContent || intl.formatMessage({ id: 'common.loading' })}
          </pre>
        </div>
      </div>
    );
  }

  return (
    <div className="overflow-hidden rounded-xl border border-surface-border">
      <ListGridContainer
        columns={SHARED_WIKI_COLUMNS}
        className="!h-auto"
        header={
          <ListGridHeader>
            <ListGridHeaderCell>{intl.formatMessage({ id: 'sharedWiki.pages' })}</ListGridHeaderCell>
            <ListGridHeaderCell hideBelow>{intl.formatMessage({ id: 'scp.col.namespace' })}</ListGridHeaderCell>
            <ListGridHeaderCell hideBelow>{intl.formatMessage({ id: 'sharedWiki.stats.lastUpdated' })}</ListGridHeaderCell>
          </ListGridHeader>
        }
      >
        {sortedPages.map((page) => {
          const ns = namespaceOf(page.path);
          const isDept = ns === 'departments';
          return (
            <ListGridRow key={page.path} onClick={() => handleSelect(page.path)}>
              <ListGridCell className="gap-2">
                <FileTextIcon className="size-4 shrink-0 text-muted-foreground" />
                <button
                  type="button"
                  className="truncate text-left text-sm font-medium text-foreground hover:text-brand hover:underline"
                  title={page.title || page.path}
                  onClick={(e) => { e.stopPropagation(); handleSelect(page.path); }}
                >
                  {page.title || page.path}
                </button>
              </ListGridCell>
              <ListGridCell hideBelow>
                {ns ? (
                  <Badge variant="outline" className={cn('gap-1', isDept && 'text-brand')}>
                    {isDept && <Building2Icon className="size-3" />}
                    {isDept ? intl.formatMessage({ id: 'sharedWiki.departments.group' }) : ns}
                  </Badge>
                ) : (
                  <span className="text-xs text-muted-foreground">—</span>
                )}
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

// ── Search view ─────────────────────────────────────────────

function SearchView() {
  const intl = useIntl();
  const [query, setQuery] = useState('');
  const [hits, setHits] = useState<ReadonlyArray<WikiSearchHit>>([]);
  const [loading, setLoading] = useState(false);

  const handleSearch = useCallback(() => {
    if (!query.trim()) return;
    setLoading(true);
    api.sharedWiki.search(query.trim()).then((res) => setHits(res?.hits ?? [])).catch(() => setHits([])).finally(() => setLoading(false));
  }, [query]);

  return (
    <div className="space-y-4">
      <div className="relative max-w-md">
        <SearchIcon className="pointer-events-none absolute top-1/2 left-2.5 size-3.5 -translate-y-1/2 text-muted-foreground" />
        <Input
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={(e) => e.key === 'Enter' && handleSearch()}
          placeholder={intl.formatMessage({ id: 'sharedWiki.search.placeholder' })}
          className="pl-8"
        />
      </div>

      {loading ? (
        <CollectionPageState state="loading" />
      ) : hits.length === 0 ? (
        <CollectionPageState state="empty" icon={SearchIcon} title={intl.formatMessage({ id: 'sharedWiki.search.empty' })} />
      ) : (
        <div className="space-y-3">
          {hits.map((hit) => (
            <Card key={hit.path} data-size="sm">
              <CardContent className="space-y-2">
                <div className="flex items-center gap-2">
                  <FileTextIcon className="size-4 shrink-0 text-brand" />
                  <span className="truncate text-sm font-medium text-foreground">{hit.title}</span>
                  <span className="truncate font-mono text-xs text-muted-foreground">{hit.path}</span>
                  <Badge variant="secondary" className="ml-auto shrink-0">{hit.score}</Badge>
                </div>
                {hit.context_lines.length > 0 && (
                  <div className="rounded-lg bg-muted p-2">
                    {hit.context_lines.map((line, i) => (
                      <div key={i} className="truncate font-mono text-xs text-muted-foreground">{line}</div>
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

// ── Stats view ──────────────────────────────────────────────

function StatsView() {
  const intl = useIntl();
  const [stats, setStats] = useState<SharedWikiStats | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    api.sharedWiki.stats().then(setStats).catch(() => setStats(null)).finally(() => setLoading(false));
  }, []);

  if (loading) return <CollectionPageState state="loading" />;

  if (!stats?.exists) {
    return <CollectionPageState state="empty" icon={BarChart3Icon} title={intl.formatMessage({ id: 'sharedWiki.empty' })} />;
  }

  const authorEntries = Object.entries(stats.by_author ?? {}).sort(([, a], [, b]) => b - a);
  const dirEntries = Object.entries(stats.by_directory ?? {}).sort(([, a], [, b]) => b - a);

  return (
    <div className="space-y-4">
      <div className="grid grid-cols-1 gap-3 sm:grid-cols-3">
        <KpiTile icon={FileTextIcon} label={intl.formatMessage({ id: 'sharedWiki.stats.totalPages' })} value={String(stats.total_pages)} />
        <KpiTile icon={UsersIcon} label={intl.formatMessage({ id: 'sharedWiki.stats.contributors' })} value={String(authorEntries.length)} />
        <KpiTile
          icon={BarChart3Icon}
          label={intl.formatMessage({ id: 'sharedWiki.stats.lastUpdated' })}
          value={stats.most_recent?.updated ? new Date(stats.most_recent.updated).toLocaleDateString() : '—'}
        />
      </div>

      {authorEntries.length > 0 && (
        <Card>
          <CardContent className="space-y-2">
            <h3 className="text-sm font-medium text-foreground">{intl.formatMessage({ id: 'sharedWiki.stats.byAuthor' })}</h3>
            {authorEntries.map(([author, count]) => (
              <div key={author} className="flex items-center gap-2">
                <ActorAvatar actorType="agent" size="xs" name={author} />
                <span className="flex-1 truncate text-sm text-foreground">{author}</span>
                <div className="h-2 w-32 overflow-hidden rounded-full bg-muted">
                  <div className="h-full rounded-full bg-chart-1" style={{ width: `${(count / stats.total_pages) * 100}%` }} />
                </div>
                <span className="w-8 text-right font-mono text-xs tabular-nums text-muted-foreground">{count}</span>
              </div>
            ))}
          </CardContent>
        </Card>
      )}

      {dirEntries.length > 0 && (
        <Card>
          <CardContent className="space-y-2">
            <h3 className="text-sm font-medium text-foreground">{intl.formatMessage({ id: 'sharedWiki.stats.byDirectory' })}</h3>
            {dirEntries.map(([dir, count]) => (
              <div key={dir} className="flex items-center gap-2">
                <span className="flex-1 truncate text-sm text-foreground">{dir}/</span>
                <span className="font-mono text-xs tabular-nums text-muted-foreground">{count}</span>
              </div>
            ))}
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
}: {
  icon: React.ComponentType<{ className?: string }>;
  label: string;
  value: string;
}) {
  return (
    <div className="rounded-lg border border-surface-border bg-card p-4">
      <div className="flex items-center gap-1.5 text-xs text-muted-foreground">
        <Icon className="size-3.5" />
        {label}
      </div>
      <p className="mt-1 text-2xl font-medium tabular-nums text-foreground">{value}</p>
    </div>
  );
}

// ── Namespace policy view (SCP.2) ───────────────────────────

const SCOPE_MODE_BADGE: Record<WikiScopeMode, { variant: 'secondary' | 'destructive'; className?: string }> = {
  agent_writable: { variant: 'secondary' },
  read_only: { variant: 'secondary', className: 'bg-warning/15 text-warning' },
  operator_only: { variant: 'destructive' },
};

const POLICY_COLUMNS = 'minmax(0,1fr) auto auto 2.5rem';

function PolicyView() {
  const intl = useIntl();
  const [namespaces, setNamespaces] = useState<ReadonlyArray<WikiScopeNamespace>>([]);
  const [loading, setLoading] = useState(false);
  const [editing, setEditing] = useState<{ ns: WikiScopeNamespace; isNew: boolean } | null>(null);

  const fetchScope = useCallback(async () => {
    setLoading(true);
    try {
      const res = await api.wikiScope.get();
      setNamespaces(res?.namespaces ?? []);
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
    } finally {
      setLoading(false);
    }
  }, [intl]);

  useEffect(() => { fetchScope(); }, [fetchScope]);

  const handleRemove = async (ns: string) => {
    try {
      await api.wikiScope.update({ namespace: ns, remove: true });
      toast.success(intl.formatMessage({ id: 'scp.removed' }));
      fetchScope();
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
    }
  };

  return (
    <div className="space-y-4">
      <div className="flex items-start justify-between gap-3">
        <p className="max-w-2xl text-sm text-muted-foreground">{intl.formatMessage({ id: 'scp.desc' })}</p>
        <Button
          variant="brand"
          size="sm"
          className="shrink-0"
          onClick={() => setEditing({ ns: { namespace: '', mode: 'agent_writable', synced_from: null }, isNew: true })}
        >
          <PlusIcon />
          <span className="hidden sm:inline">{intl.formatMessage({ id: 'scp.add' })}</span>
        </Button>
      </div>

      {loading ? (
        <CollectionPageState state="loading" />
      ) : namespaces.length === 0 ? (
        <CollectionPageState state="empty" icon={LockIcon} title={intl.formatMessage({ id: 'scp.empty' })} />
      ) : (
        <div className="overflow-hidden rounded-xl border border-surface-border">
          <ListGridContainer
            columns={POLICY_COLUMNS}
            className="!h-auto"
            header={
              <ListGridHeader>
                <ListGridHeaderCell>{intl.formatMessage({ id: 'scp.col.namespace' })}</ListGridHeaderCell>
                <ListGridHeaderCell>{intl.formatMessage({ id: 'scp.col.mode' })}</ListGridHeaderCell>
                <ListGridHeaderCell hideBelow>{intl.formatMessage({ id: 'scp.col.syncedFrom' })}</ListGridHeaderCell>
                <ListGridHeaderCell aria-hidden />
              </ListGridHeader>
            }
          >
            {namespaces.map((n) => {
              const badge = SCOPE_MODE_BADGE[n.mode];
              return (
                <ListGridRow key={n.namespace} className="cursor-default">
                  <ListGridCell>
                    <span className="truncate font-medium text-foreground">{n.namespace}/</span>
                  </ListGridCell>
                  <ListGridCell>
                    <Badge variant={badge.variant} className={badge.className}>{n.mode}</Badge>
                  </ListGridCell>
                  <ListGridCell hideBelow className="text-xs text-muted-foreground">
                    {n.synced_from ?? '—'}
                  </ListGridCell>
                  <ListGridCell className="justify-end">
                    <DropdownMenu>
                      <DropdownMenuTrigger
                        render={
                          <Button
                            variant="ghost"
                            size="icon-sm"
                            aria-label={intl.formatMessage({ id: 'common.edit' })}
                            data-stop-row-nav
                          />
                        }
                      >
                        <MoreHorizontalIcon />
                      </DropdownMenuTrigger>
                      <DropdownMenuContent>
                        <DropdownMenuItem onClick={() => setEditing({ ns: { ...n }, isNew: false })}>
                          <PencilIcon />
                          {intl.formatMessage({ id: 'common.edit' })}
                        </DropdownMenuItem>
                        <DropdownMenuItem className="text-destructive focus:bg-destructive/10" onClick={() => handleRemove(n.namespace)}>
                          <Trash2Icon />
                          {intl.formatMessage({ id: 'common.delete' })}
                        </DropdownMenuItem>
                      </DropdownMenuContent>
                    </DropdownMenu>
                  </ListGridCell>
                </ListGridRow>
              );
            })}
          </ListGridContainer>
        </div>
      )}

      {editing && (
        <NamespacePolicyDialog
          initial={editing.ns}
          isNew={editing.isNew}
          onClose={() => setEditing(null)}
          onSaved={() => { setEditing(null); fetchScope(); }}
        />
      )}
    </div>
  );
}

function NamespacePolicyDialog({
  initial,
  isNew,
  onClose,
  onSaved,
}: {
  initial: WikiScopeNamespace;
  isNew: boolean;
  onClose: () => void;
  onSaved: () => void;
}) {
  const intl = useIntl();
  const [namespace, setNamespace] = useState(initial.namespace);
  const [mode, setMode] = useState<WikiScopeMode>(initial.mode);
  const [syncedFrom, setSyncedFrom] = useState(initial.synced_from ?? '');
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const handleSubmit = async () => {
    if (!namespace.trim()) {
      setError(intl.formatMessage({ id: 'scp.error.nsRequired' }));
      return;
    }
    if (mode === 'read_only' && !syncedFrom.trim()) {
      setError(intl.formatMessage({ id: 'scp.error.syncedRequired' }));
      return;
    }
    setSubmitting(true);
    setError(null);
    try {
      await api.wikiScope.update({
        namespace: namespace.trim(),
        mode,
        ...(mode === 'read_only' ? { synced_from: syncedFrom.trim() } : {}),
      });
      onSaved();
    } catch (e) {
      setError(formatError(e));
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>{intl.formatMessage({ id: isNew ? 'scp.add' : 'scp.edit' })}</DialogTitle>
          <DialogDescription>{intl.formatMessage({ id: 'scp.desc' })}</DialogDescription>
        </DialogHeader>
        <div className="space-y-4">
          <div className="space-y-1.5">
            <label className="text-xs font-medium text-muted-foreground">{intl.formatMessage({ id: 'scp.col.namespace' })}</label>
            <Input value={namespace} onChange={(e) => setNamespace(e.target.value)} disabled={!isNew} placeholder="identity" />
            <p className="text-xs text-muted-foreground">{intl.formatMessage({ id: 'scp.field.namespace.hint' })}</p>
          </div>
          <div className="space-y-1.5">
            <label className="text-xs font-medium text-muted-foreground">{intl.formatMessage({ id: 'scp.col.mode' })}</label>
            <Select value={mode} onValueChange={(v) => setMode(String(v) as WikiScopeMode)}>
              <SelectTrigger className="w-full">
                <SelectValue>{mode}</SelectValue>
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="agent_writable">agent_writable</SelectItem>
                <SelectItem value="read_only">read_only</SelectItem>
                <SelectItem value="operator_only">operator_only</SelectItem>
              </SelectContent>
            </Select>
            <p className="text-xs text-muted-foreground">{intl.formatMessage({ id: 'scp.field.mode.hint' })}</p>
          </div>
          {mode === 'read_only' && (
            <div className="space-y-1.5">
              <label className="text-xs font-medium text-muted-foreground">{intl.formatMessage({ id: 'scp.col.syncedFrom' })}</label>
              <Input value={syncedFrom} onChange={(e) => setSyncedFrom(e.target.value)} placeholder="identity:read" />
              <p className="text-xs text-muted-foreground">{intl.formatMessage({ id: 'scp.field.syncedFrom.hint' })}</p>
            </div>
          )}
          {error && <p className="text-sm text-destructive">{error}</p>}
        </div>
        <DialogFooter>
          <DialogClose render={<Button variant="outline">{intl.formatMessage({ id: 'common.cancel' })}</Button>} />
          <Button variant="brand" onClick={handleSubmit} disabled={submitting}>
            {intl.formatMessage({ id: submitting ? 'common.saving' : 'common.save' })}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
