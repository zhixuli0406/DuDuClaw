import { useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { Plus, Trash2, UserPlus } from 'lucide-react';
import { api, type UserInfo, type UserInviteResult } from '@/lib/api';
import { useAuthStore } from '@/stores/auth-store';
import { formatError } from '@/lib/toast';
import { dashboardBaseUrl } from '@/lib/dashboard-base';
import {
  ActorAvatar, Badge, BreadcrumbHeader, Button, Card, CardContent, CardDescription,
  CardHeader, CardTitle, Checkbox, Dialog, DialogContent, DialogHeader, DialogTitle,
  Input, Select, SelectContent, SelectItem, SelectTrigger, SelectValue, Spinner,
} from '@/components/mds';
import { InviteLinkDialog } from './InviteControls';
import { roleOptions, useDepartmentNames } from './dialog-common';

type Binding = { agent_name: string; access_level: 'operator' | 'viewer' };
type Member = {
  key: number; display_name: string; email: string; role: UserInfo['role']; department: string;
  bindings: Binding[]; invite: boolean;
  result?: { user?: UserInfo; invite?: UserInviteResult; error?: string };
};
type Staff = { name: string; display_name?: string };
const blankMember = (key: number): Member => ({
  key, display_name: '', email: '', role: 'employee', department: '',
  bindings: [{ agent_name: '', access_level: 'operator' }], invite: true,
});

const MEMBER_ERROR_MESSAGES = new Map<string, string>([
  ['member_invalid_name', 'members.error.name'],
  ['member_invalid_email', 'members.error.email'],
  ['member_invalid_department', 'members.error.department'],
  ['member_invalid_bindings', 'members.error.bindings'],
  ['member_invalid_primary', 'members.error.primary'],
  ['member_storage_unavailable', 'members.error.unavailable'],
  ['member_admin_required', 'members.error.admin'],
  ['member_email_exists', 'members.error.emailExists'],
  ['member_invite_unavailable', 'members.error.invitation'],
  ['member_invalid_input', 'members.error.input'],
  ['member_invite_url_required', 'members.error.invitationUrl'],
  ['member_agent_unavailable', 'members.error.staffUnavailable'],
  ['member_agent_restricted', 'members.error.staffRestricted'],
  ['member_agent_personal', 'members.error.staffPersonal'],
]);

/** One account, its staff access and primary preference are submitted together. */
export function AddMembersPage({ onBack, onCreated }: { onBack: () => void; onCreated: () => void }) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const userId = useAuthStore(s => s.user?.id);
  const admin = useAuthStore(s => s.user?.role === 'admin');
  const nextKey = useRef(1);
  const [members, setMembers] = useState<Member[]>([blankMember(0)]);
  const [staff, setStaff] = useState<Staff[] | null>(null);
  const [loadError, setLoadError] = useState('');
  const [loadRevision, setLoadRevision] = useState(0);
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);
  const [preview, setPreview] = useState<{ subject: string; html: string } | null>(null);
  const [previewBusy, setPreviewBusy] = useState(false);
  const [link, setLink] = useState<string | null>(null);
  const departments = useDepartmentNames();
  const roles = roleOptions(intl);

  useEffect(() => {
    setMembers([blankMember(0)]); nextKey.current = 1;
    setLink(null); setPreview(null); setError(''); setBusy(false); setPreviewBusy(false);
  }, [userId]);

  useEffect(() => {
    setStaff(null); setLoadError('');
    if (!admin) return;
    let alive = true;
    api.agents.list().then(({ agents }) => { if (alive) setStaff(agents); })
      .catch(e => { if (alive) setLoadError(formatError(e)); });
    return () => { alive = false; };
  }, [admin, userId, loadRevision]);

  const update = (key: number, patch: Partial<Member>) => {
    setMembers(rows => rows.map(row => row.key === key ? { ...row, ...patch, result: undefined } : row));
  };
  const updateBinding = (member: Member, index: number, patch: Partial<Binding>) => {
    update(member.key, { bindings: member.bindings.map((binding, i) => i === index ? { ...binding, ...patch } : binding) });
  };
  const pending = members.filter(member => !member.result?.user);
  const ready = pending.length > 0 && pending.every(member =>
    member.display_name.trim() && member.email.trim() && member.bindings[0].access_level === 'operator' && member.bindings.every(binding => binding.agent_name));

  const sameAdmin = () => {
    const current = useAuthStore.getState().user;
    return current?.id === userId && current?.role === 'admin';
  };
  const showPreview = async (member: Member) => {
    setPreviewBusy(true);
    setError('');
    try {
      const result = await api.invitation.preview({ locale: intl.locale, display_name: member.display_name });
      if (sameAdmin()) setPreview(result);
    }
    catch (e) { if (sameAdmin()) setError(formatError(e)); }
    finally { if (sameAdmin()) setPreviewBusy(false); }
  };
  const submit = async () => {
    if (!admin || busy || !ready) return;
    setBusy(true);
    setError('');
    try {
      const { results } = await api.users.createMembers({
        public_url: dashboardBaseUrl(),
        locale: intl.locale,
        members: pending.map(member => ({
          display_name: member.display_name.trim(), email: member.email.trim(), role: member.role,
          department: member.department.trim() || undefined, primary_agent_id: member.bindings[0].agent_name,
          bindings: member.bindings, invite: member.invite,
        })),
      });
      if (!sameAdmin()) return;
      setMembers(rows => rows.map(row => {
        const index = pending.findIndex(member => member.key === row.key);
        if (index < 0) return row;
        const result = results.find(result => result.index === index);
        return { ...row, result: result ?? { error: 'member_unavailable' } };
      }));
      if (results.some(result => result.user)) onCreated();
    } catch (e) { if (sameAdmin()) setError(formatError(e)); }
    finally { if (sameAdmin()) setBusy(false); }
  };

  if (!admin) return <p role="alert">{t('members.adminOnly')}</p>;
  return <div className="mx-auto w-full max-w-4xl space-y-6 pb-8">
    <BreadcrumbHeader segments={[{ label: t('nav.users'), onClick: onBack }, { label: t('members.title') }]} />
    <header className="space-y-2">
      <h1 className="text-xl font-semibold">{t('members.title')}</h1>
      <p className="text-sm text-muted-foreground">{t('members.description')}</p>
    </header>
    {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
    {loadError ? <div className="space-y-2"><p role="alert" className="text-sm text-destructive">{loadError}</p><Button variant="outline" onClick={() => setLoadRevision(value => value + 1)}>{t('errorState.retry')}</Button></div>
      : staff === null ? <div role="status"><Spinner label={t('common.loading')} /> {t('common.loading')}</div>
      : staff.length === 0 && <p role="status" className="text-sm text-muted-foreground">{t('members.noStaff')}</p>}
    <datalist id="member-departments">{departments.map(name => <option key={name} value={name} />)}</datalist>
    {members.map((member, rowIndex) => <section key={member.key} aria-label={intl.formatMessage({ id: 'members.person' }, { number: rowIndex + 1 })} className="space-y-4">
      <div className="flex items-center justify-between gap-3">
        <h2 className="text-base font-medium">{intl.formatMessage({ id: 'members.person' }, { number: rowIndex + 1 })}</h2>
        {members.length > 1 && !member.result?.user && <Button type="button" variant="ghost" size="icon" disabled={busy} aria-label={t('members.removePerson')} onClick={() => setMembers(rows => rows.filter(row => row.key !== member.key))}><Trash2 /></Button>}
      </div>
      <fieldset disabled={busy || !!member.result?.user} className="space-y-4">
        <Card><CardHeader><CardTitle>{t('members.basic')}</CardTitle></CardHeader><CardContent>
          <div className="grid gap-4 sm:grid-cols-2">
            <label className="space-y-2 text-sm"><span>{t('users.field.display_name')}</span><Input value={member.display_name} maxLength={200} autoComplete="name" onChange={e => update(member.key, { display_name: e.target.value })} /></label>
            <label className="space-y-2 text-sm"><span>{t('users.field.email')}</span><Input type="email" value={member.email} maxLength={254} autoComplete="email" onChange={e => update(member.key, { email: e.target.value })} /></label>
            <div className="space-y-2 text-sm"><label id={`member-role-${member.key}`}>{t('users.col.role')}</label>
              <Select value={member.role} onValueChange={value => update(member.key, { role: value as Member['role'] })}>
                <SelectTrigger aria-labelledby={`member-role-${member.key}`}><SelectValue>{roles.find(role => role.value === member.role)?.label}</SelectValue></SelectTrigger>
                <SelectContent>{roles.map(role => <SelectItem key={role.value} value={role.value}>{role.label}</SelectItem>)}</SelectContent>
              </Select>
            </div>
            <label className="space-y-2 text-sm"><span>{t('users.field.department')}</span><Input value={member.department} maxLength={64} list="member-departments" onChange={e => update(member.key, { department: e.target.value })} /></label>
          </div>
        </CardContent></Card>
        <Card><CardHeader><CardTitle>{t('users.col.agents')}</CardTitle><CardDescription>{t('members.primaryHint')}</CardDescription></CardHeader><CardContent className="space-y-3">
          {member.bindings.map((binding, index) => {
            const chosen = staff?.find(agent => agent.name === binding.agent_name);
            const labelId = `member-${member.key}-agent-${index}`;
            return <div key={index} className="flex flex-wrap items-center gap-3 rounded-lg border border-border p-3">
              <Badge id={labelId} variant="outline">{t(index === 0 ? 'members.primary' : 'members.additional')}</Badge>
              <div className="min-w-40 flex-1"><Select value={binding.agent_name} onValueChange={value => updateBinding(member, index, { agent_name: String(value) })}>
                <SelectTrigger aria-labelledby={labelId}><SelectValue>{chosen ? <span className="flex items-center gap-2"><ActorAvatar actorType="agent" name={chosen.display_name || chosen.name} size="sm" />{chosen.display_name || chosen.name}</span> : t('users.bind.chooseAgent')}</SelectValue></SelectTrigger>
                <SelectContent>{(staff ?? []).filter(agent => agent.name === binding.agent_name || !member.bindings.some(b => b.agent_name === agent.name)).map(agent => <SelectItem key={agent.name} value={agent.name}><ActorAvatar actorType="agent" name={agent.display_name || agent.name} size="sm" />{agent.display_name || agent.name}</SelectItem>)}</SelectContent>
              </Select></div>
              <Select value={binding.access_level} onValueChange={value => updateBinding(member, index, { access_level: value as Binding['access_level'] })}>
                <SelectTrigger aria-label={intl.formatMessage({ id: 'members.accessLabel' }, { number: index + 1 })}><SelectValue>{t(`users.access.${binding.access_level}`)}</SelectValue></SelectTrigger>
                <SelectContent>{(['operator', 'viewer'] as const).filter(access => index > 0 || access === 'operator').map(access => <SelectItem key={access} value={access}>{t(`users.access.${access}`)}</SelectItem>)}</SelectContent>
              </Select>
              {index > 0 && <Button variant="ghost" size="icon" aria-label={t('members.removeStaff')} onClick={() => update(member.key, { bindings: member.bindings.filter((_, i) => i !== index) })}><Trash2 /></Button>}
            </div>;
          })}
          {member.bindings.some(binding => binding.access_level === 'viewer') && <p className="text-sm text-muted-foreground">{t('members.viewerHint')}</p>}
          <Button variant="outline" disabled={!staff || member.bindings.length >= staff.length} onClick={() => update(member.key, { bindings: [...member.bindings, { agent_name: '', access_level: 'viewer' }] })}><Plus />{t('members.addStaff')}</Button>
        </CardContent></Card>
        <Card><CardContent className="space-y-3">
          <label className="flex items-start gap-3 text-sm"><Checkbox checked={member.invite} onCheckedChange={checked => update(member.key, { invite: checked === true })} /><span>{t('members.sendInvite')}</span></label>
          <Button variant="link" disabled={previewBusy} onClick={() => void showPreview(member)}>{t('members.preview')}</Button>
          {!member.invite && <p className="text-sm text-muted-foreground">{t('members.inviteLater')}</p>}
        </CardContent></Card>
      </fieldset>
      {member.result && <div role="status" className="space-y-2 rounded-lg border border-border p-4 text-sm">
        <p>{t(member.result.user ? (member.invite ? (member.result.invite?.sent ? 'members.createdSent' : 'members.createdNotSent') : 'members.created') : MEMBER_ERROR_MESSAGES.get(member.result.error ?? '') ?? 'members.failed')}</p>
        {member.result.user && !member.result.invite?.sent && member.result.invite?.link && <Button variant="outline" onClick={() => setLink(member.result!.invite!.link!)}>{t('users.invite.copy')}</Button>}
      </div>}
    </section>)}
    <footer className="flex flex-wrap items-center justify-between gap-3">
      <Button variant="outline" disabled={busy || members.length >= 10} onClick={() => setMembers(rows => [...rows, blankMember(nextKey.current++)])}><Plus />{t('members.addNext')}</Button>
      <div className="flex gap-2"><Button variant="outline" disabled={busy} onClick={onBack}>{t(pending.length ? 'common.cancel' : 'common.done')}</Button>
        {pending.length > 0 && <Button variant="brand" disabled={busy || !ready || !staff?.length} onClick={() => void submit()}>{busy ? <Spinner label={t('common.loading')} /> : <UserPlus />}{t('members.submit')}</Button>}
      </div>
    </footer>
    {preview && <Dialog open onOpenChange={open => !open && setPreview(null)}><DialogContent className="sm:max-w-2xl"><DialogHeader><DialogTitle>{preview.subject}</DialogTitle></DialogHeader>
      <iframe title={t('members.preview')} srcDoc={preview.html} sandbox="" referrerPolicy="no-referrer" className="h-96 w-full rounded-lg border border-border bg-background" />
    </DialogContent></Dialog>}
    {link && <InviteLinkDialog link={link} onClose={() => setLink(null)} />}
  </div>;
}
