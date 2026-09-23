import type {
  RedactionFieldRule,
  RedactionFieldRuleInput,
  RedactionDataSource,
  RedactionDataSourceInput,
  RedactionRestoreScope,
  RedactionDryRunResult,
  RedactionConfig,
  RedactionUpdate,
  DbDriver,
  DbSourceSummary,
  DbSourceUpsert,
  DbSourceTestParams,
  DbSourceTestResult,
} from '@/lib/api';

// ── Redaction field rules / data sources — pure helpers (WP-W) ─────────────
//
// This file holds ONLY pure, side-effect-free functions and their types so the
// vitest suite in `redactionFieldRules.test.ts` can exercise them without a
// WebSocket or an IntlProvider. Anything that talks to the gateway (RPC calls)
// lives in the component files instead.
//
// `redaction.get`/`redaction.update`'s `data_sources` (DESIGN-redaction-field-
// rules-2026-09 §13.5 — `RedactionDataSource`/`RedactionDataSourceInput`) and
// the `db_sources.*` RPC family (§13.7, native SQL connector —
// `DbSourceSummary`/`DbSourceUpsert`/`DbSourceTestParams`/`DbSourceTestResult`)
// have both landed in `api.ts` (added in parallel by the Rust-side agents), so
// this file re-exports rather than re-declares those types; the
// `redactionDbSourcesApi.ts` local wrapper is gone — components call
// `api.dbSources.*` directly. A `db_field` rule's data source is referenced
// through the EXISTING `connector` field on `RedactionFieldRule`/
// `RedactionFieldRuleInput` — §13.5's "`connector` kept as an alias of
// `source`" resolved to `connector` staying the one wire field, not growing a
// parallel `source` field, so this file's form state calls the UI concept
// "source" but reads/writes it through `.connector`.

export type { RedactionDataSource, RedactionDataSourceInput, DbDriver };

// ── §14.3/§14.4 local stand-ins (WP-F1/WP-F2 land these in api.ts) ────────
//
// `table_result`/`free_form_names` (WP-F1) landed on `RedactionDataSource`/
// `RedactionDataSourceInput` in api.ts while this file was being written —
// `FreeFormDataSource`/`FreeFormDataSourceInput` are now plain aliases, kept
// only so this file's (and its callers') existing references don't need a
// rename; feel free to delete them and use `RedactionDataSource`/
// `RedactionDataSourceInput` directly.
//
// `data_file_guard` (WP-F2) has NOT landed on `RedactionConfig`/
// `RedactionUpdate` as of this file's last edit — TODO(api.ts):
// `DataFileGuardMode`/`RedactionConfigWithGuard`/`RedactionUpdateWithGuard`
// below are real local stand-ins until it does. A plain `RedactionConfig`
// value is structurally assignable to `RedactionConfigWithGuard` (the extra
// field is optional), so real API responses flow in without a cast, and a
// `RedactionUpdateWithGuard` value is assignable to a `RedactionUpdate`-typed
// call-site parameter for the same reason.
export type FreeFormDataSource = RedactionDataSource;
export type FreeFormDataSourceInput = RedactionDataSourceInput;

/** Data-file guard mode (§14.4) — gates whether the built-in `Read`/`Bash`
 *  tools can be used to route around de-identified `csv_read`/`xlsx_read`/
 *  `file_read`. TODO(api.ts): belongs on `RedactionConfig.data_file_guard` /
 *  `RedactionUpdate.data_file_guard` once WP-F2 lands it. */
export type DataFileGuardMode = 'on' | 'read_only' | 'off';
export const DATA_FILE_GUARD_MODES: readonly DataFileGuardMode[] = ['on', 'read_only', 'off'];

export type RedactionConfigWithGuard = RedactionConfig & { data_file_guard?: DataFileGuardMode };
export type RedactionUpdateWithGuard = RedactionUpdate & { data_file_guard?: DataFileGuardMode };

// ── constants ────────────────────────────────────────────────────────────

/** Odoo model suggestion chips for the simple db_field form (canvas screen 3).
 *  `labelId` resolves against `redaction.fieldRules.odooTable.*`. */
