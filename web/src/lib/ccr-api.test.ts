import { afterEach, describe, expect, it, vi } from 'vitest';
import { useAuthStore } from '@/stores/auth-store';
import { ccrApi } from './ccr-api';

afterEach(() => {
  vi.unstubAllGlobals();
  useAuthStore.setState({ jwt: null } as never);
});

describe('ccrApi.dashboard', () => {
  it('requests an exact tenant with admin bearer auth and returns content-free counters', async () => {
    useAuthStore.setState({ jwt: 'admin-token' } as never);
    const snapshot = {
      status: 'ready',
      entries: { eligible_retained: 2, expired_retained: 0, revoked_retained: 0, declared_original_bytes_eligible: 4096 },
      revocations: { source_calls: 1, scopes: 0, artifact_versions: 1 },
      retrieval_audit: { grants: 3, refusals: 2, returned_bytes_granted: 512, window_max_rows: 10000 },
      compression_metrics: { available: true, reason: null, window_max_rows: 10000,
        observed_loops: 1, provider_rounds: 2, usage_reported_rounds: 1,
        input_tokens: 10, output_tokens: 5, cache_read_tokens: 0,
        cache_write_tokens: 0, reasoning_tokens: 0,
        ccr_compressed_results: 1, ccr_original_bytes: 4096, ccr_delivered_bytes: 512,
        ccr_find_attempts: 0, ccr_find_hits: 0, ccr_find_misses: 0,
        ccr_retrieve_attempts: 0, ccr_retrieve_successes: 0, ccr_retrieve_misses: 0,
        ccr_retrieved_bytes: 0, p95_elapsed_millis: 42 },
    };
    const fetchMock = vi.fn().mockResolvedValue({ ok: true, json: async () => snapshot });
    vi.stubGlobal('fetch', fetchMock);
    expect(await ccrApi.dashboard('tenant a')).toEqual(snapshot);
    expect(fetchMock).toHaveBeenCalledWith('/api/ccr/dashboard?tenant_id=tenant+a', {
      method: 'GET', headers: { Authorization: 'Bearer admin-token' },
    });
  });

  it('surfaces a refused admin request without exposing a response body as success', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue({ ok: false, status: 403,
      json: async () => ({ error: 'admin required' }) }));
    await expect(ccrApi.dashboard('local')).rejects.toThrow('admin required');
  });
});

describe('ccrApi.replay', () => {
  it('posts the selected tenant and exact JSON only after the caller invokes replay', async () => {
    useAuthStore.setState({ jwt: 'admin-token' } as never);
    const result = { evidence_origin: 'synthetic', report: { arms: [] } };
    const fetchMock = vi.fn().mockResolvedValue({ ok: true, json: async () => result });
    vi.stubGlobal('fetch', fetchMock);
    const evidence = '{"version":"ccr-recorded-evidence-v1","tasks":[]}';
    expect(await ccrApi.replay('tenant a', evidence)).toEqual(result);
    expect(fetchMock).toHaveBeenCalledWith('/api/ccr/replay?tenant_id=tenant+a', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', Authorization: 'Bearer admin-token' },
      body: evidence,
    });
  });

  it('does not echo a rejected server body that might contain evidence text', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue({ ok: false, status: 400,
      json: async () => ({ error: 'private task label' }) }));
    await expect(ccrApi.replay('local', '{}')).rejects.toThrow('HTTP 400');
    await expect(ccrApi.replay('local', '{}')).rejects.not.toThrow('private task label');
  });
});
