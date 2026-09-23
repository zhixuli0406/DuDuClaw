import type {
  RedactionDataSource,
  RedactionFieldRule,
  RedactionEgressRule,
  DbSourceSummary,
} from '@/lib/api';
import type { DataSourceFormState } from './redactionFieldRules';

// ── "外部系統與資料來源" merged card — pure helpers (DESIGN-redaction-field-
// rules-2026-09 §15) ────────────────────────────────────────────────────
//
// This file holds the pure, side-effect-free logic behind the canvas screen
// 9/10 redesign: deriving a `tool_egress` key from a data source's tools
// (never shown to the operator — §15.1), assembling the merged card's rows
// from three independent RPCs (`redaction.get`, `db_sources.list`,
// `odoo.status`), and counting/filtering `field_rules` by the system they
// belong to. Component files (`RedactionSystemsCard.tsx`,
// `RedactionSourceWizard.tsx`) own all the RPC calls and React state; this
// file is exercised directly by `redactionSystems.test.ts` without a
// WebSocket or an IntlProvider, matching `redactionFieldRules.ts`'s split.

// ── egress key derivation (§15.1, §15.3) ────────────────────────────────

/** Canonical Odoo write-back key. `odoo_search` / `odoo_partner_search` /
 *  etc. are all underscore-joined, so a prefix-glob needs the underscore. */
export const ODOO_EGRESS_KEY = 'odoo_*';

/** The original (buggy) key — a trailing-`*` prefix-glob only matches tool
 *  names that literally start with `odoo.`, which none of the real Odoo
 *  tools do (§15.3). The UI never writes this key again, but a config saved
 *  before this fix may still have it, so every read falls back to it and
 *  every "is this key already accounted for" check treats it as belonging
 *  to the Odoo row (never surfaced as a mystery leftover). */
export const ODOO_LEGACY_EGRESS_KEY = 'odoo.*';

/** The part of a tool name before its first dot, including the dot
 *  (`"crm_legacy.list"` → `"crm_legacy."`), ignoring a trailing glob star
 *  first (`"crm_legacy.*"` → `"crm_legacy."` too, not `"crm_legacy.*."`).
 *  `null` when the tool has no dot (no server namespace to share). */
function toolPrefix(tool: string): string | null {
  const bare = tool.endsWith('*') ? tool.slice(0, -1) : tool;
  const dot = bare.indexOf('.');
  return dot === -1 ? null : bare.slice(0, dot + 1);
}

/** Derive the `tool_egress` key(s) a custom data source's write-back policy
 *  is stored under (§15.1): when every tool shares one `<server>.` prefix
 *  (the shape a `duduclaw mcp-proxy`-fronted server's tools take), they
 *  collapse to a single `<server>.*` glob; otherwise each tool gets its own
 *  exact-match key. Every key returned here is written the SAME rule on
 *  save, so from the operator's chair it reads as "one policy" even when
 *  the wire has several keys. Never shown to the operator (§15.1: "使用者
 *  永遠不看到 key"). Empty/blank tool tokens are dropped defensively. */
export function deriveCustomEgressKeys(tools: readonly string[]): string[] {
  const cleaned = tools.map((t) => t.trim()).filter(Boolean);
  if (cleaned.length === 0) return [];
  const prefixes = cleaned.map(toolPrefix);
  const first = prefixes[0];
  if (first !== null && prefixes.every((p) => p === first)) return [`${first}*`];
  return Array.from(new Set(cleaned));
}

/** Derive the egress key(s) for any listed (non-built-in) system row. Odoo
 *  is the one caller that should reach for `ODOO_EGRESS_KEY` directly (it
 *  isn't a `data_sources` entry the operator can edit) — this helper is for
 *  custom tool-bound sources. */
export function deriveEgressKeysForSource(source: Pick<RedactionDataSource, 'tools'>): string[] {
  return deriveCustomEgressKeys(source.tools);
}

/** Read the first present key's rule — every write here targets every
 *  derived key with the identical rule, so in the common case they never
 *  disagree. If an operator hand-edited config.toml so they DO disagree,
 *  showing the first found key is a safe, non-crashing default. `null` =
 *  nothing saved yet (the row renders the picker's own "deny" default). */
export function readEgressForKeys(
  toolEgress: Record<string, RedactionEgressRule>,
  keys: readonly string[],
): RedactionEgressRule | null {
  for (const k of keys) {
    if (toolEgress[k]) return toolEgress[k];
  }
  return null;
}

