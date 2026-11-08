import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { AlertTriangle, ArrowRight } from 'lucide-react';
import { api, type ApprovalItem, type OrgPlacement } from '@/lib/api';
import { normPlacement } from '@/lib/org-change';

function asRecord(v: unknown): Record<string, unknown> | null {
  let p: unknown = v;
  if (typeof p === 'string') {
    try {
      p = JSON.parse(p);
    } catch {
      return null;
    }
  }
  return p && typeof p === 'object' && !Array.isArray(p) ? (p as Record<string, unknown>) : null;
}

function placementOf(v: unknown): OrgPlacement {
  const r = asRecord(v);
  return normPlacement({
    reports_to: typeof r?.reports_to === 'string' ? r.reports_to : '',
    department: typeof r?.department === 'string' ? r.department : '',
  });
}

/**
 * The 「組織調整申請」 card body (`org_change` approvals): which employee, the
 * 調整前 / 調整後 manager and department, who asked and why. Whether the
 * placement moved since the request was made comes from `org.pending_changes`;
 * a stale request can only be rejected, so `onStale` lets the card disable 同意.
 */
export function OrgChangeFacts({
  approval,
  agentName,
  onStale,
}: {
  approval: ApprovalItem;
  agentName?: string;
  onStale?: (stale: boolean) => void;
}) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);
  const payload = asRecord(approval.payload);
  const agentId = approval.agent_id || (typeof payload?.agent_id === 'string' ? payload.agent_id : '');
  const [row, setRow] = useState<{ before: OrgPlacement; after: OrgPlacement; stale: boolean; proposer: string; reason: string } | null>(null);

  useEffect(() => {
    let alive = true;
    api.org
      .pendingChanges(agentId || undefined)
      .then((r) => {
        if (!alive) return;
        const hit = (Array.isArray(r?.pending) ? r.pending : []).find((p) => p.approval_id === approval.id);
        if (!hit) return;
        setRow({
          before: normPlacement(hit.before),
          after: normPlacement(hit.after),
          stale: hit.stale === true,
          proposer: hit.proposer_email || hit.proposer,
          reason: hit.reason,
        });
        onStale?.(hit.stale === true);
      })
      .catch(() => undefined);
    return () => {
      alive = false;
    };
    // `onStale` is a state setter from the parent card.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [approval.id, agentId]);

  const before = row?.before ?? placementOf(payload?.before);
  const after = row?.after ?? placementOf(payload?.after);
  const proposer = row?.proposer ?? (typeof payload?.proposer_email === 'string' && payload.proposer_email
    ? payload.proposer_email
    : typeof payload?.proposer === 'string' ? payload.proposer : '');
  const reason = row?.reason ?? (typeof payload?.reason === 'string' ? payload.reason : '');
  const stale = row?.stale === true;
  const mgr = (id: string) => (id || t('orgEdit.topLevel'));
  const dept = (d: string) => (d || t('orgEdit.noDepartment'));

  return (
    <section
      aria-label={t('orgEdit.approval.title')}
      className="space-y-3 rounded-lg border border-border p-3 text-sm"
      data-testid="org-change"
    >
      <p className="font-medium">{t('orgEdit.approval.title')}</p>

      {stale && (
        <p role="alert" className="flex items-start gap-2 rounded-md border border-warning/30 bg-warning/10 px-2 py-1.5 text-xs text-foreground">
          <AlertTriangle className="mt-0.5 size-3.5 shrink-0 text-warning" aria-hidden="true" />
          {t('orgEdit.stale')}
        </p>
      )}

      <dl className="grid grid-cols-[auto_1fr_auto_1fr] items-center gap-x-3 gap-y-1.5 rounded-md bg-muted p-3">
        <dt className="text-xs text-muted-foreground">{t('orgEdit.approval.employee')}</dt>
        <dd className="col-span-3 min-w-0 break-words font-medium">{agentName ?? agentId ?? '—'}</dd>
        <dt />
        <dd className="text-xs text-muted-foreground">{t('orgEdit.confirm.before')}</dd>
        <dd />
        <dd className="text-xs text-muted-foreground">{t('orgEdit.confirm.after')}</dd>
        <dt className="text-xs text-muted-foreground">{t('orgEdit.manager')}</dt>
        <dd className="min-w-0 break-words" data-testid="org-change-manager-before">{mgr(before.reports_to)}</dd>
        <dd aria-label={t('orgEdit.approval.changeTo')}><ArrowRight className="size-3.5 text-muted-foreground" aria-hidden="true" /></dd>
        <dd className="min-w-0 break-words font-medium" data-testid="org-change-manager-after">{mgr(after.reports_to)}</dd>
        <dt className="text-xs text-muted-foreground">{t('orgEdit.department')}</dt>
        <dd className="min-w-0 break-words">{dept(before.department)}</dd>
        <dd aria-label={t('orgEdit.approval.changeTo')}><ArrowRight className="size-3.5 text-muted-foreground" aria-hidden="true" /></dd>
        <dd className="min-w-0 break-words">
          {dept(after.department)}
          {after.department === before.department && (
            <span className="text-muted-foreground">{t('orgEdit.confirm.unchangedSuffix')}</span>
          )}
        </dd>
      </dl>

      <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1">
        <dt className="text-xs text-muted-foreground">{t('orgEdit.approval.requestedBy')}</dt>
        <dd className="min-w-0 break-words">{proposer || '—'}</dd>
        {reason && (
          <>
            <dt className="text-xs text-muted-foreground">{t('orgEdit.approval.reason')}</dt>
            <dd className="min-w-0 whitespace-pre-wrap break-words">{reason}</dd>
          </>
        )}
      </dl>
    </section>
  );
}