export const ODOO_TABLE_SUGGESTIONS: ReadonlyArray<{ value: string; labelId: string }> = [
  { value: 'res.partner', labelId: 'redaction.fieldRules.odooTable.res_partner' },
  { value: 'crm.lead', labelId: 'redaction.fieldRules.odooTable.crm_lead' },
  { value: 'sale.order', labelId: 'redaction.fieldRules.odooTable.sale_order' },
  { value: 'hr.employee', labelId: 'redaction.fieldRules.odooTable.hr_employee' },
  { value: 'account.move', labelId: 'redaction.fieldRules.odooTable.account_move' },
];

export const DEFAULT_RECORD_PATHS: readonly string[] = ['$.rows[*]', '$[*]', '$'];

/** `duduclaw_files` (§14) joined the reserved built-in registry names
 *  alongside the original two — an operator cannot create a custom source
 *  that shadows any of them. */
export const BUILTIN_SOURCE_NAMES: readonly string[] = ['odoo', 'duduclaw_db', 'duduclaw_files'];

// ── validation ───────────────────────────────────────────────────────────

const IDENTIFIER_RE = /^[a-z][a-z0-9_]*$/;
const TABLE_RE = /^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)*$/;
// eslint-disable-next-line no-control-regex -- deliberately matches C0 control chars + DEL
const CONTROL_CHAR_RE = /[\x00-\x1f\x7f]/;

export function isValidRuleId(id: string): boolean {
  return IDENTIFIER_RE.test(id.trim());
}

export function isValidDataSourceName(name: string): boolean {
  return IDENTIFIER_RE.test(name.trim());
}

/** `res.partner`, `hr_employee`, `customers` — dots allowed (Odoo models),
 *  optional (plain SQL tables). Mirrors the backend's relaxed §13.5 regex.
 *  Identifier-only — a `free_form_names` source uses
 *  `isValidFreeFormTableName` instead (see `isFreeFormSource`). */
export function isValidTableName(table: string): boolean {
  return TABLE_RE.test(table.trim());
}

/** A single field token: a lowercase identifier, or the `*` wildcard.
 *  Identifier-only — see `isValidFreeFormFieldToken` for the
 *  `free_form_names` case. */
export function isValidFieldToken(token: string): boolean {
  return token === '*' || IDENTIFIER_RE.test(token);
}

/** §14.3 free-form table name: any non-empty text without control chars or a
 *  literal `'` (the character that would break the backend's generated
 *  `['key']` path-segment quoting) — accepts CJK, spaces, and file
 *  extensions (`客戶清單.xlsx`, `customers.csv`). This is the ONLY relaxation
 *  a `free_form_names` source gets; everything else about the simple form's
 *  validation is unchanged. */
export function isValidFreeFormTableName(table: string): boolean {
  const t = table.trim();
  return t.length > 0 && !CONTROL_CHAR_RE.test(t) && !t.includes("'");
}

/** §14.3 free-form column/field name — same rule as the table, plus no `/`:
 *  the simple form always renders this joined into a `"table.field"` string,
 *  and a `/` in a column reads as a path separator to a human skimming the
 *  saved rule. `*` (the all-fields wildcard) is always accepted. */
export function isValidFreeFormFieldToken(token: string): boolean {
  if (token === '*') return true;
  const t = token.trim();
  return t.length > 0 && !CONTROL_CHAR_RE.test(t) && !t.includes("'") && !t.includes('/');
}

/** Loose JSON-pointer sanity check for `table_result` (§14.3) — same "fast,
 *  friendly, not the final word" role as `isValidPathExpr`; the backend's
 *  dry-compile is the real grammar authority. */
export function isValidResultPointer(pointer: string): boolean {
  const p = pointer.trim();
  return p.length > 1 && p.startsWith('/');
}

/** Whether a `db_field` rule's chosen source allows free-form table/column
 *  names: the built-in `duduclaw_files` source always does (files don't have
 *  identifier-shaped table names), and so does any registered data source
 *  whose entry carries `free_form_names: true`. Looked up by name rather
 *  than threaded through `FieldRuleFormState` so every validator here stays
 *  a pure function of `(form, dataSources)`. */
export function isFreeFormSource(source: string, dataSources: readonly RedactionDataSource[]): boolean {
  if (source.trim() === 'duduclaw_files') return true;
  const found = dataSources.find((s) => s.name === source);
  return (found as FreeFormDataSource | undefined)?.free_form_names === true;
}

