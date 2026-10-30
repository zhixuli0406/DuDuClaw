import { client } from './ws-client';
import { lookupMessage, toast } from './toast';

export interface ForgetReceiptCount {
  agent_id: string;
  memories_deleted: number;
  key_facts_deleted: number;
  archive_deleted: number;
  unattributed: number;
  system_kept: number;
}

export interface ForgetReceipt {
  id: string;
  batch_id: string;
  connector_ref: string;
  display_name: string;
  completed_at: string;
  approved_by: string;
  counts: ForgetReceiptCount[];
  not_covered: string[];
}

export const forgetReceipts = {
  list: () => client.call('memory.forget_receipts_list', {}) as Promise<{ receipts: ForgetReceipt[] }>,
  get: (id: string) => client.call('memory.forget_receipts_get', { id }) as Promise<{ receipt: ForgetReceipt }>,
};

/** Keep the disconnect result intact; deletion is a separate approval. */
export function notifyDisconnectForget<T>(response: T): T {
  if (!response || typeof response !== 'object' || !('forget_memories' in response)) return response;
  const notice = response.forget_memories;
  if (!notice || typeof notice !== 'object' || !('status' in notice)) return response;
  if (notice.status === 'pending_approval') {
    const approvalId = 'approval_id' in notice && typeof notice.approval_id === 'string'
      ? notice.approval_id : null;
    toast.info(lookupMessage('forget.disconnect.pending'), {
      durationMs: 12000,
      ...(approvalId ? {
        action: {
          label: lookupMessage('forget.disconnect.openApproval'),
          onClick: () => window.location.assign(`/inbox?item=${encodeURIComponent(approvalId)}`),
        },
      } : {}),
    });
  } else if (notice.status === 'disabled') {
    toast.info(lookupMessage('forget.disconnect.disabled'), { durationMs: 12000 });
  } else if (notice.status === 'failed') {
    toast.error(lookupMessage('forget.disconnect.failed'), { durationMs: 12000 });
  }
  return response;
}
