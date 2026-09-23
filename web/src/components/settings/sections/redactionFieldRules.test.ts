import { describe, it, expect } from 'vitest';
import {
  addKvRow,
  addToken,
  buildDbFields,
  BUILTIN_SOURCE_NAMES,
  dataSourceToFormState,
  dbSourceToFormState,
  dryRunDefaultsForRule,
  emptyDataSourceForm,
  emptyDbSourceForm,
  emptyFieldRuleForm,
  fieldRuleToFormState,
  formStateToDataSourceDef,
  formStateToDbSourceUpsert,
  formStateToDbSourceTestParams,
  formStateToRuleInput,
  groupDbFields,
  isDataSourceFormValid,
  isDbSourceFormValid,
  isFieldRuleFormValid,
  isFreeFormSource,
  isValidDataSourceName,
  isValidFieldToken,
  isValidFreeFormFieldToken,
  isValidFreeFormTableName,
  isValidMatchToolGlob,
  isValidPathExpr,
  isValidResultPointer,
  isValidRuleId,
  isValidTableName,
  kvRowsToRecord,
  recordToKvRows,
  removeKvRow,
  removeToken,
  splitUrlForUpsert,
  summarizeDbSourceTest,
  summarizeDryRun,
  summarizeFieldRuleCoverage,
  updateKvRow,
  validateDataSourceForm,
  validateDbSourceForm,
  validateFieldRuleForm,
} from './redactionFieldRules';
import type {
  RedactionDryRunResult,
  RedactionFieldRule,
  RedactionDataSource,
  DbSourceSummary,
  DbSourceTestResult,
} from '@/lib/api';

// ── validation ──────────────────────────────────────────────────────────

describe('isValidRuleId / isValidDataSourceName', () => {
  it('accepts lowercase snake_case identifiers', () => {
    expect(isValidRuleId('customer_master')).toBe(true);
    expect(isValidDataSourceName('crm_pg')).toBe(true);
  });
  it('rejects empty, uppercase, and leading-digit strings', () => {
    expect(isValidRuleId('')).toBe(false);
    expect(isValidRuleId('CustomerMaster')).toBe(false);
    expect(isValidRuleId('1abc')).toBe(false);
    expect(isValidRuleId('has space')).toBe(false);
  });
});

describe('isValidTableName', () => {
  it('accepts dotted Odoo models and plain SQL tables', () => {
    expect(isValidTableName('res.partner')).toBe(true);
    expect(isValidTableName('hr.employee.private')).toBe(true);
    expect(isValidTableName('customers')).toBe(true);
  });
  it('rejects empty and malformed input', () => {
    expect(isValidTableName('')).toBe(false);
    expect(isValidTableName('res.')).toBe(false);
    expect(isValidTableName('.partner')).toBe(false);
    expect(isValidTableName('Res.Partner')).toBe(false);
  });
});

describe('isValidFieldToken', () => {
  it('accepts identifiers and the wildcard', () => {
    expect(isValidFieldToken('name')).toBe(true);
    expect(isValidFieldToken('*')).toBe(true);
  });
  it('rejects anything else', () => {
    expect(isValidFieldToken('')).toBe(false);
    expect(isValidFieldToken('Name')).toBe(false);
    expect(isValidFieldToken('a-b')).toBe(false);
  });
});

describe('isValidPathExpr', () => {
  it('requires a leading $ and more than one char', () => {
    expect(isValidPathExpr('$[*].description')).toBe(true);
    expect(isValidPathExpr('$..partner_name')).toBe(true);
    expect(isValidPathExpr('$')).toBe(false);
    expect(isValidPathExpr('')).toBe(false);
    expect(isValidPathExpr('rows[*]')).toBe(false);
  });
});

describe('isValidMatchToolGlob', () => {
  it('treats empty as valid (any tool)', () => {
    expect(isValidMatchToolGlob('')).toBe(true);
    expect(isValidMatchToolGlob('   ')).toBe(true);
  });
  it('accepts an exact name or a trailing-* glob', () => {
    expect(isValidMatchToolGlob('odoo_search')).toBe(true);
    expect(isValidMatchToolGlob('odoo_*')).toBe(true);
  });
  it('rejects a glob in the middle or spaces', () => {
    expect(isValidMatchToolGlob('odoo_*_search')).toBe(false);
    expect(isValidMatchToolGlob('odoo search')).toBe(false);
  });
});

// ── §14.3 free-form table/column validation ────────────────────────────

