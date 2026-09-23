import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { Plus, Trash2, AlertTriangle, Loader2 } from 'lucide-react';
import { api, type RedactionRestoreScope, type RedactionDataSource, type DbSourceTable, type DbSourceSummary } from '@/lib/api';
import {
  Button,
  Input,
  Select,
  SelectTrigger,
  SelectValue,
  SelectContent,
  SelectItem,
  Segmented,
} from '@/components/mds';
import { FieldBlock } from '@/pages/agent-form/form-rows';
import { RedactionTokenInput, KvRowEditor } from './RedactionTokenInput';
import { categoryLabel } from './RedactionTab';
import {
  validateFieldRuleForm,
  isValidFieldToken,
  isValidFreeFormFieldToken,
  isFreeFormSource,
  addKvRow,
  updateKvRow,
  removeKvRow,
  ODOO_TABLE_SUGGESTIONS,
  type FieldRuleFormState,
  type FieldRuleMode,
} from './redactionFieldRules';

// ── Field-rule inline form panel (canvas screens 3-5) ───────────────────
//
// Shared by "new rule" and "edit rule" (RedactionFieldRulesCard swaps in the
// same panel either above the list or in place of the row being edited).
// Mode switch (資料表欄位 / JSON 路徑) preserves id/category/restoreScope/
// priority across a switch — only the mode-specific fields reset.

const KNOWN_FIELD_RULE_CATEGORIES = [
  'DB_FIELD',
  'CRM_NOTE',
  'TW_ID',
  'TW_MOBILE',
  'TW_LANDLINE',
  'TW_NHI',
  'TW_BIZ_ID',
  'EMAIL',
  'CREDIT_CARD',
  'IBAN',
  'SWIFT_BIC',
  'IPV4',
  'JWT',
  'PEM_PRIVATE_KEY',
  'AWS_ACCESS_KEY',
  'GITHUB_TOKEN',
  'GITLAB_PAT',
  'SLACK_TOKEN',
  'ANTHROPIC_KEY',
  'OPENAI_KEY',
] as const;

function CategoryPicker({ value, onChange }: { value: string; onChange: (v: string) => void }) {
  const intl = useIntl();
  const known = (KNOWN_FIELD_RULE_CATEGORIES as readonly string[]).includes(value);
  return (
    <div className="space-y-1.5">
      <Select
        value={known ? value : '__custom__'}
        onValueChange={(v) => onChange(v === null || v === '__custom__' ? '' : v)}
      >
        <SelectTrigger className="w-full">
          <SelectValue>
            {known ? categoryLabel(intl, value) : intl.formatMessage({ id: 'redaction.profile.custom' })}
          </SelectValue>
        </SelectTrigger>
        <SelectContent>
          {KNOWN_FIELD_RULE_CATEGORIES.map((c) => (
            <SelectItem key={c} value={c}>{categoryLabel(intl, c)}</SelectItem>
          ))}
          <SelectItem value="__custom__">{intl.formatMessage({ id: 'redaction.profile.custom' })}</SelectItem>
        </SelectContent>
      </Select>
      {!known && (
        <Input
          value={value}
          onChange={(e) => onChange(e.target.value.toUpperCase().replace(/[^A-Z0-9_]/g, ''))}
          placeholder="DB_FIELD"
          className="font-mono"
        />
      )}
    </div>
  );
}

