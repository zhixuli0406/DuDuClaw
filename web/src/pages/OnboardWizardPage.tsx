import { useState, useCallback, useRef, type ChangeEvent, type DragEvent } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate } from 'react-router';
import { cn } from '@/lib/utils';
import { api } from '@/lib/api';
import { inputClass, buttonPrimary, buttonSecondary } from '@/components/shared/Dialog';
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

// ---------------------------------------------------------------------------
// Industry cards config
// ---------------------------------------------------------------------------

const INDUSTRIES: ReadonlyArray<{
  readonly id: Industry;
  readonly icon: typeof UtensilsCrossed;
  readonly emoji: string;
}> = [
  { id: 'restaurant', icon: UtensilsCrossed, emoji: '\uD83C\uDF7D\uFE0F' },
  { id: 'manufacturing', icon: Factory, emoji: '\uD83C\uDFED' },
  { id: 'trading', icon: Package, emoji: '\uD83D\uDCE6' },
  { id: 'retail', icon: ShoppingBag, emoji: '\uD83D\uDECD\uFE0F' },
  { id: 'other', icon: Settings, emoji: '\u2699\uFE0F' },
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
    .replace(/[^a-z0-9\u4e00-\u9fff]+/g, '-')
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
// Step Indicator
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
                  'flex h-9 w-9 items-center justify-center rounded-full text-sm font-semibold transition-all duration-200',
                  isActive && 'bg-amber-500 text-white shadow-md shadow-amber-500/30',
                  isDone && 'bg-amber-100 text-amber-700 dark:bg-amber-900/40 dark:text-amber-400',
                  !isActive && !isDone && 'bg-stone-100 text-stone-400 dark:bg-stone-800 dark:text-stone-500',
                )}
              >
                {isDone ? <Check className="h-4 w-4" /> : step}
              </div>
              <span
                className={cn(
                  'mt-1.5 hidden text-xs font-medium sm:block',
                  isActive && 'text-amber-600 dark:text-amber-400',
                  isDone && 'text-stone-500 dark:text-stone-400',
                  !isActive && !isDone && 'text-stone-400 dark:text-stone-500',
                )}
              >
                {intl.formatMessage({ id: stepKeys[i] })}
              </span>
            </div>
            {step < total && (
              <div
                className={cn(
                  'mx-2 mt-[-1.25rem] h-0.5 w-8 sm:w-12 transition-colors duration-200',
                  isDone ? 'bg-amber-400 dark:bg-amber-500' : 'bg-stone-200 dark:bg-stone-700',
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
      {INDUSTRIES.map(({ id, emoji }) => (
        <button
          key={id}
          type="button"
          onClick={() => onSelect(id)}
          className={cn(
            'flex flex-col items-center gap-3 rounded-xl border-2 p-6 transition-all duration-200',
            selected === id
              ? 'border-amber-500 bg-amber-50 shadow-md shadow-amber-500/10 dark:border-amber-400 dark:bg-amber-900/20'
              : 'border-stone-200 bg-white hover:border-stone-300 hover:shadow-sm dark:border-stone-700 dark:bg-stone-800 dark:hover:border-stone-600',
          )}
        >
          <span className="text-4xl" role="img" aria-label={id}>
            {emoji}
          </span>
          <div className="text-center">
            <p className="text-sm font-semibold text-stone-900 dark:text-stone-50">
              {intl.formatMessage({ id: `wizard.industry.${id}` })}
            </p>
            <p className="mt-1 text-xs text-stone-500 dark:text-stone-400">
              {intl.formatMessage({ id: `wizard.industry.${id}.desc` })}
            </p>
          </div>
        </button>
      ))}
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
    <div className="mx-auto max-w-lg space-y-5">
      <div className="space-y-1.5">
        <label className="block text-sm font-medium text-stone-700 dark:text-stone-300">
          {intl.formatMessage({ id: 'wizard.companyName' })} *
        </label>
        <input
          type="text"
          value={state.companyName}
          onChange={handleCompanyChange}
          className={inputClass}
          placeholder={intl.formatMessage({ id: 'wizard.companyName' })}
        />
      </div>

      <div className="space-y-1.5">
        <label className="block text-sm font-medium text-stone-700 dark:text-stone-300">
          {intl.formatMessage({ id: 'wizard.contactName' })} *
        </label>
        <input
          type="text"
          value={state.contactName}
          onChange={(e) => onChange({ contactName: e.target.value })}
          className={inputClass}
          placeholder={intl.formatMessage({ id: 'wizard.contactName' })}
        />
      </div>

      <div className="space-y-1.5">
        <label className="block text-sm font-medium text-stone-700 dark:text-stone-300">
          {intl.formatMessage({ id: 'wizard.primaryChannel' })} *
        </label>
        <select
          value={state.primaryChannel}
          onChange={(e) => onChange({ primaryChannel: e.target.value as Channel })}
          className={inputClass}
        >
          <option value="">{intl.formatMessage({ id: 'wizard.primaryChannel.placeholder' })}</option>
          {channels.map((ch) => (
            <option key={ch} value={ch}>
              {ch.charAt(0).toUpperCase() + ch.slice(1)}
            </option>
          ))}
        </select>
      </div>

      <div className="space-y-1.5">
        <label className="block text-sm font-medium text-stone-700 dark:text-stone-300">
          {intl.formatMessage({ id: 'wizard.agentName' })}
        </label>
        <input
          type="text"
          value={state.agentName}
          onChange={(e) => onChange({ agentName: e.target.value })}
          className={inputClass}
          placeholder="my-company-agent"
        />
      </div>
    </div>
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
            className={cn(
              'flex items-start gap-4 rounded-xl border-2 p-5 text-left transition-all duration-200',
              isSelected
                ? 'border-amber-500 bg-amber-50 dark:border-amber-400 dark:bg-amber-900/20'
                : 'border-stone-200 bg-white hover:border-stone-300 dark:border-stone-700 dark:bg-stone-800 dark:hover:border-stone-600',
            )}
          >
            <div
              className={cn(
                'flex h-10 w-10 shrink-0 items-center justify-center rounded-lg',
                isSelected
                  ? 'bg-amber-500 text-white'
                  : 'bg-stone-100 text-stone-500 dark:bg-stone-700 dark:text-stone-400',
              )}
            >
              <Icon className="h-5 w-5" />
            </div>
            <div className="min-w-0 flex-1">
              <p className="text-sm font-semibold text-stone-900 dark:text-stone-50">
                {intl.formatMessage({ id: `wizard.feature.${id}` })}
              </p>
              <p className="mt-0.5 text-xs text-stone-500 dark:text-stone-400">
                {intl.formatMessage({ id: `wizard.feature.${id}.desc` })}
              </p>
            </div>
            <div
              className={cn(
                'mt-0.5 flex h-5 w-5 shrink-0 items-center justify-center rounded border-2 transition-colors',
                isSelected
                  ? 'border-amber-500 bg-amber-500 dark:border-amber-400 dark:bg-amber-400'
                  : 'border-stone-300 dark:border-stone-600',
              )}
            >
              {isSelected && <Check className="h-3.5 w-3.5 text-white" />}
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
            'flex cursor-pointer flex-col items-center justify-center rounded-xl border-2 border-dashed py-16 transition-all duration-200',
            isDragging
              ? 'border-amber-500 bg-amber-50 dark:bg-amber-900/20'
              : 'border-stone-300 bg-white hover:border-stone-400 dark:border-stone-600 dark:bg-stone-800 dark:hover:border-stone-500',
          )}
        >
          <Upload className="mb-3 h-10 w-10 text-stone-400 dark:text-stone-500" />
          <p className="text-sm font-medium text-stone-600 dark:text-stone-300">
            {intl.formatMessage({ id: 'wizard.import.dropzone' })}
          </p>
          <p className="mt-1 text-xs text-stone-400 dark:text-stone-500">
            {intl.formatMessage({ id: 'wizard.import.formats' })}
          </p>
        </div>
      ) : (
        <div className="space-y-4">
          <div className="flex items-center gap-3 rounded-xl border border-stone-200 bg-white px-4 py-3 dark:border-stone-700 dark:bg-stone-800">
            <FileUp className="h-5 w-5 text-amber-500" />
            <span className="flex-1 truncate text-sm font-medium text-stone-900 dark:text-stone-50">
              {state.importFile.name}
            </span>
            <button
              type="button"
              onClick={clearFile}
              className="rounded-lg p-1 text-stone-400 hover:bg-stone-100 hover:text-stone-600 dark:hover:bg-stone-700 dark:hover:text-stone-300"
            >
              <X className="h-4 w-4" />
            </button>
          </div>

          {state.importPreview.length > 0 && (
            <div className="overflow-x-auto rounded-xl border border-stone-200 dark:border-stone-700">
              <table className="w-full text-left text-xs">
                <thead>
                  <tr className="border-b border-stone-200 bg-stone-50 dark:border-stone-700 dark:bg-stone-800">
                    {state.importPreview[0].map((header, i) => (
                      <th key={i} className="px-3 py-2 font-semibold text-stone-600 dark:text-stone-300">
                        {header}
                      </th>
                    ))}
                  </tr>
                </thead>
                <tbody>
                  {state.importPreview.slice(1).map((row, ri) => (
                    <tr key={ri} className="border-b border-stone-100 last:border-0 dark:border-stone-800">
                      {row.map((cell, ci) => (
                        <td key={ci} className="px-3 py-2 text-stone-700 dark:text-stone-300">
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

      {fileSizeError && (
        <p className="text-sm text-rose-600 dark:text-rose-400">{fileSizeError}</p>
      )}

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
    <div className="mx-auto max-w-lg space-y-4">
      <div className="rounded-xl border border-stone-200 bg-white p-6 dark:border-stone-700 dark:bg-stone-800">
        <h3 className="mb-4 text-base font-semibold text-stone-900 dark:text-stone-50">
          {intl.formatMessage({ id: 'wizard.summary' })}
        </h3>
        <dl className="space-y-3 text-sm">
          <div className="flex justify-between">
            <dt className="text-stone-500 dark:text-stone-400">
              {intl.formatMessage({ id: 'wizard.step1.title' })}
            </dt>
            <dd className="font-medium text-stone-900 dark:text-stone-50">
              {state.industry
                ? intl.formatMessage({ id: `wizard.industry.${state.industry}` })
                : '-'}
            </dd>
          </div>
          <div className="flex justify-between">
            <dt className="text-stone-500 dark:text-stone-400">
              {intl.formatMessage({ id: 'wizard.companyName' })}
            </dt>
            <dd className="font-medium text-stone-900 dark:text-stone-50">
              {state.companyName || '-'}
            </dd>
          </div>
          <div className="flex justify-between">
            <dt className="text-stone-500 dark:text-stone-400">
              {intl.formatMessage({ id: 'wizard.contactName' })}
            </dt>
            <dd className="font-medium text-stone-900 dark:text-stone-50">
              {state.contactName || '-'}
            </dd>
          </div>
          <div className="flex justify-between">
            <dt className="text-stone-500 dark:text-stone-400">
              {intl.formatMessage({ id: 'wizard.primaryChannel' })}
            </dt>
            <dd className="font-medium text-stone-900 dark:text-stone-50">
              {state.primaryChannel
                ? state.primaryChannel.charAt(0).toUpperCase() + state.primaryChannel.slice(1)
                : '-'}
            </dd>
          </div>
          <div className="flex justify-between">
            <dt className="text-stone-500 dark:text-stone-400">
              {intl.formatMessage({ id: 'wizard.agentName' })}
            </dt>
            <dd className="font-medium text-stone-900 dark:text-stone-50">
              {state.agentName || '-'}
            </dd>
          </div>
          <div className="flex justify-between">
            <dt className="text-stone-500 dark:text-stone-400">
              {intl.formatMessage({ id: 'wizard.step3.title' })}
            </dt>
            <dd className="font-medium text-stone-900 dark:text-stone-50">
              {state.features.length > 0
                ? state.features
                    .map((f) => intl.formatMessage({ id: `wizard.feature.${f}` }))
                    .join(', ')
                : '-'}
            </dd>
          </div>
          <div className="flex justify-between">
            <dt className="text-stone-500 dark:text-stone-400">
              {intl.formatMessage({ id: 'wizard.step4.title' })}
            </dt>
            <dd className="font-medium text-stone-900 dark:text-stone-50">
              {state.importFile ? state.importFile.name : intl.formatMessage({ id: 'wizard.import.none' })}
            </dd>
          </div>
        </dl>
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Success Screen
// ---------------------------------------------------------------------------

function SuccessScreen({ intl }: { intl: ReturnType<typeof useIntl> }) {
  const navigate = useNavigate();

  return (
    <div className="flex flex-col items-center justify-center py-16">
      <div className="mb-6 flex h-20 w-20 items-center justify-center rounded-full bg-emerald-100 dark:bg-emerald-900/30">
        <Check className="h-10 w-10 text-emerald-600 dark:text-emerald-400 animate-[scale-in_0.3s_ease-out]" />
      </div>
      <h2 className="text-2xl font-semibold text-stone-900 dark:text-stone-50">
        {intl.formatMessage({ id: 'wizard.success' })}
      </h2>
      <p className="mt-2 text-sm text-stone-500 dark:text-stone-400">
        {intl.formatMessage({ id: 'wizard.success.desc' })}
      </p>
      <button
        type="button"
        onClick={() => navigate('/agents')}
        className={cn(buttonPrimary, 'mt-8')}
      >
        {intl.formatMessage({ id: 'wizard.goToAgents' })}
      </button>
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
      <div className="space-y-6">
        <SuccessScreen intl={intl} />
      </div>
    );
  }

  return (
    <div className="space-y-8">
      {/* Header */}
      <div className="text-center">
        <h2 className="text-2xl font-semibold text-stone-900 dark:text-stone-50">
          {intl.formatMessage({ id: 'wizard.title' })}
        </h2>
      </div>

      {/* Step Indicator */}
      <StepIndicator current={step} total={TOTAL_STEPS} intl={intl} />

      {/* Step Title */}
      <div className="text-center">
        <h3 className="text-lg font-medium text-stone-700 dark:text-stone-300">
          {intl.formatMessage({ id: `wizard.step${step}.title` })}
        </h3>
      </div>

      {/* Step Content */}
      <div className="transition-opacity duration-200">
        {step === 1 && (
          <Step1
            selected={state.industry}
            onSelect={(id) => updateState({ industry: id })}
            intl={intl}
          />
        )}
        {step === 2 && <Step2 state={state} onChange={updateState} intl={intl} />}
        {step === 3 && <Step3 selected={state.features} onToggle={toggleFeature} intl={intl} />}
        {step === 4 && <Step4 state={state} onChange={updateState} intl={intl} />}
        {step === 5 && <Step5 state={state} intl={intl} />}
      </div>

      {/* Error */}
      {error && (
        <p className="text-center text-sm text-rose-600 dark:text-rose-400">{error}</p>
      )}

      {/* Navigation Buttons */}
      <div className="flex items-center justify-between pt-2">
        <div>
          {step > 1 && (
            <button
              type="button"
              onClick={() => setStep((s) => s - 1)}
              className={buttonSecondary}
            >
              <ChevronLeft className="h-4 w-4" />
              {intl.formatMessage({ id: 'wizard.back' })}
            </button>
          )}
        </div>

        <div className="flex items-center gap-3">
          {step === 4 && !state.importFile && (
            <button
              type="button"
              onClick={() => setStep(5)}
              className={buttonSecondary}
            >
              {intl.formatMessage({ id: 'wizard.skip' })}
            </button>
          )}

          {step < TOTAL_STEPS ? (
            <button
              type="button"
              disabled={!canAdvance()}
              onClick={() => setStep((s) => s + 1)}
              className={buttonPrimary}
            >
              {intl.formatMessage({ id: 'wizard.next' })}
              <ChevronRight className="h-4 w-4" />
            </button>
          ) : (
            <button
              type="button"
              disabled={deploying}
              onClick={handleDeploy}
              className={cn(buttonPrimary, 'gap-2')}
            >
              <Rocket className="h-4 w-4" />
              {deploying
                ? intl.formatMessage({ id: 'wizard.deploying' })
                : intl.formatMessage({ id: 'wizard.deploy' })}
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
