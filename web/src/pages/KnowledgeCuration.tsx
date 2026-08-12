import { useState, useEffect, useCallback, useMemo } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate } from 'react-router';
import {
  Share2Icon,
  HistoryIcon,
  SearchIcon,
  XIcon,
  FileTextIcon,
  CheckCircle2Icon,
  Trash2Icon,
  EyeIcon,
  EraserIcon,
} from 'lucide-react';
import {
  Tabs,
  TabsList,
  TabsTab,
  TabsPanel,
  Card,
  CardContent,
  Button,
  Badge,
  Input,
  Empty,
  ErrorState,
  Spinner,
  CrossLink,
} from '@/components/mds';
import { ConfirmDialog } from '@/components/settings/controls/ConfirmDialog';
import { MemoryGraph } from '@/components/MemoryGraph';
import { ThinkingOrbIndicator } from '@/components/chat/ThinkingOrbIndicator';
import {
  api,
  type MemoryGraphEdge,
  type MemoryGraphResult,
  type MemoryChainEntry,
  type AutoWikiPage,
} from '@/lib/api';
import { timeAgo } from '@/lib/format';
import { isImeComposing } from '@/lib/keyboard';

type CurateTab = 'graph' | 'timeline' | 'auto';

/** A fact key that the graph tab can hand off to the timeline tab. */
interface FactKey {
  subject: string;
  predicate: string;
}

/**
 * KnowledgeCuration — the HITL knowledge-curation station, mounted as a view
 * inside KnowledgeHubPage. Three sub-tabs: the SPO 知識圖譜 (force-directed
 * viewer + provenance panel), 事實歷史 (supersession timeline), and 自動建檔
 * (WP5c audit surface for pages the AI filed on its own).
 *
 * WP5c / D20 changed this station's job. Auto-filing is now unattended, so the
 * old 待審知識 approval queue no longer belongs here — it moved out to the
 * inbox. **Only the tab was removed**: the burst-quarantine backend
 * (`approvals.list/decide` + `knowledge_quarantine`) is untouched and still
 * produces items, because same-origin burst detection keeps running. Deleting
 * the backend too would have created a queue with no release path.
 *
 * All copy is end-user facing zh-TW — no internal terms (origin_trust / PPR /
 * namespace / distill) leak into the UI.
 */
export function KnowledgeCuration({ agentId }: { agentId: string }) {
  const intl = useIntl();
  const [tab, setTab] = useState<CurateTab>('graph');
  // Lifted so the graph's "查看事實歷史" can jump to the timeline pre-filled.
  const [pinnedFact, setPinnedFact] = useState<FactKey | null>(null);

  const openHistory = useCallback((fact: FactKey) => {
    setPinnedFact(fact);
    setTab('timeline');
  }, []);

  return (
    <Tabs
      variant="line"
      value={tab}
      onValueChange={(v) => setTab(v as CurateTab)}
      className="flex flex-col gap-4"
    >
      <TabsList>
        <TabsTab value="graph">{intl.formatMessage({ id: 'curate.tab.graph' })}</TabsTab>
        <TabsTab value="timeline">{intl.formatMessage({ id: 'curate.tab.timeline' })}</TabsTab>
        <TabsTab value="auto">{intl.formatMessage({ id: 'curate.tab.auto' })}</TabsTab>
      </TabsList>

      <TabsPanel value="graph">
        <GraphTab agentId={agentId} onOpenHistory={openHistory} />
      </TabsPanel>
      <TabsPanel value="timeline">
        <TimelineTab agentId={agentId} pinnedFact={pinnedFact} />
      </TabsPanel>
      <TabsPanel value="auto">
        <AutoPagesTab agentId={agentId} />
      </TabsPanel>
    </Tabs>
  );
}

// ── Graph tab ───────────────────────────────────────────────

const TIER_SWATCH: Record<'high' | 'medium' | 'low', string> = {
  high: '#10b981',
  medium: '#f59e0b',
  low: '#ef4444',
};

