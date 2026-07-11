import { useEffect, useMemo, useState } from 'react';
import { useIntl } from 'react-intl';
import { api, type TaskInfo } from '@/lib/api';
import { useAgentsStore } from '@/stores/agents-store';
import { useAuthStore } from '@/stores/auth-store';
import { useConnectionStore } from '@/stores/connection-store';
import { useVisibleAgents } from '@/lib/data-scope';
import { Page, Card } from '@/components/ui';
import { ActivityFeed } from '@/components/ActivityFeed';
import {
  type InboxItem,
  sortInbox,
  TYPE_URGENCY,
} from '@/lib/inbox-model';
import { GreetingHud } from '@/components/home/GreetingHud';
import { WorldStagePlaceholder } from '@/components/home/WorldStagePlaceholder';
import { NeedsMeRow } from '@/components/home/NeedsMeRow';
import { LiveCards } from '@/components/home/LiveCards';
import { RecentTasks } from '@/components/home/RecentTasks';

/** Same calendar day in local time. */
function isToday(iso?: string | null): boolean {
  if (!iso) return false;
  const t = new Date(iso);
  const now = new Date();
  return t.getFullYear() === now.getFullYear() && t.getMonth() === now.getMonth() && t.getDate() === now.getDate();
}

/**
 * HomePage (`/`) — 首頁「事務所」 (dashboard-redesign-v2 §5.1). Answers "現在發生
 * 什麼事" top-to-bottom: a state-aware greeting HUD + today's war-report (T3.1),
 * the world-stage band (T3.5), the "需要我" strip (T3.2), the live "正在進行"
 * board (T3.3), and the two-column recent activity / recent tasks row (T3.4).
 *
 * The one-line launcher that used to live here has moved to /chat in the v2
 * information architecture (§5.5); Home is now purely the situational dashboard.
 */
export function HomePage() {
  const intl = useIntl();
  const user = useAuthStore((s) => s.user);
  const fetchAgents = useAgentsStore((s) => s.fetchAgents);
  // Data-scoped: an employee sees only their own AI staff (§3.4 WP11-T11.3).
  const agents = useVisibleAgents();
  const connectionState = useConnectionStore((s) => s.state);
  const authed = connectionState === 'authenticated';

  const [needsMe, setNeedsMe] = useState<InboxItem[]>([]);
  const [spentCents, setSpentCents] = useState<number | null>(null);
  const [doneToday, setDoneToday] = useState<number | null>(null);

  useEffect(() => {
    if (!authed) return;
    fetchAgents();

    const nameOf = (id: string) => {
      const a = agents.find((x) => x.name === id);
      return a?.display_name || id;
    };

    // "需要我" merged stream — four cheap sources (approvals / blocked / budget /
    // failed run). Each is best-effort: a manager-gated source that errors for
    // this viewer contributes nothing (fail-safe, not fail-loud). Per-agent
    // decisions are intentionally omitted here — they cost N calls and belong to
    // the full /inbox, not a home preview.
    Promise.all([
      api.approvals.list().catch(() => null),
      api.budget.incidents().catch(() => null),
      api.tasks.list({ status: 'blocked' }).catch(() => null),
      api.audit.unifiedLog({ sources: ['channel_failure'], limit: 20 }).catch(() => null),
    ]).then(([approvals, budget, blocked, failed]) => {
      const items: InboxItem[] = [];
      for (const a of approvals?.approvals ?? []) {
        items.push({ id: `approval:${a.id}`, type: 'approval', title: a.summary, agentId: a.agent_id, timestamp: a.created_at, urgency: TYPE_URGENCY.approval, actionable: true, status: 'pending' });
      }
      for (const t of blocked?.tasks ?? []) {
        items.push({ id: `blocked:${t.id}`, type: 'blocked', title: t.title, agentId: t.assigned_to || undefined, timestamp: t.updated_at, urgency: TYPE_URGENCY.blocked, actionable: true, status: t.status });
      }
      for (const inc of budget?.incidents ?? []) {
        items.push({ id: `budget:${inc.agent_id}:${inc.ts}`, type: 'budget', title: intl.formatMessage({ id: 'inbox.budget.title' }, { agent: nameOf(inc.agent_id), scope: inc.scope }), agentId: inc.agent_id, timestamp: inc.ts, urgency: TYPE_URGENCY.budget, actionable: true, status: inc.event });
      }
      for (const ev of failed?.events ?? []) {
        const ch = typeof ev.details?.channel === 'string' ? (ev.details.channel as string) : undefined;
        items.push({ id: `failed_run:${ev.agent_id}:${ev.timestamp}`, type: 'failed_run', title: ev.summary || intl.formatMessage({ id: 'inbox.failedRun.title' }, { agent: nameOf(ev.agent_id) }), agentId: ev.agent_id || undefined, channel: ch, timestamp: ev.timestamp, urgency: TYPE_URGENCY.failed_run, actionable: false, status: ev.severity });
      }
      setNeedsMe(items);
    }).catch(() => { /* silent — an empty strip is honest */ });

    // Cost tile: no per-day spend on the wired RPC surface, so show the
    // cumulative total (labelled 「累計」 in the HUD) rather than fake a today value.
    api.accounts.budgetSummary()
      .then((b) => setSpentCents(b?.total_spent_cents ?? 0))
      .catch(() => setSpentCents(null));

    api.tasks.list({ status: 'done' })
      .then((r) => setDoneToday((r?.tasks ?? []).filter((t: TaskInfo) => isToday(t.completed_at)).length))
      .catch(() => setDoneToday(null));
    // agents intentionally excluded from deps: the name map is a display nicety
    // resolved at fetch time; re-running on every agent tick would spam the RPCs.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [authed, fetchAgents, intl]);

  const busyCount = useMemo(() => agents.filter((a) => a.status === 'active').length, [agents]);
  const actionableCount = useMemo(() => needsMe.filter((i) => i.actionable).length, [needsMe]);
  const previewTop = useMemo(() => sortInbox(needsMe, 'urgency').slice(0, 3), [needsMe]);

  return (
    <Page wide>
      {/* T3.1 — greeting HUD + today's war-report */}
      <GreetingHud
        userName={user?.display_name || intl.formatMessage({ id: 'home.greeting.fallbackName' })}
        busyCount={busyCount}
        totalAgents={agents.length}
        actionableCount={actionableCount}
        doneToday={doneToday}
        costCents={spentCents}
      />

      {/* T3.5 — world stage mount (static illustrated placeholder until W4) */}
      <WorldStagePlaceholder agents={agents} />

      {/* T3.2 — 需要我 strip */}
      <NeedsMeRow items={previewTop} total={needsMe.length} />

      {/* T3.3 — 正在進行 live board */}
      <LiveCards agents={agents} enabled={authed} />

      {/* T3.4 — 兩欄近期：最近活動 | 最近任務 */}
      <div className="grid gap-6 lg:grid-cols-2">
        <Card title={intl.formatMessage({ id: 'activity.title' })}>
          <ActivityFeed limit={10} showFilter agents={agents} />
        </Card>
        <RecentTasks agents={agents} enabled={authed} />
      </div>
    </Page>
  );
}
