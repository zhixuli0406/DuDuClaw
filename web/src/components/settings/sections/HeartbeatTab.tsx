import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { api } from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import { Card, EmptyState } from '@/components/ui';
import { Switch, describeCron } from '@/components/settings/controls';
import { HeartPulse, Play } from 'lucide-react';

export function HeartbeatTab() {
  const intl = useIntl();
  const [heartbeats, setHeartbeats] = useState<
    ReadonlyArray<{
      agent_id: string;
      enabled: boolean;
      interval_seconds: number;
      cron: string;
      last_run?: string;
      next_run?: string;
      total_runs: number;
      active_runs: number;
      max_concurrent: number;
    }>
  >([]);

  useEffect(() => {
    api.heartbeat
      .status()
      .then((r) => setHeartbeats(r?.heartbeats ?? []))
      .catch((e) => {
        console.warn("[api]", e);
        toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
      });
    // 15s poll stays silent — transient errors would spam the user; the
    // initial load toast is enough to flag persistent problems.
    const interval = setInterval(() => {
      api.heartbeat
        .status()
        .then((r) => setHeartbeats(r?.heartbeats ?? []))
        .catch((e) => console.warn("[api]", e));
    }, 15_000);
    return () => clearInterval(interval);
  }, [intl]);

  // Plain-language schedule: prefer a cron description, else the interval in secs.
  const describe = (cron: string, intervalSeconds: number) => {
    if (cron) {
      return describeCron(cron, {
        hourly: (mm) => intl.formatMessage({ id: 'controls.cron.desc.hourly' }, { mm }),
        daily: (time) => intl.formatMessage({ id: 'controls.cron.desc.daily' }, { time }),
        weekly: (day, time) => intl.formatMessage({ id: 'controls.cron.desc.weekly' }, { day, time }),
        interval: (n) => intl.formatMessage({ id: 'controls.cron.desc.interval' }, { n }),
        custom: (raw) => intl.formatMessage({ id: 'controls.cron.desc.custom' }, { raw }),
        weekdays: [0, 1, 2, 3, 4, 5, 6].map((i) => intl.formatMessage({ id: `controls.cron.weekday.${i}` })),
      });
    }
    return intl.formatMessage({ id: 'controls.cron.desc.interval' }, { n: Math.round(intervalSeconds / 60) || 1 });
  };

  return (
    <Card
      title={
        <span className="flex items-center gap-2">
          <HeartPulse className="h-4 w-4 text-amber-500" />
          {intl.formatMessage({ id: 'settings.heartbeat' })}
        </span>
      }
    >
      <p className="mb-4 text-sm text-stone-500 dark:text-stone-400">
        {intl.formatMessage({ id: 'settings.heartbeat.desc' })}
      </p>

      {heartbeats.length === 0 ? (
        <EmptyState
          icon={HeartPulse}
          dudu="idle"
          title={intl.formatMessage({ id: 'common.noData' })}
        />
      ) : (
        <div className="space-y-3">
          {heartbeats.map((hb) => (
            <div
              key={hb.agent_id}
              className="flex items-center justify-between rounded-lg bg-stone-500/5 p-3 dark:bg-white/5"
            >
              <div className="min-w-0">
                <span className="text-sm font-medium text-stone-900 dark:text-stone-100">
                  {hb.agent_id}
                </span>
                <p className="mt-0.5 text-xs text-stone-400 dark:text-stone-500">
                  {intl.formatMessage({ id: 'settings.heartbeat.enabledHelp' })}
                </p>
                <div className="mt-1 flex flex-wrap gap-3 text-xs text-stone-400">
                  <span>{describe(hb.cron, hb.interval_seconds)}</span>
                  <span>{intl.formatMessage({ id: 'settings.heartbeat.runs' })}: {hb.total_runs}</span>
                  {hb.last_run && (
                    <span>{intl.formatMessage({ id: 'settings.heartbeat.last' })}: {new Date(hb.last_run).toLocaleTimeString()}</span>
                  )}
                </div>
              </div>
              <div className="flex items-center gap-3">
                <span className="text-xs text-stone-400">
                  {hb.active_runs}/{hb.max_concurrent}
                </span>
                <Switch
                  checked={hb.enabled}
                  label={intl.formatMessage({ id: 'settings.heartbeat.enabledHelp' })}
                  onChange={(next) => {
                    api.agents.update(hb.agent_id, { heartbeat_enabled: next }).then(() => {
                      setHeartbeats((prev) =>
                        prev.map((h) => h.agent_id === hb.agent_id ? { ...h, enabled: next } : h)
                      );
                    }).catch((e) => {
                      console.warn("[api]", e);
                      toast.error(intl.formatMessage({ id: 'toast.error.saveFailed' }, { message: formatError(e) }));
                    });
                  }}
                />
                <button
                  onClick={() => api.heartbeat.trigger(hb.agent_id).catch((e) => {
                    console.warn("[api]", e);
                    toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
                  })}
                  title={intl.formatMessage({ id: 'settings.heartbeat.triggerNow' })}
                  className="rounded px-1.5 py-0.5 text-xs text-amber-600 hover:bg-amber-500/10 dark:text-amber-400"
                >
                  <Play className="h-3 w-3" />
                </button>
              </div>
            </div>
          ))}
        </div>
      )}
    </Card>
  );
}
