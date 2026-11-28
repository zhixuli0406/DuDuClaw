import { useCallback, useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { Link } from 'react-router';
import { Button, Card, CardContent, Input, Switch, useErrorMessage } from '@/components/mds';
import { RiskConfirmDialog } from '@/components/settings/controls';
import { useAdminStepUp } from '@/components/step-up/useAdminStepUp';
import { formatDateTime, useCompanyTimeZone } from '@/lib/company-time';
import { googleSignin, type GoogleSigninForm, type GoogleSigninSettings, type GoogleSigninStatus } from '@/lib/google-signin';
import { isSettingsConflict, settingsRefusal } from '@/lib/settings-refusal';
import { templateLibrary, type LibraryTemplateDetail } from '@/lib/template-library';
import { useUserNames } from '@/lib/use-user-names';

const permissionKeys = ['can_create_agents', 'can_send_cross_agent', 'can_modify_own_skills', 'can_modify_own_soul', 'can_schedule_tasks'] as const;
function formOf(s: GoogleSigninSettings): GoogleSigninForm {
  return { enabled: s.enabled_requested, allowed_hd: [...s.allowed_hd], approval: s.approval,
    daily_new_accounts: s.daily_new_accounts, avatar_template: s.avatar_template,
    ...(s.avatar_template_revision != null ? { avatar_template_revision: s.avatar_template_revision } : {}),
    approval_grants_mfa_enrollment: s.approval_grants_mfa_enrollment };
}
function sameForm(a: GoogleSigninForm, b: GoogleSigninForm): boolean {
  return JSON.stringify([a.enabled, a.allowed_hd, a.approval, a.daily_new_accounts, a.avatar_template, a.avatar_template_revision, a.approval_grants_mfa_enrollment])
    === JSON.stringify([b.enabled, b.allowed_hd, b.approval, b.daily_new_accounts, b.avatar_template, b.avatar_template_revision, b.approval_grants_mfa_enrollment]);
}

export function GoogleSigninAdminCard() {
  const intl = useIntl();
  const t = (key: string, values?: Record<string, string | number>) => intl.formatMessage({ id: `googleSignin.${key}` }, values);
  const errorText = useErrorMessage();
  const { nameOf } = useUserNames();
  useCompanyTimeZone();
  const { ensure, invalidate, dialog } = useAdminStepUp();
  const [status, setStatus] = useState<GoogleSigninStatus | null>(null);
  const [form, setForm] = useState<GoogleSigninForm | null>(null);
  const [templates, setTemplates] = useState<LibraryTemplateDetail[]>([]);
  const [domain, setDomain] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [conflict, setConflict] = useState(false);
  const [pending, setPending] = useState<{ form: GoogleSigninForm; reasons: string[] } | null>(null);
  const [copied, setCopied] = useState(false);
  const picker = useRef<HTMLSelectElement>(null);
  const lock = useRef(false);
  const load = useCallback(async () => {
    if (lock.current) return;
    lock.current = true; setBusy(true); setError(null);
    try {
      const result = await googleSignin.get();
      const catalog = await templateLibrary.catalog();
      const details = await Promise.allSettled(catalog.templates.filter(item => item.source === 'company').map(item => templateLibrary.get(item.id)));
      setTemplates(details.flatMap(item => item.status === 'fulfilled' && item.value.source === 'company' ? [item.value] : []));
      setStatus(result); setForm(formOf(result.settings)); setConflict(false);
    } catch (err) { setStatus(null); setForm(null); setError(settingsRefusal(err, errorText(err))); }
    finally { lock.current = false; setBusy(false); }
  }, [errorText]);
  useEffect(() => { void load(); }, [load]);

  const save = async (next: GoogleSigninForm, acknowledge = false) => {
    if (!status || lock.current || conflict || typeof status.hash !== 'string' || !status.hash) return;
    lock.current = true; setBusy(true); setError(null); setPending(null);
    try {
      const pureDisable = !next.enabled && sameForm(next, { ...formOf(status.settings), enabled: false });
      const token = pureDisable ? null : await ensure(t('stepUpReason'));
      if (!pureDisable && !token) return;
      const result = await googleSignin.set({ ...next, expected_hash: status.hash, ...(acknowledge ? { acknowledge_risk: true } : {}), ...(token ? { step_up_csrf: token } : {}) });
      setStatus({ ...status, hash: result.hash, settings: result.settings, checks: { ...status.checks, domains_set: result.settings.allowed_hd.length > 0 } });
      setForm(formOf(result.settings));
    } catch (err) {
      if (err && typeof err === 'object' && 'code' in err && err.code === 'signin_step_up_required') invalidate();
      const message = settingsRefusal(err, errorText(err));
      setError(message);
      if (isSettingsConflict(message)) setConflict(true);
    } finally { lock.current = false; setBusy(false); }
  };
  const requestSave = (next: GoogleSigninForm) => {
    if (!status || busy || conflict || pending || typeof status.hash !== 'string' || !status.hash) return;
    setError(null);
    if (!next.allowed_hd.length) { setError(t('domainsRequired')); return; }
    if (!Number.isInteger(next.daily_new_accounts) || next.daily_new_accounts < 1 || next.daily_new_accounts > 200) { setError(t('capInvalid')); return; }
    if (next.avatar_template === null || (next.avatar_template !== '' && (!next.avatar_template.startsWith('company:') || next.avatar_template_revision == null))) { setError(t('templateRequired')); return; }
    const chosen = templates.find(item => item.id === next.avatar_template);
    const pureDisable = !next.enabled && sameForm(next, { ...formOf(status.settings), enabled: false });
    if (!pureDisable && next.avatar_template !== '' && (!chosen || chosen.revision !== next.avatar_template_revision)) { setError(t('templateRequired')); return; }
    const before = status.settings;
    const reasons = [
      ...(!before.enabled && next.enabled ? [t(next.approval === 'direct' ? 'riskEnableDirect' : 'riskEnable')] : []),
      ...(before.approval !== 'direct' && next.approval === 'direct' ? [t('riskDirect')] : []),
      ...(!before.approval_grants_mfa_enrollment && next.approval_grants_mfa_enrollment ? [t('riskMfa')] : []),
    ];
    if (reasons.length) setPending({ form: next, reasons }); else void save(next);
  };
  const disabled = busy || conflict || pending !== null || typeof status?.hash !== 'string' || !status.hash;
  const selected = templates.find(item => item.id === form?.avatar_template);
  const templateChanged = form != null && (form.avatar_template === null || (form.avatar_template !== '' && (!selected || selected.revision == null || selected.revision !== form.avatar_template_revision)));
  const check = (key: string, pass: boolean | null, label: string) => <li key={key} className={pass === true ? 'text-success' : 'text-warning'}>
    <span aria-hidden="true">{pass === true ? '✓' : '○'} </span>{label} · {t(pass === null ? 'unknown' : pass ? 'passed' : 'notPassed')}
  </li>;

  return <Card data-testid="google-signin-admin"><CardContent className="space-y-5 py-5">
    <div><h2 className="font-medium">{t('title')}</h2><p className="mt-1 text-sm text-muted-foreground">{t('intro')}</p></div>
    {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
    {(conflict || !status) && <Button variant="outline" disabled={busy} onClick={() => void load()}>{t('reload')}</Button>}
    {status && form && <>
      <div className="space-y-2"><div className="flex items-center gap-2"><Switch nativeButton render={<button type="button" />} aria-label={t('switch')} checked={status.settings.enabled_requested} disabled={disabled}
        onCheckedChange={enabled => requestSave({ ...form, enabled })} /><span className="font-medium">{t('switch')}</span></div>
        <p className="text-sm text-muted-foreground">{t('switchHelp')}</p>
        {status.settings.enabled && status.settings.ack && <p className="text-sm text-muted-foreground">{t('lastAck', { name: nameOf(status.settings.ack.by), time: formatDateTime(status.settings.ack.at, { locale: intl.locale }) })}</p>}
      </div>
      <section className="space-y-2" aria-label={t('domains')}><h3 className="text-sm font-medium">{t('domains')}</h3>
        <ul className="flex flex-wrap gap-2">{form.allowed_hd.map(hd => <li key={hd} className="flex items-center gap-1 rounded-lg border border-surface-border px-2 text-sm">{hd}<Button size="sm" variant="ghost" disabled={disabled} aria-label={t('removeDomain', { domain: hd })} onClick={() => setForm({ ...form, allowed_hd: form.allowed_hd.filter(item => item !== hd) })}>×</Button></li>)}</ul>
        <form className="flex gap-2" onSubmit={event => { event.preventDefault(); const hd = domain.trim().toLowerCase(); if (hd && form.allowed_hd.length < 50 && !form.allowed_hd.includes(hd)) { setForm({ ...form, allowed_hd: [...form.allowed_hd, hd] }); setDomain(''); } }}>
          <Input aria-label={t('domainInput')} placeholder="example.com" value={domain} disabled={disabled} onChange={event => setDomain(event.target.value)} />
          <Button type="submit" variant="outline" disabled={disabled || !domain.trim() || form.allowed_hd.length >= 50}>{t('add')}</Button>
        </form>
      </section>
      <fieldset disabled={disabled} className="space-y-2"><legend className="mb-2 text-sm font-medium">{t('approval')}</legend>{(['admin', 'direct'] as const).map(mode => <label key={mode} className="flex items-start gap-2 text-sm"><input type="radio" name="google-signin-approval" checked={form.approval === mode} onChange={() => setForm({ ...form, approval: mode })} /><span>{t(`approval.${mode}`)}<span className="block text-muted-foreground">{t(`approval.${mode}Help`)}</span></span></label>)}</fieldset>
      <label className="block space-y-2 text-sm">{t('cap')}<Input type="number" aria-label={t('cap')} min={1} max={200} step={1} value={Number.isNaN(form.daily_new_accounts) ? '' : form.daily_new_accounts} disabled={disabled} onChange={event => setForm({ ...form, daily_new_accounts: event.target.valueAsNumber })} /><span className="block text-muted-foreground">{t('capHelp')}</span></label>
      <section className="space-y-2"><label className="block text-sm font-medium" htmlFor="google-signin-template">{t('template')}</label>
        <select id="google-signin-template" ref={picker} className="w-full rounded-lg border border-input bg-surface px-3 py-2 text-sm" value={form.avatar_template ?? '__invalid'} disabled={disabled} onChange={event => {
          const item = templates.find(template => template.id === event.target.value);
          const { avatar_template_revision: _revision, ...rest } = form;
          setForm({ ...rest, avatar_template: event.target.value, ...(item?.revision != null ? { avatar_template_revision: item.revision } : {}) });
        }}>
          {(form.avatar_template === null || (form.avatar_template !== '' && !selected)) && <option value={form.avatar_template ?? '__invalid'} disabled>{t('unavailableTemplate')}</option>}
          <option value="">{t('blank')}</option>{templates.map(item => <option key={item.id} value={item.id} disabled={item.revision == null}>{item.name}</option>)}
        </select>
        {templateChanged ? <div className="space-y-2 text-sm text-warning"><p>{t('templateChanged', { name: selected?.name ?? t('unavailableTemplate') })}</p><Button size="sm" variant="outline" disabled={disabled} onClick={() => {
          if (selected?.revision != null) setForm({ ...form, avatar_template_revision: selected.revision });
          picker.current?.focus();
        }}>{t('reselect')}</Button></div> : selected && <div className="space-y-1 text-sm"><p>{selected.name} · {t('revision', { revision: selected.revision! })}</p>{[true, false].map(allowed => {
          const labels = permissionKeys.filter(key => selected.permissions?.[key] === allowed).map(key => t(`permission.${key}`));
          return labels.length ? <p key={String(allowed)} className="text-muted-foreground">{t(allowed ? 'can' : 'cannot', { permissions: labels.join(intl.locale === 'en' ? ', ' : '、') })}</p> : null;
        })}</div>}
        <p className="text-sm text-muted-foreground">{t('blankHelp')}</p>
      </section>
      <label className="flex items-start gap-2 text-sm"><input type="checkbox" checked={form.approval_grants_mfa_enrollment} disabled={disabled} onChange={event => setForm({ ...form, approval_grants_mfa_enrollment: event.target.checked })} /><span>{t('mfa')}<span className="block text-muted-foreground">{t('mfaHelp')}</span></span></label>
      {status.redirect_uri !== null && <section className="space-y-2"><h3 className="text-sm font-medium">{t('redirect')}</h3><div className="flex flex-wrap items-center gap-2"><code className="break-all text-sm">{status.redirect_uri}</code><Button size="sm" variant="outline" onClick={() => void navigator.clipboard.writeText(status.redirect_uri!).then(() => setCopied(true)).catch(err => setError(settingsRefusal(err, errorText(err))))}>{t('copy')}</Button>{copied && <span role="status" className="text-sm">{t('copied')}</span>}</div></section>}
      <section className="space-y-2 rounded-lg border border-surface-border p-4"><h3 className="text-sm font-medium">{t('checklist')}</h3><ul className="space-y-2 text-sm">
        {check('client', status.checks.client_configured, t('check.client'))}
        {check('url', status.checks.public_url_ok, t(status.checks.public_url_ok ? 'check.publicUrl' : 'check.publicUrlMissing'))}
        {!status.checks.public_url_ok && <li><Link className="text-brand underline underline-offset-2" to="/app/system/settings?tab=general">{t('publicUrlLink')}</Link></li>}
        {check('domains', status.checks.domains_set, t('check.domains'))}
        {check('mfa', status.checks.mfa_policy === 'unknown' ? null : status.checks.mfa_policy === 'enforced', t('check.mfa'))}
        {check('personal', status.checks.personal_connection_domains_cover, t('check.personal'))}
      </ul><p className="text-sm text-muted-foreground">{t('publicUrlHelp')}</p></section>
      <Button disabled={disabled || sameForm(form, formOf(status.settings))} onClick={() => requestSave(form)}>{t('save')}</Button>
    </>}
    <RiskConfirmDialog open={pending !== null} onClose={() => setPending(null)} onConfirm={() => { if (pending) void save(pending.form, true); }} title={t('riskTitle')} body={<div className="space-y-2">{pending?.reasons.map(reason => <p key={reason}>{reason}</p>)}</div>} acknowledgeLabel={t('riskAck')} auditNote={t('riskAudit')} confirmLabel={t('enable')} />
    {dialog}
  </CardContent></Card>;
}
