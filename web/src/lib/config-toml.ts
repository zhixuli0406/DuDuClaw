/**
 * Small helpers for the settings pages that edit `config.toml` through
 * `system.update_config` (v1.68 dashboard switches, W2).
 *
 * - `parseTomlSubset` reads the masked TOML string `system.config` returns
 *   into `{ "<table.path>": { key: value } }`. It understands the subset the
 *   gateway writes (scalars, strings, single- or multi-line arrays of scalars,
 *   dotted table headers). Array-of-tables (`[[x]]`) and inline tables are
 *   skipped on purpose — none of the controls here read them.
 * - `changedPayload` turns two flat `{ "a.b.c": value }` snapshots into the
 *   nested object the RPC expects, holding ONLY the changed leaves (plan
 *   principle 3: never overwrite a stored value with a form default).
 * - `normalizeRestartRequired` folds the three response shapes for
 *   "needs a restart" (string[] / true / absent) into a list of keys.
 */

export type TomlScalar = string | number | boolean;
export type TomlValue = TomlScalar | TomlScalar[];
export type TomlTables = Record<string, Record<string, TomlValue>>;

/** Placeholder the gateway writes in place of a stored secret (system.config). */
export const MASKED_SECRET = '********';

function stripComment(line: string): string {
  let inStr: '"' | "'" | null = null;
  for (let i = 0; i < line.length; i++) {
    const c = line[i];
    if (inStr) {
      if (c === '\\' && inStr === '"') { i++; continue; }
      if (c === inStr) inStr = null;
    } else if (c === '"' || c === "'") {
      inStr = c;
    } else if (c === '#') {
      return line.slice(0, i);
    }
  }
  return line;
}

function parseScalar(raw: string): TomlScalar | undefined {
  const s = raw.trim();
  if (s === 'true') return true;
  if (s === 'false') return false;
  if (s.startsWith('"') && s.endsWith('"') && s.length >= 2) {
    try {
      return JSON.parse(s) as string;
    } catch {
      return s.slice(1, -1);
    }
  }
  if (s.startsWith("'") && s.endsWith("'") && s.length >= 2) return s.slice(1, -1);
  const num = s.replace(/_/g, '');
  if (/^[+-]?\d+(\.\d+)?([eE][+-]?\d+)?$/.test(num)) return Number(num);
  return undefined;
}

/** Split an array body (without the brackets) on top-level commas. */
function splitArrayItems(body: string): string[] {
  const out: string[] = [];
  let cur = '';
  let inStr: '"' | "'" | null = null;
  for (let i = 0; i < body.length; i++) {
    const c = body[i];
    if (inStr) {
      cur += c;
      if (c === '\\' && inStr === '"' && i + 1 < body.length) { cur += body[++i]; continue; }
      if (c === inStr) inStr = null;
    } else if (c === '"' || c === "'") {
      inStr = c;
      cur += c;
    } else if (c === ',') {
      out.push(cur);
      cur = '';
    } else {
      cur += c;
    }
  }
  if (cur.trim() !== '') out.push(cur);
  return out;
}

function parseValue(raw: string): TomlValue | undefined {
  const s = raw.trim();
  if (s.startsWith('[')) {
    const body = s.slice(1, s.lastIndexOf(']'));
    const items = splitArrayItems(body)
      .map((p) => parseScalar(p))
      .filter((v): v is TomlScalar => v !== undefined);
    return items;
  }
  if (s.startsWith('{')) return undefined; // inline tables are not read here
  return parseScalar(s);
}

function bracketBalance(s: string): number {
  let depth = 0;
  let inStr: '"' | "'" | null = null;
  for (let i = 0; i < s.length; i++) {
    const c = s[i];
    if (inStr) {
      if (c === '\\' && inStr === '"') { i++; continue; }
      if (c === inStr) inStr = null;
    } else if (c === '"' || c === "'") inStr = c;
    else if (c === '[') depth++;
    else if (c === ']') depth--;
  }
  return depth;
}

