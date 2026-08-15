import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { FolderOpen, Download, Eye, AlertTriangle } from 'lucide-react';
import { Badge, Empty, Skeleton, buttonVariants } from '@/components/mds';
import { cn } from '@/lib/utils';
import { timeAgo } from '@/lib/format';
import { useAuthStore } from '@/stores/auth-store';
import { artifactIcon } from './TaskArtifactsPanel';

/**
 * I-2a 「檔案」— the flat `attachments/` inventory for one task, split into
 * 收到的 (a person uploaded it) vs 做出來的 (a run produced it) vs 來源不明
 * (the provenance ledger never saw it — `office_docs.rs`).
 *
 * This is deliberately the SAME provenance data `/files` (FilesPage) already
 * renders (I-2b): that page's `/api/files?agent=<id>` REST endpoint already
 * tags every row with `origin` + `task_id`. This panel reuses that endpoint
 * and filters client-side to one task, instead of standing up a parallel
 * backend path — I-2a is frontend-only. A searchable, task-correlated
 * `/files` page itself is I-4 (wave 3), out of scope here.
 *
 * Distinct from 產物 (`TaskArtifactsPanel`): that tab is the curated
 * hand-over ("what this task delivered"); this tab is the raw inventory
 * ("everything that landed in the bucket, whoever put it there").
 *
 * Same two-part split as its siblings: `TaskFilesList` is pure presentation,
 * `TaskFilesPanel` is the thin fetching wrapper.
 */

export interface TaskFileRow {
  name: string;
  size: number;
  /** Unix epoch milliseconds. */
  mtime: number;
  origin?: 'declared' | 'swept' | 'produced' | 'uploaded' | 'unknown';
  task_id?: string;
  round?: number;
  display_name?: string;
}

function isReceived(f: TaskFileRow): boolean {
  return f.origin === 'uploaded';
}
function isUnknownOrigin(f: TaskFileRow): boolean {
  return !f.origin || f.origin === 'unknown';
}

function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const units = ['KB', 'MB', 'GB', 'TB'];
  let value = bytes / 1024;
  let i = 0;
  while (value >= 1024 && i < units.length - 1) {
    value /= 1024;
    i += 1;
  }
  return `${value >= 100 ? Math.round(value) : value.toFixed(1)} ${units[i]}`;
}

/** PDFs and images stream inline; office types convert to PDF server-side.
 *  Anything else has no preview and only offers a download. */
function previewKind(name: string): 'inline' | 'office' | null {
  if (/\.(pdf|png|jpe?g|gif|webp)$/i.test(name)) return 'inline';
  if (/\.(docx?|xlsx?|pptx?|odt|ods|odp|csv)$/i.test(name)) return 'office';
  return null;
}

function FileRowItem({
  file,
  downloadHref,
  previewHref,
}: {
  file: TaskFileRow;
  downloadHref: string | null;
  previewHref: string | null;
}) {
  const intl = useIntl();
  const Icon = artifactIcon(file.name);

  return (
    <li className="flex items-start gap-2.5 rounded-lg px-2 py-2 transition-colors hover:bg-surface-hover">
      <Icon className="mt-0.5 size-4 shrink-0 text-muted-foreground" aria-hidden="true" />
      <div className="min-w-0 flex-1 space-y-1">
        <p className="break-all text-sm font-medium text-foreground">{file.display_name ?? file.name}</p>
        <div className="flex flex-wrap items-center gap-1.5 text-xs text-muted-foreground">
          <Badge variant="secondary">
            {intl.formatMessage({ id: `files.origin.${file.origin ?? 'unknown'}` })}
          </Badge>
          {file.round != null && (
            <span>{intl.formatMessage({ id: 'tasks.artifacts.round' }, { n: file.round })}</span>
          )}
          <span className="font-mono tabular-nums">{formatSize(file.size)}</span>
          <span className="font-mono tabular-nums">{timeAgo(file.mtime)}</span>
        </div>
      </div>
      <div className="flex shrink-0 items-center gap-1">
        {previewHref && (
          <a
            href={previewHref}
            target="_blank"
            rel="noreferrer"
            aria-label={intl.formatMessage({ id: 'files.preview' })}
            title={intl.formatMessage({ id: 'files.preview' })}
            className={buttonVariants({ variant: 'ghost', size: 'icon-sm' })}
          >
            <Eye className="size-3.5" />
          </a>
        )}
        {downloadHref && (
          <a
            href={downloadHref}
            download={file.display_name ?? file.name}
            aria-label={intl.formatMessage({ id: 'files.download' })}
            title={intl.formatMessage({ id: 'files.download' })}
            className={buttonVariants({ variant: 'ghost', size: 'icon-sm' })}
          >
            <Download className="size-3.5" />
          </a>
        )}
      </div>
    </li>
  );
}

