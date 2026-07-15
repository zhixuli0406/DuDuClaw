import { useEffect, useMemo, useState } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate } from 'react-router';
import { useConnectionStore } from '@/stores/connection-store';
import { useAgentsStore } from '@/stores/agents-store';
import { api, type LicenseSnapshot } from '@/lib/api';
import { TIER_LABELS } from '@/lib/license-labels';
import { cn } from '@/lib/utils';
import { toast, formatError } from '@/lib/toast';
import { Page, PageHeader, Card, Section, StatCard, Badge, Button, Mono, Field, controlClass } from '@/components/ui';
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
  Award,
  Copy,
  Ticket,
} from 'lucide-react';

const PRICING_URL = 'https://duduclaw.dudustudio.monster#pricing';

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
    tiers: new Set([
      'studio',
      'business',
      'partner',
      'personal_pro_self_host',
      'self_host_pro',
      'oem',
    ]),
  },
  {
    key: 'industry_evolution_params',
    label: 'license.feature.evolutionParams',
    icon: FileBarChart2,
    tiers: new Set(['business', 'partner', 'self_host_pro', 'oem']),
  },
  {
    key: 'dashboard_enterprise',
    label: 'license.feature.dashboardEnterprise',
    icon: Database,
    tiers: new Set(['business', 'partner', 'self_host_pro', 'oem']),
  },
  {
    key: 'priority_security_patch',
    label: 'license.feature.prioritySecurityPatch',
    icon: ShieldCheck,
    tiers: new Set([
      'business',
      'partner',
      'personal_pro_self_host',
      'self_host_pro',
      'oem',
    ]),
  },
  {
    key: 'private_discord_support',
    label: 'license.feature.privateDiscord',
    icon: MessagesSquare,
    tiers: new Set([
      'business',
      'partner',
      'personal_pro_self_host',
      'self_host_pro',
      'oem',
    ]),
  },
  {
    key: 'odoo_integration_supported',
    label: 'license.feature.odoo',
    icon: Building2,
    tiers: new Set(['business', 'partner']),
  },
  {
    key: 'white_label',
    label: 'license.feature.whiteLabel',
    icon: Globe,
    tiers: new Set(['oem']),
  },
] as const;

type ExpiryTone = 'expired' | 'critical' | 'warning' | 'ok' | 'unknown';

/**
 * Classify the days-until-expiry into a visual urgency bucket. Pure helper so
 * we can exercise it in unit tests without React state.
 */
