import { AlertTriangle } from 'lucide-react';
import { useIntl } from 'react-intl';
import type { LicenseSnapshot } from '@/lib/api';
import { useAuthStore } from '@/stores/auth-store';

const REASON_MESSAGES: Record<string, string> = {
  weak_binding: 'license.validation.weakBinding',
  machine_mismatch: 'license.validation.machineMismatch',
  hardware_identity_unavailable: 'license.validation.hardwareUnavailable',
  expired: 'license.validation.expired',
  revoked: 'license.validation.revoked',
  invalid_signature: 'license.validation.invalidSignature',
  unreadable: 'license.validation.unreadable',
  grace_period_expired: 'license.validation.graceExpired',
  invalid_license: 'license.validation.invalidLicense',
};

/** Only show fixed, translated reasons; never render server error text or identifiers. */
export function LicenseValidationWarning({ snapshot }: { snapshot: LicenseSnapshot | null }) {
  const intl = useIntl();
  const isAdmin = useAuthStore(s => s.user?.role === 'admin');
  if (!isAdmin || !snapshot?.installed || snapshot.mode !== 'opensource' || !snapshot.validation_warning) {
    return null;
  }
  const reason = Object.hasOwn(REASON_MESSAGES, snapshot.validation_warning)
    ? REASON_MESSAGES[snapshot.validation_warning]
    : 'license.validation.invalidLicense';
  return (
    <div role="alert" className="flex items-start gap-3 rounded-lg border border-destructive/30 bg-destructive/10 px-4 py-3 text-sm text-destructive">
      <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0" aria-hidden="true" />
      <div>
        <p className="font-medium">{intl.formatMessage({ id: reason })}</p>
        <p className="mt-1">{intl.formatMessage({ id: 'license.validation.help' })}</p>
      </div>
    </div>
  );
}
