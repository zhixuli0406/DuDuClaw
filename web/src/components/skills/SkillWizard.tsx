import { useState, useEffect, useRef, useCallback } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate } from 'react-router';
import { Check, Wand2, Loader2, RefreshCw, ShieldCheck, ShieldAlert, ArrowRight, ArrowLeft, Send } from 'lucide-react';
import { cn } from '@/lib/utils';
import { Card, Button, Field, controlClass, Badge, CharacterAvatar, DuDu, Mono } from '@/components/ui';
import { ChipEditor } from '@/components/shared/ChipEditor';
import { useAgentsStore } from '@/stores/agents-store';
import { toast, formatError } from '@/lib/toast';
import {
  createCustomSkill,
  generateCustomSkill,
  updateCustomSkill,
  submitCustomSkill,
  listCustomSkills,
  type CustomSkillRecord,
  type CustomSubmitResult,
  type TimeSavedUnit,
} from '@/lib/api-custom-skills';
import { formatTimeSaved } from './status-meta';
import {
  WIZARD_STEPS,
  WIZARD_FACE,
  WIZARD_STEP_LABEL,
  stepIndex,
  nextStep,
  prevStep,
  canStartGeneration,
  canProceedFromGenerate,
  canProceedFromForm,
  generationPhase,
  type WizardStep,
} from './wizard-machine';

const textareaClass = cn(controlClass, 'h-auto min-h-[96px] resize-y py-2 leading-relaxed');
const POLL_MS = 3000;

/**
 * SkillWizard — the 4-step `/skills/new` self-serve builder (T13.1). Describe →
 * generate (agent authors the SKILL.md) → fill human fields → safety scan +
 * submit for a manager's approval. DuDu accompanies each step, changing face
 * per the wizard machine (curious → writing → idle → proud).
 */
