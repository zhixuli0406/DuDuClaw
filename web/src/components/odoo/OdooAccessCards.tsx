import { useCallback, useEffect, useRef, useState, type ReactNode } from 'react';
import { useIntl } from 'react-intl';
import { api } from '@/lib/api';
import { type OdooAccess, type OdooAccessPatch, REFUSE_ACTIONS, REFUSE_GROUPS, refuseRows, validRefuseRule } from '@/lib/odoo-access';
import { memberSignin, type OdooMemberCredential } from '@/lib/member-signin';
import { useUserNames } from '@/lib/use-user-names';
import { settingsRefusal } from '@/lib/settings-refusal';
import { formatDateTime } from '@/lib/company-time';
import { useAgentsStore } from '@/stores/agents-store';
import { Button, Input, SettingsCard, SettingsSection, Switch, useErrorMessage } from '@/components/mds';
import { ConfirmDialog, RiskConfirmDialog } from '@/components/settings/controls';

function useText() {
  const intl = useIntl();
  return (key: string, values?: Record<string, string | number>) => intl.formatMessage({ id: `odooAccess.${key}` }, values);
}

export function useOdooAccess(enabled: boolean) {
  const [data, setData] = useState<OdooAccess | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const saving = useRef(false);
  const errorText = useErrorMessage();
  useEffect(() => {
    if (!enabled) return;
    let active = true;
    void api.odooAccess.get().then(result => { if (active) setData(result); })
      .catch(err => { if (active) setError(settingsRefusal(err, errorText(err))); });
    return () => { active = false; };
  }, [enabled, errorText]);
  const save = async (patch: Partial<OdooAccessPatch>) => {
    if (saving.current || !data?.readable) return false;
    saving.current = true;
    setBusy(true);
    setError(null);
    try {
      setData(await api.odooAccess.set({ acknowledge_risk: false, dismiss_upgrade_notice: false, ...patch }));
      return true;
    } catch (err) { setError(settingsRefusal(err, errorText(err))); return false; }
    finally { saving.current = false; setBusy(false); }
  };
  return { data, error, busy, save, clearError: () => setError(null) };
}
type AccessController = ReturnType<typeof useOdooAccess>;

export function OdooAccessNotice({ access }: { access: AccessController }) {
  const t = useText();
  return <div className="space-y-2">
    {access.data?.readable === false && <p role="alert" className="text-sm text-destructive">{t('unreadable')} {access.data.message}</p>}
    {access.error && <p role="alert" className="text-sm text-destructive">{access.error}</p>}
    {access.data?.readable && access.data.upgrade_notice_pending && <div className="flex flex-wrap items-center justify-between gap-3 rounded-lg border border-warning/30 bg-warning/10 p-3 text-sm">
      <p>{t('upgrade')}</p><Button variant="outline" size="sm" disabled={access.busy} onClick={() => void access.save({ dismiss_upgrade_notice: true })}>{t('dismiss')}</Button>
    </div>}
  </div>;
}

function Card({ title, description, children }: { title: string; description?: string; children: ReactNode }) {
  return <SettingsSection title={title} description={description}><SettingsCard><div className="space-y-4 p-4">{children}</div></SettingsCard></SettingsSection>;
}

interface RiskAction { title: string; body: ReactNode; ack: string; confirm: string; patch: Partial<OdooAccessPatch> }