describe('isValidFreeFormTableName', () => {
  it('accepts CJK file names, extensions, and spaces', () => {
    expect(isValidFreeFormTableName('customers.csv')).toBe(true);
    expect(isValidFreeFormTableName('客戶清單.xlsx')).toBe(true);
    expect(isValidFreeFormTableName('2026 年度 客戶名單.xlsx')).toBe(true);
  });
  it('still rejects empty, control chars, and a literal apostrophe', () => {
    expect(isValidFreeFormTableName('')).toBe(false);
    expect(isValidFreeFormTableName('   ')).toBe(false);
    expect(isValidFreeFormTableName('a\x00b')).toBe(false);
    expect(isValidFreeFormTableName("O'Brien.csv")).toBe(false);
  });
  it('is strictly more permissive than isValidTableName for the same CJK input', () => {
    expect(isValidTableName('客戶清單.xlsx')).toBe(false);
    expect(isValidFreeFormTableName('客戶清單.xlsx')).toBe(true);
  });
});

describe('isValidFreeFormFieldToken', () => {
  it('accepts CJK column names and the wildcard', () => {
    expect(isValidFreeFormFieldToken('地址')).toBe(true);
    expect(isValidFreeFormFieldToken('姓名')).toBe(true);
    expect(isValidFreeFormFieldToken('*')).toBe(true);
  });
  it('rejects empty, control chars, an apostrophe, and a slash', () => {
    expect(isValidFreeFormFieldToken('')).toBe(false);
    expect(isValidFreeFormFieldToken('a\x7fb')).toBe(false);
    expect(isValidFreeFormFieldToken("customer's name")).toBe(false);
    expect(isValidFreeFormFieldToken('a/b')).toBe(false);
  });
});

describe('isValidResultPointer', () => {
  it('accepts a leading-slash pointer', () => {
    expect(isValidResultPointer('/table')).toBe(true);
    expect(isValidResultPointer('/meta/table')).toBe(true);
  });
  it('rejects blank, root-only, and a pointer missing the leading slash', () => {
    expect(isValidResultPointer('')).toBe(false);
    expect(isValidResultPointer('/')).toBe(false);
    expect(isValidResultPointer('table')).toBe(false);
  });
});

describe('isFreeFormSource', () => {
  const FREE_FORM_CUSTOM: RedactionDataSource = {
    name: 'crm_pg',
    label: '客戶 CRM 資料庫',
    builtin: false,
    tools: ['pg_query'],
    table_arg: 'table',
    table: null,
    table_result: null,
    record_paths: ['$.rows[*]'],
    free_form_names: true,
    key_alias: {},
  };
  it('is always true for the built-in duduclaw_files source, even with no registry entry', () => {
    expect(isFreeFormSource('duduclaw_files', [])).toBe(true);
  });
  it('is true for a custom source whose registry entry carries free_form_names: true', () => {
    expect(isFreeFormSource('crm_pg', [FREE_FORM_CUSTOM])).toBe(true);
  });
  it('is false for odoo, an unregistered name, and a registered source without the flag', () => {
    expect(isFreeFormSource('odoo', [])).toBe(false);
    expect(isFreeFormSource('unknown_source', [])).toBe(false);
    expect(isFreeFormSource('crm_pg', [{ ...FREE_FORM_CUSTOM, free_form_names: false }])).toBe(false);
  });
});

