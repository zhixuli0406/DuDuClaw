import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate, useParams } from 'react-router';
import { ArrowLeft, Pause, Play, LogOut, RotateCcw, Brain, Puzzle, CalendarClock, LayoutDashboard, Settings, Upload, Trash2, ImageIcon, Wrench, Shirt } from 'lucide-react';
import {
  api,
  type AgentDetail,
  type KeyFactEntry,
  type SkillInfo,
  type TaskInfo,
  type ActivityEvent,
  type ToolCatalogEntry,
} from '@/lib/api';
import { useAgentsStore } from '@/stores/agents-store';
import { useAgentAvatarStore } from '@/stores/agent-avatar-store';
import { useConnectionStore } from '@/stores/connection-store';
import { useAgentGlyphState } from '@/stores/agent-activity-store';
import { readFileAsBase64 } from '@/lib/attachments';
import { toast, formatError } from '@/lib/toast';
import { Page, Card, Button, Badge, CharacterAvatar, EmptyState, SkeletonList, Mono, Tabs, type TabItem } from '@/components/ui';
import { AgentHero, AgentOverviewTab, agentTaskStats, isLiveState } from '@/components/agent';
import { OffboardDialog } from '@/components/agent/OffboardDialog';
import { WardrobeDialog } from '@/components/agent/WardrobeDialog';
import { formatCents } from '@/lib/format';

