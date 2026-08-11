import { useEffect, useMemo, useState } from 'react';
import { useIntl } from 'react-intl';
import { api, type ApprovalItem, type TaskInfo, type UnifiedAuditEvent } from '@/lib/api';
import { Skeleton } from '@/components/mds';
import { cn } from '@/lib/utils';
import { useSharedLeaderQuery } from '@/hooks/useSharedLeaderQuery';
import {
  buildAttentionItems,
  countStandbyAgents,
  overviewCounts,
  isAllClear,
  withinRecentWindow,
  excludeRecoveredChannelFailures,
  channelFailureChannel,
  UNDELIVERED_WINDOW_MS,
  type AttentionSourceRow,
} from '@/lib/home-overview';
import { NeedsAttentionList } from './NeedsAttentionList';

/** Rows shown before the "還有 N 件" overflow hint kicks in. */
const ROW_CAP = 6;
/** Health data refresh cadence — a calmer pace than the "進行中" widgets
 *  (8s); this is a status summary, not a live feed. */
const POLL_MS = 30_000;
/** Fetch a little headroom past what we'd ever display so the 24h recency
 *  window (see `home-overview.ts`) has enough rows to filter from even on
 *  a chatty install. `audit.unified_log`'s own `total` field (unbounded, no
 *  time filter) is NOT used for K — see the handoff note below. */
const CHANNEL_FAILURE_FETCH_LIMIT = 50;

interface RawOverviewData {
  approvals: ApprovalItem[];
  needsHumanTasks: TaskInfo[];
  channelFailures: UnifiedAuditEvent[];
}

async function fetchOverviewData(): Promise<RawOverviewData> {
  const [approvalsRes, tasksRes, auditRes] = await Promise.all([
    api.approvals.list().catch(() => null),
    api.tasks.list({ status: 'needs_human' }).catch(() => null),
    api.audit.unifiedLog({ sources: ['channel_failure'], limit: CHANNEL_FAILURE_FETCH_LIMIT }).catch(() => null),
  ]);
  return {
    approvals: approvalsRes?.approvals ?? [],
    needsHumanTasks: tasksRes?.tasks ?? [],
    channelFailures: auditRes?.events ?? [],
  };
}

export interface HealthOverviewProps {
  agents: ReadonlyArray<{ name: string; display_name: string; status: string; archived?: boolean }>;
  enabled: boolean;
}

/**
 * HealthOverview — the fixed "overview first" section at the top of Home
 * (W2-1): one health-summary line ("N 位 AI 員工待命，M 件等你決定，K 件沒
 * 送出去", collapsing to a calm all-clear line when M=K=0) plus the
 * "需要你處理" list beneath it. See `lib/home-overview.ts` for the counting
 * rules and `NeedsAttentionList` for the row rendering.
 *
 * **Known data-model gap (handoff note, narrowed by W2-8)**: most
 * `channel_failures.jsonl` writers (`channel_reply.rs`:
 * `channel_reply_fallback` / `channel_reply_silent` / `pty_pool_fallback`)
 * still carry no `channel` (platform) field and no resolved/acked flag, so
 * for THOSE rows K ("沒送出去") remains the 24h-recency proxy described in
 * `lib/home-overview.ts`. `channel_alerts.rs`'s own outage-alert writer is
 * different: it DOES stamp `channel`, and now also emits an explicit
 * `channel_recovered` resolution row — `excludeRecoveredChannelFailures`
 * precise-ifies K for exactly that subset (an already-recovered channel is
 * excluded outright, not just aged out after 24h). Fixing the remaining
 * writers requires a backend schema change (out of scope here — flagged for
 * the next backend-touching wave).
 */
export function HealthOverview({ agents, enabled }: HealthOverviewProps) {
  const intl = useIntl();
  const { data, loading } = useSharedLeaderQuery<RawOverviewData>(
    'home:overview',
    fetchOverviewData,
    POLL_MS,
    enabled,
  );
  const [lastCheckedAt, setLastCheckedAt] = useState<Date | null>(null);

  useEffect(() => {
    if (data && !loading) setLastCheckedAt(new Date());
  }, [data, loading]);

  const nameOf = useMemo(() => {
    const m = new Map<string, string>();
    for (const a of agents) m.set(a.name, a.display_name || a.name);
    return m;
  }, [agents]);

  const items = useMemo(() => {
    const approvalRows: AttentionSourceRow[] = (data?.approvals ?? []).map((a) => ({
      id: a.id,
      title: a.summary,
      agentId: a.agent_id,
      timestamp: a.created_at,
    }));
    const taskRows: AttentionSourceRow[] = (data?.needsHumanTasks ?? []).map((t) => ({
      id: t.id,
      title: t.title,
      agentId: t.assigned_to || undefined,
      timestamp: t.updated_at,
    }));
    // W2-8: drop the `channel_recovered` resolution rows themselves and any
    // failure row a later recovery already resolved, THEN apply the 24h
    // recency proxy to whatever's left (see the class doc above).
    const resolved = excludeRecoveredChannelFailures(data?.channelFailures ?? []);
    const recentFailures = withinRecentWindow(resolved, Date.now(), UNDELIVERED_WINDOW_MS);
    const failureRows: AttentionSourceRow[] = recentFailures.map((ev) => ({
      id: `${ev.agent_id}:${ev.timestamp}`,
      title:
        ev.summary ||
        intl.formatMessage({ id: 'inbox.failedRun.title' }, { agent: nameOf.get(ev.agent_id) ?? ev.agent_id }),
      agentId: ev.agent_id || undefined,
      timestamp: ev.timestamp,
      channel: channelFailureChannel(ev),
    }));
    return buildAttentionItems(approvalRows, taskRows, failureRows);
  }, [data, nameOf, intl]);

  const counts = useMemo(() => overviewCounts(countStandbyAgents(agents), items), [agents, items]);
  const clear = isAllClear(counts);
  const severe = counts.undelivered > 0;

  const separator = intl.formatMessage({ id: 'home.overview.separator' });
  const clauses: string[] = [intl.formatMessage({ id: 'home.overview.standby' }, { count: counts.standby })];
  if (clear) {
    clauses.push(intl.formatMessage({ id: 'home.overview.allClear' }));
  } else {
    if (counts.pendingDecisions > 0) {
      clauses.push(intl.formatMessage({ id: 'home.overview.pending' }, { count: counts.pendingDecisions }));
    }
    if (counts.undelivered > 0) {
      clauses.push(intl.formatMessage({ id: 'home.overview.undelivered' }, { count: counts.undelivered }));
    }
  }
  const summaryLine = clauses.join(separator);

  return (
    <div className="space-y-3">
      {loading && !data ? (
        <Skeleton className="h-5 w-72" />
      ) : (
        <div className="flex flex-wrap items-center gap-x-2 gap-y-1" role="status">
          <span
            aria-hidden
            className={cn(
              'size-2 shrink-0 rounded-full',
              clear ? 'bg-success' : severe ? 'bg-destructive' : 'bg-warning',
            )}
          />
          <p className={cn('text-base font-medium', clear ? 'text-success' : 'text-foreground')}>{summaryLine}</p>
          {clear && lastCheckedAt && (
            <span className="text-xs text-muted-foreground">
              ·{' '}
              {intl.formatMessage(
                { id: 'home.overview.lastChecked' },
                { time: intl.formatTime(lastCheckedAt, { hour: '2-digit', minute: '2-digit' }) },
              )}
            </span>
          )}
        </div>
      )}

      <NeedsAttentionList items={items.slice(0, ROW_CAP)} total={items.length} />
    </div>
  );
}
