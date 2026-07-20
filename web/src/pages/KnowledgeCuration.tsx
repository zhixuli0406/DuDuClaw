import { useState, useEffect, useCallback, useMemo } from 'react';
import { useIntl } from 'react-intl';
import {
  Share2Icon,
  HistoryIcon,
  ShieldAlertIcon,
  SearchIcon,
  XIcon,
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
  Spinner,
} from '@/components/mds';
import { ConfirmDialog } from '@/components/settings/controls/ConfirmDialog';
import { MemoryGraph } from '@/components/MemoryGraph';
import {
  api,
  type MemoryGraphEdge,
  type MemoryGraphResult,
  type MemoryChainEntry,
  type ApprovalItem,
} from '@/lib/api';
import { timeAgo } from '@/lib/format';

type CurateTab = 'graph' | 'timeline' | 'queue';

/** A fact key that the graph tab can hand off to the timeline tab. */
interface FactKey {
  subject: string;
  predicate: string;
}

/**
 * KnowledgeCuration — the D6 HITL knowledge-curation station, mounted as a view
 * inside KnowledgeHubPage. Three sub-tabs: the SPO 知識圖譜 (force-directed
 * viewer + provenance panel), 事實歷史 (supersession timeline), and 待審知識
 * (quarantine review queue). All copy is end-user facing zh-TW — no internal
 * terms (origin_trust / PPR / quarantined) leak into the UI.
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
        <TabsTab value="queue">{intl.formatMessage({ id: 'curate.tab.queue' })}</TabsTab>
      </TabsList>

      <TabsPanel value="graph">
        <GraphTab agentId={agentId} onOpenHistory={openHistory} />
      </TabsPanel>
      <TabsPanel value="timeline">
        <TimelineTab agentId={agentId} pinnedFact={pinnedFact} />
      </TabsPanel>
      <TabsPanel value="queue">
        <QueueTab agentId={agentId} />
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

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setSelected(null);
    api.memory.graph(agentId).then((res) => {
      if (!cancelled) setGraph(res);
    }).catch(() => {
      if (!cancelled) setGraph({ nodes: [], edges: [], truncated: false });
    }).finally(() => {
      if (!cancelled) setLoading(false);
    });
    return () => { cancelled = true; };
  }, [agentId]);

  if (loading && !graph) {
    return <div className="flex justify-center py-16"><Spinner /></div>;
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
  );
}

function ProvenancePanel({
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

  const runQuery = useCallback(async (subj: string, pred: string) => {
    if (!subj.trim() || !pred.trim()) return;
    setLoading(true);
    try {
      const res = await api.memory.history(agentId, { subject: subj.trim(), predicate: pred.trim() });
      setChain(res.chain);
    } catch {
      setChain([]);
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
            onKeyDown={(e) => e.key === 'Enter' && runQuery(subject, predicate)}
            placeholder={intl.formatMessage({ id: 'curate.timeline.placeholder.subject' })}
          />
        </label>
        <label className="flex-1 space-y-1">
          <span className="text-xs text-muted-foreground">{intl.formatMessage({ id: 'curate.timeline.predicate' })}</span>
          <Input
            value={predicate}
            onChange={(e) => setPredicate(e.target.value)}
            onKeyDown={(e) => e.key === 'Enter' && runQuery(subject, predicate)}
            placeholder={intl.formatMessage({ id: 'curate.timeline.placeholder.predicate' })}
          />
        </label>
        <Button variant="brand" onClick={() => runQuery(subject, predicate)} disabled={loading}>
          <SearchIcon />
          {intl.formatMessage({ id: 'curate.timeline.load' })}
        </Button>
      </div>

      {loading ? (
        <div className="flex justify-center py-16"><Spinner /></div>
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

// ── Queue tab ───────────────────────────────────────────────

interface QuarantinePayload {
  origin?: string;
  subject?: string;
  quarantined_ids?: string[];
  memory_db?: string;
}

function QueueTab({ agentId }: { agentId: string }) {
  const intl = useIntl();
  const [items, setItems] = useState<ApprovalItem[] | null>(null);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [purgeTarget, setPurgeTarget] = useState<ApprovalItem | null>(null);
  const [error, setError] = useState('');

  const fetchQueue = useCallback(async () => {
    try {
      const res = await api.approvals.list(agentId, 'knowledge_quarantine');
      setItems(res.approvals);
    } catch {
      setItems([]);
    }
  }, [agentId]);

  useEffect(() => { fetchQueue(); }, [fetchQueue]);

  // Optimistic remove + refetch after a decision.
  const decide = useCallback(async (item: ApprovalItem, approve: boolean, reason?: string) => {
    setBusyId(item.id);
    setError('');
    setItems((prev) => prev?.filter((i) => i.id !== item.id) ?? prev);
    try {
      await api.approvals.decide(item.id, approve, reason);
    } catch {
      setError(intl.formatMessage({ id: 'curate.queue.actionFailed' }));
    } finally {
      setBusyId(null);
      fetchQueue();
    }
  }, [fetchQueue, intl]);

  const purgeOrigin = useCallback(async (item: ApprovalItem) => {
    const payload = item.payload as QuarantinePayload;
    const origin = payload?.origin;
    setBusyId(item.id);
    setError('');
    setItems((prev) => prev?.filter((i) => i.id !== item.id) ?? prev);
    try {
      if (origin) await api.memory.invalidateOrigin(agentId, origin);
      await api.approvals.decide(item.id, false, 'purge origin');
    } catch {
      setError(intl.formatMessage({ id: 'curate.queue.actionFailed' }));
    } finally {
      setBusyId(null);
      setPurgeTarget(null);
      fetchQueue();
    }
  }, [agentId, fetchQueue, intl]);

  if (items === null) {
    return <div className="flex justify-center py-16"><Spinner /></div>;
  }

  if (items.length === 0) {
    return <Empty icon={ShieldAlertIcon} title={intl.formatMessage({ id: 'curate.queue.empty' })} />;
  }

  return (
    <div className="space-y-3">
      {error && <p className="text-sm text-destructive">{error}</p>}
      {items.map((item) => {
        const payload = item.payload as QuarantinePayload;
        const count = payload?.quarantined_ids?.length ?? 0;
        return (
          <Card key={item.id} data-size="sm" className="border-warning/40">
            <CardContent className="space-y-3">
              <div className="flex items-start gap-2">
                <ShieldAlertIcon className="mt-0.5 size-4 shrink-0 text-warning" />
                <p className="text-sm text-foreground">{item.summary}</p>
              </div>
              <div className="flex flex-wrap gap-x-4 gap-y-1 text-xs text-muted-foreground">
                {payload?.origin && (
                  <span>{intl.formatMessage({ id: 'curate.queue.source' })}：<span className="text-foreground">{payload.origin}</span></span>
                )}
                {payload?.subject && (
                  <span>{intl.formatMessage({ id: 'curate.queue.subject' })}：<span className="text-foreground">{payload.subject}</span></span>
                )}
                {count > 0 && (
                  <span>{intl.formatMessage({ id: 'curate.queue.count' })}：<span className="font-mono tabular-nums text-foreground">{count}</span></span>
                )}
                <span className="ml-auto">{timeAgo(item.created_at)}</span>
              </div>
              <div className="flex flex-wrap gap-2">
                <Button variant="brand" size="sm" disabled={busyId === item.id} onClick={() => decide(item, true)}>
                  {intl.formatMessage({ id: 'curate.queue.approve' })}
                </Button>
                <Button variant="outline" size="sm" disabled={busyId === item.id} onClick={() => decide(item, false)}>
                  {intl.formatMessage({ id: 'curate.queue.reject' })}
                </Button>
                <Button
                  variant="destructive"
                  size="sm"
                  disabled={busyId === item.id || !payload?.origin}
                  onClick={() => setPurgeTarget(item)}
                >
                  {intl.formatMessage({ id: 'curate.queue.purge' })}
                </Button>
              </div>
            </CardContent>
          </Card>
        );
      })}

      <ConfirmDialog
        open={!!purgeTarget}
        onClose={() => setPurgeTarget(null)}
        onConfirm={() => purgeTarget && purgeOrigin(purgeTarget)}
        title={intl.formatMessage({ id: 'curate.queue.purge.confirmTitle' })}
        message={intl.formatMessage(
          { id: 'curate.queue.purge.confirmMsg' },
          { origin: (purgeTarget?.payload as QuarantinePayload)?.origin ?? '' },
        )}
        confirmLabel={intl.formatMessage({ id: 'curate.queue.purge.confirmBtn' })}
        busy={busyId === purgeTarget?.id}
      />
    </div>
  );
}
