import { parseAdminAiSettings } from '@/components/inbox/AdminAiSettingsView';
import { isOrgChangePlacement } from '@/components/inbox/OrgChangeFacts';
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
  constructor(public code: string, public sentence: string, public httpStatus?: number) {
    super('Approval confirmation failed');
  }
}
function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}
function nonEmpty(value: unknown): value is string {
  return typeof value === 'string' && value.trim().length > 0;
}
function finiteNumber(value: unknown): value is number {
  return typeof value === 'number' && Number.isFinite(value);
}
function nullableString(value: unknown): boolean {
  return value === null || typeof value === 'string';
}
function isWorkflowActivation(value: unknown): boolean {
  if (!isRecord(value) || !nonEmpty(value.workflow_id) || !finiteNumber(value.revision)
    || !Array.isArray(value.effect_targets) || value.effect_targets.length < 1 || !value.effect_targets.every(target =>
      isRecord(target) && nonEmpty(target.step_id) && nonEmpty(target.tool)
      && nullableString(target.param) && nullableString(target.target))) return false;
  if (('expires_at' in value && !nullableString(value.expires_at))
    || ('submitted_by' in value && !nullableString(value.submitted_by))) return false;
  if ('pricing' in value && value.pricing !== null) {
    const pricing = value.pricing;
    if (!isRecord(pricing) || typeof pricing.money_limits_effective !== 'boolean') return false;
    if ('limits' in pricing) {
      const limits = pricing.limits;
      if (!isRecord(limits) || !['max_runs_per_month', 'max_steps_per_run', 'max_reads_per_run', 'max_effects_per_run']
        .every(key => finiteNumber(limits[key]))) return false;
    }
  }
  return true;
}
function isFileDiff(value: unknown): boolean {
  if (!isRecord(value) || !finiteNumber(value.added) || !finiteNumber(value.removed)
    || !Number.isInteger(value.added) || value.added < 0
    || !Number.isInteger(value.removed) || value.removed < 0
    || value.added + value.removed < 1 || !Array.isArray(value.hunks)) return false;
  let added = 0;
  let removed = 0;
  for (const hunk of value.hunks) {
    if (!isRecord(hunk) || !['equal', 'add', 'remove'].includes(hunk.op as string)
      || !Array.isArray(hunk.lines) || !hunk.lines.every(line => typeof line === 'string')) return false;
    if (hunk.op === 'add') added += hunk.lines.length;
    if (hunk.op === 'remove') removed += hunk.lines.length;
  }
  return value.added === added && value.removed === removed;
}
function isPreview(value: unknown): value is ApprovalStepUpPreview {
  if (!isRecord(value) || value.status !== 'live' || !nonEmpty(value.digest)
    || typeof value.expires !== 'number' || !Number.isFinite(value.expires)
    || typeof value.has_totp !== 'boolean' || !nonEmpty(value.kind_label)
    || !isRecord(value.approval) || !nonEmpty(value.approval.id)
    || !nonEmpty(value.approval.digest) || value.approval.digest !== value.digest
    || typeof value.approval.summary !== 'string'
    || !(value.agent_file_change === null || isRecord(value.agent_file_change))) return false;
  const { kind, payload } = value.approval;
  switch (kind) {
    case 'agent_file_change': {
      const file = value.agent_file_change;
      return isRecord(file) && file.approval_id === value.approval.id
        && file.content_available === true && file.stale === false
        && nonEmpty(file.file) && isFileDiff(file.diff)
        && typeof file.requested_by === 'string' && typeof file.reason === 'string'
        && ['pass', 'warn', 'block'].includes(file.verdict as string)
        && Array.isArray(file.reason_categories) && file.reason_categories.every(category => typeof category === 'string');
    }
    case 'workflow_activation': return isWorkflowActivation(value.approval.workflow_activation);
    case 'admin_ai_settings': return parseAdminAiSettings(payload) !== null;
    case 'org_change': return isRecord(payload) && nonEmpty(payload.agent_id)
      && isOrgChangePlacement(payload.before) && isOrgChangePlacement(payload.after)
      && (!('reason' in payload) || typeof payload.reason === 'string');
    case 'expert_skills_enable':
    case 'autopilot_action':
    case 'cli_egress':
    case 'egress_guard':
      return nonEmpty(payload) || (isRecord(payload) && Object.keys(payload).length > 0);
    default: return false;
  }
}
interface ApprovalStepUpResult {
  status: 'done';
  decision: ApprovalDecision;
  decided_by: string;
}
function unavailable(): ApprovalStepUpError {
  return new ApprovalStepUpError('unavailable', '');
}
const errorCodes: Record<number, readonly string[]> = {
  401: ['unauthenticated'],
  404: ['not_found'],
  410: ['expired'],
  409: ['used', 'digest_mismatch', 'already_decided'],
  403: ['forbidden', 'feature_off', 'not_supported', 'step_up_required'],
  400: ['invalid_request'],
  503: ['unavailable', 'internal'],
};
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
    if (!isRecord(result) || typeof result.status !== 'string' || !nonEmpty(result.error)
      || !errorCodes[response.status]?.includes(result.status)) throw unavailable();
    throw new ApprovalStepUpError(result.status, result.error, response.status);
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
