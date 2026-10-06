import { useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { useAuthStore } from '@/stores/auth-store';
import { Button } from '@/components/mds';
import { ConfirmDialog } from '@/components/settings/controls';
import { ReviewEvidencePanel } from './ReviewEvidencePanel';
import { CreateWorkflowDraft } from './CreateWorkflowDraft';
import { WorkflowDraftPanel } from './WorkflowDraftPanel';
import { workflowDraftService, workflowReviewApi, type ReviewResponse } from './workflow-api';
import { errorText } from './workflow-text';
import type { DraftView, ReviewSnapshot } from './types';

const POLL_MS = 5000;
const snapshotKey = (s: ReviewSnapshot | null | undefined) => (s ? `${s.snapshot_id}:${s.snapshot_hash}` : null);

/**
 * Fold a freshly read review into what the person is looking at. The visible
 * snapshot is never swapped for a different one behind their back: a different
 * snapshot is parked as `newer` and the person chooses to switch.
 */
export function foldReview(viewed: ReviewResponse | null, incoming: ReviewResponse):
  { viewed: ReviewResponse | null; newer: ReviewResponse | null } {
  if (!viewed || !viewed.snapshot) return { viewed: incoming, newer: null };
  if (!incoming.snapshot) return { viewed, newer: null };
  if (snapshotKey(viewed.snapshot) === snapshotKey(incoming.snapshot)) return { viewed: incoming, newer: null };
  return { viewed, newer: incoming };
}

/** Reachable task flow; reloads durable state instead of reconstructing receipts. */
export function TaskWorkflowReview({ taskId, completed }: { taskId: string; completed: boolean }) {
  const intl = useIntl();
  const text = (id: string, values?: Record<string, string>) => intl.formatMessage({ id }, values);
  const user = useAuthStore((s) => s.user);
  const canReview = user?.role === 'admin' || user?.role === 'manager';
  const generation = useRef(0);
  const currentTask = useRef(taskId);
  currentTask.current = taskId;
  // Poll ordering: a response is applied only if it is newer than the last
  // applied one and was started after the most recent user action.
  const requestSeq = useRef(0);
  const appliedSeq = useRef(0);
  const floorSeq = useRef(0);
  const busyRef = useRef(false);
  const viewedRef = useRef<ReviewResponse | null>(null);
  const [open, setOpen] = useState(false);
  const [viewed, setViewedState] = useState<ReviewResponse | null>(null);
  const [newer, setNewer] = useState<ReviewResponse | null>(null);
  const [drafts, setDrafts] = useState<DraftView[]>([]);
  const [creating, setCreating] = useState(false);
  const [busy, setBusyState] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [pendingAccept, setPendingAccept] = useState<ReviewSnapshot | null>(null);

  function setViewed(next: ReviewResponse | null) { viewedRef.current = next; setViewedState(next); }
  function setBusy(next: boolean) { busyRef.current = next; setBusyState(next); }
  function invalidateInFlightReads() { floorSeq.current = ++requestSeq.current; }

  useEffect(() => {
    setBusy(false);
    setPendingAccept(null);
    if (!open) { generation.current++; return; }
    const token = ++generation.current;
    let cancelled = false;
    requestSeq.current = 0; appliedSeq.current = 0; floorSeq.current = 0;
    setViewed(null); setNewer(null); setDrafts([]); setCreating(false); setError(null); setLoadError(null);
    async function load() {
      const seq = ++requestSeq.current;
      const stillValid = () => !cancelled && generation.current === token && seq > floorSeq.current && seq > appliedSeq.current && !busyRef.current;
      try {
        const [r, d] = await Promise.all([workflowReviewApi.get(taskId), workflowDraftService.list(taskId)]);
        if (!stillValid()) return;
        appliedSeq.current = seq;
        const folded = foldReview(viewedRef.current, r);
        setViewed(folded.viewed); setNewer(folded.newer); setDrafts(d); setLoadError(null);
      } catch (e) { if (stillValid()) setLoadError(errorText(intl, e)); }
    }
    void load();
    const timer = window.setInterval(() => void load(), POLL_MS);
    return () => { cancelled = true; if (generation.current === token) generation.current++; window.clearInterval(timer); };
    // eslint-disable-next-line react-hooks/exhaustive-deps -- intl only formats errors
  }, [taskId, open]);

  async function run(action: (token: number) => Promise<void>) {
    if (busyRef.current) return;
    const token = generation.current;
    setBusy(true); setError(null); invalidateInFlightReads();
    try { await action(token); }
    catch (e) { if (generation.current === token) setError(errorText(intl, e)); }
    finally { if (generation.current === token) { setBusy(false); invalidateInFlightReads(); } }
  }
  const capture = () => run(async (token) => {
    const result = await workflowReviewApi.capture(taskId);
    if (generation.current === token) { setViewed(result); setNewer(null); setCreating(false); }
  });
  // Accept exactly the snapshot the person confirmed (id + hash), never
  // whatever the latest poll happens to hold.
  const accept = (fixed: ReviewSnapshot) => run(async (token) => {
    await workflowReviewApi.accept(fixed);
    const result = await workflowReviewApi.get(taskId);
    if (generation.current === token) { const folded = foldReview(viewedRef.current, result); setViewed(folded.viewed); setNewer(folded.newer); }
  });
  function switchToNewer() { if (newer) { setViewed(newer); setNewer(null); setCreating(false); setPendingAccept(null); } }

  const snapshot = viewed?.snapshot ? { ...viewed.snapshot, artifacts: viewed.current_artifacts ?? viewed.snapshot.artifacts } : null;
  const canAct = Boolean(snapshot) && canReview && !newer;
  return <details className="rounded-xl border border-border p-4" onToggle={(e) => setOpen(e.currentTarget.open)}>
    <summary className="cursor-pointer text-sm font-semibold">{text('workflow.review.section')}</summary>
    {open && <div className="mt-4 space-y-4">
      {canReview && <Button variant="outline" disabled={busy} onClick={() => void capture()}>{text('workflow.review.capture')}</Button>}
      {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
      {loadError && <p role="alert" className="text-sm text-destructive">{loadError}</p>}
      {loadError && snapshot && <p role="status" className="text-xs text-amber-700 dark:text-amber-400">{text('workflow.review.outdated')}</p>}
      {newer && <div role="status" className="space-y-2 rounded-lg border border-amber-500/40 bg-amber-500/10 p-3 text-sm">
        <p>{text('workflow.review.newVersion')}</p>
        <Button size="sm" variant="outline" onClick={switchToNewer}>{text('workflow.review.switchNew')}</Button>
      </div>}
      {!viewed && !loadError && <p role="status" className="text-sm text-muted-foreground">{text('workflow.review.loading')}</p>}
      {viewed && !snapshot && <p className="text-sm text-muted-foreground">{text('workflow.review.none')}</p>}
      {snapshot && <ReviewEvidencePanel snapshot={snapshot} onCreateDraft={canReview && completed && viewed?.authority_current && !newer ? () => setCreating(true) : undefined} busy={busy} />}
      {snapshot && canReview && <Button variant="outline" disabled={busy || !canAct || !viewed?.authority_current || snapshot.artifacts.length === 0 || snapshot.artifacts.some((a) => a.integrity !== 'current')} onClick={() => setPendingAccept(snapshot)}>{text('workflow.review.accept')}</Button>}
      {viewed?.acceptance && snapshot && viewed.acceptance.snapshot_hash === snapshot.snapshot_hash && <p role="status" className="break-all text-xs">{text('workflow.review.accepted')}: <span className="font-mono">{viewed.acceptance.snapshot_hash}</span> · {viewed.acceptance.accepted_at}</p>}
      {creating && snapshot && !newer && <CreateWorkflowDraft key={snapshot.snapshot_id} snapshot={snapshot} service={workflowDraftService} onCreated={(draft) => { if (currentTask.current !== draft.draft.source_task) return; setDrafts((previous) => [draft, ...previous.filter((d) => d.draft.draft_id !== draft.draft.draft_id)]); setCreating(false); }} />}
      {drafts.map(({ draft }) => <WorkflowDraftPanel key={`${draft.draft_id}:${draft.revision}`} draftId={draft.draft_id} revision={draft.revision} service={workflowDraftService} />)}
      <ConfirmDialog
        open={pendingAccept !== null}
        onClose={() => setPendingAccept(null)}
        onConfirm={() => { const fixed = pendingAccept; setPendingAccept(null); if (fixed) void accept(fixed); }}
        title={text('workflow.confirm.accept.title')}
        message={text('workflow.confirm.accept.message', { hash: pendingAccept?.snapshot_hash.slice(0, 12) ?? '' })}
        confirmLabel={text('workflow.confirm.accept.confirm')}
        busy={busy}
      />
    </div>}
  </details>;
}
