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
  Card,
  CardHeader,
  CardTitle,
  CardContent,
  Button,
  Badge,
  Input,
  Switch,
  Table,
  TableHeader,
  TableBody,
  TableRow,
  TableHead,
  TableCell,
  type BadgeProps,
} from '@/components/mds';
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
  type MigrateBadgeTone,
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

/** Map the migrate status tone → mds Badge variant + className (token-only colours). */
function statusBadgeProps(tone: MigrateBadgeTone): {
  variant: BadgeProps['variant'];
  className?: string;
} {
  switch (tone) {
    case 'success':
      return { variant: 'secondary', className: 'bg-success/15 text-success' };
    case 'warning':
      return { variant: 'secondary', className: 'bg-warning/15 text-warning' };
    case 'danger':
      return { variant: 'secondary', className: 'bg-destructive/10 text-destructive' };
    case 'neutral':
      return { variant: 'ghost' };
  }
}

/** Text colour for a summary KPI tile keyed by summary bucket. */
const SUMMARY_TONE: Record<'imported' | 'partial' | 'skipped' | 'conflict', string> = {
  imported: 'text-success',
  partial: 'text-warning',
  skipped: 'text-muted-foreground',
  conflict: 'text-destructive',
};

/** 1-2-3 step indicator (MDS tokens). */
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
                active && 'bg-brand text-brand-foreground',
                done && 'bg-success/15 text-success',
                !active && !done && 'bg-muted text-muted-foreground',
              )}
            >
              {done ? <Check className="h-3.5 w-3.5" /> : i + 1}
            </span>
            <span className={cn('font-medium', active ? 'text-foreground' : 'text-muted-foreground')}>
              {labels[s]}
            </span>
            {i < order.length - 1 && <span className="mx-1 text-muted-foreground/50">/</span>}
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
    <div className="flex items-start gap-2 rounded-lg border border-surface-border bg-muted p-2.5">
      <Terminal className="mt-0.5 h-4 w-4 shrink-0 text-muted-foreground" />
      <code className="min-w-0 flex-1 overflow-x-auto whitespace-pre text-xs text-muted-foreground">
        {command}
      </code>
      <button
        type="button"
        onClick={copy}
        aria-label={intl.formatMessage({ id: copied ? 'migrate.copied' : 'migrate.copy' })}
        className="shrink-0 rounded-md p-1 text-muted-foreground transition-colors hover:bg-surface-hover hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50"
      >
        {copied ? <Check className="h-4 w-4 text-success" /> : <Copy className="h-4 w-4" />}
      </button>
    </div>
  );
}

