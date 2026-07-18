import { useEffect, useMemo, useState, type ReactNode } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate } from 'react-router';
import { useConnectionStore } from '@/stores/connection-store';
import { useAgentsStore } from '@/stores/agents-store';
import { api, type LicenseSnapshot } from '@/lib/api';
import { TIER_LABELS } from '@/lib/license-labels';
import { cn } from '@/lib/utils';
import { toast, formatError } from '@/lib/toast';
import {
  Card,
  CardHeader,
  CardTitle,
  CardDescription,
  CardContent,
  Badge,
  Button,
  Input,
  Textarea,
  SettingsCard,
  SettingsRow,
  Table,
  TableHeader,
  TableBody,
  TableRow,
  TableHead,
  TableCell,
} from '@/components/mds';
import {
  KeyRound,
  ShieldCheck,
  Fingerprint,
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

/** Map an expiry urgency tone onto an MDS Badge className (semantic tint). */
const BADGE_CLASS: Record<'ok' | 'warning' | 'critical', string> = {
  ok: 'bg-success/15 text-success',
  warning: 'bg-warning/15 text-warning',
  critical: 'bg-destructive/10 text-destructive',
};

/** Map an expiry tone onto the KPI value text color. */
const EXPIRY_TEXT: Record<ExpiryTone, string> = {
  expired: 'text-destructive',
  critical: 'text-destructive',
  warning: 'text-warning',
  ok: 'text-success',
  unknown: 'text-muted-foreground',
};

/** Stacked label + control block (DialogField pattern, spec §5.3). */
function LicenseField({
  label,
  help,
  children,
}: {
  label: string;
  help?: string;
  children: ReactNode;
}) {
  return (
    <div className="space-y-1.5">
      <label className="text-sm font-medium text-foreground">{label}</label>
      {children}
      {help && <p className="text-xs text-muted-foreground">{help}</p>}
    </div>
  );
}

/** A compact KPI tile built on an MDS Card. */
function StatTile({
  label,
  value,
  valueClassName,
  sub,
}: {
  label: string;
  value: ReactNode;
  valueClassName?: string;
  sub?: ReactNode;
}) {
  return (
    <Card>
      <CardContent className="space-y-1">
        <p className="text-sm text-muted-foreground">{label}</p>
        <div className={cn('text-lg font-semibold', valueClassName)}>{value}</div>
        {sub && <p className="text-xs text-muted-foreground">{sub}</p>}
      </CardContent>
    </Card>
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
      <span className="text-sm text-muted-foreground">
        {intl.formatMessage({ id: 'license.phoneHome.notApplicable' })}
      </span>
    );
  }
  const tone: 'ok' | 'warning' | 'critical' =
    daysSincePhoneHome <= 7 ? 'ok' : daysSincePhoneHome <= 30 ? 'warning' : 'critical';
  return (
    <Badge variant="secondary" className={BADGE_CLASS[tone]}>
      <RefreshCw className="size-3.5" />
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
    <div className="mx-auto w-full max-w-4xl space-y-6">
      {/* Header */}
      <div className="flex items-center justify-between gap-3">
        <div className="flex min-w-0 items-center gap-2">
          <KeyRound className="size-5 text-muted-foreground" />
          <div>
            <h1 className="text-base font-medium">{intl.formatMessage({ id: 'nav.license' })}</h1>
            <p className="text-sm text-muted-foreground">{intl.formatMessage({ id: 'license.subtitle' })}</p>
          </div>
        </div>
        <Button
          variant="outline"
          size="sm"
          onClick={() => {
            setRefreshing(true);
            void load();
          }}
          disabled={refreshing || loading}
        >
          <RefreshCw className={cn(refreshing && 'animate-spin')} />
          {intl.formatMessage({ id: 'license.refresh' })}
        </Button>
      </div>

      {loading && !snapshot && (
        <Card>
          <CardContent>
            <p className="py-8 text-center text-sm text-muted-foreground">
              {intl.formatMessage({ id: 'license.loading' })}
            </p>
          </CardContent>
        </Card>
      )}

      {snapshot && (
        <>
          {/* ── KPI status: tier / expiry / phone-home ────── */}
          <div className="grid grid-cols-1 gap-4 sm:grid-cols-3">
            <StatTile
              label={intl.formatMessage({ id: 'license.activeTier' })}
              value={TIER_LABELS[snapshot.tier]}
              sub={
                snapshot.installed
                  ? intl.formatMessage({ id: 'license.mode.commercial' })
                  : intl.formatMessage({ id: 'license.mode.opensource' })
              }
            />
            <StatTile
              label={intl.formatMessage({ id: 'license.expiresAt' })}
              valueClassName={EXPIRY_TEXT[expiryClassification.tone]}
              value={
                snapshot.days_until_expiry != null
                  ? intl.formatMessage(
                      { id: expiryClassification.labelId },
                      { days: Math.abs(snapshot.days_until_expiry) },
                    )
                  : intl.formatMessage({ id: 'license.expiry.unknown' })
              }
              sub={
                snapshot.expires_at ? new Date(snapshot.expires_at).toLocaleString() : '—'
              }
            />
            <StatTile
              label={intl.formatMessage({ id: 'license.lastPhoneHome' })}
              value={<PhoneHomeIndicator daysSincePhoneHome={snapshot.days_since_phone_home} />}
              sub={
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
          <SettingsCard>
            <SettingsRow label={intl.formatMessage({ id: 'license.customerId' })}>
              <span className="font-mono text-xs break-all">{snapshot.customer_id ?? '—'}</span>
            </SettingsRow>
            <SettingsRow label={intl.formatMessage({ id: 'license.subscriptionId' })}>
              <span className="font-mono text-xs break-all">{snapshot.subscription_id ?? '—'}</span>
            </SettingsRow>
            <SettingsRow label={intl.formatMessage({ id: 'license.expiresAt' })}>
              <span className="font-mono text-xs">
                {snapshot.expires_at ? new Date(snapshot.expires_at).toLocaleString() : '—'}
              </span>
            </SettingsRow>
            <SettingsRow label={intl.formatMessage({ id: 'license.lastPhoneHome' })}>
              <span className="font-mono text-xs">
                {snapshot.last_phone_home ? new Date(snapshot.last_phone_home).toLocaleString() : '—'}
              </span>
            </SettingsRow>
            <SettingsRow
              label={
                <span className="flex items-center gap-1.5">
                  <Fingerprint className="size-3.5" />
                  {intl.formatMessage({ id: 'license.fingerprintMatch' })}
                </span>
              }
            >
              <span className="text-sm">
                {snapshot.fingerprint_match == null
                  ? '—'
                  : snapshot.fingerprint_match
                    ? intl.formatMessage({ id: 'license.fingerprintMatch.yes' })
                    : intl.formatMessage({ id: 'license.fingerprintMatch.no' })}
              </span>
            </SettingsRow>
          </SettingsCard>

          {/* ── Commercial modules matrix ───────────────────── */}
          <Card>
            <CardHeader>
              <CardTitle>{intl.formatMessage({ id: 'license.modules.title' })}</CardTitle>
              <CardDescription>{intl.formatMessage({ id: 'license.modules.subtitle' })}</CardDescription>
            </CardHeader>
            <CardContent className="p-0">
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead>{intl.formatMessage({ id: 'license.modules.col.module' })}</TableHead>
                    <TableHead className="text-right">
                      {intl.formatMessage({ id: 'license.modules.col.status' })}
                    </TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {COMMERCIAL_FEATURES.map(({ key, label, icon: Icon, tiers }) => {
                    const unlocked = tiers.has(snapshot.tier);
                    return (
                      <TableRow key={key} className={cn(unlocked && 'bg-surface-selected')}>
                        <TableCell>
                          <span className="flex items-center gap-2">
                            <Icon
                              className={cn(
                                'size-4 shrink-0',
                                unlocked ? 'text-success' : 'text-muted-foreground/50',
                              )}
                            />
                            <span className={cn('text-sm', !unlocked && 'text-muted-foreground')}>
                              {intl.formatMessage({ id: label })}
                            </span>
                          </span>
                        </TableCell>
                        <TableCell className="text-right">
                          {unlocked ? (
                            <Badge variant="secondary" className="bg-success/15 text-success">
                              <Check className="size-3.5" />
                            </Badge>
                          ) : (
                            <Badge variant="ghost">
                              <Minus className="size-3.5" />
                            </Badge>
                          )}
                        </TableCell>
                      </TableRow>
                    );
                  })}
                </TableBody>
              </Table>
            </CardContent>
          </Card>

          {/* ── CTA: upgrade / activate / docs ──────────────── */}
          {!snapshot.installed && (
            <Card>
              <CardHeader>
                <CardTitle>{intl.formatMessage({ id: 'license.cta.opensource.title' })}</CardTitle>
                <CardDescription>{intl.formatMessage({ id: 'license.cta.opensource.body' })}</CardDescription>
              </CardHeader>
              <CardContent>
                <div className="flex flex-wrap gap-3">
                  <a href={PRICING_URL} target="_blank" rel="noopener noreferrer">
                    <Button variant="brand">
                      {intl.formatMessage({ id: 'license.cta.pricing' })}
                      <ExternalLink />
                    </Button>
                  </a>
                  <a
                    href="https://github.com/zhixuli0406/DuDuClaw#-installation"
                    target="_blank"
                    rel="noopener noreferrer"
                  >
                    <Button variant="outline">
                      {intl.formatMessage({ id: 'license.cta.docs' })}
                      <ExternalLink />
                    </Button>
                  </a>
                </div>
              </CardContent>
            </Card>
          )}

          {snapshot.installed && expiryClassification.tone !== 'ok' && (
            <Card>
              <CardHeader>
                <CardTitle>{intl.formatMessage({ id: 'license.cta.renew.title' })}</CardTitle>
                <CardDescription>{intl.formatMessage({ id: 'license.cta.renew.body' })}</CardDescription>
              </CardHeader>
              <CardContent>
                <div className="flex flex-wrap gap-3">
                  <a href={PRICING_URL} target="_blank" rel="noopener noreferrer">
                    <Button variant="brand">
                      {intl.formatMessage({ id: 'license.cta.renew.action' })}
                      <ExternalLink />
                    </Button>
                  </a>
                </div>
              </CardContent>
            </Card>
          )}

          {/* ── Replace / re-activate — collapsed once licensed ── */}
          {snapshot.installed && (
            <ActivateLicenseCard collapsed onActivated={() => void load()} />
          )}

          {/* ── CLI hint ───────────────────────────────────── */}
          <Card>
            <CardHeader>
              <CardTitle>{intl.formatMessage({ id: 'license.cli.title' })}</CardTitle>
            </CardHeader>
            <CardContent className="space-y-3">
              <p className="text-sm text-muted-foreground">
                {intl.formatMessage({ id: 'license.cli.body' })}
              </p>
              <ul className="space-y-1 font-mono text-xs text-foreground">
                <li>$ duduclaw license fingerprint</li>
                <li>$ duduclaw license activate &lt;key&gt;</li>
                <li>$ duduclaw license refresh</li>
                <li>$ duduclaw license deactivate</li>
              </ul>

              <p className="pt-1 text-sm font-medium text-foreground">
                {intl.formatMessage({ id: 'license.cli.selfService' })}
              </p>
              <ul className="space-y-1 font-mono text-xs text-foreground">
                {/* Free partner (NFR) path — redeem a code, no payment. */}
                <li>$ duduclaw license redeem &lt;PARTNER-CODE&gt;</li>
                {/* Self-service machine migration (re-sign for this machine). */}
                <li>$ duduclaw license rebind</li>
                {/* Remote subscription / renewal status from the control-plane. */}
                <li>$ duduclaw license subscriptions</li>
              </ul>
            </CardContent>
          </Card>
        </>
      )}
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
        <p className="flex items-center gap-1.5 text-xs font-medium uppercase tracking-wide text-muted-foreground">
          <Fingerprint className="size-3.5" />
          {intl.formatMessage({ id: 'license.activate.fingerprint' })}
        </p>
        <div className="flex flex-wrap items-center gap-2">
          <span className="break-all font-mono text-xs">{fingerprint ?? '—'}</span>
          <Button variant="outline" size="sm" onClick={() => void copyFingerprint()} disabled={!fingerprint}>
            <Copy />
            {intl.formatMessage({ id: 'license.activate.fingerprint.copy' })}
          </Button>
        </div>
        <p className="text-xs text-muted-foreground">
          {intl.formatMessage({ id: 'license.activate.fingerprint.hint' })}{' '}
          <a
            href={PRICING_URL}
            target="_blank"
            rel="noopener noreferrer"
            className="font-medium text-brand hover:underline"
          >
            {intl.formatMessage({ id: 'license.cta.pricing' })}
            <ExternalLink className="ml-0.5 inline size-3" />
          </a>
        </p>
      </div>

      {/* 2 — activate with a license key */}
      <div className="space-y-3">
        <LicenseField
          label={intl.formatMessage({ id: 'license.activate.key.label' })}
          help={intl.formatMessage({ id: 'license.activate.key.hint' })}
        >
          <Textarea
            value={key}
            onChange={(e) => setKey(e.target.value)}
            spellCheck={false}
            className="min-h-28 resize-y font-mono text-xs leading-relaxed"
          />
        </LicenseField>
        {activateError && <p className="text-sm text-destructive">{activateError}</p>}
        <Button variant="brand" onClick={() => void handleActivate()} disabled={activating || !key.trim()}>
          <KeyRound />
          {intl.formatMessage({
            id: activating ? 'license.activate.submitting' : 'license.activate.submit',
          })}
        </Button>
      </div>

      {/* 3 — partner (NFR) redeem code, free path */}
      <div className="space-y-3 border-t border-surface-border pt-4">
        <p className="flex items-center gap-1.5 text-xs font-medium uppercase tracking-wide text-muted-foreground">
          <Ticket className="size-3.5" />
          {intl.formatMessage({ id: 'license.activate.redeem.title' })}
        </p>
        <div className="grid gap-3 sm:grid-cols-2">
          <LicenseField label={intl.formatMessage({ id: 'license.activate.redeem.code' })}>
            <Input
              type="text"
              value={code}
              onChange={(e) => setCode(e.target.value)}
              spellCheck={false}
              className="font-mono"
              placeholder="PARTNER-CODE"
            />
          </LicenseField>
          <LicenseField label={intl.formatMessage({ id: 'license.activate.redeem.email' })}>
            <Input
              type="email"
              value={email}
              onChange={(e) => setEmail(e.target.value)}
              autoComplete="off"
            />
          </LicenseField>
        </div>
        {redeemError && <p className="text-sm text-destructive">{redeemError}</p>}
        <Button variant="secondary" onClick={() => void handleRedeem()} disabled={redeeming || !code.trim()}>
          {intl.formatMessage({
            id: redeeming ? 'license.activate.redeem.submitting' : 'license.activate.redeem.submit',
          })}
        </Button>
      </div>

      {/* First-run: the operator came from the welcome wizard — send them back. */}
      {activated && agentCount === 0 && (
        <div className="border-t border-surface-border pt-4">
          <Button variant="brand" onClick={() => navigate('/welcome')}>
            {intl.formatMessage({ id: 'license.activate.backToWizard' })}
          </Button>
        </div>
      )}
    </div>
  );

  if (collapsed) {
    return (
      <Card>
        <CardContent>
          <details>
            <summary className="cursor-pointer text-sm font-medium text-foreground">
              {intl.formatMessage({ id: 'license.activate.reopen' })}
            </summary>
            <div className="mt-4">{body}</div>
          </details>
        </CardContent>
      </Card>
    );
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle>{intl.formatMessage({ id: 'license.activate.title' })}</CardTitle>
      </CardHeader>
      <CardContent>{body}</CardContent>
    </Card>
  );
}
