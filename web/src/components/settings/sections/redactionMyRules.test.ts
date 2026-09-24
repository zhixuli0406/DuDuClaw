import { describe, it, expect } from 'vitest';
import {
  applyCategoryChip,
  buildMyRuleSampleText,
  deriveDraftCategoryToken,
  draftRuleId,
  emptyMyRuleForm,
  formToDraftRule,
  isMyRuleFormValid,
  isMyRuleStep1Valid,
  isMyRuleStep2Valid,
  isValidRegexSyntax,
  mergeCategoryChips,
  mergeKeywords,
  myRuleFormToUpsert,
  myRuleMatchMode,
  myRuleToFormState,
  parseExampleLines,
  parseKeywordInput,
  runPatternChecks,
  setMyRuleLabel,
  testPatternAgainstValue,
  validateMyRuleStep1,
  validateMyRuleStep2,
  MY_RULE_LABEL_MAX,
  MY_RULE_EXAMPLES_MAX,
  MY_RULE_COUNTER_EXAMPLES_MAX,
  type MyRuleFormState,
} from './redactionMyRules';
import type { RedactionCustomRule } from '@/lib/api';

// ── keyword input parsing ──────────────────────────────────────────────

describe('parseKeywordInput', () => {
  it('splits on commas, full-width commas, and newlines', () => {
    expect(parseKeywordInput('Falcon,Orion\nNebula，獵鷹計畫')).toEqual(['Falcon', 'Orion', 'Nebula', '獵鷹計畫']);
  });
  it('trims whitespace and drops blank entries', () => {
    expect(parseKeywordInput('  Falcon , , \n Orion \n\n')).toEqual(['Falcon', 'Orion']);
  });
  it('returns an empty array for blank input', () => {
    expect(parseKeywordInput('   \n  ')).toEqual([]);
  });
});

describe('mergeKeywords', () => {
  it('appends new tokens after existing ones, in order', () => {
    expect(mergeKeywords(['Falcon'], ['Orion', 'Nebula'])).toEqual(['Falcon', 'Orion', 'Nebula']);
  });
  it('dedupes case-sensitively against the existing list', () => {
    expect(mergeKeywords(['Falcon'], ['Falcon', 'falcon', 'Orion'])).toEqual(['Falcon', 'falcon', 'Orion']);
  });
});

describe('parseExampleLines', () => {
  it('splits on newlines, trims, drops blanks, and caps at max', () => {
    const raw = 'EMP-2024-0133\n  EMP-2023-0871  \n\nEMP-2025-0002\nEMP-2026-0003\nEMP-2027-0004\nEMP-2028-0005';
    expect(parseExampleLines(raw, MY_RULE_EXAMPLES_MAX)).toEqual([
      'EMP-2024-0133',
      'EMP-2023-0871',
      'EMP-2025-0002',
      'EMP-2026-0003',
      'EMP-2027-0004',
    ]);
  });
  it('respects a smaller cap for counter-examples', () => {
    expect(parseExampleLines('a\nb\nc\nd', MY_RULE_COUNTER_EXAMPLES_MAX)).toEqual(['a', 'b', 'c']);
  });
});

// ── local regex checking ───────────────────────────────────────────────

describe('isValidRegexSyntax', () => {
  it('accepts a compilable pattern', () => {
    expect(isValidRegexSyntax('EMP-\\d{4}-\\d{4}')).toBe(true);
  });
  it('rejects an unclosed group', () => {
    expect(isValidRegexSyntax('CU-(0-9')).toBe(false);
  });
});

