import type { RedactionCustomRule, RedactionCustomRuleUpsert, RedactionDraftRule } from '@/lib/api';

// ── "我的規則" (custom keyword/regex rules) — pure helpers (WP-W) ──────────
//
// DESIGN-redaction-ner-and-custom-rules-2026-09 §6/§12.2/§13.2. This file
// holds ONLY pure, side-effect-free functions and their types, mirroring the
// house convention set by `redactionFieldRules.ts` — the vitest suite in
// `redactionMyRules.test.ts` exercises them without a WebSocket or an
// IntlProvider. RPC calls (`api.redaction.customRules.*` /
// `api.redaction.suggestPattern` / `api.redaction.profiles.*`) live in the
// component files instead.

// ── limits (mirror the backend's §13.2 `custom_rules.upsert` validation —
//    client-side checks are "fast, friendly, first pass"; the backend's own
//    validation at save time remains the actual authority) ─────────────────

export const MY_RULE_LABEL_MAX = 32;
export const MY_RULE_KEYWORD_MIN_LEN = 2;
export const MY_RULE_KEYWORDS_MAX = 200;
export const MY_RULE_PATTERN_MAX = 512;
export const MY_RULE_EXAMPLES_MIN = 2;
export const MY_RULE_EXAMPLES_MAX = 5;
export const MY_RULE_COUNTER_EXAMPLES_MAX = 3;

// ── keyword input parsing ───────────────────────────────────────────────

/** Splits a pasted "comma or newline separated list" into trimmed, non-empty
 *  tokens (canvas screen 5's input hint: "貼上以逗號、換行分隔的清單"). Does
 *  NOT dedupe against an existing token list — see `mergeKeywords`. */
export function parseKeywordInput(raw: string): string[] {
  return raw
    .split(/[,，\n]+/)
    .map((s) => s.trim())
    .filter((s) => s.length > 0);
}

/** Adds `additions` to `existing`, deduping (case-sensitive — matches
 *  `RedactionTokenInput`'s own `addToken` dedup rule) and preserving the
 *  existing order with new tokens appended in the order they appeared. */
export function mergeKeywords(existing: readonly string[], additions: readonly string[]): string[] {
  const merged = [...existing];
  for (const a of additions) {
    if (!merged.includes(a)) merged.push(a);
  }
  return merged;
}

// ── example / counter-example textarea parsing ──────────────────────────

/** One example (or counter-example) per line, trimmed, blank lines dropped,
 *  capped at `max` (5 for examples, 3 for counter-examples per §13.2). */
export function parseExampleLines(raw: string, max: number): string[] {
  return raw
    .split('\n')
    .map((l) => l.trim())
    .filter((l) => l.length > 0)
    .slice(0, max);
}

// ── local regex checking (step 2's "給我幾個例子" live checks table, and the
//    "樣式（進階）" tab's syntax check + single-value tester) ───────────────
//
// A deliberate client-side approximation: JS `RegExp` and Rust's `regex`
// crate disagree at the edges (backreferences, some Unicode property
// escapes). This is fast, friendly, first-pass feedback — `redaction.
// suggest_pattern`'s own `checks`/`all_ok` (when the pattern came from the
// backend) and the backend's dry-compile at save time remain authoritative.

export function isValidRegexSyntax(pattern: string): boolean {
  try {
    // eslint-disable-next-line no-new -- syntax check only, the instance is discarded
    new RegExp(pattern);
    return true;
  } catch {
    return false;
  }
}

export interface MyRulePatternCheck {
  kind: 'example' | 'counter';
  value: string;
  matched: boolean;
  /** `true` when this check supports saving: a matched example, or an
   *  unmatched counter-example. */
  ok: boolean;
}

export interface MyRulePatternCheckResult {
  checks: MyRulePatternCheck[];
  /** `true` only when there is at least one check and every one is `ok`. An
   *  empty check list (no examples yet) is never "ok" — there is nothing to
   *  gate "next" on. */
  allOk: boolean;
  /** Set when `pattern` fails to compile as a `RegExp` — `checks` is empty
   *  in that case. */
  error: string | null;
}

/** Runs `pattern` against every non-blank example/counter-example and
 *  reports a per-value verdict — the live checks table in step 2's "給我幾個
 *  例子" tab (recomputed on every edit to the pattern OR the example lists,
 *  so a hand-edit of the generated pattern re-validates without another
 *  `suggest_pattern` round trip).
 *
 *  Matches the backend's own `suggest_checks` semantics (confirmed against
 *  `redaction_custom_rules.rs`): an EXAMPLE must be matched in full — the
 *  pattern is expected to describe the whole value, so it is anchored on
 *  both ends for that check only — while a COUNTER-EXAMPLE is an unanchored
 *  search: if the pattern fires ANYWHERE inside it, that counter-example
 *  would still get redacted, which is exactly the failure this check exists
 *  to catch. `ok` is already normalised (`true` = good) — render from `ok`,
 *  never from `matched` directly. */