export function SkillWizard() {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const navigate = useNavigate();
  const { agents, fetchAgents } = useAgentsStore();

  const [step, setStep] = useState<WizardStep>('describe');
  const [record, setRecord] = useState<CustomSkillRecord | null>(null);

  // Step 1 — describe
  const [description, setDescription] = useState('');
  const [builderAgent, setBuilderAgent] = useState('');
  const [starting, setStarting] = useState(false);

  // Step 2 — generate
  const [instruction, setInstruction] = useState('');
  const [regenerating, setRegenerating] = useState(false);
  const [draftConfirmed, setDraftConfirmed] = useState(false);
  const [draftPath, setDraftPath] = useState('');

  // Step 3 — form
  const [displayName, setDisplayName] = useState('');
  const [descriptionHuman, setDescriptionHuman] = useState('');
  const [timeSavedValue, setTimeSavedValue] = useState('30');
  const [timeSavedUnit, setTimeSavedUnit] = useState<TimeSavedUnit>('minutes_per_use');
  const [tags, setTags] = useState<string[]>([]);
  const [savingForm, setSavingForm] = useState(false);

  // Step 4 — review / submit
  const [submitting, setSubmitting] = useState(false);
  const [submitResult, setSubmitResult] = useState<CustomSubmitResult | null>(null);
  const [submitError, setSubmitError] = useState<string | null>(null);

  useEffect(() => {
    fetchAgents();
  }, [fetchAgents]);

  useEffect(() => {
    if (!builderAgent && agents.length > 0) setBuilderAgent(agents[0].name);
  }, [agents, builderAgent]);

  // ── Poll the record's status while on the generate step ──
  const pollRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const stopPoll = useCallback(() => {
    if (pollRef.current) {
      clearInterval(pollRef.current);
      pollRef.current = null;
    }
  }, []);

  const refreshRecord = useCallback(async () => {
    if (!record) return;
    try {
      const res = await listCustomSkills();
      const found = res.custom_skills.find((s) => s.id === record.id);
      if (found) setRecord(found);
    } catch (e) {
      console.warn('[api]', e);
    }
  }, [record]);

  useEffect(() => {
    if (step !== 'generate' || !record) {
      stopPoll();
      return;
    }
    refreshRecord();
    pollRef.current = setInterval(refreshRecord, POLL_MS);
    return stopPoll;
  }, [step, record, refreshRecord, stopPoll]);

  // ── Step 1 → 2: create the record + kick off generation ──
  const handleStart = useCallback(async () => {
    if (!canStartGeneration(description, builderAgent) || starting) return;
    setStarting(true);
    try {
      // Provisional display name (editable in step 3) derived from the request.
      const provisional = description.trim().split('\n')[0].slice(0, 60) || t('skills.new.defaultName');
      const created = await createCustomSkill({
        display_name: provisional,
        description_human: description.trim(),
        built_by_agent: builderAgent,
        time_saved_value: 30,
        time_saved_unit: 'minutes_per_use',
      });
      setRecord(created);
      setDisplayName(created.display_name);
      setDescriptionHuman(created.description_human);
      const gen = await generateCustomSkill({ id: created.id });
      setDraftPath(gen.draft_path);
      setDraftConfirmed(false);
      setStep('generate');
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
    } finally {
      setStarting(false);
    }
  }, [description, builderAgent, starting, intl, t]);

  // ── Step 2: regenerate with a revision note ──
  const handleRegenerate = useCallback(async () => {
    if (!record || regenerating) return;
    setRegenerating(true);
    try {
      const gen = await generateCustomSkill({ id: record.id, instruction: instruction.trim() || undefined });
      setDraftPath(gen.draft_path);
      setInstruction('');
      setDraftConfirmed(false);
      await refreshRecord();
      toast.success(t('skills.new.regenerated'));
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
    } finally {
      setRegenerating(false);
    }
  }, [record, regenerating, instruction, refreshRecord, intl, t]);

  // ── Step 3 → 4: persist the human fields ──
  const handleSaveForm = useCallback(async () => {
    if (!record || !canProceedFromForm(displayName) || savingForm) return;
    setSavingForm(true);
    try {
      const updated = await updateCustomSkill({
        id: record.id,
        display_name: displayName.trim(),
        description_human: descriptionHuman.trim(),
        time_saved_value: Number(timeSavedValue) || 0,
        time_saved_unit: timeSavedUnit,
        tags: tags.join(','),
      });
      setRecord(updated);
      setSubmitResult(null);
      setSubmitError(null);
      setStep('review');
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
    } finally {
      setSavingForm(false);
    }
  }, [record, displayName, descriptionHuman, timeSavedValue, timeSavedUnit, tags, savingForm, intl]);

  // ── Step 4: safety scan + submit for approval (fail-closed) ──
  const handleSubmit = useCallback(async () => {
    if (!record || submitting) return;
    setSubmitting(true);
    setSubmitError(null);
    try {
      const res = await submitCustomSkill(record.id);
      setSubmitResult(res);
    } catch (e) {
      // High/critical risk (or missing draft) comes back as an error frame —
      // surfaced verbatim; the human cannot bypass it.
      setSubmitError(formatError(e));
    } finally {
      setSubmitting(false);
    }
  }, [record, submitting]);

  return (
    <div className="mx-auto max-w-2xl space-y-6">
      <Stepper step={step} />

      {step === 'describe' && (
        <StepDescribe
          description={description}
          onDescription={setDescription}
          builderAgent={builderAgent}
          onBuilderAgent={setBuilderAgent}
          agents={agents}
          starting={starting}
          onStart={handleStart}
        />
      )}

      {step === 'generate' && record && (
        <StepGenerate
          record={record}
          draftPath={draftPath}
          instruction={instruction}
          onInstruction={setInstruction}
          regenerating={regenerating}
          onRegenerate={handleRegenerate}
          draftConfirmed={draftConfirmed}
          onConfirmDraft={() => setDraftConfirmed(true)}
          onBack={() => setStep(prevStep('generate'))}
          onNext={() => setStep(nextStep('generate'))}
        />
      )}

      {step === 'form' && record && (
        <StepForm
          displayName={displayName}
          onDisplayName={setDisplayName}
          descriptionHuman={descriptionHuman}
          onDescriptionHuman={setDescriptionHuman}
          timeSavedValue={timeSavedValue}
          onTimeSavedValue={setTimeSavedValue}
          timeSavedUnit={timeSavedUnit}
          onTimeSavedUnit={setTimeSavedUnit}
          tags={tags}
          onTags={setTags}
          saving={savingForm}
          onBack={() => setStep(prevStep('form'))}
          onNext={handleSaveForm}
        />
      )}

      {step === 'review' && record && (
        <StepReview
          record={record}
          submitting={submitting}
          result={submitResult}
          error={submitError}
          onSubmit={handleSubmit}
          onBack={() => { setSubmitError(null); setStep(prevStep('review')); }}
          onView={() => navigate(`/skills/custom/${record.id}`)}
        />
      )}
    </div>
  );
}