/** Loose, fast, friendly front-end check — full grammar validation is the
 *  backend's job at dry-compile time. Just catches the obviously-empty /
 *  no-leading-`$` cases before a round trip. */
export function isValidPathExpr(path: string): boolean {
  const p = path.trim();
  return p.length > 1 && p.startsWith('$');
}

/** Exact tool name or a trailing-`*` prefix glob. Empty = "any tool". */
export function isValidMatchToolGlob(tool: string): boolean {
  const t = tool.trim();
  if (!t) return true;
  return /^[A-Za-z0-9_.]+\*?$/.test(t);
}

// ── generic token / key-value row helpers (shared by every token input) ───

export function addToken(tokens: readonly string[], raw: string): string[] {
  const t = raw.trim();
  if (!t || tokens.includes(t)) return [...tokens];
  return [...tokens, t];
}

export function removeToken(tokens: readonly string[], target: string): string[] {
  return tokens.filter((t) => t !== target);
}

export interface KvRow {
  key: string;
  value: string;
}

export function addKvRow(rows: readonly KvRow[]): KvRow[] {
  return [...rows, { key: '', value: '' }];
}

export function updateKvRow(rows: readonly KvRow[], index: number, patch: Partial<KvRow>): KvRow[] {
  return rows.map((r, i) => (i === index ? { ...r, ...patch } : r));
}

export function removeKvRow(rows: readonly KvRow[], index: number): KvRow[] {
  return rows.filter((_, i) => i !== index);
}

export function kvRowsToRecord(rows: readonly KvRow[]): Record<string, string> {
  return Object.fromEntries(
    rows.filter((r) => r.key.trim()).map((r) => [r.key.trim(), r.value] as const),
  );
}

export function recordToKvRows(record: Record<string, string> | undefined): KvRow[] {
  return Object.entries(record ?? {}).map(([key, value]) => ({ key, value }));
}

// ── db_field <-> "table.field" list conversion ─────────────────────────────

/** Split a `db_field` rule's flat `"table.field"` list into per-table groups,
 *  preserving first-seen table order. The LAST dot separates table from
 *  field (mirrors the backend's `db_field.rs` split), so a dotted model name
 *  like `res.partner` stays intact as the table. Malformed entries (no dot)
 *  are skipped defensively — the backend's dry-compile would have rejected
 *  them before they ever reached storage. */
export function groupDbFields(
  fields: readonly string[],
): Array<{ table: string; fieldTokens: string[] }> {
  const order: string[] = [];
  const byTable = new Map<string, string[]>();
  for (const entry of fields) {
    const dot = entry.lastIndexOf('.');
    if (dot === -1) continue;
    const table = entry.slice(0, dot);
    const field = entry.slice(dot + 1);
    if (!byTable.has(table)) {
      byTable.set(table, []);
      order.push(table);
    }
    byTable.get(table)!.push(field);
  }
  return order.map((table) => ({ table, fieldTokens: byTable.get(table)! }));
}

/** Build one table's `"table.field"` entries. `*` in `fieldTokens` collapses
 *  the whole group to a single `table.*` entry (all-fields shorthand). */
export function buildDbFields(table: string, fieldTokens: readonly string[]): string[] {
  const t = table.trim();
  if (!t || fieldTokens.length === 0) return [];
  if (fieldTokens.includes('*')) return [`${t}.*`];
  return fieldTokens.map((f) => `${t}.${f}`);
}

// ── field rule form state ───────────────────────────────────────────────

export type FieldRuleMode = 'db_field' | 'json_path';

export interface FieldRuleFormState {
  mode: FieldRuleMode;
  /** Rule id (the `[redaction.rules.<id>]` key). Only editable when creating. */
  id: string;
  category: string;
  restoreScope: RedactionRestoreScope;
  priority: number;
  crossSessionStable: boolean;
  // db_field (simple mode) — the form edits ONE table's fields at a time.
  source: string;
  table: string;
  fieldTokens: string[];
  /** Raw `"table.field"` entries for tables OTHER than `table` — preserved
   *  byte-for-byte across a simple-mode edit so a multi-table rule authored
   *  in TOML (or by a previous edit) doesn't silently lose its other tables
   *  when this form only shows one table editor at a time. */
  otherTableFields: string[];
  // json_path (advanced mode)
  matchTool: string;
  matchArgs: KvRow[];
  paths: string[];
  excludeKeys: string[];
}

