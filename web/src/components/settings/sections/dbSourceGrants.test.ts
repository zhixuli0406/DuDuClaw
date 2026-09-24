import { describe, it, expect } from 'vitest';
import {
  orderAgentsForPicker,
  buildAgentPickerRows,
  toggleAgentSelection,
  countGrantsForSource,
  grantedAgentsForSource,
  buildStaleDbSourceEntries,
  resolveAgentDisplayName,
} from './dbSourceGrants';
import type { DbSourceGrantAgent, DbSourceGrantSource } from '@/lib/api';
import en from '@/i18n/en.json';
import zhTW from '@/i18n/zh-TW.json';
import jaJP from '@/i18n/ja-JP.json';

function agent(overrides: Partial<DbSourceGrantAgent>): DbSourceGrantAgent {
  return {
    id: 'sales',
    display_name: '小美',
    role: 'worker',
    status: 'active',
    archived: false,
    ...overrides,
  };
}

function source(overrides: Partial<DbSourceGrantSource>): DbSourceGrantSource {
  return {
    name: 'crm_pg',
    label: '客戶 CRM 資料庫',
    agents: [],
    ...overrides,
  };
}

describe('orderAgentsForPicker', () => {
  it('keeps active agents in given order, archived agents last in given order', () => {
    const agents = [
      agent({ id: 'a', archived: true }),
      agent({ id: 'b', archived: false }),
      agent({ id: 'c', archived: true }),
      agent({ id: 'd', archived: false }),
    ];
    expect(orderAgentsForPicker(agents).map((a) => a.id)).toEqual(['b', 'd', 'a', 'c']);
  });

  it('is a no-op when nothing is archived', () => {
    const agents = [agent({ id: 'a' }), agent({ id: 'b' })];
    expect(orderAgentsForPicker(agents).map((a) => a.id)).toEqual(['a', 'b']);
  });

  it('handles an empty list', () => {
    expect(orderAgentsForPicker([])).toEqual([]);
  });
});

describe('buildAgentPickerRows', () => {
  it('marks selected agents checked and orders archived last', () => {
    const agents = [
      agent({ id: 'main', display_name: '小豪', archived: false }),
      agent({ id: 'retired', display_name: '小張', archived: true }),
      agent({ id: 'sales', display_name: '小美', archived: false }),
    ];
    const rows = buildAgentPickerRows(agents, ['sales', 'retired']);
    expect(rows.map((r) => r.id)).toEqual(['main', 'sales', 'retired']);
    expect(rows.find((r) => r.id === 'main')?.checked).toBe(false);
    expect(rows.find((r) => r.id === 'sales')?.checked).toBe(true);
    expect(rows.find((r) => r.id === 'retired')?.checked).toBe(true);
    expect(rows.find((r) => r.id === 'retired')?.archived).toBe(true);
  });

  it('falls back to the id when display_name is blank', () => {
    const rows = buildAgentPickerRows([agent({ id: 'nameless', display_name: '' })], []);
    expect(rows[0].displayName).toBe('nameless');
  });

  it('returns an empty list for no agents', () => {
    expect(buildAgentPickerRows([], ['x'])).toEqual([]);
  });
});

describe('toggleAgentSelection', () => {
  it('adds an absent id', () => {
    expect(toggleAgentSelection(['a'], 'b')).toEqual(['a', 'b']);
  });

  it('removes a present id', () => {
    expect(toggleAgentSelection(['a', 'b'], 'a')).toEqual(['b']);
  });

  it('never mutates the input array', () => {
    const input = ['a'];
    toggleAgentSelection(input, 'b');
    expect(input).toEqual(['a']);
  });
});

describe('countGrantsForSource / grantedAgentsForSource', () => {
  const sources = [
    source({ name: 'crm_pg', agents: ['main', 'sales'] }),
    source({ name: 'empty_db', agents: [] }),
  ];

  it('counts an existing source with grants', () => {
    expect(countGrantsForSource(sources, 'crm_pg')).toBe(2);
  });

  it('counts zero for a source with no grants', () => {
    expect(countGrantsForSource(sources, 'empty_db')).toBe(0);
  });

  it('counts zero for a source not yet in the list', () => {
    expect(countGrantsForSource(sources, 'not_loaded_yet')).toBe(0);
  });

  it('returns the granted agent ids for an existing source', () => {
    expect(grantedAgentsForSource(sources, 'crm_pg')).toEqual(['main', 'sales']);
  });

  it('returns an empty array for a source not in the list', () => {
    expect(grantedAgentsForSource(sources, 'nope')).toEqual([]);
  });
});

describe('buildStaleDbSourceEntries', () => {
  it('flattens the stale map into agent/ids pairs', () => {
    const entries = buildStaleDbSourceEntries({
      ops: ['old_source_no_longer_configured'],
      sales: ['renamed_db', 'deleted_db'],
    });
    expect(entries).toEqual([
      { agentId: 'ops', ids: ['old_source_no_longer_configured'] },
      { agentId: 'sales', ids: ['renamed_db', 'deleted_db'] },
    ]);
  });

  it('drops agents with an empty id list', () => {
    expect(buildStaleDbSourceEntries({ ops: [] })).toEqual([]);
  });

  it('returns an empty array for an empty map', () => {
    expect(buildStaleDbSourceEntries({})).toEqual([]);
  });
});

describe('resolveAgentDisplayName', () => {
  const agents = [agent({ id: 'sales', display_name: '小美' })];

  it('resolves a known agent id to its display name', () => {
    expect(resolveAgentDisplayName(agents, 'sales')).toBe('小美');
  });

  it('falls back to the raw id when the agent is unknown', () => {
    expect(resolveAgentDisplayName(agents, 'ghost')).toBe('ghost');
  });
});

// ── i18n parity (mirrors `redactionAiDetection.test.ts`'s convention) ────

describe('db-source grants i18n coverage', () => {
  const catalogues = { en, 'zh-TW': zhTW, 'ja-JP': jaJP } as Record<string, Record<string, string>>;

  it('ships every db-source grants string in all three catalogues', () => {
    const required = [
      'agents.cap.dbSources',
      'agents.cap.dbSources.hint',
      'agents.cap.dbSources.empty',
      'agents.cap.dbSources.gone',
      'redaction.wizard.step4.db.prompt',
      'redaction.wizard.step4.db.grantsHint',
      'redaction.wizard.step4.db.grantsSaveFailed',
      'redaction.dbGrants.picker.empty',
      'redaction.systems.db.grants.badge',
      'redaction.systems.db.grants.badge.none',
      'redaction.systems.db.grants.dialog.title',
      'redaction.systems.db.grants.dialog.hint',
      'redaction.systems.db.grants.saveFailed',
      'redaction.systems.db.grants.staleNotice',
      'redaction.systems.db.grants.staleNotice.item',
      'redaction.systems.db.grants.loadErrors',
      'redaction.systems.db.grants.revokeFailed',
    ];
    for (const [locale, catalogue] of Object.entries(catalogues)) {
      const missing = required.filter((key) => !catalogue[key]?.trim());
      expect(missing, `${locale} is missing keys`).toEqual([]);
    }
  });
});