/** Parse the subset of TOML the settings pages read. Never throws. */
export function parseTomlSubset(text: string): TomlTables {
  const tables: TomlTables = { '': {} };
  let current: string | null = '';
  const lines = text.split(/\r?\n/);
  for (let i = 0; i < lines.length; i++) {
    const line = stripComment(lines[i]).trim();
    if (line === '') continue;
    if (line.startsWith('[[')) {
      current = null; // array-of-tables: skipped
      continue;
    }
    if (line.startsWith('[')) {
      const name = line.slice(1, line.indexOf(']')).trim().replace(/\s*\.\s*/g, '.').replace(/"/g, '');
      current = name;
      if (!tables[name]) tables[name] = {};
      continue;
    }
    const eq = line.indexOf('=');
    if (eq <= 0 || current === null) continue;
    const key = line.slice(0, eq).trim().replace(/^"|"$/g, '');
    let rhs = line.slice(eq + 1).trim();
    // Multi-line arrays: keep reading until the brackets balance.
    if (rhs.startsWith('[')) {
      while (bracketBalance(rhs) > 0 && i + 1 < lines.length) {
        i++;
        rhs += ' ' + stripComment(lines[i]).trim();
      }
    }
    const value = parseValue(rhs);
    if (value === undefined) continue;
    // Dotted keys (`a.b = 1`) land in the sub-table.
    if (key.includes('.')) {
      const parts = key.split('.').map((p) => p.trim());
      const leaf = parts.pop() as string;
      const table = [current, ...parts].filter((p) => p !== '').join('.');
      if (!tables[table]) tables[table] = {};
      tables[table][leaf] = value;
    } else {
      tables[current][key] = value;
    }
  }
  return tables;
}

/** Typed readers with a fallback when absent or of the wrong type. */
export function tomlBool(t: TomlTables, table: string, key: string, fallback: boolean): boolean {
  const v = t[table]?.[key];
  return typeof v === 'boolean' ? v : fallback;
}
export function tomlNum(t: TomlTables, table: string, key: string, fallback: number): number {
  const v = t[table]?.[key];
  return typeof v === 'number' && Number.isFinite(v) ? v : fallback;
}
export function tomlStr(t: TomlTables, table: string, key: string, fallback: string): string {
  const v = t[table]?.[key];
  return typeof v === 'string' ? v : fallback;
}
export function tomlStrList(t: TomlTables, table: string, key: string): string[] {
  const v = t[table]?.[key];
  return Array.isArray(v) ? v.filter((x): x is string => typeof x === 'string') : [];
}
/** True when a (masked) secret is stored under `key` or `<key>_enc`. */
export function tomlSecretSet(t: TomlTables, table: string, key: string): boolean {
  const a = t[table]?.[key];
  const b = t[table]?.[`${key}_enc`];
  return (typeof a === 'string' && a !== '') || (typeof b === 'string' && b !== '');
}

export type FlatValues = Record<string, unknown>;

function sameValue(a: unknown, b: unknown): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}

/** Flat dotted keys whose value differs between the two snapshots. */
export function changedKeys(initial: FlatValues, current: FlatValues): string[] {
  return Object.keys(current).filter((k) => !sameValue(initial[k], current[k]));
}

/** Build the nested RPC payload from dotted keys → values. */
export function nestPayload(flat: FlatValues): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const [path, value] of Object.entries(flat)) {
    const parts = path.split('.');
    let node: Record<string, unknown> = out;
    for (let i = 0; i < parts.length - 1; i++) {
      const existing = node[parts[i]];
      const next: Record<string, unknown> =
        existing && typeof existing === 'object' && !Array.isArray(existing)
          ? (existing as Record<string, unknown>)
          : {};
      node[parts[i]] = next;
      node = next;
    }
    node[parts[parts.length - 1]] = value;
  }
  return out;
}

/** Nested payload holding only the leaves that changed. */
export function changedPayload(initial: FlatValues, current: FlatValues): Record<string, unknown> {
  const flat: FlatValues = {};
  for (const k of changedKeys(initial, current)) flat[k] = current[k];
  return nestPayload(flat);
}

/**
 * `restart_required` comes back as a key list (`system.update_config`,
 * `config.raw.set`), as `true` (`tick.sources.*`, `channels.add`) or not at
 * all (older gateways). `fallbackKey` names the change when only `true` came.
 */
export function normalizeRestartRequired(value: unknown, fallbackKey: string): string[] {
  if (Array.isArray(value)) return value.filter((v): v is string => typeof v === 'string' && v !== '');
  if (value === true) return [fallbackKey];
  return [];
}
