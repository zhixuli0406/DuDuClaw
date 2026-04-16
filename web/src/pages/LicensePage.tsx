import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { useConnectionStore } from '@/stores/connection-store';
import { api, type LicenseInfo } from '@/lib/api';
import { cn } from '@/lib/utils';
import {
  Shield,
  Check,
  X,
  ExternalLink,
  KeyRound,
  Fingerprint,
  CalendarClock,
  Crown,
} from 'lucide-react';

const TIER_COLORS: Record<string, string> = {
  community: 'bg-stone-100 text-stone-700 dark:bg-stone-800 dark:text-stone-300',
  pro: 'bg-amber-100 text-amber-700 dark:bg-amber-900/30 dark:text-amber-400',
  enterprise: 'bg-violet-100 text-violet-700 dark:bg-violet-900/30 dark:text-violet-400',
};

const TIER_BORDER: Record<string, string> = {
  community: 'border-stone-200 dark:border-stone-700',
  pro: 'border-amber-300 dark:border-amber-700',
  enterprise: 'border-violet-300 dark:border-violet-700',
};

interface FeatureRow {
  readonly labelKey: string;
  readonly community: boolean;
  readonly pro: boolean;
  readonly enterprise: boolean;
}

// Open Core: All core features are open source (Apache-2.0).
// Pro/Enterprise add value-add services via private repo extension.
const FEATURES: ReadonlyArray<FeatureRow> = [
  // ── Open Source Core (all tiers) ──
  { labelKey: 'license.feature.fullProduct', community: true, pro: true, enterprise: true },
  { labelKey: 'license.feature.unlimitedAgents', community: true, pro: true, enterprise: true },
  { labelKey: 'license.feature.allChannels', community: true, pro: true, enterprise: true },
  { labelKey: 'license.feature.localInference', community: true, pro: true, enterprise: true },
  { labelKey: 'license.feature.evolutionEngine', community: true, pro: true, enterprise: true },
  { labelKey: 'license.feature.accountRotation', community: true, pro: true, enterprise: true },
  { labelKey: 'license.feature.securityFull', community: true, pro: true, enterprise: true },
  { labelKey: 'license.feature.costTelemetry', community: true, pro: true, enterprise: true },
  { labelKey: 'license.feature.odoo', community: true, pro: true, enterprise: true },
  // ── Pro value-add ──
  { labelKey: 'license.feature.industryTemplates', community: false, pro: true, enterprise: true },
  { labelKey: 'license.feature.gvuAdaptive', community: false, pro: true, enterprise: true },
  { labelKey: 'license.feature.gvuParams', community: false, pro: true, enterprise: true },
  { labelKey: 'license.feature.autoUpdate', community: false, pro: true, enterprise: true },
  // ── Enterprise value-add ──
  { labelKey: 'license.feature.auditExport', community: false, pro: false, enterprise: true },
  { labelKey: 'license.feature.roiReport', community: false, pro: false, enterprise: true },
  { labelKey: 'license.feature.slaSupport', community: false, pro: false, enterprise: true },
  { labelKey: 'license.feature.onboarding', community: false, pro: false, enterprise: true },
];

