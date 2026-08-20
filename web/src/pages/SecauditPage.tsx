import { useCallback, useEffect, useMemo, useState } from 'react';
import { useIntl } from 'react-intl';
import {
  ShieldCheck,
  CheckCircle2,
  XCircle,
  EyeOff,
  ChevronDown,
  AlertTriangle,
  ArrowLeft,
} from 'lucide-react';

import {
  api,
  type SecauditFinding,
  type SecauditFindingStatus,
  type SecauditReport,
  type SecauditReportRow,
} from '@/lib/api';
import { timeAgo } from '@/lib/format';
import { toast, formatError } from '@/lib/toast';
import { cn } from '@/lib/utils';
import { useUrlStateNullable } from '@/lib/use-url-state';
import { useConnectionStore } from '@/stores/connection-store';
import {
  PageHeader,
  Badge,
  Button,
  Card,
  CardContent,
  Empty,
  ErrorState,
  Skeleton,
  ResizablePanelGroup,
  ResizablePanel,
  ResizableHandle,
  useIsMobile,
} from '@/components/mds';

/**
 * SecauditPage (`/manage/secaudit`) — 安全審計 dashboard
 * (DESIGN-code-security-audit-2026-08 §3.1 "Dashboard").
 *
 * Reads reports `duduclaw secaudit --save` wrote to
 * `<home>/secaudit/reports/*.json` via `secaudit.reports` / `secaudit.report`,
 * and lets a manager+ operator record the human-review verdict on a finding
 * (confirm / suppress / refute) via `secaudit.finding_status`. Master-detail
 * layout mirrors `/runs`: report list on the left (`?report=<file>` is the
 * shareable selection), the selected report's findings — grouped by
 * severity, each expandable into its evidence chain — on the right.
 *
 * The three review actions update optimistically (the clicked finding's
 * status flips immediately) and roll back to the pre-click report on
 * failure, per the design's "樂觀更新＋失敗回滾" requirement.
 */

const SEVERITY_ORDER = ['critical', 'high', 'medium', 'low', 'info'] as const;

function severityTone(sev: string): string {
  switch (sev) {
    case 'critical':
      return 'text-rose-600 dark:text-rose-400';
    case 'high':
      return 'text-amber-600 dark:text-amber-400';
    case 'medium':
      return 'text-chart-1';
    case 'low':
      return 'text-muted-foreground';
    default:
      return 'text-muted-foreground/70';
  }
}

function severityDot(sev: string): string {
  switch (sev) {
    case 'critical':
      return 'bg-rose-500';
    case 'high':
      return 'bg-amber-500';
    case 'medium':
      return 'bg-chart-1';
    case 'low':
      return 'bg-muted-foreground';
    default:
      return 'bg-muted-foreground/50';
  }
}

function statusTone(status: string): string {
  switch (status) {
    case 'confirmed':
      return 'text-rose-600 dark:text-rose-400';
    case 'refuted':
      return 'text-emerald-600 dark:text-emerald-400';
    case 'needs_human':
      return 'text-amber-600 dark:text-amber-400';
    default:
      return 'text-muted-foreground';
  }
}

/** Pure helper — a new report with one finding's status replaced. Used for
 *  both the optimistic update and reconciling the server's response. */
function withFindingStatus(report: SecauditReport, findingId: string, status: string): SecauditReport {
  return {
    ...report,
    findings: report.findings.map((f) => (f.id === findingId ? { ...f, status } : f)),
  };
}

function SeverityCountDots({
  counts,
}: {
  counts: { critical: number; high: number; medium: number; low: number; info: number } | null;
}) {
  if (!counts) return null;
  const parts = SEVERITY_ORDER.map((s) => ({ s, n: counts[s] })).filter((p) => p.n > 0);
  if (parts.length === 0) return null;
  return (
    <span className="flex flex-wrap items-center gap-1.5">
      {parts.map((p) => (
        <span key={p.s} className="flex items-center gap-1">
          <span className={cn('size-1.5 rounded-full', severityDot(p.s))} aria-hidden />
          <span className={cn('tabular-nums', severityTone(p.s))}>{p.n}</span>
        </span>
      ))}
    </span>
  );
}

