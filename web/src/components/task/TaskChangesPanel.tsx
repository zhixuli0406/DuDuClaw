import { useCallback, useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { FileDiff, FilePlus2, FilePenLine, FileX2, Terminal, Copy, Check, AlertTriangle } from 'lucide-react';
import { Badge, Empty, Skeleton } from '@/components/mds';
import { cn } from '@/lib/utils';
import { timeAgo } from '@/lib/format';
import { api, type TaskChange, type TaskChanges } from '@/lib/api';

/**
 * WP-F (P2-c) 「變更」— the file effects a task actually left behind.
 *
 * Borrowed from WorkBuddy's changes tab: before a human approves a stuck
 * ("等你決定") task, they should be able to see WHAT it touched, not just read
 * the agent's own account of it. Pairs with the approval card's simulation
 * section — simulation is "what would happen", this is "what already did".
 *
 * ## Component split (deliberate)
 *
 * `TaskChangesList` is pure presentation, props-driven, and knows nothing
 * about tasks, fetching or approvals — the same list renders inside the task
 * detail page's footer tabs and inside the Inbox decision card, and the next
 * UX wave can drop it anywhere else (e.g. a real before/after diff view) by
 * feeding it different rows. `TaskChangesPanel` is the thin fetching wrapper.
 */

const OP_ICON = {
  write: FilePlus2,
  edit: FilePenLine,
  delete: FileX2,
  shell: Terminal,
} as const;

/** Semantic tint per operation — write/edit read as ordinary work, delete and
 *  failures are the two things a reviewer must not miss. */
const OP_TONE: Record<TaskChange['op'], string> = {
  write: 'text-brand',
  edit: 'text-brand',
  delete: 'text-destructive',
  shell: 'text-muted-foreground',
};

function CopyPathButton({ value, label, copiedLabel }: { value: string; label: string; copiedLabel: string }) {
  const [copied, setCopied] = useState(false);

  const copy = useCallback(async () => {
    try {
      await navigator.clipboard.writeText(value);
      setCopied(true);
    } catch {
      // Clipboard blocked (insecure context / permission) — the path stays
      // selectable in the DOM, so silently leaving the button un-ticked is
      // the honest outcome, not an error toast.
    }
  }, [value]);

  useEffect(() => {
    if (!copied) return;
    const id = window.setTimeout(() => setCopied(false), 1600);
    return () => window.clearTimeout(id);
  }, [copied]);

  return (
    <button
      type="button"
      onClick={() => void copy()}
      title={copied ? copiedLabel : label}
      aria-label={copied ? copiedLabel : label}
      className="shrink-0 rounded-md p-1 text-muted-foreground transition-colors hover:bg-surface-hover hover:text-foreground focus-visible:outline-none focus-visible:ring-3 focus-visible:ring-ring/50"
    >
      {copied ? (
        <Check className="size-3.5 text-success" aria-hidden="true" />
      ) : (
        <Copy className="size-3.5" aria-hidden="true" />
      )}
    </button>
  );
}

function ChangeRow({ change }: { change: TaskChange }) {
  const intl = useIntl();
  const Icon = OP_ICON[change.op] ?? FileDiff;
  const isShell = change.op === 'shell';

  return (
    <li
      className={cn(
        'flex items-start gap-2.5 rounded-lg px-2 py-2 transition-colors hover:bg-surface-hover',
        !change.success && 'bg-destructive/5',
      )}
    >
      <Icon className={cn('mt-0.5 size-4 shrink-0', OP_TONE[change.op])} aria-hidden="true" />
      <div className="min-w-0 flex-1 space-y-1">
        <div className="flex items-start gap-1.5">
          <span
            className="min-w-0 flex-1 break-all font-mono text-xs leading-relaxed text-foreground"
            title={change.path}
          >
            {change.path}
          </span>
          <CopyPathButton
            value={change.path}
            label={intl.formatMessage({ id: isShell ? 'tasks.changes.copyCommand' : 'tasks.changes.copyPath' })}
            copiedLabel={intl.formatMessage({ id: 'tasks.changes.copied' })}
          />
        </div>

        <div className="flex flex-wrap items-center gap-1.5 text-xs text-muted-foreground">
          <Badge variant={change.op === 'delete' ? 'destructive' : 'secondary'}>
            {intl.formatMessage({ id: `tasks.changes.op.${change.op}` })}
          </Badge>
          {!change.success && (
            <Badge variant="outline" className="text-destructive">
              <AlertTriangle aria-hidden="true" />
              {intl.formatMessage({ id: 'tasks.changes.failed' })}
            </Badge>
          )}
          <span className="font-mono text-[11px]">{change.tool_name}</span>
          {change.round != null && (
            <span>{intl.formatMessage({ id: 'tasks.changes.round' }, { n: change.round })}</span>
          )}
          <span className="font-mono tabular-nums">{timeAgo(change.timestamp)}</span>
        </div>

        {change.snippet && (
          <p className="whitespace-pre-wrap break-words rounded-md bg-muted px-2 py-1 text-[11px] leading-relaxed text-muted-foreground">
            {change.snippet}
          </p>
        )}
      </div>
    </li>
  );
}

/**
 * TaskChangesList — pure, props-driven rendering of change rows. No data
 * fetching, no task/approval coupling: hand it rows and it renders them.
 */
export function TaskChangesList({
  changes,
  distinctPaths = 0,
  truncated = false,
  loading = false,
  error,
  className,
}: {
  changes: readonly TaskChange[];
  distinctPaths?: number;
  truncated?: boolean;
  loading?: boolean;
  error?: string | null;
  className?: string;
}) {
  const intl = useIntl();

  if (loading) {
    return (
      <div className={cn('space-y-2 py-2', className)} aria-busy="true">
        <Skeleton className="h-12 w-full" />
        <Skeleton className="h-12 w-full" />
      </div>
    );
  }

  if (error) {
    return (
      <Empty
        icon={AlertTriangle}
        tone="destructive"
        title={intl.formatMessage({ id: 'tasks.changes.error' })}
        description={error}
        className={className}
      />
    );
  }

  // Honest empty: the audit trail recorded nothing, so we say exactly that
  // instead of implying the task made no changes at all.
  if (changes.length === 0) {
    return (
      <Empty
        icon={FileDiff}
        title={intl.formatMessage({ id: 'tasks.changes.empty' })}
        description={intl.formatMessage({ id: 'tasks.changes.emptyHint' })}
        className={className}
      />
    );
  }

  return (
    <div className={cn('space-y-2', className)}>
      <p className="px-2 text-xs text-muted-foreground">
        {intl.formatMessage(
          { id: 'tasks.changes.summary' },
          { files: distinctPaths, records: changes.length },
        )}
      </p>
      <ol className="space-y-1">
        {changes.map((change, i) => (
          <ChangeRow key={`${change.timestamp}-${change.path}-${i}`} change={change} />
        ))}
      </ol>
      {truncated && (
        <p className="px-2 text-xs text-muted-foreground">
          {intl.formatMessage({ id: 'tasks.changes.truncated' })}
        </p>
      )}
    </div>
  );
}

/**
 * TaskChangesPanel — `TaskChangesList` plus the `tasks.changes` fetch for one
 * task. Kept separate so the list stays reusable with any row source.
 */
export function TaskChangesPanel({ taskId, className }: { taskId: string; className?: string }) {
  const [data, setData] = useState<TaskChanges | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);
    api.tasks
      .changes(taskId)
      .then((res) => {
        if (cancelled) return;
        setData(res);
      })
      .catch((e: unknown) => {
        if (cancelled) return;
        setError(e instanceof Error ? e.message : String(e));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [taskId]);

  return (
    <TaskChangesList
      changes={data?.changes ?? []}
      distinctPaths={data?.distinct_paths ?? 0}
      truncated={data?.truncated ?? false}
      loading={loading}
      error={error}
      className={className}
    />
  );
}
