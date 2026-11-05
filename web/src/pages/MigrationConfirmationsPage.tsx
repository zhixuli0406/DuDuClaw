import { useCallback, useEffect, useRef, useState } from 'react';
import { Link } from 'react-router';
import { useIntl } from 'react-intl';
import { useConnectionStore } from '@/stores/connection-store';
import { bulkEligible, confirmationApi, proposalKey, type ConfirmationItem, type ConfirmationList } from '@/lib/migration-confirmations';

const buttonClass = 'rounded-md border px-3 py-2 text-sm hover:bg-muted disabled:cursor-not-allowed disabled:opacity-50';

export function MigrationConfirmationsPage() {
  const intl = useIntl();
  const t = (key: string) => intl.formatMessage({ id: `migrationConfirm.${key}` });
  const connection = useConnectionStore(state => state.state);
  const [data, setData] = useState<ConfirmationList | null>(null);
  const [selected, setSelected] = useState<string[]>([]);
  const generation = useRef(0);
  const [busy, setBusy] = useState(false);
  const [failed, setFailed] = useState(false);
  const [password, setPassword] = useState('');
  const [code, setCode] = useState('');
  const [authFailed, setAuthFailed] = useState(false);
  const [secrets, setSecrets] = useState<Record<string, string>>({});
  const load = useCallback(async () => {
    const current = ++generation.current;
    try {
      const result = await confirmationApi.list();
      if (current === generation.current) { setData(result); setSelected([]); setFailed(false); }
    } catch { if (current === generation.current) { setData(null); setFailed(true); } }
  }, []);
  useEffect(() => {
    setData(null); setSelected([]); setSecrets({}); setPassword(''); setCode('');
    if (connection === 'authenticated') void load();
    return () => { generation.current += 1; };
  }, [connection, load]);
  const ready = connection === 'authenticated' && !!data?.mutations_available && !!data.csrf_token && data.step_up_valid && data.mfa_enrolled && !busy;
  const selectedItems = data?.items.filter(item => selected.includes(proposalKey(item))) ?? [];
  async function stepUp(event: React.FormEvent) {
    event.preventDefault();
    if (busy || connection !== 'authenticated') return;
    setBusy(true); setAuthFailed(false);
    try { await confirmationApi.stepUp(password, code); await load(); }
    catch { setAuthFailed(true); }
    finally { setPassword(''); setCode(''); setBusy(false); }
  }
  async function decide(action: 'approve' | 'retry' | 'reject', items: ConfirmationItem[]) {
    if (!ready || !data?.csrf_token || items.length === 0) return;
    if (action === 'approve' && items.length > 1 && items.some(item => !bulkEligible(item))) return;
    if (items.some(item => item.high_risk) && !data.mfa_enrolled) return;
    setBusy(true); setFailed(false);
    try {
      await confirmationApi.decide(action, items, data.csrf_token);
      // Readback, never the approval response, determines displayed state.
      await load();
    } catch { setData(null); setSelected([]); setFailed(true); }
    finally { setBusy(false); }
  }
  async function rotateSecret(item: ConfirmationItem) {
    const value = secrets[proposalKey(item)];
    if (!ready || !data?.csrf_token || !value) return;
    setBusy(true); setFailed(false);
    try { await confirmationApi.rotateSecret(item.binding, value, data.csrf_token); await load(); }
    catch { setData(null); setFailed(true); }
    finally { setSecrets({}); setBusy(false); }
  }
  return <div className="mx-auto max-w-4xl space-y-5">
    <header><h1 className="text-2xl font-semibold">{t('title')}</h1><p className="mt-2 text-muted-foreground">{t('description')}</p></header>
    {failed && <p role="alert" className="rounded-lg border border-destructive p-4">{t('loadFailed')}</p>}
    <button className={buttonClass} disabled={busy || connection !== 'authenticated'} onClick={() => void load()}>{t('refresh')}</button>
    {!data && !failed && <p role="status">{t('loading')}</p>}
    {data && <>
      <p role="status">{t(!data.active ? 'inactive' : data.incomplete ? 'incomplete' : 'complete')}</p>
      {data.active && !data.mutations_available && <p className="rounded-lg border p-4">{t('unavailable')}</p>}
      {data.active && (!data.mfa_enrolled || !data.step_up_valid) && <p>{t('authentication')} <Link className="underline" to="/me/account">{t('account')}</Link></p>}
      {data.active && data.mutations_available && data.mfa_enrolled && !data.step_up_valid && <form onSubmit={event => void stepUp(event)} className="flex flex-wrap items-end gap-3 rounded-xl border p-4">
        <label className="space-y-1"><span className="block text-sm">{t('password')}</span><input type="password" autoComplete="current-password" maxLength={4096} required value={password} onChange={event => setPassword(event.target.value)} className="rounded-md border bg-background p-2" /></label>
        <label className="space-y-1"><span className="block text-sm">{t('verificationCode')}</span><input type="text" inputMode="numeric" autoComplete="one-time-code" pattern="[0-9]{6}" maxLength={6} required value={code} onChange={event => setCode(event.target.value)} className="rounded-md border bg-background p-2" /></label>
        <button className={buttonClass} disabled={busy} type="submit">{t('verifyIdentity')}</button>
        {authFailed && <p role="alert" className="w-full text-destructive">{t('verificationFailed')}</p>}
      </form>}
      {data.items.length > 0 && <div className="flex flex-wrap gap-2">
        <button className={buttonClass} disabled={!ready || selectedItems.length === 0 || selectedItems.some(item => !bulkEligible(item))} onClick={() => void decide('approve', selectedItems)}>{t('approveSelected')}</button>
        <button className={buttonClass} disabled={!ready || selectedItems.length === 0} onClick={() => void decide('reject', selectedItems)}>{t('rejectSelected')}</button>
      </div>}
      {data.items.map(item => {
        const key = proposalKey(item);
        const canAct = ready && (!item.high_risk || data.mfa_enrolled);
        return <section key={key} className="space-y-3 rounded-xl border p-4">
          <div className="flex flex-wrap items-center gap-3">
            {item.status === 'pending' && <input type="checkbox" aria-label={t('select')} checked={selected.includes(key)} disabled={!ready || (!selected.includes(key) && selected.length >= 200)} onChange={event => setSelected(current => event.target.checked ? [...current, key] : current.filter(value => value !== key))} />}
            <h2 className="font-semibold">{intl.formatMessage({ id: `migrationConfirm.category.${item.category}`, defaultMessage: t('unknownCategory') })}</h2>
            <span className="text-sm">{t(`status.${item.status}`)}</span>
            {item.high_risk && <span className="text-sm font-medium text-destructive">{t('highRisk')}</span>}
          </div>
          <dl className="space-y-3">{item.fields.map(field => <div key={field.name}>
            <dt className="text-sm font-medium break-all">{intl.formatMessage({ id: `migrationConfirm.field.${field.name}`, defaultMessage: t('field.unknown') })}</dt>
            <dd className="mt-1 max-h-80 overflow-auto whitespace-pre-wrap break-all rounded-md bg-muted p-3 text-sm">{field.value.kind === 'text' ? field.value.value : <>{t('secretMasked')}{item.rotation_pending && ` — ${t('rotationRequired')}`}</>}</dd>
          </div>)}</dl>
          {!item.apply_available && item.status === 'pending' && <p className="text-sm text-muted-foreground">{t('itemUnavailable')}</p>}
          <div className="flex flex-wrap gap-2">
            {item.status === 'pending' && <button className={buttonClass} disabled={!canAct || !item.apply_available} onClick={() => void decide('approve', [item])}>{t('approve')}</button>}
            {['failed', 'approved', 'applying'].includes(item.status) && <button className={buttonClass} disabled={!canAct || !item.apply_available} onClick={() => void decide('retry', [item])}>{t('retry')}</button>}
            {['pending', 'approved', 'applying', 'failed', 'rejecting'].includes(item.status) && <button className={buttonClass} disabled={!canAct} onClick={() => void decide('reject', [item])}>{t('reject')}</button>}
          </div>
          {item.category === 'configuration_section' && item.status === 'applied' && item.rotation_pending && <form className="flex flex-wrap items-end gap-3" onSubmit={event => { event.preventDefault(); void rotateSecret(item); }}>
            <label><span className="mb-1 block text-sm">{t('newSecret')}</span><input type="password" autoComplete="new-password" maxLength={65536} required value={secrets[key] ?? ''} onChange={event => setSecrets(current => ({ ...current, [key]: event.target.value }))} className="rounded-md border bg-background p-2" /></label>
            <button className={buttonClass} type="submit" disabled={!canAct}>{t('rotateSecret')}</button>
          </form>}
        </section>;
      })}
    </>}
  </div>;
}