describe('runPatternChecks', () => {
  it('reports every example matched and every counter-example unmatched as ok', () => {
    const result = runPatternChecks('EMP-\\d{4}-\\d{4}', ['EMP-2024-0133', 'EMP-2023-0871'], ['ORD-2024-0133']);
    expect(result.allOk).toBe(true);
    expect(result.checks).toHaveLength(3);
    expect(result.checks.every((c) => c.ok)).toBe(true);
    expect(result.checks[0]).toMatchObject({ kind: 'example', matched: true, ok: true });
    expect(result.checks[2]).toMatchObject({ kind: 'counter', matched: false, ok: true });
  });
  it('flags an example that fails to match', () => {
    const result = runPatternChecks('EMP-\\d{4}-\\d{4}', ['EMP-2024-0133', 'NOT-A-MATCH'], []);
    expect(result.allOk).toBe(false);
    expect(result.checks[1]).toMatchObject({ ok: false, matched: false });
  });
  it('flags a counter-example that DOES match (should not)', () => {
    const result = runPatternChecks('EMP-\\d{4}-\\d{4}', ['EMP-2024-0133', 'EMP-2023-0871'], ['EMP-2099-9999']);
    expect(result.allOk).toBe(false);
    expect(result.checks[2]).toMatchObject({ kind: 'counter', matched: true, ok: false });
  });
  it('is never "ok" with zero checks (nothing typed yet)', () => {
    expect(runPatternChecks('EMP-\\d{4}-\\d{4}', [], []).allOk).toBe(false);
  });
  it('reports a compile error instead of throwing', () => {
    const result = runPatternChecks('CU-(0-9', ['CU-000123'], []);
    expect(result.allOk).toBe(false);
    expect(result.checks).toEqual([]);
    expect(result.error).toBeTruthy();
  });
  it('treats a blank pattern as "nothing to check" rather than a syntax error', () => {
    expect(runPatternChecks('   ', ['x'], [])).toEqual({ checks: [], allOk: false, error: null });
  });
  // Backend parity (redaction_custom_rules.rs::suggest_checks): an example
  // must match IN FULL (anchored); a counter-example is an unanchored
  // search — a pattern firing anywhere inside it is the failure this check
  // exists to catch. Mirrors the backend's own
  // `suggest_checks_anchor_examples_and_search_counters` unit test.
  it('anchors examples but only searches counter-examples', () => {
    const result = runPatternChecks('\\d{4}', ['2024'], ['order 2024']);
    expect(result.allOk).toBe(false);
    expect(result.checks[0]).toMatchObject({ kind: 'example', matched: true, ok: true });
    expect(result.checks[1]).toMatchObject({ kind: 'counter', matched: true, ok: false });
  });
  it('fails an example the pattern only matches PART of, not the whole value', () => {
    // "EMP-\d{4}" would match a *substring* of "EMP-2024-0133" (unanchored),
    // but does not match the value in full — must be reported as NOT ok.
    const result = runPatternChecks('EMP-\\d{4}', ['EMP-2024-0133'], []);
    expect(result.checks[0]).toMatchObject({ matched: false, ok: false });
    expect(result.allOk).toBe(false);
  });
});

describe('testPatternAgainstValue', () => {
  it('reports a match', () => {
    expect(testPatternAgainstValue('EMP-\\d{4}-\\d{4}', 'EMP-2024-0133')).toEqual({ ok: true, matched: true, error: null });
  });
  it('reports a non-match', () => {
    expect(testPatternAgainstValue('EMP-\\d{4}-\\d{4}', 'nope')).toEqual({ ok: false, matched: false, error: null });
  });
  it('treats a blank value as nothing to test (never blocks)', () => {
    expect(testPatternAgainstValue('EMP-\\d{4}-\\d{4}', '  ')).toEqual({ ok: true, matched: false, error: null });
  });
  it('reports a compile error', () => {
    const result = testPatternAgainstValue('CU-(0-9', 'CU-000123');
    expect(result.ok).toBe(false);
    expect(result.error).toBeTruthy();
  });
});

// ── category chips ──────────────────────────────────────────────────────

describe('mergeCategoryChips', () => {
  it('unions selected-profile categories with category_labels keys, sorted, deduped', () => {
    expect(mergeCategoryChips(['TW_ID', 'EMAIL'], { CUSTOM_01: '內部專案代號', EMAIL: '電子郵件' })).toEqual(['CUSTOM_01', 'EMAIL', 'TW_ID']);
  });
  it('handles an undefined labels map', () => {
    expect(mergeCategoryChips(['TW_ID'], undefined)).toEqual(['TW_ID']);
  });
});

// ── wizard form state ───────────────────────────────────────────────────

