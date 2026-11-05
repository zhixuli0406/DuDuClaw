import { beforeEach, describe, expect, it, vi } from 'vitest';
import { submitProtectedUpdate, newProtectedUpdateId, pendingProtectedUpdate, protectedUpdateFinished, rememberProtectedUpdate } from './protected-update';

describe('protected update readback', () => {
  beforeEach(() => sessionStorage.clear());

  it('does not treat acceptance, preparation or activation as completion', () => {
    for (const state of ['queued', 'downloading', 'prepared', 'activated'] as const) {
      expect(protectedUpdateFinished(state)).toBe(false);
    }
    for (const state of ['committed', 'failed', 'maintenance'] as const) {
      expect(protectedUpdateFinished(state)).toBe(true);
    }
  });

  it('creates canonical v4 ids without requiring the secure-context randomUUID API', () => {
    const id = newProtectedUpdateId();
    expect(id).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
    expect(newProtectedUpdateId()).not.toBe(id);
  });

  it('retains a request through page reload until an observed terminal result', () => {
    const id = '10000000-0000-4000-8000-000000000001';
    rememberProtectedUpdate(id);
    expect(pendingProtectedUpdate()).toBe(id);
    expect(pendingProtectedUpdate()).toBe(id);
    rememberProtectedUpdate(null);
    expect(pendingProtectedUpdate()).toBeNull();
  });

  it('never restores an invalid request id as a path or URL', () => {
    for (const id of ['../update', 'https://example.com/update', 'not-an-id']) {
      rememberProtectedUpdate(id);
      expect(pendingProtectedUpdate()).toBeNull();
    }
    sessionStorage.setItem('duduclaw-protected-update-request', '{"path":"/tmp/update"}');
    expect(pendingProtectedUpdate()).toBeNull();
  });
});

describe('protected update acceptance with a lost reply', () => {
  beforeEach(() => sessionStorage.clear());

  it.each([
    new Error('socket disconnected'),
    '更新服務目前無法連線，尚未確認更新結果。',
    { code: 'backend_unavailable', message: 'reply lost' },
  ])('keeps the accepted request for status reconciliation for rejection %j', async (rejection) => {
    const id = '10000000-0000-4000-8000-000000000001';
    const durableRequests = new Map<string, string>();
    const apply = vi.fn(async (requestId: string) => {
      // The root service fsyncs acceptance, then the reply is lost before the browser.
      durableRequests.set(requestId, 'queued');
      throw rejection;
    });
    const outcome = await submitProtectedUpdate(id, apply);
    expect(outcome).toEqual({ kind: 'unknown', requestId: id });
    expect(pendingProtectedUpdate()).toBe(id);
    expect(apply).toHaveBeenCalledTimes(1);
    // A page reload can query the SAME accepted request rather than starting another.
    expect(durableRequests.get(pendingProtectedUpdate()!)).toBe('queued');
  });

  it('clears tracking only for a correlated explicit terminal response', async () => {
    const id = '10000000-0000-4000-8000-000000000001';
    const outcome = await submitProtectedUpdate(id, async () => ({
      protected_request_id: id, protected_state: 'failed', success: false,
    }));
    expect(outcome.kind).toBe('known');
    expect(pendingProtectedUpdate()).toBeNull();
  });

  it('does not accept another request completion or expose backend diagnostic text', async () => {
    const id = '10000000-0000-4000-8000-000000000001';
    const outcome = await submitProtectedUpdate(id, async () => ({
      protected_request_id: '20000000-0000-4000-8000-000000000002',
      protected_state: 'committed', success: true, message: '/root/internal/secret',
    }));
    expect(outcome).toEqual({ kind: 'unknown', requestId: id });
    expect(pendingProtectedUpdate()).toBe(id);
    expect(JSON.stringify(outcome)).not.toContain('/root/internal/secret');
  });
});
