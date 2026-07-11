import { describe, it, expect, beforeEach } from 'vitest';
import { loadTtsEnabled, saveTtsEnabled, TTS_TOGGLE_KEY } from './tts-playback';

/** Minimal in-memory Storage stub. */
function memoryStorage(): Storage {
  const map = new Map<string, string>();
  return {
    getItem: (k) => map.get(k) ?? null,
    setItem: (k, v) => void map.set(k, String(v)),
    removeItem: (k) => void map.delete(k),
    clear: () => map.clear(),
    key: (i) => Array.from(map.keys())[i] ?? null,
    get length() {
      return map.size;
    },
  } as Storage;
}

describe('tts-playback persistence', () => {
  let store: Storage;
  beforeEach(() => {
    store = memoryStorage();
  });

  it('defaults to off when unset', () => {
    expect(loadTtsEnabled(store)).toBe(false);
  });

  it('round-trips true and false', () => {
    saveTtsEnabled(true, store);
    expect(store.getItem(TTS_TOGGLE_KEY)).toBe('1');
    expect(loadTtsEnabled(store)).toBe(true);

    saveTtsEnabled(false, store);
    expect(store.getItem(TTS_TOGGLE_KEY)).toBe('0');
    expect(loadTtsEnabled(store)).toBe(false);
  });

  it('treats any non-"1" value as off', () => {
    store.setItem(TTS_TOGGLE_KEY, 'yes');
    expect(loadTtsEnabled(store)).toBe(false);
  });
});