export function runPatternChecks(
  pattern: string,
  examples: readonly string[],
  counterExamples: readonly string[],
): MyRulePatternCheckResult {
  const p = pattern.trim();
  if (!p) return { checks: [], allOk: false, error: null };
  // Validate the raw pattern first, so a syntax error is reported against
  // what the operator actually typed rather than the anchoring wrapper below.
  try {
    // eslint-disable-next-line no-new -- syntax check only, the instance is discarded
    new RegExp(p);
  } catch (e) {
    return { checks: [], allOk: false, error: e instanceof Error ? e.message : String(e) };
  }
  const fullMatch = new RegExp(`^(?:${p})$`);
  const partialMatch = new RegExp(p);
  const checks: MyRulePatternCheck[] = [
    ...examples
      .filter((v) => v.trim())
      .map((value): MyRulePatternCheck => {
        const matched = fullMatch.test(value);
        return { kind: 'example', value, matched, ok: matched };
      }),
    ...counterExamples
      .filter((v) => v.trim())
      .map((value): MyRulePatternCheck => {
        const matched = partialMatch.test(value);
        return { kind: 'counter', value, matched, ok: !matched };
      }),
  ];
  const allOk = checks.length > 0 && checks.every((c) => c.ok);
  return { checks, allOk, error: null };
}

/** The 樣式（進階）tab's single-value "拿一個值試試" tester. An empty value
 *  is treated as "nothing to test" (`ok: true`) so it never blocks the
 *  optional field from being left blank. */
export function testPatternAgainstValue(
  pattern: string,
  value: string,
): { ok: boolean; matched: boolean; error: string | null } {
  if (!value.trim()) return { ok: true, matched: false, error: null };
  try {
    const re = new RegExp(pattern);
    const matched = re.test(value);
    return { ok: matched, matched, error: null };
  } catch (e) {
    return { ok: false, matched: false, error: e instanceof Error ? e.message : String(e) };
  }
}

// ── category chips (step 1) ─────────────────────────────────────────────

/** Union of every category the operator can pick as "既有類型" in step 1:
 *  categories covered by the currently-enabled profiles, plus every custom
 *  category any loaded profile has named (`RedactionConfig.category_labels`
 *  — covers a category whose profile is temporarily unticked, which
 *  `selectedCategories` alone would drop). Sorted for stable chip order. */
export function mergeCategoryChips(
  selectedCategories: readonly string[],
  categoryLabels: Record<string, string> | undefined,
): string[] {
  return Array.from(new Set([...selectedCategories, ...Object.keys(categoryLabels ?? {})])).sort();
}

// ── wizard form state ───────────────────────────────────────────────────

export type MyRuleMethod = 'keyword' | 'example' | 'pattern';

export interface MyRuleFormState {
  /** `null` = creating a new rule; the id otherwise (only editable via a
   *  fresh id derivation server-side — this form never lets the operator
   *  type an id directly, matching §13.2's "id 省略＝新增"). */
  id: string | null;
  label: string;
  /** '' = let the backend derive a fresh `CUSTOM_*` id from `label`; a
   *  non-empty value reuses that existing category id. */
  category: string;
  /** Which of step 2's three tabs is authoritative at save time. Switching
   *  tabs never clears the other two tabs' inputs (canvas F5 note) — only
   *  the fields for the CURRENT `method` are read by `myRuleFormToUpsert`. */
  method: MyRuleMethod;
  keywords: string[];
  examples: string[];
  counterExamples: string[];
  /** The "給我幾個例子" tab's generated (and possibly hand-edited) pattern. */
  examplePattern: string;
  /** Which engine last produced `examplePattern`, for the "由本機模型協助" /
   *  "由雲端模型協助" / "未用 AI，可能較粗略" label. `null` before the first
   *  "產生樣式" click. */
  suggestionEngine: 'local' | 'cloud' | 'heuristic' | null;
  /** The 樣式（進階）tab's manually-typed pattern. */
  advancedPattern: string;
  enabled: boolean;
}

