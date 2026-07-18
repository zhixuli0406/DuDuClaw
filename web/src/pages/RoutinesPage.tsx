import { useCallback, useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { Clock, Play, Pause, Trash2, Plus, Pencil, MoreHorizontal } from 'lucide-react';
import { api } from '@/lib/api';
import { useConnectionStore } from '@/stores/connection-store';
import { useAgentsStore } from '@/stores/agents-store';
import { toast, formatError } from '@/lib/toast';
import {
  CollectionPageHeader,
  CollectionPageState,
  Button,
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
  DialogClose,
  DropdownMenu,
  DropdownMenuTrigger,
  DropdownMenuContent,
  DropdownMenuItem,
  ListGridContainer,
  ListGridHeader,
  ListGridHeaderCell,
  ListGridRow,
  ListGridCell,
  ActorAvatar,
} from '@/components/mds';
import { ScheduleBuilder } from '@/components/settings/controls';
import { timeAgo } from '@/lib/format';
import { glyphText } from '@/lib/agent-glyph';

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

/** Column template shared by the routines ListGrid header + rows (spec §4). */
const ROUTINE_COLUMNS =
  'minmax(0,1.4fr) minmax(0,1fr) minmax(0,0.9fr) auto minmax(0,0.8fr) 2.5rem';

/** Agent picker shared by the routine create/edit dialog (MDS Select). */
function AgentSelect({ value, onChange }: { value: string; onChange: (v: string) => void }) {
  const intl = useIntl();
  const { agents, fetchAgents } = useAgentsStore();
  useEffect(() => {
    fetchAgents();
  }, [fetchAgents]);
  const current = agents.find((a) => a.name === value);
  const label = current
    ? `${glyphText(current.icon)} ${current.display_name || current.name}`
    : value || intl.formatMessage({ id: 'settings.system.none' });
  return (
    <Select value={value} onValueChange={(v) => onChange(String(v))}>
      <SelectTrigger className="w-full">
        <SelectValue placeholder={intl.formatMessage({ id: 'settings.cron.agentPick' })}>
          {label}
        </SelectValue>
      </SelectTrigger>
      <SelectContent>
        {value && !agents.some((a) => a.name === value) && (
          <SelectItem value={value}>{value}</SelectItem>
        )}
        {agents.map((a) => (
          <SelectItem key={a.name} value={a.name}>
            {glyphText(a.icon) + ' ' + (a.display_name || a.name)}
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  );
}

/**
 * Create / edit dialog for a routine (MDS Dialog). `task === null` ⇒ create mode
 * (`cron.add`); otherwise edit mode (`cron.update`). Reuses the same
 * `ScheduleBuilder` so the schedule UX is unchanged — same `cron.*` RPCs, no
 * backend change.
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

  const canSave = !!schedule.trim() && (isEdit || !!name.trim());

  const handleSave = async () => {
    if (!canSave) return;
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
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>
            {intl.formatMessage({ id: isEdit ? 'routines.editTitle' : 'routines.addTitle' })}
          </DialogTitle>
        </DialogHeader>

        <div className="space-y-4">
          <div className="space-y-1.5">
            <label htmlFor="routine-name" className="text-xs font-medium text-muted-foreground">
              {intl.formatMessage({ id: 'settings.cron.name' })}
            </label>
            <Input id="routine-name" value={name} onChange={(e) => setName(e.target.value)} />
          </div>

          <div className="space-y-1.5">
            <label className="text-xs font-medium text-muted-foreground">
              {intl.formatMessage({ id: 'settings.cron.schedule' })}
            </label>
            <ScheduleBuilder value={schedule} onChange={setSchedule} />
          </div>

          <div className="space-y-1.5">
            <label className="text-xs font-medium text-muted-foreground">
              {intl.formatMessage({ id: 'settings.cron.agentPick' })}
            </label>
            <AgentSelect value={agent} onChange={setAgent} />
          </div>

          <div className="space-y-1.5">
            <label htmlFor="routine-task" className="text-xs font-medium text-muted-foreground">
              {intl.formatMessage({ id: 'settings.cron.task' })}
            </label>
            <Textarea
              id="routine-task"
              rows={3}
              value={body}
              onChange={(e) => setBody(e.target.value)}
            />
          </div>
        </div>

        <DialogFooter>
          <DialogClose
            render={<Button variant="outline">{intl.formatMessage({ id: 'common.cancel' })}</Button>}
          />
          <Button variant="brand" onClick={handleSave} disabled={saving || !canSave}>
            {saving
              ? intl.formatMessage({ id: 'common.saving' })
              : intl.formatMessage({ id: 'common.save' })}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/** One routine row in the ListGrid. */
function RoutineRow({
  routine,
  busy,
  onToggle,
  onEdit,
  onRemove,
}: {
  routine: Routine;
  busy: boolean;
  onToggle: () => void;
  onEdit: () => void;
  onRemove: () => void;
}) {
  const intl = useIntl();
  return (
    <ListGridRow className="cursor-default">
      <ListGridCell className="gap-2">
        <Clock className="size-4 shrink-0 text-muted-foreground" />
        <span className="truncate text-sm font-medium text-foreground" title={routine.name || routine.id}>
          {routine.name || routine.id}
        </span>
      </ListGridCell>
      <ListGridCell>
        <span className="truncate font-mono text-xs text-muted-foreground">
          {routine.schedule || routine.cron}
        </span>
      </ListGridCell>
      <ListGridCell className="gap-1.5">
        <ActorAvatar actorType="agent" size="sm" name={routine.agent_id} />
        <span className="truncate text-sm text-muted-foreground" title={routine.agent_id}>
          {routine.agent_id}
        </span>
      </ListGridCell>
      <ListGridCell className="justify-center" data-stop-row-nav>
        <Switch
          checked={routine.enabled}
          disabled={busy}
          onCheckedChange={onToggle}
          aria-label={intl.formatMessage({
            id: routine.enabled ? 'routines.pause' : 'routines.resume',
          })}
        />
      </ListGridCell>
      <ListGridCell>
        <span className="truncate text-xs text-muted-foreground">
          {routine.last_run_at ? timeAgo(routine.last_run_at) : '—'}
        </span>
      </ListGridCell>
      <ListGridCell className="justify-end">
        <DropdownMenu>
          <DropdownMenuTrigger
            render={
              <Button
                variant="ghost"
                size="icon-sm"
                aria-label={intl.formatMessage({ id: 'routines.moreActions' })}
                data-stop-row-nav
              />
            }
          >
            <MoreHorizontal />
          </DropdownMenuTrigger>
          <DropdownMenuContent>
            <DropdownMenuItem onClick={onEdit} disabled={busy}>
              <Pencil />
              {intl.formatMessage({ id: 'routines.edit' })}
            </DropdownMenuItem>
            <DropdownMenuItem onClick={onToggle} disabled={busy}>
              {routine.enabled ? <Pause /> : <Play />}
              {intl.formatMessage({ id: routine.enabled ? 'routines.pause' : 'routines.resume' })}
            </DropdownMenuItem>
            <DropdownMenuItem variant="destructive" onClick={onRemove} disabled={busy}>
              <Trash2 />
              {intl.formatMessage({ id: 'routines.remove' })}
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      </ListGridCell>
    </ListGridRow>
  );
}

/**
 * RoutinesPage (`/routines`, Zone B) — the single "例行工作" home (MDS surface).
 * A CollectionPageHeader + a Linear-style ListGrid of scheduled tasks with an
 * enable Switch and a kebab (edit / pause / delete). Create/edit is an MDS
 * Dialog reusing the same `ScheduleBuilder`. Same `cron.*` RPCs throughout — no
 * backend change. The Calm-Glass primitives are gone.
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
    <div className="-mx-4 -mt-4 flex flex-col md:-mx-6 md:-mt-6">
      <CollectionPageHeader
        hideTrigger
        icon={Clock}
        title={intl.formatMessage({ id: 'routines.title' })}
        count={routines.length || undefined}
        description={intl.formatMessage({ id: 'routines.subtitle' })}
        action={
          <Button variant="brand" size="sm" onClick={() => setDialog(null)}>
            <Plus />
            <span className="hidden sm:inline">{intl.formatMessage({ id: 'routines.add' })}</span>
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

      <div className="flex flex-1 flex-col p-4 md:p-6">
        {loading ? (
          <CollectionPageState state="loading" />
        ) : routines.length === 0 ? (
          <CollectionPageState
            state="empty"
            icon={Clock}
            title={intl.formatMessage({ id: 'routines.empty' })}
            description={intl.formatMessage({ id: 'routines.emptyHint' })}
            action={
              <Button variant="brand" size="sm" onClick={() => setDialog(null)}>
                <Plus />
                {intl.formatMessage({ id: 'routines.add' })}
              </Button>
            }
          />
        ) : (
          <div className="overflow-hidden rounded-xl border border-surface-border">
            <ListGridContainer
              columns={ROUTINE_COLUMNS}
              className="!h-auto [&>[aria-hidden]]:hidden"
              header={
                <ListGridHeader>
                  <ListGridHeaderCell>{intl.formatMessage({ id: 'routines.col.name' })}</ListGridHeaderCell>
                  <ListGridHeaderCell>{intl.formatMessage({ id: 'routines.col.schedule' })}</ListGridHeaderCell>
                  <ListGridHeaderCell>{intl.formatMessage({ id: 'routines.col.agent' })}</ListGridHeaderCell>
                  <ListGridHeaderCell className="justify-center">
                    {intl.formatMessage({ id: 'routines.col.enabled' })}
                  </ListGridHeaderCell>
                  <ListGridHeaderCell>{intl.formatMessage({ id: 'routines.col.lastRun' })}</ListGridHeaderCell>
                  <ListGridHeaderCell aria-hidden />
                </ListGridHeader>
              }
            >
              {routines.map((r) => (
                <RoutineRow
                  key={r.id}
                  routine={r}
                  busy={!!busy[r.id]}
                  onToggle={() =>
                    act(
                      r.id,
                      () => (r.enabled ? api.cron.pause(r.id) : api.cron.resume(r.id)),
                      r.enabled ? 'routines.pausedToast' : 'routines.resumedToast',
                    )
                  }
                  onEdit={() => setDialog(r)}
                  onRemove={() => act(r.id, () => api.cron.remove(r.id), 'routines.removedToast')}
                />
              ))}
            </ListGridContainer>
          </div>
        )}
      </div>
    </div>
  );
}
