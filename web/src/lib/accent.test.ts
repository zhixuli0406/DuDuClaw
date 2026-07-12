import { describe, it, expect, beforeEach } from 'vitest';
import { deriveAccentVars, applyAccent } from './accent';

const STYLE_ID = 'brand-accent';

describe('deriveAccentVars', () => {
  it('derives a light/dark ramp for a valid #rrggbb hex', () => {
    const vars = deriveAccentVars('#f59e0b');
    expect(vars).not.toBeNull();
    // Base hex maps to the 500 shades verbatim.
    expect(vars!['--color-primary-500']).toBe('#f59e0b');
    expect(vars!['--color-accent-500']).toBe('#f59e0b');
    // All five tokens present.
    expect(Object.keys(vars!)).toEqual([
      '--color-primary-400',
      '--color-primary-500',
      '--color-primary-600',
      '--color-accent-400',
      '--color-accent-500',
    ]);
    // Every derived value is a valid hex.
    for (const v of Object.values(vars!)) {
      expect(v).toMatch(/^#[0-9a-f]{6}$/);
    }
  });

  it('returns null for an invalid hex (no injection)', () => {
    expect(deriveAccentVars('nope')).toBeNull();
    expect(deriveAccentVars('#fff')).toBeNull(); // shorthand not accepted
    expect(deriveAccentVars('#12345g')).toBeNull();
    expect(deriveAccentVars('')).toBeNull();
  });
});

describe('applyAccent', () => {
  beforeEach(() => {
    document.getElementById(STYLE_ID)?.remove();
  });

  it('injects a <style id="brand-accent"> for a valid hex', () => {
    applyAccent('#3b82f6');
    const el = document.getElementById(STYLE_ID);
    expect(el).not.toBeNull();
    expect(el!.tagName).toBe('STYLE');
    expect(el!.textContent).toContain('--color-primary-500:#3b82f6;');
  });

  it('does not inject for an invalid hex', () => {
    applyAccent('bogus');
    expect(document.getElementById(STYLE_ID)).toBeNull();
  });

  it('removes the style tag when cleared (null)', () => {
    applyAccent('#3b82f6');
    expect(document.getElementById(STYLE_ID)).not.toBeNull();
    applyAccent(null);
    expect(document.getElementById(STYLE_ID)).toBeNull();
  });
});