describe('validateFieldRuleForm — free_form_names sources (§14.3)', () => {
  const FREE_FORM_CUSTOM: RedactionDataSource = {
    name: 'crm_pg',
    label: '客戶 CRM 資料庫',
    builtin: false,
    tools: ['pg_query'],
    table_arg: 'table',
    table: null,
    table_result: null,
    record_paths: ['$.rows[*]'],
    free_form_names: true,
    key_alias: {},
  };

  it('accepts a CJK table + column for the built-in duduclaw_files source', () => {
    const form = {
      ...emptyFieldRuleForm(),
      source: 'duduclaw_files',
      table: '客戶清單.xlsx',
      fieldTokens: ['地址', '姓名'],
    };
    expect(validateFieldRuleForm(form, false)).toEqual({});
    expect(isFieldRuleFormValid(form, false)).toBe(true);
  });

  it('accepts a CJK table + column for a custom source with free_form_names, given the registry', () => {
    const form = {
      ...emptyFieldRuleForm(),
      source: 'crm_pg',
      table: 'customers.csv',
      fieldTokens: ['姓名'],
    };
    expect(validateFieldRuleForm(form, false, [FREE_FORM_CUSTOM])).toEqual({});
  });

  it('keeps identifier-mode validation byte-identical for every other source (dataSources omitted or absent)', () => {
    const badTable = { ...emptyFieldRuleForm(), source: 'odoo', table: '客戶清單.xlsx', fieldTokens: ['name'] };
    expect(validateFieldRuleForm(badTable, false)).toEqual({ table: 'invalid' });
    // Same source name registered WITHOUT the flag — still identifier-mode.
    const stillIdentifier = {
      ...emptyFieldRuleForm(),
      source: 'crm_pg',
      table: '客戶清單.xlsx',
      fieldTokens: ['name'],
    };
    expect(validateFieldRuleForm(stillIdentifier, false, [{ ...FREE_FORM_CUSTOM, free_form_names: false }])).toEqual({
      table: 'invalid',
    });
  });

  it('still rejects a free-form field containing a slash', () => {
    const form = { ...emptyFieldRuleForm(), source: 'duduclaw_files', table: 'a.csv', fieldTokens: ['a/b'] };
    expect(validateFieldRuleForm(form, false).fields).toBe('invalid');
  });
});

// ── token / kv row helpers ──────────────────────────────────────────────

describe('addToken / removeToken', () => {
  it('trims, dedupes, and appends', () => {
    expect(addToken(['name'], '  street  ')).toEqual(['name', 'street']);
    expect(addToken(['name'], 'name')).toEqual(['name']);
    expect(addToken(['name'], '   ')).toEqual(['name']);
  });
  it('removes by exact match without touching others', () => {
    expect(removeToken(['name', 'street', 'comment'], 'street')).toEqual(['name', 'comment']);
  });
  it('never mutates the input array', () => {
    const original = ['a'];
    const next = addToken(original, 'b');
    expect(original).toEqual(['a']);
    expect(next).toEqual(['a', 'b']);
  });
});

describe('kv row helpers', () => {
  it('add / update / remove round-trip without mutating input', () => {
    const empty = addKvRow([]);
    expect(empty).toEqual([{ key: '', value: '' }]);
    const filled = updateKvRow(empty, 0, { key: 'model', value: 'crm.lead' });
    expect(filled).toEqual([{ key: 'model', value: 'crm.lead' }]);
    expect(empty).toEqual([{ key: '', value: '' }]); // untouched
    expect(removeKvRow(filled, 0)).toEqual([]);
  });
  it('kvRowsToRecord drops blank keys and trims', () => {
    expect(
      kvRowsToRecord([
        { key: ' model ', value: 'crm.lead' },
        { key: '  ', value: 'ignored' },
      ]),
    ).toEqual({ model: 'crm.lead' });
  });
  it('recordToKvRows round-trips through kvRowsToRecord', () => {
    const record = { model: 'crm.lead', stage: 'won' };
    expect(kvRowsToRecord(recordToKvRows(record))).toEqual(record);
    expect(recordToKvRows(undefined)).toEqual([]);
  });
});

// ── db_field <-> "table.field" grouping ─────────────────────────────────

describe('groupDbFields / buildDbFields', () => {
  it('groups a flat list by table, preserving first-seen order', () => {
    expect(
      groupDbFields(['res.partner.name', 'res.partner.street', 'hr.employee.*', 'res.partner.comment']),
    ).toEqual([
      { table: 'res.partner', fieldTokens: ['name', 'street', 'comment'] },
      { table: 'hr.employee', fieldTokens: ['*'] },
    ]);
  });
  it('skips malformed entries with no dot', () => {
    expect(groupDbFields(['no_dot_here', 'res.partner.name'])).toEqual([
      { table: 'res.partner', fieldTokens: ['name'] },
    ]);
  });
  it('buildDbFields collapses a wildcard to one table.* entry', () => {
    expect(buildDbFields('hr.employee', ['*'])).toEqual(['hr.employee.*']);
    expect(buildDbFields('hr.employee', ['*', 'name'])).toEqual(['hr.employee.*']);
  });
  it('buildDbFields expands named fields and returns [] for blank input', () => {
    expect(buildDbFields('res.partner', ['name', 'street'])).toEqual([
      'res.partner.name',
      'res.partner.street',
    ]);
    expect(buildDbFields('', ['name'])).toEqual([]);
    expect(buildDbFields('res.partner', [])).toEqual([]);
  });
  it('round-trips group -> build for a single table', () => {
    const fields = ['res.partner.name', 'res.partner.street'];
    const [group] = groupDbFields(fields);
    expect(buildDbFields(group.table, group.fieldTokens)).toEqual(fields);
  });
});

