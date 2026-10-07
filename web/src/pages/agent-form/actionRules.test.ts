import { describe, expect, it } from 'vitest';
import type { ActionRule } from '@/lib/api';
import { effectVerdict, setEffectVerdict, setToolRules, toolRules } from './actionRules';

describe('action rules helpers', () => {
  const base: ActionRule[] = [
    { effect: 'send', verdict: 'ask' },
    { tool: 'mail_send', verdict: 'block' },
    { effect: 'send', verdict: 'block' },
  ];

  it('shows the strictest verdict for an effect', () => {
    expect(effectVerdict(base, 'send')).toBe('block');
    expect(effectVerdict(base, 'modify')).toBe('');
  });

  it('replaces every rule of one effect and keeps tool rules', () => {
    const next = setEffectVerdict(base, 'send', 'allow');
    expect(next).toEqual([{ effect: 'send', verdict: 'allow' }, { tool: 'mail_send', verdict: 'block' }]);
    expect(setEffectVerdict(next, 'send', '')).toEqual([{ tool: 'mail_send', verdict: 'block' }]);
  });

  it('orders effect rules by class', () => {
    const next = setEffectVerdict(setEffectVerdict([], 'admin', 'block'), 'read', 'allow');
    expect(next.map((r) => r.effect)).toEqual(['read', 'admin']);
  });

  it('edits tool overrides without touching effect rules', () => {
    expect(toolRules(base)).toEqual([{ tool: 'mail_send', verdict: 'block' }]);
    const next = setToolRules(base, [{ tool: 'wiki_write', verdict: 'ask' }]);
    expect(next).toEqual([
      { effect: 'send', verdict: 'ask' },
      { effect: 'send', verdict: 'block' },
      { tool: 'wiki_write', verdict: 'ask' },
    ]);
  });
});
