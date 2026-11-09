import { useCallback, useEffect, useMemo, useRef, useState, type FormEvent } from 'react';
import { useIntl } from 'react-intl';
import { Mail, X } from 'lucide-react';
import { api, type ProtectedTenantSourceView } from '@/lib/api';
import { useConnectionStore } from '@/stores/connection-store';
import { isImeComposing } from '@/lib/keyboard';
import {
  Badge,
  Button,
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
  Input,
} from '@/components/mds';
import { StepUpFields, useStepUpAction } from './step-up';
import { Refusal, formatWhen, refusalMessage, useOptionalT, useProtectedPage, useT } from './shared';

/**
 * Saving `service_mailboxes` / `tenant_scopes` needs a gateway RPC that does
 * not exist yet: `google.delegation.settings_set` deliberately preserves both
 * (E4) and no other method writes them. Until it ships the editor is shown
 * read-write but the save stays disabled with an explanation. Flip this (and
 * wire `api.protectedMode`) when the RPC lands — see the F5 part A report.
 */
export const TENANT_SOURCE_SAVE_AVAILABLE: boolean = false;

const EMAIL = /^[^\s@]+@[^\s@]+\.[^\s@]+$/;
const SCOPE_PREFIX = 'https://www.googleapis.com/auth/';

type SourceType = 'delegation' | 'serviceAccount' | 'none';

