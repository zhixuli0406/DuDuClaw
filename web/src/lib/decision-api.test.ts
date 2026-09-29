import { afterEach, describe, expect, it, vi } from 'vitest';
import { useAuthStore } from '@/stores/auth-store';
import { decisionApi } from './decision-api';

afterEach(() => {
  vi.unstubAllGlobals();
  useAuthStore.setState({ jwt: null } as never);
});

describe('decisionApi', () => {
  it('requests a scoped overview with bearer auth and limit', async () => {
    useAuthStore.setState({ jwt: 'jwt-token' } as never);
    const fetchMock = vi.fn().mockResolvedValue({ ok: true, json: async () => ({ status: 'exploratory' }) });
    vi.stubGlobal('fetch', fetchMock);
    await decisionApi.overview({ tenant_id: 'tenant-a', acl: 'private' }, 25);
    expect(fetchMock).toHaveBeenCalledWith('/api/decision/overview?tenant_id=tenant-a&acl=private&limit=25', expect.objectContaining({
      method: 'GET', headers: { Authorization: 'Bearer jwt-token' },
    }));
  });

  it('requests an exact scoped shadow policy without sending outcome data', async () => {
    useAuthStore.setState({ jwt: 'jwt-token' } as never);
    const fetchMock = vi.fn().mockResolvedValue({ ok: true, json: async () => ({ status: 'descriptive_shadow_monitor' }) });
    vi.stubGlobal('fetch', fetchMock);
    await decisionApi.shadowMonitor({ tenant_id: 'tenant-a', acl: 'private' }, 'policy/a');
    expect(fetchMock).toHaveBeenCalledWith(
      '/api/decision/shadow-monitor?tenant_id=tenant-a&acl=private&policy_id=policy%2Fa',
      expect.objectContaining({ method: 'GET', headers: { Authorization: 'Bearer jwt-token' } }),
    );
  });

  it('posts scrub scope and parses the scrub count plus updated overview', async () => {
    const fetchMock = vi.fn().mockResolvedValue({ ok: true, json: async () => ({ scrubbed: 1, overview: { status: 'exploratory' } }) });
    vi.stubGlobal('fetch', fetchMock);
    const result = await decisionApi.scrubTicketSources({ tenant_id: 'tenant-a', acl: 'private' });
    expect(fetchMock).toHaveBeenCalledWith('/api/decision/ticket-sources/scrub', expect.objectContaining({
      method: 'POST', body: '{"tenant_id":"tenant-a","acl":"private"}',
    }));
    expect(result.scrubbed).toBe(1);
  });

  it('posts an explicit uploaded pilot pair with bearer auth and no browser file paths', async () => {
    useAuthStore.setState({ jwt: 'jwt-token' } as never);
    const receipt = { status: 'exploratory_operator_upload', snapshot_id: 'snapshot-1' };
    const fetchMock = vi.fn().mockResolvedValue({ ok: true, json: async () => receipt });
    vi.stubGlobal('fetch', fetchMock);
    const input = { tenant_id: 'tenant-a', acl: 'private', expected_queue_id: 'queue-a',
      source_lineage: 'operator-export', retention_until_utc: '2099-01-01T00:00:00Z',
      export: { snapshot_id: 'snapshot-1', baseline_scenario_id: 'baseline-1',
        window_start_utc: '2026-01-01T00:00:00Z', data_cutoff_utc: '2026-01-02T00:00:00Z',
        source_version_hashes: ['a'.repeat(64)], seed: 0, horizon_days: 1, tickets: [], staffing: [] },
      model: { version: 'model-1', service_capacity_per_agent_day: 8, sla_days: 2,
        staff_cost_cents_per_agent_day: 10_000 },
      alternative_scenario: { id: 'alternative-1', agents_by_day: [2], fixed_extra_capacity_by_day: [0] },
    };
    expect(await decisionApi.importPilot(input)).toEqual(receipt);
    expect(fetchMock).toHaveBeenCalledWith('/api/decision/import-pilot', expect.objectContaining({
      method: 'POST', headers: { Authorization: 'Bearer jwt-token', 'Content-Type': 'application/json' },
      body: JSON.stringify(input),
    }));
  });

  it('wires catalog, synthetic pilot, comparison, replay, and engineering validation contracts', async () => {
    const fetchMock = vi.fn()
      .mockResolvedValueOnce({ ok: true, json: async () => ({ snapshots: [], models: [], scenarios: [], synthetic_pilots: [] }) })
      .mockResolvedValueOnce({ ok: true, json: async () => ({ snapshot_id: 's1' }) })
      .mockResolvedValueOnce({ ok: true, json: async () => ({ stage_outcome: 'staged' }) })
      .mockResolvedValueOnce({ ok: true, json: async () => ({ brief: { status: 'exploratory' } }) })
      .mockResolvedValueOnce({ ok: true, json: async () => ({ result: { days: [] } }) })
      .mockResolvedValueOnce({ ok: true, json: async () => ({ report: { status: 'synthetic_only_exploratory' } }) });
    vi.stubGlobal('fetch', fetchMock);
    const scope = { tenant_id: 'tenant-a', acl: 'private' };
    await decisionApi.catalog(scope);
    await decisionApi.createSyntheticPilot(scope, { seed: 7, days: 21 });
    await decisionApi.stageSyntheticLifecycle(scope, { seed: 7, days: 21, kind: 'acl_lost' });
    await decisionApi.compare({ ...scope, snapshot_id: 's1', model_version: 'm1',
      baseline_scenario_id: 'base', alternative_scenario_id: 'alt' });
    await decisionApi.replay({ ...scope, snapshot_id: 's1', model_version: 'm1',
      scenario_id: 'base', expected_hash: 'hash' });
    await decisionApi.engineeringValidation({ ...scope, snapshot_id: 's1', model_version: 'm1' });
    expect(fetchMock.mock.calls.map(([url]) => url)).toEqual([
      '/api/decision/catalog?tenant_id=tenant-a&acl=private',
      '/api/decision/synthetic-pilot', '/api/decision/synthetic-pilot/lifecycle',
      '/api/decision/compare', '/api/decision/replay', '/api/decision/engineering-validation',
    ]);
    expect(fetchMock.mock.calls.slice(1).map(([, init]) => JSON.parse((init as RequestInit).body as string))).toEqual([
      { ...scope, seed: 7, days: 21 },
      { ...scope, seed: 7, days: 21, kind: 'acl_lost' },
      { ...scope, snapshot_id: 's1', model_version: 'm1', baseline_scenario_id: 'base', alternative_scenario_id: 'alt' },
      { ...scope, snapshot_id: 's1', model_version: 'm1', scenario_id: 'base', expected_hash: 'hash' },
      { ...scope, snapshot_id: 's1', model_version: 'm1' },
    ]);
  });

  it('posts the exact policy sweep resource plan and paired sensitivity fields', async () => {
    const fetchMock = vi.fn()
      .mockResolvedValueOnce({ ok: true, json: async () => ({ report: { status: 'exploratory' } }) })
      .mockResolvedValueOnce({ ok: true, json: async () => ({ report: { status: 'exploratory' } }) });
    vi.stubGlobal('fetch', fetchMock);
    const selection = { tenant_id: 'tenant-a', acl: 'private', snapshot_id: 's1', model_version: 'm1',
      baseline_scenario_id: 'base', alternative_scenario_id: 'alt' };
    const resourcePlan = { available_agents_by_day: [4, 4], max_added_agents_per_day: 2,
      max_total_agent_days: 20, max_staff_cost_cents: 50000, max_final_backlog: 100,
      service_capacity_band: { min: 2, max: 12 } };
    await decisionApi.policySweep({ ...selection, resource_plan: resourcePlan });
    await decisionApi.sensitivity({ ...selection, runs: 100, arrival_delta: 2, capacity_min: 2,
      capacity_max: 12, max_final_backlog: 100, max_staff_cost_cents: 50000 });
    expect(fetchMock.mock.calls.map(([url]) => url)).toEqual([
      '/api/decision/policy-sweep', '/api/decision/sensitivity',
    ]);
    expect(fetchMock.mock.calls.map(([, init]) => JSON.parse((init as RequestInit).body as string))).toEqual([
      { ...selection, resource_plan: resourcePlan },
      { ...selection, runs: 100, arrival_delta: 2, capacity_min: 2, capacity_max: 12,
        max_final_backlog: 100, max_staff_cost_cents: 50000 },
    ]);
  });

  it('posts scoped empirical sampling and optional review constraints as one immutable request', async () => {
    const fetchMock = vi.fn().mockResolvedValue({ ok: true, json: async () => ({ status: 'synthetic_only_exploratory' }) });
    vi.stubGlobal('fetch', fetchMock);
    const input = { tenant_id: 'tenant-a', acl: 'private', snapshot_id: 'synthetic-support-47-35',
      model_version: 'model-47', baseline_scenario_id: 'base', alternative_scenario_id: 'alt',
      training_days: 14, min_saturated_days: 7, runs: 100, arrival_block_days: 7,
      sampling_mode: 'independent' as const, max_final_backlog: 105, max_staff_cost_cents: 1_050_000,
      min_sla_resolved: 525 };
    await decisionApi.empiricalResampling(input);
    expect(fetchMock).toHaveBeenCalledWith('/api/decision/empirical-resampling', expect.objectContaining({
      method: 'POST', body: JSON.stringify(input),
    }));
  });

  it('requests a fresh brief bound to a stored empirical run and policy screen', async () => {
    const fetchMock = vi.fn().mockResolvedValue({ ok: true, json: async () => ({
      brief: { exploratory_empirical: { run_id: 'run-47' } },
    }) });
    vi.stubGlobal('fetch', fetchMock);
    const input = { tenant_id: 'tenant-a', acl: 'private', snapshot_id: 'snapshot-47',
      model_version: 'model-47', baseline_scenario_id: 'base', alternative_scenario_id: 'alt',
      empirical_run_id: 'run-47', policy_screen_hash: 'screen-hash' };
    const result = await decisionApi.compare(input);
    expect(fetchMock).toHaveBeenCalledWith('/api/decision/compare', expect.objectContaining({
      method: 'POST', body: JSON.stringify(input),
    }));
    expect(result.brief.exploratory_empirical?.run_id).toBe('run-47');
  });

  it('posts an exact run digest for human inspection and scoped status checks', async () => {
    const fetchMock = vi.fn()
      .mockResolvedValueOnce({ ok: true, json: async () => ({ review: { status: 'pending' } }) })
      .mockResolvedValueOnce({ ok: true, json: async () => ({ review: { status: 'approved' } }) });
    vi.stubGlobal('fetch', fetchMock);
    const selection = { tenant_id: 'tenant-a', acl: 'private', snapshot_id: 'snapshot-1',
      model_version: 'model-1', scenario_id: 'base', expected_hash: 'abc123' };
    await decisionApi.requestPilotReview({ ...selection, summary: 'Inspect daily trajectory', ttl_seconds: 3600 });
    await decisionApi.pilotReviewStatus({ ...selection, approval_id: 'approval-1' });
    expect(fetchMock.mock.calls.map(([url]) => url)).toEqual([
      '/api/decision/pilot-review/request', '/api/decision/pilot-review/status',
    ]);
    expect(fetchMock.mock.calls.map(([, init]) => JSON.parse((init as RequestInit).body as string))).toEqual([
      { ...selection, summary: 'Inspect daily trajectory', ttl_seconds: 3600 },
      { ...selection, approval_id: 'approval-1' },
    ]);
  });
});
