import { describe, it, expect } from 'vitest';
import en from './en.json';
import zhTW from './zh-TW.json';
import jaJP from './ja-JP.json';

/**
 * Every message id the workflow review UI can ask for must exist in all three
 * catalogues. Ids are collected from the source: literal ids in
 * `text('…')` / `id: '…'`, plus the families that are built from a closed set
 * of values (reason codes, run statuses, fixture kinds, …).
 */
const sources = import.meta.glob<string>(
  ['../components/workflow/*.{ts,tsx}', '!../components/workflow/*.test.*', '../components/task/TaskArtifactsPanel.tsx'],
  { query: '?raw', import: 'default', eager: true },
);
const literal = /(?:text\(|id:\s*)['"`](workflow\.[A-Za-z0-9_.]+)['"`]/g;
const families: Record<string, string[]> = {
  'workflow.evidence.': ['self_report', 'test', 'operator', 'recorded'],
  'workflow.integrity.': ['current', 'stale', 'unverified'],
  'workflow.fixture.': ['normal', 'empty', 'expired', 'injection', 'missing_permission'],
  'workflow.runStatus.': ['pending', 'running', 'waiting_approval', 'needs_input', 'succeeded', 'failed', 'uncertain', 'cancelled', 'blocked', 'other'],
  'workflow.outcome.': ['matched', 'mismatched', 'not_evaluated'],
  'workflow.activation.state.': ['prepared', 'active', 'suspended', 'expired', 'revoking', 'revoked', 'other'],
  'workflow.error.': ['generic', 'permission', 'stale', 'fixtureMissing', 'fixtureExpired', 'fixtureMismatch', 'approvalPending', 'exists', 'identity'],
  'workflow.confirm.': ['accept', 'run', 'request', 'commit', 'revoke'].flatMap((k) => ['title', 'message', 'confirm'].map((p) => `${k}.${p}`)),
};

describe('workflow i18n coverage', () => {
  const ids = new Set<string>();
  for (const src of Object.values(sources)) for (const m of src.matchAll(literal)) ids.add(m[1]);
  for (const [prefix, tails] of Object.entries(families)) for (const t of tails) ids.add(prefix + t);
  const reasonSrc = sources['../components/workflow/workflow-text.ts'];
  const codes = [...reasonSrc.slice(reasonSrc.indexOf('REASON_CODES'), reasonSrc.indexOf(']);')).matchAll(/'([a-z_]+)'/g)].map((m) => m[1]);
  for (const c of codes) ids.add(`workflow.reason.${c}`);
  ids.add('workflow.reason.unknown');

  it('found a realistic number of ids', () => { expect(ids.size).toBeGreaterThan(100); expect(codes.length).toBeGreaterThan(10); });
  it.each([['zh-TW', zhTW], ['en', en], ['ja-JP', jaJP]] as const)('%s defines every workflow id', (_name, cat) => {
    const missing = [...ids].filter((id) => !(id in (cat as Record<string, string>)));
    expect(missing).toEqual([]);
  });
  it('the three catalogues carry the same workflow keys', () => {
    const keys = (c: Record<string, unknown>) => Object.keys(c).filter((k) => k.startsWith('workflow.')).sort();
    expect(keys(zhTW)).toEqual(keys(en));
    expect(keys(jaJP)).toEqual(keys(en));
  });
  it('keeps placeholders identical across languages', () => {
    const ph = (s: string) => [...s.matchAll(/\{(\w+)\}/g)].map((m) => m[1]).sort().join(',');
    for (const k of Object.keys(en).filter((k) => k.startsWith('workflow.'))) {
      expect(ph((zhTW as Record<string, string>)[k])).toBe(ph((en as Record<string, string>)[k]));
      expect(ph((jaJP as Record<string, string>)[k])).toBe(ph((en as Record<string, string>)[k]));
    }
  });
});
