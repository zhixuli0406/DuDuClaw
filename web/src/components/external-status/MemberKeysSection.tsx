import { useCallback, useEffect, useRef, useState, type FormEvent } from 'react';
import { useIntl } from 'react-intl';
import { api, type MemberKeyCreated, type MemberKeyInfo } from '@/lib/api';
import { useAuthStore } from '@/stores/auth-store';
import {
  Button, Card, Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle,
  Input, Table, TableBody, TableCell, TableHead, TableHeader, TableRow,
} from '@/components/mds';
import { formatTokenDate, formatTokenLastUsed, RevokeConfirmDialog } from './shared';

const ERROR_CODES = ['member_keys_disabled', 'member_keys_browser_required', 'member_key_invalid_name',
  'member_key_invalid_expiry', 'member_key_admin_forbidden', 'member_key_limit_reached', 'member_key_step_up_required'] as const;
function errorCode(error: unknown): string | undefined {
  return error && typeof error === 'object' && 'code' in error && typeof error.code === 'string' ? error.code : undefined;
}

/** Credentials stay in dialog state only; no local storage, URL, or list cache. */
export function MemberKeysSection() {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id: `memberKeys.${id}` });
  const admin = useAuthStore(s => s.user?.role === 'admin');
  const [keys, setKeys] = useState<MemberKeyInfo[]>([]);
  const [enabled, setEnabled] = useState(false);
  const [loaded, setLoaded] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const [name, setName] = useState('');
  const [days, setDays] = useState(90);
  const [code, setCode] = useState('');
  const [created, setCreated] = useState<MemberKeyCreated | null>(null);
  const [copied, setCopied] = useState(false);
  const [revoking, setRevoking] = useState<MemberKeyInfo | null>(null);
  const generation = useRef(0);
  const message = (err: unknown) => {
    const code = errorCode(err);
    return t(code && ERROR_CODES.some(known => known === code) ? `error.${code}` : 'error.generic');
  };
  const load = useCallback(async () => {
    const current = ++generation.current;
    try {
      if (admin) {
        const config = await api.system.config();
        if (current !== generation.current) return;
        if (!config.member_api_keys_enabled) {
          setEnabled(false); setKeys([]); setLoaded(true); return;
        }
      }
      const result = await (admin ? api.memberKeys.adminList() : api.memberKeys.list());
      if (current !== generation.current) return;
      if (!Array.isArray(result.keys)) throw new Error('Invalid key list');
      setKeys(result.keys); setEnabled(true); setLoaded(true); setError(null);
    } catch (err) {
      if (current !== generation.current) return;
      setLoaded(true); setKeys([]); setEnabled(false);
      setError(errorCode(err) === 'member_keys_disabled' ? null : intl.formatMessage({ id: 'memberKeys.error.generic' }));
    }
  }, [admin, intl]);
  useEffect(() => { void load(); return () => { ++generation.current; }; }, [load]);
  const toggle = async () => {
    setBusy(true); setError(null);
    try { await api.system.updateConfig({ member_api_keys_enabled: !enabled }); await load(); }
    catch (err) { setError(message(err)); }
    finally { setBusy(false); }
  };
  const create = async (event: FormEvent) => {
    event.preventDefault();
    if (busy) return;
    setBusy(true); setError(null);
    try {
      const result = await api.memberKeys.create(name.trim(), days, code.trim() || undefined);
      setCreated(result); setCopied(false); setCreating(false); setName('');
      await load();
    } catch (err) { setError(message(err)); }
    finally { setCode(''); setBusy(false); }
  };
  const revoke = async () => {
    if (!revoking) return;
    setBusy(true); setError(null);
    try {
      await (admin ? api.memberKeys.adminRevoke(revoking.id) : api.memberKeys.revoke(revoking.id));
      setRevoking(null); await load();
    } catch (err) { setError(message(err)); }
    finally { setBusy(false); }
  };
  const copy = async () => {
    if (!created) return;
    try { await navigator.clipboard.writeText(created.secret); setCopied(true); }
    catch { setError(t('copyFailed')); }
  };
  return <Card className="gap-4 p-4">
    <div className="flex flex-wrap items-center justify-between gap-3">
      <h2 className="text-base font-medium">{t(admin ? 'adminTitle' : 'title')}</h2>
      {admin && <Button variant="outline" disabled={busy || !loaded} onClick={() => void toggle()}>{t(enabled ? 'disable' : 'enable')}</Button>}
      {!admin && enabled && <Button disabled={busy || keys.filter(key => key.active).length >= 5} onClick={() => { setError(null); setCreating(true); }}>{t('create')}</Button>}
    </div>
    <p role="note" className="text-sm text-muted-foreground">{t('warning')}</p>
    <p className="text-sm text-muted-foreground">{t('scope')}</p>
    {!enabled && loaded && <p role="status" className="text-sm">{t('disabled')}</p>}
    {admin && <p className="text-sm text-muted-foreground">{t('adminNote')}</p>}
    {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
    <Button variant="outline" size="sm" disabled={busy} onClick={() => void load()}>{t('refresh')}</Button>
    {enabled && <div className="overflow-x-auto"><Table>
      <TableHeader><TableRow>
        {admin && <TableHead>{t('owner')}</TableHead>}
        {['name', 'created', 'expires', 'lastUsed', 'status', 'revoke'].map(id => <TableHead key={id}>{t(id)}</TableHead>)}
      </TableRow></TableHeader>
      <TableBody>{keys.map(key => <TableRow key={key.id}>
        {admin && <TableCell>{key.user_email ?? key.user_id}</TableCell>}
        <TableCell>{key.name}</TableCell>
        <TableCell>{formatTokenDate(intl, key.created_at)}</TableCell>
        <TableCell>{formatTokenDate(intl, key.expires_at)}</TableCell>
        <TableCell>{formatTokenLastUsed(intl, key.last_used_at)}</TableCell>
        <TableCell>{t(key.active ? 'active' : 'inactive')}</TableCell>
        <TableCell>{!key.revoked_at && <Button variant="destructive" size="sm" disabled={busy} onClick={() => { setError(null); setRevoking(key); }}>{t('revoke')}</Button>}</TableCell>
      </TableRow>)}</TableBody>
    </Table></div>}
    {creating && <Dialog open onOpenChange={open => { if (!open && !busy) { setCreating(false); setCode(''); } }}>
      <DialogContent><form className="grid gap-4" onSubmit={event => void create(event)}>
        <DialogHeader><DialogTitle>{t('create')}</DialogTitle><DialogDescription>{t('scope')}</DialogDescription></DialogHeader>
        <label className="grid gap-1 text-sm">{t('name')}<Input value={name} maxLength={60} autoComplete="off" onChange={e => setName(e.target.value)} required /></label>
        <label className="grid gap-1 text-sm">{t('expiry')}<select className="rounded-md border border-border bg-background p-2" value={days} onChange={e => setDays(Number(e.target.value))}>
          {[30, 90, 365].map(value => <option key={value} value={value}>{intl.formatMessage({ id: 'memberKeys.days' }, { days: value })}</option>)}
        </select></label>
        <label className="grid gap-1 text-sm">{t('code')}<Input value={code} autoComplete="one-time-code" onChange={e => setCode(e.target.value)} /></label>
        {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
        <DialogFooter><Button type="submit" disabled={busy || !name.trim()}>{t('create')}</Button></DialogFooter>
      </form></DialogContent>
    </Dialog>}
    {created && <Dialog open onOpenChange={() => {}}><DialogContent showClose={false}>
      <DialogHeader><DialogTitle>{t('secretTitle')}</DialogTitle><DialogDescription>{t('secretNote')}</DialogDescription></DialogHeader>
      <Input readOnly value={created.secret} autoComplete="off" onFocus={e => e.currentTarget.select()} />
      <p className="text-sm">{t('warning')}</p>
      {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
      <DialogFooter><Button variant="outline" onClick={() => void copy()}>{t(copied ? 'copied' : 'copy')}</Button><Button onClick={() => { setCreated(null); setCopied(false); }}>{t('close')}</Button></DialogFooter>
    </DialogContent></Dialog>}
    {revoking && <RevokeConfirmDialog title={`${t('revoke')} ${revoking.name}`} body={t('revokeNote')} busy={busy} error={error}
      onCancel={() => setRevoking(null)} onConfirm={() => void revoke()} />}
  </Card>;
}
