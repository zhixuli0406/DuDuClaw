import { useEffect, useState, type ClipboardEvent } from 'react';
import { useIntl } from 'react-intl';
import {
  Check,
  X,
  ArrowLeft,
  ArrowRight,
  Loader2,
  Sparkles,
  Play,
  AlertTriangle,
} from 'lucide-react';
import { cn } from '@/lib/utils';
import { api, type RedactionCustomRule, type RedactionDryRunResult, type RedactionDryRunHit } from '@/lib/api';
import { formatError } from '@/lib/toast';
import {
  Button,
  Input,
  Textarea,
  Badge,
  Tabs,
  TabsList,
  TabsTab,
  TabsPanel,
  Table,
  TableHeader,
  TableBody,
  TableRow,
  TableHead,
  TableCell,
} from '@/components/mds';
import { FieldBlock } from '@/pages/agent-form/form-rows';
import { RedactionTokenInput } from './RedactionTokenInput';
import { categoryLabel } from './RedactionTab';
import {
  emptyMyRuleForm,
  myRuleToFormState,
  myRuleFormToUpsert,
  setMyRuleLabel,
  applyCategoryChip,
  validateMyRuleStep1,
  validateMyRuleStep2,
  isMyRuleStep1Valid,
  isMyRuleStep2Valid,
  parseExampleLines,
  parseKeywordInput,
  mergeKeywords,
  runPatternChecks,
  testPatternAgainstValue,
  buildMyRuleSampleText,
  draftRuleId,
  formToDraftRule,
  MY_RULE_LABEL_MAX,
  MY_RULE_KEYWORD_MIN_LEN,
  MY_RULE_PATTERN_MAX,
  MY_RULE_EXAMPLES_MIN,
  MY_RULE_EXAMPLES_MAX,
  MY_RULE_COUNTER_EXAMPLES_MAX,
  type MyRuleFormState,
  type MyRuleMethod,
  type MyRulePatternCheck,
} from './redactionMyRules';

// ── "我的規則" inline wizard (canvas screens 4-8, §12.2) ─────────────────
//
// Same visual grammar as `RedactionSourceWizard.tsx` (step-circle header,
// back/cancel/next-or-save footer) but rendered INLINE inside
// `RedactionMyRulesCard`'s section — like `FieldRuleFormPanel` — rather than
// in a modal `Dialog`: the canvas's `.hero-drawer` sits in the card, not a
// dialog overlay.
//
// Step 3's "試一試" now runs a REAL `redaction.dry_run` call carrying this
// rule as an unsaved `draft_rules` entry (backend follow-up): the draft is
// compiled into a candidate pipeline for that single call only, never
// written anywhere, and overrides a saved rule sharing the same id — which
// is exactly the "preview my edit" case. Hits it produces carry the draft's
// own working id (`draftRuleId`) as `rule_id`, so "這條規則" is a real hit
// count from the live engine, not a client-side estimate — there is no more
// "本機估算" disclaimer anywhere in this panel. The RPC still never returns
// original values or character offsets (RedactionDryRunHit's own doc
// comment), so there remains no contract-legal way to paint a redacted span
// over the pasted text — only the count + the pointer/category/token table.

type WizardStep = 1 | 2 | 3;
export type MyRuleWizardTarget = { mode: 'create' } | { mode: 'edit'; rule: RedactionCustomRule };

function StepIndicator({ step }: { step: WizardStep }) {
  const intl = useIntl();
  const labels = ['what', 'how', 'try'] as const;
  return (
    <ol className="flex items-center gap-2">
      {labels.map((key, i) => {
        const n = (i + 1) as WizardStep;
        const done = n < step;
        const active = n === step;
        return (
          <li key={key} className="flex flex-1 items-center gap-1.5">
            <span
              className={cn(
                'grid size-6 shrink-0 place-items-center rounded-full text-xs font-medium tabular-nums transition-colors',
                done && 'bg-success/15 text-success',
                active && 'bg-brand text-brand-foreground',
                !done && !active && 'bg-muted text-muted-foreground',
              )}
            >
              {done ? <Check className="size-3.5" /> : n}
            </span>
            <span className={cn('hidden truncate text-xs sm:block', active ? 'font-medium text-foreground' : 'text-muted-foreground')}>
              {intl.formatMessage({ id: `redaction.myRules.wizard.step.${key}` })}
            </span>
            {i < labels.length - 1 && <span className="h-px flex-1 bg-surface-border" aria-hidden="true" />}
          </li>
        );
      })}
    </ol>
  );
}

