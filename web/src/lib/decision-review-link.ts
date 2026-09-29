import type { PilotReviewSelection } from './decision-api';

export type PilotReviewDeepLink = PilotReviewSelection & { approval_id: string };

const MAX_U64 = 18_446_744_073_709_551_615n;
const UUID_V4 = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
const SHA256 = /^[0-9a-f]{64}$/;

function validScope(value: string): boolean {
  return value.length > 0 && value.length <= 256 && value.trim() === value && !/[\u0000-\u001f\u007f]/.test(value);
}

function validIdentifier(value: string): boolean {
  return value.length > 0 && value.length <= 128 && value.trim() === value
    && !/[\u0000-\u001f\u007f]/.test(value);
}

function validate(input: PilotReviewDeepLink): boolean {
  if (!validScope(input.tenant_id) || !validScope(input.acl)
    || !SHA256.test(input.expected_hash) || !UUID_V4.test(input.approval_id)
    || !validIdentifier(input.snapshot_id) || !validIdentifier(input.model_version)
    || !validIdentifier(input.scenario_id)) return false;
  if (!input.snapshot_id.startsWith('synthetic-support-')) return true;
  const match = /^synthetic-support-(0|[1-9][0-9]*)-([0-9]+)$/.exec(input.snapshot_id);
  if (!match || match[1].length > 20 || BigInt(match[1]) > MAX_U64) return false;
  const days = Number(match[2]);
  if (!Number.isInteger(days) || days < 21 || days > 90 || String(days) !== match[2]) return false;
  const suffix = `${match[1]}-${days}`;
  return input.model_version === `dashboard-synthetic-capacity-v1-${suffix}`
    && (input.scenario_id === `dashboard-synthetic-baseline-two-agents-${suffix}`
      || input.scenario_id === `dashboard-synthetic-three-agents-${suffix}`);
}

export function parsePilotReviewDeepLink(search: string): PilotReviewDeepLink | null {
  const query = new URLSearchParams(search);
  const keys = ['tenant_id', 'acl', 'snapshot_id', 'model_version', 'scenario_id', 'expected_hash', 'approval_id'] as const;
  if (keys.some((key) => query.getAll(key).length !== 1)) return null;
  const input = Object.fromEntries(keys.map((key) => [key, query.get(key)!])) as unknown as PilotReviewDeepLink;
  return validate(input) ? input : null;
}

/** Build only an internal Decision Lab path from the broker's exact payload. */
export function pilotReviewDeepLink(payload: unknown, approvalId: string): string | null {
  if (!payload || typeof payload !== 'object' || Array.isArray(payload)) return null;
  const record = payload as Record<string, unknown>;
  const keys = ['tenant_id', 'acl', 'snapshot_id', 'scenario_id', 'replay_hash'] as const;
  if (keys.some((key) => typeof record[key] !== 'string')) return null;
  const snapshotId = record.snapshot_id as string;
  const syntheticSuffix = snapshotId.startsWith('synthetic-support-')
    ? snapshotId.slice('synthetic-support-'.length) : null;
  const input: PilotReviewDeepLink = {
    tenant_id: record.tenant_id as string,
    acl: record.acl as string,
    snapshot_id: snapshotId,
    model_version: typeof record.model_version === 'string' ? record.model_version
      : syntheticSuffix === null ? '' : `dashboard-synthetic-capacity-v1-${syntheticSuffix}`,
    scenario_id: record.scenario_id as string,
    expected_hash: record.replay_hash as string,
    approval_id: approvalId,
  };
  if (!validate(input)) return null;
  return `/app/system/decision-lab?${new URLSearchParams(Object.entries(input))}`;
}