describe('setMyRuleLabel / applyCategoryChip', () => {
  it('typing a label decouples from any previously-picked category', () => {
    const form = applyCategoryChip(emptyMyRuleForm(), 'CUSTOM_01', '內部專案代號');
    expect(form.category).toBe('CUSTOM_01');
    const typed = setMyRuleLabel(form, '內部專案代號 v2');
    expect(typed.category).toBe('');
    expect(typed.label).toBe('內部專案代號 v2');
  });
  it('applyCategoryChip fills both label and category', () => {
    const form = applyCategoryChip(emptyMyRuleForm(), 'TW_MOBILE', '手機號碼');
    expect(form).toMatchObject({ category: 'TW_MOBILE', label: '手機號碼' });
  });
});

describe('myRuleToFormState / myRuleFormToUpsert round trip', () => {
  const keywordRule: RedactionCustomRule = {
    id: 'project_code',
    category: 'CUSTOM_01',
    label: '內部專案代號',
    kind: 'keyword',
    keywords: ['Falcon', 'Orion'],
    example: 'Falcon',
    enabled: true,
  };
  const regexRule: RedactionCustomRule = {
    id: 'employee_id',
    category: 'CUSTOM_EMPLOYEE_ID',
    label: '員工編號',
    kind: 'regex',
    pattern: 'EMP-\\d{4}-\\d{4}',
    example: 'EMP-2024-0133',
    enabled: false,
  };

  it('opens a keyword rule onto the keyword tab', () => {
    const form = myRuleToFormState(keywordRule);
    expect(form).toMatchObject({
      id: 'project_code',
      label: '內部專案代號',
      category: 'CUSTOM_01',
      method: 'keyword',
      keywords: ['Falcon', 'Orion'],
      enabled: true,
    });
  });

  it('opens a regex rule onto the advanced pattern tab, never the examples tab (§13.1: examples are never persisted)', () => {
    const form = myRuleToFormState(regexRule);
    expect(form).toMatchObject({
      id: 'employee_id',
      method: 'pattern',
      advancedPattern: 'EMP-\\d{4}-\\d{4}',
      examplePattern: '',
      examples: [],
      enabled: false,
    });
  });

  it('round-trips a keyword rule through myRuleFormToUpsert', () => {
    const form = myRuleToFormState(keywordRule);
    expect(myRuleFormToUpsert(form)).toEqual({
      id: 'project_code',
      label: '內部專案代號',
      category: 'CUSTOM_01',
      kind: 'keyword',
      enabled: true,
      keywords: ['Falcon', 'Orion'],
    });
  });

  it('round-trips a regex rule through myRuleFormToUpsert', () => {
    const form = myRuleToFormState(regexRule);
    expect(myRuleFormToUpsert(form)).toEqual({
      id: 'employee_id',
      label: '員工編號',
      category: 'CUSTOM_EMPLOYEE_ID',
      kind: 'regex',
      enabled: false,
      pattern: 'EMP-\\d{4}-\\d{4}',
    });
  });

  it('a new rule omits id and, when category is free-text, omits category too', () => {
    const form: MyRuleFormState = { ...emptyMyRuleForm(), label: '客戶編號', method: 'pattern', advancedPattern: 'CU-\\d{6}' };
    const body = myRuleFormToUpsert(form);
    expect(body.id).toBeUndefined();
    expect(body.category).toBeUndefined();
    expect(body).toMatchObject({ label: '客戶編號', kind: 'regex', pattern: 'CU-\\d{6}' });
  });

  it('the "give me examples" method sends examplePattern, not advancedPattern', () => {
    const form: MyRuleFormState = {
      ...emptyMyRuleForm(),
      label: '員工編號',
      method: 'example',
      examplePattern: 'EMP-\\d{4}-\\d{4}',
      advancedPattern: 'should-not-be-sent',
    };
    expect(myRuleFormToUpsert(form)).toMatchObject({ kind: 'regex', pattern: 'EMP-\\d{4}-\\d{4}' });
  });
});

// ── validation ───────────────────────────────────────────────────────────

describe('validateMyRuleStep1', () => {
  it('rejects an empty label', () => {
    expect(validateMyRuleStep1(emptyMyRuleForm())).toEqual({ label: 'empty' });
  });
  it('rejects a label over the max length', () => {
    const form = { ...emptyMyRuleForm(), label: 'x'.repeat(MY_RULE_LABEL_MAX + 1) };
    expect(validateMyRuleStep1(form)).toEqual({ label: 'tooLong' });
  });
  it('accepts a normal label', () => {
    expect(isMyRuleStep1Valid({ ...emptyMyRuleForm(), label: '員工編號' })).toBe(true);
  });
});

