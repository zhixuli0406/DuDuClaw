import { useEffect, useRef, useMemo } from 'react';
import { useIntl } from 'react-intl';
import { cn } from '@/lib/utils';
import { useLogsStore, selectFilteredEntries } from '@/stores/logs-store';
import { useAgentsStore } from '@/stores/agents-store';
import {
  Pause,
  Play,
  Trash2,
  Search,
} from 'lucide-react';

const levelStyles: Record<string, string> = {
  trace: 'text-stone-400 dark:text-stone-500',
  debug: 'text-stone-500 dark:text-stone-400',
  info: 'text-blue-600 dark:text-blue-400',
  warn: 'text-amber-600 dark:text-amber-400',
  error: 'text-rose-600 dark:text-rose-400',
};

const levelBg: Record<string, string> = {
  trace: 'bg-stone-100 dark:bg-stone-800',
  debug: 'bg-stone-100 dark:bg-stone-800',
  info: 'bg-blue-50 dark:bg-blue-900/20',
  warn: 'bg-amber-50 dark:bg-amber-900/20',
  error: 'bg-rose-50 dark:bg-rose-900/20',
};

export function LogsPage() {
  const intl = useIntl();
  const {
    entries,
    paused,
    filter,
    subscribe,
    unsubscribe,
    togglePause,
    setFilter,
    clear,
  } = useLogsStore();
  const filteredEntries = useMemo(() => selectFilteredEntries({ entries, filter }), [entries, filter]);
  const { agents, fetchAgents } = useAgentsStore();
  const listRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    fetchAgents();
    subscribe();
    return () => {
      unsubscribe();
    };
  }, [fetchAgents, subscribe, unsubscribe]);

  // Auto-scroll to bottom when new entries arrive (unless paused)
  useEffect(() => {
    if (!paused && listRef.current) {
      listRef.current.scrollTop = listRef.current.scrollHeight;
    }
  }, [filteredEntries, paused]);

  const levels = ['trace', 'debug', 'info', 'warn', 'error'];

  return (
    <div className="flex h-full flex-col space-y-4">
      <h2 className="text-2xl font-semibold text-stone-900 dark:text-stone-50">
        {intl.formatMessage({ id: 'logs.title' })}
      </h2>

      {/* Filter bar */}
      <div className="flex flex-wrap items-center gap-3">
        {/* Level select */}
        <select
          value={filter.level ?? ''}
          onChange={(e) => setFilter({ level: e.target.value || null })}
          className="rounded-lg border border-stone-200 bg-white px-3 py-2 text-sm text-stone-700 focus:border-amber-500 focus:outline-none focus:ring-1 focus:ring-amber-500 dark:border-stone-700 dark:bg-stone-800 dark:text-stone-300"
        >
          <option value="">
            {intl.formatMessage({ id: 'logs.filter.all' })}
          </option>
          {levels.map((level) => (
            <option key={level} value={level}>
              {level.toUpperCase()}
            </option>
          ))}
        </select>

        {/* Agent filter */}
        <select
          value={filter.agentId ?? ''}
          onChange={(e) => setFilter({ agentId: e.target.value || null })}
          className="rounded-lg border border-stone-200 bg-white px-3 py-2 text-sm text-stone-700 focus:border-amber-500 focus:outline-none focus:ring-1 focus:ring-amber-500 dark:border-stone-700 dark:bg-stone-800 dark:text-stone-300"
        >
          <option value="">
            {intl.formatMessage({ id: 'logs.filter.agent' })}
          </option>
          {agents.map((agent) => (
            <option key={agent.name} value={agent.name}>
              {agent.display_name}
            </option>
          ))}
        </select>

        {/* Keyword search */}
        <div className="relative flex-1 min-w-[200px]">
          <Search className="absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-stone-400" />
          <input
            type="text"
            value={filter.keyword}
            onChange={(e) => setFilter({ keyword: e.target.value })}
            placeholder="Filter..."
            className="w-full rounded-lg border border-stone-200 bg-white py-2 pl-10 pr-4 text-sm text-stone-900 placeholder:text-stone-400 focus:border-amber-500 focus:outline-none focus:ring-1 focus:ring-amber-500 dark:border-stone-700 dark:bg-stone-800 dark:text-stone-50 dark:placeholder:text-stone-500"
          />
        </div>

        {/* Pause / Clear buttons */}
        <button
          onClick={togglePause}
          className={cn(
            'inline-flex items-center gap-1.5 rounded-lg px-3 py-2 text-sm font-medium transition-colors',
            paused
              ? 'bg-emerald-100 text-emerald-700 hover:bg-emerald-200 dark:bg-emerald-900/30 dark:text-emerald-400'
              : 'bg-amber-100 text-amber-700 hover:bg-amber-200 dark:bg-amber-900/30 dark:text-amber-400'
          )}
        >
          {paused ? (
            <>
              <Play className="h-3.5 w-3.5" />
              {intl.formatMessage({ id: 'logs.resume' })}
            </>
          ) : (
            <>
              <Pause className="h-3.5 w-3.5" />
              {intl.formatMessage({ id: 'logs.pause' })}
            </>
          )}
        </button>

        <button
          onClick={clear}
          className="inline-flex items-center gap-1.5 rounded-lg bg-stone-100 px-3 py-2 text-sm font-medium text-stone-600 transition-colors hover:bg-stone-200 dark:bg-stone-800 dark:text-stone-400 dark:hover:bg-stone-700"
        >
          <Trash2 className="h-3.5 w-3.5" />
          {intl.formatMessage({ id: 'logs.clear' })}
        </button>
      </div>

      {/* Log entries */}
      <div
        ref={listRef}
        className="flex-1 overflow-y-auto rounded-xl border border-stone-200 bg-stone-950 p-1 dark:border-stone-800"
      >
        {filteredEntries.length === 0 ? (
          <div className="flex items-center justify-center py-16 text-stone-500">
            <p>{intl.formatMessage({ id: 'common.noData' })}</p>
          </div>
        ) : (
          <div className="space-y-px">
            {filteredEntries.map((entry, i) => (
              <div
                key={`${entry.target}-${entry.timestamp}-${i}`}
                className={cn(
                  'flex items-start gap-3 rounded px-3 py-1.5 font-mono text-xs',
                  levelBg[entry.level] ?? 'bg-transparent'
                )}
              >
                {/* Timestamp */}
                <span className="shrink-0 text-stone-500">
                  {formatTimestamp(entry.timestamp)}
                </span>

                {/* Level badge */}
                <span
                  className={cn(
                    'w-12 shrink-0 text-right font-semibold uppercase',
                    levelStyles[entry.level] ?? 'text-stone-400'
                  )}
                >
                  {entry.level}
                </span>

                {/* Target */}
                <span className="shrink-0 text-stone-400 dark:text-stone-500">
                  {entry.target}
                </span>

                {/* Agent ID */}
                {entry.agent_id && (
                  <span className="shrink-0 rounded bg-amber-900/30 px-1.5 py-0.5 text-amber-400">
                    {entry.agent_id}
                  </span>
                )}

                {/* Message */}
                <span
                  className={cn(
                    'flex-1 break-all',
                    entry.level === 'error'
                      ? 'text-rose-300'
                      : entry.level === 'warn'
                        ? 'text-amber-300'
                        : 'text-stone-300'
                  )}
                >
                  {entry.message}
                </span>
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

function formatTimestamp(ts: string): string {
  try {
    const date = new Date(ts);
    return date.toLocaleTimeString('zh-TW', {
      hour12: false,
      hour: '2-digit',
      minute: '2-digit',
      second: '2-digit',
      fractionalSecondDigits: 3,
    });
  } catch {
    return ts;
  }
}
