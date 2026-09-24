import { useEffect, useState, type ReactNode } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate } from 'react-router';
import { Database, Plus, ChevronDown, AlertTriangle, X } from 'lucide-react';
import { cn } from '@/lib/utils';
import {
  api,
  type RedactionConfig,
  type RedactionDataSource,
  type RedactionFieldRule,
  type RedactionEgressRule,
  type RedactionRestoreArgs,
  type OdooStatus,
  type DbSourceSummary,
  type DbSourceLoadError,
  type DbSourceGrantsListResult,
} from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import {
  Badge,
  Button,
  Select,
  SelectTrigger,
  SelectValue,
  SelectContent,
  SelectItem,
  Checkbox,
  CrossLink,
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from '@/components/mds';
import { ConfirmDialog } from '@/components/settings/controls';
import { describeCustomSourceReadIn, buildSystemRows, buildEgressPatch, unattributedEgressEntries, type SystemRow } from './redactionSystems';
import { countGrantsForSource, grantedAgentsForSource, buildStaleDbSourceEntries, resolveAgentDisplayName } from './dbSourceGrants';
import { DbSourceAgentPicker } from './DbSourceAgentPicker';
import { RedactionSourceWizard, type WizardTarget, type WizardSourceType } from './RedactionSourceWizard';

const REDACTION_RESTORE: ReadonlyArray<RedactionRestoreArgs> = ['deny', 'restore', 'passthrough'];

/** Two-column "讀進來 / 寫回去" body shared by every row kind. */
function RowBody({ readIn, writeBack }: { readIn: ReactNode; writeBack: ReactNode }) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  return (
    <div className="mt-2.5 grid gap-3 sm:grid-cols-2">
      <div className="space-y-1">
        <p className="text-xs font-semibold text-muted-foreground">{t('redaction.systems.readIn')}</p>
        <div className="text-xs text-muted-foreground">{readIn}</div>
      </div>
      <div className="space-y-1">
        <p className="text-xs font-semibold text-muted-foreground">{t('redaction.systems.writeBack')}</p>
        <div className="text-xs text-muted-foreground">{writeBack}</div>
      </div>
    </div>
  );
}

function NotApplicable({ hint }: { hint?: string }) {
  const intl = useIntl();
  return (
    <div className="space-y-0.5">
      <p className="text-foreground">{intl.formatMessage({ id: 'redaction.systems.notApplicable' })}</p>
      {hint && <p>{hint}</p>}
    </div>
  );
}

/** Restore-policy select + audit-reveal checkbox — the only editable
 *  write-back control (Odoo + custom tool sources). Saves immediately via
 *  `onChange`, matching the field-rules / data-sources cards' own-RPC
 *  pattern rather than the tab's batched Save button. */
function EgressEditor({
  value,
  onChange,
}: {
  value: RedactionEgressRule;
  onChange: (next: RedactionEgressRule) => void;
}) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  return (
    <div className="space-y-1.5">
      <Select value={value.restore_args} onValueChange={(v) => onChange({ ...value, restore_args: (v ?? 'deny') as RedactionRestoreArgs })}>
        <SelectTrigger size="sm" className="w-full sm:w-56"><SelectValue>{t(`redaction.restore.${value.restore_args}`)}</SelectValue></SelectTrigger>
        <SelectContent>
          {REDACTION_RESTORE.map((r) => <SelectItem key={r} value={r}>{t(`redaction.restore.${r}`)}</SelectItem>)}
        </SelectContent>
      </Select>
      <label className="flex items-center gap-1.5 text-xs text-muted-foreground">
        <Checkbox checked={value.audit_reveal} onCheckedChange={(v) => onChange({ ...value, audit_reveal: v === true })} aria-label={t('redaction.auditReveal')} />
        {t('redaction.auditReveal')}
      </label>
    </div>
  );
}

/** Clickable "N 條欄位規則" chip — jumps to the field-rules card, filtered
 *  to this system. A zero count is still shown (plain text, not a link —
 *  nothing to jump to yet). */
