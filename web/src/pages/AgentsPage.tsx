import { useCallback, useEffect, useMemo, useState } from 'react';
import { useIntl } from 'react-intl';
import { Link, useNavigate } from 'react-router';
import { cn } from '@/lib/utils';
import { useAgentsStore } from '@/stores/agents-store';
import { useAssignStore } from '@/stores/assign-store';
import { useTasksStore } from '@/stores/tasks-store';
import { useSystemStore } from '@/stores/system-store';
import { type AgentDetail } from '@/lib/api';
import {
  CollectionPageHeader,
  CollectionPageState,
  ErrorState,
  useErrorMessage,
  ListGridContainer,
  ListGridHeader,
  ListGridHeaderCell,
  ListGridRow,
  ListGridCell,
  ActorAvatar,
  Button,
  Segmented,
  DropdownMenu,
  DropdownMenuTrigger,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  CrossLink,
  type ActorStatus,
  type SegmentedOption,
} from '@/components/mds';
import { toast } from '@/lib/toast';
import { agentTaskStats } from '@/components/agent';
import { OffboardDialog } from '@/components/agent/OffboardDialog';
import { timeAgo } from '@/lib/format';
import {
  Bot,
  Plus,
  MoreHorizontal,
  Pause,
  Play,
  Send,
  Archive,
  RotateCcw,
  LogOut,
  Sparkles,
  X,
} from 'lucide-react';

/**
 * AgentsPage — the Multica "員工" collection (spec §5.2 + §4 ListGrid). A slim,
 * double-row (`h-16`) ListGrid of every AI staff member: name + avatar, live
 * status, model, last-active, task count, and a kebab of lifecycle actions.
 * The character-card roster (RosterCard / HireSlotCard) is retired here; those
 * files stay on disk for the P6 sweep.
 */

/** Column scope — in-service vs archived. */
type Scope = 'active' | 'archived';

/** Single grid template shared by header + rows. Secondary columns use `auto`
 *  so their tracks collapse to zero once `hideBelow` sets the cells to
 *  `display:none` on narrow containers, leaving only name + status. */
const GRID_COLUMNS = 'minmax(0,1fr) auto auto auto auto 2.5rem';

/** B+C decision (2026-07-16): the Personal edition has NO hard agent cap;
 *  above this recommended size we show a gentle, dismiss-forever upgrade hint. */
const PERSONAL_RECOMMENDED_AGENTS = 3;
const GROWTH_HINT_KEY = 'duduclaw:agents:growth-hint-dismissed';

/** Lifecycle → semantic dot colour + ActorAvatar availability status. */
function statusDotClass(status: string, archived: boolean): string {
  if (archived) return 'bg-muted-foreground';
  switch (status) {
    case 'active':
      return 'bg-success';
    case 'paused':
      return 'bg-warning';
    case 'terminated':
      return 'bg-destructive';
    default:
      return 'bg-muted-foreground';
  }
}
function actorStatus(status: string, archived: boolean): ActorStatus {
  if (archived) return 'offline';
  if (status === 'active') return 'online';
  if (status === 'paused') return 'busy';
  if (status === 'terminated') return 'error';
  return 'offline';
}

