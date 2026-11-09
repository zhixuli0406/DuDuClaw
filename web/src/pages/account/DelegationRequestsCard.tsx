import { useCallback, useEffect, useState, type ReactNode } from 'react';
import { useIntl } from 'react-intl';
import { AlertTriangle } from 'lucide-react';
import { Button, Card, CardContent, CardDescription, CardHeader, CardTitle, Skeleton } from '@/components/mds';
import { myIsolationApi, toMillis, type PersonalDelegation } from '@/lib/isolation-restore-api';
import { toast } from '@/lib/toast';
import { BidiSafeText, RestoreErrorNotice, hasBidiControls } from '@/pages/protected/restore/restore-ui';

/**
 * 委派請求 — the owner decides each delegation from their Personal
 * employee's live run to a Shared employee. The task is shown in full,
 * exactly as the card binds it; other people who use the Shared employee
 * may be able to see it.
 */
export function DelegationRequestsCard() {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);
  const [items, setItems] = useState<PersonalDelegation[] | null>(null);
  const [loadError, setLoadError] = useState<unknown>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [errors, setErrors] = useState<Record<string, unknown>>({});

  const load = useCallback(async () => {
    try {
      const res = await myIsolationApi.listDelegations();
      setItems(Array.isArray(res?.delegations) ? res.delegations : []);
      setLoadError(null);
    } catch (e) {
      setLoadError(e);
    }
  }, []);

  useEffect(() => { void load(); }, [load]);

  const decide = async (item: PersonalDelegation, approve: boolean) => {
    setBusy(item.id);
    setErrors(({ [item.id]: _drop, ...rest }) => rest);
    try {
      await myIsolationApi.decideDelegation(item.id, approve);
      toast.success(t(approve ? 'myPersonal.delegation.approved' : 'myPersonal.delegation.denied'));
      setItems((prev) => prev?.filter((d) => d.id !== item.id) ?? prev);
    } catch (e) {
      setErrors((prev) => ({ ...prev, [item.id]: e }));
    } finally {
      setBusy(null);
    }
  };

  const formatTime = (value: PersonalDelegation['expires_at']) => {
    const ms = toMillis(value);
    return ms == null ? null : intl.formatDate(ms, { dateStyle: 'medium', timeStyle: 'short' });
  };

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('myPersonal.delegation.title')}</CardTitle>
        <CardDescription>{t('myPersonal.delegation.desc')}</CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        {loadError != null && (
          <div className="space-y-2">
            <RestoreErrorNotice error={loadError} />
            <Button variant="outline" size="sm" onClick={() => void load()}>{t('restoreFlow.common.retry')}</Button>
          </div>
        )}
        {items === null && loadError == null && <Skeleton className="h-20 w-full" />}
        {items !== null && items.length === 0 && (
          <p className="text-sm text-muted-foreground">{t('myPersonal.delegation.empty')}</p>
        )}
        {items?.map((item) => {
          const taskId = `delegation-task-${item.id}`;
          const expires = formatTime(item.expires_at);
          const created = formatTime(item.created_at);
          return (
            // Board "Mine": the card waiting for the owner is outlined in the brand colour.
            <article key={item.id} aria-labelledby={`${taskId}-h`} className="space-y-3 rounded-lg border-2 border-brand/70 p-4">
              <h3 id={`${taskId}-h`} className="text-sm break-words">
                {intl.formatMessage({ id: 'myPersonal.delegation.route' }, {
                  from: item.source_agent,
                  to: item.target_agent,
                  b: (chunks: ReactNode[]) => <strong className="font-semibold">{chunks}</strong>,
                })}
              </h3>
              {created && <p className="text-xs text-muted-foreground">{t('myPersonal.delegation.created', { time: created })}</p>}
              {/* The full bound text, never shortened (dir=auto, pre-wrap, bidi controls made visible). */}
              <BidiSafeText id={taskId} text={item.task} label={t('myPersonal.delegation.taskLabel')} />
              {hasBidiControls(item.task) && (
                <p className="text-xs text-amber-700 dark:text-amber-400">{t('myPersonal.delegation.bidiNotice')}</p>
              )}
              <p className="flex items-start gap-2 rounded-lg border border-warning/30 bg-warning/5 px-3 py-2 text-sm">
                <AlertTriangle className="mt-0.5 size-4 shrink-0 text-warning" aria-hidden="true" />
                <span>{t('myPersonal.delegation.sharedWarning', { to: item.target_agent })}</span>
              </p>
              <RestoreErrorNotice error={errors[item.id]} />
              <div className="flex flex-wrap justify-end gap-2">
                <Button variant="outline" disabled={busy === item.id} onClick={() => void decide(item, false)}>
                  {t('myPersonal.delegation.deny')}
                </Button>
                <Button variant="brand" disabled={busy === item.id} onClick={() => void decide(item, true)}>
                  {t('myPersonal.delegation.approve')}
                </Button>
              </div>
              <p className="text-xs text-muted-foreground">
                {expires ? t('myPersonal.delegation.expires', { time: expires }) : t('myPersonal.delegation.expiresNoTime')}
              </p>
            </article>
          );
        })}
      </CardContent>
    </Card>
  );
}
