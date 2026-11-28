import { useCallback, useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { Link } from 'react-router';
import { Shield } from 'lucide-react';
import { api, type ChatAdminSettings, type ChatApprovalKind } from '@/lib/api';
import { formatDateTime, useCompanyTimeZone } from '@/lib/company-time';
import { formatError } from '@/lib/toast';
import { useSystemStore } from '@/stores/system-store';
import { RiskConfirmDialog } from '@/components/settings/controls';
import { Button, Card, CardHeader, CardTitle, CardDescription, CardContent, Input, Switch } from '@/components/mds';

type KindGroup = { label: string; impact: string; keys: string[] };
export function ChatApprovalCard() {
  const intl = useIntl();
  const { zone } = useCompanyTimeZone();
  const t = (key: string, values?: Record<string, string | number>) => intl.formatMessage({ id: `security.chatApproval.${key}` }, values);
  const personal = useSystemStore((s) => s.status?.edition_profile) === 'personal';
  const [loaded, setLoaded] = useState<ChatAdminSettings | null>(null);
  const [form, setForm] = useState<ChatAdminSettings | null>(null);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);
  const [dialog, setDialog] = useState<'enable' | KindGroup | null>(null);
  const load = useCallback(async () => {
    setLoading(true); setError(null);
    try {
      const next = await api.chatAdmin.get();
      setLoaded(next); setForm(next);
    } catch (err) { setLoaded(null); setForm(null); setError(formatError(err)); }
    finally { setLoading(false); }
  }, []);
  useEffect(() => { void load(); }, [load]);
  const label = (kind: ChatApprovalKind) => kind.label[intl.locale === 'zh-TW' ? 'zh-TW' : intl.locale === 'ja-JP' ? 'ja' : 'en'];
  const groups: KindGroup[] = [];
  for (const kind of form?.kinds ?? []) {
    const existing = groups.find((group) => group.label === label(kind) && group.impact === kind.impact);
    if (existing) existing.keys.push(kind.key);
    else groups.push({ label: label(kind), impact: kind.impact, keys: [kind.key] });
  }
  const checked = (group: KindGroup) => form!.kinds.filter((kind) => group.keys.includes(kind.key)).every((kind) => kind.step_up_required);
  const disabled = personal || loading || saving || !loaded?.readable || loaded.hash == null;
  function change(next: ChatAdminSettings) { setForm(next); setSaved(false); setError(null); }
  function setGroup(group: KindGroup, required: boolean) {
    if (!form || disabled) return;
    change({ ...form, kinds: form.kinds.map((kind) => group.keys.includes(kind.key) ? { ...kind, step_up_required: required } : kind) });
  }
  async function save(enabled: boolean, acknowledge = false) {
    if (!form || !loaded || disabled || saving) return;
    if (!form.step_up_methods.length) { setError(t('methodsError')); return; }
    if (!Number.isInteger(form.card_valid_hours) || form.card_valid_hours < 1 || form.card_valid_hours > 720) { setError(t('hoursError')); return; }
    if (enabled && !form.channels.length) { setError(t('channelsError')); return; }
    const exempt = form.kinds.filter((kind) => !kind.step_up_required).map((kind) => kind.key);
    const newlyExempt = exempt.filter((key) => loaded.kinds.some((kind) => kind.key === key && kind.step_up_required));
    setSaving(true); setError(null); setSaved(false);
    try {
      await api.chatAdmin.set({ expected_hash: loaded.hash!, enabled, channels: form.channels, step_up_methods: form.step_up_methods,
        card_valid_hours: form.card_valid_hours, step_up_exempt_kinds: exempt,
        ...(acknowledge ? { acknowledge_risk: true as const } : {}),
        ...(newlyExempt.length ? { acknowledge_exempt_risk: newlyExempt } : {}),
      });
      setDialog(null);
      // Discard the old hash before re-reading; a failed refresh must not allow stale writes.
      setLoaded(null); setForm(null);
      const next = await api.chatAdmin.get();
      setLoaded(next); setForm(next); setSaved(true);
    } catch (err) { setError(formatError(err)); }
    finally { setSaving(false); }
  }
  return <Card>
    <CardHeader>
      <CardTitle className="flex items-center gap-2 text-sm font-medium"><Shield className="size-4 text-brand" />{t('title')}</CardTitle>
      <CardDescription>{t('description')}</CardDescription>
    </CardHeader>
    <CardContent className="space-y-4">
      {personal && <p className="text-sm">{t('personal')}</p>}
      {loading && <p role="status">{intl.formatMessage({ id: 'common.loading' })}</p>}
      {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
      {!loading && !form && <Button disabled={saving} onClick={() => void load()}>{intl.formatMessage({ id: 'restoreFlow.common.retry' })}</Button>}
      {form && <>
        <div className="flex items-center justify-between gap-3">
          <label htmlFor="chat-approval-enabled" className="text-sm">{t('switch')}</label>
          <Switch id="chat-approval-enabled" checked={form.enabled} disabled={disabled} onCheckedChange={(value) => { if (value) { setError(null); setDialog('enable'); } else void save(false); }} />
        </div>
        <p className="text-xs text-muted-foreground">{t('switchHelp')}</p>
        {!personal && ((!form.enabled && form.enabled_ack) || !form.readable) && <p role="status" className="text-sm text-warning">{t('revoked')}</p>}
        {!personal && form.enabled && form.enabled_ack && <p className="text-xs text-muted-foreground">{t('lastEnabled', { name: form.enabled_ack.by_name, time: formatDateTime(form.enabled_ack.at, { locale: intl.locale, zone: zone ?? undefined }) })}</p>}
        <fieldset disabled={disabled} className="space-y-2">
          <legend className="text-sm font-medium">{t('channels')}</legend>
          {(['telegram', 'googlechat'] as const).map((channel) => <label key={channel} className="mr-4 inline-flex items-center gap-2 text-sm"><input type="checkbox" checked={form.channels.includes(channel)} onChange={(e) => change({ ...form, channels: e.target.checked ? [...form.channels, channel] : form.channels.filter((value) => value !== channel) })} />{t(channel)}</label>)}
          <p className="text-xs text-muted-foreground">{t('channelsHelp')}</p>
        </fieldset>
        <section className="space-y-3" aria-label={t('kinds')}>
          <h3 className="text-sm font-medium">{t('kinds')}</h3>
          <h4 className="text-sm">{t('onePress')}</h4>
          {form.t1_kinds.map((kind) => <div key={kind.key} className="flex flex-wrap items-start justify-between gap-2 border-b pb-3 text-sm"><div><p>{label(kind)}</p><p className="text-xs text-muted-foreground">{kind.impact}</p></div><span className="rounded bg-muted px-2 py-1 text-xs">{t('noStepUp')}</span></div>)}
          <h4 className="text-sm">{t('optionalStepUp')}</h4>
          <p className="text-xs text-muted-foreground">{t('recommendAll')}</p>
          {groups.map((group) => <div key={group.keys[0]} className="flex flex-wrap items-start justify-between gap-3 border-b pb-3 text-sm"><div><p>{group.label}</p><p className="text-xs text-muted-foreground">{group.impact}</p></div><label className="inline-flex items-center gap-2"><input type="checkbox" aria-label={`${group.label} ${t('stepUp')}`} checked={checked(group)} disabled={disabled} onChange={(e) => { if (e.target.checked) setGroup(group, true); else setDialog(group); }} />{t('stepUp')}</label></div>)}
          <h4 className="text-sm">{t('dashboardOnly')}</h4><p className="text-xs text-muted-foreground">{t('dashboardHelp')}</p>
          <p className="text-xs text-muted-foreground">{t('rejectHelp')}</p>
        </section>
        <fieldset disabled={disabled} className="space-y-3">
          <legend className="text-sm font-medium">{t('methods')}</legend>
          {(['web', 'dm_totp'] as const).map((method) => <div key={method}><label className="inline-flex items-center gap-2 text-sm"><input type="checkbox" checked={form.step_up_methods.includes(method)} onChange={(e) => change({ ...form, step_up_methods: e.target.checked ? [...form.step_up_methods, method] : form.step_up_methods.filter((value) => value !== method) })} />{t(method)}</label><p className="text-xs text-muted-foreground">{t(`${method}Help`)}</p></div>)}
        </fieldset>
        <div className="space-y-2"><label htmlFor="chat-approval-hours" className="text-sm font-medium">{t('hours')}</label><p className="text-xs text-muted-foreground">{t('hoursHelp')}</p><div className="flex items-center gap-2"><Input className="w-28" id="chat-approval-hours" type="number" min={1} max={720} step={1} value={Number.isNaN(form.card_valid_hours) ? '' : form.card_valid_hours} disabled={disabled} onChange={(e) => change({ ...form, card_valid_hours: e.target.value === '' ? NaN : Number(e.target.value) })} /><span className="text-sm">{t('hourUnit')}</span></div></div>
        <section className="space-y-2"><h3 className="text-sm font-medium">{t('adminLinks')}</h3>{form.admins.map((admin) => <p key={admin.user_id} className="text-sm">{t('adminStatus', { name: admin.name, status: t(admin.proven_channels.length ? 'linked' : 'unlinked') })}</p>)}<p className="text-xs text-muted-foreground">{t('adminHelp')}</p></section>
        {!personal && <div className="space-y-2 text-sm">
          {form.enabled && !form.admins.some((admin) => admin.proven_channels.length) && <div className="space-y-2"><p role="status" className="text-warning">{t('noAdminLinked')}</p><Link className="text-brand underline" to="/me/connections">{t('connections')}</Link></div>}
          {!form.web_link_available ? <p>{t('webUnavailable')}</p> : form.admins.every((admin) => admin.has_totp) && <p>{t('webAvailable')}</p>}
          {form.admins.filter((admin) => !admin.has_totp).map((admin) => <p key={admin.user_id}>{t('missingTotp', { name: admin.name })}</p>)}
        </div>}
        <Button disabled={disabled} onClick={() => void save(form.enabled)}>{saving ? intl.formatMessage({ id: 'common.saving' }) : t('save')}</Button>
        {saved && <p role="status" className="text-sm">{intl.formatMessage({ id: 'common.saved' })}</p>}
      </>}
      <RiskConfirmDialog open={dialog !== null} onClose={() => { if (!saving) setDialog(null); }} busy={saving} error={error}
        title={dialog === 'enable' ? t('enableTitle') : t('exemptTitle', { label: dialog?.label ?? '' })}
        body={dialog === 'enable' ? <div className="space-y-3"><p>{t('enableBody')}</p><ul className="max-h-64 list-disc space-y-1 overflow-y-auto pl-5">{form?.t1_kinds.map((kind) => <li key={kind.key}>{t('impactLine', { label: label(kind), impact: kind.impact })}</li>)}{groups.map((group) => <li key={group.keys[0]}>{t('impactLine', { label: group.label, impact: group.impact })}</li>)}</ul><p>{t(groups.every(checked) ? 'recommendCountAll' : 'recommendCount', { total: groups.length, count: groups.filter(checked).length })}</p></div> : t('exemptBody', { impact: dialog?.impact ?? '' })}
        acknowledgeLabel={dialog === 'enable' ? undefined : t('exemptAcknowledge')}
        auditNote={dialog === 'enable' ? undefined : t('exemptAudit')}
        confirmLabel={dialog === 'enable' ? undefined : t('exemptConfirm')}
        cancelLabel={dialog === 'enable' ? undefined : t('exemptCancel')}
        onConfirm={() => { if (dialog === 'enable') void save(true, true); else if (dialog) { setGroup(dialog, false); setDialog(null); } }} />
    </CardContent>
  </Card>;
}