export function LicensePage() {
  const intl = useIntl();
  const connectionState = useConnectionStore((s) => s.state);
  const [license, setLicense] = useState<LicenseInfo | null>(null);
  const [licenseKey, setLicenseKey] = useState('');
  const [activating, setActivating] = useState(false);
  const [activateError, setActivateError] = useState('');
  const [activateSuccess, setActivateSuccess] = useState(false);
  const [deactivateError, setDeactivateError] = useState('');

  useEffect(() => {
    if (connectionState !== 'authenticated') return;
    api.license.status().then(setLicense).catch((e) => console.warn("[api]", e));
  }, [connectionState]);

  const handleActivate = async () => {
    if (!licenseKey.trim()) return;
    setActivating(true);
    setActivateError('');
    setActivateSuccess(false);
    try {
      await api.license.activate(licenseKey.trim());
      setActivateSuccess(true);
      setLicenseKey('');
      // Refresh license status
      const updated = await api.license.status();
      setLicense(updated);
      setTimeout(() => setActivateSuccess(false), 3000);
    } catch (err) {
      const msg = typeof err === 'string' ? err : err instanceof Error ? err.message : '';
      setActivateError(msg || intl.formatMessage({ id: 'common.error' }));
    } finally {
      setActivating(false);
    }
  };

  const handleDeactivate = async () => {
    setDeactivateError('');
    try {
      await api.license.deactivate();
      const updated = await api.license.status();
      setLicense(updated);
    } catch {
      setDeactivateError(intl.formatMessage({ id: 'license.deactivateError' }));
    }
  };

  const tier = license?.tier?.toLowerCase() ?? 'community';
  const daysRemaining = license?.days_remaining ?? null;

  // Expiry warning states
  const isExpired = daysRemaining !== null && daysRemaining <= 0;
  const isExpiringSoon = daysRemaining !== null && daysRemaining > 0 && daysRemaining <= 30;
  const isCritical = daysRemaining !== null && daysRemaining > 0 && daysRemaining <= 7;

  return (
    <div className="space-y-6">
      <h2 className="text-2xl font-semibold text-stone-900 dark:text-stone-50">
        {intl.formatMessage({ id: 'license.title' })}
      </h2>

      {/* License Status Card */}
      <div className={cn(
        'rounded-xl border-2 bg-white p-6 dark:bg-stone-900',
        TIER_BORDER[tier] ?? 'border-stone-200 dark:border-stone-700'
      )}>
        <div className="flex items-center gap-3 mb-6">
          <div className="rounded-lg bg-amber-500 p-2.5">
            <Shield className="h-5 w-5 text-white" />
          </div>
          <h3 className="text-lg font-medium text-stone-900 dark:text-stone-50">
            {intl.formatMessage({ id: 'license.title' })}
          </h3>
        </div>

        <div className="grid gap-5 sm:grid-cols-2 lg:grid-cols-3">
          {/* Tier Badge */}
          <div className="space-y-1.5">
            <span className="text-sm text-stone-500 dark:text-stone-400">
              {intl.formatMessage({ id: 'license.currentTier' })}
            </span>
            <div className="flex items-center gap-2">
              <Crown className="h-4 w-4 text-amber-500" />
              <span className={cn(
                'inline-flex rounded-full px-3 py-1 text-sm font-semibold',
                TIER_COLORS[tier] ?? TIER_COLORS.community
              )}>
                {intl.formatMessage({ id: `license.${tier}` })}
              </span>
            </div>
          </div>

          {/* Expiry Date */}
          <div className="space-y-1.5">
            <span className="text-sm text-stone-500 dark:text-stone-400">
              {intl.formatMessage({ id: 'license.expiresAt' })}
            </span>
            <div className="flex items-center gap-2">
              <CalendarClock className={cn(
                'h-4 w-4',
                isExpired ? 'text-rose-500' : isCritical ? 'text-rose-500' : isExpiringSoon ? 'text-amber-500' : 'text-stone-400'
              )} />
              <div>
                <span className={cn(
                  'text-sm font-medium',
                  isExpired
                    ? 'text-rose-600 dark:text-rose-400'
                    : isCritical
                      ? 'text-rose-600 dark:text-rose-400'
                      : isExpiringSoon
                        ? 'text-amber-600 dark:text-amber-400'
                        : 'text-stone-900 dark:text-stone-50'
                )}>
                  {license?.expires_at
                    ? new Date(license.expires_at).toLocaleDateString()
                    : intl.formatMessage({ id: 'license.unlimited' })}
                </span>
                {isExpired && (
                  <span className="ml-2 inline-flex items-center rounded-full bg-rose-100 px-2 py-0.5 text-xs font-medium text-rose-700 dark:bg-rose-900/30 dark:text-rose-400">
                    {intl.formatMessage({ id: 'license.expired' })}
                  </span>
                )}
                {isExpiringSoon && !isExpired && (
                  <span className={cn(
                    'ml-2 inline-flex items-center rounded-full px-2 py-0.5 text-xs font-medium',
                    isCritical
                      ? 'bg-rose-100 text-rose-700 dark:bg-rose-900/30 dark:text-rose-400'
                      : 'bg-amber-100 text-amber-700 dark:bg-amber-900/30 dark:text-amber-400'
                  )}>
                    {intl.formatMessage({ id: 'license.daysRemaining' }, { days: daysRemaining })}
                  </span>
                )}
              </div>
            </div>
          </div>

          {/* Machine Fingerprint */}
          <div className="space-y-1.5">
            <span className="text-sm text-stone-500 dark:text-stone-400">
              {intl.formatMessage({ id: 'license.fingerprint' })}
            </span>
            <div className="flex items-center gap-2">
              <Fingerprint className="h-4 w-4 text-stone-400" />
              <code className="rounded bg-stone-100 px-2 py-0.5 font-mono text-xs text-stone-600 dark:bg-stone-800 dark:text-stone-400">
                {license?.machine_fingerprint ?? '—'}
              </code>
            </div>
          </div>
        </div>

        {/* Customer Name */}
        {license?.customer_name && (
          <div className="mt-4 border-t border-stone-100 pt-4 dark:border-stone-800">
            <span className="text-sm text-stone-500 dark:text-stone-400">
              {license.customer_name}
            </span>
          </div>
        )}

        {/* Deactivate button for non-community tiers */}
        {tier !== 'community' && (
          <>
            <div className="mt-4 flex justify-end">
              <button
                onClick={handleDeactivate}
                className="rounded-lg border border-stone-300 px-3 py-1.5 text-xs text-stone-600 transition-colors hover:bg-stone-50 dark:border-stone-600 dark:text-stone-400 dark:hover:bg-stone-800"
              >
                {intl.formatMessage({ id: 'license.deactivate' })}
              </button>
            </div>
            {deactivateError && (
              <p className="mt-2 text-sm text-rose-600 dark:text-rose-400">{deactivateError}</p>
            )}
          </>
        )}
      </div>

      {/* Expiry Warning Banner */}
      {(isExpired || isExpiringSoon) && (
        <div className={cn(
          'flex items-center gap-3 rounded-xl border p-4',
          isExpired
            ? 'border-rose-200 bg-rose-50 dark:border-rose-800 dark:bg-rose-900/20'
            : isCritical
              ? 'border-rose-200 bg-rose-50 dark:border-rose-800 dark:bg-rose-900/20'
              : 'border-amber-200 bg-amber-50 dark:border-amber-800 dark:bg-amber-900/20'
        )}>
          <Shield className={cn(
            'h-5 w-5 shrink-0',
            isExpired || isCritical ? 'text-rose-500' : 'text-amber-500'
          )} />
          <p className={cn(
            'text-sm',
            isExpired || isCritical
              ? 'text-rose-700 dark:text-rose-300'
              : 'text-amber-700 dark:text-amber-300'
          )}>
            {isExpired
              ? intl.formatMessage({ id: 'license.expired' })
              : intl.formatMessage({ id: 'license.expiringSoon' }) + ' — ' +
                intl.formatMessage({ id: 'license.daysRemaining' }, { days: daysRemaining })}
          </p>
        </div>
      )}

      {/* License Key Activation */}
      <div className="rounded-xl border border-stone-200 bg-white p-6 dark:border-stone-800 dark:bg-stone-900">
        <div className="flex items-center gap-3 mb-4">
          <KeyRound className="h-5 w-5 text-amber-600 dark:text-amber-400" />
          <h3 className="text-lg font-medium text-stone-900 dark:text-stone-50">
            {intl.formatMessage({ id: 'license.activateKey' })}
          </h3>
        </div>

        <div className="flex gap-3">
          <input
            type="text"
            value={licenseKey}
            onChange={(e) => setLicenseKey(e.target.value)}
            placeholder="XXXX-XXXX-XXXX-XXXX"
            className="flex-1 rounded-lg border border-stone-300 bg-white px-4 py-2 text-sm font-mono text-stone-900 placeholder:text-stone-400 focus:border-amber-500 focus:outline-none dark:border-stone-600 dark:bg-stone-800 dark:text-stone-50 dark:placeholder:text-stone-500"
          />
          <button
            onClick={handleActivate}
            disabled={activating || !licenseKey.trim()}
            className="inline-flex items-center gap-2 rounded-lg bg-amber-500 px-5 py-2 text-sm font-medium text-white transition-colors hover:bg-amber-600 disabled:opacity-50"
          >
            {activating
              ? intl.formatMessage({ id: 'common.saving' })
              : intl.formatMessage({ id: 'license.activate' })}
          </button>
        </div>

        {activateError && (
          <p className="mt-2 text-sm text-rose-600 dark:text-rose-400">{activateError}</p>
        )}
        {activateSuccess && (
          <p className="mt-2 text-sm text-emerald-600 dark:text-emerald-400">
            {intl.formatMessage({ id: 'common.saved' })}
          </p>
        )}
      </div>

      {/* Feature Comparison Table */}
      <div className="rounded-xl border border-stone-200 bg-white p-6 dark:border-stone-800 dark:bg-stone-900">
        <h3 className="mb-5 text-lg font-medium text-stone-900 dark:text-stone-50">
          {intl.formatMessage({ id: 'license.comparison' })}
        </h3>

        <div className="overflow-x-auto">
          <table className="w-full text-sm">
            <thead>
              <tr className="border-b border-stone-200 dark:border-stone-700">
                <th className="py-3 text-left font-medium text-stone-500 dark:text-stone-400" />
                <th className="py-3 text-center font-medium text-stone-500 dark:text-stone-400">
                  {intl.formatMessage({ id: 'license.openSource' })}
                </th>
                <th className="py-3 text-center font-medium text-amber-600 dark:text-amber-400">
                  {intl.formatMessage({ id: 'license.proLicense' })}
                </th>
                <th className="py-3 text-center font-medium text-violet-600 dark:text-violet-400">
                  {intl.formatMessage({ id: 'license.enterpriseLicense' })}
                </th>
              </tr>
            </thead>
            <tbody>
              {FEATURES.map((feature) => (
                <tr
                  key={feature.labelKey}
                  className="border-b border-stone-100 last:border-0 dark:border-stone-800"
                >
                  <td className="py-3 text-sm text-stone-700 dark:text-stone-300">
                    {intl.formatMessage({ id: feature.labelKey })}
                  </td>
                  <td className="py-3 text-center">
                    <FeatureMark enabled={feature.community} />
                  </td>
                  <td className="py-3 text-center">
                    <FeatureMark enabled={feature.pro} />
                  </td>
                  <td className="py-3 text-center">
                    <FeatureMark enabled={feature.enterprise} />
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </div>

      {/* Upgrade CTA */}
      <div className="flex justify-center">
        <a
          href="https://duduclaw.dev/pricing"
          target="_blank"
          rel="noopener noreferrer"
          className="inline-flex items-center gap-2 rounded-xl bg-amber-500 px-6 py-3 text-sm font-medium text-white transition-colors hover:bg-amber-600 focus:outline-none focus:ring-2 focus:ring-amber-500/50 focus:ring-offset-2"
        >
          <Crown className="h-4 w-4" />
          {intl.formatMessage({ id: 'license.upgrade' })}
          <ExternalLink className="h-3.5 w-3.5" />
        </a>
      </div>
    </div>
  );
}

function FeatureMark({ enabled }: { readonly enabled: boolean }) {
  return enabled ? (
    <Check className="mx-auto h-5 w-5 text-emerald-500" />
  ) : (
    <X className="mx-auto h-5 w-5 text-stone-300 dark:text-stone-600" />
  );
}