export function OdooAccessCards({ access }: { access: AccessController }) {
  const t = useText();
  const intl = useIntl();
  const { nameOf } = useUserNames();
  const agents = useAgentsStore(s => s.agents);
  const [risk, setRisk] = useState<RiskAction | null>(null);
  const [action, setAction] = useState<string>('unlink');
  const [target, setTarget] = useState('');
  const [invalid, setInvalid] = useState(false);
  const [picker, setPicker] = useState(false);
  const data = access.data?.readable ? access.data : null;
  const config = data?.config;
  const disabled = !data || access.busy;
  const mode = config?.permission_mode === 'follow_odoo_account' && config.permission_mode_ack ? 'follow_odoo_account' : 'engine';
  const ask = (next: RiskAction) => { access.clearError(); setRisk(next); };
  const label = (row: ReturnType<typeof refuseRows>[number]) => {
    if (row.group) return t(`group.${row.group}`);
    const [verb, model] = row.rules[0].split(':');
    if (verb === 'method') return t('rule.method', { target: model });
    const subject = model === '*' ? t('allTables') : model.endsWith('.*') ? t('descendants', { target: model }) : model;
    return t('rule.description', { action: t(`action.${verb}`), target: subject });
  };
  const selectClass = 'h-9 max-w-full rounded-md border border-input bg-background px-3 text-sm';
  const followBody = <div className="space-y-3">
    <div><h4 className="font-medium text-foreground">{t('skipped')}</h4><ul className="list-disc space-y-1 pl-5"><li>{t('skipTables')}</li><li>{t('skipOperations')}</li></ul></div>
    <div><h4 className="font-medium text-foreground">{t('retained')}</h4><ul className="list-disc space-y-1 pl-5"><li>{t('keepRefusals')}</li><li>{t('keepAccount')}</li><li>{t('keepApproval')}</li></ul></div>
    <div><h4 className="font-medium text-foreground">{t('affected')}</h4>{data?.follow_mode_impact ? <><p>{t('avatars', { count: data.follow_mode_impact.avatars_with_personal_credentials })}</p><p>{t('requesters', { count: data.follow_mode_impact.agents_acting_as_requester })}</p></> : <p>{t('unknownImpact')}</p>}</div>
  </div>;
  return <>
    <fieldset disabled={disabled} className="min-w-0 space-y-6">
      <Card title={t('mode')} description={t('modeDescription')}>
        <div className="grid gap-3 sm:grid-cols-2">{(['engine', 'follow_odoo_account'] as const).map(value => <label key={value} className="flex items-start gap-3 rounded-lg border border-input p-3">
          <input type="radio" name="odoo-access-mode" value={value} checked={mode === value} onChange={() => {
            if (value === 'engine') void access.save({ permission_mode: value });
            else ask({ title: t('followTitle'), body: followBody, ack: t('followAck'), confirm: t('followConfirm'), patch: { permission_mode: value, acknowledge_risk: true } });
          }} />
          <span className="space-y-1 text-sm"><span className="block font-medium">{t(`mode.${value}`)}</span><span className="block text-muted-foreground">{t(`modeDescription.${value}`)}</span></span>
        </label>)}</div>
        {mode === 'follow_odoo_account' && config?.permission_mode_ack && <p className="text-xs text-muted-foreground">{t('lastAck', { name: nameOf(config.permission_mode_ack.by), time: formatDateTime(config.permission_mode_ack.at, { locale: intl.locale }) })}</p>}
      </Card>
      <Card title={t('refuse')} description={t('refuseDescription')}>
        <ul className="divide-y divide-border rounded-lg border border-border">{refuseRows(config?.always_refuse ?? []).map(row => <li key={row.key} className="flex items-center justify-between gap-3 p-3 text-sm">
          <span>{label(row)}</span><Button size="sm" variant="outline" onClick={() => ask({ title: t('removeTitle', { label: label(row) }), body: <div className="space-y-2"><p>{data?.risk_text.always_refuse}</p>{row.group && <p>{t(`groupRisk.${row.group}`)}</p>}</div>, ack: t('removeAck'), confirm: t('remove'), patch: { always_refuse: config!.always_refuse.filter(rule => !row.rules.includes(rule)), acknowledge_risk: true } })}>{t('remove')}</Button>
        </li>)}</ul>
        <form className="flex flex-wrap items-end gap-2" onSubmit={event => {
          event.preventDefault();
          if (!validRefuseRule(action, target)) { setInvalid(true); return; }
          setInvalid(false);
          void access.save({ always_refuse: Array.from(new Set([...(config?.always_refuse ?? []), `${action}:${target}`])) }).then(ok => { if (ok) setTarget(''); });
        }}>
          <label className="space-y-1 text-sm"><span className="block">{t('action')}</span><select className={selectClass} value={action} onChange={event => setAction(event.target.value)}>{REFUSE_ACTIONS.map(verb => <option key={verb} value={verb}>{t(`action.${verb}`)}</option>)}</select></label>
          <label className="min-w-0 flex-1 space-y-1 text-sm"><span className="block">{t('target')}</span><Input value={target} onChange={event => setTarget(event.target.value)} aria-invalid={invalid} aria-describedby="odoo-rule-help" /></label>
          <Button type="submit" variant="outline">{t('add')}</Button>
        </form>
        <p id="odoo-rule-help" className={invalid ? 'text-sm text-destructive' : 'text-xs text-muted-foreground'} role={invalid ? 'alert' : undefined}>{t(invalid ? 'invalidRule' : 'ruleHelp')}</p>
        <label className="flex flex-wrap items-center gap-2 text-sm">{t('addDefault')}<select className={selectClass} value="" onChange={event => {
          const group = REFUSE_GROUPS.find(group => group.key === event.target.value);
          if (group) void access.save({ always_refuse: Array.from(new Set([...(config?.always_refuse ?? []), ...group.rules])) });
        }}><option value="">{t('choose')}</option>{REFUSE_GROUPS.filter(group => group.rules.some(rule => !config?.always_refuse.includes(rule))).map(group => <option key={group.key} value={group.key}>{t(`group.${group.key}`)}</option>)}</select></label>
        <div className="space-y-2 border-t border-border pt-4"><h4 className="text-sm font-medium">{t('fixed')}</h4><ul className="list-disc space-y-1 pl-5 text-sm text-muted-foreground">{data?.fixed_refusals.map(item => <li key={item.code}>{item.reason}</li>)}</ul></div>
      </Card>
      <Card title={t('company')} description={t('companyDescription')}>
        <ul className="flex flex-wrap gap-2">{config?.company_key_agents.map(id => {
          const name = data?.employees.find(employee => employee.id === id)?.display_name?.trim() || agents.find(agent => agent.name === id)?.display_name?.trim() || t('unknownAgent');
          return <li key={id} className="flex items-center gap-2 rounded-full border border-border py-1 pl-3 pr-1 text-sm"><span>{name}</span><Button size="sm" variant="ghost" aria-label={t('removeAgent', { name })} onClick={() => void access.save({ company_key_agents: config.company_key_agents.filter(agent => agent !== id) })}>{t('remove')}</Button></li>;
        })}</ul>
        <Button size="sm" variant="outline" onClick={() => setPicker(!picker)}>{t('addAgent')}</Button>
        {picker && <label className="block space-y-1 text-sm"><span className="block">{t('chooseAgent')}</span><select className={selectClass} value="" onChange={event => {
          const id = event.target.value;
          if (id) ask({ title: t('addAgentTitle'), body: data?.risk_text.company_key_agents, ack: t('addAgentAck'), confirm: t('add'), patch: { company_key_agents: [...(config?.company_key_agents ?? []), id], acknowledge_risk: true } });
        }}><option value="">{t('choose')}</option>{data?.employees.filter(employee => !config?.company_key_agents.includes(employee.id)).map(employee => <option key={employee.id} value={employee.id}>{employee.display_name?.trim() || agents.find(agent => agent.name === employee.id)?.display_name?.trim() || t('unknownAgent')}</option>)}</select></label>}
      </Card>
    </fieldset>
    <OdooMemberCredentialsCard />
    <RiskConfirmDialog open={risk !== null} onClose={() => { if (!access.busy) setRisk(null); }} onConfirm={() => { if (risk) void access.save(risk.patch).then(ok => { if (ok) setRisk(null); }); }}
      title={risk?.title ?? ''} body={risk?.body} acknowledgeLabel={risk?.ack} confirmLabel={risk?.confirm} auditNote={t('audit')} busy={access.busy} error={access.error} />
  </>;
}