function GraphTab({
  agentId,
  onOpenHistory,
}: {
  agentId: string;
  onOpenHistory: (fact: FactKey) => void;
}) {
  const intl = useIntl();
  const [graph, setGraph] = useState<MemoryGraphResult | null>(null);
  const [loading, setLoading] = useState(false);
  const [selected, setSelected] = useState<MemoryGraphEdge | null>(null);
  // Read failures here used to be rewritten into an empty graph — the write
  // paths on this page already reported failures properly, the read paths
  // didn't (P05 Blocker, phase-4 audit).
  const [loadError, setLoadError] = useState<unknown>(null);
  const [reloadNonce, setReloadNonce] = useState(0);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setSelected(null);
    api.memory.graph(agentId).then((res) => {
      if (cancelled) return;
      setGraph(res);
      setLoadError(null);
    }).catch((e: unknown) => {
      if (cancelled) return;
      setGraph(null);
      setLoadError(e);
    }).finally(() => {
      if (!cancelled) setLoading(false);
    });
    return () => { cancelled = true; };
  }, [agentId, reloadNonce]);

  if (loading && !graph) {
    return (
      <div className="flex justify-center py-16">
        <ThinkingOrbIndicator state="searching" />
      </div>
    );
  }

  if (loadError != null) {
    return (
      <ErrorState
        error={loadError}
        onRetry={() => setReloadNonce((n) => n + 1)}
      />
    );
  }

  if (!graph || graph.edges.length === 0) {
    return (
      <Empty
        icon={Share2Icon}
        title={intl.formatMessage({ id: 'curate.graph.empty.title' })}
        description={intl.formatMessage({ id: 'curate.graph.empty.desc' })}
      />
    );
  }

  return (
    <div className="space-y-2">
      {/* Plain-language disambiguation from KnowledgeHubPage's topic map — same
          page tree, two different "graphs" (UX audit §2-5). */}
      <p className="text-sm text-muted-foreground">
        {intl.formatMessage({ id: 'curate.graph.intro' })}
      </p>
      <div className="grid gap-4 lg:grid-cols-[1fr_20rem]">
        <Card className="overflow-hidden">
          {/* Legend + truncation notice */}
          <div className="flex flex-wrap items-center gap-x-4 gap-y-1.5 border-b border-surface-border px-4 py-2">
            <span className="font-mono text-xs tabular-nums text-muted-foreground">
              {graph.nodes.length} · {graph.edges.length}
            </span>
            <span className="hidden h-3 w-px bg-surface-border sm:inline-block" />
            {(['high', 'medium', 'low'] as const).map((tier) => (
              <span key={tier} className="flex items-center gap-1.5 text-xs text-muted-foreground">
                <span className="inline-block h-2.5 w-2.5 rounded-full" style={{ backgroundColor: TIER_SWATCH[tier] }} />
                {intl.formatMessage({ id: `curate.legend.${tier}` })}
              </span>
            ))}
            {graph.truncated && (
              <span className="ml-auto text-xs text-warning">
                {intl.formatMessage({ id: 'curate.graph.truncated' }, { n: graph.edges.length })}
              </span>
            )}
          </div>
          <MemoryGraph
            nodes={graph.nodes}
            edges={graph.edges}
            onSelectEdge={setSelected}
            selectedMemoryId={selected?.memory_id ?? null}
          />
        </Card>

        {/* Provenance side panel */}
        {selected ? (
          <ProvenancePanel
            agentId={agentId}
            edge={selected}
            onClose={() => setSelected(null)}
            onOpenHistory={onOpenHistory}
          />
        ) : (
          <Card data-size="sm">
            <CardContent className="flex h-full items-center justify-center py-10 text-center text-sm text-muted-foreground">
              {intl.formatMessage({ id: 'curate.graph.selectHint' })}
            </CardContent>
          </Card>
        )}
      </div>
    </div>
  );
}

