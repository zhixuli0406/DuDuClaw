import { describe, it, expect } from 'vitest';
import {
  ODOO_EGRESS_KEY,
  ODOO_LEGACY_EGRESS_KEY,
  deriveCustomEgressKeys,
  deriveEgressKeysForSource,
  readEgressForKeys,
  buildEgressPatch,
  attributedEgressKeys,
  unattributedEgressEntries,
  resolveRuleSourceName,
  countFieldRulesForSource,
  filterFieldRulesBySource,
  buildSystemRows,
  describeCustomSourceReadIn,
  slugifyDataSourceName,
  deriveDataSourceId,
  canProceedDbStep2,
  customSourceSmokeTestParams,
} from './redactionSystems';
import type { RedactionDataSource, RedactionEgressRule, RedactionFieldRule, DbSourceSummary } from '@/lib/api';

function dataSource(overrides: Partial<RedactionDataSource>): RedactionDataSource {
  return {
    name: 'custom',
    label: 'Custom',
    builtin: false,
    tools: [],
    table_arg: 'table',
    table: null,
    table_result: null,
    record_paths: ['$.rows[*]', '$[*]', '$'],
    free_form_names: false,
    key_alias: {},
    ...overrides,
  };
}

function dbFieldRule(overrides: Partial<RedactionFieldRule>): RedactionFieldRule {
  return {
    id: 'r1',
    kind: 'db_field',
    category: 'DB_FIELD',
    restore_scope: { kind: 'owner' },
    priority: 50,
    cross_session_stable: true,
    connector: 'odoo',
    fields: ['res.partner.name'],
    ...overrides,
  };
}

function dbSource(overrides: Partial<DbSourceSummary>): DbSourceSummary {
  return {
    name: 'crm_pg',
    label: 'CRM Postgres',
    driver: 'postgres',
    allowed_tables: ['customers'],
    max_rows: 200,
    timeout_ms: 10000,
    url_status: { configured: true, source: 'inline', source_label: 'encrypted', writable: true, residue: false },
    ...overrides,
  };
}

const rule: RedactionEgressRule = { restore_args: 'deny', audit_reveal: false };

// ── egress key derivation ────────────────────────────────────────────────

describe('deriveCustomEgressKeys', () => {
  it('collapses tools sharing one <server>. prefix into a single glob', () => {
    expect(deriveCustomEgressKeys(['crm_legacy.list', 'crm_legacy.get'])).toEqual(['crm_legacy.*']);
  });
  it('handles an already-globbed tool consistently (no double star)', () => {
    expect(deriveCustomEgressKeys(['crm_legacy.*'])).toEqual(['crm_legacy.*']);
    expect(deriveCustomEgressKeys(['crm_legacy.*', 'crm_legacy.get'])).toEqual(['crm_legacy.*']);
  });
  it('falls back to one exact key per tool when there is no shared prefix', () => {
    expect(deriveCustomEgressKeys(['pg_query', 'pg_select'])).toEqual(['pg_query', 'pg_select']);
  });
  it('drops blank tokens and dedupes', () => {
    expect(deriveCustomEgressKeys(['pg_query', '', '  ', 'pg_query'])).toEqual(['pg_query']);
  });
  it('returns empty for an empty tool list', () => {
    expect(deriveCustomEgressKeys([])).toEqual([]);
  });
});

describe('deriveEgressKeysForSource', () => {
  it('delegates to deriveCustomEgressKeys', () => {
    expect(deriveEgressKeysForSource({ tools: ['x.a', 'x.b'] })).toEqual(['x.*']);
  });
});

describe('readEgressForKeys', () => {
  it('returns the first present key\'s rule', () => {
    const map = { [ODOO_LEGACY_EGRESS_KEY]: rule };
    expect(readEgressForKeys(map, [ODOO_EGRESS_KEY, ODOO_LEGACY_EGRESS_KEY])).toBe(rule);
  });
  it('returns null when none of the keys are present', () => {
    expect(readEgressForKeys({}, ['pg_query'])).toBeNull();
  });
});

describe('buildEgressPatch', () => {
  it('writes the same rule to every key', () => {
    expect(buildEgressPatch(['a', 'b'], rule)).toEqual({ a: rule, b: rule });
  });
  it('nulls out every key when rule is null (delete)', () => {
    expect(buildEgressPatch(['a', 'b'], null)).toEqual({ a: null, b: null });
  });
  it('returns an empty patch for an empty key list', () => {
    expect(buildEgressPatch([], rule)).toEqual({});
  });
});