// ── field rule form <-> rule conversion ─────────────────────────────────

function makeDbFieldRule(overrides: Partial<RedactionFieldRule> = {}): RedactionFieldRule {
  return {
    id: 'customer_master',
    kind: 'db_field',
    category: 'DB_FIELD',
    restore_scope: { kind: 'owner' },
    priority: 50,
    cross_session_stable: true,
    fields: ['res.partner.name', 'res.partner.street', 'res.partner.comment', 'hr.employee.*'],
    ...overrides,
  };
}

describe('fieldRuleToFormState / formStateToRuleInput — db_field', () => {
  it('loads the first table group into the editable fields and stashes the rest', () => {
    const form = fieldRuleToFormState(makeDbFieldRule());
    expect(form.mode).toBe('db_field');
    expect(form.table).toBe('res.partner');
    expect(form.fieldTokens).toEqual(['name', 'street', 'comment']);
    expect(form.otherTableFields).toEqual(['hr.employee.*']);
    expect(form.source).toBe('odoo');
  });

  it('round-trips a single-table rule byte-for-byte through the form', () => {
    const rule = makeDbFieldRule({ fields: ['res.partner.name', 'res.partner.street'] });
    const form = fieldRuleToFormState(rule);
    const input = formStateToRuleInput(form);
    expect(input.type).toBe('db_field');
    expect(input.fields).toEqual(['res.partner.name', 'res.partner.street']);
    expect(input.connector).toBe('odoo');
    expect(input.category).toBe('DB_FIELD');
    expect(input.restore_scope).toEqual({ kind: 'owner' });
  });

  it('preserves an edit made only to the active table without losing other tables', () => {
    const form = fieldRuleToFormState(makeDbFieldRule());
    const edited = { ...form, fieldTokens: [...form.fieldTokens, 'email'] };
    const input = formStateToRuleInput(edited);
    expect(input.fields).toEqual([
      'hr.employee.*', // preserved untouched
      'res.partner.name',
      'res.partner.street',
      'res.partner.comment',
      'res.partner.email',
    ]);
  });

  it('honours a non-default source and non-owner restore scope', () => {
    const rule = makeDbFieldRule({
      connector: 'crm_pg',
      fields: ['customers.name'],
      restore_scope: { kind: 'any_scope', scope: 'FinanceRead' },
    });
    const form = fieldRuleToFormState(rule);
    expect(form.source).toBe('crm_pg');
    const input = formStateToRuleInput(form);
    expect(input.connector).toBe('crm_pg');
    expect(input.restore_scope).toEqual({ kind: 'any_scope', scope: 'FinanceRead' });
  });
});

function makeJsonPathRule(overrides: Partial<RedactionFieldRule> = {}): RedactionFieldRule {
  return {
    id: 'crm_notes',
    kind: 'json_path',
    category: 'CRM_NOTE',
    restore_scope: { kind: 'owner' },
    priority: 50,
    cross_session_stable: true,
    match_tool: 'odoo_*',
    match_args: { model: 'crm.lead' },
    paths: ['$[*].description', '$..partner_name'],
    exclude_keys: ['id'],
    ...overrides,
  };
}

describe('fieldRuleToFormState / formStateToRuleInput — json_path', () => {
  it('round-trips every field', () => {
    const rule = makeJsonPathRule();
    const form = fieldRuleToFormState(rule);
    expect(form.mode).toBe('json_path');
    expect(form.matchTool).toBe('odoo_*');
    expect(form.matchArgs).toEqual([{ key: 'model', value: 'crm.lead' }]);
    expect(form.paths).toEqual(['$[*].description', '$..partner_name']);
    expect(form.excludeKeys).toEqual(['id']);

    const input = formStateToRuleInput(form);
    expect(input.type).toBe('json_path');
    expect(input.match_tool).toBe('odoo_*');
    expect(input.match_args).toEqual({ model: 'crm.lead' });
    expect(input.paths).toEqual(['$[*].description', '$..partner_name']);
    expect(input.exclude_keys).toEqual(['id']);
  });

  it('nulls out an empty match_tool ("any tool")', () => {
    const form = fieldRuleToFormState(makeJsonPathRule({ match_tool: null }));
    expect(form.matchTool).toBe('');
    expect(formStateToRuleInput(form).match_tool).toBeNull();
  });

  it('filters blank paths and exclude_keys on save', () => {
    const form = { ...emptyFieldRuleForm(), mode: 'json_path' as const, paths: ['$.a', '  '], excludeKeys: ['id', ''] };
    const input = formStateToRuleInput(form);
    expect(input.paths).toEqual(['$.a']);
    expect(input.exclude_keys).toEqual(['id']);
  });
});

