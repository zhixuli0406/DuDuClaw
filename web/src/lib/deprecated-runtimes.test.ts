import { describe, expect, it } from 'vitest';
import { isRemovedRuntime, REMOVED_RUNTIMES } from './deprecated-runtimes';

describe('removed runtimes', () => {
  it('lists exactly the removed catalog ids', () => {
    expect(REMOVED_RUNTIMES).toEqual(['gemini', 'antigravity', 'agy']);
  });

  it.each(['gemini', 'antigravity', 'agy'])('recognizes %s', (id) => {
    expect(isRemovedRuntime(id)).toBe(true);
  });

  it.each([null, undefined, '', 'claude', 'codex', 'grok', 'Gemini', 'Antigravity', 'AGY', ' agy '])('does not treat %s as a removed catalog id', (id) => {
    expect(isRemovedRuntime(id)).toBe(false);
  });
});
