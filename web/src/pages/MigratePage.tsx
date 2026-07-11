import { useState, type ComponentType } from 'react';
import { useIntl } from 'react-intl';
import {
  Import,
  ArrowDownToLine,
  ArrowLeft,
  ArrowRight,
  Loader2,
  Check,
  Copy,
  AlertTriangle,
  Terminal,
  Users,
  Radio,
  Puzzle,
  CalendarClock,
  Cpu,
  Brain,
  BookOpen,
  KanbanSquare,
  KeyRound,
  FileText,
  Package,
} from 'lucide-react';
import {
  Page,
  PageHeader,
  Card,
  Button,
  Badge,
  Field,
  StatCard,
  Mono,
  controlClass,
} from '@/components/ui';
import { ConfirmDialog } from '@/components/settings/controls/ConfirmDialog';
import { cn } from '@/lib/utils';
import { toast, formatError } from '@/lib/toast';
import { api, type MigratePlatform, type MigrateResult, type MigrateItem } from '@/lib/api';
import {
  MIGRATE_PLATFORMS,
  PAPERCLIP_EXPORT_CMD,
  migratePlatformCard,
  statusChipTone,
  verdictToneClass,
  statusLabelKey,
  verdictLabelKey,
  canScan,
} from '@/lib/migrate';

type WizardStep = 'platform' | 'preview' | 'result';

/** lucide icon per item kind, with a neutral fallback. */
const KIND_ICONS: Record<string, ComponentType<{ className?: string }>> = {
  agent: Users,
  agents: Users,
  channel: Radio,
  channel_token: Radio,
  skill: Puzzle,
  skills: Puzzle,
  cron: CalendarClock,
  task: KanbanSquare,
  tasks: KanbanSquare,
  model: Cpu,
  memory: Brain,
  persona: FileText,
  soul: FileText,
  wiki: BookOpen,
  company: BookOpen,
  api_key: KeyRound,
  session: FileText,
};

function kindIcon(kind: string): ComponentType<{ className?: string }> {
  return KIND_ICONS[kind] ?? Package;
}

/** A small on/off switch (mirrors the AgentsPage / InferencePage toggle). */
function Toggle({
  checked,
  onChange,
  labelId,
}: {
  checked: boolean;
  onChange: (v: boolean) => void;
  labelId: string;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={labelId}
      onClick={() => onChange(!checked)}
      className={cn(
        'relative inline-flex h-5 w-9 shrink-0 cursor-pointer rounded-full transition-colors',
        'focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-500/50',
        checked ? 'bg-amber-500' : 'bg-stone-300 dark:bg-stone-600',
      )}
    >
      <span
        className={cn(
          'pointer-events-none mt-0.5 inline-block h-4 w-4 rounded-full bg-white shadow-sm transition-transform',
          checked ? 'ml-0.5 translate-x-4' : 'translate-x-0.5',
        )}
      />
    </button>
  );
}

/** 1-2-3 step indicator. */
function Stepper({ step }: { step: WizardStep }) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const order: WizardStep[] = ['platform', 'preview', 'result'];
  const idx = order.indexOf(step);
  const labels: Record<WizardStep, string> = {
    platform: t('migrate.step.platform'),
    preview: t('migrate.step.preview'),
    result: t('migrate.step.result'),
  };
  return (
    <ol className="flex flex-wrap items-center gap-2 text-sm">
      {order.map((s, i) => {
        const done = i < idx;
        const active = i === idx;
        return (
          <li key={s} className="flex items-center gap-2">
            <span
              className={cn(
                'grid h-6 w-6 place-items-center rounded-full text-xs font-semibold tabular-nums',
                active && 'bg-amber-500 text-white',
                done && 'bg-emerald-500/15 text-emerald-600 dark:text-emerald-400',
                !active && !done && 'bg-stone-500/10 text-stone-400 dark:text-stone-500',
              )}
            >
              {done ? <Check className="h-3.5 w-3.5" /> : i + 1}
            </span>
            <span
              className={cn(
                'font-medium',
                active ? 'text-stone-900 dark:text-stone-100' : 'text-stone-400 dark:text-stone-500',
              )}
            >
              {labels[s]}
            </span>
            {i < order.length - 1 && <span className="mx-1 text-stone-300 dark:text-stone-600">/</span>}
          </li>
        );
      })}
    </ol>
  );
}

