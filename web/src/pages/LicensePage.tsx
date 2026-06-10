import { useEffect, useMemo, useState } from 'react';
import { useIntl } from 'react-intl';
import { useConnectionStore } from '@/stores/connection-store';
import { api, type LicenseSnapshot } from '@/lib/api';
import { cn } from '@/lib/utils';
import { toast, formatError } from '@/lib/toast';
import {
  KeyRound,
  ShieldCheck,
  Fingerprint,
  Calendar,
  RefreshCw,
  ExternalLink,
  Sparkles,
  Building2,
  FileBarChart2,
  MessagesSquare,
  Database,
  Globe,
  Check,
  Minus,
} from 'lucide-react';

/** Human-readable label for each tier. */
const TIER_LABELS: Record<LicenseSnapshot['tier'], string> = {
  opensource: 'Open Source',
  hobby: 'Hobby (Trial)',
  solo: 'Solo',
  studio: 'Studio',
  business: 'Business',
  self_host_pro: 'Self-Host Pro',
  oem: 'OEM',
};

/** Commercial-module feature flags advertised on the LicensePage matrix. */
const COMMERCIAL_FEATURES: ReadonlyArray<{
  key: string;
  label: string;
  icon: typeof Sparkles;
  tiers: ReadonlySet<LicenseSnapshot['tier']>;
}> = [
  {
    key: 'premium_templates',
    label: 'license.feature.premiumTemplates',
    icon: Sparkles,
    tiers: new Set(['studio', 'business', 'self_host_pro', 'oem']),
  },
  {
    key: 'industry_evolution_params',
    label: 'license.feature.evolutionParams',
    icon: FileBarChart2,
    tiers: new Set(['business', 'self_host_pro', 'oem']),
  },
  {
    key: 'dashboard_enterprise',
    label: 'license.feature.dashboardEnterprise',
    icon: Database,
    tiers: new Set(['business', 'self_host_pro', 'oem']),
  },
  {
    key: 'priority_security_patch',
    label: 'license.feature.prioritySecurityPatch',
    icon: ShieldCheck,
    tiers: new Set(['business', 'self_host_pro', 'oem']),
  },
  {
    key: 'private_discord_support',
    label: 'license.feature.privateDiscord',
    icon: MessagesSquare,
    tiers: new Set(['business', 'self_host_pro', 'oem']),
  },
  {
    key: 'odoo_integration_supported',
    label: 'license.feature.odoo',
    icon: Building2,
    tiers: new Set(['business']),
  },
  {
    key: 'white_label',
    label: 'license.feature.whiteLabel',
    icon: Globe,
    tiers: new Set(['oem']),
  },
] as const;

/**
 * Classify the days-until-expiry into a visual urgency bucket. Pure helper so
 * we can exercise it in unit tests without React state.
 */
export function classifyExpiry(daysUntilExpiry: number | null | undefined): {
  tone: 'expired' | 'critical' | 'warning' | 'ok' | 'unknown';
  labelId: string;
} {
  if (daysUntilExpiry == null) return { tone: 'unknown', labelId: 'license.expiry.unknown' };
  if (daysUntilExpiry < 0)
    return { tone: 'expired', labelId: 'license.expiry.expired' };
  if (daysUntilExpiry <= 7)
    return { tone: 'critical', labelId: 'license.expiry.critical' };
  if (daysUntilExpiry <= 30)
    return { tone: 'warning', labelId: 'license.expiry.warning' };
  return { tone: 'ok', labelId: 'license.expiry.ok' };
}

function StatusBadge({
  tone,
  children,
}: {
  readonly tone: 'expired' | 'critical' | 'warning' | 'ok' | 'unknown';
  readonly children: React.ReactNode;
}) {
  const palette = {
    expired:
      'bg-rose-100 text-rose-900 border-rose-200 dark:bg-rose-900/30 dark:text-rose-200 dark:border-rose-800/40',
    critical:
      'bg-rose-100 text-rose-900 border-rose-200 dark:bg-rose-900/30 dark:text-rose-200 dark:border-rose-800/40',
    warning:
      'bg-amber-100 text-amber-900 border-amber-200 dark:bg-amber-900/30 dark:text-amber-200 dark:border-amber-800/40',
    ok: 'bg-emerald-100 text-emerald-900 border-emerald-200 dark:bg-emerald-900/30 dark:text-emerald-200 dark:border-emerald-800/40',
    unknown:
      'bg-stone-100 text-stone-700 border-stone-200 dark:bg-stone-800 dark:text-stone-300 dark:border-stone-700',
  }[tone];
  return (
    <span
      className={cn(
        'inline-flex items-center gap-1.5 rounded-full border px-3 py-1 text-xs font-medium',
        palette,
      )}
    >
      {children}
    </span>
  );
}

