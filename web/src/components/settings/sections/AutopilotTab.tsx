import { useEffect, useState, useCallback } from 'react';
import { useIntl } from 'react-intl';
import { cn } from '@/lib/utils';
import { useAgentsStore } from '@/stores/agents-store';
import { api, type AutopilotRule, type AutopilotHistoryEntry } from '@/lib/api';
import { Dialog } from '@/components/shared/Dialog';
import { toast, formatError } from '@/lib/toast';
import { Card, Section, Button, Badge, EmptyState } from '@/components/ui';
import { Plus, Clock, XCircle, Workflow } from 'lucide-react';

// ── Autopilot Tab ───────────────────────────────────────────

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
      <p className="rounded-lg bg-stone-500/5 px-4 py-3 text-sm text-stone-500 dark:bg-white/5 dark:text-stone-400">
        {intl.formatMessage({ id: 'settings.autopilot.desc' })}
      </p>
      <Section
        title={intl.formatMessage({ id: 'autopilot.title' })}
        description={intl.formatMessage({ id: 'autopilot.subtitle' })}
        actions={
          <Button variant="primary" icon={Plus} onClick={() => setShowCreate(true)}>
            {intl.formatMessage({ id: 'autopilot.create' })}
          </Button>
        }
      >
        {loading ? (
          <div className="py-12 text-center text-stone-400">
            {intl.formatMessage({ id: 'common.loading' })}
          </div>
        ) : rules.length === 0 ? (
          <Card padded={false}>
            <EmptyState
              icon={Workflow}
              dudu="idle"
              title={intl.formatMessage({ id: 'autopilot.empty' })}
            />
          </Card>
        ) : (
        <div className="space-y-3">
          {rules.map((rule) => (
            <Card key={rule.id} className="p-5" padded={false}>
              <div className="flex items-start justify-between">
                <div className="flex items-center gap-3">
                  <button
                    onClick={() => handleToggle(rule.id, !rule.enabled)}
                    className={cn(
                      'relative h-6 w-11 rounded-full transition-colors',
                      rule.enabled ? 'bg-emerald-500' : 'bg-stone-300 dark:bg-stone-600',
                    )}
                  >
                    <span
                      className={cn(
                        'absolute top-0.5 h-5 w-5 rounded-full bg-white shadow-sm transition-transform',
                        rule.enabled ? 'left-[22px]' : 'left-0.5',
                      )}
                    />
                  </button>
                  <div>
                    <h4 className="font-medium text-stone-900 dark:text-stone-50">{rule.name}</h4>
                    <div className="mt-1 flex items-center gap-2 text-xs text-stone-500 dark:text-stone-400">
                      <Badge tone="info">
                        {intl.formatMessage({ id: `autopilot.trigger.${rule.trigger_event}` })}
                      </Badge>
                      <span>→</span>
                      <Badge tone="accent">
                        {intl.formatMessage({ id: `autopilot.action.${rule.action.type}` })}
                      </Badge>
                      <span className="text-stone-400">({rule.action.agent_id})</span>
                    </div>
                  </div>
                </div>
                <div className="flex items-center gap-2">
                  <button
                    onClick={() => handleViewHistory(rule.id)}
                    className="rounded-lg p-1.5 text-stone-400 transition-colors hover:bg-stone-500/10 hover:text-stone-600 dark:hover:text-stone-300"
                    title={intl.formatMessage({ id: 'autopilot.history' })}
                  >
                    <Clock className="h-4 w-4" />
                  </button>
                  <button
                    onClick={() => setRemoveTarget({ id: rule.id, name: rule.name })}
                    className="rounded-lg p-1.5 text-stone-400 transition-colors hover:bg-rose-500/10 hover:text-rose-500"
                  >
                    <XCircle className="h-4 w-4" />
                  </button>
                </div>
              </div>

              <div className="mt-3 flex items-center gap-4 text-xs text-stone-400 dark:text-stone-500">
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
      </Section>

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
      {removeTarget && (
        <Dialog open onClose={() => setRemoveTarget(null)} title={intl.formatMessage({ id: 'autopilot.remove' })}>
          <div className="space-y-4">
            <p className="text-sm text-stone-600 dark:text-stone-400">
              {intl.formatMessage({ id: 'autopilot.remove.confirm' }, { name: removeTarget.name })}
            </p>
            <div className="flex justify-end gap-3">
              <button
                onClick={() => setRemoveTarget(null)}
                className="rounded-lg border border-stone-300 px-4 py-2 text-sm font-medium text-stone-700 transition-colors hover:bg-stone-50 dark:border-stone-600 dark:text-stone-300 dark:hover:bg-stone-800"
              >
                {intl.formatMessage({ id: 'common.cancel' })}
              </button>
              <button
                onClick={handleRemove}
                className="rounded-lg bg-rose-500 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-rose-600"
              >
                {intl.formatMessage({ id: 'autopilot.remove' })}
              </button>
            </div>
          </div>
        </Dialog>
      )}
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

  const inputCls = 'w-full rounded-lg border border-stone-300 bg-white px-3 py-2 text-sm text-stone-900 focus:border-amber-500 focus:outline-none focus:ring-2 focus:ring-amber-500/20 dark:border-stone-600 dark:bg-stone-800 dark:text-stone-50';
  const selectCls = inputCls;

  return (
    <Dialog open onClose={onClose} title={intl.formatMessage({ id: 'autopilot.create' })} className="max-w-lg">
      <div className="space-y-4">
        <div className="space-y-1.5">
          <label className="block text-sm font-medium text-stone-700 dark:text-stone-300">
            {intl.formatMessage({ id: 'autopilot.field.name' })}
          </label>
          <input className={inputCls} value={name} onChange={(e) => setName(e.target.value)} autoFocus />
        </div>

        <div className="space-y-1.5">
          <label className="block text-sm font-medium text-stone-700 dark:text-stone-300">
            {intl.formatMessage({ id: 'autopilot.field.triggerEvent' })}
          </label>
          <select className={selectCls} value={triggerEvent} onChange={(e) => setTriggerEvent(e.target.value)}>
            {triggerEvents.map((t) => (
              <option key={t} value={t}>{intl.formatMessage({ id: `autopilot.trigger.${t}` })}</option>
            ))}
          </select>
        </div>

        {/* Conditional fields based on trigger type */}
        {triggerEvent === 'task_status_changed' && (
          <div className="grid grid-cols-2 gap-3">
            <div className="space-y-1.5">
              <label className="block text-sm font-medium text-stone-700 dark:text-stone-300">
                {intl.formatMessage({ id: 'autopilot.field.fromStatus' })}
              </label>
              <select className={selectCls} value={fromStatus} onChange={(e) => setFromStatus(e.target.value)}>
                <option value="">{intl.formatMessage({ id: 'tasks.filter.all' })}</option>
                {statuses.map((s) => (
                  <option key={s} value={s}>{intl.formatMessage({ id: `tasks.status.${s}` })}</option>
                ))}
              </select>
            </div>
            <div className="space-y-1.5">
              <label className="block text-sm font-medium text-stone-700 dark:text-stone-300">
                {intl.formatMessage({ id: 'autopilot.field.toStatus' })}
              </label>
              <select className={selectCls} value={toStatus} onChange={(e) => setToStatus(e.target.value)}>
                <option value="">{intl.formatMessage({ id: 'tasks.filter.all' })}</option>
                {statuses.map((s) => (
                  <option key={s} value={s}>{intl.formatMessage({ id: `tasks.status.${s}` })}</option>
                ))}
              </select>
            </div>
          </div>
        )}

        {triggerEvent === 'agent_idle' && (
          <div className="space-y-1.5">
            <label className="block text-sm font-medium text-stone-700 dark:text-stone-300">
              {intl.formatMessage({ id: 'autopilot.field.idleMinutes' })}
            </label>
            <input className={inputCls} type="number" min="1" value={idleMinutes} onChange={(e) => setIdleMinutes(e.target.value)} />
          </div>
        )}

        {triggerEvent === 'schedule' && (
          <div className="space-y-1.5">
            <label className="block text-sm font-medium text-stone-700 dark:text-stone-300">
              {intl.formatMessage({ id: 'autopilot.field.cron' })}
            </label>
            <input className={inputCls} value={cronExpr} onChange={(e) => setCronExpr(e.target.value)} placeholder="0 9 * * 1-5" />
          </div>
        )}

        <div className="grid grid-cols-2 gap-3">
          <div className="space-y-1.5">
            <label className="block text-sm font-medium text-stone-700 dark:text-stone-300">
              {intl.formatMessage({ id: 'autopilot.field.action' })}
            </label>
            <select className={selectCls} value={actionType} onChange={(e) => setActionType(e.target.value)}>
              {actionTypes.map((a) => (
                <option key={a} value={a}>{intl.formatMessage({ id: `autopilot.action.${a}` })}</option>
              ))}
            </select>
          </div>
          <div className="space-y-1.5">
            <label className="block text-sm font-medium text-stone-700 dark:text-stone-300">
              {intl.formatMessage({ id: 'autopilot.field.actionAgent' })}
            </label>
            <select className={selectCls} value={actionAgent} onChange={(e) => setActionAgent(e.target.value)}>
              {agents.map((a) => (
                <option key={a.name} value={a.name}>{a.icon || '🤖'} {a.display_name}</option>
              ))}
            </select>
          </div>
        </div>

        {actionType === 'delegate' && (
          <div className="space-y-1.5">
            <label className="block text-sm font-medium text-stone-700 dark:text-stone-300">
              {intl.formatMessage({ id: 'autopilot.field.promptTemplate' })}
            </label>
            <textarea
              className={cn(inputCls, 'min-h-[80px] resize-y')}
              value={promptTemplate}
              onChange={(e) => setPromptTemplate(e.target.value)}
              placeholder="Handle the newly created task: {{task.title}}"
            />
          </div>
        )}

        {actionType === 'run_skill' && (
          <div className="space-y-1.5">
            <label className="block text-sm font-medium text-stone-700 dark:text-stone-300">
              {intl.formatMessage({ id: 'autopilot.field.skillName' })}
            </label>
            <input className={inputCls} value={skillName} onChange={(e) => setSkillName(e.target.value)} />
          </div>
        )}

        <div className="flex justify-end gap-3 pt-2">
          <button
            onClick={onClose}
            className="rounded-lg border border-stone-300 px-4 py-2 text-sm font-medium text-stone-700 transition-colors hover:bg-stone-50 dark:border-stone-600 dark:text-stone-300 dark:hover:bg-stone-800"
          >
            {intl.formatMessage({ id: 'common.cancel' })}
          </button>
          <button
            onClick={handleSubmit}
            disabled={submitting || !name.trim() || !actionAgent}
            className="rounded-lg bg-amber-500 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-amber-600 disabled:opacity-50"
          >
            {intl.formatMessage({ id: 'autopilot.create' })}
          </button>
        </div>
      </div>
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
    <Dialog open onClose={onClose} title={intl.formatMessage({ id: 'autopilot.history' })}>
      <div className="max-h-[400px] space-y-2 overflow-y-auto">
        {entries.length === 0 ? (
          <p className="py-8 text-center text-sm text-stone-400 dark:text-stone-500">
            {intl.formatMessage({ id: 'autopilot.history.empty' })}
          </p>
        ) : (
          entries.map((entry) => (
            <div
              key={entry.id}
              className="flex items-center justify-between rounded-lg border border-stone-200 px-4 py-3 dark:border-stone-700"
            >
              <div>
                <span className="text-sm text-stone-700 dark:text-stone-300">
                  {new Date(entry.triggered_at).toLocaleString('zh-TW')}
                </span>
                {entry.details && (
                  <p className="mt-0.5 text-xs text-stone-400 dark:text-stone-500">{entry.details}</p>
                )}
              </div>
              <span
                className={cn(
                  'rounded-full px-2 py-0.5 text-xs font-medium',
                  entry.result === 'success'
                    ? 'bg-emerald-100 text-emerald-700 dark:bg-emerald-900/30 dark:text-emerald-400'
                    : 'bg-rose-100 text-rose-700 dark:bg-rose-900/30 dark:text-rose-400',
                )}
              >
                {intl.formatMessage({ id: `autopilot.history.${entry.result}` })}
              </span>
            </div>
          ))
        )}
      </div>
    </Dialog>
  );
}
