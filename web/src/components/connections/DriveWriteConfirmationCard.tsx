import { Fragment, useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { api, type DriveWriteConfirmation } from '@/lib/api';
import { Button, Card, CardContent, CardHeader, CardTitle, useErrorMessage } from '@/components/mds';
import { settingsRefusal } from '@/lib/settings-refusal';
import { useAgentsStore } from '@/stores/agents-store';

export function decodeCardValue(raw: string): unknown {
  const text = raw.replace(/\\([*_`\[\]#!])/g, '$1')
    .replace(/&lt;/g, '<').replace(/&gt;/g, '>').replace(/&amp;/g, '&');
  try { return JSON.parse(text); } catch { return text; }
}

function textValue(raw: string): string {
  const value = decodeCardValue(raw);
  return typeof value === 'string' || typeof value === 'number' ? String(value) : '';
}

function sizeText(raw: string, locale: string): string {
  const value = textValue(raw);
  if (!value.trim()) return '';
  const bytes = Number(value);
  if (!Number.isFinite(bytes) || bytes < 0) return value;
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  const unit = Math.min(units.length - 1, Math.floor(Math.log(Math.max(1, bytes)) / Math.log(1024)));
  return `${new Intl.NumberFormat(locale, { maximumFractionDigits: 1 }).format(bytes / 1024 ** unit)} ${units[unit]}`;
}

export function DriveWriteConfirmationCard({ request, onDecided }: { request: DriveWriteConfirmation; onDecided: (id: string) => void }) {
  const intl = useIntl();
  const t = (key: string, values?: Record<string, string | number>) => intl.formatMessage({ id: `driveWriteConfirmation.${key}` }, values);
  const errorText = useErrorMessage();
  const agents = useAgentsStore(state => state.agents);
  const [now, setNow] = useState(Date.now());
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const expiry = Date.parse(request.expires_at);
  const expired = !Number.isFinite(expiry) || now >= expiry;
  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, []);
  const employee = textValue(request.employee);
  const agent = textValue(request.agent).trim() || agents.find(row => row.name === employee)?.display_name?.trim() || t('unknownAgent');
  const before = textValue(request.rename_before);
  const after = textValue(request.rename_after);
  const renaming = !!(before || after);
  const action = textValue(request.action);
  const source = decodeCardValue(request.source_names);
  const sources = Array.isArray(source) ? source.filter(value => typeof value === 'string').join('、') : typeof source === 'string' ? source : '';
  const attachment = textValue(request.attachment_name);
  const preview = Array.from(textValue(request.preview)).slice(0, 500).join('');
  const rows: [string, string][] = [
    ['agent', agent], ['account', textValue(request.account)],
    ['action', renaming ? t('rename') : ['create', 'modify', 'move', 'trash'].includes(action) ? t(`action.${action}`) : action],
    ['before', before], ['after', after],
    [attachment ? 'attachment' : 'file', attachment || textValue(request.file_name)],
    ['location', textValue(request.destination_name)], ['source', sources], ['size', sizeText(request.size, intl.locale)],
  ];
  const seconds = expired ? 0 : Math.max(0, Math.ceil((expiry - now) / 1000));
  const time = `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, '0')}`;
  const decide = async (approve: boolean) => {
    if (busy || !Number.isFinite(expiry) || Date.now() >= expiry) return;
    setBusy(true);
    setError('');
    try { await api.personalGoogle.writeConfirm(request.id, approve); onDecided(request.id); }
    catch (err) { setError(settingsRefusal(err, errorText(err))); }
    finally { setBusy(false); }
  };
  return <Card>
    <CardHeader><CardTitle>{t('title')}</CardTitle></CardHeader>
    <CardContent className="space-y-4">
      <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-4 gap-y-2 text-sm">
        {rows.filter(([, value]) => value !== '').map(([key, value]) => <Fragment key={key}><dt className="text-muted-foreground">{t(key)}</dt><dd className="min-w-0 whitespace-pre-wrap break-words">{value}</dd></Fragment>)}
      </dl>
      {preview && <div className="space-y-2"><p className="text-sm text-muted-foreground">{t('preview')}</p><pre className="whitespace-pre-wrap break-words rounded-lg bg-muted p-3 font-sans text-sm">{preview}</pre></div>}
      {expired ? <p role="status" className="text-sm">{t('expired')}</p> : <p role="timer" className="text-sm tabular-nums">{t('remaining', { time })}</p>}
      {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
      <div className="flex flex-wrap gap-2"><Button disabled={busy || expired} onClick={() => void decide(true)}>{t('approve')}</Button><Button variant="outline" disabled={busy || expired} onClick={() => void decide(false)}>{t('cancel')}</Button></div>
    </CardContent>
  </Card>;
}

export function DriveWriteConfirmations() {
  const [requests, setRequests] = useState<DriveWriteConfirmation[]>([]);
  const [error, setError] = useState<unknown>(null);
  const dismissed = useRef(new Set<string>());
  const errorText = useErrorMessage();
  useEffect(() => {
    let active = true;
    let inFlight = false;
    const refresh = async () => {
      if (inFlight) return;
      inFlight = true;
      try {
        const result = await api.personalGoogle.writeConfirmations();
        if (active) {
          setRequests(previous => {
            const rows = result.requests.filter(row => !dismissed.current.has(row.id));
            // Retain elapsed cards so the member can read why they can no longer confirm.
            return [...rows, ...previous.filter(row => Date.parse(row.expires_at) <= Date.now() && !dismissed.current.has(row.id) && !rows.some(next => next.id === row.id))];
          });
          setError(null);
        }
      } catch (err) { if (active) setError(err); }
      finally { inFlight = false; }
    };
    void refresh();
    const timer = window.setInterval(() => void refresh(), 3000);
    return () => { active = false; window.clearInterval(timer); };
  }, []);
  return <>
    {error != null && <p role="alert" className="text-sm text-destructive">{settingsRefusal(error, errorText(error))}</p>}
    {requests.map(request => <DriveWriteConfirmationCard key={request.id} request={request} onDecided={id => {
      dismissed.current.add(id);
      setRequests(rows => rows.filter(row => row.id !== id));
    }} />)}
  </>;
}
