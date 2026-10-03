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
  type SecauditSeverityCounts,
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

const THREAT_FIELDS = ['principal', 'input', 'control', 'boundary', 'affected', 'result'] as const;

function DetailBlock({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div className="space-y-1.5">
      <p className="text-xs font-medium text-muted-foreground">{title}</p>
      {children}
    </div>
  );
}

/** v2-only detail blocks. Every block renders nothing when its field is
 *  absent/empty, so a v1 finding expands exactly as before. */
function FindingV2Detail({ finding }: { finding: SecauditFinding }) {
  const intl = useIntl();
  const tm = finding.threat_model;
  const trace = finding.trace ?? [];
  const conditions = finding.conditions ?? [];
  const blockers = finding.blockers ?? [];
  const plan = finding.validation_plan;
  const precheck = finding.precheck;
  const failedPrecheck = precheck && !precheck.passed ? precheck : null;
  const t = (id: string, defaultMessage?: string) => intl.formatMessage({ id, defaultMessage });
  return (
    <>
      {tm && (
        <DetailBlock title={t('secaudit.finding.threat.title')}>
          <dl className="grid grid-cols-1 gap-x-3 gap-y-1 text-xs sm:grid-cols-[auto_1fr]">
            {THREAT_FIELDS.map((k) => (
              <div key={k} className="contents">
                <dt className="text-muted-foreground">{t(`secaudit.finding.threat.${k}`)}</dt>
                <dd className="text-foreground">{tm[k]}</dd>
              </div>
            ))}
          </dl>
        </DetailBlock>
      )}
      {trace.length > 0 && (
        <DetailBlock title={t('secaudit.finding.trace.title')}>
          <ol className="space-y-1.5">
            {trace.map((st, i) => (
              <li key={i} className="rounded-md border border-surface-border p-2 text-xs">
                <div className="flex flex-wrap items-center gap-2">
                  <Badge variant="secondary" className="text-[10px]">
                    {t(`secaudit.finding.trace.kind.${st.kind}`, st.kind)}
                  </Badge>
                  <span className="font-mono text-muted-foreground">
                    {st.file}:{st.line}
                  </span>
                  {st.scope && <span className="font-mono text-foreground">{st.scope}</span>}
                </div>
                <p className="mt-1 text-muted-foreground">{st.description}</p>
              </li>
            ))}
          </ol>
        </DetailBlock>
      )}
      {conditions.length > 0 && (
        <DetailBlock title={t('secaudit.finding.conditions.title')}>
          <ul className="space-y-1 text-xs">
            {conditions.map((c, i) => (
              <li key={i} className="flex flex-wrap items-start gap-2">
                <Badge variant="secondary" className="text-[10px]">
                  {t(`secaudit.finding.condition.kind.${c.kind}`, c.kind)}
                </Badge>
                <span className="min-w-0 flex-1 text-foreground">{c.description}</span>
              </li>
            ))}
          </ul>
        </DetailBlock>
      )}
      {blockers.length > 0 && (
        <DetailBlock title={t('secaudit.finding.blockers.title')}>
          <ul className="list-disc space-y-1 pl-4 text-xs text-foreground">
            {blockers.map((b, i) => (
              <li key={i}>{b}</li>
            ))}
          </ul>
        </DetailBlock>
      )}
      {plan && (plan.local || plan.deployment) && (
        <DetailBlock title={t('secaudit.finding.plan.title')}>
          <dl className="space-y-1 text-xs">
            {plan.local && (
              <div>
                <dt className="text-muted-foreground">{t('secaudit.finding.plan.local')}</dt>
                <dd className="text-foreground">{plan.local}</dd>
              </div>
            )}
            {plan.deployment && (
              <div>
                <dt className="text-muted-foreground">{t('secaudit.finding.plan.deployment')}</dt>
                <dd className="text-foreground">{plan.deployment}</dd>
              </div>
            )}
          </dl>
        </DetailBlock>
      )}
      {failedPrecheck && (
        <DetailBlock title={t('secaudit.finding.precheck.title')}>
          <ul className="list-disc space-y-1 pl-4 text-xs text-destructive">
            {failedPrecheck.violations.map((v, i) => (
              <li key={i}>{v}</li>
            ))}
          </ul>
        </DetailBlock>
      )}
    </>
  );
}

const INCOMPLETE_REASONS = [
  'budget_cannot_fund_reserves',
  'validation_budget_exhausted',
  'critic_budget_exhausted',
  'engine_unavailable',
  'interrupted',
];

/** Report-level v2 header blocks (banner, coverage, verifier, prior run,
 *  needs-human stats). Renders nothing for a v1 report. */
