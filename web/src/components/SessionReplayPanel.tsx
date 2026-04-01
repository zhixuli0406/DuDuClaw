import { useEffect } from 'react';
import { useIntl } from 'react-intl';

function isSafeUrl(url: string): boolean {
  try {
    const parsed = new URL(url);
    return parsed.protocol === 'https:' || parsed.protocol === 'http:';
  } catch {
    return false;
  }
}
import { useBrowserStore } from '@/stores/browser-store';
import { Play, Square, ExternalLink, Clock, DollarSign, MonitorPlay } from 'lucide-react';

export function SessionReplayPanel() {
  const intl = useIntl();
  const {
    browserbaseSessions,
    browserbaseCost,
    browserbaseLoading,
    fetchBrowserbaseSessions,
    fetchBrowserbaseCost,
    createBrowserbaseSession,
    closeBrowserbaseSession,
  } = useBrowserStore();

  useEffect(() => {
    fetchBrowserbaseSessions();
    fetchBrowserbaseCost(24);
  }, [fetchBrowserbaseSessions, fetchBrowserbaseCost]);

  return (
    <div className="rounded-xl border border-stone-200 bg-white p-5 dark:border-stone-700 dark:bg-stone-800/50">
      {/* Header */}
      <div className="mb-4 flex items-center justify-between">
        <div className="flex items-center gap-2">
          <MonitorPlay className="h-5 w-5 text-amber-500" />
          <h3 className="font-semibold text-stone-900 dark:text-stone-50">
            {intl.formatMessage({ id: 'browser.sessions.title' })}
          </h3>
        </div>
        <button
          onClick={() => createBrowserbaseSession()}
          className="flex items-center gap-1 rounded-lg bg-amber-50 px-3 py-1.5 text-xs font-medium text-amber-700 hover:bg-amber-100 dark:bg-amber-900/20 dark:text-amber-400 dark:hover:bg-amber-900/40"
        >
          <Play className="h-3 w-3" />
          {intl.formatMessage({ id: 'browser.sessions.create' })}
        </button>
      </div>

      {/* Cost summary */}
      {browserbaseCost && (
        <div className="mb-4 flex items-center gap-4 rounded-lg bg-stone-50 px-4 py-3 dark:bg-stone-900">
          <div className="flex items-center gap-1.5 text-sm">
            <DollarSign className="h-4 w-4 text-emerald-500" />
            <span className="font-medium text-stone-700 dark:text-stone-200">
              {browserbaseCost.total_cost_usd}
            </span>
            <span className="text-xs text-stone-400">{intl.formatMessage({ id: 'browser.sessions.period' })}</span>
          </div>
          <div className="flex items-center gap-1.5 text-sm">
            <Clock className="h-4 w-4 text-blue-500" />
            <span className="text-stone-600 dark:text-stone-300">
              {Math.round((browserbaseCost.total_duration_seconds ?? 0) / 60)}m
            </span>
          </div>
          <div className="text-xs text-stone-400">
            {browserbaseCost.total_sessions} {intl.formatMessage({ id: 'browser.sessions.count' })}
          </div>
        </div>
      )}

      {/* Sessions list */}
      {browserbaseLoading ? (
        <p className="py-6 text-center text-sm text-stone-400">
          {intl.formatMessage({ id: 'common.loading' })}
        </p>
      ) : browserbaseSessions.length === 0 ? (
        <p className="py-6 text-center text-sm text-stone-400">
          {intl.formatMessage({ id: 'browser.sessions.empty' })}
        </p>
      ) : (
        <div className="space-y-2">
          {browserbaseSessions.map((s) => (
            <div
              key={s.session_id}
              className="flex items-center gap-3 rounded-lg border border-stone-100 bg-stone-50/50 px-3 py-2.5 dark:border-stone-700 dark:bg-stone-800"
            >
              {/* Status indicator */}
              <div className={`h-2 w-2 rounded-full ${
                s.status === 'running' ? 'bg-emerald-400 animate-pulse' :
                s.status === 'completed' ? 'bg-stone-400' :
                'bg-amber-400'
              }`} />

              {/* Session ID */}
              <span className="font-mono text-xs text-stone-600 dark:text-stone-300">
                {s.session_id.slice(0, 12)}...
              </span>

              {/* Status */}
              <span className={`rounded-md px-2 py-0.5 text-xs font-medium ${
                s.status === 'running'
                  ? 'bg-emerald-100 text-emerald-700 dark:bg-emerald-900/30 dark:text-emerald-400'
                  : 'bg-stone-100 text-stone-600 dark:bg-stone-700 dark:text-stone-400'
              }`}>
                {s.status}
              </span>

              {/* Timestamp */}
              <span className="text-xs text-stone-400">
                {new Date(s.created_at).toLocaleTimeString()}
              </span>

              {/* Actions */}
              <div className="ml-auto flex items-center gap-1">
                {/* Replay link */}
                {s.replay_url && isSafeUrl(s.replay_url) ? (
                  <a
                    href={s.replay_url}
                    target="_blank"
                    rel="noopener noreferrer"
                    className="flex items-center gap-1 rounded-md px-2 py-1 text-xs text-amber-600 hover:bg-amber-50 dark:text-amber-400 dark:hover:bg-amber-900/20"
                  >
                    <ExternalLink className="h-3 w-3" />
                    {intl.formatMessage({ id: 'browser.sessions.replay', defaultMessage: 'Replay' })}
                  </a>
                ) : (
                  <span className="px-2 py-1 text-xs text-stone-400">{intl.formatMessage({ id: 'browser.sessions.replay', defaultMessage: 'Replay' })}</span>
                )}

                {/* Stop button (only for running sessions) */}
                {s.status === 'running' && (
                  <button
                    onClick={() => closeBrowserbaseSession(s.session_id)}
                    className="flex items-center gap-1 rounded-md px-2 py-1 text-xs text-rose-500 hover:bg-rose-50 dark:hover:bg-rose-900/20"
                  >
                    <Square className="h-3 w-3" />
                    {intl.formatMessage({ id: 'browser.sessions.stop', defaultMessage: 'Stop' })}
                  </button>
                )}
              </div>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
