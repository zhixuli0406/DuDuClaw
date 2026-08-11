import { useCallback, useState } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate } from 'react-router';
import { RotateCcw, CheckCircle2, XCircle, ExternalLink, Loader2, MessageSquareWarning } from 'lucide-react';
import { api, type TaskInfo } from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import { Button } from '@/components/mds';
import { DetailShell } from './DetailShell';
import { TYPE_META } from './meta';
import { OpenInChannelButton } from './OpenInChannelButton';

type ResolveAction = 'retry' | 'done' | 'abort';

/** Action → the `TaskStatus` the existing generic `tasks.update` RPC writes.
 *  Mirrors the channel-side three-choice resolution
 *  (`goal_notify::resolve_needs_human`'s `retry`/`done`/`abort` decisions) —
 *  reusing the RPC that already exists rather than adding a new one. */
const RESOLVE_STATUS: Record<ResolveAction, 'pending' | 'done' | 'cancelled'> = {
  retry: 'pending',
  done: 'done',
  abort: 'cancelled',
};

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
  const [busy, setBusy] = useState<ResolveAction | null>(null);

  const resolve = useCallback(
    async (action: ResolveAction) => {
      if (busy) return;
      setBusy(action);
      try {
        await api.tasks.update(task.id, { status: RESOLVE_STATUS[action] });
        toast.success(t(`inbox.needsHuman.${action}Toast`));
        onResolved();
      } catch (e) {
        toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
      } finally {
        setBusy(null);
      }
    },
    [busy, task.id, t, intl, onResolved],
  );

  return (
    <DetailShell icon={TYPE_META.blocked.icon} title={task.title} typeLabel={typeLabel} agentId={task.assigned_to} agentName={agentName}>
      {task.description && <p className="text-sm text-foreground">{task.description}</p>}

      {/* The judge's / dispatcher's escalation reason — "看決定的依據" (§D.7). */}
      {task.judge_feedback && (
        <p className="flex items-start gap-2 rounded-lg bg-muted px-3 py-2 text-xs text-muted-foreground">
          <MessageSquareWarning className="mt-0.5 size-3.5 shrink-0" aria-hidden="true" />
          <span>{task.judge_feedback}</span>
        </p>
      )}

      <div className="flex flex-wrap items-center gap-2">
        <Button variant="brand" disabled={!!busy} onClick={() => resolve('retry')}>
          {busy === 'retry' ? <Loader2 className="animate-spin" /> : <RotateCcw />}
          {t('inbox.needsHuman.retry')}
        </Button>
        <Button variant="outline" disabled={!!busy} onClick={() => resolve('done')}>
          {busy === 'done' ? <Loader2 className="animate-spin" /> : <CheckCircle2 />}
          {t('inbox.needsHuman.done')}
        </Button>
        <Button variant="destructive" disabled={!!busy} onClick={() => resolve('abort')}>
          {busy === 'abort' ? <Loader2 className="animate-spin" /> : <XCircle />}
          {t('inbox.needsHuman.abort')}
        </Button>
      </div>

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
