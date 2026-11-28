import { useCallback, useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { Link, useLocation } from 'react-router';
import { Badge, Button, Card, CardContent, Input, Switch, useErrorMessage } from '@/components/mds';
import { ConfirmDialog, RiskConfirmDialog } from '@/components/settings/controls';
import { GoogleDataDisclosure } from '@/components/GoogleDataDisclosure';
import { api, type DriveWriteAction, type DriveWriteApproval, type DriveWriteForm, type DriveWriteStatus, type GoogleIdentityTestStep } from '@/lib/api';
import { formatDateTime, useCompanyTimeZone } from '@/lib/company-time';
import { isSettingsConflict, settingsRefusal } from '@/lib/settings-refusal';
import { useAgentsStore } from '@/stores/agents-store';

const ACTIONS: DriveWriteAction[] = ['create', 'modify', 'move', 'trash'];
const MODES: DriveWriteApproval[] = ['none', 'member', 'member_and_admin'];
const SCOPES = ['https://www.googleapis.com/auth/drive', 'https://www.googleapis.com/auth/documents', 'https://www.googleapis.com/auth/spreadsheets'];
const selectClass = 'w-full rounded-lg border border-input bg-surface px-3 py-2 text-sm disabled:opacity-50';

function formOf(status: DriveWriteStatus): DriveWriteForm {
  return { ...status.configured, expected_hash: status.hash, approval: { ...status.approval }, folders: status.folders.map(folder => folder.id) };
}
function widens(before: DriveWriteStatus, next: DriveWriteForm): boolean {
  return ACTIONS.some(action => (!before.configured[action] && next[action])
    || MODES.indexOf(next.approval[action]) < MODES.indexOf(before.approval[action]))
    || next.folders.some(folder => !before.folders.some(existing => existing.id === folder))
    || (before.folders.length > 0 && next.folders.length === 0 && (next.create || next.modify));
}

export function GoogleDriveWriteCard({ integrationEnabled }: { integrationEnabled: boolean | null }) {
  const intl = useIntl();
  const t = (key: string, values?: Record<string, string | number>) => intl.formatMessage({ id: `driveWrite.${key}` }, values);
  const errorText = useErrorMessage();
  const location = useLocation();
  useCompanyTimeZone();
  const [status, setStatus] = useState<DriveWriteStatus | null>(null);
  const lock = useRef(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [conflict, setConflict] = useState(false);
  const [pending, setPending] = useState<{ form: DriveWriteForm; action: DriveWriteAction | 'write' } | null>(null);
  const [folderInput, setFolderInput] = useState('');
  const [dataAck, setDataAck] = useState(false);
  const [writeAck, setWriteAck] = useState(false);
  const [paste, setPaste] = useState(false);
  const [pasteUrl, setPasteUrl] = useState('');
  const [disconnect, setDisconnect] = useState(false);
  const [agentId, setAgentId] = useState('');
  const [step, setStep] = useState<GoogleIdentityTestStep | null>(null);
  const [copied, setCopied] = useState(false);
  const agents = useAgentsStore(s => s.agents);
  const fetchAgents = useAgentsStore(s => s.fetchAgents);
  const accept = useCallback((result: DriveWriteStatus) => {
    setStatus(result);
    setConflict(false);
  }, []);
  const load = useCallback(async () => {
    if (lock.current) return;
    lock.current = true; setBusy(true); setError(null);
    try { accept(await api.googleDriveWrite.get()); }
    catch (err) { setStatus(null); setError(settingsRefusal(err, errorText(err))); }
    finally { lock.current = false; setBusy(false); }
  }, [accept, errorText]);
  useEffect(() => { void load(); }, [load, integrationEnabled]);
  useEffect(() => { void fetchAgents(); }, [fetchAgents]);

  const save = async (form: DriveWriteForm, acknowledge = false) => {
    if (!status || lock.current || conflict || typeof status.hash !== 'string' || !status.hash) return;
    lock.current = true; setBusy(true); setError(null);
    try {
      accept(await api.googleDriveWrite.set({ ...form, expected_hash: status.hash, ...(acknowledge ? { acknowledge_risk: true } : {}) }));
      setFolderInput('');
    } catch (err) {
      const message = settingsRefusal(err, errorText(err));
      setError(message);
      if (isSettingsConflict(message)) setConflict(true);
    }
    finally { lock.current = false; setBusy(false); setPending(null); }
  };
  const change = (form: DriveWriteForm, action: DriveWriteAction | 'write') => {
    if (!status || busy || pending || conflict) return;
    if (widens(status, form)) { setError(null); setPending({ form, action }); }
    else void save(form);
  };
  const run = async (operation: () => Promise<unknown>) => {
    if (lock.current) return;
    lock.current = true; setBusy(true); setError(null);
    try { await operation(); }
    catch (err) { setError(settingsRefusal(err, errorText(err))); }
    finally { lock.current = false; setBusy(false); }
  };
  const connect = () => {
    if (!dataAck || !writeAck || busy) return;
    const popup = window.open('about:blank', '_blank');
    if (popup) popup.opener = null;
    void run(async () => {
      try {
        const started = await api.googleDriveWrite.consentHomeStart({ redirect_origin: window.location.origin, acknowledge_google_data: true, acknowledge_write: true });
        const url = new URL(started.authorization_url);
        if (url.origin !== 'https://accounts.google.com' || url.username || url.password) throw new Error(t('invalidConsent'));
        setPaste(started.completion === 'paste'); setDataAck(false); setWriteAck(false);
        if (popup) popup.location.replace(url.href); else window.location.assign(url.href);
      } catch (err) { popup?.close(); throw err; }
    });
  };
  const disabled = busy || conflict || pending !== null || typeof status?.hash !== 'string' || !status.hash;
  const on = status ? ACTIONS.filter(action => status.effective[action]) : [];
  const integrationOff = !status?.integration_enabled || integrationEnabled === false;
  const logsPath = location.pathname.startsWith('/app/') ? '/app/system/logs' : '/logs';

  return <Card data-testid="google-drive-write"><CardContent className="space-y-5 py-5">
    <div><h2 className="font-medium">{t('title')}</h2><p className="mt-1 text-sm text-muted-foreground">{t('intro')}</p></div>
    {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
    {(conflict || !status) && <Button disabled={busy} variant="outline" onClick={() => void load()}>{t('reload')}</Button>}
    {status && <>
      <div className="space-y-1 text-sm" aria-live="polite">
        <p className="font-medium">{on.length && !integrationOff ? t('enabled', { actions: on.map(action => t(`action.${action}`)).join(intl.locale === 'en' ? ', ' : '、') }) : t('allOff')}</p>
        {status.ack && <p className="text-muted-foreground">{t('lastAck', { email: status.ack.by_email, time: formatDateTime(status.ack.at, { locale: intl.locale }) })}</p>}
        {status.integration_enabled && !on.length && ACTIONS.some(action => status.configured[action]) && <p className="text-warning">{t('signatureMismatch')}</p>}
        {integrationOff && <p className="text-warning">{t('integrationOff')}</p>}
      </div>
      <div className="divide-y divide-surface-border rounded-lg border border-surface-border">
        {ACTIONS.map(action => <div key={action} className="grid gap-3 p-4 lg:grid-cols-[1fr_19rem]">
          <div className="space-y-1">
            <div className="flex items-center gap-2 font-medium"><Switch nativeButton render={<button type="button" />} aria-label={t(`action.${action}`)} checked={!integrationOff && status.configured[action]} disabled={disabled || integrationOff}
              onCheckedChange={checked => change({ ...formOf(status), [action]: checked }, action)} />{t(`action.${action}`)}</div>
            <p className="text-sm text-muted-foreground">{t(`description.${action}`)}</p>
            {(action === 'move' || action === 'trash') && status.configured[action] && !status.folders.length && <p className="text-sm text-warning">{t('folderRequired')}</p>}
          </div>
          <label className="space-y-1 text-sm">{t('approvalLabel')}
            <select className={selectClass} aria-label={`${t(`action.${action}`)} · ${t('approvalLabel')}`} disabled={disabled || integrationOff} value={status.approval[action]}
              onChange={event => change({ ...formOf(status), approval: { ...status.approval, [action]: event.target.value as DriveWriteApproval } }, action)}>
              {MODES.map(mode => <option key={mode} value={mode}>{t(`approval.${mode}`)}</option>)}
            </select>
          </label>
        </div>)}
      </div>
      <section className="space-y-3" aria-label={t('folders')}>
        <h3 className="text-sm font-medium">{t('folders')}</h3>
        <form className="flex flex-wrap gap-2" onSubmit={event => { event.preventDefault(); if (folderInput.trim() && status.folders.length < 50) change({ ...formOf(status), folders: [...status.folders.map(folder => folder.id), folderInput.trim()] }, 'write'); }}>
          <Input aria-label={t('folderPlaceholder')} placeholder={t('folderPlaceholder')} className="min-w-48 flex-1" value={folderInput} disabled={disabled} onChange={event => setFolderInput(event.target.value)} />
          <Button variant="outline" type="submit" disabled={disabled || !folderInput.trim() || status.folders.length >= 50}>{t('add')}</Button>
        </form>
        <ul className="space-y-2">{status.folders.map(folder => {
          const name = folder.kind === 'unverified' || !folder.name ? t('unavailableName') : folder.name;
          return <li key={folder.id} className="flex flex-wrap items-center gap-2 text-sm"><span>{name}</span><Badge variant="ghost">{t(`folderKind.${folder.kind}`)}</Badge>
            <Button className="ml-auto" size="sm" variant="ghost" aria-label={t('removeFolder', { name })} disabled={disabled} onClick={() => change({ ...formOf(status), folders: status.folders.filter(item => item.id !== folder.id).map(item => item.id) }, 'write')}>{t('remove')}</Button></li>;
        })}</ul>
        <p className="text-xs text-muted-foreground">{t('folderCount', { count: status.folders.length })}</p>
        {!status.folders.length && <p className="text-sm text-muted-foreground">{t('emptyFolders')}</p>}
      </section>
      <section className="space-y-4 rounded-lg border border-surface-border p-4" aria-label={t('readiness')}>
        <h3 className="text-sm font-medium">{t('readiness')}</h3>
        <div className="space-y-2"><h4 className="text-sm font-medium">{t('delegation')}</h4><p className="text-sm text-muted-foreground">{t('delegationHint')}</p>
          <ul className="space-y-1 break-all text-xs text-muted-foreground">{SCOPES.map(scope => <li key={scope}>{scope}</li>)}</ul>
          <Button size="sm" variant="outline" disabled={busy} onClick={() => void run(async () => { await navigator.clipboard.writeText(SCOPES.join('\n')); setCopied(true); })}>{t('copyScopes')}</Button>
          {copied && <span role="status" className="ml-2 text-xs">{t('copied')}</span>}
          <div className="flex flex-wrap gap-2"><label className="min-w-48 flex-1 text-sm">{t('employee')}<select className={selectClass} aria-label={t('employee')} value={agentId} disabled={busy} onChange={event => { setAgentId(event.target.value); setStep(null); }}>
            <option value="">{t('pickEmployee')}</option>{agents.map(agent => <option key={agent.name} value={agent.name}>{agent.display_name || t('unnamedEmployee')}</option>)}
          </select></label><Button className="self-end" size="sm" variant="outline" disabled={busy || !agentId} onClick={() => void run(async () => { const result = await api.googleIdentity.test(agentId); const found = result.steps.find(item => item.id === 'drive_write'); setStep(found ?? null); if (!found) throw new Error(t('testUnavailable')); })}>{t('test')}</Button></div>
          {step && <div role="status" className="space-y-1 text-sm"><p>{step.label} · {t(step.ok ? 'testOk' : 'testFailed')}</p><p>{step.detail}</p></div>}
        </div>
        {status.home_write.route_oauth && <div className="space-y-2 border-t border-surface-border pt-3"><h4 className="text-sm font-medium">{t('home')}</h4>
          {status.home_write.connected ? <><p className="text-sm">{status.home_write.account_email}</p><Button variant="outline" size="sm" disabled={busy} onClick={() => setDisconnect(true)}>{t('disconnect')}</Button></> : <>
            <p className="text-sm text-muted-foreground">{t('homeHint')}</p>
            <GoogleDataDisclosure confirmed={dataAck} onConfirmedChange={setDataAck} />
            <label className="flex items-start gap-2 text-sm"><input type="checkbox" checked={writeAck} disabled={busy} onChange={event => setWriteAck(event.target.checked)} />{t('homeAck')}</label>
            <Button size="sm" disabled={busy || !dataAck || !writeAck} onClick={connect}>{t('consent')}</Button>
          </>}
          {paste && <form className="flex flex-wrap gap-2" onSubmit={event => { event.preventDefault(); void run(async () => { await api.googleDriveWrite.consentHomePaste(pasteUrl); accept(await api.googleDriveWrite.get()); setPaste(false); setPasteUrl(''); }); }}>
            <Input type="url" aria-label={t('pasteUrl')} placeholder={t('pasteUrl')} value={pasteUrl} onChange={event => setPasteUrl(event.target.value)} disabled={busy} />
            <Button type="submit" size="sm" disabled={busy || !pasteUrl.trim()}>{t('finishConsent')}</Button>
          </form>}
        </div>}
        <div className="space-y-1 border-t border-surface-border pt-3 text-sm"><h4 className="font-medium">{t('personal')}</h4><p className="text-muted-foreground">{status.write_store_unreadable ? t('connectionsUnreadable') : status.write_connections ? t('connectedCount', { count: status.write_connections.connected }) : t('connectionsUnreadable')}</p></div>
        <div className="space-y-1 border-t border-surface-border pt-3 text-sm"><h4 className="font-medium">{t('unsupportedRoutes')}</h4><p className="text-muted-foreground">{t('unsupportedHint')} <Badge variant="ghost">{t('unsupported')}</Badge></p></div>
      </section>
      <Link className="inline-block text-sm text-brand underline underline-offset-2" to={`${logsPath}?tab=history&tool_name_filter=google_drive_write`}>{t('viewLogs')}</Link>
    </>}
    <RiskConfirmDialog open={pending !== null} onClose={() => { if (!busy) setPending(null); }} onConfirm={() => { if (pending) void save(pending.form, true); }} busy={busy}
      title={t('riskTitle', { action: t(`action.${pending?.action ?? 'write'}`) })} body={<><p>{t('riskIntro')}</p><ul className="mt-2 list-disc space-y-2 pl-5">{[1, 2, 3, 4, 5, 6].map(point => <li key={point}>{t(`risk.${point}`)}</li>)}</ul></>}
      acknowledgeLabel={t('riskAck')} auditNote={t('riskAudit')} confirmLabel={t('enable')} />
    <ConfirmDialog open={disconnect} onClose={() => { if (!busy) setDisconnect(false); }} onConfirm={() => void run(async () => { await api.googleDriveWrite.consentHomeDisconnect(); accept(await api.googleDriveWrite.get()); setDisconnect(false); })}
      busy={busy} title={t('disconnect')} message={t('disconnectHint')} confirmLabel={t('disconnect')} />
  </CardContent></Card>;
}