// X03 (UX audit §3.3): exported so the CrossLink into WikiTrustPage can be
// unit-tested directly, the same way `formatSource` below is — the graph tab
// it normally opens inside is a D3/SVG canvas not worth driving from a test.
export function ProvenancePanel({
  agentId,
  edge,
  onClose,
  onOpenHistory,
}: {
  agentId: string;
  edge: MemoryGraphEdge;
  onClose: () => void;
  onOpenHistory: (fact: FactKey) => void;
}) {
  const intl = useIntl();
  const navigate = useNavigate();
  // Enrich with confidence + valid interval from the fact's history (best-effort).
  const [record, setRecord] = useState<MemoryChainEntry | null>(null);

  useEffect(() => {
    let cancelled = false;
    setRecord(null);
    if (!edge.predicate) return;
    api.memory.history(agentId, { subject: edge.subject, predicate: edge.predicate })
      .then((res) => {
        if (cancelled) return;
        const match = res.chain.find((c) => c.id === edge.memory_id) ?? null;
        setRecord(match);
      })
      .catch(() => { /* edge-level info is enough */ });
    return () => { cancelled = true; };
  }, [agentId, edge]);

  const trustLabel =
    edge.origin_trust >= 0.7 ? 'high' : edge.origin_trust >= 0.3 ? 'medium' : 'low';

  return (
    <Card data-size="sm">
      <CardContent className="space-y-3">
        <div className="flex items-center justify-between">
          <h3 className="text-sm font-medium text-foreground">
            {intl.formatMessage({ id: 'curate.provenance.title' })}
          </h3>
          <Button variant="ghost" size="icon-xs" onClick={onClose} aria-label={intl.formatMessage({ id: 'common.close' })}>
            <XIcon />
          </Button>
        </div>

        {edge.quarantined && (
          <Badge variant="destructive">{intl.formatMessage({ id: 'curate.provenance.quarantined' })}</Badge>
        )}

        <dl className="space-y-2 text-sm">
          <Row label={intl.formatMessage({ id: 'curate.provenance.subject' })} value={edge.subject} />
          {edge.predicate && (
            <Row label={intl.formatMessage({ id: 'curate.provenance.predicate' })} value={edge.predicate} />
          )}
          {edge.object && (
            <Row label={intl.formatMessage({ id: 'curate.provenance.object' })} value={edge.object} />
          )}
          <div className="flex items-center justify-between gap-2">
            <dt className="text-muted-foreground">{intl.formatMessage({ id: 'curate.provenance.trust' })}</dt>
            <dd className="flex items-center gap-1.5">
              <span className="inline-block h-2.5 w-2.5 rounded-full" style={{ backgroundColor: TIER_SWATCH[trustLabel] }} />
              <span className="font-mono text-xs tabular-nums">{Math.round(edge.origin_trust * 100)}%</span>
            </dd>
          </div>
          {record?.confidence != null && (
            <Row
              label={intl.formatMessage({ id: 'curate.provenance.confidence' })}
              value={`${Math.round(record.confidence * 100)}%`}
              mono
            />
          )}
          {record?.valid_from && (
            <Row label={intl.formatMessage({ id: 'curate.provenance.validFrom' })} value={timeAgo(record.valid_from)} />
          )}
          <Row
            label={intl.formatMessage({ id: 'curate.provenance.validUntil' })}
            value={
              record?.valid_until
                ? timeAgo(record.valid_until)
                : intl.formatMessage({ id: 'curate.provenance.stillValid' })
            }
          />
        </dl>

        {/* X03 (UX audit §3.3): the trust % above is this same edge's
            `origin_trust` (write-time trust ceiling); the full page-level
            trust score audit lives on WikiTrustPage, an unrelated nav branch
            with no route back until now. */}
        <div className="flex justify-end">
          <CrossLink
            label={intl.formatMessage({ id: 'crosslink.provenance.wikiTrustLink' })}
            onClick={() => navigate('/manage/governance?tab=wikiTrust')}
          />
        </div>

        {edge.predicate && (
          <Button
            variant="outline"
            size="sm"
            className="w-full"
            onClick={() => onOpenHistory({ subject: edge.subject, predicate: edge.predicate! })}
          >
            <HistoryIcon />
            {intl.formatMessage({ id: 'curate.provenance.viewHistory' })}
          </Button>
        )}
      </CardContent>
    </Card>
  );
}

