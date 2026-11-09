import { useState } from 'react';
import { useIntl } from 'react-intl';
import { AlertTriangle } from 'lucide-react';
import { api, type AgentGoogleIdentity, type GoogleTenantSubjectHolder, type GoogleTenantSubjectNotice } from '@/lib/api';
import { TenantSubjectNotice } from '@/components/protected/TenantSubjectNotice';
import {
  Badge,
  Button,
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/mds';
import { StepUpDialog } from './step-up';
import { formatWhen, useT } from './shared';

/**
 * SR4 round 3 (N1): an Admin confirms WHICH account a "act as this colleague"
 * (delegate) mailbox belongs to. The row shows the account that holds the
 * address now — name, account id, creation date — and warns when that account
 * changed its own login email to this address (anyone can). The request names
 * that account (`user_id`); the gateway refuses if the holder changed in the
 * meantime. A self-set address needs an extra explicit checkbox
 * (`acknowledge_self_set`).
 */
interface Row {
  agent: string;
  subject: string;
  /** Absent when no single account holds the address (none / more than one):
   *  the row only explains the state; nothing can be confirmed from here. */
  holder: GoogleTenantSubjectHolder | null;
  attested: boolean;
  state: GoogleTenantSubjectNotice['state'];
}
type HeldRow = Row & { holder: GoogleTenantSubjectHolder };
const hasHolder = (row: Row): row is HeldRow => row.holder !== null;

function attestationRows(agents: AgentGoogleIdentity[] | undefined): Row[] {
  return (agents ?? []).flatMap((agent) => {
    const notice = agent.tenant_subject;
    if (!notice || !notice.subject) return [];
    if (notice.state !== 'attestation_required' && notice.state !== 'attested_person' && notice.state !== 'ambiguous') return [];
    return [{
      agent: agent.agent_id,
      subject: notice.subject,
      holder: notice.holder ?? null,
      attested: notice.state === 'attested_person',
      state: notice.state,
    }];
  });
}

export function DelegateAttestationCard({
  agents,
  onChanged,
}: {
  agents: AgentGoogleIdentity[] | undefined;
  onChanged: () => Promise<void> | void;
}) {
  const t = useT();
  const intl = useIntl();
  const rows = attestationRows(agents);
  const [confirming, setConfirming] = useState<HeldRow | null>(null);
  const [revoking, setRevoking] = useState<HeldRow | null>(null);
  const [personChecked, setPersonChecked] = useState(false);
  const [selfSetChecked, setSelfSetChecked] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);

  if (rows.length === 0) return null;

  const created = (holder: GoogleTenantSubjectHolder) => formatWhen(holder.created_at, intl.locale);
  const closeConfirm = () => {
    setConfirming(null);
    setPersonChecked(false);
    setSelfSetChecked(false);
  };

  return (
    <Card>
      <CardHeader>
        <CardTitle><h2>{t('tenant.attest.title')}</h2></CardTitle>
        <CardDescription>{t('tenant.attest.description')}</CardDescription>
      </CardHeader>
      <CardContent className="space-y-3">
        {notice && <p role="status" className="text-sm text-muted-foreground">{notice}</p>}
        <ul className="divide-y divide-surface-border rounded-lg border border-surface-border">
          {rows.map((row) => (
            <li key={row.agent} className="space-y-2 px-3 py-3 text-sm" data-testid={`attest-row-${row.agent}`}>
              <div className="flex flex-wrap items-center justify-between gap-2">
                <span className="font-medium">{row.agent}</span>
                {row.attested ? (
                  <Badge variant="secondary">{t('tenant.attest.state.attested')}</Badge>
                ) : (
                  <Badge variant="destructive">{t('tenant.attest.state.required')}</Badge>
                )}
              </div>
              <dl className="grid grid-cols-[auto_1fr] gap-x-6 gap-y-1">
                <dt className="text-muted-foreground">{t('tenant.attest.mailbox')}</dt>
                <dd className="break-all">{row.subject}</dd>
                {hasHolder(row) && (
                  <>
                    <dt className="text-muted-foreground">{t('tenant.attest.holder')}</dt>
                    <dd>{row.holder.name}</dd>
                    <dt className="text-muted-foreground">{t('tenant.attest.holderId')}</dt>
                    <dd className="break-all font-mono text-xs">{row.holder.user_id}</dd>
                    <dt className="text-muted-foreground">{t('tenant.attest.createdAt')}</dt>
                    <dd>{created(row.holder)}</dd>
                  </>
                )}
              </dl>
              {!hasHolder(row) && <TenantSubjectNotice notice={{ state: row.state }} />}
              {hasHolder(row) && row.holder.email_self_set && (
                <p role="note" className="flex items-start gap-2 rounded-lg border border-warning/30 bg-warning/10 px-3 py-2">
                  <AlertTriangle className="mt-0.5 size-4 shrink-0" aria-hidden="true" />
                  <span>{t('tenant.attest.selfSetWarning', { name: row.holder.name })}</span>
                </p>
              )}
              {hasHolder(row) && (
                <div className="flex justify-end">
                  {row.attested ? (
                    <Button variant="outline" size="sm" onClick={() => { setNotice(null); setRevoking(row); }}>
                      {t('tenant.attest.revoke')}
                    </Button>
                  ) : (
                    <Button variant="brand" size="sm" onClick={() => { setNotice(null); setConfirming(row); }}>
                      {t('tenant.attest.confirm')}
                    </Button>
                  )}
                </div>
              )}
            </li>
          ))}
        </ul>
      </CardContent>

      {confirming && (
        <StepUpDialog
          open
          idPrefix="attest"
          onOpenChange={(open) => { if (!open) closeConfirm(); }}
          title={t('tenant.attest.dialogTitle')}
          description={t('tenant.attest.dialogDescription', { employee: confirming.agent, name: confirming.holder.name })}
          confirmLabel={t('tenant.attest.confirmButton')}
          verifyConfirmLabel={t('tenant.attest.verifyConfirm')}
          disabled={!personChecked || (confirming.holder.email_self_set && !selfSetChecked)}
          onConfirm={async (csrf) => {
            await api.protectedMode.attestSubject({
              agent_id: confirming.agent,
              subject: confirming.subject,
              user_id: confirming.holder.user_id,
              acknowledge_self_set: confirming.holder.email_self_set && selfSetChecked,
            }, csrf);
            setNotice(t('tenant.attest.done', { name: confirming.holder.name }));
            // The dialog closes itself on success (onOpenChange → closeConfirm).
            await onChanged();
          }}
        >
          <div className="space-y-3 text-sm">
            <dl className="grid grid-cols-[auto_1fr] gap-x-6 gap-y-1 rounded-lg bg-muted/60 px-3 py-2">
              <dt className="text-muted-foreground">{t('tenant.attest.holder')}</dt>
              <dd>{confirming.holder.name}</dd>
              <dt className="text-muted-foreground">{t('tenant.attest.holderId')}</dt>
              <dd className="break-all font-mono text-xs">{confirming.holder.user_id}</dd>
              <dt className="text-muted-foreground">{t('tenant.attest.createdAt')}</dt>
              <dd>{created(confirming.holder)}</dd>
            </dl>
            <div className="flex items-start gap-3">
              <input
                id="attest-person"
                type="checkbox"
                className="mt-1 size-4 accent-brand"
                checked={personChecked}
                onChange={(e) => setPersonChecked(e.target.checked)}
              />
              <label htmlFor="attest-person">
                {t('tenant.attest.checkPerson', {
                  mailbox: confirming.subject,
                  name: confirming.holder.name,
                  created: created(confirming.holder),
                })}
              </label>
            </div>
            {confirming.holder.email_self_set && (
              <>
                <p role="alert" className="rounded-lg border border-warning/30 bg-warning/10 px-3 py-2">
                  {t('tenant.attest.selfSetWarning', { name: confirming.holder.name })}
                </p>
                <div className="flex items-start gap-3">
                  <input
                    id="attest-self-set"
                    type="checkbox"
                    className="mt-1 size-4 accent-brand"
                    checked={selfSetChecked}
                    onChange={(e) => setSelfSetChecked(e.target.checked)}
                  />
                  <label htmlFor="attest-self-set">
                    {t('tenant.attest.checkSelfSet', { name: confirming.holder.name })}
                  </label>
                </div>
              </>
            )}
          </div>
        </StepUpDialog>
      )}

      {revoking && (
        <StepUpDialog
          open
          destructive
          idPrefix="attest-revoke"
          onOpenChange={(open) => { if (!open) setRevoking(null); }}
          title={t('tenant.attest.revokeTitle')}
          description={t('tenant.attest.revokeDescription', { employee: revoking.agent, name: revoking.holder.name })}
          confirmLabel={t('tenant.attest.revokeButton')}
          verifyConfirmLabel={t('tenant.attest.verifyRevoke')}
          onConfirm={async (csrf) => {
            await api.protectedMode.revokeAttestation(revoking.agent, csrf);
            setNotice(t('tenant.attest.revoked', { name: revoking.holder.name }));
            await onChanged();
          }}
        />
      )}
    </Card>
  );
}
