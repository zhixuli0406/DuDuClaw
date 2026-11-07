import { useIntl } from 'react-intl';
import { passkeyUnavailableReason } from '@/lib/passkey';

// Email currently has only no_smtp; unknown/missing reasons use the same safe
// setup guidance. Never render raw server text.
export function DisabledFactorExplanation({ factor }: { factor: 'email' | 'passkey'; reason?: string | null }) {
  const intl = useIntl();
  const id = factor === 'passkey'
    ? `mfa.passkey.${passkeyUnavailableReason() ?? 'unavailable'}`
    : 'mfa.email.unavailable';
  return <p role="status" className="text-sm text-muted-foreground">{intl.formatMessage({ id })}</p>;
}
