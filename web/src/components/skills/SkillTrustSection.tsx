import { useCallback, useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { api, type SkillTrustPin, type SkillTrustSettings } from '@/lib/api';
import { settingsRefusal } from '@/lib/settings-refusal';
import { useUserNames } from '@/lib/use-user-names';
import { useAuthStore } from '@/stores/auth-store';
import { useAgentsStore } from '@/stores/agents-store';
import {
  Button, Card, Dialog, DialogContent, DialogDescription, DialogFooter,
  DialogHeader, DialogTitle, Input, Textarea, useErrorMessage,
} from '@/components/mds';

/** Mount admin-only data hooks only after the role gate. */
export function SkillTrustSection() {
  const admin = useAuthStore(s => s.user?.role === 'admin');
  return admin ? <AdminSkillTrustSection /> : null;
}

function AdminSkillTrustSection() {
  const intl = useIntl();
  const t = (key: string, values?: Record<string, string | number>) => intl.formatMessage({ id: `skillTrust.${key}` }, values);
  const errorText = useErrorMessage();
  const { nameOf } = useUserNames();
  const agents = useAgentsStore(s => s.agents);
  const fetchAgents = useAgentsStore(s => s.fetchAgents);
  const [data, setData] = useState<SkillTrustSettings | null>(null);
  const [days, setDays] = useState('7');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const [publisher, setPublisher] = useState('');
  const [publicKey, setPublicKey] = useState('');
  const [acknowledged, setAcknowledged] = useState(false);
  const [password, setPassword] = useState('');
  const [code, setCode] = useState('');
  const [removing, setRemoving] = useState<SkillTrustPin | null>(null);
  const [disableSkills, setDisableSkills] = useState(true);
  const load = useCallback(async () => {
    const result = await api.skillTrust.get();
    setData(result);
    setDays(String(result.skill_ttl_days ?? 7));
  }, []);
  useEffect(() => {
    let active = true;
    void api.skillTrust.get().then(result => {
      if (active) { setData(result); setDays(String(result.skill_ttl_days ?? 7)); }
    }).catch(err => { if (active) setError(settingsRefusal(err, errorText(err))); });
    if (!useAgentsStore.getState().loaded) void fetchAgents();
    return () => { active = false; };
  }, [errorText, fetchAgents]);

  const clearCredentials = () => { setPassword(''); setCode(''); };
  const closeAdd = () => { if (!busy) { setCreating(false); clearCredentials(); setError(null); } };
  const add = async () => {
    if (busy || !acknowledged || !publisher.trim() || !publicKey.trim()) return;
    setError(null); setNotice(null);
    if (publisher.trim().toLowerCase() === 'duduclaw') {
      setError(t('officialRefused')); clearCredentials(); return;
    }
    setBusy(true);
    try {
      await api.skillTrust.pin({ publisher: publisher.trim(), public_key: publicKey.trim(), acknowledge_risk: true, password, code });
      setCreating(false);
      await load();
    } catch (err) { setError(settingsRefusal(err, errorText(err))); }
    finally { clearCredentials(); setBusy(false); }
  };
  const remove = async () => {
    if (busy || !removing) return;
    setBusy(true); setError(null); setNotice(null);
    try {
      const result = await api.skillTrust.unpin({ publisher: removing.publisher, key_id: removing.key_id, disable_skills: disableSkills });
      setRemoving(null);
      if (result.disabled > 0) setNotice(t('removedDisabled', { count: result.disabled }));
      await load();
    } catch (err) { setError(settingsRefusal(err, errorText(err))); }
    finally { setBusy(false); }
  };
  const saveDays = async () => {
    if (busy) return;
    setError(null); setNotice(null);
    const value = Number(days);
    if (!Number.isInteger(value) || value < 1 || value > 30) { setError(t('ttlInvalid')); return; }
    setBusy(true);
    try {
      await api.system.updateConfig({ approval: { skill_ttl_days: value } });
      setNotice(t('saved'));
    } catch (err) { setError(settingsRefusal(err, errorText(err))); }
    finally { setBusy(false); }
  };
  const date = (value: string, time = false) => {
    const parsed = new Date(value);
    return Number.isNaN(parsed.getTime()) ? t('unknownDate') : intl.formatDate(parsed, {
      year: 'numeric', month: 'numeric', day: 'numeric', ...(time ? { hour: '2-digit', minute: '2-digit' } as const : {}),
    });
  };
  const alert = error && <p role="alert" className="text-sm text-destructive">{error}</p>;
  return <Card className="mt-6 gap-4 p-4">
    <div className="space-y-1"><h2 className="text-base font-medium">{t('title')}</h2><p className="text-sm text-muted-foreground">{t('description')}</p></div>
    {!creating && !removing && alert}
    {notice && <p role="status" className="text-sm">{notice}</p>}
    {!data ? <div className="flex items-center gap-3"><p role="status">{t('loading')}</p>{error && <Button variant="outline" disabled={busy} onClick={() => {
      setBusy(true); setError(null);
      void load().catch(err => setError(settingsRefusal(err, errorText(err)))).finally(() => setBusy(false));
    }}>{t('retry')}</Button>}</div> : <>
      {data.auto_approve.length === 0 ? <p className="text-sm text-muted-foreground">{t('empty')}</p> : <ul className="divide-y divide-border rounded-lg border border-border">
        {data.auto_approve.map(pin => <li key={`${pin.publisher}:${pin.key_id}`} className="flex flex-wrap items-center justify-between gap-3 p-3">
          <div className="min-w-0 space-y-1 break-words text-sm">
            <p className="font-medium">{pin.publisher}</p>
            <p className="font-mono text-xs">{t('fingerprint', { fingerprint: pin.key_id.toUpperCase().match(/.{1,4}/g)?.join(' ') ?? '' })}</p>
            <p className="text-muted-foreground">{t('configured', { name: nameOf(pin.added_by), date: date(pin.added_at) })}</p>
            {!pin.in_trust_store && <p className="text-warning-foreground">{t('missingKey')}</p>}
            {pin.conflicting && <p className="text-warning-foreground">{t('conflict')}</p>}
          </div>
          <Button variant="outline" disabled={busy} onClick={() => { setRemoving(pin); setDisableSkills(true); setError(null); setNotice(null); }}>{t('remove')}</Button>
        </li>)}
      </ul>}
      <div><Button variant="outline" disabled={busy} onClick={() => {
        setCreating(true); setPublisher(''); setPublicKey(''); setAcknowledged(false); clearCredentials(); setError(null); setNotice(null);
      }}>{t('addSource')}</Button></div>
      {data.recent_auto_approvals.length > 0 && <section className="space-y-2">
        <h3 className="text-sm font-medium">{t('recent')}</h3>
        <ul className="divide-y divide-border">{data.recent_auto_approvals.slice(0, 20).map(entry => <li key={entry.id} className="space-y-2 py-2 text-sm">
          <time dateTime={entry.decided_at} className="text-xs tabular-nums text-muted-foreground">{date(entry.decided_at, true)}</time>
          {entry.skills.map((skill, index) => <div key={`${skill.agent}:${skill.digest}:${index}`} className="flex flex-wrap justify-between gap-2 break-words">
            <span>{skill.name}</span><span className="text-muted-foreground">{agents.find(agent => agent.name === skill.agent)?.display_name.trim() || t('unknownAgent')}</span>
          </div>)}
        </li>)}</ul>
      </section>}
      <section className="space-y-2 border-t border-border pt-4">
        <label className="block text-sm font-medium" htmlFor="skill-trust-ttl">{t('ttl')}</label>
        <p id="skill-trust-ttl-help" className="text-sm text-muted-foreground">{t('ttlHelp')}</p>
        <div className="flex flex-wrap items-center gap-2"><Input id="skill-trust-ttl" aria-describedby="skill-trust-ttl-help skill-trust-ttl-note" className="w-24" type="number" min={1} max={30} step={1} value={days} disabled={busy} onChange={e => setDays(e.target.value)} /><span className="text-sm">{t('days')}</span><Button disabled={busy} onClick={() => void saveDays()}>{t('save')}</Button></div>
        <p id="skill-trust-ttl-note" className="text-xs text-muted-foreground">{t('ttlNote')}</p>
      </section>
    </>}
    {creating && <Dialog open onOpenChange={open => { if (!open) closeAdd(); }}>
      <DialogContent className="max-h-[90dvh] overflow-y-auto"><form className="grid gap-4" onSubmit={e => { e.preventDefault(); void add(); }}>
        <DialogHeader><DialogTitle>{t('addTitle')}</DialogTitle><DialogDescription>{t('risk')}</DialogDescription></DialogHeader>
        <label className="grid gap-1 text-sm">{t('publisher')}<Input value={publisher} autoComplete="off" disabled={busy} onChange={e => setPublisher(e.target.value)} /></label>
        <label className="grid gap-1 text-sm">{t('publicKey')}<Textarea value={publicKey} autoComplete="off" disabled={busy} onChange={e => setPublicKey(e.target.value)} /></label>
        <label className="flex items-start gap-2 text-sm"><input type="checkbox" checked={acknowledged} disabled={busy} onChange={e => setAcknowledged(e.target.checked)} />{t('acknowledge')}</label>
        <label className="grid gap-1 text-sm">{t('password')}<Input type="password" value={password} autoComplete="current-password" disabled={busy} onChange={e => setPassword(e.target.value)} /></label>
        <label className="grid gap-1 text-sm">{t('code')}<Input value={code} autoComplete="one-time-code" disabled={busy} onChange={e => setCode(e.target.value)} /></label>
        {alert}
        <DialogFooter><Button type="button" variant="outline" disabled={busy} onClick={closeAdd}>{t('cancel')}</Button><Button type="submit" disabled={busy || !acknowledged || !publisher.trim() || !publicKey.trim()}>{t('add')}</Button></DialogFooter>
      </form></DialogContent>
    </Dialog>}
    {removing && <Dialog open onOpenChange={open => { if (!open && !busy) { setRemoving(null); setError(null); } }}>
      <DialogContent><DialogHeader><DialogTitle>{t('removeTitle', { publisher: removing.publisher })}</DialogTitle><DialogDescription>{t('removeBody')}</DialogDescription></DialogHeader>
        <label className="flex items-start gap-2 text-sm"><input type="checkbox" checked={disableSkills} disabled={busy} onChange={e => setDisableSkills(e.target.checked)} />{t('disableSkills')}</label>
        {alert}
        <DialogFooter><Button variant="outline" disabled={busy} onClick={() => { setRemoving(null); setError(null); }}>{t('cancel')}</Button><Button variant="destructive" disabled={busy} onClick={() => void remove()}>{t('remove')}</Button></DialogFooter>
      </DialogContent>
    </Dialog>}
  </Card>;
}
