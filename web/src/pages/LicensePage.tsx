import { formatDateTime, useCompanyTimeZone } from '@/lib/company-time';
import { useAuthStore } from '@/stores/auth-store';
import { LicenseValidationWarning } from '@/components/LicenseValidationWarning';
import { useEffect, useMemo, useState, type ReactNode } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate } from 'react-router';
import { useConnectionStore } from '@/stores/connection-store';
import { useAgentsStore } from '@/stores/agents-store';
import { api, type LicenseSnapshot, type LicenseSyncResult } from '@/lib/api';
import { TIER_LABELS } from '@/lib/license-labels';
import { useCommercialContact } from '@/lib/commercial-contact';
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
  ErrorState,
  Textarea,
  SettingsCard,
  SettingsRow,
} from '@/components/mds';
import {
  KeyRound,
  Fingerprint,
  RefreshCw,
  ExternalLink,
  Copy,
  FlaskConical,
} from 'lucide-react';

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

type SyncOutcome = LicenseSyncResult['sync']['outcome'];

/** `license.sync` outcome → plain-words message id (static ids, no templating). */
const SYNC_OUTCOME_MESSAGE: Record<SyncOutcome, string> = {
  updated: 'license.sync.outcome.updated',
  unchanged: 'license.sync.outcome.unchanged',
  revoked: 'license.sync.outcome.revoked',
  pending_rebind: 'license.sync.outcome.pendingRebind',
  no_license: 'license.sync.outcome.noLicense',
  unreadable_license_file: 'license.sync.outcome.unreadableLicenseFile',
  offline: 'license.sync.outcome.offline',
  failed: 'license.sync.outcome.failed',
  busy: 'license.sync.outcome.busy',
};

const SYNC_OUTCOME_TEXT: Record<SyncOutcome, string> = {
  updated: 'text-success',
  unchanged: 'text-success',
  revoked: 'text-destructive',
  pending_rebind: 'text-warning',
  no_license: 'text-muted-foreground',
  unreadable_license_file: 'text-warning',
  offline: 'text-warning',
  failed: 'text-warning',
  busy: 'text-muted-foreground',
};

/**
 * The AI employee cap row: the enforced number, 「不限」 only when the gateway
 * says it is uncapped, nothing when the gateway did not say (older gateway
 * or a failed lookup) — never a made-up number.
 */