function RuleCountChip({ count, onClick }: { count: number; onClick: () => void }) {
  const intl = useIntl();
  const label = intl.formatMessage({ id: 'redaction.systems.ruleCount' }, { count });
  if (count === 0) return <Badge variant="outline">{label}</Badge>;
  return (
    <button type="button" onClick={onClick} className="inline-flex">
      <Badge className="cursor-pointer bg-brand/10 text-brand hover:bg-brand/20">{label}</Badge>
    </button>
  );
}

function CustomSourceReadIn({ source }: { source: RedactionDataSource }) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);
  const lines = describeCustomSourceReadIn(source);
  return (
    <div className="space-y-0.5">
      {lines.map((line, i) => {
        switch (line.kind) {
          case 'tools':
            return <p key={i}>{t('redaction.dataSources.coverage.tools', { tools: line.tools.join('、') })}</p>;
          case 'tableArg':
            return <p key={i}>{t('redaction.dataSources.coverage.tableArg', { arg: line.arg })}</p>;
          case 'tableFixed':
            return <p key={i}>{t('redaction.dataSources.coverage.tableFixed', { table: line.table })}</p>;
          case 'tableResult':
            return <p key={i}>{t('redaction.dataSources.coverage.tableResult', { pointer: line.pointer })}</p>;
          case 'freeFormNames':
            return <p key={i}>{t('redaction.dataSources.coverage.freeFormNames')}</p>;
          default:
            return null;
        }
      })}
      <p className="italic text-muted-foreground/80">{t('redaction.dataSources.proxyNote')}</p>
    </div>
  );
}