/** Copyable command block for the paperclip export step. */
function CommandBlock({ command }: { command: string }) {
  const intl = useIntl();
  const [copied, setCopied] = useState(false);
  const copy = async () => {
    try {
      await navigator.clipboard?.writeText(command);
      setCopied(true);
      setTimeout(() => setCopied(false), 1800);
    } catch {
      toast.error(intl.formatMessage({ id: 'migrate.copyFailed' }));
    }
  };
  return (
    <div className="flex items-start gap-2 rounded-control border border-[var(--panel-border)] bg-stone-500/5 p-2.5 dark:bg-white/5">
      <Terminal className="mt-0.5 h-4 w-4 shrink-0 text-stone-400" />
      <code className="min-w-0 flex-1 overflow-x-auto whitespace-pre text-xs text-stone-600 dark:text-stone-300">
        {command}
      </code>
      <button
        type="button"
        onClick={copy}
        aria-label={intl.formatMessage({ id: copied ? 'migrate.copied' : 'migrate.copy' })}
        className="shrink-0 rounded-md p-1 text-stone-400 transition-colors hover:bg-stone-500/10 hover:text-stone-700 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-500/40 dark:hover:text-stone-200"
      >
        {copied ? <Check className="h-4 w-4 text-emerald-500" /> : <Copy className="h-4 w-4" />}
      </button>
    </div>
  );
}

