import { useEffect, useMemo, useState, type FormEvent } from 'react';
import {
  api,
  type DbSourceGrantSource,
  type ProtectedConnectorKind,
  type ProtectedConnectorProvisionInput,
  type ProtectedEmployee,
  type ProtectedWebsiteCredential,
} from '@/lib/api';
import {
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  Input,
} from '@/components/mds';
import { cn } from '@/lib/utils';
import { StepUpFields, useStepUpAction } from './step-up';
import {
  CONNECTOR_PROVIDERS,
  DETAIL_PROVIDERS,
  Refusal,
  refusalMessage,
  usePersonName,
  useProviderName,
  useOptionalT,
  useT,
  type WizardPreset,
} from './shared';

const STEPS = ['employee', 'service', 'reference', 'preview'] as const;
type Step = (typeof STEPS)[number];

/** Channels a protected route can name; the server validates the rest. */
const CHANNELS = ['telegram', 'line', 'discord', 'slack', 'whatsapp', 'feishu', 'wecom', 'dingtalk', 'googlechat', 'teams'] as const;

type TokenRef = 'agent' | 'home' | 'none';

function isDetailProvider(provider: string): boolean {
  return (DETAIL_PROVIDERS as readonly string[]).includes(provider);
}

/**
 * 「新增連接器」: 選員工 → 選服務 → 填參照 → 預覽並再驗證身分.
 *
 * Only references to sources already registered on this install are offered
 * (database sources the employee is granted, website sign-ins whose allowed
 * employees include it, a channel scope and where its token is read from).
 * There is no field for a password, token or URL.
 */