/** Pure, props-driven rendering of one task's file inventory. */
export function TaskFilesList({
  files,
  agentId,
  loading = false,
  error,
  jwt,
  className,
}: {
  files: readonly TaskFileRow[];
  /** Needed to build `/api/files/*` links (and to know whether a bucket could
   *  even be resolved). Omit/empty ⇒ an honest "no assigned AI staff member"
   *  state instead of guessing a bucket. */
  agentId?: string | null;
  loading?: boolean;
  error?: string | null;
  jwt?: string | null;
  className?: string;
}) {
  const intl = useIntl();

  const fileUrl = (base: string, name: string): string => {
    const params = new URLSearchParams();
    if (agentId) params.set('agent', agentId);
    params.set('name', name);
    if (jwt) params.set('token', jwt);
    return `${base}?${params.toString()}`;
  };

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
        title={intl.formatMessage({ id: 'files.error' })}
        description={error}
        className={className}
      />
    );
  }

  if (!agentId) {
    return <Empty icon={FolderOpen} title={intl.formatMessage({ id: 'tasks.files.noAgent' })} className={className} />;
  }

  if (files.length === 0) {
    return (
      <Empty
        icon={FolderOpen}
        title={intl.formatMessage({ id: 'tasks.files.empty' })}
        description={intl.formatMessage({ id: 'tasks.files.emptyHint' })}
        className={className}
      />
    );
  }

  const received = files.filter(isReceived);
  const produced = files.filter((f) => !isReceived(f) && !isUnknownOrigin(f));
  const unknown = files.filter((f) => !isReceived(f) && isUnknownOrigin(f));

  const section = (labelId: string, rows: TaskFileRow[]) =>
    rows.length > 0 && (
      <div className="space-y-1" key={labelId}>
        <h3 className="px-2 text-xs font-medium text-muted-foreground">
          {intl.formatMessage({ id: labelId })}
          <span className="ml-1.5 font-mono tabular-nums">{rows.length}</span>
        </h3>
        <ol className="space-y-1">
          {rows.map((f) => {
            const preview = previewKind(f.name);
            const downloadHref = fileUrl('/api/files/download', f.name);
            const previewHref = preview === 'office' ? fileUrl('/api/files/preview', f.name) : downloadHref;
            return (
              <FileRowItem key={f.name} file={f} downloadHref={downloadHref} previewHref={preview ? previewHref : null} />
            );
          })}
        </ol>
      </div>
    );

  return (
    <div className={cn('space-y-4', className)}>
      <p className="px-2 text-xs text-muted-foreground">
        {intl.formatMessage({ id: 'tasks.files.summary' }, { count: files.length })}
      </p>
      {section('tasks.files.received', received)}
      {section('tasks.files.produced', produced)}
      {section('files.origin.unknown', unknown)}
    </div>
  );
}

/** `TaskFilesList` plus the `/api/files` fetch for one task's agent, filtered
 *  client-side to `task_id === taskId` — see the module doc for why there is
 *  no dedicated per-task backend endpoint yet. */
export function TaskFilesPanel({
  taskId,
  agentId,
  className,
}: {
  taskId: string;
  agentId?: string | null;
  className?: string;
}) {
  const jwt = useAuthStore((s) => s.jwt);
  const [files, setFiles] = useState<TaskFileRow[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!agentId) {
      setFiles([]);
      setLoading(false);
      setError(null);
      return;
    }
    let cancelled = false;
    setLoading(true);
    setError(null);
    const params = new URLSearchParams({ agent: agentId });
    fetch(`/api/files?${params.toString()}`, {
      headers: jwt ? { Authorization: `Bearer ${jwt}` } : {},
    })
      .then((res) => {
        if (!res.ok) throw new Error(`HTTP ${res.status}`);
        return res.json();
      })
      .then((data: { files?: TaskFileRow[] }) => {
        if (cancelled) return;
        const all = Array.isArray(data?.files) ? data.files : [];
        setFiles(all.filter((f) => f.task_id === taskId));
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
  }, [taskId, agentId, jwt]);

  return (
    <TaskFilesList files={files} agentId={agentId} loading={loading} error={error} jwt={jwt} className={className} />
  );
}
