import { describe, it, expect } from 'vitest';
import { withParam, parseEnumParam } from './url-params';

describe('withParam', () => {
  it('sets a param on an empty params object', () => {
    const next = withParam(new URLSearchParams(), 'agent', 'nova');
    expect(next.get('agent')).toBe('nova');
  });

  it('deletes the param when value is empty string', () => {
    const prev = new URLSearchParams('agent=nova');
    const next = withParam(prev, 'agent', '');
    expect(next.has('agent')).toBe(false);
  });

  it('deletes the param when value is null', () => {
    const prev = new URLSearchParams('agent=nova');
    const next = withParam(prev, 'agent', null);
    expect(next.has('agent')).toBe(false);
  });

  it('deletes the param when value is undefined', () => {
    const prev = new URLSearchParams('agent=nova');
    const next = withParam(prev, 'agent', undefined);
    expect(next.has('agent')).toBe(false);
  });

  it('preserves other existing params (merge, not replace)', () => {
    const prev = new URLSearchParams('tab=memories&q=hello');
    const next = withParam(prev, 'cat', 'work');
    expect(next.get('tab')).toBe('memories');
    expect(next.get('q')).toBe('hello');
    expect(next.get('cat')).toBe('work');
  });

  it('overwrites an existing value for the same key', () => {
    const prev = new URLSearchParams('priority=low');
    const next = withParam(prev, 'priority', 'high');
    expect(next.get('priority')).toBe('high');
  });

  it('does not mutate the input params', () => {
    const prev = new URLSearchParams('agent=nova');
    withParam(prev, 'agent', 'agnes');
    expect(prev.get('agent')).toBe('nova');
  });
});

describe('parseEnumParam', () => {
  const PRIORITIES = ['low', 'medium', 'high', 'urgent'] as const;

  it('returns the value when it is in the allowed set', () => {
    expect(parseEnumParam('high', PRIORITIES)).toBe('high');
  });

  it('returns null for a value outside the allowed set', () => {
    expect(parseEnumParam('urgentish', PRIORITIES)).toBeNull();
  });

  it('returns null for null input (missing param)', () => {
    expect(parseEnumParam(null, PRIORITIES)).toBeNull();
  });

  it('returns null for empty string (not a valid member)', () => {
    expect(parseEnumParam('', PRIORITIES)).toBeNull();
  });

  it('is case-sensitive — no coercion of near-misses', () => {
    expect(parseEnumParam('High', PRIORITIES)).toBeNull();
  });
});