function ReportListRowView({
  row,
  selected,
  onSelect,
}: {
  row: SecauditReportRow;
  selected: boolean;
  onSelect: () => void;
}) {
  const intl = useIntl();
  return (
    <li>
      <button
        type="button"
        onClick={onSelect}
        aria-current={selected ? 'true' : undefined}
        className={cn(
          'flex w-full flex-col gap-1 px-4 py-3 text-left outline-none transition-colors focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring/50',
          selected ? 'bg-accent/30' : 'hover:bg-accent/40',
        )}
      >
        {row.parse_error ? (
          <>
            <span className="flex items-center gap-2 text-sm font-medium text-destructive">
              <AlertTriangle className="size-3.5 shrink-0" aria-hidden />
              <span className="truncate font-mono">{row.file}</span>
            </span>
            <span className="truncate text-xs text-muted-foreground">
              {intl.formatMessage({ id: 'secaudit.list.parseError' }, { error: row.parse_error })}
            </span>
          </>
        ) : (
          <>
            <span className="flex items-center gap-2">
              <span className="truncate text-sm font-medium text-foreground">
                {row.repo || intl.formatMessage({ id: 'secaudit.list.repoUnknown' })}
              </span>
              {row.profile_mode && (
                <Badge variant="secondary" className="shrink-0 text-[10px]">
                  {intl.formatMessage({
                    id: `secaudit.profile.${row.profile_mode}`,
                    defaultMessage: row.profile_mode,
                  })}
                </Badge>
              )}
            </span>
            <span className="flex flex-wrap items-center gap-x-2 gap-y-1 text-xs text-muted-foreground">
              <span className="tabular-nums">{timeAgo(row.started_at ?? row.mtime)}</span>
              {row.total_findings != null && (
                <>
                  <span aria-hidden="true">·</span>
                  {row.total_findings === 0 ? (
                    <span>{intl.formatMessage({ id: 'secaudit.list.noFindings' })}</span>
                  ) : (
                    <SeverityCountDots counts={row.by_severity} />
                  )}
                </>
              )}
              {!!row.engines_missing_count && (
                <>
                  <span aria-hidden="true">·</span>
                  <span>
                    {intl.formatMessage(
                      { id: 'secaudit.list.enginesMissing' },
                      { n: row.engines_missing_count },
                    )}
                  </span>
                </>
              )}
            </span>
          </>
        )}
      </button>
    </li>
  );
}

