import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { Plus, Upload, ShieldCheck, Pencil, Trash2, AlertTriangle } from 'lucide-react';
import { cn } from '@/lib/utils';
import { api, type RedactionConfig, type RedactionCustomRule } from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import { Badge, Button, Switch } from '@/components/mds';
import { ConfirmDialog } from '@/components/settings/controls';
import { categoryLabel } from './RedactionTab';
import { RedactionMyRuleWizard } from './RedactionMyRuleWizard';
import { RedactionImportProfileDialog } from './RedactionImportProfileDialog';
import { mergeCategoryChips, myRuleMatchMode } from './redactionMyRules';

// ── "我的規則" section (WP-W, canvas screens 2-9) ───────────────────────
//
// Rendered as a section INSIDE RedactionTab's single settings Card, directly
// below "偵測規則集" — matches `RedactionFieldRulesCard`'s pattern, not a
// standalone mds <Card>. Owns its own fetch (`redaction.custom_rules.list`)
// because a custom rule's content is NOT part of `redaction.get`'s payload
// (only the derived `category_labels` and `available_profiles` entry are);
// every mutation reloads that list AND calls the parent's `onReload` so the
// "我的規則" profile checkbox above updates the moment the first rule is
// saved (the backend auto-adds it to `config.profiles`, §13.1).

type Panel = { mode: 'create' } | { mode: 'edit'; rule: RedactionCustomRule };