function RestoreScopePicker({
  value,
  onChange,
}: {
  value: RedactionRestoreScope;
  onChange: (v: RedactionRestoreScope) => void;
}) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);
  return (
    <div className="space-y-1.5">
      <Select
        value={value.kind}
        onValueChange={(k) => {
          if (k === null) return;
          if (k === 'owner') onChange({ kind: 'owner' });
          else if (k === 'any_scope') onChange({ kind: 'any_scope', scope: value.kind === 'any_scope' ? value.scope : '' });
          else onChange({ kind: 'all_scopes', scopes: value.kind === 'all_scopes' ? value.scopes : [] });
        }}
      >
        <SelectTrigger className="w-full">
          <SelectValue>{t(`redaction.fieldRules.restoreScope.${value.kind}`)}</SelectValue>
        </SelectTrigger>
        <SelectContent>
          <SelectItem value="owner">{t('redaction.fieldRules.restoreScope.owner')}</SelectItem>
          <SelectItem value="any_scope">{t('redaction.fieldRules.restoreScope.any_scope')}</SelectItem>
          <SelectItem value="all_scopes">{t('redaction.fieldRules.restoreScope.all_scopes')}</SelectItem>
        </SelectContent>
      </Select>
      {value.kind === 'any_scope' && (
        <Input
          value={value.scope}
          onChange={(e) => onChange({ kind: 'any_scope', scope: e.target.value })}
          placeholder={t('redaction.fieldRules.restoreScope.scopePlaceholder')}
          className="font-mono"
        />
      )}
      {value.kind === 'all_scopes' && (
        <RedactionTokenInput
          tokens={value.scopes}
          onChange={(scopes) => onChange({ kind: 'all_scopes', scopes })}
          placeholder={t('redaction.fieldRules.restoreScope.scopesPlaceholder')}
        />
      )}
    </div>
  );
}

