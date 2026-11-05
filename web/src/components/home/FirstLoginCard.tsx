import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { Link, useNavigate } from 'react-router';
import { Check } from 'lucide-react';
import { ConnectionsChecklist } from './ConnectionsChecklist';
import { api } from '@/lib/api';
import { formatError, toast } from '@/lib/toast';
import { useAuthStore } from '@/stores/auth-store';
import { useConversationsStore } from '@/stores/conversations-store';
import { Button, Card, CardContent, CardHeader, CardTitle, Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/mds';

type Step = 'channel' | 'google' | 'example' | 'capabilities';
type Status = 'pending' | 'done' | 'skipped';
interface Progress {
  enabled: boolean;
  dismissed: boolean;
  password_done: boolean;
  steps: Record<Step, Status>;
  examples: string[];
}
const STEPS: Step[] = ['channel', 'google', 'example', 'capabilities'];

export function FirstLoginCard({ enabled }: { enabled: boolean }) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const navigate = useNavigate();
  const user = useAuthStore(s => s.user);
  const [data, setData] = useState<Progress | null>(null);
  const [loadedFor, setLoadedFor] = useState<string | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const [panel, setPanel] = useState<'channel' | 'capabilities' | null>(null);
  const [revision, setRevision] = useState(0);

  useEffect(() => {
    let alive = true;
    setData(null); setLoadedFor(null); setError(null); setPanel(null);
    if (enabled && user?.id) void api.onboarding.get(intl.locale).then(result => {
      if (alive) { setData(result); setLoadedFor(user.id); }
    }).catch(cause => { if (alive) setError(cause); });
    return () => { alive = false; };
  }, [enabled, user?.id, intl.locale, revision]);

  const update = async (change: { step?: Step; status?: 'done' | 'skipped'; dismissed?: true }) => {
    setBusy(true);
    try {
      const result = await api.onboarding.update(change, intl.locale);
      if (useAuthStore.getState().user?.id !== user?.id) return false;
      setData(result); return true;
    }
    catch (cause) {
      if (useAuthStore.getState().user?.id === user?.id) toast.error(formatError(cause));
      return false;
    }
    finally { setBusy(false); }
  };
  if (!enabled || !user?.id) return null;
  if (error != null) return <Card><CardContent className="space-y-3"><p role="alert" className="text-sm">{formatError(error)}</p>
    <Button variant="outline" onClick={() => setRevision(r => r + 1)}>{t('errorState.retry')}</Button></CardContent></Card>;
  if (data && loadedFor !== user.id) return null;
  if (data && !data.enabled) return <ConnectionsChecklist enabled={enabled} />;
  if (!data || data.dismissed || (data.password_done && STEPS.every(step => data.steps[step] !== 'pending'))) return null;
  const tryExample = async (example: string) => {
    if (!await update({ step: 'example', status: 'done' })) return;
    useConversationsStore.getState().startNew();
    navigate('/chat', { state: { draft: example } });
  };

  return <>
    <Card>
      <CardHeader className="flex-row items-center justify-between gap-3">
        <CardTitle>{t('firstLogin.title')}</CardTitle>
        <Button variant="ghost" size="sm" disabled={busy} onClick={() => void update({ dismissed: true })}>{t('firstLogin.dismiss')}</Button>
      </CardHeader>
      <CardContent className="space-y-4">
        <p className="text-sm text-muted-foreground">{t('firstLogin.description')}</p>
        <ul className="divide-y divide-border">
          <li className="flex items-center justify-between gap-3 py-3 text-sm">
            <span>{t('firstLogin.password')}</span>
            {data.password_done && <span className="flex items-center gap-1 text-muted-foreground"><Check className="size-4" aria-hidden="true" />{t('firstLogin.done')}</span>}
          </li>
          {STEPS.map(step => <li key={step} className="space-y-3 py-3">
            <div className="flex flex-wrap items-center justify-between gap-3">
              <span className="text-sm">{t(`firstLogin.${step}`)}</span>
              {data.steps[step] !== 'pending' ? <span className="flex items-center gap-1 text-sm text-muted-foreground"><Check className="size-4" aria-hidden="true" />{t(data.steps[step] === 'done' ? 'firstLogin.done' : 'firstLogin.skipped')}</span> : <div className="flex flex-wrap items-center gap-2">
                {step === 'channel' && <Button variant="outline" size="sm" disabled={busy} onClick={() => setPanel('channel')}>{t('firstLogin.connect')}</Button>}
                {step === 'google' && <Button variant="outline" size="sm" disabled={busy} onClick={() => navigate('/me/connections')}>{t('firstLogin.connect')}</Button>}
                {step === 'capabilities' && <Button variant="outline" size="sm" disabled={busy} onClick={() => setPanel('capabilities')}>{t('firstLogin.read')}</Button>}
                {(step === 'channel' || step === 'google') && <Button variant="ghost" size="sm" disabled={busy} onClick={() => void update({ step, status: 'skipped' })}>{t('firstLogin.skip')}</Button>}
              </div>}
            </div>
            {step === 'example' && data.steps.example === 'pending' && <div className="space-y-2">{data.examples.map((example, index) => <div key={index} className="flex flex-wrap items-center justify-between gap-3 rounded-lg border border-border p-3">
              <p className="min-w-0 flex-1 whitespace-pre-wrap break-words text-sm text-muted-foreground">{example}</p>
              <Button variant="outline" size="sm" disabled={busy} onClick={() => void tryExample(example)} aria-label={intl.formatMessage({ id: 'firstLogin.tryNumber' }, { number: index + 1 })}>{t('firstLogin.try')}</Button>
            </div>)}</div>}
          </li>)}
        </ul>
        <Button variant="ghost" size="sm" disabled={busy} onClick={() => setRevision(r => r + 1)}>{t('firstLogin.refresh')}</Button>
      </CardContent>
    </Card>
    <Dialog open={panel !== null} onOpenChange={open => { if (!open) setPanel(null); }}>
      <DialogContent>
        <DialogHeader><DialogTitle>{t(panel === 'channel' ? 'firstLogin.channel' : 'firstLogin.capabilities')}</DialogTitle>
          <DialogDescription>{t(panel === 'channel' ? 'firstLogin.channelHelp' : 'firstLogin.helpIntro')}</DialogDescription></DialogHeader>
        {panel === 'channel' ? user.role === 'admin' && <Link to="/users" className="text-sm underline underline-offset-4 focus-visible:ring-3 focus-visible:ring-ring/50">{t('firstLogin.manageChannels')}</Link>
          : <div className="space-y-4 text-sm">{['can', 'cannot', 'approvals', 'privacy'].map(section => <section key={section} className="space-y-1">
            <h3 className="font-medium">{t(`firstLogin.help.${section}.title`)}</h3><p className="text-muted-foreground">{t(`firstLogin.help.${section}`)}</p>
          </section>)}</div>}
        <DialogFooter>
          {panel === 'capabilities' ? <Button variant="brand" disabled={busy} onClick={async () => { if (await update({ step: 'capabilities', status: 'done' })) setPanel(null); }}>{t('firstLogin.understood')}</Button>
            : <Button variant="outline" onClick={() => setPanel(null)}>{t('common.close')}</Button>}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  </>;
}
