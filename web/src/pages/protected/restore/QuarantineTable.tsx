import { Badge, Button, Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/mds';
import { daysUntil, normalizeRootState, type PurgeBlock, type QuarantineRow } from '@/lib/isolation-restore-api';
import { DaysLeft, KindBadge, RootStateBadge, SealBadge, useRestoreT } from './restore-ui';

export type RowAction = 'shared_restore' | 'file_for_owner' | 'purge' | 'early_purge' | 'confirm_early_purge' | 'retire';

/** What an Admin can start from one quarantine row. The server re-checks all of it. */
export function rowActions(row: QuarantineRow): RowAction[] {
  const actions: RowAction[] = [];
  const purge: PurgeBlock = row.purge ?? { purgeable: false };
  const root = normalizeRootState(row.root_state);
  if (purge.purged_at != null) {
    if (!row.retire_proposed && root !== 'pending_release') actions.push('retire');
    return actions;
  }
  if (!row.open_request && root !== 'pending_release') {
    if (row.kind === 'shared') actions.push('shared_restore');
    else if (row.admin_may_file_for_owner) actions.push('file_for_owner');
  }
  if (purge.purgeable) actions.push('purge');
  else if (purge.early_purge && !purge.early_purge.by_me) actions.push('confirm_early_purge');
  else if (purge.early && !purge.early_purge) actions.push('early_purge');
  return actions;
}

export const rowKey = (row: { employee_id: string; incarnation: number }) => `${row.employee_id}:${row.incarnation}`;

/** 已註銷的員工 (quarantine) — name, kind, seal, runtime state, days left, actions. */
export function QuarantineTable({
  rows,
  onAction,
}: {
  rows: QuarantineRow[];
  onAction: (row: QuarantineRow, action: RowAction) => void;
}) {
  const t = useRestoreT();
  if (rows.length === 0) {
    return <p className="rounded-lg border border-dashed px-4 py-8 text-center text-sm text-muted-foreground">{t('restoreFlow.quarantine.empty')}</p>;
  }
  return (
    <Table aria-label={t('restoreFlow.quarantine.title')}>
      <TableHeader>
        <TableRow>
          <TableHead>{t('restoreFlow.quarantine.col.name')}</TableHead>
          <TableHead>{t('restoreFlow.quarantine.col.kind')}</TableHead>
          <TableHead>{t('restoreFlow.quarantine.col.seal')}</TableHead>
          <TableHead>{t('restoreFlow.quarantine.col.root')}</TableHead>
          <TableHead>{t('restoreFlow.quarantine.col.days')}</TableHead>
          <TableHead>{t('restoreFlow.quarantine.col.actions')}</TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {rows.map((row) => {
          const name = row.display_name || row.label;
          const purged = row.purge?.purged_at != null;
          const actions = rowActions(row);
          return (
            <TableRow key={rowKey(row)}>
              <TableCell className="max-w-56 align-top">
                <span className="block truncate font-medium" title={name}>{name}</span>
                <span className="block truncate font-mono text-xs text-muted-foreground" title={row.label}>{row.label}</span>
                {row.kind === 'personal' && (row.owner_name || row.owner_user_id) && (
                  <span className="block truncate text-xs text-muted-foreground">
                    {t('restoreFlow.quarantine.owner', { owner: row.owner_name || row.owner_user_id || '' })}
                  </span>
                )}
              </TableCell>
              <TableCell className="align-top"><KindBadge kind={row.kind} /></TableCell>
              <TableCell className="align-top"><SealBadge seal={row.seal} /></TableCell>
              <TableCell className="align-top"><RootStateBadge state={row.root_state} /></TableCell>
              <TableCell className="align-top">
                {purged ? <Badge variant="outline">{t('restoreFlow.quarantine.purged')}</Badge> : <DaysLeft days={daysUntil(row.purge?.purge_after)} />}
              </TableCell>
              <TableCell className="align-top">
                <div className="flex min-w-48 flex-col items-start gap-1.5">
                  {row.open_request && (
                    <Badge variant="outline">
                      {t(row.open_request.kind === 'adoption' ? 'restoreFlow.quarantine.openAdoption' : 'restoreFlow.quarantine.openRestore')}
                    </Badge>
                  )}
                  {row.open_request?.owner_unverified && <Badge variant="destructive">{t('restoreFlow.ownerUnverified')}</Badge>}
                  {row.purge?.early_purge && (
                    <Badge variant="outline">
                      {t(row.purge.early_purge.by_me ? 'restoreFlow.quarantine.earlyPurgeMine' : 'restoreFlow.quarantine.earlyPurgeWaiting')}
                    </Badge>
                  )}
                  {row.retire_proposed && <Badge variant="outline">{t('restoreFlow.quarantine.retireProposed')}</Badge>}
                  <div className="flex flex-wrap gap-1.5">
                    {actions.map((action) => {
                      const adopt = (action === 'shared_restore' || action === 'file_for_owner') && normalizeRootState(row.root_state) === 'root_gone';
                      const label = t(`restoreFlow.action.${action}${adopt ? 'Adopt' : ''}`);
                      return (
                        <Button
                          key={action}
                          size="sm"
                          variant={action === 'purge' || action === 'early_purge' || action === 'confirm_early_purge' ? 'destructive' : 'outline'}
                          onClick={() => onAction(row, action)}
                          aria-label={t('restoreFlow.action.forRow', { action: label, name })}
                        >
                          {label}
                        </Button>
                      );
                    })}
                  </div>
                </div>
              </TableCell>
            </TableRow>
          );
        })}
      </TableBody>
    </Table>
  );
}