describe('validateMyRuleStep2 — keyword method', () => {
  it('rejects zero keywords', () => {
    expect(validateMyRuleStep2({ ...emptyMyRuleForm(), method: 'keyword', keywords: [] })).toEqual({ keywords: 'empty' });
  });
  it('rejects a keyword shorter than the minimum', () => {
    const form = { ...emptyMyRuleForm(), method: 'keyword' as const, keywords: ['a'] };
    expect(validateMyRuleStep2(form)).toEqual({ keywords: 'tooShort' });
  });
  it('accepts valid keywords', () => {
    const form = { ...emptyMyRuleForm(), method: 'keyword' as const, keywords: ['Falcon', 'Orion'] };
    expect(isMyRuleStep2Valid(form)).toBe(true);
  });
});

describe('validateMyRuleStep2 — example method', () => {
  it('requires a non-empty pattern', () => {
    const form = { ...emptyMyRuleForm(), method: 'example' as const, examplePattern: '' };
    expect(validateMyRuleStep2(form)).toEqual({ pattern: 'empty' });
  });
  it('requires the pattern to pass every check (all_ok)', () => {
    const form = {
      ...emptyMyRuleForm(),
      method: 'example' as const,
      examplePattern: 'EMP-\\d{4}-\\d{4}',
      examples: ['EMP-2024-0133', 'NOT-A-MATCH'],
      counterExamples: [],
    };
    expect(validateMyRuleStep2(form)).toEqual({ notVerified: true });
  });
  it('passes when every example matches and every counter-example does not', () => {
    const form = {
      ...emptyMyRuleForm(),
      method: 'example' as const,
      examplePattern: 'EMP-\\d{4}-\\d{4}',
      examples: ['EMP-2024-0133', 'EMP-2023-0871'],
      counterExamples: ['ORD-2024-0133'],
    };
    expect(isMyRuleStep2Valid(form)).toBe(true);
  });
});

describe('validateMyRuleStep2 — pattern (advanced) method', () => {
  it('rejects invalid regex syntax', () => {
    const form = { ...emptyMyRuleForm(), method: 'pattern' as const, advancedPattern: 'CU-(0-9' };
    expect(validateMyRuleStep2(form)).toEqual({ pattern: 'invalid' });
  });
  it('accepts valid regex syntax', () => {
    const form = { ...emptyMyRuleForm(), method: 'pattern' as const, advancedPattern: 'CU-\\d{6}' };
    expect(isMyRuleStep2Valid(form)).toBe(true);
  });
});

describe('isMyRuleFormValid', () => {
  it('requires both step 1 and step 2 to pass', () => {
    const missingLabel = { ...emptyMyRuleForm(), method: 'keyword' as const, keywords: ['Falcon'] };
    expect(isMyRuleFormValid(missingLabel)).toBe(false);
    const complete = { ...missingLabel, label: '內部專案代號' };
    expect(isMyRuleFormValid(complete)).toBe(true);
  });
});

// ── step 3 sample text / dry-run wrapping / preview count ──────────────

describe('buildMyRuleSampleText', () => {
  it('embeds up to 2 keywords for the keyword method', () => {
    const form = { ...emptyMyRuleForm(), method: 'keyword' as const, keywords: ['Falcon', 'Orion', 'Nebula'] };
    const text = buildMyRuleSampleText(form);
    expect(text).toContain('Falcon');
    expect(text).toContain('Orion');
    expect(text).not.toContain('Nebula');
  });
  it('embeds up to 2 examples for the example method', () => {
    const form = { ...emptyMyRuleForm(), method: 'example' as const, examples: ['EMP-2024-0133', 'EMP-2023-0871'] };
    expect(buildMyRuleSampleText(form)).toContain('EMP-2024-0133');
  });
  it('is blank for the advanced pattern method (no known-good value)', () => {
    const form = { ...emptyMyRuleForm(), method: 'pattern' as const, advancedPattern: 'CU-\\d{6}' };
    expect(buildMyRuleSampleText(form)).toBe('');
  });
});

// ── step 3 draft-rule preview (backend follow-up) ───────────────────────

