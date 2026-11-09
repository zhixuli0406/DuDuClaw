import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  BreakGlassCancelled,
  forgetBreakGlass,
  isBreakGlassRefusal,
  registerBreakGlassPrompt,
  withBreakGlass,
} from './break-glass';

const TOKEN = Array(32).fill(7);
/** `personal_data_gate::STEP_UP_REQUIRED` (abridged; it names the field). */
const REFUSAL = '這是個人專屬員工的私人資料……並在請求中帶入驗證後的 step_up_csrf；每次讀取都會寫入稽核紀錄。';

let unregister: (() => void) | null = null;
afterEach(() => {
  unregister?.();
  unregister = null;
  forgetBreakGlass();
});

describe('isBreakGlassRefusal', () => {
  it('recognises both gateway texts and a structured code', () => {
    expect(isBreakGlassRefusal(REFUSAL)).toBe(true);
    expect(isBreakGlassRefusal(new Error(REFUSAL))).toBe(true);
    expect(isBreakGlassRefusal({ code: 'STEP_UP_REQUIRED', message: '請重新驗證' })).toBe(true);
    expect(isBreakGlassRefusal('permission denied')).toBe(false);
    expect(isBreakGlassRefusal(null)).toBe(false);
  });
});

describe('withBreakGlass', () => {
  it('passes non-refusal errors through untouched and never prompts', async () => {
    const prompt = vi.fn();
    unregister = registerBreakGlassPrompt(prompt);
    await expect(withBreakGlass(() => Promise.reject(new Error('boom')), { purpose: 'read' })).rejects.toThrow('boom');
    expect(prompt).not.toHaveBeenCalled();
  });

  it('rethrows the refusal when no prompt is mounted', async () => {
    await expect(withBreakGlass(() => Promise.reject(REFUSAL), { purpose: 'decide' })).rejects.toBe(REFUSAL);
  });

  it('asks once and retries the same call with the token', async () => {
    const prompt = vi.fn().mockResolvedValue(TOKEN);
    unregister = registerBreakGlassPrompt(prompt);
    const run = vi.fn().mockRejectedValueOnce(REFUSAL).mockResolvedValueOnce('ok');
    await expect(withBreakGlass(run, { purpose: 'decide' })).resolves.toBe('ok');
    expect(prompt).toHaveBeenCalledWith({ purpose: 'decide' });
    expect(run).toHaveBeenNthCalledWith(1, undefined);
    expect(run).toHaveBeenNthCalledWith(2, TOKEN);
  });

  it('throws BreakGlassCancelled with plain text when the Admin closes the prompt', async () => {
    unregister = registerBreakGlassPrompt(() => Promise.resolve(null));
    const err = await withBreakGlass(() => Promise.reject(REFUSAL), { purpose: 'read' }).catch((e) => e);
    expect(err).toBeInstanceOf(BreakGlassCancelled);
    expect(String(err.message)).not.toContain('step_up_csrf');
  });

  it('reuses a token for the same employee and stays quiet in the background', async () => {
    const prompt = vi.fn().mockResolvedValue(TOKEN);
    unregister = registerBreakGlassPrompt(prompt);
    const first = vi.fn().mockRejectedValueOnce(REFUSAL).mockResolvedValue('a');
    await withBreakGlass(first, { purpose: 'read', scope: 'alice-bot' });
    const second = vi.fn().mockResolvedValue('b');
    await withBreakGlass(second, { purpose: 'read', scope: 'alice-bot' });
    expect(second).toHaveBeenCalledWith(TOKEN);
    expect(prompt).toHaveBeenCalledTimes(1);
    // Another employee, background refresh: never prompts.
    await expect(withBreakGlass(() => Promise.reject(REFUSAL), { purpose: 'read', scope: 'bob-bot', interactive: false }))
      .rejects.toBe(REFUSAL);
    expect(prompt).toHaveBeenCalledTimes(1);
  });

  it('does not ask again right after a cancel for the same employee', async () => {
    const prompt = vi.fn().mockResolvedValue(null);
    unregister = registerBreakGlassPrompt(prompt);
    await withBreakGlass(() => Promise.reject(REFUSAL), { purpose: 'read', scope: 'carol-bot' }).catch(() => {});
    await expect(withBreakGlass(() => Promise.reject(REFUSAL), { purpose: 'read', scope: 'carol-bot' })).rejects.toBe(REFUSAL);
    expect(prompt).toHaveBeenCalledTimes(1);
  });
});
