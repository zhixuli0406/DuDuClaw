import { useEffect, useState, useCallback, type ReactNode } from 'react';
import { useIntl } from 'react-intl';
import { cn } from '@/lib/utils';
import { useAgentsStore } from '@/stores/agents-store';
import { api, type AutopilotRule, type AutopilotHistoryEntry } from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import {
  Card,
  Button,
  Badge,
  Empty,
  Switch,
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
import { ConfirmDialog } from '@/components/settings/controls';
import { FieldBlock } from '@/pages/agent-form/form-rows';
import { Plus, Clock, XCircle, Workflow } from 'lucide-react';
import { glyphText } from '@/lib/agent-glyph';

// ── Autopilot Tab ───────────────────────────────────────────

/** Stacked label + mds Select, for the create dialog's enum pickers. */
function DialogSelect({
  label,
  value,
  onChange,
  options,
}: {
  label: ReactNode;
  value: string;
  onChange: (v: string) => void;
  options: ReadonlyArray<{ value: string; label: ReactNode }>;
}) {
  const current = options.find((o) => o.value === value);
  return (
    <FieldBlock label={label}>
      <Select value={value} onValueChange={(v) => onChange(String(v))}>
        <SelectTrigger className="w-full" aria-label={typeof label === 'string' ? label : undefined}>
          <SelectValue>{current?.label}</SelectValue>
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

export function AutopilotTab() {
  const intl = useIntl();
  const [rules, setRules] = useState<AutopilotRule[]>([]);
  const [loading, setLoading] = useState(true);
  const [showCreate, setShowCreate] = useState(false);
  const [historyRuleId, setHistoryRuleId] = useState<string | null>(null);
  const [historyEntries, setHistoryEntries] = useState<AutopilotHistoryEntry[]>([]);
  const [removeTarget, setRemoveTarget] = useState<{ id: string; name: string } | null>(null);

  const fetchRules = useCallback(async () => {
    setLoading(true);
    try {
      const result = await api.autopilot.list();
      setRules(result?.rules ?? []);
    } catch (e) {
      setRules([]);
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
    } finally {
      setLoading(false);
    }
  }, [intl]);

  useEffect(() => { fetchRules(); }, [fetchRules]);

  const handleToggle = useCallback(async (ruleId: string, enabled: boolean) => {
    setRules((prev) => prev.map((r) => (r.id === ruleId ? { ...r, enabled } : r)));
    try {
      await api.autopilot.update(ruleId, { enabled });
    } catch (e) {
      setRules((prev) => prev.map((r) => (r.id === ruleId ? { ...r, enabled: !enabled } : r)));
      toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
    }
  }, [intl]);

  const handleRemove = useCallback(async () => {
    if (!removeTarget) return;
    try {
      await api.autopilot.remove(removeTarget.id);
      setRules((prev) => prev.filter((r) => r.id !== removeTarget.id));
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
    }
    setRemoveTarget(null);
  }, [removeTarget, intl]);

  const handleViewHistory = useCallback(async (ruleId: string) => {
    setHistoryRuleId(ruleId);
    try {
      const result = await api.autopilot.history(ruleId);
      setHistoryEntries(result?.entries ?? []);
    } catch (e) {
      setHistoryEntries([]);
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
    }
  }, [intl]);

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between gap-2">
        <p className="text-sm text-muted-foreground">
          {intl.formatMessage({ id: 'autopilot.subtitle' })}
        </p>
        <Button variant="brand" size="sm" onClick={() => setShowCreate(true)}>
          <Plus />
          {intl.formatMessage({ id: 'autopilot.create' })}
        </Button>
      </div>

      {loading ? (
        <p className="py-12 text-center text-sm text-muted-foreground">
          {intl.formatMessage({ id: 'common.loading' })}
        </p>
      ) : rules.length === 0 ? (
        <Empty
          icon={Workflow}
          variant="dashed"
          title={intl.formatMessage({ id: 'autopilot.empty' })}
        />
      ) : (
        <div className="space-y-3">
          {rules.map((rule) => (
            <Card key={rule.id} className="gap-3 p-5">
              <div className="flex items-start justify-between">
                <div className="flex items-center gap-3">
                  <Switch
                    checked={rule.enabled}
                    onCheckedChange={(v) => handleToggle(rule.id, Boolean(v))}
                    aria-label={rule.name}
                  />
                  <div>
                    <h4 className="font-medium text-foreground">{rule.name}</h4>
                    <div className="mt-1 flex items-center gap-2 text-xs text-muted-foreground">
                      <Badge variant="secondary">
                        {intl.formatMessage({ id: `autopilot.trigger.${rule.trigger_event}` })}
                      </Badge>
                      <span>→</span>
                      <Badge variant="outline">
                        {intl.formatMessage({ id: `autopilot.action.${rule.action.type}` })}
                      </Badge>
                      <span className="text-muted-foreground">({rule.action.agent_id})</span>
                    </div>
                  </div>
                </div>
                <div className="flex items-center gap-1">
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    onClick={() => handleViewHistory(rule.id)}
                    title={intl.formatMessage({ id: 'autopilot.history' })}
                  >
                    <Clock />
                  </Button>
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    onClick={() => setRemoveTarget({ id: rule.id, name: rule.name })}
                    className="text-muted-foreground hover:text-destructive"
                  >
                    <XCircle />
                  </Button>
                </div>
              </div>

              <div className="flex items-center gap-4 text-xs text-muted-foreground">
                <span>{intl.formatMessage({ id: 'autopilot.triggerCount' }, { count: rule.trigger_count })}</span>
                {rule.last_triggered_at && (
                  <span>
                    {intl.formatMessage({ id: 'autopilot.lastTriggered' })}: {new Date(rule.last_triggered_at).toLocaleString('zh-TW')}
                  </span>
                )}
              </div>
            </Card>
          ))}
        </div>
      )}

      {/* Create Rule Dialog */}
      {showCreate && (
        <AutopilotCreateDialog
          onClose={() => setShowCreate(false)}
          onCreated={() => { setShowCreate(false); fetchRules(); }}
        />
      )}

      {/* History Dialog */}
      {historyRuleId && (
        <AutopilotHistoryDialog
          entries={historyEntries}
          onClose={() => setHistoryRuleId(null)}
        />
      )}

      {/* Remove Confirmation */}
      <ConfirmDialog
        open={!!removeTarget}
        onClose={() => setRemoveTarget(null)}
        onConfirm={handleRemove}
        title={intl.formatMessage({ id: 'autopilot.remove' })}
        message={removeTarget ? intl.formatMessage({ id: 'autopilot.remove.confirm' }, { name: removeTarget.name }) : ''}
        confirmLabel={intl.formatMessage({ id: 'autopilot.remove' })}
      />
    </div>
  );
}

function AutopilotCreateDialog({
  onClose,
  onCreated,
}: {
  onClose: () => void;
  onCreated: () => void;
}) {
  const intl = useIntl();
  const { agents, fetchAgents } = useAgentsStore();
  const [name, setName] = useState('');
  const [triggerEvent, setTriggerEvent] = useState<string>('task_created');
  const [actionType, setActionType] = useState<string>('delegate');
  const [actionAgent, setActionAgent] = useState('');
  const [promptTemplate, setPromptTemplate] = useState('');
  const [skillName, setSkillName] = useState('');
  const [fromStatus, setFromStatus] = useState('');
  const [toStatus, setToStatus] = useState('');
  const [idleMinutes, setIdleMinutes] = useState('30');
  const [cronExpr, setCronExpr] = useState('');
  const [submitting, setSubmitting] = useState(false);

  useEffect(() => { fetchAgents(); }, [fetchAgents]);
  useEffect(() => { if (agents.length > 0 && !actionAgent) setActionAgent(agents[0].name); }, [agents, actionAgent]);

  const triggerEvents = ['task_created', 'task_status_changed', 'channel_message', 'agent_idle', 'schedule'] as const;
  const actionTypes = ['delegate', 'notify', 'run_skill'] as const;
  const statuses = ['todo', 'in_progress', 'done', 'blocked'] as const;

  const handleSubmit = useCallback(async () => {
    if (!name.trim() || !actionAgent) return;
    setSubmitting(true);
    try {
      const conditions: Record<string, unknown> = {};
      if (triggerEvent === 'task_status_changed') {
        if (fromStatus) conditions.from_status = fromStatus;
        if (toStatus) conditions.to_status = toStatus;
      }
      if (triggerEvent === 'agent_idle' && idleMinutes) {
        conditions.idle_minutes = parseInt(idleMinutes, 10);
      }
      if (triggerEvent === 'schedule' && cronExpr) {
        conditions.cron = cronExpr;
      }

      await api.autopilot.create({
        name: name.trim(),
        trigger_event: triggerEvent as typeof triggerEvents[number],
        conditions,
        action: {
          type: actionType as typeof actionTypes[number],
          agent_id: actionAgent,
          ...(actionType === 'delegate' && promptTemplate ? { prompt_template: promptTemplate } : {}),
          ...(actionType === 'run_skill' && skillName ? { skill_name: skillName } : {}),
        },
      });
      onCreated();
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.saveFailed' }, { message: formatError(e) }));
    } finally {
      setSubmitting(false);
    }
  }, [name, triggerEvent, actionType, actionAgent, promptTemplate, skillName, fromStatus, toStatus, idleMinutes, cronExpr, onCreated]);

  const statusOptions = [
    { value: '', label: intl.formatMessage({ id: 'tasks.filter.all' }) },
    ...statuses.map((s) => ({ value: s, label: intl.formatMessage({ id: `tasks.status.${s}` }) })),
  ];

  return (
    <Dialog open onOpenChange={(o) => { if (!o) onClose(); }}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{intl.formatMessage({ id: 'autopilot.create' })}</DialogTitle>
        </DialogHeader>

        <div className="space-y-4">
          <FieldBlock label={intl.formatMessage({ id: 'autopilot.field.name' })}>
            <Input value={name} onChange={(e) => setName(e.target.value)} autoFocus />
          </FieldBlock>

          <DialogSelect
            label={intl.formatMessage({ id: 'autopilot.field.triggerEvent' })}
            value={triggerEvent}
            onChange={setTriggerEvent}
            options={triggerEvents.map((t) => ({ value: t, label: intl.formatMessage({ id: `autopilot.trigger.${t}` }) }))}
          />

          {/* Conditional fields based on trigger type */}
          {triggerEvent === 'task_status_changed' && (
            <div className="grid grid-cols-2 gap-3">
              <DialogSelect
                label={intl.formatMessage({ id: 'autopilot.field.fromStatus' })}
                value={fromStatus}
                onChange={setFromStatus}
                options={statusOptions}
              />
              <DialogSelect
                label={intl.formatMessage({ id: 'autopilot.field.toStatus' })}
                value={toStatus}
                onChange={setToStatus}
                options={statusOptions}
              />
            </div>
          )}

          {triggerEvent === 'agent_idle' && (
            <FieldBlock label={intl.formatMessage({ id: 'autopilot.field.idleMinutes' })}>
              <Input type="number" min={1} value={idleMinutes} onChange={(e) => setIdleMinutes(e.target.value)} />
            </FieldBlock>
          )}

          {triggerEvent === 'schedule' && (
            <FieldBlock label={intl.formatMessage({ id: 'autopilot.field.cron' })}>
              <Input value={cronExpr} onChange={(e) => setCronExpr(e.target.value)} placeholder="0 9 * * 1-5" />
            </FieldBlock>
          )}

          <div className="grid grid-cols-2 gap-3">
            <DialogSelect
              label={intl.formatMessage({ id: 'autopilot.field.action' })}
              value={actionType}
              onChange={setActionType}
              options={actionTypes.map((a) => ({ value: a, label: intl.formatMessage({ id: `autopilot.action.${a}` }) }))}
            />
            <DialogSelect
              label={intl.formatMessage({ id: 'autopilot.field.actionAgent' })}
              value={actionAgent}
              onChange={setActionAgent}
              options={agents.map((a) => ({ value: a.name, label: `${glyphText(a.icon)} ${a.display_name}` }))}
            />
          </div>

          {actionType === 'delegate' && (
            <FieldBlock label={intl.formatMessage({ id: 'autopilot.field.promptTemplate' })}>
              <Textarea
                className="min-h-[80px] resize-y"
                value={promptTemplate}
                onChange={(e) => setPromptTemplate(e.target.value)}
                placeholder="Handle the newly created task: {{task.title}}"
              />
            </FieldBlock>
          )}

          {actionType === 'run_skill' && (
            <FieldBlock label={intl.formatMessage({ id: 'autopilot.field.skillName' })}>
              <Input value={skillName} onChange={(e) => setSkillName(e.target.value)} />
            </FieldBlock>
          )}
        </div>

        <DialogFooter>
          <Button variant="ghost" size="sm" onClick={onClose}>
            {intl.formatMessage({ id: 'common.cancel' })}
          </Button>
          <Button
            variant="brand"
            size="sm"
            onClick={handleSubmit}
            disabled={submitting || !name.trim() || !actionAgent}
          >
            {intl.formatMessage({ id: 'autopilot.create' })}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function AutopilotHistoryDialog({
  entries,
  onClose,
}: {
  entries: ReadonlyArray<AutopilotHistoryEntry>;
  onClose: () => void;
}) {
  const intl = useIntl();
  return (
    <Dialog open onOpenChange={(o) => { if (!o) onClose(); }}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{intl.formatMessage({ id: 'autopilot.history' })}</DialogTitle>
        </DialogHeader>
        <div className="max-h-[400px] space-y-2 overflow-y-auto">
          {entries.length === 0 ? (
            <p className="py-8 text-center text-sm text-muted-foreground">
              {intl.formatMessage({ id: 'autopilot.history.empty' })}
            </p>
          ) : (
            entries.map((entry) => (
              <div
                key={entry.id}
                className="flex items-center justify-between rounded-lg border border-surface-border px-4 py-3"
              >
                <div>
                  <span className="text-sm text-foreground">
                    {new Date(entry.triggered_at).toLocaleString('zh-TW')}
                  </span>
                  {entry.details && (
                    <p className="mt-0.5 text-xs text-muted-foreground">{entry.details}</p>
                  )}
                </div>
                <Badge
                  className={cn(
                    entry.result === 'success'
                      ? 'bg-success/10 text-success'
                      : 'bg-destructive/10 text-destructive',
                  )}
                >
                  {intl.formatMessage({ id: `autopilot.history.${entry.result}` })}
                </Badge>
              </div>
            ))
          )}
        </div>
      </DialogContent>
    </Dialog>
  );
}