// ── form validation ─────────────────────────────────────────────────────

describe('validateFieldRuleForm / isFieldRuleFormValid', () => {
  it('requires a valid id only when creating', () => {
    const form = { ...emptyFieldRuleForm(), id: 'Bad Id', table: 'res.partner', fieldTokens: ['name'] };
    expect(validateFieldRuleForm(form, true)).toEqual({ id: 'invalid' });
    expect(validateFieldRuleForm(form, false)).toEqual({});
  });

  it('db_field mode requires a valid table and at least one field token', () => {
    const blank = emptyFieldRuleForm();
    expect(validateFieldRuleForm(blank, false)).toEqual({ table: 'invalid', fields: 'empty' });
    const badToken = { ...blank, table: 'res.partner', fieldTokens: ['Bad Token'] };
    expect(validateFieldRuleForm(badToken, false)).toEqual({ fields: 'invalid' });
    const good = { ...blank, table: 'res.partner', fieldTokens: ['name'] };
    expect(isFieldRuleFormValid(good, false)).toBe(true);
  });

  it('json_path mode requires at least one well-formed path', () => {
    const blank = { ...emptyFieldRuleForm(), mode: 'json_path' as const };
    expect(validateFieldRuleForm(blank, false)).toEqual({ paths: 'empty' });
    const badPath = { ...blank, paths: ['not-a-path'] };
    expect(validateFieldRuleForm(badPath, false)).toEqual({ paths: 'invalid' });
    const good = { ...blank, paths: ['$.a'] };
    expect(isFieldRuleFormValid(good, false)).toBe(true);
  });
});

// ── coverage summary ────────────────────────────────────────────────────

describe('summarizeFieldRuleCoverage', () => {
  it('renders a multi-table db_field rule with mixed all-fields / specific groups', () => {
    const lines = summarizeFieldRuleCoverage(makeDbFieldRule());
    expect(lines).toEqual([
      { kind: 'dbField', table: 'res.partner', allFields: false, fieldTokens: ['name', 'street', 'comment'] },
      { kind: 'dbField', table: 'hr.employee', allFields: true, fieldTokens: ['*'] },
      { kind: 'restoreScope', scope: { kind: 'owner' } },
    ]);
  });

  it('adds a source line only when the source is not the odoo default', () => {
    const odoo = summarizeFieldRuleCoverage(makeDbFieldRule());
    expect(odoo.some((l) => l.kind === 'source')).toBe(false);
    const custom = summarizeFieldRuleCoverage(makeDbFieldRule({ connector: 'crm_pg', fields: ['customers.name'] }));
    expect(custom[0]).toEqual({ kind: 'source', source: 'crm_pg' });
  });

  it('renders a json_path rule as tool + args, then paths, then scope', () => {
    const lines = summarizeFieldRuleCoverage(makeJsonPathRule());
    expect(lines).toEqual([
      { kind: 'jsonPathTool', tool: 'odoo_*', matchArgs: [['model', 'crm.lead']] },
      { kind: 'jsonPathPaths', paths: ['$[*].description', '$..partner_name'] },
      { kind: 'restoreScope', scope: { kind: 'owner' } },
    ]);
  });
});

// ── dry-run defaults ────────────────────────────────────────────────────

const CRM_PG_SOURCE: RedactionDataSource = {
  name: 'crm_pg',
  label: '客戶 CRM 資料庫',
  builtin: false,
  tools: ['pg_query', 'pg_select'],
  table_arg: 'table',
  table: null,
  table_result: null,
  record_paths: ['$.rows[*]'],
  free_form_names: false,
  key_alias: {},
};

