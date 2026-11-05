import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { AlertTriangle, X } from 'lucide-react';
import { api, type LicenseSnapshot } from '@/lib/api';
import { useCommercialContact } from '@/lib/commercial-contact';
import { useAuthStore } from '@/stores/auth-store';
import { useConnectionStore } from '@/stores/connection-store';
import { useLocation } from 'react-router';
import { LicenseValidationWarning } from './LicenseValidationWarning';

type Urgency = 'expired' | 'critical' | 'warning' | null;

/**
 * Classify days-until-expiry into a proactive-warning bucket. Thresholds match
 * the gateway (`license_runtime::classify_expiry_urgency`) and `LicensePage`:
 * expired (<0) / critical (≤7) / warning (≤30) / none (>30). Only an *installed*
 * license with a known expiry ever warns.
 */
function classify(snapshot: LicenseSnapshot | null): Urgency {
  if (!snapshot?.installed || snapshot.days_until_expiry == null) return null;
  const d = snapshot.days_until_expiry;
  if (d < 0) return 'expired';
  if (d <= 7) return 'critical';
  if (d <= 30) return 'warning';
  return null;
}

const TONE: Record<Exclude<Urgency, null>, string> = {
  expired: 'border-destructive/30 bg-destructive/10 text-destructive',
  critical: 'border-destructive/30 bg-destructive/10 text-destructive',
  warning: 'border-warning/30 bg-warning/10 text-warning',
};

const MESSAGE_ID: Record<Exclude<Urgency, null>, string> = {
  expired: 'licenseBanner.expired',
  critical: 'licenseBanner.critical',
  warning: 'licenseBanner.warning',
};

/**
 * Cross-page proactive license-expiry banner (mounted shell-wide in MainLayout,
 * alongside SoftLimitBanner). Fires in the 30/7-day pre-expiry window and after
 * expiry so an operator is never surprised by a downgrade — the passive
 * LicensePage countdown alone is easy to miss.
 *
 * Best-effort: fetch only for authenticated managers/admins, on reconnect and
 * every minute. Validation failures are admin-only and cannot be dismissed.
 * Expiry dismissal is keyed by urgency so an escalation re-shows the banner.
 */
export function LicenseExpiryBanner() {
  const intl = useIntl();
  const contact = useCommercialContact();
  const role = useAuthStore(s => s.user?.role);
  const connectionState = useConnectionStore(s => s.state);
  const { pathname } = useLocation();
  const [snapshot, setSnapshot] = useState<LicenseSnapshot | null>(null);
  const [dismissed, setDismissed] = useState<Urgency>(null);

  useEffect(() => {
    setSnapshot(null);
    if (connectionState !== 'authenticated' || (role !== 'admin' && role !== 'manager')) return;
    let active = true;
    const load = () => api.license
      .status()
      .then((s) => {
        if (active) setSnapshot(s);
      })
      .catch(() => {
        /* manager-gated / offline — no banner */
      });
    void load();
    const timer = window.setInterval(() => void load(), 60_000);
    return () => {
      active = false;
      window.clearInterval(timer);
    };
  }, [connectionState, role]);

  if (snapshot?.validation_warning) {
    // LicensePage uses its own refreshed snapshot, avoiding a duplicate banner.
    if (pathname === '/app/system/license') return null;
    return <LicenseValidationWarning snapshot={snapshot} />;
  }

  const urgency = classify(snapshot);
  if (!urgency || dismissed === urgency) return null;

  const days = Math.abs(snapshot?.days_until_expiry ?? 0);

  return (
    <div className={`mb-4 flex items-start gap-3 rounded-lg border px-4 py-3 text-sm ${TONE[urgency]}`}>
      <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0" />
      <div className="flex-1">
        <p className="font-medium">
          {intl.formatMessage({ id: MESSAGE_ID[urgency] }, { days })}
        </p>
        <a
          href={contact.url}
          target="_blank"
          rel="noreferrer"
          className="mt-1 inline-block font-medium underline underline-offset-2 hover:opacity-80"
        >
          {intl.formatMessage({ id: 'license.cta.renew.action' })}
        </a>
      </div>
      <button
        onClick={() => setDismissed(urgency)}
        className="rounded p-1 opacity-70 transition-opacity hover:opacity-100"
        aria-label={intl.formatMessage({ id: 'softLimit.dismiss' })}
      >
        <X className="h-4 w-4" />
      </button>
    </div>
  );
}
