import { useIntl } from 'react-intl';
import { AlertTriangle, CheckCircle2, Info, XCircle } from 'lucide-react';
import type { GoogleTenantSubjectNotice } from '@/lib/api';

export type TenantSubjectTone = 'info' | 'warning' | 'error';
type State = GoogleTenantSubjectNotice['state'];

const TONES: Record<State, TenantSubjectTone> = {
  service_mailbox: 'info',
  service_mailbox_conflict: 'error',
  attested_person: 'info',
  attestation_required: 'warning',
  ambiguous: 'warning',
  person_inactive: 'warning',
  not_admissible: 'error',
  unavailable: 'warning',
  users_unavailable: 'warning',
  refused: 'error',
};

/** Plain-language tone for a delegate / service mailbox standing. */
export function tenantSubjectTone(state: State): TenantSubjectTone {
  return TONES[state] ?? 'warning';
}

/** i18n id for a state; an unknown future state reads as "cannot confirm now". */
export function tenantSubjectMessageId(state: State): string {
  return `tenantSubject.${state in TONES ? state : 'unavailable'}`;
}

const STYLES: Record<TenantSubjectTone, string> = {
  info: 'border-surface-border bg-muted/60 text-muted-foreground',
  warning: 'border-warning/30 bg-warning/10 text-warning',
  error: 'border-destructive/30 bg-destructive/10 text-destructive',
};

/**
 * One short, end-user sentence for the standing of the mailbox an AI employee
 * uses on Google (shared company mailbox, a colleague's mailbox, …). The
 * gateway's own text is never shown — it is not written for end users.
 */
export function TenantSubjectNotice({
  notice,
  className = '',
}: {
  notice: Pick<GoogleTenantSubjectNotice, 'state'> | null | undefined;
  className?: string;
}) {
  const intl = useIntl();
  if (!notice) return null;
  const tone = tenantSubjectTone(notice.state);
  const Icon = tone === 'error' ? XCircle : tone === 'warning' ? AlertTriangle : notice.state === 'attested_person' ? CheckCircle2 : Info;
  return (
    <p
      role={tone === 'error' ? 'alert' : 'note'}
      data-testid="tenant-subject-notice"
      data-state={notice.state}
      data-tone={tone}
      className={`flex items-start gap-2 rounded-lg border px-3 py-2 text-sm ${STYLES[tone]} ${className}`.trim()}
    >
      <Icon className="mt-0.5 size-4 shrink-0" aria-hidden="true" />
      <span>{intl.formatMessage({ id: tenantSubjectMessageId(notice.state) })}</span>
    </p>
  );
}
