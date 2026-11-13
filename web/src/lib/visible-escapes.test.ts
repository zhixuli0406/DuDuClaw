import { describe, expect, it } from 'vitest';
import { escapeCodePoint, isHiddenOrControl, visibleEscapes } from './visible-escapes';

describe('visibleEscapes', () => {
  it('leaves ordinary text, CJK, line feeds and tabs alone', () => {
    const input = '請處理新任務：季報\n\tline two — done';
    expect(visibleEscapes(input)).toEqual({ text: input, escaped: 0 });
  });

  it('escapes bidi controls', () => {
    const rlo = 'invoice‮txt.exe';
    expect(visibleEscapes(rlo)).toEqual({ text: 'invoice\\u{202E}txt.exe', escaped: 1 });
    for (const cp of [0x061c, 0x200e, 0x200f, 0x202a, 0x202b, 0x202c, 0x202d, 0x2066, 0x2067, 0x2068, 0x2069]) {
      expect(visibleEscapes(`a${String.fromCodePoint(cp)}b`).text).toBe(`a${escapeCodePoint(cp)}b`);
    }
  });

  it('escapes C0 (except LF/TAB), DEL and C1 control characters, including CR', () => {
    expect(visibleEscapes('a\u0000b\u001bc\rd\u007fe\u0085f\u009fg')).toEqual({
      text: 'a\\u{0000}b\\u{001B}c\\u{000D}d\\u{007F}e\\u{0085}f\\u{009F}g',
      escaped: 6,
    });
  });

  it('escapes zero-width and invisible characters, tags and variation selectors', () => {
    const hidden = [0x00ad, 0x200b, 0x200c, 0x200d, 0x2060, 0x2063, 0xfeff, 0x3164, 0xfe00, 0xe0041, 0xe0100];
    for (const cp of hidden) {
      expect(isHiddenOrControl(cp)).toBe(true);
      const { text, escaped } = visibleEscapes(`x${String.fromCodePoint(cp)}y`);
      expect(text).toBe(`x${escapeCodePoint(cp)}y`);
      expect(escaped).toBe(1);
    }
    // A run of tag characters (an invisible "instruction") is fully exposed.
    const smuggled = 'ok' + Array.from('RUN', (c) => String.fromCodePoint(0xe0000 + c.charCodeAt(0))).join('');
    expect(visibleEscapes(smuggled)).toEqual({ text: 'ok\\u{E0052}\\u{E0055}\\u{E004E}', escaped: 3 });
  });

  it('keeps the emoji presentation selector after a pictograph only', () => {
    expect(visibleEscapes('⚠️ 注意')).toEqual({ text: '⚠️ 注意', escaped: 0 });
    expect(visibleEscapes('a️')).toEqual({ text: 'a\\u{FE0F}', escaped: 1 });
  });

  it('formats escapes with at least four upper-case hex digits', () => {
    expect(escapeCodePoint(0x9)).toBe('\\u{0009}');
    expect(escapeCodePoint(0x202e)).toBe('\\u{202E}');
    expect(escapeCodePoint(0xe0041)).toBe('\\u{E0041}');
  });
});
