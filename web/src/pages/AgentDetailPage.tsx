import { useCallback, useEffect, useMemo, useState } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate, useParams } from 'react-router';
import { ArrowLeft, Pause, Play, Trash2, Brain, Puzzle, CalendarClock, LayoutDashboard, Settings } from 'lucide-react';
import {
  api,
  type AgentDetail,
  type KeyFactEntry,
  type SkillInfo,
  type TaskInfo,
  type ActivityEvent,
} from '@/lib/api';
import { useAgentsStore } from '@/stores/agents-store';
import { useConnectionStore } from '@/stores/connection-store';
import { useAgentGlyphState } from '@/stores/agent-activity-store';
import { toast, formatError } from '@/lib/toast';
import { Page, Card, Button, Badge, EmptyState, SkeletonList, Mono, Tabs, type TabItem } from '@/components/ui';
import { AgentHero, AgentOverviewTab, agentTaskStats, isLiveState } from '@/components/agent';
import { ConfirmDialog } from '@/components/settings/controls';
import { formatCents } from '@/lib/format';

const TAB_IDS = ['overview', 'memory', 'skills', 'routines', 'settings'] as const;
type TabId = (typeof TAB_IDS)[number];

interface Routine { id: string; name?: string; agent_id: string; cron: string; schedule?: string; enabled: boolean; }

/**
 * AgentDetailPage (`/agents/:id/:tab`, dashboard-redesign-v2 §5.4 T6.2) — the
 * personified AI-staff detail. A rich character hero (bust立繪 + mood + XP +
 * skill badges + quick actions) over the 總覽 / 記憶 / 技能 / 例行 / 設定 tab set.
 * The 總覽 tab adds a live activity tail, a win tally, and a recent-task ledger;
 * the other tabs carry over their v1 content (token-aligned only). Actions use
 * the warm wording ("讓 X 休息 / 復工 / 離職") per the §5.1 terminology table.
 */
