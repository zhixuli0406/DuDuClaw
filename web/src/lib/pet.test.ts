import { describe, it, expect, afterEach, vi } from 'vitest';
import { isTauri, petGenerate, petActivate, startWindowDrag } from './pet';

afterEach(() => {
  delete (window as unknown as { __TAURI__?: unknown }).__TAURI__;
  vi.restoreAllMocks();
});

describe('pet bridge', () => {
  it('isTauri is false without the global', () => {
    expect(isTauri()).toBe(false);
  });

  it('command wrappers reject outside Tauri', async () => {
    await expect(petGenerate('x', 'data:image/png;base64,AAAA')).rejects.toThrow(
      /desktop app/
    );
  });

  it('forwards args to invoke inside Tauri', async () => {
    const invoke = vi.fn(async () => undefined);
    (window as unknown as { __TAURI__?: unknown }).__TAURI__ = { core: { invoke } };
    expect(isTauri()).toBe(true);
    await petActivate('kuro');
    expect(invoke).toHaveBeenCalledWith('pet_activate', { slug: 'kuro' });
  });

  it('startWindowDrag is a no-op (no throw) without a window API', async () => {
    await expect(startWindowDrag()).resolves.toBeUndefined();
  });
});
