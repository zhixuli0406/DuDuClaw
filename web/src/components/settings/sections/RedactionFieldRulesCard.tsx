import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { api, type RedactionConfig, type RedactionFieldRule, type RedactionDataSource, type DbSourceSummary } from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import { Badge, Button } from '@/components/mds';
import { ConfirmDialog } from '@/components/settings/controls';
import { Plus, Trash2, Play, ShieldCheck, X } from 'lucide-react';
import { categoryLabel } from './RedactionTab';
import { FieldRuleFormPanel } from './RedactionFieldRuleForm';
import { DryRunPanel, type DryRunState } from './RedactionFieldRuleDryRun';
import {
  emptyFieldRuleForm,
  fieldRuleToFormState,
  formStateToRuleInput,
  validateFieldRuleForm,
  summarizeFieldRuleCoverage,
  dryRunDefaultsForRule,
  kvRowsToRecord,
  recordToKvRows,
  type FieldRuleFormState,
} from './redactionFieldRules';
import { filterFieldRulesBySource } from './redactionSystems';

// ── Redaction "資料表欄位規則" section (WP-W, canvas screens 1-6) ─────────
//
// Rendered as a section INSIDE RedactionTab's single settings Card, directly
// under "外部系統" — matches the canvas (`.sec` inside one `.card`), not a
// standalone mds <Card>. Each row action (edit / dry-run / delete) and the
// inline form's own "儲存並套用" button call `redaction.update` immediately
// (field_rules is an upsert-merge map, independent of the tab's big batched
// Save button) and then reload the parent's config via `onReload`.
//
// Sub-components live in sibling files to keep this orchestrator under the
// house style's ~800-line file cap: `RedactionFieldRuleForm.tsx` (the inline
// add/edit panel), `RedactionFieldRuleDryRun.tsx` (the dry-run panel),
// `RedactionTokenInput.tsx` (the token input + kv-row primitives shared with
// the data-sources card).

type Panel = { type: 'form'; ruleId: string | null } | { type: 'dryrun'; ruleId: string };

/** One coverage line, localized. Data comes from the pure
 *  `summarizeFieldRuleCoverage` helper so the text logic stays testable. */
function CoverageLineText({ line }: { line: ReturnType<typeof summarizeFieldRuleCoverage>[number] }) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);
  switch (line.kind) {
    case 'source':
      return <p>{t('redaction.fieldRules.coverage.source', { source: line.source })}</p>;
    case 'dbField': {
      // §14.3: a free_form_names table can carry CJK / file extensions —
      // `font-mono` (Geist Mono) deliberately ships no CJK fallback
      // (DESIGN.md §1.5), so only apply it to plain-ASCII (identifier-style)
      // table names.
      const ascii = /^[\x00-\x7F]*$/.test(line.table);
      return (
        <p>
          <span className={ascii ? 'font-mono text-foreground' : 'text-foreground'}>{line.table}{line.allFields ? '.*' : ''}</span>
          {' → '}
          {line.allFields
            ? t('redaction.fieldRules.coverage.allFields')
            : t('redaction.fieldRules.coverage.fields', { fields: line.fieldTokens.join('、') })}
        </p>
      );
    }
    case 'jsonPathTool':
      return (
        <p>
          {t('redaction.fieldRules.coverage.tool', { tool: line.tool ?? t('redaction.fieldRules.coverage.anyTool') })}
          {line.matchArgs.length > 0 &&
            ' ' + t('redaction.fieldRules.coverage.matchArgs', {
              args: line.matchArgs.map(([k, v]) => `${k} = ${v}`).join('、'),
            })}
        </p>
      );
    case 'jsonPathPaths':
      return (
        <p>{t('redaction.fieldRules.coverage.paths', { count: line.paths.length, paths: line.paths.join('、') })}</p>
      );
    case 'restoreScope':
      if (line.scope.kind === 'owner') return <p>{t('redaction.fieldRules.coverage.scope.owner')}</p>;
      if (line.scope.kind === 'any_scope') {
        return <p>{t('redaction.fieldRules.coverage.scope.any', { scope: line.scope.scope })}</p>;
      }
      return <p>{t('redaction.fieldRules.coverage.scope.all', { scopes: line.scope.scopes.join('、') })}</p>;
    default:
      return null;
  }
}

