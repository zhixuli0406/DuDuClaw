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
import { Dialog, FormField, inputClass, selectClass, buttonPrimary, buttonSecondary } from '@/components/shared/Dialog';
import { toast, formatError } from '@/lib/toast';
import {
  Page,
  PageHeader,
  Card,
  Section,
  StatCard,
  Tabs,
  type TabItem,
  Button,
  Badge,
  EmptyState,
  Toolbar,
  CharacterAvatar,
  Mono,
} from '@/components/ui';
import {
  Globe,
  BookOpen,
  Search,
  FileText,
  Clock,
  Tag,
  BarChart3,
  ChevronRight,
  ChevronDown,
  Users,
  Lock,
  Plus,
  Pencil,
  Trash2,
  Building2,
} from 'lucide-react';

type TabId = 'browse' | 'search' | 'stats' | 'policy';

export function SharedWikiPage() {
  const intl = useIntl();
  const [activeTab, setActiveTab] = useState<TabId>('browse');

  const tabs: TabItem[] = [
    { id: 'browse', label: intl.formatMessage({ id: 'sharedWiki.tab.browse' }), icon: BookOpen },
    { id: 'search', label: intl.formatMessage({ id: 'sharedWiki.tab.search' }), icon: Search },
    { id: 'stats', label: intl.formatMessage({ id: 'sharedWiki.tab.stats' }), icon: BarChart3 },
    { id: 'policy', label: intl.formatMessage({ id: 'sharedWiki.tab.policy' }), icon: Lock },
  ];

  return (
    <Page>
      <PageHeader
        icon={Globe}
        title={intl.formatMessage({ id: 'nav.sharedWiki' })}
        subtitle={intl.formatMessage({ id: 'sharedWiki.subtitle' })}
      />

      <Tabs items={tabs} value={activeTab} onChange={(id) => setActiveTab(id as TabId)} />

      <div role="tabpanel" id={`shared-wiki-panel-${activeTab}`}>
        {activeTab === 'browse' && <BrowseTab />}
        {activeTab === 'search' && <SearchTab />}
        {activeTab === 'stats' && <StatsTab />}
        {activeTab === 'policy' && <PolicyTab />}
      </div>
    </Page>
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
      <Card>
        <EmptyState dudu="reading" icon={BookOpen} title={intl.formatMessage({ id: 'sharedWiki.empty' })} />
      </Card>
    );
  }

  return (
    <div className="grid grid-cols-12 gap-4">
      {/* Tree sidebar */}
      <Card
        className="col-span-4"
        title={`${intl.formatMessage({ id: 'sharedWiki.pages' })} (${pages.length})`}
      >
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
      </Card>

      {/* Content viewer */}
      <Card className="col-span-8" title={selectedPath || undefined}>
        {selectedPath ? (
          <pre className="max-h-[60vh] overflow-auto whitespace-pre-wrap rounded-control bg-stone-500/5 p-4 font-mono text-sm text-stone-800 dark:bg-white/5 dark:text-stone-200">
            {pageContent || 'Loading...'}
          </pre>
        ) : (
          <EmptyState dudu="idle" icon={FileText} title={intl.formatMessage({ id: 'sharedWiki.selectPage' })} />
        )}
      </Card>
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
  const intl = useIntl();
  const [expanded, setExpanded] = useState(depth < 1);

  if (node.isDir) {
    // WP7 — surface the top-level `departments/` namespace as a friendly,
    // read-only "部門知識庫" group for a non-technical audience.
    const isDeptRoot = depth === 0 && node.name === 'departments';
    return (
      <div>
        <button
          onClick={() => setExpanded((prev) => !prev)}
          className="flex w-full items-center gap-1 rounded-lg px-2 py-1 text-sm text-stone-600 hover:bg-stone-500/8 dark:text-stone-300 dark:hover:bg-white/5"
          style={{ paddingLeft: `${depth * 12 + 8}px` }}
        >
          {expanded ? <ChevronDown className="h-3.5 w-3.5" /> : <ChevronRight className="h-3.5 w-3.5" />}
          {isDeptRoot ? (
            <span className="flex items-center gap-1.5 font-medium text-amber-700 dark:text-amber-300">
              <Building2 className="h-3.5 w-3.5" />
              {intl.formatMessage({ id: 'sharedWiki.departments.group' })}
            </span>
          ) : (
            <span className="font-medium">{node.name}/</span>
          )}
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
        'flex w-full items-center gap-1.5 rounded-lg px-2 py-1 text-sm transition-colors',
        selectedPath === node.path
          ? 'bg-amber-500/12 text-amber-700 dark:text-amber-400'
          : 'text-stone-600 hover:bg-stone-500/8 dark:text-stone-300 dark:hover:bg-white/5'
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
      <Toolbar
        search={query}
        onSearchChange={setQuery}
        onSearchEnter={handleSearch}
        searchPlaceholder={intl.formatMessage({ id: 'sharedWiki.search.placeholder' })}
      >
        <Button
          variant="primary"
          icon={Search}
          onClick={handleSearch}
          disabled={searching || !query.trim()}
        />
      </Toolbar>

      {hits.length > 0 ? (
        <div className="space-y-3">
          {hits.map((hit) => (
            <Card key={hit.path}>
              <div className="flex items-center gap-2">
                <FileText className="h-4 w-4 text-amber-500" />
                <span className="font-medium text-stone-900 dark:text-stone-50">{hit.title}</span>
                <span className="text-xs text-stone-400">({hit.path})</span>
                <Badge tone="accent" className="ml-auto">
                  {hit.score}
                </Badge>
              </div>
              {hit.context_lines.length > 0 && (
                <div className="mt-2 rounded-control bg-stone-500/5 p-2 text-xs text-stone-600 dark:bg-white/5 dark:text-stone-400">
                  {hit.context_lines.map((line, i) => (
                    <div key={i} className="truncate">{line}</div>
                  ))}
                </div>
              )}
            </Card>
          ))}
        </div>
      ) : (
        <Card>
          <EmptyState dudu="concerned" icon={Search} title={intl.formatMessage({ id: 'sharedWiki.search.empty' })} />
        </Card>
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
      <Card>
        <EmptyState dudu="reading" icon={BarChart3} title={intl.formatMessage({ id: 'sharedWiki.empty' })} />
      </Card>
    );
  }

  const authorEntries = Object.entries(stats.by_author ?? {}).sort(([, a], [, b]) => b - a);
  const dirEntries = Object.entries(stats.by_directory ?? {}).sort(([, a], [, b]) => b - a);

  return (
    <div className="space-y-6">
      {/* Summary cards */}
      <div className="grid grid-cols-1 gap-4 sm:grid-cols-3">
        <StatCard
          icon={FileText}
          tone="accent"
          label={intl.formatMessage({ id: 'sharedWiki.stats.totalPages' })}
          value={stats.total_pages}
        />
        <StatCard
          icon={Users}
          tone="neutral"
          label={intl.formatMessage({ id: 'sharedWiki.stats.contributors' })}
          value={authorEntries.length}
        />
        <StatCard
          icon={Clock}
          tone="neutral"
          label={intl.formatMessage({ id: 'sharedWiki.stats.lastUpdated' })}
          value={
            stats.most_recent?.updated
              ? new Date(stats.most_recent.updated).toLocaleDateString()
              : '—'
          }
          hint={stats.most_recent?.title ?? undefined}
        />
      </div>

      {/* By author */}
      {authorEntries.length > 0 && (
        <Section title={intl.formatMessage({ id: 'sharedWiki.stats.byAuthor' })}>
          <Card>
            <div className="space-y-2">
              {authorEntries.map(([author, count]) => (
                <div key={author} className="flex items-center gap-2">
                  <CharacterAvatar agentId={author} name={author} size={24} />
                  <span className="flex-1 text-sm text-stone-700 dark:text-stone-300">{author}</span>
                  <div className="h-2 flex-1 overflow-hidden rounded-full bg-stone-500/10 dark:bg-white/10">
                    <div
                      className="h-full rounded-full bg-amber-400"
                      style={{ width: `${(count / stats.total_pages) * 100}%` }}
                    />
                  </div>
                  <Mono className="min-w-[2rem] text-right text-sm font-medium text-stone-500">{count}</Mono>
                </div>
              ))}
            </div>
          </Card>
        </Section>
      )}

      {/* By directory */}
      {dirEntries.length > 0 && (
        <Section title={intl.formatMessage({ id: 'sharedWiki.stats.byDirectory' })}>
          <Card>
            <div className="space-y-2">
              {dirEntries.map(([dir, count]) => (
                <div key={dir} className="flex items-center gap-2">
                  <Tag className="h-3.5 w-3.5 text-stone-400" />
                  <span className="flex-1 text-sm text-stone-700 dark:text-stone-300">{dir}/</span>
                  <Mono className="text-sm font-medium text-stone-500">{count}</Mono>
                </div>
              ))}
            </div>
          </Card>
        </Section>
      )}
    </div>
  );
}

// ── Namespace Policy Tab (SCP.2) ──────────────────────────

const SCOPE_MODE_TONES: Record<WikiScopeMode, 'success' | 'warning' | 'danger'> = {
  agent_writable: 'success',
  read_only: 'warning',
  operator_only: 'danger',
};

function PolicyTab() {
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

  useEffect(() => {
    fetchScope();
  }, [fetchScope]);

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
      <Section
        description={intl.formatMessage({ id: 'scp.desc' })}
        actions={
          <Button
            variant="primary"
            icon={Plus}
            onClick={() => setEditing({ ns: { namespace: '', mode: 'agent_writable', synced_from: null }, isNew: true })}
          >
            {intl.formatMessage({ id: 'scp.add' })}
          </Button>
        }
      >
        <Card padded={false}>
          {loading ? (
            <p className="py-8 text-center text-sm text-stone-400">{intl.formatMessage({ id: 'common.loading' })}</p>
          ) : namespaces.length === 0 ? (
            <EmptyState dudu="idle" icon={Lock} title={intl.formatMessage({ id: 'scp.empty' })} />
          ) : (
            <div className="overflow-x-auto">
              <table className="w-full text-sm">
                <thead>
                  <tr className="border-b border-[var(--panel-border)]">
                    <th className="px-5 py-2.5 text-left font-medium text-stone-500 dark:text-stone-400">{intl.formatMessage({ id: 'scp.col.namespace' })}</th>
                    <th className="px-5 py-2.5 text-left font-medium text-stone-500 dark:text-stone-400">{intl.formatMessage({ id: 'scp.col.mode' })}</th>
                    <th className="px-5 py-2.5 text-left font-medium text-stone-500 dark:text-stone-400">{intl.formatMessage({ id: 'scp.col.syncedFrom' })}</th>
                    <th className="px-5 py-2.5 text-right font-medium text-stone-500 dark:text-stone-400" />
                  </tr>
                </thead>
                <tbody>
                  {namespaces.map((n) => (
                    <tr key={n.namespace} className="border-b border-[var(--panel-border)] last:border-0">
                      <td className="px-5 py-2.5 font-medium text-stone-800 dark:text-stone-200">{n.namespace}/</td>
                      <td className="px-5 py-2.5">
                        <Badge tone={SCOPE_MODE_TONES[n.mode]}>{n.mode}</Badge>
                      </td>
                      <td className="px-5 py-2.5 text-xs text-stone-500 dark:text-stone-400">{n.synced_from ?? '—'}</td>
                      <td className="px-5 py-2.5 text-right">
                        <div className="flex items-center justify-end gap-1">
                          <Button
                            variant="ghost"
                            size="sm"
                            icon={Pencil}
                            onClick={() => setEditing({ ns: { ...n }, isNew: false })}
                            title={intl.formatMessage({ id: 'common.edit' })}
                          />
                          <Button
                            variant="ghost"
                            size="sm"
                            icon={Trash2}
                            onClick={() => handleRemove(n.namespace)}
                            title={intl.formatMessage({ id: 'common.delete' })}
                            className="text-rose-500 hover:bg-rose-500/10 hover:text-rose-600"
                          />
                        </div>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </Card>
      </Section>

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
    <Dialog open onClose={onClose} title={isNew ? intl.formatMessage({ id: 'scp.add' }) : intl.formatMessage({ id: 'scp.edit' })}>
      <div className="space-y-4">
        <FormField label={intl.formatMessage({ id: 'scp.col.namespace' })} hint={intl.formatMessage({ id: 'scp.field.namespace.hint' })}>
          <input type="text" value={namespace} onChange={(e) => setNamespace(e.target.value)} disabled={!isNew} placeholder="identity" className={inputClass} />
        </FormField>
        <FormField label={intl.formatMessage({ id: 'scp.col.mode' })} hint={intl.formatMessage({ id: 'scp.field.mode.hint' })}>
          <select value={mode} onChange={(e) => setMode(e.target.value as WikiScopeMode)} className={selectClass}>
            <option value="agent_writable">agent_writable</option>
            <option value="read_only">read_only</option>
            <option value="operator_only">operator_only</option>
          </select>
        </FormField>
        {mode === 'read_only' && (
          <FormField label={intl.formatMessage({ id: 'scp.col.syncedFrom' })} hint={intl.formatMessage({ id: 'scp.field.syncedFrom.hint' })}>
            <input type="text" value={syncedFrom} onChange={(e) => setSyncedFrom(e.target.value)} placeholder="identity:read" className={inputClass} />
          </FormField>
        )}
        {error && <p className="text-sm text-rose-600 dark:text-rose-400">{error}</p>}
        <div className="flex justify-end gap-3 border-t border-stone-200 pt-4 dark:border-stone-700">
          <button onClick={onClose} className={buttonSecondary}>{intl.formatMessage({ id: 'common.cancel' })}</button>
          <button onClick={handleSubmit} disabled={submitting} className={buttonPrimary}>
            {submitting ? intl.formatMessage({ id: 'common.saving' }) : intl.formatMessage({ id: 'common.save' })}
          </button>
        </div>
      </div>
    </Dialog>
  );
}
