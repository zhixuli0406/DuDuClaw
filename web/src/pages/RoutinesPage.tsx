import { useCallback, useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { CalendarClock, Play, Pause, Trash2, Clock, Plus, Pencil } from 'lucide-react';
import { api } from '@/lib/api';
import { useConnectionStore } from '@/stores/connection-store';
import { useAgentsStore } from '@/stores/agents-store';
import { toast, formatError } from '@/lib/toast';
import { Page, PageHeader, Card, Badge, Button, EmptyState, SkeletonList, Mono, CharacterAvatar, controlClass } from '@/components/ui';
import { Dialog, FormField } from '@/components/shared/Dialog';
import { ScheduleBuilder, SettingField } from '@/components/settings/controls';
import { timeAgo } from '@/lib/format';
import { cn } from '@/lib/utils';

interface Routine {
  id: string;
  name?: string;
  agent_id: string;
  cron: string;
  schedule?: string;
  task?: string;
  enabled: boolean;
  last_run_at?: string | null;
  last_status?: string | null;
}

/** Agent picker shared by the routine create/edit dialog. */
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

/**
 * Create / edit dialog for a routine. `task === null` ⇒ create mode
 * (`cron.add`); otherwise edit mode (`cron.update`). Reuses the same
 * `ScheduleBuilder` the former settings tab used, so the schedule UX is
 * unchanged — this is the create/edit surface unified onto the routines page.
 */
function RoutineFormDialog({
  task,
  onClose,
  onSaved,
}: {
  task: Routine | null;
  onClose: () => void;
  onSaved: () => Promise<void>;
}) {
  const intl = useIntl();
  const isEdit = !!task;
  const [name, setName] = useState(task?.name ?? '');
  const [schedule, setSchedule] = useState(task?.schedule ?? task?.cron ?? '0 * * * *');
  const [agent, setAgent] = useState(task?.agent_id ?? '');
  const [body, setBody] = useState(task?.task ?? '');
  const [saving, setSaving] = useState(false);

  const handleSave = async () => {
    // Create requires a name; edit may leave fields blank (⇒ unchanged).
    if (!schedule.trim() || (!isEdit && !name.trim())) return;
    setSaving(true);
    try {
      if (isEdit && task) {
        await api.cron.update(task.id, {
          name: name.trim() || undefined,
          agent_id: agent.trim() || undefined,
          cron: schedule.trim() || undefined,
          task: body.trim() || undefined,
        });
        toast.success(intl.formatMessage({ id: 'routines.savedToast' }));
      } else {
        await api.cron.add({
          name: name.trim(),
          agent_id: agent.trim() || 'default',
          cron: schedule.trim(),
          task: body.trim() || undefined,
        });
        toast.success(intl.formatMessage({ id: 'routines.addedToast' }));
      }
      await onSaved();
      onClose();
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.saveFailed' }, { message: formatError(e) }));
    } finally {
      setSaving(false);
    }
  };

  return (
    <Dialog
      open
      onClose={onClose}
      title={intl.formatMessage({ id: isEdit ? 'routines.editTitle' : 'routines.addTitle' })}
    >
      <div className="space-y-4">
        <FormField label={intl.formatMessage({ id: 'settings.cron.name' })} htmlFor="routine-name">
          <input id="routine-name" type="text" value={name} onChange={(e) => setName(e.target.value)} className={controlClass} />
        </FormField>
        <SettingField label={intl.formatMessage({ id: 'settings.cron.schedule' })} help={intl.formatMessage({ id: 'settings.cron.schedule.help' })}>
          <ScheduleBuilder value={schedule} onChange={setSchedule} />
        </SettingField>
        <SettingField label={intl.formatMessage({ id: 'settings.cron.agentPick' })}>
          <AgentSelect value={agent} onChange={setAgent} />
        </SettingField>
        <FormField label={intl.formatMessage({ id: 'settings.cron.task' })} htmlFor="routine-task">
          <textarea id="routine-task" rows={3} value={body} onChange={(e) => setBody(e.target.value)} className={cn(controlClass, 'h-auto py-2')} />
        </FormField>
        <div className="flex justify-end gap-2 pt-1">
          <Button variant="secondary" onClick={onClose}>
            {intl.formatMessage({ id: 'common.cancel' })}
          </Button>
          <Button
            variant="primary"
            onClick={handleSave}
            disabled={saving || !schedule.trim() || (!isEdit && !name.trim())}
          >
            {saving ? intl.formatMessage({ id: 'common.saving' }) : intl.formatMessage({ id: 'common.save' })}
          </Button>
        </div>
      </div>
    </Dialog>
  );
}

/**
 * RoutinesPage (`/routines`, Zone B) — the single "例行工作" home. Lists scheduled
 * tasks with pause/resume/remove AND their create/edit (the former SettingsPage
 * cron/排程任務 tab was unified here so all routine settings live in one place).
 * Same `cron.*` RPCs throughout, no backend change.
 */
export function RoutinesPage() {
  const intl = useIntl();
  const connectionState = useConnectionStore((s) => s.state);
  const [routines, setRoutines] = useState<Routine[]>([]);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState<Record<string, boolean>>({});
  // `undefined` = dialog closed; `null` = create; a Routine = edit that task.
  const [dialog, setDialog] = useState<Routine | null | undefined>(undefined);

  const load = useCallback(async () => {
    try {
      const res = await api.cron.list();
      setRoutines(res?.tasks ?? []);
    } catch (e) {
      console.warn('[api]', e);
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
      setRoutines([]);
    } finally {
      setLoading(false);
    }
  }, [intl]);

  useEffect(() => {
    if (connectionState !== 'authenticated') return;
    setLoading(true);
    load();
  }, [connectionState, load]);

  const act = useCallback(
    async (id: string, fn: () => Promise<unknown>, successId: string) => {
      setBusy((p) => ({ ...p, [id]: true }));
      try {
        await fn();
        toast.success(intl.formatMessage({ id: successId }));
        await load();
      } catch (e) {
        console.warn('[api]', e);
        toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
      } finally {
        setBusy((p) => {
          const next = { ...p };
          delete next[id];
          return next;
        });
      }
    },
    [intl, load],
  );

  return (
    <Page>
      <PageHeader
        icon={CalendarClock}
        title={intl.formatMessage({ id: 'routines.title' })}
        subtitle={intl.formatMessage({ id: 'routines.subtitle' })}
        actions={
          <Button variant="primary" icon={Plus} onClick={() => setDialog(null)}>
            {intl.formatMessage({ id: 'routines.add' })}
          </Button>
        }
      />

      {dialog !== undefined && (
        <RoutineFormDialog
          task={dialog}
          onClose={() => setDialog(undefined)}
          onSaved={load}
        />
      )}

      {loading ? (
        <Card padded={false}>
          <div className="p-5">
            <SkeletonList rows={3} rowClassName="h-16" />
          </div>
        </Card>
      ) : routines.length === 0 ? (
        <Card>
          <EmptyState
            icon={CalendarClock}
            dudu="sleep"
            title={intl.formatMessage({ id: 'routines.empty' })}
            hint={intl.formatMessage({ id: 'routines.emptyHint' })}
          />
        </Card>
      ) : (
        <div className="space-y-3">
          {routines.map((r) => {
            const b = !!busy[r.id];
            return (
              <Card key={r.id}>
                <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
                  <div className="min-w-0">
                    <div className="flex flex-wrap items-center gap-2">
                      <span className="truncate font-medium text-stone-900 dark:text-stone-50">
                        {r.name || r.id}
                      </span>
                      <Badge tone={r.enabled ? 'success' : 'neutral'}>
                        {intl.formatMessage({ id: r.enabled ? 'routines.active' : 'routines.paused' })}
                      </Badge>
                    </div>
                    <div className="mt-1 flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-stone-400 dark:text-stone-500">
                      <span className="flex items-center gap-1">
                        <CalendarClock className="h-3 w-3" />
                        <Mono>{r.schedule || r.cron}</Mono>
                      </span>
                      <span className="flex items-center gap-1">
                        <CharacterAvatar agentId={r.agent_id} name={r.agent_id} size={20} />
                        {r.agent_id}
                      </span>
                      {r.last_run_at && (
                        <span className="flex items-center gap-1">
                          <Clock className="h-3 w-3" />
                          {intl.formatMessage({ id: 'routines.lastRun' })} <Mono>{timeAgo(r.last_run_at)}</Mono>
                          {r.last_status ? ` · ${r.last_status}` : ''}
                        </span>
                      )}
                    </div>
                  </div>
                  <div className="flex shrink-0 items-center gap-2">
                    <Button size="sm" variant="secondary" icon={Pencil} disabled={b}
                      onClick={() => setDialog(r)}>
                      {intl.formatMessage({ id: 'routines.edit' })}
                    </Button>
                    {r.enabled ? (
                      <Button size="sm" variant="secondary" icon={Pause} disabled={b}
                        onClick={() => act(r.id, () => api.cron.pause(r.id), 'routines.pausedToast')}>
                        {intl.formatMessage({ id: 'routines.pause' })}
                      </Button>
                    ) : (
                      <Button size="sm" variant="primary" icon={Play} disabled={b}
                        onClick={() => act(r.id, () => api.cron.resume(r.id), 'routines.resumedToast')}>
                        {intl.formatMessage({ id: 'routines.resume' })}
                      </Button>
                    )}
                    <Button size="sm" variant="danger" icon={Trash2} disabled={b}
                      onClick={() => act(r.id, () => api.cron.remove(r.id), 'routines.removedToast')}>
                      {intl.formatMessage({ id: 'routines.remove' })}
                    </Button>
                  </div>
                </div>
              </Card>
            );
          })}
        </div>
      )}
    </Page>
  );
}
