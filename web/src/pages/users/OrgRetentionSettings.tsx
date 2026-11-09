import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { AlertTriangle } from 'lucide-react';
import { api, type OrgSettingsUpdate, type TeamHistorySettings } from '@/lib/api';
import { Button, Input } from '@/components/mds';

/** RM-173 limits, mirrored from the gateway (`session_retention::MAX_DAYS`). */
const MAX_DAYS = 3650;
const SINCE_FLOOR_DATE = '2020-01-01';
const SINCE_FLOOR_ISO = '2020-01-01T00:00:00Z';

type Props = { settings: TeamHistorySettings; onSaved: (settings: TeamHistorySettings) => void };

function errorCode(e: unknown): string {
  if (e && typeof e === 'object' && 'code' in e) return String((e as { code: unknown }).code);
  return '';
}

function errorText(e: unknown): string {
  if (e instanceof Error) return e.message;
  if (e && typeof e === 'object' && 'message' in e) return String((e as { message: unknown }).message);
  return String(e);
}

function validDays(value: string): boolean {
  return /^\d{1,4}$/.test(value) && Number(value) <= MAX_DAYS;
}

/** Local calendar date of an instant, as `<input type="date">` wants it. */
function localDate(value: string | Date): string {
  const d = typeof value === 'string' ? new Date(value) : value;
  if (Number.isNaN(d.getTime())) return '';
  const pad = (n: number) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}

/** Local midnight of a `YYYY-MM-DD` date, never before the gateway's floor. */
function dateToInstant(date: string): string {
  const local = new Date(`${date}T00:00:00`);
  return local.getTime() < Date.parse(SINCE_FLOOR_ISO) ? SINCE_FLOOR_ISO : local.toISOString();
}

/** Two day counts: when a quiet conversation is archived (收起) and when an
 *  archived one is deleted. Server-confirmed values only. */
export function RetentionSettings({ settings, onSaved }: Props) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id: `teamHistory.${id}` });
  const confirmedArchive = String(settings.archive_after_days ?? '');
  const confirmedPurge = String(settings.purge_after_days ?? '');
  const [archive, setArchive] = useState(confirmedArchive);
  const [purge, setPurge] = useState(confirmedPurge);
  const [busy, setBusy] = useState(false);
  const [status, setStatus] = useState<'idle' | 'saved' | 'error'>('idle');
  useEffect(() => { setArchive(confirmedArchive); setPurge(confirmedPurge); }, [confirmedArchive, confirmedPurge]);
  // A gateway older than v1.81 does not report these settings.
  if (typeof settings.archive_after_days !== 'number' || typeof settings.purge_after_days !== 'number') return null;

  const invalid = !validDays(archive) || !validDays(purge);
  const unchanged = archive === confirmedArchive && purge === confirmedPurge;
  async function save() {
    setBusy(true);
    setStatus('idle');
    try {
      const confirmed = await api.users.orgSettings.save({ archive_after_days: Number(archive), purge_after_days: Number(purge) });
      if (typeof confirmed.archive_after_days !== 'number' || typeof confirmed.manager_team_history !== 'boolean') throw new Error('invalid settings');
      onSaved(confirmed);
      setStatus('saved');
    } catch { setStatus('error'); }
    finally { setBusy(false); }
  }
  return <div className="space-y-3 border-t border-surface-border pt-3" aria-labelledby="retention-settings-title" role="group">
    <h3 id="retention-settings-title" className="text-sm font-medium">{t('retention.title')}</h3>
    <p className="max-w-4xl text-sm text-muted-foreground">{t('retention.description')}</p>
    <div className="grid gap-3 sm:grid-cols-2">
      <div className="space-y-1.5">
        <label className="text-sm" htmlFor="retention-archive-days">{t('retention.archiveLabel')}</label>
        <Input id="retention-archive-days" type="number" inputMode="numeric" min={0} max={MAX_DAYS} step={1} value={archive} disabled={busy}
          onChange={e => { setArchive(e.target.value.trim()); setStatus('idle'); }} />
      </div>
      <div className="space-y-1.5">
        <label className="text-sm" htmlFor="retention-purge-days">{t('retention.purgeLabel')}</label>
        <Input id="retention-purge-days" type="number" inputMode="numeric" min={0} max={MAX_DAYS} step={1} value={purge} disabled={busy}
          onChange={e => { setPurge(e.target.value.trim()); setStatus('idle'); }} />
      </div>
    </div>
    {validDays(purge) && Number(purge) > 0 && <p className="flex items-start gap-2 text-sm text-warning"><AlertTriangle className="mt-0.5 size-4 shrink-0" />{t('retention.purgeWarning')}</p>}
    {invalid && <p className="text-sm text-destructive">{t('retention.invalid')}</p>}
    <div className="flex flex-wrap items-center gap-2">
      <Button size="sm" disabled={busy || invalid || unchanged} onClick={() => void save()}>{t('retention.save')}</Button>
      {status === 'saved' && <span role="status" className="text-sm text-muted-foreground">{t('retention.saved')}</span>}
    </div>
    {status === 'error' && <p role="alert" className="text-sm text-destructive">{t('unavailable')}</p>}
  </div>;
}

