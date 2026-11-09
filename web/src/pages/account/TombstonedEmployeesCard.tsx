import { useCallback, useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { Button, Card, CardContent, CardDescription, CardHeader, CardTitle, Skeleton } from '@/components/mds';
import {
  daysUntil,
  myIsolationApi,
  rootStateOf,
  toMillis,
  type RestorableEmployee,
} from '@/lib/isolation-restore-api';
import { AdoptFlow } from '@/pages/protected/restore/AdoptFlow';
import { RestoreErrorNotice, RootStateBadge, useRestoreT } from '@/pages/protected/restore/restore-ui';
import { RestoreRequestForm } from './RestoreRequestForm';

/**
 * 已註銷的員工 — the caller's own tombstoned Personal employees still in the
 * restore window (`me.isolation.list_restorable`): ask for a restore (or,
 * when the runtime is gone, a take-over by a new employee), or withdraw a
 * pending request. The runtime state comes from `root_flag`.
 */
export function TombstonedEmployeesCard() {
  const t = useRestoreT();
  const intl = useIntl();
  const [items, setItems] = useState<RestorableEmployee[] | null>(null);
  const [loadError, setLoadError] = useState<unknown>(null);
  const [open, setOpen] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [rowError, setRowError] = useState<{ key: string; error: unknown } | null>(null);

  const load = useCallback(async () => {
    try {
      const res = await myIsolationApi.listRestorable();
      setItems(Array.isArray(res?.restorable) ? res.restorable : []);
      setLoadError(null);
    } catch (e) {
      setLoadError(e);
    }
  }, []);

  useEffect(() => { void load(); }, [load]);

  const cancel = async (e: RestorableEmployee, key: string) => {
    setBusy(key);
    setRowError(null);
    try {
      await myIsolationApi.cancelRestore(e.employee_id, e.incarnation);
      await load();
    } catch (error) {
      setRowError({ key, error });
    } finally {
      setBusy(null);
    }
  };

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('myPersonal.tombstoned.title')}</CardTitle>
        <CardDescription>{t('myPersonal.tombstoned.desc')}</CardDescription>
      </CardHeader>
      <CardContent className="space-y-3">
        {loadError != null && (
          <div className="space-y-2">
            <RestoreErrorNotice error={loadError} />
            <Button variant="outline" size="sm" onClick={() => void load()}>{t('restoreFlow.common.retry')}</Button>
          </div>
        )}
        {items === null && loadError == null && <Skeleton className="h-16 w-full" />}
        {items !== null && items.length === 0 && (
          <p className="text-sm text-muted-foreground">{t('myPersonal.tombstoned.empty')}</p>
        )}
        <ul className="divide-y divide-surface-border">
          {items?.map((e) => {
            const key = `${e.employee_id}:${e.incarnation}`;
            const days = daysUntil(e.restore_deadline);
            const root = rootStateOf(e);
            const gone = root === 'root_gone';
            const pending = e.open_request != null;
            const expanded = open === key;
            const regionId = `restore-region-${e.employee_id}-${e.incarnation}`;
            const name = e.display_name || e.label;
            // 執行環境無法確認 / window closed: nothing can be asked for now.
            const blocked = days === 0 || (!gone && root !== 'restorable');
            const removedMs = toMillis(e.tombstoned_at);
            const meta = [
              removedMs != null ? t('myPersonal.tombstoned.removedOn', { date: intl.formatDate(removedMs, { month: 'long', day: 'numeric' }) }) : null,
              days != null ? t(gone ? 'myPersonal.tombstoned.daysLeftAdopt' : 'myPersonal.tombstoned.daysLeft', { days }) : null,
            ].filter((part): part is string => part !== null);
            return (
              <li key={key} className="py-3 first:pt-0">
                <div className="flex flex-wrap items-center justify-between gap-3">
                  <div className="min-w-0 flex-1 space-y-1">
                    <p className="flex flex-wrap items-center gap-2">
                      <span className="truncate text-sm font-medium" title={name}>{name}</span>
                      {root !== 'restorable' && <RootStateBadge state={root} />}
                    </p>
                    {meta.length > 0 && <p className="text-xs text-muted-foreground">{meta.join(' · ')}</p>}
                  </div>
                  <div className="flex flex-wrap gap-2">
                    {pending ? (
                      <Button variant="outline" size="sm" disabled={busy === key} onClick={() => void cancel(e, key)}>
                        {t('myPersonal.tombstoned.cancel')}
                      </Button>
                    ) : (
                      <Button
                        variant={expanded ? 'secondary' : 'brand'}
                        disabled={blocked}
                        aria-expanded={expanded}
                        aria-controls={regionId}
                        onClick={() => setOpen(expanded ? null : key)}
                      >
                        {t(gone ? 'myPersonal.tombstoned.adopt' : 'myPersonal.tombstoned.restore')}
                      </Button>
                    )}
                  </div>
                </div>
                {pending && (
                  <p role="status" className="mt-2 text-sm text-muted-foreground">
                    {t(e.open_request?.kind === 'adoption' ? 'myPersonal.tombstoned.pendingAdoption' : 'myPersonal.tombstoned.pendingRestore')}
                  </p>
                )}
                {root === 'unknown' && !pending && (
                  <p className="mt-2 text-xs text-muted-foreground">{t('myPersonal.tombstoned.unknownRoot')}</p>
                )}
                {rowError?.key === key && <RestoreErrorNotice className="mt-2" error={rowError.error} />}
                {expanded && !pending && !blocked && (
                  <section
                    id={regionId}
                    aria-label={t(gone ? 'myPersonal.tombstoned.adopt' : 'myPersonal.tombstoned.restore')}
                    className="mt-3 rounded-lg bg-muted/40 p-4"
                  >
                    {gone ? (
                      <AdoptFlow employee={e} onFiled={() => void load()} />
                    ) : (
                      <RestoreRequestForm employee={e} onFiled={() => void load()} />
                    )}
                  </section>
                )}
              </li>
            );
          })}
        </ul>
      </CardContent>
    </Card>
  );
}
