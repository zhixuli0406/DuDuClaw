import { useCallback, useState } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate } from 'react-router';
import {
  RotateCcw,
  CheckCircle2,
  XCircle,
  ExternalLink,
  Loader2,
  MessageSquareWarning,
  FileText,
  FileDiff,
} from 'lucide-react';
import { api, type TaskInfo } from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import { Button, Tabs, TabsList, TabsTab, TabsPanel } from '@/components/mds';
import { TaskChangesPanel } from '@/components/task';
import { DetailShell } from './DetailShell';
import { TYPE_META } from './meta';
import { OpenInChannelButton } from './OpenInChannelButton';

type ResolveAction = 'retry' | 'done' | 'abort';

/**
 * NeedsHumanActions — the three-state resolution (重試 / 標記完成 / 放棄) on its
 * own, so every surface that shows a `needs_human` task offers the SAME three
 * choices instead of degrading to the generic status picker.
 *
 * WP-A (§2-6): before this extraction the decision existed only inside the
 * Inbox detail pane; the board card was read-only and the detail page fell back
 * to a 7-option picker in which `pending` (= 重試) did not even appear, so
 * "retry" was unreachable outside the Inbox and a drag could silently move the
 * card past the decision entirely. The mapping above stays the single source of
 * truth — callers never write a `needs_human` task's status themselves.
 */
export function NeedsHumanActions({
  taskId,
  onResolved,
  size,
  className,
}: {
  taskId: string;
  onResolved: () => void;
  /** `sm` for the compact board card; default for detail surfaces. */
  size?: 'sm';
  className?: string;
}) {
  const intl = useIntl();
  const [busy, setBusy] = useState<ResolveAction | null>(null);

  const resolve = useCallback(
    async (action: ResolveAction) => {
      if (busy) return;
      setBusy(action);
      try {
        // 2026-08-14: routed through the SAME fail-closed path as the channel
        // buttons (`tasks.goal_decide` → `resolve_needs_human`) instead of a
        // bare status write — the old `tasks.update` route left the stale
        // claim/lease/result behind on retry and let the previous round's
        // judge feedback leak into the next dispatch.
        await api.tasks.goalDecide(taskId, action);
        toast.success(intl.formatMessage({ id: `inbox.needsHuman.${action}Toast` }));
        onResolved();
      } catch (e) {
        toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
      } finally {
        setBusy(null);
      }
    },
    [busy, taskId, intl, onResolved],
  );

  return (
    <div className={className ?? 'flex flex-wrap items-center gap-2'}>
      <Button variant="brand" size={size} disabled={!!busy} onClick={() => resolve('retry')}>
        {busy === 'retry' ? <Loader2 className="animate-spin" /> : <RotateCcw />}
        {intl.formatMessage({ id: 'inbox.needsHuman.retry' })}
      </Button>
      <Button variant="outline" size={size} disabled={!!busy} onClick={() => resolve('done')}>
        {busy === 'done' ? <Loader2 className="animate-spin" /> : <CheckCircle2 />}
        {intl.formatMessage({ id: 'inbox.needsHuman.done' })}
      </Button>
      <Button variant="destructive" size={size} disabled={!!busy} onClick={() => resolve('abort')}>
        {busy === 'abort' ? <Loader2 className="animate-spin" /> : <XCircle />}
        {intl.formatMessage({ id: 'inbox.needsHuman.abort' })}
      </Button>
    </div>
  );
}

/**
 * NeedsHumanTaskPanel — the Inbox detail-pane body for a goal-loop task
 * escalated to `needs_human` (04 doc §D.6 "「等你決定」時做決定(重試/標記完成/
 * 放棄)"). Previously the Inbox only fetched `status: 'blocked'` tasks and
 * gave a needs_human task no distinct treatment at all (it wasn't even in the
 * merged list) — this panel plus the broadened `tasks.list` fetch in
 * `InboxPage` close that gap.
 *
 * §C.2/§C.3: `needs_human` shares "等你決定" with the approval `pending`
 * status — the single biggest vocabulary payoff of converging the three HITL
 * sources into one "待辦決定" object. This panel's type label is passed in
 * by the caller for exactly that reason (not `TYPE_META.blocked`'s generic
 * "受阻").
 */
export function NeedsHumanTaskPanel({
  task,
  typeLabel,
  agentName,
  onResolved,
}: {
  task: TaskInfo;
  typeLabel: string;
  agentName?: string;
  onResolved: () => void;
}) {
  const intl = useIntl();
  const t = useCallback((id: string) => intl.formatMessage({ id }), [intl]);
  const navigate = useNavigate();
  // WP-F: 說明 (why it stopped) / 變更 (what it already touched). The changes
  // RPC only fires once the operator opens that tab.
  const [tab, setTab] = useState<'brief' | 'changes'>('brief');

  return (
    <DetailShell icon={TYPE_META.blocked.icon} title={task.title} typeLabel={typeLabel} agentId={task.assigned_to} agentName={agentName}>
      <Tabs variant="line" value={tab} onValueChange={(v) => setTab(v as 'brief' | 'changes')}>
        <TabsList className="border-b border-surface-border">
          <TabsTab value="brief">
            <FileText />
            {t('inbox.needsHuman.tab.brief')}
          </TabsTab>
          <TabsTab value="changes">
            <FileDiff />
            {t('inbox.needsHuman.tab.changes')}
          </TabsTab>
        </TabsList>

        <TabsPanel value="brief">
          <div className="space-y-3">
            {task.description && <p className="text-sm text-foreground">{task.description}</p>}

            {/* I-1c: a 想一想 task carries a plan awaiting approval — shown as
                its own labelled card rather than the generic escalation-reason
                line below (which would otherwise duplicate the same text). */}
            {task.plan_pending ? (
              <div className="space-y-1 rounded-lg bg-muted px-3 py-2">
                <p className="text-xs font-medium text-muted-foreground">
                  {t('tasks.planFirst.hint')}
                </p>
                <p className="whitespace-pre-wrap text-sm text-foreground">{task.plan_pending}</p>
              </div>
            ) : (
              // The judge's / dispatcher's escalation reason — "看決定的依據" (§D.7).
              task.judge_feedback && (
                <p className="flex items-start gap-2 rounded-lg bg-muted px-3 py-2 text-xs text-muted-foreground">
                  <MessageSquareWarning className="mt-0.5 size-3.5 shrink-0" aria-hidden="true" />
                  <span>{task.judge_feedback}</span>
                </p>
              )
            )}
          </div>
        </TabsPanel>

        {/* WP-F (P2-c): the recorded file effects, so the decision rests on what
            the audit trail shows rather than on the agent's own account. */}
        <TabsPanel value="changes">
          {tab === 'changes' && <TaskChangesPanel taskId={task.id} />}
        </TabsPanel>
      </Tabs>

      <NeedsHumanActions taskId={task.id} onResolved={onResolved} />

      <div className="flex flex-wrap items-center gap-2">
        <Button variant="ghost" onClick={() => navigate(`/tasks/${task.id}`)}>
          <ExternalLink />
          {t('inbox.detail.viewTask')}
        </Button>
        {/* W2-3 reverse handoff (E8): jump back to the /goal conversation. */}
        <OpenInChannelButton channel={task.channel} link={task.channel_link} variant="ghost" />
      </div>
    </DetailShell>
  );
}
