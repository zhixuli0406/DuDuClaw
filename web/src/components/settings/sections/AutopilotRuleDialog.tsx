import { useCallback, useEffect, useId, useMemo, useState, type ReactNode } from 'react';
import { useIntl } from 'react-intl';
import { Link } from 'react-router';
import { Plus, Trash2, AlertTriangle } from 'lucide-react';
import { useAgentsStore } from '@/stores/agents-store';
import { api } from '@/lib/api';
import { formatError, formatErrorDetail } from '@/lib/toast';
import { channelLabel } from '@/lib/session-channel';
import { glyphText } from '@/lib/agent-glyph';
import {
  ACTION_REQUIRED_FIELDS,
  CONDITION_OPS,
  FORM_ACTION_TYPES,
  FORM_TRIGGER_EVENTS,
  FREE_FORM_FIELD_TRIGGERS,
  NOTIFY_CHANNELS,
  TRIGGER_FIELD_SUGGESTIONS,
  buildCreatePayload,
  buildUpdatePayload,
  emptyRuleForm,
  formStateFromRule,
  type AutopilotRule,
  type AutopilotTriggerEvent,
  type BuildIssue,
  type ConditionOp,
  type ConditionRow,
  type FormActionType,
  type RuleFormState,
} from '@/lib/autopilot-rules';
import {
  Button,
  Input,
  Textarea,
  Select,
  SelectTrigger,
  SelectValue,
  SelectContent,
  SelectItem,
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogFooter,
} from '@/components/mds';
import { AutonomyNote } from '@/components/AutonomyNote';
import { FieldBlock } from '@/pages/agent-form/form-rows';

/** Stacked label + mds Select, for the rule dialog's enum pickers. */
export function DialogSelect({
  label,
  value,
  onChange,
  options,
  disabled,
}: {
  label: ReactNode;
  value: string;
  onChange: (v: string) => void;
  options: ReadonlyArray<{ value: string; label: ReactNode }>;
  disabled?: boolean;
}) {
  const current = options.find((o) => o.value === value);
  return (
    <FieldBlock label={label}>
      <Select value={value} onValueChange={(v) => onChange(String(v))} disabled={disabled}>
        <SelectTrigger className="w-full" aria-label={typeof label === 'string' ? label : undefined}>
          <SelectValue>{current?.label ?? value}</SelectValue>
        </SelectTrigger>
        <SelectContent>
          {options.map((o) => (
            <SelectItem key={o.value} value={o.value}>{o.label}</SelectItem>
          ))}
        </SelectContent>
      </Select>
    </FieldBlock>
  );
}

/** Label for a trigger id; falls back to the raw id for one this build does not know. */
export function useTriggerLabel() {
  const intl = useIntl();
  return useCallback(
    (ev: string) => {
      const id = `autopilot.trigger.${ev}`;
      return intl.messages[id] ? intl.formatMessage({ id }) : ev;
    },
    [intl],
  );
}

export function useActionLabel() {
  const intl = useIntl();
  return useCallback(
    (type: string) => {
      const id = `autopilot.action.${type}`;
      return intl.messages[id] ? intl.formatMessage({ id }) : type;
    },
    [intl],
  );
}

const MISSING_FIELD_LABEL: Record<string, string> = {
  name: 'autopilot.field.name',
  target_agent: 'autopilot.field.actionAgent',
  prompt: 'autopilot.field.promptTemplate',
  skill_name: 'autopilot.field.skillName',
  channel: 'autopilot.field.channel',
  chat_id: 'autopilot.field.chatId',
  text: 'autopilot.field.text',
};

/**
 * Create / edit dialog for an autopilot rule. The payload is built by
 * `lib/autopilot-rules` so it matches the gateway's write-time validation;
 * a refused save shows the server's message inside the dialog.
 */