export function AgentsPage() {
  const intl = useIntl();
  const navigate = useNavigate();
  const errorText = useErrorMessage();
  const {
    agents,
    fetchAgents,
    pauseAgent,
    resumeAgent,
    archiveAgent,
    unarchiveAgent,
    setIncludeArchived,
    loading,
    loaded,
    error,
    clearError,
  } = useAgentsStore();
  const { tasks, fetchTasks } = useTasksStore();

  const [scope, setScope] = useState<Scope>('active');
  // UX plan I-1a: "交辦任務" on an employee card opens the one shared panel with
  // that employee preselected. It used to open a local free-text dialog posting
  // to `agents.delegate` — a fourth create surface behind the same verb, whose
  // fire-and-forget bus message left no task to follow and never met the
  // acceptance judge.
  const openAssign = useAssignStore((s) => s.openAssign);
  const [offboardTarget, setOffboardTarget] = useState<AgentDetail | null>(null);

  // Soft growth hint (B+C): Personal edition, above the recommended size,
  // dismiss-forever. Never blocks anything.
  const isPersonal = useSystemStore((s) => s.status?.edition_profile) === 'personal';
  const [growthHintDismissed, setGrowthHintDismissed] = useState(() => {
    try {
      return localStorage.getItem(GROWTH_HINT_KEY) === '1';
    } catch {
      return true; // private mode — just never show it
    }
  });
  const dismissGrowthHint = useCallback(() => {
    setGrowthHintDismissed(true);
    try {
      localStorage.setItem(GROWTH_HINT_KEY, '1');
    } catch {
      /* private mode — dismiss lasts for this session only */
    }
  }, []);

  useEffect(() => {
    fetchAgents();
    // Tasks power the per-row task count + status subtext.
    fetchTasks();
  }, [fetchAgents, fetchTasks]);

  const changeScope = useCallback(
    (next: Scope) => {
      setScope(next);
      // Archived staff are hidden by default; pull them in when the tab opens.
      void setIncludeArchived(next === 'archived');
    },
    [setIncludeArchived],
  );

  const openCreate = useCallback(() => navigate('/agents/new'), [navigate]);

  /**
   * Pause / resume / archive / unarchive used to be fire-and-forget `void`
   * calls over a store that swallowed its own errors, so a failed lifecycle
   * change produced exactly nothing on screen (P05 Blocker, phase-4 audit).
   * Confirm the success, and say so plainly when it didn't happen.
   */
  const runLifecycle = useCallback(
    async (fn: () => Promise<void>, successId: string, agent: AgentDetail) => {
      clearError();
      try {
        await fn();
        toast.success(
          intl.formatMessage({ id: successId }, { name: agent.display_name || agent.name }),
        );
      } catch (e) {
        toast.error(
          intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: errorText(e) }),
        );
      }
    },
    [clearError, intl, errorText],
  );

  const visible = useMemo(
    () => agents.filter((a) => (scope === 'archived' ? a.archived : !a.archived)),
    [agents, scope],
  );

  const activeAgentCount = useMemo(() => agents.filter((a) => !a.archived).length, [agents]);
  const showGrowthHint =
    isPersonal &&
    !growthHintDismissed &&
    scope === 'active' &&
    activeAgentCount > PERSONAL_RECOMMENDED_AGENTS;

  const scopeOptions: SegmentedOption<Scope>[] = [
    { value: 'active', label: intl.formatMessage({ id: 'agents.scope.active' }) },
    { value: 'archived', label: intl.formatMessage({ id: 'agents.scope.archived' }) },
  ];

  const hireButton = (
    <Button variant="brand" size="sm" onClick={openCreate}>
      <Plus />
      <span className="hidden sm:inline">{intl.formatMessage({ id: 'agents.create' })}</span>
    </Button>
  );

  return (
    <div className="-mx-4 -mt-4 flex flex-col md:-mx-6 md:-mt-6">
      <CollectionPageHeader
        hideTrigger
        icon={Bot}
        title={intl.formatMessage({ id: 'agents.title' })}
        count={visible.length}
        description={intl.formatMessage({ id: 'nav.agents.desc' })}
        action={hireButton}
      />

      {/* Scope control row. */}
      <div className="flex h-12 shrink-0 items-center gap-2 border-b border-surface-border px-4">
        <Segmented
          value={scope}
          onValueChange={changeScope}
          options={scopeOptions}
          aria-label={intl.formatMessage({ id: 'agents.scope.active' })}
        />
      </div>

      {showGrowthHint && (
        <div className="mx-4 mt-3 flex items-start gap-3 rounded-xl bg-brand/8 p-4 ring-1 ring-inset ring-brand/20">
          <span className="mt-0.5 grid size-8 shrink-0 place-items-center rounded-lg bg-brand/12 text-brand">
            <Sparkles className="size-4" />
          </span>
          <div className="min-w-0 flex-1 text-sm">
            <p className="font-medium text-foreground">
              {intl.formatMessage({ id: 'agents.growthHint.title' })}
            </p>
            <p className="mt-0.5 text-muted-foreground">
              {intl.formatMessage({ id: 'agents.growthHint.body' }, { count: activeAgentCount })}
            </p>
            <a
              href="https://duduclaw.dudustudio.monster#pricing"
              target="_blank"
              rel="noreferrer"
              className="mt-1.5 inline-block font-medium text-brand hover:underline"
            >
              {intl.formatMessage({ id: 'agents.growthHint.cta' })}
            </a>
          </div>
          <button
            type="button"
            onClick={dismissGrowthHint}
            aria-label={intl.formatMessage({ id: 'agents.growthHint.dismiss' })}
            title={intl.formatMessage({ id: 'agents.growthHint.dismiss' })}
            className="shrink-0 rounded-lg p-1.5 text-muted-foreground transition-colors hover:bg-surface-hover hover:text-foreground"
          >
            <X className="size-4" />
          </button>
        </div>
      )}

      {/* Body */}
      {!loaded && loading ? (
        <CollectionPageState state="loading" />
      ) : error != null && visible.length === 0 ? (
        <ErrorState
          icon={Bot}
          error={error}
          onRetry={() => {
            clearError();
            void fetchAgents();
          }}
        />
      ) : visible.length === 0 ? (
        <CollectionPageState
          state="empty"
          icon={Bot}
          title={intl.formatMessage({
            id: scope === 'archived' ? 'agents.scope.archived' : 'agents.empty',
          })}
          action={
            scope === 'active' ? (
              <Button variant="brand" size="sm" onClick={openCreate}>
                <Plus />
                {intl.formatMessage({ id: 'agents.empty.cta' })}
              </Button>
            ) : undefined
          }
        />
      ) : (
        <ListGridContainer
          columns={GRID_COLUMNS}
          className="min-h-[50vh]"
          header={
            <ListGridHeader>
              <ListGridHeaderCell>{intl.formatMessage({ id: 'agents.col.name' })}</ListGridHeaderCell>
              <ListGridHeaderCell>{intl.formatMessage({ id: 'agents.col.status' })}</ListGridHeaderCell>
              <ListGridHeaderCell hideBelow>{intl.formatMessage({ id: 'agents.col.model' })}</ListGridHeaderCell>
              <ListGridHeaderCell hideBelow>{intl.formatMessage({ id: 'agents.col.lastActive' })}</ListGridHeaderCell>
              <ListGridHeaderCell hideBelow className="justify-end">
                {intl.formatMessage({ id: 'agents.col.tasks' })}
              </ListGridHeaderCell>
              <ListGridHeaderCell aria-hidden />
            </ListGridHeader>
          }
        >
          {visible.map((agent) => (
            <AgentRow
              key={agent.name}
              agent={agent}
              taskCount={agentTaskStats(tasks, agent.name).total}
              onPause={() => void runLifecycle(() => pauseAgent(agent.name), 'agentDetail.rested', agent)}
              onResume={() => void runLifecycle(() => resumeAgent(agent.name), 'agentDetail.resumed', agent)}
              onDelegate={() => openAssign({ agentId: agent.name })}
              onArchive={() => void runLifecycle(() => archiveAgent(agent.name), 'agents.archive.done', agent)}
              onUnarchive={() =>
                void runLifecycle(() => unarchiveAgent(agent.name), 'agents.unarchive.done', agent)
              }
              onOffboard={() => setOffboardTarget(agent)}
            />
          ))}
        </ListGridContainer>
      )}

      {offboardTarget && (
        <OffboardDialog
          open
          agent={offboardTarget}
          candidates={agents.filter((a) => a.name !== offboardTarget.name && !a.archived)}
          onClose={() => setOffboardTarget(null)}
          onDone={() => {
            setOffboardTarget(null);
            fetchAgents();
          }}
        />
      )}
    </div>
  );
}

