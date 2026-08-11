import { useCallback, useState } from 'react';
import { useIntl } from 'react-intl';
import {
  Puzzle,
  Plug,
  Check,
  X,
  Clock,
  ShieldCheck,
  ShieldAlert,
  AlertTriangle,
  User,
  Loader2,
  Hourglass,
} from 'lucide-react';
import { api, type InstallRequestInfo } from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import { cn } from '@/lib/utils';
import { Badge, Button } from '@/components/mds';
import { timeAgo, timeRemaining } from '@/lib/format';
import { expiryState } from '@/lib/inbox-model';
import { useNowTick } from '@/hooks/useNowTick';
import { OpenInChannelButton } from './OpenInChannelButton';

const SEVERITY_COLOR: Record<string, string> = {
  critical: 'text-destructive',
  error: 'text-orange-500',
  high: 'text-orange-500',
  warning: 'text-warning',
  medium: 'text-warning',
  info: 'text-muted-foreground',
  low: 'text-muted-foreground',
};

type RiskBadge = { variant: 'secondary' | 'destructive'; className?: string };

const RISK_BADGE: Record<string, RiskBadge> = {
  Clean: { variant: 'secondary', className: 'text-success' },
  Low: { variant: 'secondary', className: 'text-success' },
  Medium: { variant: 'secondary', className: 'text-warning' },
  High: { variant: 'destructive' },
  Critical: { variant: 'destructive' },
};

/**
 * InstallDetailPanel — the Inbox detail-pane body for `install`-type items
 * (04 doc §E17 / §A.3 D-O2). This is the same skill/MCP two-stage signature
 * chain that previously only lived on the legacy `/approvals` page
 * (`InstallRequestsSection`) — moved in-place so the Inbox no longer bounces
 * to a different route just to show an install request's detail
 * (`InboxPage.tsx`'s former `navigate('/approvals')` stub, §E17).
 *
 * Vocabulary follows §C.3: the stage badge renders "等主管同意"/"等管理員同意"
 * (i18n keys `install.request.stage.*`, updated to match), never the raw
 * `awaiting_manager`/`awaiting_admin` token.
 */