export function emptyFieldRuleForm(): FieldRuleFormState {
  return {
    mode: 'db_field',
    id: '',
    category: 'DB_FIELD',
    restoreScope: { kind: 'owner' },
    priority: 50,
    crossSessionStable: true,
    source: 'odoo',
    table: '',
    fieldTokens: [],
    otherTableFields: [],
    matchTool: '',
    matchArgs: [],
    paths: [],
    excludeKeys: [],
  };
}

/** Convert a stored rule (as `redaction.get` renders it) into editable form
 *  state. `rule.id` is required — call sites pass a `RedactionFieldRule`
 *  straight from `config.field_rules` (it already carries `id`). */
export function fieldRuleToFormState(rule: RedactionFieldRule): FieldRuleFormState {
  const base = emptyFieldRuleForm();
  const common = {
    id: rule.id,
    category: rule.category,
    restoreScope: rule.restore_scope,
    priority: rule.priority,
    crossSessionStable: rule.cross_session_stable,
  };
  if (rule.kind === 'db_field') {
    const groups = groupDbFields(rule.fields ?? []);
    const [first, ...rest] = groups;
    return {
      ...base,
      ...common,
      mode: 'db_field',
      source: rule.connector ?? 'odoo',
      table: first?.table ?? '',
      fieldTokens: first?.fieldTokens ?? [],
      otherTableFields: rest.flatMap((g) => buildDbFields(g.table, g.fieldTokens)),
    };
  }
  return {
    ...base,
    ...common,
    mode: 'json_path',
    matchTool: rule.match_tool ?? '',
    matchArgs: recordToKvRows(rule.match_args),
    paths: rule.paths ?? [],
    excludeKeys: rule.exclude_keys ?? [],
  };
}

/** Convert form state back into the `redaction.update({ field_rules })` body
 *  (the map key carries the id, so it's omitted here). The form's "資料來源"
 *  concept is written through the wire's `connector` field — see this file's
 *  header comment on why there is no separate `source` field. */
export function formStateToRuleInput(form: FieldRuleFormState): RedactionFieldRuleInput {
  const common = {
    category: form.category.trim(),
    restore_scope: form.restoreScope,
    priority: form.priority,
    cross_session_stable: form.crossSessionStable,
  };
  if (form.mode === 'db_field') {
    return {
      ...common,
      type: 'db_field',
      connector: form.source.trim() || 'odoo',
      fields: [...form.otherTableFields, ...buildDbFields(form.table, form.fieldTokens)],
    };
  }
  return {
    ...common,
    type: 'json_path',
    match_tool: form.matchTool.trim() || null,
    match_args: kvRowsToRecord(form.matchArgs),
    paths: form.paths.filter((p) => p.trim()),
    exclude_keys: form.excludeKeys.filter((k) => k.trim()),
  };
}

export interface FieldRuleFormErrors {
  id?: 'invalid';
  table?: 'invalid';
  fields?: 'empty' | 'invalid';
  paths?: 'empty' | 'invalid';
}

/** Client-side pre-flight check — fast, friendly feedback before the round
 *  trip. The backend's dry-compile on save remains the actual authority;
 *  this only blocks obviously-incomplete input from being submitted.
 *
 *  `dataSources` (default `[]`, so every existing 2-arg call site keeps its
 *  original identifier-only behaviour byte-for-byte) decides whether
 *  `form.source` is a `free_form_names` source — if so, the table/field
 *  tokens are validated with the relaxed §14.3 rules instead. */
export function validateFieldRuleForm(
  form: FieldRuleFormState,
  isNew: boolean,
  dataSources: readonly RedactionDataSource[] = [],
): FieldRuleFormErrors {
  const errors: FieldRuleFormErrors = {};
  if (isNew && !isValidRuleId(form.id)) errors.id = 'invalid';
  if (form.mode === 'db_field') {
    const freeForm = isFreeFormSource(form.source, dataSources);
    const tableOk = freeForm ? isValidFreeFormTableName(form.table) : isValidTableName(form.table);
    if (!tableOk) errors.table = 'invalid';
    if (form.fieldTokens.length === 0) errors.fields = 'empty';
    else {
      const tokenOk = freeForm ? isValidFreeFormFieldToken : isValidFieldToken;
      if (!form.fieldTokens.every(tokenOk)) errors.fields = 'invalid';
    }
  } else {
    if (form.paths.length === 0) errors.paths = 'empty';
    else if (!form.paths.every(isValidPathExpr)) errors.paths = 'invalid';
  }
  return errors;
}

