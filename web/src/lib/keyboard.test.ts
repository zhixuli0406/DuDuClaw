import { describe, it, expect } from 'vitest';
import { isImeComposing } from './keyboard';

describe('isImeComposing (CJK IME Enter guard)', () => {
  it('is true for a React event mid-composition (Chrome/Firefox)', () => {
    // Chrome/Firefox: keydown fires with nativeEvent.isComposing === true.
    expect(isImeComposing({ nativeEvent: { isComposing: true }, keyCode: 13 })).toBe(true);
  });

  it('is true for a native event mid-composition', () => {
    expect(isImeComposing({ isComposing: true, keyCode: 13 })).toBe(true);
  });

  it('is true for the Safari post-compositionend keyCode 229 sentinel', () => {
    // Safari/WebKit: keydown fires AFTER compositionend, isComposing already
    // false, but keyCode is the 229 "processing" sentinel.
    expect(isImeComposing({ nativeEvent: { isComposing: false }, keyCode: 229 })).toBe(true);
  });

  it('is false for a plain Enter with no composition', () => {
    expect(isImeComposing({ nativeEvent: { isComposing: false }, keyCode: 13 })).toBe(false);
    expect(isImeComposing({ keyCode: 13 })).toBe(false);
    expect(isImeComposing({})).toBe(false);
  });
});