describe('attributedEgressKeys / unattributedEgressEntries', () => {
  it('always attributes both the canonical and legacy Odoo keys', () => {
    const keys = attributedEgressKeys([]);
    expect(keys.has(ODOO_EGRESS_KEY)).toBe(true);
    expect(keys.has(ODOO_LEGACY_EGRESS_KEY)).toBe(true);
  });
  it('attributes a custom source\'s derived keys and skips built-ins', () => {
    const sources = [
      dataSource({ name: 'crm_legacy', tools: ['crm_legacy.list', 'crm_legacy.get'] }),
      dataSource({ name: 'duduclaw_db', builtin: true, tools: ['db_select'] }),
    ];
    const keys = attributedEgressKeys(sources);
    expect(keys.has('crm_legacy.*')).toBe(true);
    expect(keys.has('db_select')).toBe(false);
  });
  it('leaves an operator-saved-but-unlisted key in the unattributed set', () => {
    const toolEgress = { [ODOO_EGRESS_KEY]: rule, 'mystery_tool': rule };
    const entries = unattributedEgressEntries(toolEgress, []);
    expect(entries).toEqual([['mystery_tool', rule]]);
  });
  it('never lists the legacy odoo.* key as unattributed', () => {
    const toolEgress = { [ODOO_LEGACY_EGRESS_KEY]: rule };
    expect(unattributedEgressEntries(toolEgress, [])).toEqual([]);
  });
});

// ── rule counting / filtering ────────────────────────────────────────────

describe('resolveRuleSourceName', () => {
  it('reads connector, defaulting to odoo', () => {
    expect(resolveRuleSourceName(dbFieldRule({ connector: undefined }))).toBe('odoo');
    expect(resolveRuleSourceName(dbFieldRule({ connector: 'crm_pg' }))).toBe('crm_pg');
  });
  it('returns null for json_path rules', () => {
    expect(resolveRuleSourceName(dbFieldRule({ kind: 'json_path', connector: undefined }))).toBeNull();
  });
});

describe('countFieldRulesForSource / filterFieldRulesBySource', () => {
  const rules = [
    dbFieldRule({ id: 'a', connector: 'odoo' }),
    dbFieldRule({ id: 'b', connector: 'duduclaw_db' }),
    dbFieldRule({ id: 'c', connector: undefined }), // defaults to odoo
    dbFieldRule({ id: 'd', kind: 'json_path', connector: undefined }),
  ];
  it('counts only rules resolving to the given source', () => {
    expect(countFieldRulesForSource(rules, 'odoo')).toBe(2);
    expect(countFieldRulesForSource(rules, 'duduclaw_db')).toBe(1);
    expect(countFieldRulesForSource(rules, 'nonexistent')).toBe(0);
  });
  it('filters to the matching subset, preserving order', () => {
    expect(filterFieldRulesBySource(rules, 'odoo').map((r) => r.id)).toEqual(['a', 'c']);
  });
  it('returns every rule unfiltered when source is null', () => {
    expect(filterFieldRulesBySource(rules, null)).toHaveLength(4);
  });
});

// ── row assembly ─────────────────────────────────────────────────────────

