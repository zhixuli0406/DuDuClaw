import { useCallback, useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { client } from '@/lib/ws-client';
import { Button, Card, CardContent } from '@/components/mds';
import { ConfirmDialog } from '@/components/settings/controls';
import { toast } from '@/lib/toast';

interface Reminder {
  id: string;
  agent_id: string;
  message: string;
  trigger_at: string;
  status: string;
  can_manage: boolean;
}

export function AutomationReminders({ enabled }: { enabled: boolean }) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const [rows, setRows] = useState<Reminder[]>([]);
  const [failed, setFailed] = useState(false);
  const [loading, setLoading] = useState(true);
  const [cancel, setCancel] = useState<Reminder | null>(null);
  const load = useCallback(async () => {
    try {
      const result = await client.call('reminders.list') as { reminders: Reminder[] };
      setRows(result.reminders ?? []);
      setFailed(false);
    } catch { setRows([]); setFailed(true); }
    finally { setLoading(false); }
  }, []);
  useEffect(() => {
    if (!enabled) return;
    void load();
    const timer = setInterval(() => void load(), 30_000);
    return () => clearInterval(timer);
  }, [enabled, load]);
  if (!enabled) return null;
  return <section className="space-y-2" aria-label={t('routines.reminders')}>
    <h2 className="text-sm font-medium">{t('routines.reminders')}</h2>
    {loading ? <p>{t('common.loading')}</p> : failed ? <Button onClick={() => void load()}>{t('common.refresh')}</Button> : rows.length === 0 ?
      <p className="text-xs text-muted-foreground">{t('routines.remindersEmpty')}</p> :
      <Card><CardContent className="divide-y divide-surface-border">{rows.map((row) =>
        <div key={row.id} className="flex items-center justify-between gap-3 py-3">
          <div className="min-w-0"><p className="truncate text-sm">{row.message}</p>
            <p className="text-xs text-muted-foreground">{row.agent_id} · {intl.formatDate(row.trigger_at, { dateStyle: 'medium', timeStyle: 'short' })}</p>
          </div>
          {row.status === 'pending' && row.can_manage && <Button variant="ghost" size="sm" onClick={() => setCancel(row)}>{t('common.cancel')}</Button>}
        </div>)}</CardContent></Card>}
    {cancel && <ConfirmDialog open title={t('routines.cancelReminder')} message={cancel.message}
      confirmLabel={t('common.confirm')} onClose={() => setCancel(null)} onConfirm={() => {
        const target = cancel;
        setCancel(null);
        void client.call('reminders.cancel', { id: target.id }).then(load).catch(() => toast.error(t('routines.reminderCancelFailed')));
      }} />}
  </section>;
}