function Row({ label, value, mono }: { label: string; value: string; mono?: boolean }) {
  return (
    <div className="flex items-center justify-between gap-2">
      <dt className="shrink-0 text-muted-foreground">{label}</dt>
      <dd className={`truncate text-right text-foreground${mono ? ' font-mono text-xs tabular-nums' : ''}`} title={value}>
        {value}
      </dd>
    </div>
  );
}

// ── Timeline tab ────────────────────────────────────────────

function TimelineTab({ agentId, pinnedFact }: { agentId: string; pinnedFact: FactKey | null }) {
  const intl = useIntl();
  const [subject, setSubject] = useState('');
  const [predicate, setPredicate] = useState('');
  const [chain, setChain] = useState<MemoryChainEntry[] | null>(null);
  const [loading, setLoading] = useState(false);
  const [queryError, setQueryError] = useState<unknown>(null);

  const runQuery = useCallback(async (subj: string, pred: string) => {
    if (!subj.trim() || !pred.trim()) return;
    setLoading(true);
    try {
      const res = await api.memory.history(agentId, { subject: subj.trim(), predicate: pred.trim() });
      setChain(res.chain);
      setQueryError(null);
    } catch (e) {
      // A failed query used to render as "this fact has no history".
      setChain(null);
      setQueryError(e);
    } finally {
      setLoading(false);
    }
  }, [agentId]);

  // Pre-fill + auto-run when the graph hands off a fact.
  useEffect(() => {
    if (pinnedFact) {
      setSubject(pinnedFact.subject);
      setPredicate(pinnedFact.predicate);
      runQuery(pinnedFact.subject, pinnedFact.predicate);
    }
  }, [pinnedFact, runQuery]);

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-end gap-2">
        <label className="flex-1 space-y-1">
          <span className="text-xs text-muted-foreground">{intl.formatMessage({ id: 'curate.timeline.subject' })}</span>
          <Input
            value={subject}
            onChange={(e) => setSubject(e.target.value)}
            onKeyDown={(e) => e.key === 'Enter' && !isImeComposing(e) && runQuery(subject, predicate)}
            placeholder={intl.formatMessage({ id: 'curate.timeline.placeholder.subject' })}
          />
        </label>
        <label className="flex-1 space-y-1">
          <span className="text-xs text-muted-foreground">{intl.formatMessage({ id: 'curate.timeline.predicate' })}</span>
          <Input
            value={predicate}
            onChange={(e) => setPredicate(e.target.value)}
            onKeyDown={(e) => e.key === 'Enter' && !isImeComposing(e) && runQuery(subject, predicate)}
            placeholder={intl.formatMessage({ id: 'curate.timeline.placeholder.predicate' })}
          />
        </label>
        <Button variant="brand" onClick={() => runQuery(subject, predicate)} disabled={loading}>
          <SearchIcon />
          {intl.formatMessage({ id: 'curate.timeline.load' })}
        </Button>
      </div>

      {loading ? (
        <div className="flex justify-center py-16">
          <ThinkingOrbIndicator state="searching" />
        </div>
      ) : queryError != null ? (
        <ErrorState
          icon={HistoryIcon}
          error={queryError}
          onRetry={() => void runQuery(subject, predicate)}
        />
      ) : chain === null ? (
        <Empty icon={HistoryIcon} title={intl.formatMessage({ id: 'curate.timeline.empty' })} variant="dashed" />
      ) : chain.length === 0 ? (
        <Empty icon={HistoryIcon} title={intl.formatMessage({ id: 'curate.timeline.noHistory' })} variant="dashed" />
      ) : (
        <FactTimeline chain={chain} />
      )}
    </div>
  );
}

/** Horizontal supersession timeline — one lane per version, positioned by its
 *  valid interval. Superseded = grey, current = brand, quarantined = red frame. */
