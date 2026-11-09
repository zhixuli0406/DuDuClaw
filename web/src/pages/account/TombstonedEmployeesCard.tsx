import { useCallback, useEffect, useState } from 'react';
import { Button, Card, CardContent, CardDescription, CardHeader, CardTitle, Skeleton } from '@/components/mds';
import {
  daysUntil,
  myIsolationApi,
  normalizeRootState,
  type RestorableEmployee,
} from '@/lib/isolation-restore-api';
import { AdoptFlow } from '@/pages/protected/restore/AdoptFlow';
import { DaysLeft, RestoreErrorNotice, RootStateBadge, useRestoreT } from '@/pages/protected/restore/restore-ui';
import { RestoreRequestForm } from './RestoreRequestForm';

function remainingDays(e: RestorableEmployee): number | null {
  return typeof e.days_left === 'number' ? Math.max(0, Math.floor(e.days_left)) : daysUntil(e.restore_until);
}

/**
 * 已註銷的員工 — the caller's own tombstoned Personal employees still in the
 * restore window: ask for a restore (or, when the runtime is gone, a
 * take-over by a new employee), or withdraw a pending request.
 */
export function TombstonedEmployeesCard() {
  const t = useRestoreT();
  const [items, setItems] = useState<RestorableEmployee[] | null>(null);
  const [loadError, setLoadError] = useState<unknown>(null);
  const [open, setOpen] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [rowError, setRowError] = useState<{ key: string; error: unknown } | null>(null);

  const load = useCallback(async () => {
    try {
      const res = await myIsolationApi.listRestorable();
      setItems(Array.isArray(res?.employees) ? res.employees : []);
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
        <ul className="space-y-3">
          {items?.map((e) => {
            const key = `${e.employee_id}:${e.incarnation}`;
            const days = remainingDays(e);
            const root = normalizeRootState(e.root_state);
            const gone = root === 'root_gone';
            const pending = e.open_request != null;
            const expanded = open === key;
            const regionId = `restore-region-${e.employee_id}-${e.incarnation}`;
            const name = e.display_name || e.label;
            const closed = days === 0;
            return (
              <li key={key} className="rounded-lg border border-surface-border p-3">
                <div className="flex flex-wrap items-center justify-between gap-2">
                  <div className="min-w-0">
                    <p className="truncate text-sm font-medium" title={name}>{name}</p>
                    <p className="flex flex-wrap items-center gap-2 text-xs">
                      <span className="font-mono text-muted-foreground">{e.label}</span>
                      <RootStateBadge state={root} />
                      <DaysLeft days={days} />
                    </p>
                  </div>
                  <div className="flex flex-wrap gap-2">
                    {pending ? (
                      <Button variant="outline" size="sm" disabled={busy === key} onClick={() => void cancel(e, key)}>
                        {t('myPersonal.tombstoned.cancel')}
                      </Button>
                    ) : (
                      <Button
                        variant={expanded ? 'secondary' : 'brandSubtle'}
                        size="sm"
                        disabled={closed || e.credential_changed === true}
                        aria-expanded={expanded}
                        aria-controls={regionId}
                        onClick={() => setOpen(expanded ? null : key)}
                      >
                        {gone
                          ? t('myPersonal.tombstoned.adopt')
                          : days == null
                            ? t('myPersonal.tombstoned.restore')
                            : t('myPersonal.tombstoned.restoreDays', { days })}
                      </Button>
                    )}
                  </div>
                </div>
                {pending && (
                  <p role="status" className="mt-2 text-sm text-muted-foreground">
                    {t(e.open_request?.kind === 'adoption' ? 'myPersonal.tombstoned.pendingAdoption' : 'myPersonal.tombstoned.pendingRestore')}
                  </p>
                )}
                {e.credential_changed === true && !pending && (
                  <p className="mt-2 text-sm">{t('restoreFlow.error.CREDENTIAL_CHANGED')}</p>
                )}
                {root === 'unknown' && !pending && (
                  <p className="mt-2 text-xs text-muted-foreground">{t('myPersonal.tombstoned.unknownRoot')}</p>
                )}
                {rowError?.key === key && <RestoreErrorNotice className="mt-2" error={rowError.error} />}
                {expanded && !pending && (
                  <section id={regionId} aria-label={t(gone ? 'myPersonal.tombstoned.adopt' : 'myPersonal.tombstoned.restore')} className="mt-3 border-t border-surface-border pt-3">
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
