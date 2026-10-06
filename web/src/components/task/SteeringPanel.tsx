import { useCallback, useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { Button, Textarea } from '@/components/mds';
import { api, type TaskSteeringEntry } from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import { timeAgo } from '@/lib/format';
import { errorCode } from './StopTaskButton';

/** Same limit as the server (`STEERING_BODY_MAX_CHARS`). */
const MAX_LEN = 4000;

function newRequestId(): string {
  try {
    return crypto.randomUUID();
  } catch {
    return `${Date.now()}-${Math.random().toString(36).slice(2)}`;
  }
}

/** One line saying where a direction is, in plain words. */
function entryStateLabel(entry: TaskSteeringEntry, fmt: (id: string, v?: Record<string, unknown>) => string) {
  switch (entry.state) {
    case 'pending':
      return fmt('tasks.steering.state.pending');
    case 'delivering':
      return fmt('tasks.steering.state.delivering');
    case 'applied':
      return fmt('tasks.steering.state.applied', { round: entry.applied_round ?? '?' });
    case 'discarded':
      return fmt(
        entry.discard_reason === 'task_cancelled'
          ? 'tasks.steering.state.discardedStopped'
          : 'tasks.steering.state.discardedFinished',
      );
    default:
      return fmt('tasks.steering.state.unknown');
  }
}

/** Whether the server's injection scan flagged an entry (unreadable ⇒ yes). */
export function isSuspicious(entry: TaskSteeringEntry): boolean {
  if (!entry.guard_flags_json) return false;
  try {
    const v = JSON.parse(entry.guard_flags_json) as { risk_score?: number; matched_rules?: unknown[] };
    return (v.risk_score ?? 0) > 0 || (Array.isArray(v.matched_rules) && v.matched_rules.length > 0);
  } catch {
    return true;
  }
}

/** A user-facing sentence for a refused send. */
function submitErrorId(e: unknown): string | null {
  switch (errorCode(e)) {
    case 'steering_limit':
      return 'tasks.steering.error.limit';
    case 'task_stopped':
      return 'tasks.steering.error.stopped';
    case 'task_finished':
      return 'tasks.steering.error.finished';
    case 'invalid_body':
      return 'tasks.steering.error.length';
    case 'steering_disabled':
      return 'tasks.steering.error.disabled';
    default:
      return null;
  }
}

/**
 * P2-A — give the AI employee one direction for its next round of a goal
 * task. Everything shown comes from the server; a response that arrives after
 * the page switched to another task is dropped instead of being written into
 * the new task's view.
 */
export function SteeringPanel({ taskId, terminal }: { taskId: string; terminal: boolean }) {
  const intl = useIntl();
  const fmt = useCallback(
    (id: string, values?: Record<string, unknown>) =>
      intl.formatMessage({ id }, values as Record<string, string | number>),
    [intl],
  );
  const current = useRef(taskId);
  const requestId = useRef(newRequestId());
  const [entries, setEntries] = useState<TaskSteeringEntry[]>([]);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [body, setBody] = useState('');
  const [busy, setBusy] = useState(false);

  const load = useCallback(async () => {
    const id = taskId;
    try {
      const r = await api.tasks.steering(id);
      if (current.current !== id) return;
      setEntries(Array.isArray(r?.steering) ? r.steering : []);
      setLoadError(null);
    } catch (e) {
      if (current.current !== id) return;
      setLoadError(formatError(e));
    }
  }, [taskId]);

  useEffect(() => {
    current.current = taskId;
    requestId.current = newRequestId();
    setEntries([]);
    setBody('');
    setLoadError(null);
    void load();
  }, [taskId, load]);

  const submit = useCallback(async () => {
    const id = taskId;
    const text = body.trim();
    if (!text || busy) return;
    setBusy(true);
    try {
      await api.tasks.steer(id, text, requestId.current);
      if (current.current !== id) return;
      requestId.current = newRequestId();
      setBody('');
      toast.success(fmt('tasks.steering.sent'));
      await load();
    } catch (e) {
      if (current.current !== id) return;
      const known = submitErrorId(e);
      toast.error(known ? fmt(known) : fmt('tasks.steering.failed', { message: formatError(e) }));
    } finally {
      if (current.current === id) setBusy(false);
    }
  }, [taskId, body, busy, fmt, load]);

  return (
    <section
      aria-label={fmt('tasks.steering.title')}
      className="space-y-2 rounded-xl border border-surface-border bg-surface p-4"
    >
      <div>
        <h2 className="text-sm font-medium text-foreground">{fmt('tasks.steering.title')}</h2>
        <p className="text-xs text-muted-foreground">{fmt('tasks.steering.hint')}</p>
      </div>
      {loadError && (
        <p role="alert" className="text-xs text-rose-600 dark:text-rose-400">
          {fmt('tasks.steering.loadFailed', { message: loadError })}
        </p>
      )}
      {entries.length > 0 && (
        <ul className="space-y-1.5">
          {entries.map((e) => (
            <li key={e.steering_id} className="rounded-lg bg-background px-3 py-2 text-sm">
              <p className="whitespace-pre-wrap text-foreground">{e.body}</p>
              {isSuspicious(e) && (
                <p className="mt-1 text-xs text-amber-700 dark:text-amber-400">
                  {fmt('tasks.steering.suspicious')}
                </p>
              )}
              <p className="mt-1 text-xs text-muted-foreground">
                {entryStateLabel(e, fmt)} · {timeAgo(e.created_at)}
              </p>
            </li>
          ))}
        </ul>
      )}
      {terminal ? (
        <p className="text-xs text-muted-foreground">{fmt('tasks.steering.closed')}</p>
      ) : (
        <div className="space-y-2">
          <Textarea
            value={body}
            maxLength={MAX_LEN}
            onChange={(e) => setBody(e.target.value)}
            placeholder={fmt('tasks.steering.placeholder')}
            aria-label={fmt('tasks.steering.placeholder')}
            rows={3}
          />
          <div className="flex justify-end">
            <Button size="sm" disabled={busy || !body.trim()} onClick={() => void submit()}>
              {fmt('tasks.steering.submit')}
            </Button>
          </div>
        </div>
      )}
    </section>
  );
}
