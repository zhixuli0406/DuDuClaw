import { useEffect, useMemo, useState } from 'react';
import { useIntl } from 'react-intl';
import {
  Package,
  FileText,
  FileSpreadsheet,
  FileImage,
  FileArchive,
  Presentation,
  File as FileIcon,
  Download,
  Eye,
  AlertTriangle,
  Info,
} from 'lucide-react';
import { Badge, Empty, Skeleton, buttonVariants } from '@/components/mds';
import { cn } from '@/lib/utils';
import { timeAgo } from '@/lib/format';
import { useAuthStore } from '@/stores/auth-store';
import { api, type TaskArtifact, type TaskArtifacts } from '@/lib/api';

/**
 * I-2b 「產物」— what a task actually produced.
 *
 * The task detail page could show every round, every judge verdict and every
 * comment, but not the one thing the person who assigned the work came for:
 * 東西在哪. Deliverables were already *identified* in the gateway (`📎DELIVER:`
 * declarations plus the undeclared-file sweep) — their provenance was simply
 * thrown away on the way to the dashboard. This panel renders it back.
 *
 * ## Honesty rules
 *
 * - Every row says how it got here: 主動交付 / 自動回收 / 執行中產出 / 你上傳的 /
 *   來源不明. They are different facts and the UI never blurs them.
 * - A row tied to this task only by its time window is badged 依時間推定; the
 *   panel never presents an inference as a fact.
 * - A file that was written but never archived has no download button — we say
 *   where it was written instead of offering a link that would 404.
 * - No evidence ⇒ the empty state says exactly that, never "this task produced
 *   nothing".
 *
 * `TaskArtifactsList` is pure presentation so the coming 成品畫廊 (P2-b) can
 * reuse it with a different row source; `TaskArtifactsPanel` is the thin
 * fetching wrapper.
 */

const EXT_ICON: Array<[RegExp, typeof FileIcon]> = [
  [/\.(xlsx?|csv|ods)$/i, FileSpreadsheet],
  [/\.(pptx?|odp)$/i, Presentation],
  [/\.(png|jpe?g|gif|webp|svg)$/i, FileImage],
  [/\.(zip|tar|gz|7z)$/i, FileArchive],
  [/\.(pdf|docx?|odt|md|txt|html?)$/i, FileText],
];

/** Type icon from the filename — a visual shortcut, never a claim about
 *  content (the extension is all we know). */
export function artifactIcon(name: string) {
  return EXT_ICON.find(([re]) => re.test(name))?.[1] ?? FileIcon;
}

/** PDFs and images stream inline; office types convert to PDF server-side.
 *  Anything else has no preview and only offers a download. */
function previewKind(name: string): 'inline' | 'office' | null {
  if (/\.(pdf|png|jpe?g|gif|webp)$/i.test(name)) return 'inline';
  if (/\.(docx?|xlsx?|pptx?|odt|ods|odp|csv)$/i.test(name)) return 'office';
  return null;
}

const ORIGIN_TONE: Record<TaskArtifact['origin'], string> = {
  declared: 'text-brand',
  swept: 'text-brand',
  produced: 'text-muted-foreground',
  uploaded: 'text-muted-foreground',
  unknown: 'text-muted-foreground',
};

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