export function emptyMyRuleForm(): MyRuleFormState {
  return {
    id: null,
    label: '',
    category: '',
    method: 'keyword',
    keywords: [],
    examples: [],
    counterExamples: [],
    examplePattern: '',
    suggestionEngine: null,
    advancedPattern: '',
    enabled: true,
  };
}

/** Step 1's label input — free typing always decouples from a previously
 *  picked existing category (only an explicit chip click reuses an id via
 *  `applyCategoryChip`), so a half-edited label never silently keeps
 *  claiming someone else's category. */
export function setMyRuleLabel(form: MyRuleFormState, label: string): MyRuleFormState {
  return { ...form, label, category: '' };
}

/** Step 1's "既有類型" chip click — fills the label with the chip's display
 *  name and locks `category` to its id, so saving reuses that category
 *  instead of minting a new `CUSTOM_*` one. */
export function applyCategoryChip(form: MyRuleFormState, categoryId: string, label: string): MyRuleFormState {
  return { ...form, category: categoryId, label };
}

/** Opens the wizard prefilled for an existing rule (§13.3: "kind tab
 *  preselected; for regex rules the pattern is shown in the 樣式 tab" — a
 *  saved regex rule always opens on the advanced tab, never "給我幾個例子",
 *  because §13.1 deliberately never persists the examples that produced
 *  it). */
export function myRuleToFormState(rule: RedactionCustomRule): MyRuleFormState {
  const base = emptyMyRuleForm();
  if (rule.kind === 'keyword') {
    return {
      ...base,
      id: rule.id,
      label: rule.label,
      category: rule.category,
      method: 'keyword',
      keywords: [...(rule.keywords ?? [])],
      enabled: rule.enabled,
    };
  }
  return {
    ...base,
    id: rule.id,
    label: rule.label,
    category: rule.category,
    method: 'pattern',
    advancedPattern: rule.pattern ?? '',
    enabled: rule.enabled,
  };
}

/** Converts form state into `redaction.custom_rules.upsert`'s body. Only the
 *  fields relevant to `form.method` are read — the other tabs' drafts are
 *  simply discarded (never sent). */
export function myRuleFormToUpsert(form: MyRuleFormState): RedactionCustomRuleUpsert {
  const body: RedactionCustomRuleUpsert = {
    label: form.label.trim(),
    kind: form.method === 'keyword' ? 'keyword' : 'regex',
    enabled: form.enabled,
  };
  if (form.id) body.id = form.id;
  if (form.category.trim()) body.category = form.category.trim();
  if (form.method === 'keyword') {
    body.keywords = form.keywords;
  } else {
    body.pattern = (form.method === 'example' ? form.examplePattern : form.advancedPattern).trim();
  }
  return body;
}

// ── validation ───────────────────────────────────────────────────────────

export interface MyRuleStep1Errors {
  label?: 'empty' | 'tooLong';
}

export function validateMyRuleStep1(form: MyRuleFormState): MyRuleStep1Errors {
  const errors: MyRuleStep1Errors = {};
  const len = form.label.trim().length;
  if (len === 0) errors.label = 'empty';
  else if (len > MY_RULE_LABEL_MAX) errors.label = 'tooLong';
  return errors;
}

export function isMyRuleStep1Valid(form: MyRuleFormState): boolean {
  return Object.keys(validateMyRuleStep1(form)).length === 0;
}

export interface MyRuleStep2Errors {
  keywords?: 'empty' | 'tooShort' | 'tooMany';
  pattern?: 'empty' | 'invalid' | 'tooLong';
  /** `method: 'example'` only — a pattern exists but hasn't (yet) passed
   *  every example/counter-example check. */
  notVerified?: true;
}

export function validateMyRuleStep2(form: MyRuleFormState): MyRuleStep2Errors {
  const errors: MyRuleStep2Errors = {};
  if (form.method === 'keyword') {
    if (form.keywords.length === 0) errors.keywords = 'empty';
    else if (form.keywords.length > MY_RULE_KEYWORDS_MAX) errors.keywords = 'tooMany';
    else if (form.keywords.some((k) => k.trim().length < MY_RULE_KEYWORD_MIN_LEN)) errors.keywords = 'tooShort';
    return errors;
  }
  if (form.method === 'example') {
    const pattern = form.examplePattern.trim();
    if (!pattern) errors.pattern = 'empty';
    else if (pattern.length > MY_RULE_PATTERN_MAX) errors.pattern = 'tooLong';
    else if (!runPatternChecks(pattern, form.examples, form.counterExamples).allOk) errors.notVerified = true;
    return errors;
  }
  // method === 'pattern'
  const pattern = form.advancedPattern.trim();
  if (!pattern) errors.pattern = 'empty';
  else if (pattern.length > MY_RULE_PATTERN_MAX) errors.pattern = 'tooLong';
  else if (!isValidRegexSyntax(pattern)) errors.pattern = 'invalid';
  return errors;
}

