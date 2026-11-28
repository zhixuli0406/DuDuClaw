import { useCallback, useEffect, useRef, useState, useId, type FormEvent } from 'react';
import { useIntl } from 'react-intl';
import { api, type MemberKeyCreated, type MemberKeyInfo, type MemberKeyPolicy, type MemberKeyPolicyResult, type MemberKeyPolicyUpdate, type MemberKeyPermissions, type MemberKeyScope } from '@/lib/api';
import { Link } from 'react-router';
import { useUserNames } from '@/lib/use-user-names';
import { RiskConfirmDialog } from '@/components/settings/controls';
import { useAuthStore } from '@/stores/auth-store';
import {
  Badge, Checkbox, Button, Card, Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle,
  Input, Table, TableBody, TableCell, TableHead, TableHeader, TableRow,
} from '@/components/mds';
import { formatTokenDate, formatTokenLastUsed, RevokeConfirmDialog } from './shared';

const ERROR_CODES = ['member_keys_disabled', 'member_keys_browser_required', 'member_key_invalid_name',
  'member_key_history_limit_reached', 'member_key_reauth_rate_limited', 'member_key_invalid_expiry', 'member_key_admin_forbidden', 'member_key_limit_reached', 'member_key_step_up_required', 'member_key_conversation_not_allowed', 'member_key_scope_not_allowed', 'member_key_invalid_scopes', 'member_key_cannot_widen'] as const;
function errorCode(error: unknown): string | undefined {
  return error && typeof error === 'object' && 'code' in error && typeof error.code === 'string' ? error.code : undefined;
}

const SCOPES: MemberKeyScope[] = ['tasks:read', 'agents:status', 'odoo:read', 'odoo:write'];
const EMPTY_POLICY: MemberKeyPolicy = { allow_conversation_content: false, allow_mcp_scopes: [] };
const EMPTY_PERMISSIONS: MemberKeyPermissions = { include_conversations: false, mcp_scopes: [] };

function KeyCheckbox({ label, checked, disabled, onChange }: { label: string; checked: boolean; disabled: boolean; onChange: (checked: boolean) => void }) {
  const id = useId();
  return <div className="flex items-center gap-2"><Checkbox id={id} checked={checked} disabled={disabled} onCheckedChange={onChange} /><label htmlFor={id}>{label}</label></div>;
}

/** Only administrators request the member directory. */
function AdminMemberKeysSection() {
  const { nameOf, failed } = useUserNames();
  const intl = useIntl();
  const ownerName = (member: MemberKeyInfo) => {
    const name = failed ? '' : nameOf(member.user_id);
    const missing = name === intl.formatMessage({ id: 'userNames.removed' });
    return (!missing && name) || member.user_email || name || '—';
  };
  return <MemberKeysContent ownerName={ownerName} />;
}

export function MemberKeysSection() {
  const admin = useAuthStore(s => s.user?.role === 'admin');
  return admin ? <AdminMemberKeysSection /> : <MemberKeysContent />;
}