/** Build a `redaction.update({ tool_egress })` patch writing ONE rule to
 *  every derived key at once (or clearing all of them when `rule` is
 *  `null`, e.g. on delete). */
export function buildEgressPatch(
  keys: readonly string[],
  rule: RedactionEgressRule | null,
): Record<string, RedactionEgressRule | null> {
  const patch: Record<string, RedactionEgressRule | null> = {};
  for (const k of keys) patch[k] = rule;
  return patch;
}

/** Every `tool_egress` key attributed to a row the merged card already
 *  lists: the Odoo row (canonical key + the legacy alias, so an
 *  unmigrated config doesn't show a phantom "other rule"), plus every
 *  non-built-in data source's derived key(s). Anything left over is a
 *  leftover from before this registry-driven UI existed. */
export function attributedEgressKeys(dataSources: readonly RedactionDataSource[]): Set<string> {
  const keys = new Set<string>([ODOO_EGRESS_KEY, ODOO_LEGACY_EGRESS_KEY]);
  for (const s of dataSources) {
    if (s.builtin) continue;
    for (const k of deriveCustomEgressKeys(s.tools)) keys.add(k);
  }
  return keys;
}

/** `tool_egress` entries not attributable to any row the merged card lists —
 *  rendered in the card's "其他寫回規則（進階）" fold so nothing an operator
 *  saved silently disappears (§15.1). */
export function unattributedEgressEntries(
  toolEgress: Record<string, RedactionEgressRule>,
  dataSources: readonly RedactionDataSource[],
): Array<[string, RedactionEgressRule]> {
  const attributed = attributedEgressKeys(dataSources);
  return Object.entries(toolEgress).filter(([k]) => !attributed.has(k));
}

// ── rule counting / filtering (§15.1 "N 條欄位規則") ─────────────────────

/** The system name a `db_field` rule's fields resolve against — `null` for
 *  `json_path` rules, which have no single owning system. Every DB
 *  connection (regardless of which specific `db_sources` entry) resolves
 *  through the single generic `duduclaw_db` built-in unless the operator
 *  paired it with a same-named custom `data_sources` entry (in which case
 *  that entry's own name is what `connector` carries — this function
 *  doesn't need to special-case that: it just reads whatever is stored). */
export function resolveRuleSourceName(rule: RedactionFieldRule): string | null {
  if (rule.kind !== 'db_field') return null;
  return rule.connector ?? 'odoo';
}

export function countFieldRulesForSource(fieldRules: readonly RedactionFieldRule[], sourceName: string): number {
  return fieldRules.filter((r) => resolveRuleSourceName(r) === sourceName).length;
}

/** `source === null` returns every rule unfiltered — the "no filter active"
 *  case for the field-rules card's chip. */
export function filterFieldRulesBySource(
  fieldRules: readonly RedactionFieldRule[],
  source: string | null,
): RedactionFieldRule[] {
  if (!source) return [...fieldRules];
  return fieldRules.filter((r) => resolveRuleSourceName(r) === source);
}

// ── row assembly (§15.1) ─────────────────────────────────────────────────

export type SystemRow =
  | { kind: 'odoo'; connected: boolean; ruleCount: number; egressKeys: string[]; egress: RedactionEgressRule | null }
  | { kind: 'db'; db: DbSourceSummary; ruleCount: number }
  | {
      kind: 'custom';
      source: RedactionDataSource;
      ruleCount: number;
      egressKeys: string[];
      egress: RedactionEgressRule | null;
    }
  | { kind: 'files'; ruleCount: number };

/** Assemble the merged card's rows: Odoo first (always shown — whether it's
 *  connected is plumbing state, not something the operator adds/removes
 *  here), then every non-built-in `data_sources` entry, then every
 *  `db_sources` connection not already covered by a same-named
 *  `data_sources` entry (the opt-in per-connection pairing convention —
 *  defensive against double-listing, mirrors the pre-existing
 *  `RedactionDataSourcesCard.mergeRows` behaviour), then the fixed,
 *  un-configurable "地端檔案" row last. The three built-in registry names
 *  (`odoo` / `duduclaw_db` / `duduclaw_files`) never appear as their own
 *  row — they're the invisible plumbing the Odoo / DB / files rows resolve
 *  through (§15.1). */