export function RedactionSystemsCard({
  toolEgress,
  dataSources,
  fieldRules,
  onReload,
  onJumpToRules,
}: {
  toolEgress: Record<string, RedactionEgressRule>;
  dataSources: RedactionDataSource[];
  fieldRules: RedactionFieldRule[];
  onReload: () => Promise<RedactionConfig | null>;
  onJumpToRules: (source: string, label: string) => void;
}) {
  const intl = useIntl();
  const navigate = useNavigate();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);

  const [odooStatus, setOdooStatus] = useState<OdooStatus | null>(null);
  const [dbSources, setDbSources] = useState<DbSourceSummary[]>([]);
  const [dbSourceErrors, setDbSourceErrors] = useState<DbSourceLoadError[]>([]);
  const [otherOpen, setOtherOpen] = useState(false);
  // Who can use each db source — `null` means "not loaded yet" or "the
  // operator isn't an admin" (`db_sources.grants.list` is admin-only), both
  // of which hide the badges/notice entirely rather than showing an error.
  const [grantsResult, setGrantsResult] = useState<DbSourceGrantsListResult | null>(null);

  const loadOdoo = () => { api.odoo.status().then(setOdooStatus).catch(() => {}); };
  const loadDbSources = () => {
    api.dbSources.list().then((res) => { setDbSources(res.sources); setDbSourceErrors(res.errors); }).catch(() => {});
  };
  const loadGrants = () => {
    api.dbSources.grants.list().then((res) => {
      // Defensive: a malformed/unexpected payload must never masquerade as a
      // loaded result — every consumer below indexes `.sources`/`.agents`/
      // `.stale` directly.
      if (!res || !Array.isArray(res.sources) || !Array.isArray(res.agents)) {
        setGrantsResult(null);
        return;
      }
      setGrantsResult({
        sources: res.sources,
        agents: res.agents,
        stale: res.stale ?? {},
        errors: Array.isArray(res.errors) ? res.errors : [],
      });
    }).catch(() => setGrantsResult(null));
  };
  useEffect(() => { loadOdoo(); loadDbSources(); loadGrants(); }, []);

  const [wizardTarget, setWizardTarget] = useState<WizardTarget | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<{ kind: 'db' | 'custom'; name: string; label: string } | null>(null);
  const [deleting, setDeleting] = useState(false);
  const [grantDialogTarget, setGrantDialogTarget] = useState<DbSourceSummary | null>(null);
  const [grantDialogSelected, setGrantDialogSelected] = useState<string[]>([]);
  const [grantDialogSaving, setGrantDialogSaving] = useState(false);
  const [grantDialogError, setGrantDialogError] = useState<string | null>(null);

  const openGrantDialog = (db: DbSourceSummary) => {
    setGrantDialogTarget(db);
    setGrantDialogSelected(grantedAgentsForSource(grantsResult?.sources ?? [], db.name));
    setGrantDialogError(null);
  };

  const saveGrantDialog = async () => {
    if (!grantDialogTarget) return;
    setGrantDialogSaving(true);
    setGrantDialogError(null);
    try {
      await api.dbSources.grants.set({ name: grantDialogTarget.name, agents: grantDialogSelected });
      toast.success(intl.formatMessage({ id: 'redaction.savedLive' }));
      loadGrants();
      setGrantDialogTarget(null);
    } catch (e) {
      setGrantDialogError(t('redaction.systems.db.grants.saveFailed', { message: formatError(e) }) as string);
    } finally {
      setGrantDialogSaving(false);
    }
  };

  const rows = buildSystemRows(fieldRules, dataSources, dbSources, toolEgress, odooStatus?.connected ?? false);
  const otherEgress = unattributedEgressEntries(toolEgress, dataSources);
  const staleGrantEntries = grantsResult ? buildStaleDbSourceEntries(grantsResult.stale) : [];

  const handleEgressChange = async (keys: string[], next: RedactionEgressRule) => {
    try {
      const res = await api.redaction.update({ tool_egress: buildEgressPatch(keys, next) });
      if (res.warning) { toast.error(res.warning); return; }
      await onReload();
    } catch (e) {
      toast.error(formatError(e));
    }
  };

  const handleWizardSaved = ({ type, name, label }: { type: WizardSourceType; name: string; label: string }) => {
    void onReload();
    if (type === 'db') loadDbSources();
    onJumpToRules(type === 'db' ? 'duduclaw_db' : name, label);
  };

  const confirmDelete = async () => {
    if (!deleteTarget) return;
    setDeleting(true);
    try {
      if (deleteTarget.kind === 'custom') {
        const res = await api.redaction.update({ data_sources: { [deleteTarget.name]: null } });
        if (res.warning) { toast.error(res.warning); return; }
        await onReload();
      } else {
        const removed = await api.dbSources.remove(deleteTarget.name);
        loadDbSources();
        loadGrants(); // removing a source auto-revokes every agent's grant to it
        if (removed.revoke_failed && removed.revoke_failed.length > 0) {
          const names = removed.revoke_failed
            .map((id) => resolveAgentDisplayName(grantsResult?.agents ?? [], id))
            .join('、');
          toast.error(t('redaction.systems.db.grants.revokeFailed', { names }) as string);
        }
      }
      setDeleteTarget(null);
      toast.success(intl.formatMessage({ id: 'redaction.savedLive' }));
    } catch (e) {
      toast.error(formatError(e));
    } finally {
      setDeleting(false);
    }
  };

  const removeOtherEgress = async (key: string) => {
    try {
      await api.redaction.update({ tool_egress: { [key]: null } });
      await onReload();
    } catch (e) {
      toast.error(formatError(e));
    }
  };

  return (
    <div className="border-t border-surface-border pt-4">
      <h4 className="mb-1 flex items-center gap-1.5 text-xs font-semibold uppercase text-muted-foreground">
        <Database className="h-3.5 w-3.5" />
        {t('redaction.systems.title')}
      </h4>
      <p className="mb-3 text-xs text-muted-foreground">{t('redaction.systems.desc')}</p>

      <div className="mb-3">
        <Button variant="brandSubtle" size="sm" onClick={() => setWizardTarget({ mode: 'create' })}>
          <Plus />{t('redaction.dataSources.add')}
        </Button>
      </div>

      {dbSourceErrors.length > 0 && (
        <div className="mb-3 flex gap-2.5 rounded-lg border border-destructive/30 bg-destructive/10 px-3 py-2.5 text-xs text-destructive">
          <AlertTriangle className="mt-0.5 size-4 shrink-0" />
          <div className="space-y-0.5">
            <p className="font-medium">{t('redaction.dataSources.loadErrors.title')}</p>
            <ul className="space-y-0.5">
              {dbSourceErrors.map((e) => <li key={e.name}>{t('redaction.dataSources.loadErrors.item', { name: e.name, message: e.message })}</li>)}
            </ul>
          </div>
        </div>
      )}

      <div className="space-y-2">
        {rows.map((row, i) => (
          <SystemRowView
            key={i}
            row={row}
            grantsResult={grantsResult}
            onEditDb={(db) => setWizardTarget({ mode: 'edit', type: 'db', row: db })}
            onEditCustom={(s) => setWizardTarget({ mode: 'edit', type: 'custom', row: s })}
            onDeleteDb={(db) => setDeleteTarget({ kind: 'db', name: db.name, label: db.label })}
            onDeleteCustom={(s) => setDeleteTarget({ kind: 'custom', name: s.name, label: s.label })}
            onEgressChange={handleEgressChange}
            onJumpToRules={onJumpToRules}
            onGoToIntegrations={() => navigate('/manage/integrations?tab=odoo')}
            onOpenGrants={openGrantDialog}
          />
        ))}
      </div>

      {grantsResult && grantsResult.errors.length > 0 && (
        <p className="mt-2 text-xs text-muted-foreground">
          {t('redaction.systems.db.grants.loadErrors', {
            count: grantsResult.errors.length,
            names: grantsResult.errors.map((e) => e.name).join('、'),
          })}
        </p>
      )}

      {staleGrantEntries.length > 0 && grantsResult && (
        <p className="mt-2 text-xs text-warning">
          {t('redaction.systems.db.grants.staleNotice', { count: staleGrantEntries.length })}
          {'　'}
          {staleGrantEntries
            .map((entry) =>
              t('redaction.systems.db.grants.staleNotice.item', {
                agent: resolveAgentDisplayName(grantsResult.agents, entry.agentId),
                ids: entry.ids.join('、'),
              }),
            )
            .join('；')}
        </p>
      )}

      {otherEgress.length > 0 && (
        <div className="mt-3 rounded-lg border border-surface-border">
          <button
            type="button"
            onClick={() => setOtherOpen((v) => !v)}
            className="flex w-full items-center justify-between px-3 py-2 text-xs font-medium text-muted-foreground hover:text-foreground"
          >
            {t('redaction.systems.otherEgress.title')}
            <ChevronDown className={cn('size-3.5 transition-transform', otherOpen && 'rotate-180')} />
          </button>
          {otherOpen && (
            <div className="space-y-2 border-t border-surface-border p-3">
              <p className="text-xs text-muted-foreground">{t('redaction.systems.otherEgress.desc')}</p>
              {otherEgress.map(([key, rule]) => (
                <div key={key} className="flex flex-wrap items-center gap-2 rounded-lg bg-muted/50 p-2.5">
                  <code className="rounded bg-muted px-1.5 py-0.5 font-mono text-xs text-muted-foreground">{key}</code>
                  <Badge variant="outline">{t(`redaction.restore.${rule.restore_args}`)}</Badge>
                  <button
                    onClick={() => void removeOtherEgress(key)}
                    title={intl.formatMessage({ id: 'common.remove' })}
                    className="ml-auto rounded p-1 text-destructive hover:bg-destructive/10"
                  >
                    <X className="h-3.5 w-3.5" />
                  </button>
                </div>
              ))}
            </div>
          )}
        </div>
      )}

      {wizardTarget && (
        <RedactionSourceWizard
          open
          target={wizardTarget}
          toolEgress={toolEgress}
          existingSourceNames={dataSources.map((s) => s.name)}
          existingDbNames={dbSources.map((db) => db.name)}
          grantAgents={grantsResult?.agents ?? []}
          grantSources={grantsResult?.sources ?? []}
          onClose={() => setWizardTarget(null)}
          onSaved={handleWizardSaved}
          onGrantsSaved={loadGrants}
        />
      )}

      <ConfirmDialog
        open={deleteTarget !== null}
        onClose={() => setDeleteTarget(null)}
        onConfirm={() => void confirmDelete()}
        busy={deleting}
        title={t('redaction.dataSources.deleteConfirm.title')}
        message={t('redaction.dataSources.deleteConfirm.message', { name: deleteTarget?.name ?? '' }) as string}
      />

      {grantDialogTarget && (
        <Dialog open onOpenChange={(o) => { if (!o) setGrantDialogTarget(null); }}>
          <DialogContent>
            <DialogHeader>
              <DialogTitle>
                {t('redaction.systems.db.grants.dialog.title', { label: grantDialogTarget.label || grantDialogTarget.name })}
              </DialogTitle>
            </DialogHeader>
            <div className="space-y-3">
              <DbSourceAgentPicker
                agents={grantsResult?.agents ?? []}
                selected={grantDialogSelected}
                onChange={setGrantDialogSelected}
                disabled={grantDialogSaving}
              />
              <p className="text-xs text-muted-foreground">{t('redaction.systems.db.grants.dialog.hint')}</p>
              {grantDialogError && <p className="text-xs text-destructive">{grantDialogError}</p>}
            </div>
            <div className="flex items-center justify-end gap-2 pt-1">
              <Button variant="outline" size="sm" onClick={() => setGrantDialogTarget(null)}>
                {intl.formatMessage({ id: 'common.cancel' })}
              </Button>
              <Button variant="brand" size="sm" onClick={() => void saveGrantDialog()} disabled={grantDialogSaving}>
                {grantDialogSaving ? intl.formatMessage({ id: 'common.saving' }) : intl.formatMessage({ id: 'common.save' })}
              </Button>
            </div>
          </DialogContent>
        </Dialog>
      )}
    </div>
  );
}