function FactTimeline({ chain }: { chain: MemoryChainEntry[] }) {
  const intl = useIntl();
  const nowMs = Date.now();

  const { min, span, lanes } = useMemo(() => {
    const parse = (s: string | null, fallback: number) => {
      if (!s) return fallback;
      const t = Date.parse(s);
      return Number.isNaN(t) ? fallback : t;
    };
    let min = Infinity;
    let max = -Infinity;
    const lanes = chain.map((c) => {
      const from = parse(c.valid_from, nowMs);
      const until = c.valid_until ? parse(c.valid_until, nowMs) : nowMs;
      min = Math.min(min, from);
      max = Math.max(max, until);
      return { entry: c, from, until };
    });
    if (!Number.isFinite(min)) min = nowMs;
    if (!Number.isFinite(max)) max = nowMs;
    const span = Math.max(max - min, 1);
    return { min, span, lanes };
  }, [chain, nowMs]);

  return (
    <div className="space-y-2 rounded-xl border border-surface-border bg-surface p-4">
      {lanes.map(({ entry, from, until }) => {
        const left = ((from - min) / span) * 100;
        const width = Math.max(((until - from) / span) * 100, 3);
        const tone = entry.is_current
          ? 'bg-brand text-brand-foreground'
          : 'bg-muted text-muted-foreground';
        return (
          <div key={entry.id} className="relative h-9">
            <div className="absolute inset-y-0 left-0 right-0 rounded-md bg-muted/30" />
            <div
              className={`absolute inset-y-1 flex items-center overflow-hidden rounded-md px-2 text-xs ${tone}`}
              style={{ left: `${left}%`, width: `${width}%`, minWidth: '4rem' }}
              title={`${entry.content}\n${entry.valid_from ?? '?'} → ${entry.valid_until ?? intl.formatMessage({ id: 'curate.timeline.now' })}`}
            >
              <span className="truncate">{entry.content}</span>
            </div>
          </div>
        );
      })}
      <div className="flex items-center gap-4 pt-1 text-xs text-muted-foreground">
        <span className="flex items-center gap-1.5">
          <span className="inline-block h-2.5 w-2.5 rounded-full bg-brand" />
          {intl.formatMessage({ id: 'curate.timeline.current' })}
        </span>
        <span className="flex items-center gap-1.5">
          <span className="inline-block h-2.5 w-2.5 rounded-full bg-muted" />
          {intl.formatMessage({ id: 'curate.timeline.superseded' })}
        </span>
      </div>
    </div>
  );
}

// ── Auto-filed pages tab (WP5c audit surface) ───────────────

/**
 * 自動建檔 — every page the AI created from a conversation on its own, with
 * where it came from and three reversible actions.
 *
 * Rollback semantics matter here. 「移除」 archives ONE page and expires ONLY
 * that page's memory pointer. The separate 「清除所有自動蒐集的知識」 button is
 * the blunt instrument (it expires every conversationally-learned memory this
 * AI staff member has) and says so in its confirmation — the two must never
 * read as the same action.
 */