/** Credentials stay in dialog state only; no local storage, URL, or list cache. */
function MemberKeysContent({ ownerName }: { ownerName?: (member: MemberKeyInfo) => string }) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id: `memberKeys.${id}` });
  const admin = useAuthStore(s => s.user?.role === 'admin');
  const [policy, setPolicy] = useState<MemberKeyPolicy>(EMPTY_POLICY);
  const [adminPolicy, setAdminPolicy] = useState<MemberKeyPolicyResult | null>(null);
  const [odooConnected, setOdooConnected] = useState(false);
  const [permissions, setPermissions] = useState<MemberKeyPermissions>(EMPTY_PERMISSIONS);
  const [narrowing, setNarrowing] = useState<MemberKeyInfo | null>(null);
  const [pendingPolicy, setPendingPolicy] = useState<MemberKeyPolicyUpdate | null>(null);
  const [keys, setKeys] = useState<MemberKeyInfo[]>([]);
  const [enabled, setEnabled] = useState(false);
  const [loaded, setLoaded] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const [name, setName] = useState('');
  const [days, setDays] = useState(90);
  const [code, setCode] = useState('');
  const [password, setPassword] = useState('');
  const [reauthentication, setReauthentication] = useState<'password' | 'mfa' | null>(null);
  const [nextCursor, setNextCursor] = useState<string | null>(null);
  const [created, setCreated] = useState<MemberKeyCreated | null>(null);
  const [copied, setCopied] = useState(false);
  const [revoking, setRevoking] = useState<MemberKeyInfo | null>(null);
  const generation = useRef(0);
  const message = (err: unknown) => {
    const code = errorCode(err);
    return t(code && ERROR_CODES.some(known => known === code) ? `error.${code}` : 'error.generic');
  };
  const load = useCallback(async (cursor?: string) => {
    const current = ++generation.current;
    try {
      if (admin) {
        const [config, policyResult] = await Promise.all([
          api.system.config(),
          api.memberKeys.policyGet().catch(() => ({ readable: false as const, message: intl.formatMessage({ id: 'memberKeys.error.generic' }) })),
        ]);
        if (current !== generation.current) return;
        setAdminPolicy(policyResult);
        if (!config.member_api_keys_enabled) {
          setEnabled(false); setKeys([]); setLoaded(true); return;
        }
      }
      const [result, odoo] = await Promise.all([
        admin ? api.memberKeys.adminList(cursor).then(result => ({ ...result, policy: EMPTY_POLICY })) : api.memberKeys.list(cursor),
        admin ? Promise.resolve(null) : api.memberKeys.odooStatus().catch(() => null),
      ]);
      if (current !== generation.current) return;
      if (!Array.isArray(result.keys)) throw new Error('Invalid key list');
      setPolicy(result.policy ?? EMPTY_POLICY);
      setOdooConnected(odoo?.connected === true);
      setNextCursor(result.next_cursor ?? null);
      setReauthentication('reauthentication' in result && (result.reauthentication === 'mfa' || result.reauthentication === 'password') ? result.reauthentication : null);
      setKeys(result.keys); setEnabled(true); setLoaded(true); setError(null);
    } catch (err) {
      if (current !== generation.current) return;
      setLoaded(true); setKeys([]); setEnabled(false); setNextCursor(null); setReauthentication(null);
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
    if (busy || !reauthentication) return;
    setBusy(true); setError(null);
    try {
      const result = await api.memberKeys.create(name.trim(), days, reauthentication === 'mfa' ? { code: code.trim() } : { password }, permissions);
      setCreated(result); setCopied(false); setCreating(false); setName('');
      await load();
    } catch (err) { setError(message(err)); }
    finally { setCode(''); setPassword(''); setBusy(false); }
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
  const applyNarrow = async () => {
    if (!narrowing || busy) return;
    setBusy(true); setError(null);
    try {
      await (admin ? api.memberKeys.adminNarrow(narrowing.id, permissions) : api.memberKeys.narrow(narrowing.id, permissions));
      setNarrowing(null); await load();
    } catch (err) { setError(message(err)); }
    finally { setBusy(false); }
  };
  const savePolicy = async (update: MemberKeyPolicyUpdate) => {
    if (busy || !adminPolicy?.readable) return;
    setBusy(true); setError(null);
    try {
      await api.memberKeys.policySet(update);
      setPendingPolicy(null); await load();
    } catch (err) { setError(message(err)); }
    finally { setBusy(false); }
  };
  const changePolicy = (update: MemberKeyPolicyUpdate, widening: boolean) => {
    setError(null);
    if (widening) setPendingPolicy(update);
    else void savePolicy(update);
  };
  const scopeLabel = (scope: MemberKeyScope, forAdmin = false) => t(`${forAdmin ? 'adminScope' : 'toolScope'}.${scope}`);
  const permissionFields = (original?: MemberKeyInfo) => <div className="grid gap-4 text-sm">
    <div className="grid gap-1">
      <KeyCheckbox label={t('includeConversations')} checked={permissions.include_conversations}
        disabled={busy || (original ? !original.include_conversations : !policy.allow_conversation_content)}
        onChange={checked => setPermissions(p => ({ ...p, include_conversations: checked }))} />
      {!original && <>
        <p className="text-muted-foreground">{t('conversationHelp')}</p>
        {!policy.allow_conversation_content && <p className="text-muted-foreground">{t('notAllowed')}</p>}
      </>}
    </div>
    <fieldset className="grid gap-2">
      <legend className="mb-2 font-medium">{t('toolsTitle')}</legend>
      {!original && policy.allow_mcp_scopes.length === 0 ? <p>{t('noScopes')}</p> : SCOPES.map(scope => {
        const allowed = original ? original.mcp_scopes?.includes(scope) : policy.allow_mcp_scopes.includes(scope);
        const needsOdoo = !original && scope.startsWith('odoo:') && !odooConnected;
        return <div key={scope} className="grid gap-1">
          <KeyCheckbox label={scopeLabel(scope)} checked={permissions.mcp_scopes.includes(scope)} disabled={busy || !allowed || needsOdoo}
            onChange={checked => setPermissions(p => ({ ...p, mcp_scopes: checked ? [...p.mcp_scopes, scope] : p.mcp_scopes.filter(s => s !== scope) }))} />
          {!original && !allowed && <p className="text-muted-foreground">{t('notAllowed')}</p>}
          {needsOdoo && <Link className="text-primary underline" to="/me/connections">{t('connectOdoo')}</Link>}
        </div>;
      })}
    </fieldset>
    <p className="text-muted-foreground">{t(original ? 'narrowNote' : 'toolNote')}</p>
  </div>;
  const copy = async () => {
    if (!created) return;
    try { await navigator.clipboard.writeText(created.secret); setCopied(true); }
    catch { setError(t('copyFailed')); }
  };
  return <Card className="gap-4 p-4">
    <div className="flex flex-wrap items-center justify-between gap-3">
      <h2 className="text-base font-medium">{t(admin ? 'adminTitle' : 'title')}</h2>
      {admin && <Button variant="outline" disabled={busy || !loaded} onClick={() => void toggle()}>{t(enabled ? 'disable' : 'enable')}</Button>}
      {!admin && enabled && <Button disabled={busy || !reauthentication || keys.filter(key => key.active).length >= 5} onClick={() => { setError(null); setPermissions(EMPTY_PERMISSIONS); setCreating(true); }}>{t('create')}</Button>}
    </div>
    <p role="note" className="text-sm text-muted-foreground">{t('warning')}</p>
    <p className="text-sm text-muted-foreground">{t('scope')}</p>
    {!enabled && loaded && <p role="status" className="text-sm">{t('disabled')}</p>}
    {admin && <p className="text-sm text-muted-foreground">{t('adminNote')}</p>}
    {admin && <div className="grid gap-4 rounded-md border border-border p-4 text-sm">
      {adminPolicy && !adminPolicy.readable && <p role="alert">{adminPolicy.message}</p>}
      <div className="grid gap-1">
        <KeyCheckbox label={t('policyConversation')} checked={adminPolicy?.readable ? adminPolicy.allow_conversation_content : false}
          disabled={busy || !adminPolicy?.readable} onChange={checked => changePolicy({ allow_conversation_content: checked }, checked)} />
        <p className="text-muted-foreground">{t('policyConversationNote')}</p>
      </div>
      <fieldset className="grid gap-2">
        <legend className="mb-2 font-medium">{t('policyScopes')}</legend>
        {SCOPES.map(scope => <KeyCheckbox key={scope} label={scopeLabel(scope, true)}
          checked={adminPolicy?.readable ? adminPolicy.allow_mcp_scopes.includes(scope) : false}
          disabled={busy || !adminPolicy?.readable || !adminPolicy.available_mcp_scopes.includes(scope)}
          onChange={checked => { if (adminPolicy?.readable) changePolicy({ allow_mcp_scopes: checked ? [...adminPolicy.allow_mcp_scopes, scope] : adminPolicy.allow_mcp_scopes.filter(s => s !== scope) }, checked); }} />)}
        <p className="text-muted-foreground">{t('policyScopesNote')}</p>
      </fieldset>
    </div>}
    {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
    <Button variant="outline" size="sm" disabled={busy} onClick={() => void load()}>{t('refresh')}</Button>
    {enabled && nextCursor && <Button variant="outline" size="sm" disabled={busy} onClick={() => void load(nextCursor)}>{t('nextPage')}</Button>}
    {enabled && <div className="overflow-x-auto"><Table>
      <TableHeader><TableRow>
        {admin && <TableHead>{t('owner')}</TableHead>}
        {['name', 'contentScopes', 'created', 'expires', 'lastUsed', 'status', 'actions'].map(id => <TableHead key={id}>{t(id)}</TableHead>)}
      </TableRow></TableHeader>
      <TableBody>{keys.map(key => <TableRow key={key.id}>
        {admin && <TableCell>{ownerName?.(key) ?? key.user_email ?? '—'}</TableCell>}
        <TableCell>{key.name}</TableCell>
        <TableCell><div className="flex flex-wrap gap-1">
          {key.include_conversations && <Badge variant="secondary">{t(key.conversations_migrated ? 'conversationsMigrated' : 'conversations')}</Badge>}
          {SCOPES.filter(scope => key.mcp_scopes?.includes(scope)).map(scope => <Badge key={scope} variant="outline">{scopeLabel(scope)}</Badge>)}
        </div></TableCell>
        <TableCell>{formatTokenDate(intl, key.created_at)}</TableCell>
        <TableCell>{formatTokenDate(intl, key.expires_at)}</TableCell>
        <TableCell>{formatTokenLastUsed(intl, key.last_used_at)}</TableCell>
        <TableCell>{t(key.active ? 'active' : 'inactive')}</TableCell>
        <TableCell><div className="flex gap-2">{key.active && <Button variant="outline" size="sm" disabled={busy} onClick={() => {
          setError(null); setPermissions({ include_conversations: key.include_conversations === true, mcp_scopes: key.mcp_scopes ?? [] }); setNarrowing(key);
        }}>{t('narrow')}</Button>}{!key.revoked_at && <Button variant="destructive" size="sm" disabled={busy} onClick={() => { setError(null); setRevoking(key); }}>{t('revoke')}</Button>}</div></TableCell>
      </TableRow>)}</TableBody>
    </Table></div>}
    {creating && <Dialog open onOpenChange={open => { if (!open && !busy) { setCreating(false); setCode(''); setPassword(''); } }}>
      <DialogContent><form className="grid gap-4" onSubmit={event => void create(event)}>
        <DialogHeader><DialogTitle>{t('create')}</DialogTitle><DialogDescription>{t('scope')}</DialogDescription></DialogHeader>
        <label className="grid gap-1 text-sm">{t('name')}<Input value={name} maxLength={60} autoComplete="off" onChange={e => setName(e.target.value)} required /></label>
        <label className="grid gap-1 text-sm">{t('expiry')}<select className="rounded-md border border-border bg-background p-2" value={days} onChange={e => setDays(Number(e.target.value))}>
          {[30, 90, 365].map(value => <option key={value} value={value}>{intl.formatMessage({ id: 'memberKeys.days' }, { days: value })}</option>)}
        </select></label>
        {permissionFields()}
        {reauthentication === 'mfa'
          ? <label className="grid gap-1 text-sm">{t('code')}<Input value={code} autoComplete="one-time-code" onChange={e => setCode(e.target.value)} required /></label>
          : <label className="grid gap-1 text-sm">{t('password')}<Input type="password" value={password} autoComplete="current-password" onChange={e => setPassword(e.target.value)} required /></label>}
        {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
        <DialogFooter><Button type="submit" disabled={busy || !name.trim() || !reauthentication || !(reauthentication === 'mfa' ? code.trim() : password)}>{t('create')}</Button></DialogFooter>
      </form></DialogContent>
    </Dialog>}
    {narrowing && <Dialog open onOpenChange={open => { if (!open && !busy) setNarrowing(null); }}>
      <DialogContent>
        <DialogHeader><DialogTitle>{intl.formatMessage({ id: 'memberKeys.narrowTitle' }, { name: narrowing.name })}</DialogTitle><DialogDescription>{t('contentScopes')}</DialogDescription></DialogHeader>
        {permissionFields(narrowing)}
        {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
        <DialogFooter><Button variant="outline" disabled={busy} onClick={() => setNarrowing(null)}>{intl.formatMessage({ id: 'common.cancel' })}</Button><Button disabled={busy} onClick={() => void applyNarrow()}>{t('apply')}</Button></DialogFooter>
      </DialogContent>
    </Dialog>}
    {pendingPolicy && <RiskConfirmDialog open title={t(pendingPolicy.allow_conversation_content ? 'openConversationTitle' : 'openScopesTitle')}
      body={(adminPolicy?.readable && (pendingPolicy.allow_conversation_content ? adminPolicy.risk.conversation : adminPolicy.risk.mcp_scopes)?.trim()) ||
        (pendingPolicy.allow_conversation_content ? t('conversationRisk') : intl.formatMessage({ id: 'memberKeys.scopesRisk' }, { scope: SCOPES.filter(scope => pendingPolicy.allow_mcp_scopes?.includes(scope) && !(adminPolicy?.readable && adminPolicy.allow_mcp_scopes.includes(scope))).map(scope => scopeLabel(scope, true)).join('、') }))}
      acknowledgeLabel={t('acknowledge')} auditNote={t('auditNote')} confirmLabel={t('open')} busy={busy} error={error}
      onClose={() => { if (!busy) setPendingPolicy(null); }} onConfirm={() => void savePolicy({ ...pendingPolicy, acknowledge_risk: true })} />}
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
