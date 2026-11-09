import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { DuDuClawClient } from './ws-client';
import { forgetBreakGlass, installStepUpRetry, registerBreakGlassPrompt, withBreakGlass } from './break-glass';

const TOKEN = Array(32).fill(9);
const REFUSAL = { code: 'RPC_ERROR', message: '需要再驗證身分，並在請求中帶入驗證後的 step_up_csrf' };

type Internals = { callOnce: (...args: unknown[]) => Promise<unknown> };

let unregister: (() => void) | null = null;
let uninstall: (() => void) | null = null;
beforeEach(() => {
  uninstall = installStepUpRetry();
});
afterEach(() => {
  unregister?.();
  unregister = null;
  uninstall?.();
  uninstall = null;
  forgetBreakGlass();
  vi.restoreAllMocks();
});

function stubTransport(impl: (method: string, params: Record<string, unknown>) => Promise<unknown>) {
  return vi
    .spyOn(DuDuClawClient.prototype as unknown as Internals, 'callOnce')
    .mockImplementation((method, params) => impl(method as string, params as Record<string, unknown>));
}

describe('generic step-up retry in the RPC client', () => {
  it('prompts once for any method and retries once with step_up_csrf', async () => {
    const seen: Array<Record<string, unknown>> = [];
    stubTransport(async (_m, params) => {
      seen.push(params);
      if (!('step_up_csrf' in params)) throw REFUSAL;
      return { ok: true };
    });
    const prompt = vi.fn().mockResolvedValue(TOKEN);
    unregister = registerBreakGlassPrompt(prompt);

    await expect(new DuDuClawClient().call('wiki.list', { agent_id: 'a' })).resolves.toEqual({ ok: true });
    expect(prompt).toHaveBeenCalledTimes(1);
    expect(seen).toEqual([{ agent_id: 'a' }, { agent_id: 'a', step_up_csrf: TOKEN }]);
  });

  it('surfaces the original error when the prompt is cancelled', async () => {
    const spy = stubTransport(async () => { throw REFUSAL; });
    unregister = registerBreakGlassPrompt(vi.fn().mockResolvedValue(null));
    await expect(new DuDuClawClient().call('wiki.list', {})).rejects.toBe(REFUSAL);
    expect(spy).toHaveBeenCalledTimes(1);
  });

  it('never retries more than once, even if the retry is refused again', async () => {
    const spy = stubTransport(async () => { throw REFUSAL; });
    const prompt = vi.fn().mockResolvedValue(TOKEN);
    unregister = registerBreakGlassPrompt(prompt);
    await expect(new DuDuClawClient().call('wiki.list', {})).rejects.toBe(REFUSAL);
    expect(spy).toHaveBeenCalledTimes(2);
    expect(prompt).toHaveBeenCalledTimes(1);
  });

  it('does not prompt for other errors or when the call already carried a token', async () => {
    const prompt = vi.fn().mockResolvedValue(TOKEN);
    unregister = registerBreakGlassPrompt(prompt);
    stubTransport(async () => { throw new Error('boom'); });
    await expect(new DuDuClawClient().call('wiki.list', {})).rejects.toThrow('boom');
    vi.restoreAllMocks();
    stubTransport(async () => { throw REFUSAL; });
    await expect(new DuDuClawClient().call('mail.decide', { step_up_csrf: TOKEN })).rejects.toBe(REFUSAL);
    expect(prompt).not.toHaveBeenCalled();
  });

  it('does not double-prompt a call site that already uses withBreakGlass', async () => {
    stubTransport(async (_m, params) => {
      if (!('step_up_csrf' in params)) throw REFUSAL;
      return 'done';
    });
    const prompt = vi.fn().mockResolvedValue(TOKEN);
    unregister = registerBreakGlassPrompt(prompt);
    const client = new DuDuClawClient();
    const result = await withBreakGlass(
      (token) => client.call('mail.decide', token ? { step_up_csrf: token } : {}),
      { purpose: 'decide' },
    );
    expect(result).toBe('done');
    expect(prompt).toHaveBeenCalledTimes(1);
  });
});