function FindingCard({
  finding,
  expanded,
  onToggle,
  busy,
  onAction,
}: {
  finding: SecauditFinding;
  expanded: boolean;
  onToggle: () => void;
  busy: boolean;
  onAction: (status: SecauditFindingStatus) => void;
}) {
  const intl = useIntl();
  return (
    <Card data-size="sm">
      <CardContent className="space-y-2">
        <button type="button" onClick={onToggle} aria-expanded={expanded} className="flex w-full items-start gap-2 text-left outline-none">
          <span className={cn('mt-1.5 size-2 shrink-0 rounded-full', severityDot(finding.severity))} aria-hidden />
          <span className="min-w-0 flex-1">
            <span className="flex flex-wrap items-center gap-2">
              <span className="truncate text-sm font-medium text-foreground">{finding.title}</span>
              <Badge variant="secondary" className="shrink-0 text-[10px]">
                {finding.source_engine}
              </Badge>
              <span className={cn('shrink-0 text-xs font-medium', statusTone(finding.status))}>
                {intl.formatMessage({ id: `secaudit.status.${finding.status}`, defaultMessage: finding.status })}
              </span>
            </span>
            <span className="mt-0.5 block truncate font-mono text-xs text-muted-foreground">
              {finding.file}
              {finding.line != null ? `:${finding.line}` : ''}
            </span>
          </span>
          <ChevronDown
            className={cn(
              'mt-1 size-4 shrink-0 text-muted-foreground transition-transform',
              expanded && 'rotate-180',
            )}
            aria-hidden
          />
        </button>

        {expanded && (
          <div className="space-y-3 border-t border-surface-border pt-3">
            <p className="text-xs text-muted-foreground">
              {intl.formatMessage({ id: 'secaudit.finding.rule' }, { rule: finding.rule_id })}
            </p>
            {finding.snippet && (
              <pre className="overflow-x-auto rounded-md bg-muted px-3 py-2 font-mono text-xs whitespace-pre-wrap break-words text-foreground">
                {finding.snippet}
              </pre>
            )}
            <div className="space-y-1.5">
              <p className="text-xs font-medium text-muted-foreground">
                {intl.formatMessage({ id: 'secaudit.finding.evidence.title' })}
              </p>
              {finding.evidence.length === 0 ? (
                <p className="text-xs text-muted-foreground/70">
                  {intl.formatMessage({ id: 'secaudit.finding.evidence.empty' })}
                </p>
              ) : (
                <ul className="space-y-1.5">
                  {finding.evidence.map((e, i) => (
                    <li key={i} className="rounded-md border border-surface-border p-2 text-xs">
                      <div className="flex items-center justify-between gap-2">
                        <span className="font-medium text-foreground">
                          {intl.formatMessage({
                            id: `secaudit.evidence.kind.${e.kind}`,
                            defaultMessage: e.kind,
                          })}
                        </span>
                        <span className="shrink-0 tabular-nums text-muted-foreground">{timeAgo(e.recorded_at)}</span>
                      </div>
                      <p className="mt-0.5 text-muted-foreground">{e.source}</p>
                      <pre className="mt-1 overflow-x-auto whitespace-pre-wrap break-words font-mono text-[11px] text-foreground">
                        {e.detail}
                      </pre>
                    </li>
                  ))}
                </ul>
              )}
            </div>

            <div className="flex flex-wrap gap-1.5 pt-1">
              <Button variant="outline" size="sm" disabled={busy} onClick={() => onAction('confirmed')}>
                <CheckCircle2 className="size-3.5" />
                {intl.formatMessage({ id: 'secaudit.action.confirm' })}
              </Button>
              <Button variant="outline" size="sm" disabled={busy} onClick={() => onAction('suppressed')}>
                <EyeOff className="size-3.5" />
                {intl.formatMessage({ id: 'secaudit.action.suppress' })}
              </Button>
              <Button variant="outline" size="sm" disabled={busy} onClick={() => onAction('refuted')}>
                <XCircle className="size-3.5" />
                {intl.formatMessage({ id: 'secaudit.action.refute' })}
              </Button>
            </div>
          </div>
        )}
      </CardContent>
    </Card>
  );
}