/** One `h-16` staff row (spec §4 ListGrid, agent double-row). */
function AgentRow({
  agent,
  taskCount,
  onPause,
  onResume,
  onDelegate,
  onArchive,
  onUnarchive,
  onOffboard,
}: {
  agent: AgentDetail;
  taskCount: number;
  onPause: () => void;
  onResume: () => void;
  onDelegate: () => void;
  onArchive: () => void;
  onUnarchive: () => void;
  onOffboard: () => void;
}) {
  const intl = useIntl();
  const navigate = useNavigate();
  const archived = !!agent.archived;
  const isMain = agent.role === 'main';
  const to = `/agents/${encodeURIComponent(agent.name)}`;
  const statusLabel = intl.formatMessage({ id: `status.${archived ? 'archived' : agent.status}` });
  const model = agent.model?.preferred;
  const lastRun = agent.heartbeat?.last_run;

  return (
    <ListGridRow rowSize="lg" to={to} className={archived ? 'opacity-70' : undefined}>
      {/* Name + avatar (real link for new-tab intent). */}
      <ListGridCell className="gap-3">
        <ActorAvatar
          actorType="agent"
          size="lg"
          name={agent.display_name}
          src={agent.avatar ?? undefined}
          showStatusDot
          status={actorStatus(agent.status, archived)}
        />
        <div className="flex min-w-0 flex-col">
          <Link
            to={to}
            className="truncate text-sm font-medium text-foreground hover:underline"
          >
            {agent.display_name}
          </Link>
          <span className="truncate text-xs text-muted-foreground">
            {agent.trigger || intl.formatMessage({ id: `agents.role.${agent.role}` })}
          </span>
        </div>
      </ListGridCell>

      {/* Status: coloured dot + label + task count subtext. */}
      <ListGridCell>
        <span className={cn('mr-2 size-1.5 shrink-0 rounded-full', statusDotClass(agent.status, archived))} />
        <span className="flex min-w-0 flex-col leading-tight">
          <span className="truncate text-sm text-foreground">{statusLabel}</span>
          {taskCount > 0 && (
            <span className="truncate text-xs text-muted-foreground">
              {intl.formatMessage({ id: 'agents.list.taskCount' }, { count: taskCount })}
            </span>
          )}
        </span>
      </ListGridCell>

      {/* Model. X03 (UX audit §3.3): this used to be dead text; it now opens
          the staff member's own 腦袋與引擎 edit tab. The row itself is a
          `to`-navigable ListGridRow, but `useRowLink` skips clicks landing on
          a nested button/anchor (see `list-grid.tsx`), so CrossLink's own
          `<button>` cleanly overrides the row-level navigation. */}
      <ListGridCell hideBelow>
        {model ? (
          <CrossLink
            label={model}
            onClick={() => navigate(`/agents/${encodeURIComponent(agent.name)}/edit?tab=brain`)}
          />
        ) : (
          <span className="truncate text-xs text-muted-foreground">—</span>
        )}
      </ListGridCell>

      {/* Last active. */}
      <ListGridCell hideBelow>
        <span className="font-mono text-xs tabular-nums text-muted-foreground">
          {lastRun ? timeAgo(lastRun) : intl.formatMessage({ id: 'agents.lastActive.never' })}
        </span>
      </ListGridCell>

      {/* Task count. */}
      <ListGridCell hideBelow className="justify-end">
        <span className="font-mono text-xs tabular-nums text-muted-foreground">{taskCount}</span>
      </ListGridCell>

      {/* Kebab. */}
      <ListGridCell className="justify-end">
        <DropdownMenu>
          <DropdownMenuTrigger
            render={
              <Button
                variant="ghost"
                size="icon-sm"
                aria-label={intl.formatMessage({ id: 'agentDetail.more' })}
                data-stop-row-nav
                onClick={(e) => e.stopPropagation()}
              />
            }
          >
            <MoreHorizontal />
          </DropdownMenuTrigger>
          <DropdownMenuContent>
            {!archived &&
              (agent.status === 'active' ? (
                <DropdownMenuItem onClick={onPause}>
                  <Pause />
                  {intl.formatMessage({ id: 'agents.pause' })}
                </DropdownMenuItem>
              ) : (
                <DropdownMenuItem onClick={onResume}>
                  <Play />
                  {intl.formatMessage({ id: 'agents.resume' })}
                </DropdownMenuItem>
              ))}
            {!archived && (
              <DropdownMenuItem onClick={onDelegate}>
                <Send />
                {intl.formatMessage({ id: 'agents.delegate' })}
              </DropdownMenuItem>
            )}
            {archived ? (
              <DropdownMenuItem onClick={onUnarchive}>
                <RotateCcw />
                {intl.formatMessage({ id: 'agents.unarchive' })}
              </DropdownMenuItem>
            ) : (
              !isMain && (
                <>
                  <DropdownMenuSeparator />
                  <DropdownMenuItem onClick={onArchive}>
                    <Archive />
                    {intl.formatMessage({ id: 'agents.archive' })}
                  </DropdownMenuItem>
                  <DropdownMenuItem variant="destructive" onClick={onOffboard}>
                    <LogOut />
                    {intl.formatMessage({ id: 'agentDetail.dismiss' })}
                  </DropdownMenuItem>
                </>
              )
            )}
          </DropdownMenuContent>
        </DropdownMenu>
      </ListGridCell>
    </ListGridRow>
  );
}

/** Delegate-task dialog (Multica-styled). */
