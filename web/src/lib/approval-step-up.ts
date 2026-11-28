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
function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}
function nonEmpty(value: unknown): value is string {
  return typeof value === 'string' && value.trim().length > 0;
}
function isPreview(value: unknown): value is ApprovalStepUpPreview {
  return isRecord(value) && value.status === 'live' && nonEmpty(value.digest)
    && typeof value.expires === 'number' && Number.isFinite(value.expires)
    && typeof value.has_totp === 'boolean' && nonEmpty(value.kind_label)
    && isRecord(value.approval) && nonEmpty(value.approval.id)
    && typeof value.approval.kind === 'string' && typeof value.approval.summary === 'string'
    && (value.agent_file_change === null || isRecord(value.agent_file_change));
}
interface ApprovalStepUpResult {
  status: 'done';
  decision: ApprovalDecision;
  decided_by: string;
}
function unavailable(): ApprovalStepUpError {
  return new ApprovalStepUpError('unavailable', '');
}
async function request<T>(ticket: string, validate: (value: unknown) => value is T, body?: { decision: ApprovalDecision; expected_digest: string; step_up_csrf?: number[] }): Promise<T> {
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
  let result: unknown;
  try { result = await response.json(); } catch { throw unavailable(); }
  if (!response.ok) {
    if (!isRecord(result) || typeof result.status !== 'string' || typeof result.error !== 'string') throw unavailable();
    throw new ApprovalStepUpError(result.status, result.error);
  }
  if (response.status !== 200 || !validate(result)) throw unavailable();
  return result;
}
export const approvalStepUp = {
  preview: (ticket: string) => request(ticket, isPreview),
  decide: async (ticket: string, decision: ApprovalDecision, digest: string, token?: number[]) => {
    if (!nonEmpty(digest)) throw unavailable();
    return request(ticket, (value): value is ApprovalStepUpResult =>
      isRecord(value) && value.status === 'done' && value.decision === decision
      && typeof value.decided_by === 'string', {
      decision, expected_digest: digest,
      ...(decision === 'approve' && token ? { step_up_csrf: token } : {}),
    });
  },
};