export function buildSystemRows(
  fieldRules: readonly RedactionFieldRule[],
  dataSources: readonly RedactionDataSource[],
  dbSources: readonly DbSourceSummary[],
  toolEgress: Record<string, RedactionEgressRule>,
  odooConnected: boolean,
): SystemRow[] {
  const rows: SystemRow[] = [
    {
      kind: 'odoo',
      connected: odooConnected,
      ruleCount: countFieldRulesForSource(fieldRules, 'odoo'),
      egressKeys: [ODOO_EGRESS_KEY],
      egress: readEgressForKeys(toolEgress, [ODOO_EGRESS_KEY, ODOO_LEGACY_EGRESS_KEY]),
    },
  ];

  const pairedDbNames = new Set<string>();
  for (const s of dataSources) {
    if (s.builtin) continue; // odoo / duduclaw_db / duduclaw_files are invisible plumbing
    pairedDbNames.add(s.name);
    const egressKeys = deriveEgressKeysForSource(s);
    rows.push({
      kind: 'custom',
      source: s,
      ruleCount: countFieldRulesForSource(fieldRules, s.name),
      egressKeys,
      egress: readEgressForKeys(toolEgress, egressKeys),
    });
  }

  const dbRuleCount = countFieldRulesForSource(fieldRules, 'duduclaw_db');
  for (const db of dbSources) {
    if (pairedDbNames.has(db.name)) continue; // already rendered as its own 'custom' row
    rows.push({ kind: 'db', db, ruleCount: dbRuleCount });
  }

  rows.push({ kind: 'files', ruleCount: countFieldRulesForSource(fieldRules, 'duduclaw_files') });
  return rows;
}

// ── custom source "讀進來" summary (reuses `redaction.dataSources.coverage.*`
// i18n keys — components render these as data, same split as
// `summarizeFieldRuleCoverage` in redactionFieldRules.ts) ─────────────────

export type CustomSourceReadInLine =
  | { kind: 'tools'; tools: string[] }
  | { kind: 'tableArg'; arg: string }
  | { kind: 'tableFixed'; table: string }
  | { kind: 'tableResult'; pointer: string }
  | { kind: 'freeFormNames' };

export function describeCustomSourceReadIn(source: RedactionDataSource): CustomSourceReadInLine[] {
  const lines: CustomSourceReadInLine[] = [{ kind: 'tools', tools: source.tools }];
  if (source.table) lines.push({ kind: 'tableFixed', table: source.table });
  else if (source.table_result) lines.push({ kind: 'tableResult', pointer: source.table_result });
  else if (source.table_arg) lines.push({ kind: 'tableArg', arg: source.table_arg });
  if (source.free_form_names) lines.push({ kind: 'freeFormNames' });
  return lines;
}

// ── wizard helpers (§15.2, and the 2026-09 "來源名稱" field removal) ─────
//
// The wizard no longer asks the operator to type a source/db id at all —
// it derives one automatically from 顯示名稱 and shows it read-only under
// "進階設定" as "系統識別碼". `slugifyDataSourceName` is the derivation;
// `deriveDataSourceId` is the wizard-level policy on top of it (auto-derive
// while creating, freeze once saved).

/** `'source'` → a `data_sources` key (the tool-bound custom source type);
 *  `'db'` → a `db_sources` connection name. Only used to pick the fallback
 *  id's prefix below — both charsets are otherwise identical. */
export type DataSourceIdKind = 'source' | 'db';

/** Deterministic (never random, never time-based) 6-lowercase-hex-digit
 *  fingerprint of a string — FNV-1a 32-bit, truncated. This is a display
 *  disambiguator, not a security boundary: it only needs to be stable
 *  (same input ⇒ same output, every call, every reload) and short. */
function stableHashHex6(input: string): string {
  let hash = 0x811c9dc5;
  for (let i = 0; i < input.length; i++) {
    hash ^= input.charCodeAt(i);
    hash = Math.imul(hash, 0x01000193);
  }
  return (hash >>> 0).toString(16).padStart(8, '0').slice(-6);
}

/** Append `_2`, `_3`, … until `candidate` no longer collides with anything
 *  in `existingIds`, re-truncating the base on every attempt so a suffixed
 *  id never exceeds `maxLen` (mirrors the backend's identifier length
 *  cap — a truncated-then-suffixed id is still capped, not capped-then-
 *  overflowed by the suffix). */
function disambiguateSlug(candidate: string, existingIds: ReadonlySet<string>, maxLen: number): string {
  if (!existingIds.has(candidate)) return candidate;
  for (let n = 2; ; n++) {
    const suffix = `_${n}`;
    const attempt = `${candidate.slice(0, Math.max(1, maxLen - suffix.length))}${suffix}`;
    if (!existingIds.has(attempt)) return attempt;
  }
}