/** Summary tiles + item table shared by the preview and result steps. */
function ResultReport({ result }: { result: MigrateResult }) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });

  const tiles: { key: keyof typeof SUMMARY_TONE; value: number }[] = [
    { key: 'imported', value: result.summary.imported },
    { key: 'partial', value: result.summary.partial },
    { key: 'skipped', value: result.summary.skipped },
    { key: 'conflict', value: result.summary.conflict },
  ];

  return (
    <div className="space-y-5">
      <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
        {tiles.map((tile) => (
          <Card key={tile.key} className="gap-1 p-4">
            <span className="text-xs text-muted-foreground">{t(`migrate.summary.${tile.key}`)}</span>
            <span className={cn('text-2xl font-semibold tabular-nums', SUMMARY_TONE[tile.key])}>
              {tile.value}
            </span>
          </Card>
        ))}
      </div>

      <Card className="gap-0 py-0">
        <CardHeader className="py-4">
          <CardTitle>{t('migrate.items.title')}</CardTitle>
        </CardHeader>
        {result.items.length === 0 ? (
          <p className="px-4 pb-8 text-center text-sm text-muted-foreground">{t('migrate.items.empty')}</p>
        ) : (
          <Table className="min-w-[36rem]">
            <TableHeader>
              <TableRow>
                <TableHead className="px-4">{t('migrate.col.kind')}</TableHead>
                <TableHead>{t('migrate.col.name')}</TableHead>
                <TableHead>{t('migrate.col.status')}</TableHead>
                <TableHead className="px-4">{t('migrate.col.reason')}</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {result.items.map((item: MigrateItem, i) => {
                const Icon = kindIcon(item.kind);
                const badge = statusBadgeProps(statusChipTone(item.status));
                return (
                  <TableRow key={`${item.kind}-${item.name}-${i}`}>
                    <TableCell className="px-4">
                      <span className="inline-flex items-center gap-2 text-muted-foreground">
                        <Icon className="h-4 w-4 shrink-0" />
                        <span className="text-xs">{item.kind}</span>
                      </span>
                    </TableCell>
                    <TableCell className="font-medium text-foreground">{item.name}</TableCell>
                    <TableCell>
                      <Badge variant={badge.variant} className={badge.className}>
                        {t(statusLabelKey(item.status))}
                      </Badge>
                    </TableCell>
                    <TableCell className="px-4 text-xs text-muted-foreground whitespace-normal">
                      {item.reason ?? <span className="text-muted-foreground/50">—</span>}
                    </TableCell>
                  </TableRow>
                );
              })}
            </TableBody>
          </Table>
        )}
      </Card>

      {result.notes.length > 0 && (
        <Card className="p-4">
          <CardTitle className="text-sm">{t('migrate.notes.title')}</CardTitle>
          <ul className="space-y-1.5">
            {result.notes.map((note, i) => (
              <li key={i} className="flex items-start gap-2 text-sm text-muted-foreground">
                <span className="mt-1.5 h-1 w-1 shrink-0 rounded-full bg-muted-foreground" aria-hidden="true" />
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
    <div className="mx-auto w-full max-w-3xl space-y-5">
      {/* Header */}
      <div className="space-y-4">
        <div className="flex items-center gap-2">
          <Import className="size-5 text-muted-foreground" />
          <div>
            <h1 className="text-base font-medium">{t('migrate.title')}</h1>
            <p className="text-sm text-muted-foreground">{t('migrate.subtitle')}</p>
          </div>
        </div>
        <Stepper step={step} />
      </div>

      {error && (
        <div className="rounded-xl border border-destructive/30 bg-destructive/10 p-4">
          <div className="flex items-start gap-3">
            <AlertTriangle className="mt-0.5 h-5 w-5 shrink-0 text-destructive" />
            <div className="min-w-0 flex-1">
              <p className="text-sm font-medium text-foreground">{t('migrate.error.title')}</p>
              <p className="mt-0.5 break-words text-sm text-muted-foreground">{error}</p>
            </div>
            <Button
              size="sm"
              variant="outline"
              onClick={step === 'platform' || step === 'preview' ? runScan : runApply}
            >
              {t('migrate.retry')}
            </Button>
          </div>
        </div>
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
                    'flex flex-col gap-2 rounded-xl border border-surface-border bg-surface p-4 text-left transition-colors hover:bg-surface-hover',
                    'focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50',
                    selected && 'ring-2 ring-brand/50',
                  )}
                >
                  <div className="flex items-center justify-between">
                    <span className="text-base font-semibold text-foreground">{platformName(p.id)}</span>
                    {selected && <Check className="h-4 w-4 text-brand" />}
                  </div>
                  <p className="text-xs text-muted-foreground">{t(`migrate.platform.${p.id}.desc`)}</p>
                </button>
              );
            })}
          </div>

          {card && (
            <Card className="p-4">
              <CardHeader className="px-0">
                <CardTitle>{t('migrate.source.title')}</CardTitle>
              </CardHeader>
              <CardContent className="space-y-4 px-0">
                {card.needsExport && (
                  <div className="space-y-2 rounded-xl border border-brand/25 bg-brand/5 p-4">
                    <p className="flex items-center gap-2 text-sm font-medium text-brand">
                      <ArrowDownToLine className="h-4 w-4" />
                      {t('migrate.paperclip.exportTitle')}
                    </p>
                    <p className="text-xs text-muted-foreground">{t('migrate.paperclip.exportHint')}</p>
                    <CommandBlock command={PAPERCLIP_EXPORT_CMD} />
                  </div>
                )}

                <div className="space-y-1.5">
                  <label className="text-sm font-medium text-foreground">
                    {t('migrate.source.label')}
                    {card.sourceRequired && <span className="ml-0.5 text-destructive">*</span>}
                  </label>
                  <Input
                    type="text"
                    value={source}
                    onChange={(e) => setSource(e.target.value)}
                    placeholder={card.defaultSource ?? t('migrate.source.placeholder.export')}
                    autoComplete="off"
                    spellCheck={false}
                  />
                  <p className="text-xs text-muted-foreground">
                    {card.sourceRequired
                      ? t('migrate.source.help.required')
                      : t('migrate.source.help.optional')}
                  </p>
                </div>

                <div className="flex justify-end">
                  <Button variant="brand" disabled={!scanReady || scanning} onClick={runScan}>
                    {scanning ? <Loader2 className="animate-spin" /> : <ArrowRight />}
                    {t('migrate.action.scan')}
                  </Button>
                </div>
              </CardContent>
            </Card>
          )}
        </div>
      )}

      {/* ── Step 2: scan preview ── */}
      {step === 'preview' && scan && (
        <div className="space-y-5">
          <ResultReport result={scan} />

          {scan.summary.conflict > 0 && (
            <Card className="p-4">
              <div className="flex items-start justify-between gap-4">
                <div className="min-w-0">
                  <p className="text-sm font-medium text-foreground">{t('migrate.rename.title')}</p>
                  <p className="mt-0.5 text-xs text-muted-foreground">{t('migrate.rename.hint')}</p>
                </div>
                <Switch
                  checked={rename}
                  onCheckedChange={setRename}
                  aria-label={t('migrate.rename.title')}
                />
              </div>
            </Card>
          )}

          <div className="flex items-center justify-between gap-3">
            <Button variant="ghost" onClick={() => setStep('platform')} disabled={applying}>
              <ArrowLeft />
              {t('common.back')}
            </Button>
            <Button variant="brand" onClick={() => setConfirmOpen(true)} disabled={applying}>
              <ArrowDownToLine />
              {t('migrate.action.apply')}
            </Button>
          </div>

          {applying && (
            <div className="flex items-center justify-center gap-3 rounded-xl border border-surface-border bg-muted py-6 text-sm text-muted-foreground">
              <Loader2 className="h-5 w-5 animate-spin text-brand" />
              {t('migrate.applying')}
            </div>
          )}
        </div>
      )}

      {/* ── Step 3: result ── */}
      {step === 'result' && applied && (
        <div className="space-y-5">
          <Card className="p-4">
            <div className="flex flex-col gap-1">
              <span className="text-xs font-medium text-muted-foreground">{t('migrate.verdict.label')}</span>
              <span className={cn('text-3xl font-semibold tracking-tight', verdictToneClass(applied.verdict))}>
                {t(verdictLabelKey(applied.verdict))}
              </span>
            </div>
          </Card>

          <ResultReport result={applied} />

          {applied.report_path && (
            <Card className="p-4">
              <CardTitle className="text-sm">{t('migrate.report.title')}</CardTitle>
              <p className="break-all font-mono text-xs text-muted-foreground">{applied.report_path}</p>
            </Card>
          )}

          <div className="flex justify-end">
            <Button variant="outline" onClick={reset}>
              <Import />
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
    </div>
  );
}
