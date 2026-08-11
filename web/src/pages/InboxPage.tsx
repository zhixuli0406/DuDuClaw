import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate, useSearchParams } from 'react-router';
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
import { excludeRecoveredChannelFailures, channelFailureChannel } from '@/lib/home-overview';
import { toast, formatError } from '@/lib/toast';
import { cn } from '@/lib/utils';
import {
  PageHeader,
  Button,
  Empty,
  Skeleton,
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
import { InstallDetailPanel } from '@/components/inbox/InstallDetailPanel';
import { NeedsHumanTaskPanel } from '@/components/inbox/NeedsHumanTaskPanel';
import { DetailShell } from '@/components/inbox/DetailShell';
import { OpenInChannelButton } from '@/components/inbox/OpenInChannelButton';
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
  ACTION_QUEUE_TABS,
  blockedBucket,
  groupKeyOf,
  filterByTab,
  filterByCategory,
  filterByStatus,
  distinctStatuses,
  excludeArchived,
  excludeProcessed,
  sinkProcessed,
  statusLabelKey,
  sortInbox,
  withId,
  withoutId,
  loadIdSet,
  persistIdSet,
  loadPrefs,
  persistPrefs,
  READ_KEY,
  ARCHIVED_KEY,
  PROCESSED_KEY,
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

  // H5 deep link: `?item=<id>` selects + scrolls to a specific row on load
  // (captured once at mount — the effect below resolves it against the
  // loaded entries and never re-reads the URL after that).
  const [searchParams, setSearchParams] = useSearchParams();
  const deepLinkItemRef = useRef(searchParams.get('item'));
  const deepLinkAppliedRef = useRef(false);

  const [entries, setEntries] = useState<RawEntry[]>([]);
  const [agentNames, setAgentNames] = useState<Record<string, string>>({});
  const [loading, setLoading] = useState(true);
  const [prefs, setPrefs] = useState<InboxPrefs>(loadPrefs);
  const [read, setRead] = useState<ReadonlySet<string>>(() => loadIdSet(READ_KEY));
  const [archived, setArchived] = useState<ReadonlySet<string>>(() => loadIdSet(ARCHIVED_KEY));
  // §C6: a second, independent triage axis from `read` — has the user
  // actually resolved this item? Never hides it (that's `archived`); it
  // sinks to the bottom and dims instead.
  const [processed, setProcessed] = useState<ReadonlySet<string>>(() => loadIdSet(PROCESSED_KEY));
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
    const [approvalsRes, budgetRes, tasksRes, needsHumanRes, agentsRes, failedRes, installRes] = await Promise.all([
      api.approvals.list().catch(() => null),
      api.budget.incidents().catch(() => null),
      api.tasks.list({ status: 'blocked' }).catch(() => null),
      // W1-2: a goal-loop task escalated to needs_human is a distinct status
      // from plain `blocked` (task_store.rs) — the Inbox previously never
      // fetched it at all, so it never appeared here despite being exactly
      // the kind of "等你決定" item this page exists for (04 doc §D.6).
      api.tasks.list({ status: 'needs_human' }).catch(() => null),
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
          expiresAt: a.expires_at,
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
          // W0-9 parity: install requests carry the same TTL countdown shape
          // as approvals — surface it so the row's near-expiry marker (which
          // already exists, don't regress it) also fires for install rows.
          expiresAt: req.expires_at,
        },
      });
    }

    const blockedTasks = [...(tasksRes?.tasks ?? []), ...(needsHumanRes?.tasks ?? [])];
    for (const task of blockedTasks) {
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

    // W2-8: `channel_recovered` resolution rows must never appear as a
    // "沒送出去" item, and a failure a later recovery already resolved for
    // the same channel is dropped too (see `lib/home-overview.ts`).
    for (const ev of excludeRecoveredChannelFailures(failedRes?.events ?? [])) {
      merged.push({
        raw: ev,
        item: {
          id: `failed_run:${ev.agent_id}:${ev.timestamp}`,
          type: 'failed_run',
          title: ev.summary || intl.formatMessage({ id: 'inbox.failedRun.title' }, { agent: nameMap[ev.agent_id] ?? ev.agent_id }),
          agentId: ev.agent_id || undefined,
          channel: channelFailureChannel(ev),
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

  // Pending badge = actionable, non-archived, not-yet-processed items — a
  // decided approval shouldn't keep inflating the "needs you" count (§C6).
  useEffect(() => {
    setPendingCount(nonArchived.filter((i) => i.actionable && !processed.has(i.id)).length);
  }, [nonArchived, processed, setPendingCount]);

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

  // Mark a resolved item processed (§C6) and close its detail. Unlike
  // `archive`, this does NOT hide the row outright — it graduates out of the
  // action-required tabs (`ACTION_QUEUE_TABS`) but stays visible, sunk to the
  // bottom, everywhere else. There is no undo: a server-decided item can't be
  // cheaply un-decided, so this axis has no undo stack (unlike `archive`'s).
  const markProcessed = useCallback((id: string) => {
    setProcessed((prev) => {
      if (prev.has(id)) return prev;
      const next = withId(prev, id);
      persistIdSet(PROCESSED_KEY, next);
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
        markProcessed(item.id);
      } catch (e) {
        toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
      }
    },
    [findEntry, intl, t, markProcessed],
  );

  // Open a row in the detail pane (marks it read).
  const select = useCallback(
    (item: InboxItem) => {
      markRead(item.id);
      setSelectedId(item.id);
    },
    [markRead],
  );

  // H5 deep link resolution: once the first aggregate load settles, try to
  // match `?item=<id>` against the loaded entries and open it — same as
  // clicking the row. Matched with a suffix fallback (`endsWith(':' + id)`)
  // because a gateway-side push (`deep_link.rs`) only ever has the bare
  // `ApprovalRecord`/`InstallRequest` id, not the frontend's type-prefixed
  // `approval:<id>`/`install:<id>` form — a dashboard-originated link (e.g.
  // `NeedsAttentionList`) already passes the prefixed id, which matches
  // exactly. Runs at most once (`deepLinkAppliedRef`): a miss on the first
  // (aggregate) paint is not retried once the deferred decisions poll lands,
  // so it never fights a manual selection made in between.
  useEffect(() => {
    if (loading || deepLinkAppliedRef.current) return;
    deepLinkAppliedRef.current = true;
    const param = deepLinkItemRef.current;
    if (!param) return;
    const match = entries.find((e) => e.item.id === param || e.item.id.endsWith(`:${param}`));
    if (match) select(match.item);
  }, [loading, entries, select]);

  // Mirror the open item back into the URL so the current view is
  // bookmarkable/shareable (H5). `replace: true` avoids stacking a history
  // entry per row click.
  useEffect(() => {
    setSearchParams(
      (prev) => {
        const next = new URLSearchParams(prev);
        if (selectedId) next.set('item', selectedId);
        else next.delete('item');
        return next;
      },
      { replace: true },
    );
  }, [selectedId, setSearchParams]);

  const markAllRead = useCallback(() => {
    setRead((prev) => {
      let next = prev;
      for (const it of nonArchived) if (!next.has(it.id)) next = withId(next, it.id);
      persistIdSet(READ_KEY, next);
      return next;
    });
  }, [nonArchived]);

  const isUnread = useCallback((item: InboxItem) => !read.has(item.id), [read]);
  const isProcessed = useCallback((item: InboxItem) => processed.has(item.id), [processed]);

  // ── Tab population + grouping ────────────────────────────────────────────────
  // §C6 / 04 doc §E11: the action-required tabs ("我的"/"受阻") graduate a
  // processed item out entirely; every other tab keeps it (sunk to the
  // bottom via `sinkProcessed` below) rather than hiding it outright.
  const tabItems = useMemo(() => {
    const base = filterByTab(nonArchived, prefs.tab, { readIds: read });
    return ACTION_QUEUE_TABS.includes(prefs.tab) ? excludeProcessed(base, processed) : base;
  }, [nonArchived, prefs.tab, read, processed]);
  const filtered = useMemo(() => {
    if (prefs.tab !== 'all') return tabItems;
    return filterByStatus(filterByCategory(tabItems, prefs.categoryFilter), prefs.statusFilter);
  }, [tabItems, prefs.tab, prefs.categoryFilter, prefs.statusFilter]);
  const statuses = useMemo(() => distinctStatuses(tabItems), [tabItems]);
  const sorted = useMemo(
    () => sinkProcessed(sortInbox(filtered, prefs.sortBy), processed),
    [filtered, prefs.sortBy, processed],
  );

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
      // §C.3: a needs_human task shares "等你決定" with an approval's
      // `pending` status — show that instead of the generic "受阻" category
      // label, the single biggest vocabulary payoff of the object-model
      // convergence (04 doc §C.3).
      typeLabel: (item) =>
        item.type === 'blocked' && item.status === 'needs_human'
          ? t('inbox.status.pending')
          : t(TYPE_META[item.type].labelKey),
      riskLabel: (level: RiskLevel) => t(`approval.risk.${level}`),
      archive: t('inbox.action.archive'),
      nearExpiry: t('inbox.approval.nearExpiry'),
      nearExpiryTooltip: t('inbox.approval.nearExpiryTooltip'),
      processedTooltip: t('inbox.processed.tooltip'),
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

  // §C.2/§C.3: never print a raw internal status token in the "全部" tab's
  // status filter — fall back to the raw token only when there's no vocab
  // mapping (open-ended values like a failed-run severity).
  const statusFilterLabel = useCallback(
    (status: string) => {
      const key = statusLabelKey(status);
      return key ? t(key) : status;
    },
    [t],
  );

  const tabItemsFor = useCallback(
    (tab: InboxTab) => {
      const base = filterByTab(nonArchived, tab, { readIds: read });
      return ACTION_QUEUE_TABS.includes(tab) ? excludeProcessed(base, processed) : base;
    },
    [nonArchived, read, processed],
  );

  const canArchive = prefs.tab === 'mine';

  // Keep the selection valid as the loaded set changes. Checked against
  // `nonArchived` (tab-independent), not the current tab's filtered `sorted`
  // list: the detail pane already renders off `entries` regardless of the
  // active tab (`findEntry` below), so clearing the selection just because
  // the current tab's filter excludes it would fight that — most sharply for
  // the H5 deep-link flow, where `select()` marks the item read as a side
  // effect and could otherwise immediately fall out of an `unread`-tab view
  // and get its own just-applied selection wiped. Also gated on `!loading`
  // so the deep-link resolution above has a chance to run before this ever
  // sees an (still-empty) entry set and clears it.
  useEffect(() => {
    if (loading) return;
    if (selectedId && !nonArchived.some((it) => it.id === selectedId)) setSelectedId(null);
  }, [loading, nonArchived, selectedId]);

  const selectedEntry = selectedId ? findEntry(selectedId) : undefined;

  // ── Detail pane body ─────────────────────────────────────────────────────────
  const detailBody = useMemo<ReactNode>(() => {
    if (!selectedEntry) return null;
    const { item, raw } = selectedEntry;
    // §C.3: mirrors `rowLabels.typeLabel` — a needs_human task's detail panel
    // must say the same "等你決定" the row does, not the generic "受阻".
    const typeLabel =
      item.type === 'blocked' && item.status === 'needs_human'
        ? t('inbox.status.pending')
        : t(TYPE_META[item.type].labelKey);
    switch (item.type) {
      case 'approval':
        return (
          <ApprovalDetailPanel
            approval={raw as ApprovalItem}
            agentName={agentName((raw as ApprovalItem).agent_id)}
            onApprove={() => decide(item, true)}
            onReject={() => decide(item, false)}
            onDecided={() => markProcessed(item.id)}
          />
        );
      case 'install': {
        const req = raw as InstallRequestInfo;
        // W1-2 / 04 doc §E17: self-contained in the Inbox — no more bouncing
        // to `/approvals` just to see (or act on) an install request's detail.
        return <InstallDetailPanel request={req} onDecided={() => markProcessed(item.id)} />;
      }
      case 'blocked': {
        const task = raw as TaskInfo;
        // A needs_human task gets the full "等你決定" treatment (retry/mark
        // complete/abandon) — a plain blocked task (no goal-loop escalation)
        // keeps the lighter read-only view; there is no defined resolution
        // protocol for it here.
        if (task.status === 'needs_human') {
          return (
            <NeedsHumanTaskPanel
              task={task}
              typeLabel={typeLabel}
              agentName={item.agentId ? agentName(item.agentId) : undefined}
              onResolved={() => markProcessed(item.id)}
            />
          );
        }
        return (
          <DetailShell
            icon={TYPE_META.blocked.icon}
            title={item.title}
            typeLabel={typeLabel}
            agentId={item.agentId}
            agentName={item.agentId ? agentName(item.agentId) : undefined}
          >
            {task.blocked_reason && (
              <p className="rounded-lg bg-destructive/10 px-3 py-2 text-sm text-destructive">{task.blocked_reason}</p>
            )}
            <div className="flex flex-wrap items-center gap-2">
              <Button variant="brand" onClick={() => navigate(`/tasks/${task.id}`)}>
                <ExternalLink />
                {t('inbox.detail.viewTask')}
              </Button>
              {/* W2-3 reverse handoff (E8): jump back to the /goal conversation. */}
              <OpenInChannelButton channel={task.channel} link={task.channel_link} variant="outline" />
            </div>
          </DetailShell>
        );
      }
      case 'budget':
        return (
          <DetailShell
            icon={TYPE_META.budget.icon}
            title={item.title}
            typeLabel={typeLabel}
            agentId={item.agentId}
            agentName={item.agentId ? agentName(item.agentId) : undefined}
          >
            <Button variant="brand" onClick={() => navigate('/manage/billing')}>
              <ExternalLink />
              {t('inbox.detail.viewBilling')}
            </Button>
          </DetailShell>
        );
      case 'decision':
        return (
          <DetailShell
            icon={TYPE_META.decision.icon}
            title={item.title}
            typeLabel={typeLabel}
            agentId={item.agentId}
            agentName={item.agentId ? agentName(item.agentId) : undefined}
          >
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
          <DetailShell
            icon={TYPE_META.failed_run.icon}
            title={item.title}
            typeLabel={typeLabel}
            agentId={item.agentId}
            agentName={item.agentId ? agentName(item.agentId) : undefined}
          >
            <pre className="max-h-96 overflow-auto rounded-lg bg-muted p-2 text-[11px] leading-relaxed text-muted-foreground">
              {JSON.stringify(raw, null, 2)}
            </pre>
          </DetailShell>
        );
    }
  }, [selectedEntry, agentName, decide, markProcessed, navigate, archive, t]);

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
                          <span className="flex-1 truncate">{statusFilterLabel(s)}</span>
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
            isProcessed={isProcessed}
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