function ErrorBanner({ message }: { message: string }) {
  return (
    <div className="flex gap-2.5 rounded-lg border border-destructive/30 bg-destructive/10 px-3 py-2.5 text-xs text-destructive">
      <AlertTriangle className="mt-0.5 size-4 shrink-0" />
      <span>{message}</span>
    </div>
  );
}

// ── Step 1 ───────────────────────────────────────────────────────────────

function Step1({
  form,
  onChange,
  existingCategories,
  categoryLabels,
  attempted,
}: {
  form: MyRuleFormState;
  onChange: (next: MyRuleFormState) => void;
  existingCategories: string[];
  categoryLabels: Record<string, string> | undefined;
  attempted: boolean;
}) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);
  const errors = validateMyRuleStep1(form);
  return (
    <div className="space-y-3">
      <FieldBlock label={t('redaction.myRules.wizard.step1.label.label')} description={t('redaction.myRules.wizard.step1.label.hint')}>
        <Input
          value={form.label}
          onChange={(e) => onChange(setMyRuleLabel(form, e.target.value))}
          maxLength={MY_RULE_LABEL_MAX}
          placeholder={t('redaction.myRules.wizard.step1.label.placeholder') as string}
          aria-invalid={attempted && !!errors.label}
        />
        {attempted && errors.label && (
          <p className="mt-1 text-xs text-destructive">
            {t(errors.label === 'empty' ? 'redaction.myRules.wizard.step1.error.empty' : 'redaction.myRules.wizard.step1.error.tooLong', { max: MY_RULE_LABEL_MAX })}
          </p>
        )}
      </FieldBlock>
      {existingCategories.length > 0 && (
        <div className="space-y-1.5">
          <p className="text-xs text-muted-foreground">{t('redaction.myRules.wizard.step1.existing')}</p>
          <div className="flex flex-wrap gap-1.5">
            {existingCategories.map((cat) => {
              const label = categoryLabel(intl, cat, categoryLabels);
              const selected = form.category === cat;
              return (
                <button
                  key={cat}
                  type="button"
                  onClick={() => onChange(applyCategoryChip(form, cat, label))}
                  aria-pressed={selected}
                  className={cn(
                    'rounded-full border px-2.5 py-1 text-xs font-medium transition-colors',
                    selected ? 'border-brand/40 bg-brand/10 text-brand' : 'border-input text-muted-foreground hover:bg-muted',
                  )}
                >
                  {label}
                </button>
              );
            })}
          </div>
        </div>
      )}
      <p className="text-xs text-muted-foreground">{t('redaction.myRules.wizard.step1.futureHint')}</p>
    </div>
  );
}

// ── Step 2 ───────────────────────────────────────────────────────────────

function checkRows(checks: readonly MyRulePatternCheck[]): Array<{ check: MyRulePatternCheck; n: number }> {
  let exN = 0;
  let coN = 0;
  return checks.map((check) => ({ check, n: check.kind === 'example' ? ++exN : ++coN }));
}

