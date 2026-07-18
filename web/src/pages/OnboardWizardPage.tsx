import { useState, useCallback, useRef, type ChangeEvent, type DragEvent } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate } from 'react-router';
import { cn } from '@/lib/utils';
import { api } from '@/lib/api';
import { Card, Button, Badge, Input } from '@/components/mds';
import { Field, fieldControl, CompletionBadge } from '@/components/onboarding';
import { DuDu } from '@/components/mascot';
import type { DuduFace } from '@/components/mascot/faces';
import {
  ChevronLeft,
  ChevronRight,
  Check,
  Upload,
  Rocket,
  UtensilsCrossed,
  Factory,
  Package,
  ShoppingBag,
  Settings,
  Headphones,
  TrendingUp,
  Users,
  Boxes,
  CalendarClock,
  FileUp,
  X,
} from 'lucide-react';

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

type Industry = 'restaurant' | 'manufacturing' | 'trading' | 'retail' | 'other';
type Channel = 'line' | 'telegram' | 'discord' | 'slack';
type Feature = 'customerService' | 'sales' | 'internalAssistant' | 'inventory' | 'scheduling';

interface WizardState {
  readonly industry: Industry | null;
  readonly companyName: string;
  readonly contactName: string;
  readonly primaryChannel: Channel | '';
  readonly agentName: string;
  readonly features: ReadonlyArray<Feature>;
  readonly importFile: File | null;
  readonly importPreview: ReadonlyArray<ReadonlyArray<string>>;
}

const INITIAL_STATE: WizardState = {
  industry: null,
  companyName: '',
  contactName: '',
  primaryChannel: '',
  agentName: '',
  features: [],
  importFile: null,
  importPreview: [],
};

const TOTAL_STEPS = 5;

// Shared selection-card styling (spec §4 Card + §5.8): a resting surface card
// that highlights with the brand ring when picked.
const SELECT_CARD =
  'rounded-xl border border-surface-border bg-surface shadow-[var(--surface-shadow)] outline-none ' +
  'transition-colors hover:bg-surface-hover ' +
  'focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50';
const SELECT_CARD_ACTIVE = 'border-brand ring-1 ring-brand hover:bg-surface';

/**
 * DuDu's expression for each wizard step (§7.3): curious while the operator is
 * still choosing, writing while they fill in details, proud on the final review.
 */
function faceForStep(step: number): DuduFace {
  if (step >= 5) return 'proud';
  if (step >= 3) return 'writing';
  return 'curious';
}

// ---------------------------------------------------------------------------
// Industry cards config
// ---------------------------------------------------------------------------

const INDUSTRIES: ReadonlyArray<{
  readonly id: Industry;
  readonly icon: typeof UtensilsCrossed;
  readonly emoji: string;
}> = [
  { id: 'restaurant', icon: UtensilsCrossed, emoji: '🍽️' },
  { id: 'manufacturing', icon: Factory, emoji: '🏭' },
  { id: 'trading', icon: Package, emoji: '📦' },
  { id: 'retail', icon: ShoppingBag, emoji: '🛍️' },
  { id: 'other', icon: Settings, emoji: '⚙️' },
] as const;

// ---------------------------------------------------------------------------
// Feature modules config
// ---------------------------------------------------------------------------

