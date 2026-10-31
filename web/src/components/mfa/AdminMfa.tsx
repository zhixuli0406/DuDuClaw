import { useCallback, useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { Button, Card, Input } from '@/components/mds';
import { mfaApi, mfaErrorId, type MfaOverview } from '@/lib/mfa-api';
import { api } from '@/lib/api';
import { useAuthStore } from '@/stores/auth-store';
import { useSystemStore } from '@/stores/system-store';

export function AdminMfa() {
  const user = useAuthStore(s => s.user);
  const edition = useSystemStore(s => s.status?.edition_profile);
  if (user?.role !== 'admin' || !edition || edition === 'personal') return null;
  return <AdminMfaPanel actor={user.id} />;
}

export function AdminMfaPanel({ actor }: { actor: string }) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const systemDegraded = useSystemStore(s => s.status?.mfa_policy?.degraded);
  const [overview, setOverview] = useState<MfaOverview | null>(null);
  const [error, setError] = useState('');
  const [notice, setNotice] = useState('');
  const [busy, setBusy] = useState(false);
  const [target, setTarget] = useState<string | null>(null);
  const [action, setAction] = useState<'reset' | 'issue' | null>(null);
  const [factor, setFactor] = useState('');
  const [reference, setReference] = useState('');
  const [issued, setIssued] = useState<{ enrollment_code: string; expires_in: number } | null>(null);
  const [confirmPolicy, setConfirmPolicy] = useState(false);
  const [typed, setTyped] = useState('');
  const load = useCallback(async () => { setOverview(await mfaApi.overview()); }, []);
  useEffect(() => {
    let active = true;
    void mfaApi.overview().then(value => { if (active) setOverview(value); })
      .catch(error => { if (active) setError(t(mfaErrorId(error))); });
    return () => { active = false; };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  const run = async (work: () => Promise<unknown>) => {
    setBusy(true); setError(''); setNotice('');
    try { await work(); }
    catch (error) { setError(t(mfaErrorId(error))); }
    finally { setBusy(false); }
  };
  const blocked = overview?.users.filter(u => !u.enabled).length ?? 0;
  const degraded = systemDegraded || overview?.policy.degraded;
  return <Card className="space-y-4 p-4">
    <h2 className="font-medium">{t('mfa.admin.title')}</h2>
    {error && <p role="alert" className="text-destructive">{error}</p>}
    {notice && <p role="status">{notice}</p>}
    {degraded && <p role="alert" className="text-destructive">{t('mfa.degraded')}</p>}
    {!overview && !error && <p role="status">{t('mfa.working')}</p>}
    <Button variant="outline" disabled={busy} onClick={() => void run(load)}>{t('mfa.refresh')}</Button>
    {overview && <>
      <p>{intl.formatMessage({ id: 'mfa.admin.blocked' }, { count: blocked })}</p>
      {overview.policy.enforced_at != null && <p className="text-sm text-muted-foreground">{t('mfa.enforcedAt')}: {intl.formatDate(overview.policy.enforced_at / 1000, { dateStyle: 'medium', timeStyle: 'short' })}</p>}
      {blocked > 0 && <p role="alert" className="text-sm text-warning">{t('mfa.admin.warning')}</p>}
      <p className="text-sm text-muted-foreground">{t('mfa.admin.review')}</p>
      <Button role="switch" aria-checked={overview.enforced === true} variant="outline" disabled={busy || degraded || overview.enforced === null} onClick={() => { setConfirmPolicy(true); setTyped(''); }}>{t(overview.enforced ? 'mfa.policy.disable' : 'mfa.policy.enable')}</Button>
      {confirmPolicy && <form className="space-y-3 rounded-lg border p-3" onSubmit={e => {
        e.preventDefault();
        if (typed !== t('mfa.policy.phrase')) return;
        void run(async () => {
          await api.system.updateConfig({ 'auth.mfa_enforced': !overview.enforced });
          setConfirmPolicy(false); setTyped(''); await load(); setNotice(t('mfa.updated'));
        });
      }}>
        <label className="block space-y-2">{intl.formatMessage({ id: 'mfa.policy.confirm' }, { phrase: t('mfa.policy.phrase') })}<Input value={typed} onChange={e => setTyped(e.target.value)} autoComplete="off" /></label>
        <Button type="submit" disabled={busy || degraded || typed !== t('mfa.policy.phrase')}>{t('mfa.confirm')}</Button>
        <Button type="button" variant="outline" disabled={busy} onClick={() => setConfirmPolicy(false)}>{t('mfa.cancel')}</Button>
      </form>}
      <ul className="divide-y">
        {overview.users.map(row => <li key={row.user_id} className="space-y-2 py-3">
          <p className="break-all text-sm font-medium">{row.email} · {t(row.enabled ? 'mfa.enabled' : 'mfa.notEnrolled')}</p>
          {row.channel_bindings.length === 0 && <p className="text-sm text-muted-foreground">{t('mfa.noBindings')}</p>}
          {row.channel_bindings.map(binding => <p className="break-all text-xs text-muted-foreground" key={`${binding.channel}:${binding.channel_user_id}`}>
            {binding.channel} · {t('mfa.boundAt')}: {binding.verified && binding.verified_at ? intl.formatDate(binding.verified_at / 1000, { dateStyle: 'medium', timeStyle: 'short' }) : t('mfa.unknownTime')}
          </p>)}
          <div className="flex flex-wrap gap-2">
            <Button variant="destructive" disabled={busy || row.user_id === actor || !row.enabled} title={row.user_id === actor ? t('mfa.selfReset') : undefined} onClick={() => { setTarget(row.user_id); setAction('reset'); setFactor(''); setIssued(null); }}>{t('mfa.reset')}</Button>
            <Button variant="outline" disabled={busy || row.enabled || row.user_id === actor} onClick={() => { setTarget(row.user_id); setAction('issue'); setReference(''); setIssued(null); }}>{t('mfa.issue')}</Button>
          </div>
          {target === row.user_id && action && <form className="space-y-3" onSubmit={e => {
            e.preventDefault();
            if (action === 'reset') {
              if (target === actor) return;
              void run(async () => { await mfaApi.reset(actor, target, factor); setFactor(''); setAction(null); await load(); setNotice(t('mfa.updated')); });
            } else void run(async () => { setIssued(await mfaApi.issueCode(target, reference)); setReference(''); });
          }}>
            <label className="block space-y-2">{t(action === 'reset' ? 'mfa.freshFactor' : 'mfa.challengeRef')}
              <Input autoComplete="off" pattern={action === 'issue' ? '[0-9a-fA-F]{12}' : undefined} maxLength={action === 'issue' ? 12 : 128} value={action === 'reset' ? factor : reference} onChange={e => action === 'reset' ? setFactor(e.target.value) : setReference(e.target.value)} required />
            </label>
            <Button type="submit" disabled={busy || (action === 'reset' ? !factor.trim() : !reference.trim())}>{t('mfa.confirm')}</Button>
            <Button type="button" variant="outline" disabled={busy} onClick={() => { setAction(null); setIssued(null); setFactor(''); setReference(''); }}>{t('mfa.close')}</Button>
            {issued && <p role="status" className="break-all">{t('mfa.issued')}: <code>{issued.enrollment_code}</code> · {intl.formatMessage({ id: 'mfa.expires' }, { seconds: issued.expires_in })}</p>}
          </form>}
        </li>)}
      </ul>
    </>}
  </Card>;
}