function OdooMemberCredentialsCard() {
  const t = useText();
  const intl = useIntl();
  const errorText = useErrorMessage();
  const [members, setMembers] = useState<{ credentials: OdooMemberCredential[]; enabled: boolean } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const saving = useRef(false);
  const [enabling, setEnabling] = useState(false);
  const [revoke, setRevoke] = useState<OdooMemberCredential | null>(null);
  const load = useCallback(async () => {
    try { setMembers(await memberSignin.credentials()); setError(null); }
    catch (err) { setError(settingsRefusal(err, errorText(err))); }
  }, [errorText]);
  useEffect(() => { void load(); }, [load]);
  const mutate = async (operation: () => Promise<void>) => {
    if (saving.current) return;
    saving.current = true; setBusy(true); setError(null);
    try { await operation(); }
    catch (err) { setError(settingsRefusal(err, errorText(err))); }
    finally { saving.current = false; setBusy(false); }
  };
  const setEnabled = (enabled: boolean) => mutate(async () => {
    const result = await memberSignin.setEnabled(enabled, enabled);
    setMembers(current => current && { ...current, enabled: result.enabled });
    setEnabling(false);
  });
  return <Card title={t('member')}>
    <div className="flex items-center justify-between gap-3"><label htmlFor="odoo-member-enabled" className="text-sm font-medium">{t('memberSwitch')}</label><Switch id="odoo-member-enabled" checked={members?.enabled ?? false} disabled={!members || busy} onCheckedChange={enabled => { setError(null); if (enabled) setEnabling(true); else void setEnabled(false); }} /></div>
    <p className="text-xs text-muted-foreground">{t('memberNote')}</p>
    {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
    {!members && <Button variant="outline" size="sm" disabled={busy} onClick={() => void load()}>{t('reload')}</Button>}
    <div className="overflow-x-auto"><table className="w-full text-left text-sm"><thead><tr>{['memberName', 'login', 'state', 'lastUsed', 'actions'].map(key => <th key={key} scope="col" className="border-b border-border p-2 font-medium text-muted-foreground">{t(key)}</th>)}</tr></thead><tbody>{members?.credentials?.map(member => <tr key={member.user_id}>
      <td className="border-b border-border p-2">{member.display_name?.trim() || member.email || t('unknownMember')}</td><td className="border-b border-border p-2">{member.login}</td><td className="border-b border-border p-2">{t(`state.${member.state}`)}</td><td className="border-b border-border p-2">{member.last_used_at ? formatDateTime(member.last_used_at, { locale: intl.locale }) : t('neverUsed')}</td><td className="border-b border-border p-2"><Button variant="ghost" size="sm" disabled={busy} onClick={() => { setError(null); setRevoke(member); }}>{t('revoke')}</Button></td>
    </tr>)}</tbody></table></div>
    {members && !members.credentials?.length && <p className="text-sm text-muted-foreground">{t('emptyCredentials')}</p>}
    <p className="text-xs text-muted-foreground">{t('noKeys')}</p>
    <RiskConfirmDialog open={enabling} onClose={() => { if (!busy) setEnabling(false); }} onConfirm={() => void setEnabled(true)} title={t('memberTitle')} body={t('memberRisk')} acknowledgeLabel={t('memberAck')} confirmLabel={t('enable')} auditNote={t('audit')} busy={busy} error={error} />
    <ConfirmDialog open={revoke !== null} onClose={() => { if (!busy) setRevoke(null); }} onConfirm={() => void mutate(async () => {
      if (!revoke) return;
      const result = await memberSignin.revoke(revoke.user_id);
      if (result.revoked) {
        setMembers(current => current && { ...current, credentials: current.credentials.filter(member => member.user_id !== revoke.user_id) });
        setRevoke(null);
      } else setError(t('revokeFailed'));
    })} title={t('revokeTitle')} message={t('revokeBody', { name: revoke?.display_name?.trim() || revoke?.email || t('unknownMember') })} confirmLabel={t('revoke')} busy={busy}>
      {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
    </ConfirmDialog>
  </Card>;
}
