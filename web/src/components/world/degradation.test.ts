import { describe, it, expect } from 'vitest';
import { resolveStageMode, type StageModeInputs } from './stage-mode';
import { worldObjectRoute } from './interactions';
import { truncateGraphemes } from './text';

const base: StageModeInputs = {
  prefersReducedMotion: false,
  webglAvailable: true,
  userChoice: null,
  isMobile: false,
};

describe('resolveStageMode (degradation chain §8.4)', () => {
  it('desktop, motion allowed, WebGL, no choice → stage', () => {
    expect(resolveStageMode(base)).toBe('stage');
  });

  it('reduced-motion forces static regardless of everything else', () => {
    expect(resolveStageMode({ ...base, prefersReducedMotion: true })).toBe('static');
    expect(resolveStageMode({ ...base, prefersReducedMotion: true, userChoice: 'stage' })).toBe('static');
  });

  it('no WebGL forces static', () => {
    expect(resolveStageMode({ ...base, webglAvailable: false })).toBe('static');
    expect(resolveStageMode({ ...base, webglAvailable: false, userChoice: 'stage' })).toBe('static');
  });

  it('mobile defaults to static (list) when the user has not chosen', () => {
    expect(resolveStageMode({ ...base, isMobile: true })).toBe('static');
  });

  it('an explicit stage choice overrides the mobile default', () => {
    expect(resolveStageMode({ ...base, isMobile: true, userChoice: 'stage' })).toBe('stage');
  });

  it('an explicit list choice forces static on desktop', () => {
    expect(resolveStageMode({ ...base, userChoice: 'list' })).toBe('static');
  });
});

describe('worldObjectRoute (T8.4)', () => {
  const mgr = { isManager: true };
  const emp = { isManager: false };

  it('bulletin → inbox, whiteboard → tasks (any role)', () => {
    expect(worldObjectRoute('bulletin', emp)).toBe('/inbox');
    expect(worldObjectRoute('whiteboard', emp)).toBe('/tasks');
  });

  it('door/vault are manager-gated', () => {
    expect(worldObjectRoute('door', mgr)).toBe('/manage/channels');
    expect(worldObjectRoute('vault', mgr)).toBe('/manage/billing');
    expect(worldObjectRoute('door', emp)).toBeNull();
    expect(worldObjectRoute('vault', emp)).toBeNull();
  });

  it('agent → its detail route (id-encoded); null without an id', () => {
    expect(worldObjectRoute('agent', emp, 'ops bot')).toBe('/agents/ops%20bot');
    expect(worldObjectRoute('agent', emp)).toBeNull();
  });

  it('coffee is purely decorative (no route)', () => {
    expect(worldObjectRoute('coffee', mgr)).toBeNull();
  });
});

describe('truncateGraphemes (CJK-safe)', () => {
  it('leaves short strings untouched', () => {
    expect(truncateGraphemes('hi', 8)).toBe('hi');
  });

  it('truncates by grapheme and appends an ellipsis', () => {
    const out = truncateGraphemes('回覆中回覆中回覆中回覆中', 4);
    expect(out).toBe('回覆中回…');
  });

  it('never splits a multi-byte codepoint mid-way', () => {
    const out = truncateGraphemes('😀😀😀😀😀', 2);
    // Two whole emoji + ellipsis — not a broken surrogate half.
    expect(out).toBe('😀😀…');
  });

  it('handles a zero budget', () => {
    expect(truncateGraphemes('anything', 0)).toBe('');
  });
});