// ── Stepper with DuDu companion ─────────────────────────────

function Stepper({ step }: { step: WizardStep }) {
  const intl = useIntl();
  const current = stepIndex(step);
  return (
    <div className="flex items-center gap-4">
      <DuDu face={WIZARD_FACE[step]} size="sm" />
      <ol className="flex flex-1 items-center gap-2">
        {WIZARD_STEPS.map((s, i) => {
          const done = i < current;
          const active = i === current;
          return (
            <li key={s} className="flex flex-1 items-center gap-2">
              <span
                className={cn(
                  'grid h-7 w-7 shrink-0 place-items-center rounded-full text-xs font-semibold tabular-nums transition-colors',
                  done && 'bg-emerald-500/15 text-emerald-600 dark:text-emerald-400',
                  active && 'bg-amber-500 text-white',
                  !done && !active && 'bg-stone-500/10 text-stone-400 dark:bg-white/5',
                )}
              >
                {done ? <Check className="h-3.5 w-3.5" /> : i + 1}
              </span>
              <span
                className={cn(
                  'hidden truncate text-xs font-medium sm:block',
                  active ? 'text-stone-800 dark:text-stone-100' : 'text-stone-400',
                )}
              >
                {intl.formatMessage({ id: WIZARD_STEP_LABEL[s] })}
              </span>
              {i < WIZARD_STEPS.length - 1 && (
                <span className="h-px flex-1 bg-[var(--panel-border)]" aria-hidden="true" />
              )}
            </li>
          );
        })}
      </ol>
    </div>
  );
}

// ── Step 1 — describe ───────────────────────────────────────

function StepDescribe({
  description,
  onDescription,
  builderAgent,
  onBuilderAgent,
  agents,
  starting,
  onStart,
}: {
  description: string;
  onDescription: (v: string) => void;
  builderAgent: string;
  onBuilderAgent: (v: string) => void;
  agents: ReadonlyArray<{ name: string; display_name: string; status: string }>;
  starting: boolean;
  onStart: () => void;
}) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  return (
    <Card className="space-y-5">
      <Field label={t('skills.new.describe.label')} help={t('skills.new.describe.help')}>
        <textarea
          className={textareaClass}
          value={description}
          onChange={(e) => onDescription(e.target.value)}
          placeholder={t('skills.new.describe.placeholder')}
          rows={5}
        />
      </Field>

      <Field label={t('skills.new.builder.label')} help={t('skills.new.builder.help')}>
        <div className="flex flex-wrap gap-2">
          {agents.map((a) => {
            const selected = a.name === builderAgent;
            return (
              <button
                key={a.name}
                type="button"
                onClick={() => onBuilderAgent(a.name)}
                aria-pressed={selected}
                className={cn(
                  'flex items-center gap-2 rounded-xl border px-3 py-2 text-sm transition-colors',
                  selected
                    ? 'border-amber-500/60 bg-amber-500/10 text-stone-800 dark:text-stone-100'
                    : 'border-[var(--panel-border)] text-stone-600 hover:bg-stone-500/5 dark:text-stone-300',
                )}
              >
                <CharacterAvatar agentId={a.name} name={a.display_name} size={22} />
                <span className="truncate">{a.display_name}</span>
              </button>
            );
          })}
          {agents.length === 0 && (
            <p className="text-sm text-stone-400">{t('skills.new.builder.none')}</p>
          )}
        </div>
      </Field>

      <div className="flex justify-end border-t border-[var(--panel-border)] pt-4">
        <Button
          variant="primary"
          icon={starting ? Loader2 : Wand2}
          onClick={onStart}
          disabled={starting || !description.trim() || !builderAgent}
          className={cn(starting && '[&>svg]:animate-spin')}
        >
          {starting ? t('skills.new.starting') : t('skills.new.start')}
        </Button>
      </div>
    </Card>
  );
}