const FEATURES: ReadonlyArray<{
  readonly id: Feature;
  readonly icon: typeof Headphones;
}> = [
  { id: 'customerService', icon: Headphones },
  { id: 'sales', icon: TrendingUp },
  { id: 'internalAssistant', icon: Users },
  { id: 'inventory', icon: Boxes },
  { id: 'scheduling', icon: CalendarClock },
] as const;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/** Sanitize user input before inserting into SOUL.md to prevent prompt injection. */
function sanitizeSoulField(input: string): string {
  return input
    .replace(/[\n\r]/g, ' ')        // Remove newlines (primary injection vector)
    .replace(/[`<>{}]/g, '')         // Remove backticks and angle brackets
    .slice(0, 100)                   // Limit length
    .trim();
}

function toAgentName(companyName: string): string {
  return companyName
    .toLowerCase()
    .replace(/[^a-z0-9一-鿿]+/g, '-')
    .replace(/^-+|-+$/g, '')
    .slice(0, 30)
    + '-agent';
}

async function parseCSVPreview(file: File): Promise<ReadonlyArray<ReadonlyArray<string>>> {
  const text = await file.text();
  const lines = text.split('\n').filter((l) => l.trim().length > 0);
  return lines.slice(0, 5).map((line) => line.split(',').map((c) => c.trim()));
}

async function parseJSONPreview(file: File): Promise<ReadonlyArray<ReadonlyArray<string>>> {
  const text = await file.text();
  try {
    const parsed = JSON.parse(text);
    const arr = Array.isArray(parsed) ? parsed : [parsed];
    const rows = arr.slice(0, 5);
    if (rows.length === 0) return [];
    const keys = Object.keys(rows[0]);
    return [keys, ...rows.map((row) => keys.map((k) => String(row[k] ?? '')))];
  } catch {
    return [];
  }
}

async function parseJSONLPreview(file: File): Promise<ReadonlyArray<ReadonlyArray<string>>> {
  const text = await file.text();
  const lines = text.split('\n').filter((l) => l.trim().length > 0);
  const rows = lines.slice(0, 5).map((l) => {
    try {
      return JSON.parse(l);
    } catch {
      return {};
    }
  });
  if (rows.length === 0) return [];
  const keys = Object.keys(rows[0]);
  return [keys, ...rows.map((row: Record<string, unknown>) => keys.map((k) => String(row[k] ?? '')))];
}

// ---------------------------------------------------------------------------
// Step Indicator (spec §5.8 — numbered dots + step-name captions)
// ---------------------------------------------------------------------------

function StepIndicator({ current, total, intl }: { current: number; total: number; intl: ReturnType<typeof useIntl> }) {
  const stepKeys = [
    'wizard.step1.title',
    'wizard.step2.title',
    'wizard.step3.title',
    'wizard.step4.title',
    'wizard.step5.title',
  ] as const;

  return (
    <div className="flex items-center justify-center gap-0">
      {Array.from({ length: total }, (_, i) => {
        const step = i + 1;
        const isActive = step === current;
        const isDone = step < current;
        return (
          <div key={step} className="flex items-center">
            <div className="flex flex-col items-center">
              <div
                className={cn(
                  'flex size-8 items-center justify-center rounded-full text-sm font-medium transition-colors duration-200',
                  isActive && 'bg-brand text-brand-foreground',
                  isDone && 'bg-brand/12 text-brand',
                  !isActive && !isDone && 'border border-surface-border bg-surface text-muted-foreground',
                )}
              >
                {isDone ? <Check className="size-4" /> : step}
              </div>
              <span
                className={cn(
                  'mt-1.5 hidden text-xs font-medium sm:block',
                  isActive ? 'text-foreground' : 'text-muted-foreground',
                )}
              >
                {intl.formatMessage({ id: stepKeys[i] })}
              </span>
            </div>
            {step < total && (
              <div
                className={cn(
                  'mx-2 mt-[-1.25rem] h-0.5 w-8 transition-colors duration-200 sm:w-12',
                  isDone ? 'bg-brand' : 'bg-surface-border',
                )}
              />
            )}
          </div>
        );
      })}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Step 1 — Industry Selection
// ---------------------------------------------------------------------------

function Step1({
  selected,
  onSelect,
  intl,
}: {
  selected: Industry | null;
  onSelect: (id: Industry) => void;
  intl: ReturnType<typeof useIntl>;
}) {
  return (
    <div className="grid grid-cols-2 gap-4 sm:grid-cols-3 lg:grid-cols-5">
      {INDUSTRIES.map(({ id, emoji }) => {
        const isSelected = selected === id;
        return (
          <button
            key={id}
            type="button"
            onClick={() => onSelect(id)}
            aria-pressed={isSelected}
            className={cn(
              SELECT_CARD,
              'flex flex-col items-center gap-3 p-6',
              isSelected && SELECT_CARD_ACTIVE,
            )}
          >
            <span className="text-4xl" role="img" aria-label={id}>
              {emoji}
            </span>
            <div className="text-center">
              <p className="text-sm font-medium text-foreground">
                {intl.formatMessage({ id: `wizard.industry.${id}` })}
              </p>
              <p className="mt-1 text-xs text-muted-foreground">
                {intl.formatMessage({ id: `wizard.industry.${id}.desc` })}
              </p>
            </div>
          </button>
        );
      })}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Step 2 — Basic Info
// ---------------------------------------------------------------------------

function Step2({
  state,
  onChange,
  intl,
}: {
  state: WizardState;
  onChange: (patch: Partial<WizardState>) => void;
  intl: ReturnType<typeof useIntl>;
}) {
  const channels: ReadonlyArray<Channel> = ['line', 'telegram', 'discord', 'slack'];

  const handleCompanyChange = (e: ChangeEvent<HTMLInputElement>) => {
    const companyName = e.target.value;
    const agentName = state.agentName === toAgentName(state.companyName) || state.agentName === ''
      ? toAgentName(companyName)
      : state.agentName;
    onChange({ companyName, agentName });
  };

  return (
    <Card className="mx-auto max-w-lg p-6">
      <Field label={intl.formatMessage({ id: 'wizard.companyName' })} required>
        <Input
          type="text"
          value={state.companyName}
          onChange={handleCompanyChange}
          placeholder={intl.formatMessage({ id: 'wizard.companyName' })}
        />
      </Field>

      <Field label={intl.formatMessage({ id: 'wizard.contactName' })} required>
        <Input
          type="text"
          value={state.contactName}
          onChange={(e) => onChange({ contactName: e.target.value })}
          placeholder={intl.formatMessage({ id: 'wizard.contactName' })}
        />
      </Field>

      <Field label={intl.formatMessage({ id: 'wizard.primaryChannel' })} required>
        <select
          value={state.primaryChannel}
          onChange={(e) => onChange({ primaryChannel: e.target.value as Channel })}
          className={fieldControl}
        >
          <option value="">{intl.formatMessage({ id: 'wizard.primaryChannel.placeholder' })}</option>
          {channels.map((ch) => (
            <option key={ch} value={ch}>
              {ch.charAt(0).toUpperCase() + ch.slice(1)}
            </option>
          ))}
        </select>
      </Field>

      <Field label={intl.formatMessage({ id: 'wizard.agentName' })}>
        <Input
          type="text"
          value={state.agentName}
          onChange={(e) => onChange({ agentName: e.target.value })}
          placeholder="my-company-agent"
        />
      </Field>
    </Card>
  );
}

// ---------------------------------------------------------------------------
// Step 3 — Feature Modules
// ---------------------------------------------------------------------------

function Step3({
  selected,
  onToggle,
  intl,
}: {
  selected: ReadonlyArray<Feature>;
  onToggle: (feature: Feature) => void;
  intl: ReturnType<typeof useIntl>;
}) {
  return (
    <div className="grid grid-cols-1 gap-4 sm:grid-cols-2 lg:grid-cols-3">
      {FEATURES.map(({ id, icon: Icon }) => {
        const isSelected = selected.includes(id);
        return (
          <button
            key={id}
            type="button"
            onClick={() => onToggle(id)}
            aria-pressed={isSelected}
            className={cn(
              SELECT_CARD,
              'flex items-start gap-4 p-5 text-left',
              isSelected && SELECT_CARD_ACTIVE,
            )}
          >
            <div
              className={cn(
                'flex size-10 shrink-0 items-center justify-center rounded-lg',
                isSelected ? 'bg-brand text-brand-foreground' : 'bg-muted text-muted-foreground',
              )}
            >
              <Icon className="size-5" />
            </div>
            <div className="min-w-0 flex-1">
              <p className="text-sm font-medium text-foreground">
                {intl.formatMessage({ id: `wizard.feature.${id}` })}
              </p>
              <p className="mt-0.5 text-xs text-muted-foreground">
                {intl.formatMessage({ id: `wizard.feature.${id}.desc` })}
              </p>
            </div>
            <div
              className={cn(
                'mt-0.5 flex size-5 shrink-0 items-center justify-center rounded border-2 transition-colors',
                isSelected ? 'border-brand bg-brand' : 'border-input',
              )}
            >
              {isSelected && <Check className="size-3.5 text-brand-foreground" />}
            </div>
          </button>
        );
      })}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Step 4 — Data Import
// ---------------------------------------------------------------------------

function Step4({
  state,
  onChange,
  intl,
}: {
  state: WizardState;
  onChange: (patch: Partial<WizardState>) => void;
  intl: ReturnType<typeof useIntl>;
}) {
  const fileRef = useRef<HTMLInputElement>(null);
  const [isDragging, setIsDragging] = useState(false);

  const MAX_FILE_SIZE = 5 * 1024 * 1024; // 5 MB
  const [fileSizeError, setFileSizeError] = useState('');

  const handleFile = useCallback(
    async (file: File | undefined) => {
      if (!file) return;
      setFileSizeError('');
      if (file.size > MAX_FILE_SIZE) {
        setFileSizeError(intl.formatMessage({ id: 'wizard.import.tooLarge' }));
        return;
      }
      const ext = file.name.split('.').pop()?.toLowerCase();
      let preview: ReadonlyArray<ReadonlyArray<string>> = [];
      if (ext === 'csv') {
        preview = await parseCSVPreview(file);
      } else if (ext === 'json') {
        preview = await parseJSONPreview(file);
      } else if (ext === 'jsonl') {
        preview = await parseJSONLPreview(file);
      }
      onChange({ importFile: file, importPreview: preview });
    },
    [onChange, intl],
  );

  const handleDrop = useCallback(
    (e: DragEvent) => {
      e.preventDefault();
      setIsDragging(false);
      const file = e.dataTransfer.files[0];
      handleFile(file);
    },
    [handleFile],
  );

  const handleDragOver = useCallback((e: DragEvent) => {
    e.preventDefault();
    setIsDragging(true);
  }, []);

  const handleDragLeave = useCallback(() => {
    setIsDragging(false);
  }, []);

  const clearFile = useCallback(() => {
    onChange({ importFile: null, importPreview: [] });
    if (fileRef.current) fileRef.current.value = '';
  }, [onChange]);

  return (
    <div className="mx-auto max-w-2xl space-y-6">
      {!state.importFile ? (
        <div
          role="button"
          tabIndex={0}
          onDrop={handleDrop}
          onDragOver={handleDragOver}
          onDragLeave={handleDragLeave}
          onClick={() => fileRef.current?.click()}
          onKeyDown={(e) => {
            if (e.key === 'Enter' || e.key === ' ') fileRef.current?.click();
          }}
          className={cn(
            'flex cursor-pointer flex-col items-center justify-center rounded-xl border-2 border-dashed py-16 outline-none transition-colors duration-200',
            'focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50',
            isDragging
              ? 'border-brand bg-brand/10'
              : 'border-input bg-surface hover:border-brand/50',
          )}
        >
          <Upload className="mb-3 size-10 text-muted-foreground" />
          <p className="text-sm font-medium text-foreground">
            {intl.formatMessage({ id: 'wizard.import.dropzone' })}
          </p>
          <p className="mt-1 text-xs text-muted-foreground">
            {intl.formatMessage({ id: 'wizard.import.formats' })}
          </p>
        </div>
      ) : (
        <div className="space-y-4">
          <div className="flex items-center gap-3 rounded-xl border border-surface-border bg-surface px-4 py-3 shadow-[var(--surface-shadow)]">
            <FileUp className="size-5 text-brand" />
            <span className="flex-1 truncate text-sm font-medium text-foreground">
              {state.importFile.name}
            </span>
            <Button
              variant="ghost"
              size="icon-sm"
              onClick={clearFile}
              aria-label={intl.formatMessage({ id: 'wizard.import.none' })}
            >
              <X />
            </Button>
          </div>

          {state.importPreview.length > 0 && (
            <div className="overflow-x-auto rounded-xl border border-surface-border bg-surface shadow-[var(--surface-shadow)]">
              <table className="w-full text-left text-xs">
                <thead>
                  <tr className="border-b border-surface-border bg-surface-hover">
                    {state.importPreview[0].map((header, i) => (
                      <th key={i} className="px-3 py-2 font-medium text-muted-foreground">
                        {header}
                      </th>
                    ))}
                  </tr>
                </thead>
                <tbody>
                  {state.importPreview.slice(1).map((row, ri) => (
                    <tr key={ri} className="border-b border-surface-border last:border-0">
                      {row.map((cell, ci) => (
                        <td key={ci} className="px-3 py-2 text-foreground">
                          {cell}
                        </td>
                      ))}
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </div>
      )}

      {fileSizeError && <p className="text-sm text-destructive">{fileSizeError}</p>}

      <input
        ref={fileRef}
        type="file"
        accept=".csv,.json,.jsonl"
        className="hidden"
        onChange={(e) => handleFile(e.target.files?.[0])}
      />
    </div>
  );
}

// ---------------------------------------------------------------------------
// Step 5 — Confirm & Deploy
// ---------------------------------------------------------------------------

function Step5({
  state,
  intl,
}: {
  state: WizardState;
  intl: ReturnType<typeof useIntl>;
}) {
  return (
    <div className="mx-auto max-w-lg">
      <Card className="p-4">
        <h3 className="text-base font-medium leading-snug text-foreground">
          {intl.formatMessage({ id: 'wizard.summary' })}
        </h3>
        <dl className="space-y-3 text-sm">
          <div className="flex justify-between">
            <dt className="text-muted-foreground">
              {intl.formatMessage({ id: 'wizard.step1.title' })}
            </dt>
            <dd className="font-medium text-foreground">
              {state.industry ? intl.formatMessage({ id: `wizard.industry.${state.industry}` }) : '-'}
            </dd>
          </div>
          <div className="flex justify-between">
            <dt className="text-muted-foreground">{intl.formatMessage({ id: 'wizard.companyName' })}</dt>
            <dd className="font-medium text-foreground">{state.companyName || '-'}</dd>
          </div>
          <div className="flex justify-between">
            <dt className="text-muted-foreground">{intl.formatMessage({ id: 'wizard.contactName' })}</dt>
            <dd className="font-medium text-foreground">{state.contactName || '-'}</dd>
          </div>
          <div className="flex justify-between">
            <dt className="text-muted-foreground">{intl.formatMessage({ id: 'wizard.primaryChannel' })}</dt>
            <dd className="font-medium text-foreground">
              {state.primaryChannel
                ? state.primaryChannel.charAt(0).toUpperCase() + state.primaryChannel.slice(1)
                : '-'}
            </dd>
          </div>
          <div className="flex justify-between">
            <dt className="text-muted-foreground">{intl.formatMessage({ id: 'wizard.agentName' })}</dt>
            <dd className="font-medium text-foreground">{state.agentName || '-'}</dd>
          </div>
          <div className="flex items-start justify-between gap-3">
            <dt className="text-muted-foreground">{intl.formatMessage({ id: 'wizard.step3.title' })}</dt>
            <dd className="flex flex-wrap justify-end gap-1.5">
              {state.features.length > 0 ? (
                state.features.map((f) => (
                  <Badge key={f} variant="outline" className="border-brand/25 bg-brand/10 text-brand">
                    {intl.formatMessage({ id: `wizard.feature.${f}` })}
                  </Badge>
                ))
              ) : (
                <span className="font-medium text-foreground">-</span>
              )}
            </dd>
          </div>
          <div className="flex justify-between">
            <dt className="text-muted-foreground">{intl.formatMessage({ id: 'wizard.step4.title' })}</dt>
            <dd className="font-medium text-foreground">
              {state.importFile ? state.importFile.name : intl.formatMessage({ id: 'wizard.import.none' })}
            </dd>
          </div>
        </dl>
      </Card>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Success Screen
// ---------------------------------------------------------------------------

function SuccessScreen({ intl }: { intl: ReturnType<typeof useIntl> }) {
  const navigate = useNavigate();

  return (
    <div className="flex flex-col items-center justify-center gap-4 py-16 text-center">
      {/* Completion badge springs in + draws its check (spec §5.8). */}
      <CompletionBadge size={80} label={intl.formatMessage({ id: 'wizard.success' })} />
      <h2 className="text-xl font-semibold text-foreground sm:text-2xl">
        {intl.formatMessage({ id: 'wizard.success' })}
      </h2>
      <p className="text-sm text-muted-foreground">
        {intl.formatMessage({ id: 'wizard.success.desc' })}
      </p>
      <Button variant="brand" size="lg" onClick={() => navigate('/agents')} className="mt-4">
        {intl.formatMessage({ id: 'wizard.goToAgents' })}
      </Button>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Main Wizard Page
// ---------------------------------------------------------------------------

export function OnboardWizardPage() {
  const intl = useIntl();
  const [step, setStep] = useState(1);
  const [state, setState] = useState<WizardState>(INITIAL_STATE);
  const [deploying, setDeploying] = useState(false);
  const [deployed, setDeployed] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const updateState = useCallback((patch: Partial<WizardState>) => {
    setState((prev) => ({ ...prev, ...patch }));
  }, []);

  const toggleFeature = useCallback((feature: Feature) => {
    setState((prev) => {
      const features = prev.features.includes(feature)
        ? prev.features.filter((f) => f !== feature)
        : [...prev.features, feature];
      return { ...prev, features };
    });
  }, []);

  // Validation per step
  const canAdvance = (): boolean => {
    switch (step) {
      case 1:
        return state.industry !== null;
      case 2:
        return state.companyName.trim() !== '' && state.contactName.trim() !== '' && state.primaryChannel !== '';
      case 3:
        return true; // features are optional
      case 4:
        return true; // import is optional
      case 5:
        return true;
      default:
        return false;
    }
  };

  const handleDeploy = useCallback(async () => {
    setDeploying(true);
    setError(null);
    try {
      const agentName = state.agentName || toAgentName(state.companyName);
      await api.agents.create({
        name: agentName,
        display_name: state.companyName + ' Agent',
        role: 'main',
        trigger: state.industry ?? 'general',
        soul: [
          `Industry: ${state.industry}`,
          `Company: ${sanitizeSoulField(state.companyName)}`,
          `Contact: ${sanitizeSoulField(state.contactName)}`,
          `Channel: ${state.primaryChannel}`,
          `Features: ${state.features.join(', ')}`,
        ].join('\n'),
      });
      setDeployed(true);
    } catch {
      setError(intl.formatMessage({ id: 'wizard.deploy.error' }));
    } finally {
      setDeploying(false);
    }
  }, [state, intl]);

  if (deployed) {
    return (
      <div className="min-h-screen bg-app-shell p-6">
        <SuccessScreen intl={intl} />
      </div>
    );
  }

  return (
    <div className="min-h-screen bg-app-shell p-6">
      <div className="mx-auto max-w-4xl space-y-8">
        {/* Header — DuDu is the small side illustration (§5.8 / §7.3). */}
        <div className="flex flex-col items-center gap-2 text-center">
          <DuDu face={faceForStep(step)} size="sm" />
          <h1 className="text-xl font-semibold text-foreground sm:text-2xl">
            {intl.formatMessage({ id: 'wizard.title' })}
          </h1>
        </div>

        {/* Step Indicator */}
        <StepIndicator current={step} total={TOTAL_STEPS} intl={intl} />

        {/* Step Title */}
        <div className="text-center">
          <h2 className="text-base font-medium text-foreground">
            {intl.formatMessage({ id: `wizard.step${step}.title` })}
          </h2>
        </div>

        {/* Step Content — keyed by step for the pure-opacity cross-fade (§5.8). */}
        <div key={step} className="mds-step-fade">
          {step === 1 && (
            <Step1 selected={state.industry} onSelect={(id) => updateState({ industry: id })} intl={intl} />
          )}
          {step === 2 && <Step2 state={state} onChange={updateState} intl={intl} />}
          {step === 3 && <Step3 selected={state.features} onToggle={toggleFeature} intl={intl} />}
          {step === 4 && <Step4 state={state} onChange={updateState} intl={intl} />}
          {step === 5 && <Step5 state={state} intl={intl} />}
        </div>

        {/* Error */}
        {error && <p className="text-center text-sm text-destructive">{error}</p>}

        {/* Navigation Buttons */}
        <div className="flex items-center justify-between pt-2">
          <div>
            {step > 1 && (
              <Button variant="outline" onClick={() => setStep((s) => s - 1)}>
                <ChevronLeft />
                {intl.formatMessage({ id: 'wizard.back' })}
              </Button>
            )}
          </div>

          <div className="flex items-center gap-3">
            {step === 4 && !state.importFile && (
              <Button variant="ghost" onClick={() => setStep(5)}>
                {intl.formatMessage({ id: 'wizard.skip' })}
              </Button>
            )}

            {step < TOTAL_STEPS ? (
              <Button variant="brand" disabled={!canAdvance()} onClick={() => setStep((s) => s + 1)}>
                {intl.formatMessage({ id: 'wizard.next' })}
                <ChevronRight />
              </Button>
            ) : (
              <Button variant="brand" disabled={deploying} onClick={handleDeploy}>
                <Rocket />
                {deploying
                  ? intl.formatMessage({ id: 'wizard.deploying' })
                  : intl.formatMessage({ id: 'wizard.deploy' })}
              </Button>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