function ArtifactRow({
  artifact,
  fileUrl,
}: {
  artifact: TaskArtifact;
  fileUrl: (base: string, a: TaskArtifact) => string | null;
}) {
  const intl = useIntl();
  const Icon = artifactIcon(artifact.name);
  const preview = artifact.archived_name ? previewKind(artifact.name) : null;
  const downloadHref = fileUrl('/api/files/download', artifact);
  const previewHref =
    preview === 'office' ? fileUrl('/api/files/preview', artifact) : downloadHref;

  return (
    <li className="flex items-start gap-2.5 rounded-lg px-2 py-2 transition-colors hover:bg-surface-hover">
      <Icon
        className={cn('mt-0.5 size-4 shrink-0', ORIGIN_TONE[artifact.origin])}
        aria-hidden="true"
      />
      <div className="min-w-0 flex-1 space-y-1">
        <p className="break-all text-sm font-medium text-foreground">{artifact.name}</p>

        <div className="flex flex-wrap items-center gap-1.5 text-xs text-muted-foreground">
          <Badge variant="secondary">
            {intl.formatMessage({ id: `tasks.artifacts.origin.${artifact.origin}` })}
          </Badge>
          {artifact.attribution === 'inferred' && (
            <Badge variant="outline">
              <Info aria-hidden="true" />
              {intl.formatMessage({ id: 'tasks.artifacts.inferred' })}
            </Badge>
          )}
          {artifact.round != null && (
            <span>{intl.formatMessage({ id: 'tasks.artifacts.round' }, { n: artifact.round })}</span>
          )}
          {artifact.size != null && (
            <span className="font-mono tabular-nums">{formatSize(artifact.size)}</span>
          )}
          <span className="font-mono tabular-nums">{timeAgo(artifact.produced_at)}</span>
        </div>

        {/* Written but never archived — say where it is instead of offering a
            link that cannot resolve. */}
        {!artifact.archived_name && artifact.source_path && (
          <p className="break-all font-mono text-[11px] text-muted-foreground">
            {intl.formatMessage(
              { id: 'tasks.artifacts.writtenAt' },
              { path: artifact.source_path },
            )}
          </p>
        )}
      </div>

      {artifact.archived_name && (
        <div className="flex shrink-0 items-center gap-1">
          {preview && previewHref && (
            <a
              href={previewHref}
              target="_blank"
              rel="noreferrer"
              aria-label={intl.formatMessage({ id: 'tasks.artifacts.open' })}
              title={intl.formatMessage({ id: 'tasks.artifacts.open' })}
              className={buttonVariants({ variant: 'ghost', size: 'icon-sm' })}
            >
              <Eye className="size-3.5" />
            </a>
          )}
          {downloadHref && (
            <a
              href={downloadHref}
              download={artifact.name}
              aria-label={intl.formatMessage({ id: 'tasks.artifacts.download' })}
              title={intl.formatMessage({ id: 'tasks.artifacts.download' })}
              className={buttonVariants({ variant: 'ghost', size: 'icon-sm' })}
            >
              <Download className="size-3.5" />
            </a>
          )}
        </div>
      )}
    </li>
  );
}

/** Pure, props-driven rendering of a task's deliverables. */
export function TaskArtifactsList({
  artifacts,
  inferredCount = 0,
  truncated = false,
  loading = false,
  error,
  jwt,
  className,
}: {
  artifacts: readonly TaskArtifact[];
  inferredCount?: number;
  truncated?: boolean;
  loading?: boolean;
  error?: string | null;
  /** Bearer token for the plain `<a href>` file links (they cannot set a
   *  header). Omit and the row renders without working links. */
  jwt?: string | null;
  className?: string;
}) {
  const intl = useIntl();

  const fileUrl = useMemo(
    () =>
      (base: string, a: TaskArtifact): string | null => {
        if (!a.archived_name) return null;
        const params = new URLSearchParams();
        if (a.agent_id) params.set('agent', a.agent_id);
        params.set('name', a.archived_name);
        if (jwt) params.set('token', jwt);
        return `${base}?${params.toString()}`;
      },
    [jwt],
  );

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
        title={intl.formatMessage({ id: 'tasks.artifacts.error' })}
        description={error}
        className={className}
      />
    );
  }

  if (artifacts.length === 0) {
    return (
      <Empty
        icon={Package}
        title={intl.formatMessage({ id: 'tasks.artifacts.empty' })}
        description={intl.formatMessage({ id: 'tasks.artifacts.emptyHint' })}
        className={className}
      />
    );
  }

  return (
    <div className={cn('space-y-2', className)}>
      <p className="px-2 text-xs text-muted-foreground">
        {intl.formatMessage({ id: 'tasks.artifacts.summary' }, { count: artifacts.length })}
        {inferredCount > 0 &&
          ` ${intl.formatMessage({ id: 'tasks.artifacts.inferredNote' }, { n: inferredCount })}`}
      </p>
      <ol className="space-y-1">
        {artifacts.map((a, i) => (
          <ArtifactRow key={`${a.archived_name ?? a.source_path ?? a.name}-${i}`} artifact={a} fileUrl={fileUrl} />
        ))}
      </ol>
      {truncated && (
        <p className="px-2 text-xs text-muted-foreground">
          {intl.formatMessage({ id: 'tasks.artifacts.truncated' })}
        </p>
      )}
    </div>
  );
}

/** `TaskArtifactsList` plus the `tasks.artifacts` fetch for one task. */
export function TaskArtifactsPanel({ taskId, className }: { taskId: string; className?: string }) {
  const jwt = useAuthStore((s) => s.jwt);
  const [data, setData] = useState<TaskArtifacts | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);
    api.tasks
      .artifacts(taskId)
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
    <TaskArtifactsList
      artifacts={data?.artifacts ?? []}
      inferredCount={data?.inferred_count ?? 0}
      truncated={data?.truncated ?? false}
      loading={loading}
      error={error}
      jwt={jwt}
      className={className}
    />
  );
}