function PhoneHomeIndicator({
  daysSincePhoneHome,
}: {
  readonly daysSincePhoneHome: number | null | undefined;
}) {
  const intl = useIntl();
  if (daysSincePhoneHome == null) {
    return (
      <span className="text-sm text-stone-500 dark:text-stone-400">
        {intl.formatMessage({ id: 'license.phoneHome.notApplicable' })}
      </span>
    );
  }
  const tone =
    daysSincePhoneHome <= 7
      ? 'ok'
      : daysSincePhoneHome <= 30
        ? 'warning'
        : 'critical';
  return (
    <StatusBadge tone={tone}>
      <RefreshCw className="h-3.5 w-3.5" />
      {intl.formatMessage(
        { id: 'license.phoneHome.daysAgo' },
        { days: daysSincePhoneHome },
      )}
    </StatusBadge>
  );
}

export function LicensePage() {
  const intl = useIntl();
  const connectionState = useConnectionStore((s) => s.state);
  const [snapshot, setSnapshot] = useState<LicenseSnapshot | null>(null);
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);

  const load = useMemo(
    () => async () => {
      try {
        const result = await api.license.status();
        setSnapshot(result);
      } catch (e) {
        toast.error(formatError(e));
      } finally {
        setLoading(false);
        setRefreshing(false);
      }
    },
    [],
  );

  useEffect(() => {
    if (connectionState !== 'authenticated') return;
    setLoading(true);
    void load();
  }, [connectionState, load]);

  const expiryClassification = classifyExpiry(snapshot?.days_until_expiry);

  return (
    <div className="space-y-6 p-6">
      <header className="flex flex-wrap items-start justify-between gap-4">
        <div>
          <h1 className="text-2xl font-bold text-stone-900 dark:text-stone-50">
            {intl.formatMessage({ id: 'license.title' })}
          </h1>
          <p className="mt-1 text-sm text-stone-500 dark:text-stone-400">
            {intl.formatMessage({ id: 'license.subtitle' })}
          </p>
        </div>
        <button
          type="button"
          onClick={() => {
            setRefreshing(true);
            void load();
          }}
          disabled={refreshing || loading}
          className={cn(
            'inline-flex items-center gap-2 rounded-lg border border-stone-200 bg-white px-3 py-2 text-sm font-medium text-stone-700 hover:bg-stone-50 disabled:opacity-50',
            'dark:border-stone-700 dark:bg-stone-900 dark:text-stone-200 dark:hover:bg-stone-800',
          )}
        >
          <RefreshCw
            className={cn('h-4 w-4', refreshing && 'animate-spin')}
          />
          {intl.formatMessage({ id: 'license.refresh' })}
        </button>
      </header>

      {loading && !snapshot && (
        <div className="rounded-xl border border-stone-200 bg-white p-8 text-center text-stone-500 dark:border-stone-700 dark:bg-stone-900 dark:text-stone-400">
          {intl.formatMessage({ id: 'license.loading' })}
        </div>
      )}

      {snapshot && (
        <>
          {/* ── Tier card ───────────────────────────────────── */}
          <section className="rounded-xl border border-stone-200 bg-white p-6 shadow-sm dark:border-stone-700 dark:bg-stone-900">
            <div className="flex flex-wrap items-start gap-4">
              <div className="rounded-lg bg-amber-100 p-3 text-amber-700 dark:bg-amber-900/30 dark:text-amber-300">
                <KeyRound className="h-6 w-6" />
              </div>
              <div className="min-w-0 flex-1">
                <p className="text-xs uppercase tracking-wider text-stone-500 dark:text-stone-400">
                  {intl.formatMessage({ id: 'license.activeTier' })}
                </p>
                <h2 className="mt-1 text-2xl font-semibold text-stone-900 dark:text-stone-50">
                  {TIER_LABELS[snapshot.tier]}
                </h2>
                <p className="mt-1 text-sm text-stone-500 dark:text-stone-400">
                  {snapshot.installed
                    ? intl.formatMessage({ id: 'license.mode.commercial' })
                    : intl.formatMessage({ id: 'license.mode.opensource' })}
                </p>
              </div>

              <div className="flex flex-col items-end gap-2">
                <StatusBadge tone={expiryClassification.tone}>
                  <Calendar className="h-3.5 w-3.5" />
                  {snapshot.days_until_expiry != null
                    ? intl.formatMessage(
                        { id: expiryClassification.labelId },
                        { days: Math.abs(snapshot.days_until_expiry) },
                      )
                    : intl.formatMessage({ id: 'license.expiry.unknown' })}
                </StatusBadge>
                <PhoneHomeIndicator
                  daysSincePhoneHome={snapshot.days_since_phone_home}
                />
              </div>
            </div>

            <dl className="mt-6 grid grid-cols-1 gap-4 border-t border-stone-200 pt-6 sm:grid-cols-2 dark:border-stone-800">
              <DetailRow
                label={intl.formatMessage({ id: 'license.customerId' })}
                value={snapshot.customer_id ?? '—'}
              />
              <DetailRow
                label={intl.formatMessage({ id: 'license.subscriptionId' })}
                value={snapshot.subscription_id ?? '—'}
                mono
              />
              <DetailRow
                label={intl.formatMessage({ id: 'license.expiresAt' })}
                value={
                  snapshot.expires_at
                    ? new Date(snapshot.expires_at).toLocaleString()
                    : '—'
                }
              />
              <DetailRow
                label={intl.formatMessage({ id: 'license.lastPhoneHome' })}
                value={
                  snapshot.last_phone_home
                    ? new Date(snapshot.last_phone_home).toLocaleString()
                    : '—'
                }
              />
              <DetailRow
                label={intl.formatMessage({ id: 'license.fingerprintMatch' })}
                value={
                  snapshot.fingerprint_match == null
                    ? '—'
                    : snapshot.fingerprint_match
                      ? intl.formatMessage({ id: 'license.fingerprintMatch.yes' })
                      : intl.formatMessage({ id: 'license.fingerprintMatch.no' })
                }
                icon={Fingerprint}
              />
            </dl>
          </section>

          {/* ── Commercial modules matrix ───────────────────── */}
          <section className="rounded-xl border border-stone-200 bg-white p-6 shadow-sm dark:border-stone-700 dark:bg-stone-900">
            <h3 className="text-lg font-semibold text-stone-900 dark:text-stone-50">
              {intl.formatMessage({ id: 'license.modules.title' })}
            </h3>
            <p className="mt-1 text-sm text-stone-500 dark:text-stone-400">
              {intl.formatMessage({ id: 'license.modules.subtitle' })}
            </p>
            <ul className="mt-4 divide-y divide-stone-200 dark:divide-stone-800">
              {COMMERCIAL_FEATURES.map(({ key, label, icon: Icon, tiers }) => {
                const unlocked = tiers.has(snapshot.tier);
                return (
                  <li key={key} className="flex items-center gap-3 py-3">
                    <Icon
                      className={cn(
                        'h-4 w-4',
                        unlocked
                          ? 'text-emerald-600 dark:text-emerald-400'
                          : 'text-stone-400 dark:text-stone-500',
                      )}
                    />
                    <span
                      className={cn(
                        'flex-1 text-sm',
                        unlocked
                          ? 'text-stone-800 dark:text-stone-200'
                          : 'text-stone-500 dark:text-stone-500',
                      )}
                    >
                      {intl.formatMessage({ id: label })}
                    </span>
                    {unlocked ? (
                      <Check className="h-4 w-4 text-emerald-600 dark:text-emerald-400" />
                    ) : (
                      <Minus className="h-4 w-4 text-stone-400 dark:text-stone-500" />
                    )}
                  </li>
                );
              })}
            </ul>
          </section>

          {/* ── CTA: upgrade / activate / docs ──────────────── */}
          {!snapshot.installed && (
            <section className="rounded-xl border border-amber-200 bg-amber-50 p-6 dark:border-amber-900/40 dark:bg-amber-950/20">
              <h3 className="text-lg font-semibold text-amber-900 dark:text-amber-200">
                {intl.formatMessage({ id: 'license.cta.opensource.title' })}
              </h3>
              <p className="mt-2 text-sm text-amber-800 dark:text-amber-200/80">
                {intl.formatMessage({ id: 'license.cta.opensource.body' })}
              </p>
              <div className="mt-4 flex flex-wrap gap-3">
                <a
                  href="https://duduclaw.tw#pricing"
                  target="_blank"
                  rel="noopener noreferrer"
                  className="inline-flex items-center gap-2 rounded-lg bg-amber-600 px-4 py-2 text-sm font-medium text-white hover:bg-amber-700"
                >
                  {intl.formatMessage({ id: 'license.cta.pricing' })}
                  <ExternalLink className="h-3.5 w-3.5" />
                </a>
                <a
                  href="https://github.com/zhixuli0406/DuDuClaw#-installation"
                  target="_blank"
                  rel="noopener noreferrer"
                  className="inline-flex items-center gap-2 rounded-lg border border-amber-300 bg-white px-4 py-2 text-sm font-medium text-amber-900 hover:bg-amber-50 dark:border-amber-800/40 dark:bg-stone-900 dark:text-amber-200 dark:hover:bg-stone-800"
                >
                  {intl.formatMessage({ id: 'license.cta.docs' })}
                  <ExternalLink className="h-3.5 w-3.5" />
                </a>
              </div>
            </section>
          )}

          {snapshot.installed && expiryClassification.tone !== 'ok' && (
            <section className="rounded-xl border border-amber-200 bg-amber-50 p-6 dark:border-amber-900/40 dark:bg-amber-950/20">
              <h3 className="text-lg font-semibold text-amber-900 dark:text-amber-200">
                {intl.formatMessage({ id: 'license.cta.renew.title' })}
              </h3>
              <p className="mt-2 text-sm text-amber-800 dark:text-amber-200/80">
                {intl.formatMessage({ id: 'license.cta.renew.body' })}
              </p>
              <div className="mt-4 flex flex-wrap gap-3">
                <a
                  href="https://duduclaw.tw#pricing"
                  target="_blank"
                  rel="noopener noreferrer"
                  className="inline-flex items-center gap-2 rounded-lg bg-amber-600 px-4 py-2 text-sm font-medium text-white hover:bg-amber-700"
                >
                  {intl.formatMessage({ id: 'license.cta.renew.action' })}
                  <ExternalLink className="h-3.5 w-3.5" />
                </a>
              </div>
            </section>
          )}

          {/* ── CLI hint ───────────────────────────────────── */}
          <section className="rounded-xl border border-stone-200 bg-stone-50 p-6 dark:border-stone-700 dark:bg-stone-900/50">
            <h3 className="text-sm font-semibold uppercase tracking-wider text-stone-600 dark:text-stone-400">
              {intl.formatMessage({ id: 'license.cli.title' })}
            </h3>
            <p className="mt-2 text-sm text-stone-600 dark:text-stone-300">
              {intl.formatMessage({ id: 'license.cli.body' })}
            </p>
            <ul className="mt-3 space-y-1 font-mono text-xs text-stone-700 dark:text-stone-300">
              <li>$ duduclaw license fingerprint</li>
              <li>$ duduclaw license activate &lt;key&gt;</li>
              <li>$ duduclaw license refresh</li>
              <li>$ duduclaw license deactivate</li>
            </ul>
          </section>
        </>
      )}
    </div>
  );
}

function DetailRow({
  label,
  value,
  mono = false,
  icon: Icon,
}: {
  readonly label: string;
  readonly value: string;
  readonly mono?: boolean;
  readonly icon?: typeof Fingerprint;
}) {
  return (
    <div>
      <dt className="flex items-center gap-1.5 text-xs uppercase tracking-wider text-stone-500 dark:text-stone-400">
        {Icon && <Icon className="h-3.5 w-3.5" />}
        {label}
      </dt>
      <dd
        className={cn(
          'mt-1 text-sm text-stone-800 dark:text-stone-200',
          mono && 'font-mono break-all',
        )}
      >
        {value}
      </dd>
    </div>
  );
}
