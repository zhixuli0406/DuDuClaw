import { useIntl } from 'react-intl';
import { passkeyUnavailableReason } from '@/lib/passkey';

// Map only known reason codes to catalogue IDs; never render raw server text.
export function DisabledFactorExplanation({ factor, reason }: { factor: 'email' | 'passkey'; reason?: string | null }) {
  const intl = useIntl();
  const id = factor === 'passkey'
    ? `mfa.passkey.${passkeyUnavailableReason() ?? 'unavailable'}`
    : reason === 'relay_disabled' ? 'mfa.email.relayDisabled'
    : reason === 'relay_nfr' ? 'mfa.email.relayNfr'
    : reason === 'relay_unbound' ? 'mfa.email.relayUnbound'
    : 'mfa.email.unavailable';
  return <p role="status" className="text-sm text-muted-foreground">{intl.formatMessage({ id })}</p>;
}
