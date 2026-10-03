import { describe, it, expect } from 'vitest';
import {
  changedPayload,
  normalizeRestartRequired,
  parseTomlSubset,
  tomlBool,
  tomlNum,
  tomlSecretSet,
  tomlStr,
  tomlStrList,
} from './config-toml';

describe('parseTomlSubset', () => {
  const text = `# top comment
[general]
name = "Office # not a comment"

[container.sandbox]
memory_bytes = 4_294_967_296
when_unavailable = "fail" # trailing comment

[files]
allowed_roots = [
  "/srv/exports",
  "D:\\\\data",
]

[[tick.sources]]
id = "skipped"

[team.roles.utility]
runtime = "codex"

[webchat]
widget_key = "********"
public_widget = true
`;
  const t = parseTomlSubset(text);

  it('reads strings (with # inside quotes), numbers with underscores and bools', () => {
    expect(tomlStr(t, 'general', 'name', '')).toBe('Office # not a comment');
    expect(tomlNum(t, 'container.sandbox', 'memory_bytes', 0)).toBe(4294967296);
    expect(tomlStr(t, 'container.sandbox', 'when_unavailable', '')).toBe('fail');
    expect(tomlBool(t, 'webchat', 'public_widget', false)).toBe(true);
  });

  it('reads multi-line arrays and dotted table headers', () => {
    expect(tomlStrList(t, 'files', 'allowed_roots')).toEqual(['/srv/exports', 'D:\\data']);
    expect(tomlStr(t, 'team.roles.utility', 'runtime', '')).toBe('codex');
  });

  it('skips array-of-tables and reports masked secrets as set', () => {
    expect(t['tick.sources']).toBeUndefined();
    expect(tomlSecretSet(t, 'webchat', 'widget_key')).toBe(true);
    expect(tomlSecretSet(t, 'webchat', 'missing')).toBe(false);
  });

  it('falls back when a key is absent or of the wrong type', () => {
    expect(tomlBool(t, 'general', 'name', true)).toBe(true);
    expect(tomlNum(t, 'nope', 'x', 7)).toBe(7);
  });
});

describe('changedPayload', () => {
  it('nests only the changed dotted keys', () => {
    const initial = {
      'takeover.enabled': false,
      'takeover.duration_minutes': 60,
      'container.sandbox.pids': 128,
      name: 'a',
    };
    const current = { ...initial, 'takeover.enabled': true, 'container.sandbox.pids': 256 };
    expect(changedPayload(initial, current)).toEqual({
      takeover: { enabled: true },
      container: { sandbox: { pids: 256 } },
    });
  });

  it('compares arrays and maps by value', () => {
    const initial = { 'files.allowed_roots': ['/a'], 'belief.tick_subject_map': { a: 'b' } };
    expect(changedPayload(initial, { 'files.allowed_roots': ['/a'], 'belief.tick_subject_map': { a: 'b' } })).toEqual({});
    expect(changedPayload(initial, { ...initial, 'files.allowed_roots': ['/a', '/b'] })).toEqual({
      files: { allowed_roots: ['/a', '/b'] },
    });
  });
});

describe('normalizeRestartRequired', () => {
  it('handles the three response shapes', () => {
    expect(normalizeRestartRequired(['tick', '', 3], 'x')).toEqual(['tick']);
    expect(normalizeRestartRequired(true, 'tick.sources')).toEqual(['tick.sources']);
    expect(normalizeRestartRequired(false, 'x')).toEqual([]);
    expect(normalizeRestartRequired(undefined, 'x')).toEqual([]);
  });
});
