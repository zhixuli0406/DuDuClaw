import { useEffect, useState, useCallback } from 'react';
import { useIntl } from 'react-intl';
import { api } from '@/lib/api';
import { useAgentsStore } from '@/stores/agents-store';
import { Dialog, FormField, buttonPrimary, buttonSecondary } from '@/components/shared/Dialog';
import { ConfirmDialog, ScheduleBuilder, SettingField, describeCron } from '@/components/settings/controls';
import { toast, formatError } from '@/lib/toast';
import { Card, Button, Badge, EmptyState, controlClass } from '@/components/ui';
import { cn } from '@/lib/utils';
import { Clock, Plus, Pencil } from 'lucide-react';

type CronTaskItem = {
  id?: string;
  name?: string;
  agent_id: string;
  cron?: string;
  schedule?: string;
  task?: string;
  enabled: boolean;
  last_run_at?: string | null;
  last_status?: string | null;
};

/** Backend RPCs identify cron tasks by `id`; `name` is display-only. */
const cronTaskId = (t: CronTaskItem) => t.id ?? t.name ?? '';

/** Localized cron description helper shared by dialog + table. */
function useCronLabels() {
  const intl = useIntl();
  return (cron: string) =>
    describeCron(cron, {
      hourly: (mm) => intl.formatMessage({ id: 'controls.cron.desc.hourly' }, { mm }),
      daily: (time) => intl.formatMessage({ id: 'controls.cron.desc.daily' }, { time }),
      weekly: (day, time) => intl.formatMessage({ id: 'controls.cron.desc.weekly' }, { day, time }),
      interval: (n) => intl.formatMessage({ id: 'controls.cron.desc.interval' }, { n }),
      custom: (raw) => intl.formatMessage({ id: 'controls.cron.desc.custom' }, { raw }),
      weekdays: [0, 1, 2, 3, 4, 5, 6].map((i) => intl.formatMessage({ id: `controls.cron.weekday.${i}` })),
    });
}

function AgentSelect({ value, onChange }: { value: string; onChange: (v: string) => void }) {
  const intl = useIntl();
  const { agents, fetchAgents } = useAgentsStore();
  useEffect(() => { fetchAgents(); }, [fetchAgents]);
  return (
    <select value={value} onChange={(e) => onChange(e.target.value)} className={controlClass}>
      {agents.length === 0 && <option value="">{intl.formatMessage({ id: 'settings.system.none' })}</option>}
      {value && !agents.some((a) => a.name === value) && <option value={value}>{value}</option>}
      {agents.map((a) => (
        <option key={a.name} value={a.name}>{a.display_name || a.name}</option>
      ))}
    </select>
  );
}

function CronEditDialog({
  task,
  onClose,
  onSaved,
}: {
  task: CronTaskItem;
  onClose: () => void;
  onSaved: () => Promise<void>;
}) {
  const intl = useIntl();
  const [name, setName] = useState(task.name ?? '');
  const [schedule, setSchedule] = useState(task.schedule ?? task.cron ?? '0 * * * *');
  const [agent, setAgent] = useState(task.agent_id);
  const [body, setBody] = useState(task.task ?? '');
  const [saving, setSaving] = useState(false);

  const handleSave = async () => {
    setSaving(true);
    try {
      await api.cron.update(cronTaskId(task), {
        name: name.trim() || undefined,
        agent_id: agent.trim() || undefined,
        cron: schedule.trim() || undefined,
        task: body.trim() || undefined,
      });
      toast.success(intl.formatMessage({ id: 'common.saved' }));
      await onSaved();
      onClose();
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.saveFailed' }, { message: formatError(e) }));
    } finally {
      setSaving(false);
    }
  };

  return (
    <Dialog open onClose={onClose} title={intl.formatMessage({ id: 'settings.cron.editTitle' })}>
      <div className="space-y-4">
        <FormField label={intl.formatMessage({ id: 'settings.cron.name' })} htmlFor="cron-edit-name">
          <input id="cron-edit-name" type="text" value={name} onChange={(e) => setName(e.target.value)} className={controlClass} />
        </FormField>
        <SettingField label={intl.formatMessage({ id: 'settings.cron.schedule' })} help={intl.formatMessage({ id: 'settings.cron.schedule.help' })}>
          <ScheduleBuilder value={schedule} onChange={setSchedule} />
        </SettingField>
        <SettingField label={intl.formatMessage({ id: 'settings.cron.agentPick' })}>
          <AgentSelect value={agent} onChange={setAgent} />
        </SettingField>
        <FormField label={intl.formatMessage({ id: 'settings.cron.task' })} htmlFor="cron-edit-task">
          <textarea id="cron-edit-task" rows={3} value={body} onChange={(e) => setBody(e.target.value)} className={cn(controlClass, 'h-auto py-2')} />
        </FormField>
        <div className="flex justify-end gap-2 pt-1">
          <button onClick={onClose} className={buttonSecondary}>
            {intl.formatMessage({ id: 'common.cancel' })}
          </button>
          <button onClick={handleSave} disabled={saving || !schedule.trim()} className={buttonPrimary}>
            {saving ? intl.formatMessage({ id: 'common.saving' }) : intl.formatMessage({ id: 'common.save' })}
          </button>
        </div>
      </div>
    </Dialog>
  );
}