export function InstallDetailPanel({
  request,
  onDecided,
}: {
  request: InstallRequestInfo;
  /** Called after a decision RPC succeeds so the parent can mark the row
   *  processed (§C6) without a full list refetch. */
  onDecided: () => void;
}) {
  const intl = useIntl();
  const t = useCallback((id: string) => intl.formatMessage({ id }), [intl]);
  const [busy, setBusy] = useState(false);
  const [decided, setDecided] = useState<'approved' | 'denied' | null>(null);

  const KindIcon = request.kind === 'skill' ? Puzzle : Plug;
  const stageLabel = intl.formatMessage({
    id: `install.request.stage.${request.stage}`,
    defaultMessage: request.stage,
  });
  const riskBadge = RISK_BADGE[request.risk_level] ?? { variant: 'secondary' as const };
  const passed = request.risk_level === 'Clean' || request.risk_level === 'Low' || request.risk_level === 'Medium';

  // TTL countdown — same pattern as the approval detail panel (W0-9); an
  // install request that has no `expires_at` (older cached response) simply
  // tracks nothing.
  const tracksExpiry = request.expires_at != null;
  const now = useNowTick(tracksExpiry);
  const expiry = tracksExpiry
    ? expiryState({ timestamp: request.created_at, expiresAt: request.expires_at }, now)
    : null;

  const decide = useCallback(
    async (approve: boolean) => {
      if (busy) return;
      setBusy(true);
      try {
        const res = await api.installRequests.decide(request.id, approve);
        if (res.warning) {
          toast.error(res.warning);
        } else if (res.status === 'approved') {
          toast.success(intl.formatMessage({ id: 'install.request.approvedInstalled' }, { title: request.title }));
        } else if (res.status === 'pending') {
          toast.success(intl.formatMessage({ id: 'install.request.advancedToAdmin' }, { title: request.title }));
        } else {
          toast.success(intl.formatMessage({ id: 'install.request.deniedToast' }, { title: request.title }));
        }
        setDecided(approve ? 'approved' : 'denied');
        onDecided();
      } catch (e) {
        toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
      } finally {
        setBusy(false);
      }
    },
    [busy, intl, request, onDecided],
  );

  return (
    <div className="space-y-4">
      <div className="flex items-start gap-3">
        <span className="grid size-9 shrink-0 place-items-center rounded-lg bg-brand/10 text-brand">
          <KindIcon className="size-[1.125rem]" />
        </span>
        <div className="min-w-0 flex-1 space-y-1">
          <div className="flex flex-wrap items-center gap-2">
            <Badge>{t(`install.request.kind.${request.kind}`)}</Badge>
            <Badge variant="secondary">{stageLabel}</Badge>
          </div>
          <h2 className="text-lg font-medium text-foreground">{request.title}</h2>
          <p className="flex flex-wrap items-center gap-1 text-xs text-muted-foreground">
            <User className="size-3 shrink-0" />
            <span className="truncate">{request.requester_email || request.requester_id}</span>
            {' · '}
            {t(`install.request.role.${request.requester_role}`)}
            {request.requester_department && (
              <span>
                {' · '}
                {t('install.request.dept')}
                {request.requester_department}
              </span>
            )}
          </p>
        </div>
      </div>

      {request.description && <p className="text-sm text-foreground">{request.description}</p>}

      {/* Near-expiry warning — mirrors the approval TTL banner (W0-9); don't
          regress the countdown that was just added there. */}
      {expiry?.nearExpiry && (
        <p className="flex items-start gap-2 rounded-lg border border-warning/30 bg-warning/10 px-3 py-2 text-xs text-foreground">
          <Hourglass className="mt-0.5 size-3.5 shrink-0 text-warning" aria-hidden="true" />
          {intl.formatMessage({ id: 'inbox.approval.nearExpiryBanner' }, { time: timeRemaining(expiry.remainingMs) })}
        </p>
      )}

      {/* Security scan verdict */}
      <div className="space-y-2 rounded-lg bg-muted p-3">
        <div className="flex flex-wrap items-center gap-2 text-xs">
          {passed ? (
            <ShieldCheck className="size-3.5 shrink-0 text-success" />
          ) : (
            <ShieldAlert className="size-3.5 shrink-0 text-destructive" />
          )}
          <span className="text-muted-foreground">{t('install.request.riskLabel')}:</span>
          <Badge variant={riskBadge.variant} className={riskBadge.className}>
            {request.risk_level}
          </Badge>
          <span className="text-muted-foreground">
            {intl.formatMessage({ id: 'install.request.findingsCount' }, { count: request.scan?.length ?? 0 })}
          </span>
        </div>
        {request.scan && request.scan.length > 0 && (
          <ul className="space-y-1">
            {request.scan.slice(0, 5).map((f, i) => (
              <li key={i} className="flex items-start gap-1.5 text-xs">
                <AlertTriangle
                  className={cn('mt-0.5 size-3 shrink-0', SEVERITY_COLOR[f.severity] ?? SEVERITY_COLOR.info)}
                />
                <span className="text-muted-foreground">
                  <span className={cn('font-semibold uppercase', SEVERITY_COLOR[f.severity] ?? SEVERITY_COLOR.info)}>
                    {f.severity}
                  </span>
                  {' · '}
                  {f.description}
                </span>
              </li>
            ))}
          </ul>
        )}
        <div className="flex items-center gap-1 text-[11px] text-muted-foreground">
          <Clock className="size-3 shrink-0" />
          {timeAgo(request.created_at)}
          {request.manager_by && <span className="ml-2">{t('install.request.managerSigned')}</span>}
        </div>
      </div>

      {/* E8 reverse handoff: jump to the sign-off conversation this
          request's decision card is (or was) parked in on the resolved
          channel. Renders nothing when the backend couldn't resolve a link. */}
      <OpenInChannelButton channel={request.channel} link={request.channel_link} variant="outline" />

      {/* Decision */}
      {decided ? (
        <p
          className={cn(
            'rounded-lg border p-3 text-sm',
            decided === 'approved'
              ? 'border-success/40 bg-success/10 text-success'
              : 'border-destructive/40 bg-destructive/10 text-destructive',
          )}
        >
          {decided === 'approved'
            ? intl.formatMessage({ id: 'install.request.approvedInstalled' }, { title: request.title })
            : intl.formatMessage({ id: 'install.request.deniedToast' }, { title: request.title })}
        </p>
      ) : (
        <div className="flex items-center gap-2">
          <Button variant="brand" disabled={busy} onClick={() => decide(true)} className="flex-1">
            {busy ? <Loader2 className="animate-spin" /> : <Check />}
            {t('install.request.approve')}
          </Button>
          <Button variant="destructive" disabled={busy} onClick={() => decide(false)} className="flex-1">
            <X />
            {t('install.request.deny')}
          </Button>
        </div>
      )}
    </div>
  );
}
