import type { IntlShape } from 'react-intl';

/**
 * User-facing wording for server-side codes and errors. The raw code or the
 * server's error string never reaches the screen: a known value maps to a
 * sentence in the active language and anything else gets one generic line.
 */
const REASON_CODES = new Set([
  'artifact_content_changed', 'artifact_missing', 'artifact_source_missing', 'artifact_archive_missing',
  'artifact_archive_unavailable', 'artifact_unreadable', 'artifact_too_large', 'artifact_outside_home',
  'artifact_outside_owner', 'artifact_owner_symlink_refused', 'artifact_root_unavailable',
  'legacy_no_content_hash', 'legacy_no_review_snapshot', 'task_authority_changed',
  'artifact_snapshot_truncated', 'no_recorded_deliverable', 'source_snapshot_stale_or_unverified',
]);

export function reasonText(intl: IntlShape, code: string): string {
  const id = REASON_CODES.has(code) ? `workflow.reason.${code}` : 'workflow.reason.unknown';
  return intl.formatMessage({ id });
}

/** Distinct, ordered, user-readable sentences for a list of reason codes. */
export function reasonList(intl: IntlShape, codes: readonly string[]): string[] {
  return [...new Set(codes.map((c) => reasonText(intl, c)))];
}

/** Closed codes the server returns as `{ code, message }` (gateway
 *  `handlers/workflow_errors.rs`). The code decides the sentence. */
const ERROR_CODES: Record<string, string> = {
  permission_denied: 'permission',
  identity_unavailable: 'identity',
  manager_required: 'manager',
  admin_required: 'admin',
  snapshot_not_latest: 'snapshotNotLatest',
  snapshot_truncated: 'snapshotTruncated',
  stale: 'stale',
  fixture_missing: 'fixtureMissing',
  fixture_expired: 'fixtureExpired',
  fixture_mismatch: 'fixtureMismatch',
  activation_exists: 'exists',
  approval_pending: 'approvalPending',
  invalid_proposal: 'invalidProposal',
  source_task_unfinished: 'sourceTaskUnfinished',
  workflow_activation_suspended: 'suspended',
  generic: 'generic',
};

/** Fallback for errors that still arrive as plain text (older servers, or a
 *  stored code such as an activation's `error_code`). */
const ERROR_RULES: Array<[RegExp, string]> = [
  [/identity unavailable/i, 'identity'],
  [/permission denied|forbidden|not permitted|draft not found|snapshot not found/i, 'permission'],
  [/fixture evidence missing/i, 'fixtureMissing'],
  [/fixture evidence expired/i, 'fixtureExpired'],
  [/fixture evidence mismatched/i, 'fixtureMismatch'],
  [/not approved|approval.*(pending|required)/i, 'approvalPending'],
  [/activation already exists/i, 'exists'],
  [/activation_suspended/i, 'suspended'],
  [/stale|unverified|hash mismatch|snapshot.*changed/i, 'stale'],
];

function errorCode(error: unknown): string | null {
  if (error && typeof error === 'object' && 'code' in error) {
    const code = (error as { code?: unknown }).code;
    return typeof code === 'string' ? code : null;
  }
  return null;
}

export function errorText(intl: IntlShape, error: unknown): string {
  const code = errorCode(error);
  if (code !== null) {
    return intl.formatMessage({ id: `workflow.error.${ERROR_CODES[code] ?? 'generic'}` });
  }
  const raw = error instanceof Error ? error.message : typeof error === 'string' ? error : '';
  const key = ERROR_CODES[raw] ?? ERROR_RULES.find(([re]) => re.test(raw))?.[1] ?? 'generic';
  return intl.formatMessage({ id: `workflow.error.${key}` });
}

const RUN_STATUSES = new Set(['pending', 'running', 'waiting_approval', 'needs_input', 'succeeded', 'failed', 'uncertain', 'cancelled', 'blocked']);
export function runStatusText(intl: IntlShape, status: string): string {
  return intl.formatMessage({ id: RUN_STATUSES.has(status) ? `workflow.runStatus.${status}` : 'workflow.runStatus.other' });
}

const OUTCOMES = new Set(['matched', 'mismatched', 'not_evaluated']);
export function outcomeText(intl: IntlShape, outcome: string): string {
  return intl.formatMessage({ id: OUTCOMES.has(outcome) ? `workflow.outcome.${outcome}` : 'workflow.runStatus.other' });
}

const ACTIVATION_STATES = new Set(['prepared', 'active', 'suspended', 'expired', 'revoking', 'revoked']);
export function activationStateText(intl: IntlShape, state: string): string {
  return intl.formatMessage({ id: `workflow.activation.state.${ACTIVATION_STATES.has(state) ? state : 'other'}` });
}

/** Settings groups whose change suspends an activation (gateway
 *  `approval::policy_snapshot::POLICY_CATEGORIES`). */
const POLICY_CATEGORIES = new Set(['capabilities', 'permissions', 'agent_authority', 'contract', 'preset', 'org_chain', 'delegation', 'acp', 'provenance', 'integrations', 'killswitch']);
export function policyCategoryText(intl: IntlShape, category: string): string {
  return intl.formatMessage({ id: POLICY_CATEGORIES.has(category) ? `workflow.policyCategory.${category}` : 'workflow.policyCategory.other' });
}
