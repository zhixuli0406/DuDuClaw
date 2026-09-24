import { useEffect, useState, type ReactNode } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate } from 'react-router';
import {
  Loader2,
  ArrowLeft,
  ArrowRight,
  Check,
  CheckCircle2,
  AlertTriangle,
  Play,
  ChevronDown,
  Plus,
} from 'lucide-react';
import { cn } from '@/lib/utils';
import {
  api,
  type DbSourceSummary,
  type RedactionDataSource,
  type DbSourceTestResult,
  type RedactionEgressRule,
  type RedactionRestoreArgs,
  type RedactionDryRunResult,
  type DbSourceGrantAgent,
  type DbSourceGrantSource,
} from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  Button,
  Input,
  Textarea,
  Select,
  SelectTrigger,
  SelectValue,
  SelectContent,
  SelectItem,
  Segmented,
  Checkbox,
  Badge,
  CrossLink,
} from '@/components/mds';
import { FieldBlock } from '@/pages/agent-form/form-rows';
import { SecretSourceField } from '@/components/shared/SecretSourceField';
import { RedactionTokenInput, KvRowEditor } from './RedactionTokenInput';
import { DbSourceAgentPicker } from './DbSourceAgentPicker';
import {
  emptyDataSourceForm,
  dataSourceToFormState,
  formStateToDataSourceDef,
  validateDataSourceForm,
  emptyDbSourceForm,
  dbSourceToFormState,
  formStateToDbSourceUpsert,
  formStateToDbSourceTestParams,
  validateDbSourceForm,
  summarizeDbSourceTest,
  addKvRow,
  updateKvRow,
  removeKvRow,
  type DataSourceFormState,
  type DbSourceFormState,
  type DataSourceTableMode,
} from './redactionFieldRules';
import {
  deriveCustomEgressKeys,
  readEgressForKeys,
  buildEgressPatch,
  deriveDataSourceId,
  canProceedDbStep2,
  customSourceSmokeTestParams,
} from './redactionSystems';
import { grantedAgentsForSource } from './dbSourceGrants';

// ── "新增資料來源" wizard (canvas screen 10, §15.2) ──────────────────────
//
// Four fixed steps: 類型 → 連線/工具 → 測試 → 寫回. Each step only asks for
// what its type needs; record_paths / table_result / key_alias / timeouts
// are folded behind an "進階設定" disclosure with working defaults. Editing
// an existing source skips step 1 and opens directly on step 2 (§15.2).

export type WizardSourceType = 'db' | 'custom';
type WizardStep = 1 | 2 | 3 | 4;

export type WizardTarget =
  | { mode: 'create' }
  | { mode: 'edit'; type: 'db'; row: DbSourceSummary }
  | { mode: 'edit'; type: 'custom'; row: RedactionDataSource };

const REDACTION_RESTORE: ReadonlyArray<RedactionRestoreArgs> = ['deny', 'restore', 'passthrough'];
const DB_DRIVERS = ['postgres', 'mysql', 'sqlite'] as const;

function StepIndicator({ step }: { step: WizardStep }) {
  const intl = useIntl();
  const labels = ['type', 'connection', 'test', 'writeback'] as const;
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
              {intl.formatMessage({ id: `redaction.wizard.step.${key}` })}
            </span>
            {i < labels.length - 1 && <span className="h-px flex-1 bg-surface-border" aria-hidden="true" />}
          </li>
        );
      })}
    </ol>
  );
}

/** Small "進階設定" disclosure — folded knobs that already have working
 *  defaults (record_paths / key_alias / free_form_names / max_rows /
 *  timeout_ms), per §15.2's "進階折疊的範圍". */
function AdvancedFold({ label, children }: { label: string; children: ReactNode }) {
  const [open, setOpen] = useState(false);
  return (
    <div className="rounded-lg border border-surface-border">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        className="flex w-full items-center justify-between px-3 py-2 text-xs font-medium text-muted-foreground hover:text-foreground"
      >
        {label}
        <ChevronDown className={cn('size-3.5 transition-transform', open && 'rotate-180')} />
      </button>
      {open && <div className="space-y-3 border-t border-surface-border p-3">{children}</div>}
    </div>
  );
}

