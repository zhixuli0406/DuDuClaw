import { useIntl } from 'react-intl';
import { AlertTriangle, ShieldCheck } from 'lucide-react';
import { useAuthStore } from '@/stores/auth-store';
import { useSystemStore } from '@/stores/system-store';

/**
 * RM-166: AI-employee identity verification transition. Shown to admins while
 * the transition period runs (days left until strict mode), when the setting
 * cannot be read, or when the file guard refused AI employees for an
 * unverified identity in the last 24 hours. `system.status.identity_token`.
 */
export function IdentityTransitionBanner() {
  const intl = useIntl();
  const admin = useAuthStore((s) => s.user?.role === 'admin');
  const identity = useSystemStore((s) => s.status?.identity_token);
  if (!admin || !identity) return null;

  const grace = identity.mode === 'grace';
  const broken = identity.mode === 'error';
  const refusals = identity.untrusted_refusals_24h ?? 0;
  if (!grace && !broken && refusals <= 0) return null;

  const warn = broken || refusals > 0;
  const Icon = warn ? AlertTriangle : ShieldCheck;

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
        <div className="min-w-0 flex-1 space-y-1">
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
          {refusals > 0 && (
            <p className={grace || broken ? 'text-muted-foreground' : 'font-medium'}>
              {intl.formatMessage({ id: 'identityTransition.refusals' }, { count: refusals })}
            </p>
          )}
        </div>
      </div>
    </div>
  );
}