/** 512 KB — matches the backend avatar cap (`agents.set_avatar`). */
const MAX_AVATAR_BYTES = 512 * 1024;
const AVATAR_ACCEPT = '.png,.jpg,.jpeg,.webp';
const AVATAR_MIME = new Set(['image/png', 'image/jpeg', 'image/webp']);

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
  const { pauseAgent, resumeAgent, unarchiveAgent, agents, fetchAgents } = useAgentsStore();
  const setAvatarCache = useAgentAvatarStore((s) => s.set);

  const [detail, setDetail] = useState<AgentDetail | null>(null);
  const [facts, setFacts] = useState<KeyFactEntry[] | null>(null);
  const [skills, setSkills] = useState<SkillInfo[] | null>(null);
  const [tools, setTools] = useState<ToolCatalogEntry[] | null>(null);
  const [toolsError, setToolsError] = useState(false);
  const [routines, setRoutines] = useState<Routine[] | null>(null);
  const [tasks, setTasks] = useState<TaskInfo[] | null>(null);
  const [activities, setActivities] = useState<ActivityEvent[] | null>(null);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  // 離職 flow (WP4): three-way offboard — archive / remove / handoff-then-archive.
  const [showOffboard, setShowOffboard] = useState(false);
  const [showWardrobe, setShowWardrobe] = useState(false);
  const [avatarBusy, setAvatarBusy] = useState(false);
  const avatarFileRef = useRef<HTMLInputElement>(null);

  // Live-run state for this one agent (drives the hero pose + XP live dot).
  const glyph = useAgentGlyphState(id, detail?.status);
  const live = isLiveState(glyph);

  const loadDetail = useCallback(async () => {
    setLoading(true);
    try {
      const d = await api.agents.inspect(id);
      setDetail(d);
      // Publish the resolved avatar so the hero + every other surface match.
      setAvatarCache(id, d.avatar ?? null);
    } catch (e) {
      console.warn('[api]', e);
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
    } finally {
      setLoading(false);
    }
  }, [id, intl, setAvatarCache]);

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
    // Platform tool catalog (global, not per-agent) — shown alongside skills so
    // the operator can see the full capability surface this AI staff can call.
    if (tab === 'skills' && tools === null && !toolsError) {
      api.tools.catalog()
        .then((r) => setTools(r?.tools ?? []))
        .catch((e) => { console.warn('[api]', e); setToolsError(true); setTools([]); });
    }
    if (tab === 'routines' && routines === null) {
      api.cron.list().then((r) => setRoutines((r?.tasks ?? []).filter((t) => t.agent_id === id))).catch((e) => { onTabLoadError(e); setRoutines([]); });
    }
  }, [tab, connectionState, id, facts, skills, tools, toolsError, routines, activities, onTabLoadError]);

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

  // Roster (for the handoff target picker).
  useEffect(() => {
    if (connectionState === 'authenticated') fetchAgents();
  }, [connectionState, fetchAgents]);

  const handleAvatarFile = useCallback(
    async (file: File) => {
      if (!AVATAR_MIME.has(file.type)) {
        toast.error(intl.formatMessage({ id: 'agents.avatar.badType' }));
        return;
      }
      if (file.size > MAX_AVATAR_BYTES) {
        toast.error(intl.formatMessage({ id: 'agents.avatar.tooLarge' }));
        return;
      }
      setAvatarBusy(true);
      try {
        const b64 = await readFileAsBase64(file);
        const dataUri = `data:${file.type};base64,${b64}`;
        await api.agents.setAvatar(id, dataUri);
        setAvatarCache(id, dataUri);
        setDetail((d) => (d ? { ...d, avatar: dataUri, has_avatar: true } : d));
        toast.success(intl.formatMessage({ id: 'agents.avatar.saved' }));
      } catch (e) {
        toast.error(intl.formatMessage({ id: 'toast.error.saveFailed' }, { message: formatError(e) }));
      } finally {
        setAvatarBusy(false);
      }
    },
    [id, intl, setAvatarCache],
  );

  const handleAvatarClear = useCallback(async () => {
    setAvatarBusy(true);
    try {
      await api.agents.clearAvatar(id);
      setAvatarCache(id, null);
      setDetail((d) => (d ? { ...d, avatar: null, has_avatar: false } : d));
      toast.success(intl.formatMessage({ id: 'agents.avatar.removed' }));
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.saveFailed' }, { message: formatError(e) }));
    } finally {
      setAvatarBusy(false);
    }
  }, [id, intl, setAvatarCache]);

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

  const isMain = detail.role === 'main';

  return (
    <Page wide>
      <button
        onClick={() => navigate('/agents')}
        className="mb-2 inline-flex items-center gap-1 text-sm text-stone-500 transition-colors hover:text-stone-800 dark:text-stone-400 dark:hover:text-stone-200"
      >
        <ArrowLeft className="h-4 w-4" /> {intl.formatMessage({ id: 'agentDetail.back' })}
      </button>

      <div className="space-y-4">
        {detail.archived && (
          <div className="flex flex-wrap items-center justify-between gap-3 rounded-card border border-amber-500/30 bg-amber-500/10 px-4 py-3 text-sm text-amber-800 dark:text-amber-200">
            <span>{intl.formatMessage({ id: 'agents.archived.notice' }, { name: detail.display_name })}</span>
            <Button
              size="sm"
              variant="secondary"
              icon={RotateCcw}
              disabled={busy}
              onClick={() => lifecycleAction(() => unarchiveAgent(id), 'agents.unarchive.done')}
            >
              {intl.formatMessage({ id: 'agents.unarchive' })}
            </Button>
          </div>
        )}
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
          <div className="space-y-4">
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

            <AgentToolsCard tools={tools} error={toolsError} />
          </div>
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
                <Row label={intl.formatMessage({ id: 'agents.department.label' })}>{detail.department || '—'}</Row>
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

            {/* 造型（衣帽間）— slot-based look, synced to roster + world.
                Photo upload survives as a folded secondary path; a saved
                outfit always outranks a photo. */}
            <Card title={intl.formatMessage({ id: 'wardrobe.card.title' })}>
              <div className="flex items-center gap-4">
                <CharacterAvatar
                  agentId={detail.name}
                  name={detail.display_name}
                  size={72}
                  variant="bust"
                  avatar={detail.outfit ? null : detail.avatar ?? null}
                  outfit={detail.outfit ?? null}
                />
                <div className="flex min-w-0 flex-col gap-2">
                  <div className="flex items-center gap-2">
                    <Button variant="secondary" icon={Shirt} onClick={() => setShowWardrobe(true)}>
                      {intl.formatMessage({ id: 'wardrobe.open' })}
                    </Button>
                  </div>
                  <p className="text-xs text-stone-400 dark:text-stone-500">
                    {intl.formatMessage({ id: 'wardrobe.card.hint' })}
                  </p>
                  <details>
                    <summary className="cursor-pointer text-xs text-stone-400 hover:text-stone-600 dark:text-stone-500 dark:hover:text-stone-300">
                      {intl.formatMessage({ id: 'wardrobe.photoFallback' })}
                    </summary>
                    <div className="mt-2 flex items-center gap-2">
                      <input
                        ref={avatarFileRef}
                        type="file"
                        accept={AVATAR_ACCEPT}
                        className="hidden"
                        onChange={(e) => {
                          const f = e.target.files?.[0];
                          if (f) void handleAvatarFile(f);
                          e.target.value = '';
                        }}
                      />
                      <Button
                        variant="ghost"
                        icon={Upload}
                        disabled={avatarBusy}
                        onClick={() => avatarFileRef.current?.click()}
                      >
                        {intl.formatMessage({ id: 'agents.avatar.upload' })}
                      </Button>
                      {detail.has_avatar && (
                        <Button variant="ghost" icon={Trash2} disabled={avatarBusy} onClick={handleAvatarClear}>
                          {intl.formatMessage({ id: 'agents.avatar.remove' })}
                        </Button>
                      )}
                      <span className="flex items-center gap-1 text-xs text-stone-400 dark:text-stone-500">
                        <ImageIcon className="h-3.5 w-3.5" />
                        {intl.formatMessage({ id: 'agents.avatar.hint' })}
                      </span>
                    </div>
                  </details>
                </div>
              </div>
            </Card>

            {showWardrobe && (
              <WardrobeDialog
                agentId={detail.name}
                displayName={detail.display_name}
                outfit={detail.outfit ?? null}
                onClose={() => setShowWardrobe(false)}
                onSaved={() => {
                  void loadDetail();
                  void fetchAgents();
                }}
              />
            )}

            <Card title={intl.formatMessage({ id: 'agentDetail.settings.title' })}>
              <div className="flex flex-wrap items-center gap-2">
                {detail.archived ? (
                  <Button variant="primary" icon={RotateCcw} disabled={busy} onClick={() => lifecycleAction(() => unarchiveAgent(id), 'agents.unarchive.done')}>
                    {intl.formatMessage({ id: 'agents.unarchive' })}
                  </Button>
                ) : detail.status === 'active' ? (
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
                <span title={isMain ? intl.formatMessage({ id: 'agents.offboard.mainBlocked' }) : undefined}>
                  <Button
                    variant="danger"
                    icon={LogOut}
                    disabled={busy || isMain}
                    onClick={() => setShowOffboard(true)}
                  >
                    {intl.formatMessage({ id: 'agentDetail.dismiss' })}
                  </Button>
                </span>
              </div>
              {isMain && (
                <p className="mt-2 text-xs text-stone-400 dark:text-stone-500">
                  {intl.formatMessage({ id: 'agents.offboard.mainBlocked' })}
                </p>
              )}
            </Card>
          </div>
        )}
      </div>

      <OffboardDialog
        open={showOffboard}
        agent={detail}
        candidates={agents.filter((a) => a.name !== id && !a.archived)}
        busy={busy}
        onClose={() => setShowOffboard(false)}
        onDone={() => { setShowOffboard(false); navigate('/agents'); }}
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

/**
 * AgentToolsCard — the platform-wide capability surface every AI staff can
 * call. `tools.catalog` is global (not per-agent), so this is framed as "tools
 * available on this platform" and grouped by the name prefix (agents / channels
 * / memory / …). Handles loading / error / empty inline so a catalog failure
 * never blanks the skills tab.
 */
function AgentToolsCard({ tools, error }: { tools: ToolCatalogEntry[] | null; error: boolean }) {
  const intl = useIntl();

  const groups = useMemo(() => {
    if (!tools) return [];
    const map = new Map<string, ToolCatalogEntry[]>();
    for (const t of tools) {
      const key = t.name.includes('.') ? t.name.split('.')[0] : 'other';
      const arr = map.get(key) ?? [];
      arr.push(t);
      map.set(key, arr);
    }
    return Array.from(map.entries()).sort((a, b) => a[0].localeCompare(b[0]));
  }, [tools]);

  return (
    <Card
      title={
        <span className="flex items-center gap-2">
          <Wrench className="h-4 w-4 text-amber-500" />
          {intl.formatMessage({ id: 'agentDetail.tools.title' })}
        </span>
      }
    >
      <p className="mb-3 text-xs text-stone-400 dark:text-stone-500">
        {intl.formatMessage({ id: 'agentDetail.tools.desc' })}
      </p>

      {tools === null ? (
        <SkeletonList rows={3} rowClassName="h-10" />
      ) : error ? (
        <EmptyState icon={Wrench} title={intl.formatMessage({ id: 'agentDetail.tools.error' })} />
      ) : tools.length === 0 ? (
        <EmptyState icon={Wrench} title={intl.formatMessage({ id: 'agentDetail.tools.empty' })} />
      ) : (
        <div className="space-y-4">
          {groups.map(([group, items]) => (
            <div key={group}>
              <h4 className="mb-2 text-xs font-semibold uppercase text-stone-500 dark:text-stone-400">
                {group}
                <span className="ml-1.5 font-normal text-stone-400 dark:text-stone-500">({items.length})</span>
              </h4>
              <div className="grid grid-cols-1 gap-1.5 sm:grid-cols-2">
                {items.map((t) => (
                  <div key={t.name} className="rounded-lg bg-stone-500/5 px-3 py-2 dark:bg-white/5">
                    <Mono className="text-xs text-stone-700 dark:text-stone-300">{t.name}</Mono>
                    <p className="mt-0.5 text-xs text-stone-500 dark:text-stone-400">{t.description}</p>
                  </div>
                ))}
              </div>
            </div>
          ))}
        </div>
      )}
    </Card>
  );
}
