import { useCallback, useEffect, useMemo, useState, type ReactNode } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate } from 'react-router';
import {
  Inbox as InboxIcon,
  SlidersHorizontal,
  CheckCheck,
  Undo2,
  ArrowLeft,
  Check,
  ExternalLink,
} from 'lucide-react';
import { api, type ApprovalItem, type TaskInfo, type DecisionInfo, type InstallRequestInfo } from '@/lib/api';
import { useConnectionStore } from '@/stores/connection-store';
import { useApprovalsStore } from '@/stores/approvals-store';
import { toast, formatError } from '@/lib/toast';
import { cn } from '@/lib/utils';
import {
  PageHeader,
  Button,
  Badge,
  Empty,
  Skeleton,
  ActorAvatar,
  ResizablePanelGroup,
  ResizablePanel,
  ResizableHandle,
  DropdownMenu,
  DropdownMenuTrigger,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  useIsMobile,
} from '@/components/mds';
import { InboxList, type InboxGroup } from '@/components/inbox/InboxList';
import { ApprovalDetailPanel } from '@/components/inbox/ApprovalDetailPanel';
import { TYPE_META } from '@/components/inbox/meta';
import type { InboxRowLabels } from '@/components/inbox/InboxRow';
import {
  approvalRisk,
  readApprovedToday,
  bumpApprovedToday,
  similarBatches,
  FATIGUE_NUDGE_THRESHOLD,
  type RiskLevel,
} from '@/lib/approval-risk';
import {
  type InboxItem,
  type InboxTab,
  type InboxGroupBy,
  type InboxSortBy,
  type InboxItemType,
  type InboxPrefs,
  INBOX_TABS,
  TYPE_URGENCY,
  BLOCKED_BUCKET_ORDER,
  blockedBucket,
  groupKeyOf,
  filterByTab,
  filterByCategory,
  filterByStatus,
  distinctStatuses,
  excludeArchived,
  sortInbox,
  withId,
  withoutId,
  loadIdSet,
  persistIdSet,
  loadPrefs,
  persistPrefs,
  READ_KEY,
  ARCHIVED_KEY,
} from '@/lib/inbox-model';

/** How many agents to poll for open decisions (best-effort, capped). */
const DECISION_AGENT_CAP = 12;
/** Max concurrent per-agent decision RPCs — the decisions poll is deferred and
 *  chunked so opening the Inbox no longer fires ~17 RPCs at once (Bug#2). */
const DECISION_POLL_CONCURRENCY = 4;
/** Cap on failed-run rows pulled from the unified audit log. */
const FAILED_RUN_CAP = 30;

const GROUP_OPTIONS: InboxGroupBy[] = ['none', 'type', 'agent', 'channel'];
const SORT_OPTIONS: InboxSortBy[] = ['urgency', 'time', 'stuck'];
const CATEGORY_OPTIONS: (InboxItemType | 'all')[] = ['all', 'approval', 'install', 'decision', 'blocked', 'budget', 'failed_run'];

/** Split an array into fixed-size chunks (for concurrency-capped polling). */
function chunked<T>(arr: readonly T[], size: number): T[][] {
  const out: T[][] = [];
  for (let i = 0; i < arr.length; i += size) out.push(arr.slice(i, i + size));
  return out;
}

interface RawEntry {
  item: InboxItem;
  /** Original source payload for running the action. */
  raw: unknown;
}

