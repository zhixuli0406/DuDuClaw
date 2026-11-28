import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { Plus } from 'lucide-react';
import { Button, SettingsCard, SettingsSection, useErrorMessage } from '@/components/mds';
import { RiskConfirmDialog } from '@/components/settings/controls';
import { api, type MainAccess, type UserDetail } from '@/lib/api';
import { formatDateTime } from '@/lib/company-time';
import { settingsRefusal } from '@/lib/settings-refusal';
import { useAuthStore } from '@/stores/auth-store';
import { useSystemStore } from '@/stores/system-store';

export function MainAccessSection({ agentRole, agentName }: { agentRole: string; agentName: string }) {
  const isAdmin = useAuthStore(s => s.user?.role === 'admin');
  const isPersonal = useSystemStore(s => s.status?.edition_profile) === 'personal';
  if (agentRole !== 'main' || !isAdmin || isPersonal) return null;
  return <MainAccessEditor agentName={agentName} />;
}

function MainAccessEditor({ agentName }: { agentName: string }) {
  const intl = useIntl();
  const t = (key: string, values?: Record<string, string>) => intl.formatMessage({ id: `mainAccess.${key}` }, values);
  const errorText = useErrorMessage();
  const [data, setData] = useState<MainAccess | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [members, setMembers] = useState<UserDetail[]>([]);
  const [selected, setSelected] = useState('');
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [dialogVersion, setDialogVersion] = useState(0);
  useEffect(() => {
    let cancelled = false;
    api.mainAccess.get().then(result => { if (!cancelled) setData(result); })
      .catch(err => { if (!cancelled) setError(settingsRefusal(err, errorText(err))); });
    return () => { cancelled = true; };
  }, [errorText]);

  const candidates = members.filter(member => member.status === 'active' && member.role !== 'admin'
    && !data?.users.some(user => user.user_id === member.id));
  const member = selected === '' ? undefined : candidates[Number(selected)];
  async function add() {
    setBusy(true);
    setError(null);
    try {
      const result = await api.users.list();
      setMembers(result.users);
      setSelected('');
      setOpen(true);
    } catch (err) { setError(settingsRefusal(err, errorText(err))); }
    finally { setBusy(false); }
  }
  async function save(userIds: string[], adding: boolean) {
    setBusy(true);
    setError(null);
    try {
      setData(await api.mainAccess.set({ user_ids: userIds, ...(adding ? { acknowledged: true } : {}) }));
      setOpen(false);
    } catch (err) {
      setError(settingsRefusal(err, errorText(err)));
      if (adding && err && typeof err === 'object' && 'code' in err && err.code === 'acknowledgement_required') {
        setDialogVersion(v => v + 1);
        setOpen(true);
      }
    } finally { setBusy(false); }
  }

  return <SettingsSection title={t('title')}>
    <SettingsCard>
      <div className="space-y-4 p-4">
        {error && !open && <p role="alert" className="text-sm text-destructive">{error}</p>}
        {!data ? (!error && <p role="status" className="text-sm text-muted-foreground">{t('loading')}</p>) : <>
          <p className="text-sm">{data.users.length ? t('intro', { agent: agentName }) : t('empty')}</p>
          {!data.effective && data.ineffective_reason && <p role="alert" className="rounded-lg bg-warning/10 p-3 text-sm">{data.ineffective_reason}</p>}
          <p className="rounded-lg bg-warning/10 p-3 text-sm">{typeof data.risk_notice === 'string' && data.risk_notice.trim() ? data.risk_notice : t('risk')}</p>
          {data.users.length > 0 && <div className="overflow-x-auto"><table className="w-full text-left text-sm">
            <thead className="text-muted-foreground"><tr>
              <th scope="col" className="p-2">{t('member')}</th><th scope="col" className="p-2">{t('grantedBy')}</th>
              <th scope="col" className="p-2">{t('grantedAt')}</th><th scope="col" className="p-2"><span className="sr-only">{t('remove')}</span></th>
            </tr></thead>
            <tbody>{data.users.map(user => <tr key={user.user_id} className="border-t border-surface-border">
              <td className="p-2">{user.display_name}</td><td className="p-2">{user.granted_by_name || '—'}</td>
              <td className="whitespace-nowrap p-2">{formatDateTime(user.granted_at, { locale: intl.locale })}</td>
              <td className="p-2"><Button type="button" variant="ghost" size="sm" disabled={busy} onClick={() => void save(data.users.filter(u => u.user_id !== user.user_id).map(u => u.user_id), false)}>{t('remove')}</Button></td>
            </tr>)}</tbody>
          </table></div>}
          <Button type="button" variant="outline" disabled={busy || data.users.length >= 500} onClick={() => void add()}><Plus className="size-4" />{t('add')}</Button>
          {data.users.length > 0 && <p className="text-xs text-muted-foreground">{t('note')}</p>}
        </>}
      </div>
    </SettingsCard>
    <RiskConfirmDialog key={dialogVersion} open={open} onClose={() => { if (!busy) { setOpen(false); setError(null); } }}
      title={t('dialog.title')} acknowledgeLabel={t('dialog.acknowledge')} auditNote={t('dialog.audit')} confirmLabel={t('dialog.confirm')}
      busy={busy || !member} error={open ? error : null}
      onConfirm={() => { if (member && data) void save([...data.users.map(user => user.user_id), member.id], true); }}
      body={<div className="space-y-3">
        <label className="block space-y-1"><span>{t('member')}</span>
          <select className="w-full rounded-lg border border-input bg-surface-raised p-2 text-foreground" value={selected} disabled={busy}
            onChange={event => { setSelected(event.target.value); setDialogVersion(v => v + 1); }}>
            <option value="">{t('picker')}</option>
            {candidates.map((user, index) => <option key={user.id} value={String(index)}>{user.display_name} · {user.email}</option>)}
          </select>
        </label>
        {!candidates.length && <p>{t('noCandidates')}</p>}
        {member && <p>{t('dialog.body', { member: member.display_name, agent: agentName })}</p>}
      </div>} />
  </SettingsSection>;
}