describe('dryRunDefaultsForRule', () => {
  it('uses the rule match_tool/match_args verbatim for json_path', () => {
    expect(dryRunDefaultsForRule(makeJsonPathRule(), [])).toEqual({
      tool: 'odoo_*',
      args: { model: 'crm.lead' },
    });
  });

  it('falls back to odoo_search/model for an unregistered or odoo db_field source', () => {
    expect(dryRunDefaultsForRule(makeDbFieldRule(), [])).toEqual({
      tool: 'odoo_search',
      args: { model: 'res.partner' },
    });
  });

  it('derives tool + table_arg from a registered custom data source', () => {
    const rule = makeDbFieldRule({ connector: 'crm_pg', fields: ['customers.name'] });
    expect(dryRunDefaultsForRule(rule, [CRM_PG_SOURCE])).toEqual({
      tool: 'pg_query',
      args: { table: 'customers' },
    });
  });
});

describe('summarizeDryRun', () => {
  it('splits hits into field-rule vs pattern-rule counts', () => {
    const result: RedactionDryRunResult = {
      hits: [
        { pointer: '#/0/name', rule_id: 'customer_master', category: 'DB_FIELD', token: 't1' },
        { pointer: '#/0/email', rule_id: 'email', category: 'EMAIL', token: 't2' },
        { pointer: '#/1/name', rule_id: 'customer_master', category: 'DB_FIELD', token: 't3' },
      ],
      token_count: 3,
      restored_ok: 3,
    };
    expect(summarizeDryRun(result, new Set(['customer_master']))).toEqual({
      totalHits: 3,
      restoredOk: 3,
      fieldRuleHits: 2,
      patternHits: 1,
    });
  });
});

// ── data source form ────────────────────────────────────────────────────

describe('data source form conversion + validation', () => {
  it('round-trips a from_arg-mode source', () => {
    const form = dataSourceToFormState(CRM_PG_SOURCE);
    expect(form.tableMode).toBe('from_arg');
    // `table` is omitted (undefined), not null — `RedactionDataSourceInput`'s
    // table_arg/table pair is optional-not-nullable; `toEqual` treats an
    // undefined property as equivalent to an absent one.
    expect(formStateToDataSourceDef(form)).toEqual({
      label: '客戶 CRM 資料庫',
      tools: ['pg_query', 'pg_select'],
      table_arg: 'table',
      record_paths: ['$.rows[*]'],
      key_alias: {},
    });
  });

  it('round-trips a fixed-table source with a key alias', () => {
    const source: RedactionDataSource = {
      name: 'odoo',
      label: 'Odoo ERP',
      builtin: true,
      tools: ['odoo_partner_search'],
      table_arg: null,
      table: 'res.partner',
      table_result: null,
      record_paths: ['$[*]', '$'],
      free_form_names: false,
      key_alias: { email: 'email_from' },
    };
    const form = dataSourceToFormState(source);
    expect(form.tableMode).toBe('fixed');
    expect(form.fixedTable).toBe('res.partner');
    expect(form.keyAlias).toEqual([{ key: 'email', value: 'email_from' }]);
    expect(formStateToDataSourceDef(form).table).toBe('res.partner');
    expect(formStateToDataSourceDef(form).table_arg).toBeUndefined();
  });

  it('defaults record_paths when the source specifies none', () => {
    const form = emptyDataSourceForm();
    expect(form.recordPaths).toEqual(['$.rows[*]', '$[*]', '$']);
  });

  it('rejects a builtin name and an empty tools list on create', () => {
    const form = { ...emptyDataSourceForm(), name: 'odoo' };
    expect(validateDataSourceForm(form, true)).toEqual({ name: 'builtin', tools: 'empty' });
    const good = { ...emptyDataSourceForm(), name: 'crm_pg', tools: ['pg_query'] };
    expect(isDataSourceFormValid(good, true)).toBe(true);
  });

  it('does not re-validate name uniqueness/builtin-ness when editing', () => {
    const form = { ...emptyDataSourceForm(), name: 'odoo', tools: ['x'] };
    expect(validateDataSourceForm(form, false)).toEqual({});
  });

  it('requires the active table-mode field to be filled', () => {
    const fromArgBlank = { ...emptyDataSourceForm(), name: 'crm_pg', tools: ['x'], tableArg: '' };
    expect(validateDataSourceForm(fromArgBlank, true).table).toBe('invalid');
    const fixedBlank = { ...emptyDataSourceForm(), name: 'crm_pg', tools: ['x'], tableMode: 'fixed' as const };
    expect(validateDataSourceForm(fixedBlank, true).table).toBe('invalid');
  });

  // ── §14.3 table_result / free_form_names ──────────────────────────────

  it('round-trips a table_result-mode source (the duduclaw_files shape)', () => {
    const source: RedactionDataSource = {
      name: 'duduclaw_files',
      label: '地端檔案',
      builtin: true,
      tools: ['csv_read', 'xlsx_read'],
      table_arg: null,
      table: null,
      table_result: '/table',
      record_paths: ['$.rows[*]'],
      free_form_names: true,
      key_alias: {},
    };
    const form = dataSourceToFormState(source);
    expect(form.tableMode).toBe('from_result');
    expect(form.tableResult).toBe('/table');
    expect(form.freeFormNames).toBe(true);
    const def = formStateToDataSourceDef(form);
    expect(def.table_result).toBe('/table');
    expect(def.table_arg).toBeUndefined();
    expect(def.table).toBeUndefined();
    expect(def.free_form_names).toBe(true);
  });

  it('defaults table_result to /table and free_form_names to unset (omitted) on a fresh form', () => {
    const form = emptyDataSourceForm();
    expect(form.tableResult).toBe('/table');
    expect(form.freeFormNames).toBe(false);
    expect(formStateToDataSourceDef({ ...form, tableMode: 'from_result' }).free_form_names).toBeUndefined();
  });

  it('sends free_form_names/table_result as undefined (never a literal false/value) — non-free-form sources stay byte-identical', () => {
    const form = dataSourceToFormState(CRM_PG_SOURCE);
    const def = formStateToDataSourceDef(form);
    // `toEqual` treats an `undefined`-valued property as equivalent to an
    // absent one — matching this file's existing convention for
    // `table_arg`/`table` (see the "round-trips a from_arg-mode source" test
    // above), not a `toHaveProperty` key-existence check.
    expect(def.free_form_names).toBeUndefined();
    expect(def.table_result).toBeUndefined();
    expect(def).toEqual({
      label: '客戶 CRM 資料庫',
      tools: ['pg_query', 'pg_select'],
      table_arg: 'table',
      record_paths: ['$.rows[*]'],
      key_alias: {},
    });
  });

  it('validates a from_result table mode requires a leading-slash pointer', () => {
    const blankPointer = { ...emptyDataSourceForm(), name: 'crm_pg', tools: ['x'], tableMode: 'from_result' as const, tableResult: '' };
    expect(validateDataSourceForm(blankPointer, true).table).toBe('invalid');
    const badPointer = { ...emptyDataSourceForm(), name: 'crm_pg', tools: ['x'], tableMode: 'from_result' as const, tableResult: 'table' };
    expect(validateDataSourceForm(badPointer, true).table).toBe('invalid');
    const good = { ...emptyDataSourceForm(), name: 'crm_pg', tools: ['x'], tableMode: 'from_result' as const, tableResult: '/table' };
    expect(isDataSourceFormValid(good, true)).toBe(true);
  });

  it('rejects duduclaw_files as a custom source name, alongside the original two builtins', () => {
    expect(BUILTIN_SOURCE_NAMES).toContain('duduclaw_files');
    const form = { ...emptyDataSourceForm(), name: 'duduclaw_files', tools: ['x'] };
    expect(validateDataSourceForm(form, true).name).toBe('builtin');
  });
});