export function ConnectorWizard({
  open,
  onOpenChange,
  employees,
  preset,
  onCreated,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  employees: ProtectedEmployee[];
  preset?: WizardPreset;
  onCreated: (summary: string) => Promise<void> | void;
}) {
  const t = useT();
  const optional = useOptionalT();
  const personName = usePersonName();
  const providerName = useProviderName();
  const action = useStepUpAction();
  const live = useMemo(() => employees.filter((employee) => !employee.disabled), [employees]);
  const [step, setStep] = useState<Step>('employee');
  const [employeeLabel, setEmployeeLabel] = useState('');
  const [provider, setProvider] = useState('');
  const [label, setLabel] = useState('');
  const [sharedKind, setSharedKind] = useState<Exclude<ProtectedConnectorKind, 'personal'>>('shared_home');
  const [source, setSource] = useState('');
  const [credential, setCredential] = useState('');
  const [channel, setChannel] = useState<string>(CHANNELS[0]);
  const [scope, setScope] = useState('');
  const [tokenRef, setTokenRef] = useState<TokenRef>('agent');
  const [dbSources, setDbSources] = useState<DbSourceGrantSource[] | null>(null);
  const [credentials, setCredentials] = useState<ProtectedWebsiteCredential[] | null>(null);
  const [referenceError, setReferenceError] = useState<string | null>(null);

  // Start from the preset each time the wizard opens.
  useEffect(() => {
    if (!open) return;
    const wantedEmployee = preset?.employee ?? '';
    const wantedProvider = preset?.provider ?? '';
    const presetEmployee = wantedEmployee && live.some((e) => e.label === wantedEmployee) ? wantedEmployee : '';
    const presetProvider = (CONNECTOR_PROVIDERS as readonly string[]).includes(wantedProvider) ? wantedProvider : '';
    setEmployeeLabel(presetEmployee);
    setProvider(presetProvider);
    setLabel(preset?.label ?? '');
    setSharedKind(presetProvider === 'google' ? 'shared_tenant' : 'shared_home');
    setSource('');
    setCredential('');
    setChannel(CHANNELS[0]);
    setScope('');
    setTokenRef('agent');
    setReferenceError(null);
    // Re-read the registered sources each time: grants may have changed.
    setDbSources(null);
    setCredentials(null);
    action.reset();
    setStep(presetEmployee ? (presetProvider ? 'reference' : 'service') : 'employee');
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  const employee = live.find((e) => e.label === employeeLabel) ?? null;
  const ownerId = employee && employee.binding.type === 'personal' ? employee.binding.owner_user_id : null;
  const personal = ownerId !== null;
  const kind: ProtectedConnectorKind = personal ? 'personal' : sharedKind;
  const channelName = (name: string) => optional(`channel.${name}`, name);

  // Load the registered references the chosen service can point at.
  useEffect(() => {
    if (!open || step !== 'reference') return;
    let cancelled = false;
    if (provider === 'database' && dbSources === null) {
      api.dbSources.grants.list()
        .then((result) => { if (!cancelled) setDbSources(result.sources); })
        .catch((e) => { if (!cancelled) { setDbSources([]); setReferenceError(refusalMessage(e)); } });
    }
    if (provider === 'computer' && credentials === null) {
      api.protectedMode.websiteCredentials()
        .then((result) => { if (!cancelled) setCredentials(result.credentials); })
        .catch((e) => { if (!cancelled) { setCredentials([]); setReferenceError(refusalMessage(e)); } });
    }
    return () => { cancelled = true; };
  }, [open, step, provider, dbSources, credentials]);

  const grantedSources = (dbSources ?? []).filter((s) => s.agents.includes(employeeLabel));
  const grantedCredentials = (credentials ?? []).filter((c) => c.allowed_agents.includes(employeeLabel));
  const tokenOptions: TokenRef[] = personal ? ['agent'] : ['agent', 'home', 'none'];

  const referenceComplete = (() => {
    const name = label.trim();
    if (name.length === 0 || name.length > 64) return false;
    if (provider === 'database') return grantedSources.some((s) => s.name === source);
    if (provider === 'computer') return grantedCredentials.some((c) => c.id === credential);
    if (provider === 'channel') return scope.trim().length > 0 && tokenOptions.includes(tokenRef);
    return true;
  })();

  const canNext =
    (step === 'employee' && employee !== null) ||
    (step === 'service' && provider !== '') ||
    (step === 'reference' && referenceComplete);

  const request = (): ProtectedConnectorProvisionInput | null => {
    if (!employee) return null;
    const base: ProtectedConnectorProvisionInput = {
      provider,
      kind,
      binding: ownerId !== null ? 'personal' : 'shared',
      owner_user_id: ownerId ?? undefined,
      label: label.trim(),
    };
    if (!isDetailProvider(provider)) return base;
    if (provider === 'database') return { ...base, details: { employee: employee.label, source } };
    if (provider === 'computer') return { ...base, details: { employee: employee.label, credential } };
    return { ...base, details: { employee: employee.label, channel, scope: scope.trim(), token_ref: tokenRef } };
  };

  const referenceSummary = (): string => {
    if (provider === 'database') return grantedSources.find((s) => s.name === source)?.label || source;
    if (provider === 'computer') return grantedCredentials.find((c) => c.id === credential)?.label || '';
    if (provider === 'channel') {
      return t('wizard.channelSummary', { channel: channelName(channel), scope: scope.trim(), token: t(`wizard.token.${tokenRef}`) });
    }
    return t('wizard.noReference');
  };

  const index = STEPS.indexOf(step);
  const back = () => setStep(STEPS[Math.max(0, index - 1)]);
  const next = () => { if (canNext) setStep(STEPS[Math.min(STEPS.length - 1, index + 1)]); };

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (step !== 'preview') { next(); return; }
    const input = request();
    if (!input || !action.canSubmit) return;
    const summary = `${providerName(provider)} · ${input.label}`;
    const ok = await action.run(async (csrf) => {
      await api.protectedMode.provision(input, csrf);
    });
    if (ok) {
      onOpenChange(false);
      await onCreated(summary);
    }
  };

  const radioRow = (checked: boolean) =>
    cn('flex cursor-pointer items-start gap-3 rounded-lg border px-3 py-2 text-sm', checked ? 'border-brand bg-brand/5' : 'border-surface-border hover:bg-muted/50');

  return (
    <Dialog open={open} onOpenChange={(isOpen) => { if (!isOpen) action.reset(); onOpenChange(isOpen); }}>
      <DialogContent className="max-h-[90vh] overflow-y-auto sm:max-w-xl">
        <form onSubmit={(e) => void submit(e)} className="grid gap-4">
          <DialogHeader>
            <DialogTitle>{t('wizard.title')}</DialogTitle>
            <DialogDescription>{t('wizard.description')}</DialogDescription>
          </DialogHeader>

          <ol className="flex flex-wrap gap-2 text-xs" aria-label={t('wizard.progress')}>
            {STEPS.map((name, i) => (
              <li
                key={name}
                aria-current={name === step ? 'step' : undefined}
                className={cn('rounded-full border px-2.5 py-1', name === step ? 'border-brand bg-brand/10 font-medium text-foreground' : i < index ? 'border-surface-border text-foreground' : 'border-surface-border text-muted-foreground')}
              >
                {t('wizard.stepN', { n: i + 1 })} {t(`wizard.step.${name}`)}
              </li>
            ))}
          </ol>

          {step === 'employee' && (
            <fieldset className="space-y-2">
              <legend className="mb-1 text-sm font-medium">{t('wizard.pickEmployee')}</legend>
              {live.length === 0 && <p className="text-sm text-muted-foreground">{t('wizard.noEmployees')}</p>}
              {live.map((e) => (
                <label key={e.employee_id} className={radioRow(employeeLabel === e.label)}>
                  <input type="radio" name="wizard-employee" className="mt-1 accent-brand" checked={employeeLabel === e.label} onChange={() => setEmployeeLabel(e.label)} />
                  <span>
                    <span className="font-medium">{e.label}</span>
                    <span className="block text-xs text-muted-foreground">
                      {e.binding.type === 'personal'
                        ? t('group.personal', { owner: personName(e.binding.owner_user_id) })
                        : t('group.shared')}
                    </span>
                  </span>
                </label>
              ))}
            </fieldset>
          )}

          {step === 'service' && (
            <fieldset className="grid gap-2 sm:grid-cols-2">
              <legend className="mb-1 text-sm font-medium">{t('wizard.pickService')}</legend>
              {CONNECTOR_PROVIDERS.map((p) => (
                <label key={p} className={radioRow(provider === p)}>
                  <input
                    type="radio"
                    name="wizard-provider"
                    className="mt-1 accent-brand"
                    checked={provider === p}
                    onChange={() => { setProvider(p); setReferenceError(null); if (!personal) setSharedKind(p === 'google' ? 'shared_tenant' : 'shared_home'); }}
                  />
                  <span>
                    <span className="font-medium">{providerName(p)}</span>
                    <span className="block text-xs text-muted-foreground">{t(`wizard.serviceHint.${p}`)}</span>
                  </span>
                </label>
              ))}
            </fieldset>
          )}

          {step === 'reference' && (
            <div className="space-y-4">
              <p className="rounded-lg bg-muted px-3 py-2 text-xs text-muted-foreground">{t('wizard.noSecrets')}</p>
              <div className="space-y-1">
                <label htmlFor="wizard-label" className="block text-sm font-medium">{t('wizard.accountName')}</label>
                <Input id="wizard-label" maxLength={64} value={label} aria-describedby="wizard-label-hint" onChange={(e) => setLabel(e.target.value)} />
                <p id="wizard-label-hint" className="text-xs text-muted-foreground">{t('wizard.accountNameHint')}</p>
              </div>

              {personal ? (
                <p className="text-sm">{t('wizard.personalSource', { owner: personName(ownerId) })}</p>
              ) : (
                <fieldset className="space-y-2">
                  <legend className="mb-1 text-sm font-medium">{t('wizard.sharedSource')}</legend>
                  {(['shared_tenant', 'shared_home'] as const).map((k) => (
                    <label key={k} className={radioRow(sharedKind === k)}>
                      <input type="radio" name="wizard-kind" className="mt-1 accent-brand" checked={sharedKind === k} onChange={() => setSharedKind(k)} />
                      <span>
                        <span className="font-medium">{t(`kind.${k}`)}</span>
                        <span className="block text-xs text-muted-foreground">{t(`kind.${k}.hint`)}</span>
                      </span>
                    </label>
                  ))}
                </fieldset>
              )}

              {provider === 'database' && (
                <div className="space-y-1">
                  <label htmlFor="wizard-source" className="block text-sm font-medium">{t('wizard.dbSource')}</label>
                  {dbSources === null ? (
                    <p role="status" className="text-sm text-muted-foreground">{t('loading')}</p>
                  ) : grantedSources.length === 0 ? (
                    <p className="text-sm text-muted-foreground">{t('wizard.dbSourceNone')}</p>
                  ) : (
                    <select id="wizard-source" className="h-8 w-full rounded-lg border border-input bg-transparent px-2 text-sm dark:bg-input/30" value={source} onChange={(e) => setSource(e.target.value)}>
                      <option value="">{t('wizard.choose')}</option>
                      {grantedSources.map((s) => <option key={s.name} value={s.name}>{s.label || s.name}</option>)}
                    </select>
                  )}
                </div>
              )}

              {provider === 'computer' && (
                <div className="space-y-1">
                  <label htmlFor="wizard-credential" className="block text-sm font-medium">{t('wizard.websiteLogin')}</label>
                  {credentials === null ? (
                    <p role="status" className="text-sm text-muted-foreground">{t('loading')}</p>
                  ) : grantedCredentials.length === 0 ? (
                    <p className="text-sm text-muted-foreground">{t('wizard.websiteLoginNone')}</p>
                  ) : (
                    <select id="wizard-credential" className="h-8 w-full rounded-lg border border-input bg-transparent px-2 text-sm dark:bg-input/30" value={credential} onChange={(e) => setCredential(e.target.value)}>
                      <option value="">{t('wizard.choose')}</option>
                      {grantedCredentials.map((c) => <option key={c.id} value={c.id}>{c.label}</option>)}
                    </select>
                  )}
                </div>
              )}

              {provider === 'channel' && (
                <div className="space-y-3">
                  <div className="space-y-1">
                    <label htmlFor="wizard-channel" className="block text-sm font-medium">{t('wizard.channel')}</label>
                    <select id="wizard-channel" className="h-8 w-full rounded-lg border border-input bg-transparent px-2 text-sm dark:bg-input/30" value={channel} onChange={(e) => setChannel(e.target.value)}>
                      {CHANNELS.map((c) => <option key={c} value={c}>{channelName(c)}</option>)}
                    </select>
                  </div>
                  <div className="space-y-1">
                    <label htmlFor="wizard-scope" className="block text-sm font-medium">{t('wizard.scope')}</label>
                    <Input id="wizard-scope" maxLength={256} value={scope} aria-describedby="wizard-scope-hint" onChange={(e) => setScope(e.target.value)} />
                    <p id="wizard-scope-hint" className="text-xs text-muted-foreground">{t('wizard.scopeHint')}</p>
                  </div>
                  <fieldset className="space-y-2">
                    <legend className="mb-1 text-sm font-medium">{t('wizard.tokenSource')}</legend>
                    {tokenOptions.map((ref) => (
                      <label key={ref} className={radioRow(tokenRef === ref)}>
                        <input type="radio" name="wizard-token" className="mt-1 accent-brand" checked={tokenRef === ref} onChange={() => setTokenRef(ref)} />
                        <span>{t(`wizard.token.${ref}`)}</span>
                      </label>
                    ))}
                  </fieldset>
                </div>
              )}
              <Refusal message={referenceError} />
            </div>
          )}

          {step === 'preview' && employee && (
            <div className="space-y-4">
              <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1.5 text-sm">
                <dt className="text-muted-foreground">{t('wizard.preview.employee')}</dt>
                <dd>{employee.label}</dd>
                <dt className="text-muted-foreground">{t('wizard.preview.service')}</dt>
                <dd>{providerName(provider)}</dd>
                <dt className="text-muted-foreground">{t('wizard.preview.account')}</dt>
                <dd dir="auto" className="[unicode-bidi:isolate]">{label.trim()}</dd>
                <dt className="text-muted-foreground">{t('wizard.preview.owner')}</dt>
                <dd>{ownerId !== null ? t('group.personal', { owner: personName(ownerId) }) : t('group.shared')}</dd>
                <dt className="text-muted-foreground">{t('wizard.preview.source')}</dt>
                <dd>{t(`kind.${kind}`)}</dd>
                <dt className="text-muted-foreground">{t('wizard.preview.reference')}</dt>
                <dd>{referenceSummary()}</dd>
              </dl>
              <StepUpFields action={action} idPrefix="wizard" />
            </div>
          )}

          <Refusal message={action.error} />

          <DialogFooter>
            {index > 0 && (
              <Button variant="outline" onClick={back} disabled={action.busy}>{t('wizard.back')}</Button>
            )}
            {step !== 'preview' ? (
              <Button type="submit" variant="brand" disabled={!canNext}>{t('wizard.next')}</Button>
            ) : (
              <Button type="submit" variant="brand" disabled={!action.canSubmit || !request()}>
                {action.needsVerify ? t('wizard.verifyCreate') : t('wizard.create')}
              </Button>
            )}
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