export function AutopilotRuleDialog({
  rule,
  onClose,
  onSaved,
}: {
  /** Rule to edit; omitted to create a new one. */
  rule?: AutopilotRule;
  onClose: () => void;
  onSaved: () => void;
}) {
  const intl = useIntl();
  const t = useCallback(
    (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values),
    [intl],
  );
  const triggerLabel = useTriggerLabel();
  const actionLabel = useActionLabel();
  const { agents, fetchAgents } = useAgentsStore();
  const [form, setForm] = useState<RuleFormState>(() =>
    rule ? formStateFromRule(rule) : emptyRuleForm(),
  );
  const [issues, setIssues] = useState<BuildIssue[]>([]);
  const [serverError, setServerError] = useState<unknown>(null);
  const [submitting, setSubmitting] = useState(false);
  const datalistId = useId();

  useEffect(() => { fetchAgents(); }, [fetchAgents]);
  useEffect(() => {
    if (agents.length > 0 && !form.targetAgent) {
      setForm((f) => ({ ...f, targetAgent: agents[0].name }));
    }
  }, [agents, form.targetAgent]);

  const patch = useCallback((p: Partial<RuleFormState>) => setForm((f) => ({ ...f, ...p })), []);

  const patchRow = useCallback((index: number, p: Partial<ConditionRow>) => {
    setForm((f) => ({
      ...f,
      conditionsTouched: true,
      conditions: f.conditions.map((row, i) => {
        if (i !== index) return row;
        const next = { ...row, ...p };
        // Editing the operator or the value drops the loaded typed value so
        // the new text is parsed for the (possibly new) operator.
        if ('op' in p || 'valueText' in p) delete next.typedValue;
        return next;
      }),
    }));
  }, []);

  const addRow = useCallback(() => {
    setForm((f) => ({
      ...f,
      conditionsTouched: true,
      conditions: [...f.conditions, { field: '', op: 'eq', valueText: '' }],
    }));
  }, []);

  const removeRow = useCallback((index: number) => {
    setForm((f) => ({
      ...f,
      conditionsTouched: true,
      conditions: f.conditions.filter((_, i) => i !== index),
    }));
  }, []);

  const triggerOptions = useMemo(() => {
    const ids: string[] = [...FORM_TRIGGER_EVENTS];
    // Never silently swap out a saved trigger the form does not offer.
    if (!ids.includes(form.triggerEvent)) ids.push(form.triggerEvent);
    return ids.map((v) => ({ value: v, label: triggerLabel(v) }));
  }, [form.triggerEvent, triggerLabel]);

  const agentOptions = useMemo(() => {
    const opts = agents.map((a) => ({ value: a.name, label: `${glyphText(a.icon)} ${a.display_name}` }));
    if (form.targetAgent && !opts.some((o) => o.value === form.targetAgent)) {
      opts.push({ value: form.targetAgent, label: form.targetAgent });
    }
    return opts;
  }, [agents, form.targetAgent]);

  const fieldSuggestions =
    TRIGGER_FIELD_SUGGESTIONS[form.triggerEvent as AutopilotTriggerEvent] ?? [];
  const conditionsEditable = !form.conditionsLocked && !form.sequenceRule;

  const issueText = (issue: BuildIssue): string => {
    switch (issue.kind) {
      case 'missing':
        return t('autopilot.form.missing', {
          field: t(MISSING_FIELD_LABEL[issue.field] ?? 'autopilot.field.name'),
        });
      case 'conditionField':
        return t('autopilot.form.conditionField', { row: issue.row + 1 });
      case 'conditionNumber':
        return t('autopilot.form.conditionNumber', { row: issue.row + 1 });
      default:
        return t('autopilot.form.conditionValue', { row: issue.row + 1 });
    }
  };

  const handleSubmit = useCallback(async () => {
    setServerError(null);
    if (rule) {
      const { fields, issues: found } = buildUpdatePayload(form, rule);
      setIssues(found);
      if (!fields) return;
      if (Object.keys(fields).length === 0) {
        onSaved();
        return;
      }
      setSubmitting(true);
      try {
        await api.autopilot.update(rule.id, fields);
        onSaved();
      } catch (e) {
        setServerError(e);
      } finally {
        setSubmitting(false);
      }
      return;
    }
    const { payload, issues: found } = buildCreatePayload(form);
    setIssues(found);
    if (!payload) return;
    setSubmitting(true);
    try {
      await api.autopilot.create(payload);
      onSaved();
    } catch (e) {
      setServerError(e);
    } finally {
      setSubmitting(false);
    }
  }, [form, rule, onSaved]);

  const actionNeeds = (key: string) =>
    !form.actionLocked && ACTION_REQUIRED_FIELDS[form.actionType].includes(key);

  return (
    <Dialog open onOpenChange={(o) => { if (!o) onClose(); }}>
      <DialogContent className="max-h-[90vh] overflow-y-auto sm:max-w-xl">
        <DialogHeader>
          <DialogTitle>{t(rule ? 'autopilot.edit' : 'autopilot.create')}</DialogTitle>
        </DialogHeader>

        <div className="space-y-4">
          <FieldBlock label={t('autopilot.field.name')}>
            <Input
              aria-label={t('autopilot.field.name')}
              value={form.name}
              onChange={(e) => patch({ name: e.target.value })}
              autoFocus
            />
          </FieldBlock>

          <DialogSelect
            label={t('autopilot.field.triggerEvent')}
            value={form.triggerEvent}
            onChange={(v) => patch({ triggerEvent: v })}
            options={triggerOptions}
            disabled={form.sequenceRule}
          />
          <p className="text-xs text-muted-foreground">
            {t('autopilot.scheduleHint')}{' '}
            <Link to="/routines" className="underline underline-offset-2 hover:text-foreground">
              {t('autopilot.scheduleHint.link')}
            </Link>
          </p>

          {/* Conditions */}
          <fieldset className="space-y-2">
            <legend className="text-sm font-medium">{t('autopilot.field.conditions')}</legend>
            {form.sequenceRule ? (
              <p className="text-xs text-muted-foreground">{t('autopilot.sequence.locked')}</p>
            ) : form.conditionsLocked ? (
              <p className="text-xs text-muted-foreground">{t('autopilot.conditions.locked')}</p>
            ) : (
              <>
                {form.conditions.length === 0 ? (
                  <p className="rounded-md bg-muted px-2.5 py-1.5 text-xs text-foreground">
                    {t('autopilot.conditions.none')}
                  </p>
                ) : (
                  <p className="text-xs text-muted-foreground">{t('autopilot.conditions.help')}</p>
                )}
                {FREE_FORM_FIELD_TRIGGERS.has(form.triggerEvent) && (
                  <p className="text-xs text-muted-foreground">
                    {t(`autopilot.conditions.freeFormHint.${form.triggerEvent}`)}
                  </p>
                )}
                {form.conditions.length > 1 && (
                  <DialogSelect
                    label={t('autopilot.conditions.match')}
                    value={form.match}
                    onChange={(v) => patch({ match: v === 'any' ? 'any' : 'all', conditionsTouched: true })}
                    options={[
                      { value: 'all', label: t('autopilot.conditions.match.all') },
                      { value: 'any', label: t('autopilot.conditions.match.any') },
                    ]}
                  />
                )}
                <datalist id={datalistId}>
                  {fieldSuggestions.map((f) => <option key={f} value={f} />)}
                </datalist>
                {form.conditions.map((row, i) => (
                  <ConditionRowEditor
                    key={i}
                    index={i}
                    row={row}
                    datalistId={datalistId}
                    onChange={(p) => patchRow(i, p)}
                    onRemove={() => removeRow(i)}
                  />
                ))}
                <Button variant="outline" size="sm" onClick={addRow} disabled={!conditionsEditable}>
                  <Plus />
                  {t('autopilot.conditions.add')}
                </Button>
              </>
            )}
          </fieldset>

          {/* Action */}
          {form.actionLocked ? (
            <p className="text-xs text-muted-foreground">{t('autopilot.action.locked')}</p>
          ) : (
            <>
              <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
                <DialogSelect
                  label={t('autopilot.field.action')}
                  value={form.actionType}
                  onChange={(v) => patch({ actionType: v as FormActionType })}
                  options={FORM_ACTION_TYPES.map((a) => ({ value: a, label: actionLabel(a) }))}
                />
                {actionNeeds('target_agent') && (
                  <DialogSelect
                    label={t('autopilot.field.actionAgent')}
                    value={form.targetAgent}
                    onChange={(v) => patch({ targetAgent: v })}
                    options={agentOptions}
                  />
                )}
                {actionNeeds('channel') && (
                  <DialogSelect
                    label={t('autopilot.field.channel')}
                    value={form.channel}
                    onChange={(v) => patch({ channel: v })}
                    options={NOTIFY_CHANNELS.map((c) => ({ value: c, label: channelLabel(c) }))}
                  />
                )}
              </div>

              {actionNeeds('prompt') && (
                <FieldBlock
                  label={t('autopilot.field.promptTemplate')}
                  description={t('autopilot.field.templateHint')}
                >
                  <Textarea
                    aria-label={t('autopilot.field.promptTemplate')}
                    className="min-h-[80px] resize-y"
                    value={form.prompt}
                    onChange={(e) => patch({ prompt: e.target.value })}
                    placeholder={t('autopilot.field.promptTemplate.placeholder')}
                  />
                </FieldBlock>
              )}
              {form.actionType === 'delegate' && <AutonomyNote id="autopilotDelegate" />}

              {actionNeeds('skill_name') && (
                <FieldBlock label={t('autopilot.field.skillName')}>
                  <Input
                    aria-label={t('autopilot.field.skillName')}
                    value={form.skillName}
                    onChange={(e) => patch({ skillName: e.target.value })}
                  />
                </FieldBlock>
              )}

              {actionNeeds('chat_id') && (
                <FieldBlock label={t('autopilot.field.chatId')} description={t('autopilot.field.chatId.help')}>
                  <Input
                    aria-label={t('autopilot.field.chatId')}
                    value={form.chatId}
                    onChange={(e) => patch({ chatId: e.target.value })}
                  />
                </FieldBlock>
              )}
              {actionNeeds('text') && (
                <FieldBlock label={t('autopilot.field.text')} description={t('autopilot.field.templateHint')}>
                  <Textarea
                    aria-label={t('autopilot.field.text')}
                    className="min-h-[60px] resize-y"
                    value={form.text}
                    onChange={(e) => patch({ text: e.target.value })}
                  />
                </FieldBlock>
              )}
            </>
          )}

          {(issues.length > 0 || serverError != null) && (
            <div
              role="alert"
              className="flex gap-2 rounded-lg border border-destructive/40 bg-destructive/5 px-3 py-2 text-sm text-destructive"
            >
              <AlertTriangle className="mt-0.5 size-4 shrink-0" aria-hidden />
              <div className="space-y-1">
                {serverError != null && (
                  <>
                    <p className="font-medium">{t('autopilot.form.saveFailed')}</p>
                    <p className="break-words">{formatErrorDetail(serverError) || formatError(serverError)}</p>
                  </>
                )}
                {issues.length > 0 && (
                  <ul className="list-disc space-y-0.5 pl-4">
                    {issues.map((issue, i) => <li key={i}>{issueText(issue)}</li>)}
                  </ul>
                )}
              </div>
            </div>
          )}
        </div>

        <DialogFooter>
          <Button variant="ghost" size="sm" onClick={onClose}>
            {t('common.cancel')}
          </Button>
          <Button variant="brand" size="sm" onClick={handleSubmit} disabled={submitting}>
            {t(rule ? 'autopilot.save' : 'autopilot.create')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function ConditionRowEditor({
  index,
  row,
  datalistId,
  onChange,
  onRemove,
}: {
  index: number;
  row: ConditionRow;
  datalistId: string;
  onChange: (p: Partial<ConditionRow>) => void;
  onRemove: () => void;
}) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);
  const n = index + 1;
  const isList = row.op === 'in' || row.op === 'not_in';
  const opOptions = CONDITION_OPS.map((op) => ({ value: op, label: t(`autopilot.op.${op}`) }));
  const currentOp = opOptions.find((o) => o.value === row.op);
  return (
    <div className="grid grid-cols-1 items-start gap-2 rounded-lg border border-surface-border p-2 sm:grid-cols-[1fr_9rem_1fr_auto]">
      <Input
        aria-label={t('autopilot.conditions.fieldN', { row: n })}
        placeholder={t('autopilot.conditions.field')}
        list={datalistId}
        value={row.field}
        onChange={(e) => onChange({ field: e.target.value })}
      />
      <Select value={row.op} onValueChange={(v) => onChange({ op: String(v) as ConditionOp })}>
        <SelectTrigger className="w-full" aria-label={t('autopilot.conditions.opN', { row: n })}>
          <SelectValue>{currentOp?.label}</SelectValue>
        </SelectTrigger>
        <SelectContent>
          {opOptions.map((o) => (
            <SelectItem key={o.value} value={o.value}>{o.label}</SelectItem>
          ))}
        </SelectContent>
      </Select>
      <div className="space-y-1">
        <Input
          aria-label={t('autopilot.conditions.valueN', { row: n })}
          placeholder={t('autopilot.conditions.value')}
          value={row.valueText}
          onChange={(e) => onChange({ valueText: e.target.value })}
        />
        {isList && <p className="text-xs text-muted-foreground">{t('autopilot.conditions.listHint')}</p>}
      </div>
      <Button
        variant="ghost"
        size="icon-sm"
        onClick={onRemove}
        aria-label={t('autopilot.conditions.remove', { row: n })}
        className="text-muted-foreground hover:text-destructive"
      >
        <Trash2 />
      </Button>
    </div>
  );
}
