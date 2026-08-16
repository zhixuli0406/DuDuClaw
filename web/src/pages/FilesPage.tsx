import { useCallback, useEffect, useMemo, useState } from 'react';
import { useUrlState } from '@/lib/use-url-state';
import { useIntl } from 'react-intl';
import { Download, Eye, FolderOpen, Search } from 'lucide-react';
import { useAuthStore } from '@/stores/auth-store';
import { useAgentsStore } from '@/stores/agents-store';
import { useDataScope, useVisibleAgents } from '@/lib/data-scope';
import {
  PageHeader,
  Badge,
  Button,
  buttonVariants,
  Empty,
  ErrorState,
  Input,
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
 *
 * I-2b adds the 來源 column: the flat directory used to mix a customer's upload
 * with a report the agent delivered for a goal task, with no way to tell them
 * apart. The gateway now records provenance at the moment a file lands, and
 * this page joins it — plus a task filter, so 「找回上週的產物」 stops being a
 * scroll through everything. A file the ledger does not know keeps saying
 * 來源不明 rather than being assigned a plausible-looking origin.
 *
 * I-4 adds real server round trips for two more filters: a debounced text
 * search (`q` — matches archived name / display name / origin, same haystack
 * `files_api::filter_files` already builds) and an inclusive date range
 * (`since`/`until`, epoch ms). The task filter deliberately stays CLIENT-side
 * (unchanged from I-2b): it is derived from whatever `q`/date-filtered set the
 * server already returned, so switching between tasks never needs a second
 * round trip and the dropdown's option list never collapses to just the
 * currently-selected task.
 */

/** Sentinel Select value for the shared (agent-less) bucket. */
const SHARED = '__shared__';
const REFRESH_MS = 30_000;

interface FileRow {
  name: string;
  size: number;
  /** Unix epoch milliseconds. */
  mtime: number;
  /** I-2b provenance — absent when the ledger has no row for this file. */
  origin?: 'declared' | 'swept' | 'produced' | 'uploaded' | 'unknown';
  task_id?: string;
  round?: number;
  display_name?: string;
}

/** Sentinel Select value for "no task filter". */
const ALL_TASKS = '__all__';
/** Sentinel Select value for files not tied to any task. */
const NO_TASK = '__none__';

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

/** PDFs and images preview natively (streamed inline as-is). */
function isPreviewable(name: string): boolean {
  return /\.(pdf|png|jpe?g|gif|webp)$/i.test(name);
}

/** Office types preview via the gateway's LibreOffice→PDF conversion. */
function isOfficePreviewable(name: string): boolean {
  return /\.(docx?|xlsx?|pptx?|odt|ods|odp|csv)$/i.test(name);
}

export function FilesPage() {
  const intl = useIntl();
  const jwt = useAuthStore((s) => s.jwt);
  const fetchAgents = useAgentsStore((s) => s.fetchAgents);
  const scope = useDataScope();
  const visibleAgents = useVisibleAgents();

  // Admins may browse the shared bucket; scoped users start on their first
  // visible AI staff member (the gateway fails closed without an agent).
  // P11 (state-as-URL): the browsed bucket lives in `?agent=<id>` (or the
  // shared bucket sentinel) so a refresh keeps you where you were looking.
  const [selected, setSelected] = useUrlState('agent', '');
  // I-2b: task filter lives in the URL too, so a shared link keeps the view.
  const [taskFilter, setTaskFilter] = useUrlState('task', '');
  // I-4: search text + date range also live in the URL (bookmarkable /
  // shareable, same P11 convention as `agent`/`task`). `q` is committed to the
  // URL on a short debounce so a fetch does not fire on every keystroke; the
  // draft input value is tracked separately so typing itself never stalls.
  const [q, setQ] = useUrlState('q', '');
  const [qDraft, setQDraft] = useState(q);
  const [sinceDate, setSinceDate] = useUrlState('since', '');
  const [untilDate, setUntilDate] = useUrlState('until', '');
  const [files, setFiles] = useState<FileRow[]>([]);
  const [loaded, setLoaded] = useState(false);
  const [error, setError] = useState<unknown>(null);

  useEffect(() => {
    fetchAgents();
  }, [fetchAgents]);

  // Keep the draft in sync when `q` changes externally (back/forward nav, a
  // pasted deep link) without fighting the debounce below.
  useEffect(() => {
    setQDraft(q);
  }, [q]);

  useEffect(() => {
    if (qDraft === q) return;
    const t = setTimeout(() => setQ(qDraft), 300);
    return () => clearTimeout(t);
  }, [qDraft, q, setQ]);

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

  // I-4: `<input type="date">` gives a plain `YYYY-MM-DD`, parsed as LOCAL
  // midnight (no timezone suffix ⇒ local time per the ES2015 Date grammar) so
  // the boundary matches what `dateFmt` shows the user, not UTC midnight.
  const sinceMs = useMemo(() => {
    if (!sinceDate) return undefined;
    const ms = Date.parse(`${sinceDate}T00:00:00`);
    return Number.isNaN(ms) ? undefined : ms;
  }, [sinceDate]);
  const untilMs = useMemo(() => {
    if (!untilDate) return undefined;
    const ms = Date.parse(`${untilDate}T23:59:59.999`);
    return Number.isNaN(ms) ? undefined : ms;
  }, [untilDate]);

  /** The listing endpoint alone gets the I-4 filters — download/preview keep
   *  using plain `buildUrl` (agent + name only). */
  const buildListUrl = useCallback((): string => {
    const params = new URLSearchParams();
    if (effective && effective !== SHARED) params.set('agent', effective);
    if (q.trim()) params.set('q', q.trim());
    if (sinceMs !== undefined) params.set('since', String(sinceMs));
    if (untilMs !== undefined) params.set('until', String(untilMs));
    const qs = params.toString();
    return `/api/files${qs ? `?${qs}` : ''}`;
  }, [effective, q, sinceMs, untilMs]);

  const fetchFiles = useCallback(async () => {
    if (!effective) return; // nothing selectable yet
    if (scope !== 'all' && effective === SHARED) return;
    try {
      const res = await fetch(buildListUrl(), {
        headers: jwt ? { Authorization: `Bearer ${jwt}` } : {},
      });
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const data = await res.json();
      setFiles(Array.isArray(data?.files) ? data.files : []);
      setError(null);
    } catch (e) {
      setError(e);
    } finally {
      setLoaded(true);
    }
  }, [effective, scope, buildListUrl, jwt]);

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

  /** Preview URL — office types go through the LibreOffice→PDF endpoint;
   *  natively-previewable types stream inline from the same endpoint. */
  const previewUrl = useCallback(
    (name: string): string => {
      const url = buildUrl('/api/files/preview', name);
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

  // I-2b: the tasks actually represented in this bucket. Built from the rows
  // themselves, so the filter can never offer a task with nothing behind it.
  const taskOptions = useMemo(() => {
    const ids = new Set<string>();
    for (const f of files) if (f.task_id) ids.add(f.task_id);
    return [...ids].sort();
  }, [files]);

  const visibleFiles = useMemo(() => {
    if (!taskFilter || taskFilter === ALL_TASKS) return files;
    if (taskFilter === NO_TASK) return files.filter((f) => !f.task_id);
    return files.filter((f) => f.task_id === taskFilter);
  }, [files, taskFilter]);

  // I-4: whether any filter (search / date range / task) is narrowing the
  // view — distinguishes "no files exist at all" from "filters matched
  // nothing" so the empty state never blames an empty bucket on a search.
  const filtersActive = Boolean(
    q.trim() || sinceDate || untilDate || (taskFilter && taskFilter !== ALL_TASKS),
  );

  /** Origin label — a file the ledger never saw is honestly 來源不明, not a
   *  guess about who put it there. */
  const originLabel = (f: FileRow) =>
    intl.formatMessage({ id: `files.origin.${f.origin ?? 'unknown'}` });

  return (
    <div className="flex h-full min-h-0 flex-col">
      <PageHeader hideTrigger>
        <FolderOpen className="size-4 shrink-0 text-muted-foreground" />
        <h1 className="truncate text-sm font-medium">
          {intl.formatMessage({ id: 'nav.files' })}
        </h1>
        <span className="font-mono text-xs tabular-nums text-muted-foreground">
          {visibleFiles.length}
        </span>
        <div className="ml-auto flex flex-wrap items-center gap-2">
          <div className="relative min-w-40 flex-1 sm:max-w-56 sm:flex-none">
            <Search className="pointer-events-none absolute top-1/2 left-2.5 size-3.5 -translate-y-1/2 text-muted-foreground" />
            <Input
              type="search"
              value={qDraft}
              onChange={(e) => setQDraft(e.target.value)}
              placeholder={intl.formatMessage({ id: 'files.filter.search' })}
              aria-label={intl.formatMessage({ id: 'files.filter.search' })}
              className="pl-8"
            />
          </div>
          <div className="flex items-center gap-1">
            <Input
              type="date"
              value={sinceDate}
              onChange={(e) => setSinceDate(e.target.value)}
              max={untilDate || undefined}
              aria-label={intl.formatMessage({ id: 'files.filter.dateFrom' })}
              className="w-[8.5rem]"
            />
            <span className="text-xs text-muted-foreground" aria-hidden="true">
              –
            </span>
            <Input
              type="date"
              value={untilDate}
              onChange={(e) => setUntilDate(e.target.value)}
              min={sinceDate || undefined}
              aria-label={intl.formatMessage({ id: 'files.filter.dateTo' })}
              className="w-[8.5rem]"
            />
          </div>
          {taskOptions.length > 0 && (
            <Select
              value={taskFilter || ALL_TASKS}
              onValueChange={(v) => setTaskFilter(String(v) === ALL_TASKS ? '' : String(v))}
            >
              <SelectTrigger size="sm" className="max-w-44">
                <SelectValue
                  aria-label={intl.formatMessage({ id: 'files.filter.task' })}
                  placeholder={intl.formatMessage({ id: 'files.filter.task' })}
                />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value={ALL_TASKS}>
                  {intl.formatMessage({ id: 'files.filter.task.all' })}
                </SelectItem>
                <SelectItem value={NO_TASK}>
                  {intl.formatMessage({ id: 'files.filter.task.none' })}
                </SelectItem>
                {taskOptions.map((id) => (
                  <SelectItem key={id} value={id}>
                    {intl.formatMessage({ id: 'files.origin.task' }, { id: id.slice(0, 8) })}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          )}
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
          <ErrorState
            icon={FolderOpen}
            title={intl.formatMessage({ id: 'files.error' })}
            error={error}
            onRetry={() => void fetchFiles()}
          />
        ) : visibleFiles.length === 0 ? (
          <Empty
            icon={FolderOpen}
            title={intl.formatMessage({
              id: filtersActive ? 'files.empty.filtered' : 'files.empty',
            })}
            description={
              filtersActive ? undefined : intl.formatMessage({ id: 'files.empty.hint' })
            }
          />
        ) : (
          <div className="overflow-x-auto">
            <Table className="min-w-[32rem]">
              <TableHeader>
                <TableRow>
                  <TableHead>{intl.formatMessage({ id: 'files.col.name' })}</TableHead>
                  <TableHead className="w-40">
                    {intl.formatMessage({ id: 'files.col.origin' })}
                  </TableHead>
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
                {visibleFiles.map((f) => (
                  <TableRow key={f.name}>
                    <TableCell
                      className="font-medium break-all text-foreground"
                      title={f.name}
                    >
                      {f.display_name ?? f.name}
                    </TableCell>
                    <TableCell className="text-xs">
                      <div className="flex flex-wrap items-center gap-1">
                        <Badge variant={f.origin && f.origin !== 'unknown' ? 'secondary' : 'outline'}>
                          {originLabel(f)}
                        </Badge>
                        {f.task_id && (
                          <span className="font-mono text-[11px] text-muted-foreground">
                            {intl.formatMessage(
                              { id: 'files.origin.task' },
                              { id: f.task_id.slice(0, 8) },
                            )}
                          </span>
                        )}
                      </div>
                    </TableCell>
                    <TableCell className="text-right font-mono text-xs tabular-nums text-muted-foreground">
                      {formatSize(f.size)}
                    </TableCell>
                    <TableCell className="text-xs text-muted-foreground tabular-nums">
                      {f.mtime ? dateFmt.format(new Date(f.mtime)) : '—'}
                    </TableCell>
                    <TableCell>
                      <div className="flex items-center justify-end gap-1">
                        {(isPreviewable(f.name) || isOfficePreviewable(f.name)) && (
                          <Tooltip>
                            <TooltipTrigger
                              render={
                                <a
                                  href={
                                    isPreviewable(f.name)
                                      ? downloadUrl(f.name)
                                      : previewUrl(f.name)
                                  }
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
                              {intl.formatMessage({
                                id: isPreviewable(f.name)
                                  ? 'files.preview'
                                  : 'files.previewPdf',
                              })}
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