export function isMyRuleStep2Valid(form: MyRuleFormState): boolean {
  return Object.keys(validateMyRuleStep2(form)).length === 0;
}

export function isMyRuleFormValid(form: MyRuleFormState): boolean {
  return isMyRuleStep1Valid(form) && isMyRuleStep2Valid(form);
}

// ── step 3 "試一試" ──────────────────────────────────────────────────────

/** A short demo sentence embedding up to 2 of the rule's own values, so step
 *  3 opens with something meaningful already in the sample box rather than
 *  blank (§12.2: "貼一段文字（預填一段含範例的示範文字）"). The advanced
 *  regex tab has no known-good value to embed, so it prefills blank. */
export function buildMyRuleSampleText(form: MyRuleFormState): string {
  const values =
    form.method === 'keyword'
      ? form.keywords.slice(0, 2)
      : form.method === 'example'
        ? form.examples.slice(0, 2)
        : [];
  if (values.length === 0) return '';
  return `請幫我查一下 ${values.join('、')} 的相關資料，麻煩協助處理。`;
}

/** Step 3's working rule id: a saved rule being EDITED reuses its own real
 *  id (so the draft correctly overrides it in the preview pipeline, per
 *  `RedactionDraftRule`'s doc comment); a NEW rule gets a throwaway
 *  `draft_<timestamp>` id that satisfies the backend's `^[a-z0-9][a-z0-9_-]
 *  {0,63}$` id grammar and is never persisted. `now` is injectable for
 *  deterministic tests. */
export function draftRuleId(target: { mode: 'create' } | { mode: 'edit'; rule: RedactionCustomRule }, now: number = Date.now()): string {
  return target.mode === 'edit' ? target.rule.id : `draft_${now}`;
}

/** `redaction.dry_run`'s `draft_rules[].category` is REQUIRED — unlike
 *  `upsert`, there is no server-side derivation step for a call that writes
 *  nothing. When the operator picked an existing type (step 1's chips),
 *  `form.category` already carries a valid id and this just returns it;
 *  otherwise this synthesises a throwaway token good enough to satisfy
 *  `is_valid_category` (`^[A-Z0-9_]{1,32}$`) for the PREVIEW ONLY — it is
 *  never saved and is not guaranteed to match the id `upsert` would actually
 *  assign this label on save (see the backend's own `derive_category_id`,
 *  which additionally reuses/dedupes ids across the whole profile — not
 *  worth replicating client-side for a value nothing ever persists). */
export function deriveDraftCategoryToken(form: MyRuleFormState): string {
  const explicit = form.category.trim();
  if (explicit) return explicit;
  const slug = form.label
    .toUpperCase()
    .replace(/[^A-Z0-9]+/g, '_')
    .replace(/^_+|_+$/g, '');
  const token = slug ? `CUSTOM_${slug}` : 'CUSTOM_DRAFT';
  return token.slice(0, MY_RULE_LABEL_MAX).replace(/_+$/, '') || 'CUSTOM_DRAFT';
}

/** Builds the single `draft_rules[]` entry for step 3's `redaction.
 *  dry_run` call — the current in-progress tab (`form.method`) only, same
 *  "which tab is authoritative" rule as `myRuleFormToUpsert`. */
export function formToDraftRule(form: MyRuleFormState, id: string): RedactionDraftRule {
  const draft: RedactionDraftRule = {
    id,
    label: form.label.trim() || undefined,
    category: deriveDraftCategoryToken(form),
    kind: form.method === 'keyword' ? 'keyword' : 'regex',
  };
  if (form.method === 'keyword') {
    draft.keywords = form.keywords;
  } else {
    draft.pattern = (form.method === 'example' ? form.examplePattern : form.advancedPattern).trim();
  }
  return draft;
}

// ── list-row summary (canvas screen 3) ──────────────────────────────────

export type MyRuleMatchMode = { mode: 'keyword'; count: number } | { mode: 'regex' };

/** How a saved rule matches, for the list row's "比對方式" text
 *  ("關鍵字 · N 個" / "樣式") — the component owns the i18n formatting. */
export function myRuleMatchMode(rule: RedactionCustomRule): MyRuleMatchMode {
  if (rule.kind === 'keyword') return { mode: 'keyword', count: rule.keywords?.length ?? 0 };
  return { mode: 'regex' };
}