export function classifyExpiry(daysUntilExpiry: number | null | undefined): {
  tone: ExpiryTone;
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

/** Map an expiry/phone-home urgency tone onto a Calm Glass Badge tone. */
const BADGE_TONE: Record<ExpiryTone, 'danger' | 'warning' | 'success' | 'neutral'> = {
  expired: 'danger',
  critical: 'danger',
  warning: 'warning',
  ok: 'success',
  unknown: 'neutral',
};

/** StatCard tone equivalents for the hero metric tiles. */
const STAT_TONE: Record<ExpiryTone, 'danger' | 'warning' | 'success' | 'neutral'> = {
  expired: 'danger',
  critical: 'danger',
  warning: 'warning',
  ok: 'success',
  unknown: 'neutral',
};

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
  const tone: ExpiryTone =
    daysSincePhoneHome <= 7
      ? 'ok'
      : daysSincePhoneHome <= 30
        ? 'warning'
        : 'critical';
  return (
    <Badge tone={BADGE_TONE[tone]}>
      <RefreshCw className="h-3.5 w-3.5" />
      {intl.formatMessage(
        { id: 'license.phoneHome.daysAgo' },
        { days: daysSincePhoneHome },
      )}
    </Badge>
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
    <Page>
      <PageHeader
        icon={KeyRound}
        title={intl.formatMessage({ id: 'nav.license' })}
        subtitle={intl.formatMessage({ id: 'license.subtitle' })}
        actions={
          <Button
            variant="secondary"
            onClick={() => {
              setRefreshing(true);
              void load();
            }}
            disabled={refreshing || loading}
            icon={refreshing ? undefined : RefreshCw}
          >
            {refreshing && <RefreshCw className="h-4 w-4 animate-spin" />}
            {intl.formatMessage({ id: 'license.refresh' })}
          </Button>
        }
      />

      {loading && !snapshot && (
        <Card>
          <p className="py-8 text-center text-sm text-stone-500 dark:text-stone-400">
            {intl.formatMessage({ id: 'license.loading' })}
          </p>
        </Card>
      )}

      {snapshot && (
        <>
          {/* ── Hero metrics: tier / expiry / phone-home ────── */}
          <div className="grid grid-cols-1 gap-4 sm:grid-cols-2 lg:grid-cols-3">
            <StatCard
              icon={Award}
              tone="accent"
              label={intl.formatMessage({ id: 'license.activeTier' })}
              value={TIER_LABELS[snapshot.tier]}
              hint={
                snapshot.installed
                  ? intl.formatMessage({ id: 'license.mode.commercial' })
                  : intl.formatMessage({ id: 'license.mode.opensource' })
              }
            />
            <StatCard
              icon={Calendar}
              tone={STAT_TONE[expiryClassification.tone]}
              label={intl.formatMessage({ id: 'license.expiresAt' })}
              value={
                snapshot.days_until_expiry != null
                  ? intl.formatMessage(
                      { id: expiryClassification.labelId },
                      { days: Math.abs(snapshot.days_until_expiry) },
                    )
                  : intl.formatMessage({ id: 'license.expiry.unknown' })
              }
              hint={
                snapshot.expires_at
                  ? new Date(snapshot.expires_at).toLocaleString()
                  : '—'
              }
            />
            <StatCard
              icon={RefreshCw}
              tone="neutral"
              label={intl.formatMessage({ id: 'license.lastPhoneHome' })}
              value={<PhoneHomeIndicator daysSincePhoneHome={snapshot.days_since_phone_home} />}
              hint={
                snapshot.last_phone_home
                  ? new Date(snapshot.last_phone_home).toLocaleString()
                  : '—'
              }
            />
          </div>

          {/* ── Upgrade / activate — prominent while on OpenSource ── */}
          {!snapshot.installed && (
            <ActivateLicenseCard collapsed={false} onActivated={() => void load()} />
          )}

          {/* ── License details ─────────────────────────────── */}
          <Card title={intl.formatMessage({ id: 'license.activeTier' })}>
            <dl className="grid grid-cols-1 gap-4 sm:grid-cols-2">
              <DetailRow
                label={intl.formatMessage({ id: 'license.customerId' })}
                value={snapshot.customer_id ?? '—'}
                mono
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
                mono
              />
              <DetailRow
                label={intl.formatMessage({ id: 'license.lastPhoneHome' })}
                value={
                  snapshot.last_phone_home
                    ? new Date(snapshot.last_phone_home).toLocaleString()
                    : '—'
                }
                mono
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
          </Card>

          {/* ── Commercial modules matrix ───────────────────── */}
          <Card
            title={intl.formatMessage({ id: 'license.modules.title' })}
            padded={false}
          >
            <p className="border-b border-[var(--panel-border)] px-5 py-3 text-sm text-stone-500 dark:text-stone-400">
              {intl.formatMessage({ id: 'license.modules.subtitle' })}
            </p>
            <ul className="divide-y divide-[var(--panel-border)]">
              {COMMERCIAL_FEATURES.map(({ key, label, icon: Icon, tiers }) => {
                const unlocked = tiers.has(snapshot.tier);
                return (
                  <li key={key} className="flex items-center gap-3 px-5 py-3">
                    <Icon
                      className={cn(
                        'h-4 w-4 shrink-0',
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
                      <Badge tone="success">
                        <Check className="h-3.5 w-3.5" />
                      </Badge>
                    ) : (
                      <Badge tone="neutral">
                        <Minus className="h-3.5 w-3.5" />
                      </Badge>
                    )}
                  </li>
                );
              })}
            </ul>
          </Card>

          {/* ── CTA: upgrade / activate / docs ──────────────── */}
          {!snapshot.installed && (
            <Section
              title={intl.formatMessage({ id: 'license.cta.opensource.title' })}
              description={intl.formatMessage({ id: 'license.cta.opensource.body' })}
            >
              <div className="flex flex-wrap gap-3">
                <a
                  href={PRICING_URL}
                  target="_blank"
                  rel="noopener noreferrer"
                >
                  <Button variant="primary" iconRight={ExternalLink}>
                    {intl.formatMessage({ id: 'license.cta.pricing' })}
                  </Button>
                </a>
                <a
                  href="https://github.com/zhixuli0406/DuDuClaw#-installation"
                  target="_blank"
                  rel="noopener noreferrer"
                >
                  <Button variant="secondary" iconRight={ExternalLink}>
                    {intl.formatMessage({ id: 'license.cta.docs' })}
                  </Button>
                </a>
              </div>
            </Section>
          )}

          {snapshot.installed && expiryClassification.tone !== 'ok' && (
            <Section
              title={intl.formatMessage({ id: 'license.cta.renew.title' })}
              description={intl.formatMessage({ id: 'license.cta.renew.body' })}
            >
              <div className="flex flex-wrap gap-3">
                <a
                  href={PRICING_URL}
                  target="_blank"
                  rel="noopener noreferrer"
                >
                  <Button variant="primary" iconRight={ExternalLink}>
                    {intl.formatMessage({ id: 'license.cta.renew.action' })}
                  </Button>
                </a>
              </div>
            </Section>
          )}

          {/* ── Replace / re-activate — collapsed once licensed ── */}
          {snapshot.installed && (
            <ActivateLicenseCard collapsed onActivated={() => void load()} />
          )}

          {/* ── CLI hint ───────────────────────────────────── */}
          <Card title={intl.formatMessage({ id: 'license.cli.title' })}>
            <p className="text-sm text-stone-600 dark:text-stone-300">
              {intl.formatMessage({ id: 'license.cli.body' })}
            </p>
            <ul className="mt-3 space-y-1 font-mono text-xs text-stone-700 dark:text-stone-300">
              <li>$ duduclaw license fingerprint</li>
              <li>$ duduclaw license activate &lt;key&gt;</li>
              <li>$ duduclaw license refresh</li>
              <li>$ duduclaw license deactivate</li>
            </ul>

            <p className="mt-4 text-sm font-medium text-stone-700 dark:text-stone-300">
              {intl.formatMessage({ id: 'license.cli.selfService' })}
            </p>
            <ul className="mt-2 space-y-1 font-mono text-xs text-stone-700 dark:text-stone-300">
              {/* Free partner (NFR) path — redeem a code, no payment. */}
              <li>$ duduclaw license redeem &lt;PARTNER-CODE&gt;</li>
              {/* Self-service machine migration (re-sign for this machine). */}
              <li>$ duduclaw license rebind</li>
              {/* Remote subscription / renewal status from the control-plane. */}
              <li>$ duduclaw license subscriptions</li>
            </ul>
          </Card>
        </>
      )}
    </Page>
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
      <dd className="mt-1 text-sm text-stone-800 dark:text-stone-200">
        {mono ? <Mono className="break-all text-stone-800 dark:text-stone-200">{value}</Mono> : value}
      </dd>
    </div>
  );
}

