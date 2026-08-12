import { useCallback, useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { api } from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import {
  Button,
  Empty,
  ErrorState,
  Switch,
  SettingsSection,
  SettingsCard,
  SettingsRow,
} from '@/components/mds';
import { describeCron } from '@/components/settings/controls';
import { HeartPulse, Play } from 'lucide-react';

export function HeartbeatTab() {
  const intl = useIntl();
  // P05: an empty `heartbeats` array is rendered as "no scheduled work", which
  // is exactly what a failed fetch produced too. `loadError` splits the two,
  // and `rowErrors` marks the single row whose toggle/trigger did not land.
  const [loadError, setLoadError] = useState<unknown>(null);
  const [rowErrors, setRowErrors] = useState<Readonly<Record<string, unknown>>>({});
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

  const load = useCallback(() => {
    setLoadError(null);
    return api.heartbeat
      .status()
      .then((r) => setHeartbeats(r?.heartbeats ?? []))
      .catch((e) => {
        console.warn("[api]", e);
        setLoadError(e);
        toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
      });
  }, [intl]);

  useEffect(() => {
    void load();
    // 15s poll stays silent — transient errors would spam the user; the
    // initial load error state is enough to flag persistent problems.
    const interval = setInterval(() => {
      api.heartbeat
        .status()
        .then((r) => { setHeartbeats(r?.heartbeats ?? []); setLoadError(null); })
        .catch((e) => console.warn("[api]", e));
    }, 15_000);
    return () => clearInterval(interval);
  }, [load]);

  const markRowError = (agentId: string, error: unknown) =>
    setRowErrors((prev) => ({ ...prev, [agentId]: error }));
  const clearRowError = (agentId: string) =>
    setRowErrors((prev) => {
      if (!(agentId in prev)) return prev;
      const next = { ...prev };
      delete next[agentId];
      return next;
    });

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

  // A failed fetch must never be dressed up as "nothing scheduled".
  if (loadError != null && heartbeats.length === 0) {
    return (
      <ErrorState
        error={loadError}
        title={intl.formatMessage({ id: 'errorState.manage.loadFailed' })}
        description={intl.formatMessage({ id: 'errorState.manage.notEmptyHint' })}
        onRetry={() => void load()}
      />
    );
  }

  if (heartbeats.length === 0) {
    return (
      <Empty
        icon={HeartPulse}
        variant="dashed"
        title={intl.formatMessage({ id: 'common.noData' })}
      />
    );
  }

  return (
    <SettingsSection>
      {loadError != null && (
        <ErrorState
          variant="inline"
          error={loadError}
          title={intl.formatMessage({ id: 'errorState.manage.loadFailed' })}
          onRetry={() => void load()}
        />
      )}
      <SettingsCard>
        {heartbeats.map((hb) => (
          <SettingsRow
            key={hb.agent_id}
            label={hb.agent_id}
            description={
              // A div, not a span: the row error below is a block-level alert
              // and `<div>` inside `<span>` is invalid DOM nesting.
              <div className="flex flex-col gap-1">
                <span className="flex flex-wrap gap-x-3 gap-y-0.5">
                  <span>{describe(hb.cron, hb.interval_seconds)}</span>
                  <span>{intl.formatMessage({ id: 'settings.heartbeat.runs' })}: {hb.total_runs}</span>
                  {hb.last_run && (
                    <span>{intl.formatMessage({ id: 'settings.heartbeat.last' })}: {new Date(hb.last_run).toLocaleTimeString()}</span>
                  )}
                </span>
                {/* A toggle or trigger that failed leaves its reason on the row
                    it belongs to — a toast can't say WHICH agent it was. */}
                {rowErrors[hb.agent_id] != null && (
                  <ErrorState
                    variant="inline"
                    error={rowErrors[hb.agent_id]}
                    title={intl.formatMessage({ id: 'errorState.manage.actionFailed' })}
                  />
                )}
              </div>
            }
          >
            <div className="flex items-center gap-3">
              <span className="font-mono text-xs tabular-nums text-muted-foreground">
                {hb.active_runs}/{hb.max_concurrent}
              </span>
              <Switch
                checked={hb.enabled}
                aria-label={intl.formatMessage({ id: 'settings.heartbeat.enabledHelp' })}
                onCheckedChange={(next) => {
                  clearRowError(hb.agent_id);
                  api.agents.update(hb.agent_id, { heartbeat_enabled: Boolean(next) }).then(() => {
                    setHeartbeats((prev) =>
                      prev.map((h) => h.agent_id === hb.agent_id ? { ...h, enabled: Boolean(next) } : h)
                    );
                  }).catch((e) => {
                    console.warn("[api]", e);
                    markRowError(hb.agent_id, e);
                    toast.error(intl.formatMessage({ id: 'toast.error.saveFailed' }, { message: formatError(e) }));
                  });
                }}
              />
              <Button
                variant="ghost"
                size="icon-xs"
                onClick={() => {
                  clearRowError(hb.agent_id);
                  void api.heartbeat.trigger(hb.agent_id).catch((e) => {
                    console.warn("[api]", e);
                    markRowError(hb.agent_id, e);
                    toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
                  });
                }}
                title={intl.formatMessage({ id: 'settings.heartbeat.triggerNow' })}
              >
                <Play />
              </Button>
            </div>
          </SettingsRow>
        ))}
      </SettingsCard>
    </SettingsSection>
  );
}