function FieldRuleRow({
  rule,
  onEdit,
  onDryRun,
  onDelete,
}: {
  rule: RedactionFieldRule;
  onEdit: () => void;
  onDryRun: () => void;
  onDelete: () => void;
}) {
  const intl = useIntl();
  const lines = summarizeFieldRuleCoverage(rule);
  return (
    <div className="rounded-lg border border-surface-border bg-card p-3">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="flex flex-wrap items-center gap-2">
          <code className="font-mono text-sm font-medium text-foreground">{rule.id}</code>
          <Badge variant={rule.kind === 'db_field' ? 'default' : 'outline'}>
            {intl.formatMessage({ id: `redaction.fieldRules.kind.${rule.kind}` })}
          </Badge>
          <Badge variant="secondary">{categoryLabel(intl, rule.category)}</Badge>
        </div>
        <div className="flex shrink-0 items-center gap-1">
          <Button variant="ghost" size="xs" onClick={onEdit}>
            {intl.formatMessage({ id: 'common.edit' })}
          </Button>
          <Button variant="ghost" size="xs" onClick={onDryRun}>
            <Play />
            {intl.formatMessage({ id: 'redaction.fieldRules.dryRun' })}
          </Button>
          <Button variant="ghost" size="xs" className="text-destructive hover:bg-destructive/10" onClick={onDelete}>
            <Trash2 />
            {intl.formatMessage({ id: 'common.delete' })}
          </Button>
        </div>
      </div>
      <div className="mt-2 space-y-1 text-xs text-muted-foreground">
        {lines.map((line, i) => <CoverageLineText key={i} line={line} />)}
      </div>
    </div>
  );
}