/**
 * Upgrade / activate license card — machine fingerprint (copyable), license
 * key activation, and the free partner redeem-code path. Prominent while on
 * OpenSource; collapsed into a <details> once a commercial license is live.
 * Activation hot-reloads the gateway LicenseRuntime, so `onActivated` only
 * needs to re-fetch `license.status` — no restart.
 */
function ActivateLicenseCard({
  collapsed,
  onActivated,
}: {
  readonly collapsed: boolean;
  readonly onActivated: () => void;
}) {
  const intl = useIntl();
  const navigate = useNavigate();
  // Zero agents ⇒ the operator is still mid-onboarding (came here from the
  // welcome wizard's industry step) — offer a way back after activating.
  const agentCount = useAgentsStore((s) => s.agents.length);

  const [fingerprint, setFingerprint] = useState<string | null>(null);
  const [key, setKey] = useState('');
  const [activating, setActivating] = useState(false);
  const [activateError, setActivateError] = useState<string | null>(null);
  const [code, setCode] = useState('');
  const [email, setEmail] = useState('');
  const [redeeming, setRedeeming] = useState(false);
  const [redeemError, setRedeemError] = useState<string | null>(null);
  const [activated, setActivated] = useState(false);

  useEffect(() => {
    let alive = true;
    api.license
      .fingerprint()
      .then((r) => alive && setFingerprint(r.fingerprint))
      .catch(() => {/* row shows a dash */});
    return () => {
      alive = false;
    };
  }, []);

  const copyFingerprint = async () => {
    if (!fingerprint) return;
    try {
      await navigator.clipboard.writeText(fingerprint);
      toast.success(intl.formatMessage({ id: 'license.activate.fingerprint.copied' }));
    } catch {
      toast.error(intl.formatMessage({ id: 'license.activate.fingerprint.copyFailed' }));
    }
  };

  const handleActivate = async () => {
    if (!key.trim()) return;
    setActivateError(null);
    setActivating(true);
    try {
      await api.license.activate(key.trim());
      toast.success(intl.formatMessage({ id: 'license.activate.success' }));
      setKey('');
      setActivated(true);
      onActivated();
    } catch (e) {
      // Gateway error strings are already localized zh-TW — show verbatim.
      setActivateError(formatError(e));
    } finally {
      setActivating(false);
    }
  };

  const handleRedeem = async () => {
    if (!code.trim()) return;
    setRedeemError(null);
    setRedeeming(true);
    try {
      await api.license.redeem(code.trim(), email.trim() || undefined);
      toast.success(intl.formatMessage({ id: 'license.activate.success' }));
      setCode('');
      setEmail('');
      setActivated(true);
      onActivated();
    } catch (e) {
      setRedeemError(formatError(e));
    } finally {
      setRedeeming(false);
    }
  };

  const body = (
    <div className="space-y-6">
      {/* 1 — machine fingerprint + purchase link */}
      <div className="space-y-2">
        <p className="flex items-center gap-1.5 text-xs uppercase tracking-wider text-stone-500 dark:text-stone-400">
          <Fingerprint className="h-3.5 w-3.5" />
          {intl.formatMessage({ id: 'license.activate.fingerprint' })}
        </p>
        <div className="flex flex-wrap items-center gap-2">
          <Mono className="break-all text-stone-800 dark:text-stone-200">
            {fingerprint ?? '—'}
          </Mono>
          <Button variant="secondary" icon={Copy} onClick={() => void copyFingerprint()} disabled={!fingerprint}>
            {intl.formatMessage({ id: 'license.activate.fingerprint.copy' })}
          </Button>
        </div>
        <p className="text-xs text-stone-500 dark:text-stone-400">
          {intl.formatMessage({ id: 'license.activate.fingerprint.hint' })}{' '}
          <a
            href={PRICING_URL}
            target="_blank"
            rel="noopener noreferrer"
            className="font-medium text-amber-600 hover:underline dark:text-amber-400"
          >
            {intl.formatMessage({ id: 'license.cta.pricing' })}
            <ExternalLink className="ml-0.5 inline h-3 w-3" />
          </a>
        </p>
      </div>

      {/* 2 — activate with a license key */}
      <div className="space-y-3">
        <Field
          label={intl.formatMessage({ id: 'license.activate.key.label' })}
          help={intl.formatMessage({ id: 'license.activate.key.hint' })}
        >
          <textarea
            value={key}
            onChange={(e) => setKey(e.target.value)}
            spellCheck={false}
            className={cn(controlClass, 'min-h-28 resize-y font-mono text-xs leading-relaxed')}
          />
        </Field>
        {activateError && (
          <p className="text-sm text-rose-600 dark:text-rose-400">{activateError}</p>
        )}
        <Button
          variant="primary"
          icon={KeyRound}
          onClick={() => void handleActivate()}
          disabled={activating || !key.trim()}
        >
          {intl.formatMessage({
            id: activating ? 'license.activate.submitting' : 'license.activate.submit',
          })}
        </Button>
      </div>

      {/* 3 — partner (NFR) redeem code, free path */}
      <div className="space-y-3 border-t border-[var(--panel-border)] pt-4">
        <p className="flex items-center gap-1.5 text-xs uppercase tracking-wider text-stone-500 dark:text-stone-400">
          <Ticket className="h-3.5 w-3.5" />
          {intl.formatMessage({ id: 'license.activate.redeem.title' })}
        </p>
        <div className="grid gap-3 sm:grid-cols-2">
          <Field label={intl.formatMessage({ id: 'license.activate.redeem.code' })}>
            <input
              type="text"
              value={code}
              onChange={(e) => setCode(e.target.value)}
              spellCheck={false}
              className={cn(controlClass, 'font-mono')}
              placeholder="PARTNER-CODE"
            />
          </Field>
          <Field label={intl.formatMessage({ id: 'license.activate.redeem.email' })}>
            <input
              type="email"
              value={email}
              onChange={(e) => setEmail(e.target.value)}
              className={controlClass}
              autoComplete="off"
            />
          </Field>
        </div>
        {redeemError && (
          <p className="text-sm text-rose-600 dark:text-rose-400">{redeemError}</p>
        )}
        <Button
          variant="secondary"
          onClick={() => void handleRedeem()}
          disabled={redeeming || !code.trim()}
        >
          {intl.formatMessage({
            id: redeeming ? 'license.activate.redeem.submitting' : 'license.activate.redeem.submit',
          })}
        </Button>
      </div>

      {/* First-run: the operator came from the welcome wizard — send them back. */}
      {activated && agentCount === 0 && (
        <div className="border-t border-[var(--panel-border)] pt-4">
          <Button variant="primary" onClick={() => navigate('/welcome')}>
            {intl.formatMessage({ id: 'license.activate.backToWizard' })}
          </Button>
        </div>
      )}
    </div>
  );

  if (collapsed) {
    return (
      <Card>
        <details>
          <summary className="cursor-pointer text-sm font-medium text-stone-700 dark:text-stone-300">
            {intl.formatMessage({ id: 'license.activate.reopen' })}
          </summary>
          <div className="mt-4">{body}</div>
        </details>
      </Card>
    );
  }

  return <Card title={intl.formatMessage({ id: 'license.activate.title' })}>{body}</Card>;
}
