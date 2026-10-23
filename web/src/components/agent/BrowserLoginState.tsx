import { useCallback, useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { Button, Card, CardContent, CardHeader, CardTitle, useErrorMessage } from '@/components/mds';
import { client } from '@/lib/ws-client';
import { toast } from '@/lib/toast';

interface LoginState {
  enabled: boolean;
  saved: boolean;
  sealed_bytes: number | null;
  updated_at: number | null;
  hosts_count: number | null;
  can_reset: boolean;
}

/** Non-secret metadata only. Reset authority is rechecked by the gateway. */
export function BrowserLoginState({ agentId }: { agentId: string }) {
  const intl = useIntl();
  const errorText = useErrorMessage();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);
  const [state, setState] = useState<LoginState | null>(null);
  const [confirm, setConfirm] = useState(false);
  const [busy, setBusy] = useState(false);
  const refresh = useCallback(async () => {
    try {
      setState(await client.call('computer_profiles.status', { agent_id: agentId }) as LoginState);
    } catch { setState(null); }
  }, [agentId]);
  useEffect(() => {
    void refresh();
    const timer = setInterval(() => { if (!document.hidden) void refresh(); }, 5000);
    return () => clearInterval(timer);
  }, [refresh]);
  const reset = async () => {
    setBusy(true);
    try {
      await client.call('computer_profiles.reset', { agent_id: agentId, confirm: true });
      setConfirm(false);
      toast.success(t('browserLogin.reset.done'));
      await refresh();
    } catch (error) { toast.error(errorText(error)); }
    finally { setBusy(false); }
  };
  if (!state) return null;
  return <Card>
    <CardHeader><CardTitle className="text-sm">{t('browserLogin.title')}</CardTitle></CardHeader>
    <CardContent className="space-y-3 text-sm">
      <p>{t(state.saved ? 'browserLogin.saved' : 'browserLogin.empty')}</p>
      {!state.enabled && <p className="text-muted-foreground">{t('browserLogin.disabled')}</p>}
      {state.saved && <p className="text-muted-foreground">{t('browserLogin.details', {
        size: ((state.sealed_bytes ?? 0) / (1024 * 1024)).toFixed(1),
        hosts: state.hosts_count ?? 0,
        date: state.updated_at ? intl.formatDate(state.updated_at * 1000, { year: 'numeric', month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' }) : '—',
      })}</p>}
      {state.can_reset && (state.saved || state.enabled) && (confirm ? <div className="space-y-2" role="alert">
        <p>{t('browserLogin.reset.confirm')}</p>
        <div className="flex flex-wrap gap-2">
          <Button variant="destructive" size="sm" disabled={busy} onClick={() => void reset()}>{t('browserLogin.reset')}</Button>
          <Button variant="outline" size="sm" disabled={busy} onClick={() => setConfirm(false)}>{t('common.cancel')}</Button>
        </div>
      </div> : <Button variant="outline" size="sm" onClick={() => setConfirm(true)}>{t('browserLogin.reset')}</Button>)}
    </CardContent>
  </Card>;
}
