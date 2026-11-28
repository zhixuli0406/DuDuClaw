import { dashboardEndpoint } from './dashboard-base';
import { useAuthStore } from '@/stores/auth-store';
import type { ApprovalItem } from './api';
import type { AgentFilePendingResponse } from './agent-files-types';

export function approvalConfirmReturn(raw: unknown): string | undefined {
  if (typeof raw === 'string' && raw.length === '/approvals/confirm#ticket='.length + 64 && /^\/approvals\/confirm#ticket=[a-f0-9]{64}$/.test(raw)) return raw;
}

export interface ApprovalStepUpPreview {
  status: 'live';
  kind_label: string;
  approval: ApprovalItem;
  digest: string;
  has_totp: boolean;
  expires: number;
  agent_file_change: AgentFilePendingResponse | null;
}
export type ApprovalDecision = 'approve' | 'reject';
export class ApprovalStepUpError extends Error {
  constructor(public code: string, public sentence: string) {
    super('Approval confirmation failed');
  }
}
async function request<T>(ticket: string, body?: { decision: ApprovalDecision; expected_digest: string; step_up_csrf?: number[] }): Promise<T> {
  const response = await fetch(dashboardEndpoint('/api/me/approvals/step-up'), {
    method: body ? 'POST' : 'GET',
    headers: {
      Authorization: `Bearer ${useAuthStore.getState().jwt ?? ''}`,
      'X-Approval-Ticket': ticket,
      ...(body ? { 'Content-Type': 'application/json' } : {}),
    },
    body: body ? JSON.stringify(body) : undefined,
    cache: 'no-store', referrerPolicy: 'no-referrer', credentials: 'omit',
  });
  const result = await response.json();
  if (!response.ok) throw new ApprovalStepUpError(result.status, result.error);
  return result as T;
}
export const approvalStepUp = {
  preview: (ticket: string) => request<ApprovalStepUpPreview>(ticket),
  decide: (ticket: string, decision: ApprovalDecision, digest: string, token?: number[]) =>
    request<{ status: 'done'; decision: ApprovalDecision; decided_by: string }>(ticket, {
      decision, expected_digest: digest,
      ...(decision === 'approve' && token ? { step_up_csrf: token } : {}),
    }),
};
