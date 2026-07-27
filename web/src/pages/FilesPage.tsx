import { useCallback, useEffect, useMemo, useState } from 'react';
import { useIntl } from 'react-intl';
import { Download, Eye, FolderOpen } from 'lucide-react';
import { useAuthStore } from '@/stores/auth-store';
import { useAgentsStore } from '@/stores/agents-store';
import { useDataScope, useVisibleAgents } from '@/lib/data-scope';
import {
  PageHeader,
  Button,
  buttonVariants,
  Empty,
  Skeleton,
  Select,
  SelectTrigger,
  SelectValue,
  SelectContent,
  SelectItem,
  Table,
  TableHeader,
  TableBody,
  TableRow,
  TableHead,
  TableCell,
  Tooltip,
  TooltipTrigger,
  TooltipContent,
} from '@/components/mds';

/**
 * FilesPage (WP1.4 file panel) — 檔案. Lists the attachment files an AI staff
 * member received or produced (`~/.duduclaw/agents/<id>/attachments/`), with a
 * shared fallback bucket. Files download through the Bearer-JWT-gated
 * `/api/files/download` endpoint; PDFs and images open inline in a new tab for
 * native preview. Everything shown is the real directory listing — an agent
 * that has produced nothing simply shows the empty state.
 */

/** Sentinel Select value for the shared (agent-less) bucket. */
const SHARED = '__shared__';
const REFRESH_MS = 30_000;

interface FileRow {
  name: string;
  size: number;
  /** Unix epoch milliseconds. */
  mtime: number;
}

/** Human-readable byte size (1 KB = 1024 B). */
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

/** Only PDFs and images preview natively; everything else force-downloads. */
function isPreviewable(name: string): boolean {
  return /\.(pdf|png|jpe?g|gif|webp)$/i.test(name);
}

