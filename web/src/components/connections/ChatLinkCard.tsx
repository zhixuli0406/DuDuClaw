import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { api, type ChatLinkChannel, type ChatLinkIdentity, type ChatLinkStatus, type ChatLinkTarget } from '@/lib/api';
import { Button, Card, CardContent, CardHeader, CardTitle, Skeleton } from '@/components/mds';
import { ConfirmDialog } from '@/components/settings/controls';

const channels: ChatLinkChannel[] = ['telegram', 'googlechat'];
const channelName = (channel: ChatLinkChannel) => channel === 'telegram' ? 'Telegram' : 'Google Chat';
const errorText = (error: unknown) => error instanceof Error ? error.message : String(error);

export function ChatLinkCard() {
  const intl = useIntl();
  const t = (key: string, values?: Record<string, string | number>) => intl.formatMessage({ id: `chatLink.${key}` }, values);
  const [identities, setIdentities] = useState<ChatLinkIdentity[]>([]);
  const [handshake, setHandshake] = useState<ChatLinkStatus | null>(null);
  // The single-use code stays only in this mounted component's memory.
  const [code, setCode] = useState<string | null>(null);
  const [targets, setTargets] = useState<ChatLinkTarget[]>([]);
  const [channel, setChannel] = useState<ChatLinkChannel>('telegram');
  const [choosing, setChoosing] = useState(false);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [remove, setRemove] = useState<ChatLinkIdentity | null>(null);
  const [now, setNow] = useState(Date.now());
  const [reload, setReload] = useState(0);

  useEffect(() => {
    let active = true;
    setLoading(true);
    void api.chatLink.list().then(result => {
      if (!active) return;
      setIdentities(result.identities);
      setHandshake(result.handshakes.find(row => row.status === 'waiting' || row.status === 'matched') ?? null);
      setError('');
    }).catch(e => { if (active) setError(errorText(e)); })
      .finally(() => { if (active) setLoading(false); });
    return () => { active = false; };
  }, [reload]);

  const pending = handshake?.status === 'waiting' || handshake?.status === 'matched';
  useEffect(() => {
    if (!pending || !handshake || busy) return;
    let active = true;
    let inFlight = false;
    const poll = window.setInterval(() => {
      if (inFlight) return;
      inFlight = true;
      void api.chatLink.status(handshake.handshake_id).then(result => {
        if (!active) return;
        setHandshake(result);
        setError('');
        if (result.status !== 'waiting') setCode(null);
      }).catch(e => { if (active) setError(errorText(e)); })
        .finally(() => { inFlight = false; });
    }, 2000);
    return () => { active = false; window.clearInterval(poll); };
  }, [handshake?.handshake_id, pending, busy]);

  useEffect(() => {
    if (!pending || !handshake) return;
    const tick = () => {
      const time = Date.now();
      setNow(time);
      if (time >= Date.parse(handshake.expires_at)) {
        setHandshake(row => row ? { ...row, status: 'expired' } : null);
        setCode(null);
      }
    };
    tick();
    const timer = window.setInterval(tick, 1000);
    return () => window.clearInterval(timer);
  }, [pending, handshake?.handshake_id, handshake?.expires_at]);

  useEffect(() => {
    if (handshake?.status !== 'confirmed') return;
    let active = true;
    void api.chatLink.list().then(result => { if (active) setIdentities(result.identities); })
      .catch(e => { if (active) setError(errorText(e)); });
    return () => { active = false; };
  }, [handshake?.handshake_id, handshake?.status]);

  const available = channels.filter(c => !identities.some(row => row.channel === c && row.proof_status === 'self_confirmed'));
  const selected = available.includes(channel) ? channel : available[0];
  const run = async (action: () => Promise<void>) => {
    setBusy(true);
    setError('');
    try { await action(); } catch (e) { setError(errorText(e)); } finally { setBusy(false); }
  };
  const start = (value: ChatLinkChannel) => run(async () => {
    const result = await api.chatLink.start(value);
    setCode(result.code);
    setTargets(result.targets);
    setHandshake({ handshake_id: result.handshake_id, status: 'waiting', channel: value, expires_at: result.expires_at, display_name: null, masked_target: null });
    setNow(Date.now());
    setChoosing(false);
  });
  const decide = (approve: boolean) => {
    if (!handshake) return;
    void run(async () => {
      if (approve) await api.chatLink.confirm(handshake.handshake_id);
      else await api.chatLink.reject(handshake.handshake_id);
      setHandshake({ ...handshake, status: approve ? 'confirmed' : 'rejected' });
      setCode(null);
    });
  };
  const seconds = Math.max(0, Math.ceil(((handshake ? Date.parse(handshake.expires_at) : now) - now) / 1000));
  const countdown = `${Math.floor(seconds / 60).toString().padStart(2, '0')}:${(seconds % 60).toString().padStart(2, '0')}`;
  const proven = identities.some(row => row.proof_status === 'self_confirmed');

  return <Card>
    <CardHeader><CardTitle>{t('title')}</CardTitle></CardHeader>
    <CardContent className="space-y-4">
      <p className="text-sm text-muted-foreground">{t('intro')}</p>
      {error && !remove && <p role="alert" className="text-sm text-destructive">{error}</p>}
      {loading ? <Skeleton className="h-24 w-full" /> : <>
        {identities.map((identity, index) => <section key={index} className="space-y-3 rounded-lg border border-border p-4">
          <p className="font-medium">{channelName(identity.channel)} · {t('lastFour', { digits: Array.from(identity.channel_user_id).slice(-4).join('') })}</p>
          <span className="inline-block rounded-full border border-border px-2 py-1 text-xs">{t(identity.proof_status === 'self_confirmed' ? 'confirmed' : 'unconfirmed')}</span>
          {identity.proof_status === 'self_confirmed' ? <>
            <div><Button variant="outline" disabled={busy} onClick={() => setRemove(identity)}>{t('remove')}</Button></div>
            <p className="text-sm text-muted-foreground">{t('removeNote')}</p>
          </> : <>
            <p className="text-sm text-muted-foreground">{t('prefilled')}</p>
            <p className="text-sm">{t('prefilledHelp')}</p>
            {!pending && <Button disabled={busy} onClick={() => void start(identity.channel)}>{t('finishProof')}</Button>}
          </>}
        </section>)}
        {handshake?.status === 'waiting' && <div className="space-y-3">
          {code ? <div className="flex flex-wrap items-center gap-3"><code className="break-all text-lg font-semibold">{code}</code><Button variant="outline" disabled={busy} onClick={() => void run(async () => { await navigator.clipboard.writeText(code); })}>{t('copy')}</Button></div> : <p>{t('resumed')}</p>}
          <p className="text-sm" role="timer">{t('remaining', { time: countdown })}</p>
          <ol className="list-decimal space-y-2 pl-5 text-sm">
            <li>{targets.length ? targets.map((target, index) => <p key={index}>{t('open', { channel: channelName(target.channel) })}{target.channel === 'telegram' && target.bot_username ? t('telegramTarget', { bot: target.bot_username.replace(/^@/, '') }) : target.channel === 'googlechat' ? t('googleTarget', { name: target.display_name }) : t('privateMessage')}</p>) : t('openChat', { channel: channelName(handshake.channel) })}</li>
            <li>{t('paste')} {t('noGroups')}</li>
            <li>{t('return')}</li>
          </ol>
          <Button variant="outline" disabled={busy} onClick={() => void run(async () => { await api.chatLink.cancel(handshake.handshake_id); setHandshake(null); setCode(null); setChoosing(true); })}>{t('cancel')}</Button>
        </div>}
        {handshake?.status === 'matched' && <div className="space-y-3">
          <p>{t('match', { channel: channelName(handshake.channel), name: handshake.display_name ?? '', target: Array.from(handshake.masked_target ?? '').slice(-4).join('') })}</p>
          <div className="flex gap-2"><Button disabled={busy} onClick={() => decide(true)}>{t('yes')}</Button><Button variant="outline" disabled={busy} onClick={() => decide(false)}>{t('no')}</Button></div>
          <p className="text-sm text-muted-foreground">{t('sameBrowser')}</p>
        </div>}
        {handshake?.status === 'expired' && <p role="status">{t('expired')}</p>}
        {handshake?.status === 'rejected' && <p role="status">{t('rejected')}</p>}
        {!pending && available.length > 0 && (choosing || !proven || handshake?.status === 'expired' || handshake?.status === 'rejected' ? <div className="space-y-3">
          <fieldset className="flex flex-wrap gap-4"><legend className="mb-2 text-sm">{t('choose')}</legend>{available.map(value => <label key={value} className="flex items-center gap-2 text-sm"><input type="radio" name="chat-link-channel" checked={selected === value} onChange={() => setChannel(value)} />{channelName(value)}</label>)}</fieldset>
          <Button disabled={busy || !selected} onClick={() => { if (selected) void start(selected); }}>{t('generate')}</Button>
        </div> : <Button variant="outline" disabled={busy} onClick={() => setChoosing(true)}>{t('another')}</Button>)}
        {error && !identities.length && !handshake && <Button variant="outline" disabled={busy} onClick={() => setReload(value => value + 1)}>{t('retry')}</Button>}
      </>}
      <ConfirmDialog open={remove !== null} onClose={() => { if (!busy) setRemove(null); }} title={t('removeQuestion')} message={t('removeNote')} confirmLabel={t('remove')} cancelLabel={t('cancel')} busy={busy} onConfirm={() => {
        if (!remove) return;
        void run(async () => { await api.chatLink.remove(remove.channel, remove.channel_user_id); setRemove(null); setIdentities((await api.chatLink.list()).identities); });
      }}>{error && <p role="alert" className="text-sm text-destructive">{error}</p>}</ConfirmDialog>
    </CardContent>
  </Card>;
}