export function RedactionFieldRulesCard({
  fieldRules,
  dataSources,
  onReload,
  sourceFilter = null,
  sourceFilterLabel = null,
  onClearSourceFilter,
}: {
  fieldRules: Array<RedactionFieldRule>;
  dataSources: RedactionDataSource[];
  onReload: () => Promise<RedactionConfig | null>;
  /** Set by the merged systems card's "N 條欄位規則" chip — narrows the list
   *  below to rules whose `connector` resolves to this system name. `null`
   *  (the default) shows every rule, unfiltered. */
  sourceFilter?: string | null;
  /** Display label for the active filter chip — kept separate from the raw
   *  `sourceFilter` name (a wire value like `duduclaw_db`) so the chip never
   *  shows a raw built-in id (DESIGN.md R-PLAIN-WORDS). */
  sourceFilterLabel?: string | null;
  onClearSourceFilter?: () => void;
}) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);

  const [panel, setPanel] = useState<Panel | null>(null);
  const [form, setForm] = useState<FieldRuleFormState>(emptyFieldRuleForm());
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [dryRun, setDryRun] = useState<DryRunState | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<string | null>(null);
  const [deleting, setDeleting] = useState(false);

  // db_sources connections — used to decide whether a rule's "資料來源" gets
  // the free-text+suggestion-chip treatment or the live table/column picker
  // (`api.dbSources.tables`), and to offer a connection picker for the
  // generic `duduclaw_db` source. Best-effort: an empty list on failure just
  // means every source falls back to free text, never an error banner.
  const [dbSources, setDbSources] = useState<DbSourceSummary[]>([]);
  useEffect(() => {
    api.dbSources.list().then((res) => setDbSources(res.sources)).catch(() => {});
  }, []);
  const dbSourceNames = new Set(dbSources.map((s) => s.name));

  const fieldRuleIds = new Set(fieldRules.map((r) => r.id));
  const visibleRules = filterFieldRulesBySource(fieldRules, sourceFilter);

  const openCreate = () => {
    setForm(emptyFieldRuleForm());
    setSaveError(null);
    setPanel({ type: 'form', ruleId: null });
  };
  const openEdit = (rule: RedactionFieldRule) => {
    setForm(fieldRuleToFormState(rule));
    setSaveError(null);
    setPanel({ type: 'form', ruleId: rule.id });
  };
  const openDryRun = (rule: RedactionFieldRule) => {
    const defaults = dryRunDefaultsForRule(rule, dataSources);
    setDryRun({
      tool: defaults.tool,
      args: recordToKvRows(defaults.args),
      sample: '',
      loading: false,
      error: null,
      result: null,
    });
    setPanel({ type: 'dryrun', ruleId: rule.id });
  };
  const closePanel = () => { setPanel(null); setSaveError(null); setDryRun(null); };

  const handleSaveRule = async () => {
    if (!panel || panel.type !== 'form') return;
    const isNew = panel.ruleId === null;
    const errors = validateFieldRuleForm(form, isNew, dataSources);
    if (Object.keys(errors).length > 0) return;
    const id = isNew ? form.id.trim() : panel.ruleId!;
    setSaving(true);
    setSaveError(null);
    try {
      const body = formStateToRuleInput(form);
      const res = await api.redaction.update({ field_rules: { [id]: body } });
      if (res.warning) {
        setSaveError(res.warning);
        return;
      }
      closePanel();
      const fresh = await onReload();
      toast.success(`${t('redaction.savedLive')} · ${t('redaction.audit.ruleCount', { count: fresh?.field_rules.length ?? fieldRules.length })}`);
    } catch (e) {
      setSaveError(formatError(e));
    } finally {
      setSaving(false);
    }
  };

  const runDryRun = async () => {
    if (!dryRun) return;
    setDryRun({ ...dryRun, loading: true, error: null });
    try {
      const args = kvRowsToRecord(dryRun.args);
      const result = await api.redaction.dryRun(dryRun.sample, dryRun.tool.trim() || 'odoo_search', args);
      setDryRun((d) => (d ? { ...d, loading: false, result } : d));
    } catch (e) {
      setDryRun((d) => (d ? { ...d, loading: false, error: formatError(e) } : d));
    }
  };

  const confirmDelete = async () => {
    if (!deleteTarget) return;
    setDeleting(true);
    try {
      await api.redaction.update({ field_rules: { [deleteTarget]: null } });
      setDeleteTarget(null);
      const fresh = await onReload();
      toast.success(`${t('redaction.savedLive')} · ${t('redaction.audit.ruleCount', { count: fresh?.field_rules.length ?? 0 })}`);
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.saveFailed' }, { message: formatError(e) }));
    } finally {
      setDeleting(false);
    }
  };

  return (
    <div className="border-t border-surface-border pt-4">
      <h4 className="mb-1 flex items-center gap-1.5 text-xs font-semibold uppercase text-muted-foreground">
        <ShieldCheck className="h-3.5 w-3.5" />
        {t('redaction.fieldRules.title')}
      </h4>
      <p className="mb-3 text-xs text-muted-foreground">{t('redaction.fieldRules.desc')}</p>

      {sourceFilter && (
        <div className="mb-3 flex items-center gap-1.5">
          <Badge className="bg-brand/10 text-brand">
            {t('redaction.fieldRules.filter.active', { label: sourceFilterLabel ?? sourceFilter })}
          </Badge>
          <button
            type="button"
            onClick={onClearSourceFilter}
            aria-label={t('redaction.fieldRules.filter.clear')}
            className="rounded p-0.5 text-muted-foreground hover:bg-muted hover:text-foreground"
          >
            <X className="size-3.5" />
          </button>
        </div>
      )}

      {panel === null && (
        <div className="mb-3">
          <Button variant="brandSubtle" size="sm" onClick={openCreate}>
            <Plus />{t('redaction.fieldRules.add')}
          </Button>
        </div>
      )}

      {panel?.type === 'form' && panel.ruleId === null && (
        <div className="mb-3">
          <FieldRuleFormPanel
            isNew
            form={form}
            onChange={setForm}
            onCancel={closePanel}
            onSave={() => void handleSaveRule()}
            saving={saving}
            saveError={saveError}
            dataSources={dataSources}
            dbSourceNames={dbSourceNames}
            dbSources={dbSources}
          />
        </div>
      )}

      {visibleRules.length === 0 ? (
        <div className="rounded-lg bg-muted/50 px-3 py-4 text-center">
          <p className="text-xs text-muted-foreground">{t('redaction.fieldRules.empty.title')}</p>
          <p className="mx-auto mt-1 max-w-md text-xs text-muted-foreground">{t('redaction.fieldRules.empty.desc')}</p>
          {panel === null && (
            <Button variant="brandSubtle" size="sm" className="mt-3" onClick={openCreate}>
              <Plus />{t('redaction.fieldRules.addFirst')}
            </Button>
          )}
        </div>
      ) : (
        <div className="space-y-2">
          {visibleRules.map((rule) => (
            <div key={rule.id}>
              {panel?.type === 'form' && panel.ruleId === rule.id ? (
                <FieldRuleFormPanel
                  isNew={false}
                  form={form}
                  onChange={setForm}
                  onCancel={closePanel}
                  onSave={() => void handleSaveRule()}
                  saving={saving}
                  saveError={saveError}
                  dataSources={dataSources}
                  dbSourceNames={dbSourceNames}
                  dbSources={dbSources}
                />
              ) : (
                <FieldRuleRow
                  rule={rule}
                  onEdit={() => openEdit(rule)}
                  onDryRun={() => openDryRun(rule)}
                  onDelete={() => setDeleteTarget(rule.id)}
                />
              )}
              {panel?.type === 'dryrun' && panel.ruleId === rule.id && dryRun && (
                <div className="mt-2">
                  <DryRunPanel
                    rule={rule}
                    state={dryRun}
                    onChange={setDryRun}
                    onRun={() => void runDryRun()}
                    onClose={closePanel}
                    fieldRuleIds={fieldRuleIds}
                  />
                </div>
              )}
            </div>
          ))}
        </div>
      )}

      <ConfirmDialog
        open={deleteTarget !== null}
        onClose={() => setDeleteTarget(null)}
        onConfirm={() => void confirmDelete()}
        busy={deleting}
        title={t('redaction.fieldRules.deleteConfirm.title')}
        message={t('redaction.fieldRules.deleteConfirm.message', { id: deleteTarget ?? '' }) as string}
      />
    </div>
  );
}
