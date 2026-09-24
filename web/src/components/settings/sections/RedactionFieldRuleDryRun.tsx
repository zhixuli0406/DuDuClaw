import { useIntl } from 'react-intl';
import { Play, Plus, AlertTriangle } from 'lucide-react';
import { type RedactionDryRunResult, type RedactionFieldRule } from '@/lib/api';
import {
  Badge,
  Button,
  Input,
  Textarea,
  Table,
  TableHeader,
  TableBody,
  TableRow,
  TableHead,
  TableCell,
} from '@/components/mds';
import { FieldBlock } from '@/pages/agent-form/form-rows';
import { KvRowEditor } from './RedactionTokenInput';
import { categoryLabel } from './RedactionTab';
import { addKvRow, updateKvRow, removeKvRow, summarizeDryRun, type KvRow } from './redactionFieldRules';

// ── Dry-run panel (canvas screen 6) ─────────────────────────────────────
//
// Opened from a rule row's "試跑" action, prefilled with that rule's tool +
// args (`dryRunDefaultsForRule`, computed by the caller). Never renders an
// original value — only positions, rule ids, categories, and tokens.

export interface DryRunState {
  tool: string;
  args: KvRow[];
  sample: string;
  loading: boolean;
  error: string | null;
  result: RedactionDryRunResult | null;
}

export function DryRunPanel({
  rule,
  state,
  onChange,
  onRun,
  onClose,
  fieldRuleIds,
  categoryLabels,
}: {
  rule: RedactionFieldRule;
  state: DryRunState;
  onChange: (next: DryRunState) => void;
  onRun: () => void;
  onClose: () => void;
  fieldRuleIds: Set<string>;
  /** `RedactionConfig.category_labels` — passed through to `categoryLabel`
   *  for the hit table's category column. */
  categoryLabels?: Record<string, string>;
}) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);
  const summary = state.result ? summarizeDryRun(state.result, fieldRuleIds) : null;

  return (
    <div className="space-y-3.5 rounded-xl border border-surface-border bg-card p-4">
      <div className="flex items-center justify-between gap-3">
        <h5 className="flex items-center gap-1.5 text-sm font-medium text-foreground">
          <Play className="size-4 text-muted-foreground" />
          {t('redaction.fieldRules.dryRun.title', { id: rule.id })}
        </h5>
        <Button variant="ghost" size="sm" onClick={onClose}>{intl.formatMessage({ id: 'common.close' })}</Button>
      </div>
      <p className="text-xs text-muted-foreground">{t('redaction.fieldRules.dryRun.desc')}</p>

      <div className="grid gap-3.5 sm:grid-cols-2">
        <FieldBlock label={t('redaction.fieldRules.dryRun.tool.label')}>
          <Input value={state.tool} onChange={(e) => onChange({ ...state, tool: e.target.value })} className="font-mono" />
        </FieldBlock>
        <FieldBlock label={t('redaction.fieldRules.dryRun.args.label')}>
          <div className="space-y-1.5">
            {state.args.map((row, i) => (
              <KvRowEditor
                key={i}
                row={row}
                onChange={(patch) => onChange({ ...state, args: updateKvRow(state.args, i, patch) })}
                onRemove={() => onChange({ ...state, args: removeKvRow(state.args, i) })}
                keyPlaceholder={t('redaction.fieldRules.form.matchArgs.keyPlaceholder')}
                valuePlaceholder={t('redaction.fieldRules.form.matchArgs.valuePlaceholder')}
              />
            ))}
            <button
              type="button"
              onClick={() => onChange({ ...state, args: addKvRow(state.args) })}
              className="inline-flex items-center gap-1 text-xs text-brand hover:underline"
            >
              <Plus className="size-3" />{t('redaction.fieldRules.dryRun.args.add')}
            </button>
          </div>
        </FieldBlock>
      </div>

      <FieldBlock label={t('redaction.fieldRules.dryRun.sample.label')} description={t('redaction.fieldRules.dryRun.sample.hint')}>
        <Textarea
          value={state.sample}
          onChange={(e) => onChange({ ...state, sample: e.target.value })}
          className="min-h-32 font-mono text-xs"
        />
      </FieldBlock>

      <div className="flex items-center gap-2">
        <Button variant="brand" size="sm" onClick={onRun} disabled={state.loading}>
          <Play />
          {state.loading ? intl.formatMessage({ id: 'common.saving' }) : t('redaction.fieldRules.dryRun.run')}
        </Button>
      </div>

      {state.error && (
        <div className="flex gap-2.5 rounded-lg border border-destructive/30 bg-destructive/10 px-3 py-2.5 text-xs text-destructive">
          <AlertTriangle className="mt-0.5 size-4 shrink-0" />
          <span>{t('redaction.fieldRules.dryRun.error', { message: state.error })}</span>
        </div>
      )}

      {summary && state.result && (
        <>
          <div className="flex flex-wrap gap-2">
            <Badge className="bg-success/10 text-success">{t('redaction.fieldRules.dryRun.hits', { count: summary.totalHits })}</Badge>
            <Badge className="bg-success/10 text-success">
              {t('redaction.fieldRules.dryRun.restored', { ok: summary.restoredOk, total: summary.totalHits })}
            </Badge>
            <Badge variant="secondary">
              {t('redaction.fieldRules.dryRun.split', { field: summary.fieldRuleHits, pattern: summary.patternHits })}
            </Badge>
          </div>
          {state.result.hits.length > 0 ? (
            <div className="overflow-x-auto rounded-lg border border-surface-border">
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead className="font-mono">{t('redaction.fieldRules.dryRun.table.pointer')}</TableHead>
                    <TableHead>{t('redaction.fieldRules.dryRun.table.rule')}</TableHead>
                    <TableHead>{t('redaction.fieldRules.dryRun.table.category')}</TableHead>
                    <TableHead className="font-mono">{t('redaction.fieldRules.dryRun.table.token')}</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {state.result.hits.map((hit, i) => (
                    <TableRow key={i}>
                      <TableCell className="font-mono text-xs">{hit.pointer}</TableCell>
                      <TableCell className="font-mono text-xs">{hit.rule_id}</TableCell>
                      <TableCell className="text-xs">{categoryLabel(intl, hit.category, categoryLabels)}</TableCell>
                      <TableCell className="font-mono text-xs">{hit.token}</TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            </div>
          ) : (
            <p className="rounded-lg bg-muted/50 px-3 py-4 text-center text-xs text-muted-foreground">
              {t('redaction.fieldRules.dryRun.empty')}
            </p>
          )}
        </>
      )}
    </div>
  );
}
