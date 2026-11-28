import { useEffect, useRef, useState, type ReactNode } from 'react';
import { useIntl } from 'react-intl';
import { Link, useLocation, useNavigate } from 'react-router';
import { Button, Input } from '@/components/mds';
import { OrgChangeFacts } from '@/components/inbox/OrgChangeFacts';
import { WorkflowActivationFacts } from '@/components/inbox/WorkflowActivationFacts';
import { AgentFileChangeFacts } from '@/components/inbox/AgentFileChangeFacts';
import { AdminAiSettingsFacts, parseAdminAiSettings } from '@/components/inbox/AdminAiSettingsView';
import { approvalStepUp, ApprovalStepUpError, type ApprovalStepUpPreview, type ApprovalDecision } from '@/lib/approval-step-up';
import { stepUpApi } from '@/lib/step-up';
import { api } from '@/lib/api';
import { useAuthStore } from '@/stores/auth-store';
import { useAgentsStore } from '@/stores/agents-store';

export function ApprovalConfirmPage() {
  const intl = useIntl();
  const t = (key: string, values?: Record<string, string | number>) => intl.formatMessage({ id: `approvalConfirm.${key}` }, values);
  const agents = useAgentsStore(s => s.agents);
  const fetchAgents = useAgentsStore(s => s.fetchAgents);
  const resolveAgentName = (id: string) => agents.find(agent => agent.name === id)?.display_name?.trim()
    || t('unknownAgent');
  const location = useLocation();
  const navigate = useNavigate();
  const [ticket] = useState(() => location.hash.length === 72 && /^#ticket=[a-f0-9]{64}$/.test(location.hash) ? location.hash.substring(8) : '');
  const [preview, setPreview] = useState<ApprovalStepUpPreview>();
  const [error, setError] = useState<ApprovalStepUpError>();
  const [password, setPassword] = useState('');
  const [code, setCode] = useState('');
  const [busy, setBusy] = useState(false);
  const [done, setDone] = useState<ApprovalDecision>();
  const [display, setDisplay] = useState('');
  const [now, setNow] = useState(Date.now());
  const locked = useRef(false);
  const request = useRef<Promise<ApprovalStepUpPreview> | null>(null);

  useEffect(() => {
    // Drop the fragment before any request, including in StrictMode.
    window.history.replaceState(null, '', window.location.pathname);
    if (!ticket) { setError(new ApprovalStepUpError('not_found', '')); return; }
    let active = true;
    request.current ??= approvalStepUp.preview(ticket);
    void request.current.then(value => { if (active) setPreview(value); })
      .catch(value => { if (active) setError(asError(value)); });
    return () => { active = false; };
  }, [ticket]);
  useEffect(() => {
    if (preview) void fetchAgents();
  }, [preview, fetchAgents]);
  useEffect(() => {
    if (!preview || done) return;
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [preview, done]);

  const remaining = preview ? Math.max(0, Math.ceil(preview.expires - now / 1000)) : 0;
  const expired = !!preview && remaining === 0;
  const terminal = error && error.code !== 'step_up_required' && error.code !== 'verification_failed';
  const login = () => {
    void useAuthStore.getState().logout();
    navigate('/login', { replace: true, state: { from: `/approvals/confirm#ticket=${ticket}` } });
  };
  async function decide(decision: ApprovalDecision) {
    if (!preview || locked.current || terminal || expired) return;
    locked.current = true;
    setBusy(true);
    setError(undefined);
    let posting = false;
    try {
      const token = decision === 'approve' ? await stepUpApi.adminVerify(password, code) : undefined;
      // Credentials never survive a verification attempt, including a failed one.
      setPassword(''); setCode('');
      posting = true;
      const result = await approvalStepUp.decide(ticket, decision, preview.digest, token);
      setDone(result.decision);
      // The POST's decided_by is an internal identifier, never display it.
      void api.approvals.get(preview.approval.id).then(result => {
        const value = result.approvals.find(item => item.id === preview.approval.id)?.decided_by_display;
        if (value) setDisplay(value);
      }).catch(() => undefined);
    } catch (value) {
      setError(posting ? asError(value) : new ApprovalStepUpError('verification_failed', ''));
    } finally {
      setPassword(''); setCode('');
      setBusy(false);
      locked.current = false;
    }
  }
  let message = error?.sentence || t('unavailable');
  if (error?.code === 'not_found') message = t('notFound');
  else if (error?.code === 'digest_mismatch') message = t('changed');
  else if (error?.code === 'already_decided') message = t('alreadyDecided');
  else if (error?.code === 'expired' || error?.code === 'used' || expired) message = t('expired');
  else if (error?.code === 'unauthenticated') message = t('loginAgain');
  else if (error?.code === 'step_up_required' || error?.code === 'verification_failed') message = t('verifyAgain');

  const approval = preview?.approval;
  const settings = approval?.kind === 'admin_ai_settings' ? parseAdminAiSettings(approval.payload) : null;
  const payload = payloadValue(approval?.payload);
  const agentId = approval?.agent_id || preview?.agent_file_change?.agent_id
    || (payload && typeof payload === 'object' && 'agent_id' in payload && typeof payload.agent_id === 'string' ? payload.agent_id : '');
  const evidence = evidenceView(payload);
  return <div className="mx-auto w-full max-w-md space-y-5 p-4 sm:p-6">
    <h1 className="text-xl font-semibold">{t('title')}</h1>
    {done ? <div className="space-y-4" role="status">
      <h2 className="break-words text-lg font-medium">{t(done === 'approve' ? 'approved' : 'rejected', { kind: preview!.kind_label })}{display && t('attribution', { name: display })}</h2>
      <p className="text-sm">{t('updated')}</p>
    </div> : <>
      {(error || expired) && <p role="alert" className="whitespace-pre-wrap break-words text-sm">{message}</p>}
      {error?.code === 'unauthenticated' && <Button onClick={login}>{t('login')}</Button>}
      {error?.code === 'not_found' && <Button variant="outline" onClick={() => void useAuthStore.getState().logout()}>{t('logout')}</Button>}
      {!terminal && !expired && preview && approval && <>
        <p className="text-sm tabular-nums text-muted-foreground">{t('remaining', { time: `${Math.floor(remaining / 60)}:${String(remaining % 60).padStart(2, '0')}` })}</p>
        <section className="space-y-3 rounded-lg border border-border bg-card p-4">
          <h2 className="font-medium">{preview.kind_label}</h2>
          <p className="whitespace-pre-wrap break-words text-sm">{approval.summary}</p>
          <h3 className="text-sm font-medium">{t('content')}</h3>
          {approval.kind === 'org_change' && <OrgChangeFacts approval={approval} agentName={resolveAgentName(agentId)} resolveAgentName={resolveAgentName} />}
          {approval.kind === 'workflow_activation' && <WorkflowActivationFacts approval={approval} />}
          {approval.kind === 'agent_file_change' && <AgentFileChangeFacts approval={approval} agentName={resolveAgentName(agentId)} preview={preview.agent_file_change} />}
          {approval.kind === 'admin_ai_settings' && (settings ? <AdminAiSettingsFacts payload={settings} /> : <p className="text-sm">{t('inboxContent')}</p>)}
          {!['org_change', 'workflow_activation', 'agent_file_change', 'admin_ai_settings'].includes(approval.kind)
            && (evidence ?? <p className="text-sm">{t('inboxContent')}</p>)}
        </section>
        <section className="space-y-4 border-t border-border pt-4">
          {preview.has_totp ? <form className="space-y-4" onSubmit={e => { e.preventDefault(); void decide('approve'); }}>
            <h2 className="font-medium">{t('identity')}</h2>
            <label className="block space-y-2 text-sm"><span>{t('password')}</span><Input className="min-h-11" type="password" autoComplete="current-password" value={password} onChange={e => setPassword(e.target.value)} disabled={busy} required /></label>
            <label className="block space-y-2 text-sm"><span>{t('code')}</span><Input className="min-h-11" inputMode="numeric" autoComplete="one-time-code" pattern="[0-9]{6}" maxLength={6} value={code} onChange={e => setCode(e.target.value)} disabled={busy} required /></label>
            <Button type="submit" className="min-h-11 w-full" disabled={busy || !password || !/^[0-9]{6}$/.test(code)}>{t('approve')}</Button>
          </form> : <p className="text-sm">{t('noTotp')}</p>}
          <Button type="button" variant="outline" className="min-h-11 w-full" disabled={busy} onClick={() => void decide('reject')}>{t('reject')}</Button>
          <p className="text-sm text-muted-foreground">{t('rejectNote')}</p>
        </section>
      </>}
      {!preview && !error && <p role="status">{t('loading')}</p>}
    </>}
    <Link to="/inbox" className="inline-flex min-h-11 items-center text-sm underline">{t('inbox')}</Link>
  </div>;
}
function asError(value: unknown): ApprovalStepUpError {
  return value instanceof ApprovalStepUpError ? value : new ApprovalStepUpError('unavailable', '');
}

function payloadValue(value: unknown): unknown {
  if (typeof value !== 'string') return value;
  try { return JSON.parse(value); } catch { return value; }
}

// Omit internal locators at every depth, including UUIDs embedded in text.
const uuid = /[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}/i;
function evidenceView(value: unknown): ReactNode {
  if (value === null || value === undefined) return null;
  if (typeof value === 'object') {
    const rows = Object.entries(value).flatMap(([key, field]) => {
      if (['id', 'target', 'binding', 'payload_hash', 'digest', 'hash'].includes(key)
        || /_ids?$/.test(key) || uuid.test(key)) return [];
      const content = evidenceView(field);
      if (content === null) return [];
      return [<div key={key} className="space-y-1">
        <dt className="break-words text-muted-foreground">{key}</dt>
        <dd className="ml-4 min-w-0">{content}</dd>
      </div>];
    });
    return rows.length ? <dl className="space-y-2 text-sm">{rows}</dl> : null;
  }
  if (typeof value !== 'string' && typeof value !== 'number' && typeof value !== 'boolean') return null;
  if (typeof value === 'string') {
    if (!value.trim() || new RegExp(`^${uuid.source}$`, 'i').test(value.trim())) return null;
    value = value.replace(new RegExp(uuid.source, 'gi'), '…');
  }
  return <span className="whitespace-pre-wrap break-words text-sm [overflow-wrap:anywhere]">{String(value)}</span>;
}