export function RedactionMyRulesCard({
  config,
  selectedCategories,
  onReload,
}: {
  config: RedactionConfig;
  /** Union of categories covered by the currently-enabled profiles — same
   *  value RedactionTab already computes for the sources section. */
  selectedCategories: string[];
  onReload: () => Promise<RedactionConfig | null>;
}) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);

  const [rules, setRules] = useState<RedactionCustomRule[] | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [panel, setPanel] = useState<Panel | null>(null);
  const [importOpen, setImportOpen] = useState(false);
  const [togglingId, setTogglingId] = useState<string | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<string | null>(null);
  const [deleting, setDeleting] = useState(false);

  const load = async () => {
    setLoadError(null);
    try {
      const res = await api.redaction.customRules.list();
      setRules(res.rules);
    } catch (e) {
      setLoadError(formatError(e));
    }
  };
  useEffect(() => { void load(); }, []);

  const existingCategories = mergeCategoryChips(selectedCategories, config.category_labels);

  const handleToggle = async (rule: RedactionCustomRule, enabled: boolean) => {
    setTogglingId(rule.id);
    try {
      const result = await api.redaction.customRules.setEnabled(rule.id, enabled);
      await load();
      // Written to custom.toml but not applied live (e.g. the rule set no
      // longer compiles) — say so instead of a silent, misleading success.
      if (result.warning) toast.error(result.warning);
    } catch (e) {
      toast.error(t('toast.error.saveFailed', { message: formatError(e) }));
    } finally {
      setTogglingId(null);
    }
  };

  const confirmDelete = async () => {
    if (!deleteTarget) return;
    setDeleting(true);
    try {
      const result = await api.redaction.customRules.remove(deleteTarget);
      setDeleteTarget(null);
      await load();
      await onReload();
      if (result.warning) toast.error(result.warning);
      else toast.success(t('redaction.myRules.deleted'));
    } catch (e) {
      toast.error(t('toast.error.saveFailed', { message: formatError(e) }));
    } finally {
      setDeleting(false);
    }
  };

  const handleWizardSaved = async () => {
    setPanel(null);
    await load();
    await onReload();
    toast.success(t('redaction.myRules.saved'));
  };

  const handleImported = async () => {
    setImportOpen(false);
    await onReload();
  };

  return (
    <div className="border-t border-surface-border pt-4">
      <div className="mb-3 flex flex-wrap items-center justify-between gap-2">
        <div>
          <h4 className="flex items-center gap-1.5 text-xs font-semibold uppercase text-muted-foreground">
            <ShieldCheck className="h-3.5 w-3.5" />
            {t('redaction.myRules.title')}
            {rules && rules.length > 0 && <Badge variant="secondary">{t('redaction.myRules.count', { count: rules.length })}</Badge>}
          </h4>
          <p className="mt-1 text-xs text-muted-foreground">{t('redaction.myRules.desc')}</p>
        </div>
        {panel === null && rules !== null && rules.length > 0 && (
          <div className="flex shrink-0 items-center gap-2">
            <Button variant="outline" size="sm" onClick={() => setImportOpen(true)}>
              <Upload />{t('redaction.myRules.importButton')}
            </Button>
            <Button variant="brand" size="sm" onClick={() => setPanel({ mode: 'create' })}>
              <Plus />{t('redaction.myRules.add')}
            </Button>
          </div>
        )}
      </div>

      {panel !== null && (
        <div className="mb-3">
          {/* `key` forces a fresh mount per target (create vs. each rule's
              own id) — RedactionMyRuleWizard's step-2 example/counter
              textareas keep their own raw-text local state (see that file's
              header comment), which only stays correct if switching targets
              always remounts rather than reusing the same instance. */}
          <RedactionMyRuleWizard
            key={panel.mode === 'edit' ? panel.rule.id : 'create'}
            target={panel}
            existingCategories={existingCategories}
            categoryLabels={config.category_labels}
            onCancel={() => setPanel(null)}
            onSaved={() => void handleWizardSaved()}
          />
        </div>
      )}

      {panel === null && rules === null && (
        loadError ? (
          <div className="flex items-center gap-2 rounded-lg border border-destructive/30 bg-destructive/10 px-3 py-2.5 text-xs text-destructive">
            <AlertTriangle className="h-4 w-4 shrink-0" />
            <span>{t('redaction.myRules.loadFailed', { message: loadError })}</span>
          </div>
        ) : (
          <div className="space-y-2">
            {[0, 1].map((i) => <div key={i} className="h-14 animate-pulse rounded-lg bg-muted/50" />)}
          </div>
        )
      )}

      {panel === null && rules !== null && rules.length === 0 && (
        <div className="rounded-lg bg-muted/50 px-3 py-6 text-center">
          <p className="mx-auto max-w-md text-xs text-muted-foreground">{t('redaction.myRules.empty.desc')}</p>
          <div className="mt-3 flex items-center justify-center gap-2">
            <Button variant="brand" size="sm" onClick={() => setPanel({ mode: 'create' })}>
              <Plus />{t('redaction.myRules.add')}
            </Button>
            <Button variant="outline" size="sm" onClick={() => setImportOpen(true)}>
              <Upload />{t('redaction.myRules.importButton')}
            </Button>
          </div>
        </div>
      )}

      {panel === null && rules !== null && rules.length > 0 && (
        <div className="space-y-2">
          {rules.map((rule) => {
            const mode = myRuleMatchMode(rule);
            return (
              <div key={rule.id} className={cn('rounded-lg border border-surface-border bg-card p-3', !rule.enabled && 'opacity-60')}>
                <div className="flex flex-wrap items-center justify-between gap-2">
                  <div className="flex min-w-0 flex-wrap items-center gap-2">
                    <Badge className="bg-brand/10 text-brand">{categoryLabel(intl, rule.category, config.category_labels)}</Badge>
                    <span className="text-xs text-muted-foreground">
                      {mode.mode === 'keyword' ? t('redaction.myRules.row.keywordCount', { count: mode.count }) : t('redaction.myRules.row.pattern')}
                    </span>
                    <code className="max-w-64 truncate rounded bg-muted px-1.5 py-0.5 font-mono text-xs text-foreground">{rule.example}</code>
                    {!rule.enabled && <Badge variant="outline">{t('redaction.myRules.row.disabled')}</Badge>}
                  </div>
                  <div className="flex shrink-0 items-center gap-1">
                    <Switch
                      checked={rule.enabled}
                      onCheckedChange={(v) => void handleToggle(rule, Boolean(v))}
                      disabled={togglingId === rule.id}
                      aria-label={t('redaction.myRules.row.enableToggle', { label: rule.label })}
                    />
                    <Button variant="ghost" size="xs" onClick={() => setPanel({ mode: 'edit', rule })}>
                      <Pencil />{t('common.edit')}
                    </Button>
                    <Button variant="ghost" size="xs" className="text-destructive hover:bg-destructive/10" onClick={() => setDeleteTarget(rule.id)}>
                      <Trash2 />{t('common.delete')}
                    </Button>
                  </div>
                </div>
              </div>
            );
          })}
        </div>
      )}

      <ConfirmDialog
        open={deleteTarget !== null}
        onClose={() => setDeleteTarget(null)}
        onConfirm={() => void confirmDelete()}
        busy={deleting}
        title={t('redaction.myRules.deleteConfirm.title')}
        message={t('redaction.myRules.deleteConfirm.message') as string}
      />

      <RedactionImportProfileDialog
        open={importOpen}
        onClose={() => setImportOpen(false)}
        onImported={() => void handleImported()}
        categoryLabels={config.category_labels}
      />
    </div>
  );
}
