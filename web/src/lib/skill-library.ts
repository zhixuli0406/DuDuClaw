/**
 * Ordering, searching and sort-preference helpers for the skill library (B8).
 *
 * The gateway returns a stable order, but not a human one: byte order puts
 * every uppercase name before every lowercase one and sorts CJK by UTF-8
 * bytes. Everything the library shows is therefore sorted here with an
 * `Intl.Collator` for the UI locale (numeric, case- and accent-insensitive),
 * with a stable key as the final tie-breaker so equal names never swap places
 * between renders.
 */

export type SkillSortKey = 'name' | 'recentlyUsed' | 'mostUsed' | 'recentlyAdded';

export const SKILL_SORT_KEYS: readonly SkillSortKey[] = [
  'name',
  'recentlyUsed',
  'mostUsed',
  'recentlyAdded',
];

/** What a sortable row exposes to {@link sortSkills}. */
export interface SortFields {
  /** Display name compared with the collator. */
  name: string;
  /** ISO 8601 time of the last use, if any. */
  lastUsedAt?: string | null;
  /** Use count (activations, adoptions — whatever the tab counts). */
  useCount?: number | null;
  /** When the skill was added, as epoch milliseconds or an ISO string. */
  addedAt?: number | string | null;
  /** Unique, stable key (layer + owner + name) for the last tie-break. */
  stableKey: string;
}

const collators = new Map<string, Intl.Collator>();

/** A cached collator for `locale` (numeric, ignores case and accents). */
export function collatorFor(locale: string): Intl.Collator {
  let c = collators.get(locale);
  if (!c) {
    try {
      c = new Intl.Collator(locale, { numeric: true, sensitivity: 'base' });
    } catch {
      c = new Intl.Collator(undefined, { numeric: true, sensitivity: 'base' });
    }
    collators.set(locale, c);
  }
  return c;
}

function timeOf(v: number | string | null | undefined): number {
  if (v == null || v === '') return Number.NEGATIVE_INFINITY;
  if (typeof v === 'number') return Number.isFinite(v) ? v : Number.NEGATIVE_INFINITY;
  const t = Date.parse(v);
  return Number.isNaN(t) ? Number.NEGATIVE_INFINITY : t;
}

/** Compare two rows by `key`; never returns 0 for distinct stable keys. */
export function compareSkills(a: SortFields, b: SortFields, key: SkillSortKey, locale: string): number {
  const collator = collatorFor(locale);
  const byName = () => collator.compare(a.name, b.name);
  const byStable = () => (a.stableKey < b.stableKey ? -1 : a.stableKey > b.stableKey ? 1 : 0);
  let primary = 0;
  if (key === 'recentlyUsed') primary = timeOf(b.lastUsedAt) - timeOf(a.lastUsedAt);
  else if (key === 'mostUsed') primary = (b.useCount ?? 0) - (a.useCount ?? 0);
  else if (key === 'recentlyAdded') primary = timeOf(b.addedAt) - timeOf(a.addedAt);
  if (Number.isNaN(primary)) primary = 0;
  return primary || byName() || byStable();
}

/** A sorted copy of `items` (the input is not modified). */
export function sortSkills<T>(
  items: readonly T[],
  key: SkillSortKey,
  locale: string,
  fields: (item: T) => SortFields,
): T[] {
  const keyed = items.map((item) => ({ item, f: fields(item) }));
  keyed.sort((x, y) => compareSkills(x.f, y.f, key, locale));
  return keyed.map((k) => k.item);
}

function fold(s: string): string {
  return s.normalize('NFKC').toLocaleLowerCase();
}

/**
 * Whether every whitespace-separated term of `query` occurs in at least one
 * of `fields` (name, description, tags, owner …). An empty query matches.
 */
export function matchesQuery(query: string, fields: ReadonlyArray<string | null | undefined>): boolean {
  const terms = fold(query).split(/\s+/).filter(Boolean);
  if (terms.length === 0) return true;
  const hay = fields.filter((f): f is string => typeof f === 'string' && f.length > 0).map(fold);
  return terms.every((t) => hay.some((h) => h.includes(t)));
}

/** Agents ordered by display name (falling back to the id) with the collator. */
export function sortAgentsByName<T extends { name: string; display_name?: string }>(
  agents: readonly T[],
  locale: string,
): T[] {
  const collator = collatorFor(locale);
  return [...agents].sort(
    (a, b) =>
      collator.compare(a.display_name || a.name, b.display_name || b.name) ||
      (a.name < b.name ? -1 : a.name > b.name ? 1 : 0),
  );
}

const SORT_STORAGE_PREFIX = 'duduclaw.skills.sort.';

/** The viewer's remembered sort for `scope`, or `fallback`. Storage may be unavailable. */
export function readSortPreference(scope: string, fallback: SkillSortKey): SkillSortKey {
  try {
    const v = localStorage.getItem(SORT_STORAGE_PREFIX + scope);
    return v && (SKILL_SORT_KEYS as readonly string[]).includes(v) ? (v as SkillSortKey) : fallback;
  } catch {
    return fallback;
  }
}

/** Remember the viewer's sort for `scope`; silently skipped when storage is unavailable. */
export function writeSortPreference(scope: string, key: SkillSortKey): void {
  try {
    localStorage.setItem(SORT_STORAGE_PREFIX + scope, key);
  } catch {
    /* private window / blocked storage: the choice just is not remembered */
  }
}

export type SkillTab = 'market' | 'shared' | 'mySkills' | 'leaderboard';

const TABS: readonly SkillTab[] = ['market', 'shared', 'mySkills', 'leaderboard'];

/** The tab named by a `?tab=` value, or the market tab. */
export function parseSkillTab(value: string | null | undefined): SkillTab {
  return value && (TABS as readonly string[]).includes(value) ? (value as SkillTab) : 'market';
}