/** Auto-slug a display label into a source/db-connection id — the
 *  identifier the wizard writes to `data_sources`/`db_sources` and the only
 *  thing shown read-only as "系統識別碼". The operator never types this
 *  directly (that field was removed — it was too technical: "小寫英數與
 *  底線，規則裡用 source = \"crm_pg\" 引用" meant nothing to an operator),
 *  so it must never come out invalid or blank no matter what the label is.
 *
 *  Deliberately mirrors the EXISTING backend-accepted charset
 *  (`isValidDataSourceName`, `^[a-z][a-z0-9_]*$` — no hyphens): spaces and
 *  hyphens become underscores, anything else invalid is dropped, and a
 *  slug that would start with a digit gets an `s_` prefix.
 *
 *  A label with nothing sluggable left (pure CJK / emoji / punctuation — no
 *  ASCII letter or digit survives the filter) can't produce a slug at all.
 *  Rather than the old empty-string result (not a valid id — that was the
 *  bug this fixes), it falls back to a stable `source_<hex>` / `db_<hex>`
 *  fingerprint of the label. Nothing is transliterated — the fallback is
 *  deliberately an opaque tag, not a mangled reading of the CJK text.
 *
 *  Either branch is then disambiguated against `existingIds` (every other
 *  already-used id — pass the current `data_sources`/`db_sources` name list
 *  in, so two similar or identical labels never collide: `_2`, `_3`, …) and
 *  capped at 64 chars, matching the backend's limit even after a
 *  disambiguating suffix. */
export function slugifyDataSourceName(
  label: string,
  kind: DataSourceIdKind = 'source',
  existingIds: readonly string[] = [],
): string {
  const existing = new Set(existingIds);
  const lower = label.trim().toLowerCase();
  let slug = lower.replace(/[\s-]+/g, '_').replace(/[^a-z0-9_]/g, '');
  slug = slug.replace(/^_+/, '');
  if (!slug) {
    return disambiguateSlug(`${kind}_${stableHashHex6(lower)}`, existing, 64);
  }
  if (!/^[a-z]/.test(slug)) slug = `s_${slug}`;
  return disambiguateSlug(slug.slice(0, 64), existing, 64);
}

/** Wizard glue for the id field: auto-derive it from the label while
 *  creating (`isNew`), but NEVER regenerate it once a source is saved —
 *  editing only ever changes the label. The id is a durable reference other
 *  config may already point at (`source = "…"` in a field rule, a
 *  `tool_egress` key, `connector` on a `db_field` rule), so silently
 *  reslugging it out from under an edit would break those references. */
export function deriveDataSourceId(
  label: string,
  kind: DataSourceIdKind,
  existingIds: readonly string[],
  isNew: boolean,
  currentId: string,
): string {
  return isNew ? slugifyDataSourceName(label, kind, existingIds) : currentId;
}

/** Step-2 "next" gate for the database wizard type: a display label + a
 *  credential are required to proceed when creating. The id itself is
 *  auto-derived from the label (`slugifyDataSourceName` above) and can
 *  never come out invalid, so it is no longer part of this gate.
 *  `allowedTables` is refined in step 3 after a test, and the DB source's
 *  full validator (which does require it) is the gate on step 4's "完成"
 *  instead. Editing never re-requires label/url (a blank credential on
 *  edit means "keep the stored one" — see `dbSourceToFormState`). */
export function canProceedDbStep2(label: string, url: string, isNew: boolean): boolean {
  if (!isNew) return true;
  return label.trim().length > 0 && url.trim().length > 0;
}

/** Smoke-test params for a not-yet-saved custom tool source's step-3 dry
 *  run: the first configured tool (glob suffix stripped — `redaction.dry_run`
 *  takes a literal tool name) plus, when the table is identified from an
 *  argument, that argument with an empty sample value (the operator is
 *  checking "does this source's shape get recognised at all", not testing a
 *  specific table). */
export function customSourceSmokeTestParams(
  form: Pick<DataSourceFormState, 'tools' | 'tableMode' | 'tableArg'>,
): { tool: string; args: Record<string, string> } {
  const tool = (form.tools[0] ?? '').replace(/\*+$/, '');
  const args: Record<string, string> = form.tableMode === 'from_arg' ? { [form.tableArg.trim() || 'table']: '' } : {};
  return { tool, args };
}