/** 團隊紀錄起算時間. Moving it earlier exposes older conversations to
 *  managers, so the gateway asks for the admin's own password (and second
 *  factor) — collected here in a confirmation step. */
export function HistorySinceSetting({ settings, onSaved }: Props) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id: `teamHistory.${id}` });
  const current = settings.team_history_since ?? settings.since;
  const confirmedDate = current ? localDate(current) : '';
  const [date, setDate] = useState(confirmedDate);
  const [stepUp, setStepUp] = useState(false);
  const [password, setPassword] = useState('');
  const [code, setCode] = useState('');
  const [needCode, setNeedCode] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => { setDate(confirmedDate); }, [confirmedDate]);
  // A gateway older than v1.81 cannot change the start date.
  if (!('team_history_since' in settings)) return null;

  const today = localDate(new Date());
  const enabled = settings.manager_team_history && !!current;
  const validDate = /^\d{4}-\d{2}-\d{2}$/.test(date) && date >= SINCE_FLOOR_DATE && date <= today;
  const instant = validDate ? dateToInstant(date) : '';
  const earlier = !!current && validDate && Date.parse(instant) < Date.parse(current);

  const closeStepUp = () => { setStepUp(false); setPassword(''); setCode(''); setNeedCode(false); };
  async function apply(withStepUp: boolean) {
    setBusy(true);
    setError(null);
    const change: OrgSettingsUpdate = { team_history_since: instant };
    if (withStepUp) {
      change.password = password;
      if (needCode) change.code = code.trim();
    }
    try {
      const confirmed = await api.users.orgSettings.save(change);
      if (typeof confirmed.manager_team_history !== 'boolean') throw new Error('invalid settings');
      onSaved(confirmed);
      closeStepUp();
    } catch (e) {
      const c = errorCode(e);
      if (c === 'mfa_code_required' || errorText(e).includes('MFA_CODE_REQUIRED')) { setStepUp(true); setNeedCode(true); }
      else if (c === 'step_up_required') setStepUp(true);
      else if (c === 'step_up_failed') setError(t('stepUp.failed'));
      else if (c === 'invalid_params') setError(t('since.invalid'));
      else setError(t('unavailable'));
    } finally { setBusy(false); }
  }
  const submit = () => { if (earlier) { setError(null); setStepUp(true); } else void apply(false); };

  return <div className="space-y-3 border-t border-surface-border pt-3">
    <label htmlFor="history-since-date" className="block text-sm font-medium">{t('since.label')}</label>
    <p className="max-w-4xl text-sm text-muted-foreground">{t(enabled ? 'since.description' : 'since.off')}</p>
    <p className="flex items-start gap-2 text-sm text-warning"><AlertTriangle className="mt-0.5 size-4 shrink-0" />{t('since.warning')}</p>
    <div className="flex flex-wrap items-center gap-2">
      <Input id="history-since-date" type="date" className="w-auto" min={SINCE_FLOOR_DATE} max={today} value={date} disabled={!enabled || busy || stepUp}
        onChange={e => { setDate(e.target.value); setError(null); }} />
      <Button size="sm" variant="outline" disabled={!enabled || busy || stepUp || !validDate || date === confirmedDate} onClick={submit}>{t('since.save')}</Button>
    </div>
    {enabled && date !== '' && !validDate && <p className="text-sm text-destructive">{t('since.invalid')}</p>}
    {stepUp && <div role="dialog" aria-modal="false" aria-labelledby="history-since-step-up-title" className="space-y-2 rounded-md border border-warning/30 bg-warning/5 p-3">
      <p id="history-since-step-up-title" className="text-sm font-medium">{t('stepUp.title')}</p>
      <p className="text-sm text-muted-foreground">{t('stepUp.description')}</p>
      <div className="space-y-1.5">
        <label className="text-xs font-medium text-muted-foreground" htmlFor="history-since-password">{t('stepUp.password')}</label>
        <Input id="history-since-password" type="password" autoComplete="current-password" value={password} onChange={e => setPassword(e.target.value)} />
      </div>
      {needCode && <div className="space-y-1.5">
        <label className="text-xs font-medium text-muted-foreground" htmlFor="history-since-code">{t('stepUp.code')}</label>
        <Input id="history-since-code" autoComplete="one-time-code" value={code} onChange={e => setCode(e.target.value)} />
      </div>}
      <div className="flex flex-wrap gap-2">
        <Button size="sm" disabled={busy || password.length === 0 || (needCode && code.trim().length === 0)} onClick={() => void apply(true)}>{t('stepUp.confirm')}</Button>
        <Button size="sm" variant="outline" disabled={busy} onClick={() => { closeStepUp(); setError(null); }}>{t('stepUp.cancel')}</Button>
      </div>
    </div>}
    {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
  </div>;
}
