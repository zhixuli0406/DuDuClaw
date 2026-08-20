import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { ShieldQuestion, Check, X, Compass } from 'lucide-react';
import { Badge, Button, Spinner } from '@/components/mds';
import { api, type ApprovalKind } from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import { timeRemaining } from '@/lib/format';
import { cn } from '@/lib/utils';
import type { ApprovalRequestArtifact } from './artifact-types';
import { ArtifactShell } from './ArtifactShell';

/** Same fallback-to-"其他操作" lookup `ApprovalsPage` uses for an unknown
 *  backend `kind` — duplicated here (not imported) because `ApprovalsPage`
 *  is itself a legacy bookmark alias slated for deletion (see its own doc
 *  comment); this card should not depend on a page on its way out. */
function kindLabel(intl: ReturnType<typeof useIntl>, kind: ApprovalKind): string {
  const id = `approvals.kind.${kind}`;
  const fallback = intl.formatMessage({ id: 'approvals.kind.unknown' });
  const msg = intl.formatMessage({ id, defaultMessage: fallback });
  return msg === id ? fallback : msg;
}

type CardStatus = 'idle' | 'busy' | 'approved' | 'denied';

/**
 * 不可逆審批卡 — the inline twin of `ApprovalsPage`'s per-item card (and the
 * `ApprovalModal` browser-action popup), for an `ApprovalItem` an agent
 * reply attaches directly to the conversation instead of making the user go
 * to `/inbox` to decide it. Approve/reject call the SAME `approvals.decide`
 * RPC — ApprovalBroker's TTL-expiry-is-DENY fail-closed semantics (coding
 * convention #4) are entirely server-side and untouched by this card; it
 * only adds a front door in the chat flow, same as O-0/O-1's whole premise.
 */
export function ApprovalRequestCard({ payload }: { payload: ApprovalRequestArtifact['payload'] }) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);
  const [status, setStatus] = useState<CardStatus>('idle');
  const [error, setError] = useState<string | null>(null);
  const [now, setNow] = useState(() => Date.now());

  // Live TTL tick, same idea as `ApprovalModal`'s countdown — only while a
  // decision is still pending and the server gave us a real deadline.
  useEffect(() => {
    if (status !== 'idle' || payload.expires_at == null) return;
    const id = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(id);
  }, [status, payload.expires_at]);

  const decide = async (approve: boolean) => {
    setStatus('busy');
    setError(null);
    try {
      await api.approvals.decide(payload.id, approve);
      toast.success(
        t(approve ? 'approvals.approvedToast' : 'approvals.deniedToast', { summary: payload.summary }),
      );
      setStatus(approve ? 'approved' : 'denied');
    } catch (e) {
      console.warn('[console.artifact.approval_request]', payload.id, e);
      const message = formatError(e);
      toast.error(message);
      setError(message);
      setStatus('idle'); // allow an immediate retry
    }
  };

  if (status === 'approved' || status === 'denied') {
    const approved = status === 'approved';
    return (
      <ArtifactShell icon={approved ? Check : X} title={t('console.artifact.approvalRequest.title')}>
        <p className={cn('flex items-center gap-2 text-sm', approved ? 'text-success' : 'text-muted-foreground')}>
          {approved ? <Check className="size-4 shrink-0" aria-hidden="true" /> : <X className="size-4 shrink-0" aria-hidden="true" />}
          {t(approved ? 'approvals.approvedToast' : 'approvals.deniedToast', { summary: payload.summary })}
        </p>
      </ArtifactShell>
    );
  }

  const remaining = payload.expires_at != null ? Math.max(0, payload.expires_at * 1000 - now) : null;

  return (
    <ArtifactShell
      icon={ShieldQuestion}
      title={t('console.artifact.approvalRequest.title')}
      advancedTo="/inbox"
      advancedLabel={t('approvals.legacyBanner.cta')}
    >
      <div className="space-y-2.5">
        <div className="flex flex-wrap items-center gap-2">
          <Badge variant="secondary">{kindLabel(intl, payload.kind)}</Badge>
          <span className="text-xs text-muted-foreground">
            {t('approvals.requester')}: {payload.agent_id}
          </span>
        </div>
        <p className="break-words text-sm text-foreground">{payload.summary}</p>
        {payload.simulation && payload.simulation.world_state_change.trim() !== '' && (
          <p className="flex items-start gap-1.5 text-xs text-muted-foreground">
            <Compass className="mt-0.5 size-3 shrink-0" aria-hidden="true" />
            <span className="break-words">{payload.simulation.world_state_change}</span>
          </p>
        )}
        {remaining != null && (
          <p className="text-xs text-muted-foreground">
            {t('console.artifact.approvalRequest.expiresIn', { time: timeRemaining(remaining) })}
          </p>
        )}

        {error && (
          <p role="alert" className="rounded-lg border border-destructive/30 bg-destructive/10 px-2.5 py-1.5 text-xs text-destructive">
            {error}
          </p>
        )}

        <div className="flex gap-2">
          <Button variant="brand" size="sm" disabled={status === 'busy'} onClick={() => void decide(true)}>
            {status === 'busy' ? <Spinner className="size-3.5" /> : <Check />}
            {t('approvals.approve')}
          </Button>
          <Button variant="destructive" size="sm" disabled={status === 'busy'} onClick={() => void decide(false)}>
            <X />
            {t('approvals.deny')}
          </Button>
        </div>
      </div>
    </ArtifactShell>
  );
}
