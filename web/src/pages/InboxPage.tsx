import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate } from 'react-router';
import { Inbox } from 'lucide-react';
import { api, type ApprovalItem, type TaskInfo, type DecisionInfo } from '@/lib/api';
import { useConnectionStore } from '@/stores/connection-store';
import { useApprovalsStore } from '@/stores/approvals-store';
import { toast, formatError } from '@/lib/toast';
import {
  Page,
  PageHeader,
  Card,
  EmptyState,
  SkeletonList,
  Tabs,
  usePanel,
  celebrate,
  Mono,
  DuDu,
} from '@/components/ui';
import { InboxList, type InboxGroup } from '@/components/inbox/InboxList';
import { InboxToolbar } from '@/components/inbox/InboxToolbar';
import { ApprovalDetailPanel } from '@/components/inbox/ApprovalDetailPanel';
import { TYPE_META } from '@/components/inbox/meta';
import type { InboxRowLabels } from '@/components/inbox/InboxRow';
import {
  type InboxItem,
  type InboxTab,
  type InboxColumn,
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
/** Cap on failed-run rows pulled from the unified audit log. */
const FAILED_RUN_CAP = 30;

interface RawEntry {
  item: InboxItem;
  /** Original source payload for running the action. */
  raw: unknown;
}

export function InboxPage() {
  const intl = useIntl();
  const t = useCallback((id: string) => intl.formatMessage({ id }), [intl]);
  const navigate = useNavigate();
  const panel = usePanel();
  const connectionState = useConnectionStore((s) => s.state);
  const setPendingCount = useApprovalsStore((s) => s.setPendingCount);

  const [entries, setEntries] = useState<RawEntry[]>([]);
  const [agentNames, setAgentNames] = useState<Record<string, string>>({});
  const [loading, setLoading] = useState(true);
  const [prefs, setPrefs] = useState<InboxPrefs>(loadPrefs);
  const [read, setRead] = useState<ReadonlySet<string>>(() => loadIdSet(READ_KEY));
  const [archived, setArchived] = useState<ReadonlySet<string>>(() => loadIdSet(ARCHIVED_KEY));
  const [undoStack, setUndoStack] = useState<RawEntry[]>([]);

  const updatePrefs = useCallback((patch: Partial<InboxPrefs>) => {
    setPrefs((p) => {
      const next = { ...p, ...patch };
      persistPrefs(next);
      return next;
    });
  }, []);

  const agentName = useCallback((id: string) => agentNames[id] ?? id, [agentNames]);

  // ── Load: five sources, merged. Each is best-effort (a manager-gated source
  // that errors for this viewer contributes nothing — fail-safe, not fail-loud).
  const load = useCallback(async () => {
    const [approvalsRes, budgetRes, tasksRes, agentsRes, failedRes] = await Promise.all([
      api.approvals.list().catch(() => null),
      api.budget.incidents().catch(() => null),
      api.tasks.list({ status: 'blocked' }).catch(() => null),
      api.agents.list().catch(() => null),
      // B5: no dedicated failed-run RPC — the unified audit log's channel_failure
      // source is the real, existing surface. (Not a stub.)
      api.audit.unifiedLog({ sources: ['channel_failure'], limit: FAILED_RUN_CAP }).catch(() => null),
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

    // Decisions require a per-agent call — poll a capped set of agents.
    const agentIds = (agentsRes?.agents ?? []).slice(0, DECISION_AGENT_CAP).map((a) => a.name);
    const decisionResults = await Promise.all(
      agentIds.map((name) =>
        api.decisions
          .list(name, 10)
          .then((r) => ({ name, decisions: r?.decisions ?? [] }))
          .catch(() => ({ name, decisions: [] as DecisionInfo[] })),
      ),
    );
    for (const { name, decisions } of decisionResults) {
      for (const d of decisions) {
        merged.push({
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

    setEntries(merged);
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

  // Remove a decided approval from the queue and close its panel. Archiving (vs.
  // deleting) keeps the id out of every tab; undo deliberately can't cheaply
  // resurrect a server-decided item. Shared by the generic decide() path and the
  // skill_create panel (A1) — which decides itself and only signals removal here,
  // so `approvals.decide` is never called twice for one approval.
  const markDecided = useCallback(
    (id: string) => {
      setArchived((prev) => {
        const next = withId(prev, id);
        persistIdSet(ARCHIVED_KEY, next);
        return next;
      });
      panel.clearPanel();
    },
    [panel],
  );

  const decide = useCallback(
    async (item: InboxItem, approve: boolean) => {
      const entry = findEntry(item.id);
      if (!entry) return;
      const a = entry.raw as ApprovalItem;
      try {
        await api.approvals.decide(a.id, approve); // side_effect field ignored
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

  const openApprovalPanel = useCallback(
    (item: InboxItem) => {
      const entry = findEntry(item.id);
      if (!entry) return;
      const a = entry.raw as ApprovalItem;
      panel.setPanel({
        title: t('inbox.approval.panelTitle'),
        content: (
          <ApprovalDetailPanel
            approval={a}
            agentName={agentName(a.agent_id)}
            onApprove={() => decide(item, true)}
            onReject={() => decide(item, false)}
            onDecided={() => markDecided(item.id)}
          />
        ),
      });
      panel.setSheetOpen(true);
    },
    [findEntry, panel, t, agentName, decide, markDecided],
  );

  const openDetailPanel = useCallback(
    (item: InboxItem) => {
      const entry = findEntry(item.id);
      panel.setPanel({
        title: item.title,
        content: (
          <div className="space-y-2 text-sm text-stone-700 dark:text-stone-300">
            <p>{item.title}</p>
            {item.agentId && (
              <p className="text-xs text-stone-400">
                <Mono>{agentName(item.agentId)}</Mono>
              </p>
            )}
            <pre className="max-h-64 overflow-auto rounded-control bg-stone-500/8 p-2 text-[11px] dark:bg-white/5">
              {JSON.stringify(entry?.raw ?? item, null, 2)}
            </pre>
          </div>
        ),
      });
      panel.setSheetOpen(true);
    },
    [findEntry, panel, agentName],
  );

  const open = useCallback(
    (item: InboxItem) => {
      markRead(item.id);
      switch (item.type) {
        case 'approval':
          openApprovalPanel(item);
          break;
        case 'blocked': {
          const task = findEntry(item.id)?.raw as TaskInfo | undefined;
          if (task) navigate(`/tasks/${task.id}`);
          break;
        }
        case 'budget':
          navigate('/manage/billing');
          break;
        case 'decision':
          navigate('/agents');
          break;
        case 'failed_run':
          openDetailPanel(item);
          break;
      }
    },
    [markRead, openApprovalPanel, openDetailPanel, findEntry, navigate],
  );

  const view = open; // the primary "view / go" button and Enter behave the same

  const markAllRead = useCallback(() => {
    setRead((prev) => {
      let next = prev;
      for (const it of nonArchived) if (!next.has(it.id)) next = withId(next, it.id);
      persistIdSet(READ_KEY, next);
      return next;
    });
  }, [nonArchived]);

  // ── Inbox Zero celebration (fires once on the >0 → 0 transition). ────────────
  const prevCount = useRef<number | null>(null);
  useEffect(() => {
    if (loading) return;
    const count = nonArchived.length;
    if (prevCount.current != null && prevCount.current > 0 && count === 0) {
      celebrate('inbox_zero', { message: t('inbox.zero.title') });
    }
    prevCount.current = count;
  }, [nonArchived.length, loading, t]);

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
    (key: string, by: typeof prefs.groupBy, sample: InboxItem): string => {
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
      approve: t('inbox.action.approve'),
      reject: t('inbox.action.reject'),
      view: t('inbox.action.view'),
      archive: t('inbox.action.archive'),
    }),
    [t],
  );

  const tabItemsFor = useCallback(
    (tab: InboxTab) => filterByTab(nonArchived, tab, { readIds: read }),
    [nonArchived, read],
  );

  const tabs = useMemo(
    () =>
      INBOX_TABS.map((tab) => ({
        id: tab,
        label: t(`inbox.tab.${tab}`),
        badge: tabItemsFor(tab).length || undefined,
      })),
    [t, tabItemsFor],
  );

  const toggleColumn = useCallback(
    (c: InboxColumn) => {
      const has = prefs.columns.includes(c);
      const next = has ? prefs.columns.filter((x) => x !== c) : [...prefs.columns, c];
      updatePrefs({ columns: next.length ? next : prefs.columns });
    },
    [prefs.columns, updatePrefs],
  );

  const canArchive = prefs.tab === 'mine';

  return (
    <Page>
      <PageHeader
        icon={Inbox}
        title={t('inbox.title')}
        subtitle={t('inbox.subtitle')}
      />

      {loading ? (
        <Card padded={false}>
          <div className="p-5">
            <SkeletonList rows={4} rowClassName="h-16" />
          </div>
        </Card>
      ) : nonArchived.length === 0 ? (
        <Card>
          <div className="flex flex-col items-center gap-3 py-10 text-center">
            {/* Inbox Zero — DuDu celebrates the empty inbox (V9 / §7.3). */}
            <div
              data-dudu-slot="inbox-zero"
              className="grid h-20 w-20 place-items-center rounded-bubble bg-amber-500/10"
            >
              <DuDu face="celebrating" size={72} />
            </div>
            <h2 className="text-lg font-semibold text-stone-800 dark:text-stone-100">{t('inbox.zero.title')}</h2>
            <p className="max-w-sm text-sm text-stone-500 dark:text-stone-400">{t('inbox.zero.hint')}</p>
          </div>
        </Card>
      ) : (
        <div className="space-y-4">
          <Tabs items={tabs} value={prefs.tab} onChange={(id) => updatePrefs({ tab: id as InboxTab })} />

          <InboxToolbar
            showAllFilters={prefs.tab === 'all'}
            showGroupBy={prefs.tab !== 'blocked'}
            groupBy={prefs.groupBy}
            onGroupBy={(v) => updatePrefs({ groupBy: v })}
            sortBy={prefs.sortBy}
            onSortBy={(v) => updatePrefs({ sortBy: v })}
            columns={prefs.columns}
            onToggleColumn={toggleColumn}
            categoryFilter={prefs.categoryFilter}
            onCategory={(v) => updatePrefs({ categoryFilter: v })}
            statuses={statuses}
            statusFilter={prefs.statusFilter}
            onStatus={(v) => updatePrefs({ statusFilter: v })}
            hasUndo={undoStack.length > 0}
            onUndo={undo}
            onMarkAllRead={markAllRead}
          />

          <p className="px-1 text-[11px] text-stone-400 dark:text-stone-500">{t('inbox.keyboardHint')}</p>

          <InboxList
            groups={groups}
            columns={prefs.columns}
            canArchive={canArchive}
            agentName={agentName}
            labels={rowLabels}
            onOpen={open}
            onApprove={(item) => decide(item, true)}
            onReject={(item) => decide(item, false)}
            onView={view}
            onArchive={archive}
            onUnread={(item) => markUnread(item.id)}
            onUndo={undo}
            emptyState={
              <Card>
                <EmptyState icon={Inbox} title={t('inbox.emptyTab')} />
              </Card>
            }
          />
        </div>
      )}
    </Page>
  );
}