export function TenantSourceTab() {
  const t = useT();
  const optional = useOptionalT();
  const intl = useIntl();
  const connection = useConnectionStore((s) => s.state);
  const { people } = useProtectedPage();
  const action = useStepUpAction();
  const [data, setData] = useState<ProtectedTenantSourceView | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [mailboxes, setMailboxes] = useState<string[]>([]);
  const [scopes, setScopes] = useState<string[]>([]);
  const [draft, setDraft] = useState('');
  const [draftError, setDraftError] = useState<string | null>(null);
  const generation = useRef(0);

  const load = useCallback(async () => {
    const current = ++generation.current;
    try {
      const view = await api.protectedMode.tenantSource();
      if (current !== generation.current) return;
      setData(view);
      setMailboxes(view.settings?.service_mailboxes ?? []);
      setScopes(view.settings?.tenant_scopes ?? []);
      setLoadError(null);
    } catch (e) {
      if (current !== generation.current) return;
      setData(null);
      setLoadError(refusalMessage(e));
    }
  }, []);

  useEffect(() => {
    if (connection === 'authenticated') void load();
    return () => { generation.current += 1; };
  }, [connection, load]);

  const personEmails = useMemo(
    () => new Set([...people.values()].map((person) => person.email.trim().toLowerCase()).filter(Boolean)),
    [people],
  );

  const sourceType: SourceType = data?.key ? 'delegation' : data?.home_subject ? 'serviceAccount' : 'none';
  const original = {
    mailboxes: data?.settings?.service_mailboxes ?? [],
    scopes: data?.settings?.tenant_scopes ?? [],
  };
  const sameSet = (a: string[], b: string[]) => a.length === b.length && a.every((value) => b.includes(value));
  const dirty = data !== null && (!sameSet(mailboxes, original.mailboxes) || !sameSet(scopes, original.scopes));

  const addMailbox = (event: { preventDefault: () => void }) => {
    event.preventDefault();
    const value = draft.trim().toLowerCase();
    if (!EMAIL.test(value)) { setDraftError(t('tenant.mailboxInvalid')); return; }
    if (mailboxes.includes(value)) { setDraftError(t('tenant.mailboxDuplicate')); return; }
    if (personEmails.has(value)) { setDraftError(t('tenant.mailboxIsPerson')); return; }
    setMailboxes((current) => [...current, value]);
    setDraft('');
    setDraftError(null);
  };

  const scopeLabel = (scope: string) => {
    const short = scope.startsWith(SCOPE_PREFIX) ? scope.slice(SCOPE_PREFIX.length) : scope;
    return { short, label: optional(`tenant.scope.${short}`, short) };
  };

  const save = async (event: FormEvent) => {
    event.preventDefault();
    if (!TENANT_SOURCE_SAVE_AVAILABLE || !dirty || !action.canSubmit) return;
    // Unreachable until the save RPC exists (see TENANT_SOURCE_SAVE_AVAILABLE).
  };

  return (
    <div className="space-y-6">
      <Refusal message={loadError} />
      {!data && !loadError && <p role="status" className="text-sm text-muted-foreground">{t('loading')}</p>}

      {data && (
        <>
          <Card>
            <CardHeader>
              <CardTitle><h2>{t('tenant.currentTitle')}</h2></CardTitle>
              <CardDescription>{t('tenant.delegationRule')}</CardDescription>
            </CardHeader>
            <CardContent>
              <dl className="grid grid-cols-[auto_1fr] gap-x-6 gap-y-2 text-sm">
                <dt className="text-muted-foreground">{t('tenant.type')}</dt>
                <dd>{t(`tenant.type.${sourceType}`)}</dd>
                {sourceType === 'delegation' && data.key && (
                  <>
                    <dt className="text-muted-foreground">{t('tenant.serviceAccount')}</dt>
                    <dd className="break-all">{data.key.client_email}</dd>
                    <dt className="text-muted-foreground">{t('tenant.keyId')}</dt>
                    <dd className="break-all font-mono text-xs">{data.key.key_id}</dd>
                    <dt className="text-muted-foreground">{t('tenant.importedAt')}</dt>
                    <dd>{formatWhen(data.key.imported_at, intl.locale)}</dd>
                  </>
                )}
                {sourceType === 'serviceAccount' && data.home_subject && (
                  <>
                    <dt className="text-muted-foreground">{t('tenant.subject')}</dt>
                    <dd className="break-all">{data.home_subject}</dd>
                  </>
                )}
                <dt className="text-muted-foreground">{t('tenant.status')}</dt>
                <dd>
                  {sourceType === 'none' ? (
                    <Badge variant="ghost">{t('tenant.status.none')}</Badge>
                  ) : data.key_error ? (
                    <span className="space-y-1">
                      <Badge variant="destructive">{t('tenant.status.unavailable')}</Badge>
                      <span className="block text-xs text-muted-foreground">{data.key_error}</span>
                    </span>
                  ) : (
                    <Badge variant="secondary">{t('tenant.status.ready')}</Badge>
                  )}
                </dd>
              </dl>
              <p className="mt-3 text-xs text-muted-foreground">{t('tenant.keyHidden')}</p>
              {data.settings_error && <p role="alert" className="mt-2 text-sm text-destructive">{data.settings_error}</p>}
            </CardContent>
          </Card>

          <form onSubmit={(e) => void save(e)} className="space-y-6">
            <Card>
              <CardHeader>
                <CardTitle><h2>{t('tenant.mailboxesTitle')}</h2></CardTitle>
                <CardDescription>{t('tenant.mailboxesDescription')}</CardDescription>
              </CardHeader>
              <CardContent className="space-y-3">
                {mailboxes.length === 0 ? (
                  <p className="text-sm text-muted-foreground">{t('tenant.mailboxesEmpty')}</p>
                ) : (
                  <ul className="divide-y divide-surface-border rounded-lg border border-surface-border">
                    {mailboxes.map((mailbox) => (
                      <li key={mailbox} className="flex items-center justify-between gap-2 px-3 py-2 text-sm">
                        <span className="flex min-w-0 items-center gap-2">
                          <Mail className="size-4 shrink-0 text-muted-foreground" aria-hidden="true" />
                          <span className="break-all">{mailbox}</span>
                        </span>
                        <Button
                          size="icon-sm"
                          variant="ghost"
                          aria-label={t('tenant.mailboxRemove', { mailbox })}
                          onClick={() => setMailboxes((current) => current.filter((value) => value !== mailbox))}
                        >
                          <X />
                        </Button>
                      </li>
                    ))}
                  </ul>
                )}
                <div className="space-y-1">
                  <label htmlFor="tenant-mailbox" className="block text-sm font-medium">{t('tenant.mailboxAdd')}</label>
                  <div className="flex flex-wrap gap-2">
                    <Input
                      id="tenant-mailbox"
                      type="email"
                      className="max-w-sm"
                      value={draft}
                      aria-invalid={draftError !== null}
                      aria-describedby="tenant-mailbox-hint"
                      onChange={(e) => { setDraft(e.target.value); setDraftError(null); }}
                      onKeyDown={(e) => { if (e.key === 'Enter' && !isImeComposing(e)) addMailbox(e); }}
                    />
                    <Button variant="outline" onClick={(e) => addMailbox(e)} disabled={draft.trim().length === 0}>
                      {t('tenant.mailboxAddButton')}
                    </Button>
                  </div>
                  <p id="tenant-mailbox-hint" className="text-xs text-muted-foreground">{t('tenant.mailboxHint')}</p>
                  {draftError && <p role="alert" className="text-sm text-destructive">{draftError}</p>}
                </div>
              </CardContent>
            </Card>

            <Card>
              <CardHeader>
                <CardTitle><h2>{t('tenant.scopesTitle')}</h2></CardTitle>
                <CardDescription>{t('tenant.scopesDescription')}</CardDescription>
              </CardHeader>
              <CardContent>
                <fieldset className="space-y-2">
                  <legend className="sr-only">{t('tenant.scopesTitle')}</legend>
                  {data.required_scopes.map((scope) => {
                    const { short, label } = scopeLabel(scope);
                    const id = `tenant-scope-${short.replace(/[^a-z0-9]+/gi, '-')}`;
                    return (
                      <div key={scope} className="flex items-start gap-3 text-sm">
                        <input
                          id={id}
                          type="checkbox"
                          className="mt-1 size-4 accent-brand"
                          checked={scopes.includes(scope)}
                          onChange={(e) => setScopes((current) => e.target.checked ? [...current, scope] : current.filter((value) => value !== scope))}
                        />
                        <label htmlFor={id}>
                          {label}
                          {label !== short && <span className="ml-2 text-xs text-muted-foreground">{short}</span>}
                        </label>
                      </div>
                    );
                  })}
                </fieldset>
                {scopes.length === 0 && <p className="mt-3 text-sm text-muted-foreground">{t('tenant.scopesEmpty')}</p>}
              </CardContent>
            </Card>

            <div className="space-y-3">
              {TENANT_SOURCE_SAVE_AVAILABLE && dirty && <StepUpFields action={action} idPrefix="tenant" />}
              <Refusal message={action.error} />
              {!TENANT_SOURCE_SAVE_AVAILABLE && (
                <p role="note" className="rounded-lg border border-surface-border bg-muted px-3 py-2 text-sm text-muted-foreground">
                  {t('tenant.saveUnavailable')}
                </p>
              )}
              <div className="flex flex-wrap justify-end gap-2">
                <Button
                  variant="outline"
                  disabled={!dirty}
                  onClick={() => { setMailboxes(original.mailboxes); setScopes(original.scopes); setDraftError(null); }}
                >
                  {t('tenant.discard')}
                </Button>
                <Button type="submit" variant="brand" disabled={!TENANT_SOURCE_SAVE_AVAILABLE || !dirty || !action.canSubmit}>
                  {action.needsVerify ? t('tenant.verifySave') : t('tenant.save')}
                </Button>
              </div>
            </div>
          </form>
        </>
      )}
    </div>
  );
}