describe('draftRuleId', () => {
  it('reuses the real rule id when editing (so the draft shadows the saved rule)', () => {
    const rule: RedactionCustomRule = { id: 'employee_id', category: 'CUSTOM_01', label: '員工編號', kind: 'regex', pattern: 'x', example: 'x', enabled: true };
    expect(draftRuleId({ mode: 'edit', rule })).toBe('employee_id');
  });
  it('generates a stable draft_<timestamp> id for a new rule', () => {
    expect(draftRuleId({ mode: 'create' }, 1735689600000)).toBe('draft_1735689600000');
  });
});

describe('deriveDraftCategoryToken', () => {
  it('returns the picked existing category verbatim', () => {
    const form = { ...emptyMyRuleForm(), category: 'TW_MOBILE' };
    expect(deriveDraftCategoryToken(form)).toBe('TW_MOBILE');
  });
  it('slugs an ASCII label into CUSTOM_<SLUG>', () => {
    const form = { ...emptyMyRuleForm(), label: 'Employee ID' };
    expect(deriveDraftCategoryToken(form)).toBe('CUSTOM_EMPLOYEE_ID');
  });
  it('falls back to a fixed placeholder for a pure-CJK label', () => {
    const form = { ...emptyMyRuleForm(), label: '內部專案代號' };
    expect(deriveDraftCategoryToken(form)).toBe('CUSTOM_DRAFT');
  });
  it('always produces a token matching the backend is_valid_category grammar', () => {
    const form = { ...emptyMyRuleForm(), label: '員工編號 v2!!  --  2024' };
    expect(deriveDraftCategoryToken(form)).toMatch(/^[A-Z0-9_]{1,32}$/);
  });
});

describe('formToDraftRule', () => {
  it('builds a keyword draft', () => {
    const form = { ...emptyMyRuleForm(), label: '內部專案代號', method: 'keyword' as const, keywords: ['Falcon', 'Orion'] };
    expect(formToDraftRule(form, 'draft_1')).toEqual({
      id: 'draft_1',
      label: '內部專案代號',
      category: 'CUSTOM_DRAFT',
      kind: 'keyword',
      keywords: ['Falcon', 'Orion'],
    });
  });
  it('builds a regex draft from the "give me examples" tab', () => {
    const form = { ...emptyMyRuleForm(), label: 'Employee ID', method: 'example' as const, examplePattern: 'EMP-\\d{4}-\\d{4}' };
    expect(formToDraftRule(form, 'employee_id')).toEqual({
      id: 'employee_id',
      label: 'Employee ID',
      category: 'CUSTOM_EMPLOYEE_ID',
      kind: 'regex',
      pattern: 'EMP-\\d{4}-\\d{4}',
    });
  });
  it('falls back to CUSTOM_DRAFT for a pure-CJK label with no picked category', () => {
    const form = { ...emptyMyRuleForm(), label: '員工編號', method: 'example' as const, examplePattern: 'EMP-\\d{4}-\\d{4}' };
    expect(formToDraftRule(form, 'employee_id').category).toBe('CUSTOM_DRAFT');
  });
  it('builds a regex draft from the advanced tab, reusing a picked category', () => {
    const form = { ...emptyMyRuleForm(), label: '客戶編號', category: 'CUSTOM_01', method: 'pattern' as const, advancedPattern: 'CU-\\d{6}' };
    expect(formToDraftRule(form, 'draft_2')).toEqual({
      id: 'draft_2',
      label: '客戶編號',
      category: 'CUSTOM_01',
      kind: 'regex',
      pattern: 'CU-\\d{6}',
    });
  });
});

// ── list-row summary ─────────────────────────────────────────────────────

describe('myRuleMatchMode', () => {
  it('reports the keyword count for a keyword rule', () => {
    const rule: RedactionCustomRule = { id: 'a', category: 'CUSTOM_01', label: 'x', kind: 'keyword', keywords: ['a', 'b', 'c'], example: 'a', enabled: true };
    expect(myRuleMatchMode(rule)).toEqual({ mode: 'keyword', count: 3 });
  });
  it('reports "regex" with no count for a regex rule', () => {
    const rule: RedactionCustomRule = { id: 'b', category: 'CUSTOM_02', label: 'y', kind: 'regex', pattern: 'x', example: 'x', enabled: true };
    expect(myRuleMatchMode(rule)).toEqual({ mode: 'regex' });
  });
});