function ChecksTable({ checks }: { checks: readonly MyRulePatternCheck[] }) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);
  if (checks.length === 0) return null;
  return (
    <div className="overflow-x-auto rounded-lg border border-surface-border">
      <Table>
        <TableHeader>
          <TableRow>
            <TableHead>{t('redaction.myRules.wizard.step2.example.checks.check')}</TableHead>
            <TableHead className="font-mono">{t('redaction.myRules.wizard.step2.example.checks.value')}</TableHead>
            <TableHead>{t('redaction.myRules.wizard.step2.example.checks.result')}</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {checkRows(checks).map(({ check, n }, i) => (
            <TableRow key={i}>
              <TableCell className="text-xs">
                {t(check.kind === 'example' ? 'redaction.myRules.wizard.step2.example.checks.example' : 'redaction.myRules.wizard.step2.example.checks.counter', { n })}
              </TableCell>
              <TableCell className="font-mono text-xs">{check.value}</TableCell>
              <TableCell>
                {check.ok ? (
                  <span className="flex items-center gap-1 text-xs text-success">
                    <Check className="size-3.5" />
                    {t(check.kind === 'example' ? 'redaction.myRules.wizard.step2.example.checks.matched' : 'redaction.myRules.wizard.step2.example.checks.noMatchGood')}
                  </span>
                ) : (
                  <span className="flex items-center gap-1 text-xs text-destructive">
                    <X className="size-3.5" />
                    {t(check.kind === 'example' ? 'redaction.myRules.wizard.step2.example.checks.noMatchBad' : 'redaction.myRules.wizard.step2.example.checks.matchedBad')}
                  </span>
                )}
              </TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
    </div>
  );
}

function Step2({
  form,
  onChange,
  attempted,
  suggesting,
  suggestError,
  onSuggest,
}: {
  form: MyRuleFormState;
  onChange: (next: MyRuleFormState) => void;
  attempted: boolean;
  suggesting: boolean;
  suggestError: string | null;
  onSuggest: () => void;
}) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);
  const errors = validateMyRuleStep2(form);

  // Raw textarea text lives here (NOT re-derived from `form.examples.join(
  // '\n')`) so pressing Enter to start a fresh line never gets immediately
  // collapsed by `parseExampleLines`' blank-line filter — see this file's
  // header comment on why `RedactionMyRuleWizard` renders a fresh instance
  // (via a `key`) per rule rather than trying to keep this in sync.
  const [examplesRaw, setExamplesRaw] = useState(() => form.examples.join('\n'));
  const [counterRaw, setCounterRaw] = useState(() => form.counterExamples.join('\n'));
  const [testValue, setTestValue] = useState('');

  const handleKeywordPaste = (e: ClipboardEvent<HTMLDivElement>) => {
    const text = e.clipboardData.getData('text');
    if (/[,，\n]/.test(text)) {
      e.preventDefault();
      onChange({ ...form, keywords: mergeKeywords(form.keywords, parseKeywordInput(text)) });
    }
  };

  const checkResult = runPatternChecks(form.examplePattern, form.examples, form.counterExamples);
  const testOutcome = testPatternAgainstValue(form.advancedPattern, testValue);

  return (
    <div className="space-y-3.5">
      <Tabs variant="line" value={form.method} onValueChange={(v) => onChange({ ...form, method: (v ?? 'keyword') as MyRuleMethod })}>
        <TabsList>
          <TabsTab value="keyword">{t('redaction.myRules.wizard.step2.tab.keyword')}</TabsTab>
          <TabsTab value="example">{t('redaction.myRules.wizard.step2.tab.example')}</TabsTab>
          <TabsTab value="pattern">{t('redaction.myRules.wizard.step2.tab.pattern')}</TabsTab>
        </TabsList>

        <TabsPanel value="keyword" keepMounted className="space-y-2">
          <FieldBlock label={t('redaction.myRules.wizard.step2.keyword.label')} description={t('redaction.myRules.wizard.step2.keyword.hint')}>
            <div onPaste={handleKeywordPaste}>
              <RedactionTokenInput
                tokens={form.keywords}
                onChange={(keywords) => onChange({ ...form, keywords })}
                placeholder={t('redaction.myRules.wizard.step2.keyword.placeholder') as string}
                isTokenValid={(tok) => tok.trim().length >= MY_RULE_KEYWORD_MIN_LEN}
                invalid={attempted && errors.keywords === 'empty'}
              />
            </div>
            {attempted && errors.keywords && (
              <p className="mt-1 text-xs text-destructive">
                {t(
                  errors.keywords === 'empty'
                    ? 'redaction.myRules.wizard.step2.keyword.error.empty'
                    : errors.keywords === 'tooMany'
                      ? 'redaction.myRules.wizard.step2.keyword.error.tooMany'
                      : 'redaction.myRules.wizard.step2.keyword.error.tooShort',
                  { min: MY_RULE_KEYWORD_MIN_LEN },
                )}
              </p>
            )}
          </FieldBlock>
          {form.keywords.length > 0 && (
            <p className="text-xs text-muted-foreground">{t('redaction.myRules.wizard.step2.keyword.count', { count: form.keywords.length })}</p>
          )}
        </TabsPanel>

        <TabsPanel value="example" keepMounted className="space-y-3">
          <div className="grid gap-3 sm:grid-cols-2">
            <FieldBlock label={t('redaction.myRules.wizard.step2.example.examples.label')} description={t('redaction.myRules.wizard.step2.example.examples.hint')}>
              <Textarea
                value={examplesRaw}
                onChange={(e) => {
                  setExamplesRaw(e.target.value);
                  onChange({ ...form, examples: parseExampleLines(e.target.value, MY_RULE_EXAMPLES_MAX) });
                }}
                className="min-h-24 font-mono text-xs"
              />
            </FieldBlock>
            <FieldBlock label={t('redaction.myRules.wizard.step2.example.counter.label')} description={t('redaction.myRules.wizard.step2.example.counter.hint')}>
              <Textarea
                value={counterRaw}
                onChange={(e) => {
                  setCounterRaw(e.target.value);
                  onChange({ ...form, counterExamples: parseExampleLines(e.target.value, MY_RULE_COUNTER_EXAMPLES_MAX) });
                }}
                className="min-h-24 font-mono text-xs"
              />
            </FieldBlock>
          </div>
          <div className="flex flex-wrap items-center gap-2">
            <Button variant="brand" size="sm" onClick={onSuggest} disabled={suggesting || form.examples.length < MY_RULE_EXAMPLES_MIN}>
              {suggesting ? <Loader2 className="animate-spin" /> : <Sparkles />}
              {form.examplePattern ? t('redaction.myRules.wizard.step2.example.regenerate') : t('redaction.myRules.wizard.step2.example.generate')}
            </Button>
            {form.examples.length < MY_RULE_EXAMPLES_MIN && (
              <span className="text-xs text-muted-foreground">{t('redaction.myRules.wizard.step2.example.needMore', { min: MY_RULE_EXAMPLES_MIN })}</span>
            )}
            {form.suggestionEngine && (
              <span className="text-xs text-muted-foreground">{t(`redaction.myRules.wizard.step2.example.engine.${form.suggestionEngine}`)}</span>
            )}
          </div>
          {suggestError && <ErrorBanner message={t('redaction.myRules.wizard.step2.example.suggestError', { message: suggestError }) as string} />}
          {form.examplePattern && (
            <>
              <FieldBlock label={t('redaction.myRules.wizard.step2.example.pattern.label')}>
                <Input
                  value={form.examplePattern}
                  onChange={(e) => onChange({ ...form, examplePattern: e.target.value })}
                  className="font-mono text-xs"
                  maxLength={MY_RULE_PATTERN_MAX}
                />
              </FieldBlock>
              <ChecksTable checks={checkResult.checks} />
              {checkResult.error && (
                <p className="text-xs text-destructive">{t('redaction.myRules.wizard.step2.pattern.error.invalid')}</p>
              )}
              {attempted && errors.notVerified && (
                <p className="text-xs text-destructive">{t('redaction.myRules.wizard.step2.example.notVerified')}</p>
              )}
            </>
          )}
        </TabsPanel>

        <TabsPanel value="pattern" keepMounted className="space-y-3">
          <FieldBlock label={t('redaction.myRules.wizard.step2.pattern.input.label')}>
            <Input
              value={form.advancedPattern}
              onChange={(e) => onChange({ ...form, advancedPattern: e.target.value })}
              className="font-mono text-xs"
              maxLength={MY_RULE_PATTERN_MAX}
              aria-invalid={attempted && !!errors.pattern}
            />
            {attempted && errors.pattern && (
              <p className="mt-1 text-xs text-destructive">
                {t(
                  errors.pattern === 'empty'
                    ? 'redaction.myRules.wizard.step2.pattern.error.empty'
                    : errors.pattern === 'tooLong'
                      ? 'redaction.myRules.wizard.step2.pattern.error.tooLong'
                      : 'redaction.myRules.wizard.step2.pattern.error.invalid',
                  { max: MY_RULE_PATTERN_MAX },
                )}
              </p>
            )}
          </FieldBlock>
          <FieldBlock label={t('redaction.myRules.wizard.step2.pattern.tryValue.label')}>
            <div className="flex items-center gap-2">
              <Input value={testValue} onChange={(e) => setTestValue(e.target.value)} className="font-mono text-xs" />
              {testValue.trim() &&
                (testOutcome.error ? (
                  <span className="shrink-0 text-xs text-destructive">{t('redaction.myRules.wizard.step2.pattern.tryValue.invalid')}</span>
                ) : testOutcome.matched ? (
                  <span className="shrink-0 text-xs text-success">{t('redaction.myRules.wizard.step2.pattern.tryValue.matched')}</span>
                ) : (
                  <span className="shrink-0 text-xs text-muted-foreground">{t('redaction.myRules.wizard.step2.pattern.tryValue.noMatch')}</span>
                ))}
            </div>
          </FieldBlock>
          <p className="text-xs text-muted-foreground">{t('redaction.myRules.wizard.step2.pattern.regexNote')}</p>
        </TabsPanel>
      </Tabs>
    </div>
  );
}