// ── Step 2 — generate ───────────────────────────────────────

function StepGenerate({
  record,
  draftPath,
  instruction,
  onInstruction,
  regenerating,
  onRegenerate,
  draftConfirmed,
  onConfirmDraft,
  onBack,
  onNext,
}: {
  record: CustomSkillRecord;
  draftPath: string;
  instruction: string;
  onInstruction: (v: string) => void;
  regenerating: boolean;
  onRegenerate: () => void;
  draftConfirmed: boolean;
  onConfirmDraft: () => void;
  onBack: () => void;
  onNext: () => void;
}) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const phase = generationPhase(record.status);
  const canNext = canProceedFromGenerate(record.status, draftConfirmed);

  return (
    <Card className="space-y-5">
      <div className="flex flex-col items-center gap-3 py-4 text-center">
        <DuDu face={phase === 'ready' ? 'proud' : 'writing'} size="md" animated={phase !== 'ready'} />
        <p className="text-sm font-medium text-stone-700 dark:text-stone-200">
          {phase === 'ready' ? t('skills.new.generate.ready') : t('skills.new.generate.working')}
        </p>
        <p className="max-w-md text-xs text-stone-500 dark:text-stone-400">
          {phase === 'ready' ? t('skills.new.generate.readyHint') : t('skills.new.generate.workingHint')}
        </p>
      </div>

      <Field label={t('skills.new.generate.draftPath')}>
        <Mono className="block truncate text-xs" title={draftPath}>{draftPath || '—'}</Mono>
      </Field>

      {/* Revision request → regenerate */}
      <Field label={t('skills.new.generate.reviseLabel')} help={t('skills.new.generate.reviseHelp')}>
        <textarea
          className={textareaClass}
          value={instruction}
          onChange={(e) => onInstruction(e.target.value)}
          placeholder={t('skills.new.generate.revisePlaceholder')}
          rows={3}
        />
      </Field>
      <div className="flex items-center gap-2">
        <Button
          variant="secondary"
          icon={regenerating ? Loader2 : RefreshCw}
          onClick={onRegenerate}
          disabled={regenerating}
          className={cn(regenerating && '[&>svg]:animate-spin')}
        >
          {regenerating ? t('skills.new.generate.regenerating') : t('skills.new.generate.regenerate')}
        </Button>
        {!canNext && !draftConfirmed && (
          <Button variant="ghost" size="sm" onClick={onConfirmDraft}>
            {t('skills.new.generate.confirmDone')}
          </Button>
        )}
      </div>

      <div className="flex items-center justify-between border-t border-[var(--panel-border)] pt-4">
        <Button variant="ghost" icon={ArrowLeft} onClick={onBack}>
          {t('common.back')}
        </Button>
        <Button variant="primary" iconRight={ArrowRight} onClick={onNext} disabled={!canNext}>
          {t('skills.new.generate.toForm')}
        </Button>
      </div>
    </Card>
  );
}

