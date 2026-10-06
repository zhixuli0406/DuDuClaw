import { client } from '@/lib/ws-client';
import type { DraftService, DraftView, ReviewArtifact, ReviewSnapshot } from './types';

export interface ReviewResponse {
  snapshot: ReviewSnapshot | null;
  current_artifacts?: ReviewArtifact[];
  authority_current?: boolean;
  acceptance?: { accepted_by: string; accepted_at: string; snapshot_hash: string } | null;
}
export const workflowReviewApi = {
  get: (taskId: string) => client.call('tasks.review_snapshot', { task_id: taskId }) as Promise<ReviewResponse>,
  capture: (taskId: string) => client.call('tasks.review_snapshot', { task_id: taskId, action: 'capture' }) as Promise<ReviewResponse>,
  accept: (snapshot: ReviewSnapshot) => client.call('tasks.review_accept', { task_id: snapshot.task_id, snapshot_id: snapshot.snapshot_id, snapshot_hash: snapshot.snapshot_hash }),
};
export const workflowDraftService: DraftService = {
  list: async (taskId) => {
    const drafts: DraftView[] = []; let cursor: number | undefined;
    // Read stable task-scoped pages; fail explicitly if the bounded browser
    // read cannot complete, rather than silently hiding older drafts.
    for (let page = 0; page < 32; page++) {
      const response = await client.call('workflow_drafts.list', { task_id: taskId, ...(cursor ? { cursor } : {}) }) as { drafts: DraftView[]; next_cursor?: number | null };
      if (!Array.isArray(response?.drafts)) throw new Error('Invalid workflow draft response');
      drafts.push(...response.drafts);
      if (response.next_cursor == null) return drafts;
      if (!Number.isSafeInteger(response.next_cursor) || response.next_cursor <= 0 || (cursor != null && response.next_cursor >= cursor)) throw new Error('Invalid workflow draft cursor');
      cursor = response.next_cursor;
    }
    throw new Error('Workflow draft list exceeds the bounded page limit');
  },
  create: (taskId, snapshotId, snapshotHash, proposal) => client.call('workflow_drafts.create', { task_id: taskId, snapshot_id: snapshotId, snapshot_hash: snapshotHash, proposal }) as Promise<DraftView>,
  get: (draftId, revision) => client.call('workflow_drafts.get', { draft_id: draftId, revision }) as Promise<DraftView>,
  runFixture: (draftId, revision, fixtureId, draftHash) => client.call('workflow_drafts.run_fixture', { draft_id: draftId, revision, fixture_id: fixtureId, draft_hash: draftHash }) as Promise<{ run_id: string }>,
  commitActivation: (draftId, revision, draftHash) => client.call('workflow_drafts.commit_activation', { draft_id: draftId, revision, draft_hash: draftHash }),
  revokeActivation: (draftId, revision, draftHash) => client.call('workflow_drafts.revoke_activation', { draft_id: draftId, revision, draft_hash: draftHash }),
  requestActivation: (draftId, revision, draftHash) => client.call('workflow_drafts.request_activation', { draft_id: draftId, revision, draft_hash: draftHash }) as Promise<{ approval_id: string }>,
};