export function InboxPage() {
  const intl = useIntl();
  const t = useCallback((id: string) => intl.formatMessage({ id }), [intl]);
  const navigate = useNavigate();
  const isMobile = useIsMobile();
  const connectionState = useConnectionStore((s) => s.state);
  const setPendingCount = useApprovalsStore((s) => s.setPendingCount);

  const [entries, setEntries] = useState<RawEntry[]>([]);
  const [agentNames, setAgentNames] = useState<Record<string, string>>({});
  const [loading, setLoading] = useState(true);
  const [prefs, setPrefs] = useState<InboxPrefs>(loadPrefs);
  const [read, setRead] = useState<ReadonlySet<string>>(() => loadIdSet(READ_KEY));
  const [archived, setArchived] = useState<ReadonlySet<string>>(() => loadIdSet(ARCHIVED_KEY));
  const [undoStack, setUndoStack] = useState<RawEntry[]>([]);
  // The open item in the detail pane (split layout).
  const [selectedId, setSelectedId] = useState<string | null>(null);
  // Fatigue protection (arXiv:2606.08919): today's approval volume, surfaced
  // (not enforced) so a tired operator notices before rubber-stamping.
  const [approvedToday, setApprovedToday] = useState<number>(() => readApprovedToday());

  const updatePrefs = useCallback((patch: Partial<InboxPrefs>) => {
    setPrefs((p) => {
      const next = { ...p, ...patch };
      persistPrefs(next);
      return next;
    });
  }, []);

  const agentName = useCallback((id: string) => agentNames[id] ?? id, [agentNames]);

  // ── Load: six aggregate sources merged for the first paint, then a deferred,
  // concurrency-capped per-agent decisions poll. Each source is best-effort (a
  // manager-gated source that errors for this viewer contributes nothing —
  // fail-safe, not fail-loud). Splitting the decisions poll out of the initial
  // burst keeps the Inbox from firing ~17 RPCs the moment it opens (Bug#2).
  const load = useCallback(async () => {
    const [approvalsRes, budgetRes, tasksRes, agentsRes, failedRes, installRes] = await Promise.all([
      api.approvals.list().catch(() => null),
      api.budget.incidents().catch(() => null),
      api.tasks.list({ status: 'blocked' }).catch(() => null),
      api.agents.list().catch(() => null),
      api.audit.unifiedLog({ sources: ['channel_failure'], limit: FAILED_RUN_CAP }).catch(() => null),
      // Install approval requests actionable by this viewer (manager/admin).
      // Employees get 403 → null and simply see no install rows (Bug#3).
      api.installRequests.list().catch(() => null),
    ]);

    const nameMap: Record<string, string> = {};
    for (const a of agentsRes?.agents ?? []) nameMap[a.name] = a.display_name || a.name;
    setAgentNames(nameMap);

    const merged: RawEntry[] = [];

    for (const a of approvalsRes?.approvals ?? []) {
      merged.push({
        raw: a,
        item: {
          id: `approval:${a.id}`,
          type: 'approval',
          title: a.summary,
          agentId: a.agent_id,
          timestamp: a.created_at,
          urgency: TYPE_URGENCY.approval,
          actionable: true,
          status: 'pending',
          risk: approvalRisk(a.kind, a.payload),
        },
      });
    }

    for (const req of installRes?.requests ?? []) {
      merged.push({
        raw: req,
        item: {
          id: `install:${req.id}`,
          type: 'install',
          title: req.title,
          timestamp: req.created_at,
          urgency: TYPE_URGENCY.install,
          actionable: true,
          status: req.stage,
        },
      });
    }

    for (const task of tasksRes?.tasks ?? []) {
      merged.push({
        raw: task,
        item: {
          id: `blocked:${task.id}`,
          type: 'blocked',
          title: task.title,
          agentId: task.assigned_to || undefined,
          timestamp: task.updated_at,
          urgency: TYPE_URGENCY.blocked,
          actionable: true,
          status: task.status,
        },
      });
    }

    for (const inc of budgetRes?.incidents ?? []) {
      merged.push({
        raw: inc,
        item: {
          id: `budget:${inc.agent_id}:${inc.ts}`,
          type: 'budget',
          title: intl.formatMessage({ id: 'inbox.budget.title' }, { agent: nameMap[inc.agent_id] ?? inc.agent_id, scope: inc.scope }),
          agentId: inc.agent_id,
          timestamp: inc.ts,
          urgency: TYPE_URGENCY.budget,
          actionable: true,
          status: inc.event,
        },
      });
    }

    for (const ev of failedRes?.events ?? []) {
      const ch = typeof ev.details?.channel === 'string' ? (ev.details.channel as string) : undefined;
      merged.push({
        raw: ev,
        item: {
          id: `failed_run:${ev.agent_id}:${ev.timestamp}`,
          type: 'failed_run',
          title: ev.summary || intl.formatMessage({ id: 'inbox.failedRun.title' }, { agent: nameMap[ev.agent_id] ?? ev.agent_id }),
          agentId: ev.agent_id || undefined,
          channel: ch,
          timestamp: ev.timestamp,
          urgency: TYPE_URGENCY.failed_run,
          actionable: false,
          status: ev.severity,
        },
      });
    }

    // Paint the aggregate sources immediately so the list is usable without
    // waiting on the per-agent decisions poll below.
    setEntries(merged);
    setLoading(false);

    // Decisions require a per-agent call — poll a capped set of agents in
    // small concurrency-limited waves (rather than one N-wide burst) and fold
    // the results into the already-painted list.
    const agentIds = (agentsRes?.agents ?? []).slice(0, DECISION_AGENT_CAP).map((a) => a.name);
    const decisionEntries: RawEntry[] = [];
    for (const wave of chunked(agentIds, DECISION_POLL_CONCURRENCY)) {
      const results = await Promise.all(
        wave.map((name) =>
          api.decisions
            .list(name, 10)
            .then((r) => ({ name, decisions: r?.decisions ?? [] }))
            .catch(() => ({ name, decisions: [] as DecisionInfo[] })),
        ),
      );
      for (const { name, decisions } of results) {
        for (const d of decisions) {
          decisionEntries.push({
            raw: { agentId: name, decision: d },
            item: {
              id: `decision:${name}:${d.id}`,
              type: 'decision',
              title: d.question,
              agentId: name,
              timestamp: d.created_at ?? undefined,
              urgency: TYPE_URGENCY.decision,
              actionable: true,
              status: 'open',
            },
          });
        }
      }
    }
    if (decisionEntries.length > 0) {
      setEntries([...merged, ...decisionEntries]);
    }
  }, [intl]);

  useEffect(() => {
    if (connectionState !== 'authenticated') return;
    setLoading(true);
    load().finally(() => setLoading(false));
  }, [connectionState, load]);

  const items = useMemo(() => entries.map((e) => e.item), [entries]);
  const nonArchived = useMemo(() => excludeArchived(items, archived), [items, archived]);

  // Pending badge = actionable, non-archived items.
  useEffect(() => {
    setPendingCount(nonArchived.filter((i) => i.actionable).length);
  }, [nonArchived, setPendingCount]);

  const findEntry = useCallback((id: string) => entries.find((e) => e.item.id === id), [entries]);

  // ── State mutators ──────────────────────────────────────────────────────────
  const markRead = useCallback((id: string) => {
    setRead((prev) => {
      if (prev.has(id)) return prev;
      const next = withId(prev, id);
      persistIdSet(READ_KEY, next);
      return next;
    });
  }, []);

  const markUnread = useCallback((id: string) => {
    setRead((prev) => {
      if (!prev.has(id)) return prev;
      const next = withoutId(prev, id);
      persistIdSet(READ_KEY, next);
      return next;
    });
  }, []);

  const archive = useCallback(
    (item: InboxItem) => {
      const entry = findEntry(item.id);
      if (!entry) return;
      setArchived((prev) => {
        const next = withId(prev, item.id);
        persistIdSet(ARCHIVED_KEY, next);
        return next;
      });
      setUndoStack((s) => [entry, ...s].slice(0, 20));
      setSelectedId((cur) => (cur === item.id ? null : cur));
      toast.success(t('inbox.archivedToast'));
      // Decisions have a server-side dismiss; other types are local-archive only.
      if (item.type === 'decision') {
        const raw = entry.raw as { agentId: string; decision: { id: string } };
        api.decisions.dismiss(raw.agentId, raw.decision.id).catch((e) => console.warn('[api]', e));
      }
    },
    [findEntry, t],
  );

  const undo = useCallback(() => {
    setUndoStack((s) => {
      if (s.length === 0) return s;
      const [restored, ...rest] = s;
      setArchived((prev) => {
        const next = withoutId(prev, restored.item.id);
        persistIdSet(ARCHIVED_KEY, next);
        return next;
      });
      return rest;
    });
  }, []);

  // Remove a decided approval from the queue and close its detail. Archiving (vs.
  // deleting) keeps the id out of every tab; undo deliberately can't cheaply
  // resurrect a server-decided item.
  const markDecided = useCallback((id: string) => {
    setArchived((prev) => {
      const next = withId(prev, id);
      persistIdSet(ARCHIVED_KEY, next);
      return next;
    });
    setSelectedId((cur) => (cur === id ? null : cur));
  }, []);

  const decide = useCallback(
    async (item: InboxItem, approve: boolean) => {
      const entry = findEntry(item.id);
      if (!entry) return;
      const a = entry.raw as ApprovalItem;
      try {
        await api.approvals.decide(a.id, approve); // side_effect field ignored
        if (approve) setApprovedToday(bumpApprovedToday());
        toast.success(
          approve
            ? intl.formatMessage({ id: 'approvals.approvedToast' }, { summary: a.summary })
            : t('inbox.approval.rejectedToast'),
        );
        markDecided(item.id);
      } catch (e) {
        toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
      }
    },
    [findEntry, intl, t, markDecided],
  );

  // Open a row in the detail pane (marks it read).
  const select = useCallback(
    (item: InboxItem) => {
      markRead(item.id);
      setSelectedId(item.id);
    },
    [markRead],
  );

  const markAllRead = useCallback(() => {
    setRead((prev) => {
      let next = prev;
      for (const it of nonArchived) if (!next.has(it.id)) next = withId(next, it.id);
      persistIdSet(READ_KEY, next);
      return next;
    });
  }, [nonArchived]);

  const isUnread = useCallback((item: InboxItem) => !read.has(item.id), [read]);

  // ── Tab population + grouping ────────────────────────────────────────────────
  const tabItems = useMemo(
    () => filterByTab(nonArchived, prefs.tab, { readIds: read }),
    [nonArchived, prefs.tab, read],
  );
  const filtered = useMemo(() => {
    if (prefs.tab !== 'all') return tabItems;
    return filterByStatus(filterByCategory(tabItems, prefs.categoryFilter), prefs.statusFilter);
  }, [tabItems, prefs.tab, prefs.categoryFilter, prefs.statusFilter]);
  const statuses = useMemo(() => distinctStatuses(tabItems), [tabItems]);
  const sorted = useMemo(() => sortInbox(filtered, prefs.sortBy), [filtered, prefs.sortBy]);

  const groupLabel = useCallback(
    (key: string, by: InboxGroupBy, sample: InboxItem): string => {
      if (by === 'type') return t(TYPE_META[sample.type].labelKey);
      if (by === 'agent') return key === '—' ? t('inbox.group.agent') : agentName(key);
      if (by === 'channel') return key === '—' ? t('inbox.group.channel') : key;
      return key;
    },
    [t, agentName],
  );

  const groups = useMemo<InboxGroup[]>(() => {
    if (prefs.tab === 'blocked') {
      const buckets: Record<string, InboxItem[]> = { decide: [], input: [], attention: [] };
      for (const it of sorted) buckets[blockedBucket(it)].push(it);
      return BLOCKED_BUCKET_ORDER.filter((b) => buckets[b].length).map((b) => ({
        key: b,
        label: t(`inbox.blocked.${b}`),
        hint: t(`inbox.blocked.${b}Hint`),
        items: buckets[b],
      }));
    }
    if (prefs.groupBy === 'none') {
      return [{ key: '', items: sorted }];
    }
    const map = new Map<string, InboxItem[]>();
    for (const it of sorted) {
      const k = groupKeyOf(it, prefs.groupBy);
      const arr = map.get(k);
      if (arr) arr.push(it);
      else map.set(k, [it]);
    }
    return [...map.entries()].map(([k, its]) => ({
      key: k,
      label: groupLabel(k, prefs.groupBy, its[0]),
      items: its,
    }));
  }, [sorted, prefs.tab, prefs.groupBy, t, groupLabel]);

  const rowLabels: InboxRowLabels = useMemo(
    () => ({
      typeLabel: (item) => t(TYPE_META[item.type].labelKey),
      riskLabel: (level: RiskLevel) => t(`approval.risk.${level}`),
      archive: t('inbox.action.archive'),
    }),
    [t],
  );

  // ── Fatigue signals (arXiv:2606.08919) ──────────────────────────────────────
  const approvalKinds = useMemo(
    () => entries.filter((e) => e.item.type === 'approval').map((e) => (e.raw as ApprovalItem).kind),
    [entries],
  );
  const batches = useMemo(() => similarBatches(approvalKinds), [approvalKinds]);
  const approvalKindLabel = useCallback(
    (kind: string) => {
      const key = `approvals.kind.${kind}`;
      const label = t(key);
      return label === key ? t('approvals.kind.unknown') : label;
    },
    [t],
  );

  const tabItemsFor = useCallback(
    (tab: InboxTab) => filterByTab(nonArchived, tab, { readIds: read }),
    [nonArchived, read],
  );

  const canArchive = prefs.tab === 'mine';

  // Keep the selection valid as the visible set changes.
  useEffect(() => {
    if (selectedId && !sorted.some((it) => it.id === selectedId)) setSelectedId(null);
  }, [sorted, selectedId]);

  const selectedEntry = selectedId ? findEntry(selectedId) : undefined;

  // ── Detail pane body ─────────────────────────────────────────────────────────
  const detailBody = useMemo<ReactNode>(() => {
    if (!selectedEntry) return null;
    const { item, raw } = selectedEntry;
    const typeLabel = t(TYPE_META[item.type].labelKey);
    switch (item.type) {
      case 'approval':
        return (
          <ApprovalDetailPanel
            approval={raw as ApprovalItem}
            agentName={agentName((raw as ApprovalItem).agent_id)}
            onApprove={() => decide(item, true)}
            onReject={() => decide(item, false)}
            onDecided={() => markDecided(item.id)}
          />
        );
      case 'install': {
        const req = raw as InstallRequestInfo;
        return (
          <DetailShell item={item} typeLabel={typeLabel}>
            <div className="space-y-1 text-sm text-muted-foreground">
              <p>{intl.formatMessage({ id: 'inbox.install.requester' }, { email: req.requester_email })}</p>
              {req.requester_department && (
                <p>{intl.formatMessage({ id: 'inbox.install.dept' }, { dept: req.requester_department })}</p>
              )}
              {req.description && <p className="text-foreground">{req.description}</p>}
            </div>
            <Button variant="brand" onClick={() => navigate('/approvals')}>
              <ExternalLink />
              {t('inbox.detail.reviewInstall')}
            </Button>
          </DetailShell>
        );
      }
      case 'blocked': {
        const task = raw as TaskInfo;
        return (
          <DetailShell item={item} typeLabel={typeLabel} agentName={item.agentId ? agentName(item.agentId) : undefined}>
            {task.blocked_reason && (
              <p className="rounded-lg bg-destructive/10 px-3 py-2 text-sm text-destructive">{task.blocked_reason}</p>
            )}
            <Button variant="brand" onClick={() => navigate(`/tasks/${task.id}`)}>
              <ExternalLink />
              {t('inbox.detail.viewTask')}
            </Button>
          </DetailShell>
        );
      }
      case 'budget':
        return (
          <DetailShell item={item} typeLabel={typeLabel} agentName={item.agentId ? agentName(item.agentId) : undefined}>
            <Button variant="brand" onClick={() => navigate('/manage/billing')}>
              <ExternalLink />
              {t('inbox.detail.viewBilling')}
            </Button>
          </DetailShell>
        );
      case 'decision':
        return (
          <DetailShell item={item} typeLabel={typeLabel} agentName={item.agentId ? agentName(item.agentId) : undefined}>
            <div className="flex flex-wrap items-center gap-2">
              <Button variant="brand" onClick={() => navigate('/agents')}>
                <ExternalLink />
                {t('inbox.detail.viewAgent')}
              </Button>
              <Button variant="outline" onClick={() => archive(item)}>
                <Check />
                {t('inbox.detail.dismiss')}
              </Button>
            </div>
          </DetailShell>
        );
      case 'failed_run':
        return (
          <DetailShell item={item} typeLabel={typeLabel} agentName={item.agentId ? agentName(item.agentId) : undefined}>
            <pre className="max-h-96 overflow-auto rounded-lg bg-muted p-2 text-[11px] leading-relaxed text-muted-foreground">
              {JSON.stringify(raw, null, 2)}
            </pre>
          </DetailShell>
        );
    }
  }, [selectedEntry, agentName, decide, markDecided, navigate, archive, t, intl]);

  // ── Left column: header + tabs + list ────────────────────────────────────────
  const listColumn = (
    <div className="flex h-full min-h-0 flex-col">
      <PageHeader hideTrigger>
        <InboxIcon className="size-4 shrink-0 text-muted-foreground" />
        <h1 className="truncate text-sm font-medium">{t('inbox.title')}</h1>
        <span className="font-mono text-xs tabular-nums text-muted-foreground">{nonArchived.length}</span>
        <div className="ml-auto flex items-center gap-1">
          {undoStack.length > 0 && (
            <Button variant="ghost" size="icon-sm" onClick={undo} title={t('inbox.undo')} aria-label={t('inbox.undo')}>
              <Undo2 />
            </Button>
          )}
          <Button variant="ghost" size="icon-sm" onClick={markAllRead} title={t('inbox.markAllRead')} aria-label={t('inbox.markAllRead')}>
            <CheckCheck />
          </Button>
          <DropdownMenu>
            <DropdownMenuTrigger
              render={
                <Button variant="ghost" size="icon-sm" aria-label={t('inbox.group.label')}>
                  <SlidersHorizontal />
                </Button>
              }
            />
            <DropdownMenuContent className="min-w-44">
              <DropdownMenuLabel>{t('inbox.group.label')}</DropdownMenuLabel>
              {GROUP_OPTIONS.map((g) => (
                <DropdownMenuItem
                  key={g}
                  disabled={prefs.tab === 'blocked'}
                  onClick={() => updatePrefs({ groupBy: g })}
                  className={cn(prefs.groupBy === g && 'font-medium text-foreground')}
                >
                  <span className="flex-1">{t(`inbox.group.${g}`)}</span>
                  {prefs.groupBy === g && <Check className="size-3.5 text-brand" />}
                </DropdownMenuItem>
              ))}
              <DropdownMenuSeparator />
              <DropdownMenuLabel>{t('inbox.sort.label')}</DropdownMenuLabel>
              {SORT_OPTIONS.map((s) => (
                <DropdownMenuItem
                  key={s}
                  onClick={() => updatePrefs({ sortBy: s })}
                  className={cn(prefs.sortBy === s && 'font-medium text-foreground')}
                >
                  <span className="flex-1">{t(`inbox.sort.${s}`)}</span>
                  {prefs.sortBy === s && <Check className="size-3.5 text-brand" />}
                </DropdownMenuItem>
              ))}
              {prefs.tab === 'all' && (
                <>
                  <DropdownMenuSeparator />
                  <DropdownMenuLabel>{t('inbox.filter.category')}</DropdownMenuLabel>
                  {CATEGORY_OPTIONS.map((c) => (
                    <DropdownMenuItem
                      key={c}
                      onClick={() => updatePrefs({ categoryFilter: c })}
                      className={cn(prefs.categoryFilter === c && 'font-medium text-foreground')}
                    >
                      <span className="flex-1">{c === 'all' ? t('inbox.filter.all') : t(`inbox.type.${c}`)}</span>
                      {prefs.categoryFilter === c && <Check className="size-3.5 text-brand" />}
                    </DropdownMenuItem>
                  ))}
                  {statuses.length > 0 && (
                    <>
                      <DropdownMenuLabel>{t('inbox.filter.status')}</DropdownMenuLabel>
                      <DropdownMenuItem
                        onClick={() => updatePrefs({ statusFilter: 'all' })}
                        className={cn(prefs.statusFilter === 'all' && 'font-medium text-foreground')}
                      >
                        <span className="flex-1">{t('inbox.filter.all')}</span>
                        {prefs.statusFilter === 'all' && <Check className="size-3.5 text-brand" />}
                      </DropdownMenuItem>
                      {statuses.map((s) => (
                        <DropdownMenuItem
                          key={s}
                          onClick={() => updatePrefs({ statusFilter: s })}
                          className={cn(prefs.statusFilter === s && 'font-medium text-foreground')}
                        >
                          <span className="flex-1 truncate">{s}</span>
                          {prefs.statusFilter === s && <Check className="size-3.5 text-brand" />}
                        </DropdownMenuItem>
                      ))}
                    </>
                  )}
                </>
              )}
            </DropdownMenuContent>
          </DropdownMenu>
        </div>
      </PageHeader>

      {/* Tabs — five scopes with counts. */}
      <div className="flex shrink-0 items-center gap-1 overflow-x-auto border-b border-surface-border px-2 py-1.5">
        {INBOX_TABS.map((tab) => {
          const count = tabItemsFor(tab).length;
          const active = prefs.tab === tab;
          return (
            <button
              key={tab}
              type="button"
              onClick={() => updatePrefs({ tab })}
              aria-pressed={active}
              className={cn(
                'flex shrink-0 items-center gap-1 rounded-md px-2 py-1 text-xs font-medium transition-colors',
                active ? 'bg-secondary text-foreground' : 'text-muted-foreground hover:text-foreground hover:bg-surface-hover',
              )}
            >
              {t(`inbox.tab.${tab}`)}
              {count > 0 && <span className="font-mono tabular-nums text-muted-foreground/70">{count}</span>}
            </button>
          );
        })}
      </div>

      {/* Fatigue hint (compact). */}
      {(approvedToday > 0 || batches.length > 0) && (
        <div className="flex flex-wrap items-center gap-x-2 gap-y-1 border-b border-surface-border px-3 py-1.5 text-[11px] text-muted-foreground" role="status">
          {approvedToday > 0 && (
            <span className={cn(approvedToday >= FATIGUE_NUDGE_THRESHOLD && 'font-medium text-warning')}>
              {intl.formatMessage({ id: 'approval.fatigue.today' }, { count: approvedToday })}
              {approvedToday >= FATIGUE_NUDGE_THRESHOLD && ` · ${t('approval.fatigue.nudge')}`}
            </span>
          )}
          {batches.map((b) => (
            <span key={b.kind} className="rounded bg-muted px-1.5 py-0.5">
              {intl.formatMessage({ id: 'approval.batch.hint' }, { count: b.count, kind: approvalKindLabel(b.kind) })}
            </span>
          ))}
        </div>
      )}

      {/* List */}
      <div className="min-h-0 flex-1 overflow-y-auto px-2 py-1">
        {loading ? (
          <div className="space-y-2 p-2">
            {Array.from({ length: 5 }).map((_, i) => (
              <Skeleton key={i} className="h-10 w-full" />
            ))}
          </div>
        ) : (
          <InboxList
            groups={groups}
            canArchive={canArchive}
            agentName={agentName}
            labels={rowLabels}
            selectedId={selectedId}
            isUnread={isUnread}
            onSelect={select}
            onArchive={archive}
            onUnread={(item) => markUnread(item.id)}
            onUndo={undo}
            emptyState={<Empty icon={InboxIcon} title={t('inbox.emptyTab')} variant="dashed" className="mt-6" />}
          />
        )}
      </div>
      {/* Keyboard shortcuts (j/k/Enter…) are meaningless on touch devices —
          hide the hint below the md breakpoint / on the mobile layout (#8). */}
      <p className="hidden shrink-0 border-t border-surface-border px-3 py-1.5 text-[11px] text-muted-foreground/70 md:block">
        {t('inbox.keyboardHint')}
      </p>
    </div>
  );

  // ── Detail column ────────────────────────────────────────────────────────────
  const detailColumn = (
    <div className="flex h-full min-h-0 flex-col">
      {isMobile && selectedEntry && (
        <div className="flex h-12 shrink-0 items-center gap-2 border-b border-surface-border px-2">
          <Button variant="ghost" size="icon-sm" onClick={() => setSelectedId(null)} aria-label={t('common.back')}>
            <ArrowLeft />
          </Button>
          <span className="truncate text-sm font-medium">{selectedEntry.item.title}</span>
        </div>
      )}
      {selectedEntry ? (
        <div className="min-h-0 flex-1 overflow-y-auto p-4 md:p-6">{detailBody}</div>
      ) : (
        <div className="flex h-full flex-col items-center justify-center gap-3 p-6 text-center">
          <InboxIcon className="size-10 text-muted-foreground/30" />
          <p className="text-sm text-muted-foreground">{t('inbox.detail.empty')}</p>
        </div>
      )}
    </div>
  );

  // ── Whole-inbox empty (nothing in any tab, not loading). ─────────────────────
  const totallyEmpty = !loading && nonArchived.length === 0;

  return (
    <div className="-mx-4 -mt-4 flex min-h-0 flex-1 md:-mx-6 md:-mt-6 md:-mb-6">
      {isMobile ? (
        selectedEntry ? (
          detailColumn
        ) : totallyEmpty ? (
          <div className="flex h-full w-full flex-col">
            {listColumn}
          </div>
        ) : (
          <div className="w-full">{listColumn}</div>
        )
      ) : (
        <ResizablePanelGroup orientation="horizontal" id="inbox-split" className="h-full w-full">
          <ResizablePanel defaultSize={320} minSize={240} maxSize={480} className="border-r border-surface-border">
            {listColumn}
          </ResizablePanel>
          <ResizableHandle />
          <ResizablePanel minSize="40">{detailColumn}</ResizablePanel>
        </ResizablePanelGroup>
      )}
    </div>
  );
}

/** Shared detail scaffold for the non-approval item types. */
function DetailShell({
  item,
  typeLabel,
  agentName,
  children,
}: {
  item: InboxItem;
  typeLabel: string;
  agentName?: string;
  children: ReactNode;
}) {
  const meta = TYPE_META[item.type];
  const Icon = meta.icon;
  return (
    <div className="space-y-4">
      <div className="flex items-start gap-3">
        {item.agentId ? (
          <ActorAvatar actorType="agent" size="lg" name={agentName ?? item.agentId} />
        ) : (
          <span className="grid size-8 shrink-0 place-items-center rounded-full bg-muted text-muted-foreground ring-1 ring-surface-border">
            <Icon className="size-4" />
          </span>
        )}
        <div className="min-w-0 flex-1 space-y-1">
          <div className="flex items-center gap-2">
            <Badge variant="secondary">{typeLabel}</Badge>
            {agentName && <span className="truncate text-xs text-muted-foreground">{agentName}</span>}
          </div>
          <h2 className="text-lg font-medium text-foreground">{item.title}</h2>
        </div>
      </div>
      {children}
    </div>
  );
}