// ── db_sources form ─────────────────────────────────────────────────────

const CRM_PG_DB_SOURCE: DbSourceSummary = {
  name: 'crm_pg',
  label: '客戶 CRM 資料庫',
  driver: 'postgres',
  allowed_tables: ['customers', 'orders'],
  max_rows: 500,
  timeout_ms: 8000,
  url_status: {
    configured: true,
    source: 'env',
    source_label: 'env:CRM_PG_URL',
    writable: true,
    residue: false,
  },
};

describe('splitUrlForUpsert', () => {
  it('routes a secret:// value to url_secret_ref', () => {
    expect(splitUrlForUpsert('secret://env/CRM_PG_URL')).toEqual({ url_secret_ref: 'secret://env/CRM_PG_URL' });
  });
  it('routes a plaintext value (a literal DSN or sqlite path) to url', () => {
    expect(splitUrlForUpsert('postgres://user:pass@host/db')).toEqual({ url: 'postgres://user:pass@host/db' });
  });
  it('returns neither key for a blank value — "keep the stored credential"', () => {
    expect(splitUrlForUpsert('')).toEqual({});
    expect(splitUrlForUpsert('   ')).toEqual({});
  });
});

describe('db source form conversion + validation', () => {
  it('dbSourceToFormState never prefills the credential — DbSourceSummary carries only url_status, never the value', () => {
    const form = dbSourceToFormState(CRM_PG_DB_SOURCE);
    expect(form.name).toBe('crm_pg');
    expect(form.label).toBe('客戶 CRM 資料庫');
    expect(form.driver).toBe('postgres');
    expect(form.url).toBe('');
    expect(form.allowedTables).toEqual(['customers', 'orders']);
    expect(form.maxRows).toBe(500);
    expect(form.timeoutMs).toBe(8000);
  });

  it('formStateToDbSourceUpsert splits a secret:// value into url_secret_ref', () => {
    const form = { ...dbSourceToFormState(CRM_PG_DB_SOURCE), url: 'secret://env/CRM_PG_URL' };
    expect(formStateToDbSourceUpsert(form)).toEqual({
      name: 'crm_pg',
      driver: 'postgres',
      label: '客戶 CRM 資料庫',
      url_secret_ref: 'secret://env/CRM_PG_URL',
      allowed_tables: ['customers', 'orders'],
      max_rows: 500,
      timeout_ms: 8000,
    });
  });

  it('formStateToDbSourceUpsert splits a plaintext value into url', () => {
    const form = { ...emptyDbSourceForm(), name: 'sqlite_local', url: '/var/data/local.db' };
    const upsert = formStateToDbSourceUpsert(form);
    expect(upsert.url).toBe('/var/data/local.db');
    expect(upsert.url_secret_ref).toBeUndefined();
  });

  it('formStateToDbSourceUpsert sends neither url key on a blank credential (edit: keep stored)', () => {
    const form = dbSourceToFormState(CRM_PG_DB_SOURCE); // url is always '' fresh off the list
    const upsert = formStateToDbSourceUpsert(form);
    expect(upsert.url).toBeUndefined();
    expect(upsert.url_secret_ref).toBeUndefined();
  });

  it('defaults label to the name when blank', () => {
    const form = { ...emptyDbSourceForm(), name: 'crm_pg', label: '' };
    expect(formStateToDbSourceUpsert(form).label).toBe('crm_pg');
  });

  it('formStateToDbSourceTestParams falls back to a placeholder name for an unsaved draft', () => {
    const params = formStateToDbSourceTestParams(emptyDbSourceForm());
    expect(params.name).toBe('preview');
    expect(params.label).toBeUndefined();
  });

  it('formStateToDbSourceTestParams carries the drafted name and split credential', () => {
    const form = { ...emptyDbSourceForm(), name: 'crm_pg', url: 'secret://env/X', allowedTables: ['*'] };
    const params = formStateToDbSourceTestParams(form);
    expect(params.name).toBe('crm_pg');
    expect(params.url_secret_ref).toBe('secret://env/X');
    expect(params.url).toBeUndefined();
  });

  it('requires a name and url only when creating; allowedTables is always required', () => {
    expect(validateDbSourceForm(emptyDbSourceForm(), true)).toEqual({
      name: 'invalid',
      url: 'empty',
      allowedTables: 'empty',
    });
    // Editing may leave url blank — it can never be prefilled, so blank means
    // "keep what's stored", not an error.
    const editingBlankUrl = { ...dbSourceToFormState(CRM_PG_DB_SOURCE) };
    expect(validateDbSourceForm(editingBlankUrl, false)).toEqual({});
    const editingNoTables = { ...dbSourceToFormState(CRM_PG_DB_SOURCE), allowedTables: [] };
    expect(validateDbSourceForm(editingNoTables, false)).toEqual({ allowedTables: 'empty' });
    const good = {
      ...emptyDbSourceForm(),
      name: 'crm_pg',
      url: 'secret://env/X',
      allowedTables: ['*'],
    };
    expect(isDbSourceFormValid(good, true)).toBe(true);
  });
});

describe('summarizeDbSourceTest', () => {
  it('reads only the canonical success/tables/message fields', () => {
    const result: DbSourceTestResult = {
      success: true,
      message: 'connected',
      tables: [
        { name: 'customers', columns: [{ name: 'id', type: 'integer' }] },
        { name: 'orders', columns: [{ name: 'id', type: 'integer' }] },
      ],
    };
    expect(summarizeDbSourceTest(result)).toEqual({ success: true, tableCount: 2, message: 'connected' });
  });

  it('derives tableCount from tables.length, not a deprecated table_count alias', () => {
    const result: DbSourceTestResult = { success: false, message: 'auth failed', tables: [] };
    expect(summarizeDbSourceTest(result)).toEqual({ success: false, tableCount: 0, message: 'auth failed' });
  });
});