export function FilesPage() {
  const intl = useIntl();
  const jwt = useAuthStore((s) => s.jwt);
  const fetchAgents = useAgentsStore((s) => s.fetchAgents);
  const scope = useDataScope();
  const visibleAgents = useVisibleAgents();

  // Admins may browse the shared bucket; scoped users start on their first
  // visible AI staff member (the gateway fails closed without an agent).
  const [selected, setSelected] = useState<string>('');
  const [files, setFiles] = useState<FileRow[]>([]);
  const [loaded, setLoaded] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    fetchAgents();
  }, [fetchAgents]);

  // Resolve the effective selection once agents load.
  const effective =
    selected || (scope === 'all' ? SHARED : (visibleAgents[0]?.name ?? ''));

  const dateFmt = useMemo(
    () =>
      new Intl.DateTimeFormat(intl.locale, {
        dateStyle: 'medium',
        timeStyle: 'short',
      }),
    [intl.locale],
  );

  const buildUrl = useCallback(
    (base: string, name?: string): string => {
      const params = new URLSearchParams();
      if (effective && effective !== SHARED) params.set('agent', effective);
      if (name) params.set('name', name);
      const qs = params.toString();
      return `${base}${qs ? `?${qs}` : ''}`;
    },
    [effective],
  );

  const fetchFiles = useCallback(async () => {
    if (!effective) return; // nothing selectable yet
    if (scope !== 'all' && effective === SHARED) return;
    try {
      const res = await fetch(buildUrl('/api/files'), {
        headers: jwt ? { Authorization: `Bearer ${jwt}` } : {},
      });
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const data = await res.json();
      setFiles(Array.isArray(data?.files) ? data.files : []);
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoaded(true);
    }
  }, [effective, scope, buildUrl, jwt]);

  useEffect(() => {
    setLoaded(false);
    setFiles([]);
    fetchFiles();
    const t = setInterval(fetchFiles, REFRESH_MS);
    return () => clearInterval(t);
  }, [fetchFiles]);

  /** Download URL carries the JWT as a query param so plain browser links
   *  (which can't set an Authorization header) still authenticate. */
  const downloadUrl = useCallback(
    (name: string): string => {
      const url = buildUrl('/api/files/download', name);
      return jwt ? `${url}&token=${encodeURIComponent(jwt)}` : url;
    },
    [buildUrl, jwt],
  );

  const triggerDownload = useCallback(
    (name: string) => {
      const a = document.createElement('a');
      a.href = downloadUrl(name);
      a.download = name;
      document.body.appendChild(a);
      a.click();
      a.remove();
    },
    [downloadUrl],
  );

  const showShared = scope === 'all';

  return (
    <div className="flex h-full min-h-0 flex-col">
      <PageHeader hideTrigger>
        <FolderOpen className="size-4 shrink-0 text-muted-foreground" />
        <h1 className="truncate text-sm font-medium">
          {intl.formatMessage({ id: 'nav.files' })}
        </h1>
        <span className="font-mono text-xs tabular-nums text-muted-foreground">
          {files.length}
        </span>
        <div className="ml-auto">
          <Select value={effective} onValueChange={(v) => setSelected(String(v))}>
            <SelectTrigger size="sm" className="max-w-44">
              <SelectValue
                aria-label={intl.formatMessage({ id: 'files.filter.agent' })}
                placeholder={intl.formatMessage({ id: 'files.filter.agent' })}
              />
            </SelectTrigger>
            <SelectContent>
              {showShared && (
                <SelectItem value={SHARED}>
                  {intl.formatMessage({ id: 'files.scope.shared' })}
                </SelectItem>
              )}
              {visibleAgents.map((a) => (
                <SelectItem key={a.name} value={a.name}>
                  {a.display_name || a.name}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
      </PageHeader>

      <div className="min-h-0 flex-1 overflow-y-auto p-4">
        <p className="mb-4 text-xs text-muted-foreground">
          {intl.formatMessage({ id: 'files.subtitle' })}
        </p>

        {!loaded ? (
          <div className="space-y-3">
            <Skeleton className="h-10 w-full" />
            <Skeleton className="h-10 w-full" />
            <Skeleton className="h-10 w-2/3" />
          </div>
        ) : error ? (
          <Empty
            icon={FolderOpen}
            tone="destructive"
            title={intl.formatMessage({ id: 'files.error' })}
            description={error}
          />
        ) : files.length === 0 ? (
          <Empty
            icon={FolderOpen}
            title={intl.formatMessage({ id: 'files.empty' })}
            description={intl.formatMessage({ id: 'files.empty.hint' })}
          />
        ) : (
          <div className="overflow-x-auto">
            <Table className="min-w-[32rem]">
              <TableHeader>
                <TableRow>
                  <TableHead>{intl.formatMessage({ id: 'files.col.name' })}</TableHead>
                  <TableHead className="w-24 text-right">
                    {intl.formatMessage({ id: 'files.col.size' })}
                  </TableHead>
                  <TableHead className="w-44">
                    {intl.formatMessage({ id: 'files.col.time' })}
                  </TableHead>
                  <TableHead className="w-24 text-right">
                    {intl.formatMessage({ id: 'files.col.actions' })}
                  </TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {files.map((f) => (
                  <TableRow key={f.name}>
                    <TableCell className="font-medium break-all text-foreground">
                      {f.name}
                    </TableCell>
                    <TableCell className="text-right font-mono text-xs tabular-nums text-muted-foreground">
                      {formatSize(f.size)}
                    </TableCell>
                    <TableCell className="text-xs text-muted-foreground tabular-nums">
                      {f.mtime ? dateFmt.format(new Date(f.mtime)) : '—'}
                    </TableCell>
                    <TableCell>
                      <div className="flex items-center justify-end gap-1">
                        {isPreviewable(f.name) && (
                          <Tooltip>
                            <TooltipTrigger
                              render={
                                <a
                                  href={downloadUrl(f.name)}
                                  target="_blank"
                                  rel="noreferrer"
                                  aria-label={intl.formatMessage({ id: 'files.preview' })}
                                  className={buttonVariants({
                                    variant: 'ghost',
                                    size: 'icon-sm',
                                  })}
                                >
                                  <Eye className="size-3.5" />
                                </a>
                              }
                            />
                            <TooltipContent>
                              {intl.formatMessage({ id: 'files.preview' })}
                            </TooltipContent>
                          </Tooltip>
                        )}
                        <Tooltip>
                          <TooltipTrigger
                            render={
                              <Button
                                variant="ghost"
                                size="icon-sm"
                                aria-label={intl.formatMessage({ id: 'files.download' })}
                                onClick={() => triggerDownload(f.name)}
                              >
                                <Download />
                              </Button>
                            }
                          />
                          <TooltipContent>
                            {intl.formatMessage({ id: 'files.download' })}
                          </TooltipContent>
                        </Tooltip>
                      </div>
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </div>
        )}
      </div>
    </div>
  );
}

export default FilesPage;