export function isFieldRuleFormValid(
  form: FieldRuleFormState,
  isNew: boolean,
  dataSources: readonly RedactionDataSource[] = [],
): boolean {
  return Object.keys(validateFieldRuleForm(form, isNew, dataSources)).length === 0;
}

// ── coverage summary (plain-language rendering of a stored rule) ──────────

export type CoverageLine =
  | { kind: 'dbField'; table: string; allFields: boolean; fieldTokens: string[] }
  | { kind: 'source'; source: string }
  | { kind: 'jsonPathTool'; tool: string | null; matchArgs: Array<[string, string]> }
  | { kind: 'jsonPathPaths'; paths: string[] }
  | { kind: 'restoreScope'; scope: RedactionRestoreScope };

/** Structured coverage lines for one rule's list-row summary (canvas screen
 *  1: `res.partner → name、street、comment` / `hr.employee.* → 全部欄位（保留
 *  id）` / `套用工具 odoo_*，且引數 model = crm.lead`). Returns data, not
 *  strings, so the component localizes via `intl.formatMessage` and this
 *  function stays testable without an IntlProvider. */
export function summarizeFieldRuleCoverage(rule: RedactionFieldRule): CoverageLine[] {
  const lines: CoverageLine[] = [];
  if (rule.kind === 'db_field') {
    const source = rule.connector ?? 'odoo';
    if (source !== 'odoo') lines.push({ kind: 'source', source });
    for (const g of groupDbFields(rule.fields ?? [])) {
      lines.push({
        kind: 'dbField',
        table: g.table,
        allFields: g.fieldTokens.includes('*'),
        fieldTokens: g.fieldTokens,
      });
    }
  } else {
    lines.push({
      kind: 'jsonPathTool',
      tool: rule.match_tool ?? null,
      matchArgs: Object.entries(rule.match_args ?? {}),
    });
    lines.push({ kind: 'jsonPathPaths', paths: rule.paths ?? [] });
  }
  lines.push({ kind: 'restoreScope', scope: rule.restore_scope });
  return lines;
}

// ── dry-run defaults ────────────────────────────────────────────────────

/** Pick the tool + args to prefill the dry-run panel with when it's opened
 *  from a rule row, so the operator only has to paste a sample. Mirrors the
 *  CLI verify default (`odoo_search` / `model=<table>`) when the rule's
 *  source isn't registered or carries no tools. */
export function dryRunDefaultsForRule(
  rule: RedactionFieldRule,
  dataSources: readonly RedactionDataSource[],
): { tool: string; args: Record<string, string> } {
  if (rule.kind === 'json_path') {
    return {
      tool: (rule.match_tool && rule.match_tool.trim()) || 'odoo_search',
      args: { ...(rule.match_args ?? {}) },
    };
  }
  const sourceName = rule.connector ?? 'odoo';
  const table = groupDbFields(rule.fields ?? [])[0]?.table ?? '';
  const source = dataSources.find((s) => s.name === sourceName);
  if (source && source.tools.length > 0) {
    const tool = source.tools[0].replace(/\*+$/, '') || source.tools[0];
    const args: Record<string, string> = {};
    if (source.table_arg) args[source.table_arg] = table;
    return { tool, args };
  }
  return { tool: 'odoo_search', args: table ? { model: table } : {} };
}

export interface DryRunSummary {
  totalHits: number;
  restoredOk: number;
  fieldRuleHits: number;
  patternHits: number;
}

/** Split `redaction.dry_run`'s flat hit list into "this field rule" vs
 *  "pattern rules" counts for the summary chips (canvas screen 6: `欄位規則
 *  4 處 · 樣態規則 2 處`). */
export function summarizeDryRun(
  result: RedactionDryRunResult,
  fieldRuleIds: ReadonlySet<string>,
): DryRunSummary {
  const fieldRuleHits = result.hits.filter((h) => fieldRuleIds.has(h.rule_id)).length;
  return {
    totalHits: result.hits.length,
    restoredOk: result.restored_ok,
    fieldRuleHits,
    patternHits: result.hits.length - fieldRuleHits,
  };
}

