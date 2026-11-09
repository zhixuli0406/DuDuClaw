import { useState } from 'react';
import { useIntl } from 'react-intl';
import { AlertTriangle, ShieldCheck } from 'lucide-react';
import { Button, Input } from '@/components/mds';
import { api, type IdentityGraceResult } from '@/lib/api';
import { useAuthStore } from '@/stores/auth-store';
import { useSystemStore } from '@/stores/system-store';

const DEFAULT_MAX_GRACE_DAYS = 30;

/**
 * RM-166: AI-employee identity verification. Admin-only (the gateway sends
 * `system.status.identity_token` to admins only).
 *
 * Round 2 (owner decision 2026-11-09): the upgrade transition is a NOTICE —
 * strict mode is never weakened automatically.
 * - For 30 days after this release first started on an existing install
 *   (`notice_active`): "identity verification is strict" + refusals in the
 *   last 24 h. With refusals, an Admin may turn on a manual grace (at most
 *   30 days, current password required, audited) after reading what it
 *   weakens and when it ends.
 * - While a grace runs: days left + "end grace now".
 * - The setting cannot be read: explained.
 * - Outside the notice, refusals in the last 24 h still warn.
 */
export function IdentityTransitionBanner() {
  const intl = useIntl();
  const admin = useAuthStore((s) => s.user?.role === 'admin');
  const identity = useSystemStore((s) => s.status?.identity_token);
  const [confirming, setConfirming] = useState(false);
  const [password, setPassword] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [restartRequired, setRestartRequired] = useState(false);
  if (!admin || !identity) return null;

  const grace = identity.mode === 'grace';
  const broken = identity.mode === 'error';
  const strict = identity.mode === 'strict';
  const notice = strict && identity.notice_active === true;
  const refusals = identity.untrusted_refusals_24h ?? 0;
  if (!grace && !broken && !notice && refusals <= 0) return null;

  const maxDays = identity.manual_grace_max_days ?? DEFAULT_MAX_GRACE_DAYS;
  const offerGrace = notice && refusals > 0;
  const warn = broken || refusals > 0;
  const Icon = warn ? AlertTriangle : ShieldCheck;

  const apply = (result: IdentityGraceResult) => {
    useSystemStore.setState((s) => ({
      status: s.status ? { ...s.status, identity_token: result.identity_token } : s.status,
    }));
  };

  const fail = (e: unknown) => {
    const detail = e instanceof Error ? e.message : String(e);
    setError(intl.formatMessage({ id: 'identityTransition.actionFailed' }, { error: detail }));
  };

  const startGrace = async () => {
    setBusy(true);
    setError(null);
    try {
      const result = await api.system.identityGraceStart(password, maxDays);
      apply(result);
      setRestartRequired(result.restart_required === true);
      setConfirming(false);
      setPassword('');
    } catch (e) {
      fail(e);
    } finally {
      setBusy(false);
    }
  };

  const endGrace = async () => {
    setBusy(true);
    setError(null);
    try {
      apply(await api.system.identityGraceEnd());
      setRestartRequired(false);
    } catch (e) {
      fail(e);
    } finally {
      setBusy(false);
    }
  };

  const until = intl.formatDate(new Date(Date.now() + maxDays * 86_400_000), {
    year: 'numeric',
    month: 'short',
    day: 'numeric',
  });

  return (
    <div
      role="status"
      className={
        warn
          ? 'space-y-1 rounded-lg border border-warning/30 bg-warning/10 px-4 py-3 text-sm'
          : 'space-y-1 rounded-lg border border-surface-border bg-muted/40 px-4 py-3 text-sm'
      }
    >
      <div className="flex items-start gap-3">
        <Icon className={warn ? 'mt-0.5 size-4 shrink-0 text-warning' : 'mt-0.5 size-4 shrink-0 text-muted-foreground'} />
        <div className="min-w-0 flex-1 space-y-2">
          {grace && (
            <p className="font-medium">
              {intl.formatMessage(
                { id: 'identityTransition.grace' },
                { days: Math.max(0, identity.days_left ?? 0) },
              )}
            </p>
          )}
          {broken && (
            <p className="font-medium">{intl.formatMessage({ id: 'identityTransition.error' })}</p>
          )}
          {notice && (
            <>
              <p className="font-medium">{intl.formatMessage({ id: 'identityTransition.strictNotice' })}</p>
              <p className="text-muted-foreground">
                {intl.formatMessage({ id: 'identityTransition.strictNoticeDetail' }, { count: refusals })}
              </p>
            </>
          )}
          {!notice && refusals > 0 && (
            <p className={grace || broken ? 'text-muted-foreground' : 'font-medium'}>
              {intl.formatMessage({ id: 'identityTransition.refusals' }, { count: refusals })}
            </p>
          )}
          {restartRequired && (
            <p className="text-muted-foreground">{intl.formatMessage({ id: 'identityTransition.restartRequired' })}</p>
          )}

          {offerGrace && !confirming && (
            <Button variant="outline" size="sm" onClick={() => setConfirming(true)} disabled={busy}>
              {intl.formatMessage({ id: 'identityTransition.offerGrace' }, { days: maxDays })}
            </Button>
          )}
          {offerGrace && confirming && (
            <div className="space-y-2 rounded-md border border-warning/30 bg-background/60 p-3">
              <p>{intl.formatMessage({ id: 'identityTransition.graceExplain' }, { days: maxDays, until })}</p>
              <div className="space-y-1.5">
                <label className="text-xs font-medium text-muted-foreground" htmlFor="identity-grace-password">
                  {intl.formatMessage({ id: 'identityTransition.passwordLabel' })}
                </label>
                <Input
                  id="identity-grace-password"
                  type="password"
                  value={password}
                  onChange={(e) => setPassword(e.target.value)}
                  autoComplete="current-password"
                />
              </div>
              <div className="flex flex-wrap gap-2">
                <Button size="sm" onClick={() => void startGrace()} disabled={busy || password.length === 0}>
                  {intl.formatMessage({ id: 'identityTransition.confirmGrace' }, { days: maxDays })}
                </Button>
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => {
                    setConfirming(false);
                    setPassword('');
                    setError(null);
                  }}
                  disabled={busy}
                >
                  {intl.formatMessage({ id: 'identityTransition.cancel' })}
                </Button>
              </div>
            </div>
          )}
          {grace && (
            <Button variant="outline" size="sm" onClick={() => void endGrace()} disabled={busy}>
              {intl.formatMessage({ id: 'identityTransition.endGrace' })}
            </Button>
          )}
          {error && (
            <p role="alert" className="text-danger">
              {error}
            </p>
          )}
        </div>
      </div>
    </div>
  );
}
