import { useIntl } from 'react-intl';
import { Badge } from '@/components/mds';
import { timeAgo } from '@/lib/format';
import { normPlacement, type OrgChangeRow, type OrgChangeRowStatus } from '@/lib/org-change';

const STATUS_KEY: Record<OrgChangeRowStatus, string> = {
  pending: 'orgEdit.status.pending',
  approved: 'orgEdit.status.approved',
  needs_resubmit: 'orgEdit.status.needsResubmit',
  rejected: 'orgEdit.status.rejected',
};

const STATUS_VARIANT: Record<OrgChangeRowStatus, 'default' | 'secondary' | 'outline' | 'destructive'> = {
  pending: 'default',
  approved: 'secondary',
  needs_resubmit: 'outline',
  rejected: 'destructive',
};

/**
 * 「待核准的調整」 — the org changes a Manager filed (an Admin sees everyone's).
 * `pending` / `needs_resubmit` come from `org.pending_changes`; 已同意 / 已拒絕
 * are rows that left that list during this session.
 */
export function OrgPendingTable({
  rows,
  labelOf,
  currentUserId,
}: {
  rows: ReadonlyArray<OrgChangeRow>;
  labelOf: (agentId: string) => string;
  currentUserId?: string;
}) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);
  const dept = (d: string) => d || t('orgEdit.noDepartment');

  const describe = ({ change }: OrgChangeRow) => {
    const before = normPlacement(change.before);
    const after = normPlacement(change.after);
    const parts: string[] = [];
    if (before.reports_to !== after.reports_to) {
      parts.push(
        t('orgEdit.table.manager', {
          from: before.reports_to ? labelOf(before.reports_to) : t('orgEdit.topLevel'),
          to: after.reports_to ? labelOf(after.reports_to) : t('orgEdit.topLevel'),
        }),
      );
    }
    if (before.department !== after.department) {
      parts.push(t('orgEdit.table.department', { from: dept(before.department), to: dept(after.department) }));
    }
    return `${labelOf(change.agent_id)}：${parts.join('；')}`;
  };

  return (
    <section className="overflow-hidden rounded-xl border border-surface-border" aria-labelledby="org-pending-title">
      <div className="flex items-center justify-between px-4 py-3">
        <h2 id="org-pending-title" className="text-sm font-medium">{t('orgEdit.table.title')}</h2>
      </div>
      <table className="w-full text-left text-sm">
        <thead>
          <tr className="border-t border-surface-border text-xs text-muted-foreground">
            <th scope="col" className="px-4 py-2 font-medium">{t('orgEdit.table.proposer')}</th>
            <th scope="col" className="px-4 py-2 font-medium">{t('orgEdit.table.content')}</th>
            <th scope="col" className="px-4 py-2 font-medium">{t('orgEdit.table.time')}</th>
            <th scope="col" className="px-4 py-2 font-medium">{t('orgEdit.table.status')}</th>
          </tr>
        </thead>
        <tbody>
          {rows.map((row) => {
            const { change } = row;
            const mine = !!currentUserId && change.proposer === currentUserId;
            const who = change.proposer_email || change.proposer || '—';
            return (
              <tr key={change.approval_id} className="border-t border-surface-border align-top">
                <td className="px-4 py-3">{mine ? t('orgEdit.table.you', { name: who }) : who}</td>
                <td className="px-4 py-3">{describe(row)}</td>
                <td className="px-4 py-3 tabular-nums text-muted-foreground">{timeAgo(change.created_at)}</td>
                <td className="px-4 py-3">
                  <Badge
                    variant={STATUS_VARIANT[row.status]}
                    title={row.status === 'needs_resubmit' ? t('orgEdit.stale') : undefined}
                  >
                    {t(STATUS_KEY[row.status])}
                  </Badge>
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </section>
  );
}