function SystemRowView({
  row,
  grantsResult,
  onEditDb,
  onEditCustom,
  onDeleteDb,
  onDeleteCustom,
  onEgressChange,
  onJumpToRules,
  onGoToIntegrations,
  onOpenGrants,
}: {
  row: SystemRow;
  /** `null` hides the grant badge entirely (not loaded yet, or the operator
   *  isn't an admin — same fail-quiet convention as `dbSources`/`dbSourceErrors`). */
  grantsResult: DbSourceGrantsListResult | null;
  onEditDb: (db: DbSourceSummary) => void;
  onEditCustom: (s: RedactionDataSource) => void;
  onDeleteDb: (db: DbSourceSummary) => void;
  onDeleteCustom: (s: RedactionDataSource) => void;
  onEgressChange: (keys: string[], next: RedactionEgressRule) => void;
  onJumpToRules: (source: string, label: string) => void;
  onGoToIntegrations: () => void;
  onOpenGrants: (db: DbSourceSummary) => void;
}) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);

  if (row.kind === 'odoo') {
    const label = t('redaction.systems.odoo.name') as string;
    if (!row.connected) {
      return (
        <div className="rounded-lg border border-dashed border-surface-border bg-card p-3">
          <div className="flex flex-wrap items-center justify-between gap-2">
            <div className="flex items-center gap-2">
              <span className="text-sm font-medium text-foreground">{t('redaction.systems.odoo.notConnected')}</span>
            </div>
            <CrossLink label={t('redaction.systems.odoo.goToIntegrations') as string} onClick={onGoToIntegrations} />
          </div>
          <p className="mt-1.5 text-xs text-muted-foreground">{t('redaction.systems.odoo.notConnectedHint')}</p>
        </div>
      );
    }
    return (
      <div className="rounded-lg border border-surface-border bg-card p-3">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <div className="flex flex-wrap items-center gap-2">
            <span className="text-sm font-medium text-foreground">{label}</span>
            <Badge className="bg-success/10 text-success">{t('redaction.systems.connected')}</Badge>
            <Badge variant="secondary">{t('redaction.systems.odoo.categoryChip')}</Badge>
            <RuleCountChip count={row.ruleCount} onClick={() => onJumpToRules('odoo', label)} />
          </div>
          <CrossLink label={intl.formatMessage({ id: 'common.edit' })} onClick={onGoToIntegrations} />
        </div>
        <RowBody
          readIn={
            <div className="space-y-0.5">
              <p>{t('redaction.systems.odoo.readIn')}</p>
              <p>{t('redaction.systems.odoo.readInHint')}</p>
            </div>
          }
          writeBack={
            <EgressEditor
              value={row.egress ?? { restore_args: 'deny', audit_reveal: false }}
              onChange={(next) => onEgressChange(row.egressKeys, next)}
            />
          }
        />
      </div>
    );
  }

  if (row.kind === 'db') {
    const label = row.db.label || row.db.name;
    const grantCount = grantsResult ? countGrantsForSource(grantsResult.sources, row.db.name) : null;
    return (
      <div className="rounded-lg border border-surface-border bg-card p-3">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <div className="flex flex-wrap items-center gap-2">
            <span className="text-sm font-medium text-foreground">{label}</span>
            <Badge variant="secondary">{row.db.driver}</Badge>
            <RuleCountChip count={row.ruleCount} onClick={() => onJumpToRules('duduclaw_db', label)} />
            {grantCount !== null && (
              <button type="button" onClick={() => onOpenGrants(row.db)} className="inline-flex">
                <Badge
                  className={cn(
                    'cursor-pointer',
                    grantCount === 0 ? 'bg-warning/15 text-warning' : 'bg-brand/10 text-brand hover:bg-brand/20',
                  )}
                  variant="secondary"
                >
                  {t(grantCount === 0 ? 'redaction.systems.db.grants.badge.none' : 'redaction.systems.db.grants.badge', { count: grantCount })}
                </Badge>
              </button>
            )}
          </div>
          <div className="flex shrink-0 items-center gap-1">
            <Button variant="ghost" size="xs" onClick={() => onEditDb(row.db)}>{intl.formatMessage({ id: 'common.edit' })}</Button>
            <Button variant="ghost" size="xs" className="text-destructive hover:bg-destructive/10" onClick={() => onDeleteDb(row.db)}>
              {intl.formatMessage({ id: 'common.delete' })}
            </Button>
          </div>
        </div>
        <RowBody
          readIn={
            <div className="space-y-0.5">
              <p>{t('redaction.dataSources.coverage.allowedTables', { tables: row.db.allowed_tables.join('、') })}</p>
              <p>{t('redaction.systems.db.readInHint')}</p>
            </div>
          }
          writeBack={<NotApplicable hint={t('redaction.systems.notApplicable.readOnlyHint') as string} />}
        />
      </div>
    );
  }

  if (row.kind === 'custom') {
    const label = row.source.label || row.source.name;
    return (
      <div className="rounded-lg border border-surface-border bg-card p-3">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <div className="flex flex-wrap items-center gap-2">
            <span className="text-sm font-medium text-foreground">{label}</span>
            <Badge variant="outline">{t('redaction.systems.customToolChip')}</Badge>
            <RuleCountChip count={row.ruleCount} onClick={() => onJumpToRules(row.source.name, label)} />
          </div>
          <div className="flex shrink-0 items-center gap-1">
            <Button variant="ghost" size="xs" onClick={() => onEditCustom(row.source)}>{intl.formatMessage({ id: 'common.edit' })}</Button>
            <Button variant="ghost" size="xs" className="text-destructive hover:bg-destructive/10" onClick={() => onDeleteCustom(row.source)}>
              {intl.formatMessage({ id: 'common.delete' })}
            </Button>
          </div>
        </div>
        <RowBody
          readIn={<CustomSourceReadIn source={row.source} />}
          writeBack={
            <EgressEditor
              value={row.egress ?? { restore_args: 'deny', audit_reveal: false }}
              onChange={(next) => onEgressChange(row.egressKeys, next)}
            />
          }
        />
      </div>
    );
  }

  // files
  return (
    <div className="rounded-lg border border-dashed border-surface-border bg-card p-3">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="flex flex-wrap items-center gap-2">
          <span className="text-sm font-medium text-foreground">{t('redaction.systems.files.name')}</span>
          <Badge variant="outline">{t('redaction.systems.files.chip')}</Badge>
          <RuleCountChip count={row.ruleCount} onClick={() => onJumpToRules('duduclaw_files', t('redaction.systems.files.name') as string)} />
        </div>
      </div>
      <RowBody
        readIn={
          <div className="space-y-0.5">
            <p>{t('redaction.systems.files.readIn')}</p>
            <p>{t('redaction.systems.files.readInHint')}</p>
          </div>
        }
        writeBack={<NotApplicable />}
      />
    </div>
  );
}