export function seatLimitLabel(
  snapshot: Pick<LicenseSnapshot, 'seat_limit' | 'seat_unlimited'>,
): { kind: 'number'; value: number } | { kind: 'unlimited' } | null {
  if (typeof snapshot.seat_limit === 'number') return { kind: 'number', value: snapshot.seat_limit };
  if (snapshot.seat_limit === null && snapshot.seat_unlimited === true) return { kind: 'unlimited' };
  return null;
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
  const isAdmin = useAuthStore(s => s.user?.role === 'admin');
  useCompanyTimeZone();
  const intl = useIntl();
  const contact = useCommercialContact();
  const connectionState = useConnectionStore((s) => s.state);
  const [snapshot, setSnapshot] = useState<LicenseSnapshot | null>(null);
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);
  // P05: with `snapshot` null and `loading` back to false, both render branches
  // below are false — the page went blank under its own header and the only
  // clue was a toast that had already disappeared.
  const [loadError, setLoadError] = useState<unknown>(null);
  // Admin "重新整理" really syncs with the vendor (license.sync).
  const [syncResult, setSyncResult] = useState<LicenseSyncResult['sync'] | null>(null);
  const [restartRecommended, setRestartRecommended] = useState(false);

  const load = useMemo(
    () => async () => {
      setLoadError(null);
      try {
        const result = await api.license.status();
        setSnapshot(result);
      } catch (e) {
        setLoadError(e);
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

  const sync = async () => {
    setRefreshing(true);
    setSyncResult(null);
    try {
      const result = await api.license.sync();
      setLoadError(null);
      setSnapshot(result);
      setSyncResult(result.sync ?? null);
      setRestartRecommended(result.restart_recommended === true);
    } catch (e) {
      // The licence on screen is still the last one read; only the sync failed.
      toast.error(formatError(e));
    } finally {
      setRefreshing(false);
    }
  };

  const expiryClassification = classifyExpiry(snapshot?.days_until_expiry);
  const seatLimit = snapshot ? seatLimitLabel(snapshot) : null;

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
            if (isAdmin) {
              void sync();
              return;
            }
            setRefreshing(true);
            void load();
          }}
          disabled={refreshing || loading}
        >
          <RefreshCw className={cn(refreshing && 'animate-spin')} />
          {intl.formatMessage({ id: 'license.refresh' })}
        </Button>
      </div>

      {syncResult && SYNC_OUTCOME_MESSAGE[syncResult.outcome] && (
        <p role="status" className={cn('text-sm', SYNC_OUTCOME_TEXT[syncResult.outcome])}>
          {intl.formatMessage(
            { id: SYNC_OUTCOME_MESSAGE[syncResult.outcome] },
            { seconds: syncResult.retry_after_secs ?? 30 },
          )}
        </p>
      )}

      {restartRecommended && (
        <div
          role="status"
          className="rounded-xl border border-amber-400/50 bg-amber-50 px-4 py-3 text-sm text-amber-900 dark:border-amber-500/40 dark:bg-amber-500/10 dark:text-amber-200"
        >
          {intl.formatMessage({ id: 'license.sync.restartRecommended' })}
        </div>
      )}

      {loading && !snapshot && (
        <Card>
          <CardContent>
            <p className="py-8 text-center text-sm text-muted-foreground">
              {intl.formatMessage({ id: 'license.loading' })}
            </p>
          </CardContent>
        </Card>
      )}

      {!loading && loadError != null && !snapshot && (
        <Card>
          <CardContent>
            <ErrorState
              error={loadError}
              title={intl.formatMessage({ id: 'errorState.manage.loadFailed' })}
              onRetry={() => {
                setLoading(true);
                void load();
              }}
            />
          </CardContent>
        </Card>
      )}

      {/* A refresh that failed on top of an existing snapshot: keep the data,
          but say the numbers on screen are the previous read. */}
      {loadError != null && snapshot && (
        <ErrorState
          variant="inline"
          error={loadError}
          title={intl.formatMessage({ id: 'errorState.manage.loadFailed' })}
          onRetry={() => {
            setRefreshing(true);
            void load();
          }}
        />
      )}

      {snapshot && (
        <>
          <LicenseValidationWarning snapshot={snapshot} />
          {/* ── NFR watermark: signed into the license, never hidden by
              the permanent anti-resale marker ────── */}
          {snapshot.nfr && (
            <div
              role="status"
              className="flex items-center gap-3 rounded-xl border border-amber-400/50 bg-amber-50 px-4 py-3 text-amber-900 dark:border-amber-500/40 dark:bg-amber-500/10 dark:text-amber-200"
            >
              <FlaskConical className="size-5 shrink-0" aria-hidden="true" />
              <div>
                <p className="text-sm font-semibold">
                  {intl.formatMessage({ id: 'license.nfr.title' })}
                </p>
                <p className="text-xs opacity-80">
                  {intl.formatMessage({ id: 'license.nfr.subtitle' })}
                </p>
              </div>
            </div>
          )}

          {/* ── KPI status: tier / expiry / phone-home ────── */}
          <div className="grid grid-cols-1 gap-4 sm:grid-cols-3">
            <StatTile
              label={intl.formatMessage({ id: 'license.activeTier' })}
              value={TIER_LABELS[snapshot.tier]}
              sub={
                snapshot.mode === 'commercial'
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
                snapshot.expires_at ? formatDateTime(snapshot.expires_at, { year: 'numeric', month: 'numeric', day: 'numeric', hour: 'numeric', minute: 'numeric', second: 'numeric' }) : '—'
              }
            />
            <StatTile
              label={intl.formatMessage({ id: 'license.lastPhoneHome' })}
              value={<PhoneHomeIndicator daysSincePhoneHome={snapshot.days_since_phone_home} />}
              sub={
                snapshot.last_phone_home
                  ? formatDateTime(snapshot.last_phone_home, { year: 'numeric', month: 'numeric', day: 'numeric', hour: 'numeric', minute: 'numeric', second: 'numeric' })
                  : '—'
              }
            />
          </div>

          {/* ── Activate — prominent while on OpenSource ── */}
          {isAdmin && snapshot.mode === 'opensource' && (
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
                {snapshot.expires_at ? formatDateTime(snapshot.expires_at, { year: 'numeric', month: 'numeric', day: 'numeric', hour: 'numeric', minute: 'numeric', second: 'numeric' }) : '—'}
              </span>
            </SettingsRow>
            <SettingsRow label={intl.formatMessage({ id: 'license.lastPhoneHome' })}>
              <span className="font-mono text-xs">
                {snapshot.last_phone_home ? formatDateTime(snapshot.last_phone_home, { year: 'numeric', month: 'numeric', day: 'numeric', hour: 'numeric', minute: 'numeric', second: 'numeric' }) : '—'}
              </span>
            </SettingsRow>
            {seatLimit && (
              <SettingsRow label={intl.formatMessage({ id: 'license.seats.limit' })}>
                <span className="text-sm" data-testid="license-seat-limit">
                  {seatLimit.kind === 'number'
                    ? intl.formatNumber(seatLimit.value)
                    : intl.formatMessage({ id: 'license.seats.unlimited' })}
                </span>
              </SettingsRow>
            )}
            {typeof snapshot.seats_used === 'number' && (
              <SettingsRow label={intl.formatMessage({ id: 'license.seats.used' })}>
                <span className="text-sm" data-testid="license-seats-used">
                  {intl.formatNumber(snapshot.seats_used)}
                </span>
              </SettingsRow>
            )}
            {typeof snapshot.avatars_exempt === 'number' && (
              <SettingsRow label={intl.formatMessage({ id: 'agents.seats.avatarExempt' })}>
                <span className="text-sm" data-testid="license-avatars-exempt">{intl.formatNumber(snapshot.avatars_exempt)}</span>
              </SettingsRow>
            )}
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
            {snapshot.fingerprint_strength === 'instance' && (
              <SettingsRow label={intl.formatMessage({ id: 'license.fingerprint' })}>
                <span className="text-sm">{intl.formatMessage({ id: 'license.identity.instance' })}</span>
              </SettingsRow>
            )}
          </SettingsCard>

          <Card>
            <CardContent>
              <p className="text-sm text-muted-foreground">{intl.formatMessage({ id: 'license.distributorNotice' }, {distributor: contact.name})}</p>
              <a href={contact.url} target="_blank" rel="noopener noreferrer" className="mt-2 inline-block text-sm text-brand hover:underline">
                {intl.formatMessage({id: 'license.distributorContact'})}
              </a>
            </CardContent>
          </Card>

          {snapshot.installed && snapshot.days_until_expiry != null && expiryClassification.tone !== 'ok' && (
            <Card>
              <CardHeader>
                <CardTitle>{intl.formatMessage({ id: 'license.cta.renew.title' })}</CardTitle>
                <CardDescription>{intl.formatMessage({ id: 'license.cta.renew.body' }, { distributor: contact.name })}</CardDescription>
              </CardHeader>
              <CardContent>
                <div className="flex flex-wrap gap-3">
                  <a href={contact.url} target="_blank" rel="noopener noreferrer">
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
          {isAdmin && snapshot.mode === 'commercial' && (
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

            </CardContent>
          </Card>
        </>
      )}
    </div>
  );
}

/**
 * Activate license card — machine fingerprint (copyable), license
 * key activation. Prominent while on
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
  const contact = useCommercialContact();
  const navigate = useNavigate();
  // Zero agents ⇒ the operator is still mid-onboarding (came here from the
  // welcome wizard's industry step) — offer a way back after activating.
  const agentCount = useAgentsStore((s) => s.agents.length);

  const [fingerprint, setFingerprint] = useState<string | null>(null);
  const [key, setKey] = useState('');
  const [activating, setActivating] = useState(false);
  const [activateError, setActivateError] = useState<string | null>(null);
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

  const body = (
    <div className="space-y-6">
      {/* 1 — machine fingerprint + general distributor contact */}
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
            href={contact.url}
            target="_blank"
            rel="noopener noreferrer"
            className="font-medium text-brand hover:underline"
          >
            {intl.formatMessage({ id: 'license.distributorContact' })}
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
            aria-label={intl.formatMessage({ id: 'license.activate.key.label' })}
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