// ── data source form state (§13.5 registry + §14.3 table_result/free_form) ─

/** `from_result` (§14.3) is the third, mutually-exclusive way to decide the
 *  table: read it from the tool's own RESULT via a JSON pointer, rather than
 *  from an argument (`from_arg`) or a fixed constant (`fixed`). */
export type DataSourceTableMode = 'from_arg' | 'fixed' | 'from_result';

export interface DataSourceFormState {
  name: string;
  label: string;
  tools: string[];
  tableMode: DataSourceTableMode;
  tableArg: string;
  fixedTable: string;
  /** `from_result` mode's JSON pointer into the tool's result, e.g. `/table`. */
  tableResult: string;
  recordPaths: string[];
  keyAlias: KvRow[];
  /** §14.3 — table/column names for this source accept free text (CJK,
   *  spaces, file extensions) instead of plain identifiers. */
  freeFormNames: boolean;
}

export function emptyDataSourceForm(): DataSourceFormState {
  return {
    name: '',
    label: '',
    tools: [],
    tableMode: 'from_arg',
    tableArg: 'table',
    fixedTable: '',
    tableResult: '/table',
    recordPaths: [...DEFAULT_RECORD_PATHS],
    keyAlias: [],
    freeFormNames: false,
  };
}

export function dataSourceToFormState(source: FreeFormDataSource): DataSourceFormState {
  const tableMode: DataSourceTableMode = source.table
    ? 'fixed'
    : source.table_result
      ? 'from_result'
      : 'from_arg';
  return {
    name: source.name,
    label: source.label,
    tools: [...source.tools],
    tableMode,
    tableArg: source.table_arg ?? 'table',
    fixedTable: source.table ?? '',
    tableResult: source.table_result ?? '/table',
    recordPaths: source.record_paths.length > 0 ? [...source.record_paths] : [...DEFAULT_RECORD_PATHS],
    keyAlias: recordToKvRows(source.key_alias),
    freeFormNames: source.free_form_names === true,
  };
}

/** `RedactionDataSourceInput`'s `table_arg`/`table`/`table_result` are
 *  optional-not-nullable and mutually exclusive — the two unused thirds are
 *  OMITTED (`undefined`), never sent as `null`. `free_form_names` is omitted
 *  (not sent as explicit `false`) when off, so a non-free-form source's
 *  wire shape is byte-identical to before this field existed. */
export function formStateToDataSourceDef(form: DataSourceFormState): FreeFormDataSourceInput {
  return {
    label: form.label.trim() || form.name.trim(),
    tools: form.tools,
    table_arg: form.tableMode === 'from_arg' ? form.tableArg.trim() || 'table' : undefined,
    table: form.tableMode === 'fixed' ? form.fixedTable.trim() || undefined : undefined,
    table_result: form.tableMode === 'from_result' ? form.tableResult.trim() || '/table' : undefined,
    record_paths: form.recordPaths.filter((p) => p.trim()),
    key_alias: kvRowsToRecord(form.keyAlias),
    free_form_names: form.freeFormNames ? true : undefined,
  };
}

export interface DataSourceFormErrors {
  name?: 'invalid' | 'builtin';
  tools?: 'empty';
  table?: 'invalid';
}

export function validateDataSourceForm(form: DataSourceFormState, isNew: boolean): DataSourceFormErrors {
  const errors: DataSourceFormErrors = {};
  if (isNew) {
    if (!isValidDataSourceName(form.name)) errors.name = 'invalid';
    else if (BUILTIN_SOURCE_NAMES.includes(form.name.trim())) errors.name = 'builtin';
  }
  if (form.tools.length === 0) errors.tools = 'empty';
  if (form.tableMode === 'from_arg') {
    if (!form.tableArg.trim()) errors.table = 'invalid';
  } else if (form.tableMode === 'fixed') {
    if (!form.fixedTable.trim()) errors.table = 'invalid';
  } else if (!isValidResultPointer(form.tableResult)) {
    errors.table = 'invalid';
  }
  return errors;
}

export function isDataSourceFormValid(form: DataSourceFormState, isNew: boolean): boolean {
  return Object.keys(validateDataSourceForm(form, isNew)).length === 0;
}

// ── db_sources form state (§13.7 native SQL connector) ─────────────────