export function CronTab() {
  const intl = useIntl();
  const describe = useCronLabels();
  const [tasks, setTasks] = useState<ReadonlyArray<CronTaskItem>>([]);
  const [showAdd, setShowAdd] = useState(false);
  const [editing, setEditing] = useState<CronTaskItem | null>(null);
  const [removing, setRemoving] = useState<CronTaskItem | null>(null);
  const [removeBusy, setRemoveBusy] = useState(false);
  const [newName, setNewName] = useState('');
  const [newSchedule, setNewSchedule] = useState('0 * * * *');
  const [newAgent, setNewAgent] = useState('');
  const [newTask, setNewTask] = useState('');
  const [adding, setAdding] = useState(false);

  const reportError = useCallback(
    (e: unknown) => {
      toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
    },
    [intl]
  );

  const fetchTasks = useCallback(async () => {
    try {
      const result = await api.cron.list();
      setTasks(result?.tasks ?? []);
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
    }
  }, [intl]);

  useEffect(() => {
    fetchTasks();
  }, [fetchTasks]);

  const handleAdd = async () => {
    if (!newName.trim()) return;
    setAdding(true);
    try {
      await api.cron.add({
        name: newName.trim(),
        agent_id: newAgent.trim() || 'default',
        cron: newSchedule.trim(),
        task: newTask.trim() || undefined,
      });
      setShowAdd(false);
      setNewName('');
      setNewSchedule('0 * * * *');
      setNewAgent('');
      setNewTask('');
      await fetchTasks();
    } catch (e) {
      reportError(e);
    } finally {
      setAdding(false);
    }
  };

  const handlePause = async (id: string) => {
    try {
      await api.cron.pause(id);
      await fetchTasks();
    } catch (e) {
      reportError(e);
    }
  };

  const handleResume = async (id: string) => {
    try {
      await api.cron.resume(id);
      await fetchTasks();
    } catch (e) {
      reportError(e);
    }
  };

  const confirmRemove = async () => {
    if (!removing) return;
    setRemoveBusy(true);
    try {
      await api.cron.remove(cronTaskId(removing));
      setRemoving(null);
      await fetchTasks();
    } catch (e) {
      reportError(e);
    } finally {
      setRemoveBusy(false);
    }
  };

  return (
    <Card
      padded={false}
      title={
        <span className="flex items-center gap-2">
          <Clock className="h-4 w-4 text-amber-500" />
          {intl.formatMessage({ id: 'settings.cron' })}
        </span>
      }
      actions={
        <Button variant="primary" size="sm" icon={Plus} onClick={() => setShowAdd(!showAdd)}>
          {intl.formatMessage({ id: 'settings.cron.add' })}
        </Button>
      }
    >
      <p className="px-5 pt-4 text-sm text-stone-500 dark:text-stone-400">
        {intl.formatMessage({ id: 'settings.cron.desc' })}
      </p>

      {/* Edit dialog */}
      {editing && (
        <CronEditDialog
          task={editing}
          onClose={() => setEditing(null)}
          onSaved={fetchTasks}
        />
      )}

      {/* Remove confirmation */}
      <ConfirmDialog
        open={!!removing}
        onClose={() => setRemoving(null)}
        onConfirm={confirmRemove}
        busy={removeBusy}
        title={intl.formatMessage({ id: 'settings.cron.removeConfirmTitle' })}
        message={intl.formatMessage(
          { id: 'settings.cron.removeConfirmMsg' },
          { name: removing?.name ?? removing?.id ?? '' },
        )}
      />

      {/* Add task form */}
      {showAdd && (
        <div className="m-5 mb-0 rounded-xl bg-amber-500/8 p-4 ring-1 ring-inset ring-amber-500/20">
          <div className="space-y-3">
            <SettingField label={intl.formatMessage({ id: 'settings.cron.name' })}>
              <input type="text" value={newName} onChange={(e) => setNewName(e.target.value)} className={controlClass} />
            </SettingField>
            <SettingField label={intl.formatMessage({ id: 'settings.cron.schedule' })} help={intl.formatMessage({ id: 'settings.cron.schedule.help' })}>
              <ScheduleBuilder value={newSchedule} onChange={setNewSchedule} />
            </SettingField>
            <SettingField label={intl.formatMessage({ id: 'settings.cron.agentPick' })}>
              <AgentSelect value={newAgent} onChange={setNewAgent} />
            </SettingField>
            <SettingField label={intl.formatMessage({ id: 'settings.cron.task' })}>
              <textarea
                rows={2}
                value={newTask}
                onChange={(e) => setNewTask(e.target.value)}
                className={cn(controlClass, 'h-auto w-full py-2')}
              />
            </SettingField>
          </div>
          <div className="mt-3 flex justify-end gap-2">
            <Button variant="secondary" size="sm" onClick={() => setShowAdd(false)}>
              {intl.formatMessage({ id: 'common.cancel' })}
            </Button>
            <Button variant="primary" size="sm" onClick={handleAdd} disabled={adding || !newName.trim()}>
              {adding ? intl.formatMessage({ id: 'common.saving' }) : intl.formatMessage({ id: 'common.save' })}
            </Button>
          </div>
        </div>
      )}

      {tasks.length === 0 ? (
        <EmptyState
          icon={Clock}
          dudu="idle"
          title={intl.formatMessage({ id: 'common.noData' })}
        />
      ) : (
        <div className="overflow-x-auto px-5 pb-2">
          <table className="w-full text-sm">
            <thead>
              <tr className="border-b border-[var(--panel-border)]">
                <th className="py-2 text-left font-medium text-stone-500 dark:text-stone-400">
                  {intl.formatMessage({ id: 'settings.cron.name' })}
                </th>
                <th className="py-2 text-left font-medium text-stone-500 dark:text-stone-400">
                  {intl.formatMessage({ id: 'settings.cron.agentPick' })}
                </th>
                <th className="py-2 text-left font-medium text-stone-500 dark:text-stone-400">
                  {intl.formatMessage({ id: 'settings.cron.schedule' })}
                </th>
                <th className="py-2 text-center font-medium text-stone-500 dark:text-stone-400">
                  {intl.formatMessage({ id: 'settings.cron.enabled' })}
                </th>
                <th className="py-2 text-right font-medium text-stone-500 dark:text-stone-400" />
              </tr>
            </thead>
            <tbody>
              {tasks.map((task) => {
                const taskId = cronTaskId(task);
                const taskLabel = task.name ?? task.id ?? '';
                const taskCron = task.schedule ?? task.cron ?? '';
                return (
                  <tr
                    key={taskId}
                    className="border-b border-[var(--panel-border)] last:border-0"
                  >
                    <td className="py-2 text-stone-700 dark:text-stone-300">
                      {taskLabel}
                    </td>
                    <td className="py-2 text-stone-700 dark:text-stone-300">
                      {task.agent_id}
                    </td>
                    <td className="py-2">
                      <span className="text-stone-600 dark:text-stone-400">{describe(taskCron)}</span>
                      <code className="ml-2 rounded bg-stone-500/10 px-1.5 py-0.5 font-mono text-[11px] text-stone-400 dark:text-stone-500">
                        {taskCron}
                      </code>
                    </td>
                    <td className="py-2 text-center">
                      {task.enabled ? (
                        <Badge tone="success">{intl.formatMessage({ id: 'settings.cron.enabled' })}</Badge>
                      ) : (
                        <Badge tone="neutral">{intl.formatMessage({ id: 'settings.cron.disabled' })}</Badge>
                      )}
                    </td>
                    <td className="py-2 text-right">
                      <div className="flex justify-end gap-1">
                        <Button variant="ghost" size="sm" icon={Pencil} onClick={() => setEditing(task)}>
                          {intl.formatMessage({ id: 'common.edit' })}
                        </Button>
                        {task.enabled ? (
                          <button
                            onClick={() => handlePause(taskId)}
                            className="rounded px-2 py-1 text-xs text-amber-600 hover:bg-amber-500/10 dark:text-amber-400"
                          >
                            {intl.formatMessage({ id: 'settings.cron.pause' })}
                          </button>
                        ) : (
                          <button
                            onClick={() => handleResume(taskId)}
                            className="rounded px-2 py-1 text-xs text-emerald-600 hover:bg-emerald-500/10 dark:text-emerald-400"
                          >
                            {intl.formatMessage({ id: 'settings.cron.resume' })}
                          </button>
                        )}
                        <button
                          onClick={() => setRemoving(task)}
                          className="rounded px-2 py-1 text-xs text-rose-600 hover:bg-rose-500/10 dark:text-rose-400"
                        >
                          {intl.formatMessage({ id: 'settings.cron.remove' })}
                        </button>
                      </div>
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      )}
    </Card>
  );
}