export function FieldRuleFormPanel({
  isNew,
  form,
  onChange,
  onCancel,
  onSave,
  saving,
  saveError,
  dataSources,
  dbSourceNames,
  dbSources,
}: {
  isNew: boolean;
  form: FieldRuleFormState;
  onChange: (next: FieldRuleFormState) => void;
  onCancel: () => void;
  onSave: () => void;
  saving: boolean;
  saveError: string | null;
  dataSources: RedactionDataSource[];
  dbSourceNames: Set<string>;
  /** Full `db_sources` connection list — beyond just the opt-in same-named-
   *  registry-entry pairing `dbSourceNames` already covers, this also lets
   *  the generic `duduclaw_db` source (every plain DB connection resolves
   *  through it — §15.1) offer a per-connection table/column picker without
   *  needing its own registry entry. */
  dbSources: DbSourceSummary[];
}) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);
  const freeForm = isFreeFormSource(form.source, dataSources);
  const errors = validateFieldRuleForm(form, isNew, dataSources);
  const [attempted, setAttempted] = useState(false);

  // The generic `duduclaw_db` source has no fixed connection of its own —
  // every plain database resolves through it (§15.1) — so which connection's
  // tables to suggest is a purely local, non-persisted picker choice.
  const isGenericDbSource = form.source === 'duduclaw_db';
  const [genericDbPick, setGenericDbPick] = useState('');
  useEffect(() => {
    // Zero-click convenience: with exactly one connection configured, there's
    // nothing to actually pick.
    if (isGenericDbSource && !genericDbPick && dbSources.length === 1) setGenericDbPick(dbSources[0].name);
  }, [isGenericDbSource, dbSources, genericDbPick]);

  const namedDbConnection = dbSources.find((d) => d.name === form.source);
  const activeDbConnectionName = namedDbConnection ? namedDbConnection.name : isGenericDbSource ? genericDbPick : '';
  const sourceHasDb = activeDbConnectionName !== '' && dbSourceNames.has(activeDbConnectionName);
  const [tables, setTables] = useState<DbSourceTable[] | null>(null);
  const [tablesLoading, setTablesLoading] = useState(false);

  useEffect(() => {
    if (form.mode !== 'db_field' || !sourceHasDb) {
      setTables(null);
      return;
    }
    let cancelled = false;
    setTablesLoading(true);
    api.dbSources
      .tables(activeDbConnectionName)
      .then((res) => { if (!cancelled) setTables(res.tables); })
      .catch(() => { if (!cancelled) setTables([]); })
      .finally(() => { if (!cancelled) setTablesLoading(false); });
    return () => { cancelled = true; };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [form.mode, activeDbConnectionName, sourceHasDb]);

  const selectedTableColumns = tables?.find((tb) => tb.name === form.table)?.columns ?? [];

  // Display names only — the wire value for a plain DB connection is always
  // the generic `duduclaw_db` (§15.1); never fall back to a raw built-in id
  // even if `dataSources` hasn't loaded yet.
  const builtinLabel = (name: string, fallback: string) => dataSources.find((s) => s.name === name)?.label || fallback;
  const sourceOptions = [
    { value: 'odoo', label: builtinLabel('odoo', t('redaction.systems.odoo.name') as string) },
    { value: 'duduclaw_db', label: builtinLabel('duduclaw_db', t('redaction.fieldRules.form.source.genericDb') as string) },
    { value: 'duduclaw_files', label: builtinLabel('duduclaw_files', t('redaction.systems.files.name') as string) },
    ...dataSources.filter((s) => !s.builtin).map((s) => ({ value: s.name, label: s.label })),
  ];

  const handleSave = () => {
    setAttempted(true);
    if (Object.keys(errors).length > 0) return;
    onSave();
  };

  return (
    <div className="space-y-3.5 rounded-xl border border-brand/40 bg-card p-4">
      <div className="flex items-center justify-between gap-3">
        <h5 className="text-sm font-medium text-foreground">
          {t(isNew ? 'redaction.fieldRules.form.newTitle' : 'redaction.fieldRules.form.editTitle')}
        </h5>
        <Segmented<FieldRuleMode>
          value={form.mode}
          onValueChange={(mode) => onChange({ ...form, mode })}
          options={[
            { value: 'db_field', label: t('redaction.fieldRules.kind.db_field') },
            { value: 'json_path', label: t('redaction.fieldRules.kind.json_path') },
          ]}
        />
      </div>

      <div className="grid gap-3.5 sm:grid-cols-2">
        <FieldBlock label={t('redaction.fieldRules.form.id.label')} description={t('redaction.fieldRules.form.id.hint')}>
          <Input
            value={form.id}
            onChange={(e) => onChange({ ...form, id: e.target.value.trim().toLowerCase() })}
            disabled={!isNew}
            aria-invalid={attempted && !!errors.id}
            className="font-mono"
            placeholder="customer_master"
          />
          {attempted && errors.id && <p className="text-xs text-destructive">{t('redaction.fieldRules.form.error.id')}</p>}
        </FieldBlock>
        <FieldBlock label={t('redaction.fieldRules.form.category.label')} description={t('redaction.fieldRules.form.category.hint')}>
          <CategoryPicker value={form.category} onChange={(category) => onChange({ ...form, category })} />
        </FieldBlock>
      </div>

      {form.mode === 'db_field' ? (
        <>
          <div className="grid gap-3.5 sm:grid-cols-2">
            <FieldBlock label={t('redaction.fieldRules.form.source.label')} description={t('redaction.fieldRules.form.source.hint')}>
              <Select value={form.source} onValueChange={(source) => onChange({ ...form, source: source ?? 'odoo', table: '' })}>
                <SelectTrigger className="w-full"><SelectValue>{sourceOptions.find((o) => o.value === form.source)?.label ?? form.source}</SelectValue></SelectTrigger>
                <SelectContent>
                  {sourceOptions.map((o) => <SelectItem key={o.value} value={o.value}>{o.label}</SelectItem>)}
                </SelectContent>
              </Select>
            </FieldBlock>
          </div>

          <FieldBlock label={t('redaction.fieldRules.form.table.label')}>
            {isGenericDbSource && dbSources.length > 1 && (
              <div className="mb-1.5">
                <Select value={genericDbPick} onValueChange={(v) => setGenericDbPick(v ?? '')}>
                  <SelectTrigger size="sm" className="w-full">
                    <SelectValue>{dbSources.find((d) => d.name === genericDbPick)?.label ?? t('redaction.fieldRules.form.table.pickConnection')}</SelectValue>
                  </SelectTrigger>
                  <SelectContent>
                    {dbSources.map((d) => <SelectItem key={d.name} value={d.name}>{d.label}</SelectItem>)}
                  </SelectContent>
                </Select>
              </div>
            )}
            {sourceHasDb ? (
              <>
                <Select value={form.table} onValueChange={(table) => onChange({ ...form, table: table ?? '' })}>
                  <SelectTrigger className="w-full">
                    <SelectValue>
                      {tablesLoading ? <Loader2 className="size-3.5 animate-spin" /> : form.table || t('redaction.fieldRules.form.table.label')}
                    </SelectValue>
                  </SelectTrigger>
                  <SelectContent>
                    {(tables ?? []).map((tb) => <SelectItem key={tb.name} value={tb.name}>{tb.name}</SelectItem>)}
                  </SelectContent>
                </Select>
                <p className="mt-1 text-xs text-muted-foreground">{t('redaction.fieldRules.form.table.pickerHint')}</p>
                {selectedTableColumns.length > 0 && (
                  <div className="mt-2 flex flex-wrap gap-1.5">
                    {selectedTableColumns.map((col) => (
                      <button
                        key={col.name}
                        type="button"
                        onClick={() => onChange({ ...form, fieldTokens: form.fieldTokens.includes(col.name) ? form.fieldTokens : [...form.fieldTokens, col.name] })}
                        className="rounded-full border border-input px-2.5 py-1 font-mono text-xs text-muted-foreground hover:bg-muted"
                      >
                        {col.name}
                      </button>
                    ))}
                  </div>
                )}
              </>
            ) : (
              <>
                <Input
                  value={form.table}
                  onChange={(e) => onChange({ ...form, table: e.target.value.trim() })}
                  aria-invalid={attempted && !!errors.table}
                  className={freeForm ? undefined : 'font-mono'}
                  placeholder={freeForm ? 'customers.csv' : 'res.partner'}
                />
                {form.source === 'odoo' && (
                  <div className="mt-1.5 flex flex-wrap gap-1.5">
                    {ODOO_TABLE_SUGGESTIONS.map((s) => (
                      <button
                        key={s.value}
                        type="button"
                        onClick={() => onChange({ ...form, table: s.value })}
                        className="rounded-full border border-input px-2.5 py-1 text-xs text-muted-foreground hover:bg-muted"
                      >
                        {intl.formatMessage({ id: s.labelId })}
                      </button>
                    ))}
                  </div>
                )}
                <p className="mt-1 text-xs text-muted-foreground">
                  {t(form.source === 'duduclaw_files' ? 'redaction.fieldRules.form.table.filesHint' : 'redaction.fieldRules.form.table.hint')}
                </p>
              </>
            )}
            {attempted && errors.table && <p className="mt-1 text-xs text-destructive">{t('redaction.fieldRules.form.error.table')}</p>}
          </FieldBlock>

          <FieldBlock label={t('redaction.fieldRules.form.fields.label')}>
            <RedactionTokenInput
              tokens={form.fieldTokens}
              onChange={(fieldTokens) => onChange({ ...form, fieldTokens })}
              placeholder={t('redaction.fieldRules.form.fields.placeholder')}
              invalid={attempted && !!errors.fields}
              isTokenValid={freeForm ? isValidFreeFormFieldToken : isValidFieldToken}
              mono={!freeForm}
            />
            {form.fieldTokens.includes('*') && (
              <p className="mt-1 text-xs text-warning">{t('redaction.fieldRules.form.fields.wildcardHint')}</p>
            )}
            {attempted && errors.fields && (
              <p className="mt-1 text-xs text-destructive">
                {t(errors.fields === 'empty' ? 'redaction.fieldRules.form.error.fields.empty' : 'redaction.fieldRules.form.error.fields.invalid')}
              </p>
            )}
          </FieldBlock>
        </>
      ) : (
        <>
          <div className="grid gap-3.5 sm:grid-cols-2">
            <FieldBlock label={t('redaction.fieldRules.form.matchTool.label')} description={t('redaction.fieldRules.form.matchTool.hint')}>
              <Input
                value={form.matchTool}
                onChange={(e) => onChange({ ...form, matchTool: e.target.value.trim() })}
                className="font-mono"
                placeholder="odoo_*"
              />
            </FieldBlock>
            <FieldBlock label={t('redaction.fieldRules.form.matchArgs.label')}>
              <div className="space-y-1.5">
                {form.matchArgs.map((row, i) => (
                  <KvRowEditor
                    key={i}
                    row={row}
                    onChange={(patch) => onChange({ ...form, matchArgs: updateKvRow(form.matchArgs, i, patch) })}
                    onRemove={() => onChange({ ...form, matchArgs: removeKvRow(form.matchArgs, i) })}
                    keyPlaceholder={t('redaction.fieldRules.form.matchArgs.keyPlaceholder')}
                    valuePlaceholder={t('redaction.fieldRules.form.matchArgs.valuePlaceholder')}
                  />
                ))}
                <button
                  type="button"
                  onClick={() => onChange({ ...form, matchArgs: addKvRow(form.matchArgs) })}
                  className="inline-flex items-center gap-1 text-xs text-brand hover:underline"
                >
                  <Plus className="size-3" />{t('redaction.fieldRules.form.matchArgs.add')}
                </button>
              </div>
            </FieldBlock>
          </div>

          <FieldBlock label={t('redaction.fieldRules.form.paths.label')}>
            <div className="space-y-1.5">
              {form.paths.map((p, i) => (
                <div key={i} className="flex items-center gap-1.5">
                  <Input
                    value={p}
                    onChange={(e) => onChange({ ...form, paths: form.paths.map((pp, ii) => (ii === i ? e.target.value : pp)) })}
                    className="font-mono"
                    placeholder={t('redaction.fieldRules.form.paths.placeholder')}
                    aria-invalid={attempted && !!errors.paths}
                  />
                  <Button variant="ghost" size="icon-xs" onClick={() => onChange({ ...form, paths: form.paths.filter((_, ii) => ii !== i) })}>
                    <Trash2 />
                  </Button>
                </div>
              ))}
              <button
                type="button"
                onClick={() => onChange({ ...form, paths: [...form.paths, ''] })}
                className="inline-flex items-center gap-1 text-xs text-brand hover:underline"
              >
                <Plus className="size-3" />{t('redaction.fieldRules.form.paths.add')}
              </button>
              <p className="whitespace-pre-wrap rounded-md bg-muted px-2.5 py-2 font-mono text-xs text-muted-foreground">
                {t('redaction.fieldRules.form.paths.grammarHint')}
              </p>
              {attempted && errors.paths && (
                <p className="text-xs text-destructive">
                  {t(errors.paths === 'empty' ? 'redaction.fieldRules.form.error.paths.empty' : 'redaction.fieldRules.form.error.paths.invalid')}
                </p>
              )}
            </div>
          </FieldBlock>

          <FieldBlock label={t('redaction.fieldRules.form.excludeKeys.label')}>
            <RedactionTokenInput
              tokens={form.excludeKeys}
              onChange={(excludeKeys) => onChange({ ...form, excludeKeys })}
              placeholder={t('redaction.fieldRules.form.excludeKeys.placeholder')}
            />
          </FieldBlock>
        </>
      )}

      <div className="grid gap-3.5 sm:grid-cols-2">
        <FieldBlock label={t('redaction.fieldRules.form.restoreScope.label')}>
          <RestoreScopePicker value={form.restoreScope} onChange={(restoreScope) => onChange({ ...form, restoreScope })} />
        </FieldBlock>
        <FieldBlock label={t('redaction.fieldRules.form.priority.label')} description={t('redaction.fieldRules.form.priority.hint')}>
          <Input
            type="number"
            value={form.priority}
            onChange={(e) => onChange({ ...form, priority: Number(e.target.value) })}
          />
        </FieldBlock>
      </div>

      {saveError && (
        <div className="flex gap-2.5 rounded-lg border border-destructive/30 bg-destructive/10 px-3 py-2.5 text-xs">
          <AlertTriangle className="mt-0.5 size-4 shrink-0 text-destructive" />
          <div className="space-y-0.5">
            <p className="font-medium text-foreground">{t('redaction.fieldRules.form.saveFailed.title')}</p>
            <p className="font-mono text-muted-foreground">{saveError}</p>
          </div>
        </div>
      )}

      <div className="flex items-center justify-end gap-2">
        <p className="mr-auto text-xs text-muted-foreground">{t('redaction.fieldRules.form.saveHint')}</p>
        <Button variant="outline" size="sm" onClick={onCancel}>{intl.formatMessage({ id: 'common.cancel' })}</Button>
        <Button variant="brand" size="sm" onClick={handleSave} disabled={saving}>
          {saving ? intl.formatMessage({ id: 'common.saving' }) : t('redaction.fieldRules.form.save')}
        </Button>
      </div>
    </div>
  );
}
