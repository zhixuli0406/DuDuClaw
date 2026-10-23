import { useCallback, useRef, useState } from 'react';
import { api } from '@/lib/api';
import type { AgentFileKey, AgentFileSaveAction, AgentFileSaveResponse } from '@/lib/agent-files-types';
import type { ChangeReviewState } from '@/components/agent-files/ChangeReviewDialog';

/** What was sent for checking: the follow-up (confirm / ask an admin)
 *  resends exactly this, never whatever the editor holds by then. */
export type SentChange =
  | { kind: 'save'; file: AgentFileKey; content: string; baseHash: string }
  | { kind: 'rollback'; file: AgentFileKey; versionId: string; baseHash: string };

export interface SaveCallbacks {
  /** The change was applied (or the content was already the same). */
  onApplied: (res: AgentFileSaveResponse, sent: SentChange) => void;
  /** Someone else saved first; `sent` is what this person tried to save. */
  onConflict: (sent: SentChange) => void;
  /** The change now waits for an admin. */
  onPending: (res: AgentFileSaveResponse) => void;
  /** The call itself failed (network, permission). */
  onError: (error: unknown) => void;
}

/**
 * The save / check / confirm state machine of the profile file tab. The
 * gateway answers every save with the shared envelope; anything that is not
 * a plain "applied" opens ChangeReviewDialog, whose buttons resend the SAME
 * change with `confirm` or `request_approval`.
 */
export function useAgentFileSave(agentId: string, cb: SaveCallbacks, describe: (e: unknown) => string) {
  const [review, setReview] = useState<ChangeReviewState | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const sentRef = useRef<SentChange | null>(null);
  const cbRef = useRef(cb);
  cbRef.current = cb;

  const call = useCallback(
    (sent: SentChange, follow?: { action: AgentFileSaveAction; analysis_id: string; reason: string; acknowledge_risk: boolean }) =>
      sent.kind === 'save'
        ? api.agentFiles.save({ agent_id: agentId, file: sent.file, content: sent.content, base_hash: sent.baseHash, ...follow })
        : api.agentFiles.rollback({ agent_id: agentId, file: sent.file, version_id: sent.versionId, base_hash: sent.baseHash, ...follow }),
    [agentId],
  );

  const handle = useCallback((res: AgentFileSaveResponse, sent: SentChange) => {
    if (res.status === 'conflict' && sent.kind === 'save') {
      setReview(null);
      cbRef.current.onConflict(sent);
      return;
    }
    if (res.status === 'applied' || res.status === 'unchanged') cbRef.current.onApplied(res, sent);
    if (res.status === 'pending_approval') cbRef.current.onPending(res);
    // The dialog decides itself whether an answer needs to stay on screen
    // (an applied change whose automatic check did not finish does).
    setReview({ kind: 'response', response: res });
  }, []);

  /** Send a change for checking. */
  const submit = useCallback(
    async (sent: SentChange) => {
      sentRef.current = sent;
      setError(null);
      setReview({ kind: 'checking' });
      try {
        handle(await call(sent), sent);
      } catch (e) {
        setReview(null);
        cbRef.current.onError(e);
      }
    },
    [call, handle],
  );

  /** The dialog's "confirm" / "ask an admin". */
  const followUp = useCallback(
    async (action: AgentFileSaveAction, reason: string, acknowledgeRisk: boolean) => {
      const sent = sentRef.current;
      const analysisId = review?.kind === 'response' ? review.response.review?.analysis_id : null;
      if (!sent || !analysisId) return;
      setBusy(true);
      setError(null);
      try {
        handle(await call(sent, { action, analysis_id: analysisId, reason, acknowledge_risk: acknowledgeRisk }), sent);
      } catch (e) {
        setError(describe(e));
      } finally {
        setBusy(false);
      }
    },
    [call, handle, review, describe],
  );

  const close = useCallback(() => {
    setReview(null);
    setError(null);
  }, []);

  return { review, busy, error, submit, followUp, close };
}