export function RedactionSourceWizard({
  open,
  target,
  toolEgress,
  existingSourceNames,
  existingDbNames,
  grantAgents,
  grantSources,
  onClose,
  onSaved,
  onGrantsSaved,
}: {
  open: boolean;
  target: WizardTarget;
  /** Current `tool_egress` map — used to prefill step 4 when editing an
   *  existing custom source (the derived key(s) may already carry a saved
   *  policy). */
  toolEgress: Record<string, RedactionEgressRule>;
  /** Every already-saved `data_sources` key (built-ins included) — passed
   *  to `deriveDataSourceId` so a freshly-typed 顯示名稱 never derives an id
   *  that collides with (or shadows) one already in use. */
  existingSourceNames: readonly string[];
  /** Every already-saved `db_sources` name, same purpose as above for the
   *  database wizard type. */
  existingDbNames: readonly string[];
  /** Every AI employee selectable in the db-source step 4 "誰能使用" picker,
   *  and each configured source's current grant list — both come from one
   *  `db_sources.grants.list` call the caller already made for the systems
   *  card's own badges, so the wizard doesn't fetch it a second time. Empty
   *  arrays render the picker's own empty state — never an error banner. */
  grantAgents: readonly DbSourceGrantAgent[];
  grantSources: readonly DbSourceGrantSource[];
  onClose: () => void;
  /** Called after a successful save, before `onClose` — the caller reloads
   *  `redaction.get` / `db_sources.list` and (for a fresh custom source)
   *  may want to jump the field-rules card's filter to it. */
  onSaved: (saved: { type: WizardSourceType; name: string; label: string }) => void;
  /** Called once the db-source grant write finishes (success or failure) so
   *  the caller can refresh its own `grants.list`-derived badges. */
  onGrantsSaved?: () => void;
}) {
  const intl = useIntl();
  const navigate = useNavigate();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);

  const isNew = target.mode === 'create';

  const [step, setStep] = useState<WizardStep>(1);
  const [sourceType, setSourceType] = useState<WizardSourceType>('db');
  const [attempted, setAttempted] = useState(false);
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);

  const [dbForm, setDbForm] = useState<DbSourceFormState>(emptyDbSourceForm());
  const [dbTesting, setDbTesting] = useState(false);
  const [dbTestResult, setDbTestResult] = useState<DbSourceTestResult | null>(null);
  // Step 4 (db sources) — which AI employees may use this source. Prefilled
  // from `grantSources` when editing; starts empty (deny-by-default) when
  // creating.
  const [dbGrantSelected, setDbGrantSelected] = useState<string[]>([]);

  const [customForm, setCustomForm] = useState<DataSourceFormState>(emptyDataSourceForm());
  const [customSample, setCustomSample] = useState('');
  const [customDryRun, setCustomDryRun] = useState<RedactionDryRunResult | null>(null);
  const [customDryRunLoading, setCustomDryRunLoading] = useState(false);
  const [customDryRunError, setCustomDryRunError] = useState<string | null>(null);

  const [egressPolicy, setEgressPolicy] = useState<RedactionRestoreArgs>('deny');
  const [egressAudit, setEgressAudit] = useState(false);

  // Reset every piece of local state whenever the dialog (re)opens, so a
  // stale draft from a previous "新增"/"編輯" never bleeds into the next one.
  useEffect(() => {
    if (!open) return;
    setAttempted(false);
    setSaveError(null);
    setDbTestResult(null);
    setCustomSample('');
    setCustomDryRun(null);
    setCustomDryRunError(null);

    if (target.mode === 'edit' && target.type === 'db') {
      setStep(2);
      setSourceType('db');
      setDbForm(dbSourceToFormState(target.row));
      setDbGrantSelected(grantedAgentsForSource(grantSources, target.row.name));
    } else if (target.mode === 'edit' && target.type === 'custom') {
      setStep(2);
      setSourceType('custom');
      setCustomForm(dataSourceToFormState(target.row));
      const keys = deriveCustomEgressKeys(target.row.tools);
      const existing = readEgressForKeys(toolEgress, keys);
      setEgressPolicy(existing?.restore_args ?? 'deny');
      setEgressAudit(existing?.audit_reveal ?? false);
    } else {
      setStep(1);
      setSourceType('db');
      setDbForm(emptyDbSourceForm());
      setCustomForm(emptyDataSourceForm());
      setEgressPolicy('deny');
      setEgressAudit(false);
      setDbGrantSelected([]);
    }
    // `toolEgress`/`grantSources` intentionally excluded — they should only
    // seed the form the moment the dialog opens, not re-sync while the
    // operator is mid-edit.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, target]);

  const dbErrors = validateDbSourceForm(dbForm, isNew);
  const customErrors = validateDataSourceForm(customForm, isNew);

  const handleNext = () => {
    if (step === 1) {
      setStep(2);
      return;
    }
    if (step === 2) {
      setAttempted(true);
      if (sourceType === 'db') {
        if (!canProceedDbStep2(dbForm.label, dbForm.url, isNew)) return;
      } else if (!customForm.label.trim() || Object.keys(customErrors).length > 0) {
        return;
      }
      setAttempted(false);
      setStep(3);
      return;
    }
    if (step === 3) {
      setStep(4);
    }
  };
  const handleBack = () => {
    setAttempted(false);
    if (step > 1) setStep((step - 1) as WizardStep);
  };

  const handleTestDb = async () => {
    setDbTesting(true);
    setDbTestResult(null);
    try {
      const result = await api.dbSources.test(formStateToDbSourceTestParams(dbForm));
      setDbTestResult(result);
    } catch (e) {
      setDbTestResult({ success: false, message: formatError(e), tables: [] });
    } finally {
      setDbTesting(false);
    }
  };

  const toggleAllowedTable = (name: string) => {
    setDbForm((f) => ({
      ...f,
      allowedTables: f.allowedTables.includes(name) ? f.allowedTables.filter((n) => n !== name) : [...f.allowedTables, name],
    }));
  };

  const handleDryRunCustom = async () => {
    setCustomDryRunLoading(true);
    setCustomDryRunError(null);
    try {
      const { tool, args } = customSourceSmokeTestParams(customForm);
      const result = await api.redaction.dryRun(customSample, tool || 'odoo_search', args);
      setCustomDryRun(result);
    } catch (e) {
      setCustomDryRunError(formatError(e));
    } finally {
      setCustomDryRunLoading(false);
    }
  };

  const handleFinish = async () => {
    setAttempted(true);
    setSaveError(null);
    if (sourceType === 'db') {
      if (!dbForm.label.trim() || Object.keys(dbErrors).length > 0) return;
      setSaving(true);
      const name = dbForm.name.trim();
      const label = dbForm.label.trim() || name;
      try {
        await api.dbSources.upsert(formStateToDbSourceUpsert(dbForm));
      } catch (e) {
        setSaveError(formatError(e));
        setSaving(false);
        return;
      }
      // The source itself is saved from here — a grants failure below must
      // never look like the whole save failed (WP-C: "consider the source
      // itself saved").
      onSaved({ type: 'db', name, label });
      try {
        await api.dbSources.grants.set({ name, agents: dbGrantSelected });
        toast.success(intl.formatMessage({ id: 'redaction.savedLive' }));
        onGrantsSaved?.();
        onClose();
      } catch (e) {
        setSaveError(t('redaction.wizard.step4.db.grantsSaveFailed', { message: formatError(e) }));
        onGrantsSaved?.();
      } finally {
        setSaving(false);
      }
      return;
    }
    if (!customForm.label.trim() || Object.keys(customErrors).length > 0) return;
    const name = isNew
      ? customForm.name.trim()
      : target.mode === 'edit' && target.type === 'custom'
        ? target.row.name
        : customForm.name.trim();
    setSaving(true);
    try {
      const def = formStateToDataSourceDef(customForm);
      const keys = deriveCustomEgressKeys(customForm.tools);
      const egressPatch = buildEgressPatch(keys, { restore_args: egressPolicy, audit_reveal: egressAudit });
      const res = await api.redaction.update({ data_sources: { [name]: def }, tool_egress: egressPatch });
      if (res.warning) {
        setSaveError(res.warning);
        return;
      }
      toast.success(intl.formatMessage({ id: 'redaction.savedLive' }));
      onSaved({ type: 'custom', name, label: def.label ?? name });
      onClose();
    } catch (e) {
      setSaveError(formatError(e));
    } finally {
      setSaving(false);
    }
  };

  const title = isNew ? t('redaction.wizard.title') : t('redaction.wizard.editTitle');
  const testOutcome = dbTestResult ? summarizeDbSourceTest(dbTestResult) : null;

  return (
    <Dialog open={open} onOpenChange={(o) => { if (!o) onClose(); }}>
      <DialogContent className="sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle>{title}</DialogTitle>
        </DialogHeader>

        <StepIndicator step={step} />

        <div className="max-h-[60vh] space-y-4 overflow-y-auto py-1">
          {/* ── Step 1: type ───────────────────────────────────────── */}
          {step === 1 && (
            <div className="space-y-3">
              <p className="text-sm text-muted-foreground">{t('redaction.wizard.step1.prompt')}</p>
              <div className="grid gap-3 sm:grid-cols-2">
                {(['db', 'custom'] as const).map((type) => (
                  <button
                    key={type}
                    type="button"
                    onClick={() => setSourceType(type)}
                    aria-pressed={sourceType === type}
                    className={cn(
                      'rounded-xl border p-3.5 text-left transition-colors',
                      sourceType === type ? 'border-brand bg-brand/8' : 'border-border hover:bg-muted',
                    )}
                  >
                    <p className="text-sm font-medium text-foreground">{t(`redaction.wizard.step1.${type}.title`)}</p>
                    <p className="mt-1 text-xs text-muted-foreground">{t(`redaction.wizard.step1.${type}.desc`)}</p>
                  </button>
                ))}
                <div className="rounded-xl border border-border p-3.5">
                  <p className="text-sm font-medium text-foreground">{t('redaction.wizard.step1.odoo.title')}</p>
                  <p className="mt-1 text-xs text-muted-foreground">{t('redaction.wizard.step1.odoo.desc')}</p>
                  <div className="mt-2">
                    <CrossLink
                      label={t('redaction.systems.odoo.goToIntegrations')}
                      onClick={() => { onClose(); navigate('/manage/integrations?tab=odoo'); }}
                    />
                  </div>
                </div>
                <div className="rounded-xl border border-dashed border-border p-3.5 opacity-60">
                  <p className="text-sm font-medium text-foreground">{t('redaction.wizard.step1.files.title')}</p>
                  <p className="mt-1 text-xs text-muted-foreground">{t('redaction.wizard.step1.files.desc')}</p>
                </div>
              </div>
            </div>
          )}

          {/* ── Step 2: connection / tool ──────────────────────────── */}
          {step === 2 && sourceType === 'db' && (
            <div className="space-y-3.5">
              <FieldBlock label={t('redaction.dataSources.form.label.label')} description={t('redaction.dataSources.form.label.hint')}>
                <Input
                  value={dbForm.label}
                  onChange={(e) => {
                    const label = e.target.value;
                    setDbForm((f) => ({ ...f, label, name: deriveDataSourceId(label, 'db', existingDbNames, isNew, f.name) }));
                  }}
                  placeholder={t('redaction.dataSources.title')}
                  aria-invalid={attempted && !dbForm.label.trim()}
                />
                {attempted && !dbForm.label.trim() && (
                  <p className="mt-1 text-xs text-destructive">{t('redaction.dataSources.form.error.label')}</p>
                )}
              </FieldBlock>
              <div className="grid gap-3.5 sm:grid-cols-2">
                <FieldBlock label={t('redaction.dataSources.db.driver.label')}>
                  <Select value={dbForm.driver} onValueChange={(driver) => setDbForm({ ...dbForm, driver: (driver ?? 'postgres') as DbSourceFormState['driver'] })}>
                    <SelectTrigger className="w-full"><SelectValue>{dbForm.driver}</SelectValue></SelectTrigger>
                    <SelectContent>
                      {DB_DRIVERS.map((d) => <SelectItem key={d} value={d}>{d}</SelectItem>)}
                    </SelectContent>
                  </Select>
                </FieldBlock>
                <FieldBlock label={t('redaction.dataSources.db.url.label')}>
                  <SecretSourceField value={dbForm.url} onChange={(url) => setDbForm({ ...dbForm, url })} placeholder={t('redaction.dataSources.db.url.placeholder') as string} />
                </FieldBlock>
              </div>
              {attempted && !canProceedDbStep2(dbForm.label, dbForm.url, isNew) && (
                <FormErrorBanner
                  messages={[
                    !dbForm.url.trim() ? t('redaction.dataSources.db.error.url') : null,
                  ]}
                />
              )}
              <FieldBlock label={t('redaction.dataSources.db.allowedTables.label')} description={t('redaction.wizard.step2.db.allowedTablesHint')}>
                <RedactionTokenInput
                  tokens={dbForm.allowedTables}
                  onChange={(allowedTables) => setDbForm({ ...dbForm, allowedTables })}
                  placeholder={t('redaction.dataSources.db.allowedTables.placeholder')}
                />
              </FieldBlock>
              <AdvancedFold label={t('redaction.advanced')}>
                <FieldBlock label={t('redaction.dataSources.form.systemId.label')} description={t('redaction.dataSources.form.systemId.hint')}>
                  <code className="block w-fit rounded bg-muted px-1.5 py-0.5 font-mono text-xs text-muted-foreground">{dbForm.name || '—'}</code>
                </FieldBlock>
                <div className="grid gap-3.5 sm:grid-cols-2">
                  <FieldBlock label={t('redaction.dataSources.db.maxRows.label')}>
                    <Input type="number" value={dbForm.maxRows} onChange={(e) => setDbForm({ ...dbForm, maxRows: Number(e.target.value) })} />
                  </FieldBlock>
                  <FieldBlock label={t('redaction.dataSources.db.timeoutMs.label')}>
                    <Input type="number" value={dbForm.timeoutMs} onChange={(e) => setDbForm({ ...dbForm, timeoutMs: Number(e.target.value) })} />
                  </FieldBlock>
                </div>
              </AdvancedFold>
            </div>
          )}

          {step === 2 && sourceType === 'custom' && (
            <div className="space-y-3.5">
              <FieldBlock label={t('redaction.dataSources.form.label.label')} description={t('redaction.dataSources.form.label.hint')}>
                <Input
                  value={customForm.label}
                  onChange={(e) => {
                    const label = e.target.value;
                    setCustomForm((f) => ({
                      ...f,
                      label,
                      name: deriveDataSourceId(label, 'source', existingSourceNames, isNew, f.name),
                    }));
                  }}
                  placeholder={t('redaction.ext.custom')}
                  aria-invalid={attempted && !customForm.label.trim()}
                />
                {attempted && !customForm.label.trim() && (
                  <p className="mt-1 text-xs text-destructive">{t('redaction.dataSources.form.error.label')}</p>
                )}
              </FieldBlock>

              <FieldBlock label={t('redaction.dataSources.form.tools.label')} description={t('redaction.dataSources.form.tools.hint')}>
                <RedactionTokenInput
                  tokens={customForm.tools}
                  onChange={(tools) => setCustomForm({ ...customForm, tools })}
                  placeholder={t('redaction.dataSources.form.tools.placeholder')}
                  invalid={attempted && customErrors.tools === 'empty'}
                />
                {attempted && customErrors.tools === 'empty' && (
                  <p className="mt-1 text-xs text-destructive">{t('redaction.dataSources.form.error.tools')}</p>
                )}
              </FieldBlock>

              <FieldBlock label={t('redaction.dataSources.form.tableMode.label')}>
                <Segmented<DataSourceTableMode>
                  value={customForm.tableMode}
                  onValueChange={(tableMode) => setCustomForm({ ...customForm, tableMode })}
                  options={[
                    { value: 'from_arg', label: t('redaction.dataSources.form.tableMode.fromArg') },
                    { value: 'fixed', label: t('redaction.dataSources.form.tableMode.fixed') },
                    { value: 'from_result', label: t('redaction.dataSources.form.tableMode.fromResult') },
                  ]}
                />
                {customForm.tableMode === 'from_arg' ? (
                  <>
                    <Input
                      value={customForm.tableArg}
                      onChange={(e) => setCustomForm({ ...customForm, tableArg: e.target.value.trim() })}
                      className="mt-1.5 font-mono"
                      placeholder="table"
                    />
                    <p className="mt-1 text-xs text-muted-foreground">{t('redaction.dataSources.form.tableArg.hint')}</p>
                  </>
                ) : customForm.tableMode === 'fixed' ? (
                  <Input
                    value={customForm.fixedTable}
                    onChange={(e) => setCustomForm({ ...customForm, fixedTable: e.target.value.trim() })}
                    className="mt-1.5 font-mono"
                    placeholder="customers"
                  />
                ) : (
                  <>
                    <Input
                      value={customForm.tableResult}
                      onChange={(e) => setCustomForm({ ...customForm, tableResult: e.target.value.trim() })}
                      className="mt-1.5 font-mono"
                      placeholder="/table"
                    />
                    <p className="mt-1 text-xs text-muted-foreground">{t('redaction.dataSources.form.tableResult.hint')}</p>
                  </>
                )}
                {attempted && customErrors.table && <p className="mt-1 text-xs text-destructive">{t('redaction.dataSources.form.error.table')}</p>}
              </FieldBlock>

              <AdvancedFold label={t('redaction.advanced')}>
                <FieldBlock label={t('redaction.dataSources.form.systemId.label')} description={t('redaction.dataSources.form.systemId.hint')}>
                  <code className="block w-fit rounded bg-muted px-1.5 py-0.5 font-mono text-xs text-muted-foreground">{customForm.name || '—'}</code>
                </FieldBlock>
                <FieldBlock label={t('redaction.dataSources.form.recordPaths.label')} description={t('redaction.dataSources.form.recordPaths.hint')}>
                  <div className="space-y-1.5">
                    {customForm.recordPaths.map((p, i) => (
                      <Input
                        key={i}
                        value={p}
                        onChange={(e) => setCustomForm({ ...customForm, recordPaths: customForm.recordPaths.map((pp, ii) => (ii === i ? e.target.value : pp)) })}
                        className="font-mono"
                      />
                    ))}
                    <button
                      type="button"
                      onClick={() => setCustomForm({ ...customForm, recordPaths: [...customForm.recordPaths, ''] })}
                      className="inline-flex items-center gap-1 text-xs text-brand hover:underline"
                    >
                      <Plus className="size-3" />{t('redaction.dataSources.form.recordPaths.add')}
                    </button>
                  </div>
                </FieldBlock>
                <FieldBlock label={t('redaction.dataSources.form.freeFormNames.label')} description={t('redaction.dataSources.form.freeFormNames.hint')}>
                  <label className="flex items-center gap-2 text-sm text-foreground">
                    <Checkbox
                      checked={customForm.freeFormNames}
                      onCheckedChange={(v) => setCustomForm({ ...customForm, freeFormNames: v === true })}
                      aria-label={t('redaction.dataSources.form.freeFormNames.checkboxLabel')}
                    />
                    {t('redaction.dataSources.form.freeFormNames.checkboxLabel')}
                  </label>
                </FieldBlock>
                <FieldBlock label={t('redaction.dataSources.form.keyAlias.label')} description={t('redaction.dataSources.form.keyAlias.hint')}>
                  <div className="space-y-1.5">
                    {customForm.keyAlias.map((row, i) => (
                      <KvRowEditor
                        key={i}
                        row={row}
                        onChange={(patch) => setCustomForm({ ...customForm, keyAlias: updateKvRow(customForm.keyAlias, i, patch) })}
                        onRemove={() => setCustomForm({ ...customForm, keyAlias: removeKvRow(customForm.keyAlias, i) })}
                        keyPlaceholder={t('redaction.dataSources.form.keyAlias.keyPlaceholder')}
                        valuePlaceholder={t('redaction.dataSources.form.keyAlias.valuePlaceholder')}
                      />
                    ))}
                    <button
                      type="button"
                      onClick={() => setCustomForm({ ...customForm, keyAlias: addKvRow(customForm.keyAlias) })}
                      className="inline-flex items-center gap-1 text-xs text-brand hover:underline"
                    >
                      <Plus className="size-3" />{t('redaction.dataSources.form.keyAlias.add')}
                    </button>
                  </div>
                </FieldBlock>
              </AdvancedFold>
            </div>
          )}

          {/* ── Step 3: test ───────────────────────────────────────── */}
          {step === 3 && sourceType === 'db' && (
            <div className="space-y-3.5">
              <div className="flex items-center gap-2">
                <Button variant="outline" size="sm" onClick={() => void handleTestDb()} disabled={dbTesting}>
                  {dbTesting ? <Loader2 className="animate-spin" /> : <Play />}
                  {dbTesting ? t('redaction.dataSources.db.testing') : t('redaction.dataSources.db.test')}
                </Button>
                {testOutcome && (testOutcome.success ? (
                  <span className="flex items-center gap-1 text-xs text-success">
                    <CheckCircle2 className="size-3.5" />
                    {t('redaction.dataSources.db.testResult.ok', { count: testOutcome.tableCount })}
                  </span>
                ) : (
                  <span className="flex items-center gap-1 text-xs text-destructive">
                    <AlertTriangle className="size-3.5" />
                    {t('redaction.dataSources.db.testResult.fail', { message: testOutcome.message })}
                  </span>
                ))}
              </div>
              <FieldBlock label={t('redaction.wizard.step3.db.prompt')} description={t('redaction.wizard.step3.db.hint')}>
                {dbTestResult && dbTestResult.tables.length > 0 ? (
                  <div className="flex flex-wrap gap-1.5">
                    {dbTestResult.tables.map((tb) => {
                      const checked = dbForm.allowedTables.includes(tb.name);
                      return (
                        <button
                          key={tb.name}
                          type="button"
                          onClick={() => toggleAllowedTable(tb.name)}
                          aria-pressed={checked}
                          className={cn(
                            'rounded-full border px-2.5 py-1 font-mono text-xs transition-colors',
                            checked ? 'border-brand/40 bg-brand/10 text-brand' : 'border-input text-muted-foreground hover:bg-muted',
                          )}
                        >
                          {tb.name}{checked && <Check className="ml-1 inline size-3" />}
                        </button>
                      );
                    })}
                  </div>
                ) : (
                  <div className="flex flex-wrap gap-1.5">
                    {dbForm.allowedTables.map((n) => (
                      <Badge key={n} variant="secondary" className="font-mono">{n}</Badge>
                    ))}
                    {dbForm.allowedTables.length === 0 && (
                      <p className="text-xs text-muted-foreground">{t('redaction.wizard.step3.db.testFirst')}</p>
                    )}
                  </div>
                )}
              </FieldBlock>
            </div>
          )}

          {step === 3 && sourceType === 'custom' && (
            <div className="space-y-3.5">
              <FieldBlock label={t('redaction.fieldRules.dryRun.sample.label')} description={t('redaction.fieldRules.dryRun.sample.hint')}>
                <Textarea value={customSample} onChange={(e) => setCustomSample(e.target.value)} className="min-h-32 font-mono text-xs" />
              </FieldBlock>
              <div className="flex items-center gap-2">
                <Button variant="outline" size="sm" onClick={() => void handleDryRunCustom()} disabled={customDryRunLoading || !customSample.trim()}>
                  {customDryRunLoading ? <Loader2 className="animate-spin" /> : <Play />}
                  {t('redaction.fieldRules.dryRun.run')}
                </Button>
                {customDryRun && (
                  <Badge className="bg-success/10 text-success">{t('redaction.fieldRules.dryRun.hits', { count: customDryRun.hits.length })}</Badge>
                )}
              </div>
              {customDryRunError && (
                <div className="flex gap-2.5 rounded-lg border border-destructive/30 bg-destructive/10 px-3 py-2.5 text-xs text-destructive">
                  <AlertTriangle className="mt-0.5 size-4 shrink-0" />
                  <span>{t('redaction.fieldRules.dryRun.error', { message: customDryRunError })}</span>
                </div>
              )}
              <p className="text-xs text-muted-foreground">{t('redaction.wizard.step3.custom.noRuleYetHint')}</p>
            </div>
          )}

          {/* ── Step 4: write-back ─────────────────────────────────── */}
          {step === 4 && (
            <div className="space-y-3.5">
              {sourceType === 'db' ? (
                <>
                  <FieldBlock label={t('redaction.wizard.step4.db.prompt')}>
                    <DbSourceAgentPicker
                      agents={grantAgents}
                      selected={dbGrantSelected}
                      onChange={setDbGrantSelected}
                    />
                    <p className="mt-1.5 text-xs text-muted-foreground">{t('redaction.wizard.step4.db.grantsHint')}</p>
                    <p className="mt-1 text-xs italic text-muted-foreground">{t('redaction.wizard.step4.dbHint')}</p>
                  </FieldBlock>
                  {attempted && (!dbForm.label.trim() || Object.keys(dbErrors).length > 0) && (
                    <FormErrorBanner
                      messages={[
                        !dbForm.label.trim() ? t('redaction.dataSources.form.error.label') : null,
                        dbErrors.name ? t('redaction.dataSources.form.error.name') : null,
                        dbErrors.url ? t('redaction.dataSources.db.error.url') : null,
                        dbErrors.allowedTables ? t('redaction.dataSources.db.error.allowedTables') : null,
                      ]}
                    />
                  )}
                </>
              ) : (
                <FieldBlock label={t('redaction.wizard.step4.prompt')}>
                  <Select value={egressPolicy} onValueChange={(v) => setEgressPolicy((v ?? 'deny') as RedactionRestoreArgs)}>
                    <SelectTrigger className="w-full sm:w-72"><SelectValue>{t(`redaction.restore.${egressPolicy}`)}</SelectValue></SelectTrigger>
                    <SelectContent>
                      {REDACTION_RESTORE.map((r) => <SelectItem key={r} value={r}>{t(`redaction.restore.${r}`)}</SelectItem>)}
                    </SelectContent>
                  </Select>
                  <label className="mt-2 flex items-center gap-2 text-xs text-muted-foreground">
                    <Checkbox checked={egressAudit} onCheckedChange={(v) => setEgressAudit(v === true)} aria-label={t('redaction.auditReveal')} />
                    {t('redaction.auditReveal')}
                  </label>
                </FieldBlock>
              )}

              <div className="flex gap-2.5 rounded-lg border border-success/30 bg-success/10 px-3.5 py-3 text-sm">
                <CheckCircle2 className="mt-0.5 size-4 shrink-0 text-success" />
                <div className="space-y-1">
                  <p className="font-medium text-foreground">{t('redaction.wizard.step4.doneTitle')}</p>
                  <p className="text-xs leading-relaxed text-muted-foreground">
                    {t('redaction.wizard.step4.doneHint', {
                      label: sourceType === 'db' ? (dbForm.label.trim() || dbForm.name.trim()) : (customForm.label.trim() || customForm.name.trim()),
                    })}
                  </p>
                </div>
              </div>
            </div>
          )}
        </div>

        {saveError && (
          <div className="flex gap-2.5 rounded-lg border border-destructive/30 bg-destructive/10 px-3 py-2.5 text-xs">
            <AlertTriangle className="mt-0.5 size-4 shrink-0 text-destructive" />
            <span className="text-destructive">{saveError}</span>
          </div>
        )}

        <div className="flex items-center justify-between gap-2 pt-1">
          <div>
            {step > 1 && (
              <Button variant="outline" size="sm" onClick={handleBack}>
                <ArrowLeft />
                {t('redaction.wizard.back')}
              </Button>
            )}
          </div>
          <div className="flex items-center gap-2">
            <Button variant="outline" size="sm" onClick={onClose}>{intl.formatMessage({ id: 'common.cancel' })}</Button>
            {step < 4 ? (
              <Button variant="brand" size="sm" onClick={handleNext}>
                {step === 3 && ((sourceType === 'db' && !dbTestResult) || (sourceType === 'custom' && !customDryRun))
                  ? t('redaction.wizard.skip')
                  : t('redaction.wizard.next')}
                <ArrowRight />
              </Button>
            ) : (
              <Button variant="brand" size="sm" onClick={() => void handleFinish()} disabled={saving}>
                {saving ? <Loader2 className="animate-spin" /> : <Check />}
                {saving ? intl.formatMessage({ id: 'common.saving' }) : t('redaction.wizard.finish')}
              </Button>
            )}
          </div>
        </div>
      </DialogContent>
    </Dialog>
  );
}

function FormErrorBanner({ messages }: { messages: Array<string | null> }) {
  const list = messages.filter((m): m is string => !!m);
  if (list.length === 0) return null;
  return (
    <div className="flex gap-2.5 rounded-lg border border-destructive/30 bg-destructive/10 px-3 py-2.5 text-xs text-destructive">
      <AlertTriangle className="mt-0.5 size-4 shrink-0" />
      <ul className="space-y-0.5">
        {list.map((m, i) => <li key={i}>{m}</li>)}
      </ul>
    </div>
  );
}
