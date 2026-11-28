import { useEffect, useId, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { Link } from 'react-router';
import { api, type DeptGrantEntry, type DeptGrantOptions, type UserDetail } from '@/lib/api';
import type { GrantGroup } from '@/lib/dept-grants-types';
import { settingsRefusal } from '@/lib/settings-refusal';
import { ConfirmDialog, RiskConfirmDialog } from '@/components/settings/controls';
import { Badge, Button, Checkbox, Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle, useErrorMessage } from '@/components/mds';

const GROUPS: GrantGroup[] = ['schedules', 'automation', 'skills', 'cost', 'egress_review'];
type Change = { department: string; groups: GrantGroup[] };

export function DeptGrantsDialog({ user, onClose, onUpdated }: {
  user: UserDetail; onClose: () => void; onUpdated: () => void;
}) {
  const intl = useIntl();
  const fieldId = useId();
  const errorText = useErrorMessage();
  const t = (key: string, values?: Record<string, string | number>) => intl.formatMessage({ id: `users.deptGrants.${key}` }, values);
  const [grants, setGrants] = useState<DeptGrantEntry[]>([]);
  const [options, setOptions] = useState<DeptGrantOptions | null>(null);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const pending = useRef(false);
  const [error, setError] = useState<string | null>(null);
  const [department, setDepartment] = useState('');
  const [editing, setEditing] = useState(false);
  const [groups, setGroups] = useState<GrantGroup[]>([]);
  const [risk, setRisk] = useState<Change | null>(null);
  const [revoke, setRevoke] = useState<Change | null>(null);

  useEffect(() => {
    let active = true;
    Promise.all([api.users.deptGrants.list(), api.users.deptGrants.options()]).then(([rows, data]) => {
      if (!active) return;
      setGrants(rows.filter(row => row.user_id === user.id));
      setOptions(data);
    }).catch(err => { if (active) setError(settingsRefusal(err, errorText(err))); })
      .finally(() => { if (active) setLoading(false); });
    return () => { active = false; };
  }, [user.id, errorText]);

  const count = (name: string) => options?.departments.find(item => item.name === name)?.agent_count;
  const reset = () => { setDepartment(''); setGroups([]); setEditing(false); };
  const save = async (change: Change, acknowledged = false) => {
    if (pending.current) return;
    pending.current = true;
    setBusy(true);
    setError(null);
    try {
      await api.users.deptGrants.set({ user_id: user.id, ...change, ...(acknowledged ? { acknowledged: true } : {}) });
      setRisk(null);
      setRevoke(null);
      reset();
      onUpdated();
      // Reload the authoritative list after every successful change.
      setOptions(null);
      const [rows, data] = await Promise.all([api.users.deptGrants.list(), api.users.deptGrants.options()]);
      setGrants(rows.filter(row => row.user_id === user.id));
      setOptions(data);
    } catch (err) {
      if (!acknowledged && err && typeof err === 'object' && 'code' in err && err.code === 'acknowledgement_required') {
        setRevoke(null);
        setRisk(change);
      } else setError(settingsRefusal(err, errorText(err)));
    } finally {
      pending.current = false;
      setBusy(false);
    }
  };
  const submit = () => {
    const change = { department, groups: [...groups] };
    const previous = grants.find(row => row.department === department)?.groups ?? [];
    setError(null);
    if (groups.some(group => !previous.includes(group))) setRisk(change);
    else if (!groups.length) setRevoke(change);
    else void save(change);
  };
  const available = options?.departments.filter(item => !grants.some(row => row.department === item.name)) ?? [];
  const canSave = !!options && !!department && !busy && (editing || groups.length > 0);

  return <>
    <Dialog open onOpenChange={open => { if (!open && !pending.current) onClose(); }}>
      <DialogContent className="sm:max-w-xl max-h-[90dvh] overflow-y-auto" showClose={!busy}>
        <DialogHeader>
          <DialogTitle>{t('title', { name: user.display_name })}</DialogTitle>
          <DialogDescription>{t('intro')}</DialogDescription>
        </DialogHeader>
        {loading ? <p role="status">{intl.formatMessage({ id: 'common.loading' })}</p> : <>
          <section className="space-y-2">
            <h3 className="text-sm font-medium">{t('granted')}</h3>
            {options && grants.length === 0 && <p className="rounded-lg border border-dashed p-3 text-sm text-muted-foreground">{t('empty', { name: user.display_name })}</p>}
            {grants.map(row => <div key={row.department} className="flex flex-wrap items-center justify-between gap-3 rounded-lg border border-surface-border p-3">
              <div className="min-w-0 space-y-1">
                <p className="break-words text-sm font-medium">{row.department} {count(row.department) !== undefined && <span className="text-xs text-muted-foreground">· {t('count', { count: count(row.department)! })}</span>}</p>
                <div className="flex flex-wrap gap-1">{row.groups.map(group => <Badge key={group} variant="secondary">{t(`group.${group}`)}</Badge>)}</div>
              </div>
              <div className="flex gap-2">
                <Button size="sm" variant="outline" disabled={busy || !options} onClick={() => { setDepartment(row.department); setGroups([...row.groups]); setEditing(true); setError(null); }}>{t('edit')}</Button>
                <Button size="sm" variant="ghost" disabled={busy || !options} onClick={() => { setError(null); setRevoke({ department: row.department, groups: [] }); }}>{t('revoke')}</Button>
              </div>
            </div>)}
          </section>
          <section className="space-y-3 border-t border-surface-border pt-4">
            <label className="block space-y-2 text-sm font-medium">
              <span>{t(editing ? 'editHeading' : 'addHeading')}</span>
              <select className="h-9 w-full rounded-lg border border-input bg-background px-3 text-sm" value={department} disabled={busy || editing || !options} onChange={event => setDepartment(event.target.value)}>
                <option value="">{t('select')}</option>
                {editing ? <option value={department}>{department}{count(department) !== undefined && `（${t('count', { count: count(department)! })}）`}</option>
                  : available.map(item => <option key={item.name} value={item.name}>{item.name}（{t('count', { count: item.agent_count })}）</option>)}
              </select>
            </label>
            {options && options.mirror_mismatch_count > 0 && <p className="rounded-lg bg-warning/15 p-3 text-sm">{t('mismatch', { count: options.mirror_mismatch_count })} <Link to="/org" onClick={onClose} className="underline">{t('sync')}</Link></p>}
            <fieldset disabled={busy || !options} className="space-y-3">
              {GROUPS.map(group => <div key={group} className="flex cursor-pointer items-start gap-2 text-sm">
                <Checkbox id={`${fieldId}-${group}`} aria-describedby={`${fieldId}-${group}-description`} checked={groups.includes(group)} onCheckedChange={checked => setGroups(previous => checked ? [...previous, group] : previous.filter(value => value !== group))} />
                <div className="space-y-1"><label htmlFor={`${fieldId}-${group}`} className="block cursor-pointer font-medium">{t(`group.${group}`)}</label><span id={`${fieldId}-${group}-description`} className="block text-xs text-muted-foreground">{t(`description.${group}`)}</span></div>
              </div>)}
            </fieldset>
            <p className="text-xs text-muted-foreground">{t('note')}</p>
          </section>
        </>}
        {error && !risk && !revoke && <p role="alert" className="text-sm text-destructive">{error}</p>}
        <DialogFooter>
          <Button variant="outline" disabled={busy} onClick={onClose}>{intl.formatMessage({ id: 'common.close' })}</Button>
          {editing && <Button variant="outline" disabled={busy} onClick={reset}>{intl.formatMessage({ id: 'common.cancel' })}</Button>}
          <Button disabled={!canSave || loading} onClick={submit}>{t(editing ? 'save' : 'add')}</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
    {risk && <RiskConfirmDialog open onClose={() => { if (!pending.current) { setRisk(null); setError(null); } }} onConfirm={() => void save(risk, true)} busy={busy} error={error}
      title={t('riskTitle', { name: user.display_name, department: risk.department })}
      body={t('riskBody', { name: user.display_name, department: risk.department, count: count(risk.department) ?? 0, groups: intl.formatList(risk.groups.map(group => t(`group.${group}`)), { type: 'conjunction' }) })}
      acknowledgeLabel={t('acknowledge')} auditNote={t('audit')} confirmLabel={t('confirm')} />}
    {revoke && <ConfirmDialog open onClose={() => { if (!pending.current) { setRevoke(null); setError(null); } }} onConfirm={() => void save(revoke)} busy={busy}
      title={t('revokeTitle', { department: revoke.department })} message={t('revokeBody')} confirmLabel={t('revoke')}>
      {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
    </ConfirmDialog>}
  </>;
}