/** Summary tiles + item table shared by the preview and result steps. */
function ResultReport({ result }: { result: MigrateResult }) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  return (
    <div className="space-y-5">
      <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
        <StatCard label={t('migrate.summary.imported')} value={result.summary.imported} tone="success" />
        <StatCard label={t('migrate.summary.partial')} value={result.summary.partial} tone="warning" />
        <StatCard label={t('migrate.summary.skipped')} value={result.summary.skipped} tone="neutral" />
        <StatCard label={t('migrate.summary.conflict')} value={result.summary.conflict} tone="danger" />
      </div>

      <Card title={t('migrate.items.title')} padded={false}>
        {result.items.length === 0 ? (
          <p className="px-5 py-8 text-center text-sm text-stone-500 dark:text-stone-400">
            {t('migrate.items.empty')}
          </p>
        ) : (
          <div className="overflow-x-auto">
            <table className="w-full min-w-[36rem] text-sm">
              <thead>
                <tr className="border-b border-[var(--panel-border)] text-left text-xs text-stone-400 dark:text-stone-500">
                  <th className="px-5 py-2 font-medium">{t('migrate.col.kind')}</th>
                  <th className="px-3 py-2 font-medium">{t('migrate.col.name')}</th>
                  <th className="px-3 py-2 font-medium">{t('migrate.col.status')}</th>
                  <th className="px-5 py-2 font-medium">{t('migrate.col.reason')}</th>
                </tr>
              </thead>
              <tbody>
                {result.items.map((item: MigrateItem, i) => {
                  const Icon = kindIcon(item.kind);
                  return (
                    <tr
                      key={`${item.kind}-${item.name}-${i}`}
                      className="border-b border-[var(--panel-border)] last:border-0"
                    >
                      <td className="px-5 py-2.5">
                        <span className="inline-flex items-center gap-2 text-stone-500 dark:text-stone-400">
                          <Icon className="h-4 w-4 shrink-0" />
                          <span className="text-xs">{item.kind}</span>
                        </span>
                      </td>
                      <td className="px-3 py-2.5 font-medium text-stone-800 dark:text-stone-100">
                        {item.name}
                      </td>
                      <td className="px-3 py-2.5">
                        <Badge tone={statusChipTone(item.status)}>{t(statusLabelKey(item.status))}</Badge>
                      </td>
                      <td className="px-5 py-2.5 text-xs text-stone-500 dark:text-stone-400">
                        {item.reason ?? <span className="text-stone-300 dark:text-stone-600">—</span>}
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        )}
      </Card>

      {result.notes.length > 0 && (
        <Card title={t('migrate.notes.title')}>
          <ul className="space-y-1.5">
            {result.notes.map((note, i) => (
              <li key={i} className="flex items-start gap-2 text-sm text-stone-600 dark:text-stone-300">
                <span className="mt-1.5 h-1 w-1 shrink-0 rounded-full bg-stone-400" aria-hidden="true" />
                {note}
              </li>
            ))}
          </ul>
        </Card>
      )}
    </div>
  );
}

export function MigratePage() {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) =>
    intl.formatMessage({ id }, values);

  const [step, setStep] = useState<WizardStep>('platform');
  const [platform, setPlatform] = useState<MigratePlatform | null>(null);
  const [source, setSource] = useState('');
  const [rename, setRename] = useState(false);

  const [scanning, setScanning] = useState(false);
  const [scan, setScan] = useState<MigrateResult | null>(null);

  const [confirmOpen, setConfirmOpen] = useState(false);
  const [applying, setApplying] = useState(false);
  const [applied, setApplied] = useState<MigrateResult | null>(null);

  const [error, setError] = useState<string | null>(null);

  const card = platform ? migratePlatformCard(platform) : null;
  const scanReady = platform != null && canScan(platform, source);

  const runScan = async () => {
    if (!platform) return;
    setError(null);
    setScanning(true);
    try {
      const res = await api.migrate.scan(platform, source || undefined);
      setScan(res);
      setRename(res.summary.conflict > 0 ? rename : false);
      setStep('preview');
    } catch (e) {
      setError(formatError(e));
    } finally {
      setScanning(false);
    }
  };

  const runApply = async () => {
    if (!platform) return;
    setConfirmOpen(false);
    setError(null);
    setApplying(true);
    try {
      const res = await api.migrate.apply(platform, source || undefined, rename);
      setApplied(res);
      setStep('result');
    } catch (e) {
      setError(formatError(e));
    } finally {
      setApplying(false);
    }
  };

  const reset = () => {
    setStep('platform');
    setPlatform(null);
    setSource('');
    setRename(false);
    setScan(null);
    setApplied(null);
    setError(null);
  };

  const platformName = (p: MigratePlatform) => t(`migrate.platform.${p}.name`);

  return (
    <Page>
      <PageHeader
        icon={Import}
        title={t('migrate.title')}
        subtitle={t('migrate.subtitle')}
        actions={<Stepper step={step} />}
      />

      {error && (
        <Card className="border-rose-500/30">
          <div className="flex items-start gap-3">
            <AlertTriangle className="mt-0.5 h-5 w-5 shrink-0 text-rose-500" />
            <div className="min-w-0 flex-1">
              <p className="text-sm font-medium text-stone-800 dark:text-stone-100">
                {t('migrate.error.title')}
              </p>
              <p className="mt-0.5 break-words text-sm text-stone-500 dark:text-stone-400">{error}</p>
            </div>
            <Button
              size="sm"
              variant="secondary"
              onClick={step === 'platform' || step === 'preview' ? runScan : runApply}
            >
              {t('migrate.retry')}
            </Button>
          </div>
        </Card>
      )}

      {/* ── Step 1: choose platform + source ── */}
      {step === 'platform' && (
        <div className="space-y-5">
          <div className="grid gap-4 sm:grid-cols-3">
            {MIGRATE_PLATFORMS.map((p) => {
              const selected = platform === p.id;
              return (
                <button
                  key={p.id}
                  type="button"
                  onClick={() => {
                    setPlatform(p.id);
                    setSource('');
                  }}
                  aria-pressed={selected}
                  className={cn(
                    'panel panel-hover flex flex-col gap-2 p-4 text-left transition-all',
                    'focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-500/50',
                    selected && 'ring-2 ring-amber-500/60',
                  )}
                >
                  <div className="flex items-center justify-between">
                    <span className="text-base font-semibold text-stone-900 dark:text-stone-50">
                      {platformName(p.id)}
                    </span>
                    {selected && <Check className="h-4 w-4 text-amber-500" />}
                  </div>
                  <p className="text-xs text-stone-500 dark:text-stone-400">
                    {t(`migrate.platform.${p.id}.desc`)}
                  </p>
                </button>
              );
            })}
          </div>

          {card && (
            <Card title={t('migrate.source.title')}>
              <div className="space-y-4">
                {card.needsExport && (
                  <div className="space-y-2 rounded-control border border-amber-500/25 bg-amber-500/5 p-3">
                    <p className="flex items-center gap-2 text-sm font-medium text-amber-700 dark:text-amber-300">
                      <ArrowDownToLine className="h-4 w-4" />
                      {t('migrate.paperclip.exportTitle')}
                    </p>
                    <p className="text-xs text-stone-500 dark:text-stone-400">
                      {t('migrate.paperclip.exportHint')}
                    </p>
                    <CommandBlock command={PAPERCLIP_EXPORT_CMD} />
                  </div>
                )}

                <Field
                  label={t('migrate.source.label')}
                  required={card.sourceRequired}
                  help={
                    card.sourceRequired
                      ? t('migrate.source.help.required')
                      : t('migrate.source.help.optional')
                  }
                >
                  <input
                    type="text"
                    value={source}
                    onChange={(e) => setSource(e.target.value)}
                    placeholder={card.defaultSource ?? t('migrate.source.placeholder.export')}
                    className={controlClass}
                    autoComplete="off"
                    spellCheck={false}
                  />
                </Field>

                <div className="flex justify-end">
                  <Button
                    variant="primary"
                    icon={ArrowRight}
                    pending={scanning}
                    disabled={!scanReady}
                    onClick={runScan}
                  >
                    {t('migrate.action.scan')}
                  </Button>
                </div>
              </div>
            </Card>
          )}
        </div>
      )}

      {/* ── Step 2: scan preview ── */}
      {step === 'preview' && scan && (
        <div className="space-y-5">
          <ResultReport result={scan} />

          {scan.summary.conflict > 0 && (
            <Card>
              <div className="flex items-start justify-between gap-4">
                <div className="min-w-0">
                  <p className="text-sm font-medium text-stone-800 dark:text-stone-100">
                    {t('migrate.rename.title')}
                  </p>
                  <p className="mt-0.5 text-xs text-stone-500 dark:text-stone-400">
                    {t('migrate.rename.hint')}
                  </p>
                </div>
                <Toggle checked={rename} onChange={setRename} labelId={t('migrate.rename.title')} />
              </div>
            </Card>
          )}

          <div className="flex items-center justify-between gap-3">
            <Button variant="ghost" icon={ArrowLeft} onClick={() => setStep('platform')} disabled={applying}>
              {t('common.back')}
            </Button>
            <Button
              variant="primary"
              icon={ArrowDownToLine}
              onClick={() => setConfirmOpen(true)}
              disabled={applying}
            >
              {t('migrate.action.apply')}
            </Button>
          </div>

          {applying && (
            <div className="flex items-center justify-center gap-3 rounded-control border border-[var(--panel-border)] bg-stone-500/5 py-6 text-sm text-stone-500 dark:bg-white/5 dark:text-stone-400">
              <Loader2 className="h-5 w-5 animate-spin text-amber-500" />
              {t('migrate.applying')}
            </div>
          )}
        </div>
      )}

      {/* ── Step 3: result ── */}
      {step === 'result' && applied && (
        <div className="space-y-5">
          <Card>
            <div className="flex flex-col gap-1">
              <span className="text-xs font-medium text-stone-500 dark:text-stone-400">
                {t('migrate.verdict.label')}
              </span>
              <span className={cn('text-3xl font-semibold tracking-tight', verdictToneClass(applied.verdict))}>
                {t(verdictLabelKey(applied.verdict))}
              </span>
            </div>
          </Card>

          <ResultReport result={applied} />

          {applied.report_path && (
            <Card title={t('migrate.report.title')}>
              <Mono className="break-all text-xs">{applied.report_path}</Mono>
            </Card>
          )}

          <div className="flex justify-end">
            <Button variant="secondary" icon={Import} onClick={reset}>
              {t('migrate.action.again')}
            </Button>
          </div>
        </div>
      )}

      <ConfirmDialog
        open={confirmOpen}
        onClose={() => setConfirmOpen(false)}
        onConfirm={runApply}
        title={t('migrate.confirm.title')}
        message={t('migrate.confirm.message', {
          platform: platform ? platformName(platform) : '',
          imported: scan?.summary.imported ?? 0,
        })}
        confirmLabel={t('migrate.action.apply')}
        busy={applying}
      />
    </Page>
  );
}