describe('buildSystemRows', () => {
  it('always includes an odoo row and a files row, even with nothing else configured', () => {
    const rows = buildSystemRows([], [], [], {}, false);
    expect(rows.map((r) => r.kind)).toEqual(['odoo', 'files']);
    expect(rows[0]).toMatchObject({ kind: 'odoo', connected: false, ruleCount: 0, egress: null });
  });

  it('reads the odoo egress rule via the legacy key when the canonical key is absent', () => {
    const rows = buildSystemRows([], [], [], { [ODOO_LEGACY_EGRESS_KEY]: rule }, true);
    const odoo = rows.find((r) => r.kind === 'odoo');
    expect(odoo).toMatchObject({ connected: true, egress: rule });
  });

  it('lists a custom data source row with its derived egress and rule count', () => {
    const sources = [dataSource({ name: 'crm_legacy', label: '舊版 CRM', tools: ['crm_legacy.list'] })];
    const toolEgress = { 'crm_legacy.*': rule };
    const rules = [dbFieldRule({ connector: 'crm_legacy' })];
    const rows = buildSystemRows(rules, sources, [], toolEgress, false);
    const custom = rows.find((r) => r.kind === 'custom');
    expect(custom).toMatchObject({ kind: 'custom', ruleCount: 1, egress: rule, egressKeys: ['crm_legacy.*'] });
  });

  it('hides built-in registry entries from the row list', () => {
    const sources = [
      dataSource({ name: 'odoo', builtin: true }),
      dataSource({ name: 'duduclaw_db', builtin: true }),
      dataSource({ name: 'duduclaw_files', builtin: true }),
    ];
    const rows = buildSystemRows([], sources, [], {}, true);
    expect(rows.map((r) => r.kind)).toEqual(['odoo', 'files']);
  });

  it('lists an unpaired db_sources connection as its own db row, sharing the generic duduclaw_db rule count', () => {
    const dbSources = [dbSource({ name: 'crm_pg' }), dbSource({ name: 'ops_pg' })];
    const rules = [dbFieldRule({ connector: 'duduclaw_db' })];
    const rows = buildSystemRows(rules, [], dbSources, {}, false);
    const dbRows = rows.filter((r) => r.kind === 'db');
    expect(dbRows).toHaveLength(2);
    expect(dbRows.every((r) => r.kind === 'db' && r.ruleCount === 1)).toBe(true);
  });

  it('does not double-list a db_sources connection paired with a same-named custom data source', () => {
    const sources = [dataSource({ name: 'crm_pg', tools: ['db_select'] })];
    const dbSources = [dbSource({ name: 'crm_pg' })];
    const rows = buildSystemRows([], sources, dbSources, {}, false);
    expect(rows.filter((r) => r.kind === 'db')).toHaveLength(0);
    expect(rows.filter((r) => r.kind === 'custom')).toHaveLength(1);
  });

  it('orders rows: odoo, then custom sources, then db connections, then files', () => {
    const sources = [dataSource({ name: 'crm_legacy', tools: ['crm_legacy.*'] })];
    const dbSources = [dbSource({ name: 'crm_pg' })];
    const rows = buildSystemRows([], sources, dbSources, {}, false);
    expect(rows.map((r) => r.kind)).toEqual(['odoo', 'custom', 'db', 'files']);
  });
});

// ── custom source read-in summary ─────────────────────────────────────────

describe('describeCustomSourceReadIn', () => {
  it('always leads with the tools line', () => {
    const lines = describeCustomSourceReadIn(dataSource({ tools: ['pg_query'] }));
    expect(lines[0]).toEqual({ kind: 'tools', tools: ['pg_query'] });
  });
  it('prefers a fixed table over table_arg / table_result', () => {
    const lines = describeCustomSourceReadIn(dataSource({ table: 'customers', table_arg: 'table' }));
    expect(lines).toContainEqual({ kind: 'tableFixed', table: 'customers' });
    expect(lines).not.toContainEqual({ kind: 'tableArg', arg: 'table' });
  });
  it('reports table_result when set', () => {
    const lines = describeCustomSourceReadIn(dataSource({ table_result: '/table' }));
    expect(lines).toContainEqual({ kind: 'tableResult', pointer: '/table' });
  });
  it('falls back to table_arg when neither table nor table_result is set', () => {
    const lines = describeCustomSourceReadIn(dataSource({ table_arg: 'table' }));
    expect(lines).toContainEqual({ kind: 'tableArg', arg: 'table' });
  });
  it('appends a free-form-names line when set', () => {
    const lines = describeCustomSourceReadIn(dataSource({ free_form_names: true }));
    expect(lines).toContainEqual({ kind: 'freeFormNames' });
  });
});

// ── wizard helpers ─────────────────────────────────────────────────────