// ── Step 3 — human fields form ──────────────────────────────

const TIME_UNITS: TimeSavedUnit[] = ['minutes_per_use', 'hours_per_month'];

function StepForm({
  displayName,
  onDisplayName,
  descriptionHuman,
  onDescriptionHuman,
  timeSavedValue,
  onTimeSavedValue,
  timeSavedUnit,
  onTimeSavedUnit,
  tags,
  onTags,
  saving,
  onBack,
  onNext,
}: {
  displayName: string;
  onDisplayName: (v: string) => void;
  descriptionHuman: string;
  onDescriptionHuman: (v: string) => void;
  timeSavedValue: string;
  onTimeSavedValue: (v: string) => void;
  timeSavedUnit: TimeSavedUnit;
  onTimeSavedUnit: (v: TimeSavedUnit) => void;
  tags: string[];
  onTags: (v: string[]) => void;
  saving: boolean;
  onBack: () => void;
  onNext: () => void;
}) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  return (
    <Card className="space-y-5">
      <Field label={t('skills.new.form.displayName')} required>
        <input
          className={controlClass}
          value={displayName}
          onChange={(e) => onDisplayName(e.target.value)}
          placeholder={t('skills.new.form.displayNamePlaceholder')}
        />
      </Field>

      <Field label={t('skills.new.form.description')}>
        <textarea
          className={textareaClass}
          value={descriptionHuman}
          onChange={(e) => onDescriptionHuman(e.target.value)}
          rows={4}
        />
      </Field>

      <Field label={t('skills.new.form.timeSaved')} help={t('skills.new.form.timeSavedHelp')}>
        <div className="flex gap-2">
          <input
            type="number"
            min={0}
            className={cn(controlClass, 'w-28')}
            value={timeSavedValue}
            onChange={(e) => onTimeSavedValue(e.target.value)}
          />
          <select
            className={cn(controlClass, 'w-auto flex-1')}
            value={timeSavedUnit}
            onChange={(e) => onTimeSavedUnit(e.target.value as TimeSavedUnit)}
          >
            {TIME_UNITS.map((u) => (
              <option key={u} value={u}>
                {t(`skills.custom.unit.${u}`)}
              </option>
            ))}
          </select>
        </div>
      </Field>

      <Field label={t('skills.new.form.tags')}>
        <ChipEditor values={tags} onChange={onTags} placeholder={t('skills.new.form.tagsPlaceholder')} />
      </Field>

      <div className="flex items-center justify-between border-t border-[var(--panel-border)] pt-4">
        <Button variant="ghost" icon={ArrowLeft} onClick={onBack}>
          {t('common.back')}
        </Button>
        <Button
          variant="primary"
          icon={saving ? Loader2 : undefined}
          iconRight={saving ? undefined : ArrowRight}
          onClick={onNext}
          disabled={saving || !displayName.trim()}
          className={cn(saving && '[&>svg]:animate-spin')}
        >
          {saving ? t('common.saving') : t('skills.new.form.toReview')}
        </Button>
      </div>
    </Card>
  );
}

// ── Step 4 — safety + submit ────────────────────────────────

