import { useIntl } from 'react-intl';
import { passkeyUnavailableReason } from '@/lib/passkey';

// Map only known reason codes to catalogue IDs; never render raw server text.
// Unknown or missing reasons (including no_smtp) use the SMTP setup guidance.
const EMAIL_REASONS = new Map<string, string>([
  ['relay_disabled', 'mfa.email.relayDisabled'],
  ['relay_nfr', 'mfa.email.relayNfr'],
  ['relay_unbound', 'mfa.email.relayUnbound'],
  ['relay_key_unbound', 'mfa.email.relayKeyUnbound'],
  ['relay_key_mismatch', 'mfa.email.relayKeyMismatch'],
]);
const KEY_REASONS = new Set(['relay_key_unbound', 'relay_key_mismatch']);
// The 16-character SAS (`abcd-ef01-2345-6789`); anything else is not shown.
const SAS_PATTERN = /^[0-9a-f]{4}(?:-[0-9a-f]{4}){3}$/;

export function DisabledFactorExplanation({ factor, reason, fingerprint }: {
  factor: 'email' | 'passkey';
  reason?: string | null;
  fingerprint?: string | null;
}) {
  const intl = useIntl();
  const id = factor === 'passkey'
    ? `mfa.passkey.${passkeyUnavailableReason() ?? 'unavailable'}`
    : (reason ? EMAIL_REASONS.get(reason) : undefined) ?? 'mfa.email.unavailable';
  const showFingerprint = factor === 'email' && !!reason && KEY_REASONS.has(reason)
    && typeof fingerprint === 'string' && SAS_PATTERN.test(fingerprint);
  return <p role="status" className="text-sm text-muted-foreground">
    {intl.formatMessage({ id })}
    {showFingerprint && <span className="mt-1 block font-mono" data-testid="relay-key-fingerprint">
      {intl.formatMessage({ id: 'mfa.email.relayKeyFingerprint' }, { fingerprint })}
    </span>}
  </p>;
}
