import { useCompanyTimeZone } from '@/lib/company-time';
import { useProductPolicy } from '@/lib/product-policy';
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
  eventFieldLabel,
  paramPreview,
  type ActiveWindow,
  type ParamDraft,
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
  emptyStep,
  formStateFromRule,
  type AutopilotRule,
  type AutopilotTriggerEvent,
  type BuildIssue,
  type ConditionOp,
  type ConditionRow,
  type StepDraft,
  type RuleFormState,
} from '@/lib/autopilot-rules';
import {
  DropdownMenu,
  DropdownMenuTrigger,
  DropdownMenuContent,
  DropdownMenuItem,
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
  const policy = useProductPolicy();
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
  const company = useCompanyTimeZone();
  const companyZone = company.status === 'set' ? company.zone : null;
  const [templateFields, setTemplateFields] = useState<{ trigger: string; fields: string[] } | null>(null);
  const [fieldsError, setFieldsError] = useState(false);
  const [fieldsRetry, setFieldsRetry] = useState(0);
  const availableFields = templateFields?.trigger === form.triggerEvent ? templateFields.fields : undefined;
  useEffect(() => {
    let active = true;
    setTemplateFields(null);
    setFieldsError(false);
    api.autopilot.templateFields(form.triggerEvent, rule?.sequence).then(result => {
      if (active) setTemplateFields({ trigger: form.triggerEvent, fields: result.fields });
    }).catch(() => { if (active) setFieldsError(true); });
    return () => { active = false; };
  }, [form.triggerEvent, rule?.sequence, fieldsRetry]);
  const fieldLabel = (path: string) => eventFieldLabel(path, t);
  const window = form.active_window as ActiveWindow | null | undefined;
  const patchWindow = (change: Partial<ActiveWindow>) => setForm(f => ({ ...f, windowTouched: true,
    active_window: { days: [1, 2, 3, 4, 5], start: '09:00', end: '18:00', timezone: null, ...(f.active_window as Partial<ActiveWindow> | null), ...change } }));


  useEffect(() => { fetchAgents(); }, [fetchAgents]);
  useEffect(() => {
    if (agents.length > 0) {
      setForm(f => ({ ...f, steps: f.steps.map(step =>
        !step.original && !step.targetAgent ? { ...step, targetAgent: agents[0].name } : step) }));
    }
  }, [agents]);

  const patchStep = (index: number, p: Partial<StepDraft>) => {
    setForm(f => ({ ...f, steps: f.steps.map((step, i) => i === index ? { ...step, ...p } : step) }));
  };
  const moveStep = (index: number, direction: number) => {
    setIssues([]);
    setForm(f => {
      const steps = [...f.steps];
      [steps[index], steps[index + direction]] = [steps[index + direction], steps[index]];
      return { ...f, steps };
    });
  };

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

  const agentOptions = (step: StepDraft) => {
    const opts = agents.map(a => ({ value: a.name, label: `${glyphText(a.icon)} ${a.display_name}` }));
    if (step.targetAgent && !opts.some(o => o.value === step.targetAgent)) {
      opts.push({ value: step.targetAgent, label: step.targetAgent });
    }
    return opts;
  };

  const fieldSuggestions =
    TRIGGER_FIELD_SUGGESTIONS[form.triggerEvent as AutopilotTriggerEvent] ?? [];
  const conditionsEditable = !form.conditionsLocked && !form.sequenceRule;

  const issueText = (issue: BuildIssue): string => {
    switch (issue.kind) {
      case 'windowTime': case 'windowDays': case 'windowZone': case 'windowZoneInvalid':
      case 'paramCount': case 'paramName': case 'paramDuplicate': case 'paramSyntax': case 'paramLength': case 'paramFieldsUnavailable':
        return t(`autopilot.validation.${issue.kind}`);
      case 'paramField':
        return t('autopilot.validation.paramField', { name: issue.name, label: fieldLabel(issue.path) });
      case 'stepCount':
      case 'waitDuration':
      case 'waitOrder':
        return t(`autopilot.steps.error.${issue.kind}`);
      case 'missing':
        return t('autopilot.form.missing', {
          field: t(MISSING_FIELD_LABEL[issue.field] ?? 'autopilot.field.name'),
        });
      case 'conditionField':
        return t('autopilot.form.conditionField', { row: issue.row + 1 });
      case 'conditionNumber':
        return t('autopilot.form.conditionNumber', { row: issue.row + 1 });
      default:
        return t('autopilot.form.conditionValue', { row: 'row' in issue ? issue.row + 1 : 1 });
    }
  };

  const handleSubmit = useCallback(async () => {
    setServerError(null);
    if (rule) {
      const { fields, issues: found } = buildUpdatePayload(form, rule, { companyZone, templateFields: availableFields });
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
    const { payload, issues: found } = buildCreatePayload(form, { companyZone, templateFields: availableFields });
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
  }, [form, rule, onSaved, companyZone, availableFields]);

  return (
    <Dialog open onOpenChange={(o) => { if (!o) onClose(); }}>
      <DialogContent className="max-h-[90vh] overflow-y-auto sm:max-w-xl">
        <DialogHeader>
          <DialogTitle>{t(rule ? 'autopilot.edit' : 'autopilot.create')}</DialogTitle>
        </DialogHeader>

        <div className="space-y-4">
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

          {/* Steps */}
          {form.actionLocked ? (
            <p className="text-xs text-muted-foreground">{t('autopilot.action.locked')}</p>
          ) : (
            <fieldset className="space-y-3">
              <legend className="text-sm font-medium">{t('autopilot.steps.title')}</legend>
              {form.steps.map((step, index) => {
                const actionNeeds = (key: string) => step.actionType !== 'wait' && ACTION_REQUIRED_FIELDS[step.actionType].includes(key);
                const invalid = issues.some(issue => ('steps' in issue && issue.steps.includes(index)) || ('step' in issue && issue.step === index));
                return (
                  <div key={index} role="group" aria-label={t('autopilot.steps.row', { step: index + 1 })}
                    className={`space-y-3 rounded-lg border p-3 ${invalid ? 'border-destructive outline outline-1 outline-destructive' : 'border-surface-border'}`}>
                    <div className="flex items-center justify-between gap-2">
                      <span className="text-sm font-medium">{index + 1}. {actionLabel(step.actionType)}</span>
                      <div className="flex gap-1">
                        <Button variant="ghost" size="icon-sm" disabled={index === 0} onClick={() => moveStep(index, -1)} aria-label={t('autopilot.steps.up', { step: index + 1 })}>↑</Button>
                        <Button variant="ghost" size="icon-sm" disabled={index === form.steps.length - 1} onClick={() => moveStep(index, 1)} aria-label={t('autopilot.steps.down', { step: index + 1 })}>↓</Button>
                        <Button variant="ghost" size="icon-sm" onClick={() => { patch({ steps: form.steps.filter((_, i) => i !== index) }); setIssues([]); }} aria-label={t('autopilot.steps.remove', { step: index + 1 })}>✕</Button>
                      </div>
                    </div>
                    {step.actionType === 'wait' ? (
                      <FieldBlock label={t('autopilot.steps.minutes')}>
                        <Input type="number" min={1} max={1440} step={1} aria-label={t('autopilot.steps.minutes')} value={Number.isNaN(step.minutes) ? '' : step.minutes}
                          onChange={e => patchStep(index, { minutes: e.target.valueAsNumber })} />
                      </FieldBlock>
                    ) : (<>
                      <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
                        {actionNeeds('target_agent') && (
                          <DialogSelect
                            label={t('autopilot.field.actionAgent')}
                            value={step.targetAgent}
                            onChange={(v) => patchStep(index, { targetAgent: v })}
                            options={agentOptions(step)}
                          />
                        )}
                        {actionNeeds('channel') && (
                          <DialogSelect
                            label={t('autopilot.field.channel')}
                            value={step.channel}
                            onChange={(v) => patchStep(index, { channel: v })}
                            options={NOTIFY_CHANNELS.filter(c => policy.allows('channels', c)).map((c) => ({ value: c, label: channelLabel(c) }))}
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
                            value={step.prompt}
                            onChange={(e) => patchStep(index, { prompt: e.target.value })}
                            placeholder={t('autopilot.field.promptTemplate.placeholder')}
                          />
                        </FieldBlock>
                      )}
                      {step.actionType === 'delegate' && <AutonomyNote id="autopilotDelegate" />}

                      {actionNeeds('skill_name') && (
                        <FieldBlock label={t('autopilot.field.skillName')}>
                          <Input
                            aria-label={t('autopilot.field.skillName')}
                            value={step.skillName}
                            onChange={(e) => patchStep(index, { skillName: e.target.value })}
                          />
                        </FieldBlock>
                      )}

                      {step.actionType === 'run_skill' && <fieldset className="space-y-2">
                        <legend className="text-sm font-medium">{t('autopilot.params.title')}</legend>
                        {fieldsError && <div className="text-sm text-destructive">{t('autopilot.params.loadFailed')} <Button variant="ghost" size="sm" onClick={() => setFieldsRetry(n => n + 1)}>{t('common.retry')}</Button></div>}
                        {step.params.map((param, row) => {
                          const change = (p: Partial<ParamDraft>) => patchStep(index, { paramsTouched: true, params: step.params.map((v, i) => i === row ? { ...v, ...p } : v) });
                          const invalidParam = issues.some(issue => 'step' in issue && issue.step === index && 'row' in issue && issue.row === row);
                          return <div key={row} className={`space-y-2 rounded-md border p-2 ${invalidParam ? 'border-destructive outline outline-1 outline-destructive' : 'border-surface-border'}`}>
                            <div className="grid gap-2 sm:grid-cols-3">
                              <FieldBlock label={t('autopilot.params.name')}><Input aria-label={t('autopilot.params.name')} value={param.name} onChange={e => change({ name: e.target.value })} aria-invalid={invalidParam} /></FieldBlock>
                              <FieldBlock label={t('autopilot.params.value')}><Input aria-label={t('autopilot.params.value')} value={param.value} onChange={e => change({ value: e.target.value })} aria-invalid={invalidParam} /></FieldBlock>
                              <DialogSelect label={t('autopilot.params.type')} value={param.type} onChange={v => change({ type: v as ParamDraft['type'] })} options={(['text', 'number', 'id', 'date'] as const).map(type => ({ value: type, label: t(`autopilot.params.type.${type}`) }))} />
                            </div>
                            {param.value.includes('{{') && <p className="break-words text-sm text-muted-foreground">{paramPreview(param.value, fieldLabel)}</p>}
                            <div className="flex flex-wrap gap-2">
                              <DropdownMenu>
                                <DropdownMenuTrigger render={<Button variant="outline" size="sm" disabled={!availableFields?.some(path => !path.includes('*'))} />}>{t('autopilot.params.insert')}</DropdownMenuTrigger>
                                <DropdownMenuContent aria-label={t('autopilot.params.insert')}>
                                  {availableFields?.filter(path => !path.includes('*')).map(path => <DropdownMenuItem key={path} onClick={() => change({ value: param.value + '{{' + path + '}}' })}>{fieldLabel(path)}</DropdownMenuItem>)}
                                </DropdownMenuContent>
                              </DropdownMenu>
                              <Button variant="ghost" size="sm" onClick={() => patchStep(index, { paramsTouched: true, params: step.params.filter((_, i) => i !== row) })}>{t('autopilot.params.remove')}</Button>
                            </div>
                          </div>;
                        })}
                        <Button variant="outline" size="sm" disabled={step.params.length >= 10} onClick={() => patchStep(index, { paramsTouched: true, params: [...step.params, { name: '', value: '', type: 'text' }] })}><Plus />{t('autopilot.params.add')}</Button>
                      </fieldset>}

                      {actionNeeds('chat_id') && (
                        <FieldBlock label={t('autopilot.field.chatId')} description={t('autopilot.field.chatId.help')}>
                          <Input
                            aria-label={t('autopilot.field.chatId')}
                            value={step.chatId}
                            onChange={(e) => patchStep(index, { chatId: e.target.value })}
                          />
                        </FieldBlock>
                      )}
                      {actionNeeds('text') && (
                        <FieldBlock label={t('autopilot.field.text')} description={t('autopilot.field.templateHint')}>
                          <Textarea
                            aria-label={t('autopilot.field.text')}
                            className="min-h-[60px] resize-y"
                            value={step.text}
                            onChange={(e) => patchStep(index, { text: e.target.value })}
                          />
                        </FieldBlock>
                      )}
                      <label className="flex items-center gap-2 text-sm">
                        <input type="checkbox" checked={step.requireApproval} onChange={e => patchStep(index, { requireApproval: e.target.checked })} />
                        {t('autopilot.steps.approval')}
                      </label>
                    </>)}
                  </div>
                );
              })}
              <DropdownMenu>
                <DropdownMenuTrigger render={<Button variant="outline" size="sm" />}>
                  {t('autopilot.steps.add')}
                </DropdownMenuTrigger>
                <DropdownMenuContent aria-label={t('autopilot.steps.add')}>
                  {[...FORM_ACTION_TYPES, 'wait' as const].map(type => (
                    <DropdownMenuItem key={type} onClick={() => {
                      patch({ steps: [...form.steps, emptyStep(type, agents[0]?.name)] });
                    }}>{type === 'wait' ? t('autopilot.steps.waitMenu') : type === 'delegate' ? t('autopilot.steps.delegate') : actionLabel(type)}</DropdownMenuItem>
                  ))}
                </DropdownMenuContent>
              </DropdownMenu>

            </fieldset>
          )}


          <fieldset className={`space-y-3 rounded-lg border p-3 ${issues.some(issue => issue.kind.startsWith('window')) ? 'border-destructive' : 'border-surface-border'}`}>
            <legend className="text-sm font-medium">{t('autopilot.window.title')}</legend>
            <label className="flex items-center gap-2 text-sm"><input type="checkbox" checked={!window} onChange={e => e.target.checked ? patch({ active_window: null, run_policy: null, windowTouched: true }) : patchWindow({})} />{t('autopilot.window.unlimited')}</label>
            {window && <>
              <div className="flex flex-wrap gap-2">{[1, 2, 3, 4, 5, 6, 7].map(day => <Button key={day} size="sm" variant={window.days.includes(day) ? 'brand' : 'outline'} aria-pressed={window.days.includes(day)} aria-label={t(`autopilot.window.weekday.${day}`)} onClick={() => patchWindow({ days: window.days.includes(day) ? window.days.filter(d => d !== day) : [...window.days, day].sort() })}>{t(`autopilot.window.day.${day}`)}</Button>)}</div>
              <div className="grid gap-3 sm:grid-cols-2">
                <FieldBlock label={t('autopilot.window.start')}><Input type="time" aria-label={t('autopilot.window.start')} value={window.start} onChange={e => patchWindow({ start: e.target.value })} /></FieldBlock>
                <FieldBlock label={t('autopilot.window.end')}><Input type="time" aria-label={t('autopilot.window.end')} value={window.end} onChange={e => patchWindow({ end: e.target.value })} /></FieldBlock>
              </div>
              <DialogSelect label={t('autopilot.window.zone')} value={window.timezone ?? 'company'} onChange={v => patchWindow({ timezone: v === 'company' ? null : v })} options={[
                { value: 'company', label: t('autopilot.window.company', { zone: companyZone ?? t('autopilot.window.unset') }) },
                ...[...new Set(['Asia/Taipei', 'Asia/Tokyo', 'Asia/Hong_Kong', 'Asia/Singapore', 'America/New_York', 'America/Los_Angeles', 'Europe/London', 'Europe/Paris', 'UTC', ...(window.timezone ? [window.timezone] : [])])].map(zone => ({ value: zone, label: zone })),
              ]} />
              {!window.timezone && !companyZone && <p className="text-sm text-destructive">{t('autopilot.validation.windowZone')} <Link className="underline underline-offset-2" to="/settings?tab=general">{t('autopilot.window.settings')}</Link></p>}
            </>}
            <p className="text-xs text-muted-foreground">{t('autopilot.window.hint')}</p>
          </fieldset>


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