function AutoPagesTab({ agentId }: { agentId: string }) {
  const intl = useIntl();
  const [pages, setPages] = useState<AutoWikiPage[] | null>(null);
  const [busyPath, setBusyPath] = useState<string | null>(null);
  const [error, setError] = useState('');
  const [notice, setNotice] = useState('');
  const [preview, setPreview] = useState<{ path: string; content: string } | null>(null);
  const [promoteTarget, setPromoteTarget] = useState<AutoWikiPage | null>(null);
  const [removeTarget, setRemoveTarget] = useState<AutoWikiPage | null>(null);
  const [purgeOpen, setPurgeOpen] = useState(false);

  const [loadError, setLoadError] = useState<unknown>(null);

  const fetchPages = useCallback(async () => {
    try {
      const res = await api.wiki.autoPages(agentId);
      setPages(res.pages);
      setLoadError(null);
    } catch (e) {
      // Was silently rewritten into "no auto-filed pages yet".
      setPages([]);
      setLoadError(e);
    }
  }, [agentId]);

  useEffect(() => {
    setPreview(null);
    fetchPages();
  }, [fetchPages]);

  const run = useCallback(
    async (path: string, action: () => Promise<unknown>, successId?: string) => {
      setBusyPath(path);
      setError('');
      setNotice('');
      try {
        await action();
        if (successId) setNotice(intl.formatMessage({ id: successId }));
      } catch {
        setError(intl.formatMessage({ id: 'curate.auto.actionFailed' }));
      } finally {
        setBusyPath(null);
        setPromoteTarget(null);
        setRemoveTarget(null);
        setPurgeOpen(false);
        fetchPages();
      }
    },
    [fetchPages, intl],
  );

  const view = useCallback(
    async (page: AutoWikiPage) => {
      if (preview?.path === page.path) {
        setPreview(null);
        return;
      }
      setBusyPath(page.path);
      try {
        const res = await api.wiki.read(agentId, page.path);
        setPreview({ path: page.path, content: res.content });
      } catch {
        setError(intl.formatMessage({ id: 'curate.auto.actionFailed' }));
      } finally {
        setBusyPath(null);
      }
    },
    [agentId, intl, preview],
  );

  if (pages === null) {
    return (
      <div className="flex justify-center py-16">
        <Spinner />
      </div>
    );
  }

  if (loadError != null) {
    return (
      <ErrorState
        icon={FileTextIcon}
        error={loadError}
        onRetry={() => void fetchPages()}
      />
    );
  }

  return (
    <div className="space-y-3">
      {error && <p className="text-sm text-destructive">{error}</p>}
      {notice && <p className="text-sm text-success">{notice}</p>}

      {pages.length === 0 ? (
        <Empty
          icon={FileTextIcon}
          title={intl.formatMessage({ id: 'curate.auto.empty.title' })}
          description={intl.formatMessage({ id: 'curate.auto.empty.desc' })}
        />
      ) : (
        pages.map((page) => (
          <Card key={page.path} data-size="sm">
            <CardContent className="space-y-3">
              <div className="flex flex-wrap items-center gap-2">
                <FileTextIcon className="size-4 shrink-0 text-muted-foreground" />
                <span className="text-sm font-medium text-foreground">{page.title}</span>
                <Badge variant="secondary">{page.doc_type_label}</Badge>
              </div>

              <div className="flex flex-wrap gap-x-4 gap-y-1 text-xs text-muted-foreground">
                <span>
                  {intl.formatMessage({ id: 'curate.auto.updated' })}：
                  <span className="text-foreground">{timeAgo(page.updated)}</span>
                </span>
                <span>
                  {intl.formatMessage({ id: 'curate.auto.revisions' })}：
                  <span className="font-mono tabular-nums text-foreground">
                    {page.revision_count}
                  </span>
                </span>
                {page.sources.length > 0 && (
                  <span className="truncate" title={page.sources.join('\n')}>
                    {intl.formatMessage({ id: 'curate.auto.source' })}：
                    <span className="text-foreground">
                      {formatSource(page.sources[page.sources.length - 1])}
                    </span>
                  </span>
                )}
              </div>

              <div className="flex flex-wrap gap-2">
                <Button
                  variant="outline"
                  size="sm"
                  disabled={busyPath === page.path}
                  onClick={() => view(page)}
                >
                  <EyeIcon />
                  {intl.formatMessage({ id: 'curate.auto.view' })}
                </Button>
                <Button
                  variant="brand"
                  size="sm"
                  disabled={busyPath === page.path}
                  onClick={() => setPromoteTarget(page)}
                >
                  <CheckCircle2Icon />
                  {intl.formatMessage({ id: 'curate.auto.promote' })}
                </Button>
                <Button
                  variant="outline"
                  size="sm"
                  disabled={busyPath === page.path}
                  onClick={() =>
                    run(page.path, () => api.wiki.share(agentId, page.path), 'curate.auto.shared')
                  }
                >
                  <Share2Icon />
                  {intl.formatMessage({ id: 'curate.auto.share' })}
                </Button>
                <Button
                  variant="destructive"
                  size="sm"
                  disabled={busyPath === page.path}
                  onClick={() => setRemoveTarget(page)}
                >
                  <Trash2Icon />
                  {intl.formatMessage({ id: 'curate.auto.remove' })}
                </Button>
              </div>

              {preview?.path === page.path && (
                <pre className="max-h-80 overflow-auto rounded-lg border border-surface-border bg-muted/30 p-3 text-xs whitespace-pre-wrap break-words text-foreground">
                  {preview.content}
                </pre>
              )}
            </CardContent>
          </Card>
        ))
      )}

      {/* Nuclear option — deliberately separated from the per-page 移除. */}
      <div className="flex justify-end pt-2">
        <Button variant="ghost" size="sm" onClick={() => setPurgeOpen(true)}>
          <EraserIcon />
          {intl.formatMessage({ id: 'curate.auto.purgeAll' })}
        </Button>
      </div>

      <ConfirmDialog
        open={!!promoteTarget}
        onClose={() => setPromoteTarget(null)}
        onConfirm={() =>
          promoteTarget &&
          run(
            promoteTarget.path,
            () => api.wiki.promote(agentId, promoteTarget.path),
            'curate.auto.promoted',
          )
        }
        title={intl.formatMessage({ id: 'curate.auto.promote.confirmTitle' })}
        message={intl.formatMessage(
          { id: 'curate.auto.promote.confirmMsg' },
          { title: promoteTarget?.title ?? '' },
        )}
        confirmLabel={intl.formatMessage({ id: 'curate.auto.promote.confirmBtn' })}
        busy={busyPath === promoteTarget?.path}
      />

      <ConfirmDialog
        open={!!removeTarget}
        onClose={() => setRemoveTarget(null)}
        onConfirm={() =>
          removeTarget && run(removeTarget.path, () => api.wiki.archive(agentId, removeTarget.path))
        }
        title={intl.formatMessage({ id: 'curate.auto.remove.confirmTitle' })}
        message={intl.formatMessage(
          { id: 'curate.auto.remove.confirmMsg' },
          { title: removeTarget?.title ?? '' },
        )}
        confirmLabel={intl.formatMessage({ id: 'curate.auto.remove.confirmBtn' })}
        busy={busyPath === removeTarget?.path}
      />

      <ConfirmDialog
        open={purgeOpen}
        onClose={() => setPurgeOpen(false)}
        onConfirm={() => run('*', () => api.memory.invalidateOrigin(agentId, 'channel'))}
        title={intl.formatMessage({ id: 'curate.auto.purgeAll.confirmTitle' })}
        message={intl.formatMessage({ id: 'curate.auto.purgeAll.confirmMsg' })}
        confirmLabel={intl.formatMessage({ id: 'curate.auto.purgeAll.confirmBtn' })}
        busy={busyPath === '*'}
      />
    </div>
  );
}

