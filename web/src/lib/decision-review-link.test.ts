import { describe, expect, it } from 'vitest';
import { parsePilotReviewDeepLink, pilotReviewDeepLink } from './decision-review-link';

const approvalId = '123e4567-e89b-42d3-a456-426614174000';
const payload = {
  tenant_id: 'tenant & 台北', acl: 'private/team', replay_hash: 'a'.repeat(64),
  snapshot_id: 'synthetic-support-47-35',
  scenario_id: 'dashboard-synthetic-baseline-two-agents-47-35',
};

describe('synthetic pilot review deep link', () => {
  it('constructs only a same-origin Decision Lab route from canonical broker identifiers', () => {
    const path = pilotReviewDeepLink(payload, approvalId);
    expect(path).toMatch(/^\/app\/system\/decision-lab\?/);
    expect(path).toContain('tenant+%26+%E5%8F%B0%E5%8C%97');
    expect(parsePilotReviewDeepLink(path!.split('?')[1])).toEqual({
      tenant_id: payload.tenant_id, acl: payload.acl, snapshot_id: payload.snapshot_id,
      model_version: 'dashboard-synthetic-capacity-v1-47-35',
      scenario_id: payload.scenario_id, expected_hash: payload.replay_hash, approval_id: approvalId,
    });
  });

  it('requires an explicit model for an uploaded pilot and preserves it in the link', () => {
    const uploaded = { ...payload, snapshot_id: 'uploaded-snapshot', model_version: 'uploaded-model',
      scenario_id: 'uploaded-alternative' };
    expect(pilotReviewDeepLink({ ...uploaded, model_version: undefined }, approvalId)).toBeNull();
    const path = pilotReviewDeepLink(uploaded, approvalId)!;
    expect(parsePilotReviewDeepLink(path.split('?')[1])).toEqual({
      tenant_id: uploaded.tenant_id, acl: uploaded.acl, snapshot_id: uploaded.snapshot_id,
      model_version: uploaded.model_version, scenario_id: uploaded.scenario_id,
      expected_hash: uploaded.replay_hash, approval_id: approvalId,
    });
    expect(pilotReviewDeepLink({ ...uploaded, snapshot_id: 'synthetic-support-01-35' }, approvalId)).toBeNull();
  });

  it('rejects malformed source, digest, scope, IDs, and duplicate query keys', () => {
    expect(pilotReviewDeepLink({ ...payload, tenant_id: '\n//evil.test' }, approvalId)).toBeNull();
    expect(pilotReviewDeepLink({ ...payload, scenario_id: 'other-scenario' }, approvalId)).toBeNull();
    expect(pilotReviewDeepLink({ ...payload, replay_hash: 'not-a-hash' }, approvalId)).toBeNull();
    expect(pilotReviewDeepLink({ ...payload, snapshot_id: 'synthetic-support-01-35' }, approvalId)).toBeNull();
    expect(pilotReviewDeepLink(payload, 'not-a-uuid')).toBeNull();
    const valid = pilotReviewDeepLink(payload, approvalId)!;
    expect(parsePilotReviewDeepLink(`${valid.split('?')[1]}&tenant_id=other`)).toBeNull();
  });
});