describe('slugifyDataSourceName', () => {
  it('lowercases and joins spaces/hyphens with underscores (ASCII label → slug)', () => {
    expect(slugifyDataSourceName('Legacy CRM-System')).toBe('legacy_crm_system');
  });
  it('drops characters outside the backend charset (incl. CJK) as long as something ASCII survives', () => {
    expect(slugifyDataSourceName('客戶 CRM 資料庫')).toBe('crm_');
  });
  it('prefixes a leading digit so the slug still starts with a letter', () => {
    expect(slugifyDataSourceName('2nd CRM')).toBe('s_2nd_crm');
  });
  it('falls back to a stable source_<hex>/db_<hex> fingerprint for a CJK-only label (nothing ASCII survives)', () => {
    const source = slugifyDataSourceName('客戶', 'source');
    expect(source).toMatch(/^source_[0-9a-f]{6}$/);
    // Stable across calls — same label, same kind, same existing ids ⇒
    // identical id every time (deterministic hash, never random/time-based).
    expect(slugifyDataSourceName('客戶', 'source')).toBe(source);

    const db = slugifyDataSourceName('客戶', 'db');
    expect(db).toMatch(/^db_[0-9a-f]{6}$/);
    expect(slugifyDataSourceName('客戶', 'db')).toBe(db);

    // Different labels vs the same label read as different vs identical
    // fingerprints — not just "some hex string every time".
    expect(slugifyDataSourceName('客戶', 'source')).not.toBe(slugifyDataSourceName('資料庫', 'source'));
  });
  it('defaults to kind "source" and no existing ids when not given', () => {
    expect(slugifyDataSourceName('客戶')).toBe(slugifyDataSourceName('客戶', 'source', []));
  });
  it('disambiguates against existingIds by appending _2, _3, …', () => {
    expect(slugifyDataSourceName('CRM', 'source', [])).toBe('crm');
    expect(slugifyDataSourceName('CRM', 'source', ['crm'])).toBe('crm_2');
    expect(slugifyDataSourceName('CRM', 'source', ['crm', 'crm_2'])).toBe('crm_3');
    // The CJK-only fallback is disambiguated the same way.
    const first = slugifyDataSourceName('客戶', 'source');
    expect(slugifyDataSourceName('客戶', 'source', [first])).toBe(`${first}_2`);
  });
  it('caps at 64 characters, even after a disambiguating suffix', () => {
    expect(slugifyDataSourceName('a'.repeat(100)).length).toBe(64);
    const long = 'a'.repeat(64);
    const suffixed = slugifyDataSourceName(long, 'source', [long]);
    expect(suffixed.length).toBe(64);
    expect(suffixed.endsWith('_2')).toBe(true);
  });
});

describe('deriveDataSourceId', () => {
  it('auto-derives from the label while creating', () => {
    expect(deriveDataSourceId('Customer CRM', 'source', [], true, '')).toBe('customer_crm');
  });
  it('never regenerates the id once a source is saved — editing keeps it fixed', () => {
    // Even a completely different label, or one that would collide, leaves
    // an existing row's id untouched: it is a durable reference other
    // config may already point at.
    expect(deriveDataSourceId('A brand new label', 'source', ['crm_pg'], false, 'crm_pg')).toBe('crm_pg');
    expect(deriveDataSourceId('', 'db', [], false, 'legacy_pg')).toBe('legacy_pg');
  });
});

describe('canProceedDbStep2', () => {
  it('requires a non-blank display label and credential when creating', () => {
    expect(canProceedDbStep2('CRM Postgres', 'postgres://x', true)).toBe(true);
    expect(canProceedDbStep2('', 'postgres://x', true)).toBe(false);
    expect(canProceedDbStep2('CRM Postgres', '  ', true)).toBe(false);
  });
  it('accepts a free-text label (CJK, spaces) — it is no longer validated as an identifier', () => {
    expect(canProceedDbStep2('客戶 CRM 資料庫', 'postgres://x', true)).toBe(true);
  });
  it('never blocks editing (blank credential means "keep the stored one")', () => {
    expect(canProceedDbStep2('CRM Postgres', '', false)).toBe(true);
  });
});

describe('customSourceSmokeTestParams', () => {
  it('strips a trailing glob star from the first tool', () => {
    const { tool } = customSourceSmokeTestParams({ tools: ['crm_legacy.*'], tableMode: 'fixed', tableArg: 'table' });
    expect(tool).toBe('crm_legacy.');
  });
  it('includes the table_arg key with an empty sample when table mode is from_arg', () => {
    const { args } = customSourceSmokeTestParams({ tools: ['pg_query'], tableMode: 'from_arg', tableArg: 'table' });
    expect(args).toEqual({ table: '' });
  });
  it('omits args entirely for fixed / from_result table modes', () => {
    expect(customSourceSmokeTestParams({ tools: ['pg_query'], tableMode: 'fixed', tableArg: 'table' }).args).toEqual({});
    expect(customSourceSmokeTestParams({ tools: ['pg_query'], tableMode: 'from_result', tableArg: 'table' }).args).toEqual({});
  });
});