function ReportV2Header({ report }: { report: SecauditReport }) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) =>
    intl.formatMessage({ id }, values);
  const cov = report.summary.coverage;
  const verifier = report.verifier;
  const prior = report.prior_run;
  const nh = report.summary.needs_human_by_severity;
  const gated = report.summary.gate_includes_needs_human === true;
  const incomplete = report.run_status === 'incomplete';
  const showStats = !!nh || report.summary.gate_includes_needs_human !== undefined;

  let verifierText: string | null = null;
  if (verifier) {
    const known = ['same_agent', 'different_agent', 'not_run'].includes(verifier.independence);
    const label = t(`secaudit.verifier.${known ? verifier.independence : 'unknown'}`);
    verifierText =
      verifier.audit_agent != null || verifier.verifier_agent != null
        ? t('secaudit.verifier.agents', {
            label,
            audit: verifier.audit_agent ?? t('secaudit.verifier.defaultAgent'),
            verifier: verifier.verifier_agent ?? t('secaudit.verifier.defaultAgent'),
          })
        : label;
  }

  if (!incomplete && !cov && !verifier && !prior && !showStats) return null;

  const reasonKey = INCOMPLETE_REASONS.includes(report.incomplete_reason ?? '')
    ? (report.incomplete_reason as string)
    : 'unknown';

  return (
    <div className="space-y-2 pt-1">
      {incomplete && (
        <div role="alert" className="flex items-start gap-2 rounded-md border border-amber-500/40 bg-amber-500/10 px-3 py-2 text-xs">
          <AlertTriangle className="mt-0.5 size-3.5 shrink-0 text-amber-600 dark:text-amber-400" aria-hidden />
          <div>
            <p className="font-medium text-foreground">{t('secaudit.incomplete.title')}</p>
            <p className="text-muted-foreground">{t(`secaudit.incomplete.reason.${reasonKey}`)}</p>
          </div>
        </div>
      )}
      {(verifierText || prior) && (
        <div className="flex flex-wrap gap-1.5">
          {verifierText && (
            <Badge variant="secondary" className="text-[10px]">
              {verifierText}
            </Badge>
          )}
          {prior && (
            <Badge variant="secondary" className="whitespace-normal text-[10px]">
              {t('secaudit.prior.chip', {
                suppressed: prior.carried_suppressed,
                refuted: prior.carried_refuted,
                revalidated: prior.revalidated_prior_confirmed,
                changed: prior.changed_source,
              })}
              {!!report.summary.carried_from_prior &&
                `；${t('secaudit.prior.total', { n: report.summary.carried_from_prior })}`}
            </Badge>
          )}
        </div>
      )}
      {cov && (
        <Card data-size="sm">
          <CardContent className="space-y-1.5">
            <div className="flex flex-wrap items-center gap-2">
              <p className="text-xs font-medium text-foreground">{t('secaudit.coverage.title')}</p>
              {cov.partial && (
                <Badge variant="secondary" className="text-[10px]">
                  {t('secaudit.coverage.partial')}
                </Badge>
              )}
            </div>
            <dl className="flex flex-wrap gap-x-5 gap-y-1 text-xs">
              {(['modules_total', 'covered', 'candidate', 'deferred', 'failed'] as const).map((k) => (
                <div key={k} className="flex items-baseline gap-1.5">
                  <dt className="text-muted-foreground">
                    {t(`secaudit.coverage.${k === 'modules_total' ? 'total' : k}`)}
                  </dt>
                  <dd className="tabular-nums text-foreground">{cov[k]}</dd>
                </div>
              ))}
              {!!report.summary.precheck_refuted && (
                <div className="flex items-baseline gap-1.5">
                  <dt className="text-muted-foreground">{t('secaudit.coverage.precheckRefuted')}</dt>
                  <dd className="tabular-nums text-foreground">{report.summary.precheck_refuted}</dd>
                </div>
              )}
            </dl>
          </CardContent>
        </Card>
      )}
      {showStats && (
        <div className="space-y-1 text-xs text-muted-foreground">
          <StatsRow label={t('secaudit.stats.confirmed')} counts={report.summary.by_severity} />
          {gated ? (
            <p>{t('secaudit.stats.needsHumanGated')}</p>
          ) : (
            nh && <StatsRow label={t('secaudit.stats.needsHuman')} counts={nh} />
          )}
        </div>
      )}
    </div>
  );
}

function StatsRow({ label, counts }: { label: string; counts: SecauditSeverityCounts }) {
  const intl = useIntl();
  const empty = SEVERITY_ORDER.every((s) => !counts[s]);
  return (
    <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
      <span>{label}</span>
      {empty ? (
        <span className="text-muted-foreground/70">{intl.formatMessage({ id: 'secaudit.stats.none' })}</span>
      ) : (
        <SeverityCountDots counts={counts} />
      )}
    </div>
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
              {finding.severity_basis === 'model_self_reported' && (
                <span
                  className="shrink-0 text-[10px] text-muted-foreground"
                  title={intl.formatMessage({ id: 'secaudit.finding.basis.title' })}
                >
                  {intl.formatMessage({ id: 'secaudit.finding.basis.model' })}
                </span>
              )}
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
            <FindingV2Detail finding={finding} />
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
            <ReportV2Header report={report} />
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