export interface DbSourceFormState {
  name: string;
  label: string;
  driver: DbDriver;
  /** `SecretSourceField`'s single composed value — plaintext or a
   *  `secret://…` URI. Blank means "leave the stored credential alone" on
   *  edit (`DbSourceSummary.url_status` only ever reports the credential's
   *  ORIGIN, never its value, so there is nothing to prefill on edit —
   *  see `dbSourceToFormState`). `splitUrlForUpsert` decides at write time
   *  whether this becomes `url` or `url_secret_ref`. */
  url: string;
  allowedTables: string[];
  maxRows: number;
  timeoutMs: number;
}

export function emptyDbSourceForm(): DbSourceFormState {
  return {
    name: '',
    label: '',
    driver: 'postgres',
    url: '',
    allowedTables: [],
    maxRows: 200,
    timeoutMs: 10000,
  };
}

/** `db_sources.list` never returns the connection string (only
 *  `url_status`, which describes where the credential lives, not its
 *  value) — so an edit form always opens with `url: ''`, exactly like every
 *  other credential field in this dashboard. */
export function dbSourceToFormState(source: DbSourceSummary): DbSourceFormState {
  return {
    name: source.name,
    label: source.label,
    driver: source.driver,
    url: '',
    allowedTables: [...source.allowed_tables],
    maxRows: source.max_rows,
    timeoutMs: source.timeout_ms,
  };
}

/** `DbSourceUpsert`/`DbSourceTestParams` split a credential into `url`
 *  (literal — a real DSN, or a SQLite file path) vs `url_secret_ref` (a
 *  `secret://…` reference); `SecretSourceField` only ever hands back one
 *  composed string, so this decides which half it belongs in. Blank ⇒
 *  neither key is set, which upsert treats as "keep the stored credential". */
export function splitUrlForUpsert(value: string): { url?: string; url_secret_ref?: string } {
  const v = value.trim();
  if (!v) return {};
  return v.startsWith('secret://') ? { url_secret_ref: v } : { url: v };
}

export function formStateToDbSourceUpsert(form: DbSourceFormState): DbSourceUpsert {
  return {
    name: form.name.trim(),
    driver: form.driver,
    label: form.label.trim() || form.name.trim(),
    ...splitUrlForUpsert(form.url),
    allowed_tables: form.allowedTables,
    max_rows: form.maxRows,
    timeout_ms: form.timeoutMs,
  };
}

/** Inline test-before-save params for a fresh/edited (not yet saved) form.
 *  `name` is required by the RPC even for a preview — falls back to a
 *  placeholder so an operator can test before settling on a final name. */
export function formStateToDbSourceTestParams(form: DbSourceFormState): DbSourceTestParams {
  return {
    name: form.name.trim() || 'preview',
    driver: form.driver,
    label: form.label.trim() || form.name.trim() || undefined,
    ...splitUrlForUpsert(form.url),
    allowed_tables: form.allowedTables,
    max_rows: form.maxRows,
    timeout_ms: form.timeoutMs,
  };
}

export interface DbSourceFormErrors {
  name?: 'invalid';
  url?: 'empty';
  allowedTables?: 'empty';
}

/** `url` is required only when creating — editing may leave it blank to keep
 *  the already-stored credential (it can never be prefilled, see
 *  `dbSourceToFormState`), so a blank field on edit is not an error. */
export function validateDbSourceForm(form: DbSourceFormState, isNew: boolean): DbSourceFormErrors {
  const errors: DbSourceFormErrors = {};
  if (isNew) {
    if (!isValidDataSourceName(form.name)) errors.name = 'invalid';
    if (!form.url.trim()) errors.url = 'empty';
  }
  if (form.allowedTables.length === 0) errors.allowedTables = 'empty';
  return errors;
}

export function isDbSourceFormValid(form: DbSourceFormState, isNew: boolean): boolean {
  return Object.keys(validateDbSourceForm(form, isNew)).length === 0;
}

/** `DbSourceTestResult`'s canonical fields only — `ok`/`table_count`/`error`
 *  are deprecated aliases being removed from the backend, so this file never
 *  reads them. */
export interface DbSourceTestOutcome {
  success: boolean;
  tableCount: number;
  message: string;
}

export function summarizeDbSourceTest(result: DbSourceTestResult): DbSourceTestOutcome {
  return { success: result.success, tableCount: result.tables.length, message: result.message };
}
