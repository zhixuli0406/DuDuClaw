import { useCallback, useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { CalendarClock, Play, Pause, Trash2, Clock } from 'lucide-react';
import { api } from '@/lib/api';
import { useConnectionStore } from '@/stores/connection-store';
import { toast, formatError } from '@/lib/toast';
import { Page, PageHeader, Card, Badge, Button, EmptyState, SkeletonList, Mono, CharacterAvatar } from '@/components/ui';
import { timeAgo } from '@/lib/format';

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

/**
 * RoutinesPage (`/routines`, Zone B) — the "例行工作" surface promoted out of the
 * former SettingsPage cron tab into a first-class page (dashboard-redesign
 * §3.2 / WP4-T4.3). Lists scheduled tasks with pause/resume/remove. Behaviour
 * preserved: same `cron.*` RPCs, no backend change.
 */
export function RoutinesPage() {
  const intl = useIntl();
  const connectionState = useConnectionStore((s) => s.state);
  const [routines, setRoutines] = useState<Routine[]>([]);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState<Record<string, boolean>>({});

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
      />

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