// ── Step 3 ───────────────────────────────────────────────────────────────

function Step3({
  draftId,
  sampleText,
  onSampleTextChange,
  dryRunLoading,
  dryRunError,
  dryRunResult,
  onRun,
  categoryLabels,
}: {
  /** The working rule id sent as this call's `draft_rules[0].id` — hits
   *  carrying it as `rule_id` are THIS rule; everything else is grouped
   *  as "other rules" below, same as before. */
  draftId: string;
  sampleText: string;
  onSampleTextChange: (v: string) => void;
  dryRunLoading: boolean;
  dryRunError: string | null;
  dryRunResult: RedactionDryRunResult | null;
  onRun: () => void;
  categoryLabels: Record<string, string> | undefined;
}) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);
  const allHits: RedactionDryRunHit[] = dryRunResult?.hits ?? [];
  const ownHits = allHits.filter((h) => h.rule_id === draftId);
  const otherHits = allHits.filter((h) => h.rule_id !== draftId);

  return (
    <div className="space-y-3.5">
      <FieldBlock label={t('redaction.myRules.wizard.step3.sample.label')} description={t('redaction.myRules.wizard.step3.sample.hint')}>
        <Textarea value={sampleText} onChange={(e) => onSampleTextChange(e.target.value)} className="min-h-28 text-sm" />
      </FieldBlock>

      <div className="flex flex-wrap items-center gap-2">
        <Button variant="outline" size="sm" onClick={onRun} disabled={dryRunLoading || !sampleText.trim()}>
          {dryRunLoading ? <Loader2 className="animate-spin" /> : <Play />}
          {t('redaction.myRules.wizard.step3.run')}
        </Button>
        {dryRunResult && (
          <>
            <Badge className="bg-brand/10 text-brand">{t('redaction.myRules.wizard.step3.thisRule', { count: ownHits.length })}</Badge>
            <Badge className="bg-success/10 text-success">{t('redaction.myRules.wizard.step3.otherRules', { count: otherHits.length })}</Badge>
          </>
        )}
      </div>

      {dryRunError && <ErrorBanner message={t('redaction.myRules.wizard.step3.error', { message: dryRunError }) as string} />}

      {otherHits.length > 0 && (
        <div className="overflow-x-auto rounded-lg border border-surface-border">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>{t('redaction.myRules.wizard.step3.table.category')}</TableHead>
                <TableHead className="font-mono">{t('redaction.myRules.wizard.step3.table.token')}</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {otherHits.map((hit, i) => (
                <TableRow key={i}>
                  <TableCell className="text-xs">{categoryLabel(intl, hit.category, categoryLabels)}</TableCell>
                  <TableCell className="font-mono text-xs">{hit.token}</TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>
      )}

      {dryRunResult && sampleText.trim() && ownHits.length === 0 && (
        <p className="text-xs text-warning">{t('redaction.myRules.wizard.step3.noHitWarn')}</p>
      )}
      <p className="text-xs text-muted-foreground">{t('redaction.myRules.wizard.step3.note')}</p>
    </div>
  );
}

// ── Wizard shell ─────────────────────────────────────────────────────────

export function RedactionMyRuleWizard({
  target,
  existingCategories,
  categoryLabels,
  onCancel,
  onSaved,
}: {
  target: MyRuleWizardTarget;
  /** Category ids selectable as step 1's "既有類型" chips — see
   *  `mergeCategoryChips`. */
  existingCategories: string[];
  categoryLabels: Record<string, string> | undefined;
  onCancel: () => void;
  onSaved: () => void;
}) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);
  const isNew = target.mode === 'create';

  const [step, setStep] = useState<WizardStep>(1);
  const [form, setForm] = useState<MyRuleFormState>(() => (isNew ? emptyMyRuleForm() : myRuleToFormState(target.rule)));
  const [attempted, setAttempted] = useState(false);
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);

  const [suggesting, setSuggesting] = useState(false);
  const [suggestError, setSuggestError] = useState<string | null>(null);

  const [sampleText, setSampleText] = useState(() => buildMyRuleSampleText(isNew ? emptyMyRuleForm() : myRuleToFormState(target.rule)));
  const [dryRunLoading, setDryRunLoading] = useState(false);
  const [dryRunError, setDryRunError] = useState<string | null>(null);
  const [dryRunResult, setDryRunResult] = useState<RedactionDryRunResult | null>(null);
  // Editing reuses the real rule id (so the draft shadows the saved rule in
  // the preview pipeline); a new rule gets one throwaway id for the life of
  // this wizard instance — computed once, not per dry-run, so a saved
  // draft id can never drift mid-edit.
  const [draftId] = useState(() => draftRuleId(target));

  // `RedactionMyRulesCard` gives this component a fresh `key` per target
  // (create vs. each rule's id), so this effect only ever runs once per
  // mount in practice — kept for defensiveness against a future caller that
  // swaps `target` in place without remounting.
  useEffect(() => {
    setStep(1);
    setAttempted(false);
    setSaveError(null);
    setSuggestError(null);
    setDryRunResult(null);
    setDryRunError(null);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [target]);

  const handleNext = () => {
    if (step === 1) {
      setAttempted(true);
      if (!isMyRuleStep1Valid(form)) return;
      setAttempted(false);
      setStep(2);
      return;
    }
    if (step === 2) {
      setAttempted(true);
      if (!isMyRuleStep2Valid(form)) return;
      setAttempted(false);
      if (!sampleText.trim()) setSampleText(buildMyRuleSampleText(form));
      setStep(3);
    }
  };
  const handleBack = () => {
    setAttempted(false);
    if (step > 1) setStep((step - 1) as WizardStep);
  };

  const handleSuggest = async () => {
    setSuggesting(true);
    setSuggestError(null);
    try {
      const result = await api.redaction.suggestPattern(form.examples, form.counterExamples);
      if (result.pattern === null) {
        // Honest empty result (backend: no engine — local, cloud, nor the
        // heuristic fallback — produced a pattern that survives its own
        // checks). Never silently leave the pattern box blank with no
        // explanation.
        setSuggestError(t('redaction.myRules.wizard.step2.example.noPattern') as string);
        setForm((f) => ({ ...f, examplePattern: '', suggestionEngine: null }));
        return;
      }
      setForm((f) => ({ ...f, examplePattern: result.pattern as string, suggestionEngine: result.engine }));
    } catch (e) {
      setSuggestError(formatError(e));
    } finally {
      setSuggesting(false);
    }
  };

  const handleDryRun = async () => {
    setDryRunLoading(true);
    setDryRunError(null);
    try {
      // `dryRunDraft` sends `sample_text` (the gateway wraps it — no more
      // client-side JSON.stringify) plus this rule as an unsaved
      // `draft_rules` entry, so hits tagged with `draftId` are a REAL live
      // count, not a client-side estimate. The tool name is a fixed,
      // non-user-facing label — no real tool result is being simulated,
      // only "what would this rule (plus everything already active) do to
      // this text".
      const result = await api.redaction.dryRunDraft({
        sampleText,
        draftRule: formToDraftRule(form, draftId),
      });
      setDryRunResult(result);
    } catch (e) {
      setDryRunError(formatError(e));
    } finally {
      setDryRunLoading(false);
    }
  };

  const handleSave = async () => {
    setAttempted(true);
    if (!isMyRuleStep1Valid(form) || !isMyRuleStep2Valid(form)) return;
    setSaving(true);
    setSaveError(null);
    try {
      const result = await api.redaction.customRules.upsert(myRuleFormToUpsert(form));
      if (result.warning) {
        // Written to custom.toml but NOT applied live — say so and stay on
        // the panel, matching every other `redaction.*` save in this
        // dashboard (RedactionFieldRulesCard.handleSaveRule).
        setSaveError(result.warning);
        return;
      }
      onSaved();
    } catch (e) {
      setSaveError(formatError(e));
    } finally {
      setSaving(false);
    }
  };

  const baseTitle = isNew ? t('redaction.myRules.wizard.newTitle') : t('redaction.myRules.wizard.editTitle');
  const title = form.label.trim() ? `${baseTitle} · ${form.label.trim()}` : baseTitle;

  return (
    <div className="space-y-3.5 rounded-xl border border-brand/40 bg-card p-4">
      <div className="flex items-center justify-between gap-3">
        <h5 className="truncate text-sm font-medium text-foreground">{title}</h5>
        <Button variant="ghost" size="icon-xs" onClick={onCancel} aria-label={t('common.cancel')}>
          <X />
        </Button>
      </div>

      <StepIndicator step={step} />

      <div className="max-h-[60vh] space-y-4 overflow-y-auto py-1">
        {step === 1 && (
          <Step1 form={form} onChange={setForm} existingCategories={existingCategories} categoryLabels={categoryLabels} attempted={attempted} />
        )}
        {step === 2 && (
          <Step2 form={form} onChange={setForm} attempted={attempted} suggesting={suggesting} suggestError={suggestError} onSuggest={() => void handleSuggest()} />
        )}
        {step === 3 && (
          <Step3
            draftId={draftId}
            sampleText={sampleText}
            onSampleTextChange={setSampleText}
            dryRunLoading={dryRunLoading}
            dryRunError={dryRunError}
            dryRunResult={dryRunResult}
            onRun={() => void handleDryRun()}
            categoryLabels={categoryLabels}
          />
        )}
      </div>

      {saveError && <ErrorBanner message={t('redaction.myRules.wizard.saveError', { message: saveError }) as string} />}

      <div className="flex items-center justify-between gap-2 pt-1">
        <div>
          {step > 1 && (
            <Button variant="outline" size="sm" onClick={handleBack}>
              <ArrowLeft />
              {t('redaction.myRules.wizard.back')}
            </Button>
          )}
        </div>
        <div className="flex items-center gap-2">
          <Button variant="outline" size="sm" onClick={onCancel}>
            {t('common.cancel')}
          </Button>
          {step < 3 ? (
            <Button variant="brand" size="sm" onClick={handleNext}>
              {t('redaction.myRules.wizard.next')}
              <ArrowRight />
            </Button>
          ) : (
            <Button variant="brand" size="sm" onClick={() => void handleSave()} disabled={saving}>
              {saving ? <Loader2 className="animate-spin" /> : <Check />}
              {t('redaction.myRules.wizard.save')}
            </Button>
          )}
        </div>
      </div>
    </div>
  );
}