export function SecauditPage() {
  const intl = useIntl();
  const isMobile = useIsMobile();
  const connState = useConnectionStore((s) => s.state);

  const [rows, setRows] = useState<SecauditReportRow[]>([]);
  const [listLoaded, setListLoaded] = useState(false);
  const [listError, setListError] = useState<unknown>(null);

  // `?report=<file>` — same "state lives in the URL" convention as `/runs`'s
  // `?run=<id>`, so a bookmarked/shared link opens the same report.
  const [selectedFile, setSelectedFile] = useUrlStateNullable('report');
  const [report, setReport] = useState<SecauditReport | null>(null);
  const [detailLoading, setDetailLoading] = useState(false);
  const [detailError, setDetailError] = useState<unknown>(null);

  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [pendingFindingId, setPendingFindingId] = useState<string | null>(null);

  const fetchReports = useCallback(async () => {
    try {
      const res = await api.secaudit.reports();
      setRows(res.reports);
      setListError(null);
    } catch (e) {
      setListError(e);
    } finally {
      setListLoaded(true);
    }
  }, []);

  useEffect(() => {
    if (connState !== 'authenticated') return;
    void fetchReports();
  }, [connState, fetchReports]);

  const fetchDetail = useCallback(async (file: string) => {
    setDetailLoading(true);
    setDetailError(null);
    try {
      const res = await api.secaudit.report(file);
      setReport(res.report);
    } catch (e) {
      setReport(null);
      setDetailError(e);
    } finally {
      setDetailLoading(false);
    }
  }, []);

  useEffect(() => {
    if (!selectedFile || connState !== 'authenticated') return;
    setExpanded(new Set());
    void fetchDetail(selectedFile);
  }, [selectedFile, connState, fetchDetail]);

  const toggleExpanded = (id: string) => {
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(id)) {
        next.delete(id);
      } else {
        next.add(id);
      }
      return next;
    });
  };

  const handleAction = async (findingId: string, status: SecauditFindingStatus) => {
    if (!report || !selectedFile) return;
    const prevReport = report;
    setPendingFindingId(findingId);
    // Optimistic update — the finding flips to the new status immediately.
    setReport(withFindingStatus(report, findingId, status));
    try {
      const res = await api.secaudit.findingStatus(selectedFile, findingId, status);
      // Reconcile with the server's own copy of the finding.
      setReport((cur) => (cur ? withFindingStatus(cur, findingId, res.finding.status) : cur));
      toast.success(intl.formatMessage({ id: 'secaudit.action.success' }));
    } catch (e) {
      // Roll back to the pre-click report.
      setReport(prevReport);
      toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
    } finally {
      setPendingFindingId(null);
    }
  };

  const findingsBySeverity = useMemo(() => {
    if (!report) return [];
    return SEVERITY_ORDER.map((severity) => ({
      severity,
      findings: report.findings.filter((f) => f.severity === severity),
    })).filter((g) => g.findings.length > 0);
  }, [report]);

  // ── Left column: header + report list ─────────────────────────
  const listColumn = (
    <div className="flex h-full min-h-0 flex-col">
      <PageHeader hideTrigger>
        <ShieldCheck className="size-4 shrink-0 text-muted-foreground" aria-hidden />
        <h1 className="truncate text-sm font-medium">{intl.formatMessage({ id: 'secaudit.title' })}</h1>
        <span className="font-mono text-xs tabular-nums text-muted-foreground">{rows.length}</span>
      </PageHeader>

      <div className="min-h-0 flex-1 overflow-y-auto">
        {!listLoaded ? (
          <div className="space-y-3 p-4">
            <Skeleton className="h-14 w-full" />
            <Skeleton className="h-14 w-full" />
            <Skeleton className="h-14 w-2/3" />
          </div>
        ) : listError ? (
          <div className="p-4">
            <ErrorState icon={ShieldCheck} error={listError} onRetry={() => void fetchReports()} />
          </div>
        ) : rows.length === 0 ? (
          <div className="p-4">
            <Empty
              icon={ShieldCheck}
              title={intl.formatMessage({ id: 'secaudit.list.empty.title' })}
              description={intl.formatMessage({ id: 'secaudit.list.empty.desc' })}
            />
          </div>
        ) : (
          <ul className="py-1" aria-label={intl.formatMessage({ id: 'secaudit.list.aria' })}>
            {rows.map((r) => (
              <ReportListRowView
                key={r.file}
                row={r}
                selected={r.file === selectedFile}
                onSelect={() => setSelectedFile(r.file)}
              />
            ))}
          </ul>
        )}
      </div>
    </div>
  );

  // ── Right column: selected report's findings board ─────────────
  const detailColumn = (
    <div className="flex h-full min-h-0 flex-col">
      {isMobile && selectedFile && (
        <div className="flex h-12 shrink-0 items-center gap-2 border-b border-surface-border px-2">
          <button
            type="button"
            onClick={() => setSelectedFile(null)}
            aria-label={intl.formatMessage({ id: 'common.back' })}
            className="grid size-7 place-items-center rounded-md text-muted-foreground outline-none hover:bg-muted focus-visible:ring-2 focus-visible:ring-ring/50"
          >
            <ArrowLeft className="size-4" />
          </button>
          {report && <span className="truncate text-sm font-medium">{report.repo}</span>}
        </div>
      )}

      {!selectedFile ? (
        <div className="flex h-full flex-col items-center justify-center gap-3 p-6 text-center">
          <ShieldCheck className="size-10 text-muted-foreground/30" aria-hidden />
          <div>
            <p className="text-sm font-medium text-foreground">
              {intl.formatMessage({ id: 'secaudit.detail.select' })}
            </p>
            <p className="mt-1 text-sm text-muted-foreground">
              {intl.formatMessage({ id: 'secaudit.detail.select.hint' })}
            </p>
          </div>
        </div>
      ) : detailLoading ? (
        <div className="space-y-3 p-5">
          <Skeleton className="h-8 w-1/2" />
          <Skeleton className="h-20 w-full" />
          <Skeleton className="h-12 w-full" />
        </div>
      ) : detailError ? (
        <div className="p-6">
          <ErrorState error={detailError} onRetry={() => void fetchDetail(selectedFile)} />
        </div>
      ) : report ? (
        <>
          <div className="space-y-2 border-b border-surface-border px-5 py-4">
            <div className="flex flex-wrap items-center gap-2">
              <p className="truncate text-sm font-medium text-foreground">{report.repo}</p>
              <Badge variant="secondary" className="text-[10px]">
                {intl.formatMessage({
                  id: `secaudit.profile.${report.profile.mode}`,
                  defaultMessage: report.profile.mode,
                })}
              </Badge>
            </div>
            <p className="text-xs tabular-nums text-muted-foreground">
              {intl.formatDate(report.started_at, { month: 'numeric', day: 'numeric' })}{' '}
              {intl.formatTime(report.started_at, { hour: '2-digit', minute: '2-digit' })}
            </p>
            {report.engines_run.length === 0 && report.engines_missing.length === 0 ? (
              <p className="text-xs text-muted-foreground">
                {intl.formatMessage({ id: 'secaudit.detail.engines.none' })}
              </p>
            ) : (
              <div className="flex flex-wrap gap-x-4 gap-y-1 text-xs text-muted-foreground">
                {report.engines_run.length > 0 && (
                  <span>
                    {intl.formatMessage(
                      { id: 'secaudit.detail.engines.run' },
                      { list: report.engines_run.map((e) => e.engine).join('、') },
                    )}
                  </span>
                )}
                {report.engines_missing.length > 0 && (
                  <span>
                    {intl.formatMessage(
                      { id: 'secaudit.detail.engines.missing' },
                      { list: report.engines_missing.map((e) => e.engine).join('、') },
                    )}
                  </span>
                )}
              </div>
            )}
          </div>

          <div className="min-h-0 flex-1 space-y-4 overflow-y-auto px-5 py-4">
            {report.findings.length === 0 ? (
              <Empty
                icon={CheckCircle2}
                title={intl.formatMessage({ id: 'secaudit.detail.findings.empty' })}
              />
            ) : (
              findingsBySeverity.map((group) => (
                <section key={group.severity} className="space-y-2">
                  <h2 className={cn('flex items-center gap-2 text-sm font-medium', severityTone(group.severity))}>
                    <span className={cn('size-2 rounded-full', severityDot(group.severity))} aria-hidden />
                    {intl.formatMessage({ id: `secaudit.severity.${group.severity}` })}
                    <span className="font-mono text-xs tabular-nums text-muted-foreground">
                      {group.findings.length}
                    </span>
                  </h2>
                  <div className="space-y-2">
                    {group.findings.map((f) => (
                      <FindingCard
                        key={f.id}
                        finding={f}
                        expanded={expanded.has(f.id)}
                        onToggle={() => toggleExpanded(f.id)}
                        busy={pendingFindingId === f.id}
                        onAction={(status) => void handleAction(f.id, status)}
                      />
                    ))}
                  </div>
                </section>
              ))
            )}
          </div>
        </>
      ) : null}
    </div>
  );

  return (
    <div className="-mx-4 -mt-4 flex min-h-0 flex-1 md:-mx-6 md:-mt-6 md:-mb-6">
      {isMobile ? (
        selectedFile ? (
          detailColumn
        ) : (
          <div className="w-full">{listColumn}</div>
        )
      ) : (
        <ResizablePanelGroup orientation="horizontal" id="secaudit-split" className="h-full w-full">
          <ResizablePanel defaultSize={340} minSize={260} maxSize={480} className="border-r border-surface-border">
            {listColumn}
          </ResizablePanel>
          <ResizableHandle />
          <ResizablePanel minSize="40">{detailColumn}</ResizablePanel>
        </ResizablePanelGroup>
      )}
    </div>
  );
}
