import { useState, useEffect, useRef, useCallback, type ReactNode } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate } from 'react-router';
import { Check, Wand2, Loader2, RefreshCw, ShieldCheck, ShieldAlert, ArrowRight, ArrowLeft, Send } from 'lucide-react';
import { cn } from '@/lib/utils';
import {
  Card,
  CardContent,
  Button,
  Badge,
  Input,
  Textarea,
  Select,
  SelectTrigger,
  SelectValue,
  SelectContent,
  SelectItem,
  ActorAvatar,
} from '@/components/mds';
import { DuDu } from '@/components/mascot';
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

const POLL_MS = 3000;

/** Local field wrapper (label + optional help), MDS-styled. */
function Field({
  label,
  help,
  required,
  children,
}: {
  label: ReactNode;
  help?: ReactNode;
  required?: boolean;
  children: ReactNode;
}) {
  return (
    <div className="space-y-1.5">
      <label className="flex items-center gap-1 text-xs font-medium text-muted-foreground">
        {label}
        {required && <span className="text-destructive">*</span>}
      </label>
      {children}
      {help && <p className="text-xs text-muted-foreground">{help}</p>}
    </div>
  );
}

/**
 * SkillWizard — the 4-step `/skills/new` self-serve builder (T13.1). Describe →
 * generate (agent authors the SKILL.md) → fill human fields → safety scan +
 * submit for a manager's approval. DuDu accompanies each step as a small
 * illustration; the surface is now MDS (spec §5.3 detail-page container).
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
          onBack={() => {
            setSubmitError(null);
            setStep(prevStep('review'));
          }}
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
                  'grid size-7 shrink-0 place-items-center rounded-full text-xs font-medium tabular-nums transition-colors',
                  done && 'bg-success/15 text-success',
                  active && 'bg-brand text-brand-foreground',
                  !done && !active && 'bg-muted text-muted-foreground',
                )}
              >
                {done ? <Check className="size-3.5" /> : i + 1}
              </span>
              <span
                className={cn(
                  'hidden truncate text-xs font-medium sm:block',
                  active ? 'text-foreground' : 'text-muted-foreground',
                )}
              >
                {intl.formatMessage({ id: WIZARD_STEP_LABEL[s] })}
              </span>
              {i < WIZARD_STEPS.length - 1 && <span className="h-px flex-1 bg-surface-border" aria-hidden="true" />}
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
    <Card>
      <CardContent className="space-y-5">
        <Field label={t('skills.new.describe.label')} help={t('skills.new.describe.help')}>
          <Textarea
            value={description}
            onChange={(e) => onDescription(e.target.value)}
            placeholder={t('skills.new.describe.placeholder')}
            rows={5}
            className="min-h-24"
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
                    'flex items-center gap-2 rounded-lg border px-3 py-2 text-sm transition-colors',
                    selected
                      ? 'border-brand bg-brand/8 text-foreground'
                      : 'border-border text-muted-foreground hover:bg-muted',
                  )}
                >
                  <ActorAvatar actorType="agent" size="sm" name={a.display_name} />
                  <span className="truncate">{a.display_name}</span>
                </button>
              );
            })}
            {agents.length === 0 && <p className="text-sm text-muted-foreground">{t('skills.new.builder.none')}</p>}
          </div>
        </Field>

        <div className="flex justify-end border-t border-surface-border pt-4">
          <Button variant="brand" onClick={onStart} disabled={starting || !description.trim() || !builderAgent}>
            {starting ? <Loader2 className="animate-spin" /> : <Wand2 />}
            {starting ? t('skills.new.starting') : t('skills.new.start')}
          </Button>
        </div>
      </CardContent>
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
    <Card>
      <CardContent className="space-y-5">
        <div className="flex flex-col items-center gap-3 py-4 text-center">
          <DuDu face={phase === 'ready' ? 'proud' : 'writing'} size="md" animated={phase !== 'ready'} />
          <p className="text-sm font-medium text-foreground">
            {phase === 'ready' ? t('skills.new.generate.ready') : t('skills.new.generate.working')}
          </p>
          <p className="max-w-md text-xs text-muted-foreground">
            {phase === 'ready' ? t('skills.new.generate.readyHint') : t('skills.new.generate.workingHint')}
          </p>
        </div>

        <Field label={t('skills.new.generate.draftPath')}>
          <span className="block truncate font-mono text-xs text-muted-foreground" title={draftPath}>
            {draftPath || '—'}
          </span>
        </Field>

        <Field label={t('skills.new.generate.reviseLabel')} help={t('skills.new.generate.reviseHelp')}>
          <Textarea
            value={instruction}
            onChange={(e) => onInstruction(e.target.value)}
            placeholder={t('skills.new.generate.revisePlaceholder')}
            rows={3}
          />
        </Field>

        <div className="flex items-center gap-2">
          <Button variant="outline" onClick={onRegenerate} disabled={regenerating}>
            {regenerating ? <Loader2 className="animate-spin" /> : <RefreshCw />}
            {regenerating ? t('skills.new.generate.regenerating') : t('skills.new.generate.regenerate')}
          </Button>
          {!canNext && !draftConfirmed && (
            <Button variant="ghost" size="sm" onClick={onConfirmDraft}>
              {t('skills.new.generate.confirmDone')}
            </Button>
          )}
        </div>

        <div className="flex items-center justify-between border-t border-surface-border pt-4">
          <Button variant="ghost" onClick={onBack}>
            <ArrowLeft />
            {t('common.back')}
          </Button>
          <Button variant="brand" onClick={onNext} disabled={!canNext}>
            {t('skills.new.generate.toForm')}
            <ArrowRight />
          </Button>
        </div>
      </CardContent>
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
    <Card>
      <CardContent className="space-y-5">
        <Field label={t('skills.new.form.displayName')} required>
          <Input
            value={displayName}
            onChange={(e) => onDisplayName(e.target.value)}
            placeholder={t('skills.new.form.displayNamePlaceholder')}
          />
        </Field>

        <Field label={t('skills.new.form.description')}>
          <Textarea value={descriptionHuman} onChange={(e) => onDescriptionHuman(e.target.value)} rows={4} />
        </Field>

        <Field label={t('skills.new.form.timeSaved')} help={t('skills.new.form.timeSavedHelp')}>
          <div className="flex gap-2">
            <Input
              type="number"
              min={0}
              className="w-28"
              value={timeSavedValue}
              onChange={(e) => onTimeSavedValue(e.target.value)}
            />
            <Select value={timeSavedUnit} onValueChange={(v) => onTimeSavedUnit(String(v) as TimeSavedUnit)}>
              <SelectTrigger className="w-48">
                <SelectValue>{t(`skills.custom.unit.${timeSavedUnit}`)}</SelectValue>
              </SelectTrigger>
              <SelectContent>
                {TIME_UNITS.map((u) => (
                  <SelectItem key={u} value={u}>
                    {t(`skills.custom.unit.${u}`)}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
        </Field>

        <Field label={t('skills.new.form.tags')}>
          <ChipEditor values={tags} onChange={onTags} placeholder={t('skills.new.form.tagsPlaceholder')} />
        </Field>

        <div className="flex items-center justify-between border-t border-surface-border pt-4">
          <Button variant="ghost" onClick={onBack}>
            <ArrowLeft />
            {t('common.back')}
          </Button>
          <Button variant="brand" onClick={onNext} disabled={saving || !displayName.trim()}>
            {saving ? <Loader2 className="animate-spin" /> : null}
            {saving ? t('common.saving') : t('skills.new.form.toReview')}
            {!saving && <ArrowRight />}
          </Button>
        </div>
      </CardContent>
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
      <Card>
        <CardContent className="space-y-4">
          <div className="flex flex-col items-center gap-3 py-3 text-center">
            <DuDu face="proud" size="md" />
            <p className="text-sm font-medium text-foreground">{t('skills.new.submitted.title')}</p>
            <p className="max-w-md text-xs text-muted-foreground">{t('skills.new.submitted.awaiting')}</p>
          </div>
          <div className="space-y-2 rounded-lg bg-muted/60 p-3 text-xs">
            <div className="flex items-center justify-between gap-2">
              <span className="text-muted-foreground">{t('skills.new.submitted.approvalId')}</span>
              <span className="truncate font-mono">{result.approval_id}</span>
            </div>
            <div className="flex items-center justify-between gap-2">
              <span className="text-muted-foreground">{t('skills.new.submitted.risk')}</span>
              <Badge
                variant="secondary"
                className={sr.passed ? 'bg-success/15 text-success' : 'bg-warning/15 text-warning'}
              >
                {sr.risk_level}
              </Badge>
            </div>
            <div className="flex items-center justify-between gap-2">
              <span className="text-muted-foreground">{t('skills.new.submitted.findings')}</span>
              <span className="font-mono tabular-nums text-foreground">{sr.findings.length}</span>
            </div>
          </div>
          <div className="flex justify-end">
            <Button variant="brand" onClick={onView}>
              {t('skills.new.submitted.viewDetail')}
            </Button>
          </div>
        </CardContent>
      </Card>
    );
  }

  return (
    <Card>
      <CardContent className="space-y-5">
        <div className="flex items-start gap-3">
          <ShieldCheck className="mt-0.5 size-5 shrink-0 text-brand" />
          <div className="space-y-1">
            <p className="text-sm font-medium text-foreground">{t('skills.new.review.title')}</p>
            <p className="text-xs text-muted-foreground">{t('skills.new.review.explain')}</p>
          </div>
        </div>

        <div className="space-y-1.5 rounded-lg bg-muted/60 p-3 text-xs">
          <div className="flex justify-between gap-2">
            <span className="text-muted-foreground">{t('skills.new.form.displayName')}</span>
            <span className="truncate font-medium text-foreground">{record.display_name}</span>
          </div>
          <div className="flex justify-between gap-2">
            <span className="text-muted-foreground">{t('skills.custom.slug')}</span>
            <span className="truncate font-mono text-foreground">{record.slug}</span>
          </div>
          <div className="flex justify-between gap-2">
            <span className="text-muted-foreground">{t('skills.new.form.timeSaved')}</span>
            <span className="text-foreground">
              {formatTimeSaved(intl, record.time_saved_value, record.time_saved_unit)}
            </span>
          </div>
        </div>

        {/* Fail-closed error frame: high/critical risk block or missing draft */}
        {error && (
          <div className="flex items-start gap-2 rounded-lg bg-destructive/10 p-3 text-sm text-destructive">
            <ShieldAlert className="mt-0.5 size-4 shrink-0" />
            <div className="min-w-0 space-y-1">
              <p className="font-medium">{t('skills.new.review.blocked')}</p>
              <p className="break-words text-xs">{error}</p>
            </div>
          </div>
        )}

        <div className="flex items-center justify-between border-t border-surface-border pt-4">
          <Button variant="ghost" onClick={onBack}>
            <ArrowLeft />
            {t('common.back')}
          </Button>
          <Button variant="brand" onClick={onSubmit} disabled={submitting}>
            {submitting ? <Loader2 className="animate-spin" /> : <Send />}
            {submitting ? t('skills.new.review.submitting') : t('skills.new.review.submit')}
          </Button>
        </div>
      </CardContent>
    </Card>
  );
}
