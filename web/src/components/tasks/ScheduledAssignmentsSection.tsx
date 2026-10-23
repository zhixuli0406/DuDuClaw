import { useCallback, useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { CalendarClock, Pause, Play, Trash2 } from 'lucide-react';

import { api, type AssignScheduleInfo } from '@/lib/api';
import { client } from '@/lib/ws-client';
import { describeSchedule, formatWhen, scheduleErrorText } from '@/lib/assign-schedule';
import { toast } from '@/lib/toast';
import { useAgentsStore } from '@/stores/agents-store';
import { Badge, Button, Card, CardContent, Spinner, Switch } from '@/components/mds';

/**
 * ScheduledAssignmentsSection — the 排程 part of `/goals` → 我交辦的: the
 * caller's scheduled assignments (an Admin may switch to everyone's), with
 * pause / resume / remove. The gateway checks the creator's right again
 * before every run and pauses the schedule when it is gone.
 */
export function ScheduledAssignmentsSection({ enabled, isAdmin }: { enabled: boolean; isAdmin: boolean }) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) =>
    intl.formatMessage({ id: `assignSchedule.${id}` }, values);
  const agents = useAgentsStore((s) => s.agents);
  const [rows, setRows] = useState<AssignScheduleInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [failed, setFailed] = useState(false);
  const [limitsUnavailable, setLimitsUnavailable] = useState(false);
  const [all, setAll] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  const [confirmRemove, setConfirmRemove] = useState<string | null>(null);
  const confirmTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const load = useCallback(async () => {
    if (!enabled) return;
    try {
      const r = await api.assignSchedules.list(all && isAdmin ? 'all' : 'mine');
      setRows(r.schedules ?? []);
      setLimitsUnavailable(r.max_active === null || r.min_interval_minutes === null);
      setFailed(false);
    } catch {
      setFailed(true);
    } finally {
      setLoading(false);
    }
  }, [enabled, all, isAdmin]);

  useEffect(() => {
    setLoading(true);
    void load();
  }, [load]);
  useEffect(() => {
    if (!enabled) return;
    // The shared assign panel can create a schedule while this view stays
    // mounted. Scheduler fires also change status without a dashboard push.
    const unsubscribe = client.subscribe('assign_schedule.changed', () => void load());
    const timer = setInterval(() => void load(), 30_000);
    return () => { unsubscribe(); clearInterval(timer); };
  }, [enabled, load]);
  useEffect(() => () => {
    if (confirmTimer.current) clearTimeout(confirmTimer.current);
  }, []);

  const act = async (row: AssignScheduleInfo, action: 'pause' | 'resume' | 'remove') => {
    if (action === 'remove' && confirmRemove !== row.id) {
      setConfirmRemove(row.id);
      if (confirmTimer.current) clearTimeout(confirmTimer.current);
      confirmTimer.current = setTimeout(() => setConfirmRemove(null), 4000);
      return;
    }
    setBusy(row.id);
    try {
      await api.assignSchedules[action](row.id);
      toast.success(t(`done.${action}`));
      setConfirmRemove(null);
      await load();
    } catch (e) {
      toast.error(scheduleErrorText(intl, e));
    } finally {
      setBusy(null);
    }
  };

  if (!enabled) return null;
  const agentName = (id: string) => agents.find((a) => a.name === id)?.display_name || id;

  return (
    <section className="space-y-2" aria-labelledby="assign-schedules-heading">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h2 id="assign-schedules-heading" className="flex items-center gap-1.5 text-sm font-medium text-foreground">
          <CalendarClock className="size-4 text-muted-foreground" />
          {t('heading', { n: rows.length })}
        </h2>
        {isAdmin && (
          <label className="flex items-center gap-2 text-xs text-muted-foreground">
            <Switch checked={all} onCheckedChange={(v) => setAll(Boolean(v))} />
            {t('showAll')}
          </label>
        )}
      </div>
      {limitsUnavailable && <p role="status" className="text-xs text-muted-foreground">{t('limitsUnavailable')}</p>}
      {loading ? (
        <div className="flex justify-center py-4">
          <Spinner />
        </div>
      ) : failed ? (
        <p className="text-xs text-muted-foreground">{t('loadFailed')}</p>
      ) : rows.length === 0 ? (
        <p className="text-xs text-muted-foreground">{t('empty')}</p>
      ) : (
        <Card data-size="sm">
          <CardContent className="divide-y divide-surface-border">
            {rows.map((r) => (
              <div key={r.id} className="flex flex-wrap items-start justify-between gap-3 py-2.5">
                <div className="min-w-0 flex-1 space-y-1">
                  <div className="flex flex-wrap items-center gap-2">
                    <span className="text-sm font-medium">{agentName(r.agent_id)}</span>
                    <span className="text-sm text-muted-foreground">{describeSchedule(intl, r.schedule, r.timezone)}</span>
                    {r.status !== 'active' && (
                      <Badge variant={r.status === 'paused' && r.pause_reason !== 'by_user' ? 'destructive' : 'secondary'}>
                        {r.status === 'paused' ? t(`paused.${r.pause_reason ?? 'by_user'}`) : t('status.done')}
                      </Badge>
                    )}
                  </div>
                  <p className="truncate text-sm text-foreground" title={r.title}>
                    {r.title}
                  </p>
                  <p className="text-xs text-muted-foreground">
                    {r.status === 'active' && t('next', { at: formatWhen(intl, r.next_fire_at, r.timezone) })}
                    {r.status === 'active' && r.last_fire_at && ' · '}
                    {r.last_fire_at && t('last', { at: formatWhen(intl, r.last_fire_at, r.timezone), n: r.fire_count })}
                    {r.creator_name && ` · ${t('creator', { name: r.creator_name })}`}
                  </p>
                </div>
                <div className="flex items-center gap-1">
                  {r.status === 'active' && (
                    <Button size="sm" variant="outline" disabled={busy === r.id} onClick={() => void act(r, 'pause')}>
                      <Pause />
                      {t('action.pause')}
                    </Button>
                  )}
                  {r.status === 'paused' && (
                    <Button size="sm" variant="outline" disabled={busy === r.id || limitsUnavailable} onClick={() => void act(r, 'resume')}>
                      <Play />
                      {t('action.resume')}
                    </Button>
                  )}
                  <Button
                    size="sm"
                    variant={confirmRemove === r.id ? 'destructive' : 'ghost'}
                    disabled={busy === r.id}
                    aria-label={t(confirmRemove === r.id ? 'action.confirmRemove' : 'action.remove')}
                    onClick={() => void act(r, 'remove')}
                  >
                    <Trash2 />
                    {confirmRemove === r.id && t('action.confirmRemove')}
                  </Button>
                </div>
              </div>
            ))}
          </CardContent>
        </Card>
      )}
    </section>
  );
}
