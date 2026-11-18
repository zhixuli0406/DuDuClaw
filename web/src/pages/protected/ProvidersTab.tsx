import { useCallback, useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { Plug } from 'lucide-react';
import { api, type ProtectedProviderEmployee, type ProtectedProviderInput, type ProtectedProviderProtocol,
  type ProtectedProviderResult, type ProtectedProviderStatus } from '@/lib/api';
import { toast } from '@/lib/toast';
import { useConnectionStore } from '@/stores/connection-store';
import { ActorAvatar, Badge, Button, Card, CardContent, CardDescription, CardHeader, CardTitle, Empty, Input,
  Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/mds';
import { StepUpDialog, useStepUp } from './step-up';
import { Refusal, TableBox, formatWhen, useT } from './shared';
import { PROVIDER_PROTOCOLS, providerErrorCode, providerErrorKey, validProviders } from './provider-form';

const blankProvider = (): ProtectedProviderInput => ({ name: '', base_url: '', api_key: '', protocol: 'chat_completions' });

/** Mounted only while editing: no keys survive close, submit or tab unmount. */
function ProviderSetDialog({ employee, onClose, onSubmit, errorMessage }: {
  employee: ProtectedProviderEmployee;
  onClose: () => void;
  onSubmit: (providers: ProtectedProviderInput[], csrf: number[]) => Promise<void>;
  errorMessage: (error: unknown) => string;
}) {
  const t = useT();
  const [providers, setProviders] = useState<ProtectedProviderInput[]>([blankProvider()]);
  const latest = useRef(providers);
  latest.current = providers;
  useEffect(() => () => { latest.current.forEach((entry) => { entry.api_key = ''; }); }, []);
  const update = (index: number, field: keyof ProtectedProviderInput, value: string) => {
    setProviders((current) => current.map((entry, i) => i === index ? { ...entry, [field]: value } : entry));
  };
  const valid = validProviders(providers);
  return (
    <StepUpDialog open idPrefix="provider-set" title={t('providers.setTitle', { name: employee.label })}
      description={t('providers.replace')} confirmLabel={t('providers.save')} verifyConfirmLabel={t('providers.verifySave')}
      disabled={!valid} errorMessage={errorMessage}
      onAttempt={() => setProviders((current) => current.map((entry) => ({ ...entry, api_key: '' })))}
      onOpenChange={(open) => { if (!open) onClose(); }}
      onConfirm={async (csrf) => {
        const entered = providers.map((entry) => ({ ...entry }));
        try { await onSubmit(entered, csrf); }
        finally { entered.forEach((entry) => { entry.api_key = ''; }); }
      }}>
      <p className="text-sm text-muted-foreground">{t('providers.reenter')}</p>
      {employee.providers.length > 0 && <div className="space-y-1 text-sm">
        <p className="font-medium">{t('providers.existing')}</p>
        {employee.providers.map((entry) => <p key={entry.name} dir="auto" className="break-all">{entry.name} · {entry.origin}</p>)}
      </div>}
      <div className="space-y-4">
        {providers.map((entry, index) => (
          <fieldset key={index} className="space-y-3 rounded-lg border border-surface-border p-3">
            <legend className="px-1 text-sm font-medium">{t('providers.entry', { number: index + 1 })}</legend>
            <div className="space-y-1">
              <label htmlFor={`provider-name-${index}`} className="block text-sm">{t('providers.name')}</label>
              <Input id={`provider-name-${index}`} required maxLength={128} autoComplete="off" value={entry.name}
                onChange={(event) => update(index, 'name', event.target.value)} />
            </div>
            <div className="space-y-1">
              <label htmlFor={`provider-url-${index}`} className="block text-sm">{t('providers.endpoint')}</label>
              <Input id={`provider-url-${index}`} type="url" required autoComplete="off" value={entry.base_url}
                onChange={(event) => update(index, 'base_url', event.target.value)} />
            </div>
            <div className="space-y-1">
              <label htmlFor={`provider-key-${index}`} className="block text-sm">{t('providers.apiKey')}</label>
              <Input id={`provider-key-${index}`} type="password" required autoComplete="off" value={entry.api_key}
                onChange={(event) => update(index, 'api_key', event.target.value)} />
            </div>
            <div className="space-y-1">
              <label htmlFor={`provider-protocol-${index}`} className="block text-sm">{t('providers.protocol')}</label>
              <select id={`provider-protocol-${index}`} value={entry.protocol}
                className="h-8 w-full rounded-lg border border-input bg-transparent px-2 text-sm outline-none focus-visible:ring-3 focus-visible:ring-ring/50 dark:bg-input/30"
                onChange={(event) => update(index, 'protocol', event.target.value as ProtectedProviderProtocol)}>
                {PROVIDER_PROTOCOLS.map((protocol) => <option key={protocol} value={protocol}>{t(`providers.protocol.${protocol}`)}</option>)}
              </select>
            </div>
            <Button size="sm" variant="ghost" disabled={providers.length === 1}
              aria-label={t('providers.removeNamed', { number: index + 1 })}
              onClick={() => setProviders((current) => {
                current[index].api_key = '';
                return current.filter((_, i) => i !== index);
              })}>{t('providers.remove')}</Button>
          </fieldset>
        ))}
        <Button size="sm" variant="outline" disabled={providers.length >= 32}
          onClick={() => setProviders((current) => [...current, blankProvider()])}>{t('providers.add')}</Button>
      </div>
      {!valid && <p className="text-xs text-muted-foreground">{t('providers.rules')}</p>}
      <p className="text-sm text-muted-foreground">{t('providers.risk')}</p>
    </StepUpDialog>
  );
}

export function ProvidersTab() {
  const t = useT();
  const intl = useIntl();
  const connection = useConnectionStore((state) => state.state);
  const { status, record } = useStepUp();
  const [data, setData] = useState<ProtectedProviderStatus | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<{ message: string; warning: boolean } | null>(null);
  const [editing, setEditing] = useState<ProtectedProviderEmployee | null>(null);
  const [clearing, setClearing] = useState<ProtectedProviderEmployee | null>(null);
  const [verification, setVerification] = useState<{ employee?: ProtectedProviderEmployee; reveal?: boolean } | null>(null);
  const revealToken = useRef<number[] | null>(null);
  const generation = useRef(0);
  const describe = useCallback((failure: unknown) => {
    const code = providerErrorCode(failure);
    return t(providerErrorKey(code), { code: code === 'unknown' ? t('providers.unknownCode') : code });
  }, [t]);
  const invalidate = useCallback((failure: unknown) => {
    if (['step_up_required', 'session_expired'].includes(providerErrorCode(failure))) {
      record({ step_up_valid: false, csrf_token: null, mfa_enrolled: status.mfaEnrolled });
      revealToken.current = null;
      setData(null);
    }
  }, [record, status.mfaEnrolled]);

  const load = useCallback(async (after?: string, csrf = revealToken.current) => {
    const current = ++generation.current;
    setLoading(true);
    try {
      const result = await api.protectedMode.providerStatus({ ...(after ? { after } : {}), ...(csrf ? { step_up_csrf: csrf } : {}) });
      if (current !== generation.current) return;
      record(result);
      setData((previous) => after && previous ? { ...result, employees: [...previous.employees, ...result.employees] } : result);
      setError(null);
    } catch (failure) {
      if (current !== generation.current) return;
      invalidate(failure);
      if (['step_up_required', 'session_expired'].includes(providerErrorCode(failure))) setVerification({});
      setError(describe(failure));
    } finally { if (current === generation.current) setLoading(false); }
  }, [record, describe, invalidate]);

  useEffect(() => {
    if (connection === 'authenticated') void load();
    return () => { generation.current += 1; };
  }, [connection, load]);

  const completed = async (result: ProtectedProviderResult) => {
    const warning = result.result === 'not_recorded';
    const message = t(warning ? 'providers.notRecorded' : 'providers.saved');
    setNotice({ message, warning });
    if (warning) toast.info(message); else toast.success(message);
    await load();
  };
  const guarded = async <T,>(action: () => Promise<T>): Promise<T> => {
    try { return await action(); }
    catch (failure) { invalidate(failure); throw failure; }
  };

  return <div className="space-y-6">
    <Refusal message={error} />
    <div aria-live="polite">
      {notice && <p role="status" className={`rounded-lg border px-3 py-2 text-sm ${notice.warning
        ? 'border-warning/30 bg-warning/10' : 'border-success/30 bg-success/10'}`}>{notice.message}</p>}
    </div>
    <Card>
      <CardHeader><CardTitle><h2>{t('providers.title')}</h2></CardTitle><CardDescription>{t('providers.description')}</CardDescription></CardHeader>
      <CardContent className="space-y-4">
        <p className="text-sm text-muted-foreground">{t('providers.visibility.dashboard_changes_only')}</p>
        <div className="flex flex-wrap items-center justify-between gap-2">
          {data && <p className="text-sm text-muted-foreground">{t('providers.hidden', { count: data.hidden_personal })}</p>}
          <div className="flex gap-2">
            {data && data.hidden_personal > 0 && !data.personal_revealed && <Button size="sm" variant="outline"
              disabled={loading} onClick={() => setVerification({ reveal: true })}>{t('providers.reveal')}</Button>}
            <Button size="sm" variant="outline" disabled={loading} onClick={() => void load()}>{t('providers.refresh')}</Button>
          </div>
        </div>
        {loading && <p role="status" className="text-sm text-muted-foreground">{t('loading')}</p>}
        {data?.employees.length === 0 && <Empty variant="dashed" icon={Plug} title={t('providers.empty')} />}
        {data && data.employees.length > 0 && <TableBox label={t('providers.title')}><Table>
          <TableHeader><TableRow>
            {['employee', 'kind', 'provisioned', 'connections', 'updated'].map((key) => <TableHead key={key} scope="col">{t(`providers.col.${key}`)}</TableHead>)}
            <TableHead scope="col">{t('col.action')}</TableHead>
          </TableRow></TableHeader>
          <TableBody>{data.employees.map((employee) => <TableRow key={employee.employee_id}>
            <TableCell><span className="inline-flex items-center gap-2"><ActorAvatar actorType="agent" size="sm" /><span dir="auto">{employee.label}</span></span></TableCell>
            <TableCell><Badge variant="secondary">{t(`providers.kind.${employee.kind}`)}</Badge></TableCell>
            <TableCell>{t(employee.provisioned ? 'providers.yes' : 'providers.no')}</TableCell>
            <TableCell><div className="space-y-1">{employee.providers.length ? employee.providers.map((entry) => <p key={entry.name} dir="auto" className="break-all text-sm">
              {entry.name} · {t(`providers.protocol.${entry.protocol}`)} · {entry.origin}
            </p>) : t('providers.none')}</div></TableCell>
            <TableCell className="font-mono tabular-nums">{employee.updated_unix === undefined ? t('providers.none')
              : formatWhen(new Date(employee.updated_unix * 1000).toISOString(), intl.locale)}</TableCell>
            <TableCell><div className="flex gap-2">
              <Button size="sm" variant="outline" disabled={loading} aria-label={t('providers.setNamed', { name: employee.label })}
                onClick={() => { if (status.valid) setEditing(employee); else setVerification({ employee }); }}>{t('providers.set')}</Button>
              <Button size="sm" variant="destructive" disabled={loading} aria-label={t('providers.clearNamed', { name: employee.label })}
                onClick={() => setClearing(employee)}>{t('providers.clear')}</Button>
            </div></TableCell>
          </TableRow>)}</TableBody>
        </Table></TableBox>}
        {data?.next && <Button size="sm" variant="outline" disabled={loading}
          onClick={() => void load(data.next!)}>{t('providers.more')}</Button>}
      </CardContent>
    </Card>
    {verification && <StepUpDialog open idPrefix="provider-verify" title={t('providers.verifyTitle')}
      description={t('providers.verifyDescription')} confirmLabel={t('providers.continue')} verifyConfirmLabel={t('providers.verifyContinue')}
      errorMessage={describe} onOpenChange={(open) => { if (!open) setVerification(null); }} onConfirm={async (csrf) => {
        if (verification.employee) {
          const employee = verification.employee;
          const result = await guarded(() => api.protectedMode.providerStatus({ employee_id: employee.employee_id, step_up_csrf: csrf }));
          record(result);
          if (!result.step_up_valid || !result.csrf_token) throw { code: 'step_up_required' };
          const current = result.employees.find((entry) => entry.employee_id === employee.employee_id);
          if (!current) throw { code: 'not_allocated' };
          setEditing(current);
        } else {
          // Explicitly audited reveal; ordinary lists never attach a proof implicitly.
          const result = await guarded(() => api.protectedMode.providerStatus(verification.reveal ? { step_up_csrf: csrf } : {}));
          record(result);
          revealToken.current = verification.reveal ? csrf : null;
          setData(result);
          setError(null);
        }
      }} />}
    {editing && <ProviderSetDialog employee={editing} onClose={() => setEditing(null)} errorMessage={describe}
      onSubmit={async (providers, csrf) => {
        const result = await guarded(() => api.protectedMode.providerSet(editing.employee_id, providers, csrf));
        await completed(result);
      }} />}
    {clearing && <StepUpDialog open destructive idPrefix="provider-clear" title={t('providers.clearTitle', { name: clearing.label })}
      description={t('providers.clearWarning')} confirmLabel={t('providers.clear')} verifyConfirmLabel={t('providers.verifyClear')}
      errorMessage={describe} onOpenChange={(open) => { if (!open) setClearing(null); }} onConfirm={async (csrf) => {
        const result = await guarded(() => api.protectedMode.providerClear(clearing.employee_id, csrf));
        await completed(result);
      }} />}
  </div>;
}
