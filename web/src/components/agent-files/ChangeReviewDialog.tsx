import { useEffect, useId, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { AlertTriangle, CheckCircle2, Clock, Info, ShieldAlert, XCircle } from 'lucide-react';
import { cn } from '@/lib/utils';
import {
  Button,
  Checkbox,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  Spinner,
  Textarea,
} from '@/components/mds';
import type { AgentFileReason, AgentFileSaveResponse } from '@/lib/agent-files-types';
import { analysisUnavailable, reasonCategoryText, shortApprovalId } from './review-text';

/** Minimum length of the "why" a person writes before confirming or asking. */
export const MIN_REASON_CHARS = 4;

/** What the dialog shows: the check is still running, or the server's answer. */
export type ChangeReviewState =
  | { kind: 'checking' }
  | { kind: 'response'; response: AgentFileSaveResponse };

export interface ChangeReviewDialogProps {
  /** `null` closes the dialog. */
  state: ChangeReviewState | null;
  isAdmin: boolean;
  /** A confirm / request call is in flight (buttons disabled). */
  busy?: boolean;
  /** The last confirm / request call failed (shown as a short line). */
  error?: string | null;
  onConfirm: (reason: string, acknowledgeRisk: boolean) => void;
  onRequestApproval: (reason: string) => void;
  onClose: () => void;
  onReload: () => void;
  /** Fired once per `applied` answer so the caller can toast; the dialog
   *  stays open only when the automatic check did not finish. */
  onApplied?: (response: AgentFileSaveResponse) => void;
}

type Tone = 'neutral' | 'warning' | 'danger' | 'success';

const TONE_PANEL: Record<Tone, string> = {
  neutral: 'border-surface-border bg-muted',
  warning: 'border-warning/30 bg-warning/10',
  danger: 'border-destructive/30 bg-destructive/5',
  success: 'border-success/30 bg-success/10',
};

function ReasonList({ reasons }: { reasons: AgentFileReason[] }) {
  const intl = useIntl();
  if (reasons.length === 0) return null;
  return (
    <ul className="space-y-2" data-testid="review-reasons">
      {reasons.map((r, i) => (
        <li key={`${r.category}-${i}`} className="space-y-1">
          <p className="text-sm text-foreground">{reasonCategoryText(intl, r.category, r.message)}</p>
          {r.evidence.trim() !== '' && (
            <blockquote className="border-l-2 border-surface-border pl-2 text-xs text-muted-foreground">
              <span className="sr-only">{intl.formatMessage({ id: 'agentFiles.review.evidence' })}: </span>
              <span className="whitespace-pre-wrap break-words">{r.evidence}</span>
            </blockquote>
          )}
        </li>
      ))}
    </ul>
  );
}

function UnavailableNotice() {
  const intl = useIntl();
  return (
    <p
      role="note"
      className="flex items-start gap-2 rounded-lg border border-warning/30 bg-warning/10 px-3 py-2 text-sm text-foreground"
    >
      <AlertTriangle className="mt-0.5 size-4 shrink-0 text-warning" aria-hidden="true" />
      <span>{intl.formatMessage({ id: 'agentFiles.review.analysisUnavailable' })}</span>
    </p>
  );
}

/**
 * ChangeReviewDialog — what a person sees after saving an AI employee's
 * profile file (or its boundaries) when the save did not simply go through.
 * Driven by the shared save envelope: warn asks for a written reason, block
 * lets only an admin through (reason + "I understand the risk") and offers
 * everyone else an admin review, rejected lists what to fix, conflict asks
 * for a reload, pending shows the request number. An unfinished automatic
 * check is always said out loud.
 */
export function ChangeReviewDialog({
  state,
  isAdmin,
  busy = false,
  error = null,
  onConfirm,
  onRequestApproval,
  onClose,
  onReload,
  onApplied,
}: ChangeReviewDialogProps) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);
  const reasonId = useId();
  const [reason, setReason] = useState('');
  const [ack, setAck] = useState(false);

  const response = state?.kind === 'response' ? state.response : null;
  const status = response?.status ?? null;
  const review = response?.review;
  const unavailable = analysisUnavailable(review);

  // A new answer starts with an empty reason and an unticked box.
  useEffect(() => {
    setReason('');
    setAck(false);
  }, [response]);

  // Tell the caller about an applied answer exactly once.
  const appliedFor = useRef<AgentFileSaveResponse | null>(null);
  useEffect(() => {
    if (response && response.status === 'applied' && appliedFor.current !== response) {
      appliedFor.current = response;
      onApplied?.(response);
    }
  }, [response, onApplied]);

  const open =
    state !== null &&
    !(status === 'unchanged') &&
    !(status === 'applied' && !unavailable);

  const reasonOk = reason.trim().length >= MIN_REASON_CHARS;
  const isWarn = status === 'needs_confirmation';
  const isBlock = status === 'blocked';
  // Server flags win when present; otherwise fall back to the role rules.
  const canConfirm = isWarn
    ? review?.can_confirm ?? true
    : isBlock
      ? isAdmin && (review?.can_confirm ?? true)
      : false;
  const canRequest = (isWarn || isBlock) && !isAdmin && (review?.can_request_approval ?? true);
  // A block, or a review that did not cover the whole change, needs the
  // admin to acknowledge the risk (the gateway refuses without it).
  const needsAck = isAdmin && (isBlock || (isWarn && unavailable));

  let tone: Tone = 'neutral';
  let Icon = Info;
  let titleId = 'agentFiles.review.title.checking';
  if (state?.kind === 'checking') {
    titleId = 'agentFiles.review.title.checking';
  } else if (isWarn) {
    tone = 'warning';
    Icon = AlertTriangle;
    titleId = 'agentFiles.review.title.needsConfirmation';
  } else if (isBlock) {
    tone = 'danger';
    Icon = ShieldAlert;
    titleId = 'agentFiles.review.title.blocked';
  } else if (status === 'rejected') {
    tone = 'danger';
    Icon = XCircle;
    titleId = 'agentFiles.review.title.rejected';
  } else if (status === 'conflict') {
    tone = 'warning';
    Icon = AlertTriangle;
    titleId = 'agentFiles.review.title.conflict';
  } else if (status === 'pending_approval') {
    tone = 'neutral';
    Icon = Clock;
    titleId = 'agentFiles.review.title.pending';
  } else if (status === 'applied') {
    tone = 'success';
    Icon = CheckCircle2;
    titleId = 'agentFiles.review.applied';
  }

  const handleOpenChange = (next: boolean) => {
    // The check cannot be abandoned half-way; everything else may close.
    if (!next && state?.kind !== 'checking' && !busy) onClose();
  };

  // The unfinished-check reason has its own notice above the panel.
  const reasons = (review?.reasons ?? []).filter((r) => r.category !== 'analysis_unavailable');
  const summary = review?.summary?.trim() ?? '';

  return (
    <Dialog open={open} onOpenChange={handleOpenChange}>
      <DialogContent className="sm:max-w-lg" showClose={false} data-testid="change-review-dialog">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            {state?.kind !== 'checking' && (
              <Icon
                className={cn(
                  'size-4 shrink-0',
                  tone === 'warning' && 'text-warning',
                  tone === 'danger' && 'text-destructive',
                  tone === 'success' && 'text-success',
                  tone === 'neutral' && 'text-muted-foreground',
                )}
                aria-hidden="true"
              />
            )}
            {t(titleId)}
          </DialogTitle>
          {state?.kind === 'checking' && (
            <DialogDescription className="flex items-center gap-2">
              <Spinner label={t('agentFiles.review.title.checking')} className="text-muted-foreground" />
              {t('agentFiles.review.checking')}
            </DialogDescription>
          )}
          {isWarn && <DialogDescription>{t('agentFiles.review.needsConfirmation.intro')}</DialogDescription>}
          {isBlock && (
            <DialogDescription>
              {t(isAdmin ? 'agentFiles.review.blocked.admin' : 'agentFiles.review.blocked.member')}
            </DialogDescription>
          )}
          {status === 'rejected' && <DialogDescription>{t('agentFiles.review.rejected.intro')}</DialogDescription>}
          {status === 'conflict' && <DialogDescription>{t('agentFiles.review.conflict')}</DialogDescription>}
          {status === 'pending_approval' && (
            <DialogDescription>
              {t('agentFiles.review.pending', { id: shortApprovalId(response?.approval_id ?? '') || '—' })}
            </DialogDescription>
          )}
        </DialogHeader>

        {unavailable && status !== 'rejected' && status !== 'conflict' && <UnavailableNotice />}

        {(isWarn || isBlock || status === 'rejected') && (reasons.length > 0 || summary !== '') && (
          <div className={cn('space-y-2 rounded-lg border p-3', TONE_PANEL[tone])} data-tone={tone}>
            {summary !== '' && status !== 'rejected' && (
              <p className="whitespace-pre-wrap break-words text-sm text-muted-foreground">{summary}</p>
            )}
            <ReasonList reasons={reasons} />
          </div>
        )}

        {(canConfirm || canRequest) && (
          <div className="space-y-1.5">
            <label htmlFor={reasonId} className="text-sm font-medium text-foreground">
              {t('agentFiles.review.reasonLabel')}
            </label>
            <Textarea
              id={reasonId}
              value={reason}
              onChange={(e) => setReason(e.target.value)}
              rows={3}
              className="resize-y"
              aria-describedby={`${reasonId}-hint`}
              autoFocus
              disabled={busy}
            />
            <p id={`${reasonId}-hint`} className="text-xs text-muted-foreground">
              {t('agentFiles.review.reasonHint')}
            </p>
          </div>
        )}

        {needsAck && canConfirm && (
          <label className="flex items-start gap-2 text-sm text-foreground">
            <Checkbox
              checked={ack}
              onCheckedChange={(v) => setAck(v === true)}
              disabled={busy}
            />
            <span>{t('agentFiles.review.acknowledge')}</span>
          </label>
        )}

        {error && (
          <p role="alert" className="text-sm text-destructive">
            {error}
          </p>
        )}

        {state?.kind !== 'checking' && (
          <DialogFooter>
            {status === 'conflict' ? (
              <>
                <Button variant="outline" onClick={onClose}>
                  {t('agentFiles.review.close')}
                </Button>
                <Button variant="brand" onClick={onReload}>
                  {t('agentFiles.review.reload')}
                </Button>
              </>
            ) : (
              <>
                <Button variant="outline" onClick={onClose} disabled={busy}>
                  {canConfirm || canRequest ? t('common.cancel') : t('agentFiles.review.close')}
                </Button>
                {canRequest && (
                  <Button
                    variant={canConfirm ? 'outline' : 'brand'}
                    onClick={() => onRequestApproval(reason.trim())}
                    disabled={busy || !reasonOk}
                  >
                    {t('agentFiles.review.requestApproval')}
                  </Button>
                )}
                {canConfirm && (
                  <Button
                    variant={isBlock ? 'destructive' : 'brand'}
                    onClick={() => onConfirm(reason.trim(), needsAck ? ack : false)}
                    disabled={busy || !reasonOk || (needsAck && !ack)}
                  >
                    {t('agentFiles.review.confirm')}
                  </Button>
                )}
              </>
            )}
          </DialogFooter>
        )}
      </DialogContent>
    </Dialog>
  );
}