/** Channel token → the name the user knows it by. Unlisted tokens pass through. */
const CHANNEL_LABELS: Record<string, string> = {
  telegram: 'Telegram',
  discord: 'Discord',
  slack: 'Slack',
  line: 'LINE',
  whatsapp: 'WhatsApp',
  feishu: '飛書',
  googlechat: 'Google Chat',
  msteams: 'Microsoft Teams',
  teams: 'Microsoft Teams',
  wecom: '企業微信',
  dingtalk: '釘釘',
  email: 'Email',
  webchat: '網頁對話',
};

/**
 * `conversation:telegram:12345:2026-08-04T10:12:33Z` → `Telegram · 8/4 10:12`.
 *
 * Session ids carry colons of their own (`webchat:conn#agent:a#conv:x`), so the
 * timestamp is located by shape rather than by field position. Anything
 * unfamiliar passes through unchanged — this is a display helper on an audit
 * screen and must never throw.
 */
export function formatSource(source: string): string {
  const parts = source.split(':');
  if (parts[0] !== 'conversation' || parts.length < 3) return source;
  const channel = parts[1];
  const label = CHANNEL_LABELS[channel] ?? channel;
  const iso = source.slice(source.indexOf(`${channel}:`) + channel.length + 1);
  const tsStart = iso.search(/\d{4}-\d{2}-\d{2}T/);
  if (tsStart < 0) return label;
  const t = new Date(iso.slice(tsStart));
  if (Number.isNaN(t.getTime())) return label;
  const pad = (n: number) => String(n).padStart(2, '0');
  return `${label} · ${t.getMonth() + 1}/${t.getDate()} ${pad(t.getHours())}:${pad(t.getMinutes())}`;
}
