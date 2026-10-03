import { describe, it, expect } from 'vitest';
import en from './en.json';
import zhTW from './zh-TW.json';
import jaJP from './ja-JP.json';

/**
 * v1.68 (W2): every message id exists in all three catalogues. ja-JP falls
 * back to English at runtime (i18n/index.ts), which hides a missing Japanese
 * string instead of failing — this keeps the three files in step.
 */
function missing(from: Record<string, unknown>, into: Record<string, unknown>): string[] {
  return Object.keys(from).filter((k) => !(k in into));
}

describe('i18n catalogue parity', () => {
  it('zh-TW, en and ja-JP carry the same message ids', () => {
    expect(missing(zhTW, en)).toEqual([]);
    expect(missing(en, zhTW)).toEqual([]);
    expect(missing(zhTW, jaJP)).toEqual([]);
    expect(missing(jaJP, zhTW)).toEqual([]);
  });

  it('no message is empty', () => {
    for (const [name, cat] of [['en', en], ['zh-TW', zhTW], ['ja-JP', jaJP]] as const) {
      const empty = Object.entries(cat as Record<string, unknown>)
        .filter(([, v]) => typeof v !== 'string' || v.trim() === '')
        .map(([k]) => `${name}:${k}`);
      expect(empty).toEqual([]);
    }
  });
});