export function AgentDetailPage() {
  const intl = useIntl();
  const navigate = useNavigate();
  const params = useParams();
  const id = params.id ?? '';
  const rawTab = params.tab ?? 'overview';
  const tab: TabId = (TAB_IDS as readonly string[]).includes(rawTab) ? (rawTab as TabId) : 'overview';

  const connectionState = useConnectionStore((s) => s.state);
  const { pauseAgent, resumeAgent, removeAgent } = useAgentsStore();

  const [detail, setDetail] = useState<AgentDetail | null>(null);
  const [facts, setFacts] = useState<KeyFactEntry[] | null>(null);
  const [skills, setSkills] = useState<SkillInfo[] | null>(null);
  const [routines, setRoutines] = useState<Routine[] | null>(null);
  const [tasks, setTasks] = useState<TaskInfo[] | null>(null);
  const [activities, setActivities] = useState<ActivityEvent[] | null>(null);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  // 離職 confirmation (spec §4.2 — requireText = the staff display name).
  const [showDismiss, setShowDismiss] = useState(false);

  // Live-run state for this one agent (drives the hero pose + XP live dot).
  const glyph = useAgentGlyphState(id, detail?.status);
  const live = isLiveState(glyph);

  const loadDetail = useCallback(async () => {
    setLoading(true);
    try {
      const d = await api.agents.inspect(id);
      setDetail(d);
    } catch (e) {
      console.warn('[api]', e);
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
    } finally {
      setLoading(false);
    }
  }, [id, intl]);

  useEffect(() => {
    if (connectionState !== 'authenticated' || !id) return;
    loadDetail();
  }, [connectionState, id, loadDetail]);

  // Tasks power the hero level (§6.2 derivation) + the overview tally, so load
  // them once the agent is known — not gated on the overview tab.
  useEffect(() => {
    if (connectionState !== 'authenticated' || !id) return;
    api.tasks
      .list({ agent_id: id })
      .then((r) => setTasks(r?.tasks ?? []))
      .catch((e) => {
        console.warn('[api]', e);
        setTasks([]);
      });
  }, [connectionState, id]);

  // Lazy per-tab loads. On failure we surface a toast AND set an empty array so
  // the tab shows its empty state rather than an infinite spinner — but the
  // failure is never swallowed silently (Haiku review #2).
  const onTabLoadError = useCallback(
    (e: unknown) => {
      console.warn('[api]', e);
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
    },
    [intl],
  );
  useEffect(() => {
    if (connectionState !== 'authenticated' || !id) return;
    if (tab === 'overview' && activities === null) {
      api.activity.list({ agent_id: id, limit: 10 }).then((r) => setActivities(r?.events ?? [])).catch((e) => { onTabLoadError(e); setActivities([]); });
    }
    if (tab === 'memory' && facts === null) {
      api.memory.keyFacts(id).then((r) => setFacts(r?.entries ?? [])).catch((e) => { onTabLoadError(e); setFacts([]); });
    }
    if (tab === 'skills' && skills === null) {
      api.skills.list(id).then((r) => setSkills(r?.skills ?? [])).catch((e) => { onTabLoadError(e); setSkills([]); });
    }
    if (tab === 'routines' && routines === null) {
      api.cron.list().then((r) => setRoutines((r?.tasks ?? []).filter((t) => t.agent_id === id))).catch((e) => { onTabLoadError(e); setRoutines([]); });
    }
  }, [tab, connectionState, id, facts, skills, routines, activities, onTabLoadError]);

  const setTab = (next: string) => navigate(`/agents/${encodeURIComponent(id)}/${next}`);

  const lifecycleAction = useCallback(
    async (fn: () => Promise<void>, successId: string) => {
      setBusy(true);
      try {
        await fn();
        toast.success(intl.formatMessage({ id: successId }, { name: detail?.display_name ?? id }));
        await loadDetail();
      } catch (e) {
        toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
      } finally {
        setBusy(false);
      }
    },
    [detail, id, intl, loadDetail],
  );

  const stats = useMemo(() => agentTaskStats(tasks ?? [], id), [tasks, id]);

  const tabs: TabItem[] = useMemo(
    () => [
      { id: 'overview', label: intl.formatMessage({ id: 'agentDetail.tab.overview' }) },
      { id: 'memory', label: intl.formatMessage({ id: 'agentDetail.tab.memory' }), badge: facts?.length },
      { id: 'skills', label: intl.formatMessage({ id: 'agentDetail.tab.skills' }), badge: skills?.length ?? detail?.skills?.length },
      { id: 'routines', label: intl.formatMessage({ id: 'agentDetail.tab.routines' }), badge: routines?.length },
      { id: 'settings', label: intl.formatMessage({ id: 'agentDetail.tab.settings' }) },
    ],
    [intl, facts, skills, routines, detail],
  );

  if (loading && !detail) {
    return (
      <Page>
        <Card padded={false}><div className="p-5"><SkeletonList rows={4} rowClassName="h-12" /></div></Card>
      </Page>
    );
  }

  if (!detail) {
    return (
      <Page>
        <Card>
          <EmptyState
            icon={LayoutDashboard}
            title={intl.formatMessage({ id: 'agentDetail.notFound' })}
            action={<Button icon={ArrowLeft} onClick={() => navigate('/agents')}>{intl.formatMessage({ id: 'agentDetail.back' })}</Button>}
          />
        </Card>
      </Page>
    );
  }

  return (
    <Page wide>
      <button
        onClick={() => navigate('/agents')}
        className="mb-2 inline-flex items-center gap-1 text-sm text-stone-500 transition-colors hover:text-stone-800 dark:text-stone-400 dark:hover:text-stone-200"
      >
        <ArrowLeft className="h-4 w-4" /> {intl.formatMessage({ id: 'agentDetail.back' })}
      </button>

      <div className="space-y-4">
        <AgentHero
          detail={detail}
          live={live}
          doneCount={stats.done}
          busy={busy}
          onChat={() => navigate(`/chat?agent=${encodeURIComponent(id)}`)}
          // TODO(v2-W6): TaskBoardPage reads ?new=1 + defaultAssignee param (other
          // agent's file domain); this call site just routes with the assignee.
          onDelegate={() => navigate(`/tasks?new=1&assignee=${encodeURIComponent(id)}`)}
          onPause={() => lifecycleAction(() => pauseAgent(id), 'agentDetail.rested')}
          onResume={() => lifecycleAction(() => resumeAgent(id), 'agentDetail.resumed')}
        />

        <Tabs items={tabs} value={tab} onChange={setTab} />

        {tab === 'overview' && (
          <AgentOverviewTab activities={activities} tasks={tasks} stats={stats} live={live} />
        )}

        {tab === 'memory' && (
          <Card title={intl.formatMessage({ id: 'agentDetail.memory.title' })}>
            {facts === null ? (
              <SkeletonList rows={3} rowClassName="h-10" />
            ) : facts.length === 0 ? (
              <EmptyState icon={Brain} title={intl.formatMessage({ id: 'agentDetail.memory.empty' })} />
            ) : (
              <ul className="space-y-2">
                {facts.map((f) => (
                  <li key={f.id} className="panel px-3 py-2 text-sm text-stone-700 dark:text-stone-300">{f.fact}</li>
                ))}
              </ul>
            )}
          </Card>
        )}

        {tab === 'skills' && (
          <Card title={intl.formatMessage({ id: 'agentDetail.skills.title' })}>
            {skills === null ? (
              <SkeletonList rows={3} rowClassName="h-10" />
            ) : skills.length === 0 ? (
              <EmptyState icon={Puzzle} title={intl.formatMessage({ id: 'agentDetail.skills.empty' })} />
            ) : (
              <div className="flex flex-wrap gap-2">
                {skills.map((s) => (
                  <Badge key={s.name} tone={s.security_status === 'fail' ? 'danger' : s.security_status === 'warn' ? 'warning' : 'neutral'}>
                    {s.name}
                  </Badge>
                ))}
              </div>
            )}
          </Card>
        )}

        {tab === 'routines' && (
          <Card title={intl.formatMessage({ id: 'agentDetail.routines.title' })}>
            {routines === null ? (
              <SkeletonList rows={3} rowClassName="h-10" />
            ) : routines.length === 0 ? (
              <EmptyState icon={CalendarClock} title={intl.formatMessage({ id: 'agentDetail.routines.empty' })} />
            ) : (
              <ul className="space-y-2">
                {routines.map((r) => (
                  <li key={r.id} className="panel flex items-center justify-between px-3 py-2 text-sm">
                    <span className="text-stone-700 dark:text-stone-300">{r.name || r.id}</span>
                    <Mono>{r.schedule || r.cron}</Mono>
                  </li>
                ))}
              </ul>
            )}
          </Card>
        )}

        {tab === 'settings' && (
          <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
            <Card title={intl.formatMessage({ id: 'agentDetail.overview.identity' })}>
              <dl className="space-y-2 text-sm">
                <Row label={intl.formatMessage({ id: 'agentDetail.field.name' })}><Mono>{detail.name}</Mono></Row>
                <Row label={intl.formatMessage({ id: 'agentDetail.field.reportsTo' })}>{detail.reports_to || '—'}</Row>
                <Row label={intl.formatMessage({ id: 'agentDetail.field.model' })}><Mono>{detail.model?.preferred ?? '—'}</Mono></Row>
                <Row label={intl.formatMessage({ id: 'agentDetail.field.spent' })}><Mono>{formatCents(detail.budget?.spent_cents)}</Mono></Row>
                <Row label={intl.formatMessage({ id: 'agentDetail.field.limit' })}><Mono>{formatCents(detail.budget?.monthly_limit_cents)}</Mono></Row>
                <Row label={intl.formatMessage({ id: 'agentDetail.field.heartbeat' })}>
                  {detail.heartbeat?.enabled ? intl.formatMessage({ id: 'common.enabled' }) : intl.formatMessage({ id: 'common.disabled' })}
                </Row>
              </dl>
              <button
                onClick={() => navigate('/agents')}
                className="mt-3 inline-flex items-center gap-1 border-t border-[var(--panel-border)] pt-3 text-sm font-medium text-amber-600 transition-colors hover:text-amber-700 dark:text-amber-400 dark:hover:text-amber-300"
              >
                {intl.formatMessage({ id: 'agentDetail.editFull' })} →
              </button>
            </Card>
            <Card title={intl.formatMessage({ id: 'agentDetail.settings.title' })}>
              <div className="flex flex-wrap items-center gap-2">
                {detail.status === 'active' ? (
                  <Button icon={Pause} disabled={busy} onClick={() => lifecycleAction(() => pauseAgent(id), 'agentDetail.rested')}>
                    {intl.formatMessage({ id: 'agentDetail.rest' })}
                  </Button>
                ) : (
                  <Button variant="primary" icon={Play} disabled={busy} onClick={() => lifecycleAction(() => resumeAgent(id), 'agentDetail.resumed')}>
                    {intl.formatMessage({ id: 'agentDetail.resume' })}
                  </Button>
                )}
                <Button variant="secondary" icon={Settings} onClick={() => navigate('/agents')}>
                  {intl.formatMessage({ id: 'agentDetail.editFull' })}
                </Button>
                <Button
                  variant="danger"
                  icon={Trash2}
                  disabled={busy}
                  onClick={() => setShowDismiss(true)}
                >
                  {intl.formatMessage({ id: 'agentDetail.dismiss' })}
                </Button>
              </div>
            </Card>
          </div>
        )}
      </div>

      <ConfirmDialog
        open={showDismiss}
        onClose={() => setShowDismiss(false)}
        onConfirm={async () => {
          setShowDismiss(false);
          await lifecycleAction(async () => { await removeAgent(id); navigate('/agents'); }, 'agentDetail.dismissed');
        }}
        title={intl.formatMessage({ id: 'agentDetail.dismiss' })}
        message={intl.formatMessage({ id: 'agentDetail.dismiss.confirm' }, { name: detail.display_name })}
        confirmLabel={intl.formatMessage({ id: 'agentDetail.dismiss' })}
        requireText={detail.display_name}
        requireTextHint={intl.formatMessage({ id: 'agentDetail.dismiss.requireHint' }, { name: detail.display_name })}
        busy={busy}
      />
    </Page>
  );
}

function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="flex items-center justify-between gap-3">
      <dt className="text-stone-500 dark:text-stone-400">{label}</dt>
      <dd className="text-stone-800 dark:text-stone-100">{children}</dd>
    </div>
  );
}