function StepReview({
  record,
  submitting,
  result,
  error,
  onSubmit,
  onBack,
  onView,
}: {
  record: CustomSkillRecord;
  submitting: boolean;
  result: CustomSubmitResult | null;
  error: string | null;
  onSubmit: () => void;
  onBack: () => void;
  onView: () => void;
}) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });

  // Success — submitted, awaiting approval.
  if (result) {
    const sr = result.safety_report;
    return (
      <Card className="space-y-4">
        <div className="flex flex-col items-center gap-3 py-3 text-center">
          <DuDu face="proud" size="md" />
          <p className="text-sm font-semibold text-stone-800 dark:text-stone-100">
            {t('skills.new.submitted.title')}
          </p>
          <p className="max-w-md text-xs text-stone-500 dark:text-stone-400">
            {t('skills.new.submitted.awaiting')}
          </p>
        </div>
        <div className="space-y-2 rounded-control bg-stone-500/8 p-3 text-xs dark:bg-white/5">
          <div className="flex items-center justify-between gap-2">
            <span className="text-stone-500 dark:text-stone-400">{t('skills.new.submitted.approvalId')}</span>
            <Mono className="truncate">{result.approval_id}</Mono>
          </div>
          <div className="flex items-center justify-between gap-2">
            <span className="text-stone-500 dark:text-stone-400">{t('skills.new.submitted.risk')}</span>
            <Badge tone={sr.passed ? 'success' : 'warning'}>{sr.risk_level}</Badge>
          </div>
          <div className="flex items-center justify-between gap-2">
            <span className="text-stone-500 dark:text-stone-400">{t('skills.new.submitted.findings')}</span>
            <span className="tabular-nums text-stone-700 dark:text-stone-200">{sr.findings.length}</span>
          </div>
        </div>
        <div className="flex justify-end">
          <Button variant="primary" onClick={onView}>
            {t('skills.new.submitted.viewDetail')}
          </Button>
        </div>
      </Card>
    );
  }

  return (
    <Card className="space-y-5">
      <div className="flex items-start gap-3">
        <ShieldCheck className="mt-0.5 h-5 w-5 shrink-0 text-amber-500" />
        <div className="space-y-1">
          <p className="text-sm font-medium text-stone-800 dark:text-stone-100">{t('skills.new.review.title')}</p>
          <p className="text-xs text-stone-500 dark:text-stone-400">{t('skills.new.review.explain')}</p>
        </div>
      </div>

      <div className="space-y-1.5 rounded-control bg-stone-500/8 p-3 text-xs dark:bg-white/5">
        <div className="flex justify-between gap-2">
          <span className="text-stone-500 dark:text-stone-400">{t('skills.new.form.displayName')}</span>
          <span className="truncate font-medium text-stone-800 dark:text-stone-100">{record.display_name}</span>
        </div>
        <div className="flex justify-between gap-2">
          <span className="text-stone-500 dark:text-stone-400">{t('skills.custom.slug')}</span>
          <Mono className="truncate">{record.slug}</Mono>
        </div>
        <div className="flex justify-between gap-2">
          <span className="text-stone-500 dark:text-stone-400">{t('skills.new.form.timeSaved')}</span>
          <span className="text-stone-700 dark:text-stone-200">
            {formatTimeSaved(intl, record.time_saved_value, record.time_saved_unit)}
          </span>
        </div>
      </div>

      {/* Fail-closed error frame: high/critical risk block or missing draft */}
      {error && (
        <div className="flex items-start gap-2 rounded-lg border border-rose-200 bg-rose-50 p-3 text-sm text-rose-700 dark:border-rose-800 dark:bg-rose-900/20 dark:text-rose-400">
          <ShieldAlert className="mt-0.5 h-4 w-4 shrink-0" />
          <div className="min-w-0 space-y-1">
            <p className="font-medium">{t('skills.new.review.blocked')}</p>
            <p className="break-words text-xs">{error}</p>
          </div>
        </div>
      )}

      <div className="flex items-center justify-between border-t border-[var(--panel-border)] pt-4">
        <Button variant="ghost" icon={ArrowLeft} onClick={onBack}>
          {t('common.back')}
        </Button>
        <Button
          variant="primary"
          icon={submitting ? Loader2 : Send}
          onClick={onSubmit}
          disabled={submitting}
          className={cn(submitting && '[&>svg]:animate-spin')}
        >
          {submitting ? t('skills.new.review.submitting') : t('skills.new.review.submit')}
        </Button>
      </div>
    </Card>
  );
}
